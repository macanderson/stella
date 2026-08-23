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
//!   [`ApprovalRoute`] port — implemented over the #2676
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
//! `stop_hook_consults` against [`EngineConfig::stop_holds`](super::config::EngineConfig::stop_holds), counted BEFORE
//! the hooks run — caps how many rounds. Without it, a hook that always denies
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
//! [`EngineConfig::stop_holds`](super::config::EngineConfig::stop_holds) held-open rounds now, not one), and the last
//! permitted round says it is the last ([`STOP_FINAL_ROUND_NOTE`]) — the
//! #2810 lesson that a bound nobody announces reads as an infinite
//! allowance from inside the loop. A Stop hook that *fails* (non-zero
//! exit, spawn failure) never blocks — deliberately the opposite of the
//! PreToolUse posture, because failing closed HERE means never completing,
//! which is the spiral, not safety; the failure surfaces as a diagnostic
//! instead.
//!
//! The bound was a hardcoded 3 until #3380, which is the same shape one layer
//! up: an extension that genuinely needs a fourth round could not ask for one.
//! It is now [`EngineConfig::stop_hold_allowance`](super::config::EngineConfig::stop_hold_allowance) — a host's number, clamped
//! by the engine to [`STOP_HOLD_CEILING`](super::config::STOP_HOLD_CEILING) every time it is read, because the
//! declaration a plugin manifest makes (`LoopGrant::max_holds`) is an *ask*
//! and a manifest must never be able to buy an unbounded loop.
//!
//! A `require_approval` decision parks the completion on the same
//! [`ApprovalRoute`] a `PreToolUse` hook's does, carrying
//! [`ApprovalSubject::TurnCompletion`] rather than a tool (#3486). Approved,
//! the completion stands; denied, the refusal becomes the model's next
//! observation on the `deny` path above and is bounded by the same counter.
//! **With no route attached it fails open** — the completion stands and the
//! ask is reported as a diagnostic — for the reason this section already
//! gives: refusing to complete because nobody was there to answer is the
//! spiral, not safety.
//!
//! # A denial is structured (#3380)
//!
//! [`crate::bus::HookDecision::Deny`] carries a [`Denial`], so a verifying
//! extension names the witness, the argv it ran, the tri-state flip and the
//! digest it judged. Both consumers here branch on those fields rather than
//! on prose: the tail message the model reads renders them as a checklist it
//! can act on, and the `hook.stop.blocked` lifecycle payload carries the
//! evidence object whole for the trace fold. `None` evidence is "this hook
//! does not verify" and renders exactly as it always did.
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
use stella_protocol::{AgentEvent, Denial, DenialEvidence, ToolCall, ToolOutput};

use super::{Engine, HooksHandle};
use crate::bus::names as bus_names;
use crate::event_sender::EventSender;
use crate::hooks::decision::{
    ApprovalRoute, ApprovalRouteRequest, ApprovalRouteResolution, ApprovalSubject, GateVerdict,
    OperatorPosture, run_decision_hooks,
};
use crate::hooks::{HookEvent, HookPayload, run_hooks};

/// Prefix of the tail user-role message a blocking `Stop` hook injects.
/// User-role on the wire but engine-written, so it is registered in
/// [`crate::engine_markers::ENGINE_MARKERS`] and classified as steering by
/// `receipts::user_block_kind` — never attributed to the person.
pub(crate) const STOP_HOOK_MARKER_PREFIX: &str = "[stop-hook feedback";

/// Appended to the last permitted round's `deny` reason, so the model and
/// the transcript both know the next completion stands unchecked. #2810's
/// lesson, restated for this gate: a bound nobody announces reads as an
/// infinite allowance from inside the loop.
pub(crate) const STOP_FINAL_ROUND_NOTE: &str =
    "\n\n[final verification round — the next completion will stand without another check]";

