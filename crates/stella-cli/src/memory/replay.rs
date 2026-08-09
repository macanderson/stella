//! The trace-replay learning harness — drive the learning machinery from
//! recorded traces, with zero model calls.
//!
//! Spec: `doc:trace-replay-learning-harness`. Epic #2304.
//!
//! Instead of sessions producing traces that feed learning, recorded traces
//! drive learning directly, and the harness watches what the agent builds —
//! memories, skills, rules, tools — across a simulated engagement.
//!
//! ## Why this needs no new architecture
//!
//! Every learner already sits below the model boundary or behind a port —
//! invariant 1 ("ports, not concretions") and invariant 2 ("no I/O in the
//! engine") paying out. Of the five learners, four make **no** model call at all
//! and the fifth makes exactly one, through the `Provider` port. So the whole
//! harness is one test double and a clock:
//!
//! | Learner | Model calls |
//! |---|---|
//! | Memory formation (`reflect_and_record`) | exactly one, via `Provider` |
//! | Skill learning (`mine_skill_candidates` → `decide_auto_creation`) | none |
//! | Rules mining (`extract_reflection_observations` → `induce_rule_proposals`) | none |
//! | Tool foundry (`detect_tool_gaps`) | none, **by design** |
//! | Selection / lifecycle | none |
//!
//! ## The loop is shorter than the spec's, and that is a finding
//!
//! §4.1 lists eight steps, of which it gives the replayer observation
//! extraction (5) and rule induction (6) as its own calls. They are **already
//! inside** `reflect_and_record`: `auto_create_skills_typed` runs uses
//! extraction, the retirement sweep, observation extraction, skill proposals and
//! rule induction in that order, from one pool of observations. Calling them
//! again from here would not corrupt anything — extraction is replay-idempotent
//! through content-derived record ids — but it would be a second mechanism
//! shadowing the shipped one, and the harness would then be certifying its own
//! wiring rather than the loop's. The replayer drives the seam the drivers drive
//! and lets the loop do the rest.
//!
//! ## Determinism contract (§4.2)
//!
//! - **No wall clock, no `Instant`, no pid.** #2320 put the learning plane on an
//!   injected clock; this harness sets it per turn and asserts the consequence
//!   rather than assuming it.
//! - **A per-replay temp workspace**, never the developer's `$HOME`.
//! - **Two replays of one trace produce byte-identical summaries** — an
//!   assertion, not a comment. Note what a summary may therefore *not* contain:
//!   an `edge.public_id` is minted from a process-global counter, so it differs
//!   between two replays inside one process. Compare edge content, never edge
//!   identity (see `stella_context`'s `process_nonce` audit note).

// §7: the Claude Code transcript adapter. Local-only and opt-in; it derives no
// lessons (option (b)), so it feeds the foundry and the timeline and never the
// reflection path.
pub(crate) mod cc_adapter;
pub(crate) mod summary;
pub(crate) mod trace;

#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use stella_context::{Clock, FixedClock};
use stella_core::tool_foundry::{GapDetectionConfig, ShellInvocation, detect_tool_gaps};
use stella_protocol::{
    CompletionMessage, CompletionRequestRef, CompletionResult, CompletionUsage, Provider,
    ProviderError,
};

use self::summary::ReplaySummary;
use self::trace::{ScriptedReflection, Trace, TraceError, TraceTurn};
use crate::memory::SessionMemory;

/// The single model boundary in the learning loop, stubbed.
///
/// One scripted answer per construction, because a `SessionMemory` reflection
/// turn makes exactly one call. A `ModelError` arm returns
/// `ProviderError::Terminal`, which is what a real terminal provider failure
/// looks like to `complete_standalone` — the path that produces
/// `ReflectionReport::model_error` and records nothing.
struct ScriptedProvider {
    /// `None` scripts a failed call.
    response: Option<String>,
    error: String,
    calls: Mutex<u32>,
}

impl ScriptedProvider {
    fn for_turn(reflection: &ScriptedReflection) -> Self {
        Self {
            response: reflection.as_response(),
            error: match reflection {
                ScriptedReflection::ModelError { message } => message.clone(),
                _ => String::new(),
            },
            calls: Mutex::new(0),
        }
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        "replay"
    }

