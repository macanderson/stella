//! MCP management orchestration: the shared logic behind both the `stella mcp`
//! subcommand and the deck's MCP tab. It owns *where* `.stella/mcp.toml` lives
//! and the registry-URL resolution; `stella-mcp` owns the transport shapes, the
//! registry client, and the install mapping.
//!
//! Nothing here logs a credential value: config is written to disk (the
//! pre-existing `mcp.toml` convention, owner-only where the platform allows),
//! and the [`stella_mcp::McpTransport`] `Debug` redacts values, so a diagnostic
//! never leaks a token.

use std::path::{Path, PathBuf};

use colored::Colorize;
use stella_mcp::{
    InstallOption, McpConfig, McpTransport, RegistryClient, RegistryPage, ServerCard,
};

use crate::settings::Settings;

/// The workspace's MCP server config file.
pub fn mcp_toml_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".stella").join("mcp.toml")
}

/// Load `.stella/mcp.toml` (an absent file is an empty config, not an error).
pub fn load_config(workspace_root: &Path) -> Result<McpConfig, String> {
    let path = mcp_toml_path(workspace_root);
    match stella_store::read_sensitive_file_to_string(&path) {
        Ok(text) => McpConfig::from_toml_str(&text)
            .map_err(|e| format!("{} is invalid: {e}", path.display())),
        Err(_)
            if matches!(
                std::fs::symlink_metadata(&path),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound
            ) =>
        {
            Ok(McpConfig::default())
        }
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Write `.stella/mcp.toml` atomically (temp + rename), owner-only on Unix
/// since it may hold credentials.
pub fn save_config(workspace_root: &Path, cfg: &McpConfig) -> Result<(), String> {
    let path = mcp_toml_path(workspace_root);
    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let toml = cfg.to_toml_string().map_err(|e| e.to_string())?;
    stella_store::write_sensitive_file_atomic(&path, toml.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// The configured MCP registry URL (settings.json `mcp.registry_url`, else the
/// official default).
pub fn resolve_registry_url(workspace_root: &Path) -> String {
    Settings::load(workspace_root)
        .map(|s| s.mcp_registry_url())
        .unwrap_or_else(|_| stella_mcp::DEFAULT_REGISTRY_URL.to_string())
}

/// Deck-mode report for MCP connection outcomes, including total failure.
///
/// `truncated` carries the servers whose advertised tool list was cut at
/// [`stella_mcp::MAX_TOOLS_PER_SERVER`], as `(name, dropped)`. It is reported
/// separately from `failed` and never merged into it: those servers connected
/// and their kept tools route normally, so calling them "unavailable" would be
/// false. Without this the cap is entirely silent — the model simply has fewer
/// tools than the server offers and nothing says so (#689).
/// `budgeted` carries the servers whose tools were trimmed to fit
/// [`stella_mcp::MAX_SERVER_SCHEMA_BYTES`] — a different wall from `truncated`
/// and reported in its own words (#3722). A server can trip either one without
/// the other: three hundred terse tools trip the count cap, twelve verbose ones
/// trip the byte budget, and an operator who cannot tell them apart cannot tell
/// which knob to turn.
/// `collisions` carries the wire names more than one `(server, tool)` pair
/// claimed (#2675); every claimant's route was dropped rather than letting
/// connect order pick which server answers. Reported separately from `failed`
/// for the same reason truncation is: the claimant servers are connected and
/// their uncontested tools route normally.
pub(crate) fn mcp_outcome_report(
    connected: &[&str],
    failed: &[(String, String)],
    truncated: &[(&str, usize)],
    budgeted: &[(String, usize)],
    collisions: &[stella_mcp::WireNameCollision],
) -> String {
    let mut lines = match connected.len() {
        0 => vec!["no MCP servers connected — continuing with native tools only".to_string()],
        n => vec![format!(
            "{n} MCP server(s) connected: {}",
            connected.join(", ")
        )],
    };
    lines.extend(
        failed
            .iter()
            .map(|(name, reason)| format!("MCP server `{name}` unavailable: {reason}")),
    );
    lines.extend(
        truncated
            .iter()
            .map(|(name, dropped)| truncation_note(name, *dropped)),
    );
    lines.extend(
        budgeted
            .iter()
            .map(|(name, trimmed)| budget_note(name, *trimmed)),
    );
    lines.extend(collisions.iter().map(collision_note));
    lines.join("\n")
}

/// The whole connect outcome for a live tool set, as one notice.
///
/// The deck sends this straight to the chrome; unpacking the five accessors at
/// the call site put the join in `command_deck.rs`, where a sixth diagnostic
/// meant editing a god file to add it.
pub(crate) fn mcp_connect_report(set: &stella_mcp::McpToolSet) -> String {
    mcp_outcome_report(
        &set.connected_names(),
        set.failed_servers(),
        &set.over_advertising_servers(),
        &set.over_budget_servers(),
        set.wire_name_collisions(),
    )
}

/// One contested wire name's notice, shared by deck and text mode so the
/// wording cannot drift between them. Names every claimant: the operator's
/// next move is to rename one of those servers, so the sentence must say
/// which ones are fighting over the name.
pub(crate) fn collision_note(collision: &stella_mcp::WireNameCollision) -> String {
    let claimants: Vec<String> = collision
        .claimants
        .iter()
        .map(|(server, tool)| format!("`{server}` tool `{tool}`"))
        .collect();
    format!(
        "MCP wire name `{}` is claimed by multiple servers ({}) — every claimant \
         dropped and not callable this session; rename one of the servers",
        collision.wire_name,
        claimants.join(", ")
    )
}

/// One server's truncation notice, shared by deck and text mode so the wording
/// cannot drift between them.
///
/// The cap is interpolated from [`stella_mcp::MAX_TOOLS_PER_SERVER`] rather
/// than spelled out, so the sentence cannot outlive a change to the constant.
/// "at least" is required: discovery stops on the page where the cap bites,
/// so `dropped` is a floor and the true excess may be larger.
pub(crate) fn truncation_note(name: &str, dropped: usize) -> String {
    format!(
        "MCP server `{name}` advertised more than the {}-tool cap — at least \
         {dropped} tool(s) dropped and not callable this session",
        stella_mcp::MAX_TOOLS_PER_SERVER
    )
}

/// One server's schema-budget notice, shared by deck and text mode.
///
/// Deliberately worded apart from [`truncation_note`]: that one reports the
/// per-server tool COUNT cap, this one the per-server schema BYTE budget. A
/// server that advertises a dozen tools with enormous input schemas trips this
/// and never comes near the count cap, so a reader told only "tools dropped"
/// would go looking for the wrong limit. "at least" is not needed here — the
/// budget sees the whole advertised list and counts every tool it cuts.
pub(crate) fn budget_note(name: &str, trimmed: usize) -> String {
    format!(
        "MCP server `{name}` advertised more tool schema than the {}-byte \
         per-server budget — {trimmed} tool(s) trimmed to fit and not callable \
         this session",
        stella_mcp::MAX_SERVER_SCHEMA_BYTES
    )
}

/// Text-mode connection diagnostics for a one-shot run.
///
/// Lives here rather than inline in `agent.rs` so the wording is testable: the
/// call site prints with bare `eprintln!`/`println!` and has no injectable
/// sink, so anything built there is unreachable from a test.
pub(crate) fn print_connect_diagnostics(set: &stella_mcp::McpToolSet) {
    for (name, reason) in set.failed_servers() {
        eprintln!(
            "  {} MCP server `{name}` unavailable: {reason}",
            "!".yellow()
        );
    }
    for (name, dropped) in set.over_advertising_servers() {
        eprintln!("  {} {}", "!".yellow(), truncation_note(name, dropped));
    }
    for (name, trimmed) in set.over_budget_servers() {
        eprintln!("  {} {}", "!".yellow(), budget_note(&name, trimmed));
    }
    for collision in set.wire_name_collisions() {
        eprintln!("  {} {}", "!".yellow(), collision_note(collision));
    }
    // Auth-suppressed servers (#2687) are actionable, not broken — say the
    // fix, never "unavailable" (which is what `failed_servers` renders as).
    for (name, _) in set.auth_required_servers() {
        eprintln!(
            "  {} MCP server `{name}` requires authentication — run `stella mcp login {name}`",
            "!".yellow()
        );
    }
    if set.connected_count() > 0 {
        println!(
            "  {} {} MCP server(s) connected",
            "◆".bright_cyan(),
            set.connected_count()
        );
    }
}

/// Per-server dropped-tool counts, keyed by server name.
///
/// Keys are owned because [`stella_mcp::McpToolSet::over_advertising_servers`]
/// borrows the tool set while the deck's row names come from the MCP config —
/// two different borrows that cannot both be live in the snapshot closure.
pub(crate) fn dropped_by_server(
    mcp: Option<&stella_mcp::McpToolSet>,
) -> std::collections::HashMap<String, usize> {
    mcp.map(|s| {
        s.over_advertising_servers()
            .into_iter()
            .map(|(n, c)| (n.to_string(), c))
            .collect()
    })
    .unwrap_or_default()
}

/// Tools trimmed off each server to fit the per-server schema BYTE budget.
///
/// The sibling of [`dropped_by_server`], and deliberately not folded into it:
/// the two report different walls, and the deck's rows name which one a server
/// hit (#4441). [`budget_note`] carries the same distinction in prose.
pub(crate) fn trimmed_by_server(
    mcp: Option<&stella_mcp::McpToolSet>,
) -> std::collections::HashMap<String, usize> {
    mcp.map(|s| s.over_budget_servers().into_iter().collect())
        .unwrap_or_default()
}

/// The deck's short health label for one server, or `None` when it reported no
/// health at all.
pub(crate) fn health_label(health: &[stella_mcp::ServerHealth], name: &str) -> Option<String> {
    health.iter().find(|h| h.name == name).map(|h| {
        match h.state {
            stella_mcp::HealthState::Live => "live",
            stella_mcp::HealthState::Reconnecting => "reconnecting",
            stella_mcp::HealthState::Down => "down",
            stella_mcp::HealthState::AuthRequired => "auth required",
        }
        .to_string()
    })
}

/// The measured `initialize` round trip for one server, in whole
/// milliseconds — SPEC §9.3's latency column.
///
/// `None` when the server reported no health row or has no live connection
/// that measured one. Never rounded up from zero: a sub-millisecond stdio
/// server legitimately reads `0ms`, and that is a measurement, unlike the
/// absent case beside it.
pub(crate) fn latency_ms(health: &[stella_mcp::ServerHealth], name: &str) -> Option<u64> {
    health
        .iter()
        .find(|h| h.name == name)
        .and_then(|h| h.latency)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The session's starting capability grants (SPEC §9.3's first-enable
/// handshake): the names from `servers` whose recorded decision permits use.
///
/// Three sources reach `servers` and each answers the question differently,
/// which is why this reads the decision rather than a flag:
///
/// - **A registry install** writes `granted = false`, so it starts withheld
///   until its handshake is reviewed. That door is the one the gate exists
///   for: a search result is a keystroke away from a `cmd` line this process
///   spawns.
/// - **A hand-written `mcp.toml` entry** records no decision, and is granted.
///   Writing the transport by hand is already the review the gate asks for.
/// - **A plugin-contributed server** is not in `mcp.toml` at all, and is
///   granted for the same reason: installing the plugin was the decision, and
///   there is no entry a grant could even be recorded in.
///
/// A config that will not parse yields an empty set — deny, because the
/// alternative is granting everything on the strength of a file nobody could
/// read.
pub fn initial_grants(
    workspace_root: &Path,
    servers: &[stella_mcp::McpServerConfig],
) -> stella_mcp::CapabilityGrants {
    let cfg = load_config(workspace_root).unwrap_or_default();
    let granted = servers
        .iter()
        .filter(|s| {
            cfg.grant_decision(&s.name)
                .is_none_or(|decision| decision.unwrap_or(true))
        })
        .map(|s| s.name.clone())
        .collect();
    std::sync::Arc::new(std::sync::Mutex::new(granted))
}

/// Record the operator's capability grant for a configured server (SPEC
/// §9.3's first-enable handshake) and return whether the server existed to
/// grant.
///
/// The decision persists in `.stella/mcp.toml`, so it survives the session it
/// was made in — and so a run with no deck attached reads the same answer the
/// deck recorded. The live session's grant set is the caller's to update; this is
/// only the write to disk.
pub fn set_granted(workspace_root: &Path, name: &str, granted: bool) -> Result<bool, String> {
    let mut cfg = load_config(workspace_root)?;
    if !cfg.set_granted(name, granted) {
        return Ok(false);
    }
    save_config(workspace_root, &cfg)?;
    Ok(true)
}

/// Search a registry over HTTP (async, non-blocking).
pub async fn search(
    registry_url: &str,
    query: Option<&str>,
    cursor: Option<&str>,
    limit: u32,
) -> Result<RegistryPage, String> {
    let client = RegistryClient::new(registry_url).map_err(|e| e.to_string())?;
    client
        .search(query, cursor, limit)
        .await
        .map_err(|e| e.to_string())
}

/// Install (or overwrite — MCP servers are not versioned) one server entry,
/// recording the publisher's [`ServerCard`] beside the transport so the local
/// alias is not the only thing the tab can show later.
///
/// A NEW entry lands **ungranted**: nothing it advertises is offered to the
/// model and every call to it is refused until its handshake is reviewed
/// (SPEC §9.3, and `stella_mcp::McpServerEntry::granted`). Re-installing over
/// an existing alias leaves the recorded decision alone — the operator granted
/// *that alias*, and re-fetching the same publisher's transport is not a new
/// question.
pub fn install(
    workspace_root: &Path,
    alias: &str,
    transport: McpTransport,
    card: ServerCard,
) -> Result<(), String> {
    let mut cfg = load_config(workspace_root)?;
    cfg.upsert_with_card(alias, transport, card);
    save_config(workspace_root, &cfg)
}

/// Look a configured server up in the registry and record what it finds — the
/// backfill for an entry written before cards were kept, or one installed by
/// hand.
///
/// Matching is by recorded `registry_name` when there is one, else by the
/// alias, and it is **exact**: a fuzzy match would confidently label a server
/// with another publisher's description, which is worse than the blank it
/// replaces. Returns the card it stored, or `None` when the registry has no
/// entry under either name.
pub async fn refresh_card(
    workspace_root: &Path,
    registry_url: &str,
    alias: &str,
) -> Result<Option<ServerCard>, String> {
    let cfg = load_config(workspace_root)?;
    if cfg.get(alias).is_none() {
        return Err(format!(
            "no MCP server `{alias}` in {}",
            mcp_toml_path(workspace_root).display()
        ));
    }
    let recorded = cfg.card(alias).and_then(|c| c.registry_name.clone());
    let query = recorded.clone().unwrap_or_else(|| alias.to_string());
    let page = search(registry_url, Some(&query), None, 30).await?;
    let found = page.entries.into_iter().find(|e| {
        recorded.as_deref() == Some(e.server.name.as_str())
            || e.server.name == alias
            || e.server.default_alias() == alias
    });
    let Some(entry) = found else {
        return Ok(None);
    };
    let card = entry.server.card();
    // Re-load rather than reuse `cfg`: the registry round-trip above is a
    // network call, and an auth write racing it must not be rolled back by a
    // stale in-memory document.
    let mut cfg = load_config(workspace_root)?;
    if cfg.set_card(alias, card.clone()) {
        save_config(workspace_root, &cfg)?;
    }
    Ok(Some(card))
}

/// Set a credential (env var for stdio, header for http) on a configured
/// server — the auth / re-auth write path. The value is never logged.
pub fn set_credential(
    workspace_root: &Path,
    server: &str,
    field: &str,
    value: String,
) -> Result<(), String> {
    let mut cfg = load_config(workspace_root)?;
    let transport = cfg.get_mut(server).ok_or_else(|| {
        format!(
            "no MCP server `{server}` in {}",
            mcp_toml_path(workspace_root).display()
        )
    })?;
    transport.set_credential(field, value);
    save_config(workspace_root, &cfg)
}

/// Remove a configured server; returns whether it existed.
pub fn remove(workspace_root: &Path, name: &str) -> Result<bool, String> {
    let mut cfg = load_config(workspace_root)?;
    let removed = cfg.remove(name);
    if removed {
        save_config(workspace_root, &cfg)?;
    }
    Ok(removed)
}

/// Resolve the workspace's owner-only OAuth token store, migrating a safe
/// legacy `.stella/mcp_oauth.json` before any caller constructs a token store.
pub fn oauth_store_path(workspace_root: &Path) -> Result<PathBuf, String> {
    stella_store::workspace_private_state_path(workspace_root, "mcp_oauth.json")
        .map_err(|e| format!("cannot resolve private MCP OAuth token store: {e}"))
}

/// The session's OAuth manager: lazy per-server bearer sources over the
/// workspace token store. Cheap; construct once per connect.
pub fn oauth_manager(
    workspace_root: &Path,
) -> Result<std::sync::Arc<stella_mcp::OAuthManager>, String> {
    Ok(std::sync::Arc::new(stella_mcp::OAuthManager::new(
        oauth_store_path(workspace_root)?,
    )))
}

/// Look up the URL a configured **http** server points at — the OAuth login
/// target. A stdio server has no authorization server, so it is an error.
pub fn http_server_url(workspace_root: &Path, server: &str) -> Result<String, String> {
    let cfg = load_config(workspace_root)?;
    match cfg.get(server) {
        Some(stella_mcp::McpTransport::Http { url, .. }) => Ok(url.clone()),
        Some(_) => Err(format!(
            "`{server}` is a stdio server — OAuth login applies to http servers only"
        )),
        None => Err(format!(
            "no MCP server `{server}` in {}",
            mcp_toml_path(workspace_root).display()
        )),
    }
}

/// Run the interactive OAuth login for a configured http server, emitting
/// progress through `notify` (the CLI prints; the deck forwards to its MCP
/// tab). The browser is opened best-effort for `AuthorizeUrl` events — the
/// URL is always also surfaced so the user can open it by hand.
pub async fn oauth_login(
    workspace_root: &Path,
    server: &str,
    notify: &mut (dyn FnMut(stella_mcp::LoginEvent) + Send),
) -> Result<(), String> {
    let url = http_server_url(workspace_root, server)?;
    let store_path = oauth_store_path(workspace_root)?;
    let tokens = stella_mcp::oauth::login(
        server,
        &url,
        &stella_mcp::LoginOptions::default(),
        &mut |event| {
            if let stella_mcp::LoginEvent::AuthorizeUrl(url) = &event {
                open_in_browser(url);
            }
            notify(event);
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    stella_mcp::TokenStore::new(store_path)
        .put(server, &tokens)
        .map_err(|e| e.to_string())
}

/// Forget a server's OAuth tokens; returns whether any existed.
pub fn oauth_logout(workspace_root: &Path, server: &str) -> Result<bool, String> {
    stella_mcp::TokenStore::new(oauth_store_path(workspace_root)?)
        .remove(server)
        .map_err(|e| e.to_string())
}

/// The configured servers that currently hold OAuth logins.
pub fn oauth_logged_in(workspace_root: &Path) -> Result<Vec<String>, String> {
    stella_mcp::TokenStore::new(oauth_store_path(workspace_root)?)
        .logged_in_servers()
        .map_err(|e| e.to_string())
}

/// Best-effort `open`/`xdg-open`/`start` of the authorize URL. Failure is
/// fine — the URL is printed/shown for manual opening either way.
fn open_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let launcher = ("open", vec![url.to_string()]);
    #[cfg(target_os = "windows")]
    let launcher = ("cmd", vec!["/C".into(), "start".into(), url.to_string()]);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let launcher = ("xdg-open", vec![url.to_string()]);
    let mut command = std::process::Command::new(launcher.0);
    command
        .args(&launcher.1)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    stella_tools::subprocess_env::scrub_sensitive_std_env(&mut command);
    let _ = command.spawn();
}

/// Per-(server, tool) usage aggregates from local telemetry
/// (`.stella/private/store.db`). Missing store → empty (never creates the file).
pub fn usage_stats(workspace_root: &Path) -> Result<Vec<stella_store::McpUsageStat>, String> {
    if stella_store::existing_workspace_private_sqlite_path(workspace_root, "store.db")
        .map_err(|e| format!("cannot resolve store: {e}"))?
        .is_none()
    {
        return Ok(Vec::new());
    }
    let store =
        stella_store::Store::open(workspace_root).map_err(|e| format!("cannot open store: {e}"))?;
    store
        .mcp_usage_stats()
        .map_err(|e| format!("cannot read MCP usage: {e}"))
}

/// Resolve a registry server name to `(alias, first install option)` — the
/// non-interactive install path (`stella mcp install <name>` and the deck's
/// `↵` on a search result). Prefers an exact name match; a server with neither
/// a runnable package nor a remote errors.
///
/// **An entry the registry does not vouch for is refused here** (SPEC §9.3,
/// "Unsigned blocked"). The check belongs on this path and not only in the
/// paint: an install writes a `cmd` line that this process later spawns, and
/// both callers reach it — so a row rendered red and a name typed at a shell
/// get the same answer.
pub async fn resolve_install(
    registry_url: &str,
    name: &str,
) -> Result<(String, InstallOption, ServerCard), String> {
    let page = search(registry_url, Some(name), None, 30).await?;
    let entry = page
        .entries
        .into_iter()
        .find(|e| e.server.name == name)
        .ok_or_else(|| {
            format!("no registry server named `{name}` — try `stella mcp search {name}`")
        })?;
    if let Some(reason) = entry.signature.refusal() {
        return Err(format!("`{name}`: {reason}"));
    }
    let alias = entry.server.default_alias();
    let mut options = entry.server.install_options();
    if options.is_empty() {
        return Err(format!(
            "`{name}` publishes neither a runnable package nor a remote endpoint"
        ));
    }
    Ok((alias, options.remove(0), entry.server.card()))
}

// ── `stella mcp` subcommand ──────────────────────────────────────────────────

/// Entry point for `stella mcp <cmd>`. Enable/disable are deliberately absent:
/// they are session-scoped (a running conversation's tool set), so they live in
/// the deck's MCP tab, not in a stateless CLI invocation.
pub fn run(cmd: &crate::McpCmd) -> Result<(), String> {
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    match cmd {
        crate::McpCmd::List => run_list(&workspace_root),
        crate::McpCmd::Search { query, limit } => run_search(
            &workspace_root,
            &query.join(" "),
            limit.unwrap_or(stella_mcp::registry::DEFAULT_PAGE_LIMIT),
        ),
        crate::McpCmd::Install { name, alias } => run_install(&workspace_root, name, alias.clone()),
        crate::McpCmd::Remove { name } => run_remove(&workspace_root, name),
        crate::McpCmd::Grant { name, yes, revoke } => {
            run_grant(&workspace_root, name, *yes, *revoke)
        }
        crate::McpCmd::Login { name } => run_login(&workspace_root, name),
        crate::McpCmd::Logout { name } => run_logout(&workspace_root, name),
        crate::McpCmd::Usage => run_usage(&workspace_root),
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to start runtime: {e}"))
}

fn run_list(workspace_root: &Path) -> Result<(), String> {
    crate::plain::section_header("Configured MCP servers");
    let cfg = load_config(workspace_root)?;
    if cfg.names().is_empty() {
        println!(
            "  {}",
            "none — `stella mcp search <query>` then `stella mcp install <name>`".dimmed()
        );
        return Ok(());
    }
    for name in cfg.names() {
        let transport = cfg.get(name).expect("name came from the config");
        let card = cfg.card(name).expect("name came from the config");
        let auth = if transport.has_credentials() {
            format!("· auth: {}", transport.credential_names().join(", ")).dimmed()
        } else {
            "· no auth".dimmed()
        };
        // Whether the model may use it at all comes before what it is: a
        // withheld server is configured, connectable, and inert.
        let grant = if cfg.is_granted(name) {
            "".normal()
        } else {
            format!("· ungranted (stella mcp grant {name})").yellow()
        };
        println!(
            "  {} {} {} {} {}",
            "·".green(),
            card.display_name(name).bright_magenta(),
            format!("[{}]", transport.kind_label()).dimmed(),
            auth,
            grant
        );
        // The alias is the routing token — the thing to type — so it stays
        // visible even when a title has taken the headline slot.
        if card.display_name(name) != name {
            println!("      {}", format!("alias: {name}").dimmed());
        }
        match card.description.as_deref() {
            Some(desc) => println!("      {}", truncate(desc, 100).dimmed()),
            None => println!("      {}", endpoint_summary(transport).dimmed()),
        }
    }
    println!(
        "\n  {}",
        "enable/disable is per-session — toggle servers live in the deck's MCP tab (/mcp). \
         A grant is not: it is recorded in mcp.toml and read by every session."
            .dimmed()
    );
    Ok(())
}

fn run_search(workspace_root: &Path, query: &str, limit: u32) -> Result<(), String> {
    let registry_url = resolve_registry_url(workspace_root);
    crate::plain::section_header(&format!("MCP registry search — {registry_url}"));
    let query_opt = (!query.trim().is_empty()).then_some(query);
    let page = runtime()?.block_on(search(&registry_url, query_opt, None, limit))?;
    if page.entries.is_empty() {
        println!("  {}", "no matching servers".dimmed());
        return Ok(());
    }
    // The registry returns one row per published version; collapse to one row
    // per server name (MCP servers are not versioned in stella's config).
    let mut seen = std::collections::HashSet::new();
    for entry in page
        .entries
        .iter()
        .filter(|e| seen.insert(e.server.name.clone()))
    {
        let server = &entry.server;
        let kinds = install_kinds(server);
        // Provenance beside the name, and in the same words the deck's MCP
        // tab uses: tier, installs, signature — the three facts that decide
        // whether the next line is worth reading (SPEC §9.3).
        let signature = if entry.signature.installable() {
            entry.signature.label().green()
        } else {
            entry.signature.label().red()
        };
        println!(
            "  {} {} {} {} {} {}",
            "·".green(),
            server.name.bright_magenta(),
            format!("[{kinds}]").dimmed(),
            entry.tier.label().dimmed(),
            match entry.installs {
                Some(n) => format!("{n} installs").dimmed(),
                None => "installs unknown".dimmed(),
            },
            signature
        );
        if let Some(desc) = &server.description {
            println!("      {}", truncate(desc, 100).dimmed());
        }
    }
    if page.next_cursor.is_some() {
        println!("\n  {}", "more results available (pagination)".dimmed());
    }
    println!(
        "\n  {}",
        "install with: stella mcp install <name> — a blocked entry is refused, then \
         `stella mcp grant <name>` reviews what an installed one declares"
            .dimmed()
    );
    Ok(())
}

fn run_install(workspace_root: &Path, name: &str, alias: Option<String>) -> Result<(), String> {
    let registry_url = resolve_registry_url(workspace_root);
    let (default_alias, option, card) =
        runtime()?.block_on(resolve_install(&registry_url, name))?;
    let alias = alias.unwrap_or(default_alias);
    install(workspace_root, &alias, option.transport, card)?;
    println!(
        "  {} installed {} as {} ({})",
        "◆".bright_cyan(),
        name.bright_magenta(),
        alias.bright_magenta(),
        option.label.dimmed()
    );
    if !option.auth.is_empty() {
        let required: Vec<&str> = option
            .auth
            .iter()
            .filter(|f| f.required || f.secret)
            .map(|f| f.name.as_str())
            .collect();
        if !required.is_empty() {
            println!(
                "  {} needs credentials: {} — set them in the deck's MCP tab (a) or edit {}",
                "!".yellow(),
                required.join(", "),
                mcp_toml_path(workspace_root).display()
            );
        }
    }
    Ok(())
}

fn run_remove(workspace_root: &Path, name: &str) -> Result<(), String> {
    if remove(workspace_root, name)? {
        println!("  {} removed {}", "◆".bright_cyan(), name.bright_magenta());
        Ok(())
    } else {
        Err(format!("no configured MCP server named `{name}`"))
    }
}

fn run_login(workspace_root: &Path, name: &str) -> Result<(), String> {
    crate::plain::section_header(&format!("OAuth login — {name}"));
    runtime()?.block_on(oauth_login(
        workspace_root,
        name,
        &mut |event| match event {
            stella_mcp::LoginEvent::Status(line) => println!("  {} {line}", "·".green()),
            stella_mcp::LoginEvent::AuthorizeUrl(url) => {
                println!(
                    "  {} approve access in your browser (opened automatically):",
                    "◆".bright_cyan()
                );
                println!("    {}", url.bright_magenta());
            }
        },
    ))?;
    println!(
        "  {} logged in — tokens in {} (auto-refreshed; `stella mcp logout {name}` to forget)",
        "◆".bright_cyan(),
        oauth_store_path(workspace_root)?.display()
    );
    Ok(())
}

/// `stella mcp grant <name>` — SPEC §9.3's first-enable handshake for a shell
/// rather than the deck, and the remedy every ungranted-server refusal names.
///
/// It connects, prints what the server declares, and records the decision.
/// Connecting is the point: the capabilities being granted are what the
/// process on the other end announces right now, not what a registry card once
/// said, and nothing short of asking it can show them.
///
/// `--revoke` withdraws a grant. It writes `granted = false` rather than
/// clearing the key, because "reviewed and declined" and "never asked" are
/// different states and only the first should survive a re-read.
fn run_grant(workspace_root: &Path, name: &str, yes: bool, revoke: bool) -> Result<(), String> {
    let cfg = load_config(workspace_root)?;
    let transport = cfg.get(name).cloned().ok_or_else(|| {
        format!(
            "no MCP server `{name}` in {}",
            mcp_toml_path(workspace_root).display()
        )
    })?;
    if revoke {
        set_granted(workspace_root, name, false)?;
        println!(
            "  {} revoked {} — its tools are no longer offered to the model",
            "◆".bright_cyan(),
            name.bright_magenta()
        );
        return Ok(());
    }

    crate::plain::section_header(&format!("MCP handshake — {name}"));
    let server = stella_mcp::McpServerConfig {
        name: name.to_string(),
        transport,
        candidate_safe: cfg.is_candidate_safe(name),
    };
    let auth = oauth_manager(workspace_root)?;
    // No grant set on this client: it never calls a tool, and gating the
    // handshake would make the capabilities unreadable — which is the one
    // thing this command exists to prevent.
    let set = runtime()?.block_on(stella_mcp::McpToolSet::connect_with_auth(
        std::slice::from_ref(&server),
        crate::agent::MCP_CONNECT_TIMEOUT,
        Some(auth),
    ));
    if let Some((_, reason)) = set.failed_servers().iter().find(|(s, _)| s == name) {
        return Err(format!(
            "`{name}` did not connect, so it has declared nothing to grant: {reason}"
        ));
    }
    if let Some((_, reason)) = set.auth_required_servers().iter().find(|(s, _)| s == name) {
        return Err(format!(
            "`{name}` needs a login before it will declare anything: {reason} \
             (run `stella mcp login {name}`)"
        ));
    }

    if let Some(identity) = set.identity(name) {
        let announced = identity
            .title
            .or(identity.name)
            .unwrap_or_else(|| "(no name announced)".to_string());
        println!(
            "  {} announces itself as {} {}",
            "·".green(),
            announced.bright_magenta(),
            format!("[{}]", identity.protocol_version).dimmed()
        );
    }
    println!(
        "  {} reached at {}",
        "·".green(),
        endpoint_summary(&server.transport).dimmed()
    );
    let tools = set.advertised_tools(name);
    if tools.is_empty() {
        println!("  {}", "it declares no tools at all".yellow());
    } else {
        println!(
            "\n  {}",
            format!(
                "it declares {} tool(s), every one of which the model may call once granted:",
                tools.len()
            )
            .bold()
        );
        for tool in tools {
            println!("    {} {}", "·".green(), tool.name.bright_magenta());
            let summary = tool
                .description
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or_default();
            if !summary.is_empty() {
                println!("        {}", truncate(summary, 100).dimmed());
            }
        }
    }
    runtime()?.block_on(set.close_all());

    if !yes && !confirm_grant(name)? {
        println!("  {} not granted — `{}` stays withheld", "·".dimmed(), name);
        return Ok(());
    }
    set_granted(workspace_root, name, true)?;
    println!(
        "\n  {} granted {} — its tools are offered to the model from the next session",
        "◆".bright_cyan(),
        name.bright_magenta()
    );
    Ok(())
}

/// The interactive half of `stella mcp grant`. Anything but an explicit `y`
/// declines, including an unreadable or closed stdin: a grant is the one
/// answer that must never be arrived at by default.
fn confirm_grant(name: &str) -> Result<bool, String> {
    use std::io::Write as _;
    print!("\n  grant these capabilities to `{name}`? [y/N] ");
    std::io::stdout()
        .flush()
        .map_err(|e| format!("cannot write to stdout: {e}"))?;
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return Ok(false);
    }
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn run_logout(workspace_root: &Path, name: &str) -> Result<(), String> {
    if oauth_logout(workspace_root, name)? {
        println!(
            "  {} logged out of {}",
            "◆".bright_cyan(),
            name.bright_magenta()
        );
        Ok(())
    } else {
        Err(format!("no stored OAuth login for `{name}`"))
    }
}

fn run_usage(workspace_root: &Path) -> Result<(), String> {
    crate::plain::section_header("MCP tool usage (.stella/private/store.db)");
    let stats = usage_stats(workspace_root)?;
    if stats.is_empty() {
        println!(
            "  {}",
            "no MCP tool calls recorded yet — run a session that uses an MCP server.".dimmed()
        );
        return Ok(());
    }
    for stat in &stats {
        let reason = if stat.last_reason.is_empty() {
            String::new()
        } else {
            format!("· {}", truncate(&stat.last_reason, 60))
                .dimmed()
                .to_string()
        };
        println!(
            "  {} {} {} {} {}",
            "·".green(),
            format!("{}×", stat.calls).bright_magenta(),
            stat.server.bright_magenta(),
            stat.tool,
            reason
        );
    }
    Ok(())
}

/// A compact "npm, remote, …" list of a server's install kinds, for search.
pub(crate) fn install_kinds(server: &stella_mcp::RegistryServer) -> String {
    let mut kinds: Vec<String> = Vec::new();
    if !server.remotes.is_empty() {
        kinds.push("remote".to_string());
    }
    for pkg in &server.packages {
        if !pkg.registry_type.is_empty() && !kinds.iter().any(|k| k == &pkg.registry_type) {
            kinds.push(pkg.registry_type.clone());
        }
    }
    if kinds.is_empty() {
        "no install target".to_string()
    } else {
        kinds.join(", ")
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars).collect();
    format!("{head}…")
}

/// One line saying where a server actually is: the endpoint URL for http, the
/// spawn command line for stdio.
///
/// This is the fallback identity — the thing that is true of *every* entry,
/// with or without a registry card. An alias of `mcp` pointing at
/// `https://mcp.stripe.com/v1` is only mysterious until the URL is on screen.
///
/// **Query strings are redacted.** Hosted MCP endpoints are routinely handed
/// out with the credential in the URL (`?api_key=…`), and this string is
/// printed to a terminal, shown in a shared screenshot, and pasted into bug
/// reports. Keys are kept, values become `…`, so the shape of the endpoint
/// stays readable without the secret riding along. Nothing else in a
/// transport is secret: `env`/`headers` *values* are never rendered here, only
/// their names, and a spawn command line is a package name and flags.
pub fn endpoint_summary(transport: &McpTransport) -> String {
    match transport {
        McpTransport::Stdio { cmd, args, .. } => {
            let mut line = cmd.clone();
            for arg in args {
                line.push(' ');
                line.push_str(arg);
            }
            line
        }
        McpTransport::Http { url, .. } => redact_query(url),
    }
}

/// Replace every query-parameter *value* in `url` with `…`, keeping the keys.
/// Split on the first `?` only — anything after it is the query, and a `?` in
/// a path segment is not something a URL is allowed to carry unescaped.
fn redact_query(url: &str) -> String {
    let Some((base, query)) = url.split_once('?') else {
        return url.to_string();
    };
    if query.is_empty() {
        return base.to_string();
    }
    let scrubbed: Vec<String> = query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((key, _)) => format!("{key}=…"),
            // A bare flag carries no value to hide.
            None => pair.to_string(),
        })
        .collect();
    format!("{base}?{}", scrubbed.join("&"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn install_load_and_remove_roundtrip_through_mcp_toml() {
        let dir = std::env::temp_dir().join(format!("stella-mcp-cmd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Absent file → empty config.
        assert!(load_config(&dir).unwrap().names().is_empty());

        // Install a stdio server → it round-trips through the file.
        let transport = McpTransport::Stdio {
            cmd: "npx".into(),
            args: vec!["-y".into(), "some-mcp".into()],
            env: BTreeMap::new(),
        };
        install(&dir, "some", transport.clone(), ServerCard::default()).unwrap();
        let cfg = load_config(&dir).unwrap();
        assert_eq!(cfg.names(), vec!["some"]);
        assert_eq!(cfg.get("some"), Some(&transport));

        // Auth sets a credential without disturbing the rest.
        set_credential(&dir, "some", "API_KEY", "secret".into()).unwrap();
        let cfg = load_config(&dir).unwrap();
        assert!(cfg.get("some").unwrap().has_credentials());
        // The written file must not contain the raw value under a Debug dump.
        assert!(!format!("{:?}", cfg.get("some").unwrap()).contains("secret"));

        // Remove.
        assert!(remove(&dir, "some").unwrap());
        assert!(!remove(&dir, "some").unwrap());
        assert!(load_config(&dir).unwrap().names().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The witness (#5047, persistence).** A registry install lands
    /// withheld and stays withheld across a reload; the grant survives the
    /// same round trip. Without the on-disk half, every session would ask
    /// again — which is how operators learn to grant without reading.
    #[test]
    fn an_installed_server_lands_ungranted_and_the_grant_persists() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let transport = McpTransport::Stdio {
            cmd: "npx".into(),
            args: vec!["-y".into(), "some-mcp".into()],
            env: BTreeMap::new(),
        };
        install(root, "some", transport.clone(), ServerCard::default()).unwrap();

        assert!(
            !load_config(root).unwrap().is_granted("some"),
            "a registry install must not be usable before its handshake is read"
        );
        assert!(set_granted(root, "some", true).unwrap());
        assert!(load_config(root).unwrap().is_granted("some"));

        // Re-installing the same alias does not re-ask: the operator granted
        // THAT alias, and re-fetching the publisher's transport is not a new
        // question.
        install(root, "some", transport, ServerCard::default()).unwrap();
        assert!(load_config(root).unwrap().is_granted("some"));

        // Revoking is a recorded "no", distinguishable from never having been
        // asked — which is what a hand-written entry looks like.
        assert!(set_granted(root, "some", false).unwrap());
        assert_eq!(
            load_config(root).unwrap().grant_decision("some"),
            Some(Some(false))
        );
        assert!(!set_granted(root, "absent", true).unwrap());
    }

    /// Which of the three doors a server came through decides its starting
    /// grant. A hand-written entry and a plugin's contribution were both
    /// chosen by the operator; a registry install is one keystroke.
    #[test]
    fn the_session_grants_come_from_how_each_server_was_configured() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let transport = || McpTransport::Stdio {
            cmd: "npx".into(),
            args: vec![],
            env: BTreeMap::new(),
        };
        // Installed from the registry: a recorded "not yet".
        install(root, "installed", transport(), ServerCard::default()).unwrap();
        // Hand-written: no decision recorded at all.
        let mut cfg = load_config(root).unwrap();
        cfg.servers.insert(
            "handwritten".to_string(),
            stella_mcp::config::McpServerEntry {
                transport: transport(),
                candidate_safe: false,
                granted: None,
                card: ServerCard::default(),
            },
        );
        save_config(root, &cfg).unwrap();

        let plan: Vec<stella_mcp::McpServerConfig> = ["installed", "handwritten", "contributed"]
            .into_iter()
            .map(|name| stella_mcp::McpServerConfig {
                name: name.to_string(),
                transport: transport(),
                candidate_safe: false,
            })
            .collect();
        let grants = initial_grants(root, &plan);
        let granted = grants.lock().unwrap();
        assert!(!granted.contains("installed"), "{granted:?}");
        assert!(granted.contains("handwritten"), "{granted:?}");
        // Not in mcp.toml at all — a plugin shipped it, and installing the
        // plugin was the decision.
        assert!(granted.contains("contributed"), "{granted:?}");
    }

    #[cfg(unix)]
    #[test]
    fn credential_config_is_owner_only_and_rejects_symlink_targets() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let dir = tempfile::tempdir().unwrap();
        let cfg = McpConfig::default();
        save_config(dir.path(), &cfg).unwrap();
        assert_eq!(
            std::fs::metadata(mcp_toml_path(dir.path()))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        let target = dir.path().join("outside.toml");
        std::fs::write(&target, "[servers]\n").unwrap();
        std::fs::remove_file(mcp_toml_path(dir.path())).unwrap();
        symlink(&target, mcp_toml_path(dir.path())).unwrap();
        assert!(save_config(dir.path(), &cfg).is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "[servers]\n");
    }

    #[cfg(unix)]
    #[test]
    fn oauth_path_migrates_a_safe_legacy_token_store_into_private_state() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let dot = dir.path().join(".stella");
        std::fs::create_dir_all(&dot).unwrap();
        std::fs::set_permissions(&dot, std::fs::Permissions::from_mode(0o700)).unwrap();
        let previous_generated = "*.db\n*.db-wal\n*.db-shm\nreflections.jsonl\n";
        let custom = "# keep this custom rule\nexports/\n";
        let ignore_path = dot.join(".gitignore");
        std::fs::write(&ignore_path, format!("{previous_generated}{custom}")).unwrap();
        std::fs::set_permissions(&ignore_path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let legacy = dot.join("mcp_oauth.json");
        std::fs::write(&legacy, br#"{"servers":{}}"#).unwrap();

        let resolved: Result<PathBuf, String> = oauth_store_path(dir.path());
        let resolved = resolved.unwrap();
        assert_eq!(resolved, dot.join("private/mcp_oauth.json"));
        assert!(!legacy.exists());
        assert_eq!(std::fs::read(resolved).unwrap(), br#"{"servers":{}}"#);
        assert_eq!(
            std::fs::read_to_string(&ignore_path).unwrap(),
            format!("{previous_generated}{custom}private/\n")
        );
        assert_eq!(
            std::fs::metadata(&ignore_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640,
            "committable ignore mode must survive the atomic update"
        );
        oauth_store_path(dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&ignore_path)
                .unwrap()
                .lines()
                .filter(|line| *line == "private/")
                .count(),
            1,
            "idempotent resolution must not duplicate the ignore rule"
        );

        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        let ignored = std::process::Command::new("git")
            .args(["check-ignore", ".stella/private/mcp_oauth.json"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(ignored.status.success(), "OAuth tokens must never stage");
    }

    #[test]
    fn endpoint_summary_is_the_identity_of_last_resort() {
        // An alias says nothing; the endpoint names the vendor.
        let http = McpTransport::Http {
            url: "https://mcp.stripe.com/v1".into(),
            headers: BTreeMap::new(),
        };
        assert_eq!(endpoint_summary(&http), "https://mcp.stripe.com/v1");

        let stdio = McpTransport::Stdio {
            cmd: "npx".into(),
            args: vec!["-y".into(), "@stripe/mcp".into()],
            env: BTreeMap::new(),
        };
        assert_eq!(endpoint_summary(&stdio), "npx -y @stripe/mcp");
    }

    #[test]
    fn a_credential_in_the_query_string_is_redacted_but_the_host_survives() {
        // Hosted MCP endpoints routinely carry the key in the URL, and this
        // string lands in terminals, screenshots, and bug reports.
        let leaky = McpTransport::Http {
            url: "https://server.smithery.ai/x/mcp?api_key=sk-live-abc123&profile=default".into(),
            headers: BTreeMap::new(),
        };
        let shown = endpoint_summary(&leaky);
        assert!(!shown.contains("sk-live-abc123"), "key leaked: {shown}");
        assert!(shown.contains("server.smithery.ai"), "host lost: {shown}");
        assert!(shown.contains("api_key=…"), "key name lost: {shown}");
        assert!(shown.contains("profile=…"), "{shown}");
    }

    #[test]
    fn redact_query_leaves_url_shapes_it_has_nothing_to_hide_in() {
        assert_eq!(redact_query("https://h/mcp"), "https://h/mcp");
        assert_eq!(redact_query("https://h/mcp?"), "https://h/mcp");
        // A bare flag carries no value to hide.
        assert_eq!(redact_query("https://h/mcp?debug"), "https://h/mcp?debug");
    }

    #[test]
    fn install_records_the_publishers_card_and_list_can_read_it_back() {
        let dir = std::env::temp_dir().join(format!("stella-mcp-card-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        install(
            &dir,
            "mcp",
            McpTransport::Http {
                url: "https://mcp.stripe.com/v1".into(),
                headers: BTreeMap::new(),
            },
            ServerCard {
                title: Some("Stripe".into()),
                description: Some("Payments, refunds, and balance reads.".into()),
                registry_name: Some("com.stripe/mcp".into()),
                ..ServerCard::default()
            },
        )
        .unwrap();

        // The whole point: the alias `mcp` is now legible after a restart.
        let cfg = load_config(&dir).unwrap();
        let card = cfg.card("mcp").unwrap();
        assert_eq!(card.display_name("mcp"), "Stripe");
        assert_eq!(card.registry_name.as_deref(), Some("com.stripe/mcp"));
        assert!(card.description.as_deref().unwrap().contains("refunds"));

        // Setting a credential later must not blow the card away.
        set_credential(&dir, "mcp", "Authorization", "Bearer x".into()).unwrap();
        let cfg = load_config(&dir).unwrap();
        assert_eq!(
            cfg.card("mcp").unwrap().description,
            card.description,
            "an auth write erased the recorded description"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
