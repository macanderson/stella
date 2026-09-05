// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Tool schemas as a steering-plane source.
//!
//! # Why this is here
//!
//! The plane counts every char of the record channel. It never counted the
//! biggest block of all. Each model call sends the whole tool schema list.
//! That list runs to six or nine thousand tokens. `Tool` was an arm of
//! [`SteeringSource`] with a rank and no producer. This module is the
//! producer.
//!
//! # What sets the order
//!
//! A built-in beats a tool from an MCP server. Built-ins are the working set
//! the prompt is written for. A server may add any number of tools. Inside a
//! group the cheaper schema goes first. A fixed budget then buys as many
//! tools as it can.
//!
//! Neither rule claims to know what helps this turn. A rank read off the
//! prompt would move the list from turn to turn. The list sits ahead of the
//! prompt in every cache. Moving it makes the model pay for the whole chat
//! again. This order reads the session tool set and the budget, and nothing
//! else. So the list holds still while those two do.
//!
//! # A hidden tool is not a lost tool
//!
//! Nothing here blocks a call. A tool the budget could not hold is left out
//! of the list. It still runs when the model names it. The layer that uses
//! this is `tool_lean` in `stella-cli`. It trims `schemas()` and nothing
//! else. A tight budget saves tokens. It can never wedge a turn.

use std::collections::BTreeSet;

use stella_protocol::ToolSchema;

use super::{SteeringCandidate, SteeringSet, SteeringSource, pack_to_budget};

/// The prefix every MCP server's tools carry (`mcp__<server>__…`). A schema
/// names no origin, so the prefix is all this layer has to tell a server's
/// tool from a built-in.
const MCP_NAMESPACE: &str = "mcp__";

/// What one session may spend on tool schemas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolBudget {
    /// The whole budget, over every schema sent.
    pub max_tokens: u64,
    /// The share of it MCP servers may take between them.
    ///
    /// A second cap, not trust in the order above. The order says who is cut
    /// first once the budget runs out. This says how much one source may
    /// hold when there is still room. Three servers and forty tools is the
    /// case it is for.
    pub mcp_max_tokens: u64,
}

/// How much of the tool set a session sends.
///
/// Two named states, not an `Option<ToolBudget>`. "Send them all" is what we
/// ship. A reader of a call site should see that picked, not read it out of
/// a missing value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolAdvertisement {
    /// Every schema the stack has, in the order it had them.
    #[default]
    Full,
    /// As many as the budget holds, ranked by this module.
    Lean(ToolBudget),
}

/// What one schema costs. It counts the three parts a model is sent: the
/// name, the text, and the input schema as JSON.
///
/// The `unwrap_or_default` here hides no error. A [`serde_json::Value`] has
/// no shape that can fail to turn into JSON, since its keys are strings. So
/// that arm is dead. If it ever ran, the schema would cost zero and be sent,
/// which is the safe way to be wrong.
pub fn schema_tokens(schema: &ToolSchema) -> u64 {
    let input = serde_json::to_string(&schema.input_schema).unwrap_or_default();
    stella_protocol::estimate_tokens(&schema.name)
        + stella_protocol::estimate_tokens(&schema.description)
        + stella_protocol::estimate_tokens(&input)
}

/// Is this the name of an MCP server's tool?
fn is_mcp(name: &str) -> bool {
    name.starts_with(MCP_NAMESPACE)
}

/// One candidate per schema, with its cost and its rank.
///
/// A `score` means something only inside one source, as
/// [`SteeringCandidate::score`] says. This one holds the two rules above. A
/// built-in gets `1.0` plus how cheap it is. A server tool gets how cheap it
/// is alone. Cheapness sits in `(0, 1]`. So a built-in always beats a server
/// tool, and inside a group the cheap schema sorts first.
pub fn tool_candidates(schemas: &[ToolSchema]) -> Vec<SteeringCandidate> {
    schemas
        .iter()
        .map(|schema| {
            let est_tokens = schema_tokens(schema);
            let cheapness = 1.0 / (1.0 + est_tokens as f64);
            let mcp = is_mcp(&schema.name);
            SteeringCandidate {
                source: SteeringSource::Tool,
                handle: schema.name.clone(),
                score: if mcp { cheapness } else { 1.0 + cheapness },
                why: if mcp {
                    format!(
                        "{est_tokens} tokens of schema from an MCP server — server tools are \
                         advertised after every built-in"
                    )
                } else {
                    format!(
                        "{est_tokens} tokens of schema — the allowance buys the cheapest \
                         schemas first"
                    )
                },
                est_tokens,
            }
        })
        .collect()
}

