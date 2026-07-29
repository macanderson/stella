//! `stella fleet` live dashboard — the full-screen monitor a fleet run paints
//! over the alternate screen while its workers fan out.
//!
//! Where the command deck ([`crate::deck`]) is the interactive, multi-tab
//! operations workspace and `stella observe` is the deep historical telemetry
//! surface, this is the deliberately narrow **live feed**: one dense row per
//! task, the last thing each worker did, and two clocks that answer "is
//! anything actually happening" at a glance. The deeper per-agent counters
//! (graph queries, per-file CRUD, cache economics, …) live in `stella observe`
//! — the footer says so.
//!
//! ## Purity + clocks
//!
//! [`FleetBoard`] is a fold of the [`FleetMsg`] stream, exactly like the deck's
//! `WorkspaceModel` folds `Inbound`. The one deliberate difference is the clock
//! source: every timestamp here is a monotonic [`Instant`] stamped **on
//! receipt**, never system time and never carried across the worker-thread
//! boundary — an NTP step or a laptop sleep can therefore never make a clock
//! jump or run backwards (a hard requirement of the fleet view).
//!
//! ## The two-and-a-half clocks
//!
//! - **SESSION** — age of the whole run, from the instant the dashboard opens.
//!   Never pauses, never resets.
//! - **FLEET-IDLE** — time since *any* worker last started a tool call. Resets
//!   to zero on every tool call anywhere in the fleet; if it climbs, the entire
//!   fleet is stalled. Yellow past 30s, red past 120s.
//! - **TOOL-AGO** (per task) — time since *that* task last started a tool call.
//!   Yellow past 60s, red past 180s; `----` once the task is finished.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table, Widget};
use stella_protocol::{AgentEvent, FileChangeKind, ToolOutput};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::oneshot;

use crate::term::{PanicHookGuard, TerminalGuard};
use crate::theme;

/// How often the dashboard repaints even when no event lands, so the clocks
/// tick visibly (the spec's "at least once per second"; a little quicker keeps
/// the second-hand smooth).
const TICK: Duration = Duration::from_millis(250);

// ── Wire type: fleet driver → dashboard ─────────────────────────────────────

/// One message from the fleet driver to the dashboard. Mirrors the deck's
/// `Inbound` but stays a lean, fleet-specific vocabulary: the fleet only ever
/// registers tasks, streams their `AgentEvent`s, and reports supervisor
/// lifecycle transitions.
#[derive(Clone, Debug)]
pub enum FleetMsg {
    /// A task exists — its row appears, `Queued`, before it is dispatched.
    Register { id: String, title: String },
    /// One `AgentEvent` belonging to one task's worker.
    Event { id: String, event: AgentEvent },
    /// A supervisor lifecycle transition (dispatch → `Running`, and the
    /// authoritative terminal verdict from the worker's own `WorkerOutcome`,
    /// which distinguishes done/failed more reliably than inferring from the
    /// event stream).
    Status { id: String, status: FleetStatus },
}

// ── Wire type: dashboard → fleet driver ─────────────────────────────────────

/// One supervisor verb the dashboard asks its driver to perform on one task.
///
/// The dashboard deliberately does **not** hold a `Fleet`: `stella-tui` does
/// not depend on `stella-fleet`, and keeping the view a pure fold of its
/// inbound stream is what makes it testable without a terminal. So the three
/// control verbs travel the other way down this channel, and the driver
/// (`stella-cli/src/fleet_cmd.rs`) applies them to the real `Fleet`.
///
/// The verbs map one-to-one onto `Fleet::pause_task` / `Fleet::resume_task` /
/// `Fleet::stop_task`. Each is a no-op against a task with no live worker, so
/// a stale verb is never an error — but the dashboard still declines to send
/// one for a task it can already see is terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FleetControl {
    /// Park the task's worker at its next safe step boundary.
    Pause(String),
    /// Un-park a paused worker.
    Resume(String),
    /// Fire the task's stop line; the worker drops its turn future at the
    /// next await point.
    Stop(String),
}

impl FleetControl {
    /// The task id this verb addresses.
    pub fn task_id(&self) -> &str {
        match self {
            FleetControl::Pause(id) | FleetControl::Resume(id) | FleetControl::Stop(id) => id,
        }
    }
}

// ── Status ──────────────────────────────────────────────────────────────────

/// The lifecycle of one fleet task, with the spec's exact glyph vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FleetStatus {
    /// Not started yet (fleet at max parallelism).
    Queued,
    /// Worker active: calling a tool, or reasoning between tools.
    Running,
    /// Needs input — approval, a secret, a human decision.
    Blocked,
    /// Finished, exit ok.
    Done,
    /// Finished, exit error.
    Failed,
    /// Stopped by a supervisor.
    Killed,
}

