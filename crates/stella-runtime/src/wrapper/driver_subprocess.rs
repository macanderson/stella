//! The driver channel's transport — a JSON session request on stdin, capability
//! asks and their answers in both directions, and a `next` on stdout that ends
//! it.
//!
//! `doc:backlog-self-driving` §3.0 named the channel and #3599's B0 built its
//! two ends: `stella_plugin::driver` is the wire, [`DriverCallGate`] is the
//! gate. Neither one moves a byte. This module is what runs between them, and
//! without it a `[driver]` grant is a consent document describing a program
//! nothing starts (#3634).
//!
//! ```text
//!  host                    driver process
//!   │  {"point":"drive","body":{"session":"…"}}   │
//!   ├───────────────────── stdin ────────────────>│
//!   │<──────────────────── stdout ────────────────┤  {"call":"backlog_next","id":1}
//!   ├───────────────────── stdin ────────────────>│  {"result":1,"ok":{}}
//!   │<──────────────────── stdout ────────────────┤  {"point":"drive","body":{"next":{…}}}
//! ```
//!
//! # A sibling of [`SubprocessWrapper`](super::SubprocessWrapper), not a
//! generalisation of it
//!
//! The same choice [`driver_call`](super::driver_call) made one layer up, for
//! the same reason, and it is worth stating plainly because the two loops below
//! do rhyme. What the two conversations share is mechanical and **is** shared:
//! [`Framer`] decides where a message ends and
//! [`write_frames`] owns stdin, both extracted
//! into [`framing`](super::framing) rather than copied, because the framer is
//! the one piece here whose second implementation would be quadratic and pass
//! its tests.
//!
//! What is not shared is every decision the loop makes: a wrapper answers the
//! point it was asked and is refused if it answers another one; a driver has a
//! single point and ends its session with a `next` instead. A wrapper's
//! response carries a protocol version to refuse; a driver's request carries
//! none, because `DriveRequest` has no such field. A wrapper's contribution is
//! then checked against its manifest by
//! [`admissible`](super::admissible); a driver contributes nothing to a turn,
//! so there is nothing to admit. Folding those into one parameterised pump
//! would put four host-visible policies behind an associated type, and the
//! merge would be the same one that would make `arbiter` quietly mean "may
//! merge to `main`".
//!
//! # The three bounds this transport owns
//!
//! 1. **The budget covers the whole session**, not each message of it — a
//!    driver must not be able to buy time by talking (`doc:wrapper-socket` §6b
//!    bound 1, which is about a point and holds identically for a session).
//!    It is clamped to [`MAX_WRAPPER_TIMEOUT`]: one
//!    subprocess plane, one clamp, the same argument that made
//!    [`SubprocessWrapper`](super::SubprocessWrapper) import the hook plane's
//!    ceiling rather than restate it. B2 is where a session first performs work
//!    that could want more, and it is the phase that should re-argue the number
//!    with something measured.
//! 2. **Stdin stays open only when a call is actually on offer.**
//!    [`DriverCallGate::offers_calls`] is consulted before the session is
//!    opened, because a driver that declares nothing reads its request with
//!    `cat` or `sys.stdin.read()` — both wait for EOF — and a pipe held open
//!    for a conversation it can never have hangs it until the deadline.
//! 3. **A refused call is answered and the session continues.** The refusal is
//!    a value on the wire, never a death: killing a driver for asking is how
//!    its author learns nothing, and a loop that dies on a refusal cannot
//!    degrade to the smaller cycle it is still permitted.
//!
//! # What it inherits from the wrapper socket's spawn policy, deliberately
//!
//! A driver is third-party code a user installed, exactly as a wrapper is, so
//! the four decisions `super::subprocess`'s header argues for are made here in
//! the same way and for the same reasons: the environment is **cleared** and
//! set to exactly what the host passed, model credentials are refused out of it
//! by [`refuses_env_name`] and *reported* rather than
//! dropped silently, no working directory is set (this crate reads no ambient
//! state at all — `tests/no_ambient_reads.rs`), the read is capped at
//! [`MAX_CAPTURE_BYTES`] at ingest, and the child leads its own process group
//! so a backgrounded grandchild cannot outlive the session.
//!
//! # What a session does *not* carry yet
//!
//! No arguments and no result payloads, because
//! [`DriverCall`](stella_plugin::DriverCall) has none at B0 and every capability
//! this host can perform is answered
//! [`Unsupported`](stella_plugin::HostCallRefusal::Unsupported) by
//! [`NoDriverCapabilities`](super::NoDriverCapabilities). That is the honest
//! state rather than a placeholder: this module makes a capability *holdable*,
//! and B1-B6 (#3599) give each one something to do.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use stella_plugin::{DriveRequest, DriveResponse, DriverCallResponse, DriverMessage};
use stella_tools::exec::MAX_CAPTURE_BYTES;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;

