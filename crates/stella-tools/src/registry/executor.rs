//! Production implementation of the engine's [`ToolExecutor`] port.

use async_trait::async_trait;

use super::*;

#[async_trait]
impl ToolExecutor for ToolRegistry {
    fn schemas(&self) -> Vec<ToolSchema> {
        ToolRegistry::schemas(self)
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        ToolRegistry::execute(self, name, input).await
    }

    fn drain_verification_requests(&self) -> Vec<Value> {
        std::mem::take(
            &mut *self
                .verification_requests
                .lock()
                .unwrap_or_else(|p| p.into_inner()),
        )
    }

    async fn replay_verification_request(
        &self,
        input: &Value,
    ) -> Option<stella_core::VerificationOracleResult> {
        Some(
            crate::verify::VerifyDone::with_memo(self.shadow_memo.clone())
                .replay_for_pipeline(input, &self.root)
                .await,
        )
    }

    /// Take what the `task` tool's children cost since the last step
    /// boundary. Destructive by the port's contract: the engine charges
    /// whatever this returns, so reporting twice would bill twice.
    fn drain_sub_agent_spend_usd(&self) -> f64 {
        stella_core::subagent::drain_sub_agent_spend(&self.sub_agent_spend)
    }

    /// Collect the parked-wait request a tool deposited this step, if any
    /// (#1471). Consults the primary map and the late-enabled overlay — the
    /// same two sources every other read path reads. First hit wins: a step
    /// dispatches at most a handful of calls, and `Tool::take_wait_request`
    /// is destructive, so at most one request exists per boundary.
    fn drain_wait_request(&self) -> Option<stella_core::WaitRequest> {
        let from_primary = self
            .tools
            .values()
            .find_map(|tool| tool.take_wait_request());
        from_primary.or_else(|| {
            self.late_tools
                .read()
                .ok()
                .and_then(|late| late.values().find_map(|tool| tool.take_wait_request()))
        })
    }

    /// Aggregate each registered tool's [`Tool::parallel_safe`] claim for the
    /// engine's dispatch grouping. Consults the primary map and the
    /// late-enabled overlay — the same two sources every other read path
    /// (`schemas`, dispatch) reads, so a late-enabled tool's claim cannot be
    /// lost.
    fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
        let mut names: std::collections::HashSet<String> = self
            .tools
            .iter()
            .filter(|(_, tool)| tool.parallel_safe())
            .map(|(name, _)| name.clone())
            .collect();
        // Poison-tolerant like every sibling `late_tools` read path.
        let late = self.late_tools.read().unwrap_or_else(|p| p.into_inner());
        names.extend(
            late.iter()
                .filter(|(_, tool)| tool.parallel_safe())
                .map(|(name, _)| name.clone()),
        );
        names
    }
}
