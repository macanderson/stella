// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Per-phase wall clock for one `verify_done` call — the measurement #2486
//! asks for before anything is optimised.
//!
//! # Why this exists
//!
//! `verify_done` runs the witness command twice, and across 61 local arena
//! matches neither run was where the time went. Subtracting both from the
//! call's `duration_ms` left a **median 1.5s and a p90 of 11.1s** of fixed
//! machinery — the baseline resolution, `git worktree add`, the test-file
//! copies, and the teardown. That number is an *estimate*: it comes from
//! pairing each call against a shell invocation of the same command elsewhere
//! in the trial, and three of 98 pairs estimated the working-tree run as
//! larger than the whole call, which proves the proxy is noisy in both
//! directions (#2424).
//!
//! So the estimate is not a finding to act on, it is a reason to measure.
//! [`Phases`] carries every boundary the call actually has, and
//! [`Phases::machinery`] is the same quantity the subtraction was reaching
//! for — observed rather than inferred. An optimisation lands *after* one of
//! these fields is shown to be the p90, not before.
//!
//! # Why the diagnostic plane and not the tool's own output
//!
//! `ToolOutput` is the only channel a [`Tool`](crate::registry::Tool) has into
//! `stella-events.jsonl`, and it is the wrong one — `stella-core`'s loop
//! detector keys on it. `LoopDetectionConfig::exact_repeat_threshold` counts
//! consecutive identical **(name + input + output)** calls and the stagnation
//! rung counts consecutive **byte-identical outputs** from one tool. A wall
//! clock in the verdict text never repeats, so both rungs would go
//! permanently blind for `verify_done` — the tool an agent stuck in a
//! prove-it loop calls over and over. `ci.rs`'s probe path already carries
//! this scar: it answers with one word, "deliberately free of run names,
//! elapsed times, or counts — anything that changes poll-to-poll".
//!
//! The diagnostic plane has neither problem, and it is where
//! `docs/spec/diagnostics.md` §2.1's routing rule sends this anyway: a phase
//! duration explains *why the program behaved this way*. Every field below is
//! a number, a bool, or a `&'static str` from a closed set, so §5.2's
//! "content in a log does not compile" holds by construction — invariant 3 is
//! not at risk here even though the subject is a path-bearing operation.
//!
//! # Reading a record
//!
//! One record per call, code `tools.verify_done.phases`, at `info` — so a
//! `--log-file` (which floors at `info`) collects it and an ordinary run stays
//! quiet. The record is **self-sufficient**: it carries every phase *and* the
//! total, so a median/p90 per phase needs no join back to the `tool_result`
//! event that measured the same call. That matters because a tool has no
//! `call_id` to join on.
//!
//! The phases do not have to sum to [`Phases::total`], and the gap is
//! deliberately left visible rather than folded into a neighbour: it is the
//! `async` scheduling slack plus anything between the boundaries below. A
//! reader who wants it computes `total - (sum of phases)`; a reader who sees
//! it grow has found the next thing to name.

use std::time::{Duration, Instant};

use stella_diag::{Cx, Dx, Facet, Fields, Level};

/// The stable diagnostic code every phase record carries (§11).
pub(crate) const PHASES_CODE: &str = "tools.verify_done.phases";

/// How a call ended, as a closed vocabulary — so a census can exclude the
/// paths that never built a shadow at all (`not_done`, `error`) instead of
/// letting their zeroes drag a machinery percentile down.
///
/// A `&'static str` rather than the raw verdict prose for the same reason
/// every other field here is a number: the prose is assembled from a baseline
/// provenance and a command line, and neither belongs in a log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The flip was proven: new code passed, the baseline failed.
    Confirmed,
    /// The witness also passed on the baseline.
    Vacuous,
    /// The witness failed on the new code, so no shadow was ever built.
    NotDone,
    /// The baseline run died loading the test rather than asserting (#2067).
    InconclusiveBuildError,
    /// A named refusal — bad input, no repo, no resolvable baseline, a
    /// worktree that would not create.
    Error,
}

