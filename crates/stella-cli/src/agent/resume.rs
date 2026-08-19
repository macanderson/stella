// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Continue a killed supervised run's interrupted turn (#1586) — the child
//! half of `stella daemon resume`.
//!
//! Every turn already checkpoints into the workspace's work journal at each
//! committed step boundary ([`crate::durability`]), and every terminal path
//! discards that checkpoint — so its presence *means* the process died
//! mid-turn. This module is the read side the supervised surface was missing:
//! rebuild the turn from the checkpoint with
//! [`stella_core::step::TurnState::from_checkpoint`] and drive it through
//! `Engine::run_step` to an ordinary end.
//!
//! # Why nothing is re-run or double-applied
//!
//! The checkpoint is written only at [`stella_core::step::StepOutcome::
//! Continue`], the one boundary where every `tool_use` is paired with its
//! `tool_result`. The restored transcript therefore already *contains* every
//! completed step's effects as facts the model can read; the resumed turn's
//! first provider call simply asks for the step after the last committed one.
//!
//! # What resuming honestly costs
//!
//! - **The system prompt is the checkpointed one, byte for byte.** The deck's
//!   turn-boundary resume regenerates it (rules may have changed); mid-turn,
//!   fidelity wins — the transcript the provider priced and cached is the
//!   transcript it gets back.
//! - **A turn interrupted mid-pipeline always resumes as a plain engine
//!   turn** (#3865: the staged pipeline that used to restore beyond it,
//!   `crates/stella-pipeline`, is gone workspace-wide). A frame left by an
//!   older build — [`crate::resume_frame::PipelineFrame`] — is still read and
//!   reported on, never silently dropped: the run names every stage it is
//!   not restoring, drops the green tick, and files its audit row as
//!   `resumed_complete_unverified` (#1615), the graceful refusal
//!   [`crate::resume_frame::ResumeFrame::advisory`] states in full.

use super::outcome::turn_outcome_result;
use super::*;
use crate::failure::CliFailure;

