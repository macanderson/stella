//! Event/telemetry persistence and execution closeout.

use super::*;
use stella_core::ports::Clock;

use crate::runtime::WallClock;

/// Build the session's token-drift calibration, seeded from prior sessions'
/// telemetry for the resolved provider/model (`Store::drift_samples`) so the
/// estimator starts already corrected. Best-effort like all persistence: no
/// store (or a failed query) means starting uncalibrated — factor 1.0. The
/// seeding lives in [`stella_runtime::seed_calibration`]; this wrapper only
/// unwraps the pin out of a [`Config`], which the runtime crate deliberately
/// does not know about.
pub(crate) fn seed_calibration(store: &Option<Arc<Store>>, cfg: &Config) -> CalibrationMap {
    stella_runtime::seed_calibration(store.as_ref(), cfg.provider.id, &cfg.model_id)
}

/// The id `executions.pipeline_variant` records for a run of the built-in
/// staged pipeline (#3388/#3381). Was re-exported from
/// `stella_pipeline::variant::CLASSIC_VARIANT_ID`; that crate is gone now
/// (#3865) and no live path writes a new row with this variant. The literal
/// survives as a pure historical join key so every already-written
/// `executions.pipeline_variant = 'classic'` row (a plain `TEXT` column, no
/// FK) stays queryable — do not resurrect the re-export if `stella-pipeline`
/// returns; this string belongs beside the rows it names, not that crate.
pub(crate) const PIPELINE_VARIANT_CLASSIC: &str = "classic";

mod reasoning_run;
mod turn_door;

pub(crate) use reasoning_run::{Owed as ReasoningOwed, ReasoningRun};
pub(crate) use turn_door::TurnDoor;

/// Begin an execution record; a failure degrades to "no persistence for this
/// execution" rather than blocking the work.
///
/// `kind` is the **door** — which command the user ran — and nothing else.
/// Which wrapper ran, if any, is `variant`: two separate facts in two
/// separate columns, because they used to share one and a deck turn's door
/// changed depending on whether the pipeline was on (#3388).
pub(crate) fn begin_execution(
    store: &Option<Arc<Store>>,
    kind: &str,
    prompt: &str,
    cfg: &Config,
    session: Option<&str>,
    variant: Option<&str>,
) -> Option<(Arc<Store>, i64)> {
    let store = store.as_ref()?;
    match store.begin_execution(kind, prompt, cfg.provider.id, &cfg.model_id) {
        Ok(id) => {
            // Best-effort like the session link below: an unrecorded variant
            // costs a row in a comparison, never the turn.
            if let Some(variant) = variant {
                let _ = store.set_pipeline_variant(id, variant);
            }
            // Link the execution to its session (store schema v8) — what
            // lets the deck's SESSIONS overlay reassemble and replay the
            // session's full journal later. Best-effort like every other
            // store write: a failed link degrades replay, never the turn.
            if let Some(session) = session {
                let _ = store.set_execution_session(id, session);
            }
            Some((store.clone(), id))
        }
        Err(_) => None,
    }
}

/// Emit the run's ending — the single terminator of one run's event stream
/// (#3379, #3398).
///
/// The engine says when its *turn* is over ([`AgentEvent::TurnComplete`]); it
/// does not know whether anything will ask it for another one, and it must not
/// have to. The *run's* ending therefore belongs to whoever owns the run, and
/// "owns the run" has one operational meaning: **the code that created this
/// event stream and will close it.**
///
/// That rule is why no wrapper emits this. The staged pipeline used to, and on
/// `stella goal` — which drives one pipeline run per round over a single
/// stream — that produced one terminator per round, several per run, which
/// `replay::validate_terminal` calls a violation. A wrapper cannot know
/// whether it is the whole run; the stream's owner always can.
///
/// # Nothing is emitted for a run that failed
///
/// A failed run already crossed the stream as `Error`, and a run must never
/// end on both.
pub(crate) fn emit_run_complete(tx: &stella_core::EventSender, model: &str, cost_usd: f64) {
    let _ = tx.send(AgentEvent::RunComplete {
        model: model.to_string(),
        cost_usd,
    });
}

/// [`emit_run_complete`] for a run whose ending is one engine turn's outcome:
/// emitted on a completed turn, withheld on an abort (which already sent
/// `Error`).
pub(crate) fn emit_run_complete_for_turn(
    tx: &stella_core::EventSender,
    model: &str,
    outcome: &TurnOutcome,
) {
    if let TurnOutcome::Completed { cost_usd, .. } = outcome {
        emit_run_complete(tx, model, *cost_usd);
    }
}

/// [`emit_run_complete`] for a caller holding the raw channel sender rather
/// than an [`stella_core::EventSender`] — the goal loops, whose ending is the
/// whole loop's cost rather than one turn's outcome.
pub(crate) fn emit_run_complete_on_raw(
    tx: &mpsc::UnboundedSender<AgentEvent>,
    model: &str,
    cost_usd: f64,
) {
    emit_run_complete(&stella_core::EventSender::new(tx.clone()), model, cost_usd);
}

// `emit_run_complete_raw` — the turn-outcome sibling of the above — is
// deliberately gone. It let a driver holding a raw sender pay the terminator
// half of the turn boundary without the tree measurement that rides beside it,
// and the Command Deck did exactly that for as long as both existed: every
// session's Files tab read `no files touched yet` however many files its turns
// wrote. `turn_files::close_turn_boundary_raw` is the replacement, and it pays
// both. Restoring a terminator-only raw helper reopens that hole.

#[derive(Default)]
pub(crate) struct RendererOutcome {
    pub(crate) events: Vec<AgentEvent>,
    pub(crate) persistence_complete: bool,
}

