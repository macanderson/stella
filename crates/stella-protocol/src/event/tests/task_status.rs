//! The task board's wire vocabulary: [`TaskStatus`] and the board snapshots
//! that carry it.
//!
//! A child of [`super`] rather than more lines in `tests.rs`, which sits
//! against the 1500-line ratchet — the same `registry.rs` precedent that file
//! itself cites (#1122).

use super::super::*;

#[test]
fn task_update_roundtrips_a_full_board_snapshot() {
    let event = AgentEvent::TaskUpdate {
        tasks: vec![
            TaskItem {
                id: "1".into(),
                subject: "Map the auth module".into(),
                description: None,
                status: TaskStatus::Completed,
                owner: Some("lead".into()),
                contract: None,
            },
            TaskItem {
                id: "2".into(),
                subject: "Fix the redirect loop".into(),
                description: Some("token refresh races the redirect".into()),
                status: TaskStatus::InProgress,
                owner: Some("sub:2".into()),
                contract: None,
            },
            TaskItem {
                id: "3".into(),
                subject: "Add a witness test".into(),
                description: None,
                status: TaskStatus::Pending,
                owner: None,
                contract: None,
            },
        ],
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"type\":\"task_update\""), "{json}");
    // Absent optionals are omitted, not serialized as null.
    assert!(!json.contains("null"), "{json}");
    let back: AgentEvent = serde_json::from_str(&json).unwrap();
    match back {
        AgentEvent::TaskUpdate { tasks } => {
            assert_eq!(tasks.len(), 3);
            assert_eq!(tasks[1].status, TaskStatus::InProgress);
            assert_eq!(tasks[1].owner.as_deref(), Some("sub:2"));
        }
        other => panic!("unexpected case: {other:?}"),
    }
}

#[test]
fn task_status_open_vs_terminal() {
    assert!(TaskStatus::Pending.is_open());
    assert!(TaskStatus::InProgress.is_open());
    assert!(!TaskStatus::Completed.is_open());
    assert!(!TaskStatus::Cancelled.is_open());
    // A gate awaiting its verdict, and a task a red gate stopped, both still
    // move: SPEC 8.1's `merge blocked · unblocks on green` is the transition
    // that a terminal answer here would forbid.
    assert!(TaskStatus::Verify.is_open());
    assert!(TaskStatus::Blocked.is_open());
}

/// Every status crosses the crate boundary byte-for-byte (AGENTS.md #4),
/// under the snake_case token `stella-store` writes into `tasks.status`. Each
/// token is written out here: a round-trip through `to_string`/`from_str`
/// alone passes just as happily on a renamed one.
#[test]
fn every_task_status_roundtrips_under_its_wire_token() {
    for (status, token) in [
        (TaskStatus::Pending, "pending"),
        (TaskStatus::InProgress, "in_progress"),
        (TaskStatus::Completed, "completed"),
        (TaskStatus::Cancelled, "cancelled"),
        (TaskStatus::Verify, "verify"),
        (TaskStatus::Blocked, "blocked"),
    ] {
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, format!("\"{token}\""));
        let back: TaskStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, status);
    }
}

/// The board snapshot carries the two new states over the wire, not just the
/// enum in isolation — a `TaskItem` is what a surface actually receives.
#[test]
fn a_board_snapshot_carries_a_verifying_and_a_blocked_task() {
    let event = AgentEvent::TaskUpdate {
        tasks: vec![
            TaskItem {
                id: "1".into(),
                subject: "Run the gates".into(),
                description: None,
                status: TaskStatus::Verify,
                owner: None,
                contract: None,
            },
            TaskItem {
                id: "2".into(),
                subject: "Ship it".into(),
                description: None,
                status: TaskStatus::Blocked,
                owner: None,
                contract: None,
            },
        ],
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"status\":\"verify\""), "{json}");
    assert!(json.contains("\"status\":\"blocked\""), "{json}");
    let back: AgentEvent = serde_json::from_str(&json).unwrap();
    match back {
        AgentEvent::TaskUpdate { tasks } => {
            assert_eq!(tasks[0].status, TaskStatus::Verify);
            assert_eq!(tasks[1].status, TaskStatus::Blocked);
        }
        other => panic!("unexpected case: {other:?}"),
    }
}
