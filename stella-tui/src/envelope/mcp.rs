//! The MCP tab's read models: the configured-server rows, the registry search
//! results, and the ctrl+o inspector's per-server detail.
//!
//! Split out of `envelope.rs` (#629's 1500-line ratchet) when the tab grew an
//! inspector. The shapes here are deliberately *display* types: the driver
//! joins config, live session state, and telemetry into them, and the view
//! renders them without reaching back for anything.
//!
//! Every string a server or registry supplied — title, description,
//! instructions, tool descriptions — is untrusted third-party text. It is
//! rendered to the operator and never routed to the model.

/// A configured MCP server's full state for the MCP tab. The four state axes
/// are distinct on purpose: a server can be *configured* (in `mcp.toml`) yet
/// not *connected* (failed to start, or added after session start), and
/// *enabled* (session intent) is separate from *connected*.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct McpServerInfo {
    /// The local alias (config key + tool-namespace segment). This is the
    /// routing token — what `mcp__<name>__<tool>` is built from — so it stays
    /// visible even when a friendlier title exists.
    pub name: String,
    /// The publisher's (or the live server's) display name, when it differs
    /// from the alias. An alias is a sanitized last path segment, so
    /// `com.stripe/mcp` installs as `mcp`; without this the list is a column
    /// of names that say nothing about what any of them is.
    pub title: Option<String>,
    /// One-line summary, from the registry card or the server's own handshake
    /// instructions. `None` for an entry that has neither yet.
    pub description: Option<String>,
    /// Where the server actually is: the endpoint URL (query values redacted)
    /// or the spawn command line. The identity of last resort — always
    /// present, card or no card.
    pub endpoint: String,
    /// Transport discriminant: `stdio` or `http`.
    pub kind: String,
    /// Enabled for this session (not in the disabled set).
    pub enabled: bool,
    /// Connected in the live tool set this session (tools are actually
    /// reachable). A newly-installed server shows `configured` but not
    /// `connected` until the next session.
    pub connected: bool,
    /// Short health label when connected (e.g. `live`, `reconnecting`).
    pub health: Option<String>,
    /// How many tools it advertises this session (0 when disabled/unconnected).
    pub tool_count: usize,
    /// Tools this server advertised that were **refused** past the per-server
    /// cap (`stella_mcp::MAX_TOOLS_PER_SERVER`), so the model was never told
    /// about them. `0` for every well-behaved server.
    ///
    /// Distinct from `connected: false` on purpose: the server is up, healthy,
    /// and its kept tools route normally — rendering this as "unavailable"
    /// would be a lie. A **floor**, not a total: discovery stops on the page
    /// where the cap bites, so tools the server would have listed later are
    /// never counted.
    pub dropped_tools: usize,
    /// Configured credential field names (env vars / headers) — presence means
    /// auth is set; the values are never carried here.
    pub auth_fields: Vec<String>,
    /// OAuth state: `None` = not applicable (stdio), `Some(logged_in)` for an
    /// http server (`o` starts the browser login; tokens never ride here).
    pub oauth: Option<bool>,
    /// Total recorded calls to this server's tools (from local telemetry).
    pub calls: u64,
    /// The `candidate_safe = true` opt-in (issue #248 Phase 1) — this server's
    /// tools are shared into Best-of-N candidate workspaces.
    pub candidate_safe: bool,
}

/// The outcome of an MCP registry search requested from the tab.
#[derive(Clone, Debug, PartialEq)]
pub struct McpSearchOutcome {
    /// The query that produced these results (echoed for display).
    pub query: String,
    pub items: Vec<McpSearchItem>,
    /// Set when the search failed (network/registry error) instead of matching.
    pub error: Option<String>,
    /// Whether the registry reported more pages beyond this one.
    pub has_more: bool,
}

/// One registry search result row.
#[derive(Clone, Debug, PartialEq)]
pub struct McpSearchItem {
    pub name: String,
    pub description: String,
    /// A compact install-kinds hint, e.g. `npm, remote`.
    pub kinds: String,
    /// Whether a server of this name is already configured locally.
    pub installed: bool,
}

/// What the live server said about itself during the `initialize` handshake.
///
/// Kept apart from the on-disk card because they answer different questions:
/// the card is what the *publisher* wrote and persists across sessions, this
/// is what the *process on the other end of the pipe* claims right now. When
/// they disagree, both are worth seeing — a card describing one thing and a
/// handshake announcing another is exactly the sort of surprise an inspector
/// exists to surface.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct McpLiveIdentity {
    /// The server's self-reported name (need not match the local alias).
    pub name: Option<String>,
    pub version: Option<String>,
    pub title: Option<String>,
    pub website_url: Option<String>,
    /// The server's free-prose `instructions`. Operator-facing only.
    pub instructions: Option<String>,
    /// The protocol revision this session negotiated with it.
    pub protocol_version: String,
}

/// One advertised tool, for the inspector's tool table.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct McpToolRow {
    /// The raw (un-namespaced) tool name, as the server advertised it.
    pub name: String,
    /// The server's description of the tool — the text the model reads when
    /// deciding to call it, which makes it the most honest available answer to
    /// "what does this server actually do".
    pub description: String,
    /// Recorded calls to this tool from local telemetry.
    pub calls: u64,
    /// The server annotated it read-only/idempotent, so a duplicate delivery
    /// is harmless. Untrusted — a self-report, not a guarantee.
    pub safe_to_retry: bool,
}