impl FleetStatus {
    /// The spec glyph.
    pub fn glyph(self) -> &'static str {
        match self {
            FleetStatus::Queued => "⋯",
            FleetStatus::Running => "▶",
            FleetStatus::Blocked => "⏸",
            FleetStatus::Done => "✔",
            FleetStatus::Failed => "✗",
            FleetStatus::Killed => "✋",
        }
    }

    /// The short table label.
    pub fn label(self) -> &'static str {
        match self {
            FleetStatus::Queued => "queued",
            FleetStatus::Running => "run",
            FleetStatus::Blocked => "block",
            FleetStatus::Done => "done",
            FleetStatus::Failed => "fail",
            FleetStatus::Killed => "killed",
        }
    }

    fn color(self) -> Color {
        match self {
            FleetStatus::Queued => theme::TEXT_TERTIARY,
            FleetStatus::Running => theme::ACCENT,
            FleetStatus::Blocked => theme::WARNING_BRIGHT,
            FleetStatus::Done => theme::SUCCESS_BRIGHT,
            FleetStatus::Failed | FleetStatus::Killed => theme::DANGER_BRIGHT,
        }
    }

    /// True once the task has reached a terminal state (no more work, clocks
    /// frozen, TOOL-AGO reads `----`).
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            FleetStatus::Done | FleetStatus::Failed | FleetStatus::Killed
        )
    }

    /// True while a dispatched worker backs this task, i.e. `Fleet` has live
    /// control lines registered for it. Only `Running` and `Blocked` qualify:
    /// `Blocked` is reached from an `AskUser`/`ScopeReview` event, which fires
    /// inside the worker's run span, whereas `Queued` (not yet dispatched) and
    /// the terminal states have no controls — a verb there is a guaranteed
    /// no-op.
    pub fn has_live_worker(self) -> bool {
        matches!(self, FleetStatus::Running | FleetStatus::Blocked)
    }

    /// Sort bucket for the default ordering: blocked first (can't hide), then
    /// running, then queued, then finished.
    fn group_rank(self) -> u8 {
        match self {
            FleetStatus::Blocked => 0,
            FleetStatus::Running => 1,
            FleetStatus::Queued => 2,
            FleetStatus::Done | FleetStatus::Failed | FleetStatus::Killed => 3,
        }
    }
}

// ── Last action ─────────────────────────────────────────────────────────────

/// The most recent thing a worker did, rendered on its row and never blank.
/// Between steps it holds the last real action rather than clearing.
#[derive(Clone, Debug, PartialEq, Eq)]
enum LastAction {
    /// Nothing has happened yet — the row shows a neutral placeholder keyed off
    /// its status ("queued", "starting…").
    Idle,
    /// A tool call: the display name + primary arg, optionally enriched with
    /// `+A-B` once the file change it produced lands.
    Tool(String),
    /// The model is reasoning and has produced no tool/message yet this step.
    Thinking,
    /// The first line of a model message.
    Message(String),
    /// Waiting on a human — approval, a secret, a decision.
    Blocked(String),
    /// A non-retryable error headline.
    Error(String),
}

// ── Per-task row ────────────────────────────────────────────────────────────

/// One task's live state — a pure fold of its `FleetMsg`s.
#[derive(Clone, Debug)]
pub struct TaskRow {
    pub id: String,
    pub title: String,
    pub status: FleetStatus,
    /// When the task went `Running` (its wall clock starts here). `None` while
    /// `Queued`, so a queued row's ELAPSED reads `00:00`.
    started: Option<Instant>,
    /// Frozen wall-clock anchor once terminal — ELAPSED holds this value.
    ended: Option<Instant>,
    /// When this task last *started* a tool call — the per-task TOOL-AGO anchor.
    last_tool_at: Option<Instant>,
    /// Total tool calls this task has made (the exit summary's per-task count).
    pub tool_calls: u64,
    action: LastAction,
    /// Raw name of the most recent tool call (detail pane).
    last_tool_name: Option<String>,
    /// Primary arg of the most recent tool call (detail pane).
    last_tool_arg: Option<String>,
    /// The unified-diff string of the most recent file change (detail pane).
    last_diff: Option<String>,
    /// A short output preview from the most recent non-file tool result.
    last_output: Option<String>,
}

impl TaskRow {
    fn new(id: String, title: String) -> Self {
        Self {
            id,
            title,
            status: FleetStatus::Queued,
            started: None,
            ended: None,
            last_tool_at: None,
            tool_calls: 0,
            action: LastAction::Idle,
            last_tool_name: None,
            last_tool_arg: None,
            last_diff: None,
            last_output: None,
        }
    }

    /// Wall-clock elapsed: frozen at the final value once finished, `0` while
    /// still queued.
    fn elapsed(&self, now: Instant) -> Duration {
        match (self.started, self.ended) {
            (Some(start), Some(end)) => end.saturating_duration_since(start),
            (Some(start), None) => now.saturating_duration_since(start),
            (None, _) => Duration::ZERO,
        }
    }

    /// Time since this task last started a tool call. `None` once terminal
    /// (the cell reads `----`); before the first tool it counts from dispatch.
    fn tool_ago(&self, now: Instant) -> Option<Duration> {
        if self.status.is_terminal() {
            return None;
        }
        let anchor = self.last_tool_at.or(self.started)?;
        Some(now.saturating_duration_since(anchor))
    }

    /// The one-line LAST ACTION cell — never blank.
    fn action_text(&self) -> String {
        match &self.action {
            LastAction::Idle => match self.status {
                FleetStatus::Queued => "queued".to_string(),
                FleetStatus::Done => "done".to_string(),
                FleetStatus::Failed => "failed".to_string(),
                FleetStatus::Killed => "killed".to_string(),
                _ => "starting…".to_string(),
            },
            LastAction::Tool(s) => s.clone(),
            LastAction::Thinking => "thinking…".to_string(),
            LastAction::Message(s) => s.clone(),
            LastAction::Blocked(reason) => format!("waiting: {reason}"),
            LastAction::Error(msg) => msg.clone(),
        }
    }
}

// ── Board ───────────────────────────────────────────────────────────────────

