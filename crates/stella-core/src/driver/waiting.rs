//! The parked wait's engine half (#1471): drain a tool's
//! [`WaitRequest`](crate::waiting::WaitRequest) at the step boundary and
//! probe on the engine's own clock until the watched state changes — zero
//! model calls, zero transcript growth, and the byte-stable prompt prefix
//! untouched while the turn sleeps. A child module of `driver` (the
//! `settlement.rs` pattern) so the engine internals stay reachable without
//! growing `driver.rs` past the size gate.
//!
//! The decision arithmetic — what counts as a change, how many polls a
//! deadline affords, what the wake message says — is pure and lives in
//! [`crate::waiting`]; this module owns only the loop that drives it through
//! the engine's existing ports (the tool executor for probes, the sleeper
//! for intervals, the cancel token and soft-stop latch for interruption).

use stella_protocol::{AgentEvent, CompletionMessage, ToolCall, ToolOutput};

use super::Engine;
use crate::event_sender::EventSender;
use crate::step::{StepOutcome, TurnState};
use crate::waiting::{WaitCall, WakeReason, decide, probe_fingerprint, wake_message};

/// The synthetic `call_id` probe and wake replays carry. They never enter
/// the transcript, so the id only needs to be recognizable in hook payloads
/// and diagnostics.
const PARKED_CALL_ID: &str = "parked-wait";

impl<'a> Engine<'a> {
    /// Park the turn on a deposited wait request, if the step's tools left
    /// one. Called once per step, after the tool results are committed and
    /// before the next model call — the same safe boundary as the budget
    /// enforcer and the pause gate (invariant 6), which is what makes an
    /// arbitrarily long wait cost zero model steps and force no compaction.
    ///
    /// Returns `Some` only when the turn was cancelled while parked; every
    /// other ending — a change, the deadline, a soft stop arriving — lets
    /// the turn continue into its next step, which then observes the wake
    /// message (or the soft-stop latch) exactly as it observes a user steer.
    pub(super) async fn maybe_park(
        &self,
        state: &mut TurnState,
        events: &EventSender,
    ) -> Option<StepOutcome> {
        let request = self.tools.drain_wait_request()?;
        // Structural guarantee, not a convention: a replayed call the model
        // never sees must not mutate anything. Both the probe and the wake
        // call must advertise `read_only` in the very schema set the model's
        // requests are built from.
        let read_only: std::collections::HashSet<String> = self
            .tools
            .schemas()
            .into_iter()
            .filter(|s| s.read_only)
            .map(|s| s.name)
            .collect();
        let replayed = std::iter::once(&request.probe).chain(request.on_wake.as_ref());
        for call in replayed {
            if !read_only.contains(&call.name) {
                let _ = events.send(AgentEvent::Text {
                    text: format!(
                        "\n⚠ Ignoring a parked-wait request: `{}` is not a read-only tool, and \
                         the engine only replays read-only calls while the model is not looking.\n",
                        call.name
                    ),
                });
                return None;
            }
        }

        let _ = events.send(AgentEvent::Text {
            text: format!(
                "\n⏳ Parked: waiting until {} — probing every {}s for up to {}s, with no model \
                 calls while waiting.\n",
                request.description,
                request.interval_secs(),
                request.deadline_secs()
            ),
        });

        let mut polls_used = 0u64;
        let mut last_observed: Option<String> = None;
        let reason = loop {
            if let Some(cancelled) = state.cancel_outcome(events) {
                return Some(cancelled);
            }
            // A latched soft stop ends the wait early and falls through to
            // the next step boundary, where the standard soft-stop exit
            // keeps the transcript — the park must not make Esc wait out a
            // CI run. (The latch contract says the request survives this
            // read.) User steers queued mid-park stay queued: the drain is
            // destructive by contract, so the boundary that injects them is
            // the one that reads them.
            if self
                .steering
                .is_some_and(|steering| steering.soft_stop_requested())
            {
                return None;
            }
            self.sleeper
                .sleep(request.interval_secs().saturating_mul(1000))
                .await;
            if let Some(cancelled) = state.cancel_outcome(events) {
                return Some(cancelled);
            }
            let output = self.replay(&request.probe, events).await;
            polls_used += 1;
            let observed = probe_fingerprint(&output);
            if observed.is_some() {
                last_observed.clone_from(&observed);
            }
            if let Some(reason) = decide(&request, observed.as_deref(), polls_used) {
                break reason;
            }
        };

        // The delta the model wakes to: the rich wake call's output when the
        // request named one (and the state actually changed), otherwise the
        // last thing the probe saw.
        let detail = match (reason, &request.on_wake) {
            (WakeReason::Changed, Some(call)) => match self.replay(call, events).await {
                ToolOutput::Ok { content } => Some(content),
                // The wake call failing must not hide that the wait ended —
                // surface the error as the detail and let the model re-query.
                ToolOutput::Error { message } => Some(format!("(wake query failed: {message})")),
            },
            _ => last_observed,
        };
        let message = wake_message(&request, reason, polls_used, detail.as_deref());
        let _ = events.send(AgentEvent::Text {
            text: format!(
                "\n▶ Wait ended after {polls_used} probe{}: {}\n",
                if polls_used == 1 { "" } else { "s" },
                match reason {
                    WakeReason::Changed => "the watched state changed.",
                    WakeReason::DeadlineExpired => "the deadline expired with no change.",
                }
            ),
        });
        // The wake rides the conversation tail — the volatile cache zone —
        // exactly where a user steer lands (invariant 7): the stable prefix
        // and every cacheable block before it are byte-identical to the
        // pre-park request.
        state.messages.push(CompletionMessage::user(message));
        None
    }

    /// Replay one deposited call through the standard dispatch path —
    /// malformed-input repair, hooks, and the engine's tool timeout all
    /// apply, so a probe behaves like any other tool call except that its
    /// output never reaches the transcript. Hook diagnostics still surface
    /// on the turn stream; a `PreToolUse` block comes back as an error
    /// output, which the wait loop reads as "no observation, keep waiting".
    async fn replay(&self, call: &WaitCall, events: &EventSender) -> ToolOutput {
        let tool_call = ToolCall {
            call_id: PARKED_CALL_ID.into(),
            name: call.name.clone(),
            input: call.input.clone(),
        };
        self.execute_with_repair(&tool_call, Some(events)).await
    }
}
