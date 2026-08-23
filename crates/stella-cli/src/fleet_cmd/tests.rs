//! Fleet-command tests — moved verbatim (dedented) out of the parent's
//! inline `mod tests` when `fleet_cmd.rs` crossed the file-size ratchet.

use std::path::PathBuf;

use stella_fleet::CommitRecord;

use super::*;

/// The #803 cancellation seam, pinned from both directions: dropping the
/// dispatch's abandon sender IS a stop (the claims are being released;
/// the worker must stop writing), while dropping the supervisor's stop
/// sender is NOT (the fleet settled the handle; the work wins).
#[tokio::test(start_paused = true)]
async fn a_dropped_dispatch_future_stops_the_worker_a_settled_handle_does_not() {
    use tokio::sync::oneshot;

    // Dispatch dropped → the stop line resolves.
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let (abandon_tx, abandon_rx) = oneshot::channel::<()>();
    drop(abandon_tx);
    stop_or_abandoned(stop_rx, abandon_rx).await;
    drop(stop_tx);

    // Explicit supervisor stop → resolves too.
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let (_abandon_tx, abandon_rx) = oneshot::channel::<()>();
    stop_tx.send(()).expect("receiver is live");
    stop_or_abandoned(stop_rx, abandon_rx).await;

    // Settled handle (supervisor sender dropped), dispatch still live →
    // must park forever, not read as a stop.
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    drop(stop_tx);
    let (_abandon_tx, abandon_rx) = oneshot::channel::<()>();
    let parked = tokio::time::timeout(
        std::time::Duration::from_secs(3600),
        stop_or_abandoned(stop_rx, abandon_rx),
    )
    .await;
    assert!(parked.is_err(), "a settled handle must never stop the work");
}

#[test]
fn positional_prompts_become_independent_isolated_tasks() {
    let plan =
        load_plan(&["fix the login bug".into(), "add dark mode".into()], None).expect("plan");
    assert_eq!(plan.tasks.len(), 2);
    assert_eq!(plan.tasks[0].id, "t1");
    assert!(plan.tasks[1].depends_on.is_empty());
    plan.validate().expect("valid");
}

#[test]
fn toml_and_json_plans_deserialize_with_deps_and_isolation() {
    let toml_plan = r#"
        [[tasks]]
        id = "schema"
        title = "Add the users table"
        prompt = "add a users table migration"

        [[tasks]]
        id = "api"
        title = "Expose /users"
        prompt = "add the /users endpoint"
        depends_on = ["schema"]
        isolation = "shared_tree"
        claims = ["src/users.rs"]
    "#;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("plan.toml");
    std::fs::write(&path, toml_plan).expect("write");
    let plan = load_plan(&[], Some(&path)).expect("toml plan");
    assert_eq!(plan.tasks[1].depends_on, vec!["schema".to_string()]);
    assert_eq!(plan.tasks[1].claims, vec!["src/users.rs".to_string()]);
    assert!(plan.tasks[0].claims.is_empty(), "claims default to none");
    plan.validate().expect("valid");

    let json_path = dir.path().join("plan.json");
    std::fs::write(&json_path, serde_json::to_string(&plan).expect("serialize")).expect("write");
    let round = load_plan(&[], Some(&json_path)).expect("json plan");
    assert_eq!(round, plan);
}

#[test]
fn empty_input_and_unknown_extension_are_named_errors() {
    assert!(load_plan(&[], None).is_err());
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("plan.yaml");
    std::fs::write(&path, "tasks: []").expect("write");
    assert!(load_plan(&[], Some(&path)).is_err());
}

#[test]
fn summaries_are_single_line_and_capped() {
    let long = "a\nb\n".repeat(200);
    let out = truncate(&long);
    assert!(!out.contains('\n'));
    assert!(out.chars().count() <= SUMMARY_CHARS + 1);
    assert!(out.ends_with('…'));
}

// the worker's control lines (stella-fleet WorkerControls)

