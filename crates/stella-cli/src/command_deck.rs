//! The Command Deck session — `stella` chat on a TTY.
//!
//! This is the bridge between the real engine stack (provider, tools, budget,
//! store, memory — everything `agent::run_interactive` assembles) and the
//! multi-tab deck in `stella-tui` (`run_deck`): engine `AgentEvent`s are
//! wrapped as `Inbound::Event`s for the deck's fold, and the deck's
//! [`WorkspaceInput`]s drive a lead-agent conversation loop.
//!
//! ## Shape
//!
//! One session = one **lead agent** (`"lead"`) holding one conversation, plus
//! a FIFO prompt queue and a bounded pool of **sub-session workers**
//! (`crate::subsession`). The deck's contract is "input never blocks", and
//! dispatch now honors it too: a prompt submitted while the lead's turn is in
//! flight goes straight to a dedicated worker session (`req:<n>`) instead of
//! waiting the turn out — [`Inbound::PromptStarted`] pops the deck's queue
//! display the moment whichever lane picks it up. `task_assign` spawns task
//! workers (`sub:<task-id>`) the same way, and every worker reports back via
//! its live event lane, an inbox notification, and (for task workers) the
//! board task auto-completing. Prompts queue only past the worker cap, on a
//! dispatch hold, or when they are slash commands (the lead's dispatcher owns
//! those). The fleet layer's per-task control verbs (`Fleet::pause_task` /
//! `resume_task` / `stop_task`, riding `stella_fleet::WorkerControls` through
//! the `FleetWorker` port) are driven by the `stella fleet` dashboard's
//! `[p]`/`[r]`/`[x]` keys (#645); controllable *deck* lanes and fleet-worktree
//! isolation for deck workers remain follow-ups on that seam.
//!
//! ## The two engine seams handled here
//!
//! - **Mid-turn asks** ([`mid_turn_ask`]): the plain REPL reads stdin, which
//!   raw mode owns in deck mode, so both places a tool call parks on a
//!   person get a deck-backed responder instead. An approval rides the
//!   `AskUser` card ([`mid_turn_ask::DeckAskUserIo`]) — emit, wait for the
//!   deck's `AskUserAnswer`, echo the answer back as that card's
//!   `ToolResult`, the documented event-pure path that clears the pending
//!   gate (`stella_tui::model`); an `ask_question` rides the #4220 overlay.
//! - **Cancel** (`Stop` / `UserInput::Cancel`): the engine has no abort input;
//!   cancelling drops the in-flight turn future at its next await point and
//!   truncates the partial turn out of the conversation so the next prompt
//!   starts from the last committed state. Never a mid-await corruption — the
//!   dropped future takes its channel senders with it and the forwarder
//!   drains what was already emitted. The deck's single Esc is the SOFT stop
//!   for step-loop lead turns (the engine ends at the next step boundary,
//!   keeping completed work — `stella_core::SOFT_STOP_REASON`); pipeline
//!   turns and worker lanes cancel immediately (a pipeline is a multi-stage
//!   flow with no single soft-stop continuation). Mid-turn `>` steering,
//!   though, reaches BOTH lead turn shapes — the step-loop engine and the
//!   pipeline's execute engine both drain the steering tap at their step
//!   boundaries. After a cancel the loop pops the next queued prompt as
//!   usual ("interrupt current, run next").
//!   A double-Esc `StopAndHold` is the immediate clean cancel plus
//!   queue discipline: the interrupted prompt returns to the FRONT of the
//!   backlog and dispatch parks until the user's next submission, which
//!   arrives as `EnqueueFront` and runs ahead of it. The pair reaches the
//!   driver as two FIFO messages — the plain `Stop`, then the escalation —
//!   so the first press has always dropped the turn (and would have
//!   forgotten its prompt) before `StopAndHold` is read: [`HoldState`]
//!   retains what that cancel dropped so the second press still has a
//!   prompt to requeue and park.

mod graph_input;
mod skills;
use skills::{deck_slash_commands, handle_skills_input, skills_snapshot};

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use stella_core::ports::ToolExecutor;
use stella_model::provider::Provider;
use stella_protocol::{
    AgentEvent, CompletionMessage, CompletionRequest, QuestionOutcome, TaskItem,
};
use stella_store::Store;
use stella_tools::ToolRegistry;
use stella_tools::registry::approval::ApprovalResponse;
use stella_tui::{
    AgentMeta, AgentScope, AgentStatus, DeckOptions, Inbound, SkillOp, SkillScope, SkillSearchHit,
    SkillsView, SlashCommand, SplashCue, UserInput, WorkspaceInput, run_deck,
};
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::config::Config;
use crate::interactive::SkillRegistry;
use crate::{agent, rules};

mod add_dir;
mod authoring;
mod command_side;
mod driver_support;
mod dropped_turn;
pub(crate) mod forwarder;
mod init_cmd;
mod inspect_service;
mod lead_control;
mod lead_turn;
mod model_cmd;
mod panel_snapshots;
mod pr_observe;
mod profile_cmd;
mod session_clear;
mod session_override;
mod sessions_view;
mod settings_io;
mod settle;
mod slash_commands;
mod slash_pump;
mod steering;
mod task_tap;
mod theme_cmd;
mod worker_control;
use driver_support::{
    handle_supervisor_msg, service_registry_action, spawn_mcp_connect, spawn_notification_poller,
    spawn_pr_monitor,
};
use lead_turn::run_lead_turn;
use panel_snapshots::{engine_config_inbound, tool_policy_inbound};
use pr_observe::{ci_status_token, observe_pr, pr_status_token};
use slash_commands::{DeckCommand, handle_agents_input, run_deck_command};

use crate::memory::{SessionMemory, TurnFriction};
use crate::subsession::{self, SubSessions, SupervisorMsg};
use authoring::{agents_list_creating, handle_agent_create};
pub(crate) use forwarder::{close_turn_stream, spawn_forwarder};
use sessions_view::sessions_inbound;
use settings_io::{apply_pending_reload, handle_engine_config_input, handle_tools_input};

/// Where an Esc-delivered steer lands, driver-side.
mod steer;

/// ISSUES-tab requests, served by the workspace's issue provider.
mod issues;

/// The lead agent's id — the one conversation this driver runs.
pub(crate) const LEAD: &str = "lead";

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// An ephemeral transcript notice for DIRECT deck sends (`deck_tx`), never
/// the journaled `in_tx` path: boot narration, hints, and guidance that must
/// not replay (and pile up) every time the session is resumed.
///
/// It is **marked** because of what riding the transcript costs.
/// [`AgentEvent`] has no system-notice variant, so a chrome message goes out as
/// `Text` — and a `Text` entry renders on the **agent** rail. Unmarked, the
/// transcript is asserting that the model said "conversation cleared". The
/// rail glyph is a *visual* distinction, which makes it exactly the one that
/// does not survive being read aloud (#1258 §6; #1243 fixed the same thing for
/// the surface that has since been unshipped). One `▸` — the same marker the
/// CLI already uses on the normal screen for its own voice — costs a character
/// and removes the lie.
pub(crate) fn chrome_note(text: String) -> Inbound {
    let noted = if text.starts_with(stella_tui::NOTICE_MARKER) {
        text
    } else {
        format!("{}{text}", stella_tui::NOTICE_MARKER)
    };
    Inbound::Event {
        agent: LEAD.to_string(),
        event: AgentEvent::Text { text: noted },
    }
}

/// A **session-startup system notification**: the deck talking about the
/// session itself — a resumable predecessor, what the code-graph pass
/// indexed, an `mcp.toml` that went untrusted. Shown in a transient dialog
/// ([`stella_tui::notice`]); never in the transcript.
///
/// Contrast [`chrome_note`], which fabricates an `AgentEvent::Text` and so
/// reads as though the agent had said it. That is still right for a notice
/// ANSWERING A USER ACTION mid-session (an `mcp` subcommand's result, "cannot
/// resume `{id}`") — a reply belongs where the user was looking. Startup
/// chrome is nobody's reply, and the transcript is the home for agent and
/// user messages only. Corollary for the call sites: the model fold ignores
/// this variant, so unlike `chrome_note` it never folds the lead to
/// `AgentStatus::Running`.
fn system_notice(text: String) -> Inbound {
    Inbound::Notice(text)
}

/// `STELLA_DEBUG=1` → the structured deck log path (L-T8), mirroring the
/// location `stella_tui::DeckOptions` documents. `None` otherwise, and
/// on any failure to create the directory — a lost debug log never gates the
/// session.
///
/// `OXAGEN_DEBUG` is accepted as a deprecated alias for one release: the
/// user-facing env surface is `STELLA_*` everywhere else (88 names), and this
/// was the only runtime toggle stranded in the pre-rename namespace.
fn debug_log_path() -> Option<PathBuf> {
    let requested = std::env::var_os("STELLA_DEBUG").or_else(|| std::env::var_os("OXAGEN_DEBUG"));
    if requested.is_none_or(|v| v.is_empty() || v == "0") {
        return None;
    }
    #[cfg(not(unix))]
    return None;

    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let dir = crate::paths::state_home()?.join("stella").join("logs");
        match std::fs::symlink_metadata(&dir) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return None,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut builder = std::fs::DirBuilder::new();
                builder.recursive(true).mode(0o700);
                builder.create(&dir).ok()?;
            }
            Err(_) => return None,
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).ok()?;
        Some(dir.join(format!("deck-{}.jsonl", std::process::id())))
    }
}

/// How one dispatched turn ended, as seen by the driver loop.
enum TurnEnd {
    /// The turn future resolved (completed or aborted-with-reason).
    Finished(Result<(), crate::failure::CliFailure>),
    /// The user stopped it mid-flight; the future was dropped. `hold` is the
    /// double-Esc variant: the interrupted prompt goes back to the FRONT of
    /// the backlog and dispatch parks until the user's next submission
    /// (which runs ahead of it). A plain cancel (`hold: false`) lets the
    /// loop auto-dispatch the next queued prompt as usual.
    Cancelled { hold: bool },
    /// `/clear` landed mid-turn ([`WorkspaceInput::SessionClear`]): the turn
    /// future was dropped and the loop resets the session to its seq-0 state
    /// — history, backlog, deck pane — retaining nothing.
    Cleared,
    /// The deck is going down; stop driving entirely.
    Quit,
}

/// Driver-side bookkeeping for the deck's Esc pair: single Esc cancels now,
/// double-Esc escalates to "requeue what was interrupted and park dispatch".
///
/// The two presses arrive as two FIFO messages — `AgentControl::Stop`, then
/// `WorkspaceInput::StopAndHold` — and the driver consumes the first by
/// dropping the turn future. Without retention the escalation would always
/// land after its target was already cancelled and forgotten: with an empty
/// backlog it would be a silent no-op (no requeue, no hold — while the deck's
/// own `dispatch_held` flag believes otherwise), and with a backlog it would
/// cancel the freshly auto-dispatched NEXT prompt while the prompt the user
/// actually interrupted stayed lost. So every plain cancel deposits its
/// prompt here, and the escalation requeues it whenever it lands.
struct HoldState {
    /// While set, dispatch is parked: the loop waits for the user's next
    /// submission instead of popping the backlog.
    held: bool,
    /// The prompt the last plain cancel dropped, kept until the pair's
    /// escalation consumes it or the next plain cancel replaces it. Never
    /// stale: every `StopAndHold` the deck can emit is preceded — same pair,
    /// no keys in between — by a `Stop` that overwrites this slot.
    cancelled: Option<String>,
}

impl HoldState {
    fn new() -> Self {
        Self {
            held: false,
            cancelled: None,
        }
    }

    /// Whether dispatch is parked (the loop must not pop the backlog).
    fn held(&self) -> bool {
        self.held
    }

    /// A user submission releases the hold and runs immediately.
    fn release(&mut self) {
        self.held = false;
    }

    /// `/clear`: a reset session holds nothing — neither the park nor a
    /// retained prompt for a later escalation to resurrect.
    fn reset(&mut self) {
        self.held = false;
        self.cancelled = None;
    }

    /// A plain cancel (single Esc / dashboard stop): retain the dropped
    /// prompt so a following escalation can still requeue it.
    fn cancelled(&mut self, submitted: &str) {
        self.cancelled = Some(submitted.to_string());
    }

    /// The double-Esc escalation: park dispatch and return the prompts to
    /// push to the FRONT of the backlog, in push order (front-most last).
    /// `in_flight` is the auto-dispatched prompt this escalation itself
    /// cancelled (if any); it lands BEHIND the retained one so the backlog
    /// reads exactly as the user last saw it. With nothing in flight and
    /// nothing retained there is nothing to hold — a stray escalation stays
    /// a no-op.
    fn stop_and_hold(&mut self, in_flight: Option<&str>) -> Vec<String> {
        let requeue: Vec<String> = in_flight
            .map(str::to_string)
            .into_iter()
            .chain(self.cancelled.take())
            .collect();
        if !requeue.is_empty() {
            self.held = true;
        }
        requeue
    }
}

