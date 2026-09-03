// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Driving a plugin's panel process — SPEC 12's other half.
//!
//! The deck's `plugin_panel` host draws a frame; this asks a plugin for one.
//! The exchange is one JSON [`PanelRequest`] on the child's stdin and one
//! [`PanelResponse`] **followed by a newline** on its stdout. Reading to that
//! newline rather than to EOF is what lets a panel start a helper: a
//! backgrounded grandchild inherits stdout and holds the pipe open, so a host
//! waiting for EOF would time out against a panel that answered instantly.
//!
//! Three rules bind a caller:
//!
//! - **Never on the draw path.** [`ask`] is `async` and its frame lands in
//!   state for the *next* repaint. A draw that waited on somebody else's
//!   process could freeze the terminal, and it is what makes the frame budget
//!   mean something: a slow panel costs a stale frame and a visible tag.
//! - **The environment is the caller's to resolve.** `env_clear`, then exactly
//!   the pairs passed in — a panel sees what a human consented to and nothing
//!   else. This crate reads no ambient state to build that list
//!   (`tests/no_ambient_reads.rs`), so [`resolve_env`] applies the manifest's
//!   allowlist over a lookup the caller supplies.
//! - **The process group is reaped when the tick ends.** EOF on stdout is not
//!   process exit.

use std::process::Stdio;
use std::time::{Duration, Instant};

use stella_plugin::{PanelFrame, PanelLease, PanelPoint, PanelRequest, PanelResponse, Runtime};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// What one frame cost, and what came back.
#[derive(Debug)]
pub struct PanelTick {
    /// The frame the plugin drew, or `None` when it produced none.
    pub frame: Option<PanelFrame>,
    /// Wall time the exchange took, which the caller compares against the
    /// lease's budget to decide whether to draw the throttle tag.
    pub elapsed: Duration,
}

/// Why a panel produced no frame this tick.
///
/// Typed rather than a string because the caller branches: a timeout keeps the
/// last good frame and tags it, a refusal takes the panel down, and a decode
/// error is a bug in the plugin, and the row says so.
#[derive(Debug)]
pub enum PanelError {
    /// The program could not be started.
    Spawn {
        /// The program named by `[panel.process] argv`.
        program: String,
        /// What the operating system said.
        source: std::io::Error,
    },
    /// The child did not answer inside `[panel.process] timeout_secs`.
    Timeout {
        /// The program named by `[panel.process] argv`.
        program: String,
        /// The budget it overran.
        budget_ms: u64,
    },
    /// The child wrote something that is not a [`PanelResponse`].
    Decode {
        /// The program named by `[panel.process] argv`.
        program: String,
        /// The decode failure.
        source: serde_json::Error,
    },
    /// The exchange failed at the pipe.
    Transport {
        /// The program named by `[panel.process] argv`.
        program: String,
        /// The I/O failure.
        source: std::io::Error,
    },
}

impl std::fmt::Display for PanelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn { program, source } => {
                write!(f, "the panel process `{program}` did not start: {source}")
            }
            Self::Timeout { program, budget_ms } => write!(
                f,
                "the panel process `{program}` did not answer within {budget_ms}ms"
            ),
            Self::Decode { program, source } => write!(
                f,
                "the panel process `{program}` wrote no readable frame: {source}"
            ),
            Self::Transport { program, source } => write!(
                f,
                "the panel process `{program}` failed mid-exchange: {source}"
            ),
        }
    }
}

impl std::error::Error for PanelError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source, .. } | Self::Transport { source, .. } => Some(source),
            Self::Decode { source, .. } => Some(source),
            Self::Timeout { .. } => None,
        }
    }
}

/// The environment a panel process is started with: `PATH`, then the
/// `[panel.process] env` allowlist, resolved through `lookup`.
///
/// `lookup` rather than `std::env::var` because this crate may not read
/// process-global state. A caller that owns the ambient world passes
/// `|name| std::env::var(name).ok()`; a test passes a map, and two sessions in
/// one process can hand this different answers.
///
/// `PATH` is included without the manifest naming it: it is how a program is
/// located, not a secret to be granted, and without it `env_clear` leaves the
/// child to `execvp`'s built-in default — so a `python3` in a virtualenv or in
/// Homebrew is simply not found. A manifest naming `PATH` explicitly is
/// honoured once, not twice.
#[must_use]
pub fn resolve_env(
    process: &Runtime,
    lookup: impl Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    let mut out = Vec::with_capacity(process.env.len() + 1);
    if let Some(path) = lookup("PATH") {
        out.push(("PATH".to_string(), path));
    }
    for name in &process.env {
        if name == "PATH" {
            continue;
        }
        if let Some(value) = lookup(name) {
            out.push((name.clone(), value));
        }
    }
    out
}

