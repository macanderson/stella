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

/// The operator's tool switches, filled beside the providers.
///
/// A forge tool groups several verbs over one object, so the registry's own
/// gate — which answers for a whole tool — cannot withhold `merge` and keep
/// `comment`. The tools read this slot to answer that narrower question.
/// `AGENTS.md invariant 9`'s second reason asks for it, and ADR 0044 is
/// where the grouping is decided.
///
/// An empty slot allows every action, which is the shipped posture: a host
/// that attaches no policy loses a switch, never a refusal it expected.
pub type PolicySlot = Arc<RwLock<crate::policy::ToolPolicy>>;

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
    /// Which of these tools' actions the operator has switched off.
    pub policy: PolicySlot,
}

impl ForgeSlots {
    /// Refuse `action` on `tool` when the operator has switched it off.
    ///
    /// The registry's gate answers for a whole tool, and these tools carry
    /// several verbs, so this is where `"pull_request.merge": "off"` takes
    /// effect. The refusal names the key that did it: a model told only
    /// "refused" will try a different spelling of the same action, and a
    /// model told which settings entry refused it reports that to the driver
    /// and moves on.
    ///
    /// `ErrorClass::RefusedByPolicy` rather than `Environment`: nothing is missing,
    /// somebody decided this.
    pub(crate) fn permits(&self, tool: &str, action: &str) -> Result<(), ToolOutput> {
        let allowed = self
            .policy
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .allows_action(tool, action);
        if allowed {
            return Ok(());
        }
        Err(ToolOutput::classified_error(
            ErrorClass::RefusedByPolicy,
            format!(
                "`{action}` is switched off for `{tool}` by \"tools\": \
                 {{\"{tool}.{action}\": \"off\"}} in settings. The tool's other actions still \
                 run. Ask whoever is driving to change the setting, or do this step by hand."
            ),
        ))
    }

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

    /// Is a tracker attached?
    ///
    /// [`Self::tracker`] answers the same question and builds a refusal with
    /// it. [`crate::forge::redirect`] has no refusal to build: it is deciding
    /// whether a `gh` command has a tool to be pointed at, and a bare yes or
    /// no is the whole answer. It asks through here rather than reading the
    /// slot itself, so the poison rule below is stated once. Reading it in two
    /// places is how one lock comes to have two policies: the first version of
    /// the redirect used `.read().ok()?`, which turns a poisoned lock into
    /// "nothing is attached" and lets every covered `gh` command through.
    pub(crate) fn has_tracker(&self) -> bool {
        self.issues
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }

    /// Is a forge attached? See [`Self::has_tracker`].
    pub(crate) fn has_forge(&self) -> bool {
        self.pull_requests
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
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

/// Refuse an action the tool does not have, before anything is resolved.
///
/// The match at the end of each tool's `run` reports the same thing, and both
/// are wanted. This one runs first, so a typo is answered as a typo rather
/// than as whatever the next step happens to fail on: without it, a workspace
/// with no `gh` answers `action: "frobnicate"` with "no forge is configured",
/// which sends the reader after a missing binary instead of a misspelled
/// word. The one at the end catches the other drift: an `ACTIONS` entry that
/// nobody wrote a match arm for.
pub(crate) fn known_action(action: &str, known: &[&str]) -> Result<(), ToolOutput> {
    if known.contains(&action) {
        return Ok(());
    }
    Err(unknown_action(action, known))
}

/// The refusal that lists what this tool does take.
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

    /// A misspelled action is answered as a misspelling, with no forge
    /// attached.
    ///
    /// The tools resolved the provider before reading the action, so every
    /// bad action on a workspace without `gh` came back as "no forge is
    /// configured". That reading sends someone to install a binary over a
    /// typo, and it hides the list of actions that would have fixed it.
    #[test]
    fn an_unknown_action_is_not_reported_as_a_missing_forge() {
        let slots = ForgeSlots::default();
        let known = ["create", "update"];
        assert!(known_action("create", &known).is_ok());
        let Err(ToolOutput::Error { message, .. }) = known_action("frobnicate", &known) else {
            panic!("an action the tool does not have must be an error");
        };
        assert!(message.contains("frobnicate"), "{message}");
        assert!(message.contains("create, update"), "{message}");
        assert!(
            !message.contains("gh auth status"),
            "must not read as a setup problem: {message}"
        );
        assert!(slots.forge().is_err(), "the slot really is empty");
    }
}
