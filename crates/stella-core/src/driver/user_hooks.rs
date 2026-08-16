//! The settings-declared user-hook surface on the turn path (#2684): the
//! decision-aware `PreToolUse`/`PostToolUse` dispatch wrapper (moved out of
//! `driver.rs`, which is closed to growth), the `Stop` completion gate, and
//! the `PreCompact` summarization gate. A child module of `driver` so the
//! engine internals stay reachable.
//!
//! # PreToolUse is a permission gate now
//!
//! [`Engine::execute_with_hooks`] runs the matched hooks through
//! [`crate::hooks::decision::run_decision_hooks`], so a hook's stdout JSON
//! carries the extension bus's own vocabulary
//! ([`crate::bus::HookDecision`]) — one enum, no shell-surface fork:
//!
//! - `modify` rewrites the input the tool actually receives (and the
//!   `PostToolUse` payload reports the input the tool saw, not the one the
//!   model sent);
//! - `deny` blocks with the hook's reason;
//! - `require_approval` parks the dispatch on the
//!   [`HookApprovalRoute`] port — implemented over the #2676
//!   `ApprovalBroker` by `stella-tools::hook_bridge`, so the emit-before-
//!   park / TTL / audit-event contract is the broker's, not a copy. With
//!   no route attached the call is refused with a grant-path message, the
//!   same headless posture the broker takes;
//! - an evaluation failure — non-zero exit, spawn failure, malformed
//!   decision — denies unconditionally through
//!   [`resolve_precedence`](crate::hooks::decision::resolve_precedence),
//!   whatever any enforcement-softening flag says
//!   (OXA-2056). This preserves the legacy contract (non-zero exit
//!   blocks) while routing it through the one precedence ladder.
//!
//! # The Stop gate and its bounded consultation counter
//!
//! [`Engine::stop_hook_feedback`] runs when a turn is about to complete
//! (`driver::completion`). A `deny` decision holds the turn open: the
//! hook's reason lands as a tail user-role message under
//! [`STOP_HOOK_MARKER_PREFIX`] (registered in
//! [`crate::engine_markers::ENGINE_MARKERS`]) and the model gets another
//! chance to act on it. The bound — [`crate::step::TurnState`]'s
//! `stop_hook_consults` against [`MAX_STOP_CONSULTS`], counted BEFORE the
//! hooks run — caps how many rounds. Without it, a hook that always denies
//! chains completion-attempt → feedback → completion-attempt forever (the
//! compact→error→stop-hook→retry death spiral: every held-open round can
//! grow the transcript, force compaction, and error its way back to
//! another completion attempt).
//!
//! The bound was a once-per-turn boolean until #3246: fire once, whatever
//! the decision. That shape structurally forbids the gate's own primary use
//! case — a verification hook must deny a wrong answer, watch the revision,
//! and *re-check*, and a fail→pass observation is by definition two
//! consultations. So the latch became a counter, with two properties the
//! boolean version already taught: the spiral stays capped (at
//! [`MAX_STOP_CONSULTS`] held-open rounds now, not one), and the last
//! permitted round says it is the last ([`STOP_FINAL_ROUND_NOTE`]) — the
//! #2810 lesson that a bound nobody announces reads as an infinite
//! allowance from inside the loop. A Stop hook that *fails* (non-zero
//! exit, spawn failure) never blocks — deliberately the opposite of the
//! PreToolUse posture, because failing closed HERE means never completing,
//! which is the spiral, not safety; the failure surfaces as a diagnostic
//! instead.
//!
//! # The PreCompact gate
//!
//! [`Engine::pre_compact_ruling`] runs before each overflow-summarization
//! round (`Engine::run_compaction_pass`): `deny` vetoes the round (the
//! turn proceeds un-summarized, at the operator's own risk of a later
//! hard overflow — the reactive recovery of `driver::overflow_recovery`
//! still applies), and `modify` with an `instructions` string steers the
//! summarizer's prompt. A failing PreCompact hook proceeds un-vetoed, for
//! the same reason a failing Stop hook cannot block: a broken hook must
//! not starve compaction and wedge the turn into a hard overflow.