/// End the turn's event stream and drain the renderer.
///
/// The order matters and is the whole point. `drop(tx)` on its own never
/// closed the channel: `attach_events` and `bridge_policy_plane` handed the
/// registry `EventSender` clones, and the registry outlives the turn — so
/// `recv()` stayed pending forever and a completed `stella run` printed its
/// terminal event and then hung until it was killed (#960). Every sender has
/// to go, the registry's included, before the renderer can finish.
///
/// Awaiting it here (rather than detaching and returning) keeps the guarantee
/// the await was there for: every queued event has actually been rendered and
/// persisted before the caller moves on to its close-out.
pub(crate) async fn close_event_stream(
    registry: &ToolRegistry,
    tx: stella_core::EventSender,
    renderer: tokio::task::JoinHandle<RendererOutcome>,
) -> RendererOutcome {
    registry.detach_event_stream();
    drop(tx);
    renderer.await.unwrap_or_default()
}

/// Attach a run's two registry-born event streams to the engine's own stream.
///
/// The policy/extension audit plane (receipts spec §6.4 — a no-op unless a hook
/// bus is attached) and the registry's own events (task board, sub-agent
/// lifecycle) both ride the stream the engine writes to, so a run's live output
/// and its journal agree. The two are always attached together: a run that
/// journalled one and not the other would render a transcript that disagrees
/// with its own replay. Both of `agent.rs`'s run paths had carried a verbatim
/// copy of this pairing and of the comment explaining it.
///
/// The registry half — the events and this turn's per-call work-tree
/// measurement (#4175) — is delegated to `turn_files::open_turn_streams`
/// rather than restated, so the engine path and the deck's lead turn cannot
/// open a stream with different debts paid. This function is what the engine
/// path adds on top: the policy/extension audit bridge.
pub(crate) fn attach_run_streams(
    registry: &ToolRegistry,
    cfg: &crate::config::Config,
    tx: &stella_core::EventSender,
    execution: Option<&(Arc<Store>, i64)>,
) {
    registry.bridge_policy_plane(tx.clone());
    crate::turn_files::open_turn_streams(registry, cfg, tx, execution);
}

