// SPDX-License-Identifier: AGPL-3.0-only
//! The streaming half of the Messages adapter: SSE aggregation for one
//! completion, split out of `anthropic.rs` when the streaming→non-streaming
//! fallback (#2686, extended to this dialect by #2746) landed.
//!
//! [`aggregate_anthropic_stream`] consumes one `/v1/messages` SSE body into
//! the adapter's result tuple. Its error type is [`StreamFault`] rather than
//! a bare `ProviderError`, because the caller needs one extra bit: whether
//! the fault is **fallback-eligible** — the stream hung before its first
//! byte, or ended with nothing accumulated at all — which is what arms the
//! [`crate::stream_recovery::StreamRecovery`] latch so the retry of this
//! attempt goes out as a unary request. Every other failure (a mid-stream
//! death with content already salvaged, a malformed frame, an in-band error
//! event) keeps its existing classification and never arms the latch.
//!
//! The per-call assembly rules the streaming and unary ([`super::unary`])
//! paths must agree on — how a usage frame folds into the normalized
//! envelope, and what a token-limit-truncated tool call costs — live here as
//! shared helpers so the two paths cannot drift.

use std::collections::BTreeMap;
use std::time::Duration;

use stella_protocol::{CompletionUsage, ProviderError, ToolCall};

use super::{
    AnthropicDelta, AnthropicStartBlock, AnthropicStreamEvent, AnthropicUsage,
    classify_anthropic_stream_error,
};
use crate::catalog::Pricing;
use crate::http;
use crate::provider::ToolCallObserver;
use crate::sse::SseDecoder;
use crate::stream_recovery::StreamFault;

/// Fold one usage frame into the normalized envelope. Shared verbatim by the
/// streaming aggregator (`message_start` and a `message_delta` that restates
/// the input side) and the unary fallback (the response-level `usage`
/// object), so the two paths cannot disagree about how this dialect's cache
/// telemetry is read.
///
/// Anthropic reports cache reads and writes *separately* from `input_tokens`
/// rather than folded in, so the read count is added back to keep the
/// normalized `cached_input_tokens` a subset of `input_tokens`; the write
/// count is not, because the catalog prices writes on their own line.
pub(super) fn fold_usage(frame: &AnthropicUsage, usage: &mut CompletionUsage) {
    usage.input_tokens = frame.input_tokens + frame.cache_read_input_tokens;
    usage.cached_input_tokens = frame.cache_read_input_tokens;
    usage.cache_write_tokens = frame.cache_creation_input_tokens;
}

/// The terminal error for a tool call the output-token limit cut before its
/// arguments were whole. Shared by both delivery paths so the wire-level
/// evidence they cite (`stop_reason=max_tokens`) cannot drift apart.
pub(super) fn truncated_tool_input(name: &str, raw: &str) -> ProviderError {
    http::truncated_tool_input_error("Anthropic", name, raw, "stop_reason=max_tokens")
}

/// Accumulator for one in-progress `tool_use` block, keyed by its stream
/// index until the block completes.
#[derive(Default)]
struct ToolUseAccumulator {
    id: String,
    name: String,
    input_json: String,
}

/// Everything one assembled SSE body yields.
pub(super) struct AnthropicStreamOutcome {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: CompletionUsage,
    /// The raw `stop_reason` from `message_delta`, normalized by the caller.
    pub stop_reason: Option<String>,
}

