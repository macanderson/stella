//! The engine's ports, remoted.
//!
//! Server-side the engine must never run a model call or a tool with ambient
//! authority — the whole containment story of ADR-033 Option B rests on this.
//! So [`RemoteProvider`] and [`RemoteToolExecutor`] implement the engine's
//! `Provider` / `ToolExecutor` ports by emitting a reverse-RPC request frame to
//! the host and awaiting the host's answer. The host runs the effect through
//! its own governance (`kernel.invoke()`, `@oxagen/ai`) and posts the result
//! back, which resolves the one-shot the port is parked on.
//!
//! Both ports are constructed on, and driven by, the session thread's
//! current-thread runtime; the one-shot receivers they await are fired from the
//! server runtime. That cross-runtime wake is the `!Send`-future bridge.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use stella_core::ports::ToolExecutor;
use stella_core::retry::Sleeper;
use stella_protocol::{
    CompletionRequest, CompletionResult, Provider, ProviderError, ToolOutput, ToolSchema,
};
use tokio::sync::{mpsc, oneshot};

use crate::frame::ServerFrame;
use crate::pending::Pending;

/// How long a reverse request waits for the host before the port gives up.
///
/// A reverse request is the host running a model completion or a tool call, so
/// the bound has to clear the slowest legitimate one: an extended-thinking
/// completion, or a tool that shells out to a test suite. Both run in minutes,
/// not seconds. Five minutes sits comfortably above that while still turning
/// "wedged forever" into "fails in minutes" — the whole point, since the parked
/// step also holds an OS thread.
///
/// Per-turn overridable via [`crate::SessionSpec::reverse_request_timeout`] (on
/// the wire: `reverse_request_timeout_ms` on `POST /v1/turns`), because a host
/// with genuinely slower tools should raise it rather than have the engine
/// pretend its tools failed.
pub(crate) const DEFAULT_REVERSE_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// A Tokio-backed [`Sleeper`] for the session runtime's retry backoff. The
/// session runtime is built with the time driver enabled, so `sleep` resolves
/// there.
pub(crate) struct TokioSleeper;

#[async_trait]
impl Sleeper for TokioSleeper {
    async fn sleep(&self, duration_ms: u64) {
        tokio::time::sleep(std::time::Duration::from_millis(duration_ms)).await;
    }
}

/// The `Provider` port as a reverse-RPC to the host. `complete` emits a
/// [`ServerFrame::ProviderRequest`] and blocks the step on the host's answer.
pub(crate) struct RemoteProvider {
    id: String,
    frames: mpsc::UnboundedSender<ServerFrame>,
    pending: Pending,
    counter: AtomicU64,
    timeout: Duration,
}

impl RemoteProvider {
    pub(crate) fn new(
        id: String,
        frames: mpsc::UnboundedSender<ServerFrame>,
        pending: Pending,
        timeout: Duration,
    ) -> Self {
        Self {
            id,
            frames,
            pending,
            counter: AtomicU64::new(0),
            timeout,
        }
    }
}

#[async_trait]
impl Provider for RemoteProvider {
    fn id(&self) -> &str {
        &self.id
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResult, ProviderError> {
        // Refuse to park a cancelled turn. `Cancelled` is not retryable, so this
        // is what unwinds the turn after `POST /v1/turns/{id}/cancel` — including
        // when the engine reaches for the model again after a cancelled tool.
        if self.pending.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let request_id = format!("prov-{}", self.counter.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        // Register before emitting so the entry always exists by the time the
        // host can answer (register → send ordering closes the race).
        self.pending.register_provider(request_id.clone(), tx);
        if self
            .frames
            .send(ServerFrame::ProviderRequest {
                request_id: request_id.clone(),
                request: req,
            })
            .is_err()
        {
            // The host stream is gone; a disconnect mid-flight is a retryable
            // transport condition, classified here at the adapter as upstream.
            self.pending.abandon(&request_id);
            return Err(ProviderError::Transport(
                "serve host disconnected before the model call could be dispatched".to_string(),
            ));
        }
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(result)) => result,
            // Sender dropped. Cancellation is the deliberate case and reports
            // itself as such; otherwise the session is being torn down.
            Ok(Err(_)) if self.pending.is_cancelled() => Err(ProviderError::Cancelled),
            Ok(Err(_)) => Err(ProviderError::Transport(
                "serve host dropped the model call without answering".to_string(),
            )),
            Err(_) => {
                self.pending.abandon(&request_id);
                // Deliberately `Terminal`, not `Transport`: `Transport` is
                // retryable, so a host that is simply not answering would be
                // handed the same unbounded wait again once per retry —
                // multiplying the very window this deadline exists to close.
                Err(ProviderError::Terminal(format!(
                    "serve host did not answer the model call within {:?} \
                     (reverse-request deadline)",
                    self.timeout
                )))
            }
        }
    }
}

/// The `ToolExecutor` port as a reverse-RPC to the host. `schemas` returns the
/// tool set the host advertised at session start (including `read_only` flags,
/// so the engine's partitioned concurrency still applies); `execute` emits a
/// [`ServerFrame::ToolRequest`] and blocks on the host's answer.
pub(crate) struct RemoteToolExecutor {
    schemas: Vec<ToolSchema>,
    frames: mpsc::UnboundedSender<ServerFrame>,
    pending: Pending,
    counter: AtomicU64,
    timeout: Duration,
}

impl RemoteToolExecutor {
    pub(crate) fn new(
        schemas: Vec<ToolSchema>,
        frames: mpsc::UnboundedSender<ServerFrame>,
        pending: Pending,
        timeout: Duration,
    ) -> Self {
        Self {
            schemas,
            frames,
            pending,
            counter: AtomicU64::new(0),
            timeout,
        }
    }
}

#[async_trait]
impl ToolExecutor for RemoteToolExecutor {
    fn schemas(&self) -> Vec<ToolSchema> {
        self.schemas.clone()
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        // Every exit here is a `ToolOutput::Error`: the port contract is that
        // `execute` never returns `Err`, since a tool failure is model-visible
        // data. Cancellation therefore ends the turn one step later, at the
        // provider port, which *can* report `ProviderError::Cancelled`.
        if self.pending.is_cancelled() {
            return ToolOutput::Error {
                message: "turn cancelled before the tool call could be dispatched".to_string(),
            };
        }
        let request_id = format!("tool-{}", self.counter.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.pending.register_tool(request_id.clone(), tx);
        if self
            .frames
            .send(ServerFrame::ToolRequest {
                request_id: request_id.clone(),
                name: name.to_string(),
                input: input.clone(),
            })
            .is_err()
        {
            self.pending.abandon(&request_id);
            return ToolOutput::Error {
                message: "serve host disconnected before the tool call could be dispatched"
                    .to_string(),
            };
        }
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(output)) => output,
            Ok(Err(_)) if self.pending.is_cancelled() => ToolOutput::Error {
                message: "turn cancelled while the tool call was in flight".to_string(),
            },
            Ok(Err(_)) => ToolOutput::Error {
                message: "serve host dropped the tool call without answering".to_string(),
            },
            Err(_) => {
                self.pending.abandon(&request_id);
                ToolOutput::Error {
                    message: format!(
                        "serve host did not answer the `{name}` tool call within {:?} \
                         (reverse-request deadline)",
                        self.timeout
                    ),
                }
            }
        }
    }
}