/// `durable_pre_persisted` is set when [`super::output::event_sender_for_run`]
/// already appended every event to Harbor's durable JSONL sink before admitting
/// it here. The line then only needs publishing to stdout — re-appending would
/// double the evidence record the benchmark harness audits.
pub(crate) fn spawn_renderer(
    mut rx: mpsc::UnboundedReceiver<AgentEvent>,
    format: OutputFormat,
    execution: Option<(Arc<Store>, i64)>,
    provider_id: String,
    durable_pre_persisted: bool,
    // The turn's prompt, when the caller has it. It is the anchor every row of
    // a rendered frame is read against, so a caller that knows it should say
    // so; `None` simply renders a frame with no `you` row.
    prompt: Option<String>,
) -> tokio::task::JoinHandle<RendererOutcome> {
    tokio::spawn(async move {
        let mut tool_names: HashMap<String, String> = HashMap::new();
        // Shared with every blocking persistence hop below, so the provider id
        // is not re-allocated once per persisted event.
        let provider_id: Arc<str> = provider_id.into();
        let mut outcome = RendererOutcome {
            events: Vec::new(),
            persistence_complete: true,
        };
        let mut seq = 0u64;
        // The open run of streamed reasoning fragments, joined into one row
        // per thought rather than one per few tokens (#3969).
        let mut reasoning = ReasoningRun::default();
        // Split per condition, for the same reason the deck's forwarder splits
        // them: a benign usage gap must not silence a later store fault.
        let mut usage_warned = false;
        let mut store_warned = false;
        let mut stream_terminal = None;
        // The diagnostic timeline (docs/spec/diagnostics.md §8). Built on the
        // RECEIVING side: a bridge holding an `EventSender` clone would keep the
        // channel open and hang close-out, which is the whole point of
        // `close_event_stream` above.
        let mut bridge = crate::diag_bridge::DomainBridge::new(
            crate::diag_boot::dx(),
            Some(crate::diag_boot::workspace_root()),
        );
        // The stream-json sink's clock. Wall-anchored, not `SystemClock`: a
        // journal stamp has to stay comparable across processes and runs, and a
        // per-construction origin is exactly the wrong shape for that (#2111).
        let clock = WallClock;
        // The transcript frame is this surface's default rendering (#3578).
        // `STELLA_PLAIN_STREAM=1` opts back into the line-by-line cards for a
        // caller that wants tokens as they arrive rather than a finished turn.
        let mut transcript = (format == OutputFormat::Text
            && !plain::transcript::TranscriptPrinter::streaming_requested())
        .then(|| {
            let mut printer =
                plain::transcript::TranscriptPrinter::new(String::new(), provider_id.to_string());
            if let Some(prompt) = &prompt {
                printer.start_turn(prompt);
            }
            printer
        });
        while let Some(event) = rx.recv().await {
            bridge.observe(&event);
            let event = if format == OutputFormat::StreamJson {
                let Some(event) = defer_stream_terminal(&mut stream_terminal, event) else {
                    continue;
                };
                event
            } else {
                event
            };
            let preview = matches!(event, AgentEvent::TextDelta { .. });
            let event = if let Some((store, id)) = &execution
                && !preview
            {
                // Fold a streamed reasoning fragment into the open run rather
                // than spending a row, a `seq` and a blocking hop on each one:
                // `Reasoning` IS the delta, so one page of thought used to
                // arrive as tens of thousands of single-transaction INSERTs
                // (#3969). Everything else closes the run and writes it out
                // ahead of itself, which is what keeps the table in stream
                // order. `TextDelta` never reaches here at all — `preview`
                // drops it above — so a preview interleaved with thinking
                // cannot tear a run into per-fragment rows.
                let owed = reasoning.admit(&event);
                // Buffered, and the event still falls through to rendering
                // below: only the DURABLE write coalesces, so stream-json
                // keeps emitting fragments live at the token rate.
                if owed.is_empty() {
                    event
                } else {
                    // `record_event` is a synchronous `rusqlite` write and this is a
                    // Tokio worker thread. At the default durability the write costs
                    // 37 microseconds (`stella_store::migrations::Durability`), which
                    // inline would be unremarkable — but `busy_timeout` is five
                    // seconds, and that pragma exists precisely because a second
                    // same-workspace session contending for the write lock is an
                    // expected condition rather than an exotic one. Run inline, the
                    // worst case is therefore a five-second stalled reactor rather
                    // than one slow write, and `paranoid` durability turns the
                    // ordinary case into 4 ms an event as well. Both belong on the
                    // blocking pool.
                    //
                    // The event moves in and back out rather than being cloned: it is
                    // still needed for rendering below, and a `ToolResult` payload is
                    // exactly the event least worth copying.
                    //
                    // The feeding channel stays unbounded on purpose. `EventSender`
                    // is synchronous by construction (invariant #2 keeps
                    // `stella-core` I/O-free), so a bounded channel could only
                    // `try_send` — dropping telemetry — or block the engine thread,
                    // which is strictly worse than the stall this hop removes.
                    let store = Arc::clone(store);
                    let id = *id;
                    let provider_id = Arc::clone(&provider_id);
                    // How far `seq` advances if the task dies with its writes in an
                    // unknown state. Over-advancing leaves a hole, which every
                    // reader tolerates (`after_seq` asks for `seq > cursor`);
                    // under-advancing would collide with a row that did land and
                    // lose the NEXT event to `UNIQUE (execution_id, seq)`.
                    let owed_writes =
                        u64::from(owed.block.is_some()) + u64::from(owed.writes_arriving);
                    let joined = tokio::task::spawn_blocking(move || {
                        let (persisted, next) =
                            persist_owed(&store, id, seq, owed, &event, &provider_id);
                        (event, persisted, next)
                    })
                    .await;
                    let (event, persisted, next_seq) = match joined {
                        Ok(triple) => triple,
                        // The blocking pool only fails here if `persist_owed`
                        // panicked or the runtime is shutting down. The event went with
                        // the task either way, so the turn loses one rendered line —
                        // what it must not lose is the admission that persistence is no
                        // longer complete.
                        Err(_) => {
                            outcome.persistence_complete = false;
                            seq += owed_writes;
                            continue;
                        }
                    };
                    seq = next_seq;
                    if !persisted.is_complete() {
                        outcome.persistence_complete = false;
                        // This path used to print "store write failed" for BOTH
                        // conditions, so a dropped model stream on `stella run`
                        // accused a database that was fine — the same mislabel the
                        // deck had. Name what actually happened, once per
                        // condition.
                        let (warned, scope) = match persisted {
                            PersistOutcome::StoreWriteFailed => {
                                (&mut store_warned, "this execution")
                            }
                            _ => (&mut usage_warned, "one model call"),
                        };
                        if !*warned && let Some(message) = persisted.message(scope) {
                            *warned = true;
                            eprintln!("  {} {message}", "⚠".yellow());
                        }
                    }
                    event
                }
            } else {
                event
            };
            match format {
                // One line per event — the stable machine interface.
                // Serialization of a protocol enum never fails; if it somehow
                // does, terminate before the provider loop can spend on a
                // later unmetered call.
                OutputFormat::StreamJson => emit_stream_json(&event, durable_pre_persisted, &clock),
                OutputFormat::Json => outcome.events.push(event),
                // One frame per turn, folded from the same events the cards
                // below would each have printed. The two cannot both run: the
                // frame restates every row, so a surface doing both prints the
                // turn twice.
                OutputFormat::Text if transcript.is_some() => {
                    if let AgentEvent::ToolStart { call, .. } = &event {
                        tool_names.insert(call.call_id.clone(), call.name.clone());
                    }
                    if let Some(printer) = transcript.as_mut() {
                        printer.observe(&event);
                    }
                }
                OutputFormat::Text => match &event {
                    AgentEvent::ToolStart { call, .. } => {
                        tool_names.insert(call.call_id.clone(), call.name.clone());
                        plain::tool_call_card(&call.name, &call.input, "running");
                    }
                    AgentEvent::ToolResult {
                        call_id,
                        output,
                        duration_ms,
                        ..
                    } => {
                        let name = tool_names
                            .get(call_id)
                            .map(String::as_str)
                            .unwrap_or("tool");
                        let content = match output {
                            ToolOutput::Ok { content, .. } => content.clone(),
                            ToolOutput::Error { message, .. } => message.clone(),
                        };
                        plain::tool_result_card(
                            name,
                            &content,
                            output.is_error(),
                            Duration::from_millis(*duration_ms),
                        );
                    }
                    other => plain::render_event(other),
                },
            }
        }
        // The stream is closed, so the turn is final: print whatever frame the
        // fold accumulated. A turn that never opened prints nothing.
        if let Some(printer) = transcript.as_mut() {
            printer.flush();
        }
        // The open reasoning run is final too, and it lands BEFORE the held
        // terminator below — so a turn interrupted mid-thought keeps the
        // reasoning it had already streamed, in the order it streamed it.
        if !flush_reasoning_tail(&mut reasoning, &execution, &provider_id, &mut seq).await {
            outcome.persistence_complete = false;
        }
        // `RunComplete` is the protocol terminator, not ordinary narration.
        // Hold it until every later accounting/reflection event has drained,
        // and persist/print exactly one terminal frame as the final stream
        // item. Engine `TurnComplete` events pass through inline (#3379).
        if let Some(event) = stream_terminal {
            if let Some((store, id)) = &execution
                && !persist_event(store, *id, seq, &event, &provider_id)
            {
                outcome.persistence_complete = false;
            }
            emit_stream_json(&event, durable_pre_persisted, &clock);
        }
        // The stream is closed, so the bounded tally is final: one record
        // carrying the per-token counts that deliberately produced none of
        // their own (§3.8).
        bridge.finish();
        outcome
    })
}

