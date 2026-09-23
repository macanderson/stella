//! The steering allowance is one turn's, and this is the seam that says
//! when a turn starts.
//!
//! `stella_core::steering::ledger` holds the arithmetic and tests it there.
//! This asks the other half of the question. Every driver opens a turn
//! through `inject_opening_recall`, so that is where the ledger is told a new
//! turn began. Leave the call out and the arithmetic is still right. Nothing
//! resets, so the spend grows all session. An operator who raises the
//! allowance is then charged for every turn behind them.

use stella_core::steering::ledger::SteeringLedger;
use stella_core::steering::tools::ToolBudget;

use crate::memory::recall::RecalledBlock;
use crate::memory::{RECALL_MARKER, inject_opening_recall};

/// A block shaped the way a rendered one is. It carries the marker, so the
/// injection's dedup reads it as a recall block and not as ordinary talk.
fn rendered(body: &str) -> String {
    format!("{RECALL_MARKER}\n{body}")
}

/// The recall a turn hands the injection. Only the text matters here; the
/// skills and handles beside it are another seam's subject.
fn block(text: &str) -> RecalledBlock {
    RecalledBlock {
        text: Some(text.to_string()),
        ..RecalledBlock::default()
    }
}

/// **The witness.** Two turns, two distinct blocks, one ledger.
/// The allowance is charged for the turn that is open, not for both.
///
/// Before the turn boundary the spend was a running total. This test read the
/// sum of both blocks. Twenty turns in, that sum passed the allowance and the
/// tool array emptied.
#[test]
fn a_turn_charges_the_allowance_for_its_own_block_alone() {
    let ledger = SteeringLedger::new();
    let mut messages = Vec::new();

    let first = rendered("turn one recalled the billing migration window");
    let second = rendered("turn two recalled the streaming dialect notes, at some length");

    inject_opening_recall(&mut messages, block(&first), &ledger);
    inject_opening_recall(&mut messages, block(&second), &ledger);

    assert_eq!(
        ledger.spent(),
        stella_protocol::estimate_tokens(&second),
        "the open turn is charged for its own block"
    );
    assert!(
        stella_protocol::estimate_tokens(&first) > 0,
        "and turn one's block was not free, so the two readings differ"
    );
}

/// The array a session settles holds still across turns. It sits ahead of
/// the prompt in every cache, so re-ranking it bills the whole chat again.
/// A new turn resets the spend and must not touch the answer.
#[test]
fn a_later_turn_does_not_move_an_array_the_session_settled() {
    let ledger = SteeringLedger::new();
    let mut messages = Vec::new();
    let declared = ToolBudget {
        max_tokens: 8_000,
        mcp_max_tokens: 4_000,
    };

    inject_opening_recall(&mut messages, block(&rendered("turn one")), &ledger);
    let settled = ledger.settle(declared);

    inject_opening_recall(
        &mut messages,
        block(&rendered(
            "turn two, a much longer block than the one before it",
        )),
        &ledger,
    );

    assert_eq!(ledger.settle(declared), settled);
}

/// Every production door that charges the steering ledger, by file.
///
/// `SteeringLedger::spend` charges whichever turn `open_turn` opened last. It
/// has no way to know whether that turn is the one its caller is in. The only
/// place a turn opens is `inject_opening_recall`, and a spender is right only
/// if it runs after that call on the same turn. Charge from a path that never
/// reaches it, and the cost lands on a turn that has already ended. The next
/// re-settle then reads the previous turn's block plus this cost, and the tool
/// array shrinks for a reason no operator can find.
///
/// The injection's own `spend` is paired by construction: it opens the turn and
/// charges it in one body. A `ContextAllowance` hands the ledger to the plugin
/// context plane, which charges it in rounds after the driver has opened the
/// turn. A new entry here is a new door, and whoever adds one answers the
/// question this test cannot: which turn is open when it runs.
///
/// Scanned from source for the reason
/// `every_recalling_driver_routes_its_block_through_the_opening_seam` is. The
/// question is about call sites, and no run of one door can see another.
const LEDGER_SPENDERS: [(&str, &[&str]); 2] = [
    (".spend(", &["memory/recall.rs"]),
    ("ContextAllowance::new(", &["plugin_steering.rs"]),
];

