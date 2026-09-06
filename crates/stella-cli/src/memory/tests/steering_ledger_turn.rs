//! The steering allowance is one turn's, and this is the seam that says
//! when a turn starts.
//!
//! `stella_core::steering::ledger` holds the arithmetic and tests it there.
//! This asks the other half of the question. Every driver opens a turn
//! through `inject_opening_recall`, so that is where the ledger is told a new
//! turn began. Leave the call out and the arithmetic is still right and
//! nothing ever resets: the spend grows all session, and an operator who
//! raises the allowance is charged for every turn behind them.

use stella_core::steering::ledger::SteeringLedger;
use stella_core::steering::tools::ToolBudget;

use crate::memory::recall::RecalledBlock;
use crate::memory::{RECALL_MARKER, inject_opening_recall};

/// A block shaped the way a rendered one is — marked, so the injection's own
/// dedup reads it as a recall block rather than as ordinary talk.
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
/// Before the turn boundary the spend was a running total, so this read the
/// sum of the two blocks. Twenty turns in, the sum was larger than the
/// allowance itself and the tool array emptied.
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