/// Publish one stream-json line, honoring Harbor's durable sink. Failures are
/// terminal rather than a warning: a benchmark run whose evidence file is
/// incomplete must not keep spending on later calls.
///
/// The line is stamped with `clock`'s wall clock (#2111) — the instant *this*
/// sink admitted it. When the durable sender already persisted the event it
/// stamped its own instant a moment earlier; see `output::ordered_durable_event_sender`.
fn emit_stream_json(event: &AgentEvent, durable_pre_persisted: bool, clock: &dyn Clock) {
    match stella_protocol::stamped_line(event, clock.now_ms()) {
        Ok(line) if durable_pre_persisted => {
            emit_pre_persisted_stream_json_line_or_terminate(&line)
        }
        Ok(line) => emit_stream_json_line_or_terminate(&line),
        Err(error) => terminate_stream_json(&format!("stream-json serialization failed: {error}")),
    }
}

fn defer_stream_terminal(
    pending: &mut Option<AgentEvent>,
    event: AgentEvent,
) -> Option<AgentEvent> {
    if matches!(event, AgentEvent::RunComplete { .. }) {
        *pending = Some(event);
        None
    } else {
        Some(event)
    }
}

/// Close out one execution's audit record: drain the registry's agent-use
/// and MCP-usage ledgers (each already single-execution), settle the
/// outcome, and hand the finished row to the downstream projections.
pub(crate) fn record_execution_end(
    store: &Store,
    execution_id: i64,
    registry: &ToolRegistry,
    outcome_label: &str,
    cost_usd: f64,
    persistence_complete: bool,
) -> bool {
    let uses: Vec<stella_store::AgentUseRow> = registry
        .drain_agent_uses()
        .into_iter()
        .map(|u| stella_store::AgentUseRow {
            agent: u.agent,
            version: u.version,
            reason: u.reason,
            // The one place the two sibling crates' vocabularies meet:
            // `stella-tools` owns the enum, `stella-store` owns the stored
            // token, and neither depends on the other (#3822).
            kind: u.kind.as_str().to_string(),
        })
        .collect();
    let uses_ok = uses.is_empty() || store.record_agent_uses(execution_id, &uses).is_ok();
    let mcp_usage = mcp_usage_rows(registry);
    let mcp_usage_ok = store.record_mcp_usage(execution_id, &mcp_usage).is_ok();
    // Cancellation can race a provider response after dispatch. Even when all
    // local writes succeed, the provider-side usage envelope is unknowable and
    // the execution must never become exportable.
    let terminal_usage_known = outcome_label != "cancelled";
    let audit_complete = persistence_complete && uses_ok && mcp_usage_ok && terminal_usage_known;
    let finish_ok = store
        .finish_execution_accounted(execution_id, outcome_label, cost_usd, audit_complete)
        .is_ok();
    let _ = store.materialize_tool_calls(execution_id);
    let _ = store.finalize_execution_reflection(execution_id);
    let _ = store.sync_to_usage_default(execution_id);
    let _ = crate::enterprise_telemetry::enqueue_finalized_execution(store, execution_id);
    audit_complete && finish_ok
}

fn mcp_usage_rows(registry: &ToolRegistry) -> Vec<stella_store::McpUsageRow> {
    registry
        .take_mcp_usage()
        .into_iter()
        .map(|u| stella_store::McpUsageRow {
            server: u.server,
            tool: u.tool,
            reason: u.reason,
            called_at_ms: u.called_at_ms as i64,
        })
        .collect()
}

/// Serialize a serde enum (BlockKind / CacheZone / ModelCallRole) to its stable
/// snake_case tag for storage, falling back to `"unknown"` if it somehow does
/// not serialize to a string. Keeps the store string-typed while the wire
/// carries the real enum.
fn enum_tag<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

/// A compact, human-readable rendering of recovered token counts — the
/// difference between a warning a user can act on and one they can only worry
/// about. Cache reads are named separately because they are the cheap part: a
/// 14k-token prompt that was 12k cache hits is a very different number than
/// one that was not, and collapsing them hides that.
fn token_summary(partial: &stella_protocol::PartialUsage) -> String {
    let input = partial.usage.input_tokens;
    let cached = partial.usage.cached_input_tokens;
    let output = partial.usage.output_tokens;
    let qualifier = if partial.input_reported {
        "reported"
    } else {
        "estimated"
    };
    if cached > 0 {
        format!("{input} input ({cached} cached, {qualifier}) + {output} output tokens")
    } else {
        format!("{input} input ({qualifier}) + {output} output tokens")
    }
}

pub(crate) fn warn_store_write_failed(what: &str) {
    eprintln!(
        "  {} store write failed — {what} for this execution is incomplete",
        "⚠".yellow()
    );
}