/// The whole dashboard state — a fold of the [`FleetMsg`] stream.
pub struct FleetBoard {
    /// Run label (the workspace basename), shown in the header.
    pub label: String,
    /// Monotonic anchor for the SESSION clock.
    session_start: Instant,
    /// Monotonic anchor for the global FLEET-IDLE clock — the most recent tool
    /// call anywhere in the fleet.
    fleet_last_tool: Instant,
    /// Rows in first-registered order; the display order is computed at render.
    pub rows: Vec<TaskRow>,
    index: HashMap<String, usize>,
}

impl FleetBoard {
    /// A fresh board seeded with every task `Queued`, clocks anchored to now.
    pub fn new(label: impl Into<String>, tasks: &[(String, String)], now: Instant) -> Self {
        let mut board = Self {
            label: label.into(),
            session_start: now,
            fleet_last_tool: now,
            rows: Vec::with_capacity(tasks.len()),
            index: HashMap::with_capacity(tasks.len()),
        };
        for (id, title) in tasks {
            board.register(id.clone(), title.clone());
        }
        board
    }

    fn register(&mut self, id: String, title: String) {
        if let Some(&i) = self.index.get(&id) {
            self.rows[i].title = title;
            return;
        }
        self.index.insert(id.clone(), self.rows.len());
        self.rows.push(TaskRow::new(id, title));
    }

    fn row_mut(&mut self, id: &str) -> Option<&mut TaskRow> {
        self.index.get(id).copied().map(|i| &mut self.rows[i])
    }

    /// Fold one message. `now` is stamped by the caller on receipt.
    pub fn apply(&mut self, msg: FleetMsg, now: Instant) {
        match msg {
            FleetMsg::Register { id, title } => self.register(id, title),
            FleetMsg::Status { id, status } => {
                // Auto-register an unknown id so a status is never dropped.
                if !self.index.contains_key(&id) {
                    self.register(id.clone(), id.clone());
                }
                let Some(row) = self.row_mut(&id) else { return };
                // A terminal verdict wins outright and freezes the clock; never
                // walk a finished task back to running.
                if row.status.is_terminal() {
                    return;
                }
                if status == FleetStatus::Running && row.started.is_none() {
                    row.started = Some(now);
                }
                if status.is_terminal() {
                    row.ended = Some(now);
                }
                row.status = status;
            }
            FleetMsg::Event { id, event } => {
                if !self.index.contains_key(&id) {
                    self.register(id.clone(), id.clone());
                }
                self.apply_event(&id, &event, now);
            }
        }
    }

    fn apply_event(&mut self, id: &str, event: &AgentEvent, now: Instant) {
        // Global idle resets the instant ANY worker starts a tool call — do it
        // before the per-row borrow.
        if matches!(event, AgentEvent::ToolStart { .. }) {
            self.fleet_last_tool = now;
        }
        let Some(row) = self.row_mut(id) else { return };
        // The first event proves the task is live; lift it out of Queued (the
        // explicit Running status usually arrives first, but never rely on it).
        if row.status == FleetStatus::Queued {
            row.status = FleetStatus::Running;
            if row.started.is_none() {
                row.started = Some(now);
            }
        }
        // Late events after the verdict only refresh the detail preview: the
        // arms below all guard `is_terminal()` before touching status, and
        // `elapsed`/`tool_ago` read from the frozen `ended` anchor, so a
        // straggler can never walk a finished row back to running.
        match event {
            AgentEvent::ToolStart { call } => {
                row.tool_calls += 1;
                row.last_tool_at = Some(now);
                let display = tool_display_name(&call.name);
                let arg = primary_arg(&call.input);
                row.last_tool_name = Some(display.clone());
                row.last_tool_arg = arg.clone();
                row.last_diff = None;
                row.last_output = None;
                row.action = LastAction::Tool(match &arg {
                    Some(a) => format!("{display}  {a}"),
                    None => display,
                });
            }
            AgentEvent::FileChange { path, kind, diff } => {
                // Enrich the last action with the +A-B the tool produced, and
                // keep the diff for the focused task's detail pane.
                let (added, removed) = diff
                    .as_deref()
                    .map(crate::diff::count_diff_lines)
                    .unwrap_or((0, 0));
                let verb = row
                    .last_tool_name
                    .clone()
                    .unwrap_or_else(|| file_verb(*kind).to_string());
                row.last_diff = diff.clone();
                row.action =
                    LastAction::Tool(format!("{verb}  {}  +{added}-{removed}", short_path(path)));
            }
            AgentEvent::ToolResult { output, .. } => {
                let preview = match output {
                    ToolOutput::Ok { content } => first_line(content),
                    ToolOutput::Error { message } => format!("error: {}", first_line(message)),
                };
                if !preview.is_empty() {
                    row.last_output = Some(preview);
                }
            }
            AgentEvent::Reasoning { .. } => {
                // Thinking only surfaces before any real action this run —
                // otherwise the last tool/message is held while TOOL-AGO counts.
                if row.action == LastAction::Idle {
                    row.action = LastAction::Thinking;
                }
            }
            AgentEvent::Text { delta } => {
                let line = first_line(delta);
                if !line.is_empty() {
                    row.action = LastAction::Message(line);
                }
            }
            AgentEvent::AskUser { question, .. } => {
                row.action = LastAction::Blocked(first_line(question));
                if !row.status.is_terminal() {
                    row.status = FleetStatus::Blocked;
                }
            }
            AgentEvent::ScopeReview { proposal } => {
                row.action = LastAction::Blocked(first_line(&proposal.summary));
                if !row.status.is_terminal() {
                    row.status = FleetStatus::Blocked;
                }
            }
            // Only a non-retryable error is worth surfacing as the last action;
            // a retryable one means the turn continues.
            AgentEvent::Error {
                message,
                retryable: false,
                ..
            } => {
                row.action = LastAction::Error(first_line(message));
            }
            _ => {}
        }
    }

