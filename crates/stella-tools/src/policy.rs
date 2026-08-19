//! Which tools an operator has switched off.
//!
//! Stella ships with **every tool on**. A tool is available unless something
//! says otherwise, and there is exactly one way to say otherwise — a
//! `"tools"` entry in `settings.json` — whether the tool is a built-in, an MCP
//! server's, or one the customer wrote themselves:
//!
//! ```json
//! "tools": {
//!   "delegate": "off",
//!   "scratch": "off",
//!   "task_assign": "off"
//! }
//! ```
//!
//! This replaces the per-capability booleans that used to be the only
//! switches. Those were not really *availability* — they were policy
//! defaults welded into the tool table, which is why every new "should this
//! be on?" question needed another enum variant, another construction field,
//! and another hand-written branch. Availability is only about prerequisites
//! the environment either satisfies or does not; whether a *satisfiable*
//! tool is allowed is this module's business.
//!
//! # Precedence
//!
//! Most specific wins, and there are only three levels:
//!
//! 1. the exact tool name — `"task_assign"`
//! 2. its group — `"scratch"`, or `"mcp"` / `"custom"` for tools the catalog
//!    does not know (see [`crate::catalog::group_for`])
//! 3. `"*"`, every tool
//!
//! So `{"*": "off", "task_list": "on"}` is a board-reader in two lines, and
//! `{"scratch": "off", "get_state": "on"}` keeps exactly one of the four
//! scratch tools. Anything unmentioned is on.
//!
//! # Composing scopes
//!
//! [`ToolPolicy::deny_all_from`] folds one scope into another by **union of
//! denials**: if any scope turns a tool off, it is off. That is what lets an
//! org-managed `settings.json` turn something off and a project be unable to
//! turn it back on, without inventing a "locked" or "pinned" syntax — turning
//! things *off* is always permitted, turning them *on* never overrides a
//! deny from higher up.

use std::collections::BTreeMap;

use crate::catalog;

/// The wildcard key matching every tool.
pub const WILDCARD: &str = "*";

/// An operator's tool switches, resolved by name at query time.
///
/// Empty means "everything on", which is the shipped default — construct that
/// with [`ToolPolicy::allow_all`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolPolicy {
    /// Keys are tool names, group names, or [`WILDCARD`]; `false` is off.
    /// A `BTreeMap` so iteration (and therefore any rendering of the policy)
    /// is deterministic.
    switches: BTreeMap<String, bool>,
}

impl ToolPolicy {
    /// The shipped posture: every tool on.
    pub fn allow_all() -> Self {
        Self::default()
    }

    /// Build from `(key, enabled)` pairs, where a key is a tool name, a group
    /// name, or [`WILDCARD`].
    pub fn from_switches(switches: impl IntoIterator<Item = (String, bool)>) -> Self {
        Self {
            switches: switches.into_iter().collect(),
        }
    }

    /// Whether `name` may register and execute.
    ///
    /// Unknown names are allowed — a policy is a deny list, and a tool the
    /// catalog has never heard of is either an MCP tool or a customer's own,
    /// both of which are on unless their group or the wildcard says otherwise.
    pub fn allows(&self, name: &str) -> bool {
        if let Some(&enabled) = self.switches.get(name) {
            return enabled;
        }
        if let Some(&enabled) = self.switches.get(catalog::group_for(name)) {
            return enabled;
        }
        self.switches.get(WILDCARD).copied().unwrap_or(true)
    }

