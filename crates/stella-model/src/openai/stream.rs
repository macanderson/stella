// SPDX-License-Identifier: AGPL-3.0-only
//! The streaming half of the Responses adapter: SSE aggregation for one
//! completion, split out of `openai.rs` (a grandfathered god file closed to
//! growth) when the streaming→non-streaming fallback (#2686, extended to
//! this dialect by #2746) landed.
//!
//! [`aggregate_openai_stream`] consumes one `/responses` SSE body into the
//! adapter's outcome. Its error type is [`StreamFault`] rather than a bare
//! `ProviderError`, because the caller needs one extra bit: whether the fault
//! is **fallback-eligible** — the stream hung before its first byte, or ended
//! with nothing accumulated at all — which is what arms the
//! [`crate::stream_recovery::StreamRecovery`] latch so the retry of this
//! attempt goes out as a unary request. Every other failure (a mid-stream
//! death with content already salvaged, a `response.failed`, a
//! `response.incomplete`) keeps its existing classification and never arms
//! the latch.
//!
//! The per-call assembly rules the streaming and unary ([`super::unary`])
//! paths must agree on — how a usage envelope folds, how accumulated
//! arguments become a tool input, and which finish reason the two facts
//! imply — live here as shared helpers so the two paths cannot drift.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;
use stella_protocol::{CompletionUsage, FinishReason, ProviderError, ToolCall};

use super::{OpenAiOutputItem, OpenAiStreamEvent, OpenAiUsage, classify_openai_stream_error};
use crate::catalog::Pricing;
use crate::http;
use crate::provider::ToolCallObserver;
use crate::sse::SseDecoder;
use crate::stream_recovery::StreamFault;

/// Fold one usage envelope into the normalized shape. Shared verbatim by the
/// streaming aggregator (`response.completed` / `.failed` / `.incomplete`)
/// and the unary fallback (the response-level `usage` object), so the two
/// paths cannot disagree about how this dialect's cache and reasoning
/// telemetry is read. The Responses API folds cached tokens INTO
/// `input_tokens` already, unlike Anthropic's, so nothing is added back.
pub(super) fn fold_usage(frame: OpenAiUsage, usage: &mut CompletionUsage) {
    usage.input_tokens = frame.input_tokens;
    usage.output_tokens = frame.output_tokens;
    usage.cached_input_tokens = frame
        .input_tokens_details
        .map(|d| d.cached_tokens)
        .unwrap_or(0);
    usage.reasoning_tokens = frame.output_tokens_details.and_then(|d| d.reasoning_tokens);
}

/// One completed tool call's argument string as a JSON input. An empty body
/// is an empty *object*, not null — a downstream tool deserializing its input
/// as an object must not be handed `null`. Anything else that fails to parse
/// is the model's own broken JSON: it becomes the `Value::Null` sentinel
/// `driver.rs::execute_with_repair` checks for, so the repair loop can ask
/// the model to retry. Shared by both delivery paths.
pub(super) fn tool_call_input(raw: &str) -> Value {
    if raw.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(raw).unwrap_or(Value::Null)
    }
}

/// The finish reason both delivery paths report from the same two facts.
///
/// `Length` wins over `ToolCalls` when the response was cut off at
/// `max_output_tokens`: a step that hit the cap is not finished just because
/// some tool call happened to complete before the cut, and the driver's
/// continuation is what recovers the rest. Every sibling adapter reports the
/// cap this way, and one adapter disagreeing is how the same truncation came
/// to mean "continue" on four dialects and "the turn is dead" on this one.
pub(super) fn final_finish_reason(truncated_at_limit: bool, has_calls: bool) -> FinishReason {
    if truncated_at_limit {
        FinishReason::Length
    } else if has_calls {
        FinishReason::ToolCalls
    } else {
        FinishReason::Stop
    }
}

/// Accumulator for one in-progress `function_call` item, keyed by the
/// stream's `output_index` until it completes.
#[derive(Default)]
struct ToolCallAccumulator {
    call_id: String,
    name: String,
    arguments: String,
}

/// Everything one assembled SSE body yields.
pub(super) struct OpenAiStreamOutcome {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: CompletionUsage,
    /// Whether the response stopped at `max_output_tokens`.
    pub truncated_at_limit: bool,
}

