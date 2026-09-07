//! The SETTINGS tab's synchronous overlay handlers — the ENGINE overlay and
//! the TOOLS panel's refresh/save ops, split out of `command_deck.rs` (closed
//! to growth) the way `skills.rs` and `authoring.rs` were.
//!
//! Both handlers answer with a fresh snapshot built by the parent module's
//! `engine_config_inbound` / `tool_policy_inbound`, which stay there because
//! the deck's other arms (boot seeding, `/reload`) share them.
//!
//! **Neither handler reloads the live [`Config`] itself.** A save makes the
//! session's `Config` stale, and re-deriving it is the caller's job at a safe
//! boundary — the same discipline `/budget` already follows with
//! `pending_budget`. Both call sites run this code, and one of them runs it
//! *while a turn is in flight*, holding `&Config` and reading the very fields
//! a reload overwrites (tool policy, authority, engine posture). Mutating
//! there would tear config out from under a running turn, so the handlers
//! report `stale` and let the caller pick the moment.
//!
//! The panels are unaffected by the delay: both snapshot builders re-read the
//! scope chain from disk, so what the overlay shows is what the files say,
//! reloaded or not. Only *subsequent turns* depend on the live `Config`.

use stella_core::ports::ToolExecutor;
use stella_tools::custom::CustomTool;
use stella_tui::{AgentScope, Inbound, WorkspaceInput};
use tokio::sync::mpsc::UnboundedSender;

use super::{engine_config_inbound, tool_policy_inbound};
use crate::config::Config;

/// Handle one ENGINE-overlay op (refresh / save) — cheap local settings
/// I/O, answered with a fresh [`Inbound::EngineConfig`]. Called from BOTH
/// recv sites so the overlay works mid-turn too. Returns `true` when the
/// input was one of the overlay's.
///
/// `stale` is set when a write lands, meaning the live [`Config`] no longer
/// matches the files — see the module docs for why the reload is the
/// caller's to perform.
pub(super) fn handle_engine_config_input(
    input: &WorkspaceInput,
    cfg: &Config,
    stale: &mut bool,
    in_tx: &UnboundedSender<Inbound>,
) -> bool {
    match input {
        WorkspaceInput::EngineConfigRefresh => {
            let _ = in_tx.send(engine_config_inbound(cfg, None));
            true
        }
        WorkspaceInput::EngineConfigSave { state, scope } => {
            let engine = crate::engine_config::settings_from_state(state);
            let path = match scope {
                AgentScope::User => crate::settings::user_config_path(),
                AgentScope::Project => {
                    Some(crate::settings::project_config_path(&cfg.workspace_root))
                }
            };
            let status = match path {
                None => "save failed: cannot determine $HOME for user settings".to_string(),
                Some(path) => match engine.save_to(&path) {
                    Ok(()) => {
                        *stale = true;
                        format!(
                            "saved to {} — applies to runs started from now on",
                            path.display()
                        )
                    }
                    Err(e) => format!("save failed: {e}"),
                },
            };
            // The snapshot sent back is the MERGED view — if a project
            // scope overrides what was just saved at the user scope, the
            // overlay shows the effective value, not the wish.
            let _ = in_tx.send(engine_config_inbound(cfg, Some(status)));
            true
        }
        _ => false,
    }
}

/// Handle one TOOLS-panel op (refresh / save) — cheap local settings I/O,
/// answered with a fresh [`Inbound::ToolPolicy`]. Called from BOTH recv sites
/// so the panel works mid-turn too. Returns `true` when the input was one of
/// the panel's.
///
/// A save applies to turns started afterwards: the in-flight turn already
/// resolved its tool stack, and rebuilding it under a running engine is a
/// different (and much larger) change than editing settings. `stale` carries
/// the reload the caller owes — see the module docs.
pub(super) fn handle_tools_input(
    input: &WorkspaceInput,
    cfg: &Config,
    names: &[String],
    stale: &mut bool,
    in_tx: &UnboundedSender<Inbound>,
) -> bool {
    match input {
        WorkspaceInput::ToolsRefresh => {
            let _ = in_tx.send(tool_policy_inbound(cfg, names, None));
            true
        }
        WorkspaceInput::ToolsSave { switches, scope } => {
            let path = match scope {
                AgentScope::User => crate::settings::user_config_path(),
                AgentScope::Project => {
                    Some(crate::settings::project_config_path(&cfg.workspace_root))
                }
            };
            // The ceiling is re-read from disk rather than taken from the
            // session's merged policy: the merged map cannot say which
            // denials are the org's, and only the org's may refuse a grant.
            let ceiling = crate::settings::Settings::load_tool_scopes(&cfg.workspace_root)
                .map(|scopes| scopes.managed)
                .unwrap_or_default();
            let status = match path {
                None => "save failed: cannot determine $HOME for user settings".to_string(),
                Some(path) => {
                    match crate::tool_switches::save_switches(&path, switches, &ceiling) {
                        Ok(status) => {
                            *stale = true;
                            status
                        }
                        Err(e) => format!("save failed: {e}"),
                    }
                }
            };
            let _ = in_tx.send(tool_policy_inbound(cfg, names, Some(status)));
            true
        }
        _ => false,
    }
}

