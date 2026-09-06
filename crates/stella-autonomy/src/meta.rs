//! What the loop keeps getting wrong, read from its own ledger.
//!
//! Every cycle appends a row. The rows say how much was fixed, how much was
//! filed, whether the gate was red, and which lens was open. Read together
//! they show habits that no single cycle shows.
//!
//! This module turns those habits into defects the loop files against itself.
//! Two kinds:
//!
//! - A named signal the fold already raises — `STUCK`, `NOISY`, `FRAGILE`,
//!   `STARVED`. Each of these is reported and acted on by nobody, which is
//!   what a signal with no reader always becomes.
//! - A lens that has looked several times and found nothing at all. Its
//!   backing is probably not running.
//!
//! Each finding has a fixed title, so the dedup key holds. A habit is filed
//! once, not once per cycle.
//!
//! `doc:backlog-self-driving` §4.1 is the design.

use std::collections::BTreeMap;

use crate::supply::Finding;
use crate::{Calibration, CycleRecord, Metrics, Signal, metrics, starved};

/// The label a finding about the loop carries.
///
/// The loop wasting its own cycles is a defect, and the type axis of a backlog
/// convention spells that `bug`.
pub const DEFECT_LABEL: &str = "bug";

/// How many cycles a lens must have run before its silence counts.
///
/// Three, matching the dry-streak the ladder already uses. One quiet cycle is
/// a clean tree; three in a row on one lens is a tool that is not looking.
pub const BARREN_CYCLES: u64 = 3;

/// The defects the ledger shows about the loop itself.
#[must_use]
pub fn sweep(rows: &[CycleRecord], cal: &Calibration) -> Vec<Finding> {
    let folded = metrics(rows);
    let controller = starved(cal);

    let mut out: Vec<Finding> = folded
        .signals
        .iter()
        .chain(controller.iter())
        .map(|signal| signal_finding(signal, &folded))
        .collect();

    for lens in barren_lenses(rows) {
        out.push(barren_finding(&lens));
    }
    out
}

/// One defect for one raised signal.
fn signal_finding(signal: &Signal, folded: &Metrics) -> Finding {
    let Signal { code, text } = signal;
    let Metrics {
        cycles,
        fixed,
        filed,
        new_findings,
        zero_fix_cycles,
        red_gate_cycles,
        ..
    } = folded;
    Finding {
        title: format!("the self-driving loop raises {code} against its own ledger"),
        body: format!(
            "The loop's ledger raises `{code}`: {text}.\n\n\
             Over {cycles} cycle(s) it fixed {fixed}, filed {filed}, and found \
             {new_findings} new defect(s). {zero_fix_cycles} cycle(s) fixed nothing and \
             {red_gate_cycles} ended on a red gate.\n\n\
             A signal nobody acts on trains people to skip signals, so this one is filed \
             as work rather than printed again.\n\n\
             ## How to check\n\n\
             1. Read `ledger.jsonl` in this repository's self-driving state directory.\n\
             2. Find which cycles raise `{code}` and what they have in common.\n\
             3. Fix the cause, or change the rule that raises it.\n\n\
             ## Definition of done\n\n\
             - [ ] The cause of `{code}` is named, with the ledger rows that show it.\n\
             - [ ] Either the loop stops meeting it, or the rule that raises it changes.\n"
        ),
        labels: vec![DEFECT_LABEL.to_owned()],
    }
}

/// One defect for one lens that never finds anything.
fn barren_finding(lens: &str) -> Finding {
    Finding {
        title: format!("the `{lens}` lens has looked several times and found nothing"),
        body: format!(
            "The `{lens}` rung of the aperture ladder has run at least {BARREN_CYCLES} \
             cycles and has never produced a new finding.\n\n\
             That is one of two things. Either the tree is clean on this question, or the \
             backing the lens declares is not running. The second is far more likely, and \
             it is silent: a lens whose tool fails reports the same nothing as a lens whose \
             answer is nothing.\n\n\
             ## How to check\n\n\
             1. Read the backing `{lens}` declares in the aperture list \
             (`stella self-driving aperture --list`).\n\
             2. Run that tool by hand against a clean tree.\n\
             3. If it runs and finds nothing, say so on this issue and close it. If it \
             fails, fix the tool or the declaration.\n\n\
             ## Definition of done\n\n\
             - [ ] The `{lens}` backing has been run by hand and its output read.\n\
             - [ ] Either the tool is fixed, or this issue records that the silence is \
             real.\n"
        ),
        labels: vec![DEFECT_LABEL.to_owned()],
    }
}

