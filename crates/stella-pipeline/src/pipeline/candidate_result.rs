//! The outcome of running one candidate, and the typed abort it may carry.
//!
//! Split out of `pipeline.rs` (at its file-size ceiling) when the abort grew
//! from a bare reason string into [`CandidateAbort`]: the pipeline's terminal
//! emit needs to know whether an abort already crossed the event bus inside
//! the worker turn that produced it, and the [`stella_core::AbortKind`] has
//! to survive the trip from `TurnOutcome::Aborted` to `PipelineStatus::
//! Aborted` so the process boundary can tell a deliberate stop from a crash
//! (#1524).

use stella_core::AbortKind;
use stella_protocol::CompletionMessage;

use crate::candidate::CandidateScore;

use super::Verdict;

/// An abort as one engine turn reported it — the `reason` and `kind` of a
/// `TurnOutcome::Aborted`, carried through the pipeline's error channels
/// (`execute_plan`, `revise_candidate`) without collapsing back to a string.
///
/// Existing so a turn-originated abort stays distinguishable from a
/// pipeline-originated one: the driver has already published (or, for a soft
/// stop and a host cancel, deliberately declined to publish) the turn's
/// `Error` event by the time this value exists.
pub(super) struct TurnAbort {
    pub(super) reason: String,
    pub(super) kind: AbortKind,
}

/// Why a candidate aborted, typed for the two decisions the pipeline's
/// terminal path takes: whether to emit the abort as an `Error` event at all
/// (`from_turn` — the driver-side emit already happened, and one abort must
/// be exactly one `error` on the stream, #1524), and which [`AbortKind`] the
/// `PipelineStatus::Aborted` carries to the process boundary.
pub(super) struct CandidateAbort {
    pub(super) reason: String,
    pub(super) kind: AbortKind,
    /// `true` when this abort came out of an engine turn, which surfaced it
    /// on the bus per its own policy (an `Error` for a policy stop or a
    /// failure; silence for a user's soft stop and a host cancel — "a
    /// decision, not a failure"). The pipeline re-emitting it produced the
    /// byte-identical duplicate the nightly loop-bench recorded, and turned
    /// deliberate user stops into `error` events.
    pub(super) from_turn: bool,
}

/// The outcome of running one candidate (execute + verify + bounded revise).
pub(super) struct CandidateResult {
    pub(super) messages: Vec<CompletionMessage>,
    pub(super) final_text: String,
    /// `Some` if a turn aborted (budget/loop/step-cap) or the pipeline
    /// aborted the candidate itself (a sealed-workspace failure, a stage
    /// budget, a poisoned witness).
    pub(super) aborted: Option<CandidateAbort>,
    /// Whether this abort is a degradable **infrastructure** setup failure —
    /// no isolation port, or a workspace that could not be snapshotted. `run`
    /// degrades these to a bare worker run rather than ending the turn with
    /// zero work (the "never choose nothing" rule). Deliberately does NOT
    /// cover a witness-integrity rejection (symlink artifact, language
    /// mismatch, a witness author touching production code): those are
    /// fail-closed security decisions and keep their abort. `false` on every
    /// executed and every verified/unverified result.
    pub(super) degradable: bool,
    /// The verification verdict, if verification ran.
    pub(super) verdict: Option<Verdict>,
    /// This candidate's verification score, for best-of-N selection.
    pub(super) score: CandidateScore,
    pub(super) diff_lines: u32,
    pub(super) revisions: u32,
    /// Workspace-relative paths of the witness artifact this candidate had
    /// grafted into it, withheld from adoption unless
    /// [`crate::PipelineConfig::keep_witness`]. Empty when no witness was
    /// warranted, which under demand-driven authoring is the common case.
    ///
    /// These ride on the result rather than beside the workspace because
    /// authoring now happens *inside* the candidate, after execution: the
    /// paths are an output of the run, and the caller indexes them by the same
    /// index it already uses to find the workspace.
    pub(super) witness_paths: Vec<String>,
}

