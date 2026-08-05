//! Pty smoke coverage for the deck's `run_deck` event loop (#826).
//!
//! Everything *below* the loop is unit-tested through pure layers
//! (`handle_deck_key`, `ingest_inbound`, `render_deck`), but the loop itself —
//! terminal guard, panic hook, blocking key-reader thread, the five-lane
//! `select!`, the tick cadence, the `!` shell lane — only ever ran under a
//! human's terminal. This test runs it the way a terminal does: the
//! `deck_demo` example is spawned on a real pseudo-terminal (its controlling
//! terminal, so `/dev/tty` resolves and `TIOCSWINSZ` delivers `SIGWINCH`),
//! driven with raw bytes, and judged on the byte stream it writes back.
//!
//! The harness is hand-rolled on `libc::openpty` — no new dependency — and
//! asserts on *stripped* text plus the raw escape sequences that matter
//! (alternate-screen enter/leave, bracketed paste off). It is deliberately a
//! smoke test: it pins "paints, folds, answers input, survives a resize,
//! quits clean, restores the terminal", not any particular layout.
#![cfg(unix)]

use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Generous: CI machines are slow, the first frame waits out crossterm's
/// keyboard-enhancement probe (which this pty never answers), and the demo
/// paces its scripted scenario at one event per 500ms — the scope-review
/// card this test answers is the LAST of those events, ~20s in (re-checked
/// when the scenario grew the task/witness events; still well inside WAIT).
const WAIT: Duration = Duration::from_secs(60);

/// `cargo test` builds examples by default, so the demo binary normally sits
/// next to the test's own artifacts. An explicitly-targeted run
/// (`cargo test --test deck_pty_smoke`) skips example builds — build it then.
fn deck_demo_bin() -> PathBuf {
    let mut path = std::env::current_exe().expect("current_exe");
    path.pop(); // …/deps
    path.pop(); // …/{debug,release}
    path.push("examples");
    path.push("deck_demo");
    if !path.exists() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args(["build", "-p", "stella-tui", "--example", "deck_demo"])
            .status()
            .expect("run cargo build --example deck_demo");
        assert!(status.success(), "building deck_demo failed");
    }
    assert!(path.exists(), "deck_demo not found at {}", path.display());
    path
}