/// How the on-demand registry lookup behind the inspector is going.
///
/// A description the operator does not have is worth one network round-trip,
/// but only on request: this is a third-party service, so it is never
/// consulted by simply opening the tab.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum McpLookupState {
    /// Nothing was asked of the registry — the description was already known,
    /// or the operator has not opened the inspector.
    #[default]
    Idle,
    /// A lookup is in flight.
    Fetching,
    /// The registry answered and had no entry under this name.
    Missing,
    /// The lookup failed (offline, registry down, bad URL).
    Failed(String),
    /// The registry answered and the card was backfilled into `mcp.toml`.
    Found,
}

/// Everything the MCP tab's ctrl+o inspector shows about one configured
/// server: its recorded identity, where it is, how it authenticates, what it
/// negotiated this session, and every tool it advertises.
///
/// Assembled by the driver from three sources that no single one of them can
/// answer alone — `.stella/mcp.toml` (identity + transport), the live tool set
/// (handshake + advertised tools), and local telemetry (call counts).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct McpServerDetail {
    /// The local alias this detail describes — the key everything else joins
    /// on, and the row the view must keep highlighted.
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    /// The fully-qualified registry id (`com.stripe/mcp`) recorded at install.
    pub registry_name: Option<String>,
    /// Source repository, for vetting the publisher.
    pub repository: Option<String>,
    /// The registry entry's version at install time. Informational only.
    pub version: Option<String>,
    pub kind: String,
    /// Endpoint URL (query values redacted) or spawn command line.
    pub endpoint: String,
    /// Credential field names configured on the transport — never values.
    pub auth_fields: Vec<String>,
    pub oauth: Option<bool>,
    pub candidate_safe: bool,
    pub enabled: bool,
    pub connected: bool,
    pub health: Option<String>,
    /// Tools refused past the per-server cap (a floor — see
    /// [`McpServerInfo::dropped_tools`]).
    pub dropped_tools: usize,
    /// Total recorded calls across this server's tools.
    pub calls: u64,
    /// The handshake identity, when the server is connected this session.
    pub live: Option<McpLiveIdentity>,
    /// Every tool the server advertised, whether or not it is enabled right
    /// now — an operator inspecting a server they just switched off still
    /// needs to see what it offers.
    pub tools: Vec<McpToolRow>,
    /// State of the on-demand registry lookup for a missing description.
    pub lookup: McpLookupState,
}

impl McpServerDetail {
    /// The best human label for this server: the recorded title, else the live
    /// server's title or self-reported name, else the alias.
    pub fn display_name(&self) -> &str {
        [
            self.title.as_deref(),
            self.live.as_ref().and_then(|l| l.title.as_deref()),
            self.live.as_ref().and_then(|l| l.name.as_deref()),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|t| !t.is_empty())
        .unwrap_or(&self.name)
    }

    /// Whether a registry lookup could still tell the operator something —
    /// there is no description, and the last attempt has not already answered.
    ///
    /// Drives whether the inspector offers `R` at all: an affordance that
    /// cannot change anything is noise, and re-asking a registry that just
    /// said "no such server" would only produce the same answer more slowly.
    pub fn lookup_would_help(&self) -> bool {
        self.description.is_none() && matches!(self.lookup, McpLookupState::Idle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail() -> McpServerDetail {
        McpServerDetail {
            name: "mcp".into(),
            ..McpServerDetail::default()
        }
    }

    #[test]
    fn display_name_falls_back_through_card_then_handshake_then_alias() {
        // Nothing known → the alias, which is all the old list ever showed.
        assert_eq!(detail().display_name(), "mcp");

        // The live handshake alone is enough to stop showing a bare alias.
        let mut live_only = detail();
        live_only.live = Some(McpLiveIdentity {
            name: Some("stripe".into()),
            ..McpLiveIdentity::default()
        });
        assert_eq!(live_only.display_name(), "stripe");

        // A handshake `title` outranks the self-reported name.
        let mut titled = live_only.clone();
        titled.live.as_mut().unwrap().title = Some("Stripe".into());
        assert_eq!(titled.display_name(), "Stripe");

        // The recorded card outranks both — it is what the publisher wrote.
        let mut carded = titled.clone();
        carded.title = Some("Stripe Payments".into());
        assert_eq!(carded.display_name(), "Stripe Payments");
    }

    #[test]
    fn a_blank_title_does_not_win_the_headline() {
        let mut blank = detail();
        blank.title = Some("   ".into());
        blank.live = Some(McpLiveIdentity {
            name: Some("stripe".into()),
            ..McpLiveIdentity::default()
        });
        assert_eq!(blank.display_name(), "stripe");
    }

    #[test]
    fn lookup_is_offered_only_when_it_could_answer_something() {
        assert!(detail().lookup_would_help());

        let mut described = detail();
        described.description = Some("Charge cards.".into());
        assert!(!described.lookup_would_help());

        // Already asked and answered — offering it again promises nothing.
        for state in [
            McpLookupState::Fetching,
            McpLookupState::Missing,
            McpLookupState::Found,
            McpLookupState::Failed("offline".into()),
        ] {
            let mut asked = detail();
            asked.lookup = state;
            assert!(!asked.lookup_would_help(), "{:?} re-offered", asked.lookup);
        }
    }
}