use serde_json::Value;
use stella_protocol::{AgentEvent, ToolCall, ToolOutput};

use super::{Engine, HooksHandle};
use crate::bus::names as bus_names;
use crate::event_sender::EventSender;
use crate::hooks::decision::{
    GateVerdict, HookApprovalRequest, HookApprovalResolution, HookApprovalRoute, OperatorPosture,
    run_decision_hooks,
};
use crate::hooks::{HookEvent, HookPayload, run_hooks};

/// Prefix of the tail user-role message a blocking `Stop` hook injects.
/// User-role on the wire but engine-written, so it is registered in
/// [`crate::engine_markers::ENGINE_MARKERS`] and classified as steering by
/// `receipts::user_block_kind` — never attributed to the person.
pub(crate) const STOP_HOOK_MARKER_PREFIX: &str = "[stop-hook feedback";

/// How many times one turn's `Stop` hooks may be consulted — the bound on
/// the deny → revise → re-check loop (module docs).
///
/// Three, not one, because the gate's primary tenant is verification
/// (#3246): a verifier needs at least two consultations to observe a
/// fail→pass flip, and the third admits one more revision round — the
/// empirically common case of a model landing the fix within three
/// attempts. Not configurable yet on purpose: a knob here is an invitation
/// to uncap the spiral, and no caller has demonstrated a need.
pub(crate) const MAX_STOP_CONSULTS: u32 = 3;

/// Appended to the last permitted round's `deny` reason, so the model and
/// the transcript both know the next completion stands unchecked. #2810's
/// lesson, restated for this gate: a bound nobody announces reads as an
/// infinite allowance from inside the loop.
pub(crate) const STOP_FINAL_ROUND_NOTE: &str =
    "\n\n[final verification round — the next completion will stand without another check]";

/// What a `PreCompact` hook run decided about the imminent summarization
/// round.
#[derive(Debug, Default, PartialEq)]
pub(super) struct PreCompactRuling {
    /// `Some(reason)`: the round is vetoed and must not run.
    pub(super) veto_reason: Option<String>,
    /// Operator instructions for the summarizer, from a `modify`
    /// decision's `instructions` field.
    pub(super) instructions: Option<String>,
}

impl<'a> Engine<'a> {
    /// Attach the approval route a `PreToolUse` hook's `require_approval`
    /// decision parks on — `stella-tools::hook_bridge`'s broker-backed
    /// implementation in production. Opt-in like every other seam on this
    /// builder; an engine without one refuses such calls with a grant-path
    /// message instead of asking.
    pub fn with_hook_approval_route(mut self, route: &'a dyn HookApprovalRoute) -> Self {
        self.hook_approvals = Some(route);
        self
    }

    /// The `read_only` bit `name` advertises on this engine's executor —
    /// `false` for a name the executor does not advertise, the cautious
    /// direction. Walks the full schema list, so hot paths thread the
    /// precomputed per-step set instead; this is for cold paths (the
    /// parked-wait probe replay).
    pub(super) fn advertised_read_only(&self, name: &str) -> bool {
        self.tools
            .schemas()
            .iter()
            .any(|schema| schema.name == name && schema.read_only)
    }

    /// One tool dispatch, unbounded — the body `Engine::execute_with_repair`
    /// wraps in the timeout backstop. `read_only` is the tool's advertised
    /// bit, threaded from the caller's schema snapshot so the hook payload
    /// can carry it without re-walking `schemas()` per call.
    pub(super) async fn dispatch_tool_call(
        &self,
        call: &ToolCall,
        read_only: bool,
        events: Option<&EventSender>,
    ) -> ToolOutput {
        if call.input.is_null() {
            return ToolOutput::error(format!(
                "malformed tool call: `{}`'s arguments were not valid JSON (the model's \
                     streamed output didn't parse) — retry this call with well-formed JSON \
                     arguments",
                call.name
            ));
        }
        match self.hooks {
            None => self.tools.execute(&call.name, &call.input).await,
            Some(handle) => {
                self.execute_with_hooks(handle, call, read_only, events)
                    .await
            }
        }
    }

