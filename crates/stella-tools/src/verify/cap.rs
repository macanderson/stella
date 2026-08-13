// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The bound on how many times one turn may re-derive the same rejection.
//!
//! # What this costs when it is missing
//!
//! `verify_done`'s expensive half is a fresh shadow checkout and a cold build,
//! and nothing stopped a turn from buying it again for an answer it already
//! held. Measured 2026-08-10: `kv-store-grpc__RpPZcUD` spent 430 of 911s and
//! 20 of 52 steps in the gate loop for five `VACUOUS` verdicts;
//! `pypi-server__HRUntZQ` spent 665 of 921s and 65 of 89 steps for twelve
//! (#2863). Across the 2026-08-11 panel the verification apparatus was 24-36%
//! of every tool call Stella made, on a benchmark Claude Code solves 20/20
//! while spending none.
//!
//! # What the cap is a cap ON, and why not the other two
//!
//! **Consecutive rejections of the same [`Verdict`] class, inside one turn.**
//! Not total calls and not spend, and the difference is the whole design:
//!
//! - A cap on **total calls** punishes a progressing repair loop exactly as
//!   hard as a stuck one. A turn that goes `NOT DONE` → fix → `VACUOUS` →
//!   restage → `WITNESS CONFIRMED` is three calls doing three different
//!   things, and a call budget cannot tell it from three identical ones. A cap
//!   that fires on a loop that is converging is worse than no cap: it takes
//!   away a proof the turn was about to earn.
//! - A cap on **spend** cannot live here at all. A `Tool` is handed an input
//!   and a root; it holds no budget, no turn clock, and no session. Wall-clock
//!   share of budget — #2668's other half — belongs to the driver that owns
//!   the budget, and is tracked separately rather than approximated here from
//!   numbers this crate would have to invent.
//! - **Same-class consecutive** is the signal that no progress is being made:
//!   the class *is* what the last edit was supposed to change. Any change of
//!   class resets the run, so a turn that is genuinely converging is never
//!   capped, however many calls it takes; a turn that is not converging stops
//!   after the second identical answer.
//!
//! [`Verdict::Confirmed`] clears the run outright — the gate opened, and
//! whatever comes after is a new question. [`Verdict::Error`] is deliberately
//! **neutral**: it counts for nothing and clears nothing. Those are the input
//! refusals and the workspace refusals, which never build a shadow and so are
//! not the cost this bounds — and the closure notice below is itself one of
//! them, so counting it would let the gate re-arm by refusing.
//!
//! # A bounded loop must still fail honestly
//!
//! The closure is a refusal, never a pass. [`refusal`] returns a message the
//! caller hands back as `ToolOutput::Error`, and its text says the change is
//! **UNPROVEN** and must be reported as unwitnessed. Nothing here can produce
//! a `WITNESS CONFIRMED`, and no verdict is manufactured to end the loop —
//! that would trade a wall-clock defect for a correctness one, and #2915
//! settled that an unprovable claim ends nothing.
//!
//! # Where the state lives
//!
//! In [`ShadowMemo`](super::ShadowMemo), the turn-scoped seam the registry
//! already brackets (`begin_workspace_probe` arms, `settle_workspace_probe`
//! clears). The tool itself holds no turn state — and the tally must not ride
//! the `ToolOutput` text either, because `stella-core`'s loop detector keys on
//! it and a counter in the verdict would blind both of its rungs for the one
//! tool a stuck agent calls repeatedly (see [`phases`](super::phases)).
//!
//! Like the memo, the tally is inert until a host brackets a turn: an unarmed
//! [`ShadowMemo`](super::ShadowMemo) never closes the gate, so a standalone
//! `VerifyDone` behaves exactly as it did before this existed.

use super::phases::Verdict;

/// How many consecutive rejections of one class a turn may buy before the
/// gate closes. Two, per #2668 — so the *third* call that would answer the
/// same way is refused before it builds a shadow worktree.
///
/// Two rather than one because the first re-derivation is legitimate: the
/// agent edits the tree between calls, and one repeat is how it learns the
/// edit did not move the verdict. The second repeat teaches nothing the first
/// did not.
pub(super) const CONSECUTIVE_LIMIT: u32 = 2;

