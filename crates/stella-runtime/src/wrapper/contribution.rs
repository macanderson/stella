//! What a plugin's text costs, and how much of it a turn can afford.
//!
//! `stella_core::steering::plugins` ranks and packs. This measures. The split
//! is the one every steering source keeps. The engine holds no I/O and no
//! rendering. So the number comes from the layer that holds the messages
//! (AGENTS.md #2).
//!
//! # It measures the text that reaches the model
//!
//! Each admitted `before_turn` answer becomes user messages. The cost is the
//! sum over their bodies. So the ledger and the prompt cannot drift. What is
//! charged is what is sent.
//!
//! # A refusal here cuts bytes, never a dispatch
//!
//! Text the allowance cannot hold is left out of the prompt and named in the
//! drop report. The plugin still ran. Its scope and witness lists still stand.
//! What it published still reaches the next stage. This is a budget on what
//! goes in front of the model, and on nothing else.

use stella_core::steering::SteeringSet;
use stella_core::steering::plugins::{ContextAllowance, PluginContribution, contribute};
use stella_plugin::StageName;
use stella_protocol::completion::CompletionMessage;

/// One member's answer at one stage: what it said, and what saying it costs.
pub(crate) struct StageContribution {
    contribution: PluginContribution,
    messages: Vec<CompletionMessage>,
}

impl StageContribution {
    /// Price `messages` as what `plugin` said at `stage`.
    ///
    /// `None` for an answer with no message. A plugin that put nothing in
    /// front of the model is not a row that costs zero. It is not a row.
    pub(crate) fn measured(
        plugin: &str,
        stage: &StageName,
        messages: Vec<CompletionMessage>,
    ) -> Option<Self> {
        if messages.is_empty() {
            return None;
        }
        let est_tokens = messages
            .iter()
            .map(|message| stella_protocol::estimate_tokens(&message.content))
            .sum();
        Some(Self {
            contribution: PluginContribution {
                plugin: plugin.to_string(),
                stage: stage.to_string(),
                est_tokens,
            },
            messages,
        })
    }
}

/// Fit a round's text into what the turn can still afford, and charge the
/// allowance for what got through.
///
/// `None` is a host that named no ceiling. Everything is kept, nothing is cut,
/// and the plane still reports what each plugin said. Unmeasured is not the
/// same as unbounded. The record is what tells the two apart.
///
/// What lives keeps arrival order, which is the stage order the members agreed
/// on. The pack's own order decides only who is cut.
pub(crate) fn afford(
    gathered: Vec<StageContribution>,
    allowance: Option<&ContextAllowance>,
) -> (Vec<CompletionMessage>, SteeringSet) {
    let budget = allowance.map_or(u64::MAX, ContextAllowance::remaining);
    let decided = contribute(
        gathered
            .iter()
            .map(|stage| stage.contribution.clone())
            .collect(),
        budget,
    );
    let kept: Vec<CompletionMessage> = gathered
        .into_iter()
        .filter(|stage| decided.kept.contains(&stage.contribution))
        .flat_map(|stage| stage.messages)
        .collect();
    if let Some(allowance) = allowance {
        allowance.spend(decided.steering.est_tokens());
    }
    (kept, decided.steering)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use stella_core::steering::SteeringSource;
    use stella_core::steering::ledger::SteeringLedger;

    use super::*;

    fn said(plugin: &str, word: &str, text: &str) -> StageContribution {
        StageContribution::measured(
            plugin,
            &StageName::new(word),
            vec![CompletionMessage::user(text)],
        )
        .expect("a message is a contribution")
    }

    /// A stage that said nothing is not a row on the plane.
    #[test]
    fn an_answer_with_no_message_is_no_contribution() {
        assert!(
            StageContribution::measured("quiet", &StageName::new("plan"), Vec::new()).is_none()
        );
    }

    /// **The witness.** Text over what the turn has left is kept out of the
    /// prompt and named in the drop report.
    #[test]
    fn a_contribution_over_the_allowance_is_withheld_and_named() {
        let ledger = Arc::new(SteeringLedger::new());
        let allowance = ContextAllowance::new(4, Arc::clone(&ledger));

        let (messages, steering) = afford(
            vec![said("chatty", "research", &"word ".repeat(200))],
            Some(&allowance),
        );

        assert!(messages.is_empty(), "nothing reached the prompt");
        assert_eq!(steering.dropped.len(), 1);
        assert_eq!(steering.dropped[0].handle, "chatty/research");
        assert_eq!(steering.dropped[0].source, SteeringSource::Plugin);
        assert_eq!(ledger.spent(), 0, "and nothing was charged for it");
    }

    /// Text the allowance holds reaches the prompt. It is charged to the one
    /// cell the block and the tool list meet in.
    #[test]
    fn an_affordable_contribution_reaches_the_prompt_and_is_charged() {
        let ledger = Arc::new(SteeringLedger::new());
        let allowance = ContextAllowance::new(10_000, Arc::clone(&ledger));

        let (messages, steering) = afford(
            vec![said("vera", "witness", "run the test")],
            Some(&allowance),
        );

        assert_eq!(messages.len(), 1);
        assert_eq!(steering.selected.len(), 1);
        assert_eq!(steering.selected[0].handle, "vera/witness");
        assert_eq!(ledger.spent(), steering.est_tokens());
        assert!(ledger.spent() > 0, "the text cost something");
    }

    /// No allowance keeps it all and still reports it. An unmeasured ceiling
    /// is not an unbounded one, and the record is what tells them apart.
    #[test]
    fn no_allowance_keeps_everything_and_still_reports_it() {
        let (messages, steering) = afford(
            vec![
                said("a", "research", "one"),
                said("b", "plan", &"word ".repeat(400)),
            ],
            None,
        );

        assert_eq!(messages.len(), 2);
        assert_eq!(steering.selected.len(), 2);
        assert!(steering.dropped.is_empty());
    }

    /// What lives keeps arrival order, which is the stage order the members
    /// agreed on. Prompt bytes may not turn on what a stage cost.
    #[test]
    fn the_survivors_keep_the_stage_order() {
        let ledger = Arc::new(SteeringLedger::new());
        let allowance = ContextAllowance::new(40, Arc::clone(&ledger));

        let (messages, _) = afford(
            vec![
                said("first", "research", "aaaa bbbb cccc"),
                said("second", "plan", "dddd"),
            ],
            Some(&allowance),
        );

        let text: Vec<&str> = messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(text, vec!["aaaa bbbb cccc", "dddd"]);
    }
}