/// The model-visible text for one `Stop` denial: the hook's prose, plus a
/// checklist of whatever structured evidence it attached (#3380).
///
/// Rendering happens here rather than in `stella-protocol` because how a
/// denial reads to a model is the engine's presentation choice, and the types
/// crate holds no view (invariant #1: zero logic there). The lines are
/// deliberately terse and field-per-line so the model can act on each one;
/// the same evidence rides the `hook.stop.blocked` payload structurally for
/// anything that needs to branch instead of read.
///
/// `command` renders as a JSON array, not as a joined string, because that is
/// what it is: argv, where an argument containing a space must stay
/// distinguishable from two arguments.
fn render_denial(denial: &Denial) -> String {
    let mut text = denial.reason.clone();
    let Some(evidence) = &denial.evidence else {
        return text;
    };
    let DenialEvidence {
        witness,
        command,
        flip,
        digest,
    } = evidence;
    text.push_str("\n\nverification evidence:");
    if let Some(witness) = witness {
        text.push_str(&format!("\n- witness: {witness}"));
    }
    if !command.is_empty() {
        let argv = serde_json::to_string(command).unwrap_or_else(|_| command.join(" "));
        text.push_str(&format!("\n- command: {argv}"));
    }
    // Always stated, including `unobserved`: "nothing was measured" and
    // "measured and came back short" are different findings, and a reader
    // who has to infer the difference from an absent line is the exact
    // conflation `FlipOutcome` exists to prevent.
    text.push_str(&format!("\n- flip: {}", flip.as_str()));
    if let Some(digest) = digest {
        text.push_str(&format!("\n- digest: {digest}"));
    }
    text
}

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
    pub fn with_hook_approval_route(mut self, route: &'a dyn ApprovalRoute) -> Self {
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
                let request = ApprovalRouteRequest {
                    subject: ApprovalSubject::Tool {
                        name: call.name.clone(),
                        read_only,
                    },
                    reason,
                };
                match route.resolve(&request).await {
                    ApprovalRouteResolution::Approved => {}
                    ApprovalRouteResolution::Denied { reason } => {
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
    /// has still consumed its round. The allowance is
    /// [`EngineConfig::stop_holds`](super::config::EngineConfig::stop_holds) — the host's ask, already clamped — and
    /// the last permitted `deny` carries [`STOP_FINAL_ROUND_NOTE`] so the
    /// model knows the next completion stands unchecked.
    pub(super) async fn stop_hook_feedback(
        &self,
        final_text: &str,
        stop_consults: &mut u32,
        events: &EventSender,
    ) -> Option<String> {
        let handle = self.hooks?;
        let allowance = self.config.stop_holds();
        if handle.hooks.matchers_for(HookEvent::Stop).is_empty() || *stop_consults >= allowance {
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
            Ok(crate::bus::HookDecision::Deny(denial)) => {
                // The last permitted round announces itself (#2810's lesson):
                // composed here rather than at the injection site so the
                // lifecycle event below and the tail message carry one text.
                let mut reason = render_denial(&denial);
                if *stop_consults >= allowance {
                    reason.push_str(STOP_FINAL_ROUND_NOTE);
                }
                self.emit_lifecycle(bus_names::HOOK_STOP_BLOCKED, || {
                    // The evidence rides structurally as well as in the prose:
                    // a trace fold reading this payload must be able to answer
                    // "did the flip happen?" without parsing the message the
                    // model was shown.
                    serde_json::json!({ "reason": reason, "evidence": denial.evidence })
                });
                Some(reason)
            }
            Ok(crate::bus::HookDecision::RequireApproval { reason }) => {
                // #3486. `ApprovalRouteRequest` used to be keyed on a tool
                // name and a `read_only` bit, so asking here would have meant
                // inventing a tool that does not exist and putting it in the
                // audit trail; the verb was reported as inapplicable instead.
                // It carries an `ApprovalSubject` now, and a turn boundary has
                // one of its own.
                let Some(route) = self.hook_approvals else {
                    // **Fail open, and the module docs § "The Stop gate" argue
                    // it.** Failing closed at a turn boundary is not caution:
                    // it is the compact→error→stop-hook→retry spiral, on a
                    // question nobody was asked. A headless run completes.
                    self.surface_hook_diagnostics(
                        "Stop",
                        &[format!(
                            "a Stop hook asked for a human decision ({reason}), and no \
                             interactive surface is attached to answer it; completion allowed"
                        )],
                        events,
                    );
                    return None;
                };
                let request = ApprovalRouteRequest {
                    subject: ApprovalSubject::TurnCompletion {
                        final_text_digest: crate::receipts::sha256_hex_prefixed(final_text),
                    },
                    reason,
                };
                match route.resolve(&request).await {
                    // Approved: the completion stands, exactly as an `allow`
                    // would have left it.
                    ApprovalRouteResolution::Approved => None,
                    // Denied: the turn stays open and the refusal is the
                    // model's next observation — the same tail-message path a
                    // `deny` takes, and bounded by the same consultation
                    // allowance, so a surface that keeps refusing cannot loop
                    // the turn forever.
                    ApprovalRouteResolution::Denied { reason } => {
                        let mut reason =
                            format!("a Stop hook held this turn open for approval: {reason}");
                        if *stop_consults >= allowance {
                            reason.push_str(STOP_FINAL_ROUND_NOTE);
                        }
                        self.emit_lifecycle(bus_names::HOOK_STOP_BLOCKED, || {
                            serde_json::json!({ "reason": reason, "evidence": Vec::<String>::new() })
                        });
                        Some(reason)
                    }
                }
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
            // Prose only: a compaction veto is a scheduling decision about
            // this turn's transcript, so a witness/flip/digest has nothing to
            // say about it. A hook that attaches evidence here is answering a
            // question that was not asked, and it is dropped rather than
            // rendered into a summarizer's ear.
            Ok(crate::bus::HookDecision::Deny(denial)) => {
                let reason = denial.reason;
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
