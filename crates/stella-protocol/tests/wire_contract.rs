//! The committed wire contract, checked against the types it describes.
//!
//! `docs/wire/agentevent.schema.json` is generated from [`AgentEvent`] and
//! committed so that a change to the wire format lands as a reviewable diff
//! (issue #971). `scripts/check-wire-schema.sh` proves the committed file
//! still matches the types. This file proves the other direction, which is the
//! one a consumer actually depends on: **every event this build can emit
//! validates against the file consumers were handed.**
//!
//! Those are different claims. A schema can match its types perfectly and
//! still be useless — if the derive mis-describes an `Option`, or a
//! `serde(default)` field is written as required, both halves stay
//! self-consistent while every real stream fails the published contract.
//!
//! # Coverage, stated honestly
//!
//! 155 sample events, covering:
//!
//! - All 36 `AgentEvent` variants: exhaustive, and *proved* exhaustive — the
//!   sample table is checked against [`KNOWN_TYPE_TAGS`], which the
//!   `agent_event_tags!` macro generates from a match the compiler requires to
//!   be total. A new variant that reaches the wire without a sample here fails
//!   [`every_known_tag_has_a_sample`].
//! - Every arm of all nineteen enums in the payload graph — `ProofStep`,
//!   `SubAgentPhase`, `MediaJobState`, `ToolOutput`, and fifteen unit-only
//!   vocabularies. Two independent checks, because they catch different
//!   mistakes: [`every_nested_vocabulary_is_fully_sampled`] compares each
//!   sample list's length against the arm count in the schema's own `$defs`
//!   (so growing a vocabulary without extending the samples fails), and
//!   [`every_enum_arm_in_the_payload_graph_actually_reaches_the_wire`] proves
//!   each arm was actually *serialized* into a validated sample. Listing an
//!   arm and wiring it into an event are not the same thing — the second check
//!   found `ProofTree::Baseline` sampled in a list but never emitted.
//! - Both the "all optional fields present" and "all optional fields absent"
//!   shapes of the events that carry them.
//!
//! What is **not** covered: [`AgentEvent::Unknown`], deliberately and
//! necessarily. It carries no wire tag of its own, so it is absent from the
//! schema, and it re-serializes as the foreign object it wrapped — whose
//! `"type"` is by construction one this build does not know.
//! [`an_event_from_the_future_is_outside_the_schema_by_construction`] pins
//! that as the expected result rather than leaving it as a silent gap.
//!
//! The validator below implements the subset of JSON Schema 2020-12 that
//! `schemars` emits for these types. It is deliberately strict: an unmodelled
//! keyword panics rather than being skipped, so the proof can never quietly
//! become weaker than it looks.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde_json::{Value, json};
use stella_protocol::completion::FinishReason;
use stella_protocol::event::{
    BudgetMode, BudgetScope, CiStatus, FileChangeKind, MediaJobState, MediaKind, ModelCallRole,
    PolicyKind, PrStatus, ProofStep, ProofTree, ScopeProposal, StageKind, TaskItem, TaskStatus,
    UsageIncompleteReason,
};
use stella_protocol::ladder::{LadderRung, LadderSnapshot, OracleObservation};
use stella_protocol::receipt::{
    BlockKind, BlockOrigin, CacheZone, ContextFrameRef, ContextProviderUsage, ContextUsage,
    ManifestEntry, ProviderShare,
};
use stella_protocol::{
    AgentEvent, CompiledContextFrameBuilt, KNOWN_TYPE_TAGS, MediaArtifactRef, SubAgentPhase,
    SubAgentStatus, ToolCall, ToolOutput, VerdictEvidence,
};

// ---------------------------------------------------------------------------
// The committed artifact
// ---------------------------------------------------------------------------

/// The schema as *committed*, not as regenerated. Reading the file is the
/// point: this test speaks for the artifact consumers were given.
fn committed_schema() -> Value {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "..",
        "docs",
        "wire",
        "agentevent.schema.json",
    ]
    .iter()
    .collect();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "reading the committed wire schema {path:?}: {e}\n\
             Generate it with: bash scripts/export-agentevent-schema.sh"
        )
    });
    serde_json::from_str(&text).expect("the committed wire schema is valid JSON")
}

// ---------------------------------------------------------------------------
// A minimal JSON Schema 2020-12 validator
// ---------------------------------------------------------------------------

/// Keywords that carry documentation or numeric annotation only. Ignored on
/// purpose; every *other* unrecognized keyword panics, so the validator cannot
/// silently stop checking something.
const IGNORED: &[&str] = &[
    "$schema",
    "$defs",
    "$comment",
    "title",
    "description",
    "default",
    "examples",
    "deprecated",
    "format",
    "minimum",
    "maximum",
];