    /// Every tool this policy turns off, by name — used to explain a posture
    /// (`stella tools`, the settings UI) rather than to enforce it. Only
    /// resolves names the catalog knows; MCP and custom tools are resolved
    /// live against the session's actual tool list.
    pub fn denied_builtins(&self) -> Vec<&'static str> {
        catalog::CATALOG
            .iter()
            .map(|entry| entry.name)
            .filter(|name| !self.allows(name))
            .collect()
    }

    /// Whether any switch is set at all — `false` for the shipped default.
    pub fn is_default(&self) -> bool {
        self.switches.is_empty()
    }

    /// The raw switches, for rendering and persistence.
    pub fn switches(&self) -> &BTreeMap<String, bool> {
        &self.switches
    }

    /// Fold another scope's denials into this one.
    ///
    /// Union of denials, never of grants: a key the other scope turns off is
    /// turned off here too, and a key it turns *on* is ignored. So a lower
    /// scope can narrow what a higher one allowed but can never widen it,
    /// which is the whole enforcement story for org-managed settings.
    pub fn deny_all_from(&mut self, other: &ToolPolicy) {
        for (key, &enabled) in &other.switches {
            if !enabled {
                self.switches.insert(key.clone(), false);
            }
        }
    }

    /// The answer for a member of `group` that no exact key names: the
    /// group's own switch, else the wildcard, else on.
    ///
    /// [`allows`](Self::allows) cannot be asked this question, because it
    /// takes a *tool* name: handed a group name it resolves through
    /// [`catalog::group_for`], which reads it as a tool the catalog has never
    /// heard of and answers for the `custom` group instead (#2800).
    fn group_default(&self, group: &str) -> bool {
        match self.switches.get(group) {
            Some(&enabled) => enabled,
            None => self.switches.get(WILDCARD).copied().unwrap_or(true),
        }
    }

    /// Narrow this policy by another scope: a tool ends up allowed only if
    /// **both** policies allow it.
    ///
    /// This is [`deny_all_from`](Self::deny_all_from)'s guarantee — a lower
    /// scope may narrow, never widen — computed over *resolved* answers
    /// instead of over raw keys, which is what a wildcard-plus-exception
    /// scope needs.
    ///
    /// `deny_all_from` folds key by key, so the read-only idiom
    /// `{"*": off, "task_list": on}` folds in as `{"*": off}` alone: the
    /// grant is dropped (correctly — grants never transfer), and `task_list`
    /// then falls through to the wildcard and is denied. The scope says
    /// "nothing except task_list" and the fold hears "nothing". Resolving
    /// each key through [`allows`](Self::allows) first keeps the exception,
    /// because `other.allows("task_list")` is `true` while
    /// `other.allows("*")` is `false`.
    ///
    /// The narrowing guarantee is unchanged and still structural: every tool
    /// name's entry is `self.allows(n) && other.allows(n)`, so a tool this
    /// policy denies stays denied whatever `other` says.
    ///
    /// # Why a key is not resolved as written
    ///
    /// A key is a tool name, a group, or the wildcard, and only the first can
    /// go through [`allows`](Self::allows). Resolving all three that way
    /// could **widen** past the per-name intersection (#2800): `allows`
    /// classifies a group key through [`catalog::group_for`], which reads
    /// `"scratch"` as an unknown *tool* and answers for the dynamic `custom`
    /// group — so a scope of `{"*": off, "custom": on}` folded onto an
    /// operator's `{"scratch": on}` used to resolve to `{"scratch": on}` and
    /// keep the whole scratch family alive, though the scope denies every one
    /// of them by name. So each key is resolved as the kind of key it is:
    ///
    /// - a **catalog group** expands to its member names
    ///   ([`catalog::names_in_group`]) and each is resolved as a name. The
    ///   group key itself is then dropped, because every member now carries a
    ///   more specific entry that outranks it, and leaving a `{"scratch": on}`
    ///   behind a family that is entirely off would misreport the posture to
    ///   [`switches`](Self::switches)' readers.
    /// - a **dynamic group** (`"mcp"`, `"custom"` — see
    ///   [`catalog::DYNAMIC_GROUPS`]) has no fixed member list, so there is
    ///   nothing to expand to: it stays a group key and is resolved at group
    ///   level on both sides. That is exact for its members, which are
    ///   precisely the names neither side keys individually.
    /// - the **wildcard**, likewise, at wildcard level.
    /// - anything else is a tool name, resolved by [`allows`](Self::allows)
    ///   as before.
    ///
    /// The result is the exact per-name intersection —
    /// `narrow_with` now agrees with [`crate::skill_grant::effective_allows`],
    /// which computes it one concrete name at a time — pinned as a property
    /// by `the_fold_is_exactly_the_per_name_intersection`.
    pub fn narrow_with(&mut self, other: &ToolPolicy) {
        let keys: std::collections::BTreeSet<String> = self
            .switches
            .keys()
            .chain(other.switches.keys())
            .cloned()
            .collect();
        let mut resolved: Vec<(String, bool)> = Vec::new();
        let mut expanded: Vec<String> = Vec::new();
        for key in keys {
            let members = catalog::names_in_group(&key);
            if !members.is_empty() {
                resolved.extend(
                    members
                        .into_iter()
                        .map(|name| (name.to_string(), self.allows(name) && other.allows(name))),
                );
                expanded.push(key);
            } else if key == WILDCARD || catalog::DYNAMIC_GROUPS.contains(&key.as_str()) {
                let allowed = self.group_default(&key) && other.group_default(&key);
                resolved.push((key, allowed));
            } else {
                let allowed = self.allows(&key) && other.allows(&key);
                resolved.push((key, allowed));
            }
        }
        // After the reads above, never during them: `allows` answers against
        // the pre-narrowing policy for every key.
        for key in expanded {
            self.switches.remove(&key);
        }
        self.switches.extend(resolved);
    }

    /// Parse a comma-separated switch spec — the CLI spelling of the
    /// `settings.json` `"tools"` table (#1263).
    ///
    /// `*:off,task_list:on,get_state:on` is a board-and-scratch reader;
    /// `task_assign:off` removes one capability. `on`/`off` are the only values, matching the
    /// settings file exactly rather than inventing a second vocabulary for
    /// the same concept. Whitespace around either side is ignored so a
    /// quoted, spaced-out spec behaves.
    pub fn parse_spec(spec: &str) -> Result<Self, String> {
        let mut switches = BTreeMap::new();
        for entry in spec.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let Some((key, value)) = entry.rsplit_once(':') else {
                return Err(format!(
                    "`{entry}` is not a tool switch — write it as `name:on` or `name:off` \
                     (e.g. `*:off,task_list:on`)"
                ));
            };
            let key = key.trim();
            if key.is_empty() {
                return Err(format!("`{entry}` has no tool name before the `:`"));
            }
            let enabled = match value.trim() {
                "on" => true,
                "off" => false,
                other => {
                    return Err(format!("`{key}` must be `on` or `off`, not `{other}`"));
                }
            };
            switches.insert(key.to_string(), enabled);
        }
        if switches.is_empty() {
            return Err("empty tool spec — pass at least one `name:on`/`name:off`".into());
        }
        Ok(Self { switches })
    }
}