/// The schemas a session sends, and the plane's record of the choice.
#[derive(Debug, Clone, PartialEq)]
pub struct AdvertisedTools {
    /// The schemas that fit, **in the order they came in**. Cutting a tool
    /// must not move the rest. The list is cache prefix, so a new order
    /// costs a cold write and buys nothing.
    pub schemas: Vec<ToolSchema>,
    /// What was kept and what was cut, for the drop report.
    pub steering: SteeringSet,
}

/// Fit `schemas` into `budget`.
///
/// Two packs, because the two caps are different things. The MCP pack is a
/// rule on one source, like `skills.max_skills` above the shared budget. It
/// runs first, so a server's spill is named against the cap that cut it.
/// What lives through it then goes into one [`pack_to_budget`] call with
/// the rest. That call is the plane's one budgeter, which is how a tool and
/// a rule end up costed in the same unit.
pub fn advertise(schemas: Vec<ToolSchema>, budget: ToolBudget) -> AdvertisedTools {
    let (mcp, native): (Vec<_>, Vec<_>) = tool_candidates(&schemas)
        .into_iter()
        .partition(|candidate| is_mcp(&candidate.handle));

    let namespaced = pack_to_budget(mcp, budget.mcp_max_tokens);
    let mut competing = native;
    competing.extend(namespaced.selected);

    let mut steering = pack_to_budget(competing, budget.max_tokens);
    steering.dropped.extend(namespaced.dropped);

    let kept: BTreeSet<&str> = steering
        .selected
        .iter()
        .map(|candidate| candidate.handle.as_str())
        .collect();
    let schemas = schemas
        .into_iter()
        .filter(|schema| kept.contains(schema.name.as_str()))
        .collect();
    AdvertisedTools { schemas, steering }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A schema whose advertised text is padded to roughly `bytes`, so a test
    /// can say what a tool costs instead of guessing.
    fn schema(name: &str, bytes: usize) -> ToolSchema {
        ToolSchema {
            name: name.to_string(),
            description: "x".repeat(bytes),
            input_schema: serde_json::json!({}),
            read_only: true,
            speculation_safe: false,
        }
    }

    /// Forty tools, twenty of them from one MCP server.
    fn forty() -> Vec<ToolSchema> {
        (0..40)
            .map(|i| {
                if i % 2 == 0 {
                    schema(&format!("builtin_{i:02}"), 400)
                } else {
                    schema(&format!("mcp__server__tool_{i:02}"), 400)
                }
            })
            .collect()
    }

    fn cost(schemas: &[ToolSchema]) -> u64 {
        schemas.iter().map(schema_tokens).sum()
    }

    /// **The witness.** Forty schemas against an allowance that cannot hold
    /// them: what is advertised fits, what does not is named in the ledger,
    /// and every tool is accounted for in one place or the other.
    #[test]
    fn a_tool_the_allowance_cannot_afford_is_withheld_and_named() {
        let all = forty();
        let full = cost(&all);
        let budget = ToolBudget {
            max_tokens: full / 4,
            mcp_max_tokens: full,
        };

        let advertised = advertise(all.clone(), budget);

        assert!(
            cost(&advertised.schemas) <= budget.max_tokens,
            "the advertised schemas fit the allowance: {} tokens against {}",
            cost(&advertised.schemas),
            budget.max_tokens
        );
        assert!(
            advertised.schemas.len() < all.len(),
            "and the allowance really did bind: {} of {} advertised",
            advertised.schemas.len(),
            all.len()
        );
        assert_eq!(
            advertised.steering.selected.len() + advertised.steering.dropped.len(),
            all.len(),
            "every tool is either selected or dropped, never lost: {:?}",
            advertised.steering.by_source()
        );
        assert!(
            advertised
                .steering
                .dropped
                .iter()
                .all(|drop| drop.source == SteeringSource::Tool),
            "the drops arrive on the plane as tool drops"
        );
    }

    /// The advertised schemas keep the order they arrived in. The tools array
    /// is prompt-cache prefix, so a filter that also reordered would pay for a
    /// cold write it had no need of.
    #[test]
    fn withholding_a_tool_does_not_reorder_the_rest() {
        let all = forty();
        let full = cost(&all);
        let advertised = advertise(
            all.clone(),
            ToolBudget {
                max_tokens: full / 3,
                mcp_max_tokens: full,
            },
        );

        let kept: Vec<&str> = advertised
            .schemas
            .iter()
            .map(|schema| schema.name.as_str())
            .collect();
        let expected: Vec<&str> = all
            .iter()
            .map(|schema| schema.name.as_str())
            .filter(|name| kept.contains(name))
            .collect();
        assert_eq!(kept, expected, "the survivors are in arrival order");
    }

    /// An allowance that covers everything withholds nothing, which is what
    /// makes a wide budget indistinguishable from no budget at all.
    #[test]
    fn an_allowance_that_covers_everything_withholds_nothing() {
        let all = forty();
        let full = cost(&all);
        let advertised = advertise(
            all.clone(),
            ToolBudget {
                max_tokens: full,
                mcp_max_tokens: full,
            },
        );

        assert_eq!(advertised.schemas, all);
        assert!(advertised.steering.dropped.is_empty());
    }

    /// A built-in outranks every MCP tool, so a chatty server is cut before
    /// the working surface is.
    #[test]
    fn a_server_tool_is_cut_before_a_builtin_is() {
        let all = forty();
        let builtins: u64 = all
            .iter()
            .filter(|schema| !schema.name.starts_with(MCP_NAMESPACE))
            .map(schema_tokens)
            .sum();
        // Room for every built-in and one token over.
        let budget = ToolBudget {
            max_tokens: builtins + 1,
            mcp_max_tokens: u64::MAX,
        };

        let advertised = advertise(all, budget);

        let names: Vec<&str> = advertised
            .schemas
            .iter()
            .map(|schema| schema.name.as_str())
            .collect();
        assert_eq!(names.len(), 20, "every built-in survived: {names:?}");
        assert!(
            names.iter().all(|name| !name.starts_with(MCP_NAMESPACE)),
            "and no server tool did: {names:?}"
        );
    }

    /// The MCP ceiling binds on its own, with the session allowance wide open
    /// — which is the case it exists for: room to spare, and one producer
    /// still held to its share.
    #[test]
    fn the_mcp_ceiling_binds_with_the_allowance_wide_open() {
        let all = forty();
        let one_server_tool = schema_tokens(&schema("mcp__server__tool_01", 400));
        let budget = ToolBudget {
            max_tokens: u64::MAX,
            mcp_max_tokens: one_server_tool * 3,
        };

        let advertised = advertise(all, budget);

        let server: Vec<&str> = advertised
            .schemas
            .iter()
            .map(|schema| schema.name.as_str())
            .filter(|name| name.starts_with(MCP_NAMESPACE))
            .collect();
        assert_eq!(server.len(), 3, "the server keeps its share: {server:?}");
        assert_eq!(
            advertised.schemas.len(),
            23,
            "and every built-in is still advertised"
        );
    }

    /// The cost a candidate carries is measured over the three fields a
    /// provider is sent, so the ledger and the wire cannot drift.
    #[test]
    fn a_candidate_costs_exactly_its_advertised_schema() {
        let one = schema("read_file", 120);
        let candidates = tool_candidates(std::slice::from_ref(&one));

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].source, SteeringSource::Tool);
        assert_eq!(candidates[0].handle, "read_file");
        assert_eq!(candidates[0].est_tokens, schema_tokens(&one));
        assert!(
            candidates[0].why.contains(&schema_tokens(&one).to_string()),
            "the why names what it costs: {}",
            candidates[0].why
        );
    }

    /// Two runs over one tool set advertise the same array. The tools array
    /// sits ahead of the conversation in every provider's cache prefix, so an
    /// ordering that moved between turns would re-bill the whole transcript.
    #[test]
    fn the_same_tool_set_advertises_the_same_array_twice() {
        let all = forty();
        let full = cost(&all);
        let budget = ToolBudget {
            max_tokens: full / 2,
            mcp_max_tokens: full,
        };

        assert_eq!(
            advertise(all.clone(), budget).schemas,
            advertise(all, budget).schemas
        );
    }
}
