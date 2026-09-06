// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a plugin adds to the prompt, as a steering-plane source.
//!
//! # Why this is here
//!
//! [`SteeringSource::Plugin`] had a rank and a tag. It had no producer. A
//! plugin that is switched on answers `before_turn` with text. The host puts
//! that text in front of the model. Nothing priced it. Nothing ranked it.
//! Nothing said what became of it. So the one source from outside this
//! repository was the one source the shared budget could not see.
//!
//! This module is the producer. It has the shape [`super::tools`] has. The
//! choice is made here, over owned data. The measuring and the socket work
//! stay in the layer above (AGENTS.md #2).
//!
//! # The cost is measured
//!
//! A manifest could have named a token figure per stage. That number would be
//! the plugin pricing itself. A plugin that names too small a number wins
//! budget it never paid for. Then the plane prices a claim, not a cost. So
//! [`PluginContribution::est_tokens`] is measured over the exact text. The
//! host that holds the text does the measuring.
//!
//! # A plugin that says nothing costs nothing
//!
//! A stage with no text is not a candidate. Nor is it a candidate that costs
//! zero. A row for a plugin that added nothing reads as text somebody could
//! go and find.

use std::sync::Arc;

use super::ledger::SteeringLedger;
use super::{SteeringCandidate, SteeringSet, SteeringSource, pack_to_budget};

/// What one plugin said at one stage, and what it costs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginContribution {
    /// The plugin's own name. It is the word a person writes in
    /// `active_plugins`, and the word they take out again. Never the
    /// `[wrapper] id`. A drop report is read by whoever can act on it.
    pub plugin: String,
    /// The stage this text was added at.
    pub stage: String,
    /// What sending it costs, measured over the exact text.
    pub est_tokens: u64,
}

impl PluginContribution {
    /// What this row is called on the plane.
    ///
    /// The stage is always part of it. That holds even for a plugin that
    /// speaks at one stage. A handle is what a receipt joins on. So it may
    /// not mean two things on two turns.
    #[must_use]
    pub fn handle(&self) -> String {
        format!("{}/{}", self.plugin, self.stage)
    }
}

/// One candidate per row, with its cost and its rank.
///
/// A `score` means something only inside one source, as
/// [`SteeringCandidate::score`] says. Here it is cheapness. The row that costs
/// less sorts first. That is the rule [`super::tools::tool_candidates`] uses
/// inside a group. Nothing here reads the prompt. What a plugin has to say is
/// the plugin's call. A host that ranked it against the goal would be adding a
/// view nobody asked for.
#[must_use]
pub fn plugin_candidates(contributions: &[PluginContribution]) -> Vec<SteeringCandidate> {
    contributions
        .iter()
        .map(|contribution| {
            let est_tokens = contribution.est_tokens;
            SteeringCandidate {
                source: SteeringSource::Plugin,
                handle: contribution.handle(),
                score: 1.0 / (1.0 + est_tokens as f64),
                why: format!(
                    "the \"{}\" plugin added {est_tokens} tokens at the {} stage",
                    contribution.plugin, contribution.stage
                ),
                est_tokens,
            }
        })
        .collect()
}

/// What a turn may still spend on a plugin's text.
///
/// A handle on the one shared cell, not a number of its own. Records, skills
/// and recalled frames spend this allowance first. The tool list takes what is
/// left. So a plugin that adds eight thousand tokens has to be visible to
/// both. Else "the block is too big" is a question no one piece of code can
/// answer.
#[derive(Debug, Clone)]
pub struct ContextAllowance {
    /// The whole volatile allowance the session declares.
    declared: u64,
    /// What the open turn has spent of it, and what the tool list settled.
    ledger: Arc<SteeringLedger>,
}

impl ContextAllowance {
    /// An allowance of `declared` tokens, spent against `ledger`.
    #[must_use]
    pub fn new(declared: u64, ledger: Arc<SteeringLedger>) -> Self {
        Self { declared, ledger }
    }

    /// What is left for this round.
    ///
    /// The sum saturates. A block already past the allowance leaves nothing. A
    /// subtraction that wrapped would hand a plugin the whole budget at the one
    /// moment there is none.
    #[must_use]
    pub fn remaining(&self) -> u64 {
        self.declared.saturating_sub(self.ledger.spent())
    }

    /// Note what this round took.
    ///
    /// Charged after the pack. So what is noted is what reached the prompt, not
    /// what was offered. A later round of the turn then sees less left. That is
    /// right: the text piles up in one chat.
    pub fn spend(&self, tokens: u64) {
        self.ledger.spend(tokens);
    }
}

/// What the plane decided about one round.
#[derive(Debug, Clone, PartialEq)]
pub struct ContributedContext {
    /// What fit, in the order it arrived.
    ///
    /// Arrival order, not pack order. The messages go to the model in the stage
    /// order the members agreed on. A sort here would make the prompt turn on
    /// what each stage cost.
    pub kept: Vec<PluginContribution>,
    /// What was kept and what was cut, for the drop report.
    pub steering: SteeringSet,
}