/// Drop ANSI escape sequences (CSI, OSC, two-byte ESC forms) so assertions
/// match the text the deck painted, not its styling. Ratatui emits each
/// same-style cell run contiguously, so a first-paint label like `SESSION`
/// survives stripping as a contiguous substring. Bytes ≥ 0x80 are kept
/// latin-1-lossy — every needle asserted here is ASCII.
fn strip_ansi(raw: &[u8]) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        match raw[i] {
            0x1b => match raw.get(i + 1) {
                Some(b'[') => {
                    i += 2;
                    while i < raw.len() && !(0x40..=0x7e).contains(&raw[i]) {
                        i += 1;
                    }
                    i += 1; // the final byte
                }
                Some(b']') => {
                    i += 2;
                    while i < raw.len() {
                        if raw[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if raw[i] == 0x1b && raw.get(i + 1) == Some(&b'\\') {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                Some(_) => i += 2,
                None => i += 1,
            },
            b'\n' => {
                out.push('\n');
                i += 1;
            }
            b if (0x20..0x7f).contains(&b) || b >= 0x80 => {
                out.push(b as char);
                i += 1;
            }
            _ => i += 1, // other control bytes
        }
    }
    out
}

struct DeckPty {
    child: Child,
    /// The pty master, owned: writes are keystrokes, `TIOCSWINSZ` here is a
    /// window resize.
    master: File,
    /// Everything the deck has written, appended by the reader thread.
    output: Arc<Mutex<Vec<u8>>>,
}

/// Enter the alternate screen / leave it. An accessible session must emit
/// NEITHER — and the leave matters as much as the enter: an unbalanced
/// `LeaveAlternateScreen` clears the user's real terminal, taking the whole
/// conversation with it at the very last instant.
const ENTER_ALT_SCREEN: &[u8] = b"\x1b[?1049h";
const LEAVE_ALT_SCREEN: &[u8] = b"\x1b[?1049l";
/// Mouse tracking on. Capturing the mouse disables the terminal's own
/// selection, which is how several assistive technologies read a terminal.
const ENABLE_MOUSE: &[u8] = b"\x1b[?1000h";
/// DECTCEM show — what ratatui emits for a frame that positions the cursor
/// (#935). Its absence means the hardware cursor is hidden and there is no
/// insertion point for a reader or an IME to track.
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";
/// "Where is the cursor?" — the report an inline viewport blocks on while it
/// anchors itself. The harness answers it; see the reader thread.
const DEVICE_STATUS_REPORT: &[u8] = b"\x1b[6n";

impl DeckPty {
    fn spawn() -> Self {
        Self::spawn_with(false, true)
    }

    /// `accessible` runs the same demo under `STELLA_ACCESSIBLE=1` — the deck
    /// on the user's own screen, in an inline viewport (#1258).
    ///
    /// `answer_dsr` is what a *terminal* does, not what the deck does: with it
    /// off this pty behaves like the minimal emulators and harnesses that never
    /// reply to a cursor-position request, which is the case that made #1237's
    /// surface fail to start outright.
    fn spawn_with(accessible: bool, answer_dsr: bool) -> Self {
        let mut master: libc::c_int = -1;
        let mut slave: libc::c_int = -1;
        let mut ws = libc::winsize {
            ws_row: 40,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                // A raw pointer, not `&mut`: libc's openpty takes `*const
                // winsize` on Linux but `*mut winsize` on macOS, and `*mut`
                // coerces to both — `&mut` trips clippy's
                // unnecessary_mut_passed on the `*const` platforms.
                &raw mut ws,
            )
        };
        assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());

        let slave_stdio = || unsafe { Stdio::from_raw_fd(libc::dup(slave)) };
        let mut cmd = Command::new(deck_demo_bin());
        cmd.stdin(slave_stdio())
            .stdout(slave_stdio())
            .stderr(slave_stdio())
            .env("TERM", "xterm-256color")
            // A static frame: no shimmer / pulse / caret blink, and the
            // splash mark lights immediately (`DeckOptions::no_anim`).
            .env("STELLA_NO_ANIM", "1")
            // Deterministic color mode regardless of the invoking shell.
            .env_remove("NO_COLOR")
            .env_remove("CLICOLOR_FORCE")
            .env_remove("COLORTERM")
            .env_remove("STELLA_DEBUG");
        if accessible {
            cmd.env("STELLA_ACCESSIBLE", "1");
        } else {
            cmd.env_remove("STELLA_ACCESSIBLE");
        }
        unsafe {
            cmd.pre_exec(move || {
                // A fresh session with the pty as controlling terminal:
                // `/dev/tty` resolves, and a later `TIOCSWINSZ` on the master
                // delivers `SIGWINCH` — the resize path a merely-piped child
                // never exercises.
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().expect("spawn deck_demo on the pty");
        unsafe { libc::close(slave) };

        let output = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&output);
        let mut reader = unsafe { File::from_raw_fd(libc::dup(master)) };
        // The same fd for writing: this thread has to answer the child, not
        // just record it (see `DEVICE_STATUS_REPORT`).
        let mut responder = unsafe { File::from_raw_fd(libc::dup(master)) };
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            // EIO (Linux) or EOF (macOS) once the child's side is gone —
            // either way the session is over and the tail is already stashed.
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = &buf[..n];
                        sink.lock().unwrap().extend_from_slice(chunk);
                        // An inline viewport anchors itself to the cursor's
                        // current row, which it learns by asking: it writes a
                        // Device Status Report and BLOCKS until the terminal
                        // answers. A bare pty is a pipe with a line discipline
                        // — it answers nothing — so without this the accessible
                        // deck degrades to the no-scrollback path and the test
                        // would be measuring the harness rather than the code.
                        // Every real emulator replies; so does this one.
                        if answer_dsr
                            && chunk
                                .windows(DEVICE_STATUS_REPORT.len())
                                .any(|w| w == DEVICE_STATUS_REPORT)
                        {
                            let _ = responder.write_all(b"\x1b[1;1R");
                            let _ = responder.flush();
                        }
                    }
                }
            }
        });

        Self {
            child,
            master: unsafe { File::from_raw_fd(master) },
            output,
        }
    }

    fn stripped(&self) -> String {
        strip_ansi(&self.output.lock().unwrap())
    }

    /// The painted text with spaces removed — the form content probes match
    /// against. Ratatui re-emits only *changed* cells, so a repaint skips any
    /// cell already holding the right character; space cells collide all the
    /// time (observed live: `scope decision: Approve` painting as
    /// `scope decision:Approve`). Space-free needles are diff-proof.
    fn painted(&self) -> String {
        self.stripped().replace(' ', "")
    }

    fn raw_len(&self) -> usize {
        self.output.lock().unwrap().len()
    }

    fn raw_contains(&self, needle: &[u8]) -> bool {
        let out = self.output.lock().unwrap();
        out.windows(needle.len()).any(|w| w == needle)
    }

    fn send(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).expect("write keystrokes");
    }

    fn resize(&self, rows: u16, cols: u16) {
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let rc = unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &ws) };
        assert_eq!(rc, 0, "TIOCSWINSZ: {}", std::io::Error::last_os_error());
    }

    fn wait_for(&self, what: &str, pred: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if pred(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "timed out waiting for {what}; the deck painted:\n{}",
            self.stripped()
        );
    }

    fn wait_exit(&mut self) -> ExitStatus {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                return status;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("deck_demo did not exit after Ctrl-C");
    }
}

