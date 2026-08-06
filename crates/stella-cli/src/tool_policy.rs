//! The one place a tool an operator switched off is actually withheld.
//!
//! [`stella_tools::policy::ToolPolicy`] decides *what* is off; this decorator
//! is *where* that is enforced. It sits above the MCP and custom layers rather
//! than inside [`stella_tools::registry::ToolRegistry`], because that is the
//! only position that sees the complete session surface — built-ins, every
//! connected MCP server's tools, and whatever the customer registered in
//! `.stella/tools/*.toml`. Enforcing inside the registry would cover only the
//! first of the three, which is precisely the gap the old `RegistryOptions`
//! booleans had.
//!
//! # Both sides, deliberately
//!
//! A disabled tool is withheld from [`ToolExecutor::schemas`] *and* refused by
//! [`ToolExecutor::execute`]. The two-sided shape is the point: hiding a schema
//! is a prompt-budget measure, but a capability gate has to hold when the model
//! calls the name anyway — from a stale prompt, a replayed trajectory, or a
//! hand-written call. `DiscoveryToolSet`'s lean mode is deliberately the
//! opposite (it hides without gating); this is not that, and the two must not
//! be confused.
//!
//! The refusal reads like the unknown-tool error rather than announcing a
//! hidden capability, so a disabled tool is indistinguishable from one that was
//! never built — the property `bash` had when it was opt-in.

use async_trait::async_trait;
use serde_json::Value;
use stella_core::ports::ToolExecutor;
use stella_protocol::tool::{ToolOutput, ToolSchema};
use stella_tools::policy::ToolPolicy;

/// Wraps a tool surface and withholds everything the policy turns off.
pub struct PolicyToolSet<'a> {
    inner: Inner<'a>,
    policy: ToolPolicy,
}

/// The wrapped executor, held either by borrow (a session's per-turn tool
/// chain, a stack local) or owned via `Arc` (a best-of-N candidate, whose
/// chain is built dynamically and outlives every borrow). Mirrors
/// [`stella_tools::custom::CustomToolSet`]'s shape deliberately — the two sit
/// next to each other in the same stacks.
enum Inner<'a> {
    Borrowed(&'a dyn ToolExecutor),
    Owned(std::sync::Arc<dyn ToolExecutor>),
}

impl Inner<'_> {
    fn get(&self) -> &dyn ToolExecutor {
        match self {
            Inner::Borrowed(inner) => *inner,
            Inner::Owned(inner) => inner.as_ref(),
        }
    }
}

impl<'a> PolicyToolSet<'a> {
    pub fn new(inner: &'a dyn ToolExecutor, policy: ToolPolicy) -> Self {
        Self {
            inner: Inner::Borrowed(inner),
            policy,
        }
    }
}

impl PolicyToolSet<'static> {
    /// Own the inner executor by `Arc` — for callers that must hold the whole
    /// chain as one value, e.g. a boxed best-of-N candidate workspace. A
    /// candidate's registry is built from the same `RegistryOptions` as the
    /// session's, so without this the policy would stop at the candidate
    /// boundary and best-of-N would be a way around it.
    pub fn new_owned(inner: std::sync::Arc<dyn ToolExecutor>, policy: ToolPolicy) -> Self {
        Self {
            inner: Inner::Owned(inner),
            policy,
        }
    }
}

#[async_trait]
impl ToolExecutor for PolicyToolSet<'_> {
    fn schemas(&self) -> Vec<ToolSchema> {
        let mut schemas = self.inner.get().schemas();
        schemas.retain(|schema| self.policy.allows(&schema.name));
        schemas
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        if !self.policy.allows(name) {
            // Same wording shape as an unknown tool: a disabled tool must not
            // advertise itself through its own refusal.
            return ToolOutput::Error {
                message: format!("unknown tool: {name}"),
            };
        }
        self.inner.get().execute(name, input).await
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
}

