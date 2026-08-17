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
//! # What this transport does not do, on purpose
//!
//! - **It sets no working directory.** `doc:wrapper-socket` §6 bans cwd from
//!   every signature, and everything the plugin needs arrives in the request —
//!   the candidate worktree as a `CandidateHandle` the host resolves, never a
//!   path the plugin is trusted to have stayed inside. It is also a hard
//!   constraint here rather than a preference: this crate must not read
//!   process-global state at all (`tests/no_ambient_reads.rs`), and a cwd is
//!   the most process-global thing there is.
//! - **It clears the environment and sets exactly what it was given.** A
//!   plugin is third-party code a user installed; inheriting the operator's
//!   environment hands it `ANTHROPIC_API_KEY` and every other credential the
//!   shell was carrying, silently, forever — which would make invariant 3's
//!   "every model call is made by the host" a policy rather than a property.
//!   Default-deny is structural: [`SubprocessWrapper::new`] takes the pairs,
//!   so an empty list is what a caller that thought about nothing gets. The
//!   manifest half of that decision is `[runtime] env`, whose `child_env`
//!   returns exactly the pairs to pass here.
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

/// A wrapper that runs as a child process and speaks the wire contract.
#[derive(Debug, Clone)]
pub struct SubprocessWrapper {
    program: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    timeout: Duration,
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
    /// # Errors
    ///
    /// [`WrapperError::EmptyArgv`] when `argv` names no program, and
    /// [`WrapperError::ZeroTimeout`] for a budget that would kill the child
    /// before it ran — the `[oracle] command.timeout_secs` rule, which a host
    /// building this from a manifest has already had enforced at load.
    pub fn new(
        argv: Vec<String>,
        env: Vec<(String, String)>,
        timeout: Duration,
    ) -> Result<Self, WrapperError> {
        let Some((program, args)) = argv.split_first() else {
            return Err(WrapperError::EmptyArgv);
        };
        let timeout = timeout.min(MAX_WRAPPER_TIMEOUT);
        if timeout.is_zero() {
            return Err(WrapperError::ZeroTimeout);
        }
        Ok(Self {
            program: program.clone(),
            args: args.to_vec(),
            env,
            timeout,
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
            // gathering evidence for has been reported.
            .kill_on_drop(true);

        let mut child = command.spawn().map_err(|source| WrapperError::Spawn {
            program: self.program.clone(),
            source,
        })?;
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
        let (written, waited) =
            match tokio::time::timeout(self.timeout, futures_join(write, child.wait_with_output()))
                .await
            {
                Ok(pair) => pair,
                Err(_) => {
                    return Err(WrapperError::Timeout {
                        program: self.program.clone(),
                        timeout: self.timeout,
                    });
                }
            };

        let output = waited.map_err(|source| WrapperError::Transport {
            program: self.program.clone(),
            source,
        })?;
        // A child that answered without reading its request is not an error:
        // a fixed-answer plugin closing stdin early is legitimate, and the
        // broken pipe it causes says nothing about the answer. Any other write
        // failure did lose the request, so it is reported.
        if let Err(source) = written
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

/// Await both futures to completion.
///
/// `tokio::join!` is a macro over its own arguments, which cannot be handed a
/// pair built elsewhere; this is the same thing as a function so the timeout
/// above wraps *one* future covering both halves. Written out rather than
/// pulled from `futures` because that would be a new dependency for eight
/// lines (AGENTS.md § no new dependencies casually).
async fn futures_join<A, B>(a: A, b: B) -> (A::Output, B::Output)
where
    A: Future,
    B: Future,
{
    tokio::join!(a, b)
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