impl Drop for DeckPty {
    fn drop(&mut self) {
        // A failed assertion must not leave an orphaned deck holding the pty.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn run_deck_paints_folds_resizes_and_restores_under_a_real_pty() {
    let mut deck = DeckPty::spawn();

    // The TerminalGuard acquired the terminal: raw mode + alternate screen.
    deck.wait_for("the alternate screen", |d| d.raw_contains(b"\x1b[?1049h"));

    // Any key dismisses the splash; Esc inserts nothing if it already ended
    // on its own (reduced splashes hold only briefly).
    deck.send(b"\x1b");

    // The deck chrome paints: the tab bar's fixed labels…
    deck.wait_for("the tab bar", |d| {
        let s = d.painted();
        s.contains("SESSION") && s.contains("AGENTS") && s.contains("SETTINGS")
    });
    // …and the scripted scenario folded in over the inbound lane.
    deck.wait_for("the scripted recall fold", |d| {
        d.painted().contains("enginestep-driver(driver.rs)")
    });

    // Resize: ratatui's autoresize forces a full repaint (and the SIGWINCH
    // reaches crossterm's reader). Liveness, not layout — the deck must
    // repaint rather than wedge or die.
    let before = deck.raw_len();
    deck.resize(30, 100);
    deck.wait_for("a repaint after resize", |d| d.raw_len() > before);

    // The scenario's LAST event is a scope review on the focused agent, and
    // the plan-review dialog owns the keyboard while it is pending. Wait for
    // the dialog, answer it with the single keypress it advertises:
    // submissions channel → demo reactor → `scope decision: Approve` echoed
    // back over the inbound lane. The full user→engine→user round trip.
    deck.wait_for("the plan-review dialog", |d| {
        d.painted().contains("Refactortheautomationsstore")
    });
    deck.send(b"a");
    deck.wait_for("the gate answer round trip", |d| {
        d.painted().contains("scopedecision:Approve")
    });

    // The `!` shell lane, end to end: key reader → composer → Shell action →
    // spawned child → local event lane → fold → paint. The needle `abc-wz`
    // is the child's OUTPUT only — the typed command echoes as `abc-XY`, so
    // seeing `abc-wz` proves the round trip, not just the composer echo.
    deck.send(b"!echo abc-XY | tr XY wz\r");
    deck.wait_for("the shell command's output", |d| {
        d.painted().contains("abc-wz")
    });

    // Ctrl-C: clean cancel + quit, from anywhere.
    deck.send(&[0x03]);
    let status = deck.wait_exit();
    assert!(status.success(), "deck_demo exited with {status:?}");

    // The guard restored the terminal on the way out: alternate screen left,
    // bracketed paste disabled. (Raw mode is a termios change, invisible in
    // the byte stream.)
    deck.wait_for("the terminal restore", |d| {
        d.raw_contains(b"\x1b[?1049l") && d.raw_contains(b"\x1b[?2004l")
    });
}

/// The accessibility contract of `--accessible` (#1258), driven through the
/// real entry point.
///
/// The load-bearing assertion is negative: this mode must never enter the
/// alternate screen. That single escape sequence is the difference between
/// "the conversation is terminal output a screen reader can read" and "the
/// conversation is a grid of cells being rewritten wholesale", and it is the
/// kind of thing a well-meaning refactor re-adds without noticing.
///
/// The positive half is that it is still the whole deck: it paints, it folds
/// the scripted scenario, it answers input, and it quits clean — plus the
/// settled transcript really does reach the terminal's scrollback, which the
/// last assertion pins by looking only at bytes written *after* the quit key.
///
/// The finer scrollback invariants — a settled entry planned exactly once, the
/// counter never advancing without a write, a lane change neither re-flushing
/// nor dropping — are unit tests (`accessible::tests`), because a repainting
/// byte stream cannot distinguish "painted in the live pane" from "written into
/// scrollback" and an assertion that pretended otherwise would be measuring
/// ratatui's damage tracking.
#[test]
fn the_accessible_deck_runs_inline_answers_input_and_never_takes_the_screen() {
    let mut deck = DeckPty::spawn_with(true, true);

    // The deck chrome paints — same program, no feature removed.
    deck.send(b"\x1b");
    deck.wait_for("the tab bar", |d| {
        let s = d.painted();
        s.contains("SESSION") && s.contains("AGENTS") && s.contains("SETTINGS")
    });

    // THE invariant. Everything else about this mode follows from drawing on
    // the user's own screen: output that scrolls off stays in scrollback, a
    // reader announces each line once as it arrives, and quitting leaves the
    // conversation behind instead of tearing it down with the screen.
    assert!(
        !deck.raw_contains(ENTER_ALT_SCREEN),
        "accessible mode must never enter the alternate screen; the deck painted:\n{}",
        deck.stripped()
    );
    // …and the mouse stays free, so the terminal's own selection — which is
    // how several assistive technologies read — keeps working.
    assert!(
        !deck.raw_contains(ENABLE_MOUSE),
        "mouse capture disables native selection"
    );

    // #935, which #1258's acceptance carries forward as a must-not-regress: the
    // hardware cursor is positioned at the composer caret, so there is a real
    // insertion point for a reader to follow and for a CJK IME to anchor its
    // candidate window to. Its absence means the cursor is hidden and there is
    // no such point.
    deck.wait_for("a positioned hardware cursor", |d| {
        d.raw_contains(SHOW_CURSOR)
    });

    // The scripted scenario folds in and paints, inline.
    deck.wait_for("the scripted recall fold", |d| {
        d.painted().contains("enginestep-driver(driver.rs)")
    });

    // Liveness under resize: the inline viewport is anchored to a row, so this
    // is the path most likely to wedge if the anchor is wrong.
    let before = deck.raw_len();
    deck.resize(30, 100);
    deck.wait_for("a repaint after resize", |d| d.raw_len() > before);

    // Input still round-trips: the scenario's last event is a scope-review
    // gate, and its one-keypress answer goes key → submissions → demo
    // reactor → inbound lane → fold → paint.
    deck.wait_for("the plan-review dialog", |d| {
        d.painted().contains("Refactortheautomationsstore")
    });
    deck.send(b"a");
    deck.wait_for("the gate answer round trip", |d| {
        d.painted().contains("scopedecision:Approve")
    });

    // A `!` command puts an entry on the focused lane that this test owns the
    // text of — `acc-wz` is the child's OUTPUT only (the typed command echoes
    // as `acc-XY`), and nothing in the scripted scenario can produce it.
    deck.send(b"!echo acc-XY | tr XY wz\r");
    deck.wait_for("the shell command's output", |d| {
        d.painted().contains("acc-wz")
    });

    // Everything written from here on is teardown. Nothing can coalesce into a
    // lane's trailing entry any more, so the final flush moves it into
    // scrollback — the conversation survives the session instead of ending in a
    // pane that stops being redrawn, which is the property the whole mode
    // exists to deliver.
    let before_quit = deck.raw_len();
    deck.send(&[0x03]);
    let status = deck.wait_exit();
    assert!(status.success(), "deck_demo exited with {status:?}");

    let tail = {
        let out = deck.output.lock().unwrap();
        strip_ansi(&out[before_quit.min(out.len())..]).replace(' ', "")
    };
    assert!(
        tail.contains("acc-wz"),
        "the teardown flush must write each lane's last settled entry into scrollback; \
         the bytes after quit were:\n{tail}"
    );

    // Restoration must not include leaving a screen that was never entered —
    // an unbalanced leave clears the user's real terminal, taking the whole
    // conversation with it at the very last instant.
    assert!(
        !deck.raw_contains(ENTER_ALT_SCREEN) && !deck.raw_contains(LEAVE_ALT_SCREEN),
        "the alternate screen was neither entered nor left"
    );
    // Bracketed paste is still restored — the states that WERE acquired all
    // roll back.
    deck.wait_for("the terminal restore", |d| d.raw_contains(b"\x1b[?2004l"));
}

/// A terminal that never answers `ESC [ 6 n` must still get a deck.
///
/// Anchoring an inline viewport means blocking on a cursor-position report.
/// Every real emulator answers; some minimal ones and most harnesses do not,
/// and #1237 found out the hard way that unguarded this makes the surface fail
/// to start at all. It has to degrade — to a full-viewport draw on the user's
/// OWN screen, never the alternate one, with the flush disabled and the loss
/// announced (`accessible::degrade_notice`, unit-tested).
#[test]
fn an_accessible_deck_degrades_rather_than_refusing_to_start() {
    let mut deck = DeckPty::spawn_with(true, false);

    deck.send(b"\x1b");
    deck.wait_for("the tab bar on a terminal that answers nothing", |d| {
        let s = d.painted();
        s.contains("SESSION") && s.contains("AGENTS") && s.contains("SETTINGS")
    });
    assert!(
        !deck.raw_contains(ENTER_ALT_SCREEN),
        "the degrade is to the user's own screen — never to the one the mode exists to avoid"
    );

    deck.send(&[0x03]);
    let status = deck.wait_exit();
    assert!(status.success(), "deck_demo exited with {status:?}");
    assert!(
        !deck.raw_contains(ENTER_ALT_SCREEN) && !deck.raw_contains(LEAVE_ALT_SCREEN),
        "the alternate screen was neither entered nor left"
    );
}
