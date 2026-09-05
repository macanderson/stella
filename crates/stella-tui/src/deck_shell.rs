//! The deck's async run loop: terminal setup/teardown, the crossterm event
//! loop, and channel plumbing.
//!
//! Deliberately thin, like the single-session shell: every decision
//! (key→action via [`crate::deck_ui`], event→state via
//! [`crate::deck_ui::ingest_inbound`], the frame via [`crate::deck_render`])
//! lives in pure, unit-tested layers. This file only wires them to real I/O.
//!
//! It differs from a plain single-session loop in one structural way: a fixed
//! **animation/resource tick** (~30 fps) is a third `select!` arm. A live
//! dashboard — CPU gauges, elapsed timers, sparklines, the run progress bar —
//! must repaint on a clock, not only when the agent streams. That tick is also
//! where the clock advances and the resource monitor samples, so all
//! time-based UI shares one heartbeat.

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossterm::event::{Event, KeyEventKind};
use ratatui::backend::CrosstermBackend;
use ratatui::text::{Line, Text};
use ratatui::widgets::{Paragraph, Widget};
use ratatui::{Terminal, TerminalOptions, Viewport};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::accessible;
use crate::composer::{Composer, SlashCommand};
use crate::debug_log::DebugLog;
use crate::deck::WorkspaceModel;
use crate::deck_render::render_deck;
use crate::deck_ui::{DeckAction, DeckUi, focused_id, handle_deck_key, ingest_inbound};
use crate::envelope::{AgentId, AgentMeta, AgentStatus, Inbound, WorkspaceInput};
use crate::graph::GraphSnapshot;
use crate::resource::ResourceMonitor;
use crate::term::{PanicHookGuard, TerminalGuard};
use crate::theme;

/// The repaint / sample cadence. ~30 fps keeps animations smooth and the CPU
/// gauge / elapsed timers live without busy-spinning.
const TICK: Duration = Duration::from_millis(33);

/// The synthetic agent id a `$` shell command falls back to when the deck has
/// no agent registered yet. Normally a command borrows the focused agent's
/// lane instead, so its output reads inline in the transcript the user is
/// looking at — see [`spawn_shell_command`]. This lane exists so output is
/// never dropped in the gap before the session registers.
const SHELL_AGENT: &str = "shell";

/// Cap on captured shell output fed back as an event. Head and tail are both
/// kept (errors live at the tail); the middle is elided.
const SHELL_OUTPUT_CAP: usize = 4000;

/// Configuration for one deck session.
#[derive(Debug, Clone, Default)]
pub struct DeckOptions {
    /// Enable mouse capture. Off by default so native terminal selection
    /// keeps working (L-T2). Opted in, events dispatch through
    /// [`crate::mouse::handle_deck_mouse`] — a tab-row click switches tabs,
    /// the wheel scrolls the Session transcript — at the price of the
    /// terminal's own text selection.
    pub mouse_capture: bool,
    /// Structured debug log path (`STELLA_DEBUG=1`), or `None` for a no-op sink.
    pub debug_log_path: Option<PathBuf>,
    /// An initial code-graph snapshot to seed the Graph tab (the caller, which
    /// owns a `CodeGraph`, queries it and hands it in — the TUI stays decoupled).
    pub initial_graph: Option<GraphSnapshot>,
    /// The slash-command vocabulary for the `/` popup (the caller owns the
    /// real list, exactly like the single-session `RunOptions`).
    pub slash_commands: Vec<SlashCommand>,
    /// Disable all motion (progress shimmer / pulse / caret blink) — the
    /// `--no-anim` flag, for CI and asciinema-style recordings that want a
    /// static frame. Also forced on by `STELLA_NO_ANIM` or `NO_COLOR`.
    pub no_anim: bool,
    /// Run the deck the way a screen reader can actually read it —
    /// `stella --accessible` / `STELLA_ACCESSIBLE` (#1258).
    ///
    /// This is a **mode, not a surface**: the same `run_deck`, all nine tabs,
    /// the prompt queue, sub-agents, steering and resume. Four things follow
    /// from it, and they are one decision rather than four:
    ///
    /// * the deck draws on the user's own screen instead of the alternate one
    ///   (`term::Screen`), so nothing is hidden and nothing is torn
    ///   down on exit;
    /// * it draws into an **inline** viewport, which is what makes
    ///   `Terminal::insert_before` — and therefore the whole scrollback path
    ///   in [`crate::accessible::Scrollback`] — possible;
    /// * motion is frozen regardless of [`Self::no_anim`], because a region
    ///   that repaints on a clock is a region a reader may keep picking up;
    /// * mouse capture is forced off regardless of [`Self::mouse_capture`] —
    ///   a captured mouse takes the selection away from the assistive
    ///   technology that needs it.
    ///
    /// `--plain` is unaffected and stays what it is: the no-terminal path.
    pub accessible: bool,
    /// Where the palette's `recent` section is kept for this workspace
    /// (`.stella/private/palette-recent.json`), or `None` for a caller that
    /// has no workspace to keep it in — the section is then in-session only.
    /// The caller resolves the path for the reason it resolves
    /// [`Self::debug_log_path`]: this crate does not decide where a workspace
    /// lives.
    pub recent_path: Option<PathBuf>,
    /// What a plain prompt typed at a running agent does (`ui.mid_turn_prompt`):
    /// queue for the lead so Esc can steer it (the default), raise the routing
    /// card, or fork a sidecar. See [`crate::deck_ui::MidTurnPrompt`].
    pub mid_turn_prompt: crate::deck_ui::MidTurnPrompt,
    /// Whether push-to-talk dictation is armed (`voice.enabled` in settings;
    /// off by default — ADR 0020). The caller reads settings for the reason
    /// it resolves [`Self::mid_turn_prompt`]: this crate does not read config.
    pub voice_enabled: bool,
    /// Which dictation gesture is bound to Space (`voice.mode` in settings,
    /// `hold` by default). Read by the caller for the same reason as
    /// [`Self::voice_enabled`]; `/voice` re-sends it mid-session as
    /// [`Inbound::VoiceConfig`].
    pub voice_mode: crate::voice::VoiceMode,
}

/// Carry out a [`crate::voice::VoiceCmd`]: the composer edit it asks for, and
/// the input it sends the driver.
///
/// Three arms fold voice events — a reported release, a swallowed space, and
/// the tick clock — and every one of them can now open or close a capture, so
/// the retraction and the send live here rather than being written out three
/// times and drifting apart.
fn apply_voice_cmd(
    cmd: crate::voice::VoiceCmd,
    ui: &mut DeckUi,
    submissions: &UnboundedSender<WorkspaceInput>,
) {
    match cmd {
        crate::voice::VoiceCmd::Start { retract } => {
            for _ in 0..retract {
                ui.composer.backspace();
            }
            let _ = submissions.send(WorkspaceInput::VoiceStart);
        }
        crate::voice::VoiceCmd::Stop => {
            let _ = submissions.send(WorkspaceInput::VoiceStop);
        }
        crate::voice::VoiceCmd::Cancel => {
            let _ = submissions.send(WorkspaceInput::VoiceCancel);
        }
        crate::voice::VoiceCmd::None => {}
    }
}

