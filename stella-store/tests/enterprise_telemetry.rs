use stella_store::enterprise_telemetry::{
    EnterpriseTelemetrySpool, OperationalEventContext, SpoolLimits, StellaOperationalEventV1,
};
use stella_store::usage::ExecutionRollupRow;

fn rollup(execution_id: i64) -> ExecutionRollupRow {
    ExecutionRollupRow {
        project_id: "local-project-hash-must-not-escape".into(),
        project_name: "secret-project-name".into(),
        project_root: "/secret/source/path".into(),
        execution_id,
        kind: "run".into(),
        prompt_digest: "secret-prompt-digest".into(),
        prompt_preview: "secret prompt source args results reasoning errors git memory rules"
            .into(),
        model: "anthropic/claude-sonnet-4".into(),
        provider: "anthropic".into(),
        outcome: "completed".into(),
        cost_usd: 0.125,
        input_tokens: 11,
        output_tokens: 7,
        duration_ms: 42,
        tool_calls: 3,
        files_written: 2,
        produced_output: true,
        self_rating: Some(5),
        started_at: "2026-07-21 12:00:00".into(),
        day: "2026-07-21".into(),
        tool_histogram: Vec::new(),
    }
}

fn context() -> OperationalEventContext {
    OperationalEventContext::new("enroll_01", "org_01", "workspace_01").unwrap()
}

#[test]
fn event_is_deterministic_and_serializes_only_content_free_fields() {
    let a = StellaOperationalEventV1::from_finalized_rollup(&context(), &rollup(7)).unwrap();
    let b = StellaOperationalEventV1::from_finalized_rollup(&context(), &rollup(7)).unwrap();
    let different =
        StellaOperationalEventV1::from_finalized_rollup(&context(), &rollup(8)).unwrap();

    assert_eq!(a.event_id(), b.event_id());
    assert_ne!(a.event_id(), different.event_id());

    let json = serde_json::to_string(&a).unwrap();
    for forbidden in [
        "secret",
        "source",
        "args",
        "results",
        "reasoning",
        "errors",
        "git",
        "memory",
        "rules",
        "local-project-hash",
        "execution_id",
        "project_id",
        "prompt",
        "path",
    ] {
        assert!(!json.contains(forbidden), "leaked {forbidden}: {json}");
    }
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["schema"], "stella.operational.v1");
    assert_eq!(value["event_class"], "execution_rollup");
    assert_eq!(value["cost_microusd"], 125_000);
    assert_eq!(value["changed_file_count"], 2);
    assert_eq!(value["provider"], "anthropic");
    assert_eq!(value["model"], "anthropic/claude-sonnet-4");

    let mut unknown = value.clone();
    unknown["prompt"] = serde_json::json!("forbidden");
    assert!(serde_json::from_value::<StellaOperationalEventV1>(unknown).is_err());
    let mut invalid_provider = value;
    invalid_provider["provider"] = serde_json::json!("evil/path");
    assert!(serde_json::from_value::<StellaOperationalEventV1>(invalid_provider).is_err());
    let mut invalid_id: serde_json::Value = serde_json::from_str(&json).unwrap();
    invalid_id["event_id"] = serde_json::json!("local-execution-7");
    assert!(serde_json::from_value::<StellaOperationalEventV1>(invalid_id).is_err());
}

#[test]
fn event_rejects_unfinished_or_unbounded_rollups() {
    let mut unfinished = rollup(1);
    unfinished.outcome.clear();
    assert!(StellaOperationalEventV1::from_finalized_rollup(&context(), &unfinished).is_err());

    let invalid = OperationalEventContext::new("enroll 01", "org_01", "workspace_01");
    assert!(invalid.is_err());

    let mut path_like_model = rollup(2);
    path_like_model.model = "../../secret/model".into();
    assert!(StellaOperationalEventV1::from_finalized_rollup(&context(), &path_like_model).is_err());
}

#[test]
fn spool_is_idempotent_bounded_and_evicts_oldest_with_durable_drop_count() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("enterprise-telemetry.db");
    let spool = EnterpriseTelemetrySpool::open_at(
        &path,
        SpoolLimits {
            max_rows: 2,
            max_bytes: 64 * 1024,
        },
    )
    .unwrap();
    let first = StellaOperationalEventV1::from_finalized_rollup(&context(), &rollup(1)).unwrap();
    let second = StellaOperationalEventV1::from_finalized_rollup(&context(), &rollup(2)).unwrap();
    let third = StellaOperationalEventV1::from_finalized_rollup(&context(), &rollup(3)).unwrap();

    assert!(spool.enqueue(&first, 10).unwrap());
    assert!(
        !spool.enqueue(&first, 11).unwrap(),
        "deterministic idempotency"
    );
    assert!(spool.enqueue(&second, 20).unwrap());
    assert!(spool.enqueue(&third, 30).unwrap());

    let status = spool.status().unwrap();
    assert_eq!(status.pending_rows, 2);
    assert_eq!(status.dropped_rows, 1);
    let claimed = spool
        .claim_batch("worker", 40, 1_000, 10, 64 * 1024)
        .unwrap();
    let ids: Vec<_> = claimed.iter().map(|item| item.event.event_id()).collect();
    assert!(
        !ids.contains(&first.event_id()),
        "oldest event was not evicted"
    );

    drop(spool);
    let reopened = EnterpriseTelemetrySpool::open_at(&path, SpoolLimits::default()).unwrap();
    assert_eq!(reopened.status().unwrap().dropped_rows, 1);
}

