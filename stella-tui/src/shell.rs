//! The interactive shell: terminal setup/teardown, the crossterm event loop,
//! and channel plumbing. **Deliberately thin** — every decision (key→action,
//! event→state) lives in the pure, unit-tested layers ([`crate::ui`],
//! [`crate::model`], [`mod@crate::render`]); this file only wires them to real I/O
//! and is the one part not covered by unit tests (its integration smoke test
//! is `#[ignore]`d because it needs a TTY).
//!
//! ## Terminal restoration & signals (L-L1, L-T2)
//!
//! `TerminalGuard` enables raw mode + the alternate screen on entry and
//! **always** restores them on drop — including during a panic unwind, so a
//! crash never leaves the user's terminal wedged. Mouse capture is **off by
//! default** (L-T2): native terminal text selection/copy keeps working; it is
//! opt-in via [`RunOptions::mouse_capture`].
//!
//! In raw mode Ctrl-C is delivered as a *key event*, not a `SIGINT`, so we
//! handle it in-band as a clean cancel + quit — we own no native library locks
//! here, so there is nothing to drain and no signal to re-raise (the L-L1
//! re-raise discipline applies to the engine/context runtimes that hold
//! SQLite/ONNX handles, not to this render loop). A genuine `SIGINT` received
//! while *not* in raw mode keeps its default disposition.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{self, Event, KeyEventKind};
use ratatui::backend::CrosstermBackend;
use ratatui::text::{Line, Text};
use ratatui::widgets::{Paragraph, Widget};
use ratatui::{Terminal, TerminalOptions, Viewport};
use stella_protocol::AgentEvent;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::composer::{Composer, SlashCommand};
use crate::input::UserInput;
use crate::model::SessionModel;
use crate::render::render;
use crate::term::{PanicHookGuard, Screen, TerminalGuard};
use crate::theme;
use crate::ui::{ShellAction, UiState, handle_key, ingest};

/// Configuration for one interactive session.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// The slash-command vocabulary shown by the composer's `/` menu.
    pub slash_commands: Vec<SlashCommand>,
    /// Line threshold above which a paste collapses to a chip (L-T3).
    pub paste_line_threshold: usize,
    /// When `Some`, structured event/log lines are appended here (L-T8). The
    /// CLI wires the real `~/.local/state/stella/logs/` path when
    /// `STELLA_DEBUG=1`; taking it as an option keeps this crate decoupled
    /// from that location.
    pub debug_log_path: Option<PathBuf>,
    /// Enable mouse capture. **Off by default** so native text selection
    /// keeps working (L-T2) — turn on only for an explicitly mouse-driven,
    /// keyboard-degradable feature.
    pub mouse_capture: bool,
    /// Run the surface the way a screen reader can actually read it. Set by
    /// `stella --simple` (#936); see [`run`] for what it changes and
    /// [`crate::term::Screen`] for why the alternate screen is the root of
    /// the problem.
    ///
    /// Three things follow from it, and they are one decision, not three:
    /// the session draws **inline** on the user's own screen instead of on
    /// the alternate one; each settled transcript entry is written into the
    /// terminal's scrollback as ordinary output, exactly once; and the panels
    /// stack in a single column so reading order matches document order
    /// ([`crate::ui::UiState::screen_reader`]). Mouse capture is forced off
    /// regardless of [`Self::mouse_capture`] — a captured mouse takes the
    /// selection away from the assistive technology that needs it.
    pub screen_reader: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            slash_commands: Vec::new(),
            paste_line_threshold: crate::composer::DEFAULT_PASTE_LINE_THRESHOLD,
            debug_log_path: None,
            mouse_capture: false,
            screen_reader: false,
        }
    }
}

/// Rows the inline viewport reserves in screen-reader mode: the HUD (3), a
/// live tail of the transcript plus the files list, and the composer (3).
/// Everything older has already moved into scrollback, so this is a live
/// working area rather than the session's whole history — it does not need to
/// be large, and a smaller stable region is a quieter one for a reader.
const INLINE_VIEWPORT_ROWS: u16 = 14;

/// How tall the inline viewport should be on a terminal of `terminal_rows`.
///
/// One row is always left above it whenever the terminal has one to spare:
/// the viewport is anchored to the cursor's row, and claiming the entire
/// screen would leave `insert_before` nowhere to put a line — every flush
/// would scroll the whole screen instead of appending above a stable region.
///
/// There is deliberately no lower bound beyond one row. A floor could only be
/// honoured by claiming rows the terminal does not have, which is the one
/// outcome this must never produce; a terminal too short for the full working
/// area gets a cramped one, not a broken flush.
fn inline_viewport_rows(terminal_rows: u16) -> u16 {
    terminal_rows
        .saturating_sub(1)
        .min(INLINE_VIEWPORT_ROWS)
        .max(1)
}

