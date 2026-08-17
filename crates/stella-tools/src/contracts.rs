//! Every tool's [`ToolContract`], resolved by name (#2716).
//!
//! This module is one function and one rule, and the rule is the trust
//! boundary: **a name in the canonical catalog gets a reviewed contract; every
//! other name gets an untrusted one.** There is no third case and no
//! configuration that adds one.
//!
//! # Why name lookup is sound
//!
//! Resolving trust by name is only safe because a name cannot be squatted.
//! [`crate::custom::RESERVED_NAMES`] is aliased directly to
//! [`crate::catalog::ALL_NAMES`], so a `.stella/tools/*.toml` manifest cannot
//! register `read_file`, and MCP tools are namespaced `mcp__<server>__<tool>`
//! and so can never collide with a bare catalog name. Were either of those to
//! stop holding, a third party could inherit a built-in's reviewed grade by
//! choosing its name — which is why the test
//! `a_third_party_cannot_inherit_a_builtins_contract_by_name` asserts the
//! aliasing rather than assuming it.
//!
//! # What "untrusted" costs a tool
//!
//! [`ToolContract::declared`] grades an unreviewed tool
//! [`stella_protocol::RiskLevel::High`] and marks it
//! [`stella_protocol::Provenance::Declared`], which has two
//! consequences that need no rule written about the specific tool: a
//! risk-ceiling gate below `High` refuses it, and its `read_only` claim buys
//! it nothing (`ToolContract::trusted_read_only`). That is deliberate — the
//! honest grade for code nobody reviewed is "unknown", and a ceiling is how
//! you say "unknown" in a way a policy can act on.

use stella_protocol::{ToolContract, ToolSchema};

use crate::catalog;

/// The contract for an advertised tool.
///
/// A catalog name resolves to its reviewed row (risk included); anything else
/// — an MCP server's tool, a customer's manifest tool, a name this build has
/// never heard of — resolves to an untrusted contract.
#[must_use]
pub fn contract_for(schema: &ToolSchema) -> ToolContract {
    match catalog::get(&schema.name) {
        Some(entry) => {
            let mut contract = ToolContract::builtin(schema.clone(), entry.risk);
            if let Some(output_schema) = declared_output_schema(&schema.name) {
                contract = contract.with_output_schema(output_schema);
            }
            contract.with_events(declared_events(&schema.name))
        }
        None => ToolContract::declared(schema.clone()),
    }
}

/// The output promise a reviewed tool makes, when it makes one (#3285): what
/// a successful call's structured half (`ToolOutput::Ok.data`) must look
/// like, in the same JSON-Schema subset `registry/validate.rs` enforces for
/// inputs. `None` — most rows — means the tool's output is prose only, and
/// the registry checks nothing.
///
/// Beside the catalog rather than a macro column because almost every row
/// would be `None`: a promise is the exception, and each one should read as
/// a deliberate declaration next to the tool that keeps it.
/// The event names a reviewed tool may emit mid-execution (#3284). The
/// registry's per-call `ToolCtx` drops anything not listed here, so this
/// table is the allowlist. Empty — most rows — means the tool emits nothing.
///
/// `task` is the one long-running built-in: it announces the child it
/// dispatched as `tool.call.progress`, so an observer can tell a slow
/// delegation from a wedged one.
fn declared_events(name: &str) -> Vec<String> {
    match name {
        "task" => vec!["tool.call.progress".to_string()],
        _ => Vec::new(),
    }
}

fn declared_output_schema(name: &str) -> Option<serde_json::Value> {
    match name {
        "list_state" => Some(serde_json::json!({
            "type": "object",
            "properties": {
                "entries": {
                    "type": "array",
                    "items": { "type": "object" }
                }
            },
            "required": ["entries"]
        })),
        _ => None,
    }
}

