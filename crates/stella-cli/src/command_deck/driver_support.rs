//! Driver-loop support: the channel-owning background tasks (MCP connect,
//! session replay, the PR and notification pollers) and the synchronous
//! session-registry / supervisor-message handlers they feed.
//!
//! Split out of `command_deck.rs` (closed to growth) the way `skills.rs` and
//! `authoring.rs` were. Every function here takes its channels and state as
//! explicit owned/borrowed arguments — none of it closes over the driver
//! loop's locals — so the move is a relocation, not a rewrite.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use stella_protocol::{AgentEvent, CiStatus, PrStatus};
use stella_store::Store;
use stella_tools::ToolRegistry;
use stella_tui::{AgentMeta, AgentStatus, Inbound, WorkspaceInput};
use tokio::sync::mpsc::{self, UnboundedSender};

use super::{
    LEAD, agent, ci_status_token, now_ms, observe_pr, pr_status_token, session_clear,
    sessions_view, system_notice, worker_control,
};
use crate::config::Config;
use crate::subsession::{self, SubSessions, SupervisorMsg};

/// Run the MCP connect on its own task, landing the connected set in `slot`
/// for turns to pick up at dispatch. Returns whether any servers are
/// configured at all (`false` = the slot will stay empty forever, so no
/// "still connecting" note is ever warranted). Always seeds the MCP tab and
/// releases the splash leg, whatever the plan resolves to.
///
/// Connect narration is session chrome (`chrome_tx`, the direct deck path):
/// it re-runs at every boot, so journaling it would pile stale "connecting…"
/// lines onto every resumed transcript. The status flips ride the journaled
/// `in_tx` — `waiting_input` is also the journal's settle marker.
pub(super) fn spawn_mcp_connect(
    cfg: Config,
    registry: Arc<ToolRegistry>,
    disabled: stella_mcp::DisabledServers,
    slot: Arc<tokio::sync::OnceCell<Arc<stella_mcp::McpToolSet>>>,
    in_tx: UnboundedSender<Inbound>,
    chrome_tx: UnboundedSender<Inbound>,
    release_splash: impl FnOnce() + Send + 'static,
) -> bool {
    let plan = agent::load_mcp_plan(&cfg);
    let configured = matches!(plan, agent::McpPlan::Servers(..));
    tokio::spawn(async move {
        match plan {
            agent::McpPlan::None => {}
            agent::McpPlan::Invalid(reason) => {
                let _ = chrome_tx.send(system_notice(reason));
                let _ = in_tx.send(Inbound::Status {
                    agent: LEAD.to_string(),
                    status: AgentStatus::WaitingInput,
                });
            }
            agent::McpPlan::Servers(servers, notices) => {
                // Whose server was dropped, and whose package's file did not
                // parse — before the connect, so the report below is read
                // against a list the human knows was narrowed.
                for notice in notices {
                    let _ = chrome_tx.send(system_notice(notice));
                }
                let _ = chrome_tx.send(system_notice(format!(
                    "connecting {} MCP server(s)…",
                    servers.len()
                )));
                match crate::mcp_cmd::oauth_manager(&cfg.workspace_root) {
                    Ok(auth) => {
                        let set = agent::connect_mcp_servers(
                            &servers,
                            registry.clone(),
                            Some(registry.mcp_usage_ledger()),
                            Some(disabled.clone()),
                            Some(auth),
                        )
                        .await;
                        let _ =
                            chrome_tx.send(system_notice(crate::mcp_cmd::mcp_connect_report(&set)));
                        // `set` is infallible here (the cell is set exactly once,
                        // by this task); an in-flight turn keeps its resolved
                        // executor and the NEXT turn picks the servers up. Arc'd so
                        // a turn can share it into Best-of-N candidates (#248 Ph1).
                        let _ = slot.set(Arc::new(set));
                    }
                    Err(error) => {
                        let _ = chrome_tx.send(system_notice(format!(
                            "MCP authentication unavailable: {error} — continuing with native tools only"
                        )));
                    }
                }
                // No turn is in flight — assert the idle status so the
                // dashboard cannot show a busy lead. (The chrome above no
                // longer folds it to `Running`; see `system_notice`.)
                let _ = in_tx.send(Inbound::Status {
                    agent: LEAD.to_string(),
                    status: AgentStatus::WaitingInput,
                });
            }
        }
        // Seed the MCP tab with the configured servers and their live state.
        crate::deck_mcp::send_mcp_snapshot(&cfg, slot.get().map(Arc::as_ref), &disabled, &in_tx)
            .await;
        // MCP connect settled (or there was nothing to connect) — the other
        // init leg the launch splash waits on.
        release_splash();
    });
    configured
}