/// The child driver: adopt the record, rebuild the turn, drive it to an end.
///
/// `id` is the record to resume (a unique prefix is enough); `None` falls
/// back to this process's own supervised identity, which is how the spawned
/// child path always arrives.
///
/// Answers in [`CliFailure`], not a `String`, because a resumed turn must end
/// the process exactly as an un-resumed one would: a resumed deliberate stop
/// (a stuck loop escalated, the step cap reached) exits
/// [`crate::failure::DELIBERATE_STOP_EXIT_CODE`] and a resumed failure exits
/// `1`. The parent half (`crate::daemon::resume_supervised`) already forwards
/// its child's code verbatim, so retyping this boundary is the whole of what
/// the resumed path was missing (#1637).
pub(crate) async fn run_resume(cfg: &Config, id: Option<&str>) -> Result<(), CliFailure> {
    let registry = stella_store::SessionRegistry::open_default();
    let record = match (id, crate::daemon::supervised_id()) {
        (Some(id), _) => crate::daemon::resolve(&registry, Some(id))?,
        (None, Some(own)) => crate::daemon::resolve(&registry, Some(&own))?,
        (None, None) => return Err(CliFailure::error("daemon resume needs a run id")),
    };
    // The hand-run `--foreground` guard: a resumed turn must run where its
    // work is. The spawned child always passes (the parent pinned its cwd).
    let recorded = std::fs::canonicalize(&record.workspace).unwrap_or_default();
    let current = std::fs::canonicalize(&cfg.workspace_root).unwrap_or_default();
    if recorded != current {
        return Err(CliFailure::error(format!(
            "{} belongs to {} — resume it from there, or without --foreground",
            record.id, record.workspace
        )));
    }

    let provider = build_provider(cfg)?;
    let tools_registry: std::sync::Arc<ToolRegistry> =
        std::sync::Arc::new(crate::write_dirs::registry_for(cfg));
    crate::subagent::install_for_session(cfg, &tools_registry)?;
    // `interactive_approvals: false` — a daemon-resumed turn answers approvals
    // through the daemon's own one-shot gate (sidecar or staged stdio), never
    // a TTY prompt raced against it (#2676).
    // The return value fed only the pipeline-restore arm's own recall block
    // (#3865 removed it); the call itself still attaches the workspace's rule
    // guards to the registry, which every resumed turn needs regardless.
    let _ = crate::rules::enforce_workspace_rules(
        &tools_registry,
        &cfg.workspace_root,
        &cfg.authority,
        false,
    );
    let (_session_graph, _graph_build) = spawn_session_graph(
        &cfg.workspace_root,
        Box::new(crate::agent::init::stderr_narrator()),
        Box::new(|| {}),
    );
    let mcp = connect_mcp(
        cfg,
        tools_registry.clone(),
        Some(tools_registry.mcp_usage_ledger()),
        true,
    )
    .await?;
    let base_tools: &dyn ToolExecutor = match &mcp {
        Some(set) => set.as_ref(),
        None => &*tools_registry,
    };
    let custom_tools = discover_custom_tools(cfg, true).await;
    let store = open_store(&cfg.workspace_root);
    let calibration = seed_calibration(&store, cfg);

    // Adopt the record and bind durability to the SAME session id — this is
    // how the checkpoint is found.
    let record =
        crate::session_persist::adopt_record(record, stella_store::SessionStatus::InProgress);
    registry
        .upsert(&record)
        .map_err(|e| format!("cannot re-own session {}: {e}", record.id))?;
    if let Some(warning) =
        crate::durability::bind_session(&cfg.durability, &cfg.workspace_root, &record.id)
    {
        eprintln!("  {warning}");
    }
    let Some(json) = cfg.durability.checkpoint() else {
        return Err(CliFailure::error(format!(
            "{} has no resume point. A turn leaves one only when its process was killed \
             mid-turn; a run that completed, was stopped, or aborted discards it on the \
             way out",
            record.id
        )));
    };
    let checkpoint = stella_core::step::Checkpoint::from_json(&json).map_err(|e| {
        format!(
            "{}'s resume point cannot be read by this build ({e}); refusing to resume a \
             turn from a shape this build half-recognizes",
            record.id
        )
    })?;

    // Read BEFORE the first step, and print before it too: an operator who is
    // going to be handed less than the run they lost has to learn that while
    // they can still stop it, not from an audit row afterwards (#1615).
    let frame = crate::resume_frame::ResumeFrame::read(&cfg.durability);

    let turn_start = Instant::now();
    let step = checkpoint.step;
    eprintln!(
        "  resuming {} at step {step} — completed steps stay done, nothing re-runs",
        record.id
    );
    // A frame naming a staged pipeline (#3865: the crate that could restore
    // one is gone) always takes the bare-turn path below — `frame.advisory()`
    // reports exactly what that costs, the graceful refusal this module's own
    // doc comment promises.
    if let Some(advisory) = frame.advisory() {
        for (i, line) in advisory.iter().enumerate() {
            // The marker leads the summary line only; the rest are already
            // indented continuations of it.
            let marker = if i == 0 {
                "!".yellow().bold().to_string()
            } else {
                " ".to_string()
            };
            eprintln!("  {marker} {line}");
        }
    }
    let execution = begin_execution(&store, "resume", &record.title, cfg, Some(&record.id), None);
    let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
    let events = stella_core::EventSender::new(tx.clone());
    tools_registry.bridge_policy_plane(events.clone());
    tools_registry.attach_events(events.clone());
    let renderer = spawn_renderer(
        rx,
        OutputFormat::Text,
        execution.clone(),
        cfg.provider.id.to_string(),
        false,
        Some(record.title.clone()),
    );

    // A killed turn always resumes as a bare engine turn now (#3865: the
    // staged pipeline that could restore beyond it is gone) — `frame`'s
    // advisory, printed above, is what tells the operator so.
    let end = {
        let tools = super::tool_stack::session_stack(
            base_tools,
            custom_tools.to_vec(),
            cfg,
            Principal::User,
            tools_registry.hook_bus(),
        );
        let hook_runner = ShellHookRunner;
        let engine_config = engine_config_for(cfg);
        let state = stella_core::step::TurnState::from_checkpoint(checkpoint, &engine_config);
        let mut engine = Engine::with_sleeper(&*provider, &tools, engine_config, &TokioSleeper)
            .with_calibration(&calibration);
        if let Some(hooks) = &cfg.hooks {
            engine = engine.with_hooks(hooks, &hook_runner);
        }
        drive_resumed_turn(&engine, state, &events).await
    };

    // Project the ending onto the one reporting surface. `resumed_complete_unverified`
    // keeps meaning exactly what it always did for a degraded (`frame != BareTurn`) resume.
    struct Reported {
        /// The `executions` row's outcome label.
        label: &'static str,
        cost_usd: f64,
        /// The terminal banner: `(got the green tick, the line)`, or `None`
        /// for an ending that prints its failure through the event stream.
        banner: Option<(bool, String)>,
        result: Result<(), CliFailure>,
    }
    let reported = {
        let label = match &end {
            // A degraded resume gets its own label: the scrollback warning is
            // gone by the next command, and a stats query that cannot
            // separate a verified resume from an unverified one reports the
            // wrong number forever.
            TurnOutcome::Completed { .. } => frame.completed_label(),
            TurnOutcome::Aborted { .. } => "resumed_aborted",
        };
        let cost = match &end {
            TurnOutcome::Completed { cost_usd, .. } | TurnOutcome::Aborted { cost_usd, .. } => {
                *cost_usd
            }
        };
        // A degraded resume does not get a green tick. The tick is this
        // surface's claim that the run finished as it was meant to, and a
        // turn whose witness and verify stages never ran did not.
        let banner = matches!(&end, TurnOutcome::Completed { .. })
            .then(|| (!frame.degrades(), frame.completed_banner(step)));
        // The abort's typed `kind` decides the exit code here exactly as it
        // does for a fresh turn — a resumed stuck-loop stop exits `3`, not
        // `1` (#1637).
        Reported {
            label,
            cost_usd: cost,
            banner,
            result: turn_outcome_result(&end),
        }
    };
    let Reported {
        label,
        cost_usd,
        banner,
        result,
    } = reported;

    // One terminator for the resumed run (#3398), on both arms: the bare
    // restored turn never had one, and the pipeline arm used to get the
    // pipeline's. A failed resume already reported through `result`.
    if result.is_ok() {
        persistence::emit_run_complete(&events, &cfg.model_id, cost_usd);
    }
    // The canonical teardown (#960): detach the registry's sender clones,
    // drop ours, and only then await the renderer — otherwise the channel
    // never closes and a completed resume hangs.
    drop(tx);
    let persistence_complete = close_event_stream(&tools_registry, events, renderer)
        .await
        .persistence_complete;
    crate::agent::turn_close::close_turn(
        cfg,
        &store,
        &execution,
        &tools_registry,
        Some(&record.id),
        crate::agent::turn_close::TurnOutcomeRecord {
            label,
            cost_usd,
            persistence_complete,
        },
    );
    if let Some(set) = &mcp {
        set.close_all().await;
    }

    if let Some((verified, line)) = banner {
        let mark = if verified {
            "✓".green().bold()
        } else {
            "!".yellow().bold()
        };
        println!("\n  {mark} {line}");
    }
    plain::cost_summary(
        cost_usd,
        &format!("{}/{}", cfg.provider.id, cfg.model_id),
        turn_start.elapsed(),
    );
    // Written here as well as by `record_outcome_if_supervised` (which only
    // covers the spawned child), so the hand-run `--foreground` case records
    // a terminal status too. Same value on both paths — both now read the
    // failure itself, so a resumed deliberate stop records `Stopped`, not
    // `Error` (#1653); double-writing it is harmless, leaving it unwritten
    // reads as a crash forever.
    let _ = registry.set_status(
        &record.id,
        crate::daemon::outcome_status(result.as_ref().map(|_| ())),
    );
    result
}

