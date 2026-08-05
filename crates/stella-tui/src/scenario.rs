//! A scripted multi-agent scenario for demos, screenshots, and full-deck render
//! tests. It emits a realistic [`Inbound`] sequence plus a sample
//! [`GraphSnapshot`] so the command deck is fully populated without a live
//! engine — the same models snap onto real fleet data once a supervisor exists.
//!
//! `demo_inbound` is deterministic and order-stable, so it can drive both the
//! timed example playback (`examples/deck_demo.rs`) and a render snapshot test.

use serde_json::json;
use stella_protocol::{
    AgentEvent, ContextFrameRef, FileChangeKind, ModelCallRole, PrStatus, ProofStep, ProviderShare,
    ScopeProposal, StageKind, ToolCall, ToolOutput, VerdictEvidence,
};

use crate::envelope::{AgentMeta, AgentStatus, Inbound};
use crate::graph::{GraphEdge, GraphNode, GraphSnapshot};

/// A sample code-graph neighborhood centered on the engine step driver.
pub fn demo_graph() -> GraphSnapshot {
    let nodes = vec![
        GraphNode {
            label: "run_turn".into(),
            kind: "function".into(),
            location: Some("stella-core/src/driver.rs:239".into()),
        },
        GraphNode {
            label: "Engine".into(),
            kind: "struct".into(),
            location: Some("stella-core/src/engine.rs:48".into()),
        },
        GraphNode {
            label: "Router".into(),
            kind: "struct".into(),
            location: Some("stella-core/src/router.rs:373".into()),
        },
        GraphNode {
            label: "AgentEvent".into(),
            kind: "enum".into(),
            location: Some("stella-protocol/src/event.rs:55".into()),
        },
        GraphNode {
            label: "Ledger".into(),
            kind: "struct".into(),
            location: Some("stella-fleet/src/ledger.rs:90".into()),
        },
        GraphNode {
            label: "driver.rs".into(),
            kind: "file".into(),
            location: Some("stella-core/src/driver.rs".into()),
        },
    ];
    let edges = vec![
        GraphEdge {
            from: 0,
            to: 1,
            kind: "calls".into(),
        },
        GraphEdge {
            from: 1,
            to: 2,
            kind: "uses".into(),
        },
        GraphEdge {
            from: 0,
            to: 3,
            kind: "emits".into(),
        },
        GraphEdge {
            from: 5,
            to: 0,
            kind: "defines".into(),
        },
        GraphEdge {
            from: 1,
            to: 4,
            kind: "writes".into(),
        },
    ];
    GraphSnapshot {
        focus: "run_turn — engine step driver".into(),
        nodes,
        edges,
        // The picker's file list — the demo's source files, so `/` in the
        // Graph tab opens a browsable, filterable list during playback.
        files: vec![
            "stella-core/src/driver.rs".into(),
            "stella-core/src/engine.rs".into(),
            "stella-core/src/router.rs".into(),
            "stella-fleet/src/ledger.rs".into(),
            "stella-protocol/src/event.rs".into(),
        ],
    }
}

fn tool_start(id: &str, name: &str, input: serde_json::Value) -> AgentEvent {
    AgentEvent::ToolStart {
        call: ToolCall {
            call_id: id.into(),
            name: name.into(),
            input,
        },
    }
}

/// One recalled context frame. The scenario only varies the citation label, the
/// provider/source pair, the frame kind and the token cost — every demo frame
/// is unmaterialized (`id: None`) and carries no receipt.
fn frame(
    citation_label: &str,
    provider: &str,
    source: &str,
    kind: &str,
    token_cost: u32,
) -> ContextFrameRef {
    ContextFrameRef {
        id: None,
        citation_label: citation_label.into(),
        provider: provider.into(),
        source: source.into(),
        kind: kind.into(),
        uri: None,
        method: None,
        token_cost,
        block_id: None,
        content_digest: None,
    }
}