/// Why an event's accounting came out incomplete — the distinction the old
/// bare `bool` collapsed.
///
/// `persist_event` folds two unrelated conditions together: whether our own
/// rows hit disk, and whether the *provider* reported a terminal usage frame.
/// A stream that dies mid-turn (a gateway 5xx, a cancelled call) leaves usage
/// unreported even though every INSERT succeeded — and the deck then told the
/// user "store write failed", sending them to look at a database that was
/// perfectly fine. Splitting them lets each surface say what actually
/// happened.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum PersistOutcome {
    /// Rows written and usage fully reported.
    Complete,
    /// An INSERT failed — a real persistence problem.
    StoreWriteFailed,
    /// Everything was written, but the provider never delivered final usage
    /// for one attempt, so that attempt's accounting is short.
    ///
    /// Carries whatever the adapter salvaged from the failure, because the
    /// difference between "we recovered 14k input tokens" and "we know
    /// nothing" is the difference between a footnote and a real gap — and the
    /// surface that renders this is the only place a user ever learns which
    /// one happened.
    UsageIncomplete {
        /// Accounting the adapter salvaged from the failure, if any reached
        /// us.
        partial: Option<stella_protocol::PartialUsage>,
        /// Why the attempt produced no usage frame — `None` when the call
        /// **settled** and only the provider's frame never arrived.
        ///
        /// Two unrelated conditions reach this variant, and collapsing them
        /// is what made the rendered sentence lie (#4147): an
        /// [`AgentEvent::StepUsage`] with `complete: false` is a call that
        /// did its work and simply went unbilled, while an
        /// [`AgentEvent::UsageIncomplete`] is an attempt that **died** and
        /// produced nothing. The engine already distinguishes them on the
        /// wire — this field is that distinction, kept instead of dropped.
        died: Option<stella_protocol::UsageIncompleteReason>,
    },
}

impl PersistOutcome {
    pub(crate) fn is_complete(self) -> bool {
        matches!(self, PersistOutcome::Complete)
    }

    /// The user-facing sentence for this outcome, or `None` when complete.
    ///
    /// `what` names the scope that actually failed. It is deliberately a
    /// *call*, not a session: the earlier wording said "accounting for this
    /// session is incomplete" for a single retried attempt, which reads as
    /// "your whole run's books are wrong" and sent users looking for damage
    /// that did not exist. One dropped attempt out of hundreds is a footnote,
    /// and the sentence should sound like one.
    pub(crate) fn message(self, what: &str) -> Option<String> {
        match self {
            PersistOutcome::Complete => None,
            PersistOutcome::StoreWriteFailed => {
                Some(format!("store write failed — {what} could not be written"))
            }
            PersistOutcome::UsageIncomplete {
                partial: Some(partial),
                ..
            } => Some(format!(
                "{what} dropped before its final usage frame — recovered {} \
                 (~${:.4}) as an estimate; every other call is accounted normally",
                token_summary(&partial),
                partial.cost_usd,
            )),
            // Deliberately not "failed": this call SETTLED without a provider
            // usage frame, so the work landed fine and only the accounting is
            // short. Asserting a failure that did not happen is the same class
            // of mistake as the session-wide wording this replaces.
            PersistOutcome::UsageIncomplete {
                partial: None,
                died: None,
            } => Some(format!(
                "{what} reported no final usage — that call's tokens and cost \
                 are unaccounted (the work itself is unaffected)"
            )),
            // ...and the mirror image, which the sentence above used to claim
            // too (#4147). This attempt DIED, so the reassurance is false: it
            // was printed directly above the abort rows reporting the very
            // failure it denied. Say what happened and stop there — the turn's
            // own error rows are what speak to the work.
            PersistOutcome::UsageIncomplete {
                partial: None,
                died: Some(reason),
            } => Some(format!(
                "{what} {} before reporting usage — that call's tokens and \
                 cost are unaccounted",
                match reason {
                    stella_protocol::UsageIncompleteReason::ProviderError => "failed",
                    stella_protocol::UsageIncompleteReason::Timeout => "timed out",
                    stella_protocol::UsageIncompleteReason::Cancelled => "was cancelled",
                }
            )),
        }
    }
}

/// Thin `bool` face of [`persist_event_detailed`], kept for call sites that
/// only care whether accounting was complete.
pub(crate) fn persist_event(
    store: &Store,
    execution_id: i64,
    seq: u64,
    event: &AgentEvent,
    legacy_provider_id: &str,
) -> bool {
    persist_event_detailed(store, execution_id, seq, event, legacy_provider_id).is_complete()
}

/// Persist everything one arriving event owes the store, in stream order: the
/// coalesced reasoning block that must land ahead of it, then the event
/// itself — skipped when the event was a fragment folded into an open run
/// (#3969). Returns the worst outcome of the writes it made, and the next free
/// `seq`.
///
/// A buffered run spends **one** `seq`, not one per fragment, which is the
/// whole saving: `seq` stays monotonic and unique per execution
/// (`UNIQUE (execution_id, seq)`), and no reader treats it as a count of
/// fragments — `journal::entries`' `after_seq` poll only ever asks for
/// `seq > cursor`.
///
/// `StoreWriteFailed` wins over a usage gap. It is the condition that points
/// at a broken database, and both drains split their warning latches precisely
/// so the benign one cannot silence it.
pub(crate) fn persist_owed(
    store: &Store,
    execution_id: i64,
    mut seq: u64,
    owed: ReasoningOwed,
    event: &AgentEvent,
    legacy_provider_id: &str,
) -> (PersistOutcome, u64) {
    let mut outcome = PersistOutcome::Complete;
    if let Some(block) = owed.block {
        if !persist_event(store, execution_id, seq, &block, legacy_provider_id) {
            outcome = PersistOutcome::StoreWriteFailed;
        }
        seq += 1;
    }
    if owed.writes_arriving {
        let arriving = persist_event_detailed(store, execution_id, seq, event, legacy_provider_id);
        if outcome.is_complete() {
            outcome = arriving;
        }
        seq += 1;
    }
    (outcome, seq)
}