use super::MAX_WRAPPER_TIMEOUT;
use super::driver_call::{DriverCallGate, DriverSession};
use super::error::{DriverError, bounded};
use super::framing::{Framer, OUTPUT_EXCERPT_CHARS, READ_CHUNK, write_frames};
use super::subprocess::refuses_env_name;

/// A driver that runs as a child process and speaks the driver channel.
///
/// Its [`Debug`] is hand-written and prints environment **names** only, for
/// [`SubprocessWrapper`](super::SubprocessWrapper)'s reason: deriving it would
/// put every resolved value in every `tracing::debug!` and every assertion
/// message.
#[derive(Clone)]
pub struct SubprocessDriver {
    program: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    timeout: Duration,
    gate: Option<Arc<DriverCallGate>>,
}

/// A declared driver, together with what the channel took away from it.
///
/// [`SubprocessDriver::declare`] answers with this rather than a bare driver so
/// a refusal has to be *read*, which is
/// [`AdmittedWrapper`](super::AdmittedWrapper)'s argument unchanged: a plugin
/// that silently does not get what its manifest asked for is a bug its author
/// cannot diagnose.
#[derive(Debug)]
#[must_use = "the refused names must be surfaced: a driver that silently does not get what its \
              manifest asked for is a bug its author cannot diagnose"]
pub struct AdmittedDriver {
    /// The transport, carrying the admitted pairs and no others.
    pub driver: SubprocessDriver,
    /// The declared names withheld as model credentials, in the order they were
    /// given. Empty in the normal case.
    pub refused: Vec<String>,
}

/// Names, never values — see [`SubprocessWrapper`](super::SubprocessWrapper)'s
/// identical implementation for why deriving this is a credential leak.
impl std::fmt::Debug for SubprocessDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubprocessDriver")
            .field("program", &self.program)
            .field("args", &self.args)
            .field(
                "env_names",
                &self.env.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            )
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// The writer task's state for one session.
///
/// [`super::subprocess`]'s `WriterState`, and the two fields move together for
/// the same reason: an ask arriving after a prior send already failed joins
/// `writer` to recover the fault that killed it
/// ([`DriverError::AnswerChannelFailed`]) rather than reporting the unrelated
/// "no channel" error.
struct WriterState<'a> {
    /// The channel an answer is written through. `None` from the start when no
    /// session conversation was ever offered (no gate, or the manifest declares
    /// no `[driver] calls`); moves from `Some` to `None` when a send finds the
    /// writer already gone.
    frames: Option<mpsc::Sender<Vec<u8>>>,
    /// The writer task, taken (and joined) only when an ask arrives with
    /// `frames` already `None` and the conversation was genuinely open.
    writer: &'a mut Option<tokio::task::JoinHandle<std::io::Result<()>>>,
}

/// What a session's read loop accumulates for the caller to read once it ends.
///
/// One value rather than three out-parameters: every one of these is only
/// meaningful *after* `converse` returns, and the caller reads all three
/// together to decide which failure a silent driver was.
struct SessionTrace<'a> {
    /// Everything the driver wrote to stderr, which is where its reason
    /// usually is.
    stderr_seen: &'a mut Vec<u8>,
    /// Bytes read across both streams, checked against [`MAX_CAPTURE_BYTES`]
    /// at ingest.
    read: &'a mut usize,
    /// The last capability this session served, if any (#3794). Read only when
    /// the driver exited cleanly with no `next`: an ask and then silence is an
    /// abandoned session, and silence with no ask is
    /// [`DriverError::NoResponse`].
    served_call: &'a mut Option<stella_plugin::DriverCall>,
}

