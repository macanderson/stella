//! **The witness (#4368).** The MCP tab's browse verbs that had none: `o`
//! (OAuth login), `x` (remove the server) and `r` (refresh), pressed through
//! [`super::handle_deck_key`].
//!
//! `space / e`, `ctrl-o` and `s` are witnessed in `queue.rs`
//! (`mcp_tab_navigates_toggles_and_enters_search`,
//! `ctrl_o_opens_the_mcp_inspector_for_the_highlighted_server`); the three
//! below reached the wire with nothing pressing them, so the keymap row was a
//! claim about an arm no test had ever entered.

use super::*;
use crate::envelope::McpServerInfo;

fn server(name: &str, oauth: Option<bool>) -> McpServerInfo {
    McpServerInfo {
        name: name.into(),
        kind: if oauth.is_some() { "http" } else { "stdio" }.into(),
        enabled: true,
        connected: true,
        oauth,
        ..Default::default()
    }
}

fn mcp_ui(servers: Vec<McpServerInfo>) -> (WorkspaceModel, DeckUi) {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.set_tab(DeckTab::Mcp);
    ui.mcp.servers = servers;
    (model, ui)
}

/// `o` starts the browser OAuth login for the highlighted server — and on a
/// stdio server, which has no authorization server to log into, says so
/// instead of sending a request that cannot be answered.
#[test]
fn mcp_o_starts_an_oauth_login_and_explains_itself_on_a_stdio_server() {
    let (model, mut ui) = mcp_ui(vec![server("github", Some(false)), server("fs", None)]);

    assert_eq!(
        handle_deck_key(ch('o'), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::McpOauthLogin {
            server: "github".into()
        })
    );

    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    assert_eq!(
        handle_deck_key(ch('o'), &model, &mut ui),
        DeckAction::Handled,
        "a stdio server has no OAuth to start"
    );
    let status = ui.mcp.status.clone().unwrap_or_default();
    assert!(
        status.contains("http servers"),
        "the key says why it did nothing: {status:?}"
    );
}

/// `x` removes the highlighted server from `mcp.toml`, and `r` rebuilds the
/// snapshot. Both are letter verbs, so both stand down for a prompt in
/// progress rather than firing under a half-typed word.
#[test]
fn mcp_x_removes_the_selected_server_and_r_refreshes() {
    let (model, mut ui) = mcp_ui(vec![server("github", Some(true))]);

    assert_eq!(
        handle_deck_key(ch('x'), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::McpRemove {
            name: "github".into()
        })
    );
    assert_eq!(
        handle_deck_key(ch('r'), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::McpRefresh)
    );

    handle_deck_key(ch('z'), &model, &mut ui);
    for verb in ['o', 'x', 'r'] {
        handle_deck_key(ch(verb), &model, &mut ui);
    }
    assert_eq!(
        ui.composer.buffer(),
        "zoxr",
        "with a prompt in progress the verbs are letters"
    );
}