/// Which `"tools"` key withheld `name` — the exact name, its group, or the
/// wildcard — resolved in the same most-specific-first order the policy
/// itself uses. `None` when the tool is allowed.
///
/// For explaining a posture (`stella tools`, the settings UI), never for
/// enforcing one: enforcement is [`ToolPolicy::allows`]. Reporting "off" with
/// no key would leave an operator hunting through three scopes for a switch
/// they may not have written themselves.
pub fn disabled_by(policy: &ToolPolicy, name: &str) -> Option<String> {
    if policy.allows(name) {
        return None;
    }
    // An MCP or custom tool can be denied by its exact name without the
    // catalog ever having heard of it, so `name` leads and the fallbacks
    // follow — never the reverse, or the report would name the group when a
    // more specific key is what actually did it.
    [
        name,
        stella_tools::catalog::group_for(name),
        stella_tools::policy::WILDCARD,
    ]
    .into_iter()
    .find(|key| policy.switches().get(*key) == Some(&false))
    .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_tools::policy::WILDCARD;

    struct Fake;

    #[async_trait]
    impl ToolExecutor for Fake {
        fn schemas(&self) -> Vec<ToolSchema> {
            [
                "read_file",
                "bash",
                "start_process",
                "mcp__gh__create_issue",
            ]
            .into_iter()
            .map(|name| ToolSchema {
                name: name.into(),
                description: String::new(),
                input_schema: serde_json::json!({}),
                read_only: false,
                speculation_safe: false,
            })
            .collect()
        }
        async fn execute(&self, name: &str, _input: &Value) -> ToolOutput {
            ToolOutput::Ok {
                content: format!("ran {name}"),
            }
        }
    }

    fn names(set: &PolicyToolSet<'_>) -> Vec<String> {
        set.schemas().into_iter().map(|s| s.name).collect()
    }

    #[tokio::test]
    async fn the_default_policy_withholds_nothing() {
        let fake = Fake;
        let set = PolicyToolSet::new(&fake, ToolPolicy::allow_all());
        assert_eq!(names(&set).len(), 4);
        // bash ships ON. This is the assertion that changes with the default.
        assert!(matches!(
            set.execute("bash", &serde_json::json!({})).await,
            ToolOutput::Ok { .. }
        ));
    }

    #[tokio::test]
    async fn a_disabled_tool_is_hidden_and_refused() {
        let fake = Fake;
        let set = PolicyToolSet::new(&fake, ToolPolicy::from_switches([("bash".into(), false)]));

        assert!(!names(&set).contains(&"bash".to_string()), "hidden");
        assert!(names(&set).contains(&"read_file".to_string()), "untouched");

        // The half that matters: calling it by name anyway must not execute.
        match set.execute("bash", &serde_json::json!({})).await {
            ToolOutput::Error { message } => assert!(
                message.contains("unknown tool"),
                "a disabled tool must not announce itself: {message}"
            ),
            other => panic!("a disabled tool must be refused, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_group_switch_covers_the_whole_family() {
        let fake = Fake;
        let set = PolicyToolSet::new(
            &fake,
            ToolPolicy::from_switches([("process".into(), false)]),
        );
        assert!(!names(&set).contains(&"start_process".to_string()));
        assert!(matches!(
            set.execute("start_process", &serde_json::json!({})).await,
            ToolOutput::Error { .. }
        ));
    }

    /// The reason this decorator sits above the MCP and custom layers instead
    /// of inside the registry: those tools never pass through `RegistryOptions`.
    #[tokio::test]
    async fn mcp_tools_are_covered_too() {
        let fake = Fake;
        let set = PolicyToolSet::new(&fake, ToolPolicy::from_switches([("mcp".into(), false)]));
        assert!(!names(&set).contains(&"mcp__gh__create_issue".to_string()));
        assert!(matches!(
            set.execute("mcp__gh__create_issue", &serde_json::json!({}))
                .await,
            ToolOutput::Error { .. }
        ));
        assert!(names(&set).contains(&"read_file".to_string()));
    }

    /// `stella tools` has to name the entry that did it — "disabled (default)"
    /// is not an answer any more, and an operator staring at three settings
    /// scopes needs the key, not just the verdict.
    #[test]
    fn a_disabled_tool_reports_the_key_that_withheld_it() {
        let policy = ToolPolicy::from_switches([
            (WILDCARD.into(), false),
            ("process".into(), true),
            ("send_stdin".into(), false),
            ("mcp__gh__create_issue".into(), false),
        ]);
        // Most specific first, in the same order `allows` resolves.
        assert_eq!(
            disabled_by(&policy, "send_stdin").as_deref(),
            Some("send_stdin")
        );
        assert_eq!(disabled_by(&policy, "read_file").as_deref(), Some(WILDCARD));
        // An MCP tool denied by exact name, which no catalog lookup can find.
        assert_eq!(
            disabled_by(&policy, "mcp__gh__create_issue").as_deref(),
            Some("mcp__gh__create_issue")
        );
        // An allowed tool has no key to report.
        assert_eq!(disabled_by(&policy, "start_process"), None);

        // A group denial is reported as the group, not as the tool.
        let policy = ToolPolicy::from_switches([("process".into(), false)]);
        assert_eq!(
            disabled_by(&policy, "start_process").as_deref(),
            Some("process")
        );
    }

    #[tokio::test]
    async fn deny_by_default_leaves_only_what_is_re_enabled() {
        let fake = Fake;
        let set = PolicyToolSet::new(
            &fake,
            ToolPolicy::from_switches([(WILDCARD.into(), false), ("read_file".into(), true)]),
        );
        assert_eq!(names(&set), vec!["read_file".to_string()]);
    }
}
