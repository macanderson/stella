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
    assert!(store.execution_rollup(id, root.path()).unwrap().is_none());
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
