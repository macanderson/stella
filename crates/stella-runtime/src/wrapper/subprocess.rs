//! The subprocess transport — one JSON request on stdin, one JSON response on
//! stdout, in whatever language the plugin is written in.
//!
//! This is the path that makes Python and TypeScript first-class rather than
//! bolted on (`doc:pipeline-as-plugins` §5), so it is the path the tests
//! exercise: `tests/wrapper_socket.rs` runs a wrapper written in `sh` — no
//! Rust, no SDK, a JSON parser it does not even need — through a whole turn.
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
//!   output *is* the product), this transport refuses: the answer is one JSON
//!   document, a truncated copy of it cannot be decoded, and every byte read
//!   after the ceiling is spent on a call already lost
//!   ([`stella_tools::exec::Overflow::Refuse`], [`WrapperError::OutputCap`]).
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
use std::time::Duration;

use async_trait::async_trait;
use stella_core::hooks::{DEFAULT_HOOK_TIMEOUT_MS, MAX_HOOK_TIMEOUT_MS};
use stella_plugin::{
    AfterTurnRequest, AfterTurnResponse, BeforeTurnRequest, BeforeTurnResponse, PROTOCOL_VERSION,
    WrapperRequest, WrapperResponse,
};
use stella_tools::exec::{Capture, MAX_CAPTURE_BYTES, Overflow, capture};
use stella_tools::subprocess_env::is_sensitive_env_name;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::error::bounded;
use super::{TurnWrapper, WrapperError};

/// The default budget for one wrapper call — 60s, the hook plane's default.
pub const DEFAULT_WRAPPER_TIMEOUT: Duration = Duration::from_millis(DEFAULT_HOOK_TIMEOUT_MS);

/// The ceiling a configured budget is clamped to — 10 minutes, the hook
/// plane's ceiling. Long enough for the workload the in-process bus cannot
/// host (a test suite, a benchmark) and short enough that a wedged plugin is a
/// bounded loss.
pub const MAX_WRAPPER_TIMEOUT: Duration = Duration::from_millis(MAX_HOOK_TIMEOUT_MS);

/// How much of a child's stderr or unreadable stdout reaches an error message.
const OUTPUT_EXCERPT_CHARS: usize = 2_000;

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
            },
            refused,
        })
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

    /// One request out, one response back.
    ///
    /// The write and the read are concurrent on purpose. Writing the whole
    /// request before reading a byte deadlocks the moment either side fills a
    /// pipe buffer — a wrapper that streams progress to stdout while the host
    /// is still writing a large `changed_files` list is not exotic, it is
    /// Tuesday.
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
        let Some(mut stdin) = child.stdin.take() else {
            return Err(WrapperError::Transport {
                program: self.program.clone(),
                source: std::io::Error::other("the child was started without a stdin pipe"),
            });
        };

        let write = async move {
            stdin.write_all(&body).await?;
            // The newline is not part of the framing — one request is one
            // process — but a line-oriented reader (`read()` in a shell,
            // `sys.stdin.readline()` in Python) is the first thing a plugin
            // author reaches for, and denying them it buys nothing.
            stdin.write_all(b"\n").await?;
            stdin.shutdown().await
        };
        let mut write = std::pin::pin!(write);
        let mut written: Option<std::io::Result<()>> = None;
        let mut capture = std::pin::pin!(capture(&mut child, MAX_CAPTURE_BYTES, Overflow::Refuse));
        // The capture ends the exchange, and the write is abandoned if it has
        // not finished by then. Awaiting both — which is what this was — makes
        // the refusal below wait on a write the plugin has by definition
        // stopped reading, so a request past the OS pipe buffer (~64 KiB: one
        // ordinary `changed_files` list) would block there until the budget
        // ran out and turn a prompt refusal back into a late `Timeout`.
        let waited = tokio::time::timeout(self.timeout, async {
            loop {
                tokio::select! {
                    // Biased so a write that is already finished is collected
                    // rather than discarded by a capture ready in the same
                    // poll: the error it may carry is worth more than one
                    // iteration of the loop.
                    biased;
                    result = &mut write, if written.is_none() => written = Some(result),
                    result = &mut capture => break result,
                }
            }
        })
        .await;

        let waited = match waited {
            Ok(waited) => waited,
            Err(_) => {
                // The child is alive and over budget: kill the group before
                // answering, rather than leaving the drop to reach the direct
                // child alone.
                #[cfg(unix)]
                guard.kill_now();
                return Err(WrapperError::Timeout {
                    program: self.program.clone(),
                    timeout: self.timeout,
                });
            }
        };
        // A wait failure deliberately does not disarm: the child's state is
        // unknown, so dropping the still-armed guard kills the group.
        let output = match waited.map_err(|source| WrapperError::Transport {
            program: self.program.clone(),
            source,
        })? {
            Capture::Exited(output) => output,
            // The read stopped at the ceiling and the child is still writing.
            // Killing the group here is the half that makes the cap a memory
            // bound rather than a pause: an unrefused writer would otherwise
            // sit in the pipe until the budget expired.
            Capture::Refused { stream } => {
                #[cfg(unix)]
                guard.kill_now();
                return Err(WrapperError::OutputCap {
                    program: self.program.clone(),
                    stream,
                    cap: MAX_CAPTURE_BYTES,
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
        // failure did lose the request, so it is reported. `None` — the answer
        // landed before the write finished — is the same case one step
        // earlier, and is read the same way.
        if let Some(Err(source)) = written
            && source.kind() != std::io::ErrorKind::BrokenPipe
        {
            return Err(WrapperError::Transport {
                program: self.program.clone(),
                source,
            });
        }

        if !output.status.success() {
            return Err(WrapperError::Exit {
                program: self.program.clone(),
                status: output
                    .status
                    .code()
                    .map_or_else(|| "a signal".to_string(), |code| format!("code {code}")),
                stderr: bounded(&output.stderr, OUTPUT_EXCERPT_CHARS),
            });
        }

        let response: WrapperResponse =
            serde_json::from_slice(&output.stdout).map_err(|source| WrapperError::Decode {
                program: self.program.clone(),
                answer: bounded(&output.stdout, OUTPUT_EXCERPT_CHARS),
                source,
            })?;
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