/// Which screen this session draws on.
///
/// A screen-reader session is never allowed the alternate one: it is the
/// whole reason the surface exists (see [`Screen`]), so it is not a
/// preference the caller can combine away.
fn screen_for(opts: &RunOptions) -> Screen {
    if opts.screen_reader {
        Screen::Normal
    } else {
        Screen::Alternate
    }
}

/// Whether this session captures the mouse.
///
/// [`RunOptions::mouse_capture`] is a request, not a decision: a screen-reader
/// session refuses it. Mouse capture disables the terminal's own selection,
/// and selection is how several assistive technologies read a terminal — so
/// honouring the request would take away the thing the surface exists to
/// provide.
fn mouse_capture_enabled(opts: &RunOptions) -> bool {
    opts.mouse_capture && !opts.screen_reader
}

/// How many leading transcript entries are safe to move into scrollback.
///
/// Everything but the last: [`SessionModel`] coalesces streaming deltas into
/// the **trailing** entry, so it is the only one that can still grow. Flushing
/// it would put half a sentence into scrollback and then, once it finished
/// growing, put the whole sentence there again — a screen reader would read
/// the fragment and the sentence as two separate utterances.
///
/// At teardown the caller flushes the remainder ([`flush_settled`] with
/// `include_trailing`), because by then nothing can grow.
fn settled_entry_count(transcript_len: usize, include_trailing: bool) -> usize {
    if include_trailing {
        transcript_len
    } else {
        transcript_len.saturating_sub(1)
    }
}

/// The rendered lines for transcript entries `from..to`, wrapped to `width`.
///
/// `live` is `false` for every entry: liveness is the tail-follow affordance
/// for a thought still being written, and nothing in this range is still being
/// written — that is what makes it flushable at all.
fn scrollback_lines(
    model: &SessionModel,
    from: usize,
    to: usize,
    expand_thinking: bool,
    width: usize,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for entry in &model.transcript[from.min(to)..to] {
        crate::render::entry_lines(
            entry,
            &model.files,
            expand_thinking,
            expand_thinking,
            false,
            width,
            &mut out,
        );
    }
    out
}

/// Move every newly-settled transcript entry into the terminal's scrollback
/// and record that it is there.
///
/// This is the whole accessibility mechanism. `insert_before` writes the lines
/// **above** the inline viewport, so they are ordinary terminal output: a
/// screen reader announces them once as they arrive, the reader's review
/// cursor can walk back through them, and the user's own scrollback keys
/// reach them. The alternative — leaving them in a pane that repaints every
/// frame — gives a reader a rectangle of cells that changes wholesale several
/// times a second and no notion of "a new line appeared".
///
/// Bumping [`UiState::scrollback_entries`] is what stops the live pane from
/// drawing the same entries again; the two must move together or the session
/// is announced twice.
fn flush_settled<B>(
    terminal: &mut Terminal<B>,
    model: &SessionModel,
    ui: &mut UiState,
    color_mode: theme::ColorMode,
    include_trailing: bool,
) -> io::Result<()>
where
    B: ratatui::backend::Backend<Error = io::Error>,
{
    let settled = settled_entry_count(model.transcript.len(), include_trailing);
    if settled <= ui.scrollback_entries {
        return Ok(());
    }
    let width = terminal.size()?.width;
    let lines = scrollback_lines(
        model,
        ui.scrollback_entries,
        settled,
        ui.thinking_expanded,
        width as usize,
    );
    ui.scrollback_entries = settled;
    let height = match u16::try_from(lines.len()) {
        Ok(0) => return Ok(()),
        Ok(h) => h,
        // A single settled entry cannot plausibly wrap to 65k rows, but the
        // cast must not wrap around into a tiny viewport if it ever did.
        Err(_) => u16::MAX,
    };
    terminal.insert_before(height, |buf| {
        Paragraph::new(Text::from(lines)).render(buf.area, buf);
        theme::apply_theme(buf, color_mode);
        theme::degrade_buffer(buf, color_mode);
    })
}

/// A best-effort structured debug log (L-T8). Never panics and never fails the
/// TUI on an IO error — a lost log line must never take down the session.
#[derive(Debug, Clone, Default)]
pub struct DebugLog {
    path: Option<PathBuf>,
}