impl Verdict {
    /// The wire spelling. `snake_case` to match every other closed enum that
    /// reaches a record.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Vacuous => "vacuous",
            Self::NotDone => "not_done",
            Self::InconclusiveBuildError => "inconclusive_build_error",
            Self::Error => "error",
        }
    }
}

impl Default for Verdict {
    /// A call that has not reached a verdict yet has, so far, only failed to
    /// produce one. Defaulting to [`Verdict::Confirmed`] would let a panic or
    /// an early return report a proof that never happened.
    fn default() -> Self {
        Self::Error
    }
}

/// A running stopwatch for one phase.
///
/// Deliberately not a guard type: several phases below end at a `return`, and
/// a `Drop`-based timer would have to guess which field it was filling.
pub(crate) struct Phase(Instant);

impl Phase {
    pub(crate) fn start() -> Self {
        Self(Instant::now())
    }

    /// Stop, and hand back what elapsed.
    pub(crate) fn stop(self) -> Duration {
        self.0.elapsed()
    }
}

/// Every boundary one `verify_done` call has, in the order it crosses them.
///
/// All fields default to zero, which is also what a path that never reached
/// them reports — a `not_done` verdict returns before any shadow exists, and
/// a memo hit ([`crate::verify::ShadowMemo`], #2208) skips the four shadow
/// phases outright. [`Self::shadow_reused`] and [`Self::verdict`] are what let
/// a reader tell "this phase was free" from "this phase never ran", which a
/// bare zero cannot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Phases {
    /// Argument validation, `root.canonicalize()`, `git rev-parse
    /// --show-toplevel`, resolving every `test_files` entry, and `git
    /// rev-parse HEAD`. Three of those are subprocesses, which is why this is
    /// named rather than assumed negligible.
    pub(crate) setup: Duration,
    /// The prune-on-start that reclaims `.git/worktrees` registrations
    /// stranded by a cancelled earlier run (#613).
    pub(crate) prune: Duration,
    /// [`crate::verify::resolve_witness_baseline`] — up to two `rev-parse`
    /// calls and a `git log` walk past pipeline seal commits.
    pub(crate) baseline: Duration,
    /// Half 1: the witness command against the working tree.
    pub(crate) working_tree_run: Duration,
    /// `git worktree add --detach` — a full checkout of the baseline commit,
    /// so its cost scales with the repository rather than with the diff.
    pub(crate) worktree_add: Duration,
    /// Copying the witness files into the shadow.
    pub(crate) stage_tests: Duration,
    /// Half 2: the witness command against the baseline.
    pub(crate) shadow_run: Duration,
    /// `git worktree remove --force`, `remove_dir_all`, and the trailing
    /// `git worktree prune`.
    pub(crate) cleanup: Duration,
    /// The whole call, measured at `execute`'s entry and exit. The authority
    /// for the total; the phases are a partition attempt, not a definition.
    pub(crate) total: Duration,
    /// Whether half 2 came from the turn-scoped memo instead of a fresh
    /// shadow (#2208). When true the four shadow phases are legitimately
    /// zero.
    pub(crate) shadow_reused: bool,
    /// How many witness files were staged — the copy loop's trip count, so a
    /// slow [`Self::stage_tests`] can be read against its size.
    pub(crate) test_files: u32,
    /// How the call ended. Written once, at the single exit in
    /// [`VerifyDone::execute`](crate::verify::VerifyDone).
    pub(crate) verdict: Verdict,
}

