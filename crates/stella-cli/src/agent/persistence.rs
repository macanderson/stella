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

/// The wrapper variant the staged pipeline records (#3388/#3381). Named once
/// here so the value the store groups by cannot drift from the value the
/// pipeline calls itself.
pub(crate) const PIPELINE_VARIANT_CLASSIC: &str = "classic";

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

/// Begin the execution row for a `stella run` turn that the staged pipeline
/// wraps.
///
/// The door is **`run`** — this is `stella run`, whatever wrapped it. That
/// the pipeline wrapped it is the `variant`, a separate fact in a separate
/// column (#3388). It used to be recorded as a door called `"pipeline"`,
/// which made the door depend on the wrapper and split one door in two for
/// anything grouping by it.
///
/// It lives here rather than at its one call site because `agent.rs` is a
/// grandfathered god file closed to growth.
pub(crate) fn begin_pipeline_execution(
    store: &Option<Arc<Store>>,
    prompt: &str,
    cfg: &Config,
    session: &str,
) -> Option<(Arc<Store>, i64)> {
    begin_execution(
        store,
        "run",
        prompt,
        cfg,
        Some(session),
        Some(PIPELINE_VARIANT_CLASSIC),
    )
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

pub(crate) fn emit_run_complete_raw(
    tx: &mpsc::UnboundedSender<AgentEvent>,
    model: &str,
    outcome: &TurnOutcome,
) {
    emit_run_complete_for_turn(&stella_core::EventSender::new(tx.clone()), model, outcome);
}

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
                let joined = tokio::task::spawn_blocking(move || {
                    let persisted = persist_event_detailed(&store, id, seq, &event, &provider_id);
                    (event, persisted)
                })
                .await;
                let (event, persisted) = match joined {
                    Ok(pair) => pair,
                    // The blocking pool only fails here if `persist_event_detailed`
                    // panicked or the runtime is shutting down. The event went with
                    // the task either way, so the turn loses one rendered line —
                    // what it must not lose is the admission that persistence is no
                    // longer complete.
                    Err(_) => {
                        outcome.persistence_complete = false;
                        seq += 1;
                        continue;
                    }
                };
                if !persisted.is_complete() {
                    outcome.persistence_complete = false;
                    // This path used to print "store write failed" for BOTH
                    // conditions, so a dropped model stream on `stella run`
                    // accused a database that was fine — the same mislabel the
                    // deck had. Name what actually happened, once per
                    // condition.
                    let (warned, scope) = match persisted {
                        PersistOutcome::StoreWriteFailed => (&mut store_warned, "this execution"),
                        _ => (&mut usage_warned, "one model call"),
                    };
                    if !*warned && let Some(message) = persisted.message(scope) {
                        *warned = true;
                        eprintln!("  {} {message}", "⚠".yellow());
                    }
                }
                seq += 1;
                event
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
                OutputFormat::Text => match &event {
                    AgentEvent::ToolStart { call } => {
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
        // `Complete` is a protocol terminator, not ordinary narration. Hold
        // it until every later accounting/reflection event has drained, and
        // persist/print exactly one terminal frame as the final stream item.
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
    if matches!(event, AgentEvent::TurnComplete { .. }) {
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
    UsageIncomplete(Option<stella_protocol::PartialUsage>),
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
            PersistOutcome::UsageIncomplete(Some(partial)) => Some(format!(
                "{what} dropped before its final usage frame — recovered {} \
                 (~${:.4}) as an estimate; every other call is accounted normally",
                token_summary(&partial),
                partial.cost_usd,
            )),
            // Deliberately not "failed": this branch also covers a call that
            // SETTLED without a provider usage frame, where the work landed
            // fine and only the accounting is short. Asserting a failure that
            // did not happen is the same class of mistake as the session-wide
            // wording this replaces.
            PersistOutcome::UsageIncomplete(None) => Some(format!(
                "{what} reported no final usage — that call's tokens and cost \
                 are unaccounted (the work itself is unaffected)"
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
        ..
    } = event
    {
        usage_complete = false;
        recovered = *partial;
        // A failed attempt that salvaged accounting gets a real telemetry row,
        // flagged `usage_complete = false`. Before this the row was simply not
        // written: `stella stats` showed the turn as though the dead attempt
        // had never been dispatched, so a session with repeated drops
        // under-reported its own token use with no trace that anything was
        // missing. A flagged lower bound is recoverable information; silence
        // is not.
        //
        // `cost_usd` here is catalog-priced, never provider-attested — the
        // `usage_complete = false` flag is what tells a reader which kind of
        // number they are looking at.
        if let Some(partial) = partial {
            let actual_provider = if provider.is_empty() {
                legacy_provider_id
            } else {
                provider
            };
            telemetry_ok = store
                .record_telemetry(
                    execution_id,
                    &TelemetryRow {
                        step: seq,
                        provider: actual_provider.to_string(),
                        call_role: enum_tag(role),
                        model: model.clone(),
                        input_tokens: partial.usage.input_tokens,
                        // No pre-dispatch estimate reaches this event, and
                        // claiming one we do not have would be worse than
                        // reporting the observed figure twice over.
                        estimated_input_tokens: partial.usage.input_tokens,
                        output_tokens: partial.usage.output_tokens,
                        cache_read_tokens: partial.usage.cached_input_tokens,
                        cache_miss_tokens: partial
                            .usage
                            .input_tokens
                            .saturating_sub(partial.usage.cached_input_tokens),
                        cache_write_tokens: partial.usage.cache_write_tokens,
                        cost_usd: partial.cost_usd,
                        duration_ms: *duration_ms,
                        retries: retries.unwrap_or(0),
                        // The attempt died before any tool call could settle.
                        tool_calls: 0,
                        usage_complete: false,
                    },
                )
                .is_ok();
        }
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
        model,
        blocks,
        effective_budget_tokens,
        calibration_factor,
        estimated_input_tokens,
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
                model: model.clone(),
                call_role: enum_tag(role),
                effective_budget_tokens: *effective_budget_tokens,
                calibration_factor: *calibration_factor,
                estimated_input_tokens: *estimated_input_tokens,
                // Phase 2 (#713): the two travel together or not at all — a
                // half-written frame identity would be a hash no id resolves.
                compiled_frame_id: compiled_frame.as_ref().map(|f| f.compiled_frame_id.clone()),
                frame_hash: compiled_frame.as_ref().map(|f| f.frame_hash.clone()),
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
    let complete = recorded && telemetry_ok && usage_complete;
    if !complete {
        let _ = store.mark_execution_usage_incomplete(execution_id);
    }
    // A failed write outranks unreported usage: it is the more serious of the
    // two and the only one that points at the store.
    if !recorded || !telemetry_ok {
        PersistOutcome::StoreWriteFailed
    } else if !usage_complete {
        PersistOutcome::UsageIncomplete(recovered)
    } else {
        PersistOutcome::Complete
    }
}

#[cfg(test)]
mod usage_recovery_tests {
    use super::*;

    fn partial(input: u64, cached: u64, output: u64, cost: f64) -> stella_protocol::PartialUsage {
        stella_protocol::PartialUsage {
            usage: stella_protocol::CompletionUsage {
                input_tokens: input,
                cached_input_tokens: cached,
                output_tokens: output,
                ..Default::default()
            },
            cost_usd: cost,
            input_reported: true,
        }
    }

    fn incomplete(partial: Option<stella_protocol::PartialUsage>) -> AgentEvent {
        AgentEvent::UsageIncomplete {
            role: stella_protocol::ModelCallRole::Worker,
            provider: "anthropic".into(),
            model: "claude-opus-5".into(),
            reason: stella_protocol::UsageIncompleteReason::ProviderError,
            duration_ms: 4_200,
            retries: Some(1),
            partial,
        }
    }

    /// The storage half of the fix. A dropped attempt that salvaged real
    /// numbers must leave a row behind — flagged, but present. Writing
    /// nothing at all is what made `stella stats` under-report a session's
    /// token use with no trace that anything was missing.
    #[test]
    fn a_recovered_attempt_lands_a_row_flagged_incomplete() {
        let store = stella_store::Store::in_memory().expect("store");
        let execution_id = store
            .begin_execution("cli", "prompt", "anthropic", "claude-opus-5")
            .expect("begin");

        let outcome = persist_event_detailed(
            &store,
            execution_id,
            0,
            &incomplete(Some(partial(14_000, 12_000, 130, 0.0213))),
            "anthropic",
        );

        let rows = store.telemetry_rows_after(0, 10).expect("rows");
        assert_eq!(rows.len(), 1, "the salvaged attempt is recorded");
        let row = &rows[0].telemetry;
        assert_eq!(row.input_tokens, 14_000);
        assert_eq!(row.cache_read_tokens, 12_000);
        assert_eq!(row.cache_miss_tokens, 2_000, "miss = input - cached");
        assert_eq!(row.output_tokens, 130);
        assert!((row.cost_usd - 0.0213).abs() < f64::EPSILON);
        assert!(
            !row.usage_complete,
            "a catalog-priced lower bound must never pass as settled accounting"
        );
        // And the execution as a whole is marked short.
        assert!(!store.execution_usage_complete(execution_id).unwrap());
        assert!(matches!(outcome, PersistOutcome::UsageIncomplete(Some(_))));
    }

    /// A failure that learned nothing writes no telemetry row. Recording a
    /// zeroed one would be worse than silence: it reads as a real, free call.
    #[test]
    fn an_attempt_that_recovered_nothing_writes_no_row() {
        let store = stella_store::Store::in_memory().expect("store");
        let execution_id = store
            .begin_execution("cli", "prompt", "anthropic", "claude-opus-5")
            .expect("begin");

        let outcome =
            persist_event_detailed(&store, execution_id, 0, &incomplete(None), "anthropic");

        assert!(store.telemetry_rows_after(0, 10).expect("rows").is_empty());
        assert!(!store.execution_usage_complete(execution_id).unwrap());
        assert!(matches!(outcome, PersistOutcome::UsageIncomplete(None)));
    }

    /// The wording defect from the report: one retried call must not be
    /// described as the whole session, and when numbers were recovered the
    /// sentence should say so rather than leaving the user to assume the
    /// worst.
    #[test]
    fn the_warning_names_one_call_and_reports_what_was_recovered() {
        let message = PersistOutcome::UsageIncomplete(Some(partial(14_000, 12_000, 130, 0.0213)))
            .message("one model call")
            .expect("an incomplete outcome has a sentence");
        assert!(message.starts_with("one model call"), "{message}");
        assert!(!message.contains("this session"), "{message}");
        assert!(message.contains("14000 input"), "{message}");
        assert!(message.contains("12000 cached"), "{message}");
        assert!(message.contains("130 output"), "{message}");
        assert!(message.contains("0.0213"), "{message}");

        // With nothing recovered it stays honest about the gap, and still
        // scopes itself to the one attempt.
        let bare = PersistOutcome::UsageIncomplete(None)
            .message("one model call")
            .expect("still a sentence");
        assert!(bare.contains("tokens and cost"), "{bare}");
        assert!(!bare.contains("this session"), "{bare}");
        // A call that settled without a usage frame did not "fail", and the
        // sentence must not claim it did.
        assert!(!bare.contains("failed"), "{bare}");

        // A genuine store failure keeps its own, more serious wording.
        let store_failed = PersistOutcome::StoreWriteFailed
            .message("this session")
            .expect("a sentence");
        assert!(
            store_failed.contains("store write failed"),
            "{store_failed}"
        );
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;

    #[test]
    fn complete_is_unique_and_final_even_when_later_events_arrive() {
        let events = vec![
            AgentEvent::Stage {
                name: stella_protocol::StageKind::Execute,
                scope: stella_protocol::StageScope::Run,
            },
            AgentEvent::TurnComplete {
                model: "old".into(),
                cost_usd: 1.0,
            },
            AgentEvent::Stage {
                name: stella_protocol::StageKind::Reflect,
                scope: stella_protocol::StageScope::Run,
            },
            AgentEvent::TurnComplete {
                model: "final".into(),
                cost_usd: 1.25,
            },
        ];
        let mut terminal = None;
        let mut ordered: Vec<_> = events
            .into_iter()
            .filter_map(|event| defer_stream_terminal(&mut terminal, event))
            .collect();
        ordered.extend(terminal);

        assert_eq!(
            ordered
                .iter()
                .filter(|event| matches!(event, AgentEvent::TurnComplete { .. }))
                .count(),
            1
        );
        assert!(matches!(
            ordered.last(),
            Some(AgentEvent::TurnComplete { model, cost_usd })
                if model == "final" && (*cost_usd - 1.25).abs() < f64::EPSILON
        ));
    }

    #[tokio::test]
    async fn stream_renderer_persists_reflection_before_one_terminal_complete() {
        let store = std::sync::Arc::new(stella_store::Store::in_memory().expect("store"));
        let execution_id = store
            .begin_execution("pipeline", "prompt", "anthropic", "claude")
            .expect("begin");
        store
            .set_execution_session(execution_id, "stream-order")
            .expect("session");
        let (tx, rx) = mpsc::unbounded_channel();
        let renderer = spawn_renderer(
            rx,
            OutputFormat::StreamJson,
            Some((store.clone(), execution_id)),
            "anthropic".into(),
            false,
        );
        tx.send(AgentEvent::TurnComplete {
            model: "worker".into(),
            cost_usd: 1.0,
        })
        .unwrap();
        tx.send(AgentEvent::Stage {
            name: stella_protocol::StageKind::Reflect,
            scope: stella_protocol::StageScope::Run,
        })
        .unwrap();
        tx.send(AgentEvent::TurnComplete {
            model: "worker+reflection".into(),
            cost_usd: 1.25,
        })
        .unwrap();
        drop(tx);

        let outcome = renderer.await.expect("renderer");
        assert!(outcome.persistence_complete);
        let journal = store.session_events("stream-order").expect("journal");
        assert_eq!(journal.events.len(), 2);
        assert!(matches!(
            journal.events.first().map(|record| &record.event),
            Some(AgentEvent::Stage {
                name: stella_protocol::StageKind::Reflect,
                scope: stella_protocol::StageScope::Run
            })
        ));
        assert!(matches!(
            journal.events.last().map(|record| &record.event),
            Some(AgentEvent::TurnComplete { model, cost_usd })
                if model == "worker+reflection"
                    && (*cost_usd - 1.25).abs() < f64::EPSILON
        ));
    }

    #[test]
    fn receipt_events_persist_into_queryable_block_and_manifest_rows() {
        // The increment-1 promise, end to end: a BlockRegistered + StepManifest
        // pair flowing through persist_event lands as queryable receipt rows,
        // and the manifest reconstructs the step's block order with token_cost
        // joined back from the block registry.
        use stella_protocol::{BlockKind, BlockOrigin, CacheZone, ManifestEntry, ModelCallRole};
        let store = Store::in_memory().expect("store");
        let id = store
            .begin_execution("run", "p", "anthropic", "opus")
            .expect("exec");

        let registered = AgentEvent::BlockRegistered {
            block_id: "blk_tool1".into(),
            kind: BlockKind::ToolResult,
            origin: BlockOrigin {
                turn_instance: 0,
                step: 0,
                call_id: Some("c1".into()),
                memory_id: None,
            },
            token_cost: 40,
            content_digest: "sha256:abc".into(),
            citation_label: None,
            content: None,
        };
        assert!(persist_event(&store, id, 0, &registered, "anthropic"));

        let manifest = AgentEvent::StepManifest {
            turn_instance: 0,
            step: 0,
            call_seq: 0,
            role: ModelCallRole::Worker,
            provider: "anthropic".into(),
            model: "opus".into(),
            blocks: vec![ManifestEntry {
                block_id: "blk_tool1".into(),
                cache_zone: CacheZone::Volatile,
                token_cost: 40,
                resident_since_step: 0,
                message_index: 0,
                call_id: Some("call_tool1".into()),
            }],
            effective_budget_tokens: 136_363,
            calibration_factor: 1.1,
            estimated_input_tokens: 40,
            compiled_frame: None,
        };
        assert!(persist_event(&store, id, 1, &manifest, "anthropic"));

        // The block registry row, with its call_id join key and snake_case kind.
        let blocks = store.context_blocks(id).expect("blocks");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].block_id, "blk_tool1");
        assert_eq!(blocks[0].call_id.as_deref(), Some("c1"));
        assert_eq!(blocks[0].kind, "tool_result");

        // The manifest reconstructs the step's ordered blocks, token_cost joined
        // back from context_blocks.
        let entries = store.step_manifest(id, 0, 0, 0).expect("manifest");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].block_id, "blk_tool1");
        assert_eq!(entries[0].cache_zone, "volatile");
        assert_eq!(entries[0].token_cost, Some(40));
    }

    #[test]
    fn end_to_end_receipt_reconstructs_the_step_byte_exact_from_the_persisted_store() {
        // The increment-2 gate: the REAL emitter produces a receipt that, once
        // persisted, reconstructs byte-exact what the model saw — resolved from
        // the fold (tool I/O, assistant text) + local gaps (system/user), never
        // from the emitter's in-memory state. Exercises a full tool round-trip.
        use stella_core::event_sender::EventSender;
        use stella_core::receipts::ReceiptLedger;
        use stella_protocol::{CompletionMessage, MessageRole, ToolCall, ToolOutput, ToolResult};

        let call = ToolCall {
            call_id: "c1".into(),
            name: "read_file".into(),
            input: serde_json::json!({ "path": "a.rs" }),
        };
        let output = ToolOutput::Ok {
            content: "fn a() {}".into(),
            data: None,
        };
        // The step-1 input: system, user, assistant (text + tool call), result.
        let original = vec![
            CompletionMessage::system("you are a careful engineer"),
            CompletionMessage::user("fix the failing test"),
            CompletionMessage {
                role: MessageRole::Assistant,
                content: "let me read the file".into(),
                tool_calls: vec![call.clone()],
                tool_results: vec![],
                attachments: vec![],
            },
            CompletionMessage {
                role: MessageRole::Tool,
                content: String::new(),
                tool_calls: vec![],
                tool_results: vec![ToolResult {
                    call_id: "c1".into(),
                    output: output.clone(),
                }],
                attachments: vec![],
            },
        ];

        // Drive the REAL emitter + the journal events the driver would emit.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let events = EventSender::new(tx);
        let _ = events.send(AgentEvent::Text {
            text: "let me read the file".into(),
        });
        let _ = events.send(AgentEvent::ToolStart { call: call.clone() });
        let _ = events.send(AgentEvent::ToolResult {
            call_id: "c1".into(),
            output: output.clone(),
            duration_ms: 5,
            speculated: false,
        });
        let mut ledger = ReceiptLedger::new(0);
        ledger.set_effective_budget(136_363, 1.1);
        ledger.emit_step_receipt_estimating(
            &original,
            1,
            stella_core::receipts::ServedBy {
                role: stella_protocol::ModelCallRole::Worker,
                provider: "anthropic",
                model: "opus",
            },
            &events,
        );
        drop(events);

        // Persist the whole stream exactly as the renderer would.
        let store = Store::in_memory().expect("store");
        let id = store
            .begin_execution("run", "fix the failing test", "anthropic", "opus")
            .expect("exec");
        let mut seq = 0u64;
        while let Ok(event) = rx.try_recv() {
            persist_event(&store, id, seq, &event, "anthropic");
            seq += 1;
        }

        // Reconstruct purely from the persisted store, and prove it byte-exact.
        let recon = store
            .reconstruct_worker_step(id, 0, 1)
            .expect("reconstruct");
        assert!(
            recon.is_verified(),
            "unresolved={:?} mismatches={:?}",
            recon.unresolved,
            recon.digest_mismatches
        );
        assert_eq!(recon.messages, original);
    }

    #[test]
    fn a_decomposed_recall_turn_still_reconstructs_byte_exact() {
        // The Phase 2 gate (#713): "byte-exact turn reconstruction still
        // passes, NOW INCLUDING DECOMPOSED RECALL." The recall block splits
        // into one block per recalled item, the summary and the steer stop
        // being attributed to the user, and an attachment gets a block of its
        // own — and after all of that the persisted receipt must still rebuild
        // the exact messages the model saw.
        //
        // This is the single most important assertion in the phase: every other
        // benefit of decomposition is worthless if the receipt stops being a
        // faithful record.
        use stella_core::event_sender::EventSender;
        use stella_core::receipts::{RECALL_MARKER, ReceiptLedger};
        use stella_protocol::{Attachment, CompletionMessage, MessageRole};

        let recall = format!(
            "{RECALL_MARKER}\n\nRelevant context:\n\
             - [nod_abc123] auth module — always validate the token before use\n\
             - [nod_def456] deploy runbook — staging first, then production\n\
             - engine step-driver (driver.rs) — the step loop lives here"
        );
        let summary = "[earlier history summarized to fit context — full detail was compacted \
                       away; re-read files or re-run tools for specifics]\n\nwe read three files";
        let steer = "[stuck-loop warning] you appear to be looping: same call twice.";

        let original = vec![
            CompletionMessage::system("you are a careful engineer"),
            CompletionMessage::user(summary),
            CompletionMessage::user(recall.clone()),
            CompletionMessage {
                role: MessageRole::User,
                content: "fix the failing test".into(),
                tool_calls: vec![],
                tool_results: vec![],
                attachments: vec![Attachment::from_path(
                    "screenshot.png",
                    "image/png",
                    2048,
                    "/tmp/screenshot.png",
                )],
            },
            CompletionMessage::user(steer),
        ];

        let (tx, mut rx) = mpsc::unbounded_channel();
        let events = EventSender::new(tx);
        let mut ledger = ReceiptLedger::new(0).with_lifecycle(true);
        ledger.emit_step_receipt_estimating(
            &original,
            0,
            stella_core::receipts::ServedBy {
                role: stella_protocol::ModelCallRole::Worker,
                provider: "anthropic",
                model: "opus",
            },
            &events,
        );
        drop(events);

        let store = Store::in_memory().expect("store");
        let id = store
            .begin_execution("run", "fix the failing test", "anthropic", "opus")
            .expect("exec");
        let mut seq = 0u64;
        let mut kinds: Vec<String> = Vec::new();
        let mut memory_ids: Vec<String> = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::BlockRegistered { kind, origin, .. } = &event {
                kinds.push(enum_tag(kind));
                if let Some(memory_id) = &origin.memory_id {
                    memory_ids.push(memory_id.clone());
                }
            }
            persist_event(&store, id, seq, &event, "anthropic");
            seq += 1;
        }

        // Decomposition actually happened — otherwise the round-trip below
        // would pass trivially by never having split anything.
        assert!(
            kinds.contains(&"summary".to_string()),
            "the overflow summary is no longer the user's goal: {kinds:?}"
        );
        assert!(
            kinds.contains(&"steered".to_string()),
            "the stuck-loop steer is no longer the user's goal: {kinds:?}"
        );
        assert!(
            kinds.contains(&"attachment".to_string()),
            "the attachment has a block: {kinds:?}"
        );
        assert_eq!(
            kinds.iter().filter(|k| *k == "recalled_frame").count(),
            4,
            "one leading segment + three recalled items: {kinds:?}"
        );
        // Per-item provenance: the two memory frames resolve to their records.
        assert_eq!(memory_ids, vec!["nod_abc123", "nod_def456"]);

        // And the receipt is still a faithful record of what the model saw.
        let recon = store
            .reconstruct_worker_step(id, 0, 0)
            .expect("reconstruct");
        assert!(
            recon.is_verified(),
            "unresolved={:?} mismatches={:?}",
            recon.unresolved,
            recon.digest_mismatches
        );
        assert_eq!(recon.messages, original);
    }
}

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
    /// `persist_event_detailed` purely to be observed, and the first is flaky
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
            // in the same file belong to tests.
            let call = body
                .find("persist_event_detailed(")
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