/// Service a session-registry / inbox verb from the deck. Returns `true` if
/// `input` was one (so the caller skips its own dispatch). All of these are
/// cheap local file ops, serviced identically idle or mid-turn.
// Every argument is a distinct handle the verbs need (registry, store, config,
// budget, the two identities, the channel) — bundling them into a struct would
// move the same list one hop away from the one call site.
#[allow(clippy::too_many_arguments)]
pub(super) fn service_registry_action(
    input: &WorkspaceInput,
    scope: &sessions_view::SessionScope<'_>,
    in_tx: &mpsc::UnboundedSender<Inbound>,
) -> bool {
    let sessions_view::SessionScope { registry, mine, .. } = *scope;
    match input {
        WorkspaceInput::SessionsRefresh => {
            let _ = in_tx.send(scope.snapshot());
            // The rows without a description get one, off the pump.
            sessions_view::describe_sessions(scope, in_tx.clone());
        }
        WorkspaceInput::SessionOpen { id } => {
            spawn_session_replay(id.clone(), registry.list(), in_tx.clone());
        }
        WorkspaceInput::SessionArchive { id } => {
            let _ = registry.set_status(id, stella_store::SessionStatus::Archived);
            let _ = in_tx.send(scope.snapshot());
        }
        WorkspaceInput::SessionDelete { id } => {
            // The deck refuses to delete its own record UI-side too; this is
            // the belt-and-suspenders check.
            if id != mine {
                let _ = registry.remove(id);
            }
            let _ = in_tx.send(scope.snapshot());
        }
        WorkspaceInput::NotificationRead { id } => {
            let store = stella_store::NotificationStore::open_default();
            let _ = store.mark_read(id);
            let _ = in_tx.send(notifications_inbound(&store));
        }
        WorkspaceInput::NotificationsReadAll => {
            let store = stella_store::NotificationStore::open_default();
            let _ = store.mark_all_read();
            let _ = in_tx.send(notifications_inbound(&store));
        }
        _ => return false,
    }
    true
}

/// The inbox snapshot for the deck (badge + overlay), newest first.
pub(super) fn notifications_inbound(store: &stella_store::NotificationStore) -> Inbound {
    let items = store
        .list()
        .into_iter()
        .map(|n| stella_tui::NotificationInfo {
            id: n.id,
            title: n.title,
            body: n.body,
            source: n.source,
            created_ms: n.created_at_ms,
            read: n.read,
            session_id: n.session_id,
        })
        .collect();
    Inbound::Notifications(items)
}

/// Service one supervisor message: dispatch or park a `task_assign` spawn,
/// and on a worker's end free its slot, close the delegation loop (a task
/// worker succeeding completes its board task), meter the worker's spend
/// toward the session budget, nudge the PR monitor, then drain whatever the
/// freed slot can take — parked spawns first, then the prompt backlog.
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_supervisor_msg(
    msg: SupervisorMsg,
    subs: &mut SubSessions,
    pending_controls: &mut worker_control::Pending,
    pending_spawns: &mut VecDeque<subsession::QueuedSpawn>,
    queue: &mut crate::session_persist::DurableQueue,
    dispatch_held: bool,
    registry: &ToolRegistry,
    store: &Option<Arc<Store>>,
    session_id: &str,
    workspace_name: &str,
    cfg: &Config,
    budget_limit: Option<f64>,
    unmetered_spend: &mut f64,
    pr_nudge: &Arc<tokio::sync::Notify>,
    in_tx: &UnboundedSender<Inbound>,
    sup_tx: &UnboundedSender<SupervisorMsg>,
) {
    match msg {
        SupervisorMsg::SpawnTask(queued) => {
            // A task's lane is its identity: a second worker on a live lane
            // would share (and corrupt) its channels, so a re-assign of an
            // in-flight task is reported instead of spawned.
            if subs.is_live(&subsession::task_lane(&queued.request.task_id)) {
                let _ = in_tx.send(Inbound::Event {
                    agent: LEAD.to_string(),
                    event: AgentEvent::Text {
                        text: format!(
                            "note: task #{} already has a live worker — the duplicate \
                             task_assign was not dispatched",
                            queued.request.task_id
                        ),
                    },
                });
            } else if subs.has_slot() {
                subsession::spawn_task_worker(
                    &queued,
                    subs,
                    cfg,
                    budget_limit,
                    session_id,
                    workspace_name,
                    in_tx,
                    sup_tx,
                );
            } else {
                pending_spawns.push_back(queued);
            }
        }
        SupervisorMsg::Ended {
            lane,
            generation,
            execution_id,
            cost_usd,
            end,
        } => {
            // Only the generation that ended frees the lane — a late Ended
            // from a replaced worker must not steal its replacement's slot
            // (or, below, respawn the lane a second time).
            let freed = subs.ended(&lane, generation);
            // A Delete accepted while this worker was live takes the row
            // down now — and outranks a Restart armed earlier: the later
            // intent won at `worker_control::service`.
            let deleted =
                freed && worker_control::finish_delete(&lane, pending_controls, subs, in_tx);
            // A Restart that arrived while this worker was live respawns it
            // now — restart takes the freed slot ahead of parked spawns.
            if freed && !deleted && pending_controls.restarts.remove(&lane) {
                let _ = subsession::respawn(
                    &lane,
                    subs,
                    cfg,
                    budget_limit,
                    session_id,
                    workspace_name,
                    in_tx,
                    sup_tx,
                );
            }
            // Worker spend reaches the session's parent budget guard (the
            // L-E9 discipline). The guard is mutably borrowed by any in-
            // flight lead turn, so the driver accumulates here and meters at
            // the loop top, the next safe boundary — budget aborts happen at
            // boundaries only, never mid-flight.
            *unmetered_spend += cost_usd;
            // A worker may have just pushed a branch / opened a PR — observe
            // now, not at the next 45s tick.
            pr_nudge.notify_one();
            // The delegation loop closes against the task board — unless the
            // worker predates a `/clear`, in which case there is no longer a
            // board of its to close (#1692).
            session_clear::settle_worker_task(
                &lane,
                generation,
                &end,
                subs,
                registry,
                session_clear::BoardMirror::of(store.as_ref(), session_id, execution_id),
                in_tx,
            );
            while subs.has_slot()
                && let Some(queued) = pending_spawns.pop_front()
            {
                // A parked duplicate of a task whose worker is (still) live
                // is dropped for the same reason as at arrival.
                if subs.is_live(&subsession::task_lane(&queued.request.task_id)) {
                    continue;
                }
                subsession::spawn_task_worker(
                    &queued,
                    subs,
                    cfg,
                    budget_limit,
                    session_id,
                    workspace_name,
                    in_tx,
                    sup_tx,
                );
            }
            subsession::drain_queue(
                queue,
                subs,
                dispatch_held,
                cfg,
                budget_limit,
                session_id,
                workspace_name,
                in_tx,
                sup_tx,
            );
        }
    }
}

