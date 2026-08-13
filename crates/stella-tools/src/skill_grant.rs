// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A skill's `allowed-tools` grant as policy algebra (#2682).
//!
//! A skill that declares `allowed-tools: read_file, grep` runs against a
//! **scoped narrowing layer**: the effective surface is the grant
//! intersected with whatever the operator (and any org-managed scope above
//! them) already allows — [`ToolPolicy::narrow_with`]'s semantics, "allowed
//! only if both allow", so a grant can select within the operator surface
//! but can never re-enable a tool the operator denied. That is the
//! union-of-denials rule `deny_all_from` enforces between settings scopes,
//! restated for skills.
//!
//! The grant itself is the read-only idiom: `{"*": off, <name>: on, …}`.
//! Entries may be exact tool names or catalog groups (`"file"`, `"mcp"`),
//! because [`ToolPolicy::allows`] resolves both — a skill author writes the
//! same vocabulary an operator writes in `settings.json`.
//!
//! # The intersection is computed per name, never folded into one policy
//!
//! Enforcement stacks the two answers per concrete tool name
//! ([`effective_allows`]): the operator's `PolicyToolSet` sits below the
//! invoke layer and the grant is consulted above it, so a call proceeds only
//! when **both** allow it — the exact intersection, structurally. A folded
//! single-policy form (`operator.narrow_with(&grant_policy(...))`) is
//! deliberately not offered: `narrow_with` resolves raw *keys*, and a group
//! key like `"build"` resolves through `catalog::group_for` as an unknown
//! tool name into the dynamic `"custom"` group, which can make the folded
//! policy *wider* than the per-name intersection (#2682 review finding; the
//! property tests below pin the per-name form instead).
//!
//! Enforcement sites: the CLI's `invoke_skill` layer holds the grant for an
//! inline skill's span, and a forked skill's grant is resolved to concrete
//! names ([`resolve_grant`]) and enforced structurally by
//! `stella_core::ports::GrantedTools` inside the child turn.

use crate::policy::{ToolPolicy, WILDCARD};

/// The grant as a [`ToolPolicy`]: everything off except the named tools and
/// groups. An empty grant list is everything-off — a skill that declares
/// `allowed-tools:` with names gets exactly those; declaring none at all is
/// the caller's `None` and never reaches here.
#[must_use]
pub fn grant_policy(allowed: &[String]) -> ToolPolicy {
    let mut switches: Vec<(String, bool)> = vec![(WILDCARD.to_string(), false)];
    switches.extend(allowed.iter().map(|name| (name.clone(), true)));
    ToolPolicy::from_switches(switches)
}

/// The effective answer for one tool while the grant is active:
/// `operator ∧ grant`, evaluated per concrete name.
///
/// This is the shape enforcement actually takes — the operator's
/// `PolicyToolSet` below, the grant consulted above, both must say yes —
/// which makes the "cannot re-enable" guarantee structural: a tool the
/// operator denied stays denied whatever the skill asks for. See the module
/// docs for why this is a per-name conjunction rather than a folded policy.
#[must_use]
pub fn effective_allows(operator: &ToolPolicy, allowed: &[String], name: &str) -> bool {
    operator.allows(name) && grant_policy(allowed).allows(name)
}

