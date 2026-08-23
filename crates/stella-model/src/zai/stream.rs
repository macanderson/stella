//! The streaming half of the shared OpenAI-compatible adapter: SSE
//! aggregation for one completion, split out of `zai.rs` (a grandfathered
//! god file closed to growth) when the streaming→non-streaming fallback
//! (#2686) landed.
//!
//! [`aggregate_zai_stream`] consumes one `chat/completions` SSE body into
//! the adapter's result tuple. Its error type is [`StreamFault`] rather than
//! a bare `ProviderError`, because the caller needs one extra bit: whether
//! the fault is **fallback-eligible** — the stream hung before its first
//! byte, or ended with nothing accumulated at all — which is what arms the
//! [`crate::stream_recovery::StreamRecovery`] latch so the retry of this
//! attempt goes out as a unary request. Every other failure (a mid-stream
//! death with content already salvaged, a malformed frame, an in-band error
//! frame) keeps its existing classification and never arms the latch.
//!
//! The per-call assembly rules the streaming and unary
//! ([`super::unary`]) paths must agree on — argument-JSON repair, the
//! reasoning-only text promotion, usage folding, the final finish reason —
//! live here as shared helpers so the two paths cannot drift.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;
use stella_protocol::{CompletionUsage, FinishReason, ProviderError, ToolCall};

use super::{ZaiUsage, classify_zai_stream_error};
use crate::catalog::Pricing;
use crate::http;
use crate::provider::ToolCallObserver;
use crate::sse::SseDecoder;
use crate::stream_recovery::StreamFault;

/// The stream chunk shape, deserialized per SSE `data:` frame — see the
/// field docs in `zai.rs` for the dialect quirks each one absorbs.
use super::ZaiStreamChunk;

/// Fold one usage frame into the normalized envelope. Shared verbatim by
/// the streaming aggregator (final usage frame) and the unary fallback
/// (response-level `usage` object), so the two paths cannot disagree about
/// how this dialect's cache/reasoning telemetry is read.
pub(super) fn fold_usage(
    frame: ZaiUsage,
    usage: &mut CompletionUsage,
    reported_cost_usd: &mut Option<f64>,
) {
    usage.input_tokens = frame.prompt_tokens;
    usage.output_tokens = frame.completion_tokens;
    let details = frame.prompt_tokens_details.unwrap_or_default();
    // Two wire spellings for the same fact: the OpenAI-compatible
    // details object, or DeepSeek's native top-level field — take
    // whichever the server spoke (never both on one endpoint).
    usage.cached_input_tokens = if details.cached_tokens > 0 {
        details.cached_tokens
    } else {
        frame.prompt_cache_hit_tokens
    };
    usage.cache_write_tokens = details.cache_write_tokens;
    // Only overwrite when this frame carried the breakdown: a
    // later usage frame without the detail object must not erase
    // a count an earlier one reported.
    if let Some(reasoning) = frame
        .completion_tokens_details
        .and_then(|d| d.reasoning_tokens)
    {
        usage.reasoning_tokens = Some(reasoning);
    }
    if frame.cost.is_some() {
        *reported_cost_usd = frame.cost;
    }
}

/// Parse one completed tool call's accumulated argument string under this
/// dialect's repair rules — shared by both delivery paths.
///
/// An empty body is an empty *object*, not null — a downstream tool
/// deserializing its input as an object must not be handed `null`. When the
/// token limit cut the call (`truncated`), executing it anyway would fail on
/// missing parameters and re-enter the same unwinnable retry-retruncate
/// loop as a mid-payload cut, so it is a terminal error either way the cut
/// landed. A *non-truncated* body that fails to parse is the model's own
/// broken JSON (GLM emits these): fall back to the `Value::Null` sentinel
/// `driver.rs::execute_with_repair` checks for, so the repair loop can ask
/// the model to retry. Mirrors anthropic.rs.
pub(super) fn tool_call_input(
    label: &str,
    name: &str,
    raw: &str,
    truncated: bool,
) -> Result<Value, ProviderError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        if truncated {
            return Err(http::truncated_tool_input_error(
                label,
                name,
                "",
                "finish_reason=length",
            ));
        }
        return Ok(Value::Object(serde_json::Map::new()));
    }
    match serde_json::from_str(trimmed) {
        Ok(value) => Ok(value),
        Err(_) if truncated => Err(http::truncated_tool_input_error(
            label,
            name,
            trimmed,
            "finish_reason=length",
        )),
        Err(_) => Ok(Value::Null),
    }
}

