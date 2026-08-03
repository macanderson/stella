//! Pty coverage for the accessible single-pane surface — `shell::run` with
//! `RunOptions::screen_reader` (#936, #935).
//!
//! Issue #936's acceptance is explicit: the surface must have a caller *and*
//! "a test that exercises the real entry point". Unit tests cover the pure
//! layers (`handle_key`, `ingest`, `render`), but the entry point itself —
//! the terminal guard, the inline viewport, the scrollback flush, the
//! blocking key-reader thread, the `select!` loop — only ever ran under a
//! human's terminal. That is exactly the gap that let the surface sit
//! "shipped, tested, unreachable" with a green suite.
//!
//! So this runs it the way a terminal does: the `simple_demo` example is
//! spawned on a real pseudo-terminal (its controlling terminal, so
//! `TIOCSWINSZ` delivers `SIGWINCH`), driven with raw bytes, and judged on
//! the byte stream it writes back.
//!
//! The assertions are chosen to be the *accessibility contract* rather than a
//! layout snapshot. The load-bearing one is negative: this surface must never
//! enter the alternate screen. That single escape sequence is the difference
//! between "the conversation is terminal output a screen reader can read" and
//! "the conversation is a grid of cells being rewritten wholesale", and it is
//! the kind of thing a well-meaning refactor re-adds without noticing.
#![cfg(unix)]

use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Generous: CI machines are slow, and the first frame waits out crossterm's
/// keyboard-enhancement probe, which this pty never answers.
const WAIT: Duration = Duration::from_secs(60);

/// Enter the alternate screen / leave it. The surface must emit NEITHER.
const ENTER_ALT_SCREEN: &[u8] = b"\x1b[?1049h";
const LEAVE_ALT_SCREEN: &[u8] = b"\x1b[?1049l";
/// DECTCEM show — what ratatui emits for a frame that positions the cursor
/// (#935). Its absence means the hardware cursor is hidden and there is no
/// insertion point for a reader or an IME to track.
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";
/// Mouse tracking on. Capturing the mouse disables the terminal's own
/// selection, which is how several assistive technologies read a terminal.
const ENABLE_MOUSE: &[u8] = b"\x1b[?1000h";
/// "Where is the cursor?" — the report an inline viewport blocks on while it
/// anchors itself. The harness answers it; see the reader thread.
const DEVICE_STATUS_REPORT: &[u8] = b"\x1b[6n";

/// `cargo test` builds examples by default, so the demo binary normally sits
/// next to the test's own artifacts. An explicitly-targeted run
/// (`cargo test --test simple_pty_smoke`) skips example builds — build it then.
fn simple_demo_bin() -> PathBuf {
    let mut path = std::env::current_exe().expect("current_exe");
    path.pop(); // …/deps
    path.pop(); // …/{debug,release}
    path.push("examples");
    path.push("simple_demo");
    if !path.exists() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args(["build", "-p", "stella-tui", "--example", "simple_demo"])
            .status()
            .expect("run cargo build --example simple_demo");
        assert!(status.success(), "building simple_demo failed");
    }
    assert!(path.exists(), "simple_demo not found at {}", path.display());
    path
}

/// Drop ANSI escape sequences (CSI, OSC, two-byte ESC forms) so assertions
/// match the text the surface painted, not its styling (L-T6). Bytes ≥ 0x80
/// are kept latin-1-lossy — every needle asserted here is ASCII.
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

struct SimplePty {
    child: Child,
    /// The pty master, owned: writes are keystrokes, `TIOCSWINSZ` here is a
    /// window resize.
    master: File,
    /// Everything the surface has written, appended by the reader thread.
    output: Arc<Mutex<Vec<u8>>>,
}