/// Lenses that have run enough cycles to be judged and found nothing at all.
fn barren_lenses(rows: &[CycleRecord]) -> Vec<String> {
    let mut tally: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    for row in rows {
        let name = row.aperture.trim();
        if name.is_empty() || name == crate::WATCH {
            continue;
        }
        let entry = tally.entry(name).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += row.new_findings;
    }
    tally
        .into_iter()
        .filter(|(_, (cycles, found))| *cycles >= BARREN_CYCLES && *found == 0)
        .map(|(name, _)| name.to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AimdLimits;

    fn row(aperture: &str, fixed: u64, filed: u64, found: u64, gate: &str) -> CycleRecord {
        CycleRecord {
            cycle: 1,
            run_id: "r".to_owned(),
            ended_at: String::new(),
            ended_at_unix: 0,
            fixed,
            filed,
            new_findings: found,
            bench: String::new(),
            gate: gate.to_owned(),
            prs: Vec::new(),
            tier: String::new(),
            aperture: aperture.to_owned(),
            lens_tool: None,
            outcome: String::new(),
            minutes: 0,
            dry: found == 0,
            extra: serde_json::Map::new(),
        }
    }

    fn seeded() -> Calibration {
        Calibration::seeded(&AimdLimits::default())
    }

    /// **The witness.** A lens that has looked three times and found nothing
    /// becomes a defect about the lens.
    #[test]
    fn a_lens_that_never_finds_anything_becomes_a_finding() {
        let rows = vec![
            row("properties", 1, 0, 0, "green"),
            row("properties", 1, 0, 0, "green"),
            row("properties", 1, 0, 0, "green"),
        ];

        let found = sweep(&rows, &seeded());

        assert_eq!(found.len(), 1, "got {found:?}");
        assert!(found[0].title.contains("properties"));
        assert_eq!(found[0].labels, vec![DEFECT_LABEL.to_owned()]);
    }

    /// Two cycles is not enough to judge a lens.
    #[test]
    fn two_quiet_cycles_are_not_enough() {
        let rows = vec![
            row("properties", 1, 0, 0, "green"),
            row("properties", 1, 0, 0, "green"),
        ];

        assert!(sweep(&rows, &seeded()).is_empty());
    }

    /// A lens that found something is not barren, however many cycles it ran.
    #[test]
    fn a_lens_that_found_something_is_not_barren() {
        let rows = vec![
            row("rubric", 1, 0, 0, "green"),
            row("rubric", 1, 0, 2, "green"),
            row("rubric", 1, 0, 0, "green"),
            row("rubric", 1, 0, 0, "green"),
        ];

        assert!(sweep(&rows, &seeded()).is_empty());
    }

    /// `watch` is a mode and not a lens, so its silence says nothing.
    #[test]
    fn watch_is_never_reported_as_barren() {
        let rows = vec![
            row(crate::WATCH, 1, 0, 0, "green"),
            row(crate::WATCH, 1, 0, 0, "green"),
            row(crate::WATCH, 1, 0, 0, "green"),
        ];

        assert!(sweep(&rows, &seeded()).is_empty());
    }

    /// A raised signal becomes a defect, so it reaches the queue instead of
    /// being printed to nobody.
    #[test]
    fn a_raised_signal_becomes_a_finding() {
        let rows = vec![
            row("rubric", 0, 0, 1, "green"),
            row("rubric", 0, 0, 1, "green"),
            row("rubric", 0, 0, 1, "green"),
        ];

        let found = sweep(&rows, &seeded());

        assert!(
            found.iter().any(|f| f.title.contains("STUCK")),
            "got {:?}",
            found.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    /// An empty ledger says nothing about anything.
    #[test]
    fn an_empty_ledger_yields_nothing() {
        assert!(sweep(&[], &seeded()).is_empty());
    }
}