    /// Wrap a single (well-formed) executor invocation in its `PreToolUse` /
    /// `PostToolUse` hooks. Only reached when hooks are attached.
    ///
    /// `PreToolUse` fires first as a decision gate (module docs): a denied
    /// or evaluation-failed call is NOT executed and the model sees a
    /// `ToolOutput::Error` naming the block, exactly as the engine surfaces
    /// every other tool failure as model-visible data rather than an engine
    /// error; a `require_approval` parks on the route port; a `modify`
    /// rewrites the input the tool receives. Otherwise the tool runs and
    /// `PostToolUse` fires as a pure observation — its outcome can never
    /// block or alter the result, so a failing post-hook cannot abort the
    /// turn. Non-blocking failures from either phase surface as one
    /// non-fatal `Error { retryable: true }` on the turn stream when an
    /// event channel is present.
    async fn execute_with_hooks(
        &self,
        handle: HooksHandle<'a>,
        call: &ToolCall,
        read_only: bool,
        events: Option<&EventSender>,
    ) -> ToolOutput {
        let pre = run_decision_hooks(
            handle.runner,
            Some(handle.hooks),
            &HookPayload::pre_tool_use(
                self.config.cwd.clone(),
                &call.name,
                call.input.clone(),
                read_only,
            ),
        )
        .await;
        // The engine carries no operator posture of its own (operator tool
        // switches act upstream by withholding the tool — see
        // `OperatorPosture`) and no softening switch; producers that carry
        // either feed the same ladder with them.
        match pre.verdict(&OperatorPosture::NoOpinion, false) {
            GateVerdict::Deny { reason } => {
                return ToolOutput::error(format!(
                    "tool `{}` was blocked by a PreToolUse hook: {reason}",
                    call.name
                ));
            }
            GateVerdict::RequireApproval { reason } => {
                let Some(route) = self.hook_approvals else {
                    return ToolOutput::error(format!(
                        "tool `{}` requires approval — a PreToolUse hook asked for a human \
                             decision ({reason}), but no interactive surface is attached to \
                             answer it; grant the call via policy or rerun interactively",
                        call.name
                    ));
                };
                let request = HookApprovalRequest {
                    tool: call.name.clone(),
                    read_only,
                    reason,
                };
                match route.resolve(&request).await {
                    HookApprovalResolution::Approved => {}
                    HookApprovalResolution::Denied { reason } => {
                        return ToolOutput::error(format!(
                            "tool `{}` requires approval — {reason}",
                            call.name
                        ));
                    }
                }
            }
            GateVerdict::Allow => {}
        }
        let mut diagnostics = pre.diagnostics;
        // The input the tool actually receives — a `modify` rewrite, or the
        // model's original. `PostToolUse` reports this same value: the hook
        // observes what ran, not what was asked.
        let input: Value = pre.rewritten_input.unwrap_or_else(|| call.input.clone());

        let output = self.tools.execute(&call.name, &input).await;

        let result_str = match &output {
            ToolOutput::Ok { content, .. } => content.clone(),
            ToolOutput::Error { message, .. } => message.clone(),
        };
        // Observation only — a non-zero PostToolUse exit never blocks or
        // rewrites the result; its failures ride `diagnostics` instead.
        let post = run_hooks(
            handle.runner,
            Some(handle.hooks),
            &HookPayload::post_tool_use(
                self.config.cwd.clone(),
                &call.name,
                input,
                read_only,
                result_str,
            ),
        )
        .await;
        diagnostics.extend(post.diagnostics);

        if !diagnostics.is_empty()
            && let Some(events) = events
        {
            let _ = events.send(AgentEvent::Error {
                message: format!(
                    "hook problem(s) around tool `{}` (non-blocking): {}",
                    call.name,
                    diagnostics.join("; ")
                ),
                retryable: true,
            });
        }

        output
    }