impl SubprocessDriver {
    /// Declare a driver process.
    ///
    /// `argv` is a program and its arguments, never a shell string (#1400).
    /// `env` is the **entire** environment the child will see; the transport
    /// clears whatever this process was carrying first. `timeout` is clamped to
    /// [`MAX_WRAPPER_TIMEOUT`] — a driver cannot buy
    /// itself an unbounded session any more than a wrapper can buy an unbounded
    /// point.
    ///
    /// Every pair whose name [`refuses_env_name`]
    /// grades as a model credential is dropped here and named in
    /// [`AdmittedDriver::refused`]. A driver is the plugin with the *most*
    /// reason to want one — it is trying to get work done — and the least
    /// business holding one: invariant 3's "every model call is made by the
    /// host" is what `work_start` will be for.
    ///
    /// # Errors
    ///
    /// [`DriverError::EmptyArgv`] when `argv` names no program, and
    /// [`DriverError::ZeroTimeout`] for a budget that would kill the child
    /// before it ran. A refused credential is deliberately **not** an error: the
    /// driver still runs, exactly as the install consent said it would.
    pub fn declare(
        argv: Vec<String>,
        env: Vec<(String, String)>,
        timeout: Duration,
    ) -> Result<AdmittedDriver, DriverError> {
        let Some((program, args)) = argv.split_first() else {
            return Err(DriverError::EmptyArgv);
        };
        let timeout = timeout.min(MAX_WRAPPER_TIMEOUT);
        if timeout.is_zero() {
            return Err(DriverError::ZeroTimeout);
        }
        let mut refused = Vec::new();
        let mut admitted = Vec::with_capacity(env.len());
        for (name, value) in env {
            if refuses_env_name(&name) {
                // The value is dropped rather than kept beside the name: a
                // refused credential must not be reachable from the report
                // either.
                refused.push(name);
            } else {
                admitted.push((name, value));
            }
        }
        Ok(AdmittedDriver {
            driver: Self {
                program: program.clone(),
                args: args.to_vec(),
                env: admitted,
                timeout,
                gate: None,
            },
            refused,
        })
    }

    /// Serve this driver's declared capabilities from `gate`.
    ///
    /// Without one the session is strictly one-directional: the request goes
    /// out, stdin closes, and the driver answers with its `next`. That is a
    /// legitimate driver — one that only reports what it decided — and it is
    /// also what a driver whose manifest declares no `[driver] calls` gets.
    ///
    /// The gate is `Arc` because the host holds it too: the refusals it recorded
    /// are what the host reports, and a report only this transport could read
    /// would be the silence the channel refuses.
    #[must_use]
    pub fn serving(mut self, gate: Arc<DriverCallGate>) -> Self {
        self.gate = Some(gate);
        self
    }

    /// The program this driver starts — what an error message names.
    #[must_use]
    pub fn program(&self) -> &str {
        &self.program
    }