/// One provider's share of the frames that won fusion in a recall.
fn share(provider: &str, frames: u32) -> ProviderShare {
    ProviderShare {
        provider: provider.into(),
        frames,
    }
}

/// The lead's edit to the automations page. Hoisted out of the event list so
/// the script stays readable; `concat!` splits the literal across lines without
/// changing a byte of the diff.
const PAGE_DIFF: &str = concat!(
    "@@ -10,3 +10,7 @@\n",
    "-  const items = []\n",
    "+  const items = useAutomations()\n",
    "+  const [q, setQ] = useState(\"\")\n",
    "+  const filtered = filter(items, q)\n",
    "+  const onNew = () => open()\n",
);

/// The auth subagent's new triggers route, as a unified diff.
const TRIGGERS_DIFF: &str = concat!(
    "+export const triggers = router()\n",
    "+  .post(\"/\", create)\n",
    "+  .get(\"/\", list)\n",
);

/// The scripted event sequence, in play order. `self_pid` is stamped as every
/// agent's pid so the resource monitor shows real CPU/MEM for a live demo.
pub fn demo_inbound(started_ms: u64, self_pid: u32) -> Vec<Inbound> {
    let lead = "lead";
    let auth = "sub:auth";
    let ci = "sub:ci";

    let ev = |agent: &str, event: AgentEvent| Inbound::Event {
        agent: agent.into(),
        event,
    };
    let reg = |id: &str, title: &str, role: &str| {
        Inbound::Register(
            AgentMeta::new(id, title, started_ms)
                .with_role(role)
                .with_pid(self_pid),
        )
    };

    vec![
        // ── the lead agent boots and plans ──────────────────────────────
        reg(lead, "web-app-2.0 · phase 3 automations", "lead"),
        ev(
            lead,
            AgentEvent::Stage {
                name: StageKind::Triage,
            },
        ),
        ev(
            lead,
            AgentEvent::ContextRecall {
                frames: vec![
                    frame(
                        "engine step-driver (driver.rs)",
                        "code-graph",
                        "code-graph",
                        "symbol",
                        120,
                    ),
                    frame(
                        "event-log REPL (stella-tui)",
                        "workspace-memory",
                        "stella-context",
                        "memory",
                        90,
                    ),
                ],
                provider_mix: vec![share("code-graph", 1), share("memory", 1)],
                tokens: 210,
                usage: None,
                latency_ms: 34,
                used_ann_index: Some(true),
            },
        ),
        ev(
            lead,
            AgentEvent::Stage {
                name: StageKind::Plan,
            },
        ),
        ev(
            lead,
            AgentEvent::Text {
                delta: "Planning the automations cluster: list, editor, triggers, workflows."
                    .into(),
            },
        ),
        // The task board (`task_*` tools): a full snapshot per change, so the
        // task card, its slash-menu live hint, and the statline collapse all
        // have real rows to render.
        ev(
            lead,
            AgentEvent::TaskUpdate {
                tasks: demo_tasks(4),
            },
        ),
        ev(
            lead,
            AgentEvent::StepUsage {
                output_text: None,
                step: 1,
                role: ModelCallRole::Worker,
                provider: "zai".into(),
                model: "glm-5.2".into(),
                input_tokens: 12_400,
                output_tokens: 640,
                cached_input_tokens: 9_000,
                cache_write_tokens: 0,
                estimated_input_tokens: 12_000,
                cost_usd: 0.021,
                duration_ms: 1830,
                retries: 0,
                tool_calls: 0,
                complete: true,
                finish_reason: None,
            },
        ),
        ev(
            lead,
            AgentEvent::BudgetTick {
                spent_usd: 0.021,
                limit_usd: Some(2.5),
                mode: stella_protocol::BudgetMode::Observed,
                session_spent_usd: Some(0.021),
                session_limit_usd: Some(7.50),
            },
        ),
        // ── the witness is authored before the worker executes ──────────
        // The proof events the witness panel folds: triage bought a witness,
        // an independent model authored the failing test, and the oracle
        // confirmed it red on the untouched tree. Fixed seed/fingerprint so
        // the goldens stay byte-stable.
        ev(
            lead,
            AgentEvent::Stage {
                name: StageKind::Witness,
            },
        ),
        ev(
            lead,
            AgentEvent::Proof {
                step: ProofStep::Assurance {
                    witness: true,
                    verifier: false,
                },
            },
        ),
        ev(
            lead,
            AgentEvent::Proof {
                step: ProofStep::WitnessAuthored {
                    path: "apps/app/automations/triggers.test.ts".into(),
                    command: "pnpm --filter app test:unit triggers".into(),
                    fingerprint: "sha256:9f3c1d2e4a5b6c7d8e9f".into(),
                },
            },
        ),
        ev(
            lead,
            AgentEvent::Proof {
                step: ProofStep::Oracle {
                    command: "pnpm --filter app test:unit triggers".into(),
                    passed: false,
                    tree: stella_protocol::ProofTree::Baseline,
                    run: None,
                    runs_required: Some(3),
                    seed: Some(7741),
                },
            },
        ),
        // ── two subagents are dispatched ────────────────────────────────
        reg(auth, "wire automations triggers API", "subagent"),
        reg(ci, "watch CI + open PR", "subagent"),
        Inbound::Status {
            agent: auth.into(),
            status: AgentStatus::Running,
        },
        Inbound::Status {
            agent: ci.into(),
            status: AgentStatus::Running,
        },
        // ── lead executes: reads, edits, commits ────────────────────────
        ev(
            lead,
            AgentEvent::Stage {
                name: StageKind::Execute,
            },
        ),
        ev(
            lead,
            tool_start(
                "c1",
                "read_file",
                json!({ "path": "apps/app/automations/page.tsx" }),
            ),
        ),
        ev(
            lead,
            AgentEvent::ToolResult {
                call_id: "c1".into(),
                output: ToolOutput::Ok {
                    content: "312 lines".into(),
                },
                duration_ms: 42,
                speculated: false,
            },
        ),
        ev(
            lead,
            AgentEvent::FileChange {
                path: "apps/app/automations/page.tsx".into(),
                kind: FileChangeKind::Modified,
                // Scripted scenario: the counts come from the fixture diff, so
                // the demo shows the same numbers a real turn would.
                added: crate::diff::count_diff_lines(PAGE_DIFF).0,
                removed: crate::diff::count_diff_lines(PAGE_DIFF).1,
                diff: Some(PAGE_DIFF.into()),
            },
        ),
        ev(
            lead,
            AgentEvent::StepUsage {
                output_text: None,
                step: 2,
                role: ModelCallRole::Worker,
                provider: "zai".into(),
                model: "glm-5.2".into(),
                input_tokens: 8_200,
                output_tokens: 900,
                cached_input_tokens: 6_000,
                cache_write_tokens: 0,
                estimated_input_tokens: 8_000,
                cost_usd: 0.018,
                duration_ms: 2100,
                retries: 0,
                tool_calls: 1,
                complete: true,
                finish_reason: None,
            },
        ),
        ev(
            lead,
            AgentEvent::BudgetTick {
                spent_usd: 0.039,
                limit_usd: Some(2.5),
                mode: stella_protocol::BudgetMode::Observed,
                session_spent_usd: Some(0.039),
                session_limit_usd: Some(7.50),
            },
        ),
        // ── the oracle replays against the patched tree ─────────────────
        // Two of the three required deterministic replays have passed — the
        // witness panel's `execute` record is live at `run 2 of 3` with the
        // pinned seed.
        ev(
            lead,
            AgentEvent::Proof {
                step: ProofStep::Oracle {
                    command: "pnpm --filter app test:unit triggers".into(),
                    passed: true,
                    tree: stella_protocol::ProofTree::Candidate,
                    run: Some(1),
                    runs_required: Some(3),
                    seed: Some(7741),
                },
            },
        ),
        ev(
            lead,
            AgentEvent::Proof {
                step: ProofStep::Oracle {
                    command: "pnpm --filter app test:unit triggers".into(),
                    passed: true,
                    tree: stella_protocol::ProofTree::Candidate,
                    run: Some(2),
                    runs_required: Some(3),
                    seed: Some(7741),
                },
            },
        ),
        // The board moves: the triggers task completes, the workflows task
        // goes active — restamping the task card's elapsed/cost anchors.
        ev(
            lead,
            AgentEvent::TaskUpdate {
                tasks: demo_tasks(5),
            },
        ),
        // ── auth subagent: an approved scope, a file, then a question ───
        // The scope approves silently (ScopeReview followed by a non-review
        // stage), so the subagent's SESSION vitals can carry its write globs
        // without raising a gate card.
        ev(
            auth,
            AgentEvent::ScopeReview {
                proposal: ScopeProposal {
                    summary: "wire the automations triggers API".into(),
                    steps: vec!["add the route".into(), "wire the guard".into()],
                    estimated_files: 3,
                    estimated_cost_usd: Some(0.15),
                    repo: Some("macanderson/web-app".into()),
                    branch: Some("feat/automations-triggers".into()),
                    write_globs: vec!["apps/api/routes/v1/**".into()],
                    read_globs: vec!["apps/api/**".into()],
                    shell_policy: Some("allowlisted".into()),
                },
            },
        ),
        ev(
            auth,
            AgentEvent::Stage {
                name: StageKind::Execute,
            },
        ),
        ev(
            auth,
            tool_start("c2", "grep", json!({ "pattern": "trigger" })),
        ),
        ev(
            auth,
            AgentEvent::FileChange {
                path: "apps/api/routes/v1/automations.ts".into(),
                kind: FileChangeKind::Created,
                added: crate::diff::count_diff_lines(TRIGGERS_DIFF).0,
                removed: crate::diff::count_diff_lines(TRIGGERS_DIFF).1,
                diff: Some(TRIGGERS_DIFF.into()),
            },
        ),
        ev(
            auth,
            AgentEvent::StepUsage {
                output_text: None,
                step: 1,
                role: ModelCallRole::Worker,
                provider: "zai".into(),
                model: "glm-5.2".into(),
                input_tokens: 5_100,
                output_tokens: 420,
                cached_input_tokens: 3_000,
                cache_write_tokens: 0,
                estimated_input_tokens: 5_000,
                cost_usd: 0.011,
                duration_ms: 1400,
                retries: 0,
                tool_calls: 1,
                complete: true,
                finish_reason: None,
            },
        ),
        ev(
            auth,
            AgentEvent::BudgetTick {
                spent_usd: 0.011,
                limit_usd: Some(1.0),
                mode: stella_protocol::BudgetMode::Observed,
                session_spent_usd: None,
                session_limit_usd: None,
            },
        ),
        ev(
            auth,
            AgentEvent::AskUser {
                id: "q1".into(),
                question: "Which auth guard should the triggers route use?".into(),
                options: vec!["assertOrgMember".into(), "assertBillingManager".into()],
            },
        ),
        // ── ci subagent verifies + opens a PR ───────────────────────────
        ev(
            ci,
            AgentEvent::Stage {
                name: StageKind::Verify,
            },
        ),
        ev(
            ci,
            AgentEvent::Verdict {
                passed: true,
                evidence: VerdictEvidence {
                    summary: "flip oracle: fail→pass on `pnpm --filter app test:unit`".into(),
                    deterministic: true,
                    evidence_refs: vec![],
                    ladder: None,
                },
            },
        ),
        ev(
            ci,
            AgentEvent::Commit {
                sha: "a1b2c3d".into(),
                message: "feat(automations): triggers + workflows UI".into(),
            },
        ),
        ev(
            ci,
            AgentEvent::Pr {
                url: "https://github.com/macanderson/stella/pull/981".into(),
                status: PrStatus::Open,
                number: Some(981),
                ci: Some(stella_protocol::CiStatus::Running),
            },
        ),
        ev(
            ci,
            AgentEvent::StepUsage {
                output_text: None,
                step: 1,
                role: ModelCallRole::Worker,
                provider: "zai".into(),
                model: "glm-5.2-air".into(),
                input_tokens: 3_000,
                output_tokens: 180,
                cached_input_tokens: 0,
                cache_write_tokens: 0,
                estimated_input_tokens: 2_900,
                cost_usd: 0.004,
                duration_ms: 900,
                retries: 0,
                tool_calls: 0,
                complete: true,
                finish_reason: None,
            },
        ),
        // ── lead proposes a larger scope change (gate) ──────────────────
        // Carries the full scope-card grid facts (repo/branch, globs, shell
        // policy) so `/scope` renders every labeled row from scripted data.
        ev(
            lead,
            AgentEvent::ScopeReview {
                proposal: ScopeProposal {
                    summary: "Refactor the automations store into a shared workspace package"
                        .into(),
                    steps: vec![
                        "extract types".into(),
                        "move hooks".into(),
                        "update 9 imports".into(),
                    ],
                    estimated_files: 11,
                    estimated_cost_usd: Some(0.42),
                    repo: Some("macanderson/web-app".into()),
                    branch: Some("feat/automations-store".into()),
                    write_globs: vec![
                        "packages/automations/**".into(),
                        "apps/app/automations/**".into(),
                    ],
                    read_globs: vec!["apps/**".into(), "packages/**".into()],
                    shell_policy: Some("allowlisted".into()),
                },
            },
        ),
    ]
}