/// Fit `contributions` into `budget_tokens`, and name what did not fit.
///
/// One [`pack_to_budget`] call, which is the point. A plugin's text and a
/// published record are priced in one unit by one packer. So the order between
/// them is the plane's fixed one. It is not an accident of which layer ran
/// first.
#[must_use]
pub fn contribute(
    contributions: Vec<PluginContribution>,
    budget_tokens: u64,
) -> ContributedContext {
    let steering = pack_to_budget(plugin_candidates(&contributions), budget_tokens);
    let kept = contributions
        .into_iter()
        .filter(|contribution| {
            let handle = contribution.handle();
            steering
                .selected
                .iter()
                .any(|candidate| candidate.handle == handle)
        })
        .collect();
    ContributedContext { kept, steering }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contribution(plugin: &str, stage: &str, est_tokens: u64) -> PluginContribution {
        PluginContribution {
            plugin: plugin.to_string(),
            stage: stage.to_string(),
            est_tokens,
        }
    }

    /// **The witness.** Text the allowance cannot hold is cut and named. Every
    /// row lands in one list or the other.
    #[test]
    fn a_contribution_the_allowance_cannot_afford_is_withheld_and_named() {
        let offered = vec![
            contribution("stella-research", "research", 100),
            contribution("stella-plan", "plan", 900),
        ];

        let decided = contribute(offered, 500);

        assert_eq!(decided.kept.len(), 1, "{:?}", decided.kept);
        assert_eq!(decided.kept[0].plugin, "stella-research");
        assert_eq!(decided.steering.dropped.len(), 1);
        assert_eq!(decided.steering.dropped[0].handle, "stella-plan/plan");
        assert_eq!(
            decided.steering.dropped[0].source,
            SteeringSource::Plugin,
            "the drop arrives on the plane as a plugin drop"
        );
        assert_eq!(
            decided.steering.selected.len() + decided.steering.dropped.len(),
            2,
            "every row is either selected or dropped, never lost"
        );
    }

    /// An allowance that covers it all cuts nothing. That is what makes a wide
    /// budget the same as no budget.
    #[test]
    fn an_allowance_that_covers_everything_withholds_nothing() {
        let offered = vec![
            contribution("a", "research", 100),
            contribution("b", "plan", 900),
        ];

        let decided = contribute(offered.clone(), u64::MAX);

        assert_eq!(decided.kept, offered);
        assert!(decided.steering.dropped.is_empty());
    }

    /// What lives keeps the order it arrived in. This is prompt text. A filter
    /// that also sorted would move the block between two turns that said the
    /// same thing.
    #[test]
    fn withholding_a_contribution_does_not_reorder_the_rest() {
        let offered = vec![
            contribution("expensive", "research", 400),
            contribution("cheap", "plan", 10),
            contribution("middling", "review", 100),
        ];

        let kept: Vec<String> = contribute(offered, 120)
            .kept
            .into_iter()
            .map(|contribution| contribution.plugin)
            .collect();

        assert_eq!(kept, vec!["cheap".to_string(), "middling".to_string()]);
    }

    /// A handle names the plugin and the stage. So one plugin at two stages is
    /// two rows a receipt can tell apart.
    #[test]
    fn one_plugin_at_two_stages_is_two_handles() {
        let candidates = plugin_candidates(&[
            contribution("vera", "witness", 10),
            contribution("vera", "verify", 20),
        ]);

        assert_eq!(candidates[0].handle, "vera/witness");
        assert_eq!(candidates[1].handle, "vera/verify");
        assert!(
            candidates[0].why.contains("10") && candidates[0].why.contains("vera"),
            "the why names the plugin and what it costs: {}",
            candidates[0].why
        );
    }

    /// **The witness for the shared cell.** What the block spent is gone from
    /// what a plugin may add. What the plugin adds is gone from what the tool
    /// list is settled against.
    #[test]
    fn a_contribution_spends_the_same_allowance_the_block_does() {
        let ledger = Arc::new(SteeringLedger::new());
        ledger.open_turn();
        ledger.spend(400);
        let allowance = ContextAllowance::new(1_000, Arc::clone(&ledger));

        assert_eq!(allowance.remaining(), 600);

        let decided = contribute(vec![contribution("vera", "witness", 250)], 600);
        allowance.spend(decided.steering.est_tokens());

        assert_eq!(allowance.remaining(), 350);
        assert_eq!(
            ledger.spent(),
            650,
            "the block and the plugin share one cell"
        );
    }

    /// A block past the allowance leaves nothing. It does not wrap back around
    /// to the whole budget.
    #[test]
    fn a_block_over_the_allowance_leaves_a_plugin_nothing() {
        let ledger = Arc::new(SteeringLedger::new());
        ledger.spend(u64::MAX);

        assert_eq!(
            ContextAllowance::new(1_000, ledger).remaining(),
            0,
            "the remainder saturates at nothing"
        );
    }
}