/// Apply one dispatch outcome to the deck, whichever device produced it —
/// the key arm and the mouse arm both land here, so the queue mirror, the
/// shell lane, and quit have one mutation site. Returns `true` when the
/// session should end; the caller owns the loop and the `break`.
fn apply_deck_action(
    action: DeckAction,
    model: &mut WorkspaceModel,
    ui: &mut DeckUi,
    submissions: &UnboundedSender<WorkspaceInput>,
    local_tx: &UnboundedSender<Inbound>,
    shell_active: &Arc<AtomicUsize>,
    debug: &DebugLog,
) -> bool {
    match action {
        DeckAction::Quit => {
            debug.note("user quit");
            let _ = submissions.send(WorkspaceInput::Quit);
            true
        }
        DeckAction::Send(input) => {
            // Queue edits are reflected locally so they show
            // immediately, then forwarded for dispatch — the
            // input path never blocks on a busy agent. (The
            // queue is the labeled out-of-band fold of the
            // OUTBOUND stream; this is its one mutation site.)
            match &input {
                WorkspaceInput::Enqueue { text } | WorkspaceInput::EnqueueNext { text } => {
                    model.queue.enqueue(text.clone(), model.now_ms);
                }
                // The first submission after a double-Esc
                // hold: front-insert, exactly as the
                // driver will (it runs before the prompt
                // the hold returned to the queue).
                WorkspaceInput::EnqueueFront { text } => {
                    model.queue.enqueue_front(text.clone(), model.now_ms);
                }
                WorkspaceInput::QueueRemove { index } => {
                    model.queue.remove(*index);
                }
                WorkspaceInput::QueueClear => model.queue.clear(),
                // Esc-with-something-to-say drains the
                // backlog into the running turn, so the
                // deck's mirror of it empties here — the
                // same one-mutation-site discipline every
                // other queue edit follows. Leaving the
                // rows up would show prompts as "waiting"
                // that are already in the model's hands.
                WorkspaceInput::Steer { .. } => model.queue.clear(),
                // `/clear`: a session reset to seq-0 has no
                // backlog — the transcript itself resets on
                // the driver's `Inbound::SessionReset`, so
                // stale in-flight events can never repaint
                // a pane the deck already blanked.
                WorkspaceInput::SessionClear => model.queue.clear(),
                _ => {}
            }
            let _ = submissions.send(input);
            false
        }
        DeckAction::Shell(cmd) => {
            // `$` commands run NOW — never queued, never
            // waiting on the engine. Output returns on the
            // local lane as ordinary events.
            //
            // It lands in the transcript the user is
            // reading — the focused agent's — so `$ pwd`
            // answers where they asked, like Claude Code.
            // Before any agent registers there is no such
            // lane, and only then does it fall back to the
            // synthetic one.
            debug.note(&format!("shell: {cmd}"));
            let target = focused_id(model, ui);
            spawn_shell_command(
                cmd,
                local_tx.clone(),
                model.now_ms,
                shell_active.clone(),
                target,
            );
            false
        }
        DeckAction::Handled | DeckAction::Ignored => false,
    }
}

/// The one wall-clock read in the deck's run loop. It runs once per tick
/// and is stored into [`WorkspaceModel::now_ms`] (`model.now_ms = now_ms()`
/// at the two call sites below) — nothing else reads the clock here. This
/// file is the I/O edge its own module doc names. Everything downstream —
/// the deck's rendering, staleness, and elapsed-time logic — takes
/// `model.now_ms` as plain data, the same parameter style
/// `self_driving.rs::liveness` uses. Many existing tests already set
/// `model.now_ms` to a fixed instant (`deck/tests.rs`, `deck_render/tests.rs`,
/// `views/*.rs`), so a fake clock is already honoured here. Routing
/// `now_ms()` itself through a parameter would only rename this one call.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Whether `key` is `⌃V`, the deck's explicit clipboard pull.
///
/// The run loop claims this chord *above* [`handle_deck_key`], because the
/// capture is blocking OS I/O the pure key layer cannot do. That makes it the
/// one binding in [`crate::keymap`] whose witness cannot press a key through
/// the dispatcher, so the predicate is a named function with a test rather
/// than a guard inlined into a `select!` arm (#4368).
fn is_clipboard_pull(key: crossterm::event::KeyEvent) -> bool {
    key.code == crossterm::event::KeyCode::Char('v')
        && key
            .modifiers
            .contains(crossterm::event::KeyModifiers::CONTROL)
}

/// Whether `key` is the bare spacebar — the push-to-talk hold key
/// (`crate::voice`, ADR 0020). Bare only: a modified space is a chord, and
/// chords are the pure key layer's to route. A named predicate with a
/// witness for the same reason [`is_clipboard_pull`] is one (#4368) — the
/// run loop consults it around the dispatcher, so no test can press it
/// *through* the dispatcher.
fn is_plain_space(key: crossterm::event::KeyEvent) -> bool {
    key.code == crossterm::event::KeyCode::Char(' ')
        && key.modifiers == crossterm::event::KeyModifiers::NONE
}

/// Whether normal dispatch of a plain space actually landed one space in the
/// main composer at the cursor — the observation `crate::voice` folds
/// instead of predicting the key-precedence chain (see its module docs). A
/// space claimed by a list, a toggle, or a modal sub-composer moves nothing
/// here and therefore never arms dictation.
fn space_landed_in_composer(composer: &Composer, before_len: usize, before_cursor: usize) -> bool {
    composer.buffer().len() == before_len + 1
        && composer.cursor() == before_cursor + 1
        && composer.buffer()[..composer.cursor()].ends_with(' ')
}