/// Open a session in a replay lane ([`WorkspaceInput::SessionOpen`]): load
/// its persisted journal from the session's own workspace store (linked via
/// `executions.session_id`, store schema v8) and stream it through the
/// deck's ordinary fold. Replay IS the fold — a session dead for 12 hours
/// reconstructs to exactly the state it reached, through the same rendering
/// path a live session uses. Heavy reads run on the blocking pool.
pub(super) fn spawn_session_replay(
    id: String,
    records: Vec<stella_store::SessionRecord>,
    in_tx: mpsc::UnboundedSender<Inbound>,
) {
    tokio::task::spawn_blocking(move || {
        let Some(record) = records.into_iter().find(|r| r.id == id) else {
            let _ = in_tx.send(Inbound::Event {
                agent: LEAD.to_string(),
                event: AgentEvent::Text {
                    text: format!("session {id} is no longer in the registry"),
                },
            });
            return;
        };
        // The prefix is the journal tee's filter key
        // (`session_persist::REPLAY_LANE_PREFIX`): everything on this lane
        // rides the ordinary inbound channel but must never be journaled as
        // the CURRENT session's history.
        let lane = format!("{}{id}", crate::session_persist::REPLAY_LANE_PREFIX);
        let meta = AgentMeta::new(lane.clone(), format!("replay — {}", record.title), now_ms())
            .with_role("replay");
        let _ = in_tx.send(Inbound::Register(meta));
        let lane_text = |text: String| Inbound::Event {
            agent: lane.clone(),
            event: AgentEvent::Text { text },
        };
        let Some(store) = agent::open_store(std::path::Path::new(&record.workspace)) else {
            let _ = in_tx.send(lane_text(format!(
                "no store found at {} — nothing to replay",
                record.workspace
            )));
            let _ = in_tx.send(Inbound::Status {
                agent: lane,
                status: AgentStatus::Failed,
            });
            return;
        };
        match store.session_events(&id) {
            Ok(journal) => {
                if journal.events.is_empty() {
                    let _ = in_tx.send(lane_text(
                        "no persisted events for this session (it predates session-linked \
                         journals, store schema v8)"
                            .to_string(),
                    ));
                }
                for rec in journal.events {
                    let _ = in_tx.send(Inbound::Event {
                        agent: lane.clone(),
                        event: rec.event,
                    });
                }
                if journal.skipped > 0 {
                    let _ = in_tx.send(lane_text(format!(
                        "{} event(s) could not be decoded and were skipped",
                        journal.skipped
                    )));
                }
                let _ = in_tx.send(Inbound::Status {
                    agent: lane,
                    status: match record.status {
                        stella_store::SessionStatus::Error => AgentStatus::Failed,
                        _ => AgentStatus::Done,
                    },
                });
            }
            Err(e) => {
                let _ = in_tx.send(lane_text(format!(
                    "failed to read the session journal: {e}"
                )));
                let _ = in_tx.send(Inbound::Status {
                    agent: lane,
                    status: AgentStatus::Failed,
                });
            }
        }
    });
}