#[tokio::test]
async fn watch_gate_parks_while_paused_and_releases_on_resume_or_teardown() {
    use stella_core::ports::TurnGate;
    let (tx, rx) = watch::channel(true);
    let gate = WatchGate(rx);
    let wait = gate.wait_if_paused();
    tokio::pin!(wait);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut wait)
            .await
            .is_err(),
        "a paused gate must park"
    );
    tx.send(false).unwrap();
    tokio::time::timeout(std::time::Duration::from_millis(500), wait)
        .await
        .expect("resume releases the gate");

    // A dropped sender (the fleet settled the task's controls) must
    // release, never park forever.
    let (tx2, rx2) = watch::channel(true);
    let gate2 = WatchGate(rx2);
    drop(tx2);
    tokio::time::timeout(
        std::time::Duration::from_millis(500),
        gate2.wait_if_paused(),
    )
    .await
    .expect("teardown releases the gate");
}

#[tokio::test]
async fn fleet_attempt_persists_usage_before_complete_closeout() {
    let root = tempfile::tempdir().expect("root");
    let store = Arc::new(stella_store::Store::open(root.path()).expect("store"));
    let id = store
        .begin_execution("fleet", "task", "anthropic", "claude")
        .expect("begin");
    let execution = Some((store.clone(), id));
    let (tx, rx) = mpsc::unbounded_channel();
    let renderer = agent::spawn_renderer(
        rx,
        crate::OutputFormat::Json,
        execution.clone(),
        "anthropic".into(),
        false,
        None,
    );
    tx.send(AgentEvent::StepUsage {
        upstream_provider: None,
        reasoning_tokens: None,
        output_text: None,
        step: 0,
        role: stella_protocol::ModelCallRole::Worker,
        provider: "anthropic".into(),
        model: "claude".into(),
        input_tokens: 10,
        output_tokens: 2,
        cached_input_tokens: 0,
        cache_write_tokens: 0,
        estimated_input_tokens: 9,
        cost_usd: 0.25,
        duration_ms: 10,
        retries: 0,
        tool_calls: 0,
        complete: true,
        finish_reason: None,
        sub_agent_id: None,
    })
    .expect("event");
    drop(tx);
    let rendered = renderer.await.expect("renderer");
    let registry = ToolRegistry::new(root.path().to_path_buf());

    assert!(finalize_fleet_execution(
        &execution,
        &registry,
        "completed",
        0.25,
        rendered.persistence_complete,
        false,
    ));
    assert_eq!(store.count("telemetry").unwrap(), 1);
    assert!(store.execution_usage_complete(id).unwrap());
}

#[test]
fn stopped_fleet_attempt_never_becomes_exportable() {
    let root = tempfile::tempdir().expect("root");
    let store = Arc::new(stella_store::Store::open(root.path()).expect("store"));
    let id = store
        .begin_execution("fleet", "task", "anthropic", "claude")
        .expect("begin");
    let execution = Some((store.clone(), id));
    let registry = ToolRegistry::new(root.path().to_path_buf());

    assert!(!finalize_fleet_execution(
        &execution,
        &registry,
        "cancelled",
        0.0,
        true,
        true,
    ));
    assert!(!store.execution_usage_complete(id).unwrap());
    // The flag is what withholds it from the enterprise export
    // (`pending_enterprise_export_page` gates on `usage_complete = 1`, covered
    // by `stella-store`'s own
    // `pending_page_skips_incomplete_rows_without_consuming_them`). The
    // *rollup* is no longer that gate: since #4171 it arrives carrying the
    // flag, because a project total that reads a stopped attempt's spend as
    // zero is worse than one that reads it as a floor.
    let rollup = store
        .execution_rollup(id, root.path())
        .unwrap()
        .expect("a stopped attempt rolls up as a floor");
    assert!(!rollup.usage_complete, "and is marked as one");
}

// the post-fanout PR/CI watch (--watch)

use std::sync::{Arc, Mutex};

use stella_core::BudgetOutcome;
use stella_fleet::{CiConclusion, GhError, GhOutput, TaskHandle};

/// A routed fake `gh`: `run list` answers with the scripted CI snapshot,
/// `pr view` with the scripted PR json (or the real "no pull requests"
/// failure shape when the branch has no PR). Records every call.
struct RoutedGh {
    runs: String,
    pr: Option<String>,
    calls: Arc<Mutex<Vec<Vec<String>>>>,
}