/// The contract for a name whose schema is not (or no longer) advertised.
///
/// The fail-closed path: a tool that appears mid-session, a name the model
/// invented, a server that reconnected with a tool the cached view predates.
/// All of them resolve to an untrusted `High` contract rather than skipping
/// authorization, because "I have never heard of this" is the one case where
/// guessing generously is indefensible.
#[must_use]
pub fn unknown_contract(name: &str) -> ToolContract {
    ToolContract::declared(ToolSchema {
        name: name.to_string(),
        description: String::new(),
        input_schema: serde_json::json!({}),
        read_only: false,
        speculation_safe: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::{Provenance, RiskLevel};

    fn schema(name: &str) -> ToolSchema {
        ToolSchema {
            name: name.into(),
            description: "d".into(),
            input_schema: serde_json::json!({}),
            read_only: true,
            speculation_safe: false,
        }
    }

    /// The invariant test the catalog has always had, extended to the new
    /// column: every reviewed row must produce a self-consistent contract.
    /// This is what stops a future tool being added as `read_only` and
    /// `Destructive` at the same time.
    #[test]
    fn every_catalog_row_produces_a_valid_contract() {
        for entry in catalog::CATALOG {
            let contract = ToolContract::builtin(
                ToolSchema {
                    name: entry.name.into(),
                    description: "d".into(),
                    input_schema: serde_json::json!({}),
                    read_only: entry.read_only,
                    speculation_safe: entry.speculation_safe,
                },
                entry.risk,
            );
            assert_eq!(
                contract.validate(),
                Ok(()),
                "`{}` declares an inconsistent contract",
                entry.name
            );
        }
    }

    /// Anti-vacuity for the row above: the column carries information rather
    /// than one repeated value, so a ceiling can actually discriminate.
    ///
    /// This asserts the *shape* of the grading rather than a fixed set of
    /// grades, because the surface changes and a test pinned to "these exact
    /// four names are `Low`" is a changelog. What must hold is that the
    /// grades separate the three things an operator writes a ceiling against:
    ///
    /// - something that only observes is `Low` (`read_file`, `search`, the
    ///   board reads);
    /// - something that writes the workspace is above it but bounded and
    ///   locally undoable (`write_file`, `edit_file` at `Medium`);
    /// - something the agent cannot undo, or that runs unbounded code, sits
    ///   at the top (`delete_file`, `bash`, `task`).
    ///
    /// The one previously-enforced claim that must NOT survive unchanged is
    /// "`task` outranks everything": it did while delegation was the only
    /// built-in that reached the world, and restoring the shell and the file
    /// writers is precisely the change that ends it.
    ///
    /// This test replaces `the_catalog_grades_delegation_above_the_rest`,
    /// which asserted exactly that now-false claim.
    #[test]
    fn the_catalog_grades_separate_observation_from_mutation_from_the_unbounded() {
        let grades: std::collections::BTreeSet<RiskLevel> =
            catalog::CATALOG.iter().map(|entry| entry.risk).collect();
        assert!(
            grades.len() > 1,
            "one grade for every tool makes the column decorative"
        );

        let risk = |name: &str| {
            catalog::get(name)
                .unwrap_or_else(|| panic!("`{name}` is declared"))
                .risk
        };

        // Every read-only row observes, and nothing that observes is graded
        // above a workspace write.
        for entry in catalog::CATALOG {
            if entry.read_only {
                assert!(
                    entry.risk <= RiskLevel::Medium,
                    "`{}` only observes but is graded {:?}",
                    entry.name,
                    entry.risk
                );
            }
        }

        assert!(
            risk("read_file") < risk("write_file"),
            "reading a file must grade below writing one"
        );
        assert!(
            risk("write_file") < risk("delete_file"),
            "an overwrite is recoverable from the turn's diff; an unlink is not"
        );
        assert_eq!(
            risk("delete_file"),
            RiskLevel::Destructive,
            "an unlinked file is the one built-in effect the agent cannot undo"
        );
        assert_eq!(
            risk("bash"),
            RiskLevel::High,
            "a shell runs code nobody bounded"
        );
        assert_eq!(
            risk("task"),
            RiskLevel::High,
            "delegation spends real money and hands a child a whole tool surface"
        );
    }

    #[test]
    fn a_builtin_resolves_to_its_reviewed_row() {
        let contract = contract_for(&schema("get_state"));
        assert_eq!(contract.provenance, Provenance::Builtin);
        assert_eq!(contract.risk, RiskLevel::Low);
        assert!(contract.trusted_read_only());
    }

    #[test]
    fn an_mcp_tool_resolves_to_an_untrusted_contract() {
        let contract = contract_for(&schema("mcp__vendor__anything"));
        assert_eq!(contract.provenance, Provenance::Declared);
        assert_eq!(contract.risk, RiskLevel::High);
        assert!(
            !contract.trusted_read_only(),
            "its read_only claim must buy it nothing"
        );
    }

    #[test]
    fn an_unheard_of_name_is_untrusted_rather_than_unchecked() {
        let contract = unknown_contract("something_new");
        assert_eq!(contract.provenance, Provenance::Declared);
        assert_eq!(contract.risk, RiskLevel::High);
    }

    /// The assumption `contract_for` rests on, asserted instead of assumed:
    /// if a manifest could claim a catalog name, it would inherit that
    /// built-in's reviewed grade and its trusted read-only bit.
    #[test]
    fn a_third_party_cannot_inherit_a_builtins_contract_by_name() {
        for name in catalog::ALL_NAMES {
            assert!(
                crate::custom::RESERVED_NAMES.contains(name),
                "`{name}` is a catalog name a custom manifest could squat"
            );
        }
    }
}