/// Run one `$` shell command **immediately** on the local event lane.
///
/// `target` is the lane the output belongs to. `Some(agent)` — the normal
/// case — is the lane the user is actually reading (the focused agent), so a
/// `$` command reads inline in the session transcript exactly like Claude
/// Code's bash mode. Those events go out as [`Inbound::ShellEvent`], which
/// folds into the transcript and nothing else; the lane gets no `Register`
/// (re-registering an existing agent overwrites its meta, renaming the
/// session to `$ cmd` and restyling it as a shell row) and no terminal
/// `Status` (which would clobber the real agent's own lifecycle).
///
/// `None` — only when the deck has no agent registered yet — falls back to
/// the synthetic [`SHELL_AGENT`] lane and its original shape: a `Register`
/// (idempotent — re-registering only refreshes the title to the latest
/// command), a `ToolStart`, and a `ToolResult` + terminal `Status`. Output is
/// never dropped just because no session lane exists yet.
///
/// Either way stdout and stderr are both captured; a non-zero exit reports as
/// a tool error. The TUI never blocks on the child — it runs on a spawned
/// task and reports back over `tx`.
///
/// `active` counts shell commands currently in flight on the shared
/// [`SHELL_AGENT`] lane. Because immediate `$` commands can overlap (a second
/// one dispatched before the first finishes), only the invocation that drains
/// the count to zero is allowed to park the lane with a terminal `Status` —
/// otherwise an earlier command finishing first would mark the lane
/// Done/Failed while a sibling command is still genuinely running.
fn spawn_shell_command(
    cmd: String,
    tx: UnboundedSender<Inbound>,
    started_ms: u64,
    active: Arc<AtomicUsize>,
    target: Option<AgentId>,
) {
    use stella_protocol::{AgentEvent, ToolCall, ToolOutput};

    // A synthetic lane owns its whole lifecycle (register + park); a real
    // lane owns none of it and only lends its transcript.
    let synthetic = target.is_none();
    let agent_id: AgentId = target.unwrap_or_else(|| SHELL_AGENT.to_string());
    // Route by lane kind: `ShellEvent` is transcript-only (safe for a live
    // agent), `Event` carries the full fold the synthetic lane still wants so
    // its row shows Running while the child is alive.
    let envelope = move |agent: AgentId, event: AgentEvent| {
        if synthetic {
            Inbound::Event { agent, event }
        } else {
            Inbound::ShellEvent { agent, event }
        }
    };

    // `started_ms` is the deck's tick clock (~33ms granularity), so two
    // overlapping `$` commands can share a timestamp — and the fold pairs
    // ToolResult to ToolStart by `call_id`, so a shared id mispairs their
    // rows. The process-wide counter makes the id unique regardless.
    static SHELL_CALL_SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SHELL_CALL_SEQ.fetch_add(1, Ordering::Relaxed);
    let call_id = format!("shell-{started_ms}-{seq}");
    active.fetch_add(1, Ordering::SeqCst);
    // Only the synthetic lane is registered. Sending this for a real agent
    // would overwrite its meta — `WorkspaceModel::register` replaces `meta`
    // wholesale on a known id — retitling the session `$ cmd`.
    if synthetic {
        let _ = tx.send(Inbound::Register(
            AgentMeta::new(SHELL_AGENT, format!("$ {cmd}"), started_ms).with_role("shell"),
        ));
    }
    let _ = tx.send(envelope(
        agent_id.clone(),
        AgentEvent::ToolStart {
            call: ToolCall {
                call_id: call_id.clone(),
                name: "shell".to_string(),
                input: serde_json::json!({ "cmd": cmd }),
            },
            sub_agent_id: None,
            task_id: None,
        },
    ));

    tokio::spawn(async move {
        let started = std::time::Instant::now();
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg(&cmd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A `$` command must not outlive the deck. `kill_on_drop` reaps the
            // child when this task's handle drops, which is what stops a
            // long-running command surviving deck exit. It is not a hard kill:
            // it only fires while the tokio runtime is still alive, and it
            // signals the direct child, not its whole process group.
            .kill_on_drop(true);
        // `$` commands execute user/repository-controlled shell text. Keep
        // normal task configuration but never inherit Stella/provider,
        // repository, cloud, or tracker credentials.
        scrub_shell_command(&mut command);
        let spawned = command.spawn();

        let (ok, content) = match spawned {
            Ok(mut child) => {
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                let mut out_buf = CappedOutput::new();
                let mut err_buf = CappedOutput::new();
                // Read both pipes and wait for exit concurrently — draining
                // stdout/stderr as they arrive (bounded, never fully
                // buffered) so neither pipe can back up and stall the child.
                let (_, _, status) = tokio::join!(
                    read_capped(stdout, &mut out_buf),
                    read_capped(stderr, &mut err_buf),
                    child.wait(),
                );
                let mut text = out_buf.finish();
                if !err_buf.is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&err_buf.finish());
                }
                let success = status.as_ref().map(|s| s.success()).unwrap_or(false);
                if text.trim().is_empty() {
                    let label = status
                        .map(|s| s.to_string())
                        .unwrap_or_else(|e| format!("error: {e}"));
                    text = format!("(no output — exit {label})");
                }
                (success, text)
            }
            Err(e) => (false, format!("failed to spawn `sh -c`: {e}")),
        };
        let output = if ok {
            ToolOutput::Ok {
                content,
                data: None,
            }
        } else {
            ToolOutput::classified_error(stella_protocol::ErrorClass::Environment, content)
        };
        let _ = tx.send(envelope(
            agent_id.clone(),
            AgentEvent::ToolResult {
                call_id,
                output,
                duration_ms: started.elapsed().as_millis() as u64,
                speculated: false,
                sub_agent_id: None,
                task_id: None,
            },
        ));
        // Park the lane so it never reads as still-working (a lingering
        // Running shell agent would keep the spinner alive forever) — but
        // only once this was the last command in flight; `fetch_sub` returns
        // the pre-decrement count, so `1` means we just brought it to zero.
        // Only the synthetic lane is ever parked: a real agent's status is
        // the engine's to own, and stamping Done/Failed on it here would
        // report the shell command's exit as the agent's own outcome.
        let last = active.fetch_sub(1, Ordering::SeqCst) == 1;
        if last && synthetic {
            let _ = tx.send(Inbound::Status {
                agent: agent_id,
                status: if ok {
                    AgentStatus::Done
                } else {
                    AgentStatus::Failed
                },
            });
        }
    });
}

fn scrub_shell_command(command: &mut tokio::process::Command) {
    stella_tools::subprocess_env::scrub_sensitive_env(command);
}

/// Streams a piped child stream into `buf`, chunk by chunk, so output is
/// capped as it arrives rather than fully buffered first.
async fn read_capped<R: tokio::io::AsyncRead + Unpin>(stream: Option<R>, buf: &mut CappedOutput) {
    let Some(mut stream) = stream else { return };
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.push(&chunk[..n]),
        }
    }
}

/// Bounded middle-out accumulator for shell output: keeps a head window (up
/// to [`SHELL_OUTPUT_CAP`] bytes) and a sliding tail window (the last
/// `SHELL_OUTPUT_CAP / 2` bytes seen), so memory use stays capped regardless
/// of how much a verbose command actually writes — unlike buffering the full
/// stream and truncating only afterward. Errors live at the tail, matching
/// [`spawn_shell_command`]'s stdout-then-stderr ordering.
struct CappedOutput {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: usize,
}

