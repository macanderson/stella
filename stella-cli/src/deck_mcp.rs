//! The deck's MCP-tab driver: everything behind the tab's verbs.
//!
//! Split out of `command_deck.rs` (#629's 1500-line ratchet) when the tab
//! grew an inspector. Three sources are joined here and nowhere else:
//!
//! - **`.stella/mcp.toml`** — the transport and the publisher's
//!   [`stella_mcp::ServerCard`] (what the server *is*, recorded at install).
//! - **The live [`stella_mcp::McpToolSet`]** — which servers connected, what
//!   they said about themselves at handshake, and every tool they advertise.
//! - **Local telemetry** (`.stella/private/store.db`) — per-tool call counts.
//!
//! No single one of those can answer "what is this server and is it working",
//! which is why the read models in [`stella_tui::McpServerInfo`] /
//! [`stella_tui::McpServerDetail`] are assembled rather than passed through.
//!
//! Nothing here renders a credential *value*: card text and endpoints are
//! shown, credential field **names** are shown, and the values stay in the
//! transport where the redacted `Debug` keeps them out of every log.

use std::collections::{HashMap, HashSet};

use stella_core::ports::ToolExecutor as _;
use stella_tui::{Inbound, McpLookupState, WorkspaceInput};
use tokio::sync::mpsc;

use crate::command_deck::chrome_note;
use crate::config::Config;