/// Drain a closed stream's open reasoning run to the store — the close-out
/// both event drains owe [`ReasoningRun`], and what keeps an interrupted turn's
/// half-streamed thought (#3969).
///
/// Called once the channel has closed and *before* any held terminator is
/// written, so the tail lands in stream order rather than after the event that
/// ended the turn. Returns false when the write failed, which the caller must
/// carry into its `persistence_complete` bit like every other write.
///
/// The hop is `spawn_blocking` for the reason the in-loop one is: this is a
/// synchronous `rusqlite` write on a Tokio task, and a contended write can hold
/// the reactor for the full five-second `busy_timeout`.
pub(crate) async fn flush_reasoning_tail(
    run: &mut ReasoningRun,
    execution: &Option<(Arc<Store>, i64)>,
    provider_id: &Arc<str>,
    seq: &mut u64,
) -> bool {
    let Some((store, id)) = execution else {
        return true;
    };
    let Some(block) = run.close() else {
        return true;
    };
    let store = Arc::clone(store);
    let id = *id;
    let provider_id = Arc::clone(provider_id);
    let at = *seq;
    *seq += 1;
    tokio::task::spawn_blocking(move || persist_event(&store, id, at, &block, &provider_id))
        .await
        .unwrap_or(false)
}

pub(crate) fn persist_event_detailed(
    store: &Store,
    execution_id: i64,
    seq: u64,
    event: &AgentEvent,
    legacy_provider_id: &str,
) -> PersistOutcome {
    let recorded = store.record_event(execution_id, seq, event).is_ok();
    let mut telemetry_ok = true;
    let mut usage_complete = true;
    let mut recovered = None;
    let mut died = None;
    if let AgentEvent::StepUsage {
        role,
        provider,
        model,
        input_tokens,
        output_tokens,
        cached_input_tokens,
        cache_write_tokens,
        estimated_input_tokens,
        cost_usd,
        duration_ms,
        retries,
        tool_calls,
        complete,
        sub_agent_id,
        ..
    } = event
    {
        let actual_provider = if provider.is_empty() {
            legacy_provider_id
        } else {
            provider
        };
        telemetry_ok = store
            .record_telemetry(
                execution_id,
                &TelemetryRow {
                    // Event-stream seq is the execution-global call identity;
                    // engine-local `step` restarts on each pipeline turn.
                    step: seq,
                    provider: actual_provider.to_string(),
                    call_role: serde_json::to_value(role)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .unwrap_or_else(|| "unknown".into()),
                    model: model.clone(),
                    input_tokens: *input_tokens,
                    estimated_input_tokens: *estimated_input_tokens,
                    output_tokens: *output_tokens,
                    cache_read_tokens: *cached_input_tokens,
                    cache_miss_tokens: input_tokens.saturating_sub(*cached_input_tokens),
                    cache_write_tokens: *cache_write_tokens,
                    cost_usd: *cost_usd,
                    duration_ms: *duration_ms,
                    retries: *retries,
                    tool_calls: *tool_calls as u64,
                    usage_complete: *complete,
                    // Whose call this was. `None` is the lead's own — the
                    // ordinary case, and a fact rather than a gap (#4383).
                    sub_agent_id: sub_agent_id.clone(),
                },
            )
            .is_ok();
        usage_complete = *complete;
        crate::model_catalog::note_wire_model(actual_provider, model);
    } else if let AgentEvent::UsageIncomplete {
        role,
        provider,
        model,
        duration_ms,
        retries,
        partial,
        reason,
        sub_agent_id,
        ..
    } = event
    {
        usage_complete = false;
        recovered = *partial;
        // This attempt died; the `StepUsage` branch above is the one where the
        // call settled. Keeping the engine's own reason is what lets the
        // rendered sentence tell those apart instead of reassuring the user
        // about a call that produced nothing (#4147).
        died = Some(*reason);
        // A dead attempt gets a telemetry row either way, flagged
        // `usage_complete = false`. Before this the row was simply not
        // written: `stella stats` showed the turn as though the dead attempt
        // had never been dispatched, so a session with repeated drops
        // under-reported its own token use with no trace that anything was
        // missing. A flagged lower bound is recoverable information; silence
        // is not.
        //
        // With no `partial` the row is all zeros, and that is the case the
        // argument above was written for and did not reach (#4383). A
        // sub-agent model call still open when the `delegate` tool hit its
        // 900 s ceiling wrote NOTHING: the execution came out flagged
        // `usage_complete = 0` with no row anywhere saying which call did it,
        // which is a hole a reader cannot even see. A zero-usage row is not a
        // claim that the call cost nothing — the flag says the envelope never
        // landed — it is the record that the call happened.
        //
        // `cost_usd` here is catalog-priced, never provider-attested — the
        // `usage_complete = false` flag is what tells a reader which kind of
        // number they are looking at.
        let salvaged = partial.as_ref();
        let actual_provider = if provider.is_empty() {
            legacy_provider_id
        } else {
            provider
        };
        let input_tokens = salvaged.map_or(0, |p| p.usage.input_tokens);
        telemetry_ok = store
            .record_telemetry(
                execution_id,
                &TelemetryRow {
                    step: seq,
                    provider: actual_provider.to_string(),
                    call_role: enum_tag(role),
                    model: model.clone(),
                    input_tokens,
                    // No pre-dispatch estimate reaches this event, and
                    // claiming one we do not have would be worse than
                    // reporting the observed figure twice over.
                    estimated_input_tokens: input_tokens,
                    output_tokens: salvaged.map_or(0, |p| p.usage.output_tokens),
                    cache_read_tokens: salvaged.map_or(0, |p| p.usage.cached_input_tokens),
                    cache_miss_tokens: input_tokens
                        .saturating_sub(salvaged.map_or(0, |p| p.usage.cached_input_tokens)),
                    cache_write_tokens: salvaged.map_or(0, |p| p.usage.cache_write_tokens),
                    cost_usd: salvaged.map_or(0.0, |p| p.cost_usd),
                    duration_ms: *duration_ms,
                    retries: retries.unwrap_or(0),
                    // The attempt died before any tool call could settle.
                    tool_calls: 0,
                    usage_complete: false,
                    // A delegate abandoned at the engine's dispatch ceiling
                    // lands here, and a row nobody can attribute explains the
                    // execution's `usage_complete = 0` without saying whose
                    // call it was.
                    sub_agent_id: sub_agent_id.clone(),
                },
            )
            .is_ok();
    } else if let AgentEvent::BlockRegistered {
        block_id,
        kind,
        origin,
        token_cost,
        content_digest,
        citation_label,
        content,
    } = event
    {
        // Context receipts (spec §4). Best-effort — a receipt write failure
        // never fails the paid-call accounting boundary (these rows are
        // observability, not billing), and the block also survives verbatim in
        // the generic `events` table via record_event above. `content` is the
        // local-only gap preimage (spec §5.3), present only for gap kinds.
        let _ = store.record_context_block(
            execution_id,
            &ContextBlockRow {
                block_id: block_id.clone(),
                kind: enum_tag(kind),
                origin_turn: origin.turn_instance,
                origin_step: origin.step as u64,
                call_id: origin.call_id.clone(),
                memory_id: origin.memory_id.clone(),
                // Always `Some` on the live path: the emitter had the block's
                // content in hand to hash it, so it always knows the cost.
                // `None` is reachable only for history the v19 migration could
                // not re-derive (#925).
                token_cost: Some(*token_cost),
                content_digest: content_digest.clone(),
                citation_label: citation_label.clone(),
                content: content.clone(),
            },
        );
    } else if let AgentEvent::StepManifest {
        turn_instance,
        step,
        call_seq,
        role,
        provider,
        upstream_provider,
        model,
        blocks,
        effective_budget_tokens,
        calibration_factor,
        estimated_input_tokens,
        stall_seconds_requested,
        compiled_frame,
    } = event
    {
        let _ = store.record_step_manifest(
            execution_id,
            &StepManifestRow {
                turn_instance: *turn_instance,
                step: *step as u64,
                call_seq: *call_seq,
                provider: provider.clone(),
                // Who actually served a gateway-routed call (#3054) — the
                // projection that finally lands `upstream_provider` in a
                // real column instead of only the raw `events` payload.
                upstream_provider: upstream_provider.clone(),
                model: model.clone(),
                call_role: enum_tag(role),
                effective_budget_tokens: *effective_budget_tokens,
                calibration_factor: *calibration_factor,
                estimated_input_tokens: *estimated_input_tokens,
                // Phase 2 (#713): the two travel together or not at all — a
                // half-written frame identity would be a hash no id resolves.
                compiled_frame_id: compiled_frame.as_ref().map(|f| f.compiled_frame_id.clone()),
                frame_hash: compiled_frame.as_ref().map(|f| f.frame_hash.clone()),
                // Carried through as-is, `None` included: the receipt records
                // what the emitter classified, and a store that turned "not
                // classified" into zero would be the invented number #3621
                // exists to avoid.
                stall_seconds_requested: *stall_seconds_requested,
                blocks: blocks
                    .iter()
                    .map(|b| ManifestBlockRow {
                        block_id: b.block_id.clone(),
                        cache_zone: enum_tag(&b.cache_zone),
                        token_cost: Some(b.token_cost),
                        resident_since_step: b.resident_since_step as u64,
                        message_index: b.message_index as u64,
                        call_id: b.call_id.clone(),
                    })
                    .collect(),
            },
        );
    }
    // What disqualifies the execution's COST ROLLUP, which is the only thing
    // `usage_complete` claims — deliberately not `recorded`.
    //
    // `recorded` is whether *this event* reached the append-only event log,
    // for any of the ~40 variants that flow through here: a task update, a
    // block registration, a manifest. A lossy event log is a real durability
    // problem and it already has its own channel — `StoreWriteFailed` below,
    // which the deck surfaces (`command_deck::forwarder`). It says nothing
    // about whether the cost total is whole, and folding it in here meant one
    // transient SQLite failure on any event permanently disqualified an
    // execution whose accounting was provably complete.
    //
    // Permanently, because `mark_execution_usage_incomplete` is a one-way
    // latch: `finish_execution_accounted`'s CASE re-reads `usage_status` and
    // holds the row at 0 once tripped, and nothing in `stella-store` ever
    // writes it back to 1. Both consumers require `usage_complete = 1` — the
    // local rollup and hub replication — so the execution's real spend simply
    // vanishes from `stella usage report`. Witnessed in the wild: an
    // execution carrying 69 telemetry rows, every one flagged complete,
    // summing to its recorded $2.40705242 to the last decimal, reported as
    // incomplete and excluded.
    //
    // The two that DO bear on the total stay: `telemetry_ok` is a receipt
    // that failed to land (the sum is now short by that call), and
    // `usage_complete` is a call the provider never accounted for. Neither is
    // recoverable by reconciliation — a receipt that was never written is
    // invisible to any later sum — so the latch is still the right shape for
    // them. It is only the third input that never belonged.
    if !telemetry_ok || !usage_complete {
        let _ = store.mark_execution_usage_incomplete(execution_id);
    }
    // A failed write outranks unreported usage: it is the more serious of the
    // two and the only one that points at the store.
    if !recorded || !telemetry_ok {
        PersistOutcome::StoreWriteFailed
    } else if !usage_complete {
        PersistOutcome::UsageIncomplete {
            partial: recovered,
            died,
        }
    } else {
        PersistOutcome::Complete
    }
}