impl RoutedGh {
    fn new(runs: &str, pr: Option<&str>) -> Self {
        Self {
            runs: runs.to_string(),
            pr: pr.map(str::to_string),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait::async_trait]
impl GhCli for RoutedGh {
    async fn run(&self, args: &[&str]) -> Result<GhOutput, GhError> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(args.iter().map(|s| s.to_string()).collect());
        if args.first() == Some(&"run") {
            Ok(GhOutput::ok(self.runs.clone()))
        } else {
            match &self.pr {
                Some(json) => Ok(GhOutput::ok(json.clone())),
                None => Ok(GhOutput::failed(1, "no pull requests found")),
            }
        }
    }
}

fn handle(task_id: &str, success: bool, branch: Option<&str>) -> TaskHandle {
    TaskHandle {
        task_id: task_id.to_string(),
        attempt_id: 1,
        outcome: WorkerOutcome {
            cost_usd: 0.0,
            commits: branch
                .map(|b| {
                    vec![CommitRecord {
                        sha: format!("sha-{task_id}"),
                        branch: b.to_string(),
                        task_id: task_id.to_string(),
                        message: "m".to_string(),
                        timestamp_ms: 1,
                    }]
                })
                .unwrap_or_default(),
            summary: String::new(),
            success,
        },
        worktree: None,
        budget: BudgetOutcome::Continue,
        ledger_error: None,
        lease_loss: None,
    }
}

#[test]
fn watch_targets_are_successful_committed_branches_watched_once() {
    let report = FleetRunReport {
        handles: vec![
            handle("t1", true, Some("fleet/t1-a")),
            // No commits → nothing on the branch to watch.
            handle("t2", true, None),
            // Failed → already fails the run; not watched.
            handle("t3", false, Some("fleet/t3-c")),
            // Same branch as t1 (shared-tree) → watched once.
            handle("t4", true, Some("fleet/t1-a")),
        ],
        ..FleetRunReport::default()
    };
    assert_eq!(
        watch_targets(&report),
        vec![("t1".to_string(), "fleet/t1-a".to_string())]
    );
}

#[tokio::test]
async fn watch_branch_reports_green_ci_and_open_pr() {
    let gh = RoutedGh::new(
        r#"[{"status":"completed","conclusion":"success","name":"ci"}]"#,
        Some(r#"{"state":"OPEN","isDraft":false}"#),
    );
    let calls = gh.calls.clone();
    let monitor = Monitor::new(gh, Box::new(SystemClock::new()));

    let watched = watch_branch(&monitor, "t1", "fleet/t1-abc").await;
    assert!(watched.is_green());
    assert_eq!(watched.pr, Some(PrStatus::Open));

    // The CI poll and the PR reconcile both targeted the fleet branch.
    let calls = calls.lock().unwrap();
    assert!(
        calls
            .iter()
            .any(|c| c.first().map(String::as_str) == Some("run")
                && c.iter().any(|a| a == "fleet/t1-abc"))
    );
    assert!(
        calls
            .iter()
            .any(|c| c.first().map(String::as_str) == Some("pr")
                && c.iter().any(|a| a == "fleet/t1-abc"))
    );
}

#[tokio::test]
async fn watch_branch_red_ci_and_a_missing_pr_are_states_not_errors() {
    let gh = RoutedGh::new(
        r#"[{"status":"completed","conclusion":"failure","name":"ci"}]"#,
        None,
    );
    let monitor = Monitor::new(gh, Box::new(SystemClock::new()));

    let watched = watch_branch(&monitor, "t1", "fleet/t1-abc").await;
    assert!(!watched.is_green());
    assert_eq!(watched.pr, None, "no PR for the branch is a normal state");
    assert!(matches!(
        watched.ci,
        Ok(CiWatchOutcome::Completed {
            conclusion: CiConclusion::Failure,
            ..
        })
    ));
}

#[tokio::test]
async fn watch_branch_treats_a_ci_timeout_as_red() {
    // No runs ever appear and the startup grace is already spent at the
    // first decision (elapsed >= grace with a 0ms grace) — the watch ends
    // as NoRunsStarted without sleeping, and the branch is red.
    let gh = RoutedGh::new("[]", None);
    let monitor = Monitor::new(gh, Box::new(SystemClock::new())).with_config(WatchConfig {
        poll_interval_ms: 1,
        max_total_ms: 60_000,
        stall_timeout_ms: 60_000,
        startup_grace_ms: 0,
        ..WatchConfig::default()
    });

    let watched = watch_branch(&monitor, "t1", "fleet/t1-abc").await;
    assert!(!watched.is_green());
    assert!(matches!(
        watched.ci,
        Ok(CiWatchOutcome::TimedOut {
            reason: TimeoutReason::NoRunsStarted,
            ..
        })
    ));
}

/// A raw fleet worker frames its turn on both sides (#3428).
///
/// #3416 moved stage boundaries out of the engine and into each run owner. The
/// fleet lane took only the opening half: it sent `Stage(Execute)` and never a
/// closer, so a worker's journal ended on `TurnComplete` with the run still
/// nominally executing — where every other run owner's ends with the pair.
/// Nothing caught it, because the fleet renders lanes from the ledger rather
/// than from a stage HUD, so no consumer that exists today reads the boundary
/// it was missing. That is exactly the shape a test has to hold.
///
/// The ordering is the substance, not a detail: the closer must arrive AHEAD
/// of the terminal event it annotates, because `TurnComplete` is emitted from
/// inside the turn and a consumer that stops there would never see a boundary
/// appended afterwards.
#[test]
fn a_raw_worker_closes_the_stage_it_opened() {
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    let worker = worker_event_sender(&tx);
    worker
        .send(AgentEvent::TurnComplete {
            model: "opus".to_string(),
            cost_usd: 0.5,
        })
        .expect("the receiver is alive");

    let mut seen = Vec::new();
    while let Ok(event) = rx.try_recv() {
        seen.push(event);
    }
    assert!(
        matches!(
            seen.as_slice(),
            [
                AgentEvent::Stage {
                    name,
                    scope: stella_protocol::StageScope::Run,
                },
                AgentEvent::TurnComplete { .. },
            ] if name.kind() == Some(stella_protocol::StageKind::Complete)
        ),
        "the closing boundary rides ahead of the terminal event it annotates: {seen:?}"
    );
}

/// ...and only for a turn that finished. An aborted worker reached no
/// completion, and a boundary claiming otherwise would be the journal's last
/// word on a run that failed.
#[test]
fn an_unfinished_worker_turn_claims_no_closing_boundary() {
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    let worker = worker_event_sender(&tx);
    worker
        .send(AgentEvent::Error {
            message: "the turn aborted".to_string(),
            retryable: false,
        })
        .expect("the receiver is alive");

    let mut seen = Vec::new();
    while let Ok(event) = rx.try_recv() {
        seen.push(event);
    }
    assert!(
        matches!(seen.as_slice(), [AgentEvent::Error { .. }]),
        "a turn that never completed gets no closing boundary: {seen:?}"
    );
}

/// A workspace a fleet worker could be dispatched into: one context record on
/// the volatile channel, and one workspace skill whose wording the prompt below
/// shares.
///
/// The user-home redirect is not decoration — skills load from
/// `~/.stella/skills` as well as from the workspace, so without it whoever runs
/// this suite would have their own real skills join the block and the
/// assertions would be reading someone's disk instead of this fixture.
fn steered_workspace() -> (tempfile::TempDir, crate::paths::TestPathsGuard, PathBuf) {
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let guard = crate::paths::test_user_home(home);

    let root = td.path().join("repo");
    let rules = root.join(crate::context_records::RULES_DIR);
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::write(
        rules.join("ctx.acme.migrations.toml"),
        r#"
schema = "context-record/v0.1"
set_id = "acme"

[defaults]
sharing_scope = "repository"
origin = "user"
status = "active"

[[record]]
lineage_id = "ctx.acme.migration-window"
kind = "preference"
statement = "Migrations only run inside the Tuesday window."

[record.steering]
force = "may"
"#,
    )
    .unwrap();

    let skill_dir = root.join(".stella").join("skills").join("migration-notes");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: migration-notes\ndescription: how to run a database migration safely\n---\n\n\
         Take a snapshot before the migration.\n",
    )
    .unwrap();

    (td, guard, root)
}

/// A `Config` that reaches the disk above and nothing else — offline: nothing
/// in this test builds a provider or spends a call.
fn steered_config(workspace_root: PathBuf) -> Config {
    let provider = crate::config::PROVIDERS
        .iter()
        .find(|p| p.id == "anthropic")
        .unwrap()
        .clone();
    Config {
        model_id: provider.default_model.to_string(),
        provider,
        turn_timeout: None,
        max_output_tokens: None,
        plan_mode: false,
        model_pinned_by_flag: false,
        durability: Default::default(),
        output_ceilings: Default::default(),
        create_worktrees: Default::default(),
        allowed_write_dirs: Vec::new(),
        api_key: stella_model::ApiKey::new("dummy-key-unused-offline"),
        workspace_root,
        base_url_override: None,
        hooks: None,
        engine_settings: None,
        engine_settings_trusted: false,
        tool_policy: Default::default(),
        ignore_gitignore: true,
        reward_policy: crate::reward::RewardPolicy::default(),
        // Workspace skills sit behind the project-trust boundary, so a witness
        // about a worker receiving one has to open the session the way a
        // trusted project does.
        authority: crate::settings::AuthorityPolicy {
            project_prompts_allowed: true,
            ..Default::default()
        },
        credential_source: None,
        credential_advisories: Vec::new(),
        aux_credentials: Default::default(),
        cache_ttl: None,
    }
}

/// **Witness (#3947).** A fleet worker's steering carries the volatile block —
/// the matched context record, the selected skill, and today's date.
///
/// Fails on base, where `worker_recall_block` does not exist because the fleet
/// door never assembled one: a worker got `build_system_prompt`'s byte-stable
/// prefix and stopped there, while `stella run`, `/goal`, the REPL and the deck
/// all got this half too.
///
/// The date assertion is the one that makes this a defect rather than a
/// preference. `append_session_environment` keeps today's date out of the
/// stable prefix *on purpose*, because it rides here (#2901) — so a worker
/// carried the knowledge-cutoff clause telling it to treat anything that may
/// have moved since as unverified, with no "now" to measure "since" against.
#[tokio::test]
async fn a_fleet_worker_receives_skills_records_and_todays_date() {
    let (_td, _guard, root) = steered_workspace();
    let cfg = steered_config(root.clone());
    let active_rules = crate::rules::load_workspace_rules(&root, &cfg.authority);

    // The control. If the registry rendered nothing on the volatile channel the
    // record assertion below would be looking for a string this workspace never
    // produced — a pass for entirely the wrong reason.
    assert!(
        active_rules
            .registry()
            .render(stella_core::records::Channel::Volatile, None)
            .text
            .contains("Tuesday window"),
        "the planted `may` record must render on the volatile channel before \
         this test can say anything about who receives it"
    );

    let (block, _event) = worker_recall_block(
        &root,
        &cfg,
        &active_rules,
        "run the database migration for the billing service",
    )
    .await;
    let block = block.expect("a worker in a steered workspace gets a volatile block");

    assert!(
        block.starts_with(crate::memory::RECALL_MARKER),
        "the block a worker receives is the same marked volatile block every \
         other door injects:\n{block}"
    );
    assert!(
        block.contains("Migrations only run inside the Tuesday window."),
        "the workspace's own published context record must reach the \
         unattended lane:\n{block}"
    );
    assert!(
        block.contains("migration-notes"),
        "a skill the prompt selects must reach the worker:\n{block}"
    );
    assert!(
        block.contains("Today's date:"),
        "the knowledge-cutoff clause in the stable prefix has no second \
         operand without this line (#2901):\n{block}"
    );
}

/// Answers one reflection call with a single lesson, at a known price.
///
/// The price is the point of the `cost_usd` field: a reflection that is not
/// settled back into the attempt's guard is a call the `--spend-limit` never
/// saw, and only a non-zero cost can tell that apart from a settled one.
struct OneLesson;

#[async_trait::async_trait]
impl stella_protocol::Provider for OneLesson {
    fn id(&self) -> &str {
        "one-lesson"
    }