    /// The `Stop` gate: `Some(reason)` when a hook held the completing turn
    /// open — the caller (`driver::completion`) injects the marked tail
    /// message and keeps stepping. `None` lets the completion stand.
    ///
    /// `stop_consults` is the turn's bounded consultation counter (module
    /// docs), spent BEFORE the hooks run so a hook that fails mid-flight
    /// has still consumed its round. The last permitted `deny` carries
    /// [`STOP_FINAL_ROUND_NOTE`] so the model knows the next completion
    /// stands unchecked.
    pub(super) async fn stop_hook_feedback(
        &self,
        final_text: &str,
        stop_consults: &mut u32,
        events: &EventSender,
    ) -> Option<String> {
        let handle = self.hooks?;
        if handle.hooks.matchers_for(HookEvent::Stop).is_empty()
            || *stop_consults >= MAX_STOP_CONSULTS
        {
            return None;
        }
        *stop_consults += 1;
        let run = run_decision_hooks(
            handle.runner,
            Some(handle.hooks),
            &HookPayload::stop(self.config.cwd.clone(), final_text),
        )
        .await;
        self.surface_hook_diagnostics("Stop", &run.diagnostics, events);
        match run.evaluation {
            Ok(crate::bus::HookDecision::Deny { reason }) => {
                // The last permitted round announces itself (#2810's lesson):
                // composed here rather than at the injection site so the
                // lifecycle event below and the tail message carry one text.
                let reason = if *stop_consults >= MAX_STOP_CONSULTS {
                    format!("{reason}{STOP_FINAL_ROUND_NOTE}")
                } else {
                    reason
                };
                self.emit_lifecycle(
                    bus_names::HOOK_STOP_BLOCKED,
                    || serde_json::json!({ "reason": reason }),
                );
                Some(reason)
            }
            Ok(crate::bus::HookDecision::RequireApproval { .. }) => {
                // No call exists to approve at a turn boundary; surfaced so
                // the hook author learns the verb is inapplicable here.
                self.surface_hook_diagnostics(
                    "Stop",
                    &[
                        "a Stop hook returned `require_approval`, which does not apply at a turn \
                       boundary; completion allowed"
                            .to_string(),
                    ],
                    events,
                );
                None
            }
            Ok(_) => None,
            // A broken Stop hook must not wedge the turn into never
            // completing (module docs): diagnostic, never a block.
            Err(failure) => {
                self.surface_hook_diagnostics("Stop", &[failure.to_string()], events);
                None
            }
        }
    }

    /// The `PreCompact` gate, consulted before each overflow-summarization
    /// round (`Engine::run_compaction_pass`).
    pub(super) async fn pre_compact_ruling(&self, events: &EventSender) -> PreCompactRuling {
        let Some(handle) = self.hooks else {
            return PreCompactRuling::default();
        };
        if handle.hooks.matchers_for(HookEvent::PreCompact).is_empty() {
            return PreCompactRuling::default();
        }
        let run = run_decision_hooks(
            handle.runner,
            Some(handle.hooks),
            &HookPayload::pre_compact(self.config.cwd.clone()),
        )
        .await;
        self.surface_hook_diagnostics("PreCompact", &run.diagnostics, events);
        match run.evaluation {
            Ok(crate::bus::HookDecision::Deny { reason }) => {
                self.emit_lifecycle(
                    bus_names::HOOK_PRE_COMPACT_VETOED,
                    || serde_json::json!({ "reason": reason }),
                );
                PreCompactRuling {
                    veto_reason: Some(reason),
                    instructions: None,
                }
            }
            Ok(_) => PreCompactRuling {
                veto_reason: None,
                instructions: run
                    .modify_payload
                    .as_ref()
                    .and_then(|payload| payload.get("instructions"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            },
            // A broken PreCompact hook must not starve compaction into a
            // hard overflow (module docs): diagnostic, proceed un-vetoed.
            Err(failure) => {
                self.surface_hook_diagnostics("PreCompact", &[failure.to_string()], events);
                PreCompactRuling::default()
            }
        }
    }

    /// One non-fatal diagnostic event for a turn-boundary hook's problems —
    /// the same never-silent posture the per-tool dispatch takes.
    fn surface_hook_diagnostics(&self, event: &str, diagnostics: &[String], events: &EventSender) {
        if diagnostics.is_empty() {
            return;
        }
        let _ = events.send(AgentEvent::Error {
            message: format!(
                "{event} hook problem(s) (non-blocking): {}",
                diagnostics.join("; ")
            ),
            retryable: true,
        });
    }
}
