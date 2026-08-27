//! The gate board's wire contract (AGENTS.md #4): every gate state round-trips
//! byte-for-byte, an empty board elides rather than writing empty cells, and an
//! abstention is not counted as a failure.
//!
//! Split from `event/tests.rs` for `tag_table`'s reason (#1857): the parent
//! gains a test per new case and sits inside the file-size ratchet, so a case
//! that wants three tests brings its own file.

use super::*;

/// AGENTS.md #4 for SPEC 8.1's board: every gate state round-trips
/// byte-for-byte, and the wire spells each one the way the schema publishes it.
#[test]
fn a_gate_board_roundtrips_every_gate_state() {
    let board = GateBoard {
        patch: Some("patch-7".into()),
        gates: vec![
            GateRow {
                name: "tests-green".into(),
                state: GateState::Green,
                deterministic: true,
            },
            GateRow {
                name: "witness-flip".into(),
                state: GateState::Failed {
                    case: "budget \"p50 <= 105\" was reported as 141".into(),
                    log: "assertion failed: left == right\n  left: 141\n  right: 105".into(),
                },
                deterministic: true,
            },
            GateRow {
                name: "no-regression".into(),
                state: GateState::Undecided {
                    reason: "the oracle reported no value for \"p99\"".into(),
                },
                deterministic: true,
            },
        ],
    };
    let event = AgentEvent::GateBoard {
        board: board.clone(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "gate_board");
    assert_eq!(json["board"]["patch"], "patch-7");
    assert_eq!(json["board"]["gates"][0]["state"], "green");
    assert!(json["board"]["gates"][1]["state"]["failed"]["case"].is_string());
    assert!(json["board"]["gates"][2]["state"]["undecided"]["reason"].is_string());

    let back: AgentEvent = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(serde_json::to_value(&back).unwrap(), json);
    let AgentEvent::GateBoard { board: back } = back else {
        panic!("a gate board decodes as one");
    };
    assert_eq!(back, board);
    assert_eq!(back.green(), 1);
    assert_eq!(back.total(), 3);
    assert!(back.has_failure());
}

/// A board with nothing to say elides rather than writing empty cells, and a
/// stream carrying only `{}` still decodes — the additive posture every payload
/// in this module keeps.
#[test]
fn an_empty_gate_board_elides_both_of_its_fields() {
    let json = serde_json::to_value(AgentEvent::GateBoard {
        board: GateBoard::default(),
    })
    .unwrap();
    assert_eq!(json["board"], serde_json::json!({}));
    let back: AgentEvent = serde_json::from_value(json).unwrap();
    let AgentEvent::GateBoard { board } = back else {
        panic!("a gate board decodes as one");
    };
    assert_eq!(board, GateBoard::default());
}

/// An abstention is not a failure, which is the whole reason the state has
/// three answers rather than two — and the reading a renderer keys on when it
/// decides whether to spend red (SPEC 8.1).
#[test]
fn an_undecided_gate_is_neither_green_nor_a_failure() {
    let undecided = GateBoard {
        patch: None,
        gates: vec![GateRow {
            name: "flip".into(),
            state: GateState::Undecided {
                reason: "no snapshot to compare".into(),
            },
            deterministic: true,
        }],
    };
    assert!(!undecided.has_failure());
    assert_eq!(undecided.green(), 0);
    assert_eq!(undecided.total(), 1);
}