    async fn complete_ref(
        &self,
        _req: stella_protocol::CompletionRequestRef<'_>,
    ) -> Result<stella_protocol::CompletionResult, stella_protocol::ProviderError> {
        Ok(stella_protocol::CompletionResult {
            upstream_provider: None,
            text: r#"{"lessons": [
                {"lesson": "the billing migration must be run with the ledger writer stopped",
                 "trigger": "a task touches the billing migration",
                 "saves": "a corrupted ledger",
                 "kind": "domain", "domains": []}
            ]}"#
            .into(),
            tool_calls: vec![],
            usage: stella_protocol::CompletionUsage {
                reported: true,
                input_tokens: 1,
                ..stella_protocol::CompletionUsage::default()
            },
            model: "one-lesson".into(),
            cost_usd: 0.02,
            finish_reason: None,
        })
    }
}

/// A transcript that warrants reflection — a tool call, which is what
/// `turn_warrants_reflection` gates the whole mining call on.
fn mined_transcript() -> Vec<CompletionMessage> {
    let mut worked = CompletionMessage::assistant("running it");
    worked.tool_calls = vec![stella_protocol::ToolCall {
        call_id: "c1".into(),
        name: "bash".into(),
        input: serde_json::json!({"command": "make migrate"}),
    }];
    vec![
        CompletionMessage::user("run the billing migration"),
        worked,
        CompletionMessage::assistant("migration applied"),
    ]
}