/// The `/reload` command: re-read the settings scope chain (user + project,
/// managed ceiling folded in) and re-apply everything it derives — engine
/// posture, tool policy, authority, recap/trace/reward/worktree switches — to
/// THIS session's live [`Config`], without restarting.
///
/// Provider/model/credential resolution is deliberately untouched (see
/// [`Config::reload_from_disk`]); `/model` and the SETTINGS tab are the seam
/// for that. Returns the line to print in the lead transcript.
pub(super) fn reload_command(cfg: &mut Config, in_tx: &UnboundedSender<Inbound>) -> String {
    if let Err(e) = cfg.reload_from_disk() {
        return format!("reload failed: {e}");
    }
    // Refresh an open SETTINGS tab with the merged view, the same courtesy
    // `/model` pays. The deck renders the last snapshot it was sent, so a
    // hand edit picked up by `/reload` is invisible in an open overlay until
    // one arrives. The TOOLS panel is refreshed separately, at the driver
    // loop's `DeckCommand::Reloaded` arm: an accurate row list needs the
    // MCP-inclusive live stack, which this function does not hold (`#1990`).
    let _ = in_tx.send(engine_config_inbound(cfg, None));
    "configuration reloaded — engine, tools, and authority settings re-read from disk.".to_string()
}

/// Refresh an open TOOLS panel with the current, MCP-inclusive row list,
/// sent as a fresh [`Inbound::ToolPolicy`] (`#1990`).
///
/// The list is built fresh each time, not cached: MCP servers join the
/// session later, so the panel must ask what the stack holds right now.
/// Only the driver loop holds `mcp_slot`, `registry`, and `custom_tools`
/// together, so it calls this once a command marks the panel stale
/// (`DeckCommand::Reloaded`, `DeckCommand::SettingsReloaded`).
pub(super) fn refresh_tools_panel(
    cfg: &Config,
    mcp_slot: &tokio::sync::OnceCell<std::sync::Arc<stella_mcp::McpToolSet>>,
    registry: &dyn ToolExecutor,
    custom_tools: &[CustomTool],
    in_tx: &UnboundedSender<Inbound>,
) {
    let names = crate::tool_switches::session_tool_names(
        live_tool_executor(mcp_slot, registry),
        custom_tools,
    );
    let _ = in_tx.send(tool_policy_inbound(cfg, &names, None));
}

/// Which tool executor is "the live stack" right now: the connected MCP
/// set if one exists, the bare registry if not. Split out so this function
/// and the driver loop's own between-prompts refresh give the same answer,
/// checked by a test that needs no `Config` at all.
///
/// The boot-seed snapshot does not use this: MCP has not tried to connect
/// yet at that point, so it always means the registry.
pub(super) fn live_tool_executor<'a>(
    mcp_slot: &'a tokio::sync::OnceCell<std::sync::Arc<stella_mcp::McpToolSet>>,
    registry: &'a dyn ToolExecutor,
) -> &'a dyn ToolExecutor {
    match mcp_slot.get() {
        Some(set) => set.as_ref(),
        None => registry,
    }
}

/// Re-derive the live [`Config`] from disk after a save, reporting a failure
/// to the deck rather than swallowing it.
///
/// The one place both call sites converge, so "a save is live" is stated once.
/// A failed reload leaves the session on its previous (still coherent) values
/// — the files are already written, so a restart picks them up regardless,
/// which is what the note says. That coherence is not this function's doing:
/// it rests on [`Config::reload_from_disk`] being all-or-nothing, so the note
/// below is only honest while that holds.
pub(super) fn apply_pending_reload(cfg: &mut Config, in_tx: &UnboundedSender<Inbound>) {
    if let Err(e) = cfg.reload_from_disk() {
        let _ = in_tx.send(super::chrome_note(format!(
            "settings saved, but reloading them failed: {e} — restart to pick them up."
        )));
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use serde_json::Value;
    use stella_protocol::tool::{ToolOutput, ToolSchema};

    use super::*;

    /// A base executor standing in for "registry" — enough to tell it apart
    /// from an MCP set by name alone.
    struct Base(&'static str);

    #[async_trait]
    impl ToolExecutor for Base {
        fn schemas(&self) -> Vec<ToolSchema> {
            vec![ToolSchema {
                name: self.0.to_string(),
                description: String::new(),
                input_schema: serde_json::json!({}),
                read_only: false,
                speculation_safe: false,
            }]
        }
        async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
            ToolOutput::Ok {
                content: String::new(),
                data: None,
            }
        }
    }

    fn names_of(executor: &dyn ToolExecutor) -> Vec<String> {
        executor.schemas().into_iter().map(|s| s.name).collect()
    }

    /// `#1990`: this and the boot-seed code must answer "what is the live
    /// tool stack" the same way — the bare registry, or the MCP set once one
    /// connects. Without `live_tool_executor` as its own function, this test
    /// cannot compile, because nothing here exists yet to call.
    #[tokio::test]
    async fn live_tool_executor_switches_to_the_connected_mcp_set() {
        let registry = Base("registry_only_tool");
        let slot: tokio::sync::OnceCell<std::sync::Arc<stella_mcp::McpToolSet>> =
            tokio::sync::OnceCell::new();

        // Nothing connected yet: the registry is the whole live stack.
        assert_eq!(
            names_of(live_tool_executor(&slot, &registry)),
            vec!["registry_only_tool".to_string()]
        );

        // Connected (to nothing, here — the point is the *slot*, not what a
        // real server would advertise): the answer switches to the MCP set,
        // which never claims the registry-only tool by name.
        let mcp = stella_mcp::McpToolSet::connect(&[], std::time::Duration::from_secs(1)).await;
        slot.set(std::sync::Arc::new(mcp)).unwrap();
        assert!(
            !names_of(live_tool_executor(&slot, &registry))
                .contains(&"registry_only_tool".to_string()),
            "a connected (even empty) MCP set must replace the bare registry, not add to it"
        );
    }
}