impl CappedOutput {
    fn new() -> Self {
        Self {
            head: Vec::new(),
            tail: VecDeque::new(),
            total: 0,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.total += chunk.len();
        if self.head.len() < SHELL_OUTPUT_CAP {
            let take = (SHELL_OUTPUT_CAP - self.head.len()).min(chunk.len());
            self.head.extend_from_slice(&chunk[..take]);
        }
        let half = SHELL_OUTPUT_CAP / 2;
        for &b in chunk {
            if self.tail.len() >= half {
                self.tail.pop_front();
            }
            self.tail.push_back(b);
        }
    }

    fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// Renders as a full-buffer-then-truncate implementation would have:
    /// unchanged if it fit, otherwise head + elision marker + tail.
    fn finish(self) -> String {
        if self.total <= SHELL_OUTPUT_CAP {
            return String::from_utf8_lossy(&self.head).into_owned();
        }
        let half = SHELL_OUTPUT_CAP / 2;
        let head = String::from_utf8_lossy(&self.head[..half.min(self.head.len())]).into_owned();
        let tail_bytes: Vec<u8> = self.tail.into_iter().collect();
        let tail = String::from_utf8_lossy(&tail_bytes).into_owned();
        format!("{head}\n…[output truncated]…\n{tail}")
    }
}

/// Most envelopes [`drain_inbound`] folds into one frame. High enough that an
/// ordinary streaming burst never spills into a second draw, low enough that a
/// pathological producer cannot starve the key reader for a whole frame.
const INBOUND_COALESCE_CAP: usize = 512;

/// Fold every envelope already queued on `rx`, up to [`INBOUND_COALESCE_CAP`],
/// without awaiting. Returns `false` when the stream closed mid-drain (the
/// caller's cue to end the session), `true` otherwise.
///
/// This is what keeps the deck's frame rate independent of the engine's event
/// rate: the folds are O(1)-ish each, the draw is the expensive part, and one
/// draw can present any number of folded events.
fn drain_inbound(
    rx: &mut UnboundedReceiver<Inbound>,
    model: &mut WorkspaceModel,
    ui: &mut DeckUi,
) -> bool {
    use tokio::sync::mpsc::error::TryRecvError;
    for _ in 0..INBOUND_COALESCE_CAP {
        match rx.try_recv() {
            Ok(ev) => ingest_inbound(&ev, model, ui),
            Err(TryRecvError::Empty) => return true,
            Err(TryRecvError::Disconnected) => return false,
        }
    }
    true
}

/// The deck's terminal, and whether it got an inline viewport.
///
/// An ordinary session takes the full viewport on the alternate screen and is
/// never inline. An accessible session asks for an inline one, because that is
/// what leaves the rows above it alone — which is what makes `insert_before`,
/// and therefore the scrollback path, possible at all.
///
/// Anchoring an inline viewport means writing a Device Status Report and
/// **blocking** until the terminal answers with its cursor position. Every real
/// emulator answers; some minimal ones and most test harnesses do not, and the
/// read times out. Found the hard way in #1237: unguarded, that makes the
/// surface fail to start outright. So it degrades — to a full-viewport draw on
/// the user's OWN screen, never to the alternate one, because the alternate
/// screen is the thing accessible mode exists to avoid.
fn open_terminal(
    accessible: bool,
    debug: &DebugLog,
) -> io::Result<(Terminal<CrosstermBackend<io::Stdout>>, bool)> {
    if !accessible {
        return Ok((Terminal::new(CrosstermBackend::new(io::stdout()))?, false));
    }
    let rows =
        accessible::inline_viewport_rows(crossterm::terminal::size().map_or(24, |(_, rows)| rows));
    let options = TerminalOptions {
        viewport: Viewport::Inline(rows),
    };
    match Terminal::with_options(CrosstermBackend::new(io::stdout()), options) {
        Ok(terminal) => Ok((terminal, true)),
        Err(error) => {
            debug.note(&format!(
                "inline viewport unavailable ({error}); drawing full-screen without scrollback"
            ));
            Ok((Terminal::new(CrosstermBackend::new(io::stdout()))?, false))
        }
    }
}

/// Move every newly-settled transcript entry — and every queued announcement —
/// out of the repainting viewport and into the terminal's scrollback.
///
/// This is the whole accessibility mechanism. `insert_before` writes the lines
/// **above** the inline viewport, so they are ordinary terminal output: a
/// screen reader announces them once as they arrive, the reader's review cursor
/// can walk back through them, and the user's own scrollback keys reach them.
/// The alternative — leaving them in a pane that repaints every frame — gives a
/// reader a rectangle of cells that changes wholesale several times a second
/// and no notion of "a new line appeared".
///
/// The plan/record split is the invariant (see [`crate::accessible`]): the
/// counter moves only after the write returned `Ok`, one block at a time, so a
/// mid-flush failure leaves the un-written remainder still owned by the live
/// pane rather than lost between the two.
fn flush_scrollback(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    model: &WorkspaceModel,
    ui: &mut DeckUi,
    color_mode: theme::ColorMode,
    include_trailing: bool,
) -> io::Result<()> {
    if !ui.scrollback.is_live() {
        return Ok(());
    }
    let width = terminal.size()?.width;
    // Announcements first: they say where the session now *is*, so they must
    // precede whatever landed after the move.
    let notes: Vec<Line<'static>> = ui
        .scrollback
        .take_announcements()
        .into_iter()
        .map(Line::from)
        .collect();
    write_scrollback(terminal, notes, color_mode)?;

    for block in ui.scrollback.plan(model, include_trailing) {
        let lines = accessible::block_lines(model, &block, ui.thinking_expanded, width as usize);
        write_scrollback(terminal, lines, color_mode)?;
        // Only now — the lines really are in the terminal.
        ui.scrollback.record(&block);
    }
    Ok(())
}

/// `insert_before` for one already-rendered run of lines, themed exactly like
/// the live pane so a flushed row and a painted one are the same row.
fn write_scrollback(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    lines: Vec<Line<'static>>,
    color_mode: theme::ColorMode,
) -> io::Result<()> {
    let height = match u16::try_from(lines.len()) {
        Ok(0) => return Ok(()),
        Ok(h) => h,
        // A single settled entry cannot plausibly wrap to 65k rows, but the
        // cast must not wrap around into a tiny region if it ever did.
        Err(_) => u16::MAX,
    };
    terminal.insert_before(height, |buf| {
        Paragraph::new(Text::from(lines)).render(buf.area, buf);
        theme::apply_theme(buf, color_mode);
        theme::degrade_buffer(buf, color_mode);
    })
}

/// Run the command deck to completion. [`Inbound`] envelopes stream in over
/// `inbound`; the user's [`WorkspaceInput`]s stream out over `submissions`.
/// Returns when the inbound stream closes or the user quits, having always
/// restored the terminal first. Returns an error when the key reader stops on
/// its own — the terminal could not be read, or stdin closed — so the driver
/// can say why the deck ended instead of reporting a quit.
pub async fn run_deck(
    opts: DeckOptions,
    mut inbound: UnboundedReceiver<Inbound>,
    submissions: UnboundedSender<WorkspaceInput>,
) -> io::Result<()> {
    let debug = DebugLog::new(opts.debug_log_path.clone());
    debug.note("deck session start");

    // The hook shares the guard's state so a panic restores the terminal even
    // in abort builds, where Drop never runs (see `crate::term`).
    // An accessible session owns both of these — see `accessible::screen_for`
    // and `accessible::mouse_capture_enabled` for why neither is negotiable.
    let guard = TerminalGuard::enter(
        accessible::mouse_capture_enabled(opts.mouse_capture, opts.accessible),
        accessible::screen_for(opts.accessible),
    )?;
    let _hook_guard = PanicHookGuard::install(opts.debug_log_path.clone(), &guard);
    // `inline` is the required bit, not `opts.accessible`: it records
    // whether the inline viewport was actually obtained, and the whole
    // scrollback path is gated on it (see `crate::accessible`).
    let (mut terminal, inline) = open_terminal(opts.accessible, &debug)?;
    // Detected once (see `theme::color_mode`) and threaded through the draw loop
    // below, rather than touching every `theme::TOKEN` call site in
    // `deck_render.rs`/the view modules.
    let color_mode = theme::detect_color_mode();
    // Motion off-switch for CI/recording: the explicit `--no-anim`, its env
    // synonym, or `NO_COLOR` (a recording context wants a static frame). Gates
    // the progress shimmer / pulse / caret blink; the deck otherwise only ever
    // runs on a TTY, so no additional TTY check is needed.
    //
    // Accessible mode forces it: inline on the user's own screen, a statline
    // and a progress bar repainting on a 30fps clock are a live region a
    // reader may keep picking up, and a frozen frame is a quiet one.
    let no_anim = opts.no_anim
        || opts.accessible
        || std::env::var_os("STELLA_NO_ANIM").is_some()
        || color_mode == theme::ColorMode::None;

    let mut model = WorkspaceModel::new();
    model.now_ms = now_ms();
    let mut ui = DeckUi::new(Composer::with_paste_threshold(
        crate::composer::DECK_PASTE_LINE_THRESHOLD,
    ));
    ui.graph = opts.initial_graph.clone();
    ui.slash_commands = opts.slash_commands.clone();
    if let Some(path) = opts.recent_path.as_ref() {
        ui.composer.keep_recent_in(path);
    }
    ui.color_mode = color_mode;
    ui.no_anim = no_anim;
    ui.accessible = opts.accessible;
    ui.mid_turn_prompt = opts.mid_turn_prompt;
    ui.scrollback.set_live(inline);
    // A degraded accessible session must say so. The whole promise of the mode
    // is that finished messages become durable terminal output; if the inline
    // viewport could not be anchored they do not, and a user who scrolls back
    // to re-read an answer that is not there has been told a silent lie. It
    // rides the deck's own startup-notice channel because the shell owns the
    // screen from here on — a `println!` would be painted over by the first
    // frame, and with no scrollback there is nowhere else for it to go.
    if let Some(notice) = accessible::degrade_notice(opts.accessible, inline) {
        ui.notice.push(notice);
    }
    // A no-anim session lights the launch mark immediately, with no reveal
    // step (and `ingest_inbound` drops any later replay cues).
    ui.splash.set_reduced(no_anim);
    // Enter semantics follow the terminal's actual capability (see
    // `crate::term::TerminalGuard::kitty` and `crate::composer::classify_enter`).
    ui.enter_submits = !guard.kitty();
    // Push-to-talk: enablement is the caller's (settings), release reporting
    // is the terminal's (the same push as the Enter semantics above).
    ui.voice.enabled = opts.voice_enabled;
    ui.voice.mode = opts.voice_mode;
    ui.voice.release_events = guard.kitty();
    let mut resources = ResourceMonitor::new();

    // Synthetic-event lane for `$` shell commands: spawned commands report
    // back here and are folded exactly like engine events. The sender lives
    // for the whole loop, so this arm never closes it.
    let (local_tx, mut local_rx) = tokio::sync::mpsc::unbounded_channel::<Inbound>();
    // `⌃V` results return here: the OS clipboard round-trip (and the PNG
    // encode + attachment write behind an image) is blocking I/O that can
    // stall for hundreds of ms on some platforms, so it runs on the blocking
    // pool instead of freezing the ~30fps draw loop. Same lifetime contract
    // as `local_tx`.
    let (clip_tx, mut clip_rx) =
        tokio::sync::mpsc::unbounded_channel::<Result<crate::clipboard::ClipboardPaste, String>>();
    // Shared in-flight count for overlapping `$` commands (see
    // `spawn_shell_command`) — persists across every dispatch this loop makes.
    let shell_active = Arc::new(AtomicUsize::new(0));

    // Blocking crossterm reader → async loop, with a shutdown flag. The
    // reader reports why it stopped, and the loop returns that report
    // (`crate::key_reader`) — a lane that closes is never read as a quit.
    let shutdown = Arc::new(AtomicBool::new(false));
    let (reader, mut key_rx) = crate::key_reader::spawn(shutdown.clone());
    // Why the loop ended, when it ended on something other than a quit or
    // the engine closing the stream. Returned after the terminal is restored.
    let mut ended: io::Result<()> = Ok(());

    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // `local_tx`/`clip_tx` outlive the loop, so those lanes should never close
    // — but if one ever did, `select!` would see `Ready(None)` on every poll
    // and spin the deck's draw loop at 100% CPU. Enforce the invariant instead
    // of asserting it in prose: disable the branch the moment it closes,
    // exactly as `fleet_dashboard`'s `keys_open` guard does for its key reader.
    let mut local_open = true;
    let mut clip_open = true;

    'run: loop {
        // Requests queued by handlers/ingest beyond their single return value
        // (a CONTEXT open refreshing two snapshots, a finished OAuth login
        // refreshing the MCP tab). Drained before the draw so a request made
        // by the branch below it never waits an extra loop turn.
        for input in ui.pending_inputs.drain(..) {
            let _ = submissions.send(input);
        }

        // The panel loop's outbound half (SPEC 12.4): each seated panel that
        // has been drawn once and has no request outstanding asks the driver
        // for its next frame, against the rectangle the last draw measured.
        // Here rather than inside the draw because the draw is a pure
        // projection — the ask is a message, and the answer arrives as an
        // ordinary `Inbound` on the next turn of this loop.
        for input in ui.panels.requests() {
            let _ = submissions.send(input);
        }

        // Settled history leaves the repainting viewport and becomes ordinary
        // terminal output BEFORE the draw, so the pane never paints a line
        // that is already in scrollback. No-op unless an inline viewport was
        // obtained.
        flush_scrollback(&mut terminal, &model, &mut ui, color_mode, false)?;
        terminal.draw(|f| {
            render_deck(&model, &mut ui, f);
            theme::apply_theme(f.buffer_mut(), color_mode);
            theme::degrade_buffer(f.buffer_mut(), color_mode);
        })?;

        tokio::select! {
            maybe_inbound = inbound.recv() => {
                match maybe_inbound {
                    Some(ev) => {
                        ingest_inbound(&ev, &mut model, &mut ui);
                        // Coalesce the burst behind it. A streaming turn emits
                        // one `TextDelta` per token, and the loop draws once
                        // per iteration — so without this a fast stream cost
                        // one full-frame repaint *per token*, which is both
                        // the deck's worst frame rate and its worst input
                        // latency (a keystroke waits behind every one of those
                        // draws). Folding is cheap; drawing is not. The tick
                        // arm already guarantees the ~30fps floor, so nothing
                        // goes unseen for longer than a frame.
                        if !drain_inbound(&mut inbound, &mut model, &mut ui) {
                            break 'run;
                        }
                    }
                    // The engine closed the stream — session over.
                    None => break 'run,
                }
            }
            maybe_key = key_rx.recv() => {
                let event = match crate::key_reader::event(maybe_key) {
                    Ok(event) => event,
                    // The reader stopped — a terminal crossterm could not
                    // parse, a stdin that closed, a thread that died. The
                    // session ends as it did before, and now says why.
                    Err(error) => {
                        debug.note(&error.to_string());
                        ended = Err(error);
                        break 'run;
                    }
                };
                match event {
                    // `⌃V`: explicit clipboard pull — the only way a *bitmap*
                    // (a copied screenshot) reaches the deck, since bracketed
                    // paste never carries one. The image payload is stored
                    // under `.stella/attachments/` and its path is pasted as
                    // text: the deck's prompt queue is text-shaped, and the
                    // driver extracts media paths into attachments at
                    // dispatch. Clipboard text falls back to a normal paste.
                    // The capture itself is blocking OS I/O, so it runs on
                    // the blocking pool and the result returns on `clip_rx` —
                    // the draw loop never waits on the clipboard.
                    Event::Key(key)
                        if key.kind != KeyEventKind::Release && is_clipboard_pull(key) =>
                    {
                        let tx = clip_tx.clone();
                        tokio::task::spawn_blocking(move || {
                            let _ = tx.send(crate::clipboard::capture(
                                &crate::clipboard::default_attachments_dir(),
                            ));
                        });
                    }
                    // A reported key release reaches only the voice fold —
                    // the dispatcher's no-Release contract is unchanged. On
                    // terminals without `REPORT_EVENT_TYPES` this arm never
                    // fires and the repeat-gap fallback in the tick arm ends
                    // the hold instead (`crate::voice`).
                    Event::Key(key) if key.kind == KeyEventKind::Release => {
                        if is_plain_space(key) {
                            let cmd = ui.voice.space_release(model.now_ms);
                            apply_voice_cmd(cmd, &mut ui, &submissions);
                        }
                    }
                    // While recording, a space is never a character. In hold
                    // mode it is a repeat saying the key is still down; in
                    // tap mode it is the second tap, which ends the capture
                    // (`crate::voice::VoiceUi::swallowed_space`). Esc
                    // abandons the capture in both.
                    Event::Key(key) if ui.voice.swallows_space() && is_plain_space(key) => {
                        let cmd = ui.voice.swallowed_space(model.now_ms);
                        apply_voice_cmd(cmd, &mut ui, &submissions);
                    }
                    Event::Key(key)
                        if ui.voice.esc_cancels()
                            && key.code == crossterm::event::KeyCode::Esc =>
                    {
                        if ui.voice.cancel() == crate::voice::VoiceCmd::Cancel {
                            let _ = submissions.send(WorkspaceInput::VoiceCancel);
                        }
                    }
                    Event::Key(key) => {
                        // Snapshot for the voice observation below: dispatch
                        // decides where the key goes, and the composer's own
                        // growth says whether a plain space became text.
                        let space = is_plain_space(key);
                        let before_len = ui.composer.buffer().len();
                        let before_cursor = ui.composer.cursor();
                        let action = handle_deck_key(key, &model, &mut ui);
                        if apply_deck_action(
                            action,
                            &mut model,
                            &mut ui,
                            &submissions,
                            &local_tx,
                            &shell_active,
                            &debug,
                        ) {
                            break 'run;
                        }
                        // A dispatched palette row changed the `recent` list;
                        // every other keystroke leaves this a no-op.
                        ui.composer.flush_recent();
                        // The voice observation (`crate::voice`): a plain
                        // space that really landed in the composer extends
                        // the hold; any other key says the user is typing.
                        if space
                            && space_landed_in_composer(&ui.composer, before_len, before_cursor)
                        {
                            // Whether that space met an *empty* composer is
                            // what arms tap mode, and only dispatch knows it
                            // — hence the snapshot above rather than a guess
                            // inside the machine.
                            let cmd =
                                ui.voice.typed_space(model.now_ms, before_len == 0);
                            apply_voice_cmd(cmd, &mut ui, &submissions);
                        } else if !space {
                            ui.voice.interrupt();
                        }
                    }
                    // Bracketed paste: the whole paste arrives as one event
                    // (the guard enabled it), so the composer can fold it
                    // into a chip instead of replaying N raw Enter keys —
                    // each of which would have submitted a separate prompt.
                    // Routed by the UI: the agent-definition editor claims
                    // it while open (`DeckUi::paste`).
                    Event::Paste(text) => {
                        ui.paste(&text);
                        // A paste is typing, not holding.
                        ui.voice.interrupt();
                    }
                    // Any mouse event dismisses the startup notice, exactly as
                    // any key does, then dispatches (tab-row clicks, wheel
                    // scroll — `crate::mouse`). This arm only fires for
                    // sessions that opted in via `DeckOptions::mouse_capture`
                    // (L-T2 — capture takes the terminal's own text selection
                    // away); on a default session the keypress and the dwell
                    // timer are the notice's dismissal paths, and no click
                    // ever reaches the process. The outcome is applied by the
                    // same `apply_deck_action` a key's is: a wheel notch over
                    // a modal overlay re-enters the key dispatch (synthesized
                    // arrows), so its action can be anything a key's can.
                    Event::Mouse(mouse) => {
                        ui.notice.dismiss();
                        let width = terminal.size()?.width;
                        let action = crate::mouse::handle_deck_mouse(mouse, width, &model, &mut ui);
                        if apply_deck_action(
                            action,
                            &mut model,
                            &mut ui,
                            &submissions,
                            &local_tx,
                            &shell_active,
                            &debug,
                        ) {
                            break 'run;
                        }
                    }
                    // Resize / focus change: the next draw picks them up.
                    _ => {}
                }
            }
            maybe_local = local_rx.recv(), if local_open => {
                // Shell-command lane (see `spawn_shell_command`).
                match maybe_local {
                    Some(ev) => {
                        ingest_inbound(&ev, &mut model, &mut ui);
                        // Coalesced like the engine lane above — a chatty `$`
                        // command must not cost one repaint per event either.
                        let _ = drain_inbound(&mut local_rx, &mut model, &mut ui);
                    }
                    // Unreachable while `local_tx` is held above; stop
                    // selecting on it rather than spinning if it ever isn't.
                    None => local_open = false,
                }
            }
            maybe_clip = clip_rx.recv(), if clip_open => {
                // A finished `⌃V` capture — applied exactly as the old
                // in-loop capture was, just a beat later. Repeated `⌃V`s
                // apply in press order (the channel is FIFO).
                match maybe_clip {
                    Some(Ok(crate::clipboard::ClipboardPaste::Image(att))) => {
                        debug.note(&format!("clipboard image stored: {}", att.label()));
                        if let stella_protocol::AttachmentSource::Path { path } = &att.source {
                            ui.paste(&format!("{path} "));
                        }
                    }
                    Some(Ok(crate::clipboard::ClipboardPaste::Text(text))) => ui.paste(&text),
                    Some(Ok(crate::clipboard::ClipboardPaste::Empty)) => {}
                    Some(Err(err)) => debug.note(&err),
                    // Unreachable while `clip_tx` is held above; stop
                    // selecting on it rather than spinning if it ever isn't.
                    None => clip_open = false,
                }
            }
            _ = tick.tick() => {
                // The heartbeat: advance the clock and re-sample resources so
                // gauges, elapsed timers, sparklines, and effects stay live.
                model.now_ms = now_ms();
                resources.sample(&mut model);
                // Push-to-talk timing (`crate::voice`): the warmup completes
                // here — retracting exactly the spaces the arming run typed —
                // and, without release reporting, a quiet repeat stream ends
                // the recording here too.
                let cmd = ui.voice.tick(model.now_ms);
                apply_voice_cmd(cmd, &mut ui, &submissions);
            }
        }
    }

