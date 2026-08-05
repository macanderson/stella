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
//! The staleness map rides the same checkpoint commit, so the no-clobber
//! guard survives the crash too — a file something else edited while the run
//! was dead is refused, not overwritten ([`crate::durability::bind_session`]).
//!
//! # What resuming honestly costs
//!
//! Three things, said here rather than discovered:
//!
//! - **The system prompt is the checkpointed one, byte for byte.** The deck's
//!   turn-boundary resume regenerates it (rules may have changed); mid-turn,
//!   fidelity wins — the transcript the provider priced and cached is the
//!   transcript it gets back.
//! - **A turn interrupted mid-pipeline resumes as a plain engine turn.** The
//!   checkpoint captures the worker's turn, not the pipeline's staging, so
//!   the verify/verdict stages that would have followed do not re-run. The
//!   resumed turn still answers, and says what it did; re-verifying it is
//!   `stella run` with the verifier of your choice.
//! - **A turn that executed in a candidate worktree resumes in the
//!   workspace.** The candidate died with the process; the restored staleness
//!   map is what keeps the resumed writes honest.

use super::*;

/// The child driver: adopt the record, rebuild the turn, drive it to an end.
///
/// `id` is the record to resume (a unique prefix is enough); `None` falls
/// back to this process's own supervised identity, which is how the spawned
/// child path always arrives.
pub(crate) async fn run_resume(cfg: &Config, id: Option<&str>) -> Result<(), String> {
    let registry = stella_store::SessionRegistry::open_default();
    let record = match (id, crate::daemon::supervised_id()) {
        (Some(id), _) => crate::daemon::resolve(&registry, Some(id))?,
        (None, Some(own)) => crate::daemon::resolve(&registry, Some(&own))?,
        (None, None) => return Err("daemon resume needs a run id".to_string()),
    };
    // The hand-run `--foreground` guard: a resumed turn must run where its
    // work is. The spawned child always passes (the parent pinned its cwd).
    let recorded = std::fs::canonicalize(&record.workspace).unwrap_or_default();
    let current = std::fs::canonicalize(&cfg.workspace_root).unwrap_or_default();
    if recorded != current {
        return Err(format!(
            "{} belongs to {} — resume it from there, or without --foreground",
            record.id, record.workspace
        ));
    }

    let provider = build_provider(cfg)?;
    let registry_options = registry_options(cfg);
    let tools_registry: std::sync::Arc<ToolRegistry> =
        std::sync::Arc::new(new_tool_registry(cfg.workspace_root.clone(), registry_options).await);
    populate_schema_index(&tools_registry, &cfg.workspace_root)?;
    crate::subagent::install_for_session(cfg, &tools_registry)?;
    let _active_rules =
        crate::rules::enforce_workspace_rules(&tools_registry, &cfg.workspace_root, &cfg.authority);
    let (_session_graph, _graph_build) = spawn_session_graph(
        &cfg.workspace_root,
        tools_registry.clone(),
        Box::new(|line| eprintln!("  {line}")),
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
    // both how the checkpoint is found and how the staleness map is restored
    // before anything can touch a file.
    let record =
        crate::session_persist::adopt_record(record, stella_store::SessionStatus::InProgress);
    registry
        .upsert(&record)
        .map_err(|e| format!("cannot re-own session {}: {e}", record.id))?;
    if let Some(warning) = crate::durability::bind_session(
        &cfg.durability,
        &tools_registry,
        &cfg.workspace_root,
        &record.id,
    ) {
        eprintln!("  {warning}");
    }
    let Some(json) = cfg.durability.checkpoint() else {
        return Err(format!(
            "{} has no resume point. A turn leaves one only when its process was killed \
             mid-turn; a run that completed, was stopped, or aborted discards it on the \
             way out",
            record.id
        ));
    };
    let checkpoint = stella_core::step::Checkpoint::from_json(&json).map_err(|e| {
        format!(
            "{}'s resume point cannot be read by this build ({e}); refusing to resume a \
             turn from a shape this build half-recognizes",
            record.id
        )
    })?;

    let turn_start = Instant::now();
    let step = checkpoint.step;
    eprintln!(
        "  resuming {} at step {step} — completed steps stay done, nothing re-runs",
        record.id
    );
    let execution = begin_execution(&store, "resume", &record.title, cfg, Some(&record.id));
    let files_before = tools_registry.files_touched().len();
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
    );

    let outcome = {
        let customs = CustomToolSet::new(
            base_tools,
            custom_tools.to_vec(),
            cfg.workspace_root.clone(),
        );
        let interactive = InteractiveToolSet::new(&customs, tx.clone(), default_ask_io(true))
            .with_skill_registry(SkillRegistry::from_env(cfg.workspace_root.clone()));
        let permitted = PolicyToolSet::new(&interactive, session_tool_policy(cfg));
        let tools = crate::discovery::DiscoveryToolSet::new(&permitted, cfg.workspace_root.clone())
            .with_project_prompts_allowed(cfg.authority.project_prompts_allowed);
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

    // The canonical teardown (#960): detach the registry's sender clones,
    // drop ours, and only then await the renderer — otherwise the channel
    // never closes and a completed resume hangs.
    drop(tx);
    let persistence_complete = close_event_stream(&tools_registry, events, renderer)
        .await
        .persistence_complete;
    let files = tools_registry.files_touched();
    if let Some((store, id)) = &execution {
        let (label, cost) = match &outcome {
            TurnOutcome::Completed { cost_usd, .. } => ("resumed_complete", *cost_usd),
            TurnOutcome::Aborted { cost_usd, .. } => ("resumed_aborted", *cost_usd),
        };
        if !record_execution_end(
            store,
            *id,
            &tools_registry,
            files_before,
            label,
            cost,
            persistence_complete,
        ) {
            warn_store_write_failed(
                "the audit record (files touched / memory citations / outcome)",
            );
        }
    }
    tui::files_touched_panel(&files);
    if let Some(set) = &mcp {
        set.close_all().await;
    }

    let result = match outcome {
        TurnOutcome::Completed { cost_usd, .. } => {
            println!(
                "\n  {} resumed turn completed (from step {step})",
                "✓".green().bold()
            );
            tui::cost_summary(
                cost_usd,
                &format!("{}/{}", cfg.provider.id, cfg.model_id),
                turn_start.elapsed(),
            );
            Ok(())
        }
        TurnOutcome::Aborted {
            reason, cost_usd, ..
        } => {
            tui::cost_summary(
                cost_usd,
                &format!("{}/{}", cfg.provider.id, cfg.model_id),
                turn_start.elapsed(),
            );
            Err(reason)
        }
    };
    // Written here as well as by `record_outcome_if_supervised` (which only
    // covers the spawned child), so the hand-run `--foreground` case records
    // a terminal status too. Same value on both paths; double-writing it is
    // harmless, leaving it unwritten reads as a crash forever.
    let _ = registry.set_status(&record.id, crate::daemon::outcome_status(result.is_ok()));
    result
}

/// Drive a checkpoint-restored turn to an ordinary end — the host loop
/// `Engine::run_turn_with_sender` runs for a fresh turn, replayed over a
/// [`TurnState`] that begins where the killed process's last committed step
/// left off.
///
/// The same three obligations as the engine's own loop, in the same places:
/// persist on every `Continue` (a resumed run must stay resumable), discard
/// on every terminal path (a checkpoint that outlives its turn invites
/// resuming finished work), and gate on the step cap the state carried
/// across the crash — a turn killed at step 37 of 40 gets 3 more, not 40.
///
/// [`TurnState`]: stella_core::step::TurnState
pub(crate) async fn drive_resumed_turn(
    engine: &Engine<'_>,
    mut state: stella_core::step::TurnState,
    events: &stella_core::EventSender,
) -> TurnOutcome {
    use stella_core::step::StepOutcome;
    let _ = events.send(AgentEvent::Stage {
        name: stella_protocol::StageKind::Execute,
    });
    loop {
        if state.step() >= engine.max_steps() {
            let reason = stella_core::driver::step_cap_reason(engine.max_steps());
            let _ = events.send(AgentEvent::Error {
                message: reason.clone(),
                retryable: false,
            });
            engine.discard_checkpoint();
            return TurnOutcome::Aborted {
                reason,
                kind: stella_core::step::AbortKind::DeliberateStop,
                cost_usd: state.total_cost_usd(),
            };
        }
        match engine.run_step(&mut state, events).await {
            StepOutcome::Continue => engine.persist_checkpoint(&state),
            StepOutcome::Done { text, cost_usd } => {
                engine.discard_checkpoint();
                return TurnOutcome::Completed { text, cost_usd };
            }
            StepOutcome::Aborted {
                reason,
                kind,
                cost_usd,
            } => {
                engine.discard_checkpoint();
                return TurnOutcome::Aborted {
                    reason,
                    kind,
                    cost_usd,
                };
            }
        }
    }
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
            ToolOutput::Error {
                message: format!("no tool {name} in this test"),
            }
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
            transcript_rewrites: 0,
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
                .any(|r| matches!(&r.output, ToolOutput::Ok { content } if content == "the file's contents"))),
            "the completed step's tool result must arrive as transcript, not be re-executed"
        );
        assert_eq!(
            transcript[0].content, "the original system prompt",
            "mid-turn fidelity: the checkpointed system prompt rides verbatim"
        );

        // A finished resume discards its checkpoint — leaving it would offer
        // to resume a turn everyone watched complete.
        assert_eq!(sink.discards.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            sink.persists.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a turn that finishes on its first resumed step has no new boundary to persist"
        );

        // The renderer's contract: the resumed turn frames itself as an
        // execute stage before its first event.
        let first = rx.recv().await.expect("at least the stage event");
        assert!(matches!(
            first,
            AgentEvent::Stage {
                name: stella_protocol::StageKind::Execute
            }
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
}
