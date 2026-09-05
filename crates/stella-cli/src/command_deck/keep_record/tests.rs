// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What the driver does with a marked interrupt. The failed write is here
//! too. That is the case a person must still be told about.

use super::*;

use stella_protocol::AgentEvent;

/// The notes that were sent, as plain text.
fn notes(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Inbound>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(inbound) = rx.try_recv() {
        if let Inbound::Event {
            event: AgentEvent::Text { text },
            ..
        } = inbound
        {
            out.push(text);
        }
    }
    out
}

fn marked(keep: KeepStrength, text: &str) -> Option<WorkspaceInput> {
    Some(WorkspaceInput::Interrupt {
        agent: "lead".into(),
        texts: vec![text.to_string()],
        keep: Some(keep),
    })
}

/// **The witness for the driver's half.** A marked interrupt writes the file.
/// It comes back as the plain interrupt the stop path already knows. The mark
/// is spent, so nothing can write it twice.
#[test]
fn a_marked_interrupt_writes_the_record_and_hands_back_the_plain_stop() {
    let root = tempfile::tempdir().unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    let passed = intercept(
        marked(KeepStrength::Rule, "Never force-push to main."),
        root.path(),
        &tx,
    );

    assert_eq!(
        passed,
        Some(WorkspaceInput::Interrupt {
            agent: "lead".into(),
            texts: vec!["Never force-push to main.".into()],
            keep: None,
        }),
        "the stop is untouched, and the mark is spent"
    );
    let registry = crate::context_records::load_registry(root.path());
    assert!(
        registry
            .entries
            .iter()
            .any(|entry| entry.record.record.statement == "Never force-push to main."),
        "the next session loads what was kept"
    );
    assert!(
        notes(&mut rx).iter().any(|n| n.contains("a rule (must)")),
        "the driver says what it kept"
    );
}

/// **The witness for a failed write.** The interrupt still travels. The error
/// is on screen. The worst end here is a rule a person trusts that was never
/// written.
#[test]
fn a_write_that_fails_still_hands_back_the_stop_and_says_so() {
    let root = tempfile::tempdir().unwrap();
    // `.stella/rules` is where the file has to land. A plain file there
    // cannot be a folder. So the write fails on real I/O, not on a seam made
    // up for the test.
    std::fs::create_dir_all(root.path().join(".stella")).unwrap();
    std::fs::write(root.path().join(".stella/rules"), "not a directory").unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    let passed = intercept(
        marked(KeepStrength::Guidance, "Use short sentences."),
        root.path(),
        &tx,
    );

    assert_eq!(
        passed,
        Some(WorkspaceInput::Interrupt {
            agent: "lead".into(),
            texts: vec!["Use short sentences.".into()],
            keep: None,
        }),
        "a failed save must not swallow the interrupt"
    );
    let said = notes(&mut rx);
    assert!(
        said.iter().any(|n| n.contains("keeping that failed")),
        "the failure is visible: {said:?}"
    );
}

/// **The witness for the room's record.** A broadcast at every live
/// session writes one record for this workspace, and hands the broadcast on
/// with the mark spent — so the fan-out cannot ask a target to write it
/// again.
#[test]
fn a_broadcast_writes_one_record_for_the_workspace() {
    let root = tempfile::tempdir().unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let room = |keep| {
        Some(WorkspaceInput::Whistle(stella_tui::Broadcast {
            message: "Never force-push to main.".into(),
            session: None,
            deep: true,
            interrupt: true,
            keep,
        }))
    };

    let passed = intercept(room(Some(KeepStrength::Rule)), root.path(), &tx);

    assert_eq!(passed, room(None), "the mark is spent, the stop travels");
    let registry = crate::context_records::load_registry(root.path());
    let kept: Vec<_> = registry
        .entries
        .iter()
        .filter(|entry| entry.record.record.statement == "Never force-push to main.")
        .collect();
    assert_eq!(kept.len(), 1, "one workspace, one record");
    assert!(notes(&mut rx).iter().any(|n| n.contains("a rule (must)")));

    // The fan-out hands each target this same message. Reading it again must
    // write nothing.
    let again = intercept(passed, root.path(), &tx);
    assert_eq!(again, room(None));
    assert!(
        notes(&mut rx).is_empty(),
        "a spent mark writes nothing on a second read"
    );
}

/// An unmarked message is not this file's job. It passes as it came.
#[test]
fn an_unmarked_message_passes_straight_through() {
    let root = tempfile::tempdir().unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let plain = Some(WorkspaceInput::Enqueue {
        text: "and now the tests".into(),
    });

    assert_eq!(intercept(plain.clone(), root.path(), &tx), plain);
    assert!(notes(&mut rx).is_empty(), "nothing to say about a prompt");
    assert!(
        !root.path().join(".stella/rules").exists(),
        "an unmarked message writes no record"
    );
}