    shutdown.store(true, Ordering::Relaxed);
    let _ = reader.join();
    // The last flush includes the trailing entry of every lane: nothing can
    // coalesce into it any more, and leaving it behind would end the session
    // with its final answer in a pane that is about to stop being redrawn.
    // Best-effort — a failed write here must not turn a clean quit into an
    // error exit, and the debug log is where a lost flush is diagnosable.
    if let Err(error) = flush_scrollback(&mut terminal, &model, &mut ui, color_mode, true) {
        debug.note(&format!("final scrollback flush failed: {error}"));
    }
    debug.note("deck session end");
    ended
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The witness for `ctrl-v` (#4368).** The run loop's clipboard arm
    /// fires on `⌃V` and on nothing else — a bare `v` is the next character
    /// of a prompt, and `⌥V` / `⌘V` are the terminal's own paste, which
    /// arrives as bracketed paste rather than a key.
    #[test]
    fn ctrl_v_is_the_clipboard_pull_and_a_bare_v_is_not() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        assert!(is_clipboard_pull(KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_clipboard_pull(KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::NONE
        )));
        assert!(!is_clipboard_pull(KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::ALT
        )));
        assert!(!is_clipboard_pull(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
    }

    /// **The witness for the push-to-talk hold key.** Bare Space and nothing
    /// else: a modified space is a chord for the pure key layer, and every
    /// other key is typing (which aborts an arming run).
    #[test]
    fn a_bare_space_is_the_push_to_talk_key_and_a_modified_space_is_not() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        assert!(is_plain_space(KeyEvent::new(
            KeyCode::Char(' '),
            KeyModifiers::NONE
        )));
        assert!(!is_plain_space(KeyEvent::new(
            KeyCode::Char(' '),
            KeyModifiers::CONTROL
        )));
        assert!(!is_plain_space(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::NONE
        )));
    }

    /// The voice fold consumes what dispatch *did*, not what the key was: a
    /// space the composer actually absorbed extends the hold, and a space a
    /// list or toggle claimed must not (`crate::voice`'s module docs).
    #[test]
    fn the_voice_observation_requires_a_space_that_actually_landed() {
        let mut composer = Composer::with_paste_threshold(48);
        composer.insert_char('h');
        let (len, cur) = (composer.buffer().len(), composer.cursor());

        // Claimed elsewhere: the composer did not move.
        assert!(!space_landed_in_composer(&composer, len, cur));

        // A non-space insertion is typing, not holding.
        composer.insert_char('i');
        assert!(!space_landed_in_composer(&composer, len, cur));

        let (len, cur) = (composer.buffer().len(), composer.cursor());
        composer.insert_char(' ');
        assert!(space_landed_in_composer(&composer, len, cur));
    }

    #[tokio::test]
    async fn shell_child_cannot_receive_explicit_credentials() {
        let mut command = tokio::process::Command::new("sh");
        command
            .args([
                "-c",
                "printf '%s|%s|%s|%s' \"${OPENROUTER_API_KEY-unset}\" \"${GITHUB_TOKEN-unset}\" \"${AWS_SECRET_ACCESS_KEY-unset}\" \"${STELLA_TEST_BENIGN-unset}\"",
            ])
            .env("OPENROUTER_API_KEY", "provider-secret")
            .env("GITHUB_TOKEN", "repository-secret")
            .env("AWS_SECRET_ACCESS_KEY", "cloud-secret")
            .env("STELLA_TEST_BENIGN", "visible");
        scrub_shell_command(&mut command);

        let output = command.output().await.unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "unset|unset|unset|visible"
        );
    }

    /// The draw loop draws once per iteration, so a burst that arrives faster
    /// than a frame must be folded in one pass. Without this the deck repainted
    /// once per streamed token — its worst frame rate and its worst input
    /// latency at exactly the moment a user is watching it work.
    #[test]
    fn a_burst_of_envelopes_folds_in_one_pass() {
        use stella_protocol::AgentEvent;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for n in 0..200 {
            tx.send(Inbound::Event {
                agent: "lead".into(),
                event: AgentEvent::TextDelta {
                    delta: format!("{n} "),
                },
            })
            .unwrap();
        }
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "t", 0)));
        let mut ui = DeckUi::default();

        assert!(drain_inbound(&mut rx, &mut model, &mut ui));
        let streamed = &model.agents[0].model.streaming_text;
        assert!(
            streamed.starts_with("0 1 ") && streamed.ends_with("199 "),
            "every queued delta folded in the one pass: {streamed:?}"
        );
        assert!(rx.try_recv().is_err(), "the queue is drained");
    }

    #[test]
    fn a_drain_reports_a_closed_stream_instead_of_swallowing_it() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Inbound>();
        drop(tx);
        let mut model = WorkspaceModel::new();
        let mut ui = DeckUi::default();
        assert!(
            !drain_inbound(&mut rx, &mut model, &mut ui),
            "a hangup mid-drain must end the session, not spin the loop"
        );
    }

    #[test]
    fn capped_output_passes_short_text_through_unchanged() {
        let mut buf = CappedOutput::new();
        buf.push(b"fits");
        assert_eq!(buf.finish(), "fits");
    }

    #[test]
    fn capped_output_keeps_head_and_tail_when_truncated() {
        let mut buf = CappedOutput::new();
        buf.push(b"HEAD");
        buf.push(&vec![b'x'; SHELL_OUTPUT_CAP * 2]);
        buf.push(b"TAIL");
        let out = buf.finish();
        assert!(out.starts_with("HEAD"), "{out}");
        assert!(out.ends_with("TAIL"), "{out}");
        assert!(out.contains("[output truncated]"), "{out}");
    }

    #[test]
    fn capped_output_bounds_memory_regardless_of_input_size() {
        // The whole point of streaming with a bounded accumulator: pushing
        // far more than the cap must not grow internal storage past it,
        // unlike collecting the full output before truncating.
        let mut buf = CappedOutput::new();
        let chunk = vec![b'x'; 8192];
        for _ in 0..64 {
            buf.push(&chunk);
        }
        assert!(buf.head.len() <= SHELL_OUTPUT_CAP);
        assert!(buf.tail.len() <= SHELL_OUTPUT_CAP / 2);
        assert!(buf.finish().contains("[output truncated]"));
    }

    #[test]
    fn shell_commands_report_on_the_local_lane() {
        // The spawner's synchronous part: Register + ToolStart land on the
        // channel immediately, before the child even runs.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let active = Arc::new(AtomicUsize::new(0));
        spawn_shell_command("echo hi".into(), tx, 42, active, None);
        match rx.try_recv() {
            Ok(Inbound::Register(meta)) => {
                assert_eq!(meta.id, SHELL_AGENT);
                assert!(meta.title.contains("echo hi"));
            }
            other => panic!("expected Register first, got {other:?}"),
        }
        match rx.try_recv() {
            Ok(Inbound::Event { agent, .. }) => assert_eq!(agent, SHELL_AGENT),
            other => panic!("expected ToolStart second, got {other:?}"),
        }
        // The async completion (ToolResult + terminal Status) needs the
        // runtime to run the child; the sync part above is the determinism
        // this test pins, so completion just needs to arrive eventually.
        rt.block_on(async {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await;
        });
    }

    #[test]
    fn a_targeted_shell_command_borrows_the_lane_without_registering_or_parking_it() {
        // The real-lane shape: no `Register` (it would overwrite the agent's
        // meta) and no terminal `Status` (it would report the command's exit
        // as the agent's own outcome). Just transcript-only `ShellEvent`s.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let active = Arc::new(AtomicUsize::new(0));
        spawn_shell_command("echo hi".into(), tx, 42, active, Some("lead".into()));

        match rx.try_recv() {
            Ok(Inbound::ShellEvent { agent, .. }) => assert_eq!(agent, "lead"),
            other => panic!("expected a ShellEvent ToolStart first, got {other:?}"),
        }
        // Drain the async completion, then assert on everything that landed.
        rt.block_on(async {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await;
        });
        let mut rest = Vec::new();
        while let Ok(inbound) = rx.try_recv() {
            rest.push(inbound);
        }
        assert!(
            !rest.iter().any(|i| matches!(i, Inbound::Register(_))),
            "a real lane is never re-registered: {rest:?}"
        );
        assert!(
            !rest.iter().any(|i| matches!(i, Inbound::Status { .. })),
            "a real lane is never parked by a `$` command: {rest:?}"
        );
    }

    #[test]
    fn overlapping_shell_commands_within_one_tick_get_distinct_call_ids() {
        // Two `$` commands dispatched inside the same 33ms tick share
        // `started_ms`; the fold pairs ToolResult to ToolStart by `call_id`,
        // so the ids must differ anyway or the rows mispair.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let active = Arc::new(AtomicUsize::new(0));
        spawn_shell_command("echo one".into(), tx.clone(), 42, active.clone(), None);
        spawn_shell_command("echo two".into(), tx, 42, active, None);

        let mut call_ids = Vec::new();
        while let Ok(inbound) = rx.try_recv() {
            if let Inbound::Event {
                event: stella_protocol::AgentEvent::ToolStart { call, .. },
                ..
            } = inbound
            {
                call_ids.push(call.call_id);
            }
        }
        assert_eq!(call_ids.len(), 2, "both ToolStarts land synchronously");
        assert_ne!(
            call_ids[0], call_ids[1],
            "a shared timestamp must not produce a shared call_id"
        );
    }

    #[test]
    fn overlapping_shell_commands_only_park_the_lane_once_the_last_finishes() {
        // Two `$` commands dispatched before either finishes share the same
        // SHELL_AGENT lane. The fast one (`echo`) must not send a terminal
        // Status while the slow one (`sleep`) is still running — only the
        // last to finish may park the lane.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let active = Arc::new(AtomicUsize::new(0));
        spawn_shell_command("echo fast".into(), tx.clone(), 1, active.clone(), None);
        spawn_shell_command("sleep 0.2 && echo slow".into(), tx, 2, active, None);

        rt.block_on(async {
            let mut statuses = Vec::new();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                match tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
                {
                    Ok(Some(Inbound::Status { status, .. })) => statuses.push(status),
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(_) => continue,
                }
            }
            assert_eq!(
                statuses.len(),
                1,
                "exactly one terminal Status should be sent for two overlapping commands: {statuses:?}"
            );
        });
    }
}
