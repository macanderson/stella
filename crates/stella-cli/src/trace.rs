//! Trajectory trace capture (#1042 — self-improvement track, Phase 1:
//! Instrument).
//!
//! One [`TraceRecord`] per finished execution: the exact model inputs, the
//! staged path, the tool activity, and what it cost — the record a training
//! loop (SFT pairs, reward labels) needs and receipts alone don't carry.
//!
//! ## Assembly, not a second capture path
//!
//! The store already persists everything a trace needs: the receipts plane
//! holds one row per model call, the `events` journal holds the preimages
//! that make [`stella_store::Store::reconstruct_call`] byte-exact, and
//! `StepUsage`/`Stage` events carry cost and the staged path. So this module
//! deliberately captures nothing at runtime — it is a fold over what the
//! closeout just settled, run once per execution after
//! `record_execution_end`. A second live capture path would duplicate the
//! journal and could drift from it; the journal is already the append-only
//! trace store.
//!
//! ## Stage and cost attribution
//!
//! Neither is a column on the receipt. Both are recovered by ordered replay:
//! `Stage` and `StepManifest` land in the same journal with one monotonic
//! `seq`, so the last stage seen before a call's manifest is that call's
//! stage; the `StepUsage` that follows a manifest (the driver emits the pair
//! together at the settled boundary) is that call's cost. Joining
//! `telemetry.step` to `step_receipt.step` instead would be wrong: the former
//! is the event-stream seq, the latter the engine-local step.
//!
//! ## Privacy
//!
//! `prompt_messages` is raw prompt content — the most sensitive bytes this
//! workspace produces. Traces therefore live in `.stella/private/` (0700)
//! as an owner-only (0600) JSONL file, every string leaf passes through
//! [`stella_core::redact::redact_secrets`] first, and nothing here writes to
//! any store table an egress path reads (AGENTS.md invariant 3 — the
//! content-free gate never sees this data because it never enters `store.db`).

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use stella_core::redact::redact_secrets;
use stella_pipeline::reward::{RewardLabel, RewardPolicy, Settlement, TrajectoryCost, label};
use stella_protocol::{AgentEvent, CompletionMessage};
use stella_store::Store;

/// Bump when [`TraceRecord`]'s shape changes incompatibly. Readers (#872's
/// dataset exporter) skip records whose schema they don't know rather than
/// misparsing them.
///
/// `2` added the reward label (#1043), which every record now carries and an
/// older record does not — so a v1 line is skipped rather than parsed with an
/// invented label.
///
/// `3` stamps the [`RewardPolicy`] onto that label, now that the weights are
/// configurable. Every v2 line was in fact written under the defaults — the
/// weights were constants then — so a reader *could* upgrade one by assuming
/// them. The bump says no: `policy` is a required field, nothing reads traces
/// yet, and the alternative is every future reader carrying "v2 implies the
/// 2026-08 defaults" as a permanent special case. Skipping is cheaper and
/// cannot rot.
pub const TRACE_SCHEMA_VERSION: u32 = 3;

/// The append-only trace file, one JSON record per line, under
/// `.stella/private/`.
pub const TRACES_FILE: &str = "traces.jsonl";

/// One execution's complete trajectory, assembled from the store after
/// closeout. Serialized as one JSONL line in [`TRACES_FILE`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceRecord {
    pub schema: u32,
    pub execution_id: i64,
    /// `executions.kind` — "run", "chat", "goal", …
    pub kind: String,
    /// The user's prompt, secret-redacted.
    pub prompt: String,
    /// Terminal outcome label ("completed", "failed", …); `None` if the
    /// execution was never settled.
    pub outcome: Option<String>,
    /// Total settled cost of the execution (all calls + reflection).
    pub cost_usd: f64,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// The staged path actually taken, in order, consecutive repeats
    /// collapsed (the engine re-emits `execute` on every step entry).
    pub stage_trajectory: Vec<String>,
    /// Paths this execution touched, with their `[C·R·U·D]` op letters.
    pub files_touched: Vec<String>,
    /// SHA-256 (truncated to 16 hex) over the sorted `path\0ops` rows — a
    /// stable digest of the *shape* of the change, comparable across runs.
    /// Named `change_digest`, not "fingerprint": this workspace already has
    /// three unrelated fingerprints (failure, embedder, frame).
    pub change_digest: Option<String>,
    /// Whether any secret was replaced anywhere in this record. Recorded so
    /// "this was redacted" is a visible fact, not an assumption
    /// (`stella_core::redact` posture).
    pub redacted: bool,
    /// The training label this trajectory earned (#1043): the ladder rung it
    /// came to rest on, the composite reward, or the stated reason there is
    /// none.
    ///
    /// Always present, including for a discard — a trace whose label says
    /// `nothing_attempted` is exactly the trace a failure-mode study wants,
    /// and deleting it would be the second time this project lost that
    /// evidence. Carries no model-authored text by construction; see
    /// [`stella_pipeline::reward`].
    pub reward: RewardLabel,
    /// Every model call, in wire order.
    pub calls: Vec<TraceCall>,
}