/// Build the MCP tab snapshot: every configured server (`.stella/mcp.toml`)
/// joined with its live session state — enabled (not in the disabled set),
/// connected (in the live tool set), health, per-server tool count (derived
/// from the advertised schemas, so it is 0 the moment a server is disabled),
/// configured credential field names, and total recorded tool calls.
///
/// Also carries each row's *identity*: the recorded title and description, and
/// the endpoint. A row keyed only on the alias is unreadable — an alias is a
/// sanitized last path segment, so `com.stripe/mcp` shows up as `mcp` and the
/// list says nothing about what any server is for.
pub(crate) async fn mcp_snapshot(
    cfg: &Config,
    mcp: Option<&stella_mcp::McpToolSet>,
    disabled: &stella_mcp::DisabledServers,
) -> Result<Vec<stella_tui::McpServerInfo>, String> {
    let config = crate::mcp_cmd::load_config(&cfg.workspace_root)?;
    let connected: HashSet<String> = mcp
        .map(|s| {
            s.connected_names()
                .into_iter()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let health = match mcp {
        Some(s) => s.health().await,
        None => Vec::new(),
    };
    let schemas = mcp.map(|s| s.schemas()).unwrap_or_default();
    let dropped = crate::mcp_cmd::dropped_by_server(mcp);
    let usage = crate::mcp_cmd::usage_stats(&cfg.workspace_root)?;
    let disabled_set = disabled.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let oauth_logins: HashSet<String> = crate::mcp_cmd::oauth_logged_in(&cfg.workspace_root)?
        .into_iter()
        .collect();

    Ok(config
        .names()
        .into_iter()
        .map(|name| {
            let transport = config.get(name).expect("name came from the config");
            let card = config.card(name).expect("name came from the config");
            let enabled = !disabled_set.contains(name);
            let connected_now = connected.contains(name);
            let prefix = format!("mcp__{name}__");
            let tool_count = schemas
                .iter()
                .filter(|s| s.name.starts_with(&prefix))
                .count();
            let health = crate::mcp_cmd::health_label(&health, name);
            let calls: u64 = usage
                .iter()
                .filter(|s| s.server == name)
                .map(|s| s.calls.max(0) as u64)
                .sum();
            // The live handshake is the fallback identity for a server that
            // was never installed from a registry — a hand-written entry has
            // no card at all, and its own `serverInfo` is then the only name
            // it has ever offered.
            let live = mcp.and_then(|s| s.identity(name));
            let title = card
                .title
                .clone()
                .or_else(|| live.as_ref().and_then(|l| l.title.clone()))
                .or_else(|| live.as_ref().and_then(|l| l.name.clone()))
                .filter(|t| t.trim() != name);
            stella_tui::McpServerInfo {
                name: name.to_string(),
                title,
                description: card
                    .description
                    .clone()
                    .or_else(|| live.and_then(|l| l.instructions).map(|i| first_line(&i))),
                endpoint: crate::mcp_cmd::endpoint_summary(transport),
                kind: transport.kind_label().to_string(),
                enabled,
                connected: connected_now,
                health: connected_now.then_some(health).flatten(),
                tool_count,
                dropped_tools: dropped.get(name).copied().unwrap_or(0),
                auth_fields: transport
                    .credential_names()
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                oauth: (transport.kind_label() == "http").then(|| oauth_logins.contains(name)),
                calls,
                candidate_safe: config.is_candidate_safe(name),
            }
        })
        .collect())
}

/// The first non-empty line of a server's `instructions`, bounded for a list
/// row.
///
/// Handshake instructions are prose written for a model, not a summary written
/// for a row — they routinely run to paragraphs. The first line is the closest
/// thing to a description they contain, and the full text is still available
/// in the inspector.
fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.chars().count() <= 160 {
        return line.to_string();
    }
    let head: String = line.chars().take(159).collect();
    format!("{head}…")
}

/// Build and push a fresh MCP tab snapshot.
pub(crate) async fn send_mcp_snapshot(
    cfg: &Config,
    mcp: Option<&stella_mcp::McpToolSet>,
    disabled: &stella_mcp::DisabledServers,
    in_tx: &mpsc::UnboundedSender<Inbound>,
) {
    match mcp_snapshot(cfg, mcp, disabled).await {
        Ok(rows) => {
            let _ = in_tx.send(Inbound::McpServers(rows));
        }
        Err(error) => {
            let _ = in_tx.send(Inbound::McpServers(Vec::new()));
            let _ = in_tx.send(Inbound::McpOauthStatus {
                server: "MCP state".to_string(),
                message: error,
                outcome: Some(false),
            });
        }
    }
}

/// Assemble one server's inspector detail from config + live session +
/// telemetry.
///
/// `lookup` is carried through as the *state* the view should show while a
/// registry round-trip runs; this function never performs one itself, so the
/// inspector always opens on locally-known facts at the speed of a keystroke.
pub(crate) async fn mcp_detail(
    cfg: &Config,
    mcp: Option<&stella_mcp::McpToolSet>,
    disabled: &stella_mcp::DisabledServers,
    name: &str,
    lookup: McpLookupState,
) -> Result<stella_tui::McpServerDetail, String> {
    let config = crate::mcp_cmd::load_config(&cfg.workspace_root)?;
    let transport = config
        .get(name)
        .ok_or_else(|| format!("`{name}` is not configured in .stella/mcp.toml"))?;
    let card = config.card(name).expect("name resolved above");
    let connected = mcp.is_some_and(|s| s.connected_names().contains(&name));
    let health = match mcp {
        Some(s) => crate::mcp_cmd::health_label(&s.health().await, name),
        None => None,
    };
    let usage = crate::mcp_cmd::usage_stats(&cfg.workspace_root)?;
    let per_tool: HashMap<&str, u64> = usage
        .iter()
        .filter(|s| s.server == name)
        .map(|s| (s.tool.as_str(), s.calls.max(0) as u64))
        .collect();
    let disabled_now = disabled
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(name);
    let oauth_logins = crate::mcp_cmd::oauth_logged_in(&cfg.workspace_root)?;

    let live = mcp
        .and_then(|s| s.identity(name))
        .map(|i| stella_tui::McpLiveIdentity {
            name: i.name,
            version: i.version,
            title: i.title,
            website_url: i.website_url,
            instructions: i.instructions,
            protocol_version: i.protocol_version,
        });
    // Sorted by name, not by advertise order: the inspector is something an
    // operator scans for a specific tool, and a server's own ordering is
    // arbitrary.
    let mut tools: Vec<stella_tui::McpToolRow> = mcp
        .map(|s| s.advertised_tools(name))
        .unwrap_or_default()
        .iter()
        .map(|t| stella_tui::McpToolRow {
            calls: per_tool.get(t.name.as_str()).copied().unwrap_or(0),
            name: t.name.clone(),
            description: t.description.clone(),
            safe_to_retry: t.safe_to_retry,
        })
        .collect();
    tools.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(stella_tui::McpServerDetail {
        name: name.to_string(),
        title: card.title.clone(),
        description: card.description.clone(),
        registry_name: card.registry_name.clone(),
        repository: card.repository.clone(),
        version: card.version.clone(),
        kind: transport.kind_label().to_string(),
        endpoint: crate::mcp_cmd::endpoint_summary(transport),
        auth_fields: transport
            .credential_names()
            .into_iter()
            .map(str::to_string)
            .collect(),
        oauth: (transport.kind_label() == "http").then(|| oauth_logins.iter().any(|s| s == name)),
        candidate_safe: config.is_candidate_safe(name),
        enabled: !disabled_now,
        connected,
        health: connected.then_some(health).flatten(),
        dropped_tools: crate::mcp_cmd::dropped_by_server(mcp)
            .get(name)
            .copied()
            .unwrap_or(0),
        calls: per_tool.values().sum(),
        live,
        tools,
        lookup,
    })
}

/// Assemble and push one server's inspector detail, optionally consulting the
/// registry for a description the local config does not have.
///
/// The detail is pushed **before** any lookup so the inspector opens
/// immediately, then pushed again with the outcome. A lookup that succeeds
/// also backfills `.stella/mcp.toml`, so the answer is paid for once and the
/// list row carries it from then on.
async fn send_mcp_detail(
    cfg: &Config,
    mcp: Option<&stella_mcp::McpToolSet>,
    disabled: &stella_mcp::DisabledServers,
    name: &str,
    lookup: bool,
    in_tx: &mpsc::UnboundedSender<Inbound>,
) {
    let initial = match mcp_detail(cfg, mcp, disabled, name, McpLookupState::Idle).await {
        Ok(detail) => detail,
        Err(error) => {
            let _ = in_tx.send(chrome_note(format!("mcp: {error}\n")));
            return;
        }
    };
    // A lookup that cannot change anything is not worth a round-trip — and
    // asking would quietly contact a third party for no gain.
    let wanted = lookup && initial.description.is_none();
    let _ = in_tx.send(Inbound::McpDetail(Box::new(stella_tui::McpServerDetail {
        lookup: if wanted {
            McpLookupState::Fetching
        } else {
            McpLookupState::Idle
        },
        ..initial
    })));
    if !wanted {
        return;
    }

    let registry_url = crate::mcp_cmd::resolve_registry_url(&cfg.workspace_root);
    let state = match crate::mcp_cmd::refresh_card(&cfg.workspace_root, &registry_url, name).await {
        Ok(Some(_)) => McpLookupState::Found,
        Ok(None) => McpLookupState::Missing,
        Err(error) => McpLookupState::Failed(error),
    };
    // Re-assemble rather than patch: the backfill wrote to `mcp.toml`, so the
    // whole detail (and the snapshot below) must be read back from it.
    match mcp_detail(cfg, mcp, disabled, name, state).await {
        Ok(detail) => {
            let _ = in_tx.send(Inbound::McpDetail(Box::new(detail)));
        }
        Err(error) => {
            let _ = in_tx.send(chrome_note(format!("mcp: {error}\n")));
        }
    }
    send_mcp_snapshot(cfg, mcp, disabled, in_tx).await;
}

/// Run a registry search and shape it for the tab, flagging already-configured
/// servers and deduping the registry's per-version rows to one per name.
async fn run_mcp_search(cfg: &Config, query: &str) -> stella_tui::McpSearchOutcome {
    let query = query.trim().to_string();
    let configured: HashSet<String> = crate::mcp_cmd::load_config(&cfg.workspace_root)
        .map(|c| c.names().into_iter().map(str::to_string).collect())
        .unwrap_or_default();
    let registry_url = crate::mcp_cmd::resolve_registry_url(&cfg.workspace_root);
    match crate::mcp_cmd::search(&registry_url, Some(&query), None, 20).await {
        Ok(page) => {
            let mut seen = HashSet::new();
            let items = page
                .entries
                .into_iter()
                .filter(|e| seen.insert(e.server.name.clone()))
                .map(|e| {
                    let alias = e.server.default_alias();
                    stella_tui::McpSearchItem {
                        installed: configured.contains(&e.server.name)
                            || configured.contains(&alias),
                        kinds: crate::mcp_cmd::install_kinds(&e.server),
                        description: e.server.description.clone().unwrap_or_default(),
                        name: e.server.name,
                    }
                })
                .collect();
            stella_tui::McpSearchOutcome {
                query,
                items,
                error: None,
                has_more: page.next_cursor.is_some(),
            }
        }
        Err(error) => stella_tui::McpSearchOutcome {
            query,
            items: Vec::new(),
            error: Some(error),
            has_more: false,
        },
    }
}

/// Service one MCP-tab action from the deck. Returns `true` if `input` was an
/// MCP verb (so the caller skips its own dispatch). Search/install/remove/auth
/// touch `.stella/mcp.toml` (and, for search, the registry over HTTP); toggle
/// flips the shared disabled set that the tool layer consults live.
pub(crate) async fn service_mcp_action(
    input: &WorkspaceInput,
    cfg: &Config,
    mcp: Option<&stella_mcp::McpToolSet>,
    disabled: &stella_mcp::DisabledServers,
    in_tx: &mpsc::UnboundedSender<Inbound>,
) -> bool {
    match input {
        WorkspaceInput::McpToggle { name } => {
            {
                let mut set = disabled.lock().unwrap_or_else(|p| p.into_inner());
                if !set.remove(name) {
                    set.insert(name.clone());
                }
            }
            send_mcp_snapshot(cfg, mcp, disabled, in_tx).await;
        }
        WorkspaceInput::McpRefresh => send_mcp_snapshot(cfg, mcp, disabled, in_tx).await,
        WorkspaceInput::McpInspect { name, lookup } => {
            send_mcp_detail(cfg, mcp, disabled, name, *lookup, in_tx).await;
        }
        WorkspaceInput::McpRemove { name } => {
            match crate::mcp_cmd::remove(&cfg.workspace_root, name) {
                Ok(true) => {}
                // Not an error, but not what the operator asked for either:
                // the row is gone from the tab and `.stella/mcp.toml` never
                // had it. Silence here reads as success.
                Ok(false) => {
                    let _ = in_tx.send(chrome_note(format!(
                        "mcp: `{name}` was not configured in .stella/mcp.toml — nothing removed\n"
                    )));
                }
                Err(e) => {
                    let _ = in_tx.send(chrome_note(format!(
                        "mcp: could not remove `{name}`: {e}\n"
                    )));
                }
            }
            send_mcp_snapshot(cfg, mcp, disabled, in_tx).await;
        }
        WorkspaceInput::McpAuth {
            server,
            field,
            value,
        } => {
            if let Err(e) = crate::mcp_cmd::set_credential(
                &cfg.workspace_root,
                server,
                field,
                value.reveal().to_string(),
            ) {
                // The single worst failure to swallow on this tab: the user
                // typed a secret at a masked prompt, saw the tab refresh, and
                // had no way to know it never reached disk.
                let _ = in_tx.send(chrome_note(format!(
                    "mcp: could not store the `{field}` credential for `{server}`: {e}\n"
                )));
            }
            send_mcp_snapshot(cfg, mcp, disabled, in_tx).await;
        }
        WorkspaceInput::McpSearch { query } => {
            let outcome = run_mcp_search(cfg, query).await;
            let _ = in_tx.send(Inbound::McpSearchResults(outcome));
        }
        WorkspaceInput::McpInstall { name } => {
            let registry_url = crate::mcp_cmd::resolve_registry_url(&cfg.workspace_root);
            // Both halves of an install can fail for reasons the operator can
            // act on — an unreachable registry, a name that is not in it, a
            // read-only `.stella/mcp.toml`. Every one of them used to end with
            // the tab refreshing unchanged and no explanation anywhere.
            let outcome = match crate::mcp_cmd::resolve_install(&registry_url, name).await {
                Ok((alias, option, card)) => {
                    crate::mcp_cmd::install(&cfg.workspace_root, &alias, option.transport, card)
                }
                Err(e) => Err(e),
            };
            if let Err(e) = outcome {
                let _ = in_tx.send(chrome_note(format!(
                    "mcp: could not install `{name}`: {e}\n"
                )));
            }
            send_mcp_snapshot(cfg, mcp, disabled, in_tx).await;
        }
        WorkspaceInput::McpOauthLogin { server } => {
            spawn_mcp_oauth_login(server.clone(), cfg.workspace_root.clone(), in_tx.clone());
        }
        _ => return false,
    }
    true
}

/// Run the browser OAuth login for an http MCP server in the background.
/// Progress streams to the MCP tab's status line; the authorize URL and the
/// final outcome also land in the persist-until-read inbox (the browser may
/// have opened on another screen, and the tab may not be visible).
pub(crate) fn spawn_mcp_oauth_login(
    server: String,
    workspace_root: std::path::PathBuf,
    in_tx: mpsc::UnboundedSender<Inbound>,
) {
    tokio::spawn(async move {
        let inbox = stella_store::NotificationStore::open_default();
        let progress_tx = in_tx.clone();
        let progress_server = server.clone();
        let progress_inbox = inbox.clone();
        let mut on_event = move |event: stella_mcp::LoginEvent| {
            let message = match event {
                stella_mcp::LoginEvent::Status(line) => line,
                stella_mcp::LoginEvent::AuthorizeUrl(url) => {
                    let _ = progress_inbox.push(&stella_store::Notification::new(
                        format!("MCP OAuth: approve `{progress_server}` in your browser"),
                        url.clone(),
                        progress_server.clone(),
                    ));
                    format!("approve in your browser: {url}")
                }
            };
            let _ = progress_tx.send(Inbound::McpOauthStatus {
                server: progress_server.clone(),
                message,
                outcome: None,
            });
        };
        let result = crate::mcp_cmd::oauth_login(&workspace_root, &server, &mut on_event).await;
        let (message, ok) = match result {
            Ok(()) => ("logged in — tokens auto-refresh".to_string(), true),
            Err(e) => (e, false),
        };
        let title = if ok {
            format!("MCP OAuth: `{server}` logged in")
        } else {
            format!("MCP OAuth: `{server}` login failed")
        };
        let _ = inbox.push(&stella_store::Notification::new(
            title,
            message.clone(),
            server.clone(),
        ));
        let _ = in_tx.send(Inbound::McpOauthStatus {
            server,
            message,
            outcome: Some(ok),
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_line_takes_the_first_non_empty_line_and_bounds_it() {
        assert_eq!(
            first_line("\n\n  Charge cards.\nMore prose.\n"),
            "Charge cards."
        );
        assert_eq!(first_line(""), "");
        let long = "x".repeat(400);
        let short = first_line(&long);
        assert_eq!(short.chars().count(), 160, "160 chars: 159 + the ellipsis");
        assert!(short.ends_with('…'));
    }
}
