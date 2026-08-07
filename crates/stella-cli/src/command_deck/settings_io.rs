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

/// Re-derive the live [`Config`] from disk after a save, reporting a failure
/// to the deck rather than swallowing it.
///
/// The one place both call sites converge, so "a save is live" is stated once.
/// A failed reload leaves the session on its previous (still coherent) values
/// — the files are already written, so a restart picks them up regardless,
/// which is what the note says.
pub(super) fn apply_pending_reload(cfg: &mut Config, in_tx: &UnboundedSender<Inbound>) {
    if let Err(e) = cfg.reload_from_disk() {
        let _ = in_tx.send(super::chrome_note(format!(
            "settings saved, but reloading them failed: {e} — restart to pick them up."
        )));
    }
}
