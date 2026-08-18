//! The subprocess transport — a JSON request on stdin, a JSON response on
//! stdout, and between them whatever the plugin asks the host for, in whatever
//! language the plugin is written in.
//!
//! This is the path that makes Python and TypeScript first-class rather than
//! bolted on (`doc:pipeline-as-plugins` §5), so it is the path the tests
//! exercise: `tests/wrapper_socket.rs` runs a wrapper written in `sh` — no
//! Rust, no SDK, a JSON parser it does not even need — through a whole turn.
//!
//! # One exchange, or a bounded conversation
//!
//! A point was strictly one request and one response until #3540. It is now a
//! **conversation that ends in the point response** (`doc:wrapper-socket` §6b):
//! the plugin may emit host calls and read their answers first, if its manifest
//! declared any and a [`HostCallGate`] was attached ([`SubprocessWrapper::serving`]).
//! Two consequences worth stating where the code is:
//!
//! - **Stdin stays open only when a call is actually on offer.** A plugin that
//!   declares none reads its request with `cat` or `sys.stdin.read()`, which
//!   wait for EOF — so holding the pipe open for a conversation it can never
//!   have would hang it until the deadline.
//! - **The timeout covers the whole conversation.** Not each message: a plugin
//!   must not be able to buy time by talking.
//!
//! # The shape is the one hooks already proved
//!
//! `stella_core::hooks` has run operator-authored commands with the event
//! payload as JSON on stdin since the TypeScript port, with a 60s default and
//! a 600s ceiling. Those two constants are **imported, not restated**: the
//! budgets are the same budgets — one subprocess plane, one clamp — and a
//! number in two places is how the last limit died (AGENTS.md § God files).
//!
//! The other two halves of that spawn policy are imported for the same reason,
//! and a plugin needs them **more** than a hook does: a hook is at least
//! repository- or operator-authored, while a plugin is third-party code a user
//! installed once.
//!
//! - **The read is capped** at [`stella_tools::exec::MAX_CAPTURE_BYTES`], at
//!   ingest. `wait_with_output` holds whatever the child writes, so a cap
//!   applied to an error message afterwards only ever ran once the whole
//!   stream was already resident — and `after_turn` is documented as the point
//!   that runs a test suite or a benchmark, so the volume is not this host's to
//!   trust. Where the hook plane elides the middle and keeps reading (its
//!   output *is* the product), this transport refuses: a truncated message
//!   cannot be decoded, and every byte read after the ceiling is spent on a
//!   call already lost ([`WrapperError::OutputCap`]).
//!
//!   The ceiling is enforced here rather than by
//!   [`stella_tools::exec::capture`], and the difference is the conversation:
//!   `capture` reads until the child **exits**, and a plugin waiting for an
//!   answer to a host call has not exited. What accumulates toward the ceiling
//!   is therefore what is not yet a whole message — see [`Framer`], which also
//!   refuses output that is not a message at all on the byte rather than at the
//!   ceiling.
//! - **The child leads its own process group**
//!   ([`stella_tools::exec::detach_into_own_process_group`]), with
//!   [`stella_tools::exec::GroupKillGuard`] armed on its pid. `kill_on_drop`
//!   reaches the direct child and nothing else, so a plugin that backgrounds
//!   work — again, the `after_turn` workload — would leave grandchildren
//!   running past the turn they were gathering evidence for. The guard is
//!   fired on the timeout and on the refusal, and disarmed once the child has
//!   answered.
//!
//! # What this transport does not do, on purpose
//!
//! - **It sets no working directory.** `doc:wrapper-socket` §6 bans cwd from
//!   every signature, and everything the plugin needs arrives in the request —
//!   the candidate worktree as a `CandidateGrant`, which is the host's own
//!   handle plus the canonical root and test invocation the plugin acts on
//!   (#3498). The root in that grant is where the plugin works; it is not a
//!   permission, because every path the plugin names *back* is re-resolved
//!   against the handle host-side and refused if it escapes. Setting a cwd
//!   would additionally break a hard constraint here rather than a preference:
//!   this crate must not read process-global state at all
//!   (`tests/no_ambient_reads.rs`), and a cwd is the most process-global thing
//!   there is.
//! - **It clears the environment and sets exactly what it was given.** A
//!   plugin is third-party code a user installed; inheriting the operator's
//!   environment hands it `ANTHROPIC_API_KEY` and every other credential the
//!   shell was carrying, silently, forever — which would make invariant 3's
//!   "every model call is made by the host" a policy rather than a property.
//!   Default-deny is structural: [`SubprocessWrapper::declare`] takes the pairs,
//!   so an empty list is what a caller that thought about nothing gets. The
//!   manifest half of that decision is `[runtime] env`, whose `child_env`
//!   returns exactly the pairs to pass here.
//! - **It refuses a model credential even when the manifest asked for one.**
//!   Default-deny answers "what did the author not name?"; it does not answer
//!   "what may the author name?", and `[runtime] env = ["ANTHROPIC_API_KEY"]`
//!   is a well-formed manifest. So [`refuses_env_name`] is applied *here*, at
//!   the boundary every host crosses, rather than in one binary's spawn
//!   builder — a driver that is not `stella-cli` (`stella-serve`, an embedded
//!   `stella-engine` host, a test) reaches this constructor and nothing else,
//!   and a security property that only holds for one of the three drivers
//!   `doc:wrapper-socket` §6 requires is a policy again (#3512).
//!
//!   The refusal is **reported, never silent**: [`SubprocessWrapper::declare`]
//!   answers with an [`AdmittedWrapper`], whose `refused` list a caller has to
//!   name to reach the wrapper beside it. A plugin that quietly does not get
//!   what its manifest asked for is a bug its author cannot diagnose, and the
//!   fix they have — declare a `[roles]` tier and let the host make the call —
//!   is one they can only take if they are told.
//! - **It does not interpret a shell string.** argv, always — the #1400 rule
//!   every other spawned thing in this workspace follows.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use stella_core::hooks::{DEFAULT_HOOK_TIMEOUT_MS, MAX_HOOK_TIMEOUT_MS};
use stella_plugin::{
    AfterTurnRequest, AfterTurnResponse, BeforeTurnRequest, BeforeTurnResponse, HostCallResponse,
    PROTOCOL_VERSION, PluginMessage, WrapperRequest, WrapperResponse,
};
use stella_tools::exec::MAX_CAPTURE_BYTES;
use stella_tools::subprocess_env::is_sensitive_env_name;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;

