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
//!
//! ## Plugins do not write traces (A9, docs/spec/pipeline-as-plugins.md §4)
//!
//! A plugin contributes a fact by emitting a `plugin.<id>.*` journal event
//! ([`stella_core::bus::names::plugin_event_name`]) — never by appending to
//! [`TRACES_FILE`] itself. [`fold_journal`]'s `AgentEvent::Unknown` arm folds
//! those events into [`PluginFact`] exactly like every other field on
//! [`TraceRecord`], which is what a plugin-contributed fact inherits by
//! going through the fold instead of around it: replayability (the fact is
//! reconstructible from the same journal every other field is), the
//! [`TRACE_SCHEMA_VERSION`] skip-on-unknown contract (a reader that predates
//! plugin facts sees an ordinary additive field, not a line it cannot
//! parse), redaction (every payload leaf passes through
//! [`stella_core::redact::redact_secrets`] before it reaches the file, same
//! as `prompt_messages`), and the guarantee that nothing here reaches
//! `store.db` (the fold reads the journal already written there; a plugin
//! writing `traces.jsonl` directly would be a second, ungoverned write path
//! with none of the above — not a shortcut, a regression on all four).

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use stella_core::bus::names as plugin_names;
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
///
/// `4` makes the label's step count nullable ([`TrajectoryCost::steps`]).
/// A v3 record whose execution recorded no model call wrote `steps: 0` and a
/// reward scalar shaped as though the turn had bought none — the step penalty
/// silently absent from every trace of a store predating the receipts plane
/// (#2123). v4 writes `steps: null` and discards the scalar instead, so a v3
/// line is skipped rather than pooled beside a v4 one that priced the same
/// trajectory differently.
pub const TRACE_SCHEMA_VERSION: u32 = 4;

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
    /// Facts plugins contributed to this execution, folded from
    /// `plugin.<id>.*` journal events — see the module doc's "Plugins do not
    /// write traces". Empty, never omitted, when no plugin ran: that is a
    /// fact about this execution a dataset reader should see directly rather
    /// than infer from a missing field.
    #[serde(default)]
    pub plugin_facts: Vec<PluginFact>,
    /// Every model call, in wire order.
    pub calls: Vec<TraceCall>,
}

/// One model call inside a trace: what it saw, what it did, what it cost.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceCall {
    pub turn_instance: u32,
    pub step: u64,
    pub call_seq: u64,
    /// `ModelCallRole`, as its wire token ("worker", "verifier", …).
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
    /// Which compaction-journaling era wrote this execution's journal —
    /// `compaction_journaled` or `compaction_unjournaled`, the same two words
    /// `stella inspect --format json` and `/api/execution-context` emit.
    /// `None` only when reconstruction failed outright and produced no
    /// [`stella_store::Reconstruction`] to read it from: unknown is recorded
    /// as unknown, never guessed (#2030).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_era: Option<String>,
    /// What this call's digest mismatches mean — `none`, `compaction`, or
    /// `integrity`. Present whenever a reconstruction was produced, including
    /// the verified path, so "did any call in this run raise a real integrity
    /// signal" is one grep over a field rather than a regex over
    /// [`Self::reconstruction_error`]'s prose (#2030, #1981). `None` carries
    /// the same meaning as on [`Self::journal_era`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest_mismatch_severity: Option<String>,
    /// Why reconstruction fell short, when it did. The human sentence; the two
    /// fields above are what a consumer filters on.
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

/// One fact a plugin contributed to this execution, folded from a
/// `plugin.<id>.*` journal event — see the module doc's "Plugins do not
/// write traces". Additive: a reader compiled before this field existed
/// simply never sees it, the same posture every other optional field on
/// [`TraceRecord`] already has, so no [`TRACE_SCHEMA_VERSION`] bump is
/// needed to add it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginFact {
    /// The plugin's own id, recovered from the event name by
    /// [`stella_core::bus::names::plugin_id_of`] — never trusted from the
    /// payload, only from the namespace the name itself proves ownership of.
    pub plugin_id: String,
    /// The full dotted event name (`plugin.<id>.<local>`).
    pub name: String,
    /// The call this fact was attributed to — the same `(turn_instance,
    /// step)` a [`TraceCall`] is keyed by — when one was open at the time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_instance: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<u64>,
    /// The event's payload, secret-redacted exactly like `prompt_messages`.
    pub payload: serde_json::Value,
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
    workspace_root: &Path,
    policy: &RewardPolicy,
) {
    if let Err(error) = capture(store, execution_id, workspace_root, policy) {
        eprintln!("  ⚠ trace capture failed: {error}");
    }
}

