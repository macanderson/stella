//! The one place a tool an operator switched off is actually withheld.
//!
//! [`stella_tools::policy::ToolPolicy`] decides *what* is off; this decorator
//! is *where* that is enforced. It sits above the MCP and custom layers rather
//! than inside [`stella_tools::registry::ToolRegistry`], because that is the
//! only position that sees the complete session surface — built-ins, every
//! connected MCP server's tools, and whatever the customer registered in
//! `.stella/tools/*.toml`. Enforcing inside the registry would cover only the
//! first of the three.
//!
//! # Both sides, deliberately
//!
//! A disabled tool is withheld from [`ToolExecutor::schemas`] *and* refused by
//! [`ToolExecutor::execute`]. The two-sided shape is the point: hiding a schema
//! is a prompt-budget measure, but a capability gate has to hold when the model
//! calls the name anyway — from a stale prompt, a replayed trajectory, or a
//! hand-written call.
//!
//! The refusal reads like the unknown-tool error rather than announcing a
//! hidden capability, so a disabled tool is indistinguishable from one that
//! was never built.

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
    Boxed(Box<dyn ToolExecutor + 'a>),
    Owned(std::sync::Arc<dyn ToolExecutor>),
}

impl Inner<'_> {
    fn get(&self) -> &dyn ToolExecutor {
        match self {
            Inner::Borrowed(inner) => *inner,
            Inner::Boxed(inner) => inner.as_ref(),
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

    /// Own the inner executor by value — for [`crate::agent::tool_stack`],
    /// which builds the custom layer below this one and returns the whole
    /// chain as a single value. The lifetime rides along, so the boxed chain
    /// may still borrow the session's registry at its base.
    pub fn new_boxed(inner: Box<dyn ToolExecutor + 'a>, policy: ToolPolicy) -> Self {
        Self {
            inner: Inner::Boxed(inner),
            policy,
        }
    }
}

impl PolicyToolSet<'static> {
    /// Own the inner executor by `Arc` — for callers that must hold the whole
    /// chain as one value, e.g. a boxed best-of-N candidate workspace.
    /// Without this the policy would stop at the candidate boundary and
    /// best-of-N would be a way around it.
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

    /// Forwarded with exactly the filter `schemas()` applies (#3287) — a
    /// withheld tool has no advertised contract, and a decorator that let
    /// the port's declared-`High` default stand would degrade every built-in
    /// underneath it to untrusted.
    fn contracts(&self) -> Vec<stella_protocol::ToolContract> {
        let mut contracts = self.inner.get().contracts();
        contracts.retain(|contract| self.policy.allows(contract.name()));
        contracts
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        if !self.policy.allows(name) {
            // Same wording shape as an unknown tool: a disabled tool must not
            // advertise itself through its own refusal.
            return ToolOutput::error(format!("unknown tool: {name}"));
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

    /// Forwarded: a decorator that let the empty default stand would silently
    /// turn the end-of-turn service assertion (#2764) off for every surface
    /// composed through it — the agent goes back to declaring a service done
    /// without ever being asked whether it is still listening.
    fn live_services(&self) -> Vec<stella_core::LiveService> {
        self.inner.get().live_services()
    }

    /// Forwarded minus what the policy withholds. Letting the empty default
    /// stand would silently serialize the inner executor's sibling spawns —
    /// but blindly delegating would advertise names `execute` above refuses,
    /// an empty promise. Same two-sided shape as `schemas`/`execute`.
    fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
        let mut names = self.inner.get().parallel_safe_names();
        names.retain(|name| self.policy.allows(name));
        names
    }

    /// Forwarded, and NOT narrowed: this layer decides whether a tool is
    /// switched on, the gate decides what the session's extension policy said
    /// about a call that is. Letting the `None` default stand would un-gate
    /// every custom tool, since the custom set sits directly beneath this one
    /// in the shipped chain (#2793).
    fn dispatch_gate(&self) -> Option<&dyn stella_core::ports::DispatchGate> {
        self.inner.get().dispatch_gate()
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
                "task_list",
                "save_state",
                "get_environment",
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
                data: None,
            }
        }
        fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
            std::collections::HashSet::from(["delegate".to_string()])
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
        assert!(matches!(
            set.execute("save_state", &serde_json::json!({})).await,
            ToolOutput::Ok { .. }
        ));
    }

    #[tokio::test]
    async fn a_disabled_tool_is_hidden_and_refused() {
        let fake = Fake;
        let set = PolicyToolSet::new(
            &fake,
            ToolPolicy::from_switches([("save_state".into(), false)]),
        );

        assert!(!names(&set).contains(&"save_state".to_string()), "hidden");
        assert!(names(&set).contains(&"task_list".to_string()), "untouched");

        // The half that matters: calling it by name anyway must not execute.
        match set.execute("save_state", &serde_json::json!({})).await {
            ToolOutput::Error { message, .. } => assert!(
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
            ToolPolicy::from_switches([("scratch".into(), false)]),
        );
        assert!(!names(&set).contains(&"save_state".to_string()));
        assert!(matches!(
            set.execute("save_state", &serde_json::json!({})).await,
            ToolOutput::Error { .. }
        ));
    }

    /// The reason this decorator sits above the MCP and custom layers instead
    /// of inside the registry: those tools never pass through it.
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
        assert!(names(&set).contains(&"task_list".to_string()));
    }

    /// `stella tools` has to name the entry that did it — "disabled (default)"
    /// is not an answer any more, and an operator staring at three settings
    /// scopes needs the key, not just the verdict.
    #[test]
    fn a_disabled_tool_reports_the_key_that_withheld_it() {
        let policy = ToolPolicy::from_switches([
            (WILDCARD.into(), false),
            ("scratch".into(), true),
            ("delete_state".into(), false),
            ("mcp__gh__create_issue".into(), false),
        ]);
        // Most specific first, in the same order `allows` resolves.
        assert_eq!(
            disabled_by(&policy, "delete_state").as_deref(),
            Some("delete_state")
        );
        assert_eq!(disabled_by(&policy, "task_list").as_deref(), Some(WILDCARD));
        // An MCP tool denied by exact name, which no catalog lookup can find.
        assert_eq!(
            disabled_by(&policy, "mcp__gh__create_issue").as_deref(),
            Some("mcp__gh__create_issue")
        );
        // An allowed tool has no key to report.
        assert_eq!(disabled_by(&policy, "save_state"), None);

        // A group denial is reported as the group, not as the tool.
        let policy = ToolPolicy::from_switches([("scratch".into(), false)]);
        assert_eq!(
            disabled_by(&policy, "save_state").as_deref(),
            Some("scratch")
        );
    }

    /// The concurrency claim survives the decorator when the policy allows
    /// the tool — a set that forgot to forward would silently serialize
    /// sibling spawns in every real session (the default is empty).
    #[test]
    fn parallel_safe_names_are_forwarded_when_the_policy_allows() {
        let fake = Fake;
        let set = PolicyToolSet::new(&fake, ToolPolicy::allow_all());
        assert!(
            set.parallel_safe_names().contains("delegate"),
            "the inner executor's claim must survive the policy layer"
        );
    }

    /// The other half of the two-sided shape: a tool the policy withholds is
    /// refused by `execute`, so advertising it as parallel-safe would claim
    /// concurrency for a call this decorator will never run.
    #[test]
    fn a_disabled_tool_is_not_advertised_as_parallel_safe() {
        let fake = Fake;
        let set = PolicyToolSet::new(
            &fake,
            ToolPolicy::from_switches([("delegate".into(), false)]),
        );
        assert!(
            set.parallel_safe_names().is_empty(),
            "a withheld tool must not be advertised as parallel-safe"
        );
    }

    #[tokio::test]
    async fn deny_by_default_leaves_only_what_is_re_enabled() {
        let fake = Fake;
        let set = PolicyToolSet::new(
            &fake,
            ToolPolicy::from_switches([(WILDCARD.into(), false), ("task_list".into(), true)]),
        );
        assert_eq!(names(&set), vec!["task_list".to_string()]);
    }

    // -----------------------------------------------------------------------
    // #3120 witnesses: the assembled session surface, not the policy
    // arithmetic. That issue shipped precisely because the policy's own unit
    // tests were green while the layers above and beside it leaked — so these
    // build the real stack the way every session driver does and census what
    // it advertises, which is the exact `tools` array the provider serializes.
    // -----------------------------------------------------------------------

    /// The real session chain — native registry, customs, the policy filter
    /// on top — censused exactly as a session driver assembles it.
    async fn wire_surface(
        root: &std::path::Path,
        policy: ToolPolicy,
        call: Option<(&str, Value)>,
    ) -> (Vec<ToolSchema>, Option<ToolOutput>) {
        let registry = stella_tools::ToolRegistry::new(root.to_path_buf());
        let customs =
            stella_tools::custom::CustomToolSet::new(&registry, vec![], root.to_path_buf());
        let tools = PolicyToolSet::new(&customs, policy);
        let schemas = tools.schemas();
        let output = match call {
            Some((name, input)) => Some(tools.execute(name, &input).await),
            None => None,
        };
        (schemas, output)
    }

    /// **The #3120 witness.** `--tools "*:off,task_list:on,get_state:on"`
    /// puts exactly the two requested names on the wire — an only-set is the
    /// whole advertised surface, with nothing re-added above the policy
    /// filter and no group key resolving wider than its family.
    #[tokio::test]
    async fn a_tools_spec_advertises_exactly_the_requested_set() {
        let root = tempfile::tempdir().unwrap();
        let spec = "*:off,task_list:on,get_state:on";
        let mut policy = ToolPolicy::allow_all();
        policy.narrow_with(&ToolPolicy::parse_spec(spec).unwrap());

        let (schemas, _) = wire_surface(root.path(), policy, None).await;
        let mut advertised: Vec<String> = schemas.into_iter().map(|s| s.name).collect();
        advertised.sort_unstable();
        assert_eq!(
            advertised,
            ["get_state", "task_list"].map(String::from),
            "two requested, exactly two on the wire"
        );
    }
}