impl Phases {
    /// Everything that is not one of the two test runs: the quantity #2424
    /// estimated as `verify_ms - 2 * half1_ms` and could only bound from
    /// below. Here it is observed.
    ///
    /// Derived from [`Self::total`] rather than by summing the named phases,
    /// so unattributed time counts as machinery instead of vanishing — an
    /// optimisation must beat the honest number, not the flattering one.
    /// Saturating because `total` is measured across the same span the two
    /// runs sit inside and can only be shorter if a clock went backwards.
    ///
    /// Computed at full precision and truncated once, on the way into the
    /// record. So a reader who instead subtracts the record's *millisecond*
    /// fields — `total - working_tree_run - shadow_run` — can land up to 3ms
    /// away, because truncation does not distribute over subtraction. That
    /// gap is the field's resolution and nothing else; it is stated here
    /// rather than hidden by re-rounding the parts to force the identity,
    /// which would make the record self-consistent by making it less true.
    pub(crate) fn machinery(&self) -> Duration {
        self.total
            .saturating_sub(self.working_tree_run)
            .saturating_sub(self.shadow_run)
    }

    /// Emit this call's record.
    ///
    /// `Cx::EMPTY`: a [`Tool`](crate::registry::Tool) is handed an input and a
    /// root and nothing else — no session, turn, or `call_id` — and §7.1
    /// forbids reaching for an ambient one. The record is built to be read
    /// without correlation for exactly this reason.
    pub(crate) fn emit(&self, dx: &Dx) {
        dx.emit_facet(self, module_path!(), Cx::EMPTY);
    }
}