use super::error::bounded;
use super::framing::{Framer, OUTPUT_EXCERPT_CHARS, READ_CHUNK, write_frames};
use super::host_call::{HostCallChannel, HostCallGate};
use super::{TurnWrapper, WrapperError};

/// The default budget for one wrapper call — 60s, the hook plane's default.
pub const DEFAULT_WRAPPER_TIMEOUT: Duration = Duration::from_millis(DEFAULT_HOOK_TIMEOUT_MS);

/// The ceiling a configured budget is clamped to — 10 minutes, the hook
/// plane's ceiling. Long enough for the workload the in-process bus cannot
/// host (a test suite, a benchmark) and short enough that a wedged plugin is a
/// bounded loss.
pub const MAX_WRAPPER_TIMEOUT: Duration = Duration::from_millis(MAX_HOOK_TIMEOUT_MS);

/// Whether the socket withholds a declared environment name from a plugin.
///
/// The single implementation of that judgement, exported so a host's
/// consent-time report and the spawn's actual behaviour cannot disagree — a
/// prompt that promises a variable the spawn then refuses is a prompt
/// describing a different program than the one that runs. `stella plugin
/// install` prints from this predicate; [`SubprocessWrapper::declare`] enforces
/// with it.
///
/// **Credentials only.** An ambient-authority name (`SSH_AUTH_SOCK`,
/// `GIT_SSH_COMMAND`) is deliberately admitted: it is disclosed in the consent
/// text, a plugin that drives git over a deploy key genuinely needs one, and
/// unlike a credential it cannot spend the user's model budget. Refusing it
/// would break a legitimate plugin to prevent nothing the install consent did
/// not already show.
///
/// The judgement itself is `stella-tools`', including the names trusted
/// settings registered at startup. That registry is process-global and
/// monotonic, which is not a `no_ambient_reads` breach in either letter or
/// spirit: it is read, never written, from here, and it can only ever move a
/// name from admitted to refused — the direction that cannot widen what a
/// plugin receives.
#[must_use]
pub fn refuses_env_name(name: &str) -> bool {
    is_sensitive_env_name(name.as_ref())
}