/// Resolve a grant against an advertised surface: the concrete tool names
/// (in the surface's order) the grant permits. This is what a forked
/// skill's child turn is scoped to — group entries expand to whichever of
/// their tools the surface actually advertises, and a granted name nothing
/// advertises resolves to nothing rather than to a phantom capability.
#[must_use]
pub fn resolve_grant(allowed: &[String], advertised: &[String]) -> Vec<String> {
    let grant = grant_policy(allowed);
    advertised
        .iter()
        .filter(|name| grant.allows(name))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The tool-name universe the properties quantify over: real catalog
    /// names (so group resolution is exercised for real) plus an MCP and a
    /// custom name the catalog has never heard of.
    fn universe() -> Vec<&'static str> {
        let mut names: Vec<&'static str> = crate::catalog::ALL_NAMES.to_vec();
        names.push("mcp__github__create_issue");
        names.push("deploy_to_staging");
        names
    }

    fn any_key() -> impl Strategy<Value = String> {
        let mut keys: Vec<String> = universe().iter().map(|s| s.to_string()).collect();
        keys.extend(crate::catalog::groups().iter().map(|g| g.to_string()));
        keys.push(WILDCARD.to_string());
        proptest::sample::select(keys)
    }

    fn any_operator() -> impl Strategy<Value = ToolPolicy> {
        proptest::collection::vec((any_key(), any::<bool>()), 0..6)
            .prop_map(ToolPolicy::from_switches)
    }

    fn any_grant() -> impl Strategy<Value = Vec<String>> {
        proptest::collection::vec(
            any_key().prop_filter("no wildcard", |k| k != WILDCARD),
            0..5,
        )
    }

    proptest! {
        /// #2682 witness (b), the load-bearing half: whatever the skill's
        /// grant names — including the very tool, its group, or a grant that
        /// spells `on` for it — a tool the operator denied stays denied.
        #[test]
        fn a_grant_can_never_re_enable_an_operator_denied_tool(
            operator in any_operator(),
            grant in any_grant(),
        ) {
            for name in universe() {
                if !operator.allows(name) {
                    prop_assert!(
                        !effective_allows(&operator, &grant, name),
                        "`{name}` was operator-denied but the grant {grant:?} re-enabled it"
                    );
                }
            }
        }

        /// The other direction of the same intersection: the effective
        /// answer is exactly `operator ∧ grant`, so the grant genuinely
        /// narrows (nothing outside it survives) and genuinely selects
        /// (everything inside it the operator allows survives).
        #[test]
        fn the_effective_surface_is_exactly_the_intersection(
            operator in any_operator(),
            grant in any_grant(),
        ) {
            let grant_only = grant_policy(&grant);
            for name in universe() {
                prop_assert_eq!(
                    effective_allows(&operator, &grant, name),
                    operator.allows(name) && grant_only.allows(name),
                    "`{}` diverged from operator ∧ grant under {:?}",
                    name,
                    &grant
                );
            }
        }

        /// Resolution against an advertised surface is the same intersection
        /// once more: a resolved name is advertised AND granted, and every
        /// advertised granted name is resolved.
        #[test]
        fn resolution_is_the_grant_applied_to_the_advertised_surface(
            grant in any_grant(),
        ) {
            let advertised: Vec<String> =
                universe().iter().map(|s| s.to_string()).collect();
            let resolved = resolve_grant(&grant, &advertised);
            let grant_only = grant_policy(&grant);
            for name in &advertised {
                prop_assert_eq!(
                    resolved.contains(name),
                    grant_only.allows(name),
                    "`{}` resolution diverged from the grant {:?}",
                    name,
                    &grant
                );
            }
        }
    }

    /// The concrete shape a skill author writes: names select tools, a group
    /// selects its family, and everything unnamed is off.
    #[test]
    fn a_grant_is_the_read_only_idiom_over_the_named_tools() {
        let grant = grant_policy(&["read_file".to_string(), "retrieval".to_string()]);
        assert!(grant.allows("read_file"));
        assert!(grant.allows("grep"), "group entries cover their family");
        assert!(grant.allows("glob"));
        assert!(!grant.allows("bash"));
        assert!(!grant.allows("write_file"));
        assert!(!grant.allows("mcp__github__create_issue"));
    }

    /// #3120: a granted *tool* name selects that tool alone — `search` no
    /// longer doubles as its family's group key, so a skill that asks for the
    /// fused entry point does not silently receive grep and glob too.
    #[test]
    fn a_granted_tool_name_does_not_expand_to_its_family() {
        let grant = grant_policy(&["search".to_string()]);
        assert!(grant.allows("search"));
        for sibling in ["grep", "glob", "graph_query", "semantic_code_search"] {
            assert!(
                !grant.allows(sibling),
                "`search` must not grant `{sibling}`"
            );
        }
    }

    /// An empty grant list denies everything — `allowed-tools` with names is
    /// exactly those names, never "everything" by accident.
    #[test]
    fn an_empty_grant_denies_everything() {
        let grant = grant_policy(&[]);
        for name in universe() {
            assert!(
                !grant.allows(name),
                "`{name}` must be off under an empty grant"
            );
        }
    }
}