/// Drive a checkpoint-restored turn to an ordinary end.
///
/// A thin adapter over [`stella_core::Engine::drive`], which owns the loop and
/// its obligations (persist on `Continue`, discard on every terminal path, the
/// carried step cap, the turn-halt check). This crate used to carry its own
/// copy of that loop — and, exactly as two copies predict, the copy here had
/// silently dropped the halt obligation.
///
/// A restored turn needs no driver of its own: `drive` takes the state the
/// caller minted, and [`TurnState::from_checkpoint`] is one of the three ways
/// to mint it (#2452).
///
/// [`TurnState`]: stella_core::step::TurnState
/// [`TurnState::from_checkpoint`]: stella_core::step::TurnState::from_checkpoint
pub(crate) async fn drive_resumed_turn(
    engine: &Engine<'_>,
    mut state: stella_core::step::TurnState,
    events: &stella_core::EventSender,
) -> TurnOutcome {
    // The run owner emits its own stage boundaries — the engine emits none
    // (#3416). A resumed turn is still an execute turn, and the consumers that
    // render `hud.stage` cannot tell one from a fresh one.
    let events = events.pairing_stage_complete();
    let _ = events.send(stella_protocol::AgentEvent::Stage {
        name: stella_protocol::StageKind::Execute.into(),
        scope: stella_protocol::StageScope::Run,
    });
    engine.drive(&mut state, &events).await
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;
    use stella_core::step::{
        BudgetSnapshot, CHECKPOINT_VERSION, Checkpoint, CheckpointSink, TurnState,
    };
    use stella_protocol::{
        CompletionRequestRef, CompletionResult, CompletionUsage, ProviderError, ToolSchema,
    };

    /// A provider that answers "resumed answer" and records every request's
    /// transcript, so the witness below can look at what a resumed turn
    /// actually sent.
    struct ScriptedProvider {
        requests: Mutex<Vec<Vec<CompletionMessage>>>,
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        fn id(&self) -> &str {
            "scripted"
        }
        async fn complete_ref(
            &self,
            request: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            self.requests
                .lock()
                .unwrap()
                .push(request.messages.to_vec());
            Ok(CompletionResult {
                upstream_provider: None,
                text: "resumed answer".into(),
                tool_calls: vec![],
                usage: CompletionUsage {
                    reported: true,
                    input_tokens: 1,
                    ..CompletionUsage::default()
                },
                model: "scripted".into(),
                cost_usd: 0.0,
                finish_reason: None,
            })
        }
    }

    /// No tools: the resumed step under test answers in text.
    struct NoTools;

    #[async_trait]
    impl ToolExecutor for NoTools {
        fn schemas(&self) -> Vec<ToolSchema> {
            Vec::new()
        }
        async fn execute(&self, name: &str, _input: &serde_json::Value) -> ToolOutput {
            ToolOutput::error(format!("no tool {name} in this test"))
        }
    }

    /// Records the sink calls a resumed turn makes.
    #[derive(Debug, Default)]
    struct RecordingSink {
        persists: std::sync::atomic::AtomicUsize,
        discards: std::sync::atomic::AtomicUsize,
    }

    impl CheckpointSink for RecordingSink {
        fn persist(&self, _json: &str) {
            self.persists
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        fn discard(&self) {
            self.discards
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// The turn a `kill -9` interrupted: step 0 completed (a tool ran and its
    /// result committed), step 1 checkpointed as next.
    fn killed_mid_turn() -> Checkpoint {
        let call = stella_protocol::ToolCall {
            call_id: "call_0".into(),
            name: "read_file".into(),
            input: serde_json::json!({"path": "src/lib.rs"}),
        };
        let mut with_tools = CompletionMessage::assistant("reading the file first");
        with_tools.tool_calls = vec![call];
        let mut tool_reply = CompletionMessage {
            role: stella_protocol::MessageRole::Tool,
            ..CompletionMessage::user("")
        };
        tool_reply.tool_results = vec![stella_protocol::ToolResult {
            call_id: "call_0".into(),
            output: ToolOutput::Ok {
                content: "the file's contents".into(),
                data: None,
            },
        }];
        Checkpoint {
            version: CHECKPOINT_VERSION,
            step: 1,
            messages: vec![
                CompletionMessage::system("the original system prompt"),
                CompletionMessage::user("summarize src/lib.rs"),
                with_tools,
                tool_reply,
            ],
            budget: BudgetSnapshot {
                mode: stella_protocol::BudgetMode::Observed,
                turn_limit_usd: None,
                session_limit_usd: None,
                turn_spent_usd: 0.0,
                session_spent_usd: 0.0,
            },
            total_cost_usd: 0.0,
            calibration_model: None,
            loop_steered: false,
            loop_steered_pattern: Vec::new(),
            loop_steered_inputs: None,
            transcript_rewrites: 0,
            loop_steers_spent: 0,
        }
    }

    /// The #1586 witness: a turn killed between steps resumes at the NEXT
    /// step — its one provider call carries the completed step's `tool_use`/
    /// `tool_result` pair as transcript, so the completed step is not re-run
    /// and its tool is not re-executed (the executor here would error if it
    /// were). On `main` there is no resume driver at all: a killed supervised
    /// run's turn is simply lost.
    #[tokio::test]
    async fn a_resumed_turn_continues_after_the_completed_step() {
        let provider = ScriptedProvider {
            requests: Mutex::new(Vec::new()),
        };
        let tools = NoTools;
        let sink = std::sync::Arc::new(RecordingSink::default());
        let config = EngineConfig {
            checkpoint_sink: Some(sink.clone()),
            ..EngineConfig::default()
        };
        let state = TurnState::from_checkpoint(killed_mid_turn(), &config);
        let engine = Engine::with_sleeper(&provider, &tools, config, &crate::runtime::TokioSleeper);
        let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
        let events = stella_core::EventSender::new(tx);

        let outcome = drive_resumed_turn(&engine, state, &events).await;

        let TurnOutcome::Completed { text, .. } = outcome else {
            panic!("a resumable turn must complete: {outcome:?}");
        };
        assert_eq!(text, "resumed answer");

        // Scoped so the guard drops before the `rx.recv().await` below — a
        // std MutexGuard must never be held across an await point.
        {
            let requests = provider.requests.lock().unwrap();
            assert_eq!(
                requests.len(),
                1,
                "resuming after step 0 makes exactly one further call — re-running \
                 the completed step would make two"
            );
            let transcript = &requests[0];
            assert!(
                transcript.iter().any(|m| m
                    .tool_results
                    .iter()
                    .any(|r| matches!(&r.output, ToolOutput::Ok { content , .. } if content == "the file's contents"))),
                "the completed step's tool result must arrive as transcript, not be re-executed"
            );
            assert_eq!(
                transcript[0].content, "the original system prompt",
                "mid-turn fidelity: the checkpointed system prompt rides verbatim"
            );
        }

        // A finished resume discards its checkpoint — leaving it would offer
        // to resume a turn everyone watched complete.
        assert_eq!(sink.discards.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            sink.persists.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a turn that finishes on its first resumed step has no new boundary to persist"
        );

        // The renderer's contract: the resumed turn frames itself as an
        // execute stage before its first event. RUN-scoped: the engine emits
        // no `Stage` at all any more (#3416), so this boundary is
        // `drive_resumed_turn`'s — the run owner's — statement about its run,
        // which is the scope `StageScope::Run` names (#3398).
        let first = rx.recv().await.expect("at least the stage event");
        assert!(matches!(
            first,
            AgentEvent::Stage {
                name,
                scope: stella_protocol::StageScope::Run
            } if name.kind() == Some(stella_protocol::StageKind::Execute)
        ));
    }

    /// The step cap survives the crash: a turn checkpointed AT the cap gets
    /// no further model calls — the allowance does not reset to zero.
    #[tokio::test]
    async fn a_resumed_turn_keeps_its_spent_step_allowance() {
        let provider = ScriptedProvider {
            requests: Mutex::new(Vec::new()),
        };
        let tools = NoTools;
        let config = EngineConfig::default();
        let mut at_cap = killed_mid_turn();
        at_cap.step = config.max_steps;
        let state = TurnState::from_checkpoint(at_cap, &config);
        let engine = Engine::with_sleeper(&provider, &tools, config, &crate::runtime::TokioSleeper);
        let (tx, _rx) = mpsc::unbounded_channel::<AgentEvent>();
        let events = stella_core::EventSender::new(tx);

        let outcome = drive_resumed_turn(&engine, state, &events).await;

        assert!(
            matches!(outcome, TurnOutcome::Aborted { .. }),
            "a turn that had spent its steps stays spent: {outcome:?}"
        );
        assert!(
            provider.requests.lock().unwrap().is_empty(),
            "no model call may happen past the cap"
        );
    }

    /// The #1637 witness: a resumed turn's deliberate stop reaches the process
    /// boundary AS a deliberate stop.
    ///
    /// It drives the real resume path — `drive_resumed_turn` over a restored
    /// checkpoint, stopping at the step cap the crash was carrying — through
    /// the exact projection `run_resume` now ends with, and asserts the exit
    /// code is [`DELIBERATE_STOP_EXIT_CODE`] rather than the generic `1`.
    ///
    /// The fail half on `main` is type-level and therefore total: `run_resume`
    /// answered with a `String`, `turn_outcome_result` did not exist, and no
    /// value on this path could carry an [`AbortKind`] — so this test does not
    /// compile there, let alone pass. A full end-to-end `run_resume` witness
    /// would need a spawned supervised child, a session registry and a live
    /// provider; the composition below pins the same claim without them, and
    /// the last mile (`Err(CliFailure)` → `main`'s `e.exit_code()`) is the
    /// unconditional boundary `crate::failure`'s own tests already cover.
    ///
    /// [`AbortKind`]: stella_core::AbortKind
    /// [`DELIBERATE_STOP_EXIT_CODE`]: crate::failure::DELIBERATE_STOP_EXIT_CODE
    #[tokio::test]
    async fn a_resumed_deliberate_stop_exits_distinctly_from_a_crash() {
        let provider = ScriptedProvider {
            requests: Mutex::new(Vec::new()),
        };
        let tools = NoTools;
        let config = EngineConfig::default();
        let mut at_cap = killed_mid_turn();
        at_cap.step = config.max_steps;
        let state = TurnState::from_checkpoint(at_cap, &config);
        let engine = Engine::with_sleeper(&provider, &tools, config, &crate::runtime::TokioSleeper);
        let (tx, _rx) = mpsc::unbounded_channel::<AgentEvent>();
        let events = stella_core::EventSender::new(tx);

        let outcome = drive_resumed_turn(&engine, state, &events).await;

        assert!(
            matches!(
                outcome,
                TurnOutcome::Aborted {
                    kind: stella_core::AbortKind::DeliberateStop,
                    ..
                }
            ),
            "reaching the step cap is the engine stopping by policy: {outcome:?}"
        );
        let failure = turn_outcome_result(&outcome).expect_err("an aborted turn is not a success");
        assert_eq!(
            failure.exit_code(),
            std::process::ExitCode::from(crate::failure::DELIBERATE_STOP_EXIT_CODE),
            "a resumed stop must exit 3, the same code the un-resumed stop gives — \
             exiting 1 tells a wrapper the run fell over"
        );
    }
}