#[cfg(test)]
mod narrowing_tests {
    use super::*;
    use proptest::prelude::*;

    /// The read-only idiom, and the reason `narrow_with` exists at all:
    /// `deny_all_from` folds key by key, drops the `task_list:on` grant, and
    /// leaves `task_list` falling through to `*:off` — so the scope that says
    /// "nothing except task_list" would have been heard as "nothing".
    #[test]
    fn a_wildcard_off_with_an_exception_keeps_the_exception() {
        let scope = ToolPolicy::parse_spec("*:off,task_list:on,get_state:on").unwrap();
        let mut effective = ToolPolicy::allow_all();
        effective.narrow_with(&scope);

        assert!(effective.allows("task_list"), "the exception must survive");
        assert!(effective.allows("get_state"));
        assert!(!effective.allows("delegate"));
        assert!(!effective.allows("save_state"));

        // The old fold is what this replaces — pinned so a future
        // simplification back to it fails loudly rather than silently
        // disarming every `--tools` exception.
        let mut folded = ToolPolicy::allow_all();
        folded.deny_all_from(&scope);
        assert!(
            !folded.allows("task_list"),
            "deny_all_from drops grants — if this ever passes, narrow_with is redundant"
        );
    }

    /// The guarantee that makes a lowest-authority CLI scope safe: it can
    /// narrow, never widen.
    #[test]
    fn a_cli_scope_cannot_re_enable_what_settings_denied() {
        let mut effective = ToolPolicy::from_switches([("delegate".to_string(), false)]);
        let scope = ToolPolicy::parse_spec("delegate:on").unwrap();
        effective.narrow_with(&scope);
        assert!(!effective.allows("delegate"), "a grant must never transfer");
    }

    /// A narrowing scope must not disturb tools neither side mentions.
    #[test]
    fn tools_no_scope_mentions_are_untouched() {
        let mut effective = ToolPolicy::allow_all();
        effective.narrow_with(&ToolPolicy::parse_spec("task_assign:off").unwrap());
        assert!(!effective.allows("task_assign"));
        assert!(effective.allows("task_list"));
        assert!(effective.allows("save_state"));
    }