/// Return cancelled prompts to the FRONT of the backlog (push order:
/// front-most last) and mirror each front-insert into the deck's queue view
/// (`Inbound::PromptRequeued` is the exact inverse of `PromptStarted`'s
/// front-pop), so the two queues never drift.
fn requeue_front(
    queue: &mut crate::session_persist::DurableQueue,
    in_tx: &UnboundedSender<Inbound>,
    texts: Vec<String>,
) {
    for text in texts {
        queue.push_front(text.clone());
        let _ = in_tx.send(Inbound::PromptRequeued {
            agent: LEAD.to_string(),
            text,
        });
    }
}

/// Run a full deck session: the deck shell on its own task, the engine
/// driver inline. Returns when the user quits (Ctrl-C) or the deck's input
/// stream ends.
pub async fn run_deck_session(
    cfg: &mut Config,
    budget_limit: Option<f64>,
    presentation: crate::term_policy::DeckPresentation,
    resume: Option<crate::session_persist::ResumeRequest>,
) -> Result<(), String> {
    let crate::term_policy::DeckPresentation {
        no_anim,
        accessible,
    } = presentation;
    crate::enterprise_telemetry::authorize_execution_surface(
        crate::enterprise_telemetry::ExecutionSurface::Deck,
    )?;
    // `mut`: `/model` swaps the adapter between turns; `Arc` so the swapped-in
    // one is the allocation the sub-agent dispatcher is re-pointed at, too.
    let mut provider: Arc<dyn Provider> = Arc::from(agent::build_provider(cfg)?);
    let registry: Arc<ToolRegistry> = Arc::new(crate::write_dirs::registry_for(cfg));

    // ── Channels: engine → deck (Inbound) and deck → driver (WorkspaceInput)
    // The driver's send side (`in_tx`) reaches the deck through the journal
    // tee — the single choke point that makes the session durable. Direct
    // `deck_tx` sends bypass the journal: replay (which must never
    // re-journal itself) and ephemeral session chrome (boot narration,
    // hints) that would otherwise pile up in the transcript on every resume.
    //
    // Created here, ahead of the registry wiring rather than beside the deck
    // spawn, because the deck's mid-turn ask responders are built over them
    // and `enforce_workspace_rules` below is where a surface declares who
    // answers (#4220). Attaching them later would leave one window in which
    // the registry's answer to "who is driving?" was wrong.
    let (in_tx, raw_rx) = mpsc::unbounded_channel::<Inbound>();
    let (deck_tx, deck_rx) = mpsc::unbounded_channel::<Inbound>();
    let (sub_tx, mut sub_rx) = mpsc::unbounded_channel::<WorkspaceInput>();
    let (ask_tx, ask_rx) = mpsc::unbounded_channel::<String>();
    let (question_tx, question_rx) = mpsc::unbounded_channel::<QuestionOutcome>();
    let (approval_tx, approval_rx) = mpsc::unbounded_channel::<ApprovalResponse>();

    let sub_agents = crate::subagent::install_for_session(cfg, &registry)?;
    // The deck can park a turn on a human, so it declares a surface rather
    // than the headless posture it was stuck with before it had an overlay
    // to park on. Both responders ride the deck's own channels: the
    // approvals half reuses the existing `AskUser` card through
    // [`DeckAskUserIo`], and the question half raises the #4220 overlay.
    // Neither may be the plain-TTY responder — the deck holds the terminal
    // in raw mode, and a blocking stdin read behind its render loop would
    // fight it for every keystroke.
    let (mid_turn_posture, ask_io) = mid_turn_ask::surface(
        LEAD.to_string(),
        in_tx.clone(),
        ask_rx,
        approval_rx,
        question_rx,
    );
    let active_rules = rules::enforce_workspace_rules(
        &registry,
        &cfg.workspace_root,
        &cfg.authority,
        mid_turn_posture,
    );
    let custom_tools = agent::discover_custom_tools(cfg, true).await;
    let mut budget = agent::build_budget_guard(budget_limit);
    let store = agent::open_store(&cfg.workspace_root);
    let calibration = agent::seed_calibration(&store, cfg);
    // The most recent execution this session opened — the INSPECT overlay's
    // subject. `execution` itself is per-turn and out of scope at the idle
    // service site, and the overlay is most useful precisely when no turn is
    // running, so the id is retained here across the whole session loop.
    let mut last_execution_id: Option<i64> = None;

    // The system prompt and seed message are built AFTER `resume_state`
    // resolves below — the persona is chosen from the same state that decides
    // the turn driver, and choosing it blind here gave every deck session the
    // generic REPL persona even though the deck drives the staged pipeline by
    // default. See the build beside `restore_messages`.
    // `warn: false`: past this point diagnostics would land on the alternate
    // screen; a memory-less session degrades silently here.
    let mut memory =
        SessionMemory::open_for_session(&cfg.workspace_root, false, &cfg.authority, &active_rules);
    // Custom extensions: ⚡ commands/skills in the slash menu, custom agents
    // behind `/agents`. Reloaded after `/init`, which may adopt new ones.
    let mut custom = crate::extensions::CustomExtensions::load_with_authority(
        &cfg.workspace_root,
        &cfg.authority,
    );
    // The npx skills registry (search/install), constructed once for the whole
    // session — the SKILLS tab's ops route through it (see `handle_skills_input`).
    let skill_registry = SkillRegistry::from_env(cfg.workspace_root.clone());

    // ── Durable session identity (still on the normal screen) ──────────────
    // This session announces itself in the machine-wide registry, and every
    // fold-relevant envelope it produces is journaled to the record's sidecar
    // (`session_persist`) — quit / crash / power cut, the session reopens
    // where it stood. A resume request resolves HERE so its errors print on
    // the normal screen instead of dying behind the alternate one.
    let session_registry = stella_store::SessionRegistry::open_default();
    let _ = session_registry.prune(SESSION_RECORD_MAX_AGE_MS);
    let _ = stella_store::NotificationStore::open_default().prune(NOTIFICATION_MAX_AGE_MS);
    let workspace_path = cfg.workspace_root.display().to_string();
    let workspace_name = cfg
        .workspace_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| workspace_path.clone());
    let mut resume_state = match &resume {
        Some(request) => {
            let target = crate::session_persist::resolve_resume_target(
                &session_registry,
                &workspace_path,
                request,
            )?;
            Some(crate::session_persist::load_resume(
                &session_registry,
                &target.id,
                &workspace_path,
            )?)
        }
        None => None,
    };
    let mut session_record = match &mut resume_state {
        // Re-own the stored record: same id (the registry never forks a
        // resumed session's identity), this process's pid, back to waiting.
        Some(rs) => crate::session_persist::adopt_record(
            rs.record.clone(),
            stella_store::SessionStatus::NeedsInput,
        ),
        None => stella_store::SessionRecord::new(workspace_path.clone(), workspace_name.clone()),
    };
    let _ = session_registry.upsert(&session_record);
    // What the record's terminal status will be at exit (last turn wins);
    // quitting with a pending backlog overrides to Paused below — the work
    // is durable now, so an exit with prompts waiting is a pause, not loss.
    let mut session_exit = stella_store::SessionStatus::Complete;
    let mut sidecar_dir = session_registry.sidecar_dir(&session_record.id);
    // Persona matches the driver: a genuinely fresh session gets the plain REPL
    // persona (raw is the default since #3381); one resumed with prior journal
    // history but no explicit `Pipeline` record restores the pipeline persona
    // instead — it predates the flip and always ran staged, so swapping
    // personas on a mere resume would breach invariant #7. Chosen ONCE, byte-
    // stable (L-E8): see `session_persist::initial_pipeline_persona`.
    let pipeline_persona = crate::session_persist::initial_pipeline_persona(resume_state.as_ref());
    let bare_prompt = if pipeline_persona {
        // Assembled once per session, before any turn resolves wiring: no
        // model line rather than a possibly-false one (#2721).
        agent::build_pipeline_system_prompt(cfg, &cfg.workspace_root, &active_rules, None)
    } else {
        agent::build_system_prompt(cfg, &cfg.workspace_root, &active_rules)
    };
    let mut system_prompt = agent::with_session_hook_context(bare_prompt.clone(), cfg).await;
    // What the SessionStart hooks appended, captured once: a session model
    // switch rebuilds the prompt around the NEW model line and re-appends
    // this verbatim — hooks run once per session, not once per switch.
    let hook_context_suffix = system_prompt[bare_prompt.len()..].to_string();
    // The persona-free prompt an assumed agent's block is appended to
    // (`WorkspaceInput::AgentAssume`), so assuming twice never stacks.
    let mut base_system_prompt = system_prompt.clone();
    // The assumed persona itself, kept so a model switch can re-append it to
    // the rebuilt base — and the session's own tool policy, the floor every
    // assumed agent's `tools:` grant narrows from (never the previous
    // agent's, so scopes swap rather than compound).
    let mut assumed_persona: Option<String> = None;
    let base_tool_policy = cfg.tool_policy.clone();
    let mut messages = vec![CompletionMessage::system(system_prompt.clone())];
    if let Some(rs) = &mut resume_state {
        messages = crate::session_persist::restore_messages(
            std::mem::take(&mut rs.history).unwrap_or_default(),
            &system_prompt,
        );
        // `--spend-limit` means THIS session on every resume path: the guard's
        // session accumulator reseeds to exactly what the session had
        // already spent (its journal's last `BudgetTick`), so spend stays
        // monotone across interruptions. Same seam as the in-deck session
        // switch (`SessionResume` in the driver loop below).
        budget.reseed_session_spend(rs.spent_usd.unwrap_or(0.0));
    }

    // The supervisor channel: `task_assign` spawn requests (tap → driver)
    // and sub-session endings (worker → driver). See `crate::subsession`.
    let (sup_tx, mut sup_rx) = mpsc::unbounded_channel::<SupervisorMsg>();
    let journal_sink = crate::session_persist::SessionSink::shared(
        match stella_store::journal::SessionJournal::open(&sidecar_dir) {
            Ok(j) => Some(j),
            Err(e) => {
                let _ = deck_tx.send(system_notice(format!(
                    "session journaling unavailable — this session will not be resumable ({e})"
                )));
                None
            }
        },
    );
    // The other two halves of the same promise, bound beside the journal
    // because all three name the session and all three must name the SAME one:
    // every turn from here checkpoints into this record's sidecar, and every
    // file the agent touches commits to this session's ref in stella's own git
    // store. Re-bound on an in-deck session switch, below.
    if let Some(warning) =
        crate::durability::bind_session(&cfg.durability, &cfg.workspace_root, &session_record.id)
    {
        let _ = deck_tx.send(system_notice(warning));
    }
    // Which of the two durable stores this session's conversation comes back
    // from, decided HERE because the fresher candidate — the resume point an
    // interrupted turn left mid-flight — lives in the record the bind above
    // opens. `restore_conversation` owns the rule and the fallback; this owns
    // only when it can be asked.
    //
    // The note is held rather than sent: the journal replay below is what puts
    // the restored transcript on screen, and a line explaining that transcript
    // belongs after it rather than ahead of it.
    let mut resume_note: Option<String> = None;
    if let Some(rs) = &mut resume_state {
        let restored = crate::session_persist::restore_conversation(
            cfg.durability.checkpoint().as_deref(),
            std::mem::take(&mut rs.history),
            &system_prompt,
        );
        messages = restored.messages;
        resume_note = Some(restored.note);
        // `--spend-limit` means THIS session on every resume path: the guard's
        // session accumulator reseeds to exactly what the session had
        // already spent, so spend stays monotone across interruptions. Same
        // seam as the in-deck session switch (`SessionResume` in the driver
        // loop below).
        //
        // Two meters can answer that, and the larger wins. The journal's last
        // `BudgetTick` and the checkpoint's step-boundary snapshot are written
        // at different moments of the same turn, so either can be the later
        // one — and spend only ever goes up, so taking the max cannot
        // over-count, while taking the wrong one silently hands a resumed
        // session budget it already spent.
        budget.reseed_session_spend(
            rs.spent_usd
                .unwrap_or(0.0)
                .max(restored.spent_usd.unwrap_or(0.0)),
        );
    }
    // A release-build panic aborts before any `Drop` or `catch_unwind` runs
    // (the workers' panic catch included), so this hook is the journal's
    // only flush point on that path — the terminal is restored by
    // stella-tui's own hook the same way.
    let _journal_panic_guard =
        crate::session_persist::JournalPanicGuard::install(journal_sink.clone());
    let _tee = crate::session_persist::spawn_journal_tee(
        raw_rx,
        deck_tx.clone(),
        journal_sink.clone(),
        LEAD,
    );
    // Replay a resumed session's journal straight onto the deck BEFORE the
    // first live send, so the restored transcript precedes everything this
    // run adds. (The fresh `Register` below then restamps the lead's meta —
    // pid, model, clock — over the replayed one.) The non-lead lanes the
    // replay puts on the dashboard are remembered so an in-deck session
    // switch can deregister them — rows of a session left behind must not
    // linger on the next session's dashboard.
    let mut replayed_lanes: Vec<String> = Vec::new();
    if let Some(rs) = &mut resume_state {
        replayed_lanes = crate::session_persist::journal_lanes(&rs.records, LEAD);
        crate::session_persist::replay_session(
            std::mem::take(&mut rs.records),
            now_ms(),
            LEAD,
            &deck_tx,
        );
    }

    let title = cfg
        .workspace_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace")
        .to_string();
    let mut lead_meta = AgentMeta::new(LEAD, title, now_ms())
        .with_role("lead")
        .with_pid(std::process::id());
    lead_meta.model = Some(format!("{}/{}", cfg.provider.id, cfg.model_id));
    // A clone, not a move: a session model switch re-registers the lead with
    // its new model (`session_override`), so the meta stays owned here.
    let _ = in_tx.send(Inbound::Register(lead_meta.clone()));
    // Name all three pipeline pins before the first turn: without this the
    // statline's MODEL cell can name nothing until a role has already served.
    let pins = model_cmd::configured_role_pins(cfg);
    let _ = in_tx.send(Inbound::ConfiguredRoles(pins));
    // The model vocabulary the SETTINGS tab, the `/model` picker and the
    // `/model` argument menu all read, before the first keystroke — without
    // it each of those surfaces opens empty and has to ask.
    let _ = in_tx.send(engine_config_inbound(cfg, None));
    // Custom definitions that failed to load are reported in the startup
    // dialog — stdout belongs to the alternate screen, and a
    // silently-missing /command is otherwise undiagnosable. Session chrome:
    // re-checked every boot, so it never journals.
    if let Some(report) = custom.problems_report() {
        let _ = deck_tx.send(system_notice(report));
    }
    // Honest degradation: every silently-dropped or silently-defaulted
    // setting the boot can name gets one line instead. Session chrome —
    // re-checked every boot, never journaled. See `engine_config::boot_notices`
    // for the roster and why each one earns its line.
    for notice in crate::engine_config::boot_notices(cfg) {
        let _ = deck_tx.send(system_notice(notice));
    }
    steering::announce_withheld(cfg, &in_tx);
    // An idle lead is waiting on the human, not queued behind a supervisor —
    // asserted outright, since the startup chrome above no longer folds it to
    // `Running` (see `system_notice`).
    let _ = in_tx.send(Inbound::Status {
        agent: LEAD.to_string(),
        status: AgentStatus::WaitingInput,
    });

    // ── The durable prompt backlog ──────────────────────────────────────────
    // Every mutation writes through to the sidecar, so a queued prompt
    // survives any interruption from the moment it is queued. On resume the
    // restored backlog (and the prompt an interruption cut short, back at
    // the FRONT) is mirrored into the deck's queue view, and dispatch parks
    // until the user's next submission — resuming shows where things stood
    // and costs nothing until the user says go.
    let mut queue = crate::session_persist::DurableQueue::fresh(sidecar_dir.clone());
    let mut resume_hold = false;
    if let Some(rs) = &mut resume_state {
        // Which granularity the transcript above came back at, now that it is
        // on screen for the line to be about.
        if let Some(note) = resume_note.take() {
            let _ = deck_tx.send(system_notice(note));
        }
        // Interrupted prompts (any lane's unsettled dispatch) go back at the
        // FRONT, ahead of the stored backlog, in their original order.
        let mut restored = std::mem::take(&mut rs.interrupted);
        restored.extend(std::mem::take(&mut rs.queue));
        if !restored.is_empty() {
            resume_hold = true;
            // Front-inserts mirror back-to-front so the view reads in order.
            for text in restored.iter().rev() {
                let _ = in_tx.send(Inbound::PromptRequeued {
                    agent: LEAD.to_string(),
                    text: text.clone(),
                });
            }
            let _ = deck_tx.send(system_notice(format!(
                "{} prompt(s) waiting, dispatch held. Submit anything to run it first (then \
                 the backlog), or ctrl+t to edit the queue.",
                restored.len()
            )));
            queue.adopt(sidecar_dir.clone(), restored);
        }
    } else if session_registry.latest_resumable(&workspace_path).is_some() {
        // A fresh session in a workspace that has something to go back to:
        // one pointer, so "navigate back in" is discoverable.
        let _ = deck_tx.send(system_notice(
            "◂ a previous session is resumable — ctrl-e opens SESSIONS, ⏎ reopens one; \
             or run `stella resume`."
                .to_string(),
        ));
    }
    // Seed the SKILLS tab so it has data the instant it is opened (both scopes),
    // without waiting on a `/skills` round-trip.
    let _ = in_tx.send(skills_snapshot(&cfg.workspace_root, None));
    // Seed the ENGINE panel the same way: the merged
    // agent_engine_config plus the picker vocabularies, ready before the
    // user first opens it.
    let _ = in_tx.send(engine_config_inbound(cfg, None));
    // …and the TOOLS panel beside it. MCP servers are still connecting at this
    // point, so this first list is the native + custom surface; opening the
    // panel (or `r`) re-enumerates and picks up every connected server.
    let _ = in_tx.send(tool_policy_inbound(
        cfg,
        &crate::tool_switches::session_tool_names(&*registry, &custom_tools),
        None,
    ));

    // Honour the persisted colour theme (`ui.theme`) before the deck spawns its
    // render task, so the very first frame — the launch cinematic — is already
    // in the chosen theme. Best-effort: an unset/unknown value keeps the
    // default (`stella-dark`).
    theme_cmd::apply_persisted(cfg);

    let opts = DeckOptions {
        debug_log_path: debug_log_path(),
        slash_commands: deck_slash_commands(&custom),
        initial_graph: agent::graph_snapshot(&cfg.workspace_root),
        no_anim,
        accessible,
        mid_turn_prompt: steer::mid_turn_prompt_policy(cfg),
        ..Default::default()
    };
    // The deck owns its channel ends and runs on its own task so rendering
    // never waits on the driver (and vice versa).
    let deck = tokio::spawn(run_deck(opts, deck_rx, sub_tx));

    // The launch cinematic: hold the splash's battle loop open over session
    // init and release it once BOTH async legs — the background code-graph
    // build below and the MCP connect after it — have finished, so the movie
    // covers however long a first launch's indexing takes instead of handing
    // off to a deck that is still visibly assembling itself. Any key still
    // skips; `--no-anim` sessions ignore the cue entirely.
    let _ = in_tx.send(Inbound::Splash(SplashCue::Replay));
    let init_pending = Arc::new(std::sync::atomic::AtomicUsize::new(2));
    let release_splash = {
        let tx = in_tx.clone();
        move || {
            if init_pending.fetch_sub(1, Ordering::SeqCst) == 1 {
                let _ = tx.send(Inbound::Splash(SplashCue::Release));
            }
        }
    };
    let release_on_graph_ready = release_splash.clone();

    // Auto-build the code-graph index in the background (a cheap incremental
    // refresh if it already exists) and keep it fresh via the live watcher, so
    // `stella search` answers from it — and the Graph tab populates —
    // without a manual `stella init`. Spawned AFTER the deck is up so its
    // `◈ indexing…`/`✓ …` lines render as transcript events; non-blocking, and
    // the watcher stops when `_session_graph` drops at session end. `_graph_build`
    // (the setup task's JoinHandle) is detached — freshness outlives it.
    // Indexing narration is session chrome (direct `deck_tx`): it re-runs at
    // every boot, so journaling it would replay stale "indexing…" lines on
    // top of every resumed transcript.
    let status_tx = deck_tx.clone();
    let ready_tx = deck_tx.clone();
    let ready_root = cfg.workspace_root.clone();
    let (_session_graph, _graph_build) = agent::spawn_session_graph(
        &cfg.workspace_root,
        Box::new(agent::deck_notice_narrator(status_tx, LEAD)),
        Box::new(move || {
            // Populate the Graph tab now the index exists (it opened on the
            // "run stella init" hint), and assert the lead is idle — the
            // index progress above no longer folds it to `Running`.
            if let Some(snapshot) = agent::graph_snapshot(&ready_root) {
                let _ = ready_tx.send(Inbound::GraphSnapshot(snapshot));
            }
            let _ = ready_tx.send(Inbound::Status {
                agent: LEAD.to_string(),
                status: AgentStatus::WaitingInput,
            });
            // One of the two init legs the launch splash waits on.
            release_on_graph_ready();
        }),
        Box::new(agent::deck_readiness_reporter(deck_tx.clone())),
    );

    // ── MCP connect, OFF the first prompt's critical path (#98 continued) ──
    // The connect used to run inline here: the deck was live, but the driver
    // loop — and therefore the FIRST prompt's dispatch — waited up to 10s
    // per server. It now runs on its own task and lands the connected set in
    // `mcp_slot`; every turn resolves its tool executor from the slot at
    // dispatch, so servers join the session the moment they connect and the
    // first prompt starts immediately (on native tools when connect is still
    // running — narrated once, never silent). Prompts are never lost either
    // way: the deck's input never blocks and `sub_rx` buffers.
    // Session-scoped MCP management state, shared with the MCP tab:
    //   • `mcp_disabled` — server names disabled this session; toggling it
    //     hides a server's tools from the model on the next call (live, no
    //     reconnect), because the engine re-reads schemas each call.
    //   • the usage ledger (from the registry) records every MCP call for the
    //     `mcp_usage` telemetry table.
    let mcp_disabled: stella_mcp::DisabledServers =
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
    // `Arc<McpToolSet>` (not a bare `McpToolSet`) so a turn can cheaply clone
    // the connected set into the Best-of-N candidate surface + orchestrator
    // pre-fetch (issue #248 Phase 1) alongside its own `&dyn ToolExecutor`.
    let mcp_slot: Arc<tokio::sync::OnceCell<Arc<stella_mcp::McpToolSet>>> =
        Arc::new(tokio::sync::OnceCell::new());
    let mcp_configured = spawn_mcp_connect(
        cfg.clone(),
        registry.clone(),
        mcp_disabled.clone(),
        mcp_slot.clone(),
        in_tx.clone(),
        deck_tx.clone(),
        release_splash.clone(),
    );
    // Whether the "still connecting" note has been narrated (once, on the
    // first turn that dispatches before the slot fills).
    let mut mcp_pending_noted = false;

    // The registry record and hygiene ran during assembly (the durable
    // session identity block). Claim-on-first-write identity for the lead's
    // turns, and crash hygiene for the whole workspace: sweep claims old
    // enough that their process is surely gone (a crashed writer cannot
    // release its own). The holder is remade whenever the deck navigates to
    // another session (`SessionResume` below) — claims must name the session
    // actually doing the writing.
    let mut lead_holder = format!("{}/lead", session_record.id);
    if let Some(store) = &store {
        let _ = store.prune_stale_file_locks(crate::claims::STALE_CLAIM_MAX_AGE_SECS);
    }
    // The inbox poller keeps the badge live as other sessions produce
    // persist-until-read notifications.
    spawn_notification_poller(in_tx.clone());

    // Ingest staleness sweep: compare every alert-active ingested source file
    // against the hash it was ingested at, and drop a deduped inbox item per
    // drift (#2683). Its own blocking thread, not a deck task: the work is
    // synchronous filesystem reads plus a `git hash-object` per lineage, it
    // must never delay the splash gate or a turn, and the poller above picks
    // up whatever it pushes. Best-effort — the handle is dropped on purpose.
    {
        let sweep_root = cfg.workspace_root.clone();
        std::thread::spawn(move || {
            let _ = crate::ingest_cmd::lineage::surface_stale_sources(&sweep_root);
        });
    }

    // ── The driver loop ─────────────────────────────────────────────────────
    // (`queue` — the durable backlog — was constructed with the session
    // identity above, restored contents and all.)
    // Double-Esc bookkeeping: parks dispatch and retains what the pair's
    // first press cancelled (see [`HoldState`]). A resumed backlog starts
    // parked — reopening a session shows where it stood; the user's next
    // submission is what sets it moving (and runs first).
    let mut dispatch = HoldState::new();
    dispatch.held = resume_hold;
    // An agent-creation request that arrived mid-turn: drafting needs the
    // provider (borrowed by the running turn), so it parks here and runs
    // right after the turn settles.
    let mut pending_create: Option<(String, AgentScope)> = None;
    // A `/budget` cap change that arrived mid-turn: the running turn holds
    // the guard, so the retarget waits for the settle boundary (invariant
    // #6 — budget changes act between steps/turns, never mid-flight).
    let mut pending_budget: Option<Option<f64>> = None;
    // A SETTINGS save landed mid-turn: the files changed, but the running
    // turn holds `&Config` and is reading the fields a reload rewrites, so
    // the re-derive waits for the same safe boundary `pending_budget` uses.
    let mut pending_settings_reload = false;
    // Sub-session bookkeeping: live-worker slots, and `task_assign` requests
    // waiting for one (drained oldest-first as workers end).
    let mut subs = SubSessions::new();
    let mut pending_spawns: VecDeque<subsession::QueuedSpawn> = VecDeque::new();
    // Lanes whose Restart arrived while the worker was still live: stop
    // first, respawn on its Ended.
    let mut pending_controls = worker_control::Pending::default();
    // Worker spend not yet metered into the session budget guard — applied
    // at the loop top, where the guard is free (budget aborts happen at
    // safe boundaries only).
    let mut unmetered_spend: f64 = 0.0;
    // Inputs that reached the driver during a turn's post-turn bookkeeping
    // (see [`settle::run_while_listening`]) that that window could not
    // service itself — a
    // worker control, a SKILLS-tab op. They run at the idle arm below, ahead
    // of the backlog, in arrival order.
    let mut deferred: VecDeque<WorkspaceInput> = VecDeque::new();
    // PR/CI reconcile: polls `gh` for the workspace's current-branch PR and
    // its checks, feeding the footer's PR cell, the store mirror, and
    // failing-CI inbox notifications. The nudge skips the wait after turns
    // and worker endings — the moments a PR most plausibly just changed.
    let pr_nudge = Arc::new(tokio::sync::Notify::new());
    // The monitor attributes PR rows and CI notifications to a session id;
    // shared + mutable because an in-deck `SessionResume` re-keys it to the
    // adopted session (the monitor follows the deck, not the process's
    // first session).
    let pr_session_id = Arc::new(std::sync::Mutex::new(session_record.id.clone()));
    spawn_pr_monitor(
        cfg.workspace_root.clone(),
        pr_session_id.clone(),
        store.clone(),
        workspace_name.clone(),
        pr_nudge.clone(),
        in_tx.clone(),
    );
    'session: loop {
        // Meter accumulated worker spend into the session guard at this
        // safe boundary — the engine's own budget checks then see the true
        // session total on the next turn.
        if unmetered_spend > 0.0 {
            let _ = budget.record_spend(unmetered_spend);
            unmetered_spend = 0.0;
        }
        // Take the next prompt: backlog first (unless held), else wait for
        // deck input.
        //
        // A pending `deferred` input pre-empts the backlog. It arrived FIRST —
        // during the previous turn's bookkeeping — and it is the kind of input
        // that cannot wait behind a prompt: a `Stop` aimed at a worker must not
        // sit out the whole turn a queued prompt is about to start. Each one
        // falls through the idle-arm match below, which `continue`s, so the
        // loop drains them in order and only then pops the backlog.
        let next = if dispatch.held() || !deferred.is_empty() {
            None
        } else {
            queue.pop_front()
        };
        // Between prompts the driver waits on BOTH channels: deck input and
        // the supervisor (a sub-session ending or a stray spawn request must
        // not wait for the user's next keystroke to be serviced).
        enum IdleWake {
            Input(Option<WorkspaceInput>),
            Sup(Option<SupervisorMsg>),
        }
        let prompt = match next {
            Some(text) => text,
            None => {
                let wake = match deferred.pop_front() {
                    // Serviced before the channel is read, so a deferred input
                    // keeps its place ahead of anything typed since.
                    Some(input) => IdleWake::Input(Some(input)),
                    None => tokio::select! {
                        input = sub_rx.recv() => IdleWake::Input(input),
                        msg = sup_rx.recv() => IdleWake::Sup(msg),
                    },
                };
                let input = match wake {
                    // The driver holds a live `sup_tx`, so `None` cannot
                    // occur; treat it as a spurious wake regardless.
                    IdleWake::Sup(None) => continue 'session,
                    IdleWake::Sup(Some(msg)) => {
                        handle_supervisor_msg(
                            msg,
                            &mut subs,
                            &mut pending_controls,
                            &mut pending_spawns,
                            &mut queue,
                            dispatch.held(),
                            &registry,
                            &store,
                            &session_record.id,
                            &workspace_name,
                            cfg,
                            budget_limit,
                            &mut unmetered_spend,
                            &pr_nudge,
                            &in_tx,
                            &sup_tx,
                        );
                        continue 'session;
                    }
                    IdleWake::Input(input) => input,
                };
                match input {
                    None => break 'session,
                    Some(WorkspaceInput::Quit) => break 'session,
                    // Worker controls work between lead turns too — the
                    // lead being idle says nothing about a running worker.
                    Some(WorkspaceInput::Control { agent, control }) if agent != LEAD => {
                        worker_control::service(
                            &agent,
                            control,
                            &mut subs,
                            &mut pending_controls,
                            cfg,
                            budget_limit,
                            &session_record.id,
                            &workspace_name,
                            &in_tx,
                            &sup_tx,
                        );
                        continue 'session;
                    }
                    // Any submission releases a hold and runs NOW — ahead of the
                    // parked backlog. `EnqueueFront` is the deck's explicit
                    // front-insert (sent while it knows dispatch is held); a
                    // plain `Enqueue` and an `EnqueueNext` behave identically
                    // here because running the text immediately IS the front.
                    Some(WorkspaceInput::Enqueue { text })
                    | Some(WorkspaceInput::EnqueueFront { text })
                    | Some(WorkspaceInput::EnqueueNext { text })
                    | Some(WorkspaceInput::ToAgent {
                        input: UserInput::Prompt { text, .. },
                        ..
                    }) => {
                        dispatch.release();
                        text
                    }
                    // A queue-free command (`command_side`): runs NOW, and
                    // an argument-carrying text it declines becomes the next
                    // prompt — the route it had before the flag existed.
                    Some(WorkspaceInput::Command { text }) => {
                        if command_side::run(text.trim(), cfg, &in_tx, &session_record.id) {
                            continue 'session;
                        }
                        dispatch.release();
                        text
                    }
                    // The agents page's new-task prompt: a lane, never the
                    // lead's next turn — that is this message's whole meaning.
                    Some(WorkspaceInput::SpawnLane { text }) => {
                        subsession::spawn_lane_or_notice(
                            text,
                            &mut subs,
                            cfg,
                            budget_limit,
                            &session_record.id,
                            &workspace_name,
                            &in_tx,
                            &sup_tx,
                        );
                        continue 'session;
                    }
                    // A steer at a worker, or one whose lead turn ended
                    // before this recv read it — see `steer::steer_idle`.
                    Some(WorkspaceInput::Steer { agent, texts }) if !texts.is_empty() => {
                        match steer::steer_idle(&agent, &subs, &mut queue, texts, &in_tx) {
                            Some(first) => {
                                dispatch.release();
                                first
                            }
                            None => continue 'session,
                        }
                    }
                    // While a hold parks a non-empty backlog at this recv, the
                    // user can still edit it from the queue popup — mirror the
                    // edits exactly like the in-turn arm does. (Before holds
                    // existed the queue was always empty by the time this recv
                    // ran, so these inputs had nothing to act on here.)
                    Some(WorkspaceInput::QueueRemove { index }) => {
                        if index < queue.len() {
                            queue.remove(index);
                        }
                        continue 'session;
                    }
                    Some(WorkspaceInput::QueueClear) => {
                        queue.clear();
                        continue 'session;
                    }
                    // `/clear` between turns: reset NOW — the deck never
                    // queues it, and the backlog goes with it (a session
                    // reset to seq-0 has nothing pending by definition).
                    Some(WorkspaceInput::SessionClear) => {
                        dispatch.reset();
                        queue.clear();
                        session_clear::reset_lead(
                            &mut messages,
                            &system_prompt,
                            &sidecar_dir,
                            &mut subs,
                            registry.as_ref(),
                            store.as_deref().zip(Some(session_record.id.as_str())),
                            &in_tx,
                        );
                        continue 'session;
                    }
                    // The double-Esc escalation, landing AFTER its pair's plain
                    // `Stop` already dropped the turn — with an empty backlog
                    // this recv is exactly where it lands (the channel is FIFO,
                    // so the escalation can never reach the turn the pair
                    // targeted). Requeue what that cancel dropped and park
                    // dispatch; with nothing retained there is nothing to hold
                    // and a stray escalation stays a no-op.
                    Some(WorkspaceInput::StopAndHold { .. }) => {
                        requeue_front(&mut queue, &in_tx, dispatch.stop_and_hold(None));
                        continue 'session;
                    }
                    // The Graph tab asked to re-root — on a file (the picker)
                    // or on a query (the `q` box). The loop is idle here, so
                    // the read runs inline.
                    Some(
                        input @ (WorkspaceInput::FocusGraphFile { .. }
                        | WorkspaceInput::GraphQuery { .. }),
                    ) => {
                        graph_input::answer(input, &cfg.workspace_root, &in_tx);
                        continue 'session;
                    }
                    // SKILLS-tab ops work whether or not a turn is running — handled
                    // at both recv sites so the manager is live mid-turn too.
                    Some(WorkspaceInput::Skill(op)) => {
                        handle_skills_input(
                            &op,
                            cfg,
                            &in_tx,
                            &skill_registry,
                            agent::remaining_budget(&budget),
                        );
                        continue 'session;
                    }
                    // LLM-assisted agent creation needs the provider, which is
                    // free here (no turn in flight) — draft, install, refresh.
                    // The lead assumes an installed agent's identity: the system
                    // prompt grows the agent's persona block and the seeded
                    // system message follows it, so the next turn runs as that
                    // agent. Between turns only — the prompt is byte-stable
                    // across a turn (invariant #7).
                    Some(WorkspaceInput::AgentAssume { name, scope }) => {
                        session_override::assume_agent(
                            &name,
                            scope,
                            cfg,
                            &mut provider,
                            &sub_agents,
                            session_override::PromptPlane {
                                base_system_prompt: &mut base_system_prompt,
                                system_prompt: &mut system_prompt,
                                messages: &mut messages,
                            },
                            &hook_context_suffix,
                            &mut assumed_persona,
                            pipeline_persona,
                            &active_rules,
                            &base_tool_policy,
                            &mut lead_meta,
                            &in_tx,
                        );
                        continue 'session;
                    }
                    // `/model` (picked or typed): swap this session's model —
                    // between turns only, like AgentAssume above.
                    Some(WorkspaceInput::ModelOverride { spec }) => {
                        session_override::apply_model_override(
                            &spec,
                            cfg,
                            &mut provider,
                            &sub_agents,
                            session_override::PromptPlane {
                                base_system_prompt: &mut base_system_prompt,
                                system_prompt: &mut system_prompt,
                                messages: &mut messages,
                            },
                            &hook_context_suffix,
                            assumed_persona.as_deref(),
                            pipeline_persona,
                            &active_rules,
                            &mut lead_meta,
                            &in_tx,
                        );
                        continue 'session;
                    }
                    Some(WorkspaceInput::AgentCreate { description, scope }) => {
                        handle_agent_create(
                            &description,
                            scope,
                            cfg,
                            &*provider,
                            agent::remaining_budget(&budget),
                            &in_tx,
                        )
                        .await;
                        continue 'session;
                    }
                    // ⏎ on a resumable row in the SESSIONS overlay: navigate into
                    // that session. Only serviced HERE, between turns and with no
                    // live workers — running work is never torn down by a
                    // navigation (the mid-turn arm answers with guidance
                    // instead, and live sub-sessions stream into THIS session's
                    // lanes and settle against its records). The current
                    // session's durable state is already on disk, so switching
                    // away loses nothing.
                    Some(
                        nav @ (WorkspaceInput::SessionResume { .. } | WorkspaceInput::SessionNew),
                    ) => {
                        let id = match &nav {
                            WorkspaceInput::SessionResume { id } => id.clone(),
                            _ => "new".to_string(),
                        };
                        let loaded = if id == session_record.id {
                            Err("that is this session — you are already in it".to_string())
                        } else if subs.live() > 0 {
                            Err(format!(
                                "{} worker(s) are still running — stop them (s on the lane) \
                                 or wait for them to finish, then press ⏎ on the session \
                                 again",
                                subs.live()
                            ))
                        } else if matches!(nav, WorkspaceInput::SessionNew) {
                            Ok(crate::session_persist::fresh_state(
                                &workspace_path,
                                &workspace_name,
                            ))
                        } else {
                            crate::session_persist::load_resume(
                                &session_registry,
                                &id,
                                &workspace_path,
                            )
                        };
                        match loaded {
                            Err(reason) => {
                                let _ = deck_tx
                                    .send(chrome_note(format!("cannot resume `{id}`: {reason}")));
                            }
                            Ok(mut rs) => {
                                // Park the CURRENT session: sync the journal,
                                // snapshot the conversation, and either mark it
                                // Paused — or, if nothing ever happened in it,
                                // remove the empty shell instead of littering
                                // the registry with it.
                                journal_sink
                                    .lock()
                                    .unwrap_or_else(|p| p.into_inner())
                                    .sync();
                                let _ = crate::session_persist::snapshot_history(
                                    &sidecar_dir,
                                    &messages,
                                );
                                if session_record.summary.is_empty() && queue.is_empty() {
                                    let _ = session_registry.remove(&session_record.id);
                                } else {
                                    session_record.status = stella_store::SessionStatus::Paused;
                                    let _ = session_registry.upsert(&session_record);
                                }

                                // Clear the departing session's worker rows
                                // off the dashboard before the target's
                                // replay claims it: every non-lead lane is
                                // terminal here (the switch refuses while
                                // workers are live), so each one — spawned
                                // this tenancy or replayed at the last
                                // adoption — gets a `Deregister`. Direct
                                // sends (deck_tx): the removal is part of
                                // THIS process's dashboard handover and is
                                // never journaled, so resuming the departing
                                // session later still shows its worker rows.
                                let mut stale_lanes = subs.lanes();
                                stale_lanes.append(&mut replayed_lanes);
                                stale_lanes.sort();
                                stale_lanes.dedup();
                                for lane in stale_lanes {
                                    let _ = deck_tx.send(Inbound::Deregister { agent: lane });
                                }

                                // Adopt the target: same id, this pid, waiting.
                                // Re-key everything that names the session —
                                // the lead's claim holder and the PR monitor's
                                // store/notification attribution follow the
                                // deck, not the process's first session.
                                session_record = crate::session_persist::adopt_record(
                                    rs.record.clone(),
                                    stella_store::SessionStatus::NeedsInput,
                                );
                                let _ = session_registry.upsert(&session_record);
                                sidecar_dir = session_registry.sidecar_dir(&session_record.id);
                                lead_holder = format!("{}/lead", session_record.id);
                                *pr_session_id.lock().unwrap_or_else(|p| p.into_inner()) =
                                    session_record.id.clone();
                                {
                                    let mut sink =
                                        journal_sink.lock().unwrap_or_else(|p| p.into_inner());
                                    match stella_store::journal::SessionJournal::open(&sidecar_dir)
                                    {
                                        Ok(j) => sink.swap(Some(j)),
                                        Err(e) => {
                                            sink.swap(None);
                                            let _ = deck_tx.send(chrome_note(format!(
                                                "session journaling unavailable — this session \
                                                 will no longer be resumable ({e})"
                                            )));
                                        }
                                    }
                                }
                                // Durability re-keys with everything else. The
                                // next turn's engine reads the sink afresh, so
                                // the checkpoint lands in the session the user
                                // just switched TO; the departing session's
                                // resume point and commits are left as they
                                // stood.
                                if let Some(warning) = crate::durability::bind_session(
                                    &cfg.durability,
                                    &cfg.workspace_root,
                                    &session_record.id,
                                ) {
                                    let _ = deck_tx.send(chrome_note(warning));
                                }

                                // Blank the lead pane, replay the adopted
                                // transcript in its place (direct sends — a
                                // replay must never re-journal itself), then
                                // restore conversation, backlog, and pipeline.
                                // (The departing session's worker rows were
                                // deregistered above; the lanes THIS replay
                                // creates are remembered for the next switch.)
                                let _ = deck_tx.send(Inbound::SessionReset {
                                    agent: LEAD.to_string(),
                                });
                                replayed_lanes =
                                    crate::session_persist::journal_lanes(&rs.records, LEAD);
                                crate::session_persist::replay_session(
                                    std::mem::take(&mut rs.records),
                                    now_ms(),
                                    LEAD,
                                    &deck_tx,
                                );
                                // Same two-store choice as the startup resume,
                                // and for the same reason it is made here: the
                                // bind above is what re-keys the record the
                                // adopted session's resume point lives in, so
                                // asking any earlier would read the session
                                // being left behind.
                                let adopted = crate::session_persist::restore_conversation(
                                    cfg.durability.checkpoint().as_deref(),
                                    rs.history.take(),
                                    &system_prompt,
                                );
                                messages = adopted.messages;
                                // Interrupted prompts (any lane's unsettled
                                // dispatch) go back at the FRONT, ahead of the
                                // stored backlog, in their original order.
                                let mut restored = std::mem::take(&mut rs.interrupted);
                                restored.extend(std::mem::take(&mut rs.queue));
                                dispatch = HoldState::new();
                                dispatch.held = !restored.is_empty();
                                for text in restored.iter().rev() {
                                    let _ = in_tx.send(Inbound::PromptRequeued {
                                        agent: LEAD.to_string(),
                                        text: text.clone(),
                                    });
                                }
                                queue.adopt(sidecar_dir.clone(), restored);
                                // `--spend-limit` means THIS session, decided and
                                // implemented on both resume paths: reseed
                                // the guard's session accumulator to what
                                // the adopted session had journaled
                                // (`ResumeState::spent_usd`, its last
                                // `BudgetTick` — the same derivation the
                                // startup resume uses). No synthetic tick is
                                // emitted; the next real turn's ticks
                                // reflect the reseeded guard naturally.
                                // The larger of the journal's last tick and the
                                // adopted checkpoint's step-boundary meter —
                                // see the startup resume for why max, not
                                // either one.
                                budget.reseed_session_spend(
                                    rs.spent_usd
                                        .unwrap_or(0.0)
                                        .max(adopted.spent_usd.unwrap_or(0.0)),
                                );

                                // Fresh meta over the replayed one (pid, model,
                                // clock), back to waiting-on-you, and a fresh
                                // overlay snapshot reflecting the handover.
                                let mut meta =
                                    AgentMeta::new(LEAD, workspace_name.clone(), now_ms())
                                        .with_role("lead")
                                        .with_pid(std::process::id());
                                meta.model = Some(format!("{}/{}", cfg.provider.id, cfg.model_id));
                                let _ = in_tx.send(Inbound::Register(meta));
                                let _ = in_tx.send(Inbound::Status {
                                    agent: LEAD.to_string(),
                                    status: AgentStatus::WaitingInput,
                                });
                                let _ = deck_tx.send(chrome_note(adopted.note));
                                if !queue.is_empty() {
                                    let _ = deck_tx.send(chrome_note(format!(
                                        "{} prompt(s) waiting, dispatch held. Submit anything \
                                         to run it first, or ctrl+t to edit the queue.",
                                        queue.len()
                                    )));
                                }
                                let _ = in_tx.send(sessions_inbound(
                                    &session_registry,
                                    store.as_deref(),
                                    &session_record.id,
                                    &workspace_path,
                                ));
                            }
                        }
                        continue 'session;
                    }
                    // The task card's skip and the scope card's `e` become
                    // the lead's next prompt: with no turn running, only the
                    // model can move its own board / re-propose its scope.
                    Some(WorkspaceInput::TaskSkip { id, .. }) => {
                        dispatch.release();
                        format!(
                            "The user skipped task {id} on the task board: cancel it \
                             (task_cancel) and do not work on it."
                        )
                    }
                    Some(WorkspaceInput::ScopeChangeRequest { .. }) => {
                        dispatch.release();
                        "The user wants to change the approved scope: propose an updated \
                         scope (raise a scope review with the revised plan)."
                            .to_string()
                    }
                    // The cap retargets immediately at an idle boundary; the
                    // deck renders it when the next metered call's BudgetTick
                    // folds the new `session_limit_usd` back.
                    Some(WorkspaceInput::SetBudget { limit_usd }) => {
                        budget.set_session_limit_usd(limit_usd);
                        let _ = deck_tx.send(chrome_note(match limit_usd {
                            Some(cap) => format!(
                                "session budget cap set to ${cap:.2} (enforced between \
                                 steps)."
                            ),
                            None => "session budget cap cleared.".to_string(),
                        }));
                        continue 'session;
                    }
                    // Fallthrough for everything else, serviced between turns
                    // (install/search hit the network, so they must not stall a
                    // live turn): MCP tab actions first, then the session-registry
                    // / inbox verbs, then the INSTALLED AGENTS pane's synchronous
                    // filesystem ops, then the ISSUES tab's spawned tracker ops.
                    // A stray answer/decision/control with no turn in flight
                    // falls through all four no-ops.
                    Some(other) => {
                        // Set by a SETTINGS save below; applied before the
                        // loop turns over, so the next turn reads the files
                        // as they are now.
                        let mut settings_stale = false;
                        if !crate::deck_mcp::service_mcp_action(
                            &other,
                            cfg,
                            mcp_slot.get().map(Arc::as_ref),
                            &mcp_disabled,
                            &in_tx,
                        )
                        .await
                            && !service_registry_action(
                                &other,
                                &sessions_view::SessionScope {
                                    registry: &session_registry,
                                    store: &store,
                                    cfg,
                                    budget_limit,
                                    mine: &session_record.id,
                                    workspace: &workspace_path,
                                },
                                &in_tx,
                            )
                            && !inspect_service::service_inspect_action(
                                &other,
                                &store,
                                last_execution_id,
                                &in_tx,
                            )
                            && !handle_agents_input(&other, cfg, &in_tx)
                            && !issues::handle_issues_input(&other, cfg, &in_tx)
                            && !handle_engine_config_input(&other, cfg, &mut settings_stale, &in_tx)
                        {
                            // The tool list is enumerated here rather than
                            // cached: MCP servers join the session
                            // asynchronously, so the panel must ask what the
                            // stack holds now, not at boot.
                            let mcp = mcp_slot.get().cloned();
                            let base: &dyn ToolExecutor = match &mcp {
                                Some(set) => set.as_ref(),
                                None => &*registry,
                            };
                            let names =
                                crate::tool_switches::session_tool_names(base, &custom_tools);
                            handle_tools_input(&other, cfg, &names, &mut settings_stale, &in_tx);
                        }
                        // No turn is in flight here, so "applies from now on"
                        // means the very next prompt.
                        if settings_stale {
                            apply_pending_reload(cfg, &in_tx);
                        }
                        continue 'session;
                    }
                }
            }
        };

        let _ = in_tx.send(Inbound::PromptStarted {
            agent: LEAD.to_string(),
            text: prompt.clone(),
        });
        // What the user actually submitted — a hold-cancel returns THIS to
        // the queue, not the expansion a custom command may rewrite `prompt`
        // into below (re-dispatching it re-expands).
        let submitted = prompt.clone();

        // Session-level slash commands are the driver's, never the model's —
        // the deck's popup enqueues them like any prompt (tab switches and
        // the help overlay were already handled TUI-side and never reach us).
        // Awaited through the pump, not directly: a command that raises a
        // question (`/init`) is answered on `sub_rx`, and nothing else reads
        // that channel until the command returns (#4357).
        let command = run_deck_command(
            &prompt,
            &in_tx,
            &mut messages,
            &system_prompt,
            &*provider,
            &registry,
            cfg,
            &custom,
            agent::remaining_budget(&budget),
            &session_record.id,
            &ask_io,
        );
        let command = match slash_pump::await_answerable(
            command,
            &mut sub_rx,
            &ask_tx,
            &mut deferred,
        )
        .await
        {
            slash_pump::CommandWake::Finished(command) => command,
            slash_pump::CommandWake::Quit => break 'session,
        };
        if matches!(
            command,
            DeckCommand::Handled | DeckCommand::InitCompleted | DeckCommand::SessionModel(_)
        ) {
            // A handled command emits its answer as `Text`, which flips the
            // lead to `Running` in the deck's fold — but no turn is in flight.
            // Return it to `WaitingInput` so the dashboard reflects reality.
            // (That status is also the journal's settle marker for this
            // prompt — a resume must not re-run `/clear`.)
            let _ = in_tx.send(Inbound::Status {
                agent: LEAD.to_string(),
                status: AgentStatus::WaitingInput,
            });
            // `/clear` (and friends) may have rewritten the conversation —
            // keep the boundary snapshot current before the next dispatch.
            let _ = crate::session_persist::snapshot_history(&sidecar_dir, &messages);
        }
        let prompt = match command {
            DeckCommand::Prompt => prompt,
            // A custom command/skill invocation: the transcript already shows
            // what was typed (`PromptStarted` above); the model runs the
            // expanded template.
            DeckCommand::Expanded(text) => text,
            DeckCommand::Handled => continue 'session,
            DeckCommand::SessionModel(id) => {
                session_override::apply_model_override(
                    &id,
                    cfg,
                    &mut provider,
                    &sub_agents,
                    session_override::PromptPlane {
                        base_system_prompt: &mut base_system_prompt,
                        system_prompt: &mut system_prompt,
                        messages: &mut messages,
                    },
                    &hook_context_suffix,
                    assumed_persona.as_deref(),
                    pipeline_persona,
                    &active_rules,
                    &mut lead_meta,
                    &in_tx,
                );
                continue 'session;
            }
            DeckCommand::InitCompleted => {
                // `/init` changed the taxonomy and rebuilt the index. Re-open
                // memory so recall/reflection use the new domains this session
                // (not just the next), and push a fresh Graph-tab snapshot.
                memory = SessionMemory::open_for_session(
                    &cfg.workspace_root,
                    false,
                    &cfg.authority,
                    &active_rules,
                );
                if let Some(snapshot) = agent::graph_snapshot(&cfg.workspace_root) {
                    let _ = in_tx.send(Inbound::GraphSnapshot(snapshot));
                }
                // `/init` may also have adopted new custom commands/skills —
                // reload them and refresh the deck's slash menu in place,
                // reporting anything that failed to load (then restoring the
                // idle status the report's Text event flipped).
                custom = crate::extensions::CustomExtensions::load_with_authority(
                    &cfg.workspace_root,
                    &cfg.authority,
                );
                let _ = in_tx.send(Inbound::SlashCommands(deck_slash_commands(&custom)));
                if let Some(report) = custom.problems_report() {
                    let _ = in_tx.send(Inbound::Event {
                        agent: LEAD.to_string(),
                        event: AgentEvent::Text { text: report },
                    });
                    let _ = in_tx.send(Inbound::Status {
                        agent: LEAD.to_string(),
                        status: AgentStatus::WaitingInput,
                    });
                }
                continue 'session;
            }
        };

        // A real model turn is about to run — announce the work machine-wide.
        // The first prompt names the session (`<workspace>: <prompt…>`),
        // every prompt refreshes the summary, and the phase flips to
        // In Progress for other decks' SESSIONS overlays. Uses `submitted`
        // (what the user typed), never a custom command's expansion.
        if session_record.summary.is_empty() {
            session_record.title = format!("{workspace_name}: {}", prompt_line(&submitted, 48));
        }
        session_record.summary = prompt_line(&submitted, 240);
        session_record.status = stella_store::SessionStatus::InProgress;
        let _ = session_registry.upsert(&session_record);

        // Per-turn conversation bookkeeping, mirroring `run_interactive`:
        // refresh the volatile recall block, then append the user prompt.
        // `turn_base` is the truncation point that erases the whole turn if
        // it is cancelled; `reflect_start` scopes the reflection gate to what
        // the turn itself appends.
        // Phase 2 (#713): the deck recalled and reported nothing. The event
        // is carried to `run_lead_turn`, which owns the turn's channel.
        let mut recall = crate::memory::OpeningRecall::default();
        if let Some(m) = &mut memory {
            // The A/B control, armed before recall (#1221).
            m.arm_recall_control();
            // Anchors from what the conversation has already touched, not the
            // prompt alone. A prompt that names no path used to leave recall
            // unscoped across the whole index, where a common word matches an
            // unrelated subtree as well as the file being edited — see #4249.
            //
            // The same derivation the mid-turn re-query uses, over the same
            // messages, so the two cannot disagree about what this turn is
            // about.
            let touched =
                stella_core::driver::loop_evidence::turn_evidence(&messages).touched_paths;
            let recalled = m.recall_block_reported(&prompt, &touched).await;
            recall = crate::memory::inject_opening_recall(&mut messages, recalled);
        }
        let turn_base = messages.len();
        // Attach any media files the prompt names (including `⌃V`
        // clipboard images, which arrive as their stored payload path).
        messages.push(crate::attachments::user_message_in(
            &prompt,
            &cfg.workspace_root,
        ));
        let reflect_start = messages.len();

        // The execution record outlives the turn future so a cancelled turn
        // can still be closed out in the store.
        // The session link (store schema v8) is what lets the SESSIONS overlay's
        // `Enter` reassemble and replay the full journal after this process is gone.
        let execution = agent::begin_execution(
            &store,
            "deck",
            &prompt,
            cfg,
            Some(&session_record.id),
            // No live path drives a deck turn through the staged pipeline any
            // more (#3865) — every turn's variant is `None`, exactly like every
            // other door's raw arm.
            None,
        );
        if let Some((_, id)) = &execution {
            last_execution_id = Some(*id);
        }
        // The shared execution seam (#1872): stamp the execution onto memory
        // (the post-turn self-review is stored 1:1 with it) and record this
        // turn's skill-version usage — the same seam every headless path hits,
        // so the deck no longer carries a private copy of the recorder.
        agent::stamp_and_record_skill_usage(
            &execution,
            memory.as_mut(),
            &prompt,
            &cfg.workspace_root,
        );
        let started_unix = crate::memory::unix_now_secs();

        // Resolve the turn's tool executor from the MCP slot at dispatch:
        // connected servers join the session the moment the background
        // connect lands, and a turn that beats it runs on native tools —
        // narrated once, never silently degraded.
        // Cloned once per turn (an `Arc` clone, not a reconnect) so it can
        // also be shared into Best-of-N candidates below (issue #248 Ph1).
        let mcp = mcp_slot.get().cloned();
        let base_tools: &dyn ToolExecutor = match &mcp {
            Some(set) => set.as_ref(),
            None => &*registry,
        };
        if mcp_configured && mcp.is_none() && !mcp_pending_noted {
            mcp_pending_noted = true;
            let _ = in_tx.send(Inbound::Event {
                agent: LEAD.to_string(),
                event: AgentEvent::Text {
                    text: "MCP servers are still connecting — this turn runs with native \
                            tools; connected servers join from the next turn"
                        .to_string(),
                },
            });
        }

        let dispatch_spend_usd = budget.session_spent_usd();

        // Shared with the live input arms below: `>` steers, Esc soft-stops.
        // Per-turn by construction — a stop latched here can't leak into
        // the next turn.
        //
        // `Arc` because the turn runner also publishes a clone to the
        // registry, so sub-agents this turn dispatches stop when it does
        // (`crate::subagent`). The engine still takes it by reference.
        let steering: Arc<subsession::SteeringTap> = Arc::default();
        // The lead lane's pause seam — `p` on the lead row (#1219).
        let lead_pause = lead_control::LeadPause::new();
        let mut friction = TurnFriction::default(); // #3962
        let end = {
            // Both arms return `Result<(), CliFailure>`, so one pinned future
            // drives either path through the same select loop.
            let turn = run_lead_turn(
                &*provider,
                base_tools,
                &custom_tools,
                &registry,
                &mut messages,
                &mut budget,
                &calibration,
                cfg,
                execution.clone(),
                &in_tx,
                &sup_tx,
                &lead_holder,
                &steering,
                &lead_pause,
                recall,
                memory.as_ref(),
                &mut friction,
            );
            tokio::pin!(turn);
            loop {
                tokio::select! {
                    outcome = &mut turn => break TurnEnd::Finished(outcome),
                    // Supervisor traffic is serviced while the lead works —
                    // that is the point: a task_assign spawns its worker
                    // mid-turn, and a worker ending frees its slot for the
                    // next backlogged prompt without waiting for the lead.
                    Some(msg) = sup_rx.recv() => {
                        handle_supervisor_msg(
                            msg,
                            &mut subs,
                            &mut pending_controls,
                            &mut pending_spawns,
                            &mut queue,
                            dispatch.held(),
                            &registry,
                            &store,
                            &session_record.id,
                            &workspace_name,
                            cfg,
                            budget_limit,
                            &mut unmetered_spend,
                            &pr_nudge,
                            &in_tx,
                            &sup_tx,
                        );
                    }
                    input = sub_rx.recv() => match input {
                        None | Some(WorkspaceInput::Quit) => break TurnEnd::Quit,
                        // The lead is busy — the prompt does NOT wait for it.
                        // It backlogs and immediately drains to a dedicated
                        // sub-session if a worker slot is free ("the agent's
                        // job is to spawn a sub-session just for that
                        // request"); only slot exhaustion or a slash command
                        // leaves it queued for the lead.
                        Some(WorkspaceInput::Enqueue { text })
                        | Some(WorkspaceInput::ToAgent {
                            input: UserInput::Prompt { text, .. }, ..
                        }) => {
                            // `>`-prefix = steer THIS turn (step-boundary
                            // injection; the `Steered` event is the ack).
                            // Works for both the step-loop lead turn and the
                            // pipeline execute engine — both drain the tap.
                            // A turn that is only settling has no boundary
                            // left, so everything it receives continues the
                            // thread instead. See `subsession::route_mid_turn`.
                            match subsession::route_mid_turn(text, steering.is_settling()) {
                                subsession::MidTurnRoute::Steer(note) => {
                                    steering.push(note);
                                }
                                // Queued but deliberately NOT drained: the
                                // idle arm at the top of `'session` pops it as
                                // the lead's next turn.
                                subsession::MidTurnRoute::NextTurn(text) => {
                                    queue.push_back(text);
                                }
                                subsession::MidTurnRoute::Sidecar(text) => {
                                    queue.push_back(text);
                                    subsession::drain_queue(
                                        &mut queue,
                                        &mut subs,
                                        dispatch.held(),
                                        cfg,
                                        budget_limit,
                                        &session_record.id,
                                        &workspace_name,
                                        &in_tx,
                                        &sup_tx,
                                    );
                                }
                            }
                        }
                        // A queue-free command runs beside the turn — the
                        // whole point of the route (`command_side`); a text
                        // it declines waits its turn as it always did.
                        Some(WorkspaceInput::Command { text }) => {
                            if !command_side::run(text.trim(), cfg, &in_tx, &session_record.id) {
                                queue.push_back(text);
                            }
                        }
                        // The agents page's new-task prompt: a lane now,
                        // exactly as at idle — the lead's turn is untouched.
                        Some(WorkspaceInput::SpawnLane { text }) => {
                            subsession::spawn_lane_or_notice(
                                text,
                                &mut subs,
                                cfg,
                                budget_limit,
                                &session_record.id,
                                &workspace_name,
                                &in_tx,
                                &sup_tx,
                            );
                        }
                        // Esc with something to say — see `steer`.
                        Some(WorkspaceInput::Steer { agent, texts }) if agent == LEAD =>
                            steer::steer_lead(&steering, &mut queue, texts),
                        Some(WorkspaceInput::Steer { agent, texts }) =>
                            steer::steer_worker(&subs, &mut queue, &agent, texts, &in_tx),
                        // An explicit front-insert stays a front-insert even
                        // if a turn started before it arrived — the deck's
                        // queue view already shows it first.
                        Some(WorkspaceInput::EnqueueFront { text }) => queue.push_front(text),
                        // Waits its turn; never drained to a sidecar (see `steer`).
                        Some(WorkspaceInput::EnqueueNext { text }) => queue.push_back(text),
                        // The deck's queue editor mutates its own view of the
                        // backlog and mirrors each edit here so the dispatch
                        // queue never drifts from what the user is looking at.
                        Some(WorkspaceInput::QueueRemove { index }) => {
                            if index < queue.len() {
                                queue.remove(index);
                            }
                        }
                        Some(WorkspaceInput::QueueClear) => queue.clear(),
                        // `/clear` mid-turn: drop the turn future and
                        // reset at the boundary below — keep nothing.
                        Some(WorkspaceInput::SessionClear) => break TurnEnd::Cleared,
                        Some(WorkspaceInput::ToAgent {
                            input: UserInput::AskUserAnswer { answer, .. }, ..
                        }) => {
                            let _ = ask_tx.send(answer);
                        }
                        // The settled `ask_question` card: hand the outcome
                        // to the parked `DeckQuestionResponder`, which is
                        // what unblocks the tool call and the turn behind it.
                        Some(WorkspaceInput::QuestionAnswered(outcome)) => {
                            let _ = question_tx.send(*outcome);
                        }
                        // The decided approval card: hand the response to the
                        // parked `DeckApprovalResponder`, which is what
                        // releases (or refuses) the gated dispatch.
                        Some(WorkspaceInput::ApprovalAnswered(response)) => {
                            let _ = approval_tx.send(*response);
                        }
                        // A hunk decision answers no card this driver raises —
                        // journal replays can render the card, but nothing
                        // parks on the answer. Dropped.
                        Some(WorkspaceInput::ToAgent {
                            input: UserInput::HunkDecision { .. }, ..
                        }) => {}
                        // Stop routes by lane: aimed at the lead it cancels
                        // this turn; aimed at a worker it stops THAT worker
                        // and the lead's turn keeps running.
                        Some(WorkspaceInput::ToAgent { input: UserInput::Cancel, agent })
                        | Some(WorkspaceInput::Control {
                            control: stella_tui::AgentControl::Stop, agent,
                        }) => {
                            // With prompts parked, the first Esc *delivers*
                            // them — see `steer::stop_steers_backlog`. Only
                            // an empty backlog makes it a stop.
                            if agent == LEAD && !steer::stop_steers_backlog(&steering, &mut queue, &in_tx) {
                                // First Esc = SOFT stop: end at the next
                                // boundary keeping completed steps. The
                                // pair's second press (StopAndHold below)
                                // stays the immediate hard cancel.
                                steering.request_soft_stop();
                                // A paused turn can't reach the stop's boundary.
                                lead_pause.release_for_soft_stop();
                                let _ = in_tx.send(Inbound::Event {
                                    agent: LEAD.to_string(),
                                    event: AgentEvent::Text {
                                        text: "\n[stopping at the next step boundary — Esc again to cancel immediately]\n".to_string(),
                                    },
                                });
                            } else if agent != LEAD {
                                subs.stop(&agent);
                            }
                        }
                        // Worker Pause/Resume/Restart/Delete while the lead works.
                        Some(WorkspaceInput::Control { agent, control }) if agent != LEAD => {
                            worker_control::service(
                                &agent,
                                control,
                                &mut subs,
                                &mut pending_controls,
                                cfg,
                                budget_limit,
                                &session_record.id,
                                &workspace_name,
                                &in_tx,
                                &sup_tx,
                            );
                        }
                        // Double-Esc: cancel AND park dispatch — the
                        // interrupted prompt returns to the front of the
                        // backlog and the user's next submission runs first.
                        Some(WorkspaceInput::StopAndHold { .. }) => {
                            break TurnEnd::Cancelled { hold: true }
                        }
                        // The task card's skip rides the steering tap: the
                        // model owns its board (`task_cancel`), so the
                        // request lands at the next step boundary as
                        // guidance rather than as a driver-side mutation —
                        // the row flips only when the next `TaskUpdate`
                        // snapshot folds back.
                        Some(WorkspaceInput::TaskSkip { id, .. }) => {
                            steering.push(format!(
                                "The user skipped task {id} on the task board: cancel it \
                                 (task_cancel) and do not work on it."
                            ));
                        }
                        // A scope change is next-turn work — the running
                        // turn's scope is locked; the request queues as the
                        // lead's next prompt.
                        Some(WorkspaceInput::ScopeChangeRequest { .. }) => {
                            queue.push_back(
                                "The user wants to change the approved scope: revise the task \
                                 board (task_create / task_cancel) and start the revised plan \
                                 — that is what raises a fresh scope review for them."
                                    .to_string(),
                            );
                        }
                        // The budget guard is borrowed by the running turn;
                        // the cap retargets at the settle boundary below.
                        Some(WorkspaceInput::SetBudget { limit_usd }) => {
                            pending_budget = Some(limit_usd);
                            let _ = deck_tx.send(chrome_note(match limit_usd {
                                Some(cap) => format!(
                                    "session budget cap ${cap:.2} — applies when this turn \
                                     settles."
                                ),
                                None => "session budget cap cleared when this turn settles."
                                    .to_string(),
                            }));
                        }
                        // The Graph tab can re-root mid-turn (a user browsing
                        // the graph while an agent works). The requery opens
                        // SQLite + loads grammars, so run it on the blocking
                        // pool rather than stalling this event pump; it sends
                        // the fresh snapshot back when done.
                        Some(
                            input @ (WorkspaceInput::FocusGraphFile { .. }
                            | WorkspaceInput::GraphQuery { .. }),
                        ) => {
                            let tx = in_tx.clone();
                            let root = cfg.workspace_root.clone();
                            tokio::task::spawn_blocking(move || {
                                graph_input::answer(input, &root, &tx);
                            });
                        }
                        // The INSTALLED AGENTS pane stays live while a turn
                        // runs — refresh / save / pin are pure filesystem
                        // ops, the same shared helper as the idle recv site.
                        Some(
                            input @ (WorkspaceInput::AgentsRefresh
                            | WorkspaceInput::AgentSave { .. }
                            | WorkspaceInput::AgentPin { .. }
                            | WorkspaceInput::AgentDelete { .. }),
                        ) => {
                            handle_agents_input(&input, cfg, &in_tx);
                        }
                        // Creation needs the provider, which the running
                        // turn is borrowing — park it; it runs the moment
                        // the turn settles (see `pending_create`).
                        Some(WorkspaceInput::AgentCreate { description, scope }) => {
                            pending_create = Some((description, scope));
                            // `creating: true`: the deck's create dialog keeps
                            // its spinner up through this interim snapshot.
                            let _ = in_tx.send(agents_list_creating(
                                &cfg.workspace_root,
                                Some(
                                    "agent creation queued — it runs when the current turn \
                                     finishes"
                                        .to_string(),
                                ),
                            ));
                        }
                        // SKILLS-tab ops run alongside the in-flight turn (disk
                        // ops inline, npx/model ops spawned) — the manager stays
                        // usable while an agent is working. Create spawns its own
                        // provider, so unlike AgentCreate it needs no parking.
                        Some(WorkspaceInput::Skill(op)) => {
                            handle_skills_input(
                                &op,
                                cfg,
                                &in_tx,
                                &skill_registry,
                                budget_limit,
                            );
                        }
                        // MCP tab: a live enable/disable toggle mid-turn is
                        // honored immediately — it only flips the shared set the
                        // tool layer already consults, so the next model call in
                        // this turn sees the change (the tab display refreshes at
                        // the next idle snapshot). The other MCP actions (search,
                        // install, remove, auth) touch config/network and are
                        // serviced between turns; mid-turn they are no-ops.
                        Some(WorkspaceInput::McpToggle { name }) => {
                            let mut set =
                                mcp_disabled.lock().unwrap_or_else(|p| p.into_inner());
                            if !set.remove(&name) {
                                set.insert(name);
                            }
                        }
                        Some(WorkspaceInput::McpSearch { .. })
                        | Some(WorkspaceInput::McpInstall { .. })
                        | Some(WorkspaceInput::McpRemove { .. })
                        | Some(WorkspaceInput::McpAuth { .. })
                        | Some(WorkspaceInput::McpRefresh) => {}
                        // The inspector IS serviced mid-turn: it is a read of
                        // config + the live tool set + telemetry, and the
                        // alternative is an overlay the user opened that hangs
                        // on "gathering server detail…" until the turn ends.
                        // `lookup` is dropped here rather than honored — a
                        // registry round-trip is the one part that is not a
                        // local read, and mid-turn is the wrong time to start
                        // one; the `r` press is repeatable once the turn ends.
                        Some(WorkspaceInput::McpInspect { name, .. }) => {
                            let mcp = mcp_slot.get().cloned();
                            match crate::deck_mcp::mcp_detail(
                                cfg,
                                mcp.as_deref(),
                                &mcp_disabled,
                                &name,
                                stella_tui::McpLookupState::Idle,
                            )
                            .await
                            {
                                Ok(detail) => {
                                    let _ = in_tx.send(Inbound::McpDetail(Box::new(detail)));
                                }
                                Err(error) => {
                                    let _ = in_tx.send(chrome_note(format!("mcp: {error}\n")));
                                }
                            }
                        }
                        // OAuth login is a spawned browser round-trip — safe
                        // to start mid-turn (its transport picks the tokens
                        // up lazily on the next call either way).
                        Some(WorkspaceInput::McpOauthLogin { server }) => {
                            crate::deck_mcp::spawn_mcp_oauth_login(
                                server,
                                cfg.workspace_root.clone(),
                                in_tx.clone(),
                            );
                        }
                        // The SESSIONS overlay and the inbox stay live while a
                        // turn runs — cheap local file reads/writes, exactly
                        // like the INSTALLED AGENTS pane above.
                        Some(
                            input @ (WorkspaceInput::SessionsRefresh
                            | WorkspaceInput::SessionOpen { .. }
                            | WorkspaceInput::SessionArchive { .. }
                            | WorkspaceInput::SessionDelete { .. }
                            | WorkspaceInput::NotificationRead { .. }
                            | WorkspaceInput::NotificationsReadAll),
                        ) => {
                            service_registry_action(
                                &input,
                                &sessions_view::SessionScope {
                                    registry: &session_registry,
                                    store: &store,
                                    cfg,
                                    budget_limit,
                                    mine: &session_record.id,
                                    workspace: &workspace_path,
                                },
                                &in_tx,
                            );
                        }
                        // INSPECT is answered mid-turn too: the receipts of
                        // earlier steps are already durable, and watching the
                        // context grow while a turn runs is the point.
                        Some(
                            input @ (WorkspaceInput::InspectRefresh
                            | WorkspaceInput::InspectCall { .. }),
                        ) => {
                            inspect_service::service_inspect_action(&input, &store, last_execution_id, &in_tx);
                        }
                        // Navigation waits for the road to clear: switching
                        // sessions mid-turn would tear down live work, so the
                        // deck is told how to proceed instead.
                        Some(WorkspaceInput::AgentAssume { name, .. }) => {
                            let _ = deck_tx.send(chrome_note(format!(
                                "a turn is running — press a on {name} again once it settles"
                            )));
                        }
                        // A model switch moves the provider handle and the
                        // prompt prefix, both of which the running turn is
                        // using — between turns only, like AgentAssume.
                        Some(WorkspaceInput::ModelOverride { spec }) => {
                            let _ = deck_tx.send(chrome_note(format!(
                                "a turn is running — pick {spec} again once it settles"
                            )));
                        }
                        Some(WorkspaceInput::SessionResume { .. } | WorkspaceInput::SessionNew) => {
                            let _ = deck_tx.send(chrome_note(
                                "a turn is running — esc stops it (esc esc holds the queue \
                                 too), then press ⏎ on the session again."
                                    .to_string(),
                            ));
                        }
                        // The ENGINE overlay stays live while a turn runs —
                        // settings reads/writes are cheap local filesystem
                        // ops, exactly like the INSTALLED AGENTS pane. A
                        // mid-turn save applies to runs started afterwards;
                        // the in-flight turn keeps its resolved models.
                        Some(
                            input @ (WorkspaceInput::EngineConfigSave { .. }
                            | WorkspaceInput::EngineConfigRefresh),
                        ) => {
                            handle_engine_config_input(
                                &input,
                                cfg,
                                &mut pending_settings_reload,
                                &in_tx,
                            );
                        }
                        // The TOOLS panel likewise. `base_tools` is the very
                        // stack the running turn is using, so the list the
                        // panel shows mid-turn is exactly what that turn has.
                        Some(
                            input @ (WorkspaceInput::ToolsSave { .. }
                            | WorkspaceInput::ToolsRefresh),
                        ) => {
                            let names = crate::tool_switches::session_tool_names(
                                base_tools,
                                &custom_tools,
                            );
                            handle_tools_input(
                                &input,
                                cfg,
                                &names,
                                &mut pending_settings_reload,
                                &in_tx,
                            );
                        }
                        // The ISSUES tab stays live while a turn runs too —
                        // real work spawns its own task and answers from it,
                        // so nothing here blocks the event pump.
                        Some(
                            input @ (WorkspaceInput::IssuesRefresh { .. }
                            | WorkspaceInput::IssueCreate { .. }
                            | WorkspaceInput::IssueAct { .. }
                            | WorkspaceInput::EntitySearch { .. }),
                        ) => {
                            issues::handle_issues_input(&input, cfg, &in_tx);
                        }
                        // Everything above peeled off `Stop` and every worker
                        // lane, so this is the LEAD's own pause/resume/restart.
                        Some(WorkspaceInput::Control { control, .. }) => {
                            lead_pause.control(control, &in_tx);
                        }
                    },
                }
            }
            // `turn` (and the channel senders it holds) drops here.
        };

        // Repay the dashboard if this turn was ever painted paused (#1219).
        lead_pause.settle(&in_tx);

        // A `/budget` change parked during the turn applies here — the safe
        // boundary. The deck shows the new cap when the next metered call's
        // BudgetTick folds `session_limit_usd` back.
        if let Some(cap) = pending_budget.take() {
            budget.set_session_limit_usd(cap);
        }

        // Likewise a SETTINGS save parked during the turn: the turn that was
        // reading `cfg` has ended, so re-deriving it here is both sound and
        // the earliest honest moment for "applies to runs started from now
        // on" to become true.
        if std::mem::take(&mut pending_settings_reload) {
            apply_pending_reload(cfg, &in_tx);
        }

        match end {
            TurnEnd::Finished(outcome) => {
                if let Err(reason) = &outcome {
                    if reason.message() == stella_core::SOFT_STOP_REASON {
                        // A user choice, not a failure: no Error row — the
                        // work is kept and the next prompt continues from it.
                        let _ = in_tx.send(Inbound::Event {
                            agent: LEAD.to_string(),
                            event: AgentEvent::Text {
                                text: "\n[stopped at the step boundary — completed work kept]\n"
                                    .to_string(),
                            },
                        });
                    } else {
                        // An aborted turn emits no `Complete`; this row flips
                        // the dashboard to failed AND clears any pending gate.
                        let _ = in_tx.send(Inbound::Event {
                            agent: LEAD.to_string(),
                            event: AgentEvent::Error {
                                message: reason.to_string(),
                                retryable: false,
                            },
                        });
                    }
                }
                // A turn that ended by asking is not a turn that is over. Say
                // so BEFORE the bookkeeping below, not after: the whole point
                // is that the user is reading this screen right now, deciding
                // whether they are expected to answer. `WaitingInput` renders
                // `needs input` with a `?` where `done`/`✓` would have been,
                // and — unlike `Done` — is not `is_terminal()`, so nothing
                // downstream reads the session as finished. See [`settle`].
                if outcome.is_ok() && settle::ends_with_a_question(&messages[reflect_start..]) {
                    let _ = in_tx.send(Inbound::Status {
                        agent: LEAD.to_string(),
                        status: AgentStatus::WaitingInput,
                    });
                }
                // Bookkeeping runs behind the settle window, which keeps reading
                // the deck across it. Before this, the reflection model call
                // inside `record_and_reflect_turn` left the driver deaf for as
                // long as that call took, with the deck already painted done —
                // so a prompt typed at a finished-looking turn queued and never
                // ran.
                deferred.extend(
                    settle::run_while_listening(
                        authoring::record_and_reflect_turn(
                            &mut memory,
                            &prompt,
                            &outcome,
                            started_unix,
                            &messages,
                            reflect_start,
                            &friction,
                            &*provider,
                            cfg,
                            &mut budget,
                            &in_tx,
                        ),
                        &mut sub_rx,
                        &mut queue,
                    )
                    .await,
                );
                // Name the workspace state this turn ended at — readable by
                // turn number (`WorkJournal::read_at_turn`), with the turn's
                // diff precomputed beside it (#1870). An ABORTED turn still
                // ended, and its files are exactly what a reader comparing
                // turns wants — so this is not conditioned on the outcome.
                cfg.durability
                    .mark_turn_end(&store, &session_record.id, last_execution_id);
                // One decider for every terminal writer (#1653/#1826/#1862):
                // a lead turn that ended in a deliberate stop exits `Stopped`.
                session_exit = crate::daemon::outcome_status(outcome.as_ref().map(|_| ()));
                session_record.status = stella_store::SessionStatus::NeedsInput;
                let _ = session_registry.upsert(&session_record);
                let turn_secs = crate::memory::unix_now_secs().saturating_sub(started_unix);
                let inbox = stella_store::NotificationStore::open_default();
                if let Err(reason) = &outcome {
                    let _ = inbox.push(
                        &stella_store::Notification::new(
                            format!("{workspace_name}: turn failed"),
                            format!("{} — {reason}", prompt_line(&submitted, 80)),
                            session_record.id.clone(),
                        )
                        .with_session_id(session_record.id.clone()),
                    );
                } else if turn_secs >= LONG_TURN_NOTIFY_SECS {
                    let _ = inbox.push(
                        &stella_store::Notification::new(
                            format!("{workspace_name}: work finished ({turn_secs}s)"),
                            prompt_line(&submitted, 160),
                            session_record.id.clone(),
                        )
                        .with_session_id(session_record.id.clone()),
                    );
                }
                // The turn may have committed / pushed / opened a PR —
                // reconcile now instead of waiting out the poll interval.
                pr_nudge.notify_one();
                // Mirror the lead's final board into the store's `tasks`
                // table — cross-session findability for what this turn
                // planned and finished (the event-log copy already rode the
                // forwarder for replay).
                if let Some((store, id)) = &execution {
                    let board = registry.task_board();
                    let items: Vec<TaskItem> = board
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .items()
                        .to_vec();
                    if !items.is_empty() {
                        let _ = store.record_task_board(
                            *id,
                            Some(&session_record.id),
                            &items,
                            now_ms(),
                        );
                    }
                }
            }
            TurnEnd::Cancelled { hold } => {
                // Erase the partial turn: the next prompt continues from the
                // last committed conversation state.
                messages.truncate(turn_base);
                // The dropped turn future never reached its own claim
                // release — free the lead's write claims by holder so
                // workers (and other sessions) aren't blocked on a turn
                // that no longer exists.
                if let Some(store) = &store {
                    let _ = store.release_file_locks_for_holder(&lead_holder);
                }
                if hold {
                    // Double-Esc landing mid-turn: this turn is the NEXT
                    // prompt, auto-dispatched in the gap between the pair's
                    // two messages. Park dispatch and return both to the
                    // FRONT of the backlog — the retained prompt (the one
                    // the pair's first press cancelled) ahead of this one,
                    // restoring the order the user last saw. The next
                    // submission will run ahead of them all.
                    requeue_front(&mut queue, &in_tx, dispatch.stop_and_hold(Some(&submitted)));
                } else {
                    // A plain cancel: retain the dropped prompt so the pair's
                    // escalation — which always lands after this press has
                    // already dropped the turn — still has something to
                    // requeue and park on (see [`HoldState`]).
                    dispatch.cancelled(&submitted);
                }
                dropped_turn::close_dropped_execution(
                    execution.as_ref(),
                    registry.as_ref(),
                    "cancelled",
                    dispatch_spend_usd,
                    &mut budget,
                    &in_tx,
                );
                // Must stay AFTER the store warning above: the warning is
                // retryable (folds to Running) while this one is not (folds
                // to Failed), so this event is what leaves the lead in a
                // terminal state on the dashboard.
                let _ = in_tx.send(Inbound::Event {
                    agent: LEAD.to_string(),
                    event: AgentEvent::Error {
                        message: "turn stopped by user".to_string(),
                        retryable: false,
                    },
                });
                // Registry: an interrupted turn leaves the session waiting on
                // the user; if the deck exits from this state, it exits
                // Cancelled (the user abandoned the interrupted work).
                session_exit = stella_store::SessionStatus::Cancelled;
                session_record.status = stella_store::SessionStatus::NeedsInput;
                let _ = session_registry.upsert(&session_record);
            }
            // `/clear` mid-turn: dropped like a cancel, retaining nothing.
            // The dying turn's events precede the `SessionReset` on the FIFO
            // inbound channel, so no stale delta repaints the cleared pane.
            TurnEnd::Cleared => {
                messages.truncate(turn_base);
                // Free the lead's write claims the dropped future never
                // released (same as a cancel).
                if let Some(store) = &store {
                    let _ = store.release_file_locks_for_holder(&lead_holder);
                }
                dispatch.reset();
                queue.clear();
                dropped_turn::close_dropped_execution(
                    execution.as_ref(),
                    registry.as_ref(),
                    "cleared",
                    dispatch_spend_usd,
                    &mut budget,
                    &in_tx,
                );
                session_clear::reset_lead(
                    &mut messages,
                    &system_prompt,
                    &sidecar_dir,
                    &mut subs,
                    registry.as_ref(),
                    store.as_deref().zip(Some(session_record.id.as_str())),
                    &in_tx,
                );
                // No `continue`: the shared tail below re-snapshots the
                // reset history (a no-op) and still services a parked create.
                session_exit = stella_store::SessionStatus::Cancelled;
                session_record.status = stella_store::SessionStatus::NeedsInput;
                let _ = session_registry.upsert(&session_record);
            }
            // Quit landing mid-turn: erase the partial turn from the
            // conversation before the boundary snapshot below — a dangling
            // assistant tool call with no result is a broken history, and
            // the journal's unsettled `PromptStarted` puts this prompt back
            // at the front of the queue on resume anyway.
            TurnEnd::Quit => {
                messages.truncate(turn_base);
                break 'session;
            }
        }

        // Durable turn boundary: the conversation as committed (post-turn or
        // post-cancel-truncation) — what a resume continues from. The queue
        // is write-through already; its one-time failure warning surfaces
        // here, on the same cadence as every other persistence warning.
        if let Some(warning) = crate::session_persist::snapshot_history(&sidecar_dir, &messages)
            .or_else(|| queue.take_warning())
        {
            let _ = in_tx.send(Inbound::Event {
                agent: LEAD.to_string(),
                event: AgentEvent::Error {
                    message: warning,
                    retryable: true,
                },
            });
        }

        // A creation request parked during the turn: the provider is free
        // again, so draft + install it before the next dispatch.
        if let Some((description, scope)) = pending_create.take() {
            handle_agent_create(
                &description,
                scope,
                cfg,
                &*provider,
                agent::remaining_budget(&budget),
                &in_tx,
            )
            .await;
        }
    }

    // Quitting (Ctrl-C included) must not orphan live workers as detached
    // threads that die mid-tool at process exit: signal every stop and wait
    // — bounded — for their endings, so drop-guards run, executions close
    // out, notifications land, and their claims release instead of blocking
    // rivals until the age-based sweep.
    subsession::shutdown_workers(&mut subs, &mut sup_rx, subsession::QUIT_JOIN_DEADLINE).await;

    // The session is over — leave the registry record in its terminal state
    // and the durable state current. (A crash never reaches here; readers
    // downgrade a dead pid to Error — and the journal makes even that
    // resumable.) Quitting with prompts still queued is a PAUSE now, not an
    // abandonment: the backlog is durable and reopens intact. The journal
    // syncs HERE, not just in the tee's own teardown — background senders
    // (the inbox poller) keep the tee alive past this point, and runtime
    // teardown must never be what a buffered tail was waiting on.
    journal_sink
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .sync();
    let _ = crate::session_persist::snapshot_history(&sidecar_dir, &messages);
    // Pack the durable record's loose objects. Every step of every turn writes
    // a commit, a tree and a blob or two, so a workspace worked in for months
    // accumulates them without bound — a leak, not an untidiness. `git gc
    // --auto` is a no-op below git's own loose-object threshold, so the cost on
    // a short session is one subprocess that returns immediately.
    //
    // Here rather than in the deck's teardown: this runs before the terminal is
    // handed back, and it is the last point at which the session's record is
    // still bound. Best-effort and silent, like the sink — a session whose work
    // is already done and persisted must never be delayed, or failed, by
    // housekeeping.
    cfg.durability.compact();
    session_record.status = if queue.is_empty() {
        session_exit
    } else {
        stella_store::SessionStatus::Paused
    };
    let _ = session_registry.upsert(&session_record);

    // Closing our inbound sender ends the deck's stream if the user hasn't
    // already quit (the journal tee drains, fsyncs, and forwards the close);
    // then wait for it to restore the terminal.
    drop(in_tx);
    let deck_result = deck.await;
    if let Some(set) = mcp_slot.get() {
        set.close_all().await;
    }
    match deck_result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(format!("deck terminal error: {e}")),
        Err(e) => Err(format!("deck task failed: {e}")),
    }
}