/// One model call inside a trace: what it saw, what it did, what it cost.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceCall {
    pub turn_instance: u32,
    pub step: u64,
    pub call_seq: u64,
    /// `ModelCallRole`, as its wire token ("worker", "judge", …).
    pub role: String,
    /// The pipeline stage active when this call was made, as its wire token
    /// ("execute", "witness", …). `None` for calls made before any stage
    /// event (bare step-loop runs emit stages too, so this is rare).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    pub provider: String,
    pub model: String,
    /// The exact messages this call was sent, reconstructed from the receipt
    /// and verified against emission digests, then secret-redacted. Each
    /// entry is a full serialized [`CompletionMessage`] (role, content, tool
    /// calls with arguments, tool results), so SFT pairs need no other
    /// source.
    pub prompt_messages: Vec<serde_json::Value>,
    /// Whether reconstruction verified byte-exactly
    /// ([`stella_store::Reconstruction::is_verified`]). A trace with
    /// `false` here is still honest — it says so.
    pub reconstruction_verified: bool,
    /// Why reconstruction fell short, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reconstruction_error: Option<String>,
    /// Tools the model requested after this call, in order.
    pub tool_uses: Vec<TraceToolUse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// One tool invocation attributed to the call that requested it. Arguments
/// and outputs are not duplicated here — they ride the *next* call's
/// `prompt_messages` (that is what the model saw) — only the identity and
/// outcome of the invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceToolUse {
    pub name: String,
    pub call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// The `[trace:exec-N]` episode tag — the join key recall carries back to
/// the full trajectory — when capture is on and an execution row exists.
pub fn episode_tag(enabled: bool, execution_id: Option<i64>) -> Option<String> {
    match execution_id {
        Some(id) if enabled => Some(format!(" [trace:exec-{id}]")),
        _ => None,
    }
}

/// [`capture`], demoting failure to a warning: a trace failure must never
/// fail the turn it describes.
pub fn capture_or_warn(
    store: &Store,
    execution_id: i64,
    files_touched: &[(String, String)],
    workspace_root: &Path,
    policy: &RewardPolicy,
) {
    if let Err(error) = capture(store, execution_id, files_touched, workspace_root, policy) {
        eprintln!("  ⚠ trace capture failed: {error}");
    }
}

/// Assemble one execution's trace from the store, then append it to
/// `.stella/private/traces.jsonl`. Returns the file written.
pub fn capture(
    store: &Store,
    execution_id: i64,
    files_touched: &[(String, String)],
    workspace_root: &Path,
    policy: &RewardPolicy,
) -> Result<PathBuf, String> {
    let record = assemble(store, execution_id, files_touched, policy)?;
    append_trace(workspace_root, &record)
}