impl DebugLog {
    /// A log that writes to `path`, or a no-op sink when `path` is `None`.
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path }
    }

    /// True when this log actually writes somewhere.
    pub fn is_active(&self) -> bool {
        self.path.is_some()
    }

    /// Record an inbound `AgentEvent`.
    pub fn event(&self, event: &AgentEvent) {
        let payload = serde_json::to_value(event).unwrap_or(serde_json::Value::Null);
        self.append("event", payload);
    }

    /// Record an outbound user input.
    pub fn input(&self, input: &UserInput) {
        self.append(
            "input",
            serde_json::json!({ "input": format!("{input:?}") }),
        );
    }

    /// Record a free-form note.
    pub fn note(&self, msg: &str) {
        self.append("note", serde_json::json!({ "msg": msg }));
    }

    fn append(&self, kind: &str, payload: serde_json::Value) {
        if let Some(path) = &self.path {
            let _ = append_json_line(path, kind, payload);
        }
    }
}

/// Append one structured JSON line to `path` (best-effort). Also used by the
/// panic hook in [`crate::term`] to record panics into the same log.
#[cfg(unix)]
pub(crate) fn append_json_line(
    path: &PathBuf,
    kind: &str,
    payload: serde_json::Value,
) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(io::Error::other(
            "debug log must be a single-link regular file",
        ));
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    let line = serde_json::json!({ "ts_ms": ts_ms, "kind": kind, "payload": payload });
    writeln!(file, "{line}")
}

#[cfg(not(unix))]
pub(crate) fn append_json_line(
    path: &PathBuf,
    _kind: &str,
    _payload: serde_json::Value,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "secure debug-log persistence is unsupported on this platform: {}",
            path.display()
        ),
    ))
}