#[cfg(test)]
mod pipeline_variant_tests;

#[cfg(test)]
mod usage_recovery_tests;

#[cfg(test)]
mod run_terminator_tests {
    use super::*;

    fn drain(rx: &mut mpsc::UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            out.push(event);
        }
        out
    }

    /// A completed raw turn ends its run; an aborted one does not, because the
    /// abort already crossed the stream as `Error` and a run must never end on
    /// both.
    ///
    /// The rule was documented on [`emit_run_complete_for_turn`] and nowhere
    /// asserted, which is how the raw one-shot path came to call nothing at all
    /// (#3379 landing against #3418/#3425): the only signal was a dead-code
    /// lint on an uncalled helper, and a lint on the producer says nothing
    /// about whether a run still ends.
    #[test]
    fn a_completed_turn_ends_the_run_and_an_aborted_one_does_not() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let events = stella_core::EventSender::new(tx);

        emit_run_complete_for_turn(
            &events,
            "worker",
            &TurnOutcome::Completed {
                text: "done".into(),
                cost_usd: 0.5,
            },
        );
        let seen = drain(&mut rx);
        assert!(
            matches!(
                seen.as_slice(),
                [AgentEvent::RunComplete { model, cost_usd }]
                    if model == "worker" && (*cost_usd - 0.5).abs() < f64::EPSILON
            ),
            "a completed turn ends the run, carrying its model and spend: {seen:?}"
        );

        emit_run_complete_for_turn(
            &events,
            "worker",
            &TurnOutcome::Aborted {
                reason: "budget".into(),
                kind: stella_core::AbortKind::DeliberateStop,
                cost_usd: 0.5,
            },
        );
        assert!(
            drain(&mut rx).is_empty(),
            "an aborted turn already sent Error; sealing it as a success would \
             make the run end on both"
        );
    }
}

