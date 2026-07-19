//! The engine's port boundary. `stella-core`
//! never imports a provider SDK, a filesystem call, or a terminal library —
//! it drives through these traits, mirroring the TS engine's `ports.ts`.

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::{ToolOutput, ToolSchema};

/// Executes one tool call. Implemented by `stella-tools::ToolRegistry` (and
/// by test doubles). The engine treats it as a black box that never panics.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Schemas advertised to the model.
    fn schemas(&self) -> Vec<ToolSchema>;

    /// Execute a tool by name. Unknown names return an error `ToolOutput`,
    /// never an Err — tool failures are model-visible data, not engine
    /// failures.
    async fn execute(&self, name: &str, input: &Value) -> ToolOutput;
}

/// A read-only view over another executor: advertises only the schemas
/// marked `read_only` and refuses to execute anything else. This is how a
/// judge gets real evidence-gathering power (read files, grep, check
/// saved explorations) with a structural guarantee it cannot mutate the
/// workspace it is judging — the restriction is enforced at execution
/// time, not just by prompt.
pub struct ReadOnlyTools<'a> {
    inner: &'a dyn ToolExecutor,
}

impl<'a> ReadOnlyTools<'a> {
    pub fn new(inner: &'a dyn ToolExecutor) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl ToolExecutor for ReadOnlyTools<'_> {
    fn schemas(&self) -> Vec<ToolSchema> {
        self.inner
            .schemas()
            .into_iter()
            .filter(|s| s.read_only)
            .collect()
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        let allowed = self
            .inner
            .schemas()
            .iter()
            .any(|s| s.name == name && s.read_only);
        if !allowed {
            return ToolOutput::Error {
                message: format!(
                    "`{name}` is not available here: this context is read-only (verification/\
                     judging) and may only use read-only tools"
                ),
            };
        }
        self.inner.execute(name, input).await
    }
}

/// Time source, injectable for deterministic tests. Only the trait lives
/// here — the production implementation belongs to the binary that wires
/// the engine (the CLI's `runtime` module), so `stella-core` never carries
/// a concrete time source of its own.
pub trait Clock: Send + Sync {
    /// Monotonic milliseconds since an arbitrary epoch.
    fn now_ms(&self) -> u64;
}

/// Boundary pause gate — polled by the engine between model calls, the same
/// safe boundary as budget aborts (L-E6: never mid-tool). `wait_if_paused`
/// returns immediately when the turn may proceed and parks (await) while it
/// is paused; the driver flips the underlying state from supervisor input.
/// A port so `stella-core` stays I/O-free — the CLI implements it over a
/// watch channel.
#[async_trait::async_trait]
pub trait TurnGate: Send + Sync {
    async fn wait_if_paused(&self);
}

/// Step-boundary steering — polled by the engine at the same safe boundary
/// as [`TurnGate`] (L-E6: never mid-tool). Turns "wait, pause, or kill"
/// into "steer": user messages queued while a turn runs are injected as
/// the model's next observation instead of waiting for the turn to end,
/// and a soft stop ends the turn at the boundary KEEPING the work so far —
/// unlike the caller-side hard cancel, which drops the turn future and
/// truncates the whole turn (all paid tokens) out of history. A port so
/// `stella-core` stays I/O-free — the CLI implements it over shared state
/// fed by the deck's input loop.
pub trait TurnSteering: Send + Sync {
    /// Drain the user messages queued since the last boundary, oldest
    /// first. Non-destructive peeks are deliberately not offered: whatever
    /// this returns WILL be injected, so the implementation owns dedup.
    fn drain_steering(&self) -> Vec<String>;
    /// True when the user asked to end the turn at the next boundary. The
    /// engine reads this once per step; the implementation should latch it
    /// (a stop request must not evaporate between steps).
    fn soft_stop_requested(&self) -> bool;
}
