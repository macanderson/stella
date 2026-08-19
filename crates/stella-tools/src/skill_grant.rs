// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A skill's `allowed-tools` grant as policy algebra (#2682).
//!
//! A skill that declares `allowed-tools: task_list, get_state` runs against a
//! **scoped narrowing layer**: the effective surface is the grant
//! intersected with whatever the operator (and any org-managed scope above
//! them) already allows — [`ToolPolicy::narrow_with`]'s semantics, "allowed
//! only if both allow", so a grant can select within the operator surface
//! but can never re-enable a tool the operator denied. That is the
//! union-of-denials rule `deny_all_from` enforces between settings scopes,
//! restated for skills.
//!
//! The grant itself is the read-only idiom: `{"*": off, <name>: on, …}`.
//! Entries may be exact tool names or catalog groups (`"scratch"`, `"mcp"`),
//! because [`ToolPolicy::allows`] resolves both — a skill author writes the
//! same vocabulary an operator writes in `settings.json`.
//!
//! # The intersection is computed per name
//!
//! Enforcement stacks the two answers per concrete tool name
//! ([`effective_allows`]): the operator's `PolicyToolSet` sits below the
//! invoke layer and the grant is consulted above it, so a call proceeds only
//! when **both** allow it — the exact intersection, structurally, with no
//! third object holding a copy of the answer. That is why this module offers
//! no folded single-policy form: the shape enforcement actually takes is two
//! layers, and a fold would be a second spelling of a decision the stack
//! already makes.
//!
//! It was also, until #2800, the only *correct* spelling. A folded
//! `operator.narrow_with(&grant_policy(…))` resolved raw **keys**, and a
//! group key like `"scratch"` went through `catalog::group_for` as an unknown
//! tool name into the dynamic `"custom"` group — so a grant naming `custom`
//! could keep a whole catalog family alive and make the fold *wider* than
//! this per-name intersection (#2682 review finding, filed as #2800).
//! [`ToolPolicy::narrow_with`] now expands group keys to their member names
//! and resolves the two dynamic groups at group level, and its own
//! `the_fold_is_exactly_the_per_name_intersection` property pins that the two
//! forms agree. The properties below pin this one.
//!
//! Enforcement sites: a session layer that mounts skill invocation holds the
//! grant for an inline skill's span, and a forked skill's grant is resolved
//! to concrete names ([`resolve_grant`]) and enforced structurally by
//! `stella_core::ports::GrantedTools` inside the child turn. No shipped
//! surface mounts skill invocation today, so nothing constructs a grant in
//! production.

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
        let grant = grant_policy(&["task_list".to_string(), "scratch".to_string()]);
        assert!(grant.allows("task_list"));
        assert!(
            grant.allows("save_state"),
            "group entries cover their family"
        );
        assert!(grant.allows("get_state"));
        assert!(!grant.allows("delegate"));
        assert!(!grant.allows("task_create"));
        assert!(!grant.allows("mcp__github__create_issue"));
    }

    /// #3120: a granted *tool* name selects that tool alone — a name that is
    /// not also a group key must never expand to its family, so a skill that
    /// asks for one scratch verb does not silently receive the other three.
    #[test]
    fn a_granted_tool_name_does_not_expand_to_its_family() {
        let grant = grant_policy(&["save_state".to_string()]);
        assert!(grant.allows("save_state"));
        for sibling in ["get_state", "list_state", "delete_state"] {
            assert!(
                !grant.allows(sibling),
                "`save_state` must not grant `{sibling}`"
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