/// **Witness (#3956).** A lesson mined by a fleet worker is readable from the
/// primary workspace after the run — and is not written into the disposable
/// tree the attempt ran in.
///
/// Fails on base, where `mine_attempt_lesson` does not exist because a fleet
/// attempt mined nothing at all: `worker_recall_block` opened a `SessionMemory`,
/// recalled through it and dropped it before the turn ran, so the fan-out read
/// the workspace's accumulated lessons and contributed none back.
///
/// The two roots are the substance of the test, not scenery. An isolated task
/// runs in a linked worktree whose `.stella/private/` is gitignored and
/// therefore absent, so mining into the attempt's own root would write every
/// lesson a wave produced into databases that are deleted with their worktrees —
/// a learning loop that runs, costs money, and forgets. Asserting only that a
/// lesson was recorded would pass in exactly that broken arrangement, which is
/// why the negative half below is what makes this a witness.
#[tokio::test]
async fn a_worker_mines_its_lesson_into_the_invocation_root_not_its_worktree() {
    let (td, _guard, invocation_root) = steered_workspace();
    // The attempt's own tree — a sibling of the invocation root, standing in for
    // the linked worktree an isolated task runs in.
    let attempt_root = td.path().join("worktree");
    std::fs::create_dir_all(&attempt_root).expect("attempt tree");
    let cfg = steered_config(attempt_root.clone());

    let messages = mined_transcript();
    let friction = crate::memory::TurnFriction::default();
    let mut budget = agent::build_budget_guard(Some(10.0));
    budget.begin_turn();

    let report = super::mine_attempt_lesson(
        &invocation_root,
        &cfg,
        &OneLesson,
        crate::memory::TurnEvidence::with_friction(&messages, &friction, true),
        None,
        &super::attempt_task_boundary(&Task::new("t1", "billing", "run the billing migration")),
        &mut budget,
    )
    .await
    .expect("the invocation root's memory opens");

    assert_eq!(
        report.recorded, 1,
        "the scripted lesson must reach a store: {report:?}"
    );

    // The DoD's question, asked the way an operator would ask it: open the
    // primary workspace fresh, as a later session does, and recall.
    let after_the_run =
        crate::memory::SessionMemory::open(&invocation_root, false).expect("primary workspace");
    let recalled = after_the_run
        .recall_block_reported("a task touches the billing migration", &[])
        .await
        .text
        .unwrap_or_default();
    assert!(
        recalled.contains("ledger writer stopped"),
        "a lesson a worker mined must be readable from the primary workspace \
         after the run — this is the block a later session would receive:\n{recalled}"
    );

    // The negative half: nothing was written into the tree that gets deleted.
    let stranded = attempt_root
        .join(".stella")
        .join("private")
        .join("context.db");
    assert!(
        !stranded.exists(),
        "the attempt's own tree must hold no mined lesson — a worktree's context \
         store dies with the worktree, so a lesson written there is a call the \
         operator paid for and can never read back: {}",
        stranded.display()
    );

    // And the call is inside the fleet's spend contract rather than beside it.
    assert!(
        (budget.session_spent_usd() - 0.02).abs() < f64::EPSILON,
        "the reflection call must settle into the attempt's guard, which is what \
         puts it inside the `--spend-limit` the fleet enforces twice: {}",
        budget.session_spent_usd()
    );
}