/// Reasoning-only fallback: whether to surface the chain-of-thought as the
/// answer text so the turn is never blank. Shared by both delivery paths.
///
/// `calls.is_empty()` is required, not defensive. A tool-calling turn
/// legitimately has empty `content` — that is the *normal* shape for every
/// Anthropic model routed through OpenRouter, which streams `content: ""`
/// alongside `reasoning` and the tool call. Without this guard the fallback
/// fired on essentially every reasoning-model tool turn and published the
/// model's private chain-of-thought as its user-visible answer. A turn that
/// called a tool is not blank, so it never needs the fallback.
/// `!truncated` is the second required guard. "Otherwise blank" holds
/// for a model that FINISHED thinking and wrote no answer, not one CUT OFF
/// mid-thought — `stella_core::driver::truncation` handles that case and can
/// only recognize it while `text` is still empty. Promoting anyway published
/// an abandoned scratchpad as the answer.
pub(super) fn promote_reasoning_as_text(
    text: &str,
    calls: &[ToolCall],
    reasoning: &str,
    truncated: bool,
) -> bool {
    text.trim().is_empty() && calls.is_empty() && !reasoning.trim().is_empty() && !truncated
}

/// The finish reason both delivery paths report from the same two facts.
pub(super) fn final_finish_reason(truncated: bool, has_calls: bool) -> Option<FinishReason> {
    if truncated {
        Some(FinishReason::Length)
    } else if has_calls {
        Some(FinishReason::ToolCalls)
    } else {
        Some(FinishReason::Stop)
    }
}

/// Accumulator for one in-progress streamed tool call, keyed by the
/// provider's `index` field until it's complete.
#[derive(Default)]
struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
    /// Whether this call was already announced to a [`ToolCallObserver`].
    /// OpenAI-style tool calls stream sequentially by index, so a call is
    /// complete the moment a HIGHER index appears — that boundary announces
    /// it exactly once.
    ///
    /// The stream's LAST call has no higher index behind it, so its boundary
    /// is the chunk that carries `finish_reason: "tool_calls"` instead —
    /// which arrives before the final usage frame and `[DONE]`, with an
    /// end-of-stream fallback for a server that jumps straight to `[DONE]`.
    /// Without that boundary a turn that makes exactly one tool call — the
    /// common shape for this agent — was never announced at all, and
    /// `stella-core`'s speculative execution never fired for any single-call
    /// turn on this dialect. The window is smaller than what the dialects
    /// with a per-call terminator get (`response.function_call_arguments.done`
    /// on `openai.rs`, `content_block_stop` on `anthropic.rs`, whole
    /// `functionCall` parts on `gemini.rs`), but not empty. This flag is what
    /// keeps every boundary exactly-once: a call announced at one boundary is
    /// skipped at every later one.
    announced: bool,
}