/// Run the interactive TUI to completion.
///
/// `AgentEvent`s stream in over `events`; the user's [`UserInput`]s (prompt
/// submissions and scope-review decisions) stream out over `submissions`.
/// Returns when the engine closes the event stream, the input reader ends, or
/// the user quits (`q` from a panel / Ctrl-C), having always restored the
/// terminal first.
pub async fn run(
    opts: RunOptions,
    mut events: UnboundedReceiver<AgentEvent>,
    submissions: UnboundedSender<UserInput>,
) -> io::Result<()> {
    let debug = DebugLog::new(opts.debug_log_path.clone());
    debug.note("tui session start");

    // The hook shares the guard's state so a panic restores the terminal even
    // in abort builds, where Drop never runs (see `crate::term`).
    // Screen-reader mode owns both of these (see `screen_for` /
    // `mouse_capture_enabled` for why neither is negotiable).
    let term_guard = TerminalGuard::enter(mouse_capture_enabled(&opts), screen_for(&opts))?;
    let _hook_guard = PanicHookGuard::install(opts.debug_log_path.clone(), &term_guard);

    // `inline` is the load-bearing bit, not `opts.screen_reader`: it records
    // whether the inline viewport was actually obtained, and the scrollback
    // flush is gated on it. `insert_before` is a silent no-op on any other
    // viewport, so flushing against one would advance the "already in
    // scrollback" counter while writing nothing — entries would leave the
    // live pane and land nowhere. The counter must only ever move when the
    // lines really were written.
    let (mut terminal, inline) = match opts.screen_reader {
        // An inline viewport draws in place and leaves everything above it
        // alone, which is what makes `insert_before` — and therefore the
        // whole scrollback path in `flush_settled` — possible.
        true => {
            let rows =
                inline_viewport_rows(crossterm::terminal::size().map_or(24, |(_, rows)| rows));
            let options = TerminalOptions {
                viewport: Viewport::Inline(rows),
            };
            match Terminal::with_options(CrosstermBackend::new(io::stdout()), options) {
                Ok(terminal) => (terminal, true),
                // Anchoring an inline viewport means asking the terminal
                // where the cursor is (a DSR report) and waiting for the
                // answer. Every real emulator answers; some minimal ones and
                // some harnesses never do, and the read times out. That must
                // degrade rather than refuse to start — but it degrades to a
                // full-viewport draw on the user's OWN screen, never to the
                // alternate one, because the alternate screen is the thing
                // this surface exists to avoid.
                Err(error) => {
                    debug.note(&format!(
                        "inline viewport unavailable ({error}); drawing full-screen without \
                         scrollback"
                    ));
                    (Terminal::new(CrosstermBackend::new(io::stdout()))?, false)
                }
            }
        }
        false => (Terminal::new(CrosstermBackend::new(io::stdout()))?, false),
    };
    // Detected once (see `theme::color_mode`) and threaded through the draw
    // loop below, rather than touching every `theme::TOKEN` call site in
    // `render.rs`/`textline.rs`/the view modules.
    let color_mode = theme::detect_color_mode();

    let mut model = SessionModel::new();
    let mut ui = UiState::new(
        Composer::with_paste_threshold(opts.paste_line_threshold),
        opts.slash_commands,
    );
    // Enter semantics follow the terminal's actual capability (see
    // `crate::term::TerminalGuard::kitty` and `crate::composer::classify_enter`).
    ui.enter_submits = !term_guard.kitty();
    ui.screen_reader = opts.screen_reader;
    // A degraded screen-reader session must say so. The whole promise of the
    // surface is that finished messages become durable terminal output; if
    // the inline viewport could not be anchored they do not, and a user who
    // scrolls back to re-read an answer that is not there has been told a
    // silent lie. It rides the transcript because the shell owns the screen
    // from here on — a `println!` would be painted over.
    if opts.screen_reader && !inline {
        ingest(
            &AgentEvent::Text {
                delta: "this terminal did not report its cursor position, so messages stay in \
                        this pane instead of moving into scrollback"
                    .to_string(),
            },
            &mut model,
            &mut ui,
        );
    }

    // A blocking reader thread forwards crossterm input events to the async
    // loop. It polls so it can observe the shutdown flag and exit promptly.
    let (key_tx, mut key_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let shutdown = Arc::new(AtomicBool::new(false));
    let reader_shutdown = shutdown.clone();
    let reader = std::thread::spawn(move || {
        while !reader_shutdown.load(Ordering::Relaxed) {
            match event::poll(std::time::Duration::from_millis(100)) {
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

    loop {
        // Settled history leaves the repainting viewport and becomes ordinary
        // terminal output BEFORE the draw, so the pane never paints a line
        // that is already in scrollback. No-op unless `screen_reader` is set.
        if inline {
            flush_settled(&mut terminal, &model, &mut ui, color_mode, false)?;
        }
        terminal.draw(|f| {
            render(&model, &mut ui, f);
            theme::apply_theme(f.buffer_mut(), color_mode);
            theme::degrade_buffer(f.buffer_mut(), color_mode);
        })?;

        tokio::select! {
            maybe_event = events.recv() => {
                match maybe_event {
                    Some(ev) => {
                        debug.event(&ev);
                        ingest(&ev, &mut model, &mut ui);
                    }
                    // Engine closed the stream — the session is over.
                    None => break,
                }
            }
            maybe_key = key_rx.recv() => {
                match maybe_key {
                    // `⌃V`: explicit clipboard pull — the only way a *bitmap*
                    // (a copied screenshot) can enter the composer, since
                    // bracketed paste never carries one. An image becomes an
                    // attachment chip; text falls back to a normal paste.
                    Some(Event::Key(key))
                        if key.kind != KeyEventKind::Release
                            && key.code == crossterm::event::KeyCode::Char('v')
                            && key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                            && model.pending_scope_review.is_none()
                            && model.pending_ask_user.is_none() =>
                    {
                        match crate::clipboard::capture(&crate::clipboard::default_attachments_dir()) {
                            Ok(crate::clipboard::ClipboardPaste::Image(att)) => {
                                debug.note(&format!("clipboard image attached: {}", att.label()));
                                ui.composer.attach(att);
                            }
                            Ok(crate::clipboard::ClipboardPaste::Text(text)) => {
                                match crate::attach::probe_path_attachment(&text) {
                                    Some(att) => ui.composer.attach(att),
                                    None => ui.composer.paste(&text),
                                }
                            }
                            Ok(crate::clipboard::ClipboardPaste::Empty) => {}
                            Err(err) => debug.note(&err),
                        }
                    }
                    Some(Event::Key(key)) if key.kind != KeyEventKind::Release => {
                        match handle_key(key, &model, &mut ui) {
                            ShellAction::Quit => {
                                debug.note("user quit");
                                let _ = submissions.send(UserInput::Cancel);
                                break;
                            }
                            ShellAction::Submit(input) => {
                                debug.input(&input);
                                let _ = submissions.send(input);
                            }
                            ShellAction::Handled | ShellAction::Ignored => {}
                        }
                    }
                    // Bracketed paste: the whole paste arrives as one event
                    // (the guard enabled it), so the composer can fold it
                    // into a chip instead of replaying N raw Enter keys.
                    Some(Event::Paste(text)) => {
                        // A pending gate takes typed free text as its answer —
                        // a scope-review note, an `ask_user` reply — and the
                        // composer stays on screen throughout (it is an
                        // unconditional band; see `render`). So paste lands in
                        // the composer here too: swallowing it meant a reviewer
                        // could *type* a note but not paste one, which is the
                        // same answer arriving by a different key.
                        //
                        // Verbatim text while a gate is pending, though — no
                        // attachment probe. A pasted path is part of the
                        // sentence being written ("only the file at src/x.rs"),
                        // and a gate answer carries no attachments, so turning
                        // it into a chip would silently drop it on submit.
                        let gated = model.pending_scope_review.is_some()
                            || model.pending_ask_user.is_some();
                        match (gated, crate::attach::probe_path_attachment(&text)) {
                            // A paste that names a media file on disk (drag &
                            // drop) attaches the file itself; anything else
                            // stays literal text.
                            (false, Some(att)) => ui.composer.attach(att),
                            _ => ui.composer.paste(&text),
                        }
                    }
                    // Resize/other events: the next draw picks them up.
                    Some(_) => {}
                    // Reader thread ended (stdin closed).
                    None => break,
                }
            }
        }
    }

    shutdown.store(true, Ordering::Relaxed);
    let _ = reader.join();

    // The trailing entry was held back all session because it could still
    // grow (see `settled_entry_count`); nothing can grow now, so it goes to
    // scrollback with the rest. Without this the last thing the assistant
    // said — usually the answer — would vanish with the viewport on exit.
    // Best-effort: a failed flush must not turn a clean quit into an error.
    if inline {
        let _ = flush_settled(&mut terminal, &model, &mut ui, color_mode, true);
        // Leave the cursor below the viewport rather than inside it, so the
        // shell prompt that follows does not overwrite the last frame.
        let _ = terminal.clear();
    }
    debug.note("tui session end");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_capture_is_off_by_default_for_native_copy() {
        // L-T2: enabling mouse tracking breaks the terminal's own selection —
        // the default must never do it.
        assert!(!RunOptions::default().mouse_capture);
    }

    #[test]
    fn debug_log_is_inactive_without_a_path() {
        let log = DebugLog::new(None);
        assert!(!log.is_active());
        // No path → no panic, no file, pure no-op.
        log.event(&AgentEvent::Complete {
            model: "glm".into(),
            cost_usd: 0.0,
        });
        log.note("nothing happens");
    }

    #[test]
    fn debug_log_appends_structured_event_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cli.output");
        let log = DebugLog::new(Some(path.clone()));
        assert!(log.is_active());
        log.event(&AgentEvent::Stage {
            name: stella_protocol::StageKind::Execute,
        });
        log.input(&UserInput::Prompt {
            text: "hi".into(),
            attachments: Vec::new(),
        });
        log.note("done");

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 3, "one JSON line per record:\n{contents}");
        // Each line is valid JSON carrying the kind + a timestamp.
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(v.get("kind").is_some());
            assert!(v.get("ts_ms").is_some());
        }
        let kinds: Vec<String> = lines
            .iter()
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).unwrap()["kind"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(kinds, vec!["event", "input", "note"]);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn debug_log_rejects_a_symlink_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("outside.jsonl");
        std::fs::write(&target, "outside\n").unwrap();
        let path = dir.path().join("debug.jsonl");
        symlink(&target, &path).unwrap();

        assert!(append_json_line(&path, "note", serde_json::json!({})).is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "outside\n");
    }

    // ── The screen-reader surface (#936) ────────────────────────────────

    #[test]
    fn a_screen_reader_session_refuses_the_alternate_screen_and_the_mouse() {
        // Both are the point of the surface, not preferences a caller can
        // combine away — so an explicit `mouse_capture: true` alongside
        // `screen_reader: true` still yields no capture, and the session
        // still draws on the user's own screen.
        let asked_for_both = RunOptions {
            mouse_capture: true,
            screen_reader: true,
            ..RunOptions::default()
        };
        assert_eq!(screen_for(&asked_for_both), Screen::Normal);
        assert!(
            !mouse_capture_enabled(&asked_for_both),
            "capture disables the terminal selection several readers read by"
        );

        // Every other session is untouched by this: the deck and the
        // full-screen REPL keep the alternate screen and their opt-in mouse.
        let ordinary = RunOptions {
            mouse_capture: true,
            ..RunOptions::default()
        };
        assert_eq!(screen_for(&ordinary), Screen::Alternate);
        assert!(mouse_capture_enabled(&ordinary));
        assert_eq!(screen_for(&RunOptions::default()), Screen::Alternate);
        assert!(!mouse_capture_enabled(&RunOptions::default()));
    }

    #[test]
    fn the_trailing_entry_waits_for_teardown_before_it_reaches_scrollback() {
        // It is the only entry `SessionModel` can still grow (streaming
        // deltas coalesce into it), so flushing it mid-session would put a
        // fragment in scrollback and then the whole sentence again — a reader
        // would announce them as two separate utterances.
        assert_eq!(settled_entry_count(4, false), 3);
        // At teardown nothing can grow, so the last one goes too — otherwise
        // the assistant's final answer vanishes with the viewport on exit.
        assert_eq!(settled_entry_count(4, true), 4);
        // An empty transcript has nothing to hold back and must not underflow.
        assert_eq!(settled_entry_count(0, false), 0);
        assert_eq!(settled_entry_count(0, true), 0);
        assert_eq!(settled_entry_count(1, false), 0);
    }

    #[test]
    fn the_inline_viewport_always_leaves_a_row_above_itself() {
        // `insert_before` needs somewhere to put a line. A viewport claiming
        // the whole terminal would turn every flush into a full-screen scroll
        // instead of an append above a stable region — so this holds at every
        // size, including the short terminals an earlier lower bound got
        // wrong by rounding *up* past the rows that existed.
        for rows in 2u16..=200 {
            assert!(
                inline_viewport_rows(rows) < rows,
                "{rows}-row terminal: viewport claimed every row"
            );
        }
        // Roomy terminals get the working area, not the whole screen — the
        // history is in scrollback, so the live region stays small and quiet.
        assert_eq!(inline_viewport_rows(50), INLINE_VIEWPORT_ROWS);
        assert_eq!(inline_viewport_rows(200), INLINE_VIEWPORT_ROWS);
        // A cramped terminal gets everything but the reserved row.
        assert_eq!(inline_viewport_rows(12), 11);
        // Degenerate sizes must not panic, wrap around, or ask for 0 rows.
        assert_eq!(inline_viewport_rows(1), 1);
        assert_eq!(inline_viewport_rows(0), 1);
    }

    #[test]
    fn scrollback_lines_cover_exactly_the_newly_settled_range() {
        let mut model = SessionModel::new();
        model.apply(&AgentEvent::Text {
            delta: "alpha".into(),
        });
        model.apply(&AgentEvent::Stage {
            name: stella_protocol::StageKind::Execute,
        });
        model.apply(&AgentEvent::Text {
            delta: "omega".into(),
        });

        let text = |from: usize, to: usize| {
            scrollback_lines(&model, from, to, false, 60)
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let head = text(0, 1);
        assert!(head.contains("alpha"), "the flushed range renders:\n{head}");
        assert!(
            !head.contains("omega"),
            "…and nothing past it, or the tail lands in scrollback twice:\n{head}"
        );
        // A second flush picks up where the first stopped — no gap, no
        // overlap, which is what makes "announced exactly once" true.
        let tail = text(1, 3);
        assert!(!tail.contains("alpha"), "no re-flush:\n{tail}");
        assert!(tail.contains("omega"), "the newly settled entry:\n{tail}");
        // An empty range is a no-op rather than a panic (`from == to` happens
        // on every frame where nothing settled).
        assert!(scrollback_lines(&model, 3, 3, false, 60).is_empty());
    }

    /// The one interactive path a unit test cannot drive: it needs a real TTY
    /// (raw mode + alternate screen). Documented and `#[ignore]`d per the
    /// task; run manually with `--ignored` on a terminal.
    #[tokio::test]
    #[ignore = "requires an interactive TTY (raw mode + alternate screen)"]
    async fn run_smoke_requires_a_tty() {
        let (ev_tx, ev_rx) = tokio::sync::mpsc::unbounded_channel();
        let (sub_tx, _sub_rx) = tokio::sync::mpsc::unbounded_channel();
        // Dropping the event sender immediately closes the stream, so `run`
        // returns as soon as it starts — proving the wiring, when a TTY is
        // present.
        drop(ev_tx);
        let _ = super::run(RunOptions::default(), ev_rx, sub_tx).await;
    }
}