/// Ask `process` for one frame against `lease`, started with exactly `env`.
///
/// `env` is the whole environment the child sees — [`resolve_env`] builds it
/// from the manifest, and nothing here adds to it.
///
/// # Errors
///
/// [`PanelError`], naming which of the four ways the exchange failed.
pub async fn ask(
    process: &Runtime,
    lease: PanelLease,
    env: &[(String, String)],
) -> Result<PanelTick, PanelError> {
    let Some((program, args)) = process.argv.split_first() else {
        return Err(PanelError::Spawn {
            program: String::new(),
            source: std::io::Error::other("`[panel.process] argv` named no program"),
        });
    };
    let budget = Duration::from_secs(process.timeout_secs.max(1));
    let request = PanelRequest::new(lease);
    let body = serde_json::to_vec(&request).map_err(|source| PanelError::Decode {
        program: program.clone(),
        source,
    })?;

    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Discarded rather than piped. A pipe nobody reads deadlocks the child
        // the moment it outgrows one buffer — it blocks in `write(2)`, never
        // closes stdout, and every tick then hits the timeout, so one stray
        // `eprintln` in a plugin's draw loop would throttle its panel forever.
        // `wrapper::subprocess::settle`'s doc states the same hazard for the
        // socket, where the answer is a concurrent drain; a panel has nowhere
        // to put the bytes, so it does not ask for them.
        .stderr(Stdio::null())
        // A panel that outlives its tick is killed. Without this, a plugin that
        // ignores a closed stdin keeps drawing for a session that has moved on.
        .kill_on_drop(true);
    for (name, value) in env {
        command.env(name, value);
    }
    stella_tools::exec::detach_into_own_process_group(&mut command);

    let started = Instant::now();
    let mut child = command.spawn().map_err(|source| PanelError::Spawn {
        program: program.clone(),
        source,
    })?;
    // The child leads its group, so its pid is the group id — armed before the
    // first `await` so every way out of this function reaps the whole group.
    //
    // A missing pid is refused rather than defaulted: `GroupKillGuard` no-ops
    // on a non-positive pid, so `unwrap_or(0)` would arm a guard that silently
    // guards nothing. `id()` returns `Some` for a child that has not been
    // waited on, so this is unreachable — and if it ever fires, no backstop is
    // the one outcome that must not be silent.
    let Some(pid) = child.id().and_then(|pid| i32::try_from(pid).ok()) else {
        return Err(PanelError::Spawn {
            program: program.clone(),
            source: std::io::Error::other(
                "the panel process reported no usable pid, so its process group could not be \
                 reaped; refusing to run it unguarded",
            ),
        });
    };
    let mut guard = stella_tools::exec::GroupKillGuard::arm(pid);

    let exchange = async {
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(&body)
                .await
                .map_err(|source| PanelError::Transport {
                    program: program.clone(),
                    source,
                })?;
            // Closing stdin is what tells a plugin the request is complete; a
            // panel asks the host for nothing, so the pipe has no second use.
            drop(stdin);
        }
        // One line, not everything until EOF.
        //
        // A frame is one JSON object and `serde_json` escapes every newline
        // inside one, so a line is a complete message and reading further buys
        // nothing. Reading to EOF costs a great deal: a backgrounded helper
        // inherits the child's stdout, so the pipe stays open until the
        // *grandchild* exits — a panel that starts a worker would hit its
        // timeout on every tick while having answered correctly and instantly.
        let mut out = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            let mut reader = BufReader::new(stdout);
            reader
                .read_until(b'\n', &mut out)
                .await
                .map_err(|source| PanelError::Transport {
                    program: program.clone(),
                    source,
                })?;
        }
        Ok::<Vec<u8>, PanelError>(out)
    };

    let out = match tokio::time::timeout(budget, exchange).await {
        Ok(result) => result?,
        Err(_) => {
            // Killed here rather than left to the guard's `Drop`, which is what
            // both existing transports do and what `kill_now`'s own contract
            // asks for: the child is still running and must die before the
            // caller returns, not at some later scope exit a future edit could
            // move.
            guard.kill_now();
            return Err(PanelError::Timeout {
                program: program.clone(),
                budget_ms: budget.as_millis().min(u128::from(u64::MAX)) as u64,
            });
        }
    };
    // The guard is NOT disarmed on the way out. EOF on stdout is not process
    // exit — a panel can close the pipe and keep running, and one that forked a
    // helper leaves a live grandchild in the group that `kill_on_drop` cannot
    // reach. A panel is one frame per tick, so once the frame is in hand there
    // is nothing left in that group worth keeping: letting the armed guard drop
    // reaps it.

    let elapsed = started.elapsed();
    if out.iter().all(u8::is_ascii_whitespace) {
        return Ok(PanelTick {
            frame: None,
            elapsed,
        });
    }
    let response: PanelResponse =
        serde_json::from_slice(&out).map_err(|source| PanelError::Decode {
            program: program.clone(),
            source,
        })?;
    Ok(PanelTick {
        frame: Some(response.body),
        elapsed,
    })
}