    /// The effective session budget after the clamp.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Open one driver session: the request out, every capability ask answered,
    /// and the `next` that ends it back.
    ///
    /// **One budget covers the whole of it**, which is bound 1 of this module's
    /// header rather than an implementation choice.
    ///
    /// The write and the read are concurrent on purpose: writing the whole
    /// request before reading a byte deadlocks the moment either side fills a
    /// pipe buffer, and a host writing an answer while the driver writes its
    /// next ask would deadlock the same way. So stdin belongs to one task that
    /// does nothing else, and this loop only ever reads.
    ///
    /// # Errors
    ///
    /// Every variant of [`DriverError`]. The one worth naming here is
    /// [`DriverError::NoResponse`]: a driver that exits without a `next` has
    /// decided nothing, and a caller must not read that as either terminal
    /// answer.
    pub async fn drive(&self, request: DriveRequest) -> Result<DriveResponse, DriverError> {
        let body = serde_json::to_vec(&request).map_err(DriverError::Encode)?;

        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A driver that outlives its budget is killed; without this, a child
            // that ignores the drop keeps running after the session it was
            // deciding for has been reported.
            .kill_on_drop(true);
        // The rest of the tree. A driver is the plugin most likely to background
        // something — it exists to start long work — and a backgrounded
        // grandchild is exactly what `kill_on_drop` cannot reach.
        stella_tools::exec::detach_into_own_process_group(&mut command);

        let mut child = command.spawn().map_err(|source| DriverError::Spawn {
            program: self.program.clone(),
            source,
        })?;
        // The child leads the group, so its pid *is* the group id. Armed
        // immediately: between here and the disarm below, every path out — a
        // returned error, a dropped future, a panic — reaps the whole group.
        let mut guard =
            stella_tools::exec::GroupKillGuard::arm(child.id().unwrap_or(0).cast_signed());
        let (Some(stdin), Some(mut stdout), Some(mut stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(DriverError::Transport {
                program: self.program.clone(),
                source: std::io::Error::other(
                    "the child was started without a full set of stdio pipes",
                ),
            });
        };

        let (frames, outbox) = mpsc::channel::<Vec<u8>>(1);
        let mut writer = Some(tokio::spawn(write_frames(stdin, outbox)));
        // A send failing means the writer is gone, which a join diagnoses with
        // the actual I/O error; there is nothing better to say here than "carry
        // on and read what the child has to say".
        let _ = frames.send(body).await;

        // Stdin closes now unless the driver can actually ask for something —
        // bound 2 of this module's header.
        let conversation = self.gate.as_ref().filter(|gate| gate.offers_calls());
        let session = conversation.map(|gate| gate.open());
        let frames = session.is_some().then_some(frames);
        let writer_state = WriterState {
            frames,
            writer: &mut writer,
        };

        let mut stderr_seen = Vec::new();
        let mut read = 0usize;
        // The last capability this session served, if any. Read only on the
        // path where the driver never ended its session: one that asked and
        // then went silent abandoned it, which is a different failure from one
        // that decided nothing — see `DriverError::UnansweredCall`.
        let mut served_call = None;
        let settled = tokio::time::timeout(self.timeout, async {
            let response = self
                .converse(
                    &mut stdout,
                    &mut stderr,
                    writer_state,
                    session.as_ref(),
                    SessionTrace {
                        stderr_seen: &mut stderr_seen,
                        read: &mut read,
                        served_call: &mut served_call,
                    },
                )
                .await?;
            // `converse` has dropped the sender, so the writer has shut stdin
            // down and the child has its EOF. Waiting for the exit *after* the
            // answer is what keeps a driver that answers and then dies loudly an
            // `Exit` rather than a success.
            let status = self
                .settle(
                    &mut child,
                    &mut stdout,
                    &mut stderr,
                    &mut stderr_seen,
                    &mut read,
                )
                .await?;
            Ok::<_, DriverError>((response, status))
        })
        .await;

        let (response, status) = match settled {
            Ok(Ok(settled)) => settled,
            Ok(Err(error)) => {
                // The session failed with the child's state unknown — it may
                // still be writing past the ceiling, or wedged. Kill the group
                // before answering rather than leaving the drop to reach the
                // direct child alone.
                guard.kill_now();
                // `converse` already joined the writer when it needed to report
                // its own failure (`AnswerChannelFailed`); any other error path
                // leaves it running, so it is aborted here.
                if let Some(writer) = writer.take() {
                    writer.abort();
                }
                return Err(error);
            }
            Err(_) => {
                guard.kill_now();
                if let Some(writer) = writer.take() {
                    writer.abort();
                }
                return Err(DriverError::Timeout {
                    program: self.program.clone(),
                    timeout: self.timeout,
                });
            }
        };
        // The child has answered and been reaped. Anything it detached on
        // purpose is not this transport's to kill.
        guard.disarm();

        // A child that answered without reading its request is not an error: a
        // fixed-answer driver closing stdin early is legitimate, and the broken
        // pipe it causes says nothing about the answer. Any other write failure
        // did lose the request, so it is reported. A join failure — the writer
        // task panicked or was cancelled — is the same class of loss.
        //
        // `writer` is always `Some` here: `converse` only ever takes it on its
        // way to returning an error, and this path is reached only when it
        // returned `Ok`.
        if let Some(writer) = writer {
            match writer.await {
                Ok(Err(source)) if source.kind() != std::io::ErrorKind::BrokenPipe => {
                    return Err(DriverError::Transport {
                        program: self.program.clone(),
                        source,
                    });
                }
                Err(join) => {
                    return Err(DriverError::Transport {
                        program: self.program.clone(),
                        source: std::io::Error::other(join),
                    });
                }
                Ok(_) => {}
            }
        }

        if !status.success() {
            return Err(DriverError::Exit {
                program: self.program.clone(),
                status: status
                    .code()
                    .map_or_else(|| "a signal".to_string(), |code| format!("code {code}")),
                stderr: bounded(&stderr_seen, OUTPUT_EXCERPT_CHARS),
            });
        }

        // The child exited cleanly and wrote no `next`. Which failure that is
        // depends on whether it had asked the host for something first: an ask
        // and then silence is an abandoned session, and naming the call is
        // what tells its author where.
        response.ok_or_else(|| match served_call {
            Some(call) => DriverError::UnansweredCall {
                program: self.program.clone(),
                call,
            },
            None => DriverError::NoResponse {
                program: self.program.clone(),
                stderr: bounded(&stderr_seen, OUTPUT_EXCERPT_CHARS),
            },
        })
    }

    /// Read the driver's messages until the one that ends the session,
    /// answering every capability ask on the way.
    ///
    /// `Ok(None)` is stdout reaching EOF with no session response in it — the
    /// child ended without deciding anything, which the caller reports as a
    /// [`DriverError::Exit`] or a [`DriverError::NoResponse`] once it knows how
    /// the child died.
    ///
    /// Framing is [`Framer`]'s: a message ends where its JSON value ends,
    /// decided by the parser rather than by a newline, so a driver may
    /// pretty-print and may pipeline two asks in one write.
    async fn converse(
        &self,
        stdout: &mut tokio::process::ChildStdout,
        stderr: &mut tokio::process::ChildStderr,
        mut writer_state: WriterState<'_>,
        session: Option<&DriverSession<'_>>,
        trace: SessionTrace<'_>,
    ) -> Result<Option<DriveResponse>, DriverError> {
        let SessionTrace {
            stderr_seen,
            read,
            served_call,
        } = trace;
        // Whether a conversation was ever open, decided once and fixed for the
        // whole session: `writer_state.frames` only ever moves from `Some` to
        // `None` below (a send failure), never the reverse, so this is exactly
        // what tells a later `None` apart from the manifest/gate case that
        // starts `None` and stays there.
        let conversation_was_open = writer_state.frames.is_some();
        let mut framer = Framer::default();
        let mut chunk = vec![0u8; READ_CHUNK];
        let mut errchunk = vec![0u8; READ_CHUNK];
        let mut stdout_open = true;
        let mut stderr_open = true;

        while stdout_open {
            let (result, is_stdout) = tokio::select! {
                // Both reads are cancellation-safe (`AsyncReadExt::read` either
                // fills or does nothing), which is what lets the loser of each
                // race simply be asked again.
                result = stdout.read(&mut chunk), if stdout_open => (result, true),
                result = stderr.read(&mut errchunk), if stderr_open => (result, false),
            };
            let filled = result.map_err(|source| DriverError::Transport {
                program: self.program.clone(),
                source,
            })?;
            if filled == 0 {
                if is_stdout {
                    stdout_open = false;
                } else {
                    stderr_open = false;
                }
                continue;
            }
            // The cap is applied at ingest, over both streams together: a
            // ceiling checked after the fact is a ceiling paid for.
            *read += filled;
            if *read > MAX_CAPTURE_BYTES {
                return Err(DriverError::OutputCap {
                    program: self.program.clone(),
                    stream: if is_stdout { "stdout" } else { "stderr" },
                    cap: MAX_CAPTURE_BYTES,
                });
            }
            if !is_stdout {
                stderr_seen.extend_from_slice(&errchunk[..filled]);
                continue;
            }
            framer.push(&chunk[..filled]);

            while let Some(message) = self.take_message(&mut framer)? {
                match message {
                    // The session response terminates the conversation. Dropping
                    // the sender here is what shuts stdin down, so the driver
                    // gets its EOF and can exit.
                    DriverMessage::Response(response) => return Ok(Some(response)),
                    DriverMessage::Call(ask) => {
                        let Some(sender) = &writer_state.frames else {
                            // Two different shapes read as "no sender", and
                            // conflating them sends whoever is debugging a
                            // broken pipe to edit `[driver] calls`, which does
                            // nothing.
                            if conversation_was_open {
                                // The channel was open — proven by the fact that
                                // something was already sent through it — and
                                // failed transport-side afterwards, most likely
                                // because the child closed its stdin without
                                // reading a prior answer. Join the writer to
                                // recover *why*, rather than aborting it unread.
                                let source = match writer_state.writer.take() {
                                    Some(handle) => match handle.await {
                                        Ok(Err(source)) => source,
                                        Ok(Ok(())) => std::io::Error::other(
                                            "the writer task exited normally, but a send to it \
                                             had already failed",
                                        ),
                                        Err(join) => std::io::Error::other(join),
                                    },
                                    // Unreachable in practice: nothing else takes
                                    // `writer` before this arm can run. Named
                                    // rather than unwrapped, per invariant 5.
                                    None => std::io::Error::other(
                                        "the writer task's outcome was already taken",
                                    ),
                                };
                                return Err(DriverError::AnswerChannelFailed {
                                    program: self.program.clone(),
                                    call: ask.call,
                                    source,
                                });
                            }
                            // Stdin is already closed because no callable
                            // capability was on offer, so there is no way to tell
                            // the driver it may not have this. A hang until the
                            // deadline would be the alternative; this at least
                            // names the two things that cause it.
                            return Err(DriverError::UnannouncedCall {
                                program: self.program.clone(),
                                call: ask.call,
                            });
                        };
                        let outcome = match session {
                            Some(session) => session.call(ask.call).await,
                            // Unreachable: `frames` is `Some` only when a session
                            // was opened. Written as a value rather than an
                            // `expect`, because invariant 5 makes no exception
                            // for "I checked earlier".
                            None => super::driver_call::unserved(ask.call),
                        };
                        let answer = serde_json::to_vec(&DriverCallResponse {
                            result: ask.id,
                            outcome,
                        })
                        .map_err(DriverError::Encode)?;
                        if sender.send(answer).await.is_err() {
                            // The writer is gone, so no further answer can reach
                            // the driver. Stop pretending otherwise: if the
                            // session response is what arrives next anyway,
                            // `drive`'s own join on `writer` reports why; if
                            // another ask arrives instead, the branch above joins
                            // it here and reports `AnswerChannelFailed`.
                            writer_state.frames = None;
                        }
                        // Served, and the session is still open. If stdout ends
                        // before a `next` arrives, this is the capability the
                        // driver abandoned it after.
                        *served_call = Some(ask.call);
                    }
                }
            }
        }
        // Stdout ended. Nothing is left to parse: `Framer` completes a value
        // the moment its nesting depth returns to zero rather than on a
        // newline, and the drain above runs until `take` yields `None`, so any
        // message the child finished writing was already taken inside the
        // loop. The caller decides what the silence means.
        Ok(None)
    }

    /// Take the next complete message the framer has assembled, if there is one.
    fn take_message(&self, framer: &mut Framer) -> Result<Option<DriverMessage>, DriverError> {
        let Some(message) = framer.take().map_err(|detail| DriverError::Decode {
            program: self.program.clone(),
            answer: bounded(framer.buffered(), OUTPUT_EXCERPT_CHARS),
            source: serde::de::Error::custom(detail),
        })?
        else {
            return Ok(None);
        };
        serde_json::from_slice(&message)
            .map(Some)
            .map_err(|source| DriverError::Decode {
                program: self.program.clone(),
                answer: bounded(&message, OUTPUT_EXCERPT_CHARS),
                source,
            })
    }

    /// Drain what the child still has to say on **both** streams and wait for it
    /// to exit.
    ///
    /// `converse` stops reading stdout the instant it finds the session
    /// response, but the pipe behind that file descriptor does not know the
    /// session is over and neither does a child still writing to it. A driver
    /// that prints trailing output after answering fills that pipe and blocks in
    /// `write(2)`, and a child blocked in `write(2)` never reaches `exit(2)` —
    /// so waiting for it here without draining would turn a driver that had
    /// already answered correctly into a [`DriverError::Timeout`]. The bytes are
    /// discarded; only the ceiling still applies, over both streams together.
    async fn settle(
        &self,
        child: &mut tokio::process::Child,
        stdout: &mut tokio::process::ChildStdout,
        stderr: &mut tokio::process::ChildStderr,
        stderr_seen: &mut Vec<u8>,
        read: &mut usize,
    ) -> Result<std::process::ExitStatus, DriverError> {
        let mut chunk = vec![0u8; READ_CHUNK];
        let mut errchunk = vec![0u8; READ_CHUNK];
        let mut stdout_open = true;
        let mut stderr_open = true;
        while stdout_open || stderr_open {
            let (result, is_stdout) = tokio::select! {
                result = stdout.read(&mut chunk), if stdout_open => (result, true),
                result = stderr.read(&mut errchunk), if stderr_open => (result, false),
            };
            let filled = result.map_err(|source| DriverError::Transport {
                program: self.program.clone(),
                source,
            })?;
            if filled == 0 {
                if is_stdout {
                    stdout_open = false;
                } else {
                    stderr_open = false;
                }
                continue;
            }
            *read += filled;
            if *read > MAX_CAPTURE_BYTES {
                return Err(DriverError::OutputCap {
                    program: self.program.clone(),
                    stream: if is_stdout { "stdout" } else { "stderr" },
                    cap: MAX_CAPTURE_BYTES,
                });
            }
            if !is_stdout {
                stderr_seen.extend_from_slice(&errchunk[..filled]);
            }
            // Trailing stdout carries nothing this transport reads — the session
            // response already ended the conversation — so it is drained and
            // dropped, never buffered.
        }
        child.wait().await.map_err(|source| DriverError::Transport {
            program: self.program.clone(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_driver_needs_a_program_and_a_budget() {
        assert!(matches!(
            SubprocessDriver::declare(Vec::new(), Vec::new(), Duration::from_secs(1)),
            Err(DriverError::EmptyArgv)
        ));
        assert!(matches!(
            SubprocessDriver::declare(vec!["/bin/sh".into()], Vec::new(), Duration::ZERO),
            Err(DriverError::ZeroTimeout)
        ));
    }

    /// The budget is an ask, the ceiling is the host's — a driver cannot buy an
    /// unbounded session by declaring one.
    #[test]
    fn the_session_budget_is_clamped_to_the_hosts_ceiling() {
        let admitted = SubprocessDriver::declare(
            vec!["/bin/sh".into()],
            Vec::new(),
            MAX_WRAPPER_TIMEOUT + Duration::from_secs(3_600),
        )
        .expect("a program and a budget");
        assert_eq!(admitted.driver.timeout(), MAX_WRAPPER_TIMEOUT);
        assert!(admitted.refused.is_empty());
    }

    /// A model credential is withheld and **named**. The driver still runs —
    /// the install consent promised "it will not get it", never "it will not
    /// run".
    #[test]
    fn a_model_credential_is_withheld_from_a_driver_and_reported() {
        let admitted = SubprocessDriver::declare(
            vec!["/bin/sh".into()],
            vec![
                ("ANTHROPIC_API_KEY".into(), "sk-secret".into()),
                ("PATH".into(), "/usr/bin".into()),
            ],
            Duration::from_secs(1),
        )
        .expect("a program and a budget");
        assert_eq!(admitted.refused, vec!["ANTHROPIC_API_KEY".to_string()]);
        // And the value is gone with the name, not kept beside it: the `Debug`
        // that a host logs prints names only, so the refused value is not
        // reachable from either half.
        let printed = format!("{:?}", admitted.driver);
        assert!(!printed.contains("sk-secret"), "{printed}");
        assert!(printed.contains("PATH"), "{printed}");
    }
}