/// A wrapper that runs as a child process and speaks the wire contract.
///
/// Its [`Debug`] is hand-written and prints environment **names** only; see
/// the implementation below for why deriving it is a leak.
#[derive(Clone)]
pub struct SubprocessWrapper {
    program: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    timeout: Duration,
    gate: Option<Arc<HostCallGate>>,
}

/// A declared wrapper, together with what the socket took away from it.
///
/// [`SubprocessWrapper::declare`] answers with this rather than a bare wrapper so
/// that a refusal has to be *read*: a caller reaching for the transport names
/// the field beside it on the way past. A silent filter is its own defect —
/// the plugin author's fix is to stop asking for the credential and ask the
/// host for a `[roles]` tier instead, and they can only take it if the
/// refusal reaches them.
#[derive(Debug)]
#[must_use = "the refused names must be surfaced: a plugin that silently does not get what its \
              manifest asked for is a bug its author cannot diagnose"]
pub struct AdmittedWrapper {
    /// The transport, carrying the admitted pairs and no others.
    pub wrapper: SubprocessWrapper,
    /// The declared names withheld as model credentials, in the order they
    /// were given. Empty in the normal case.
    pub refused: Vec<String>,
}

/// Names, never values.
///
/// Deriving [`Debug`] over the resolved pairs puts `GITHUB_TOKEN`'s value in
/// every `tracing::debug!(?wrapper)`, every `dbg!`, and every assertion
/// failure message — a credential leak whose blast radius is "wherever the
/// host writes logs". The route type this is built from holds only an
/// `env_allowlist: Vec<String>` for the same reason
/// (`stella-cli`'s `plugin_cmd::roster::PluginHookRoute`); the transport is
/// the first place the values exist, so it is the first place they can escape.
impl std::fmt::Debug for SubprocessWrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubprocessWrapper")
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

/// The writer task's state for one conversation, bundled so [`SubprocessWrapper::converse`]
/// takes it as one parameter rather than two.
///
/// The two fields move together for a reason, not just to satisfy
/// `clippy::too_many_arguments`: every place `converse` reads or clears
/// `frames` is also a place that may need to reach `writer` — a call
/// arriving after a prior send already failed joins `writer` to recover the
/// fault that killed it (`WrapperError::AnswerChannelFailed`) rather than
/// reporting the unrelated "no gate" error. Splitting them back into two
/// parameters would not remove that coupling, only hide it.
struct WriterState<'a> {
    /// The channel a host call is answered through. `None` from the start
    /// when no conversation was ever offered (no gate, or the manifest
    /// declares no `[loop] calls`); moves from `Some` to `None` when a send
    /// finds the writer already gone.
    frames: Option<mpsc::Sender<Vec<u8>>>,
    /// The writer task, taken (and joined) only when a call arrives with
    /// `frames` already `None` and the conversation was genuinely open — the
    /// one case where the real transport fault has to be recovered rather
    /// than discarded.
    writer: &'a mut Option<tokio::task::JoinHandle<std::io::Result<()>>>,
}

