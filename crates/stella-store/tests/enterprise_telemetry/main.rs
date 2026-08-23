use stella_protocol::AgentEvent;
use stella_store::enterprise_telemetry::{
    ClaimedOperationalEvent, EnqueueOutcome, EnterpriseExportSkipReason, EnterpriseTelemetrySpool,
    ManagedModelDimension, OperationalEventContext, OperationalIdentity, SpoolLimits,
    StellaOperationalEventV1, load_or_create_installation_uuid,
};
use stella_store::usage::ExecutionRollupRow;
use stella_store::{FileTouchRow, Store, StoreError, TelemetryRow};

trait ClaimBatchAt {
    fn claim_batch_at(
        &self,
        sink_fingerprint: &str,
        owner: &str,
        now_ms: i64,
        lease_ms: i64,
        max_events: usize,
        max_payload_bytes: usize,
    ) -> Result<Vec<ClaimedOperationalEvent>, StoreError>;
}

impl ClaimBatchAt for EnterpriseTelemetrySpool {
    fn claim_batch_at(
        &self,
        sink_fingerprint: &str,
        owner: &str,
        now_ms: i64,
        lease_ms: i64,
        max_events: usize,
        max_payload_bytes: usize,
    ) -> Result<Vec<ClaimedOperationalEvent>, StoreError> {
        let observed_clock = self.observe_claim_clock(sink_fingerprint, now_ms)?;
        self.claim_batch(
            sink_fingerprint,
            owner,
            observed_clock,
            lease_ms,
            max_events,
            max_payload_bytes,
        )
    }
}

fn rollup(execution_id: i64) -> ExecutionRollupRow {
    ExecutionRollupRow {
        usage_complete: true,
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
    OperationalEventContext::new(
        "enroll_01",
        "org_01",
        "workspace_01",
        OperationalIdentity::new(
            "11111111-1111-4111-8111-111111111111",
            "22222222-2222-4222-8222-222222222222",
        )
        .unwrap(),
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        [ManagedModelDimension::new("anthropic", "anthropic/claude-sonnet-4").unwrap()],
    )
    .unwrap()
}

const SINK_A: &str = "sink_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SINK_B: &str = "sink_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

mod export_ledger;
mod spool;
