//! The tool arm of the steering plane — the layer that spends a session's
//! tool budget.
//!
//! [`stella_core::steering::tools`] makes the choice: what a schema costs,
//! how the candidates rank, and which of them a budget holds. This is the
//! adapter behind it. It copies the shape of
//! [`crate::tool_policy::PolicyToolSet`]: a `ToolExecutor` wrapper that
//! trims what the model is shown.
//!
//! # It trims the list and nothing else
//!
//! `schemas()` is the only method this layer narrows. `execute`,
//! `contracts` and `parallel_safe_names` pass straight through. The
//! difference from the policy layer below is the point. A switched-off tool
//! is one the operator took away. A tool this layer leaves out is one the
//! session could not afford to describe. It still runs when it is called. It
//! keeps its reviewed contract at the gate above. It keeps its concurrency
//! claim. A tight budget is a cost measure. It can never wedge a turn.
//!
//! # The list holds still for the session
//!
//! It is worked out once, in [`LeanToolSet::new`], over the schemas the
//! layer below has. The list sits ahead of the system prompt in every
//! cache, so a list that moved between two turns of one session would make
//! the model pay for the whole chat again.

use async_trait::async_trait;
use serde_json::Value;
use stella_core::ports::ToolExecutor;
use stella_core::steering::SteeringSet;
use stella_core::steering::tools::{ToolBudget, advertise};
use stella_protocol::tool::{ToolOutput, ToolSchema};

/// A session's advertised tool surface, budgeted by the steering plane.
pub(crate) struct LeanToolSet<'a> {
    inner: Box<dyn ToolExecutor + 'a>,
    advertised: Vec<ToolSchema>,
    steering: SteeringSet,
}

impl<'a> LeanToolSet<'a> {
    /// Budget `inner`'s schemas once. The record is kept for the drop
    /// report.
    pub(crate) fn new(inner: Box<dyn ToolExecutor + 'a>, budget: ToolBudget) -> Self {
        let decided = advertise(inner.schemas(), budget);
        Self {
            inner,
            advertised: decided.schemas,
            steering: decided.steering,
        }
    }

    /// Name every cut tool on stderr. It goes through
    /// `memory::report_steering_drops`, the one writer every other
    /// steering source already uses.
    ///
    /// The recall budget it takes is `0` and is never read. That number
    /// shapes the memory line, and a set built here holds tool drops
    /// alone.
    pub(crate) fn report_drops(&self) {
        use colored::Colorize;
        crate::memory::report_steering_drops(&self.steering, 0, |message| {
            eprintln!("  {} {message}", "!".yellow())
        });
    }

    /// What the plane kept and cut for this session.
    #[cfg(test)]
    pub(crate) fn steering(&self) -> &SteeringSet {
        &self.steering
    }
}

