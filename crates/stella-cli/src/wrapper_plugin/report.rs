// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a wrapper run tells the terminal.
//!
//! One renderer for every `! wrapper:` line — the faults, the host's
//! refusals, the child-turn and fan-out spends, and the end-of-run candidate
//! sweep — plus the summary beside them. Split out of `wrapper_plugin.rs`
//! rather than grown inside it: that file carries the flag decisions, the
//! host assembly and the driver, and sits under the 1500-line ratchet.
//!
//! Pure above the one `eprintln!`, so the wording and the attribution on it
//! are assertable without capturing stderr (`tests/report.rs`).

use std::sync::Arc;

use stella_runtime::wrapper::{
    CandidateFanoutSpend, ChildTurnSpend, DispatchReport, HostCallGate, TestRunRecord,
};

use crate::OutputFormat;

/// Print what the wrapper concluded, every point that abstained, and every
/// capability this host refused it.
///
/// The refusals are the host's half of "a refusal is reported, never silent"
/// (#3561): the plugin already read the `err` and degraded, and a user watching
/// a plugin contribute nothing needs the same sentence to know why. They print
/// in every format, like the faults beside them, because an unanswered
/// capability is a fact about the run rather than commentary about it.
///
/// stderr in every format: stdout may be machine-readable JSON, and a wrapper's
/// commentary is not part of either summary contract.
///
/// `scope` names the lane these lines came from, and is `Some` exactly where
/// several lanes share one stderr: `stella fleet --max-concurrency > 1` binds
/// one wrapper per worker attempt, so an unattributed `! wrapper: …` cannot be
/// told from the next worker's (#3883). `stella run` and `stella goal` are one
/// wrapper over one lane in one process and pass `None`.
pub(super) fn report_to(
    scope: Option<&str>,
    format: OutputFormat,
    report: &DispatchReport,
    gates: &[Arc<HostCallGate>],
    spends: &[ChildTurnSpend],
    fanouts: &[CandidateFanoutSpend],
    test_runs: &[TestRunRecord],
) {
    for line in report_lines(scope, format, report, gates, spends, fanouts, test_runs) {
        eprintln!("{line}");
    }
}

/// The lines [`report_to`] prints, in order — the composition split out from
/// the printing so the wording, and the attribution on it, is assertable
/// without capturing stderr.
///
/// The marker stays in its own column and the scope follows it, rather than
/// preceding it: an operator scanning a concurrent run's stderr for `!` is
/// looking down a column, and a variable-width task id in front of it is what
/// takes that column away.
pub(super) fn report_lines(
    scope: Option<&str>,
    format: OutputFormat,
    report: &DispatchReport,
    gates: &[Arc<HostCallGate>],
    spends: &[ChildTurnSpend],
    fanouts: &[CandidateFanoutSpend],
    test_runs: &[TestRunRecord],
) -> Vec<String> {
    let mut lines = Vec::new();
    for fault in &report.faults {
        lines.push(wrapper_line(scope, fault));
    }
    // Every member's refusals, in selection order: a composition has one gate
    // per plugin (`ResolvedWrapper::serving`), and reading only the first
    // would be silent about exactly the plugin whose ask went unanswered.
    for refused in gates.iter().flat_map(|gate| gate.refusals()) {
        lines.push(wrapper_line(scope, &refused));
    }
    for line in spend_lines(spends) {
        lines.push(wrapper_line(scope, &line));
    }
    for line in fanout_spend_lines(fanouts) {
        lines.push(wrapper_line(scope, &line));
    }
    for line in test_run_lines(test_runs) {
        lines.push(wrapper_line(scope, &line));
    }
    if format == OutputFormat::Text || !report.met() {
        let tag = scope.map_or_else(String::new, |scope| format!("[{scope}] "));
        lines.push(format!("  ◇ {tag}{}", report.summary()));
    }
    lines
}

/// The one rendering of a `! wrapper:` line, marker and scope tag included.
///
/// A free function rather than a closure inside [`report_lines`] because it
/// has a second caller: the end-of-run candidate sweep formatted the same
/// prefix by hand, which is the copy that drifts the next time the marker or
/// the tag's position changes (#4418). One lane makes a second producer
/// harmless and does not make it correct.
fn wrapper_line(scope: Option<&str>, what: &dyn std::fmt::Display) -> String {
    let tag = scope.map_or_else(String::new, |scope| format!("[{scope}] "));
    format!("  ! {tag}wrapper: {what}")
}