/// How often the PR monitor re-reads `gh` (live reconcile, L-V3 — nothing
/// renders from cache; every push is a fresh observation).
const PR_POLL_MS: u64 = 45_000;

/// One reconciled PR observation, as compared for change detection.
#[derive(PartialEq, Clone)]
pub(super) struct PrObservation {
    pub(super) url: String,
    pub(super) number: Option<u64>,
    pub(super) status: PrStatus,
    pub(super) ci: Option<CiStatus>,
}

/// Poll `gh` for the workspace's current-branch PR and its checks. On every
/// change: a `Pr` event on the lead lane (the deck folds it into the
/// footer's PR cell and the transcript), a store mirror row, and — when CI
/// flips to failing — a persist-until-read inbox notification linked to
/// this session. No PR (or no `gh`) is quietly nothing: the cell stays
/// hidden rather than wrong.
pub(super) fn spawn_pr_monitor(
    root: PathBuf,
    session_id: Arc<std::sync::Mutex<String>>,
    store: Option<Arc<Store>>,
    workspace_name: String,
    nudge: Arc<tokio::sync::Notify>,
    in_tx: mpsc::UnboundedSender<Inbound>,
) {
    tokio::spawn(async move {
        let mut last: Option<PrObservation> = None;
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(PR_POLL_MS));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            // The tick paces routine reconciles; a nudge (turn settled,
            // worker ended) skips straight to one.
            tokio::select! {
                _ = tick.tick() => {}
                _ = nudge.notified() => {}
            }
            if in_tx.is_closed() {
                break;
            }
            let Some(observed) = observe_pr(&root).await else {
                continue;
            };
            if last.as_ref() == Some(&observed) {
                continue;
            }
            let ci_flipped_to_failing = observed.ci == Some(CiStatus::Failing)
                && last
                    .as_ref()
                    .is_none_or(|l| l.ci != Some(CiStatus::Failing));
            last = Some(observed.clone());
            // Resolved per observation: an in-deck session switch re-keys
            // which session this PR activity belongs to.
            let session_id = session_id.lock().unwrap_or_else(|p| p.into_inner()).clone();
            let _ = in_tx.send(Inbound::Event {
                agent: LEAD.to_string(),
                event: AgentEvent::Pr {
                    url: observed.url.clone(),
                    status: observed.status,
                    number: observed.number,
                    ci: observed.ci,
                },
            });
            if let Some(store) = &store {
                let _ = store.upsert_pull_request(
                    Some(&session_id),
                    &observed.url,
                    observed.number,
                    pr_status_token(observed.status),
                    observed.ci.map(ci_status_token),
                    now_ms(),
                );
            }
            if ci_flipped_to_failing {
                let number = observed
                    .number
                    .map(|n| format!("#{n}"))
                    .unwrap_or_else(|| observed.url.clone());
                let _ = stella_store::NotificationStore::open_default().push(
                    &stella_store::Notification::new(
                        format!("{workspace_name}: CI failing on PR {number}"),
                        observed.url.clone(),
                        session_id.clone(),
                    )
                    .with_session_id(session_id.clone()),
                );
            }
        }
    });
}

/// How often the deck re-reads the machine-wide notification store.
const NOTIFY_POLL_MS: u64 = 3_000;

/// Poll the machine-wide notification store and push a fresh snapshot when
/// it changes — other sessions produce into the same store, so the badge
/// must not wait for a local action. Exits with the deck (send fails once
/// the inbound channel closes).
pub(super) fn spawn_notification_poller(in_tx: mpsc::UnboundedSender<Inbound>) {
    tokio::spawn(async move {
        let store = stella_store::NotificationStore::open_default();
        let mut fingerprint: Vec<(String, bool)> = Vec::new();
        let mut first = true;
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(NOTIFY_POLL_MS));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if in_tx.is_closed() {
                break;
            }
            let list = store.list();
            let next: Vec<(String, bool)> = list.iter().map(|n| (n.id.clone(), n.read)).collect();
            // The first pass always pushes (the badge must show pre-existing
            // unread messages); afterwards only changes do.
            if first || next != fingerprint {
                first = false;
                fingerprint = next;
                if in_tx.send(notifications_inbound(&store)).is_err() {
                    break;
                }
            }
        }
    });
}