/// The guard on the cross-store hazard (#3956): an execution id means something
/// only in the database that minted it.
///
/// An isolated attempt's execution is opened in its worktree's `store.db` while
/// its lesson is written into the invocation root's, and both are
/// autoincrement keys — so a stamped id would file the turn's self-review
/// against whatever unrelated execution happens to hold that number. The
/// non-normalized form is the case worth pinning: it must still be recognised
/// as the same tree, because a false negative here silently drops the
/// self-review of every shared-tree attempt.
#[test]
fn only_a_shared_tree_attempt_shares_the_invocation_root_s_row_ids() {
    let td = tempfile::tempdir().unwrap();
    let invocation = td.path().join("repo");
    let worktree = td.path().join("worktrees").join("t1");
    std::fs::create_dir_all(&invocation).unwrap();
    std::fs::create_dir_all(&worktree).unwrap();

    assert!(
        same_tree(&invocation, &invocation),
        "a shared-tree task runs in the invocation root itself"
    );
    assert!(
        same_tree(&invocation.join("."), &invocation),
        "a non-normalized path to the same tree must not read as a different \
         store — that would drop the self-review it is safe to write"
    );
    assert!(
        !same_tree(&worktree, &invocation),
        "an isolated attempt's worktree is a different database; stamping its \
         execution id into the invocation root's store writes a wrong row"
    );
}