impl SubprocessWrapper {
    /// Declare a wrapper process.
    ///
    /// `argv` is a program and its arguments, never a shell string. `env` is
    /// the **entire** environment the child will see; the transport clears
    /// whatever this process was carrying first. `timeout` is clamped to
    /// [`MAX_WRAPPER_TIMEOUT`] — a plugin cannot buy itself an unbounded call
    /// any more than it can buy an unbounded loop.
    ///
    /// Every pair whose name [`refuses_env_name`] grades as a model credential
    /// is dropped here and named in [`AdmittedWrapper::refused`]. This is the
    /// boundary every host crosses, which is the whole point: the CLI's
    /// install-time correction is a *report* of this decision, not the place
    /// it is made (#3512).
    ///
    /// # Errors
    ///
    /// [`WrapperError::EmptyArgv`] when `argv` names no program, and
    /// [`WrapperError::ZeroTimeout`] for a budget that would kill the child
    /// before it ran — the `[oracle] command.timeout_secs` rule, which a host
    /// building this from a manifest has already had enforced at load.
    ///
    /// A refused credential is deliberately **not** an error: the manifest is
    /// still loadable and the plugin still runs, exactly as the install
    /// consent said it would ("it will not get it", never "it will not run").
    ///
    /// Named `declare` rather than `new` because it does not return `Self` —
    /// the refusal report comes back beside the transport, and a `new` that
    /// hands back something else is the shape `clippy::new_ret_no_self` exists
    /// to catch. Renaming also makes the signature change visible at every
    /// call site, which is the point of moving the refusal here.
    pub fn declare(
        argv: Vec<String>,
        env: Vec<(String, String)>,
        timeout: Duration,
    ) -> Result<AdmittedWrapper, WrapperError> {
        let Some((program, args)) = argv.split_first() else {
            return Err(WrapperError::EmptyArgv);
        };
        let timeout = timeout.min(MAX_WRAPPER_TIMEOUT);
        if timeout.is_zero() {
            return Err(WrapperError::ZeroTimeout);
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
        Ok(AdmittedWrapper {
            wrapper: Self {
                program: program.clone(),
                args: args.to_vec(),
                env: admitted,
                timeout,
                gate: None,
            },
            refused,
        })
    }

    /// Serve this plugin's declared host calls from `gate`
    /// (`doc:wrapper-socket` §6b).
    ///
    /// Without one the transport is the strictly one-directional socket it was:
    /// the request goes out, stdin closes, and the plugin answers. With one, the
    /// plugin may ask — bounded by what its manifest declared, by the gate's
    /// clamped per-point allowance, and by the same point timeout, which covers
    /// the **whole** conversation so a plugin cannot buy time by talking.
    ///
    /// The gate is `Arc` because a host holds it too: the refusals it recorded
    /// are what the host reports, and a report only this transport could read
    /// would be a silence.
    #[must_use]
    pub fn serving(mut self, gate: Arc<HostCallGate>) -> Self {
        self.gate = Some(gate);
        self
    }

    /// The program this wrapper starts — what an error message names.
    #[must_use]
    pub fn program(&self) -> &str {
        &self.program
    }

    /// The effective budget after the clamp.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// One conversation: the request out, every host call answered, and the
    /// point response back (`doc:wrapper-socket` §6b).
    ///
    /// **One timeout covers the whole of it.** That is contract, not an
    /// implementation choice: a conversation can hang where a single exchange
    /// could not, and a per-message budget would let a plugin buy unbounded time
    /// by talking. A plugin that spends its point arguing is killed at the same
    /// deadline as one that spends it thinking.
    ///
    /// The write and the read are concurrent on purpose, and now for two
    /// reasons. Writing the whole request before reading a byte deadlocks the
    /// moment either side fills a pipe buffer — a wrapper that streams progress
    /// to stdout while the host is still writing a large `changed_files` list is
    /// not exotic, it is Tuesday. The channel adds the mirror image: a host
    /// writing a recall answer while the plugin writes its next message would
    /// deadlock the same way. So stdin belongs to one task that does nothing
    /// else, and this loop only ever reads.
    async fn exchange(&self, request: WrapperRequest) -> Result<WrapperResponse, WrapperError> {
        let asked = request.point();
        let body = serde_json::to_vec(&request).map_err(WrapperError::Encode)?;

        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A wrapper that outlives its budget is killed; without this, a
            // child that ignores the drop keeps running after the turn it was
            // gathering evidence for has been reported. It reaches the direct
            // child and nothing else, which is all there is on non-unix.
            .kill_on_drop(true);
        // The rest of the tree. `after_turn` is where a plugin runs a test or a
        // benchmark, so it is the point most likely to background something,
        // and a backgrounded grandchild is exactly what `kill_on_drop` cannot
        // reach — it outlives the turn it was gathering evidence for.
        #[cfg(unix)]
        stella_tools::exec::detach_into_own_process_group(&mut command);

        let mut child = command.spawn().map_err(|source| WrapperError::Spawn {
            program: self.program.clone(),
            source,
        })?;
        // The child leads the group, so its pid *is* the group id. Taken
        // before anything borrows the child, and armed immediately: between
        // here and the disarm below, every path out — a returned error, a
        // dropped future, a panic — reaps the whole group.
        #[cfg(unix)]
        let mut guard =
            stella_tools::exec::GroupKillGuard::arm(child.id().unwrap_or(0).cast_signed());
        let (Some(stdin), Some(mut stdout), Some(mut stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(WrapperError::Transport {
                program: self.program.clone(),
                source: std::io::Error::other(
                    "the child was started without a full set of stdio pipes",
                ),
            });
        };

        // Stdin belongs to one task and nothing else touches it. The alternative
        // — writing inline from the read loop — stops reading for the duration
        // of a write, which is the deadlock the channel makes reachable: the
        // host writes a recall answer the plugin is not reading because it is
        // writing something the host is not reading.
        let (frames, outbox) = mpsc::channel::<Vec<u8>>(1);
        // `Option`, not a bare handle: `converse` takes it the moment it needs
        // the writer's actual outcome — a host call arriving after a prior
        // send already failed — so the fault that killed it is joined and
        // reported instead of discarded (`WrapperError::AnswerChannelFailed`).
        // Left `Some` here, it is this function's to abort or join below.
        let mut writer = Some(tokio::spawn(write_frames(stdin, outbox)));
        // A send failing means the writer is gone, which a join diagnoses with
        // the actual I/O error; there is nothing better to say here than
        // "carry on and read what the child has to say".
        let _ = frames.send(body).await;

        // Stdin closes now unless the plugin can actually ask for something.
        // A plugin that declared no calls reads its request with `cat` or
        // `sys.stdin.read()` — both wait for EOF — so holding the pipe open for
        // a conversation it can never have would hang it until the deadline.
        let conversation = self.gate.as_ref().filter(|gate| gate.offers_calls());
        let channel = conversation.map(|gate| gate.open());
        let frames = channel.is_some().then_some(frames);
        let writer_state = WriterState {
            frames,
            writer: &mut writer,
        };

        let mut stderr_seen = Vec::new();
        let mut read = 0usize;
        let settled = tokio::time::timeout(self.timeout, async {
            let response = self
                .converse(
                    &mut stdout,
                    &mut stderr,
                    writer_state,
                    channel.as_ref(),
                    &mut stderr_seen,
                    &mut read,
                )
                .await?;
            // `converse` has dropped the sender, so the writer has shut stdin
            // down and the child has its EOF. Waiting for the exit *after* the
            // answer is what keeps a plugin that answers and then dies loudly
            // an `Exit` rather than a success. Trailing stdout is drained too
            // — see `settle`'s own doc for why that half is not optional.
            let status = self
                .settle(
                    &mut child,
                    &mut stdout,
                    &mut stderr,
                    &mut stderr_seen,
                    &mut read,
                )
                .await?;
            Ok::<_, WrapperError>((response, status))
        })
        .await;

        let (response, status) = match settled {
            Ok(Ok(settled)) => settled,
            Ok(Err(error)) => {
                // The conversation failed with the child's state unknown — it
                // may still be writing past the ceiling, or wedged. Kill the
                // group before answering rather than leaving the drop to reach
                // the direct child alone.
                #[cfg(unix)]
                guard.kill_now();
                // `converse` already joined the writer when it needed to
                // report its own failure (`AnswerChannelFailed`) — nothing to
                // abort at that point, the task is gone. Any other error path
                // leaves it running, so it is aborted here as before.
                if let Some(writer) = writer.take() {
                    writer.abort();
                }
                return Err(error);
            }
            Err(_) => {
                #[cfg(unix)]
                guard.kill_now();
                if let Some(writer) = writer.take() {
                    writer.abort();
                }
                return Err(WrapperError::Timeout {
                    program: self.program.clone(),
                    timeout: self.timeout,
                });
            }
        };
        // The child has answered and been reaped. Anything it detached on
        // purpose is not this transport's to kill.
        #[cfg(unix)]
        guard.disarm();

        // A child that answered without reading its request is not an error:
        // a fixed-answer plugin closing stdin early is legitimate, and the
        // broken pipe it causes says nothing about the answer. Any other write
        // failure did lose the request, so it is reported. A join failure — the
        // writer task panicked or was cancelled — is the same class of loss.
        //
        // `writer` is always `Some` on this path: `converse` only ever takes
        // it on its way to returning an error, and this branch is reached
        // only when `converse` returned `Ok`.
        if let Some(writer) = writer {
            match writer.await {
                Ok(Err(source)) if source.kind() != std::io::ErrorKind::BrokenPipe => {
                    return Err(WrapperError::Transport {
                        program: self.program.clone(),
                        source,
                    });
                }
                Err(join) => {
                    return Err(WrapperError::Transport {
                        program: self.program.clone(),
                        source: std::io::Error::other(join),
                    });
                }
                Ok(_) => {}
            }
        }

        if !status.success() {
            return Err(WrapperError::Exit {
                program: self.program.clone(),
                status: status
                    .code()
                    .map_or_else(|| "a signal".to_string(), |code| format!("code {code}")),
                stderr: bounded(&stderr_seen, OUTPUT_EXCERPT_CHARS),
            });
        }

        let Some(response) = response else {
            return Err(WrapperError::NoResponse {
                program: self.program.clone(),
                stderr: bounded(&stderr_seen, OUTPUT_EXCERPT_CHARS),
            });
        };
        if response.protocol_version() > PROTOCOL_VERSION {
            return Err(WrapperError::ProtocolVersion {
                program: self.program.clone(),
                declared: response.protocol_version(),
                supported: PROTOCOL_VERSION,
            });
        }
        if response.point() != asked {
            return Err(WrapperError::PointMismatch {
                program: self.program.clone(),
                expected: asked,
                actual: response.point(),
            });
        }
        Ok(response)
    }

    /// Read the plugin's messages until the one that ends the point, answering
    /// every host call on the way.
    ///
    /// `Ok(None)` is stdout reaching EOF with no point response in it — the
    /// child ended without answering, which the caller reports as an
    /// [`WrapperError::Exit`] or a [`WrapperError::NoResponse`] once it knows
    /// how the child died.
    ///
    /// # Framing
    ///
    /// **A message ends where its JSON value ends**, decided by the parser
    /// rather than by a newline. `serde_json`'s streaming reader is what makes
    /// that free: it either yields a value and says how many bytes it consumed,
    /// or reports an incomplete document, which is exactly the "keep reading"
    /// signal a framing needs. Two things fall out that a line rule would have
    /// cost: a plugin may pretty-print its answer across lines the way its
    /// language's JSON encoder does by default, and several messages arriving in
    /// one read are all handled — a plugin that pipelines two calls does not
    /// have to flush between them.
    async fn converse(
        &self,
        stdout: &mut tokio::process::ChildStdout,
        stderr: &mut tokio::process::ChildStderr,
        mut writer_state: WriterState<'_>,
        channel: Option<&super::host_call::PointChannel<'_>>,
        stderr_seen: &mut Vec<u8>,
        read: &mut usize,
    ) -> Result<Option<WrapperResponse>, WrapperError> {
        // Whether a conversation was ever open, decided once and fixed for the
        // whole call: `writer_state.frames` only ever moves from `Some` to
        // `None` below (a send failure), never the reverse, so this is
        // exactly what tells a later `None` apart from the manifest/gate case
        // that starts `None` and stays there. See the two arms below where a
        // call finds no sender.
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
            let filled = result.map_err(|source| WrapperError::Transport {
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
                return Err(WrapperError::OutputCap {
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
                    // The point response terminates the conversation. Dropping
                    // the sender here is what shuts stdin down, so the plugin
                    // gets its EOF and can exit.
                    PluginMessage::Response(response) => return Ok(Some(response)),
                    PluginMessage::Call(call) => {
                        let asked = call.call();
                        let Some(sender) = &writer_state.frames else {
                            // Two different shapes read as "no sender", and
                            // conflating them was the defect: reporting both
                            // as a manifest/gate problem sends whoever is
                            // debugging a broken pipe to edit `[loop] calls`,
                            // which does nothing.
                            if conversation_was_open {
                                // The channel was open — the manifest declares
                                // this call and a gate was attached, proven by
                                // the fact that something was already sent
                                // through it. It failed transport-side after
                                // that, most likely because the child closed
                                // its stdin without reading a prior answer
                                // (the framing this socket documents blesses a
                                // plugin pipelining calls ahead of reading
                                // them). Join the writer — it has already
                                // exited, or this call could not have found
                                // `frames` empty — to recover *why*, rather
                                // than aborting it unread the way the caller
                                // once did.
                                let source = match writer_state.writer.take() {
                                    Some(handle) => match handle.await {
                                        Ok(Err(source)) => source,
                                        Ok(Ok(())) => std::io::Error::other(
                                            "the writer task exited normally, but a send to it \
                                             had already failed",
                                        ),
                                        Err(join) => std::io::Error::other(join),
                                    },
                                    // Unreachable in practice: nothing else
                                    // takes `writer` before this arm can run.
                                    // Named rather than unwrapped, per
                                    // invariant 5.
                                    None => std::io::Error::other(
                                        "the writer task's outcome was already taken",
                                    ),
                                };
                                return Err(WrapperError::AnswerChannelFailed {
                                    program: self.program.clone(),
                                    call: asked,
                                    source,
                                });
                            }
                            // Stdin is already closed because the manifest
                            // declared no callable capability, so there is no
                            // way to tell the plugin it may not have this. A
                            // hang until the deadline would be the alternative;
                            // this at least names the manifest bug.
                            return Err(WrapperError::UnannouncedCall {
                                program: self.program.clone(),
                                call: asked,
                            });
                        };
                        let outcome = match channel {
                            Some(channel) => channel.call(call.args).await,
                            // Unreachable: `frames` is `Some` only when a
                            // channel was opened. Written as a value rather
                            // than an `expect`, because invariant 5 makes no
                            // exception for "I checked earlier".
                            None => super::host_call::NoHostCalls.call(call.args).await,
                        };
                        let answer = serde_json::to_vec(&HostCallResponse {
                            result: call.id,
                            outcome,
                        })
                        .map_err(WrapperError::Encode)?;
                        if sender.send(answer).await.is_err() {
                            // The writer is gone, so no further answer can
                            // reach the plugin. Stop pretending otherwise. If
                            // the point response is what arrives next anyway,
                            // `exchange`'s own join on `writer` reports why
                            // once this function returns; if instead another
                            // call arrives while `frames` is `None`, the
                            // branch above joins it here and reports
                            // `AnswerChannelFailed` directly, since
                            // `conversation_was_open` proves that call is not
                            // the manifest/gate problem `UnannouncedCall`
                            // means.
                            writer_state.frames = None;
                        }
                    }
                }
            }
        }
        // EOF with bytes still buffered: one last parse, so a plugin that
        // answered without a trailing newline and exited is read exactly as it
        // always was.
        self.take_message(&mut framer)?.map_or(Ok(None), |message| {
            match message {
                PluginMessage::Response(response) => Ok(Some(response)),
                // A call as the very last thing the plugin said: it asked and
                // then closed its own stdout, so no answer could be read even
                // if one were written.
                PluginMessage::Call(call) => Err(WrapperError::UnansweredCall {
                    program: self.program.clone(),
                    call: call.call(),
                }),
            }
        })
    }

    /// Take the next complete message the framer has assembled, if there is one.
    fn take_message(&self, framer: &mut Framer) -> Result<Option<PluginMessage>, WrapperError> {
        let Some(message) = framer.take().map_err(|detail| WrapperError::Decode {
            program: self.program.clone(),
            answer: bounded(framer.buffered(), OUTPUT_EXCERPT_CHARS),
            source: serde::de::Error::custom(detail),
        })?
        else {
            return Ok(None);
        };
        serde_json::from_slice(&message)
            .map(Some)
            .map_err(|source| WrapperError::Decode {
                program: self.program.clone(),
                answer: bounded(&message, OUTPUT_EXCERPT_CHARS),
                source,
            })
    }

    /// Drain what the child still has to say on **both** streams and wait for
    /// it to exit.
    ///
    /// `converse` stops reading stdout the instant it finds the point
    /// response — there is no more of the wire contract left to parse — but
    /// the pipe behind that file descriptor does not know the conversation is
    /// over, and neither does a child still writing to it. A plugin that
    /// prints trailing output after answering (an unflushed logger that also
    /// targets stdout, a debug print, anything past one OS pipe buffer, ~64
    /// KiB on Linux) fills that pipe and blocks in `write(2)`, and a child
    /// blocked in `write(2)` never reaches `exit(2)` — so `child.wait()` below
    /// would hang until the outer timeout killed the group, turning a plugin
    /// that had already answered correctly into a `Timeout`. Stdout is
    /// therefore read to EOF here exactly as stderr already was, for the
    /// identical reason stated on that half below: waiting for a process that
    /// cannot finish writing is a deadlock with a helpful-looking cause. The
    /// bytes themselves are discarded — the response was already parsed and
    /// returned — only the ceiling still applies, over both streams together,
    /// the same accounting `converse` uses.
    async fn settle(
        &self,
        child: &mut tokio::process::Child,
        stdout: &mut tokio::process::ChildStdout,
        stderr: &mut tokio::process::ChildStderr,
        stderr_seen: &mut Vec<u8>,
        read: &mut usize,
    ) -> Result<std::process::ExitStatus, WrapperError> {
        let mut chunk = vec![0u8; READ_CHUNK];
        let mut errchunk = vec![0u8; READ_CHUNK];
        let mut stdout_open = true;
        let mut stderr_open = true;
        while stdout_open || stderr_open {
            let (result, is_stdout) = tokio::select! {
                result = stdout.read(&mut chunk), if stdout_open => (result, true),
                result = stderr.read(&mut errchunk), if stderr_open => (result, false),
            };
            let filled = result.map_err(|source| WrapperError::Transport {
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
                return Err(WrapperError::OutputCap {
                    program: self.program.clone(),
                    stream: if is_stdout { "stdout" } else { "stderr" },
                    cap: MAX_CAPTURE_BYTES,
                });
            }
            if !is_stdout {
                stderr_seen.extend_from_slice(&errchunk[..filled]);
            }
            // Trailing stdout carries nothing this transport reads — the point
            // response already ended the conversation — so it is drained and
            // dropped, never buffered.
        }
        child
            .wait()
            .await
            .map_err(|source| WrapperError::Transport {
                program: self.program.clone(),
                source,
            })
    }
}

#[async_trait]
impl TurnWrapper for SubprocessWrapper {
    async fn before_turn(
        &self,
        request: BeforeTurnRequest,
    ) -> Result<BeforeTurnResponse, WrapperError> {
        match self.exchange(WrapperRequest::BeforeTurn(request)).await? {
            WrapperResponse::BeforeTurn(response) => Ok(response),
            // Unreachable: `exchange` already refused a mismatched point, and
            // it refused it with the error that names both halves. Repeating
            // the check here rather than unwrapping keeps that fact enforced
            // by the compiler if `exchange` ever loosens.
            WrapperResponse::AfterTurn(response) => Err(WrapperError::PointMismatch {
                program: self.program.clone(),
                expected: stella_plugin::WrapperPoint::BeforeTurn,
                actual: WrapperResponse::AfterTurn(response).point(),
            }),
        }
    }

    async fn after_turn(
        &self,
        request: AfterTurnRequest,
    ) -> Result<AfterTurnResponse, WrapperError> {
        match self.exchange(WrapperRequest::AfterTurn(request)).await? {
            WrapperResponse::AfterTurn(response) => Ok(response),
            WrapperResponse::BeforeTurn(response) => Err(WrapperError::PointMismatch {
                program: self.program.clone(),
                expected: stella_plugin::WrapperPoint::AfterTurn,
                actual: WrapperResponse::BeforeTurn(response).point(),
            }),
        }
    }
}
