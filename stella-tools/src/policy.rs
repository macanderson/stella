//! Which tools an operator has switched off.
//!
//! Stella ships with **every tool on**. A tool is available unless something
//! says otherwise, and there is exactly one way to say otherwise — a
//! `"tools"` entry in `settings.json` — whether the tool is a built-in, an MCP
//! server's, or one the customer wrote themselves:
//!
//! ```json
//! "tools": {
//!   "bash": "off",
//!   "process": "off",
//!   "repo_push": "off"
//! }
//! ```
//!
//! This replaces the per-capability booleans (`tools.bash`, `tools.web`) that
//! used to be the only switches. Those were not really *availability* — they
//! were policy defaults welded into the tool table, which is why every new
//! "should this be on?" question needed another enum variant, another
//! `RegistryOptions` field, and another hand-written branch. Availability is
//! now only about prerequisites the environment either satisfies or does not
//! (a media key, an issue backend, a search key); whether a *satisfiable* tool
//! is allowed is this module's business.
//!
//! # Precedence
//!
//! Most specific wins, and there are only three levels:
//!
//! 1. the exact tool name — `"repo_push"`
//! 2. its group — `"repo"`, or `"mcp"` / `"custom"` for tools the catalog does
//!    not know (see [`crate::catalog::group_for`])
//! 3. `"*"`, every tool
//!
//! So `{"*": "off", "read_file": "on"}` is a read-only agent in two lines, and
//! `{"process": "off", "read_output": "on"}` keeps exactly one of the four
//! process tools. Anything unmentioned is on.
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

    /// Narrow this policy by another scope: a tool ends up allowed only if
    /// **both** policies allow it.
    ///
    /// This is [`deny_all_from`](Self::deny_all_from)'s guarantee — a lower
    /// scope may narrow, never widen — computed over *resolved* answers
    /// instead of over raw keys, which is what a wildcard-plus-exception
    /// scope needs.
    ///
    /// `deny_all_from` folds key by key, so the read-only idiom
    /// `{"*": off, "read_file": on}` folds in as `{"*": off}` alone: the
    /// grant is dropped (correctly — grants never transfer), and `read_file`
    /// then falls through to the wildcard and is denied. The scope says
    /// "nothing except read_file" and the fold hears "nothing". Resolving
    /// each key through [`allows`](Self::allows) first keeps the exception,
    /// because `other.allows("read_file")` is `true` while
    /// `other.allows("*")` is `false`.
    ///
    /// The narrowing guarantee is unchanged and still structural: every entry
    /// is `self.allows(k) && other.allows(k)`, so a tool this policy denies
    /// stays denied whatever `other` says.
    pub fn narrow_with(&mut self, other: &ToolPolicy) {
        let keys: std::collections::BTreeSet<String> = self
            .switches
            .keys()
            .chain(other.switches.keys())
            .cloned()
            .collect();
        let resolved: Vec<(String, bool)> = keys
            .into_iter()
            .map(|key| {
                let allowed = self.allows(&key) && other.allows(&key);
                (key, allowed)
            })
            .collect();
        self.switches.extend(resolved);
    }

    /// Parse a comma-separated switch spec — the CLI spelling of the
    /// `settings.json` `"tools"` table (#1263).
    ///
    /// `*:off,read_file:on,grep:on` is a read-only agent; `repo_push:off`
    /// removes one capability. `on`/`off` are the only values, matching the
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
                     (e.g. `*:off,read_file:on`)"
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

    /// The read-only idiom, and the reason `narrow_with` exists at all:
    /// `deny_all_from` folds key by key, drops the `read_file:on` grant, and
    /// leaves `read_file` falling through to `*:off` — so the scope that says
    /// "nothing except read_file" would have been heard as "nothing".
    #[test]
    fn a_wildcard_off_with_an_exception_keeps_the_exception() {
        let scope = ToolPolicy::parse_spec("*:off,read_file:on,grep:on").unwrap();
        let mut effective = ToolPolicy::allow_all();
        effective.narrow_with(&scope);

        assert!(effective.allows("read_file"), "the exception must survive");
        assert!(effective.allows("grep"));
        assert!(!effective.allows("bash"));
        assert!(!effective.allows("write_file"));

        // The old fold is what this replaces — pinned so a future
        // simplification back to it fails loudly rather than silently
        // disarming every `--tools` exception.
        let mut folded = ToolPolicy::allow_all();
        folded.deny_all_from(&scope);
        assert!(
            !folded.allows("read_file"),
            "deny_all_from drops grants — if this ever passes, narrow_with is redundant"
        );
    }

    /// The guarantee that makes a lowest-authority CLI scope safe: it can
    /// narrow, never widen.
    #[test]
    fn a_cli_scope_cannot_re_enable_what_settings_denied() {
        let mut effective = ToolPolicy::from_switches([("bash".to_string(), false)]);
        let scope = ToolPolicy::parse_spec("bash:on").unwrap();
        effective.narrow_with(&scope);
        assert!(!effective.allows("bash"), "a grant must never transfer");
    }

    /// A narrowing scope must not disturb tools neither side mentions.
    #[test]
    fn tools_no_scope_mentions_are_untouched() {
        let mut effective = ToolPolicy::allow_all();
        effective.narrow_with(&ToolPolicy::parse_spec("repo_push:off").unwrap());
        assert!(!effective.allows("repo_push"));
        assert!(effective.allows("read_file"));
        assert!(effective.allows("bash"));
    }

    #[test]
    fn a_spec_is_parsed_or_named_as_malformed() {
        let p = ToolPolicy::parse_spec(" *:off , read_file:on ").unwrap();
        assert!(p.allows("read_file"));
        assert!(!p.allows("bash"));

        for bad in ["read_file", "read_file:yes", ":on", ""] {
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
    fn the_default_policy_allows_every_tool_including_bash() {
        let policy = ToolPolicy::allow_all();
        // The shipped posture. `bash` is the one that used to be off by
        // default, so it is the one worth naming here.
        assert!(policy.allows("bash"));
        assert!(policy.allows("start_process"));
        assert!(policy.allows("web_fetch"));
        assert!(policy.allows("read_file"));
        assert!(policy.is_default());
        assert!(policy.denied_builtins().is_empty());
    }

    #[test]
    fn an_exact_name_beats_its_group_which_beats_the_wildcard() {
        let policy = ToolPolicy::from_switches([
            (WILDCARD.into(), false),
            ("process".into(), true),
            ("send_stdin".into(), false),
        ]);
        // wildcard off → an unmentioned tool is off
        assert!(!policy.allows("read_file"));
        // group on → beats the wildcard
        assert!(policy.allows("start_process"));
        assert!(policy.allows("read_output"));
        // exact off → beats the group
        assert!(!policy.allows("send_stdin"));
    }

    #[test]
    fn a_group_switch_covers_every_tool_in_it() {
        let policy = ToolPolicy::from_switches([("process".into(), false)]);
        for name in catalog::names_in_group("process") {
            assert!(!policy.allows(name), "{name} should be off");
        }
        assert!(policy.allows("read_file"), "other groups are untouched");
    }

    #[test]
    fn mcp_and_custom_tools_are_addressable_without_being_in_the_catalog() {
        let policy = ToolPolicy::from_switches([("mcp".into(), false), ("custom".into(), false)]);
        assert!(!policy.allows("mcp__github__create_issue"));
        // A customer's own registered tool, which no table lists.
        assert!(!policy.allows("deploy_to_staging"));
        assert!(policy.allows("read_file"));

        // ...and each is individually re-enablable by exact name.
        let policy = ToolPolicy::from_switches([
            ("custom".into(), false),
            ("deploy_to_staging".into(), true),
        ]);
        assert!(policy.allows("deploy_to_staging"));
    }

    #[test]
    fn folding_scopes_unions_denials_and_ignores_grants() {
        // The org turns the process group off.
        let managed = ToolPolicy::from_switches([("process".into(), false)]);
        // The project tries to turn it back on and to turn bash off.
        let mut project =
            ToolPolicy::from_switches([("process".into(), true), ("bash".into(), false)]);

        project.deny_all_from(&managed);

        assert!(
            !project.allows("start_process"),
            "a project must not be able to re-enable what the org denied"
        );
        assert!(
            !project.allows("bash"),
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
