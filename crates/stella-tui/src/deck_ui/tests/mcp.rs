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
use crate::envelope::{McpServerDetail, McpServerInfo, McpToolRow};
use crate::views::mcp_tab::McpMode;

fn server(name: &str, oauth: Option<bool>) -> McpServerInfo {
    McpServerInfo {
        name: name.into(),
        kind: if oauth.is_some() { "http" } else { "stdio" }.into(),
        enabled: true,
        connected: true,
        granted: true,
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

/// **The witness (#5047).** SPEC §9.3: the first enable of a server shows what
/// it declares and requires the grant. `e` on an ungranted row must NOT reach
/// the wire as a toggle — it opens the handshake and asks the driver for the
/// declared capabilities instead.
///
/// The `assert_ne` is the point: the old arm sent `McpToggle` for every row,
/// so enabling an unreviewed third-party server was one keystroke with nothing
/// in between.
#[test]
fn e_on_an_ungranted_server_opens_the_handshake_instead_of_enabling_it() {
    let mut fresh = server("stripe", Some(false));
    fresh.granted = false;
    let (model, mut ui) = mcp_ui(vec![fresh]);

    let action = handle_deck_key(ch('e'), &model, &mut ui);
    assert_ne!(
        action,
        DeckAction::Send(WorkspaceInput::McpToggle {
            name: "stripe".into()
        }),
        "an unreviewed server must not be enabled by one keystroke"
    );
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::McpInspect {
            name: "stripe".into(),
            // Reviewing what a server declares must not call a registry.
            lookup: false,
        })
    );
    assert_eq!(ui.mcp.mode, McpMode::Handshake);
    assert_eq!(
        ui.mcp.handshake.as_ref().map(|g| g.server.as_str()),
        Some("stripe")
    );

    // The grant stands down until there is something to grant: a decision
    // about capabilities nobody was shown is not a decision.
    assert_eq!(
        handle_deck_key(ch('g'), &model, &mut ui),
        DeckAction::Handled
    );
    assert_eq!(ui.mcp.mode, McpMode::Handshake, "still asking");

    ui.mcp.apply_detail(McpServerDetail {
        name: "stripe".into(),
        tools: vec![McpToolRow {
            name: "create_refund".into(),
            ..McpToolRow::default()
        }],
        ..McpServerDetail::default()
    });
    assert_eq!(
        handle_deck_key(ch('g'), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::McpGrant {
            name: "stripe".into()
        })
    );
    assert_eq!(ui.mcp.mode, McpMode::Browse);
    assert!(ui.mcp.handshake.is_none());
}

/// Walking away from the gate grants nothing. Esc is the only exit that is not
/// the grant, and it must reach no wire — the withheld state is the default,
/// so denying costs no message.
#[test]
fn esc_denies_the_handshake_and_sends_nothing() {
    let mut fresh = server("stripe", Some(false));
    fresh.granted = false;
    let (model, mut ui) = mcp_ui(vec![fresh]);
    handle_deck_key(ch('e'), &model, &mut ui);

    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        DeckAction::Handled
    );
    assert_eq!(ui.mcp.mode, McpMode::Browse);
    assert!(ui.mcp.handshake.is_none());
}

/// The gate is modal: a stray letter while it is up must not act on the list
/// behind it. `x` there would remove the very server being reviewed.
#[test]
fn the_handshake_swallows_the_browse_verbs_behind_it() {
    let mut fresh = server("stripe", Some(false));
    fresh.granted = false;
    let (model, mut ui) = mcp_ui(vec![fresh]);
    handle_deck_key(ch('e'), &model, &mut ui);

    for verb in ['x', 'o', 'a', 'r', 's', 'e'] {
        assert_eq!(
            handle_deck_key(ch(verb), &model, &mut ui),
            DeckAction::Handled,
            "`{verb}` reached the list behind the handshake"
        );
    }
    assert_eq!(ui.mcp.mode, McpMode::Handshake);
}
