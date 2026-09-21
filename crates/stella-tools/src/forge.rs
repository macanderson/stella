//! The forge plane: issues, pull requests, and the checks that run on them.
//!
//! # Signing in Rust
//!
//! A `gh pr create` in `bash` writes the text the model typed and no more. To
//! get a footer onto it, you have to ask for one in the prompt. That is what
//! `self_driving_cmd::work::prompt_for` does. A model can skip that line, or
//! reword it, or lay it out some new way. What lands is then unsigned, and
//! nothing downstream can tell.
//!
//! A guard hook cannot patch that up. `HookDecision` is `Allow` or `Deny`,
//! with no third answer that rewrites. A hook can block `gh pr create`; it
//! cannot add a line to the body.
//!
//! A tool can. Every body that leaves this module goes through
//! [`stella_autonomy::sign`] on the way out. That runs in Rust, once the model
//! has had its say. The model writes what it wants the reader to know. The
//! footer is not part of what it writes.
//!
//! # Three tools for eleven verbs
//!
//! The verbs are grouped by the thing they act on. A tool schema is not free.
//! It rides the cached prompt prefix on every call, even when the session
//! opens no pull request. One tool per verb would cost eleven names and
//! eleven blocks of schema text. The model has to tell them all apart.
//!
//! - [`pull_request`](pr::PullRequestTool): `create`, `update`, `close`,
//!   `merge`, `comment`, `edit_comment`.
//! - [`issue`](issue::IssueTool): `create`, `update`, `close`, `comment`,
//!   `edit_comment`.
//! - [`watch_ci`](watch::WatchCi): one branch, its checks, and what the
//!   verdict is.
//!
//! # Slots the host fills
//!
//! The adapters that reach a forge live in `stella-cli` (`issue_provider` and
//! `pull_request_provider`). They sit one crate up, so this crate cannot name
//! them. The tools hold [`ForgeSlots`] instead: handles the host fills after
//! assembly, the shape [`crate::subagent::DispatcherSlot`] and
//! [`crate::registry::question`] take.
//!
//! A tool whose slot is empty is still listed. It answers by saying no forge is
//! set up, and by naming what would set one up. Drop the schema instead, and the
//! tool list depends on settings the prompt cache never sees change.

pub mod issue;
pub mod pr;
pub mod redirect;
pub mod watch;

use std::sync::{Arc, RwLock};

use serde_json::Value;
use stella_autonomy::Attribution;
use stella_protocol::issue::IssueProvider;
use stella_protocol::pull_request::PullRequestProvider;
use stella_protocol::tool::{ErrorClass, ToolOutput};

/// The tracker handle a host fills after assembly. `None` until
/// [`crate::ToolRegistry::attach_forge`] runs.
pub type IssueSlot = Arc<RwLock<Option<Arc<dyn IssueProvider>>>>;

/// The forge handle, filled beside [`IssueSlot`].
///
/// The two are set up on their own. A workspace can put its code on GitHub and
/// its issues in Linear. One slot would force them to be one system.
pub type PullRequestSlot = Arc<RwLock<Option<Arc<dyn PullRequestProvider>>>>;

/// What the footer says on each surface, filled beside the providers.
///
/// It sits in a slot rather than being fixed when the tools are built. The text
/// comes from `stella.toml`, the host reads that file, and this crate reads no
/// settings. An empty slot signs with [`Attribution::default`], so a host that
/// attaches none still signs. A missing setup costs you the default footer,
/// never no footer.
pub type AttributionSlot = Arc<RwLock<Attribution>>;

/// Everything the three forge tools need from the host, in one bundle.
///
/// One struct rather than three loose arguments. The host fills all three at
/// once, in one call. A part-filled attach is not a state a caller should be
/// able to write down.
#[derive(Clone, Default)]
pub struct ForgeSlots {
    /// The tracker, for [`issue::IssueTool`].
    pub issues: IssueSlot,
    /// The forge, for [`pr::PullRequestTool`] and [`watch::WatchCi`].
    pub pull_requests: PullRequestSlot,
    /// What every body written through these tools is signed with.
    pub attribution: AttributionSlot,
}

impl ForgeSlots {
    /// The footer to sign with right now.
    ///
    /// Cloned rather than borrowed. The lock must not be held across the
    /// `await` that comes after each read of it.
    pub(crate) fn attribution(&self) -> Attribution {
        self.attribution
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// The tracker, or the refusal that says how to get one.
    pub(crate) fn tracker(&self) -> Result<Arc<dyn IssueProvider>, ToolOutput> {
        self.issues
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .ok_or_else(|| {
                unavailable(
                    "no issue tracker is configured for this workspace, so there is nothing to \
                     file against. `gh` on PATH and authenticated (`gh auth status`) is what \
                     configures the default one.",
                )
            })
    }

    /// The forge, or the refusal that says how to get one.
    pub(crate) fn forge(&self) -> Result<Arc<dyn PullRequestProvider>, ToolOutput> {
        self.pull_requests
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .ok_or_else(|| {
                unavailable(
                    "no forge is configured for this workspace, so there are no pull requests to \
                     act on. `gh` on PATH and authenticated (`gh auth status`) is what configures \
                     the default one.",
                )
            })
    }
}

/// The refusal a tool gives when its slot is empty.
///
/// An error rather than an `Ok` with bad news inside it. Hand a model a
/// cheerful paragraph and it will report the pull request as opened.
fn unavailable(reason: &str) -> ToolOutput {
    ToolOutput::classified_error(ErrorClass::Environment, reason)
}

/// Read a required string argument, or say which one is missing.
pub(crate) fn required<'a>(input: &'a Value, field: &str) -> Result<&'a str, ToolOutput> {
    input
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ToolOutput::classified_error(
                ErrorClass::InvalidInput,
                format!("`{field}` is required and must be a non-empty string"),
            )
        })
}

/// Read an optional string argument.
///
/// An empty string reads as absent. On an update that means leave it alone,
/// which is the safe reading. A model that sends `body: ""` to change the title
/// alone must not blank the text.
pub(crate) fn optional<'a>(input: &'a Value, field: &str) -> Option<&'a str> {
    input
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// The refusal for an `action` this tool does not have.
pub(crate) fn unknown_action(action: &str, known: &[&str]) -> ToolOutput {
    ToolOutput::classified_error(
        ErrorClass::InvalidInput,
        format!(
            "unknown action `{action}`. This tool takes one of: {}.",
            known.join(", ")
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// An empty string is absent. It is not an order to blank the field.
    ///
    /// Read `""` as "set it to empty" and a change of title wipes the body
    /// with it. A title is quick to type again. A body is not.
    #[test]
    fn an_empty_optional_string_reads_as_absent() {
        let input = json!({"title": "  ", "body": "real"});
        assert_eq!(optional(&input, "title"), None);
        assert_eq!(optional(&input, "body"), Some("real"));
        assert_eq!(optional(&input, "missing"), None);
    }

    /// An unfilled slot refuses by naming the fix, and refuses as an error.
    #[test]
    fn an_unconfigured_forge_refuses_rather_than_reporting_success() {
        let slots = ForgeSlots::default();
        let Err(ToolOutput::Error { message, .. }) = slots.forge() else {
            panic!("an absent forge must be an error, not an Ok carrying bad news");
        };
        assert!(message.contains("gh auth status"), "{message}");
    }

    /// A slot nobody filled signs with the default footer, never with nothing.
    #[test]
    fn an_unattached_attribution_still_signs() {
        assert_eq!(ForgeSlots::default().attribution(), Attribution::default());
    }
}