/// The demo task board: `tasks_done` rows completed, the next in progress,
/// the rest pending — first snapshot 4 of 7 done (the exact fraction the
/// task card's title advertises), second 5 of 7 after the triggers task
/// lands. Public so `deck_demo` can fold a skip back over the same board.
pub fn demo_tasks(tasks_done: usize) -> Vec<stella_protocol::TaskItem> {
    use stella_protocol::{TaskItem, TaskStatus};
    let rows = [
        ("1", "Scaffold the automations routes"),
        ("2", "List view with live filters"),
        ("3", "Editor panel plumbing"),
        ("4", "Persist automation drafts"),
        ("5", "Wire the triggers API"),
        ("6", "Workflows canvas"),
        ("7", "Verify: witness suite green"),
    ];
    rows.iter()
        .enumerate()
        .map(|(i, (id, subject))| TaskItem {
            id: (*id).to_string(),
            subject: (*subject).to_string(),
            description: (i == tasks_done)
                .then(|| "Route POST/GET /v1/automations/triggers through the org guard".into()),
            status: if i < tasks_done {
                TaskStatus::Completed
            } else if i == tasks_done {
                TaskStatus::InProgress
            } else {
                TaskStatus::Pending
            },
            owner: (i <= tasks_done).then(|| "lead".to_string()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck::WorkspaceModel;

    #[test]
    fn demo_inbound_folds_into_a_populated_workspace() {
        let mut model = WorkspaceModel::new();
        model.now_ms = 60_000;
        for inbound in demo_inbound(0, 1234) {
            model.apply_inbound(&inbound);
        }
        // Three agents, files in the ledger, routes observed, a pending gate.
        assert_eq!(model.agents.len(), 3);
        assert!(model.ledger.file_count() >= 2);
        assert!(model.ledger.total_added() > 0);
        assert!(model.latest_model().is_some());
        assert!(!model.trace.rows.is_empty());
        let lead = &model.agents[model.index_of("lead").unwrap()];
        assert!(lead.model.pending_scope_review.is_some());
        let auth = &model.agents[model.index_of("sub:auth").unwrap()];
        assert_eq!(auth.status, AgentStatus::WaitingInput);
    }

    #[test]
    fn demo_graph_is_non_empty_and_consistent() {
        let g = demo_graph();
        assert!(!g.is_empty());
        for e in &g.edges {
            assert!(e.from < g.nodes.len() && e.to < g.nodes.len());
        }
    }
}