    /// **The #2800 witness.** A scope that grants a dynamic group (`custom`)
    /// must not, by that grant alone, keep a *catalog* group alive.
    ///
    /// The issue's own pair is `operator = {"build": on}` folded with
    /// `scope = {"*": off, "custom": on}`, asserting on `verify_done`; #3244
    /// retired both that tool and the `build` group, so this is the same pair
    /// on live names — `scratch` / `save_state` — and the same mechanism:
    /// `scope.allows("scratch")` routes a *group* key through
    /// `catalog::group_for`, which reads it as an unknown TOOL name and
    /// answers with the dynamic `custom` group.
    #[test]
    fn a_scope_granting_custom_does_not_keep_a_catalog_group_alive() {
        let operator = ToolPolicy::from_switches([("scratch".to_string(), true)]);
        let scope = ToolPolicy::parse_spec("*:off,custom:on").unwrap();

        // What the scope says about the tool, by name: nothing grants
        // `scratch`, so `*:off` denies it.
        assert!(!scope.allows("save_state"), "premise: the scope denies it");

        let mut folded = operator.clone();
        folded.narrow_with(&scope);
        assert!(
            !folded.allows("save_state"),
            "the fold widened past `operator ∧ scope`: the scope denies \
             `save_state` and the operator's `scratch` key resolved through \
             the scope's `custom` grant"
        );
    }

    /// The same defect as an operator meets it: `--tools "*:off,custom:on"`
    /// is "only my own tools", and it must not leave four built-ins on the
    /// wire because a settings file happened to spell a group key.
    #[test]
    fn a_custom_only_scope_leaves_no_builtin_on() {
        // Settings: nothing but the scratch plane and the operator's own
        // tools. The scope below then keeps only the second half.
        let mut effective = ToolPolicy::from_switches([
            (WILDCARD.to_string(), false),
            ("scratch".to_string(), true),
            ("custom".to_string(), true),
        ]);
        effective.narrow_with(&ToolPolicy::parse_spec("*:off,custom:on").unwrap());

        for name in catalog::ALL_NAMES {
            assert!(
                !effective.allows(name),
                "`{name}` survived a custom-only scope"
            );
        }
        assert!(
            effective.allows("deploy_to_staging"),
            "...and the custom tools the scope asked for are still there"
        );
        assert!(
            !effective.allows("mcp__github__create_issue"),
            "`custom:on` grants the custom group, never the mcp one"
        );
    }

    /// The dynamic groups have no member list to expand into, so they stay
    /// group keys and are resolved at group level. Both directions: a
    /// dynamic-group denial on either side reaches every member of it, and
    /// reaches nothing else.
    #[test]
    fn a_dynamic_group_key_narrows_its_own_members_only() {
        let mut effective = ToolPolicy::allow_all();
        effective.narrow_with(&ToolPolicy::parse_spec("mcp:off").unwrap());
        assert!(!effective.allows("mcp__github__create_issue"));
        assert!(effective.allows("deploy_to_staging"), "custom is a sibling");
        assert!(effective.allows("task_list"), "so is every built-in");

        // ...and the other side: an operator-level `custom:off` is not
        // re-openable by a scope that grants the tool's exact name.
        let mut effective = ToolPolicy::from_switches([("custom".to_string(), false)]);
        effective.narrow_with(&ToolPolicy::parse_spec("deploy_to_staging:on").unwrap());
        assert!(
            !effective.allows("deploy_to_staging"),
            "a grant never transfers"
        );
    }

    /// Expanding a catalog group leaves the policy honest: the family's
    /// resolved members are what `switches()` reports, not a stale group key
    /// saying `on` over four tools that are all off.
    #[test]
    fn an_expanded_group_key_does_not_outlive_its_members() {
        let mut effective = ToolPolicy::from_switches([("scratch".to_string(), true)]);
        effective.narrow_with(&ToolPolicy::parse_spec("*:off,custom:on").unwrap());

        assert!(
            !effective.switches().contains_key("scratch"),
            "the group key must not survive its expansion: {:?}",
            effective.switches()
        );
        for name in catalog::names_in_group("scratch") {
            assert_eq!(effective.switches().get(name), Some(&false));
        }
    }