    fn session_elapsed(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.session_start)
    }

    fn fleet_idle(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.fleet_last_tool)
    }

    fn count(&self, status: FleetStatus) -> usize {
        self.rows.iter().filter(|r| r.status == status).count()
    }
}

// ── View state (focus + sort) ───────────────────────────────────────────────

/// How the grid is ordered. `Default` is the spec ordering (blocked first, then
/// running by most-stalled, then queued, then finished); the rest are the `[s]`
/// cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SortKey {
    Default,
    Id,
    Status,
    ToolAgo,
    Elapsed,
}

impl SortKey {
    fn next(self) -> SortKey {
        match self {
            SortKey::Default => SortKey::Id,
            SortKey::Id => SortKey::Status,
            SortKey::Status => SortKey::ToolAgo,
            SortKey::ToolAgo => SortKey::Elapsed,
            SortKey::Elapsed => SortKey::Default,
        }
    }
    fn label(self) -> &'static str {
        match self {
            SortKey::Default => "default",
            SortKey::Id => "id",
            SortKey::Status => "status",
            SortKey::ToolAgo => "tool-ago",
            SortKey::Elapsed => "elapsed",
        }
    }
}

/// Ephemeral interaction state — focus is pinned to a task **id** so it survives
/// re-sorts, exactly as the spec requires.
///
/// The three supervisor sets below record what *this supervisor asked for*, not
/// what the fleet reports: a pause has no wire representation coming back (the
/// worker parks silently at a step boundary), so without local intent the grid
/// could not distinguish a parked worker from a slow one. Fleet truth still
/// wins wherever the two can disagree — a task that reaches a terminal status
/// is rendered from its status, never from these sets.
struct FleetView {
    focused_id: Option<String>,
    sort: SortKey,
    /// Tasks this supervisor has paused and not yet resumed.
    paused: HashSet<String>,
    /// Tasks whose stop line this supervisor has already fired. Kept so the
    /// grid can show the stop as in-flight during the window between the verb
    /// and the worker's terminal `Status`.
    stopped: HashSet<String>,
    /// The task a first `[x]` armed. Stop is the one irreversible verb here —
    /// it drops a worker's turn and loses whatever it had not committed — so it
    /// takes two keystrokes. Any other keystroke disarms it.
    pending_stop: Option<String>,
}

impl FleetView {
    fn new(rows: &[TaskRow]) -> Self {
        Self {
            focused_id: rows.first().map(|r| r.id.clone()),
            sort: SortKey::Default,
            paused: HashSet::new(),
            stopped: HashSet::new(),
            pending_stop: None,
        }
    }

    /// The supervisor marker for one row, if any. Terminal status wins: a task
    /// that finished while paused is done, not paused.
    fn marker(&self, entry: &TaskRow) -> Option<&'static str> {
        if entry.status.is_terminal() {
            return None;
        }
        if self.stopped.contains(&entry.id) {
            Some("stopping")
        } else if self.paused.contains(&entry.id) {
            Some("paused")
        } else {
            None
        }
    }
}

/// What one keystroke decided. Returned rather than acted on so the whole key
/// surface is unit-testable without a terminal — the gap that kept these verbs
/// unwired (#645).
#[derive(Clone, Debug, PartialEq, Eq)]
enum KeyAction {
    /// Handled internally (focus, sort, arming a stop) or ignored.
    None,
    /// Leave the dashboard; the run continues in the background.
    Detach,
    /// Send this verb to the driver.
    Control(FleetControl),
}

/// The focused task, but only when a supervisor verb could still reach a live
/// worker. Control lines exist only for the span a worker runs, so a task
/// without a live worker — a terminal task *or* a still-`Queued` one that has
/// not been dispatched — would have `Fleet` answer `false`. The dashboard
/// declines rather than record local intent (`paused`/`stopped`) and render a
/// marker for a verb that is guaranteed to be a no-op.
fn controllable_focus(board: &FleetBoard, view: &FleetView) -> Option<String> {
    let id = view.focused_id.as_deref()?;
    let row = board.rows.iter().find(|r| r.id == id)?;
    row.status.has_live_worker().then(|| row.id.clone())
}