/// Validate `instance` against `schema`, resolving `$ref`s against `root`.
/// Returns one message per failure; empty means valid.
fn errors(root: &Value, schema: &Value, instance: &Value, path: &str) -> Vec<String> {
    match schema {
        Value::Bool(true) => return Vec::new(),
        Value::Bool(false) => return vec![format!("{path}: schema is `false`, nothing validates")],
        Value::Object(_) => {}
        other => panic!("{path}: subschema is neither object nor boolean: {other}"),
    }
    let map = schema.as_object().expect("checked above");
    let mut out = Vec::new();

    for key in map.keys() {
        assert!(
            IGNORED.contains(&key.as_str())
                || matches!(
                    key.as_str(),
                    "$ref"
                        | "oneOf"
                        | "anyOf"
                        | "allOf"
                        | "const"
                        | "enum"
                        | "type"
                        | "properties"
                        | "required"
                        | "additionalProperties"
                        | "items"
                ),
            "{path}: unmodelled JSON Schema keyword `{key}` — extend this validator \
             rather than letting the proof silently weaken"
        );
    }

    if let Some(Value::String(reference)) = map.get("$ref") {
        let name = reference
            .strip_prefix("#/$defs/")
            .unwrap_or_else(|| panic!("{path}: only local `#/$defs/…` refs are modelled"));
        let target = root
            .pointer(&format!("/$defs/{name}"))
            .unwrap_or_else(|| panic!("{path}: dangling $ref {reference}"));
        out.extend(errors(root, target, instance, path));
    }

    if let Some(Value::Array(branches)) = map.get("oneOf") {
        let matched = branches
            .iter()
            .filter(|b| errors(root, b, instance, path).is_empty())
            .count();
        if matched != 1 {
            out.push(format!(
                "{path}: matched {matched} of {} oneOf branches (want exactly 1)",
                branches.len()
            ));
        }
    }
    if let Some(Value::Array(branches)) = map.get("anyOf") {
        let matched = branches
            .iter()
            .filter(|b| errors(root, b, instance, path).is_empty())
            .count();
        if matched == 0 {
            out.push(format!(
                "{path}: matched none of {} anyOf branches",
                branches.len()
            ));
        }
    }
    if let Some(Value::Array(branches)) = map.get("allOf") {
        for (i, branch) in branches.iter().enumerate() {
            out.extend(errors(root, branch, instance, &format!("{path}/allOf/{i}")));
        }
    }

    if let Some(constant) = map.get("const")
        && instance != constant
    {
        out.push(format!("{path}: expected const {constant}, got {instance}"));
    }
    if let Some(Value::Array(values)) = map.get("enum")
        && !values.contains(instance)
    {
        out.push(format!("{path}: {instance} is not one of {values:?}"));
    }

    if let Some(ty) = map.get("type") {
        let names: Vec<&str> = match ty {
            Value::String(one) => vec![one.as_str()],
            Value::Array(many) => many.iter().filter_map(Value::as_str).collect(),
            other => panic!("{path}: `type` is neither string nor array: {other}"),
        };
        if !names.iter().any(|n| json_is(n, instance)) {
            out.push(format!("{path}: {instance} is not of type {names:?}"));
        }
    }

    if let Some(Value::Array(required)) = map.get("required")
        && let Some(object) = instance.as_object()
    {
        for name in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(name) {
                out.push(format!("{path}: missing required property `{name}`"));
            }
        }
    }

    if let Some(object) = instance.as_object() {
        let properties = map.get("properties").and_then(Value::as_object);
        if let Some(properties) = properties {
            for (name, value) in object {
                if let Some(subschema) = properties.get(name) {
                    out.extend(errors(root, subschema, value, &format!("{path}/{name}")));
                }
            }
        }
        if map.get("additionalProperties") == Some(&Value::Bool(false)) {
            let known: BTreeSet<&str> = properties
                .map(|p| p.keys().map(String::as_str).collect())
                .unwrap_or_default();
            for name in object.keys() {
                if !known.contains(name.as_str()) {
                    out.push(format!("{path}: unexpected property `{name}`"));
                }
            }
        }
    }

    if let Some(items) = map.get("items")
        && let Some(array) = instance.as_array()
    {
        for (i, element) in array.iter().enumerate() {
            out.extend(errors(root, items, element, &format!("{path}/{i}")));
        }
    }

    out
}

fn json_is(name: &str, value: &Value) -> bool {
    match name {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        other => panic!("unmodelled JSON type `{other}`"),
    }
}

/// Assert one event validates, with a message that names the offending field.
fn assert_validates(root: &Value, event: &AgentEvent) {
    let instance = serde_json::to_value(event).expect("AgentEvent always serializes");
    let failures = errors(root, root, &instance, event.type_tag());
    assert!(
        failures.is_empty(),
        "`{}` does not validate against the committed schema:\n  {}\n  instance: {instance}",
        event.type_tag(),
        failures.join("\n  ")
    );
}

// ---------------------------------------------------------------------------
// Sample values — one per arm of every enum in the payload graph
// ---------------------------------------------------------------------------

fn all_stage_kinds() -> Vec<StageKind> {
    use StageKind::*;
    vec![
        Triage,
        ContextRecall,
        Plan,
        ScopeReview,
        Witness,
        Execute,
        Verify,
        Verdict,
        Reflect,
        ContextWrite,
        Complete,
    ]
}

fn all_budget_modes() -> Vec<BudgetMode> {
    vec![BudgetMode::Off, BudgetMode::Observed, BudgetMode::Enforced]
}

fn all_budget_scopes() -> Vec<BudgetScope> {
    vec![BudgetScope::Turn, BudgetScope::Session]
}

fn all_policy_kinds() -> Vec<PolicyKind> {
    use PolicyKind::*;
    vec![Evaluated, Blocked, ApprovalRequested, SecretDetected]
}

fn all_model_call_roles() -> Vec<ModelCallRole> {
    use ModelCallRole::*;
    vec![
        Unknown,
        Triage,
        Plan,
        PlanRepair,
        WitnessAuthor,
        WitnessRepair,
        Worker,
        DistressGuidance,
        Verdict,
        AgentAuthor,
        SkillAuthor,
        DomainInference,
        Reflection,
        Summarization,
    ]
}

fn all_usage_incomplete_reasons() -> Vec<UsageIncompleteReason> {
    use UsageIncompleteReason::*;
    vec![ProviderError, Timeout, Cancelled]
}

fn all_finish_reasons() -> Vec<FinishReason> {
    use FinishReason::*;
    vec![Stop, Length, ToolCalls, ContentFilter]
}

fn all_file_change_kinds() -> Vec<FileChangeKind> {
    use FileChangeKind::*;
    vec![Read, Created, Modified, Deleted]
}

