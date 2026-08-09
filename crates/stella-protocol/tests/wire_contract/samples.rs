//! The sample values the [wire contract](super) validates: one per arm of
//! every enum in the payload graph, plus the `AgentEvent` table itself.
//!
//! Split out of `wire_contract.rs` when it reached the file-size ceiling
//! (`scripts/check-file-size.sh`). The guard's rule is that new code goes in
//! its own module rather than onto the end of an already-full file, and this
//! is the coherent half: the fixtures grow with every new variant, while the
//! validator and the proofs above them do not. Everything here is `pub(crate)`
//! because the proofs are the only consumers.

use serde_json::json;
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
    AgentEvent, CompiledContextFrameBuilt, MediaArtifactRef, SubAgentPhase, SubAgentStatus,
    ToolCall, ToolOutput, VerdictEvidence,
};

pub(crate) fn all_stage_kinds() -> Vec<StageKind> {
    use StageKind::*;
    vec![
        Triage,
        ContextRecall,
        Research,
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

pub(crate) fn all_budget_modes() -> Vec<BudgetMode> {
    vec![BudgetMode::Off, BudgetMode::Observed, BudgetMode::Enforced]
}

pub(crate) fn all_budget_scopes() -> Vec<BudgetScope> {
    vec![BudgetScope::Turn, BudgetScope::Session]
}

pub(crate) fn all_policy_kinds() -> Vec<PolicyKind> {
    use PolicyKind::*;
    vec![Evaluated, Blocked, ApprovalRequested, SecretDetected]
}

pub(crate) fn all_model_call_roles() -> Vec<ModelCallRole> {
    use ModelCallRole::*;
    vec![
        Unknown,
        Triage,
        Research,
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

pub(crate) fn all_usage_incomplete_reasons() -> Vec<UsageIncompleteReason> {
    use UsageIncompleteReason::*;
    vec![ProviderError, Timeout, Cancelled]
}

pub(crate) fn all_finish_reasons() -> Vec<FinishReason> {
    use FinishReason::*;
    vec![Stop, Length, ToolCalls, ContentFilter]
}

pub(crate) fn all_file_change_kinds() -> Vec<FileChangeKind> {
    use FileChangeKind::*;
    vec![Read, Created, Modified, Deleted]
}

pub(crate) fn all_proof_trees() -> Vec<ProofTree> {
    vec![ProofTree::Baseline, ProofTree::Candidate]
}

pub(crate) fn all_ladder_rungs() -> Vec<LadderRung> {
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

pub(crate) fn all_media_kinds() -> Vec<MediaKind> {
    vec![MediaKind::Image, MediaKind::Svg, MediaKind::Video]
}

pub(crate) fn all_pr_statuses() -> Vec<PrStatus> {
    use PrStatus::*;
    vec![Draft, Open, Merged, Closed]
}

pub(crate) fn all_ci_statuses() -> Vec<CiStatus> {
    use CiStatus::*;
    vec![Pending, Running, Passing, Failing]
}

pub(crate) fn all_task_statuses() -> Vec<TaskStatus> {
    use TaskStatus::*;
    vec![Pending, InProgress, Completed, Cancelled]
}

pub(crate) fn all_block_kinds() -> Vec<BlockKind> {
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

pub(crate) fn all_cache_zones() -> Vec<CacheZone> {
    use CacheZone::*;
    vec![StablePrefix, Cacheable, Volatile, Other]
}

pub(crate) fn all_subagent_statuses() -> Vec<SubAgentStatus> {
    use SubAgentStatus::*;
    vec![Completed, Incomplete, Refused]
}

pub(crate) fn all_proof_steps() -> Vec<ProofStep> {
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
        ProofStep::VerdictDegraded {
            candidate: 2,
            reason: "the verifier call failed or timed out".into(),
        },
        ProofStep::TriageDegraded {
            reason: "the triage call timed out at its 30s ceiling".into(),
        },
    ]
}

pub(crate) fn all_media_job_states() -> Vec<MediaJobState> {
    vec![
        MediaJobState::Queued,
        MediaJobState::Running,
        MediaJobState::Succeeded,
        MediaJobState::Failed {
            reason: "provider rejected the prompt".into(),
        },
    ]
}

pub(crate) fn all_tool_outputs() -> Vec<ToolOutput> {
    vec![
        ToolOutput::Ok {
            content: "hello".into(),
        },
        ToolOutput::Error {
            message: "boom".into(),
        },
    ]
}

pub(crate) fn all_subagent_phases() -> Vec<SubAgentPhase> {
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

pub(crate) fn tool_call() -> ToolCall {
    ToolCall {
        call_id: "call_1".into(),
        name: "read_file".into(),
        input: json!({ "path": "src/main.rs" }),
    }
}

/// One sample per `KNOWN_TYPE_TAGS` entry, plus extra samples that exercise
/// every arm of the nested vocabularies and both optional-field shapes.
pub(crate) fn sample_events() -> Vec<AgentEvent> {
    let mut events = vec![
        AgentEvent::Text {
            text: "the answer".into(),
        },
        AgentEvent::TextDelta {
            delta: "the ".into(),
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
        AgentEvent::TurnParked {
            description: "CI for branch main settles".into(),
            poll_interval_secs: 5,
            deadline_secs: 600,
        },
        AgentEvent::TurnWoken {
            reason: "changed".into(),
            polls_used: 3,
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
            rewrites: vec![stella_protocol::CompactionRewrite {
                block_id: "blk_f".into(),
                content_digest: "sha256:aa".into(),
                content: "{\"ok\":{\"content\":\"[stub]\"}}".into(),
            }],
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
            rewrites: vec![],
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
                        verifier_independent: Some(false),
                    })),
                },
            }),
    );

    events
}