    /// The tool-name universe the property quantifies over: every catalog
    /// name (so group resolution is exercised for real) plus an MCP and a
    /// custom name the catalog has never heard of. Mirrors
    /// [`crate::skill_grant`]'s, which pins the same intersection per name.
    fn universe() -> Vec<&'static str> {
        let mut names: Vec<&'static str> = catalog::ALL_NAMES.to_vec();
        names.push("mcp__github__create_issue");
        names.push("deploy_to_staging");
        names
    }

    fn any_key() -> impl Strategy<Value = String> {
        let mut keys: Vec<String> = universe().iter().map(|s| s.to_string()).collect();
        keys.extend(catalog::groups().iter().map(|g| g.to_string()));
        keys.push(WILDCARD.to_string());
        proptest::sample::select(keys)
    }

    fn any_policy() -> impl Strategy<Value = ToolPolicy> {
        proptest::collection::vec((any_key(), any::<bool>()), 0..6)
            .prop_map(ToolPolicy::from_switches)
    }

    proptest! {
        /// **The #2800 property.** The fold is the intersection *exactly*,
        /// per concrete tool name — the semantics
        /// [`crate::skill_grant::effective_allows`] already computed one name
        /// at a time, and which the folded form is now safe to stand in for.
        ///
        /// Widening is the failure this catches (a name the scope denies that
        /// the fold allows); the assertion is equality, so an over-narrowing
        /// fold — safe, but no longer the intersection — fails it too.
        #[test]
        fn the_fold_is_exactly_the_per_name_intersection(
            operator in any_policy(),
            scope in any_policy(),
        ) {
            let mut folded = operator.clone();
            folded.narrow_with(&scope);
            for name in universe() {
                prop_assert_eq!(
                    folded.allows(name),
                    operator.allows(name) && scope.allows(name),
                    "`{}` diverged from operator ∧ scope; operator {:?}, scope {:?}, folded {:?}",
                    name,
                    operator.switches(),
                    scope.switches(),
                    folded.switches()
                );
            }
        }
    }

    #[test]
    fn a_spec_is_parsed_or_named_as_malformed() {
        let p = ToolPolicy::parse_spec(" *:off , task_list:on ").unwrap();
        assert!(p.allows("task_list"));
        assert!(!p.allows("delegate"));

        for bad in ["task_list", "task_list:yes", ":on", ""] {
            assert!(
                ToolPolicy::parse_spec(bad).is_err(),
                "`{bad}` must be rejected"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_policy_allows_every_tool() {
        let policy = ToolPolicy::allow_all();
        assert!(policy.allows("delegate"));
        assert!(policy.allows("save_state"));
        assert!(policy.allows("get_environment"));
        assert!(policy.is_default());
        assert!(policy.denied_builtins().is_empty());
    }

    #[test]
    fn an_exact_name_beats_its_group_which_beats_the_wildcard() {
        let policy = ToolPolicy::from_switches([
            (WILDCARD.into(), false),
            ("scratch".into(), true),
            ("delete_state".into(), false),
        ]);
        // wildcard off → an unmentioned tool is off
        assert!(!policy.allows("task_list"));
        // group on → beats the wildcard
        assert!(policy.allows("save_state"));
        assert!(policy.allows("get_state"));
        // exact off → beats the group
        assert!(!policy.allows("delete_state"));
    }

    #[test]
    fn a_group_switch_covers_every_tool_in_it() {
        let policy = ToolPolicy::from_switches([("scratch".into(), false)]);
        for name in catalog::names_in_group("scratch") {
            assert!(!policy.allows(name), "{name} should be off");
        }
        assert!(policy.allows("task_list"), "other groups are untouched");
    }

    #[test]
    fn mcp_and_custom_tools_are_addressable_without_being_in_the_catalog() {
        let policy = ToolPolicy::from_switches([("mcp".into(), false), ("custom".into(), false)]);
        assert!(!policy.allows("mcp__github__create_issue"));
        // A customer's own registered tool, which no table lists.
        assert!(!policy.allows("deploy_to_staging"));
        assert!(policy.allows("task_list"));

        // ...and each is individually re-enablable by exact name.
        let policy = ToolPolicy::from_switches([
            ("custom".into(), false),
            ("deploy_to_staging".into(), true),
        ]);
        assert!(policy.allows("deploy_to_staging"));
    }

    #[test]
    fn folding_scopes_unions_denials_and_ignores_grants() {
        // The org turns the scratch group off.
        let managed = ToolPolicy::from_switches([("scratch".into(), false)]);
        // The project tries to turn it back on and to turn task_assign off.
        let mut project =
            ToolPolicy::from_switches([("scratch".into(), true), ("task_assign".into(), false)]);

        project.deny_all_from(&managed);

        assert!(
            !project.allows("save_state"),
            "a project must not be able to re-enable what the org denied"
        );
        assert!(
            !project.allows("task_assign"),
            "a project may still narrow further on its own"
        );
    }

    #[test]
    fn every_catalog_row_has_a_group_that_groups_reports() {
        let groups = catalog::groups();
        for entry in catalog::CATALOG {
            assert!(
                groups.contains(&entry.group),
                "`{}` has group `{}`, absent from groups()",
                entry.name,
                entry.group
            );
            assert!(!entry.group.is_empty(), "`{}` has no group", entry.name);
        }
    }
}
