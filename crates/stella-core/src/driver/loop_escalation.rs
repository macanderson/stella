//! The steer→abort escalation ladder loop detection drives: warn once, then
//! abort — and say honestly which loop the abort is about.
//!
//! `crate::loop_detect` decides *whether* the turn is looping, and
//! `super::loop_evidence` assembles what it reasons over. This module owns
//! what happens next: the first detection of a turn injects a steering
//! warning through the same seam as user steering and records the pattern it
//! warned about; any detection after that is the turn's clean abort. The
//! recorded pattern is what keeps the abort message honest — "persisted
//! after a steering warning" is a claim about the *warned* loop, and the
//! nightly loop-bench caught the old unconditional phrasing blaming a fresh
//! `edit_file` loop on a `write_file` warning the model had actually obeyed
//! (#1524). Split out of `driver.rs` the same way `super::settlement` was:
//! the parent file sits at its file-size ceiling.

use stella_protocol::{AgentEvent, CompletionMessage};

use crate::event_sender::EventSender;
use crate::loop_detect::{LoopDetectionConfig, LoopVerdict, detect_loop};
use crate::step::AbortKind;

use super::loop_evidence::{ResultIdentities, recent_call_records};
use super::{LOOP_STEER_PREFIX, TurnOutcome};

/// Loop detection, before spending a model call on a step that's already
/// stuck. Progress-aware: each call is paired with the output it produced
/// ([`recent_call_records`]), so only repeats and cycles with byte-identical
/// outputs count — legitimate polling (identical input, changing output)
/// never trips it (`crate::loop_detect`). `result_identities` is the turn's
/// pre-compaction snapshot ([`super::loop_evidence::snapshot_result_identities`])
/// — compaction runs immediately before this check and rewrites older
/// results in place, so without it a repeat streak reads as
/// `[stub, stub, real]` and never counts.
///
/// Steer first, abort second: the FIRST detection of a turn injects a
/// warning through the same seam as user steering — a user-role message the
/// model answers this very step, plus its `Steered` transcript event — and
/// lets the turn continue, recording the detected pattern in `loop_steered`.
/// Only a detection after that warning is the turn's clean abort (`Some`):
/// the model was told and the turn is looping *again*, so more steps would
/// be pure waste. Whether the abort may claim the warned loop persisted is
/// [`abort_reason`]'s job, not a given.
pub(super) fn check_loop_detection(
    loop_detection: LoopDetectionConfig,
    turn_instance: u32,
    messages: &mut Vec<CompletionMessage>,
    result_identities: &ResultIdentities,
    loop_steered: &mut Option<Vec<String>>,
    total_cost_usd: f64,
    events: &EventSender,
) -> Option<TurnOutcome> {
    let records = recent_call_records(messages, result_identities);
    let verdict = detect_loop(&records, loop_detection);
    // The typed twin of the prose steer/abort (receipts spec §6.3): a
    // receipt parses this instead of string-matching "stuck-loop
    // detected:" prefixes. Emitted for both outcomes — `aborted` says
    // whether this detection steered or killed the turn. Matching the
    // verdict is also the healthy-turn early return, so there is exactly
    // one place that decides "is this a loop".
    let (kind, pattern, repeats) = match &verdict {
        LoopVerdict::NoLoop => return None,
        LoopVerdict::ExactRepeat { tool, count, .. } => {
            ("exact_repeat", vec![tool.clone()], *count)
        }
        LoopVerdict::ShortCycle { pattern, repeats } => (
            "short_cycle",
            pattern.iter().map(|c| c.name.clone()).collect(),
            *repeats,
        ),
        LoopVerdict::Stagnant { tool, count } => ("stagnation", vec![tool.clone()], *count),
    };
    let evidence = verdict
        .evidence()
        .unwrap_or_else(|| "loop detected".to_string());
    let _ = events.send(AgentEvent::LoopDetected {
        turn_instance,
        kind: kind.to_string(),
        pattern: pattern.clone(),
        repeats,
        evidence: evidence.clone(),
        aborted: loop_steered.is_some(),
    });
    let Some(warned) = loop_steered else {
        *loop_steered = Some(pattern);
        let text = format!(
            "{LOOP_STEER_PREFIX}] you appear to be looping: {evidence}. Repeating the \
             same call cannot produce new information — change strategy: vary the \
             arguments, try a different tool, or report what is blocking you. If you \
             keep looping, the turn will be aborted."
        );
        let _ = events.send(AgentEvent::Steered { text: text.clone() });
        messages.push(CompletionMessage::user(text));
        return None;
    };
    let reason = abort_reason(warned, &pattern, &evidence);
    let _ = events.send(AgentEvent::Error {
        message: reason.clone(),
        retryable: false,
    });
    Some(TurnOutcome::Aborted {
        reason,
        kind: AbortKind::DeliberateStop,
        cost_usd: total_cost_usd,
    })
}