    async fn complete_ref(
        &self,
        _request: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        *self.calls.lock().expect("call count") += 1;
        let Some(text) = self.response.clone() else {
            return Err(ProviderError::Terminal(self.error.clone()));
        };
        Ok(CompletionResult {
            text,
            tool_calls: Vec::new(),
            // `reported: true` is load-bearing. The standalone call's accounting
            // closeout refuses to settle a call whose usage never arrived, and
            // reflection surfaces that as a model error — indistinguishable,
            // from outside, from a model that said nothing. A harness whose
            // whole subject is "did anything get learned" cannot afford to
            // confuse those two.
            usage: CompletionUsage {
                reported: true,
                input_tokens: 1,
                ..CompletionUsage::default()
            },
            model: "replay".to_string(),
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// Whether a trace-supplied relative path could reach outside the workspace.
///
/// Rejects anything absolute, rooted, or containing a `..` component. A
/// component walk rather than a `starts_with` check, because `starts_with` is
/// lexical and `root/../escaped.txt` satisfies it.
fn escapes_root(relative: &str) -> bool {
    use std::path::Component;
    let path = Path::new(relative);
    path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
}

/// Drives one trace against one temp workspace.
pub(crate) struct Replayer<'a> {
    root: &'a Path,
    clock: std::sync::Arc<FixedClock>,
    /// Every shell invocation seen so far, in trace order. The foundry mines the
    /// whole history each turn (it is pure over the slice), which is what the
    /// live CLI adapter does when it reads the durable `tool_calls` receipt log.
    shell_history: Vec<ShellInvocation>,
    foundry: GapDetectionConfig,
}

impl<'a> Replayer<'a> {
    /// Replay `trace` against `root`, returning what the learners built.
    ///
    /// `root` must be a fresh directory the caller owns — a temp dir in
    /// practice. The harness seeds `.stella/` itself.
    pub(crate) async fn run(trace: &Trace, root: &'a Path) -> Result<ReplaySummary, TraceError> {
        let mut replayer = Replayer {
            root,
            // The first turn sets it anyway; starting at the trace's own first
            // instant keeps the store's own open-time writes on the timeline
            // rather than at the epoch.
            clock: std::sync::Arc::new(FixedClock::new(
                trace.turns().next().map(|(_, t)| t.at).unwrap_or(0),
            )),
            shell_history: Vec::new(),
            foundry: GapDetectionConfig::default(),
        };
        replayer.seed(trace)?;

        let mut summary = ReplaySummary::new(trace);
        for session in &trace.sessions {
            // One `SessionMemory` per trace session, because several pieces of
            // state are per-session by contract — the skill auto-creation cap
            // most visibly. Reusing one instance across sessions would model a
            // single endless session and silently relax that cap.
            //
            // `include_workspace_skills: true` models a trusted project. It is
            // not a convenience: with it false, `write_candidates` and
            // `induce_rules` both decline to write a FILE (#737), so a harness
            // that left it false would replay a whole corpus and correctly build
            // nothing — and read as a broken learner.
            let mut memory = SessionMemory::open_with_clock(
                root,
                false,
                true,
                replayer.clock.clone() as std::sync::Arc<dyn Clock>,
            )
            .ok_or_else(|| {
                TraceError::Unreplayable(format!(
                    "session memory would not open at {}",
                    root.display()
                ))
            })?;
            memory.set_task_id(&session.task_id);

            for turn in &session.turns {
                replayer.turn(&mut memory, turn, &mut summary).await;
            }
        }

        summary.observe_final_state(root, &replayer.shell_history, replayer.foundry);
        Ok(summary)
    }

    /// Write the trace's workspace seed to disk.
    fn seed(&self, trace: &Trace) -> Result<(), TraceError> {
        let unwritable =
            |what: &str, e: std::io::Error| TraceError::Unreplayable(format!("{what}: {e}"));
        std::fs::create_dir_all(self.root.join(".stella"))
            .map_err(|e| unwritable("cannot create .stella", e))?;

        if !trace.workspace.domains.is_empty() {
            let domains = crate::domains::Domains {
                version: 1,
                inferred_by: "replay".to_string(),
                domains: trace
                    .workspace
                    .domains
                    .iter()
                    .map(|name| crate::domains::Domain {
                        name: name.clone(),
                        description: String::new(),
                        paths: Vec::new(),
                    })
                    .collect(),
            };
            domains.save(self.root).map_err(TraceError::Unreplayable)?;
        }

        for (relative, contents) in &trace.workspace.files {
            // A seed path is fixture data, not model output, but it still must
            // not escape the temp root — a `..` in a committed fixture would
            // write into the developer's tree.
            //
            // Checked on components rather than with `Path::starts_with`, which
            // is *lexical*: `root/../escaped.txt` starts with `root` by that
            // test and sails straight through. Measured, not assumed — the
            // obvious version of this guard passed the fixture that escapes.
            if escapes_root(relative) {
                return Err(TraceError::Unreplayable(format!(
                    "seed path {relative:?} escapes the workspace root"
                )));
            }
            let path = self.root.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| unwritable("cannot seed tree", e))?;
            }
            std::fs::write(&path, contents).map_err(|e| unwritable("cannot seed file", e))?;
        }
        Ok(())
    }

    /// Apply this turn's forgetting and tree deletions.
    ///
    /// Best-effort in the same direction the loop is: a tombstone that will not
    /// write means the lesson is not suppressed, which the assertion will catch
    /// loudly. Silently swallowing it here would be worse than a panic, because
    /// the suite would then report a *passing* suppression it never applied.
    fn apply_world_changes(&self, turn: &TraceTurn) {
        if !turn.forget.is_empty()
            && let Ok(store) = stella_store::Store::open(self.root)
        {
            for (index, statement) in turn.forget.iter().enumerate() {
                let _ = store.forget(
                    stella_store::ContextSurface::Memory,
                    &format!("replay-forget-{}-{index}", turn.at),
                    statement,
                    "forgotten by the trace",
                );
            }
        }
        for relative in &turn.removed_files {
            if !escapes_root(relative) {
                let _ = std::fs::remove_file(self.root.join(relative));
            }
        }
    }

    /// One turn, in the order §4.1 fixes.
    async fn turn(
        &mut self,
        memory: &mut SessionMemory,
        turn: &TraceTurn,
        summary: &mut ReplaySummary,
    ) {
        // 1. The clock is the only time source in the system.
        self.clock.set(turn.at);

        // 1a. The world as it is at the top of this turn: what the user forgot,
        //     and what left the tree. Both happen *before* the reflection,
        //     which is where they land in a real engagement — you forget a
        //     lesson, then the next turn must not re-learn it.
        self.apply_world_changes(turn);

        // 2-4. The single model boundary, scripted — unless the source carried
        //      no reflection at all, in which case the turn does not reach it.
        //      Skipping is the whole of option (b) in §7.2: an adapter over a
        //      corpus with no reflection JSON supplies shell history, session
        //      boundaries and timing, and scripting a reflection it never saw
        //      would launder an invention into a result.
        //
        //      Everything downstream of the call — the forgetting filter, the
        //      store write, the mining log, observation extraction, both miners,
        //      the retirement sweep — is already deterministic and runs inside
        //      `reflect_and_record`.
        let report = if turn.reflection.reaches_the_model() {
            let provider = ScriptedProvider::for_turn(&turn.reflection);
            let transcript: Vec<CompletionMessage> = turn
                .transcript
                .iter()
                .map(CompletionMessage::from)
                .collect();
            memory
                .reflect_and_record(
                    &provider,
                    "replay",
                    &transcript,
                    true,
                    turn.succeeded,
                    None,
                    crate::memory::ReflectionPosture::default(),
                )
                .await
        } else {
            crate::memory::ReflectionReport::default()
        };

        // 5. The foundry, over the history accumulated so far. Pure over the
        //    slice and documented to be byte-identical for identical input, so
        //    it needs no adaptation at all.
        self.shell_history
            .extend(turn.shell.iter().map(ShellInvocation::from));
        let tools = detect_tool_gaps(&self.shell_history, self.foundry);

        // 6. Snapshot.
        summary.observe_turn(turn, &report, tools.len());
    }
}