#[async_trait]
impl ToolExecutor for LeanToolSet<'_> {
    /// The budgeted list. This is the one thing this layer narrows.
    fn schemas(&self) -> Vec<ToolSchema> {
        self.advertised.clone()
    }

    /// Passed on whole, unlike [`crate::tool_policy::PolicyToolSet`]'s. A
    /// tool left out of the list still runs. So it still needs its reviewed
    /// contract at the gate above. Trimming here would spoil the very calls
    /// this layer said it would not touch.
    fn contracts(&self) -> Vec<stella_protocol::ToolContract> {
        self.inner.contracts()
    }

    /// Passed on whole. A hidden tool is not a lost tool: one the budget
    /// could not describe runs as normal when it is named.
    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        self.inner.execute(name, input).await
    }

    /// Passed on. A wrapper that let the `0.0` default stand would drop
    /// sub-agent spend out of the parent's budget.
    fn drain_sub_agent_spend_usd(&self) -> f64 {
        self.inner.drain_sub_agent_spend_usd()
    }

    /// Passed on. A dropped wait request turns a parked wait back into
    /// polling by model step.
    fn drain_wait_request(&self) -> Option<stella_core::WaitRequest> {
        self.inner.drain_wait_request()
    }

    /// Passed on whole, for the reason `contracts` is: the claim belongs to
    /// a tool that can still be called.
    fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
        self.inner.parallel_safe_names()
    }

    /// Passed on. Live skill text has to survive a summary behind this view
    /// too.
    fn active_skill_slugs(&self) -> Vec<String> {
        self.inner.active_skill_slugs()
    }

    /// Passed on. Letting the empty default stand would turn off the
    /// end-of-turn service check for every surface built through this.
    fn live_services(&self) -> Vec<stella_core::LiveService> {
        self.inner.live_services()
    }

    /// Passed on. This layer runs no name of its own, so a wrapper above it
    /// that does must still find the base's gate.
    fn dispatch_gate(&self) -> Option<&dyn stella_core::ports::DispatchGate> {
        self.inner.dispatch_gate()
    }

    /// Passed on whole. The question is about a name that has already run,
    /// so trimming it could only lose an answer the caller needs.
    fn tool_origin(&self, name: &str) -> Option<stella_core::loop_detect::ToolOrigin> {
        self.inner.tool_origin(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use stella_core::steering::SteeringSource;
    use stella_core::steering::tools::schema_tokens;

    /// A base with `count` tools, half of them from an MCP server. It
    /// records what reached it.
    struct Leaf {
        count: usize,
        reached: std::sync::Mutex<Vec<String>>,
    }

    impl Leaf {
        fn new(count: usize) -> Self {
            Self {
                count,
                reached: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    fn leaf_schema(index: usize) -> ToolSchema {
        let name = if index % 2 == 0 {
            format!("builtin_{index:02}")
        } else {
            format!("mcp__server__tool_{index:02}")
        };
        ToolSchema {
            name,
            description: "x".repeat(400),
            input_schema: json!({"type": "object"}),
            read_only: true,
            speculation_safe: false,
        }
    }

    #[async_trait]
    impl ToolExecutor for Leaf {
        fn schemas(&self) -> Vec<ToolSchema> {
            (0..self.count).map(leaf_schema).collect()
        }
        async fn execute(&self, name: &str, _input: &Value) -> ToolOutput {
            self.reached.lock().unwrap().push(name.to_string());
            ToolOutput::Ok {
                content: format!("ran {name}"),
                data: None,
            }
        }
    }

    fn names(set: &LeanToolSet<'_>) -> Vec<String> {
        set.schemas().into_iter().map(|s| s.name).collect()
    }

    fn advertised_tokens(set: &LeanToolSet<'_>) -> u64 {
        set.schemas().iter().map(schema_tokens).sum()
    }

    /// **The witness.** Forty tools, and a budget that holds a quarter of
    /// them. The schemas sent fit the cap. The rest are left out. The layer
    /// below still has all forty.
    ///
    /// Only this layer narrows the list. Nothing else in the chain touches
    /// `schemas()`. So without it, every schema the stack has reaches the
    /// request.
    #[test]
    fn a_lean_session_advertises_only_what_the_allowance_affords() {
        let leaf = Leaf::new(40);
        let full: u64 = leaf.schemas().iter().map(schema_tokens).sum();
        let budget = ToolBudget {
            max_tokens: full / 4,
            mcp_max_tokens: full,
        };

        let set = LeanToolSet::new(Box::new(leaf), budget);

        assert_eq!(
            set.inner.schemas().len(),
            40,
            "the layer beneath still resolves every tool"
        );
        assert!(
            advertised_tokens(&set) <= budget.max_tokens,
            "the request's schema bytes fit the ceiling: {} against {}",
            advertised_tokens(&set),
            budget.max_tokens
        );
        assert!(
            set.schemas().len() < 40,
            "and the ceiling really did bind: {} advertised",
            set.schemas().len()
        );
        assert!(
            set.steering()
                .dropped
                .iter()
                .all(|drop| drop.source == SteeringSource::Tool),
            "the withheld tools reach the plane's ledger"
        );
    }

    /// A tool left out still runs. Hiding a schema saves prompt tokens. It
    /// is not a gate. So nothing here can wedge a turn that names a tool it
    /// was not shown.
    #[tokio::test]
    async fn a_withheld_tool_still_executes() {
        let leaf = Leaf::new(40);
        let set = LeanToolSet::new(
            Box::new(leaf),
            ToolBudget {
                max_tokens: 1,
                mcp_max_tokens: 1,
            },
        );
        assert!(
            names(&set).is_empty(),
            "the control: this allowance affords nothing"
        );

        assert!(matches!(
            set.execute("builtin_00", &json!({})).await,
            ToolOutput::Ok { .. }
        ));
    }

    /// A budget nothing spills past sends the base's list as it is, in its
    /// own order. This is the control arm for the lever being off.
    #[test]
    fn a_wide_allowance_advertises_the_base_list_unchanged() {
        let leaf = Leaf::new(40);
        let expected = leaf.schemas();
        let set = LeanToolSet::new(
            Box::new(leaf),
            ToolBudget {
                max_tokens: u64::MAX,
                mcp_max_tokens: u64::MAX,
            },
        );

        assert_eq!(set.schemas(), expected);
        assert!(set.steering().dropped.is_empty());
    }

    /// Every tool left out is named on the drop report, by the same writer a
    /// cut record already uses.
    #[test]
    fn every_withheld_tool_is_named_in_the_drop_report() {
        let leaf = Leaf::new(40);
        let full: u64 = leaf.schemas().iter().map(schema_tokens).sum();
        let set = LeanToolSet::new(
            Box::new(leaf),
            ToolBudget {
                max_tokens: full / 4,
                mcp_max_tokens: full,
            },
        );

        let mut lines = Vec::new();
        crate::memory::report_steering_drops(set.steering(), 0, |message| lines.push(message));

        assert_eq!(
            lines.len(),
            set.steering().dropped.len(),
            "one line per withheld tool: {lines:?}"
        );
        for drop in &set.steering().dropped {
            assert!(
                lines.iter().any(|line| line.contains(&drop.handle)),
                "{} is named: {lines:?}",
                drop.handle
            );
        }
    }
}
