//! Pty coverage for `TerminalPrompt::can_prompt`.
//!
//! `rpassword` reads and writes `/dev/tty` on Unix. It never touches
//! `stdin` or `stdout`. So `can_prompt` must check `/dev/tty` too, not
//! `stdin`/`stdout`.
//!
//! The check here: redirect stdout to a plain file, but keep a real
//! terminal attached. That is what `stella config > out.txt` looks
//! like from a real shell. `stdout().is_terminal()` says no. The
//! right answer is yes, because `rpassword` never reads stdout.
//!
//! Only a real pseudo-terminal can show this. The harness builds one
//! with `libc::openpty`, `setsid`, and `TIOCSCTTY` — the same tools
//! `stella-tui`'s `tests/deck_pty_smoke.rs` uses. It runs the
//! `tty-probe` fixture binary as a child process, because the test
//! needs a real controlling terminal on that child, not on this test
//! binary's own session.
#![cfg(unix)]

use std::fs::File;
use std::io::Read;
use std::os::unix::io::FromRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

fn tty_probe_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_tty-probe"))
}

/// Open a pseudo-terminal. Returns the raw `(master, slave)` fds.
fn open_pty() -> (libc::c_int, libc::c_int) {
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            // One null works on both Linux and macOS here, even
            // though the two declare this parameter with different
            // pointer types.
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());
    (master, slave)
}

/// Run `tty-probe` with the pty slave as its stdin and controlling
/// terminal. Stdout goes to a plain file, not the pty. Returns the line
/// the probe printed.
fn probe_with_redirected_stdout() -> String {
    let (master, slave) = open_pty();
    let out_file = tempfile::NamedTempFile::new().expect("temp file for stdout");

    let mut cmd = Command::new(tty_probe_bin());
    cmd.stdin(unsafe { Stdio::from_raw_fd(libc::dup(slave)) })
        .stdout(Stdio::from(
            out_file.reopen().expect("independent handle on temp file"),
        ))
        .stderr(Stdio::null());
    unsafe {
        cmd.pre_exec(move || {
            // Start a new session with the pty as its controlling
            // terminal. Now `/dev/tty` finds this pty no matter what
            // fd 1 (stdout) points at.
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn().expect("spawn tty-probe");
    unsafe {
        libc::close(slave);
    }
    let status = child.wait_with_output().expect("wait for tty-probe").status;
    unsafe {
        libc::close(master);
    }
    assert!(status.success(), "tty-probe exited with {status:?}");

    let mut out = String::new();
    File::open(out_file.path())
        .expect("open probe output")
        .read_to_string(&mut out)
        .expect("read probe output");
    out.trim().to_string()
}

/// Run `tty-probe` with no terminal at all: its own session, every
/// stdio handle `/dev/null`. This is the control: it proves the probe
/// does not just always say `true`.
fn probe_with_no_terminal_at_all() -> String {
    let mut cmd = Command::new(tty_probe_bin());
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            // Start a new session. Skip `TIOCSCTTY`, so it gets no
            // controlling terminal at all.
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = cmd.output().expect("spawn tty-probe");
    assert!(
        output.status.success(),
        "tty-probe exited with {:?}",
        output.status
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A check that only asks `std::io::stdout().is_terminal()` would print
/// `false` here, since stdout is a plain file. `can_prompt` checks
/// `/dev/tty` instead, which this pty makes reachable. So it must
/// print `true`.
#[test]
fn can_prompt_reaches_dev_tty_even_when_stdout_is_redirected_away() {
    assert_eq!(
        probe_with_redirected_stdout(),
        "true",
        "a redirected stdout must not hide a live controlling terminal from rpassword"
    );
}

/// With no controlling terminal anywhere, `can_prompt` still says no.
/// The probe checks the real terminal; it does not just always say
/// yes.
#[test]
fn can_prompt_declines_with_no_controlling_terminal_at_all() {
    assert_eq!(probe_with_no_terminal_at_all(), "false");
}