#[cfg(test)]
mod stream_tests;

#[cfg(test)]
mod reactor_tests {
    /// The two async drains that write telemetry to SQLite, named by the
    /// function whose body must show the blocking hop.
    const DRAIN_SITES: [(&str, &str); 2] = [
        ("src/agent/persistence.rs", "pub(crate) fn spawn_renderer("),
        (
            "src/command_deck/forwarder.rs",
            "pub(crate) fn spawn_forwarder(",
        ),
    ];

    /// Every SQLite write on an event drain must run on the blocking pool.
    ///
    /// `Store::record_event` is a synchronous `rusqlite` write, and both drains
    /// are `tokio::spawn`ed tasks — so an inline call blocks a Tokio worker for
    /// as long as the write takes. That is 37 microseconds at the default
    /// durability, 4 ms under `STELLA_STORE_DURABILITY=paranoid`, and up to the
    /// full five-second `busy_timeout` whenever a second same-workspace session
    /// holds the write lock. The pragma exists because that contention is
    /// expected, which is what makes the worst case reachable rather than
    /// theoretical.
    ///
    /// This is a source-grep guard for the same reason `resume_frame.rs`'s
    /// `every_pipeline_construction_declares_its_resume_frame` is one: what can
    /// regress here is *wiring*, not logic. Observing "this ran on the blocking
    /// pool" at runtime needs either a wall-clock race or a seam injected into
    /// `persist_owed` purely to be observed, and the first is flaky
    /// while the second is test-shaped production code. The lexical check has
    /// neither problem and fails the moment a call site moves back inline.
    #[test]
    fn every_event_drain_persists_off_the_reactor() {
        let crate_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for (relative, signature) in DRAIN_SITES {
            let path = crate_root.join(relative);
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            let start = source
                .find(signature)
                .unwrap_or_else(|| panic!("{relative}: `{signature}` not found"));
            let body = &source[start..];

            // The first call after the signature is the drain's own; later ones
            // in the same file belong to tests. `persist_owed` is the single
            // entry point both drains write through (#3969) — it wraps
            // `persist_event_detailed` and `persist_event`, so naming it here
            // covers every synchronous SQLite write the loop makes.
            let call = body
                .find("persist_owed(")
                .unwrap_or_else(|| panic!("{relative}: no persistence call after `{signature}`"));
            let hop = body[..call].rfind("spawn_blocking(").unwrap_or_else(|| {
                panic!(
                    "{relative}: the SQLite write in `{signature}` is inline on a Tokio worker \
                     thread — a contended write stalls the reactor for up to the five-second \
                     busy_timeout. Move it onto `tokio::task::spawn_blocking`."
                )
            });
            assert!(
                !body[hop..call].contains(".await"),
                "{relative}: `spawn_blocking` closes before the persistence call, so the write \
                 is still running on the reactor"
            );
            assert!(
                body[call..]
                    .get(..600)
                    .is_some_and(|w| w.contains(".await")),
                "{relative}: the blocking hop is never awaited — detaching it would let events \
                 persist out of `seq` order"
            );
        }
    }
}