/// Assemble one execution's trace from the store, then append it to
/// `.stella/private/traces.jsonl`. Returns the file written.
pub fn capture(
    store: &Store,
    execution_id: i64,
    workspace_root: &Path,
    policy: &RewardPolicy,
) -> Result<PathBuf, String> {
    let record = assemble(store, execution_id, policy)?;
    append_trace(workspace_root, &record)
}

/// One call's reconstruction, already reduced to the shape [`TraceCall`]
/// stores. A struct rather than a tuple because the two arms below now settle
/// five values, and a five-tuple destructure names none of them.
struct CallReconstruction {
    messages: Vec<serde_json::Value>,
    verified: bool,
    journal_era: Option<String>,
    digest_mismatch_severity: Option<String>,
    error: Option<String>,
}

/// Build the [`TraceRecord`] for `execution_id`. Read after closeout so
/// `outcome`, `cost_usd`, and the full journal are settled.
pub fn assemble(
    store: &Store,
    execution_id: i64,
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
        let reconstructed = match store.reconstruct_call(
            execution_id,
            receipt.turn_instance,
            receipt.step,
            receipt.call_seq,
        ) {
            Ok(reconstruction) => {
                let verified = reconstruction.is_verified();
                let error = (!verified).then(|| {
                    format!(
                        "unresolved: [{}]; digest mismatches ({}): [{}]",
                        reconstruction.unresolved.join(", "),
                        crate::inspect::severity_tag(reconstruction.mismatch_severity()),
                        reconstruction.digest_mismatches.join(", ")
                    )
                });
                CallReconstruction {
                    messages: redact_messages(&reconstruction.messages, &mut redacted),
                    verified,
                    // The era and the severity ride along with the ids as
                    // fields, because a trace is read long after the run, by a
                    // machine, and by someone who cannot ask which build wrote
                    // it. `compaction` says the mismatch is the pre-#1667
                    // journal's routine one; `integrity` says nothing routine
                    // explains it. Stamped on the verified path too, where the
                    // answer is `none`: a reader should never have to infer a
                    // verdict from a field's absence. Same words `stella
                    // inspect` and the deck render — one source, four surfaces
                    // (#1981, #2030).
                    journal_era: Some(crate::inspect::era_tag(reconstruction.journal_era).into()),
                    digest_mismatch_severity: Some(
                        crate::inspect::severity_tag(reconstruction.mismatch_severity()).into(),
                    ),
                    error,
                }
            }
            // Nothing was read, so nothing is known: the era and severity stay
            // absent rather than defaulting to a word that would read as a
            // verdict.
            Err(e) => CallReconstruction {
                messages: Vec::new(),
                verified: false,
                journal_era: None,
                digest_mismatch_severity: None,
                error: Some(format!("reconstruction failed: {e}")),
            },
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
            prompt_messages: reconstructed.messages,
            reconstruction_verified: reconstructed.verified,
            journal_era: reconstructed.journal_era,
            digest_mismatch_severity: reconstructed.digest_mismatch_severity,
            reconstruction_error: reconstructed.error,
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
            steps: TrajectoryCost::recorded_steps(calls.len()),
            cost_usd: summary.cost_usd,
            revisions: fold.verdicts.saturating_sub(1),
        },
        policy,
    );

    // Redacted once here, the same discipline `prompt` and `prompt_messages`
    // already follow, rather than inline in the fold — a plugin's payload is
    // no more trusted than a model's.
    let mut plugin_facts = fold.plugin_facts;
    for fact in &mut plugin_facts {
        redact_value(&mut fact.payload, &mut redacted);
    }

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
        files_touched: Vec::new(),
        change_digest: None,
        redacted,
        reward,
        plugin_facts,
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
    /// The last `Verdict` the journal holds — the one that settled the
    /// turn. Earlier verdicts are revision rounds, counted below.
    settlement: Option<Settlement>,
    /// How many verdicts the journal holds. `saturating_sub(1)` is the
    /// revision count the reward's shaping prices; see
    /// [`TrajectoryCost::revisions`] for what that over-counts and why the
    /// direction is safe.
    verdicts: u32,
    /// Every `plugin.<id>.*` event seen, in journal order, attributed to
    /// whatever call was open when it arrived. Payloads are unredacted here
    /// — [`assemble`] redacts once, after the fold, the same way it redacts
    /// `prompt` and reconstructed messages.
    plugin_facts: Vec<PluginFact>,
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
            AgentEvent::Stage { name, .. } => {
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
            AgentEvent::Verdict { passed, evidence } => {
                // Overwritten on every verdict, so the last one wins: that is
                // the one that settled the turn, and the earlier ones are the
                // revision rounds this counts.
                fold.settlement = Some(Settlement::from_evidence(*passed, evidence));
                fold.verdicts = fold.verdicts.saturating_add(1);
            }
            // A `plugin.<id>.*` name decodes to `Unknown` because it is not
            // one of `KNOWN_TYPE_TAGS` — that variant is the vocabulary's
            // designed extension point (`AgentEvent::Unknown`'s doc comment),
            // so a plugin-contributed fact needs no new `AgentEvent` variant
            // and therefore no new row in the signal-consumer ledger
            // (`stella-protocol/src/event/consumers.rs`): nothing there
            // changed shape. `plugin_id_of` re-validates the namespace rather
            // than trusting `event_type` verbatim, so a malformed or
            // non-plugin unknown tag is silently not a fact, not a crash.
            AgentEvent::Unknown {
                event_type,
                payload,
            } => {
                if let Some(plugin_id) = plugin_names::plugin_id_of(event_type) {
                    fold.plugin_facts.push(PluginFact {
                        plugin_id: plugin_id.to_string(),
                        name: event_type.clone(),
                        turn_instance: current.map(|(turn, _, _)| turn),
                        step: current.map(|(_, step, _)| step),
                        payload: payload.clone(),
                    });
                }
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
        FlipOutcome, LadderRung, LadderSnapshot, ModelCallRole, StageKind, ToolCall, ToolOutput,
        VerdictEvidence,
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
                scope: stella_protocol::StageScope::Run,
            },
            AgentEvent::Stage {
                name: StageKind::Execute,
                scope: stella_protocol::StageScope::Run,
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
                upstream_provider: None,
                reasoning_tokens: None,
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
                    data: None,
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

        let record = assemble(&store, id, &RewardPolicy::default()).unwrap();

        assert_eq!(record.schema, TRACE_SCHEMA_VERSION);
        assert_eq!(record.outcome.as_deref(), Some("completed"));
        assert_eq!(record.stage_trajectory, vec!["triage", "execute"]);
        assert_eq!(record.cost_usd, 0.25);
        assert_eq!(record.change_digest, None);
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

    /// A9 witness (docs/spec/pipeline-as-plugins.md §4): a plugin never
    /// writes `traces.jsonl` — it emits a `plugin.<id>.*` journal event, and
    /// the fact reaches the trace only through [`fold_journal`]. This
    /// exercises both halves the task asks for: a well-formed
    /// plugin-namespaced event becomes a [`PluginFact`], redacted like
    /// everything else on the record, attributed to the call that was open
    /// when it arrived; and a malformed `plugin.`-prefixed tag — one no
    /// plugin id could ever own — is rejected by the fold rather than
    /// faked into a fact.
    #[test]
    fn a_plugin_namespaced_journal_event_becomes_a_redacted_trace_fact() {
        let store = Store::in_memory().unwrap();
        let id = store
            .begin_execution("run", "review the diff", "zai", "glm-5.2")
            .unwrap();

        let owned_name = plugin_names::plugin_event_name("demo-reviewer", "finding.raised")
            .expect("well-formed plugin id and local segment");

        let events = [
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
            // A well-formed plugin event: owned by "demo-reviewer", carrying
            // a secret the fold must redact exactly like `prompt_messages`.
            AgentEvent::Unknown {
                event_type: owned_name.clone(),
                payload: serde_json::json!({
                    "type": owned_name,
                    "message": "leaked token ghp_0123456789abcdef0123456789abcdef012345",
                }),
            },
            // A `plugin.`-prefixed tag with no id segment at all — outside
            // any plugin's own namespace, and must not be attributed to one.
            AgentEvent::Unknown {
                event_type: "plugin.".to_string(),
                payload: serde_json::json!({"type": "plugin."}),
            },
        ];
        for (seq, event) in events.iter().enumerate() {
            store.record_event(id, seq as u64, event).unwrap();
        }
        store
            .finish_execution_accounted(id, "completed", 0.0, true)
            .unwrap();

        let record = assemble(&store, id, &RewardPolicy::default()).unwrap();

        assert_eq!(
            record.plugin_facts.len(),
            1,
            "the malformed plugin.-prefixed tag must not become a fact: {:?}",
            record.plugin_facts
        );
        let fact = &record.plugin_facts[0];
        assert_eq!(fact.plugin_id, "demo-reviewer");
        assert_eq!(fact.name, "plugin.demo-reviewer.finding.raised");
        assert_eq!(fact.turn_instance, Some(0));
        assert_eq!(fact.step, Some(0));
        assert!(record.redacted, "the ghp_ token must be recognized");
        let payload_json = fact.payload.to_string();
        assert!(
            !payload_json.contains("ghp_") && payload_json.contains("[redacted]"),
            "plugin fact payload is redacted: {payload_json}"
        );

        // The JSONL line round-trips: a plugin fact is an ordinary field on
        // the serialized record, not a second write path.
        let root = std::env::temp_dir().join(format!(
            "stella-trace-plugin-test-{id}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let path = append_trace(&root, &record).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let back: TraceRecord = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
        assert_eq!(back.plugin_facts.len(), 1);
        assert_eq!(back.plugin_facts[0].plugin_id, "demo-reviewer");
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

        let record = assemble(&store, id, &RewardPolicy::default()).unwrap();
        assert_eq!(record.calls.len(), 1);
        assert!(!record.calls[0].reconstruction_verified);
        assert!(record.calls[0].reconstruction_error.is_some());
        // Unresolved is not a digest mismatch: the severity says `none` while
        // `reconstruction_verified` says false. That distinction is exactly
        // what the prose sentence cannot express (#2030).
        let call = &serde_json::to_value(&record).unwrap()["calls"][0];
        assert_eq!(call["digest_mismatch_severity"], "none");
        assert_eq!(call["journal_era"], "compaction_journaled");
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

    /// #2030: a digest mismatch on a current-era journal is a real integrity
    /// signal, and the trace record says so in a field a consumer can filter
    /// on — not only inside `reconstruction_error`'s sentence. Asserted
    /// against the serialized line rather than the struct, because the JSONL
    /// artifact is what a dataset reader actually greps.
    #[test]
    fn a_digest_mismatch_is_a_field_not_a_substring() {
        let store = Store::in_memory().unwrap();
        // `begin_execution` stamps the current era, which is what makes an
        // unexplained mismatch `integrity` rather than routine `compaction`.
        let id = store.begin_execution("run", "p", "zai", "glm-5.2").unwrap();
        store
            .record_event(
                id,
                0,
                &AgentEvent::ToolResult {
                    call_id: "call_1".to_string(),
                    output: ToolOutput::Ok {
                        content: "fn login() {}".to_string(),
                        data: None,
                    },
                    duration_ms: 5,
                    speculated: false,
                },
            )
            .unwrap();
        // The block resolves through the journal by `call_id`, but its
        // recorded digest is not the digest of those bytes — a mismatch by
        // construction. `gap_block` cannot express this: it stores `content`
        // locally, which short-circuits the digest check to true.
        store
            .record_context_block(
                id,
                &ContextBlockRow {
                    block_id: "blk_tool".to_string(),
                    kind: "tool_result".to_string(),
                    origin_turn: 0,
                    origin_step: 0,
                    call_id: Some("call_1".to_string()),
                    memory_id: None,
                    token_cost: Some(1),
                    content_digest: format!("sha256:{}", "0".repeat(64)),
                    citation_label: None,
                    content: None,
                },
            )
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
                    blocks: vec![manifest_entry("blk_tool", 0)],
                },
            )
            .unwrap();

        let record = assemble(&store, id, &RewardPolicy::default()).unwrap();
        assert_eq!(record.calls.len(), 1);
        assert!(!record.calls[0].reconstruction_verified);

        let call = &serde_json::to_value(&record).unwrap()["calls"][0];
        assert_eq!(
            call["digest_mismatch_severity"], "integrity",
            "the verdict is a key, not a substring: {call}"
        );
        assert_eq!(call["journal_era"], "compaction_journaled");
        // The human sentence stays: the fields are additive to it, not a
        // replacement for it.
        assert!(
            call["reconstruction_error"]
                .as_str()
                .is_some_and(|e| e.contains("blk_tool")),
            "the prose still names the block: {call}"
        );
    }

    /// One `Verdict` with a rung, and the trace carries the composite
    /// reward that rung earns — the #1043 acceptance shape at the trace seam.
    #[test]
    fn a_settled_verdict_becomes_a_reward_label() {
        let store = Store::in_memory().unwrap();
        let id = store
            .begin_execution("run", "fix the flake", "zai", "glm-5.2")
            .unwrap();
        let verdict = |passed: bool, rung: LadderRung| AgentEvent::Verdict {
            passed,
            evidence: VerdictEvidence {
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

        let record = assemble(&store, id, &RewardPolicy::default()).unwrap();
        assert_eq!(record.reward.rung, Some(LadderRung::SubmitFast));
        assert_eq!(record.reward.outcome, Some(1.0));
        assert_eq!(
            record.reward.cost.revisions, 1,
            "two verdicts, one revision"
        );
        // No receipts in this fixture, so how many calls the turn bought was
        // never recorded. Shaped against a zero it would have scored
        // 1.0 − 0.02·0 − 0.5·0.40 − 0.1·1 = 0.70, a step penalty lighter than
        // any counted trajectory pays; the label withholds the scalar instead
        // and keeps the rung and the outcome term, which are still true.
        assert_eq!(
            record.reward.cost.steps, None,
            "no receipts in this fixture"
        );
        assert_eq!(record.reward.reward, None);
        assert_eq!(
            record.reward.discard,
            Some(stella_pipeline::reward::DiscardReason::StepsUnknown)
        );

        // The airlock at the trace seam: the verifier's own sentence is in the
        // journal this was folded from, and nowhere in the label.
        let label_json = serde_json::to_string(&record.reward).unwrap();
        assert!(
            !label_json.contains("does not address"),
            "verifier prose reached the label: {label_json}"
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
                &AgentEvent::Verdict {
                    passed: true,
                    evidence: VerdictEvidence {
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

        let record = assemble(&store, id, &RewardPolicy::default()).unwrap();
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
            flip: FlipOutcome::NotAchieved,
            unstable_flip: false,
            flip_refused_different_failure: false,
            touched_tests_passed: None,
            test_infra: None,
            diff_lines: 0,
            diff_budget: 0,
            diff_available: false,
            mutating_actions: 0,
            new_diag_errors: 0,
            new_diag_warnings: 0,
            witness_intact: None,
            witness_mutation: None,
            diff_coverage: None,
            verify_done_flip: false,
            no_test_surface: false,
            errored_commands: 0,
            verifier_independent: None,
        }
    }
}
