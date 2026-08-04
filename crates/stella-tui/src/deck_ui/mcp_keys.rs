//! MCP-tab key handling: four modal layers over one tab.
//!
//! The inspector (ctrl+o) outranks everything — it is the topmost surface, so
//! it claims keys before Browse's letter actions can fire on a row that is no
//! longer visible. Below it, Search and Auth are modal in the same sense
//! (every key is swallowed so nothing leaks into the composer), while Browse's
//! letter actions gate on `composer_empty` so they never shadow the first
//! character of a prompt.
//!
//! Split from `deck_ui.rs` (#629's 1500-line ratchet).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{DeckAction, DeckUi};
use crate::envelope::{Secret, WorkspaceInput};
use crate::views::mcp::{AuthPrompt, AuthStep, McpInspector, McpMode};

/// MCP tab keys. Three sub-modes: Browse (navigate the configured servers and
/// act on the selection), Search (type a registry query, then Enter to search
/// and Enter again to install the highlighted result), and Auth (a two-step
/// masked credential prompt). Search/Auth are modal — they claim every key so
/// typing never leaks into the composer — while Browse's letter actions gate on
/// `composer_empty` so they don't shadow the first character of a prompt.
pub(super) fn handle_mcp_key(
    key: KeyEvent,
    ui: &mut DeckUi,
    composer_empty: bool,
) -> Option<DeckAction> {
    // The inspector is modal and outranks every mode: it is the topmost
    // surface, so it must claim keys before Browse's letter actions can act on
    // a row the operator can no longer see.
    if ui.mcp.inspector.is_some() {
        return Some(handle_mcp_inspector_key(key, ui));
    }
    match ui.mcp.mode {
        McpMode::Browse => handle_mcp_browse_key(key, ui, composer_empty),
        McpMode::Search => Some(handle_mcp_search_key(key, ui)),
        McpMode::Auth => Some(handle_mcp_auth_key(key, ui)),
    }
}

/// Keys while the ctrl+o inspector is up: scroll, ask the registry, close.
///
/// Modal — everything else is swallowed rather than falling through to the
/// list, so a stray `x` cannot remove the server whose detail is on screen.
fn handle_mcp_inspector_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    let Some(inspector) = ui.mcp.inspector.as_mut() else {
        return DeckAction::Handled;
    };
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            ui.mcp.inspector = None;
            DeckAction::Handled
        }
        KeyCode::Up | KeyCode::Char('k') => {
            inspector.scroll = inspector.scroll.saturating_sub(1);
            DeckAction::Handled
        }
        KeyCode::Down | KeyCode::Char('j') => {
            // Clamped to content at render time — the view knows the height,
            // this handler does not.
            inspector.scroll = inspector.scroll.saturating_add(1);
            DeckAction::Handled
        }
        KeyCode::PageUp => {
            inspector.scroll = inspector.scroll.saturating_sub(10);
            DeckAction::Handled
        }
        KeyCode::PageDown => {
            inspector.scroll = inspector.scroll.saturating_add(10);
            DeckAction::Handled
        }
        // Ask the registry for a description this server does not carry. Only
        // when it could help: re-asking after a "no such server" would spend a
        // round-trip to redisplay the same answer.
        KeyCode::Char('r') => {
            let helps = inspector
                .detail
                .as_ref()
                .is_some_and(crate::envelope::McpServerDetail::lookup_would_help);
            if !helps {
                return DeckAction::Handled;
            }
            DeckAction::Send(WorkspaceInput::McpInspect {
                name: inspector.server.clone(),
                lookup: true,
            })
        }
        _ => DeckAction::Handled,
    }
}