#[test]
fn claims_are_transactional_retryable_and_expired_leases_recover() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("enterprise-telemetry.db");
    let spool = EnterpriseTelemetrySpool::open_at(&path, SpoolLimits::default()).unwrap();
    for id in 1..=2 {
        let event =
            StellaOperationalEventV1::from_finalized_rollup(&context(), &rollup(id)).unwrap();
        spool.enqueue(&event, id).unwrap();
    }

    let a = spool.claim_batch("worker-a", 10, 50, 1, 64 * 1024).unwrap();
    assert_eq!(a.len(), 1);
    let b = spool
        .claim_batch("worker-b", 10, 50, 10, 64 * 1024)
        .unwrap();
    assert_eq!(b.len(), 1);
    assert_ne!(a[0].event.event_id(), b[0].event.event_id());

    spool.retry("worker-a", &a, 20).unwrap();
    assert!(
        spool
            .claim_batch("worker-c", 20, 50, 10, 64 * 1024)
            .unwrap()
            .is_empty(),
        "backoff keeps a failed request retryable but not hot-looping"
    );
    let recovered = spool
        .claim_batch("worker-d", 100, 50, 10, 64 * 1024)
        .unwrap();
    assert_eq!(recovered.len(), 1, "worker-b lease recovered after expiry");
    spool.ack("worker-d", &recovered).unwrap();
    let retried = spool
        .claim_batch("worker-c", 2_000, 50, 10, 64 * 1024)
        .unwrap();
    assert_eq!(retried.len(), 1);
    spool.ack("worker-c", &retried).unwrap();
    assert_eq!(spool.status().unwrap().pending_rows, 0);
}

#[test]
fn claim_api_rejects_unbounded_batch_requests() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("enterprise-telemetry.db");
    let spool = EnterpriseTelemetrySpool::open_at(&path, SpoolLimits::default()).unwrap();

    assert!(
        spool
            .claim_batch("worker", 10, 1_000, 1_001, 64 * 1024)
            .is_err()
    );
    assert!(
        spool
            .claim_batch("worker", 10, 1_000, 10, 16 * 1024 * 1024 + 1)
            .is_err()
    );
}

#[test]
fn separate_connections_cannot_claim_the_same_event_concurrently() {
    use std::sync::{Arc, Barrier};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("enterprise-telemetry.db");
    let first = EnterpriseTelemetrySpool::open_at(&path, SpoolLimits::default()).unwrap();
    let event = StellaOperationalEventV1::from_finalized_rollup(&context(), &rollup(1)).unwrap();
    first.enqueue(&event, 1).unwrap();
    let second = EnterpriseTelemetrySpool::open_at(&path, SpoolLimits::default()).unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let a_barrier = barrier.clone();
    let a = std::thread::spawn(move || {
        a_barrier.wait();
        first.claim_batch("a", 10, 1_000, 1, 64 * 1024).unwrap()
    });
    let b_barrier = barrier.clone();
    let b = std::thread::spawn(move || {
        b_barrier.wait();
        second.claim_batch("b", 10, 1_000, 1, 64 * 1024).unwrap()
    });
    barrier.wait();
    let claimed = a.join().unwrap().len() + b.join().unwrap().len();
    assert_eq!(claimed, 1);
}

#[test]
fn byte_limit_and_owner_only_file_mode_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("host-data/enterprise-telemetry.db");
    let event = StellaOperationalEventV1::from_finalized_rollup(&context(), &rollup(1)).unwrap();
    let one_event_bytes = serde_json::to_vec(&event).unwrap().len() as u64;
    let spool = EnterpriseTelemetrySpool::open_at(
        &path,
        SpoolLimits {
            max_rows: 10,
            max_bytes: one_event_bytes + 8,
        },
    )
    .unwrap();
    spool.enqueue(&event, 1).unwrap();
    let second = StellaOperationalEventV1::from_finalized_rollup(&context(), &rollup(2)).unwrap();
    spool.enqueue(&second, 2).unwrap();
    let status = spool.status().unwrap();
    assert_eq!(status.pending_rows, 1);
    assert_eq!(status.dropped_rows, 1);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::symlink_metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