/// The other half of the #3956 witness: the first test calls the seam directly,
/// so it would still pass with the production call site reverted. This asserts
/// the wiring — that the attempt reflects, and that it does so where the cost
/// can still reach the ledger.
///
/// Source-level for the reason `the_reflecting_doors_fold_a_ledger_and_reflect_with_it`
/// (`memory/reflection/tests.rs`) is: observing it otherwise means standing up a
/// provider, a git worktree, a store and a renderer task to watch one call. The
/// names asserted on are real items, so a rename breaks the compile too.
#[test]
fn a_fleet_attempt_reflects_before_its_spend_is_read() {
    const FLEET: &str = include_str!("../fleet_cmd.rs");

    // `rfind` so we anchor on the call site, not the `async fn` definition —
    // the definition also matches `mine_attempt_lesson(\n` but sits above both
    // the call and the spend read, which would make the ordering assert vacuous.
    let call = FLEET
        .rfind("mine_attempt_lesson(\n")
        .expect("run_task must mine the attempt's turn (#3956)");
    let spend = FLEET
        .find("let spent = budget.session_spent_usd();")
        .expect("run_task must still read the attempt's spend");
    assert!(
        call < spend,
        "the reflection has to settle into the guard BEFORE the spend is read — \
         after it, the call is real money the execution row, the fleet ledger and \
         the parent's metered total never see"
    );
    assert!(
        FLEET.contains("&invocation_root,"),
        "the lesson must be mined against the invocation root; against the \
         attempt's root it dies with the worktree"
    );
}

/// Answers each reflection call with a lesson of its own.
///
/// Two attempts saying the identical thing would derive the identical
/// observation id and the ledger would absorb the second as a replay — one
/// record, and a distinct-task count of 1 for a reason that has nothing to do
/// with the boundary under test.
struct ALessonPerAttempt(std::sync::atomic::AtomicUsize);

#[async_trait::async_trait]
impl stella_protocol::Provider for ALessonPerAttempt {
    fn id(&self) -> &str {
        "a-lesson-per-attempt"
    }