/// One turn's run of same-class rejections.
///
/// A plain value with pure transitions, so the policy is testable without a
/// registry, a repository, or a clock — the engine-logic discipline, applied
/// to the one piece of `verify_done` that is a decision rather than an
/// observation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Tally {
    /// The class currently repeating, and how many times it has been seen in
    /// a row. `None` when nothing is repeating — the start of a turn, or
    /// after a confirmation.
    run: Option<(Verdict, u32)>,
}

impl Tally {
    /// Fold one call's verdict in.
    #[must_use]
    pub(super) fn record(self, verdict: Verdict) -> Self {
        match verdict {
            // The gate opened. Whatever the turn asks next is a new question.
            Verdict::Confirmed => Self::default(),
            // Neutral — see the module docs.
            Verdict::Error => self,
            rejection => Self {
                run: Some(match self.run {
                    Some((class, seen)) if class == rejection => (class, seen.saturating_add(1)),
                    _ => (rejection, 1),
                }),
            },
        }
    }

    /// The class that closed the gate, or `None` while the turn may still
    /// call. Asked *before* a call, which is what makes the saving real: the
    /// refused call never resolves a baseline, never checks out a shadow, and
    /// never builds.
    pub(super) fn closed(self) -> Option<Verdict> {
        self.run
            .filter(|(_, seen)| *seen >= CONSECUTIVE_LIMIT)
            .map(|(class, _)| class)
    }
}

/// What the repeating verdict was, in the words the agent saw — so the notice
/// names the answer it is declining to re-derive rather than a wire tag.
const fn class_name(class: Verdict) -> &'static str {
    match class {
        Verdict::Confirmed => "WITNESS CONFIRMED",
        Verdict::Vacuous => "VACUOUS TEST",
        Verdict::NotDone => "NOT DONE",
        Verdict::InconclusiveBuildError => "WITNESS_INCONCLUSIVE_BUILD_ERROR",
        Verdict::InstrumentBroken => "WITNESS UNRUNNABLE",
        Verdict::Error => "a refusal",
    }
}