impl CandidateResult {
    /// Whether this candidate got as far as dispatching worker work.
    ///
    /// Only a degradable **setup** abort answers `false`: by construction it is
    /// the arm taken before any model call, so it is exactly the "generated
    /// nothing" case. An execution abort (budget, loop, step-cap) and a
    /// fail-closed witness rejection both ran real turns and count.
    pub(super) fn executed(&self) -> bool {
        !(self.aborted.is_some() && self.degradable)
    }

    /// A candidate the *pipeline* aborted, for a reason that is NOT a
    /// degradable infrastructure setup failure: a stage budget stop
    /// ([`AbortKind::DeliberateStop`]) or a fail-closed rejection such as a
    /// poisoned witness ([`AbortKind::Failure`]). These keep their stop, and
    /// the terminal path owes the stream their one `Error` event — nothing
    /// upstream has emitted it.
    pub(super) fn aborted(
        messages: Vec<CompletionMessage>,
        reason: String,
        kind: AbortKind,
    ) -> Self {
        Self {
            messages,
            final_text: String::new(),
            aborted: Some(CandidateAbort {
                reason,
                kind,
                from_turn: false,
            }),
            degradable: false,
            verdict: None,
            score: CandidateScore::Failed,
            diff_lines: 0,
            revisions: 0,
            witness_paths: Vec::new(),
        }
    }

    /// A candidate whose *engine turn* aborted (budget, loop, step-cap — the
    /// worker ran). The driver already surfaced the abort on the event bus
    /// per its own policy, so the terminal path must not emit it again.
    pub(super) fn turn_aborted(messages: Vec<CompletionMessage>, abort: TurnAbort) -> Self {
        Self {
            aborted: Some(CandidateAbort {
                reason: abort.reason,
                kind: abort.kind,
                from_turn: true,
            }),
            ..Self::aborted(messages, String::new(), abort.kind)
        }
    }

    /// A candidate that aborted on a degradable **infrastructure** setup
    /// failure — no isolation port, or a tree that could not be snapshotted.
    /// Flagged so `run` degrades it to a bare worker run rather than ending
    /// the turn having done nothing.
    pub(super) fn setup_aborted(messages: Vec<CompletionMessage>, reason: String) -> Self {
        Self {
            degradable: true,
            ..Self::aborted(messages, reason, AbortKind::Failure)
        }
    }
}

/// The abort reason for a candidate that wrote through its isolation
/// (#1538): the post-seal check found the real tree already holding this
/// candidate's own sealed bytes. Named as the candidate's defect — never a
/// user edit, never an adoption conflict — because that misattribution is
/// exactly what the check exists to remove.
pub(super) fn escape_abort_reason(paths: &[String]) -> String {
    // Same inline cap as adoption's conflict rendering: a candidate that
    // escaped on a hundred paths must not paste a hundred paths into one
    // error line.
    const INLINE: usize = 4;
    let shown = paths
        .iter()
        .take(INLINE)
        .map(|path| format!("`{path}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let listed = match paths.len().checked_sub(INLINE) {
        Some(rest) if rest > 0 => format!("{shown} (+{rest} more)"),
        _ => shown,
    };
    format!(
        "candidate wrote outside its isolated workspace: the real tree already holds its \
         sealed bytes at {listed} — isolation was breached by the candidate, so its work is \
         discarded rather than adopted"
    )
}

#[cfg(test)]
mod tests {
    use super::escape_abort_reason;

    #[test]
    fn the_escape_reason_names_the_candidate_and_the_paths() {
        let reason = escape_abort_reason(&["app/vm.js".to_string()]);
        assert!(reason.contains("wrote outside its isolated workspace"), "{reason}");
        assert!(reason.contains("`app/vm.js`"), "{reason}");
        assert!(
            !reason.contains("user"),
            "the escape must never read as a user edit: {reason}"
        );
    }

    #[test]
    fn many_escaped_paths_are_summarized_not_pasted() {
        let paths: Vec<String> = (0..9).map(|i| format!("f{i}.rs")).collect();
        let reason = escape_abort_reason(&paths);
        assert!(reason.contains("`f3.rs`") && !reason.contains("`f4.rs`"), "{reason}");
        assert!(reason.contains("+5 more"), "{reason}");
    }
}