fn handle_mcp_browse_key(
    key: KeyEvent,
    ui: &mut DeckUi,
    composer_empty: bool,
) -> Option<DeckAction> {
    let count = ui.mcp.servers.len();
    // Ctrl-chords are matched before the bare-letter actions below: `o` alone
    // starts an OAuth login, so a ctrl+o arm placed after it would never be
    // reached with an empty composer.
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        // Open the inspector for the highlighted server. A Ctrl-chord, not a
        // bare letter, matching SKILLS' ctrl+o preview — and unlike the letter
        // actions it works with a half-typed prompt in the composer.
        if matches!(key.code, KeyCode::Char('o')) {
            let server = ui.mcp.selected_server()?.name.clone();
            ui.mcp.inspector = Some(McpInspector {
                server: server.clone(),
                detail: None,
                scroll: 0,
            });
            // `lookup: false` — opening the inspector must not contact a
            // third-party registry. `r` inside it opts in.
            return Some(DeckAction::Send(WorkspaceInput::McpInspect {
                name: server,
                lookup: false,
            }));
        }
        return None;
    }
    match key.code {
        KeyCode::Up => {
            ui.mcp.selected = ui.mcp.selected.saturating_sub(1);
            Some(DeckAction::Handled)
        }
        KeyCode::Down => {
            if count > 0 {
                ui.mcp.selected = (ui.mcp.selected + 1).min(count - 1);
            }
            Some(DeckAction::Handled)
        }
        // Enter registry-search mode. `s` sits with the tab's other letter
        // actions (e/a/x/r, all gated on an empty composer). `/` deliberately
        // does NOT enter search anymore: it belongs to the command menu
        // everywhere — `/mcp-search` in that menu lands here too.
        KeyCode::Char('s') if composer_empty => {
            ui.mcp.mode = McpMode::Search;
            ui.mcp.status = None;
            Some(DeckAction::Handled)
        }
        // Enable/disable the selected server (session-scoped, live).
        KeyCode::Char('e') | KeyCode::Char(' ') if composer_empty => {
            ui.mcp.selected_server().map(|s| {
                DeckAction::Send(WorkspaceInput::McpToggle {
                    name: s.name.clone(),
                })
            })
        }
        // Enter the auth prompt for the selected server, prefilled with its
        // first configured credential field (if any).
        KeyCode::Char('a') if composer_empty => {
            let server = ui.mcp.selected_server()?;
            let name = server.name.clone();
            let field = server.auth_fields.first().cloned().unwrap_or_default();
            ui.mcp.auth = AuthPrompt {
                server: name,
                field,
                value: String::new(),
                step: AuthStep::Field,
            };
            ui.mcp.mode = McpMode::Auth;
            Some(DeckAction::Handled)
        }
        // Start the browser OAuth login for the selected server. Http-only —
        // a stdio server has no authorization server, so the key explains
        // instead of firing.
        KeyCode::Char('o') if composer_empty => {
            let server = ui.mcp.selected_server()?;
            let name = server.name.clone();
            if server.oauth.is_none() {
                ui.mcp.status = Some(format!(
                    "{name}: OAuth login applies to http servers (use `a` for env credentials)"
                ));
                return Some(DeckAction::Handled);
            }
            ui.mcp.status = Some(format!("{name}: starting OAuth login…"));
            Some(DeckAction::Send(WorkspaceInput::McpOauthLogin {
                server: name,
            }))
        }
        // Remove the selected server from mcp.toml.
        KeyCode::Char('x') if composer_empty => ui.mcp.selected_server().map(|s| {
            DeckAction::Send(WorkspaceInput::McpRemove {
                name: s.name.clone(),
            })
        }),
        // Rebuild the snapshot.
        KeyCode::Char('r') if composer_empty => Some(DeckAction::Send(WorkspaceInput::McpRefresh)),
        _ => None,
    }
}

fn handle_mcp_search_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    match key.code {
        KeyCode::Esc => {
            ui.mcp.mode = McpMode::Browse;
            ui.mcp.searching = false;
            DeckAction::Handled
        }
        KeyCode::Backspace => {
            ui.mcp.query.pop();
            DeckAction::Handled
        }
        KeyCode::Up => {
            ui.mcp.search_selected = ui.mcp.search_selected.saturating_sub(1);
            DeckAction::Handled
        }
        KeyCode::Down => {
            let items = ui.mcp.search.as_ref().map(|o| o.items.len()).unwrap_or(0);
            if items > 0 {
                ui.mcp.search_selected = (ui.mcp.search_selected + 1).min(items - 1);
            }
            DeckAction::Handled
        }
        KeyCode::Enter => {
            // Results already match the query → Enter installs the highlight;
            // otherwise Enter runs the search.
            if ui.mcp.results_match_query() {
                match ui.mcp.selected_search_name().map(str::to_string) {
                    Some(name) => {
                        ui.mcp.status = Some(format!("installing {name}…"));
                        // Drop back to the Browse list so the refreshed
                        // installed-servers snapshot (pushed once the install
                        // lands) is actually on screen — Search mode would
                        // otherwise hide it behind now-stale results.
                        ui.mcp.mode = McpMode::Browse;
                        DeckAction::Send(WorkspaceInput::McpInstall { name })
                    }
                    None => DeckAction::Handled,
                }
            } else {
                let query = ui.mcp.query.trim().to_string();
                if query.is_empty() {
                    return DeckAction::Handled;
                }
                ui.mcp.searching = true;
                ui.mcp.search = None;
                DeckAction::Send(WorkspaceInput::McpSearch { query })
            }
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            ui.mcp.query.push(c);
            DeckAction::Handled
        }
        // Modal: swallow everything else so nothing leaks to the composer.
        _ => DeckAction::Handled,
    }
}

fn handle_mcp_auth_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    match key.code {
        KeyCode::Esc => {
            ui.mcp.mode = McpMode::Browse;
            ui.mcp.auth = AuthPrompt::default();
            DeckAction::Handled
        }
        KeyCode::Enter => match ui.mcp.auth.step {
            AuthStep::Field => {
                if ui.mcp.auth.field.trim().is_empty() {
                    return DeckAction::Handled;
                }
                ui.mcp.auth.step = AuthStep::Value;
                DeckAction::Handled
            }
            AuthStep::Value => {
                let server = ui.mcp.auth.server.clone();
                let field = ui.mcp.auth.field.trim().to_string();
                let value = std::mem::take(&mut ui.mcp.auth.value);
                ui.mcp.mode = McpMode::Browse;
                ui.mcp.auth = AuthPrompt::default();
                ui.mcp.status = Some(format!("set credential {field} for {server}"));
                DeckAction::Send(WorkspaceInput::McpAuth {
                    server,
                    field,
                    value: Secret::new(value),
                })
            }
        },
        KeyCode::Backspace => {
            match ui.mcp.auth.step {
                AuthStep::Field => {
                    ui.mcp.auth.field.pop();
                }
                AuthStep::Value => {
                    ui.mcp.auth.value.pop();
                }
            }
            DeckAction::Handled
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            match ui.mcp.auth.step {
                AuthStep::Field => ui.mcp.auth.field.push(c),
                AuthStep::Value => ui.mcp.auth.value.push(c),
            }
            DeckAction::Handled
        }
        _ => DeckAction::Handled,
    }
}