/// Registry hygiene: terminal session records older than this are swept at
/// deck startup (30 days).
const SESSION_RECORD_MAX_AGE_MS: u64 = 30 * 24 * 60 * 60 * 1000;
/// Inbox hygiene: **read** notifications older than this are swept at deck
/// startup (14 days). Unread ones persist regardless — that is the contract.
const NOTIFICATION_MAX_AGE_MS: u64 = 14 * 24 * 60 * 60 * 1000;
/// A successful turn at least this long lands a "work finished" notification
/// — long enough that the user has plausibly looked away.
const LONG_TURN_NOTIFY_SECS: i64 = 60;

/// One prompt flattened to a single display line, char-safe-truncated — the
/// session registry's title/summary shape.
pub(crate) fn prompt_line(prompt: &str, max_chars: usize) -> String {
    let flat: String = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        return flat;
    }
    let head: String = flat.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{head}…")
}

// ── ISSUES tab: tracker-backed operations ───────────────────────────────────

// The deck's productized vocabulary (`DECK_BUILTINS`) and the
// reserved-name guard (`deck_reserved`) live in `skills`, beside the
// slash-menu builder that consumes them (the god-file rule).

// ── Agent-engine config (the SETTINGS tab's config panel) ─────────────────────

// ── Tool switches (the SETTINGS tab's TOOLS panel) ─────────────────────────

// ── Installed-agents manager (the AGENTS tab's INSTALLED AGENTS pane) ───────

mod mid_turn_ask;

#[cfg(test)]
mod tests;
