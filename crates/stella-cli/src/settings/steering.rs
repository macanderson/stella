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

    /// How much of the tool set this session sends.
    ///
    /// [`Self::steering_enabled`] is asked first and can settle it alone.
    /// With the plane off, no tool may be held back. Else the switch would
    /// take a tool away, not just a hint.
    ///
    /// The budget it hands back is the one the workspace **declares**. That
    /// is the whole volatile allowance, `context.steering.max_tokens`. The
    /// turn's recall block spends from it before the tool stack sees it. So
    /// the packer takes its number from `stella_core::steering::ledger`, not
    /// from here.
    ///
    /// `STELLA_TOOLS_LEAN` beats the settings chain, as
    /// `STELLA_CONTEXT_STEERING` does. A bench arm is picked by the harness
    /// that starts the run, not by an edit to the tree it measures.
    pub fn tool_advertisement(&self) -> stella_core::steering::tools::ToolAdvertisement {
        use stella_core::steering::tools::{ToolAdvertisement, ToolBudget};

        if !self.steering_enabled() {
            return ToolAdvertisement::Full;
        }
        let steering = self
            .context
            .as_ref()
            .map(|context| context.steering.clone())
            .unwrap_or_default();
        let lean = env_toggle("STELLA_TOOLS_LEAN").unwrap_or(steering.tools.lean);
        if !lean {
            return ToolAdvertisement::Full;
        }
        ToolAdvertisement::Lean(ToolBudget {
            max_tokens: steering.max_tokens,
            mcp_max_tokens: steering.tools.mcp_max_tokens,
        })
    }

    /// What an installed plugin may put in front of the model this session,
    /// or `None` when the plane is off.
    ///
    /// The same number [`Self::tool_advertisement`] hands the tool arm: the
    /// whole volatile allowance, `context.steering.max_tokens`. A plugin's
    /// `before_turn` text is volatile context like a record or a recalled
    /// frame, so it spends the allowance they spend rather than one of its
    /// own — a second key would be a second budget nobody could weigh against
    /// the first, which is the defect the plane exists to end.
    ///
    /// `None` is the master switch, read exactly as the tool arm reads it: with
    /// the plane off nothing may be withheld, or turning steering off would
    /// take a plugin's contribution away instead of a ranking.
    ///
    /// It does **not** ask `tools.lean`. That lever is about the tool array,
    /// and a workspace that sends every schema has said nothing about what a
    /// third-party plugin may spend.
    pub fn plugin_context_budget(&self) -> Option<u64> {
        if !self.steering_enabled() {
            return None;
        }
        Some(
            self.context
                .as_ref()
                .map(|context| context.steering.clone())
                .unwrap_or_default()
                .max_tokens,
        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use stella_core::steering::tools::ToolAdvertisement;

    fn from_json(json: &str) -> Settings {
        serde_json::from_str(json).expect("settings")
    }

    /// The shipped default sends every tool. A workspace that says nothing
    /// about the lever must behave as it did before the lever existed.
    #[test]
    fn a_workspace_that_says_nothing_sends_every_tool() {
        assert_eq!(
            from_json("{}").tool_advertisement(),
            ToolAdvertisement::Full
        );
    }

    /// **Witness.** Turning the lever on hands back a budget. The budget is
    /// the one the settings name. That is the whole volatile allowance from
    /// `context.steering`, plus the server share from the block under it.
    #[test]
    fn the_lever_carries_the_budget_the_settings_name() {
        let settings = from_json(
            r#"{"context":{"steering":{"max_tokens":900,
               "tools":{"lean":true,"mcp_max_tokens":100}}}}"#,
        );
        match settings.tool_advertisement() {
            ToolAdvertisement::Lean(budget) => {
                assert_eq!(budget.max_tokens, 900);
                assert_eq!(budget.mcp_max_tokens, 100);
            }
            other => panic!("the lever is on, so a budget is owed: {other:?}"),
        }
    }

    /// **Witness.** A plugin's contribution is held to the allowance the
    /// settings name — the same one the block and the tool array spend.
    #[test]
    fn a_plugin_is_held_to_the_allowance_the_settings_name() {
        let settings = from_json(r#"{"context":{"steering":{"max_tokens":900}}}"#);
        assert_eq!(settings.plugin_context_budget(), Some(900));
    }

    /// The master switch settles it here too. With the plane off nothing may
    /// be withheld, so a plugin's contribution is not budgeted at all.
    #[test]
    fn the_plane_being_off_budgets_no_plugin_contribution() {
        let settings = from_json(r#"{"context":{"steering":{"enabled":false}}}"#);
        assert_eq!(settings.plugin_context_budget(), None);
    }

    /// The tool lever is about the tool array. A workspace that sends every
    /// schema has said nothing about what a plugin may spend.
    #[test]
    fn the_tool_lever_does_not_decide_what_a_plugin_may_spend() {
        let settings =
            from_json(r#"{"context":{"steering":{"max_tokens":700,"tools":{"lean":false}}}}"#);
        assert_eq!(settings.tool_advertisement(), ToolAdvertisement::Full);
        assert_eq!(settings.plugin_context_budget(), Some(700));
    }

    /// The master switch settles it. With the plane off, no tool may be held
    /// back, or turning steering off would take a tool away rather than a
    /// hint.
    #[test]
    fn the_plane_being_off_sends_every_tool_even_with_the_lever_on() {
        let settings =
            from_json(r#"{"context":{"steering":{"enabled":false,"tools":{"lean":true}}}}"#);
        assert_eq!(settings.tool_advertisement(), ToolAdvertisement::Full);
    }
}
