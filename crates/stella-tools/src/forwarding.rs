//! One inner executor and one assertion, for every decorator's forwarding
//! test.
//!
//! Four [`ToolExecutor`] methods have a trait default that reads "this
//! executor mounts none of that". That is right for a leaf. It is wrong for a
//! wrapper, and the port says so under its own `# Decorators MUST forward
//! this` headings. A decorator that drops one compiles clean and answers the
//! default. Only a test can catch it.
//!
//! One audit hit that shape twice in a single pass. `CustomToolSet` dropped
//! `active_skill_slugs`, so a live skill's procedure text stopped surviving
//! summarization in any workspace with a custom tool installed.
//!
//! [`LoadedExecutor`] answers all four with a value of its own.
//! [`assert_forwards`] reads them back through the decorator. A method added
//! to the port is added here once, rather than to every decorator's own copy.
//!
//! **Not a `#[cfg(test)]` module.** Three crates hold decorators of this
//! trait, and a test-gated item is invisible to the other two.
//!
//! # What this does not check
//!
//! Narrowing. Decorators narrow `schemas`, `contracts`,
//! `parallel_safe_names`, `tool_origin` and `dispatch_gate` on purpose.
//! `ReadOnlyTools` hides a mutating tool, and hiding it is the whole job. The
//! four here are the ones the port marks as pass-through. Narrowing one of
//! those is always a defect.

use stella_core::LiveService;
use stella_core::ports::ToolExecutor;
use stella_core::waiting::{WaitCall, WaitRequest};
use stella_protocol::tool::{ToolOutput, ToolSchema};

/// The sub-agent spend [`LoadedExecutor`] reports.
pub const SPEND_USD: f64 = 2.5;
/// The skill slug [`LoadedExecutor`] reports as active.
pub const SKILL_SLUG: &str = "release-checklist";
/// The handle of the service [`LoadedExecutor`] reports as still up.
pub const SERVICE_HANDLE: &str = "proc-3";
/// The description of the wait [`LoadedExecutor`] deposits.
pub const WAIT_DESCRIPTION: &str = "CI settles";

/// An inner executor that answers every pass-through method with a value no
/// trait default returns. A decorator that drops one is then visible.
pub struct LoadedExecutor;

#[async_trait::async_trait]
impl ToolExecutor for LoadedExecutor {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "bash".into(),
            description: "run".into(),
            input_schema: serde_json::json!({ "type": "object" }),
            read_only: false,
            speculation_safe: false,
        }]
    }

    async fn execute(&self, name: &str, _input: &serde_json::Value) -> ToolOutput {
        ToolOutput::Ok {
            content: format!("inner ran {name}"),
            data: None,
        }
    }

    fn drain_sub_agent_spend_usd(&self) -> f64 {
        SPEND_USD
    }

    fn drain_wait_request(&self) -> Option<WaitRequest> {
        Some(WaitRequest {
            description: WAIT_DESCRIPTION.to_string(),
            probe: WaitCall {
                name: "bash".to_string(),
                input: serde_json::json!({ "cmd": "gh run list" }),
            },
            baseline: "none yet".to_string(),
            on_wake: None,
            poll_interval_secs: 30,
            timeout_secs: 300,
        })
    }

    fn active_skill_slugs(&self) -> Vec<String> {
        vec![SKILL_SLUG.to_string()]
    }

    fn live_services(&self) -> Vec<LiveService> {
        vec![LiveService {
            handle: SERVICE_HANDLE.to_string(),
            name: None,
            display: "npm run dev".to_string(),
        }]
    }
}

/// Read every pass-through method back through `decorator`, which must wrap
/// a [`LoadedExecutor`].
///
/// `what` names the decorator. A failure then says which wrapper dropped
/// which method, rather than which line of this file noticed.
///
/// # Panics
///
/// When `decorator` answers any of the four with the trait default instead of
/// the inner's value.
pub fn assert_forwards(what: &str, decorator: &dyn ToolExecutor) {
    assert_eq!(
        decorator.drain_sub_agent_spend_usd(),
        SPEND_USD,
        "{what} dropped drain_sub_agent_spend_usd: a nested turn's spend \
         vanishes from the ceiling"
    );
    assert_eq!(
        decorator
            .drain_wait_request()
            .map(|request| request.description),
        Some(WAIT_DESCRIPTION.to_string()),
        "{what} dropped drain_wait_request: the model burns steps polling \
         instead of parking"
    );
    assert_eq!(
        decorator.active_skill_slugs(),
        vec![SKILL_SLUG.to_string()],
        "{what} dropped active_skill_slugs: a live skill's procedure text \
         stops surviving summarization"
    );
    assert_eq!(
        decorator
            .live_services()
            .into_iter()
            .map(|service| service.handle)
            .collect::<Vec<_>>(),
        vec![SERVICE_HANDLE.to_string()],
        "{what} dropped live_services: the turn declares a service done \
         without asking whether it is still listening"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The helper's own anti-vacuity. A wrapper that forwards nothing must
    /// fail every check. Without that, `assert_forwards` proves nothing about
    /// the ones that pass.
    struct ForgetsEverything<'a>(&'a dyn ToolExecutor);

    #[async_trait::async_trait]
    impl ToolExecutor for ForgetsEverything<'_> {
        fn schemas(&self) -> Vec<ToolSchema> {
            self.0.schemas()
        }
        async fn execute(&self, name: &str, input: &serde_json::Value) -> ToolOutput {
            self.0.execute(name, input).await
        }
    }

    #[test]
    fn the_loaded_executor_forwards_to_itself() {
        assert_forwards("LoadedExecutor", &LoadedExecutor);
    }

    #[test]
    fn a_decorator_that_forwards_nothing_is_caught() {
        let inner = LoadedExecutor;
        let forgetful = ForgetsEverything(&inner);
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_forwards("ForgetsEverything", &forgetful);
        }));
        assert!(
            caught.is_err(),
            "a wrapper answering every default must fail the assertion"
        );
    }
}
