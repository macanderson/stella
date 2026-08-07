//! The SETTINGS tab's synchronous overlay handlers — the ENGINE overlay and
//! the TOOLS panel's refresh/save ops, split out of `command_deck.rs` (closed
//! to growth) the way `skills.rs` and `authoring.rs` were.
//!
//! Both handlers answer with a fresh snapshot built by the parent module's
//! `engine_config_inbound` / `tool_policy_inbound`, which stay there because
//! the deck's other arms (boot seeding, `/reload`) share them.

use stella_tui::{AgentScope, Inbound, WorkspaceInput};
use tokio::sync::mpsc::UnboundedSender;

use super::{engine_config_inbound, tool_policy_inbound};
use crate::config::Config;

/// Handle one ENGINE-overlay op (refresh / save) — cheap local settings
/// I/O, answered with a fresh [`Inbound::EngineConfig`]. Called from BOTH
/// recv sites so the overlay works mid-turn too. Returns `true` when the
/// input was one of the overlay's.
pub(super) fn handle_engine_config_input(
    input: &WorkspaceInput,
    cfg: &mut Config,
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
                    // A save is immediately live: reload this session's
                    // `Config` from the same scope chain the write just
                    // landed in, the same effect `/reload` has. Saving and
                    // then needing a second manual step to make it count
                    // was exactly the surprise this closes.
                    Ok(()) => match cfg.reload_from_disk() {
                        Ok(()) => format!(
                            "saved to {} and reloaded — applies to runs started from now on",
                            path.display()
                        ),
                        Err(e) => format!(
                            "saved to {} but reload failed: {e} (restart to pick it up)",
                            path.display()
                        ),
                    },
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
/// different (and much larger) change than editing settings.
pub(super) fn handle_tools_input(
    input: &WorkspaceInput,
    cfg: &mut Config,
    names: &[String],
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
                        // Live the moment it lands, same as `/reload` — see
                        // the identical seam in `handle_engine_config_input`.
                        Ok(status) => match cfg.reload_from_disk() {
                            Ok(()) => format!("{status} (reloaded)"),
                            Err(e) => format!("{status} (reload failed: {e})"),
                        },
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