impl SimplePty {
    fn spawn() -> Self {
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
                // coerces to both.
                &raw mut ws,
            )
        };
        assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());

        let slave_stdio = || unsafe { Stdio::from_raw_fd(libc::dup(slave)) };
        let mut cmd = Command::new(simple_demo_bin());
        cmd.stdin(slave_stdio())
            .stdout(slave_stdio())
            .stderr(slave_stdio())
            .env("TERM", "xterm-256color")
            // Deterministic color mode regardless of the invoking shell.
            .env_remove("NO_COLOR")
            .env_remove("CLICOLOR_FORCE")
            .env_remove("COLORTERM")
            .env_remove("STELLA_DEBUG");
        unsafe {
            cmd.pre_exec(move || {
                // A fresh session with the pty as controlling terminal, so a
                // later `TIOCSWINSZ` on the master delivers `SIGWINCH`.
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().expect("spawn simple_demo on the pty");
        unsafe { libc::close(slave) };

        let output = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&output);
        let mut reader = unsafe { File::from_raw_fd(libc::dup(master)) };
        // The same fd for writing: this thread has to answer the child, not
        // just record it (see `DEVICE_STATUS_REPORT`).
        let mut responder = unsafe { File::from_raw_fd(libc::dup(master)) };
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            // EIO (Linux) or EOF (macOS) once the child's side is gone.
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = &buf[..n];
                        sink.lock().unwrap().extend_from_slice(chunk);
                        // An inline viewport anchors itself to the cursor's
                        // current row, which it learns by asking: it writes
                        // a Device Status Report and BLOCKS until the
                        // terminal answers. A bare pty is a pipe with a
                        // line discipline — it answers nothing — so without
                        // this the surface never starts, and the test would
                        // be measuring the harness rather than the code.
                        // Every real emulator replies; so does this one.
                        if chunk
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

    /// The painted text with spaces removed. Ratatui re-emits only *changed*
    /// cells, so a repaint skips any cell already holding the right
    /// character; space cells collide constantly. Space-free needles are
    /// diff-proof.
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
            "timed out waiting for {what}; the surface painted:\n{}",
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
        panic!("simple_demo did not exit after Ctrl-C");
    }
}

impl Drop for SimplePty {
    fn drop(&mut self) {
        // A failed assertion must not leave an orphaned session holding the pty.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn the_simple_surface_runs_inline_answers_input_and_never_takes_the_screen() {
    let mut session = SimplePty::spawn();

    // The scripted opening turn folds in and paints.
    session.wait_for("the opening turn", |s| {
        s.painted().contains("readingtheworkspace")
    });

    // THE invariant. Everything else about this surface follows from drawing
    // on the user's own screen: output that scrolls off stays in scrollback,
    // a reader announces each line once as it arrives, and quitting leaves
    // the conversation behind instead of tearing it down with the screen.
    assert!(
        !session.raw_contains(ENTER_ALT_SCREEN),
        "the accessible surface must never enter the alternate screen"
    );
    // …and the mouse stays free, so the terminal's own selection — which is
    // how several assistive technologies read — keeps working.
    assert!(
        !session.raw_contains(ENABLE_MOUSE),
        "mouse capture disables native selection"
    );

    // #935: the hardware cursor is positioned at the caret, so there is an
    // insertion point for a reader to follow and for an IME to anchor its
    // candidate window to.
    session.wait_for("a positioned hardware cursor", |s| {
        s.raw_contains(SHOW_CURSOR)
    });

    // The tool call and the file it touched both reach the transcript — the
    // panels are stacked here, so this is also the single-column layout
    // rendering real content rather than an empty frame.
    session.wait_for("the tool result and the file ledger", |s| {
        let p = s.painted();
        p.contains("read_file") && p.contains("src/lib.rs")
    });

    // Liveness under resize: ratatui's autoresize forces a repaint and the
    // SIGWINCH reaches crossterm's reader. The inline viewport is anchored to
    // a row, so this is the path most likely to wedge if the anchor is wrong.
    let before = session.raw_len();
    session.resize(30, 100);
    session.wait_for("a repaint after resize", |s| s.raw_len() > before);

    // The full round trip: type a prompt, the reactor answers it. This is the
    // real entry point driving the real key handler over a real terminal —
    // the coverage #936 said was missing.
    session.send(b"does this echo?\r");
    session.wait_for("the prompt round trip", |s| {
        s.painted().contains("yousaid:doesthisecho?")
    });

    // The gate: an `ask_user` question parks the turn, the typed answer is
    // routed to it, and the echoed ToolResult clears the card.
    session.wait_for("the ask_user card", |s| {
        s.painted().contains("whichbranchshouldthislandon?")
    });
    session.send(b"main\r");
    session.wait_for("the gate answer round trip", |s| {
        s.painted().contains("answered:main")
    });

    // Ctrl-C quits: `handle_key` returns Quit, the shell sends Cancel, the
    // reactor closes the stream, and the guard restores the terminal.
    session.send(b"\x03");
    let status = session.wait_exit();
    assert!(status.success(), "clean exit, got {status:?}");

    // Restoration must not include leaving a screen that was never entered —
    // an unbalanced `LeaveAlternateScreen` clears the user's real terminal,
    // taking the whole conversation with it. That would undo the surface's
    // entire reason for existing at the very last instant.
    assert!(
        !session.raw_contains(ENTER_ALT_SCREEN) && !session.raw_contains(LEAVE_ALT_SCREEN),
        "the alternate screen was neither entered nor left"
    );
}
