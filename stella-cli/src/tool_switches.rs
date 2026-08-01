//! The session's tool surface, reported and edited — the driver half of the
//! SETTINGS tab's TOOLS panel.
//!
//! [`crate::tool_policy`] decides *what* is off and enforces it. This module
//! answers the two questions a settings editor asks that enforcement never
//! needs to:
//!
//! 1. **What tools does this operator actually have?** Not what Stella ships:
//!    MCP servers' tools and a customer's own `.stella/tools/*.toml` tools
//!    exist only in the assembled session stack, so the catalog cannot answer
//!    it. [`session_tool_names`] reads the live executor instead, which is why
//!    a customer sees their own tools listed under a `custom` section.
//! 2. **Who switched it off?** [`tool_rows`] resolves the `"tools"` key that
//!    did it *and* the scope whose file carries that key, because "off" alone
//!    leaves an operator hunting three settings files, and because an
//!    org-managed denial is categorically different from their own: it cannot
//!    be lifted, so the row is reported LOCKED and the editor refuses to write
//!    a grant the next load would silently drop.
//!
//! The write path ([`save_switches`]) is a read-modify-write of ONE scope's
//! own `"tools"` object via [`ToolsSettings::save_to`], so every other key in
//! that settings file — and every switch the panel did not touch — survives
//! byte-for-byte at the value level.

use std::collections::BTreeMap;
use std::path::Path;

use stella_core::ports::ToolExecutor;
use stella_tools::catalog::{self, Availability};
use stella_tools::custom::CustomTool;
use stella_tools::policy::ToolPolicy;
use stella_tui::envelope::{ToolDenial, ToolPolicyState, ToolRow, ToolScope};

use crate::settings::{Toggle, ToolScopePolicies, ToolsSettings};

/// Every tool name this session can dispatch, before the policy filters any of
/// them out — the union of what the base executor advertises (the native
/// registry, plus each connected MCP server's tools when the MCP set has
/// landed), the workspace's custom tools, and the CLI's own session layer.
///
/// `base` must be the executor from BELOW
/// [`crate::agent::PolicyToolSet`]: the panel lists what could be switched on,
/// not what currently is, so reading a filtered surface would make every tool
/// an operator turned off disappear from the editor that turns it back on.
pub(crate) fn session_tool_names(
    base: &dyn ToolExecutor,
    custom_tools: &[CustomTool],
) -> Vec<String> {
    let mut names: Vec<String> = base
        .schemas()
        .into_iter()
        .map(|schema| schema.name)
        .collect();
    names.extend(custom_tools.iter().map(|tool| tool.name.clone()));
    // The interactive/discovery layer is wrapped ABOVE the policy filter and so
    // never appears in `base`, but its tools are as switchable as any other.
    names.extend(
        catalog::names_where(|availability| availability == Availability::Session)
            .into_iter()
            .map(str::to_string),
    );
    names.sort_unstable();
    names.dedup();
    names
}

/// Build the panel's rows: one per live tool, grouped and sorted, each
/// carrying whether the org locked it and — when it is off — the key and scope
/// that did it.
///
/// `effective` is the merged policy the runtime enforces
/// ([`crate::config::Config::tool_policy`]); `scopes` is the same three files
/// read apart, which is the only way to attribute a switch to a file.
pub(crate) fn tool_rows(
    names: &[String],
    effective: &ToolPolicy,
    scopes: &ToolScopePolicies,
) -> Vec<ToolRow> {
    let mut rows: Vec<ToolRow> = names
        .iter()
        .map(|name| {
            let locked = !scopes.managed.allows(name);
            // The merged policy carries the ceiling already, so it normally
            // answers for a locked tool too; falling back to the ceiling keeps
            // a locked row explained even if the two were read a moment apart.
            let key = crate::tool_policy::disabled_by(effective, name)
                .or_else(|| crate::tool_policy::disabled_by(&scopes.managed, name));
            let off = key.map(|key| {
                let denied_by = |policy: &ToolPolicy| policy.switches().get(&key) == Some(&false);
                let scope = if locked {
                    Some(ToolScope::Managed)
                } else if denied_by(&scopes.project) {
                    Some(ToolScope::Project)
                } else if denied_by(&scopes.user) {
                    Some(ToolScope::User)
                } else {
                    None
                };
                ToolDenial { key, scope }
            });
            ToolRow {
                name: name.clone(),
                group: catalog::group_for(name).to_string(),
                locked,
                off,
            }
        })
        .collect();
    rows.sort_by(|a, b| a.group.cmp(&b.group).then_with(|| a.name.cmp(&b.name)));
    rows
}