/// One line per candidate workspace the end-of-run sweep could not remove.
///
/// Empty for the ordinary run, in which everything went — so a clean run
/// stays silent, exactly as [`spend_lines`] does for a plugin that spent
/// nothing.
pub(super) fn sweep_lines(scope: Option<&str>, leaked: &[String]) -> Vec<String> {
    leaked
        .iter()
        .map(|leaked| wrapper_line(scope, leaked))
        .collect()
}

/// One line per child turn this host ran for the plugin, naming the seat it
/// was attributed to and what it cost.
///
/// The money half of "a refusal is reported, never silent": a plugin's child
/// turn is a model call the *user* pays for and never asked for directly, so
/// the run says so in the same place it says what the plugin was refused.
/// Pure, so the wording is testable without a run — and empty for the
/// overwhelmingly common case of a plugin that spent nothing, which keeps a
/// silent run silent.
pub(super) fn spend_lines(spends: &[ChildTurnSpend]) -> Vec<String> {
    spends
        .iter()
        .map(|spend| {
            format!(
                "spent a child turn at \"{}\" (seat {:?}, {} step(s), ${:.4}){}",
                spend.role,
                spend.seat,
                spend.steps,
                spend.cost_usd,
                if spend.completed {
                    ""
                } else {
                    " — it did not finish"
                }
            )
        })
        .collect()
}

/// One line per candidate fan-out this host performed, naming what the plugin
/// asked for, what it got, and what the difference cost.
///
/// A fan-out is the largest spend a plugin can cause on a single host call —
/// N *writing* worker turns — so a run that printed [`spend_lines`]' child
/// turns and not these would be visible about the cheap spend and silent
/// about the expensive one.
///
/// "asked 8, ran 3" and "asked 3, ran 3" are different facts about a plugin
/// and only the host knows which one happened, so the requested width is
/// printed whenever it differs from what ran — which is also how a user
/// discovers that
/// [`stella_runtime::wrapper::DEFAULT_HOST_MAX_FANOUT_WIDTH`] is why a plugin
/// scored fewer candidates than its README promised.
///
/// Pure, so the wording is testable without a run, and empty for the
/// overwhelmingly common plugin that never fans out.
pub(super) fn fanout_spend_lines(spends: &[CandidateFanoutSpend]) -> Vec<String> {
    spends
        .iter()
        .map(|spend| {
            let clamped = if spend.requested_width == spend.width {
                String::new()
            } else {
                format!(" — it asked for {}", spend.requested_width)
            };
            format!(
                "fanned out {} candidate turn(s) at \"{}\" (seat {:?}, {} finished, ${:.4}){clamped}",
                spend.width, spend.role, spend.seat, spend.completed, spend.cost_usd,
            )
        })
        .collect()
}

/// One line per test run this host performed for the plugin, naming the
/// workspace it ran in and what the assertions said.
///
/// [`spend_lines`]' sibling with the money taken out. Nothing a test run costs
/// is the user's model budget, so nothing here is an amount — but a plugin that
/// made this machine build a workspace four times is a fact about the run, and
/// the plane is the only thing that knows it happened (#4536). Reporting what
/// the assertions said, rather than only the count, is what makes the line
/// worth reading: a verifier whose every re-run was `unobserved` is a run whose
/// deadline is too short, and no other line in this report would say so.
///
/// Pure, and empty for the overwhelmingly common plugin that never asks.
pub(super) fn test_run_lines(runs: &[TestRunRecord]) -> Vec<String> {
    runs.iter()
        .map(|run| {
            format!(
                "re-ran the tests of \"{}\" ({})",
                run.candidate,
                match run.assertions {
                    stella_plugin::TestBaseline::Passed => "they passed",
                    stella_plugin::TestBaseline::Failed => "they failed",
                    stella_plugin::TestBaseline::Unobserved => "nothing was observed",
                    // Unreachable: `TestObservation::assertions` is never
                    // `NotRun` by that type's own contract, and this host
                    // refuses rather than answering with one. Rendered rather
                    // than asserted, because a binary must not abort a user's
                    // run over a variant it did not expect.
                    stella_plugin::TestBaseline::NotRun => "it did not run",
                }
            )
        })
        .collect()
}
