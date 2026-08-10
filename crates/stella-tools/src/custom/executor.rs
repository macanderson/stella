//! [`ToolExecutor`] forwarding for the custom-tool decorator.

use async_trait::async_trait;

use super::*;

#[async_trait]
impl ToolExecutor for CustomToolSet<'_> {
    fn schemas(&self) -> Vec<ToolSchema> {
        let mut schemas = self.inner.get().schemas();
        schemas.extend(self.tools.iter().map(CustomTool::schema));
        schemas
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        if let Some(tool) = self.tools.iter().find(|t| t.name == name) {
            return run_custom(tool, input, &self.workspace_root).await;
        }
        self.inner.get().execute(name, input).await
    }

    fn drain_verification_requests(&self) -> Vec<Value> {
        self.inner.get().drain_verification_requests()
    }

    async fn replay_verification_request(
        &self,
        input: &Value,
    ) -> Option<stella_core::VerificationOracleResult> {
        self.inner.get().replay_verification_request(input).await
    }

    /// Forwarded: this is a decorator, and a decorator that let the default
    /// `0.0` stand would silently drop sub-agent spend out of the parent's
    /// budget (see the port's contract).
    fn drain_sub_agent_spend_usd(&self) -> f64 {
        self.inner.get().drain_sub_agent_spend_usd()
    }

    /// Forwarded for the same reason as the spend drain above: a swallowed
    /// wait request silently turns parked waits (#1471) back into
    /// model-step polling.
    fn drain_wait_request(&self) -> Option<stella_core::WaitRequest> {
        self.inner.get().drain_wait_request()
    }

    /// Forwarded: letting the empty default stand would silently serialize the
    /// inner executor's sibling spawns (see the port's contract). Custom
    /// script tools spawn workspace subprocesses and make no such claim.
    fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
        self.inner.get().parallel_safe_names()
    }
}