/// Build the [`TraceRecord`] for `execution_id`. Read after closeout so
/// `outcome`, `cost_usd`, and the full journal are settled.
pub fn assemble(
    store: &Store,
    execution_id: i64,
    files_touched: &[(String, String)],
    policy: &RewardPolicy,
) -> Result<TraceRecord, String> {
    let summary = store
        .execution_summary(execution_id)
        .map_err(|e| format!("read execution: {e}"))?
        .ok_or_else(|| format!("unknown execution {execution_id}"))?;
    let receipts = store
        .recorded_calls(execution_id)
        .map_err(|e| format!("read receipts: {e}"))?;
    let journal = store
        .execution_events(execution_id)
        .map_err(|e| format!("read journal: {e}"))?;

    let mut redacted = false;
    let fold = fold_journal(&journal.events);

    let mut calls = Vec::with_capacity(receipts.len());
    for receipt in &receipts {
        let key = (receipt.turn_instance, receipt.step, receipt.call_seq);
        let (prompt_messages, verified, error) = match store.reconstruct_call(
            execution_id,
            receipt.turn_instance,
            receipt.step,
            receipt.call_seq,
        ) {
            Ok(reconstruction) => {
                let verified = reconstruction.is_verified();
                let error = (!verified).then(|| {
                    format!(
                        "unresolved: [{}]; digest mismatches: [{}]",
                        reconstruction.unresolved.join(", "),
                        reconstruction.digest_mismatches.join(", ")
                    )
                });
                (
                    redact_messages(&reconstruction.messages, &mut redacted),
                    verified,
                    error,
                )
            }
            Err(e) => (
                Vec::new(),
                false,
                Some(format!("reconstruction failed: {e}")),
            ),
        };
        let usage = fold.usage_by_call.get(&key);
        calls.push(TraceCall {
            turn_instance: receipt.turn_instance,
            step: receipt.step,
            call_seq: receipt.call_seq,
            role: receipt.call_role.clone(),
            stage: fold.stage_by_call.get(&key).cloned().flatten(),
            provider: receipt.provider.clone(),
            model: receipt.model.clone(),
            prompt_messages,
            reconstruction_verified: verified,
            reconstruction_error: error,
            tool_uses: fold.tools_by_call.get(&key).cloned().unwrap_or_default(),
            input_tokens: usage.map(|u| u.input_tokens),
            output_tokens: usage.map(|u| u.output_tokens),
            cost_usd: usage.map(|u| u.cost_usd),
            duration_ms: usage.map(|u| u.duration_ms),
        });
    }

    let prompt = {
        let r = redact_secrets(&summary.prompt);
        redacted |= r.redacted;
        r.text
    };

    // The reward is folded from the settled record, never from a live
    // observation: the same discipline as the rest of this module, and the
    // reason a label can be recomputed under new weights from a stored trace.
    let reward = label(
        fold.settlement.unwrap_or(Settlement::Absent),
        TrajectoryCost {
            steps: u32::try_from(calls.len()).unwrap_or(u32::MAX),
            cost_usd: summary.cost_usd,
            revisions: fold.verdicts.saturating_sub(1),
        },
        policy,
    );

    Ok(TraceRecord {
        schema: TRACE_SCHEMA_VERSION,
        execution_id,
        kind: summary.kind,
        prompt,
        outcome: summary.outcome,
        cost_usd: summary.cost_usd,
        started_at: summary.started_at,
        finished_at: summary.finished_at,
        stage_trajectory: fold.stage_trajectory,
        files_touched: files_touched
            .iter()
            .map(|(path, ops)| format!("{path} ({ops})"))
            .collect(),
        change_digest: change_digest(files_touched),
        redacted,
        reward,
        calls,
    })
}

/// What one ordered pass over the journal recovers: the staged path, the
/// verification settlement, and per-call stage, cost, and tool attributions.
#[derive(Default)]
struct JournalFold {
    stage_trajectory: Vec<String>,
    stage_by_call: HashMap<(u32, u64, u64), Option<String>>,
    usage_by_call: HashMap<(u32, u64, u64), UsageBits>,
    tools_by_call: HashMap<(u32, u64, u64), Vec<TraceToolUse>>,
    /// The last `JudgeVerdict` the journal holds — the one that settled the
    /// turn. Earlier verdicts are revision rounds, counted below.
    settlement: Option<Settlement>,
    /// How many verdicts the journal holds. `saturating_sub(1)` is the
    /// revision count the reward's shaping prices; see
    /// [`TrajectoryCost::revisions`] for what that over-counts and why the
    /// direction is safe.
    verdicts: u32,
}

#[derive(Clone, Copy)]
struct UsageBits {
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: f64,
    duration_ms: u64,
}