fn all_proof_trees() -> Vec<ProofTree> {
    vec![ProofTree::Baseline, ProofTree::Candidate]
}

fn all_ladder_rungs() -> Vec<LadderRung> {
    use LadderRung::*;
    vec![
        SubmitFast,
        Revise,
        NothingAttempted,
        Unverifiable,
        ModelVerdict,
        HeuristicFallback,
        Waived,
    ]
}

fn all_media_kinds() -> Vec<MediaKind> {
    vec![MediaKind::Image, MediaKind::Svg, MediaKind::Video]
}

fn all_pr_statuses() -> Vec<PrStatus> {
    use PrStatus::*;
    vec![Draft, Open, Merged, Closed]
}

fn all_ci_statuses() -> Vec<CiStatus> {
    use CiStatus::*;
    vec![Pending, Running, Passing, Failing]
}

fn all_task_statuses() -> Vec<TaskStatus> {
    use TaskStatus::*;
    vec![Pending, InProgress, Completed, Cancelled]
}

fn all_block_kinds() -> Vec<BlockKind> {
    use BlockKind::*;
    vec![
        SystemPrefix,
        UserGoal,
        RecalledFrame,
        AssistantText,
        ToolCall,
        ToolResult,
        Steered,
        Summary,
        Attachment,
        Other,
    ]
}

fn all_cache_zones() -> Vec<CacheZone> {
    use CacheZone::*;
    vec![StablePrefix, Cacheable, Volatile, Other]
}

fn all_subagent_statuses() -> Vec<SubAgentStatus> {
    use SubAgentStatus::*;
    vec![Completed, Incomplete, Refused]
}

fn all_proof_steps() -> Vec<ProofStep> {
    vec![
        ProofStep::Assurance {
            witness: true,
            verifier: false,
        },
        ProofStep::Warrant {
            required: false,
            reason: Some("docs only".into()),
            diff_lines: 4,
        },
        ProofStep::WitnessAuthored {
            path: "tests/witness.rs".into(),
            command: "cargo test -p x witness".into(),
            fingerprint: "sha256:aa".into(),
        },
        ProofStep::WitnessUnavailable {
            reason: "no independent author".into(),
        },
        ProofStep::VerificationUnavailable {
            reason: "every channel was blind".into(),
        },
        ProofStep::Oracle {
            command: "cargo test".into(),
            passed: true,
            tree: ProofTree::Candidate,
            run: Some(2),
            runs_required: Some(3),
            seed: Some(7741),
        },
    ]
}

fn all_media_job_states() -> Vec<MediaJobState> {
    vec![
        MediaJobState::Queued,
        MediaJobState::Running,
        MediaJobState::Succeeded,
        MediaJobState::Failed {
            reason: "provider rejected the prompt".into(),
        },
    ]
}

fn all_tool_outputs() -> Vec<ToolOutput> {
    vec![
        ToolOutput::Ok {
            content: "hello".into(),
        },
        ToolOutput::Error {
            message: "boom".into(),
        },
    ]
}

fn all_subagent_phases() -> Vec<SubAgentPhase> {
    vec![
        SubAgentPhase::Started {
            agent_id: "search-1".into(),
            instruction_preview: "find the retry policy".into(),
            budget_usd: Some(0.25),
            write_access: false,
            depth: 1,
        },
        SubAgentPhase::Finished {
            agent_id: "search-1".into(),
            status: SubAgentStatus::Completed,
            summary: "retry policy lives in retry.rs".into(),
            truncated: false,
            cost_usd: 0.004,
            steps: 3,
            absorbed_messages: 9,
            reason: None,
        },
    ]
}

fn tool_call() -> ToolCall {
    ToolCall {
        call_id: "call_1".into(),
        name: "read_file".into(),
        input: json!({ "path": "src/main.rs" }),
    }
}