/// Fold one keystroke into the view, returning what the caller should do.
///
/// Pure apart from the `view` mutation: it never touches the terminal and never
/// sends anything, which is what lets the tests drive the entire supervisor
/// surface as a sequence of `KeyEvent`s.
fn on_key(key: KeyEvent, board: &FleetBoard, view: &mut FleetView, now: Instant) -> KeyAction {
    // Taken unconditionally: an armed stop must not survive an unrelated
    // keystroke, so every branch below starts from a disarmed view and only
    // the confirming `[x]` reads what was armed.
    let armed = view.pending_stop.take();

    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => KeyAction::Detach,
        KeyCode::Char('q') | KeyCode::Esc => KeyAction::Detach,
        KeyCode::Up | KeyCode::Char('k') => {
            move_focus(board, view, now, -1);
            KeyAction::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            move_focus(board, view, now, 1);
            KeyAction::None
        }
        KeyCode::Char('s') => {
            view.sort = view.sort.next();
            KeyAction::None
        }
        KeyCode::Char('p') => match controllable_focus(board, view) {
            Some(id) => {
                view.paused.insert(id.clone());
                KeyAction::Control(FleetControl::Pause(id))
            }
            None => KeyAction::None,
        },
        KeyCode::Char('r') => match controllable_focus(board, view) {
            Some(id) => {
                view.paused.remove(&id);
                KeyAction::Control(FleetControl::Resume(id))
            }
            None => KeyAction::None,
        },
        KeyCode::Char('x') => match controllable_focus(board, view) {
            // Second `[x]` on the same task confirms.
            Some(id) if armed.as_deref() == Some(id.as_str()) => {
                view.paused.remove(&id);
                view.stopped.insert(id.clone());
                KeyAction::Control(FleetControl::Stop(id))
            }
            // First `[x]` arms; the footer says so until something disarms it.
            Some(id) => {
                view.pending_stop = Some(id);
                KeyAction::None
            }
            None => KeyAction::None,
        },
        _ => KeyAction::None,
    }
}

/// The display order as row indices, honoring the active sort. Focus is applied
/// by the caller against the returned order.
fn display_order(board: &FleetBoard, sort: SortKey, now: Instant) -> Vec<usize> {
    let mut order: Vec<usize> = (0..board.rows.len()).collect();
    match sort {
        SortKey::Default => order.sort_by(|&a, &b| {
            let (ra, rb) = (&board.rows[a], &board.rows[b]);
            ra.status
                .group_rank()
                .cmp(&rb.status.group_rank())
                // Within running, most-stalled (largest tool-ago) rises.
                .then_with(|| {
                    let ta = ra.tool_ago(now).unwrap_or(Duration::ZERO);
                    let tb = rb.tool_ago(now).unwrap_or(Duration::ZERO);
                    tb.cmp(&ta)
                })
                .then_with(|| ra.id.cmp(&rb.id))
        }),
        SortKey::Id => order.sort_by(|&a, &b| board.rows[a].id.cmp(&board.rows[b].id)),
        SortKey::Status => order.sort_by(|&a, &b| {
            board.rows[a]
                .status
                .group_rank()
                .cmp(&board.rows[b].status.group_rank())
                .then_with(|| board.rows[a].id.cmp(&board.rows[b].id))
        }),
        SortKey::ToolAgo => order.sort_by(|&a, &b| {
            let ta = board.rows[a].tool_ago(now).unwrap_or(Duration::ZERO);
            let tb = board.rows[b].tool_ago(now).unwrap_or(Duration::ZERO);
            tb.cmp(&ta)
                .then_with(|| board.rows[a].id.cmp(&board.rows[b].id))
        }),
        SortKey::Elapsed => order.sort_by(|&a, &b| {
            board.rows[b]
                .elapsed(now)
                .cmp(&board.rows[a].elapsed(now))
                .then_with(|| board.rows[a].id.cmp(&board.rows[b].id))
        }),
    }
    order
}

// ── Result ──────────────────────────────────────────────────────────────────

/// One task's line in the end-of-run summary the caller prints after the
/// dashboard restores the normal screen.
pub struct TaskSummary {
    pub id: String,
    pub title: String,
    pub status: FleetStatus,
    pub elapsed: Duration,
    pub tool_calls: u64,
}

/// What the dashboard hands back when it exits.
pub struct FleetDashResult {
    /// True if the user pressed `q`/`Ctrl-C` to close the view before the run
    /// finished on its own (the run keeps going to completion in the
    /// background; see the module/PR notes on `attach`).
    pub detached: bool,
    pub session_elapsed: Duration,
    pub tasks: Vec<TaskSummary>,
}

// ── The shell ───────────────────────────────────────────────────────────────