fn fold_journal(events: &[stella_store::SessionEventRecord]) -> JournalFold {
    let mut fold = JournalFold::default();
    let mut last_stage: Option<String> = None;
    let mut current: Option<(u32, u64, u64)> = None;
    // call_id → (call key, index into its tool_uses)
    let mut open_tools: HashMap<String, ((u32, u64, u64), usize)> = HashMap::new();
    for record in events {
        match &record.event {
            AgentEvent::Stage { name } => {
                let stage = wire_token(name);
                if fold.stage_trajectory.last() != Some(&stage) {
                    fold.stage_trajectory.push(stage.clone());
                }
                last_stage = Some(stage);
            }
            AgentEvent::StepManifest {
                turn_instance,
                step,
                call_seq,
                ..
            } => {
                let key = (*turn_instance, *step as u64, *call_seq);
                fold.stage_by_call.insert(key, last_stage.clone());
                current = Some(key);
            }
            AgentEvent::StepUsage {
                input_tokens,
                output_tokens,
                cost_usd,
                duration_ms,
                ..
            } => {
                // The driver emits receipt + usage as a pair at the settled
                // boundary, so the usage following a manifest is that call's.
                if let Some(key) = current {
                    fold.usage_by_call.entry(key).or_insert(UsageBits {
                        input_tokens: *input_tokens,
                        output_tokens: *output_tokens,
                        cost_usd: *cost_usd,
                        duration_ms: *duration_ms,
                    });
                }
            }
            AgentEvent::ToolStart { call } => {
                if let Some(key) = current {
                    let uses = fold.tools_by_call.entry(key).or_default();
                    open_tools.insert(call.call_id.clone(), (key, uses.len()));
                    uses.push(TraceToolUse {
                        name: call.name.clone(),
                        call_id: call.call_id.clone(),
                        is_error: None,
                        duration_ms: None,
                    });
                }
            }
            AgentEvent::ToolResult {
                call_id,
                output,
                duration_ms,
                ..
            } => {
                if let Some((key, index)) = open_tools.remove(call_id)
                    && let Some(uses) = fold.tools_by_call.get_mut(&key)
                    && let Some(entry) = uses.get_mut(index)
                {
                    entry.is_error = Some(output.is_error());
                    entry.duration_ms = Some(*duration_ms);
                }
            }
            AgentEvent::JudgeVerdict { passed, evidence } => {
                // Overwritten on every verdict, so the last one wins: that is
                // the one that settled the turn, and the earlier ones are the
                // revision rounds this counts.
                fold.settlement = Some(Settlement::from_evidence(*passed, evidence));
                fold.verdicts = fold.verdicts.saturating_add(1);
            }
            _ => {}
        }
    }
    fold
}

/// A protocol enum's wire token (`snake_case` serde name) — the same string
/// the event stream carries, so trace consumers and stream consumers share
/// one vocabulary.
fn wire_token<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        _ => "unknown".to_string(),
    }
}

/// Serialize each message and redact every string leaf. The whole message
/// object is kept (role, content, tool calls with arguments, tool results)
/// because that IS the model input; only secret spans are replaced.
fn redact_messages(messages: &[CompletionMessage], redacted: &mut bool) -> Vec<serde_json::Value> {
    messages
        .iter()
        .filter_map(|message| serde_json::to_value(message).ok())
        .map(|mut value| {
            redact_value(&mut value, redacted);
            value
        })
        .collect()
}

fn redact_value(value: &mut serde_json::Value, redacted: &mut bool) {
    match value {
        serde_json::Value::String(s) => {
            let r = redact_secrets(s);
            if r.redacted {
                *redacted = true;
                *s = r.text;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_value(item, redacted);
            }
        }
        serde_json::Value::Object(map) => {
            for (_key, item) in map.iter_mut() {
                redact_value(item, redacted);
            }
        }
        _ => {}
    }
}

/// SHA-256 over the sorted `path\0ops` rows, truncated to 16 hex — `None`
/// when nothing was touched (a read-only run has no change shape).
fn change_digest(files_touched: &[(String, String)]) -> Option<String> {
    if files_touched.is_empty() {
        return None;
    }
    let mut rows: Vec<String> = files_touched
        .iter()
        .map(|(path, ops)| format!("{path}\0{ops}"))
        .collect();
    rows.sort();
    let mut hasher = Sha256::new();
    for row in &rows {
        hasher.update(row.as_bytes());
        hasher.update(b"\n");
    }
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    Some(hex[..16].to_string())
}