/// The point every panel exchange opens at — one name, so a caller cannot
/// spell it differently from the wire.
#[must_use]
pub fn point() -> PanelPoint {
    PanelPoint::Frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_plugin::{PanelPaint, PanelRect, PanelSurface};

    fn lease(cols: u16, rows: u16) -> PanelLease {
        PanelLease::new(
            "hello",
            PanelSurface::Command,
            1,
            PanelRect { cols, rows },
            33,
        )
    }

    fn shell(script: &str) -> Runtime {
        Runtime {
            argv: vec!["sh".into(), "-c".into(), script.into()],
            timeout_secs: 5,
            env: Vec::new(),
        }
    }

    /// The exchange end to end against a real process: the host writes a
    /// request, the child answers a frame, and the frame arrives decoded.
    #[tokio::test]
    async fn a_panel_process_answers_one_frame() {
        let script = r#"cat > /dev/null; printf '%s\n' '{"point":"frame","body":{"protocol_version":1,"surface":"command","tick":1,"paint":{"lines":[{"spans":[{"text":"hello"}]}]}}}'"#;
        let tick = ask(&shell(script), lease(20, 3), &[])
            .await
            .expect("a frame");
        let frame = tick.frame.expect("the child drew one");
        assert_eq!(frame.tick, 1);
        match frame.paint {
            PanelPaint::Lines(lines) => {
                assert_eq!(lines[0].spans[0].text.as_str(), "hello");
            }
            PanelPaint::Diff(_) => panic!("expected lines"),
        }
    }

    /// The request actually reaches the child: it echoes the lease back, so a
    /// host that sent nothing would fail this rather than pass on a fixture.
    #[tokio::test]
    async fn the_child_is_told_the_rectangle_it_was_leased() {
        let script = r#"REQ=$(cat); printf '{"point":"frame","body":{"protocol_version":1,"surface":"command","tick":1,"paint":{"lines":[{"spans":[{"text":"%s"}]}]}}}\n' "$(printf '%s' "$REQ" | tr -cd '0-9' | head -c 4)""#;
        let tick = ask(&shell(script), lease(20, 3), &[])
            .await
            .expect("a frame");
        let frame = tick.frame.expect("the child drew one");
        let PanelPaint::Lines(lines) = frame.paint else {
            panic!("expected lines")
        };
        let echoed = lines[0].spans[0].text.as_str();
        assert!(
            echoed.contains('2') && echoed.contains('0'),
            "the lease's own numbers came back: {echoed}"
        );
    }

    /// A plugin that never answers costs a timeout, not a hung deck.
    #[tokio::test]
    async fn a_silent_panel_times_out_rather_than_hanging() {
        let mut process = shell("sleep 30");
        process.timeout_secs = 1;
        let err = ask(&process, lease(10, 2), &[])
            .await
            .expect_err("times out");
        assert!(matches!(err, PanelError::Timeout { .. }), "{err:?}");
    }

    /// The reaping witness: a panel's **grandchild** is dead once the tick is
    /// over, on both the timeout path and the success path.
    ///
    /// `a_silent_panel_times_out_rather_than_hanging` proves only that the call
    /// returns. This proves the thing that actually matters — that nothing is
    /// left running — and it uses a grandchild because `kill_on_drop` reaps the
    /// direct child alone; a backgrounded helper is exactly what only the
    /// process-group kill reaches. It is also the witness for the success path,
    /// where an earlier version disarmed the guard on EOF and leaked one
    /// grandchild per tick forever.
    #[tokio::test]
    async fn a_panels_grandchild_does_not_outlive_its_tick() {
        // `TempDir`, not a name under the process temp dir: the scan in
        // `tests/no_ambient_reads.rs` reads this file too.
        let scratch = tempfile::TempDir::new().expect("a scratch directory");
        let dir = scratch.path();

        for (label, script, answers) in [
            ("timeout", "sleep 30 & echo $! > PIDFILE; sleep 30", false),
            (
                "success",
                "sleep 30 & echo $! > PIDFILE; cat > /dev/null;                  printf '{\"point\":\"frame\",\"body\":{\"protocol_version\":1,\"surface\":\"command\",\"tick\":1,\
                 \"paint\":{\"lines\":[]}}}\n'",
                true,
            ),
        ] {
            let pidfile = dir.join(format!("{label}.pid"));
            let _ = std::fs::remove_file(&pidfile);
            let mut process = shell(&script.replace("PIDFILE", &pidfile.display().to_string()));
            process.timeout_secs = 1;

            let outcome = ask(&process, lease(10, 2), &[]).await;
            assert_eq!(
                outcome.is_ok(),
                answers,
                "{label}: the exchange itself behaved as expected"
            );

            // The grandchild's pid, as the script recorded it.
            let mut pid = None;
            for _ in 0..50 {
                if let Ok(text) = std::fs::read_to_string(&pidfile)
                    && let Ok(parsed) = text.trim().parse::<i32>()
                {
                    pid = Some(parsed);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let pid =
                pid.unwrap_or_else(|| panic!("{label}: the script recorded its helper's pid"));

            // The kill is a signal, so let the group actually die before asking.
            let mut alive = true;
            for _ in 0..50 {
                // SAFETY: signal 0 performs the permission and existence check
                // without delivering anything, which is the only way to ask
                // "is this pid still there".
                if unsafe { libc::kill(pid, 0) } != 0 {
                    alive = false;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert!(
                !alive,
                "{label}: the panel's backgrounded helper (pid {pid}) outlived its tick"
            );
        }
    }

    /// A plugin that writes rubbish is named, not silently ignored.
    #[tokio::test]
    async fn an_unreadable_frame_is_a_decode_error() {
        let err = ask(&shell("cat > /dev/null; echo not-json"), lease(10, 2), &[])
            .await
            .expect_err("decodes nothing");
        assert!(matches!(err, PanelError::Decode { .. }), "{err:?}");
    }

    /// The environment is the manifest's allowlist and nothing else — the
    /// consent document's promise, enforced at the spawn.
    ///
    /// The lookup is a map rather than the process environment, which is what
    /// `tests/no_ambient_reads.rs` requires of this crate and what lets this
    /// test state its inputs instead of mutating the world other tests share.
    #[tokio::test]
    async fn a_panel_sees_only_the_variables_its_manifest_named() {
        let world = |name: &str| match name {
            "STELLA_PANEL_ALLOWED" => Some("yes".to_string()),
            "STELLA_PANEL_SECRET" => Some("no".to_string()),
            "PATH" => std::option_env!("PATH").map(str::to_string),
            _ => None,
        };
        let mut process = shell(
            r#"cat > /dev/null; printf '{"point":"frame","body":{"protocol_version":1,"surface":"command","tick":1,"paint":{"lines":[{"spans":[{"text":"%s/%s"}]}]}}}\n' "${STELLA_PANEL_ALLOWED:-absent}" "${STELLA_PANEL_SECRET:-absent}""#,
        );
        process.env = vec!["STELLA_PANEL_ALLOWED".into()];
        let env = resolve_env(&process, world);
        assert!(
            env.iter().all(|(name, _)| name != "STELLA_PANEL_SECRET"),
            "the un-named variable never even reaches the spawn: {env:?}"
        );
        let tick = ask(&process, lease(30, 2), &env).await.expect("a frame");
        let PanelPaint::Lines(lines) = tick.frame.expect("a frame").paint else {
            panic!("expected lines")
        };
        assert_eq!(
            lines[0].spans[0].text.as_str(),
            "yes/absent",
            "the allowlisted variable is present and the other is not"
        );
    }
}