/// Run the live dashboard until every task is terminal, the `done` signal
/// fires, or the user detaches with `q`. Owns the alternate screen for its
/// lifetime and restores the terminal on every exit path (including panic, via
/// `PanicHookGuard`).
///
/// `control` carries the supervisor verbs back to the driver. It is dropped
/// when this function returns, which is how the driver's control pump learns
/// the dashboard is gone and stops.
pub async fn run(
    label: impl Into<String>,
    tasks: Vec<(String, String)>,
    mut inbound: UnboundedReceiver<FleetMsg>,
    mut done: oneshot::Receiver<()>,
    control: tokio::sync::mpsc::UnboundedSender<FleetControl>,
) -> io::Result<FleetDashResult> {
    let guard = TerminalGuard::enter(false)?;
    let _hook = PanicHookGuard::install(None, &guard);
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let color_mode = theme::detect_color_mode();

    let mut board = FleetBoard::new(label, &tasks, Instant::now());
    let mut view = FleetView::new(&board.rows);

    // Blocking crossterm reader → async loop (mirrors `deck_shell`).
    let (key_tx, mut key_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let shutdown = Arc::new(AtomicBool::new(false));
    let reader_shutdown = shutdown.clone();
    let reader = std::thread::spawn(move || {
        while !reader_shutdown.load(Ordering::Relaxed) {
            match event::poll(Duration::from_millis(50)) {
                Ok(true) => match event::read() {
                    Ok(ev) => {
                        if key_tx.send(ev).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                },
                Ok(false) => {}
                Err(_) => break,
            }
        }
    });

    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut detached = false;
    let mut run_done = false;
    // Once the key reader thread exits (stdin EOF / a poll error), its sender
    // drops and `key_rx.recv()` would return `Ready(None)` on every poll —
    // which, under a `biased` select, would spin the draw loop at 100% CPU. The
    // guard disables that branch the moment it closes so the loop keeps idling
    // on the tick instead.
    let mut keys_open = true;

    'run: loop {
        let now = Instant::now();
        terminal.draw(|f| {
            render(&board, &view, now, f.area(), f.buffer_mut());
            theme::apply_theme(f.buffer_mut(), color_mode);
            theme::degrade_buffer(f.buffer_mut(), color_mode);
        })?;

        // The run finished: drain any straggler messages so the summary is
        // exact, then exit and let the caller print it.
        if run_done {
            while let Ok(msg) = inbound.try_recv() {
                board.apply(msg, Instant::now());
            }
            break 'run;
        }

        tokio::select! {
            biased;
            maybe_key = key_rx.recv(), if keys_open => {
                match maybe_key {
                    Some(Event::Key(key)) if key.kind != KeyEventKind::Release => {
                        match on_key(key, &board, &mut view, now) {
                            KeyAction::Detach => { detached = true; break 'run; }
                            // A closed control channel means the driver already
                            // settled the run; the verb is moot, not an error.
                            KeyAction::Control(verb) => { let _ = control.send(verb); }
                            KeyAction::None => {}
                        }
                    }
                    Some(_) => {}
                    // The reader thread closed — stop selecting on it so the
                    // loop idles on the tick instead of spinning.
                    None => keys_open = false,
                }
            }
            maybe_msg = inbound.recv() => {
                match maybe_msg {
                    Some(msg) => board.apply(msg, Instant::now()),
                    // Every sender dropped — treat as run complete.
                    None => run_done = true,
                }
            }
            res = &mut done => {
                // The driver signalled the run returned. Finish this frame,
                // then exit on the next loop turn via the `run_done` drain.
                if res.is_ok() { run_done = true; }
            }
            _ = tick.tick() => {}
        }
    }

    // Restore the terminal before returning so the caller prints on the normal
    // screen.
    shutdown.store(true, Ordering::Relaxed);
    let _ = reader.join();
    drop(terminal);
    drop(guard);

    let now = Instant::now();
    let tasks = board
        .rows
        .iter()
        .map(|r| TaskSummary {
            id: r.id.clone(),
            title: r.title.clone(),
            status: r.status,
            elapsed: r.elapsed(now),
            tool_calls: r.tool_calls,
        })
        .collect();
    Ok(FleetDashResult {
        detached,
        session_elapsed: board.session_elapsed(now),
        tasks,
    })
}

/// Move focus by `delta` steps through the current display order, pinning the
/// new focus to that task's id so it survives the next re-sort.
fn move_focus(board: &FleetBoard, view: &mut FleetView, now: Instant, delta: i32) {
    let order = display_order(board, view.sort, now);
    if order.is_empty() {
        return;
    }
    let cur = view
        .focused_id
        .as_deref()
        .and_then(|id| order.iter().position(|&i| board.rows[i].id == id))
        .unwrap_or(0) as i32;
    let next = (cur + delta).rem_euclid(order.len() as i32) as usize;
    view.focused_id = Some(board.rows[order[next]].id.clone());
}

// ── Rendering ───────────────────────────────────────────────────────────────

const GRID_HEADERS: [&str; 6] = ["#", "TASK", "STATUS", "LAST ACTION", "TOOL-AGO", "ELAPSED"];

fn render(board: &FleetBoard, view: &FleetView, now: Instant, area: Rect, buf: &mut Buffer) {
    if area.height < 6 || area.width < 20 {
        return;
    }
    let bands = Layout::vertical([
        Constraint::Length(1), // title clocks
        Constraint::Length(1), // counts
        Constraint::Length(1), // rule
        Constraint::Min(3),    // grid
        Constraint::Length(1), // rule
        Constraint::Length(7), // detail
        Constraint::Length(1), // footer
    ])
    .split(area);

    render_header(board, now, bands[0], buf);
    render_counts(board, view, bands[1], buf);
    rule(bands[2], buf);
    let order = display_order(board, view.sort, now);
    render_grid(board, view, &order, now, bands[3], buf);
    rule(bands[4], buf);
    render_detail(board, view, &order, now, bands[5], buf);
    render_footer(view, bands[6], buf);
}

fn render_header(board: &FleetBoard, now: Instant, area: Rect, buf: &mut Buffer) {
    let idle = board.fleet_idle(now);
    let idle_color = if idle.as_secs() > 120 {
        theme::DANGER_BRIGHT
    } else if idle.as_secs() > 30 {
        theme::WARNING_BRIGHT
    } else {
        theme::TEXT_TERTIARY
    };
    let line = Line::from(vec![
        Span::styled(
            " FLEET ",
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("▸ ", theme::muted()),
        Span::styled(
            board.label.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("     "),
        Span::styled("SESSION ", theme::muted()),
        Span::styled(
            fmt_hms(board.session_elapsed(now)),
            Style::default().fg(theme::TEXT_PRIMARY),
        ),
        Span::raw("     "),
        Span::styled("FLEET-IDLE ", theme::muted()),
        Span::styled(fmt_clock(idle), Style::default().fg(idle_color)),
    ]);
    Paragraph::new(line).render(area, buf);
}

fn render_counts(board: &FleetBoard, view: &FleetView, area: Rect, buf: &mut Buffer) {
    let total = board.rows.len();
    let cell = |glyph: &str, n: usize, label: &str, color: Color| {
        vec![
            Span::styled(format!("{n} "), Style::default().fg(color)),
            Span::styled(format!("{glyph} "), Style::default().fg(color)),
            Span::styled(format!("{label}   "), theme::muted()),
        ]
    };
    let mut spans = vec![Span::styled(format!(" {total} tasks   "), theme::muted())];
    spans.extend(cell(
        FleetStatus::Running.glyph(),
        board.count(FleetStatus::Running),
        "running",
        FleetStatus::Running.color(),
    ));
    spans.extend(cell(
        FleetStatus::Blocked.glyph(),
        board.count(FleetStatus::Blocked),
        "blocked",
        FleetStatus::Blocked.color(),
    ));
    spans.extend(cell(
        FleetStatus::Queued.glyph(),
        board.count(FleetStatus::Queued),
        "queued",
        FleetStatus::Queued.color(),
    ));
    spans.extend(cell(
        FleetStatus::Done.glyph(),
        board.count(FleetStatus::Done),
        "done",
        FleetStatus::Done.color(),
    ));
    spans.extend(cell(
        FleetStatus::Failed.glyph(),
        board.count(FleetStatus::Failed) + board.count(FleetStatus::Killed),
        "failed",
        FleetStatus::Failed.color(),
    ));
    // The sort hint trails the counts on the same line (the counts are
    // variable-width, so there is nothing stable to right-align against).
    spans.push(Span::styled(
        format!("sort: {}", view.sort.label()),
        theme::muted(),
    ));
    Paragraph::new(Line::from(spans)).render(area, buf);
}

fn render_grid(
    board: &FleetBoard,
    view: &FleetView,
    order: &[usize],
    now: Instant,
    area: Rect,
    buf: &mut Buffer,
) {
    let header = Row::new(GRID_HEADERS.iter().map(|h| Cell::from(*h))).style(theme::accent());
    let rows: Vec<Row> = order
        .iter()
        .enumerate()
        .map(|(pos, &i)| {
            let entry = &board.rows[i];
            let focused = view.focused_id.as_deref() == Some(entry.id.as_str());
            grid_row(pos + 1, entry, now, focused, view.marker(entry))
        })
        .collect();
    let widths = [
        Constraint::Length(3),  // #
        Constraint::Length(22), // TASK
        // 10, not 8: wide enough for the longest supervisor marker
        // ("⏸ stopping") as well as every `FleetStatus::label`.
        Constraint::Length(10), // STATUS
        Constraint::Fill(1),    // LAST ACTION
        Constraint::Length(9),  // TOOL-AGO
        Constraint::Length(8),  // ELAPSED
    ];
    Table::new(rows, widths)
        .header(header)
        .column_spacing(1)
        .render(area, buf);
}

fn grid_row(
    pos: usize,
    entry: &TaskRow,
    now: Instant,
    focused: bool,
    marker: Option<&'static str>,
) -> Row<'static> {
    let status_color = entry.status.color();
    let caret = if focused { "▸" } else { " " };
    let num = Cell::from(format!("{caret}{pos}")).style(theme::muted());
    let task = Cell::from(truncate(&entry.title_or_id(), 22)).style(theme::body());
    // A supervised task reads as its supervisor verb rather than its fleet
    // status: "running" on a worker the operator just paused would be a lie the
    // grid tells for as long as the park lasts.
    let (status_text, status_color) = match marker {
        Some(m) => (format!("⏸ {m}"), theme::WARNING_BRIGHT),
        None => (
            format!("{} {}", entry.status.glyph(), entry.status.label()),
            status_color,
        ),
    };
    let status = Cell::from(status_text).style(Style::default().fg(status_color));
    let action = Cell::from(entry.action_text()).style(theme::body());

    let (tool_ago_text, tool_ago_color) = match entry.tool_ago(now) {
        None => ("----".to_string(), theme::TEXT_DIM),
        Some(d) => {
            let color = if d.as_secs() > 180 {
                theme::DANGER_BRIGHT
            } else if d.as_secs() > 60 {
                theme::WARNING_BRIGHT
            } else {
                theme::TEXT_TERTIARY
            };
            (fmt_clock(d), color)
        }
    };
    let tool_ago = Cell::from(tool_ago_text).style(Style::default().fg(tool_ago_color));
    let elapsed = Cell::from(fmt_ms(entry.elapsed(now))).style(theme::muted());

    let mut row = Row::new(vec![num, task, status, action, tool_ago, elapsed]);
    if focused {
        row = row.style(Style::default().add_modifier(Modifier::REVERSED));
    }
    row
}

fn render_detail(
    board: &FleetBoard,
    view: &FleetView,
    order: &[usize],
    now: Instant,
    area: Rect,
    buf: &mut Buffer,
) {
    let Some(entry) = view
        .focused_id
        .as_deref()
        .and_then(|id| board.rows.iter().find(|r| r.id == id))
    else {
        return;
    };
    let pos = order
        .iter()
        .position(|&i| board.rows[i].id == entry.id)
        .map(|p| p + 1)
        .unwrap_or(0);

    let mut lines: Vec<Line> = Vec::new();
    // Heading: ▸ TASK n  id                          status · tool-ago
    let tool_ago = match entry.tool_ago(now) {
        Some(d) => fmt_clock(d),
        None => "----".to_string(),
    };
    lines.push(Line::from(vec![
        Span::styled(
            format!(" ▸ TASK {pos}  "),
            Style::default().fg(theme::ACCENT),
        ),
        Span::styled(
            entry.id.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(
            entry.status.label(),
            Style::default().fg(entry.status.color()),
        ),
        Span::styled(format!(" · tool-ago {tool_ago}"), theme::muted()),
    ]));
    // last: <tool> <arg>
    let last = match (&entry.last_tool_name, &entry.last_tool_arg) {
        (Some(name), Some(arg)) => format!("{name}  {arg}"),
        (Some(name), None) => name.clone(),
        _ => entry.action_text(),
    };
    lines.push(Line::from(vec![
        Span::styled("   last: ", theme::muted()),
        Span::styled(last, Style::default().fg(theme::TEXT_PRIMARY)),
    ]));
    // A diff or output preview, up to 4 lines.
    if let Some(diff) = &entry.last_diff {
        for l in diff.lines().filter(|l| !l.is_empty()).take(4) {
            let color = if l.starts_with('+') {
                theme::SUCCESS_BRIGHT
            } else if l.starts_with('-') {
                theme::DANGER_BRIGHT
            } else {
                theme::TEXT_TERTIARY
            };
            lines.push(Line::from(Span::styled(
                format!(
                    "         {}",
                    truncate(l, area.width.saturating_sub(10) as usize)
                ),
                Style::default().fg(color),
            )));
        }
    } else if let Some(out) = &entry.last_output {
        lines.push(Line::from(Span::styled(
            format!(
                "         {}",
                truncate(out, area.width.saturating_sub(10) as usize)
            ),
            theme::muted(),
        )));
    }
    Paragraph::new(lines).render(area, buf);
}

fn render_footer(view: &FleetView, area: Rect, buf: &mut Buffer) {
    // An armed stop replaces the whole help line: it is the only state in this
    // view where the next keystroke is destructive, so it must not have to
    // compete for attention with the key legend.
    if let Some(id) = &view.pending_stop {
        let line = Line::from(vec![
            Span::styled(
                format!("  stop {id}? "),
                Style::default()
                    .fg(theme::DANGER_BRIGHT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("[x] again to confirm   ", theme::muted()),
            Span::styled("any other key cancels", theme::muted()),
        ]);
        Paragraph::new(line).render(area, buf);
        return;
    }
    let line = Line::from(vec![
        Span::styled("  [↑/↓] focus   ", theme::muted()),
        Span::styled("[s] sort   ", theme::muted()),
        Span::styled("[p] pause   ", theme::muted()),
        Span::styled("[r] resume   ", theme::muted()),
        Span::styled("[x] stop   ", theme::muted()),
        Span::styled("[q] detach   ", theme::muted()),
        Span::styled("·   deeper telemetry: ", theme::muted()),
        Span::styled("stella observe", Style::default().fg(theme::ACCENT)),
    ]);
    Paragraph::new(line).render(area, buf);
}

fn rule(area: Rect, buf: &mut Buffer) {
    if area.width == 0 {
        return;
    }
    let dashes = "─".repeat(area.width as usize);
    Paragraph::new(Line::from(Span::styled(dashes, theme::muted()))).render(area, buf);
}

impl TaskRow {
    fn title_or_id(&self) -> String {
        if self.title.trim().is_empty() {
            self.id.clone()
        } else {
            self.title.clone()
        }
    }
}

// ── Formatting + tool helpers ───────────────────────────────────────────────

/// `HH:MM:SS` — the SESSION clock (age of the whole run).
fn fmt_hms(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

/// `MM:SS`, zero-padded minutes that grow past two digits — the per-task
/// ELAPSED wall clock.
fn fmt_ms(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// `M:SS` (minutes un-padded) — the idle / tool-ago clocks (`0:03`, `2:10`).
fn fmt_clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

/// A human, title-cased display name for a tool call.
fn tool_display_name(name: &str) -> String {
    match name {
        "read_file" => "Read".into(),
        "write_file" => "Write".into(),
        "edit_file" => "Edit".into(),
        "bash" => "Bash".into(),
        "grep" => "Grep".into(),
        "glob" => "Glob".into(),
        "read_symbol" => "Symbol".into(),
        "graph_query" => "Graph".into(),
        other => {
            let mut c = other.chars();
            match c.next() {
                Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                None => other.to_string(),
            }
        }
    }
}

/// The verb for a file change with no originating tool name known.
fn file_verb(kind: FileChangeKind) -> &'static str {
    match kind {
        FileChangeKind::Created => "Write",
        FileChangeKind::Modified => "Edit",
        FileChangeKind::Deleted => "Delete",
        FileChangeKind::Read => "Read",
    }
}

/// The primary argument of a tool call — the one field worth showing on the
/// row. Tries the well-known keys in priority order, then falls back to the
/// first string value; always truncated so the row stays a row.
fn primary_arg(input: &serde_json::Value) -> Option<String> {
    const KEYS: [&str; 8] = [
        "path", "command", "pattern", "query", "target", "symbol", "name", "glob",
    ];
    let raw = KEYS
        .iter()
        .find_map(|k| input.get(*k).and_then(|v| v.as_str()))
        .map(str::to_string)
        .or_else(|| {
            input
                .as_object()
                .and_then(|o| o.values().find_map(|v| v.as_str()))
                .map(str::to_string)
        })?;
    Some(truncate(&first_line(&raw), 60))
}

/// A short, workspace-relative-ish rendering of a path for the row.
fn short_path(path: &str) -> String {
    truncate(path, 48)
}

/// The first non-empty line of free text, flattened and trimmed.
fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

/// Truncate to `max` chars with a trailing ellipsis, never splitting a
/// codepoint.
fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests;