/// The whole snapshot the panel renders: the rows plus the merged `"tools"`
/// map they resolve against.
pub(crate) fn tool_policy_state(
    names: &[String],
    effective: &ToolPolicy,
    scopes: &ToolScopePolicies,
) -> ToolPolicyState {
    ToolPolicyState {
        tools: tool_rows(names, effective, scopes),
        switches: effective.switches().clone(),
    }
}

/// Apply the panel's switch edits to ONE settings file, returning the status
/// line the panel shows.
///
/// Two properties this has to hold:
///
/// - **Merge, never replace.** The file's existing `"tools"` object is read
///   first and only the edited keys are inserted, so a switch the panel never
///   showed (a tool from a session with different MCP servers connected) is
///   not deleted by an editor that had never heard of it.
/// - **A grant the org denies is refused here too.** The UI already locks such
///   rows; this is the second half of the same rule, at the layer that writes.
///   Persisting `{"bash": "on"}` under a managed `{"bash": "off"}` would put a
///   line in the operator's own file that reads like a capability they have
///   and that the next load discards — the settings file must not lie.
pub(crate) fn save_switches(
    path: &Path,
    edits: &BTreeMap<String, bool>,
    ceiling: &ToolPolicy,
) -> Result<String, String> {
    let mut section = ToolsSettings::read_from(path)?;
    let mut refused: Vec<&str> = Vec::new();
    for (key, &on) in edits {
        if on && !ceiling.allows(key) {
            refused.push(key.as_str());
            continue;
        }
        section
            .entries
            .insert(key.clone(), if on { Toggle::On } else { Toggle::Off });
    }
    section.save_to(path)?;
    let mut status = format!(
        "saved to {} — applies to turns started from now on",
        path.display()
    );
    if !refused.is_empty() {
        status.push_str(&format!(
            " · {} stayed off (org-managed settings deny it)",
            refused.join(", ")
        ));
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::Value;
    use stella_protocol::tool::{ToolOutput, ToolSchema};

    /// A base executor standing in for "registry (+ MCP)".
    struct Base(Vec<&'static str>);

    #[async_trait]
    impl ToolExecutor for Base {
        fn schemas(&self) -> Vec<ToolSchema> {
            self.0
                .iter()
                .map(|name| ToolSchema {
                    name: (*name).to_string(),
                    description: String::new(),
                    input_schema: serde_json::json!({}),
                    read_only: false,
                    speculation_safe: false,
                })
                .collect()
        }
        async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
            ToolOutput::Ok {
                content: String::new(),
            }
        }
    }

    fn custom(name: &str) -> CustomTool {
        CustomTool {
            name: name.to_string(),
            description: String::new(),
            command: vec!["true".into()],
            timeout_ms: 1000,
            input_schema: serde_json::json!({}),
            env: Default::default(),
            source: std::path::PathBuf::from("/tmp/x.toml"),
        }
    }

    fn row<'a>(rows: &'a [ToolRow], name: &str) -> &'a ToolRow {
        rows.iter().find(|r| r.name == name).expect("row present")
    }

    /// The catalog alone cannot answer "what tools do I have" — this is the
    /// property that makes the panel worth building.
    #[test]
    fn the_live_surface_includes_mcp_and_custom_tools_the_catalog_never_heard_of() {
        let base = Base(vec!["read_file", "bash", "mcp__gh__create_issue"]);
        let names = session_tool_names(&base, &[custom("deploy_to_staging")]);

        assert!(names.contains(&"mcp__gh__create_issue".to_string()));
        assert!(names.contains(&"deploy_to_staging".to_string()));
        assert!(names.contains(&"read_file".to_string()));
        // The CLI's own session layer is switchable too, and never appears in
        // the base executor.
        assert!(names.contains(&"ask_user".to_string()));
        assert!(names.windows(2).all(|w| w[0] < w[1]), "sorted and deduped");
    }

    #[test]
    fn rows_are_grouped_and_name_the_key_and_scope_that_switched_each_off() {
        let base = Base(vec!["read_file", "bash", "start_process", "send_stdin"]);
        let names = session_tool_names(&base, &[custom("deploy_to_staging")]);
        let scopes = ToolScopePolicies {
            managed: ToolPolicy::allow_all(),
            user: ToolPolicy::from_switches([("process".into(), false)]),
            project: ToolPolicy::from_switches([("bash".into(), false)]),
        };
        let effective =
            ToolPolicy::from_switches([("process".into(), false), ("bash".into(), false)]);
        let rows = tool_rows(&names, &effective, &scopes);

        // A whole family off by ONE group key, attributed to the file with it.
        for name in ["start_process", "send_stdin"] {
            let denial = row(&rows, name).off.as_ref().expect("off");
            assert_eq!(denial.key, "process");
            assert_eq!(denial.scope, Some(ToolScope::User));
        }
        let denial = row(&rows, "bash").off.as_ref().expect("off");
        assert_eq!(denial.key, "bash");
        assert_eq!(denial.scope, Some(ToolScope::Project));
        assert!(
            row(&rows, "read_file").off.is_none(),
            "everything else is on"
        );

        // Grouping, including the two sections that exist only at runtime.
        assert_eq!(row(&rows, "deploy_to_staging").group, "custom");
        assert_eq!(row(&rows, "start_process").group, "process");
        assert!(
            rows.windows(2)
                .all(|w| (&w[0].group, &w[0].name) <= (&w[1].group, &w[1].name)),
            "sorted by group then name"
        );
        assert!(rows.iter().all(|r| !r.locked), "nothing is org-denied here");
    }

    #[test]
    fn an_org_denied_tool_is_locked_and_attributed_to_the_managed_scope() {
        let base = Base(vec!["read_file", "bash"]);
        let names = session_tool_names(&base, &[]);
        let scopes = ToolScopePolicies {
            managed: ToolPolicy::from_switches([("bash".into(), false)]),
            // The user granted it; the ceiling is what stands.
            user: ToolPolicy::from_switches([("bash".into(), true)]),
            project: ToolPolicy::allow_all(),
        };
        let effective = ToolPolicy::from_switches([("bash".into(), false)]);
        let rows = tool_rows(&names, &effective, &scopes);

        let bash = row(&rows, "bash");
        assert!(bash.locked, "the org's denial is not a switch");
        let denial = bash.off.as_ref().expect("off");
        assert_eq!(denial.key, "bash");
        assert_eq!(
            denial.scope,
            Some(ToolScope::Managed),
            "the managed scope outranks the user's grant in the report too"
        );
        assert!(!row(&rows, "read_file").locked);
    }

    #[test]
    fn saving_merges_into_the_scope_and_preserves_every_other_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{
              "providers": {"anthropic": {"api_key_env": "ANTHROPIC_API_KEY"}},
              "tools": {"web": "off"},
              "some_future_key": [1, 2, 3]
            }"#,
        )
        .unwrap();

        let status = save_switches(
            &path,
            &BTreeMap::from([("bash".to_string(), false), ("read_file".to_string(), true)]),
            &ToolPolicy::allow_all(),
        )
        .unwrap();
        assert!(status.contains("saved to"), "{status}");

        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root["tools"]["bash"], "off", "the edited key landed");
        assert_eq!(root["tools"]["read_file"], "on");
        assert_eq!(
            root["tools"]["web"], "off",
            "a switch the panel did not touch survives"
        );
        assert_eq!(
            root["providers"]["anthropic"]["api_key_env"], "ANTHROPIC_API_KEY",
            "every other settings key survives a tool save"
        );
        assert_eq!(root["some_future_key"], serde_json::json!([1, 2, 3]));

        // The section round-trips back through the reader it was written with.
        let back = ToolsSettings::read_from(&path).unwrap();
        assert_eq!(back.entries.get("bash"), Some(&Toggle::Off));
        assert_eq!(back.entries.get("read_file"), Some(&Toggle::On));
        assert!(!back.policy().allows("bash"));
        assert!(back.policy().allows("read_file"));
    }

    #[test]
    fn a_grant_the_org_denies_is_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let ceiling = ToolPolicy::from_switches([("process".into(), false)]);

        let status = save_switches(
            &path,
            &BTreeMap::from([
                // Denied by the ceiling's GROUP key even though this is a name.
                ("start_process".to_string(), true),
                ("bash".to_string(), false),
            ]),
            &ceiling,
        )
        .unwrap();

        let back = ToolsSettings::read_from(&path).unwrap();
        assert_eq!(
            back.entries.get("start_process"),
            None,
            "the settings file must not claim a capability the org denied"
        );
        assert_eq!(
            back.entries.get("bash"),
            Some(&Toggle::Off),
            "narrowing is always permitted"
        );
        assert!(
            status.contains("start_process"),
            "the refusal is reported: {status}"
        );
        assert!(status.contains("org-managed"), "{status}");
    }

    #[test]
    fn clearing_every_switch_removes_the_section_rather_than_writing_an_empty_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"tools": {"bash": "off"}, "enable_recap": "on"}"#).unwrap();

        let mut section = ToolsSettings::read_from(&path).unwrap();
        section.entries.clear();
        section.save_to(&path).unwrap();

        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            root.get("tools").is_none(),
            "no switches is the shipped posture — the file should read like it"
        );
        assert_eq!(root["enable_recap"], "on");
    }

    /// The whole loop the panel drives, through the real scope chain: save a
    /// switch to the user scope, reload, and see it in the rows — with the
    /// org-managed file's denial still standing over a user grant.
    #[test]
    fn a_saved_switch_round_trips_through_the_settings_chain_under_the_managed_ceiling() {
        let _env = crate::test_env::lock();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(home.join(".stella")).unwrap();
        std::fs::create_dir_all(workspace.join(".stella")).unwrap();
        let managed = tmp.path().join("managed.json");
        std::fs::write(&managed, r#"{"tools": {"web": "off"}}"#).unwrap();
        let _restore = crate::test_env::EnvRestore::capture(&["HOME", "STELLA_MANAGED_SETTINGS"]);
        // SAFETY: serialized behind the binary-wide environment lock.
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("STELLA_MANAGED_SETTINGS", &managed);
        }

        let base = Base(vec!["read_file", "bash", "web_fetch", "start_process"]);
        let names = session_tool_names(&base, &[]);
        let snapshot = || {
            let scopes = crate::settings::Settings::load_tool_scopes(&workspace).unwrap();
            let effective = crate::settings::Settings::load(&workspace)
                .unwrap()
                .tool_policy();
            tool_policy_state(&names, &effective, &scopes)
        };

        // Before: everything ships on except what the org denied.
        let before = snapshot();
        assert!(row(&before.tools, "bash").off.is_none(), "bash ships on");
        let web = row(&before.tools, "web_fetch");
        assert!(web.locked, "the org's group denial locks the whole family");
        assert_eq!(web.off.as_ref().unwrap().scope, Some(ToolScope::Managed));

        // Save exactly what the panel would send: two switches the operator
        // flipped, plus one grant the org denies.
        let user_path = crate::settings::user_settings_path().unwrap();
        std::fs::write(&user_path, r#"{"enable_recap": "on"}"#).unwrap();
        let status = save_switches(
            &user_path,
            &BTreeMap::from([
                ("bash".to_string(), false),
                ("process".to_string(), false),
                ("web_fetch".to_string(), true),
            ]),
            &snapshot_ceiling(&workspace),
        )
        .unwrap();
        assert!(status.contains("web_fetch"), "the refused grant is named");

        // After: the reload shows the change, attributed to the file that
        // carries it — and the org's denial is untouched by the user's grant.
        let after = snapshot();
        let bash = row(&after.tools, "bash");
        let denial = bash.off.as_ref().expect("bash is off now");
        assert_eq!(denial.key, "bash");
        assert_eq!(denial.scope, Some(ToolScope::User));
        assert!(!bash.locked, "the operator's own switch is not a lock");
        let start = row(&after.tools, "start_process");
        assert_eq!(
            start.off.as_ref().map(|d| d.key.as_str()),
            Some("process"),
            "the group key covers the family"
        );
        let web = row(&after.tools, "web_fetch");
        assert!(web.locked, "the org denial survives a user grant");
        assert_eq!(web.off.as_ref().unwrap().scope, Some(ToolScope::Managed));
        // The panel's own resolution map reflects the save.
        assert_eq!(after.switches.get("bash"), Some(&false));
        assert_eq!(after.switches.get("process"), Some(&false));
        // …and the unrelated key in the same file survived the write.
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&user_path).unwrap()).unwrap();
        assert_eq!(root["enable_recap"], "on");
    }

    fn snapshot_ceiling(workspace: &Path) -> ToolPolicy {
        crate::settings::Settings::load_tool_scopes(workspace)
            .unwrap()
            .managed
    }

    #[test]
    fn saving_through_a_symlink_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.json");
        std::fs::write(&real, "{}").unwrap();
        let link = dir.path().join("settings.json");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        #[cfg(not(unix))]
        std::fs::copy(&real, &link).unwrap();

        let result = save_switches(
            &link,
            &BTreeMap::from([("bash".to_string(), false)]),
            &ToolPolicy::allow_all(),
        );
        #[cfg(unix)]
        assert!(
            result.is_err_and(|e| e.contains("symlink")),
            "a settings write must not follow a symlink"
        );
        #[cfg(not(unix))]
        assert!(result.is_ok());
    }
}