/// No production code in `stella-cli` charges the steering ledger except
/// through the doors [`LEDGER_SPENDERS`] names.
#[test]
fn every_steering_ledger_spender_is_one_the_turn_boundary_accounts_for() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_production_sources(&src, &src, &mut files);
    assert!(
        files.iter().any(|(name, _)| name == "memory/recall.rs"),
        "the scan must reach the turn-opening seam, or it proves nothing"
    );

    for (pattern, allowed) in LEDGER_SPENDERS {
        let mut found: Vec<&str> = files
            .iter()
            .filter(|(_, text)| {
                production_lines(text)
                    .iter()
                    .any(|line| line.contains(pattern))
            })
            .map(|(name, _)| name.as_str())
            .collect();
        found.sort_unstable();
        assert_eq!(
            found, allowed,
            "`{pattern}` in production stella-cli code is a door into the \
             steering ledger. A spend charges whichever turn \
             inject_opening_recall opened last, so a door off that path bills \
             a turn that already ended. Route the cost through the seam, or \
             add the file to LEDGER_SPENDERS once you can say which turn is \
             open when it runs. If the receiver is not a SteeringLedger, \
             narrow the pattern rather than listing the file."
        );
    }
}

/// Every `.rs` file under `dir` that ships, keyed by its path below `root`
/// with `/` separators. Test-only files are left out: a `tests.rs`, and
/// anything under a `tests/` directory.
fn collect_production_sources(
    root: &std::path::Path,
    dir: &std::path::Path,
    out: &mut Vec<(String, String)>,
) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry
            .unwrap_or_else(|e| panic!("list {}: {e}", dir.display()))
            .path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if name != "tests" {
                collect_production_sources(root, &path, out);
            }
        } else if name.ends_with(".rs") && name != "tests.rs" {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            out.push((relative, text));
        }
    }
}

/// The lines of `text` that ship: comments dropped, and the body of every
/// `#[cfg(test)]` item whose head opens a block.
///
/// The body ends at the first `}` at the head's own indent. That is where
/// rustfmt puts it, and CI holds the tree to rustfmt. Do not cut to the end of
/// the file. Some files declare `#[cfg(test)] mod tests;` above their code.
/// Some keep code below a test module. A gated item whose head does not end in
/// `{` stays in. So a mistake here is a false alarm on test code. It is never
/// a spender the scan missed.
fn production_lines(text: &str) -> Vec<&str> {
    fn indent(line: &str) -> usize {
        line.len() - line.trim_start().len()
    }

    let lines: Vec<&str> = text.lines().collect();
    let mut shipped = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if trimmed.starts_with("#[cfg(test)]") {
            // Skip the other attributes to reach the item itself.
            let head = (i + 1..lines.len())
                .find(|&j| !lines[j].trim_start().starts_with("#["))
                .unwrap_or(lines.len());
            if lines.get(head).is_some_and(|l| l.trim_end().ends_with('{')) {
                let depth = indent(lines[head]);
                let close = (head + 1..lines.len())
                    .find(|&j| indent(lines[j]) == depth && lines[j].trim_start().starts_with('}'))
                    .unwrap_or(lines.len());
                i = close + 1;
                continue;
            }
        }
        if !trimmed.starts_with("//") {
            shipped.push(lines[i]);
        }
        i += 1;
    }
    shipped
}

/// The scan's filter drops test bodies and nothing else. A filter that cut
/// too much would pass the guard above over a real spender.
#[test]
fn the_production_filter_keeps_code_around_test_modules() {
    let text = "\
#[cfg(test)]
mod tests;

fn before() { ledger.spend(1); }

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod inline {
    fn helper() {
        ledger.spend(2);
    }
}

// ledger.spend(3) in a comment
fn after() { ledger.spend(4); }
";
    let spends: Vec<&str> = production_lines(text)
        .into_iter()
        .filter(|line| line.contains(".spend("))
        .collect();
    assert_eq!(
        spends,
        [
            "fn before() { ledger.spend(1); }",
            "fn after() { ledger.spend(4); }"
        ]
    );
}