/// Announce every un-announced accumulator below `next_index` to the
/// observer. Only calls whose arguments already parse are announced — a
/// call the end-of-stream assembly would hand the `Null` repair sentinel
/// must never reach speculative execution. Announced calls re-parse the
/// same bytes at final assembly, so an announced call and its committed
/// twin are structurally identical.
fn announce_completed_below(
    observer: &dyn ToolCallObserver,
    tool_calls: &mut BTreeMap<usize, ToolCallAccumulator>,
    next_index: usize,
) {
    for (_, acc) in tool_calls.range_mut(..next_index) {
        if acc.announced {
            continue;
        }
        acc.announced = true;
        if acc.id.is_empty() {
            continue;
        }
        let trimmed = acc.arguments.trim();
        let input = if trimmed.is_empty() {
            Some(Value::Object(serde_json::Map::new()))
        } else {
            serde_json::from_str(trimmed).ok()
        };
        if let Some(input) = input {
            observer.tool_call_streamed(&ToolCall {
                call_id: acc.id.clone(),
                name: acc.name.clone(),
                input,
            });
        }
    }
}

/// Everything one assembled SSE body yields. A named struct rather than the
/// tuple this used to return: the fifth and sixth members are both optional
/// gateway-only metadata, and at that width a positional result invites the
/// exact mix-up it cannot catch.
pub(super) struct ZaiStreamOutcome {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: CompletionUsage,
    pub finish_reason: Option<FinishReason>,
    /// The gateway's own cost accounting, authoritative over list pricing.
    pub reported_cost_usd: Option<f64>,
    /// The upstream the gateway routed to, when it names one.
    pub upstream_provider: Option<String>,
}