/// The abort's reason line, phrased by what the steering warning was about.
///
/// The one-warning-per-turn policy is untouched — a second detection always
/// aborts. What varies is the claim: "persisted" is only true when the
/// re-detected pattern is the warned one. A different pattern means the
/// steer *worked* on the loop it named and a fresh loop formed, and saying
/// so matters precisely because the two read the same in a verdict table —
/// the false "persisted" hid a working steer for months (#1524). An empty
/// `warned` pattern is a turn resumed from a checkpoint written before the
/// pattern was recorded; it cannot support either claim, so it makes none.
fn abort_reason(warned: &[String], detected: &[String], evidence: &str) -> String {
    if warned.is_empty() {
        format!("stuck-loop detected (after an earlier steering warning): {evidence}")
    } else if warned == detected {
        format!("stuck-loop detected (persisted after a steering warning): {evidence}")
    } else {
        format!(
            "stuck-loop detected (a new loop after a steering warning about {}, which the \
             model did break): {evidence}",
            warned.join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::abort_reason;

    fn tools(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn re_detecting_the_warned_pattern_claims_persistence() {
        let reason = abort_reason(&tools(&["bash"]), &tools(&["bash"]), "bash repeated 4×");
        assert_eq!(
            reason, "stuck-loop detected (persisted after a steering warning): bash repeated 4×",
            "the byte-exact legacy phrasing must survive for the one case it was true of"
        );
    }

    /// #1524 witness: on `prove-plus-comm` the model was warned about a
    /// `write_file` loop, stopped repeating `write_file`, and was then
    /// aborted over a fresh `edit_file` loop by a message claiming the
    /// warned loop had "persisted". The abort stays; the false claim goes.
    #[test]
    fn a_different_pattern_does_not_claim_the_warned_loop_persisted() {
        let reason = abort_reason(
            &tools(&["write_file"]),
            &tools(&["edit_file"]),
            "edit_file repeated 3×",
        );
        assert!(
            !reason.contains("persisted"),
            "a fresh loop must not be blamed on the warned one: {reason}"
        );
        assert!(
            reason.contains("write_file"),
            "the abort should name what the warning was actually about: {reason}"
        );
        assert!(
            reason.contains("edit_file repeated 3×"),
            "the detector's evidence for the loop that DID abort stays: {reason}"
        );
    }

    #[test]
    fn a_cycle_pattern_only_persists_if_the_whole_cycle_matches() {
        let reason = abort_reason(
            &tools(&["read_file", "bash"]),
            &tools(&["bash"]),
            "bash repeated 3×",
        );
        assert!(!reason.contains("persisted"), "{reason}");
    }

    #[test]
    fn an_unknown_warned_pattern_makes_no_claim_either_way() {
        let reason = abort_reason(&[], &tools(&["bash"]), "bash repeated 3×");
        assert!(
            !reason.contains("persisted") && !reason.contains("new loop"),
            "a checkpoint-resumed turn with no recorded pattern cannot support \
             either claim: {reason}"
        );
        assert!(reason.contains("steering warning"), "{reason}");
    }
}
