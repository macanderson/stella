// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a turn's reflection is asked about.

use stella_protocol::CompletionMessage;

use super::InteractiveTurn;

/// The distinctive text of a turn that already reflected, several turns back.
const EARLIER_TURN: &str = "swapped db() for withTenantDb in the tenant loader";

/// **Witness (#4382).** A turn's reflection sees that turn, not the session it
/// happened in.
///
/// Both halves are asserted from one `InteractiveTurn`, because the defect was
/// that they disagreed: the gate read `messages[turn_start..]` and the evidence
/// read `messages`. So a `status` turn with one `task_list` call was mined for
/// the *previous* turn's lessons, and a `thank you` turn was self-rated on a
/// sub-agent failure six executions earlier — both rows keyed to an
/// `execution_id` whose work they did not describe.
///
/// That the earlier turn's text is genuinely in `messages` is asserted too: an
/// absence proves nothing unless the thing could have been there.
#[test]
fn a_turns_evidence_is_that_turn_and_not_the_session() {
    let mut messages = vec![
        CompletionMessage::user("fix the tenant leak"),
        CompletionMessage::assistant(EARLIER_TURN),
    ];
    let turn_start = messages.len();
    messages.push(CompletionMessage::user("status"));
    messages.push(CompletionMessage::assistant("six tasks, all done"));

    let turn = InteractiveTurn {
        messages: &messages,
        turn_start,
        friction: &[],
    };

    assert!(
        messages.iter().any(|m| m.content.contains(EARLIER_TURN)),
        "the earlier turn is in the history, so its absence below is a choice"
    );
    assert_eq!(
        turn.turn_slice(),
        &messages[turn_start..],
        "the gate and the evidence read one slice"
    );

    let evidence = turn.evidence(true);
    assert_eq!(evidence.transcript, &messages[turn_start..]);
    assert!(
        !evidence
            .transcript
            .iter()
            .any(|m| m.content.contains(EARLIER_TURN)),
        "an earlier turn's work is not this execution's to mine or rate"
    );
    assert!(
        evidence
            .transcript
            .iter()
            .any(|m| m.content.contains("status")),
        "and this turn's own work is still there"
    );
}