pub(super) async fn aggregate_anthropic_stream(
    response: reqwest::Response,
    observer: Option<&dyn ToolCallObserver>,
    pricing: Option<&Pricing>,
    first_byte: Duration,
) -> Result<AnthropicStreamOutcome, StreamFault> {
    let mut decoder = SseDecoder::new();
    let mut text = String::new();
    let mut usage = CompletionUsage::default();
    let mut tool_uses: BTreeMap<usize, ToolUseAccumulator> = BTreeMap::new();
    // Why generation ended, from the `message_delta` event. `"max_tokens"`
    // means the stream was cut off at the output-token limit — the signal a
    // truncated tool-call payload needs to be reported as such rather than
    // silently nulled.
    let mut stop_reason: Option<String> = None;
    // The highest content-block index that started. Blocks stream
    // sequentially, so only this block can have been cut off by the token
    // limit — a later block starting proves every earlier one closed.
    let mut last_block_index: Option<usize> = None;
    let mut message_stop_seen = false;
    // Anything at all having arrived, including a frame this adapter does not
    // model: a `message_start` or a `ping` is proof the streaming path works,
    // so a stall after one is a mid-stream death, not the buffering proxy the
    // fallback exists for.
    let mut anything_arrived = false;
    // The first body read runs against the (shorter) first-byte deadline
    // rather than the inter-fragment idle bound: a response that has sent its
    // headers and then not one body byte is a buffering proxy, not a thinking
    // model (#2686). Any chunk at all — even a keep-alive — moves the stream
    // onto the ordinary idle clock.
    let mut awaiting_first_chunk = true;
    let mut stream = response.bytes_stream();

    // The stream can die at any chunk, and by then `message_start` has
    // usually already told us exactly what the prompt cost. Attaching that to
    // the error is the difference between "this attempt is unaccounted" and a
    // real floor under the turn's spend.
    loop {
        let idle = if awaiting_first_chunk {
            first_byte
        } else {
            http::STREAM_IDLE_TIMEOUT
        };
        let chunk = match http::next_stream_read(&mut stream, idle).await {
            http::StreamRead::Item(chunk) => chunk,
            http::StreamRead::End => break,
            // A transport fault (reset, TLS error) is NOT fallback-eligible:
            // it says nothing about the streaming path specifically, and the
            // ordinary retry may well succeed over the same stream.
            http::StreamRead::Failed(message) => {
                return Err(StreamFault::ineligible(http::attach_partial(
                    ProviderError::transport(message),
                    &usage,
                    &text,
                    pricing,
                )));
            }
            http::StreamRead::Idle => {
                let error = if awaiting_first_chunk {
                    http::hung_before_first_byte("Anthropic", idle)
                } else {
                    ProviderError::transport(format!(
                        "stream idle timeout: no data for {}s",
                        idle.as_secs()
                    ))
                };
                // A hang is fallback-eligible exactly when there is nothing
                // to lose: no frame of any kind arrived. A stream that hung
                // after real content is the existing mid-stream-death case —
                // retried as a stream, its salvage attached.
                return Err(StreamFault {
                    fallback_eligible: !anything_arrived,
                    error: http::attach_partial(error, &usage, &text, pricing),
                });
            }
        };
        awaiting_first_chunk = false;
        decoder
            .push_bytes(&chunk)
            .map_err(|e| ProviderError::Malformed(e.to_string()))?;
        for event in decoder.poll() {
            if event.data.trim() == "[DONE]" || event.data.is_empty() {
                continue;
            }
            anything_arrived = true;
            let parsed: Result<AnthropicStreamEvent, _> = serde_json::from_str(&event.data);
            match parsed {
                Ok(AnthropicStreamEvent::Error { error }) => {
                    // A mid-stream error aborts the turn with a typed error —
                    // never a truncated Ok with the text seen so far. It is a
                    // loss site like the two above, and carries the same
                    // accounting: `message_start` has usually already reported
                    // what the prompt cost, and the provider bills it whether
                    // or not the stream then dies (#3859).
                    return Err(http::attach_partial(
                        classify_anthropic_stream_error(&error),
                        &usage,
                        &text,
                        pricing,
                    )
                    .into());
                }
                Ok(AnthropicStreamEvent::MessageStart { message }) => {
                    if let Some(u) = message.usage {
                        fold_usage(&u, &mut usage);
                    }
                }
                Ok(AnthropicStreamEvent::ContentBlockStart {
                    index,
                    content_block,
                }) => {
                    last_block_index = Some(index);
                    if let AnthropicStartBlock::ToolUse { id, name } = content_block {
                        tool_uses.insert(
                            index,
                            ToolUseAccumulator {
                                id,
                                name,
                                input_json: String::new(),
                            },
                        );
                    }
                }
                Ok(AnthropicStreamEvent::ContentBlockDelta { index, delta }) => match delta {
                    // Answer text and thinking are announced on separate
                    // observer channels and never merged: `text` remains the
                    // reply, while thinking renders as its own collapsible
                    // transcript entry.
                    AnthropicDelta::TextDelta { text: delta } => {
                        if let Some(observer) = observer {
                            observer.text_delta(&delta);
                        }
                        text.push_str(&delta);
                    }
                    AnthropicDelta::ThinkingDelta { thinking } => {
                        if let Some(observer) = observer {
                            observer.reasoning_delta(&thinking);
                        }
                    }
                    AnthropicDelta::InputJsonDelta { partial_json } => {
                        // Liveness only — the partial JSON itself is never
                        // announced (tool_call_streamed owns the parsed
                        // whole). Without this tick a generation that is one
                        // large tool call streams in total observer silence
                        // and an idle deadline kills it as stalled.
                        if let Some(observer) = observer {
                            observer.tool_input_delta();
                        }
                        if let Some(acc) = tool_uses.get_mut(&index) {
                            acc.input_json.push_str(&partial_json);
                        }
                    }
                    AnthropicDelta::Other => {}
                },
                Ok(AnthropicStreamEvent::ContentBlockStop { index }) => {
                    // The earliest moment a tool call is complete. Announce
                    // it to the observer ONLY when its input already parses —
                    // a block whose JSON is broken or truncated must go
                    // through the end-of-stream repair/truncation logic
                    // below, never reach speculative execution. The
                    // accumulator stays in the map: the final assembly below
                    // remains the single source of truth, and it re-parses
                    // the same bytes, so an announced call and its committed
                    // twin are structurally identical.
                    if let (Some(observer), Some(acc)) = (observer, tool_uses.get(&index)) {
                        let input = if acc.input_json.is_empty() {
                            Some(serde_json::json!({}))
                        } else {
                            serde_json::from_str(&acc.input_json).ok()
                        };
                        if let Some(input) = input
                            && !acc.id.is_empty()
                        {
                            observer.tool_call_streamed(&ToolCall {
                                call_id: acc.id.clone(),
                                name: acc.name.clone(),
                                input,
                            });
                        }
                    }
                }
                Ok(AnthropicStreamEvent::MessageDelta { delta, usage: u }) => {
                    if let Some(reason) = delta.stop_reason {
                        stop_reason = Some(reason);
                    }
                    if let Some(u) = u {
                        usage.reported = true;
                        if u.input_tokens > 0 {
                            // Take the whole input-side picture from ONE
                            // frame: `input_tokens` and its cache splits
                            // describe the same request, so under independent
                            // `> 0` guards a delta that restates
                            // `input_tokens` without the cache fields (some
                            // gateways do) kept an earlier frame's
                            // `cached_input_tokens` — leaving cached > input,
                            // a 100% "hit rate" on a turn that read nothing,
                            // and thousands of cache-read tokens off the bill.
                            fold_usage(&u, &mut usage);
                        } else {
                            // A cache-only delta (no input restatement) still
                            // updates the split it actually reports.
                            if u.cache_read_input_tokens > 0 {
                                usage.cached_input_tokens = u.cache_read_input_tokens;
                            }
                            if u.cache_creation_input_tokens > 0 {
                                usage.cache_write_tokens = u.cache_creation_input_tokens;
                            }
                        }
                        usage.output_tokens = u.output_tokens;
                    }
                }
                Ok(AnthropicStreamEvent::MessageStop) => {
                    message_stop_seen = true;
                }
                Ok(_) => {}
                Err(_) => {
                    // A data line that did not deserialize into
                    // `AnthropicStreamEvent` at all — malformed or partial
                    // JSON, or JSON lacking the string `type` this tagged enum
                    // needs. An event carrying a `type` we simply don't model
                    // (e.g. `ping`) is NOT here: it deserializes to `Other`
                    // above via `#[serde(other)]` and lands in `Ok(_)`.
                    // Tolerated, never fatal: one unparseable frame is dropped
                    // and the stream continues; a genuinely truncated stream is
                    // caught by the `message_stop` check below, not here.
                }
            }
        }
    }

    // EOF without `message_stop` is a disconnect, not a completion — and
    // nothing accumulated from a cut stream (text, stop_reason, tool
    // fragments) is trustworthy enough to classify further. Retryable
    // Transport, upholding the same "never a truncated Ok" promise as the
    // in-stream error path above. When NOTHING arrived it is the other
    // broken-stream shape #2686 names — a gateway answering 200 with an empty
    // stream — and is fallback-eligible: the retry loses nothing by going out
    // unary.
    if !message_stop_seen {
        return Err(StreamFault {
            fallback_eligible: !anything_arrived,
            error: http::attach_partial(
                http::stream_ended_before_terminal("Anthropic", "message_stop"),
                &usage,
                &text,
                pricing,
            ),
        });
    }

    // The one content block the token limit could have cut: the last block
    // started, and only when the stream actually stopped at `max_tokens`.
    // Pinning truncation to that block keeps the blame on the call that was
    // cut — an *earlier* call whose JSON is broken is the model's own
    // malformed output and still gets the repair sentinel below.
    let truncated_index = if stop_reason.as_deref() == Some("max_tokens") {
        last_block_index
    } else {
        None
    };

    let mut tool_calls = Vec::with_capacity(tool_uses.len());
    for (index, acc) in tool_uses {
        let truncated = Some(index) == truncated_index;
        let input = if acc.input_json.is_empty() {
            if truncated {
                // The limit landed after this call's `content_block_start`
                // but before its first `input_json_delta`: executing it with
                // `{}` would fail on missing parameters and re-enter the same
                // unwinnable retry-retruncate loop as a mid-payload cut.
                return Err(truncated_tool_input(&acc.name, "").into());
            }
            // A no-argument tool call arrives with no `input_json_delta` at
            // all: that is an empty object, never null.
            serde_json::json!({})
        } else {
            match serde_json::from_str(&acc.input_json) {
                Ok(value) => value,
                // The fragments were concatenated byte-exactly (the SSE
                // decoder's own tests prove arbitrary chunk boundaries
                // reassemble losslessly), so an unparseable buffer on the
                // block the token limit cut means the arguments never
                // finished streaming. Terminal and turn-aborting — mirroring
                // openai.rs's `response.incomplete` handling — because
                // retrying the identical request re-truncates identically:
                // the old silent `Null` here sent the driver's repair loop
                // into exactly that "stuck-loop".
                Err(_) if truncated => {
                    return Err(truncated_tool_input(&acc.name, &acc.input_json).into());
                }
                // Broken JSON on a block that *finished* is the model's own
                // malformed output: fall back to the `Value::Null` sentinel
                // `driver.rs::execute_with_repair` consumes (the documented
                // adapter contract), so the repair loop asks the model to
                // re-emit just this call instead of aborting the turn.
                Err(_) => serde_json::Value::Null,
            }
        };
        tool_calls.push(ToolCall {
            call_id: acc.id,
            name: acc.name,
            input,
        });
    }

    Ok(AnthropicStreamOutcome {
        text,
        tool_calls,
        usage,
        stop_reason,
    })
}