/// §5.1's per-crate facet: `stella-tools` owns this vocabulary, so it owns the
/// rendering too and nobody edits a shared enum to add a phase.
impl Facet for Phases {
    fn code(&self) -> &'static str {
        PHASES_CODE
    }

    /// `info`, not `debug`: `--log-file` floors at `info`, so an operator who
    /// asked for a log file gets the measurement without also having to guess
    /// a level. stderr stays at `warn`, so an ordinary run is unchanged.
    fn level(&self) -> Level {
        Level::Info
    }

    fn fields(&self) -> Fields {
        Fields::new()
            .with("verdict", self.verdict.as_str())
            .with("shadow_reused", self.shadow_reused)
            .with("test_files", self.test_files)
            .with("setup", self.setup)
            .with("prune", self.prune)
            .with("baseline", self.baseline)
            .with("working_tree_run", self.working_tree_run)
            .with("worktree_add", self.worktree_add)
            .with("stage_tests", self.stage_tests)
            .with("shadow_run", self.shadow_run)
            .with("cleanup", self.cleanup)
            .with("machinery", self.machinery())
            .with("total", self.total)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use stella_diag::{FieldValue, Record};
    use stella_protocol::tool::ToolOutput;

    use super::*;
    use crate::registry::Tool;
    use crate::verify::{ShadowMemo, ShadowMemoHandle, VerifyDone};
    // The git-repo fixture the tool's own tests already build — shared for the
    // same reason `memo.rs` shares it.
    use crate::verify::tests::scaffold;

    fn uint(record: &Record, key: &str) -> u64 {
        match record.fields.get(key) {
            Some(FieldValue::Uint(value)) => *value,
            other => panic!("field `{key}` is {other:?}, not a duration"),
        }
    }

    fn text(record: &Record, key: &str) -> String {
        match record.fields.get(key) {
            Some(FieldValue::Str(value)) => value.as_str().to_string(),
            other => panic!("field `{key}` is {other:?}, not a string"),
        }
    }

    fn witness_input() -> serde_json::Value {
        serde_json::json!({
            "test_cmd": "bash witness.sh",
            "test_files": ["witness.sh"],
            "timeout_secs": 60
        })
    }

    /// #2486 witness: a real call leaves a per-phase record behind.
    ///
    /// Fails on `main`, where a `verify_done` call reports one number — the
    /// registry's `duration_ms` for the whole thing — and the 1.5s median /
    /// 11.1s p90 of machinery inside it could only be reached by subtracting
    /// a proxy measured somewhere else in the trial (#2424).
    ///
    /// The assertions are identities, not thresholds: a wall clock is the one
    /// thing a test must never assert a bound on, and the point here is that
    /// the phases are *reported*, not that this machine is slow.
    #[tokio::test]
    async fn a_call_reports_every_phase_it_crossed() {
        let root = scaffold("phases_report").await;
        std::fs::write(root.join("impl.txt"), "new behavior\n").unwrap();
        std::fs::write(root.join("witness.sh"), "grep -q 'new behavior' impl.txt\n").unwrap();

        let (dx, records) = Dx::capturing();
        let tool = VerifyDone::new(Arc::default(), Some(Arc::new(dx)));
        let out = tool.execute(&witness_input(), &root).await;
        assert!(
            matches!(&out, ToolOutput::Ok { content } if content.contains("WITNESS CONFIRMED")),
            "{out:?}"
        );

        let record = records.find(PHASES_CODE).expect("one record per call");
        assert_eq!(text(&record, "verdict"), "confirmed");
        assert_eq!(uint(&record, "test_files"), 1);
        assert_eq!(
            record.fields.get("shadow_reused"),
            Some(&FieldValue::Bool(false)),
            "this call built its own shadow"
        );

        // The whole call contains both runs, and machinery is exactly what is
        // left over — the quantity #2424 could only bound from below.
        let total = uint(&record, "total");
        let half_one = uint(&record, "working_tree_run");
        let half_two = uint(&record, "shadow_run");
        assert!(total > 0, "a call that ran two commands took some time");
        assert!(
            total >= half_one + half_two,
            "total {total}ms cannot be under its two runs ({half_one}ms + {half_two}ms)"
        );
        // Not an equality: `machinery` is subtracted at full precision and
        // truncated once, while this recomputes it from three already-
        // truncated fields. Each truncation loses under a millisecond, so the
        // two can disagree by up to 3 — see [`Phases::machinery`].
        let machinery = uint(&record, "machinery");
        assert!(
            machinery.abs_diff(total - half_one - half_two) <= 3,
            "machinery {machinery}ms is not the leftover of {total} - {half_one} - {half_two}"
        );

        // Every phase boundary the issue names is present, so an attribution
        // never has to fall back to subtraction again.
        for phase in [
            "setup",
            "prune",
            "baseline",
            "worktree_add",
            "stage_tests",
            "cleanup",
        ] {
            assert!(
                record.fields.get(phase).is_some(),
                "phase `{phase}` was not reported"
            );
        }

        std::fs::remove_dir_all(&root).ok();
    }

    /// The last link in the chain: a handle put in
    /// [`RegistryOptions`](crate::RegistryOptions) has to reach the tool.
    ///
    /// Everything else here builds a `VerifyDone` by hand, so a registry that
    /// dropped the handle between `builtins` and the constructor would pass
    /// every one of those tests and still record nothing in a real session —
    /// the failure this whole change exists to prevent, and the one with no
    /// symptom until someone goes looking for a measurement that was never
    /// taken.
    #[tokio::test]
    async fn the_registry_hands_the_handle_to_the_tool() {
        let root = scaffold("phases_wiring").await;
        std::fs::write(root.join("impl.txt"), "new behavior\n").unwrap();
        std::fs::write(root.join("witness.sh"), "grep -q 'new behavior' impl.txt\n").unwrap();

        let (dx, records) = Dx::capturing();
        let registry = crate::ToolRegistry::with_backends_and_options(
            root.clone(),
            None,
            None,
            crate::RegistryOptions {
                diagnostics: Some(Arc::new(dx)),
                ..Default::default()
            },
        );
        let out = registry.execute("verify_done", &witness_input()).await;
        assert!(!out.is_error(), "{out:?}");
        assert!(
            records.find(PHASES_CODE).is_some(),
            "the registry built the tool without its diagnostic handle"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// A memo hit (#2208) legitimately reports zero for all four shadow
    /// phases. `shadow_reused` is what tells a census that those zeroes mean
    /// "free", not "never measured" — without it the second call would drag
    /// every machinery percentile down while looking exactly like a fast one.
    #[tokio::test]
    async fn a_memo_hit_reports_zero_shadow_phases_and_says_why() {
        let root = scaffold("phases_memo").await;
        std::fs::write(root.join("impl.txt"), "new behavior\n").unwrap();
        std::fs::write(root.join("witness.sh"), "grep -q 'new behavior' impl.txt\n").unwrap();

        let (dx, records) = Dx::capturing();
        let memo: ShadowMemoHandle = Arc::new(ShadowMemo::default());
        memo.arm();
        let tool = VerifyDone::new(memo, Some(Arc::new(dx)));

        assert!(!tool.execute(&witness_input(), &root).await.is_error());
        assert!(!tool.execute(&witness_input(), &root).await.is_error());

        let all = records.records();
        let hits: Vec<_> = all
            .iter()
            .filter(|record| record.code.as_str() == PHASES_CODE)
            .collect();
        assert_eq!(hits.len(), 2, "one record per call, hit or miss");

        assert_eq!(
            hits[0].fields.get("shadow_reused"),
            Some(&FieldValue::Bool(false))
        );
        assert_eq!(
            hits[1].fields.get("shadow_reused"),
            Some(&FieldValue::Bool(true))
        );
        for phase in ["worktree_add", "stage_tests", "shadow_run", "cleanup"] {
            assert_eq!(
                uint(hits[1], phase),
                0,
                "a reused observation ran no `{phase}`"
            );
        }

        std::fs::remove_dir_all(&root).ok();
    }

    /// The record's code is what a census greps for, and the field names are
    /// what it reads. Both are load-bearing for #2486's definition of done, so
    /// they are pinned here rather than left to the emit site.
    #[test]
    fn the_record_names_every_phase() {
        let (dx, records) = Dx::capturing();
        Phases {
            total: Duration::from_millis(900),
            working_tree_run: Duration::from_millis(100),
            shadow_run: Duration::from_millis(200),
            verdict: Verdict::Confirmed,
            ..Phases::default()
        }
        .emit(&dx);

        let record = records.find(PHASES_CODE).expect("a record per call");
        for field in [
            "setup",
            "prune",
            "baseline",
            "working_tree_run",
            "worktree_add",
            "stage_tests",
            "shadow_run",
            "cleanup",
            "machinery",
            "total",
        ] {
            assert!(
                record.fields.get(field).is_some(),
                "phase `{field}` missing from the record"
            );
        }
        assert_eq!(
            record.fields.get("verdict"),
            Some(&stella_diag::FieldValue::Str(stella_diag::StaticStr::new(
                "confirmed"
            )))
        );
    }

    /// Machinery is the total minus the two runs — including any time the
    /// named phases do not account for. Summing the named phases instead
    /// would let unattributed time disappear, which is the direction that
    /// flatters us.
    #[test]
    fn machinery_is_the_total_minus_both_runs_including_unattributed_time() {
        let phases = Phases {
            total: Duration::from_millis(1_000),
            working_tree_run: Duration::from_millis(100),
            shadow_run: Duration::from_millis(200),
            // Deliberately less than the 700ms that remains: the rest is
            // unattributed and must still count as machinery.
            worktree_add: Duration::from_millis(50),
            ..Phases::default()
        };
        assert_eq!(phases.machinery(), Duration::from_millis(700));
    }

    /// A clock that goes backwards must not panic — invariant #5 covers
    /// arithmetic on runtime measurements too.
    #[test]
    fn machinery_saturates_rather_than_underflowing() {
        let phases = Phases {
            total: Duration::from_millis(1),
            working_tree_run: Duration::from_millis(500),
            shadow_run: Duration::from_millis(500),
            ..Phases::default()
        };
        assert_eq!(phases.machinery(), Duration::ZERO);
    }
}
