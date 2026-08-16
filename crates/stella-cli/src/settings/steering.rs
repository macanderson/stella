//! The steering plane's on/off switch (#3243): the managed ceiling type and
//! the one derivation every caller reads it through.
//!
//! The [`SteeringCeiling`] value itself is captured by
//! [`Settings::merge_captured_scopes`](super::Settings) in `merge.rs`; this
//! module owns the type and the precedence logic so the two halves of the
//! answer — "did anyone turn it off?" and "did the org forbid it?" — live
//! beside each other.

use super::{Settings, truthy_flag};

/// The managed scope's answer to "may this workspace steer at all?".
///
/// A named type rather than a bare `bool` for the reason `Toggle` is not a
/// `bool` either: `true` here means "the org expressed no objection", which
/// is the default and is *not* the same fact as "the org enabled it".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SteeringCeiling(pub(crate) bool);

impl Default for SteeringCeiling {
    fn default() -> Self {
        Self(true)
    }
}

impl Settings {
    /// Whether the steering plane may inject anything this session (#3243).
    ///
    /// The precedence, which is the house convention plus the one deliberate
    /// inversion the tool ceiling already uses:
    ///
    /// ```text
    /// built-in default (ON)
    ///   < user settings < org-managed < project settings
    ///   < STELLA_CONTEXT_STEERING        ← env wins the ordinary chain
    ///   ⟂ managed ceiling applied LAST   ← the org's "off" is final
    /// ```
    ///
    /// Two sentences: **anyone may turn steering off; only the org may
    /// prevent it being turned back on.** That is exactly `apply_tool_ceiling`'s
    /// shape, so this needs no new concept — and it is why the ceiling is a
    /// separate field rather than folded into the merged block. Folded in,
    /// "the project turned it off" and "the org forbade it" would be the same
    /// value, and the env var could not be allowed to override the first
    /// without also overriding the second.
    pub fn steering_enabled(&self) -> bool {
        if !self.steering_ceiling.0 {
            return false;
        }
        if let Some(from_env) = env_toggle("STELLA_CONTEXT_STEERING") {
            return from_env;
        }
        self.context
            .as_ref()
            .is_none_or(|context| context.steering.enabled)
    }
}

/// The tri-state reading of an environment variable: unset, explicitly on, or
/// explicitly off.
///
/// [`super::env_flag`] collapses "unset" and "set to something falsy" into
/// `false`, which is right for a flag that only ever turns something ON. A
/// switch that defaults on needs the third state, because "unset" must mean
/// "defer to settings" and not "off". Same strict truthy vocabulary, so
/// `STELLA_CONTEXT_STEERING=false` turns steering off rather than opening it
/// — the `STELLA_TRUST_PROJECT` defect that gave `truthy_flag` its
/// allowlist.
pub(crate) fn env_toggle(name: &str) -> Option<bool> {
    std::env::var_os(name).map(|v| truthy_flag(&v))
}