/// One sample per `KNOWN_TYPE_TAGS` entry, plus extra samples that exercise
/// every arm of the nested vocabularies and both optional-field shapes.
fn sample_events() -> Vec<AgentEvent> {
    let mut events = vec![
        AgentEvent::Text {
            delta: "the answer".into(),
        },
        AgentEvent::TextDelta {
            text: "the ".into(),
        },
        AgentEvent::Reasoning {
            delta: "considering".into(),
        },
        AgentEvent::ToolStart { call: tool_call() },
        AgentEvent::SpeculationDiscarded {
            call_id: "call_2".into(),
            name: "read_file".into(),
            reason: "harvest_mismatch".into(),
        },
        AgentEvent::Retry {
            attempt: 1,
            reason: "429 from the provider".into(),
        },
        AgentEvent::Steered {
            text: "actually, use the other file".into(),
        },
        AgentEvent::LoopDetected {
            turn_instance: 3,
            kind: "short_cycle".into(),
            pattern: vec!["read_file".into(), "write_file".into()],
            repeats: 4,
            evidence: "same two calls, four times".into(),
            aborted: true,
        },
        AgentEvent::RetriesExhausted {
            turn_instance: 3,
            attempts: 2,
            reasons: vec!["timeout".into(), "timeout".into()],
            retryable: true,
        },
        AgentEvent::Compaction {
            before_tokens: 10_000,
            after_tokens: 4_000,
            evicted: 2,
            deduped: 1,
            superseded: 1,
            aged: 1,
            summarized: 3,
            evicted_blocks: vec!["blk_a".into()],
            deduped_blocks: vec!["blk_b".into()],
            superseded_blocks: vec!["blk_c".into()],
            aged_blocks: vec!["blk_d".into()],
            summarized_blocks: vec!["blk_e".into()],
            effective_budget_tokens: 136_363,
            calibration_factor: 1.1,
        },
        // The same event with every `skip_serializing_if` field absent — the
        // shape a real stream is far more likely to carry.
        AgentEvent::Compaction {
            before_tokens: 10_000,
            after_tokens: 4_000,
            evicted: 0,
            deduped: 0,
            superseded: 0,
            aged: 0,
            summarized: 0,
            evicted_blocks: vec![],
            deduped_blocks: vec![],
            superseded_blocks: vec![],
            aged_blocks: vec![],
            summarized_blocks: vec![],
            effective_budget_tokens: 0,
            calibration_factor: 0.0,
        },
        AgentEvent::GoalVerdict {
            round: 1,
            met: true,
            reasoning: "the tests pass".into(),
            cost_usd: 0.02,
        },
        AgentEvent::ProviderFallback {
            from: "anthropic".into(),
            to: "openrouter".into(),
            reason: "circuit breaker open".into(),
        },
        AgentEvent::ContextRecall {
            frames: vec![
                ContextFrameRef {
                    id: Some("frm_1".into()),
                    citation_label: "engine step-driver (driver.rs)".into(),
                    provider: "workspace-memory".into(),
                    source: "stella-context".into(),
                    kind: "symbol".into(),
                    uri: Some("file:///src/driver.rs".into()),
                    method: Some("lexical".into()),
                    token_cost: 120,
                    block_id: Some("blk_f".into()),
                    content_digest: Some("sha256:bb".into()),
                },
                // Every optional field absent.
                ContextFrameRef {
                    id: None,
                    citation_label: "a bare frame".into(),
                    provider: String::new(),
                    source: "stella-context".into(),
                    kind: String::new(),
                    uri: None,
                    method: None,
                    token_cost: 0,
                    block_id: None,
                    content_digest: None,
                },
            ],
            provider_mix: vec![ProviderShare {
                provider: "workspace-memory".into(),
                frames: 2,
            }],
            tokens: 120,
            usage: Some(ContextUsage {
                budget_requested: 2_000,
                budget_consumed: 120,
                as_of: "2026-07-30T00:00:00Z".into(),
                providers: vec![ContextProviderUsage {
                    provider_id: "workspace-memory".into(),
                    frames_served: 2,
                    frames_rejected: 0,
                    token_cost: 120,
                }],
            }),
            latency_ms: 42,
            used_ann_index: Some(true),
        },
        AgentEvent::ContextRecall {
            frames: vec![],
            provider_mix: vec![],
            tokens: 0,
            usage: None,
            latency_ms: 0,
            used_ann_index: None,
        },
        AgentEvent::ContextWrite {
            provider: "workspace-memory".into(),
            upserts: 2,
            superseded: 1,
        },
        AgentEvent::StepManifest {
            turn_instance: 1,
            step: 0,
            call_seq: 0,
            role: ModelCallRole::Worker,
            provider: "anthropic".into(),
            model: "opus".into(),
            blocks: vec![ManifestEntry {
                block_id: "blk_g".into(),
                cache_zone: CacheZone::StablePrefix,
                token_cost: 900,
                resident_since_step: 0,
                message_index: 0,
                call_id: Some("call_1".into()),
            }],
            effective_budget_tokens: 120_000,
            calibration_factor: 1.0,
            estimated_input_tokens: 900,
            compiled_frame: Some(CompiledContextFrameBuilt {
                compiled_frame_id: "cfr_1".into(),
                frame_hash: "sha256:cc".into(),
            }),
        },
        AgentEvent::StepManifest {
            turn_instance: 1,
            step: 1,
            call_seq: 1,
            role: ModelCallRole::Summarization,
            provider: "anthropic".into(),
            model: "opus".into(),
            blocks: vec![],
            effective_budget_tokens: 0,
            calibration_factor: 0.0,
            estimated_input_tokens: 0,
            compiled_frame: None,
        },
        AgentEvent::Verdict {
            passed: true,
            evidence: VerdictEvidence {
                summary: "the tracked command flipped".into(),
                deterministic: true,
                evidence_refs: vec!["trace:t1#verify".into()],
                ladder: None,
            },
        },
        AgentEvent::Verdict {
            passed: false,
            evidence: VerdictEvidence {
                summary: "no evidence".into(),
                deterministic: false,
                evidence_refs: vec![],
                ladder: None,
            },
        },
        AgentEvent::ScopeReview {
            proposal: ScopeProposal {
                summary: "rewrite the router".into(),
                steps: vec!["read".into(), "edit".into()],
                estimated_files: 12,
                estimated_cost_usd: Some(1.5),
                repo: Some("macanderson/stella".into()),
                branch: Some("feat/router".into()),
                write_globs: vec!["src/router/**".into()],
                read_globs: vec!["src/**".into()],
                shell_policy: Some("allowlisted".into()),
            },
        },
        AgentEvent::ScopeReview {
            proposal: ScopeProposal {
                summary: "a small change".into(),
                steps: vec![],
                estimated_files: 1,
                estimated_cost_usd: None,
                ..Default::default()
            },
        },
        AgentEvent::HunkReview {
            proposal: stella_protocol::HunkProposal {
                id: "hunk-review-1".into(),
                tool: "apply_edits".into(),
                hunks: vec![
                    stella_protocol::ProposedHunk {
                        path: "src/router.rs".into(),
                        diff: "@@ -1,2 +1,2 @@\n-old\n+new\n".into(),
                        lines_added: 1,
                        lines_removed: 1,
                    },
                    stella_protocol::ProposedHunk {
                        path: "src/lib.rs".into(),
                        diff: "@@ -9,0 +10,1 @@\n+added\n".into(),
                        lines_added: 1,
                        lines_removed: 0,
                    },
                ],
            },
        },
        // A review with no hunks is unreachable in practice (the gate skips an
        // unchanged call) but must still round-trip: an empty vector is where a
        // hand-rolled `skip_serializing_if` would silently drop the field.
        AgentEvent::HunkReview {
            proposal: stella_protocol::HunkProposal {
                id: "hunk-review-2".into(),
                tool: "write_file".into(),
                hunks: vec![],
            },
        },
        AgentEvent::AskUser {
            id: "call_3".into(),
            question: "which branch?".into(),
            options: vec!["main".into(), "develop".into()],
        },
        AgentEvent::MediaComplete {
            artifact: MediaArtifactRef {
                id: "art_1".into(),
                kind: MediaKind::Image,
                path: ".stella/artifacts/art_1.png".into(),
                label: "the diagram".into(),
            },
        },
        AgentEvent::Commit {
            sha: "d7c98624".into(),
            message: "feat: the task tool".into(),
        },
        AgentEvent::Pr {
            url: "https://example.invalid/pull/1".into(),
            status: PrStatus::Open,
            number: Some(1),
            ci: Some(CiStatus::Passing),
        },
        AgentEvent::Pr {
            url: "https://example.invalid/pull/2".into(),
            status: PrStatus::Draft,
            number: None,
            ci: None,
        },
        AgentEvent::TaskUpdate {
            tasks: vec![
                TaskItem {
                    id: "1".into(),
                    subject: "fix the redirect loop".into(),
                    description: Some("it loops on 302".into()),
                    status: TaskStatus::InProgress,
                    owner: Some("lead".into()),
                },
                TaskItem {
                    id: "2".into(),
                    subject: "a bare task".into(),
                    description: None,
                    status: TaskStatus::Pending,
                    owner: None,
                },
            ],
        },
        AgentEvent::Error {
            message: "the provider refused".into(),
            retryable: false,
        },
        AgentEvent::Complete {
            model: "opus".into(),
            cost_usd: 0.42,
        },
    ];

    // One event per arm of every nested vocabulary.
    events.extend(
        all_stage_kinds()
            .into_iter()
            .map(|name| AgentEvent::Stage { name }),
    );
    events.extend(
        all_tool_outputs()
            .into_iter()
            .map(|output| AgentEvent::ToolResult {
                call_id: "call_1".into(),
                output,
                duration_ms: 12,
                speculated: true,
            }),
    );
    events.extend(
        all_budget_scopes()
            .into_iter()
            .zip(all_budget_modes())
            .map(|(scope, mode)| AgentEvent::BudgetDenied {
                scope,
                spent_usd: 1.0,
                limit_usd: 0.5,
                mode,
            }),
    );
    events.extend(all_budget_modes().into_iter().flat_map(|mode| {
        [
            AgentEvent::BudgetTick {
                spent_usd: 0.42,
                limit_usd: Some(2.5),
                mode,
                session_spent_usd: Some(1.75),
                session_limit_usd: Some(10.0),
            },
            AgentEvent::BudgetTick {
                spent_usd: 0.42,
                limit_usd: None,
                mode,
                session_spent_usd: None,
                session_limit_usd: None,
            },
        ]
    }));
    events.extend(
        all_policy_kinds()
            .into_iter()
            .map(|kind| AgentEvent::PolicyDecision {
                kind,
                subject: "run_command".into(),
                outcome: "deny".into(),
            }),
    );
    events.extend(all_model_call_roles().into_iter().flat_map(|role| {
        [
            AgentEvent::StepUsage {
                step: 0,
                role,
                provider: "anthropic".into(),
                output_text: Some("a management call's output".into()),
                model: "opus".into(),
                input_tokens: 900,
                output_tokens: 40,
                cached_input_tokens: 800,
                cache_write_tokens: 100,
                reasoning_tokens: None,
                estimated_input_tokens: 880,
                cost_usd: 0.01,
                duration_ms: 1_200,
                retries: 0,
                tool_calls: 1,
                complete: true,
                // The "all optional fields present" shape must actually carry
                // the optional field, or the sample proves nothing about it.
                finish_reason: Some(FinishReason::Stop),
            },
            AgentEvent::StepUsage {
                step: 1,
                role,
                provider: String::new(),
                output_text: None,
                model: "opus".into(),
                input_tokens: 0,
                output_tokens: 0,
                cached_input_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: None,
                estimated_input_tokens: 0,
                cost_usd: 0.0,
                duration_ms: 0,
                retries: 0,
                tool_calls: 0,
                complete: false,
                // ...and the "all absent" shape must omit it entirely, which
                // is what `skip_serializing_if` promises consumers.
                finish_reason: None,
            },
        ]
    }));
    // Every stop reason on the wire, `Length` above all: it is the only
    // truthful signal that a step was cut off at the output ceiling, and a
    // consumer that cannot parse it is back to inferring truncation from step
    // shape — the reading that produced an unexplained `cap_hits: 106`.
    events.extend(
        all_finish_reasons()
            .into_iter()
            .map(|finish_reason| AgentEvent::StepUsage {
                step: 2,
                role: ModelCallRole::Worker,
                provider: "zai".into(),
                output_text: None,
                model: "glm-5.2".into(),
                input_tokens: 1_000,
                output_tokens: 64_000,
                cached_input_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: None,
                estimated_input_tokens: 980,
                cost_usd: 0.04,
                duration_ms: 90_000,
                retries: 0,
                tool_calls: 0,
                complete: true,
                finish_reason: Some(finish_reason),
            }),
    );
    events.extend(
        all_usage_incomplete_reasons()
            .into_iter()
            .flat_map(|reason| {
                [
                    // The recovering shape: a stream that died after the
                    // provider had already reported what the prompt cost.
                    AgentEvent::UsageIncomplete {
                        role: ModelCallRole::Worker,
                        provider: "anthropic".into(),
                        model: "opus".into(),
                        reason,
                        duration_ms: 30_000,
                        retries: Some(2),
                        partial: Some(stella_protocol::PartialUsage {
                            usage: stella_protocol::CompletionUsage {
                                input_tokens: 14_000,
                                cached_input_tokens: 12_000,
                                cache_write_tokens: 400,
                                reasoning_tokens: None,
                                output_tokens: 130,
                                reported: false,
                            },
                            cost_usd: 0.0213,
                            input_reported: true,
                        }),
                    },
                    // The shape that genuinely learned nothing — a failure
                    // before any usage frame. `partial` must stay absent from
                    // the wire rather than serializing as a zeroed envelope,
                    // which would read as "this attempt was free".
                    AgentEvent::UsageIncomplete {
                        role: ModelCallRole::Worker,
                        provider: "anthropic".into(),
                        model: "opus".into(),
                        reason,
                        duration_ms: 30_000,
                        retries: None,
                        partial: None,
                    },
                ]
            }),
    );
    events.extend(all_file_change_kinds().into_iter().flat_map(|kind| {
        [
            AgentEvent::FileChange {
                path: "src/main.rs".into(),
                kind,
                added: 4,
                removed: 1,
                diff: Some("+ a\n- b".into()),
            },
            AgentEvent::FileChange {
                path: "src/main.rs".into(),
                kind,
                added: 0,
                removed: 0,
                diff: None,
            },
        ]
    }));
    events.extend(all_block_kinds().into_iter().flat_map(|kind| {
        [
            AgentEvent::BlockRegistered {
                block_id: "blk_h".into(),
                kind,
                origin: BlockOrigin {
                    turn_instance: 1,
                    step: 0,
                    call_id: Some("call_1".into()),
                    memory_id: Some("nod_1".into()),
                },
                token_cost: 90,
                content_digest: "sha256:dd".into(),
                citation_label: Some("a recalled memory".into()),
                content: Some("the system prefix".into()),
            },
            AgentEvent::BlockRegistered {
                block_id: "blk_i".into(),
                kind,
                origin: BlockOrigin {
                    turn_instance: 1,
                    step: 0,
                    call_id: None,
                    memory_id: None,
                },
                token_cost: 0,
                content_digest: "sha256:ee".into(),
                citation_label: None,
                content: None,
            },
        ]
    }));
    events.extend(
        all_cache_zones()
            .into_iter()
            .map(|cache_zone| AgentEvent::StepManifest {
                turn_instance: 1,
                step: 0,
                call_seq: 0,
                role: ModelCallRole::Worker,
                provider: "anthropic".into(),
                model: "opus".into(),
                blocks: vec![ManifestEntry {
                    block_id: "blk_j".into(),
                    cache_zone,
                    token_cost: 10,
                    resident_since_step: 0,
                    message_index: 0,
                    call_id: None,
                }],
                effective_budget_tokens: 1,
                calibration_factor: 1.0,
                estimated_input_tokens: 10,
                compiled_frame: None,
            }),
    );
    events.extend(
        all_proof_steps()
            .into_iter()
            .map(|step| AgentEvent::Proof { step }),
    );
    events.extend(all_media_job_states().into_iter().flat_map(|state| {
        all_media_kinds()
            .into_iter()
            .map(move |kind| AgentEvent::MediaProgress {
                artifact_id: "art_1".into(),
                kind,
                state: state.clone(),
            })
    }));
    events.extend(
        all_pr_statuses()
            .into_iter()
            .zip(all_ci_statuses())
            .map(|(status, ci)| AgentEvent::Pr {
                url: "https://example.invalid/pull/3".into(),
                status,
                number: Some(3),
                ci: Some(ci),
            }),
    );
    events.extend(
        all_task_statuses()
            .into_iter()
            .map(|status| AgentEvent::TaskUpdate {
                tasks: vec![TaskItem {
                    id: "1".into(),
                    subject: "a task".into(),
                    description: None,
                    status,
                    owner: None,
                }],
            }),
    );
    // `all_proof_steps` samples one Oracle, so it pins only one `ProofTree`.
    // The tree is the whole content of a flip — a fail in Baseline followed by
    // a pass in Candidate — so both arms have to reach the wire.
    events.extend(all_proof_trees().into_iter().map(|tree| AgentEvent::Proof {
        step: ProofStep::Oracle {
            command: "cargo test -p x".into(),
            passed: false,
            tree,
            run: None,
            runs_required: None,
            seed: None,
        },
    }));
    events.extend(
        all_subagent_phases()
            .into_iter()
            .map(|phase| AgentEvent::SubAgent { phase }),
    );
    events.extend(
        all_subagent_statuses()
            .into_iter()
            .map(|status| AgentEvent::SubAgent {
                phase: SubAgentPhase::Finished {
                    agent_id: "search-1".into(),
                    status,
                    summary: "done".into(),
                    truncated: true,
                    cost_usd: 0.0,
                    steps: 0,
                    absorbed_messages: 0,
                    reason: Some("no budget headroom".into()),
                },
            }),
    );
    events.push(AgentEvent::SubAgent {
        phase: SubAgentPhase::Started {
            agent_id: "search-2".into(),
            instruction_preview: String::new(),
            budget_usd: None,
            write_access: true,
            depth: 2,
        },
    });
    // Every ladder rung (#1043). Each has to reach the wire on its own,
    // because the rung is the *only* thing separating verdicts that the
    // surrounding `passed`/`deterministic` flags spell identically — a
    // deterministic pass from a waived review, a verifier that answered from one
    // that was unavailable.
    events.extend(
        all_ladder_rungs()
            .into_iter()
            .map(|rung| AgentEvent::Verdict {
                passed: rung.is_deterministic(),
                evidence: VerdictEvidence {
                    summary: "sampled for the rung".into(),
                    deterministic: rung.is_deterministic(),
                    evidence_refs: vec![],
                    ladder: Some(Box::new(LadderSnapshot {
                        rung: Some(rung),
                        tracked_command: Some("cargo test -p x".into()),
                        oracle_trace: vec![OracleObservation {
                            tree: ProofTree::Candidate,
                            passed: true,
                        }],
                        flip_achieved: true,
                        unstable_flip: false,
                        flip_refused_different_failure: false,
                        touched_tests_passed: Some(true),
                        test_infra: Some("timed_out".into()),
                        diff_lines: 12,
                        diff_budget: 400,
                        diff_available: true,
                        file_change_events: 2,
                        mutating_actions: 3,
                        new_diag_errors: 0,
                        new_diag_warnings: 1,
                        witness_intact: Some(true),
                        witness_mutation: Some(true),
                        diff_coverage: Some("covered".into()),
                    })),
                },
            }),
    );

    events
}