pub(super) async fn aggregate_zai_stream(
    response: reqwest::Response,
    label: &str,
    observer: Option<&dyn ToolCallObserver>,
    pricing: Option<&Pricing>,
    first_byte: Duration,
) -> Result<ZaiStreamOutcome, StreamFault> {
    let mut decoder = SseDecoder::new();
    let mut text = String::new();
    // Chain-of-thought streamed under `reasoning_content`, kept separate from
    // the answer. Used only as a fallback when `content` never arrives, so a
    // reasoning-only turn is visible instead of blank.
    let mut reasoning = String::new();
    let mut usage = CompletionUsage::default();
    let mut tool_calls: BTreeMap<usize, ToolCallAccumulator> = BTreeMap::new();
    // Set once any choice reports `finish_reason: "length"` — the output was
    // cut off at the token limit, so a tool call whose argument JSON didn't
    // finish streaming is truncated, not merely malformed.
    let mut truncated_at_token_limit = false;
    // Gateway-reported per-call cost (OpenRouter usage accounting), from the
    // final usage frame. `None` when the endpoint doesn't report one.
    let mut reported_cost_usd: Option<f64> = None;
    let mut usage_seen = false;
    let mut upstream_provider: Option<String> = None;
    let mut terminal_seen = false;
    // The first body read runs against the (shorter) first-byte deadline
    // rather than the inter-fragment idle bound: a response that has sent
    // its headers and then not one body byte is a buffering proxy, not a
    // thinking model (#2686). Any chunk at all — even a keep-alive — moves
    // the stream onto the ordinary idle clock.
    let mut awaiting_first_chunk = true;
    let mut stream = response.bytes_stream();

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
                    http::hung_before_first_byte(label, idle)
                } else {
                    ProviderError::transport(format!(
                        "stream idle timeout: no data for {}s",
                        idle.as_secs()
                    ))
                };
                // A hang is fallback-eligible exactly when there is nothing
                // to lose: no content, no usage frame, no terminal marker.
                // A stream that hung after real content is the existing
                // mid-stream-death case — retried as a stream, its salvage
                // attached.
                let fallback_eligible = !terminal_seen
                    && !usage_seen
                    && text.is_empty()
                    && reasoning.is_empty()
                    && tool_calls.is_empty();
                return Err(StreamFault {
                    fallback_eligible,
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
            if data == "[DONE]" {
                terminal_seen = true;
                continue;
            }
            // Two failure modes hide behind one `from_str`, and they must not
            // share a branch. A frame that is not JSON at all is a keep-alive
            // or a gateway comment: ignoring it is correct. A frame that IS
            // valid JSON but does not fit the chunk shape is a dialect
            // deviation, and swallowing it loses whatever it carried. Since
            // every field in this tree defaults, an unknown or empty object
            // still parses — so reaching the error arm means a real type
            // mismatch on a field that matters, and the likeliest casualty is
            // `tool_calls`. Dropping that produces a turn with no calls, which
            // the driver reads as a clean completion: the run reports success
            // having done nothing. The `error` field above documents this exact
            // failure happening once already. Fail loudly instead.
            let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
                continue; // not JSON: keep-alive / ping / gateway comment
            };
            let parsed: ZaiStreamChunk = match serde_json::from_value(value) {
                Ok(v) => v,
                Err(e) => {
                    return Err(ProviderError::Malformed(format!(
                        "{label}: unparseable stream frame ({e}); refusing to \
                         treat a dropped frame as an empty turn"
                    ))
                    .into());
                }
            };
            // A mid-stream error frame aborts the turn with a typed error —
            // never a truncated Ok with the partial text seen so far. It is a
            // loss site like the hang above and carries the same salvage: a
            // usage frame that already arrived is what the provider bills,
            // whether or not the stream then died (#3859).
            if let Some(err) = &parsed.error {
                let error = classify_zai_stream_error(err, label);
                return Err(http::attach_partial(error, &usage, &text, pricing).into());
            }
            if let Some(u) = parsed.usage {
                usage_seen = true;
                fold_usage(u, &mut usage, &mut reported_cost_usd);
            }
            // The gateway repeats its upstream on every chunk; first one wins,
            // so a stream cut before its usage frame still records who served
            // it. Kept out of `fold_usage` deliberately — that folds the usage
            // envelope, and this rides the chunk itself.
            if upstream_provider.is_none() {
                upstream_provider = parsed.provider;
            }
            for choice in parsed.choices {
                if choice.finish_reason.as_deref() == Some("length") {
                    truncated_at_token_limit = true;
                }
                if let Some(content) = choice.delta.content {
                    // `content` is the user-visible answer; thinking rides
                    // `reasoning_delta` below. The two channels stay separate
                    // all the way to the transcript.
                    if let Some(observer) = observer {
                        observer.text_delta(&content);
                    }
                    text.push_str(&content);
                }
                // GLM's name for chain-of-thought and OpenRouter's normalized
                // one. Both announce as thinking, so a reasoning model's
                // deliberation is visible live (collapsed, dimmed) instead of
                // the turn looking idle until the answer lands.
                if let Some(rc) = choice.delta.reasoning_content {
                    if let Some(observer) = observer {
                        observer.reasoning_delta(&rc);
                    }
                    reasoning.push_str(&rc);
                }
                if let Some(r) = choice.delta.reasoning {
                    if let Some(observer) = observer {
                        observer.reasoning_delta(&r);
                    }
                    reasoning.push_str(&r);
                }
                for (position, tc_delta) in choice
                    .delta
                    .tool_calls
                    .unwrap_or_default()
                    .into_iter()
                    .enumerate()
                {
                    // Absent `index` falls back to the fragment's position in
                    // this chunk — see `ZaiStreamToolCallDelta`.
                    let index = tc_delta.index.unwrap_or(position);
                    // A delta for index N proves every lower index finished
                    // streaming (the dialect emits calls sequentially) —
                    // the moment those calls can be announced for
                    // speculative execution.
                    if let Some(observer) = observer {
                        announce_completed_below(observer, &mut tool_calls, index);
                    }
                    let acc = tool_calls.entry(index).or_default();
                    if let Some(id) = tc_delta.id {
                        acc.id = id;
                    }
                    if let Some(function) = tc_delta.function {
                        if let Some(name) = function.name {
                            // Accumulated, not assigned: the dialect contract
                            // (pinned by
                            // `complete_reassembles_a_streamed_tool_call_split_across_many_chunks`)
                            // allows the name to arrive in fragments like the
                            // arguments do, so append is the correct
                            // reassembly. A gateway that instead repeats the
                            // WHOLE name per fragment would garble this — no
                            // such gateway has been observed; if one appears,
                            // dedupe the exact-repeat case rather than
                            // switching to last-wins.
                            acc.name.push_str(&name);
                        }
                        if let Some(args) = function.arguments {
                            // Liveness only (see
                            // `ToolCallObserver::tool_input_delta`): a
                            // call-only generation must still register as
                            // producing against the idle deadline.
                            if let Some(observer) = observer {
                                observer.tool_input_delta();
                            }
                            acc.arguments.push_str(&args);
                        }
                    }
                }
                // The chunk carrying `finish_reason: "tool_calls"` is this
                // dialect's end-of-calls terminator: the LAST call has no
                // higher index behind it, so this is its completion boundary.
                // Announcing here — before the final usage frame and `[DONE]`
                // — is what opens the speculative-execution window for a
                // single-call turn; `announced` keeps calls already announced
                // at an index boundary from repeating.
                if choice.finish_reason.as_deref() == Some("tool_calls")
                    && let Some(observer) = observer
                {
                    announce_completed_below(observer, &mut tool_calls, usize::MAX);
                }
            }
        }
    }

    // EOF without the `[DONE]` sentinel is a disconnect, not a completion —
    // whatever accumulated is a half-answer, and even a `finish_reason` seen
    // earlier can't prove the stream wasn't cut after it. Retryable
    // Transport, upholding the same "never a truncated Ok" promise as the
    // mid-stream error-frame path above. When NOTHING accumulated it is the
    // other broken-stream shape #2686 names — a gateway answering 200 with
    // an empty stream — and is fallback-eligible: the retry loses nothing by
    // going out unary.
    if !terminal_seen {
        let fallback_eligible =
            !usage_seen && text.is_empty() && reasoning.is_empty() && tool_calls.is_empty();
        return Err(StreamFault {
            fallback_eligible,
            error: http::attach_partial(
                http::stream_ended_before_terminal(label, "[DONE]"),
                &usage,
                &text,
                pricing,
            ),
        });
    }

    // Fallback terminator: a server that ends the stream without ever sending
    // a `finish_reason: "tool_calls"` chunk still completed its last call —
    // `[DONE]` proves the stream is whole, so announce whatever is still open
    // before final assembly (`announced` makes this a no-op when the finish
    // chunk already fired). Skipped when the token limit cut the stream: a
    // truncated call must never reach speculative execution, and final
    // assembly below turns it into a terminal error instead.
    if !truncated_at_token_limit && let Some(observer) = observer {
        announce_completed_below(observer, &mut tool_calls, usize::MAX);
    }

    usage.reported = usage_seen;

    // OpenAI-style tool calls stream sequentially by index, so when the
    // stream reports `finish_reason: "length"` only the highest-index call
    // can be the one the token limit cut. Pinning truncation there keeps the
    // blame on the right call — an earlier call whose JSON is broken is the
    // model's own malformed output and still gets the repair sentinel.
    let truncated_index = if truncated_at_token_limit {
        tool_calls.keys().next_back().copied()
    } else {
        None
    };

    let mut calls = Vec::with_capacity(tool_calls.len());
    for (index, acc) in tool_calls {
        let truncated = Some(index) == truncated_index;
        let input = tool_call_input(label, &acc.name, &acc.arguments, truncated)?;
        calls.push(ToolCall {
            call_id: acc.id,
            name: acc.name,
            input,
        });
    }

    if promote_reasoning_as_text(&text, &calls, &reasoning, truncated_at_token_limit) {
        text = reasoning;
    }

    let finish_reason = final_finish_reason(truncated_at_token_limit, !calls.is_empty());

    Ok(ZaiStreamOutcome {
        text,
        tool_calls: calls,
        usage,
        finish_reason,
        reported_cost_usd,
        upstream_provider,
    })
}