/// Append one record to `.stella/private/traces.jsonl`, owner-only. The
/// private dir is created 0700 if missing (a trace can outlive a store that
/// was opened by an older build), and the file is tightened to 0600 on every
/// append — permissions are re-asserted, not assumed.
pub fn append_trace(workspace_root: &Path, record: &TraceRecord) -> Result<PathBuf, String> {
    let dir = workspace_root.join(".stella").join("private");
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&dir)
        .map_err(|e| format!("create private dir: {e}"))?;
    let path = dir.join(TRACES_FILE);
    let line = serde_json::to_string(record).map_err(|e| format!("serialize trace: {e}"))?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    file.write_all(line.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .map_err(|e| format!("append trace: {e}"))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::{
        JudgeEvidence, LadderRung, LadderSnapshot, ModelCallRole, StageKind, ToolCall, ToolOutput,
    };
    use stella_store::{ContextBlockRow, ManifestBlockRow, StepManifestRow};

    fn gap_block(id: &str, kind: &str, content: &str) -> ContextBlockRow {
        ContextBlockRow {
            block_id: id.to_string(),
            kind: kind.to_string(),
            origin_turn: 0,
            origin_step: 0,
            call_id: None,
            memory_id: None,
            token_cost: Some(1),
            content_digest: "sha256:unchecked-local".to_string(),
            citation_label: None,
            content: Some(content.to_string()),
        }
    }

    fn manifest_entry(id: &str, message_index: u64) -> ManifestBlockRow {
        ManifestBlockRow {
            block_id: id.to_string(),
            cache_zone: "stable".to_string(),
            token_cost: Some(1),
            resident_since_step: 0,
            message_index,
            call_id: None,
        }
    }

    /// The acceptance shape of #1042: one finished run leaves one complete
    /// trace — exact (redacted) prompt messages, stage attribution, joined
    /// cost, tool activity, change digest — and the JSONL line round-trips.
    #[test]
    fn assembles_one_complete_trace_from_a_settled_execution() {
        let store = Store::in_memory().unwrap();
        let secret_prompt = "fix auth, token is ghp_0123456789abcdef0123456789abcdef012345";
        let id = store
            .begin_execution("run", secret_prompt, "zai", "glm-5.2")
            .unwrap();

        // The journal, in stream order.
        let events = [
            AgentEvent::Stage {
                name: StageKind::Triage,
            },
            AgentEvent::Stage {
                name: StageKind::Execute,
            },
            AgentEvent::StepManifest {
                turn_instance: 0,
                step: 0,
                call_seq: 0,
                role: ModelCallRole::Worker,
                provider: "zai".to_string(),
                model: "glm-5.2".to_string(),
                blocks: Vec::new(),
                effective_budget_tokens: 1000,
                calibration_factor: 1.0,
                estimated_input_tokens: 10,
                compiled_frame: None,
            },
            AgentEvent::StepUsage {
                step: 0,
                role: ModelCallRole::Worker,
                provider: "zai".to_string(),
                output_text: None,
                model: "glm-5.2".to_string(),
                input_tokens: 120,
                output_tokens: 30,
                cached_input_tokens: 0,
                cache_write_tokens: 0,
                estimated_input_tokens: 10,
                cost_usd: 0.25,
                duration_ms: 900,
                retries: 0,
                tool_calls: 1,
                complete: true,
                finish_reason: None,
            },
            AgentEvent::ToolStart {
                call: ToolCall {
                    call_id: "call_1".to_string(),
                    name: "read_file".to_string(),
                    input: serde_json::json!({"path": "src/auth.rs"}),
                },
            },
            AgentEvent::ToolResult {
                call_id: "call_1".to_string(),
                output: ToolOutput::Ok {
                    content: "fn login() {}".to_string(),
                },
                duration_ms: 42,
                speculated: false,
            },
        ];
        for (seq, event) in events.iter().enumerate() {
            store.record_event(id, seq as u64, event).unwrap();
        }

        // The receipt: two gap-kind blocks (content stored locally), so
        // reconstruction resolves without journal preimages.
        store
            .record_context_block(
                id,
                &gap_block("blk_sys", "system_prefix", "You are stella."),
            )
            .unwrap();
        store
            .record_context_block(id, &gap_block("blk_goal", "user_goal", secret_prompt))
            .unwrap();
        store
            .record_step_manifest(
                id,
                &StepManifestRow {
                    turn_instance: 0,
                    step: 0,
                    call_seq: 0,
                    provider: "zai".to_string(),
                    model: "glm-5.2".to_string(),
                    call_role: "worker".to_string(),
                    effective_budget_tokens: 1000,
                    calibration_factor: 1.0,
                    estimated_input_tokens: 10,
                    compiled_frame_id: None,
                    frame_hash: None,
                    blocks: vec![manifest_entry("blk_sys", 0), manifest_entry("blk_goal", 1)],
                },
            )
            .unwrap();
        store
            .finish_execution_accounted(id, "completed", 0.25, true)
            .unwrap();

        let files = vec![("src/auth.rs".to_string(), "U".to_string())];
        let record = assemble(&store, id, &files, &RewardPolicy::default()).unwrap();

        assert_eq!(record.schema, TRACE_SCHEMA_VERSION);
        assert_eq!(record.outcome.as_deref(), Some("completed"));
        assert_eq!(record.stage_trajectory, vec!["triage", "execute"]);
        assert_eq!(record.cost_usd, 0.25);
        assert_eq!(record.change_digest.as_deref().map(str::len), Some(16));
        assert!(record.redacted, "the ghp_ token must be recognized");
        assert!(
            !record.prompt.contains("ghp_"),
            "prompt is redacted: {}",
            record.prompt
        );

        assert_eq!(record.calls.len(), 1);
        let call = &record.calls[0];
        assert_eq!(call.role, "worker");
        assert_eq!(call.stage.as_deref(), Some("execute"));
        assert!(call.reconstruction_verified);
        assert_eq!(call.prompt_messages.len(), 2);
        let goal = serde_json::to_string(&call.prompt_messages[1]).unwrap();
        assert!(
            !goal.contains("ghp_") && goal.contains("[redacted]"),
            "message content is redacted: {goal}"
        );
        assert_eq!(call.cost_usd, Some(0.25));
        assert_eq!(call.input_tokens, Some(120));
        assert_eq!(call.tool_uses.len(), 1);
        assert_eq!(call.tool_uses[0].name, "read_file");
        assert_eq!(call.tool_uses[0].is_error, Some(false));
        assert_eq!(call.tool_uses[0].duration_ms, Some(42));

        // The JSONL line round-trips and lands owner-only.
        let root =
            std::env::temp_dir().join(format!("stella-trace-test-{id}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let path = append_trace(&root, &record).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw.lines().count(), 1);
        let back: TraceRecord = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
        assert_eq!(back.execution_id, record.execution_id);
        assert_eq!(back.calls.len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "traces are owner-only");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A journal that predates receipts (or a call whose reconstruction
    /// fails) still yields a trace — with the shortfall stated, never
    /// invented. Report regardless of state.
    #[test]
    fn missing_receipt_data_degrades_honestly() {
        let store = Store::in_memory().unwrap();
        let id = store.begin_execution("run", "p", "zai", "glm-5.2").unwrap();
        // A receipt whose blocks were never registered: reconstruction
        // resolves nothing.
        store
            .record_step_manifest(
                id,
                &StepManifestRow {
                    turn_instance: 0,
                    step: 0,
                    call_seq: 0,
                    provider: "zai".to_string(),
                    model: "glm-5.2".to_string(),
                    call_role: "worker".to_string(),
                    effective_budget_tokens: 1000,
                    calibration_factor: 1.0,
                    estimated_input_tokens: 10,
                    compiled_frame_id: None,
                    frame_hash: None,
                    blocks: vec![manifest_entry("blk_ghost", 0)],
                },
            )
            .unwrap();

        let record = assemble(&store, id, &[], &RewardPolicy::default()).unwrap();
        assert_eq!(record.calls.len(), 1);
        assert!(!record.calls[0].reconstruction_verified);
        assert!(record.calls[0].reconstruction_error.is_some());
        assert_eq!(record.change_digest, None);
        assert!(record.outcome.is_none(), "unsettled execution says so");
        // No verdict reached the journal, so the label says exactly that
        // rather than scoring the turn on its absence (#1043).
        assert_eq!(record.reward.reward, None);
        assert_eq!(
            record.reward.discard,
            Some(stella_pipeline::reward::DiscardReason::NoVerdict)
        );
    }

    /// One `JudgeVerdict` with a rung, and the trace carries the composite
    /// reward that rung earns — the #1043 acceptance shape at the trace seam.
    #[test]
    fn a_settled_verdict_becomes_a_reward_label() {
        let store = Store::in_memory().unwrap();
        let id = store
            .begin_execution("run", "fix the flake", "zai", "glm-5.2")
            .unwrap();
        let verdict = |passed: bool, rung: LadderRung| AgentEvent::JudgeVerdict {
            passed,
            evidence: JudgeEvidence {
                summary: "FAIL — the diff does not address the goal".to_string(),
                deterministic: false,
                evidence_refs: Vec::new(),
                ladder: Some(Box::new(LadderSnapshot {
                    rung: Some(rung),
                    ..bare_snapshot()
                })),
            },
        };
        // Two verdicts: one revision round, then the deterministic pass.
        for (seq, event) in [
            verdict(false, LadderRung::Revise),
            verdict(true, LadderRung::SubmitFast),
        ]
        .into_iter()
        .enumerate()
        {
            store.record_event(id, seq as u64, &event).unwrap();
        }
        store
            .finish_execution_accounted(id, "completed", 0.40, true)
            .unwrap();

        let record = assemble(&store, id, &[], &RewardPolicy::default()).unwrap();
        assert_eq!(record.reward.rung, Some(LadderRung::SubmitFast));
        assert_eq!(record.reward.outcome, Some(1.0));
        assert_eq!(
            record.reward.cost.revisions, 1,
            "two verdicts, one revision"
        );
        assert_eq!(record.reward.cost.steps, 0, "no receipts in this fixture");
        // 1.0 − 0.02·0 − 0.5·0.40 − 0.1·1 = 0.70
        let reward = record.reward.reward.expect("scored");
        assert!((reward - 0.70).abs() < 1e-9, "got {reward}");

        // The airlock at the trace seam: the judge's own sentence is in the
        // journal this was folded from, and nowhere in the label.
        let label_json = serde_json::to_string(&record.reward).unwrap();
        assert!(
            !label_json.contains("does not address"),
            "judge prose reached the label: {label_json}"
        );
    }

    /// An abstain rung is marked discard, and the trace survives — the record
    /// a failure-mode study needs is exactly the one with no scalar.
    #[test]
    fn an_abstaining_verdict_is_discarded_not_scored() {
        let store = Store::in_memory().unwrap();
        let id = store.begin_execution("run", "p", "zai", "glm-5.2").unwrap();
        store
            .record_event(
                id,
                0,
                &AgentEvent::JudgeVerdict {
                    passed: true,
                    evidence: JudgeEvidence {
                        summary: "UNVERIFIABLE — no channel could observe this turn".to_string(),
                        deterministic: false,
                        evidence_refs: Vec::new(),
                        ladder: Some(Box::new(LadderSnapshot {
                            rung: Some(LadderRung::Unverifiable),
                            ..bare_snapshot()
                        })),
                    },
                },
            )
            .unwrap();
        store
            .finish_execution_accounted(id, "completed", 0.10, true)
            .unwrap();

        let record = assemble(&store, id, &[], &RewardPolicy::default()).unwrap();
        assert_eq!(record.reward.rung, Some(LadderRung::Unverifiable));
        assert_eq!(record.reward.reward, None);
        assert_eq!(
            record.reward.discard,
            Some(stella_pipeline::reward::DiscardReason::Abstained)
        );
    }

    /// The all-dark snapshot every rung fixture above builds on.
    fn bare_snapshot() -> LadderSnapshot {
        LadderSnapshot {
            rung: None,
            tracked_command: None,
            oracle_trace: Vec::new(),
            flip_achieved: false,
            unstable_flip: false,
            flip_refused_different_failure: false,
            touched_tests_passed: None,
            test_infra: None,
            diff_lines: 0,
            diff_budget: 0,
            diff_available: false,
            file_change_events: 0,
            mutating_actions: 0,
            new_diag_errors: 0,
            new_diag_warnings: 0,
            witness_intact: None,
            witness_mutation: None,
            diff_coverage: None,
        }
    }
}