// ---------------------------------------------------------------------------
// The proofs
// ---------------------------------------------------------------------------

#[test]
fn every_sample_event_validates_against_the_committed_schema() {
    let schema = committed_schema();
    let samples = sample_events();
    assert!(samples.len() > 100, "the sample set should be substantial");
    for event in &samples {
        assert_validates(&schema, event);
    }
}

/// How one arm of an enum shows up in serialized JSON.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Discriminant {
    /// A unit enum: the arm is a bare string value somewhere.
    Value(String),
    /// Internally tagged: `{"<tag>": "<arm>", …}`.
    Tagged(String, String),
    /// Externally tagged: `{"<arm>": {…}}`.
    Key(String),
}

/// Read the discriminants of one `$defs` enum straight out of the schema.
fn discriminants(def: &Value, name: &str) -> Vec<Discriminant> {
    def["oneOf"]
        .as_array()
        .unwrap_or_else(|| panic!("$defs/{name} is not a oneOf"))
        .iter()
        .map(|arm| {
            if let Some(Value::String(token)) = arm.get("const") {
                return Discriminant::Value(token.clone());
            }
            if let Some(properties) = arm.get("properties").and_then(Value::as_object) {
                for (property, subschema) in properties {
                    if let Some(Value::String(token)) = subschema.get("const") {
                        return Discriminant::Tagged(property.clone(), token.clone());
                    }
                }
            }
            if let Some(Value::Array(required)) = arm.get("required")
                && let [Value::String(only)] = required.as_slice()
            {
                return Discriminant::Key(only.clone());
            }
            panic!("$defs/{name}: cannot read a discriminant from {arm}")
        })
        .collect()
}