    async fn complete_ref(
        &self,
        _req: stella_protocol::CompletionRequestRef<'_>,
    ) -> Result<stella_protocol::CompletionResult, stella_protocol::ProviderError> {
        const LESSONS: [&str; 2] = [
            "the billing migration must be run with the ledger writer stopped",
            "the invoice backfill has to be replayed from the last checkpoint file",
        ];
        let nth = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(stella_protocol::CompletionResult {
            upstream_provider: None,
            text: format!(
                r#"{{"lessons": [
                    {{"lesson": "{}",
                     "trigger": "a task touches the billing migration",
                     "saves": "a corrupted ledger",
                     "kind": "domain", "domains": []}}
                ]}}"#,
                LESSONS[nth % LESSONS.len()]
            ),
            tool_calls: vec![],
            usage: stella_protocol::CompletionUsage {
                reported: true,
                input_tokens: 1,
                ..stella_protocol::CompletionUsage::default()
            },
            model: "a-lesson-per-attempt".into(),
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// **Witness (#3989).** Two attempts at one fleet task stamp one task boundary,
/// and that boundary is derived from the plan's task rather than from the
/// process the attempts happened to run in.
///
/// Fails on base, where `mine_attempt_lesson` never calls `set_task_id`: each
/// attempt opens a fresh `SessionMemory` and takes the session default, so a
/// retry wave reads to governance as several distinct tasks and can clear a
/// distinct-task promotion threshold on one task's worth of evidence.
///
/// The derivation is the half that discriminates; equality alone would not.
/// Two attempts in the same second of the same process already share
/// `session:<secs>-<pid>`, so "both boundaries are the same string" passes on
/// base by coincidence — which is why the assertion names `fleet:<task id>`.
///
/// The observation half is what makes it a statement about governance rather
/// than about a log line: `ObservationRecord::task_id` is the field the
/// distinct-task threshold counts, and it is reached through extraction, which
/// falls back to `turn:<timestamp>` for any lesson that carries no boundary.
#[tokio::test]
async fn two_attempts_at_one_task_are_one_task_to_governance() {
    let (_td, _guard, root) = steered_workspace();
    let cfg = steered_config(root.clone());
    let task = Task::new(
        "migrate-billing",
        "billing migration",
        "run the billing migration",
    );
    let messages = mined_transcript();
    let friction = crate::memory::TurnFriction::default();
    let provider = ALessonPerAttempt(std::sync::atomic::AtomicUsize::new(0));

    // Two attempts at the SAME task — a retry wave, which is the shape the
    // session default over-counts.
    for attempt in 0..2 {
        let mut budget = agent::build_budget_guard(Some(10.0));
        budget.begin_turn();
        let report = super::mine_attempt_lesson(
            &root,
            &cfg,
            &provider,
            crate::memory::TurnEvidence::with_friction(&messages, &friction, true),
            None,
            &super::attempt_task_boundary(&task),
            &mut budget,
        )
        .await
        .unwrap_or_else(|| panic!("attempt {attempt}: the workspace's memory opens"));
        // The control: an unreadable response mines nothing, and every
        // assertion below would then be reading an empty log.
        assert!(
            report.model_error.is_none(),
            "attempt {attempt} must mine a lesson before this test can say \
             anything about its boundary: {report:?}"
        );
    }

    let log = stella_store::workspace_private_state_path(&root, "reflections.jsonl")
        .expect("the mining log's path");
    let lessons: Vec<crate::memory::ReflectionLesson> = std::fs::read_to_string(&log)
        .expect("both attempts appended to the mining log")
        .lines()
        .map(|line| serde_json::from_str(line).expect("a mined lesson"))
        .collect();
    assert_eq!(lessons.len(), 2, "one lesson per attempt reached the log");
    for lesson in &lessons {
        assert_eq!(
            lesson.task_id, "fleet:migrate-billing",
            "a fleet attempt stamps the boundary it genuinely knows; the \
             session default `session:<secs>-<pid>` mints a synthetic task per \
             attempt, which is the over-count governance must not see"
        );
    }

    let db = stella_store::workspace_private_sqlite_path(&root, "context.db").expect("db path");
    let store = stella_context::ContextStore::open(db).expect("the context ledger opens");
    let observed = crate::memory::observations::all_observations(&store, 100);
    assert_eq!(
        observed.len(),
        2,
        "both attempts left an observation, so the count below is over two \
         records and not over one the ledger deduplicated"
    );
    let distinct: std::collections::BTreeSet<&str> =
        observed.iter().map(|o| o.task_id.as_str()).collect();
    assert_eq!(
        distinct,
        std::collections::BTreeSet::from(["fleet:migrate-billing"]),
        "the field the distinct-task threshold counts must hold ONE task for a \
         retry wave at one task: {distinct:?}"
    );
}