/// The closure notice. Deterministic in its inputs — one class in, one string
/// out, no counts that move call to call — so the loop detector keeps working
/// on it.
pub(super) fn refusal(class: Verdict) -> String {
    format!(
        "VERIFICATION GATE CLOSED — this turn has already had `{}` back from verify_done \
         {CONSECUTIVE_LIMIT} times in a row, so this call was refused before it built a shadow \
         worktree. A third identical answer costs a full checkout and a cold build and buys \
         nothing: the verdict is not moving, and re-deriving it is where a stuck turn spends \
         its budget.\n\
         This is NOT a pass, and it is NOT permission to claim the work is verified. The change \
         is UNPROVEN by a witness test. Report it as unwitnessed — say plainly what you \
         changed, that no witness test proves it, and what you tried — and stop calling \
         verify_done for the rest of this turn. Do not weaken the test to get past this, and do \
         not delete or edit working code in response to it.",
        class_name(class)
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::{Value, json};
    use stella_protocol::tool::ToolOutput;

    use super::*;
    use crate::registry::Tool;
    use crate::verify::tests::scaffold;
    use crate::verify::{ShadowMemo, ShadowMemoHandle, VerifyDone};

    #[test]
    fn a_run_of_one_class_closes_the_gate_at_the_limit() {
        let mut tally = Tally::default();
        assert_eq!(tally.closed(), None);
        tally = tally.record(Verdict::Vacuous);
        assert_eq!(tally.closed(), None, "one rejection is not a loop");
        tally = tally.record(Verdict::Vacuous);
        assert_eq!(tally.closed(), Some(Verdict::Vacuous));
    }

    /// The property the whole design rests on: a turn that is getting
    /// somewhere is never capped. Each new class restarts the run, so a repair
    /// loop that keeps changing the answer can call as often as it needs to.
    #[test]
    fn a_progressing_loop_is_never_capped() {
        let mut tally = Tally::default();
        for verdict in [
            Verdict::NotDone,
            Verdict::Vacuous,
            Verdict::NotDone,
            Verdict::InconclusiveBuildError,
            Verdict::InstrumentBroken,
            Verdict::Vacuous,
        ] {
            tally = tally.record(verdict);
            assert_eq!(
                tally.closed(),
                None,
                "capped a converging turn at {verdict:?}"
            );
        }
    }

    #[test]
    fn a_confirmation_clears_the_run_and_a_refusal_is_neutral() {
        let stuck = Tally::default()
            .record(Verdict::Vacuous)
            .record(Verdict::Vacuous);
        assert_eq!(stuck.closed(), Some(Verdict::Vacuous));
        assert_eq!(
            stuck.record(Verdict::Confirmed).closed(),
            None,
            "the gate opened; the next question is a new one"
        );
        assert_eq!(
            Tally::default()
                .record(Verdict::Vacuous)
                .record(Verdict::Error)
                .record(Verdict::Vacuous)
                .closed(),
            Some(Verdict::Vacuous),
            "a refusal between two identical rejections is not progress"
        );
    }

    fn vacuous_input() -> Value {
        json!({
            "test_cmd": "bash witness.sh",
            "test_files": ["witness.sh"],
            "timeout_secs": 60
        })
    }

    /// How many shadow builds have happened: the witness script appends a line
    /// on every run inside a linked worktree (where `.git` is a file, not a
    /// directory), which is exactly the shadow. The same counter `memo.rs`
    /// uses, for the same reason — the saving has to be observed, not assumed.
    fn shadow_runs(marker: &std::path::Path) -> usize {
        std::fs::read_to_string(marker)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    /// #2863 witness.
    ///
    /// Fails on `main`, where a turn can re-enter `verify_done` on the same
    /// rejection class without bound and pay for a shadow checkout every time.
    ///
    /// Two assertions, and both matter: the third call does not build a
    /// shadow, and it reports the change as UNPROVEN rather than manufacturing
    /// the pass the bound could have been made to produce (#2915).
    #[tokio::test]
    async fn the_third_same_class_rejection_builds_no_shadow_and_reports_unproven() {
        let root = scaffold("cap_vacuous").await;
        let marker = root.join("shadow-runs.txt");
        // Vacuous by construction: true on the baseline and on the working
        // tree, so every call answers the same way however the agent edits.
        std::fs::write(
            root.join("witness.sh"),
            format!(
                "if [ ! -d .git ]; then printf 'r\\n' >> '{}'; fi\ngrep -q 'behavior' impl.txt\n",
                marker.display()
            ),
        )
        .unwrap();

        let memo: ShadowMemoHandle = Arc::new(ShadowMemo::default());
        memo.arm();
        let tool = VerifyDone::new(memo, None);

        for call in 1..=2 {
            let out = tool.execute(&vacuous_input(), &root).await;
            let message = match &out {
                ToolOutput::Error { message, .. } => message.clone(),
                other => panic!("call {call} should be VACUOUS, got {other:?}"),
            };
            assert!(message.contains("VACUOUS"), "call {call}: {message}");
        }
        // The memo (#2208) serves an identical repeat, so only the first call
        // built. Either way the count must not move again below.
        let built = shadow_runs(&marker);
        assert!(built >= 1, "the first call must build a shadow");

        let out = tool.execute(&vacuous_input(), &root).await;
        let message = match &out {
            ToolOutput::Error { message, .. } => message.clone(),
            other => panic!("the capped call must be a refusal, not a pass: {other:?}"),
        };
        assert!(
            message.contains("VERIFICATION GATE CLOSED"),
            "the third same-class call was not capped: {message}"
        );
        assert!(
            message.contains("UNPROVEN"),
            "a bounded loop must still fail honestly: {message}"
        );
        assert!(
            !message.contains("WITNESS CONFIRMED"),
            "the cap must never manufacture a pass: {message}"
        );
        assert_eq!(
            shadow_runs(&marker),
            built,
            "the capped call built a shadow worktree anyway"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// The tool is inert outside a bracketed turn — the memo's own contract,
    /// and the reason every pre-existing test that builds a bare `VerifyDone`
    /// still sees exactly the behaviour it always did.
    #[tokio::test]
    async fn an_unbracketed_host_is_never_capped() {
        let root = scaffold("cap_unarmed").await;
        std::fs::write(root.join("witness.sh"), "grep -q 'behavior' impl.txt\n").unwrap();
        let tool = VerifyDone::default();

        for call in 1..=3 {
            let out = tool.execute(&vacuous_input(), &root).await;
            match &out {
                ToolOutput::Error { message, .. } => assert!(
                    message.contains("VACUOUS"),
                    "call {call} was capped without a turn bracket: {message}"
                ),
                other => panic!("call {call}: {other:?}"),
            }
        }

        std::fs::remove_dir_all(&root).ok();
    }
}