pub(super) async fn aggregate_openai_stream(
    response: reqwest::Response,
    observer: Option<&dyn ToolCallObserver>,
    pricing: Option<&Pricing>,
    first_byte: Duration,
) -> Result<OpenAiStreamOutcome, StreamFault> {
    let mut decoder = SseDecoder::new();
    let mut text = String::new();
    let mut usage = CompletionUsage::default();
    let mut tool_calls: BTreeMap<usize, ToolCallAccumulator> = BTreeMap::new();
    let mut completed_seen = false;
    // Set when the response stopped at `max_output_tokens`. Carried out as a
    // normal result rather than an error so the engine sees the same
    // `FinishReason::Length` every other adapter reports — see the
    // `Incomplete` arm below.
    let mut truncated_at_limit = false;
    // Anything at all having arrived, including an event this adapter does
    // not model: a `response.created` is proof the streaming path works, so a
    // stall after one is a mid-stream death, not the buffering proxy the
    // fallback exists for.
    let mut anything_arrived = false;
    // The first body read runs against the (shorter) first-byte deadline
    // rather than the inter-fragment idle bound: a response that has sent its
    // headers and then not one body byte is a buffering proxy, not a thinking
    // model (#2686). Any chunk at all — even a keep-alive — moves the stream
    // onto the ordinary idle clock.
    let mut awaiting_first_chunk = true;
    let mut stream = response.bytes_stream();

    'stream: loop {
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
                    http::hung_before_first_byte("OpenAI", idle)
                } else {
                    ProviderError::transport(format!(
                        "stream idle timeout: no data for {}s",
                        idle.as_secs()
                    ))
                };
                // A hang is fallback-eligible exactly when there is nothing
                // to lose: no event of any kind arrived. A stream that hung
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
            let data = event.data.trim();
            if data.is_empty() {
                continue;
            }
            anything_arrived = true;
            let parsed: OpenAiStreamEvent = match serde_json::from_str(data) {
                Ok(v) => v,
                Err(_) => continue, // tolerate event shapes we don't model
            };
            match parsed {
                OpenAiStreamEvent::OutputItemAdded {
                    output_index,
                    item: OpenAiOutputItem::FunctionCall { call_id, name },
                } => {
                    let acc = tool_calls.entry(output_index).or_default();
                    acc.call_id = call_id;
                    acc.name = name;
                }
                OpenAiStreamEvent::OutputItemAdded { .. } => {}
                OpenAiStreamEvent::OutputTextDelta { delta } => {
                    if let Some(observer) = observer {
                        observer.text_delta(&delta);
                    }
                    text.push_str(&delta);
                }
                OpenAiStreamEvent::FunctionCallArgumentsDelta {
                    output_index,
                    delta,
                } => {
                    // Liveness only (see `ToolCallObserver::tool_input_delta`):
                    // a call-only generation must still register as producing.
                    if let Some(observer) = observer {
                        observer.tool_input_delta();
                    }
                    tool_calls
                        .entry(output_index)
                        .or_default()
                        .arguments
                        .push_str(&delta);
                }
                OpenAiStreamEvent::FunctionCallArgumentsDone { output_index } => {
                    // Announce from the ACCUMULATOR, not from the event's own
                    // copy of the arguments: final assembly below parses these
                    // same bytes, so what the observer is told is exactly what
                    // the completion will return, as the trait requires.
                    if let Some(observer) = observer
                        && let Some(acc) = tool_calls.get(&output_index)
                        && !acc.call_id.is_empty()
                    {
                        // Never announce a call whose input failed to parse —
                        // speculation on a malformed call would execute
                        // something the model did not ask for. A broken
                        // payload still reaches final assembly, where it
                        // becomes `Value::Null` and the repair path owns it.
                        let input = if acc.arguments.is_empty() {
                            Some(serde_json::json!({}))
                        } else {
                            serde_json::from_str(&acc.arguments).ok()
                        };
                        if let Some(input) = input {
                            observer.tool_call_streamed(&ToolCall {
                                call_id: acc.call_id.clone(),
                                name: acc.name.clone(),
                                input,
                            });
                        }
                    }
                }
                OpenAiStreamEvent::Completed { response } => {
                    completed_seen = true;
                    if let Some(u) = response.usage {
                        usage.reported = true;
                        fold_usage(u, &mut usage);
                    }
                }
                // A mid-stream failure/incompletion/error aborts the turn with
                // a typed error — never a truncated Ok with the text so far.
                OpenAiStreamEvent::Failed { response } => {
                    // A failed response still carries its usage envelope, and
                    // the provider bills the prompt whether or not it served
                    // an answer — the same reason `response.incomplete` below
                    // reads it. Left on the floor, a brownout that parks and
                    // recovers reports the abandoned attempt as free (#3859).
                    if let Some(u) = response.usage {
                        fold_usage(u, &mut usage);
                    }
                    let (code, message) = response
                        .error
                        .map(|e| (e.code, e.message.unwrap_or_default()))
                        .unwrap_or((None, String::new()));
                    let error = classify_openai_stream_error(code.as_deref(), &message);
                    return Err(http::attach_partial(error, &usage, &text, pricing).into());
                }
                OpenAiStreamEvent::Incomplete { response } => {
                    // An incomplete response still carries the final usage
                    // envelope, and a cap-hit turn is the MOST expensive kind
                    // (the whole output budget was spent). Dropping it here
                    // billed every truncated turn as $0 — invisible to the
                    // budget guard and `stella stats` alike.
                    if let Some(u) = response.usage {
                        usage.reported = true;
                        fold_usage(u, &mut usage);
                    }
                    let reason = response
                        .incomplete_details
                        .and_then(|d| d.reason)
                        .unwrap_or_else(|| "unspecified".to_string());
                    // Hitting the output cap is not a provider failure — it is
                    // the ordinary "cut off mid-work" outcome that zai,
                    // Anthropic, Gemini/Vertex and Bedrock all surface as
                    // `FinishReason::Length`, and that the driver answers with
                    // an in-turn continuation. Returning `Terminal` here made
                    // this dialect the one place a truncation killed the turn
                    // outright, and non-retryably at that: the same event, on
                    // the same model, behaved differently depending on which
                    // adapter carried it. Break with what accumulated and let
                    // the engine apply one policy to every provider.
                    if reason == "max_output_tokens" {
                        truncated_at_limit = true;
                        completed_seen = true;
                        // Exit the STREAM, not just this chunk's events: a
                        // plain `break` left the outer read loop waiting on a
                        // keep-alive connection for the full idle timeout
                        // after the terminal event had already arrived.
                        break 'stream;
                    }
                    // Every other incompletion (content filters, and whatever
                    // OpenAI adds later) stays terminal: those are not
                    // "continue and it will finish" conditions, and guessing
                    // that they are would trade a loud stop for a silent loop.
                    return Err(ProviderError::Terminal(format!(
                        "OpenAI response incomplete: {reason}"
                    ))
                    .into());
                }
                OpenAiStreamEvent::Error { code, message } => {
                    let error = classify_openai_stream_error(
                        code.as_deref(),
                        message.as_deref().unwrap_or_default(),
                    );
                    return Err(http::attach_partial(error, &usage, &text, pricing).into());
                }
                OpenAiStreamEvent::Other => {}
            }
        }
    }

    // EOF without `response.completed` (and without the failed/incomplete/
    // error events handled above) is a disconnect, not a completion —
    // whatever accumulated is a half-answer. Retryable Transport, upholding
    // the same "never a truncated Ok" promise as the mid-stream error paths.
    // When NOTHING arrived it is the other broken-stream shape #2686 names —
    // a gateway answering 200 with an empty stream — and is fallback-eligible:
    // the retry loses nothing by going out unary.
    if !completed_seen {
        return Err(StreamFault {
            fallback_eligible: !anything_arrived,
            error: http::attach_partial(
                http::stream_ended_before_terminal("OpenAI", "response.completed"),
                &usage,
                &text,
                pricing,
            ),
        });
    }

    let tool_calls = tool_calls
        .into_values()
        .map(|acc| ToolCall {
            call_id: acc.call_id,
            name: acc.name,
            input: tool_call_input(&acc.arguments),
        })
        .collect();

    Ok(OpenAiStreamOutcome {
        text,
        tool_calls,
        usage,
        truncated_at_limit,
    })
}