/// Every object key, string value, and (key, string value) pair anywhere in a
/// serialized sample — enough to answer "did this arm actually reach the wire".
#[derive(Default)]
struct Wire {
    keys: BTreeSet<String>,
    values: BTreeSet<String>,
    pairs: BTreeSet<(String, String)>,
}

impl Wire {
    fn absorb(&mut self, value: &Value) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    self.keys.insert(key.clone());
                    if let Value::String(text) = child {
                        self.pairs.insert((key.clone(), text.clone()));
                    }
                    self.absorb(child);
                }
            }
            Value::Array(items) => items.iter().for_each(|i| self.absorb(i)),
            Value::String(text) => {
                self.values.insert(text.clone());
            }
            _ => {}
        }
    }

    fn has(&self, discriminant: &Discriminant) -> bool {
        match discriminant {
            Discriminant::Value(token) => self.values.contains(token),
            Discriminant::Tagged(tag, token) => self.pairs.contains(&(tag.clone(), token.clone())),
            Discriminant::Key(key) => self.keys.contains(key),
        }
    }
}

#[test]
fn every_enum_arm_in_the_payload_graph_actually_reaches_the_wire() {
    // The length check below ratchets the sample *lists*; this checks the
    // thing that actually matters — that each arm was serialized into a
    // validated sample. The two are not the same: a vocabulary can be listed
    // in full and still only be half-wired into events (`BudgetDenied` zips
    // two of the three budget modes; the third rides `BudgetTick`).
    let schema = committed_schema();
    let mut wire = Wire::default();
    for event in sample_events() {
        wire.absorb(&serde_json::to_value(&event).expect("AgentEvent always serializes"));
    }

    let mut missing = Vec::new();
    for (name, def) in schema["$defs"].as_object().expect("$defs is an object") {
        if def.get("oneOf").is_none() {
            continue; // a struct, not an enum
        }
        for discriminant in discriminants(def, name) {
            if !wire.has(&discriminant) {
                missing.push(format!("{name}: {discriminant:?}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "these enum arms are never serialized by any sample, so nothing proves \
         they validate:\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn every_known_tag_has_a_sample() {
    // `KNOWN_TYPE_TAGS` is generated from a match the compiler requires to be
    // total over `AgentEvent`, so covering it IS covering every variant.
    let samples = sample_events();
    let covered: BTreeSet<&str> = samples.iter().map(AgentEvent::type_tag).collect();
    let expected: BTreeSet<&str> = KNOWN_TYPE_TAGS.iter().copied().collect();
    let missing: Vec<&&str> = expected.difference(&covered).collect();
    assert!(
        missing.is_empty(),
        "no sample event for {missing:?} — a new variant reached the wire without \
         being proved against docs/wire/agentevent.schema.json"
    );
    assert_eq!(
        covered, expected,
        "a sample carries a tag outside the vocabulary"
    );
}

#[test]
fn every_nested_vocabulary_is_fully_sampled() {
    // The schema's `$defs` are derived from the enums, so the arm count there
    // is the enum's arm count. Comparing each sample list against it makes
    // growing a vocabulary without extending the samples a test failure rather
    // than a silent coverage hole.
    let schema = committed_schema();
    let arms = |name: &str| -> usize {
        let def = schema
            .pointer(&format!("/$defs/{name}"))
            .unwrap_or_else(|| panic!("the schema has no $defs/{name}"));
        def["oneOf"]
            .as_array()
            .unwrap_or_else(|| panic!("$defs/{name} is not a oneOf"))
            .len()
    };

    let counts: BTreeMap<&str, usize> = BTreeMap::from([
        ("StageKind", all_stage_kinds().len()),
        ("BudgetMode", all_budget_modes().len()),
        ("BudgetScope", all_budget_scopes().len()),
        ("PolicyKind", all_policy_kinds().len()),
        ("ModelCallRole", all_model_call_roles().len()),
        (
            "UsageIncompleteReason",
            all_usage_incomplete_reasons().len(),
        ),
        ("FinishReason", all_finish_reasons().len()),
        ("FileChangeKind", all_file_change_kinds().len()),
        ("ProofTree", all_proof_trees().len()),
        ("LadderRung", all_ladder_rungs().len()),
        ("MediaKind", all_media_kinds().len()),
        ("PrStatus", all_pr_statuses().len()),
        ("CiStatus", all_ci_statuses().len()),
        ("TaskStatus", all_task_statuses().len()),
        ("BlockKind", all_block_kinds().len()),
        ("CacheZone", all_cache_zones().len()),
        ("SubAgentStatus", all_subagent_statuses().len()),
        ("ProofStep", all_proof_steps().len()),
        ("MediaJobState", all_media_job_states().len()),
        ("ToolOutput", all_tool_outputs().len()),
        ("SubAgentPhase", all_subagent_phases().len()),
    ]);
    for (name, sampled) in counts {
        assert_eq!(
            sampled,
            arms(name),
            "{name} has {} arms in the schema but {sampled} sample(s) here",
            arms(name)
        );
    }

    // And the list above must not itself go stale: every `$defs` entry that is
    // an enum (a `oneOf`) needs a row.
    let enum_defs: BTreeSet<&str> = schema["$defs"]
        .as_object()
        .expect("$defs is an object")
        .iter()
        .filter(|(_, def)| def.get("oneOf").is_some())
        .map(|(name, _)| name.as_str())
        .collect();
    let rows: BTreeSet<&str> = BTreeSet::from([
        "StageKind",
        "BudgetMode",
        "BudgetScope",
        "PolicyKind",
        "ModelCallRole",
        "UsageIncompleteReason",
        "FinishReason",
        "FileChangeKind",
        "ProofTree",
        "LadderRung",
        "MediaKind",
        "PrStatus",
        "CiStatus",
        "TaskStatus",
        "BlockKind",
        "CacheZone",
        "SubAgentStatus",
        "ProofStep",
        "MediaJobState",
        "ToolOutput",
        "SubAgentPhase",
    ]);
    assert_eq!(
        enum_defs, rows,
        "an enum in the payload graph has no sampled-arm-count row above"
    );
}

#[test]
fn a_wrong_field_name_fails_validation() {
    // The validator must actually reject things, or every assertion above is
    // vacuous. A `complete` event with its `model` field renamed is exactly
    // the drift the whole exercise exists to catch.
    let schema = committed_schema();
    let broken = json!({ "type": "complete", "modle": "opus", "cost_usd": 0.42 });
    let failures = errors(&schema, &schema, &broken, "complete");
    assert!(
        !failures.is_empty(),
        "a renamed required field must fail validation"
    );
}

#[test]
fn a_wrong_field_type_fails_validation() {
    let schema = committed_schema();
    let broken = json!({ "type": "complete", "model": "opus", "cost_usd": "free" });
    assert!(!errors(&schema, &schema, &broken, "complete").is_empty());
}

#[test]
fn an_event_from_the_future_is_outside_the_schema_by_construction() {
    // `AgentEvent::Unknown` re-serializes as the foreign object it wrapped, so
    // it matches no variant here. That is the correct and intended result: the
    // schema describes THIS build's vocabulary, and an unmatched `"type"` is
    // version skew, not corruption. Consumers must branch on that difference,
    // which is why it is pinned rather than left implicit.
    let schema = committed_schema();
    let future = AgentEvent::Unknown {
        event_type: "quantum_reticulation".into(),
        payload: json!({ "type": "quantum_reticulation", "splines": ["alpha", "beta"] }),
    };
    let instance = serde_json::to_value(&future).unwrap();
    assert!(
        !errors(&schema, &schema, &instance, "unknown").is_empty(),
        "an unrecognized tag must not validate — it is news, not a known event"
    );
    assert!(!KNOWN_TYPE_TAGS.contains(&future.type_tag()));
}

#[test]
fn the_committed_schema_declares_the_whole_known_vocabulary() {
    let schema = committed_schema();
    let tags: Vec<&str> = schema["oneOf"]
        .as_array()
        .expect("the root is a oneOf over tagged variants")
        .iter()
        .map(|v| {
            v.pointer("/properties/type/const")
                .and_then(Value::as_str)
                .expect("every variant carries a type const")
        })
        .collect();
    assert_eq!(tags, KNOWN_TYPE_TAGS, "in order, and complete");
}

/// The drift check the shell gate runs, available to `cargo test` when the
/// `schema` feature is on. The gate script is the required one — this exists
/// so `cargo test -p stella-protocol --features schema` catches it too.
#[cfg(feature = "schema")]
#[test]
fn the_committed_artifacts_match_the_types() {
    for (name, generated) in stella_protocol::schema_export::artifacts().unwrap() {
        let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "..", "..", "docs", "wire", name]
            .iter()
            .collect();
        let committed = std::fs::read_to_string(&path).expect("the artifact is committed");
        assert_eq!(
            committed, generated,
            "docs/wire/{name} is stale — run: bash scripts/export-agentevent-schema.sh"
        );
    }
}
