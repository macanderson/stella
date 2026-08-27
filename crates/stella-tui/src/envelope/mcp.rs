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
    /// Round-trip time of the connect handshake's `initialize` request, in
    /// whole milliseconds — SPEC §9.3's latency column.
    ///
    /// `None` for a server with no live connection to have measured, and the
    /// row then shows nothing rather than `0ms`: an unmeasured server would
    /// otherwise sort and read as the nearest one on the list.
    pub latency_ms: Option<u64>,
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
    /// Tools this server advertised that were **trimmed** to fit the
    /// per-server schema byte budget (`stella_mcp::MAX_SERVER_SCHEMA_BYTES`).
    /// `0` for every server that fits, which is nearly all of them.
    ///
    /// A different wall from `dropped_tools`, and the row says which one was
    /// hit: 300 tools trips the count cap, twelve verbose ones trip this. A
    /// reader told only "dropped past cap" would go looking for the wrong
    /// limit and find the server nowhere near it (#4441).
    ///
    /// Unlike `dropped_tools` this is a total, not a floor: the budget sees
    /// the whole advertised list and counts every tool it cuts.
    pub trimmed_tools: usize,
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
    /// Whether the operator has granted this server its declared capabilities
    /// (SPEC §9.3's first-enable handshake). An ungranted server advertises no
    /// tools to the model and answers every call with a refusal, so the row
    /// says so and `e` opens the handshake instead of toggling.
    ///
    /// `true` for a hand-written `mcp.toml` entry that records no decision —
    /// see `stella_mcp::McpServerEntry::granted` for why writing the transport
    /// by hand is itself the grant.
    pub granted: bool,
}

impl McpServerInfo {
    /// Whether this row is the pinned graph server — SPEC §9.3's
    /// `graph is pinned · it is the product, not an integration`.
    ///
    /// Matched on the alias, which is the tool-namespace segment: the graph's
    /// tools are reachable as `mcp__graph__…` or they are not the graph's, so
    /// the alias is the only name that can decide this. A row named `graph`
    /// that is somebody else's server still pins, and correctly — that name is
    /// what every tool call in the session will route through.
    pub fn is_graph(&self) -> bool {
        self.name == GRAPH_SERVER
    }
}

/// The alias stella's own graph server is configured under. The MCP tab pins
/// it above every third-party row.
pub const GRAPH_SERVER: &str = "graph";

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

/// How far a registry's own publisher verification goes for one entry — SPEC
/// §9.3's `official · vendor · community` column.
///
/// A display mirror of `stella_mcp::SourceTier`, which is where the derivation
/// and its evidence live. This crate takes only leaf dependencies (see its
/// `Cargo.toml`), and `stella-mcp` is not one; the driver maps across the seam
/// with an exhaustive `match`, so a tier added there fails to compile here
/// rather than rendering as something it is not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum McpSourceTier {
    /// The registry operator's own namespace.
    Official,
    /// A namespace under a domain the publisher proved they control.
    Vendor,
    /// A code-host account's namespace, or the registry's anonymous one.
    #[default]
    Community,
}

impl McpSourceTier {
    /// The one word the row renders.
    pub fn label(self) -> &'static str {
        match self {
            McpSourceTier::Official => "official",
            McpSourceTier::Vendor => "vendor",
            McpSourceTier::Community => "community",
        }
    }
}

/// Whether the registry attributes an entry to a verified publisher and still
/// stands behind it — SPEC §9.3's signature column, and the reason a row is
/// blocked.
///
/// The display mirror of `stella_mcp::SignatureStatus`; see that type for what
/// "signed" claims (the publisher's namespace was verified at publish time)
/// and what it does not (nothing verifies the package bytes). `Unsigned` is
/// the default so a row assembled without an answer is refused rather than
/// installed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum McpSignature {
    /// Attributed to a verified namespace, lifecycle `active`.
    Signed,
    /// Attributed to nobody.
    #[default]
    Unsigned,
    /// Published, then withdrawn by the registry.
    Withdrawn,
}

impl McpSignature {
    /// Whether install may proceed. The driver enforces the same rule against
    /// the `stella-mcp` value; this is what stops the row from *offering* it.
    pub fn installable(self) -> bool {
        matches!(self, McpSignature::Signed)
    }

    /// The label the row renders. The blocked states carry the word `blocked`
    /// rather than relying on the red alone (SPEC §13).
    pub fn label(self) -> &'static str {
        match self {
            McpSignature::Signed => "signed",
            McpSignature::Unsigned => "unsigned · blocked",
            McpSignature::Withdrawn => "withdrawn · blocked",
        }
    }
}

/// One registry search result row.
///
/// The three provenance fields are what SPEC §9.3 asks a registry row to carry
/// before an operator spends a keystroke on it: who the registry says
/// published this, how many people run it, and whether the registry vouches
/// for it at all.
#[derive(Clone, Debug, PartialEq)]
pub struct McpSearchItem {
    pub name: String,
    pub description: String,
    /// A compact install-kinds hint, e.g. `npm, remote`.
    pub kinds: String,
    /// Whether a server of this name is already configured locally.
    pub installed: bool,
    /// How far the registry's publisher verification goes for this entry.
    pub tier: McpSourceTier,
    /// Recorded installs, where the registry publishes a count. `None` renders
    /// as unknown, never as `0` — a registry that counts nothing must not make
    /// every server look unused.
    pub installs: Option<u64>,
    /// Whether the registry attributes and stands behind this entry. Anything
    /// but [`McpSignature::Signed`] is refused by the install path, and the row
    /// says so in words as well as colour (SPEC §13).
    pub signature: McpSignature,
}

impl McpSearchItem {
    /// Whether pressing install on this row can do anything — the row's own
    /// copy of the policy the driver enforces.
    pub fn installable(&self) -> bool {
        self.signature.installable()
    }
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
    /// The connect handshake's `initialize` round trip in whole milliseconds —
    /// see [`McpServerInfo::latency_ms`].
    pub latency_ms: Option<u64>,
    /// Whether the operator has granted this server its declared capabilities
    /// — see [`McpServerInfo::granted`].
    pub granted: bool,
    /// Tools refused past the per-server cap (a floor — see
    /// [`McpServerInfo::dropped_tools`]).
    pub dropped_tools: usize,
    /// Tools trimmed to fit the per-server schema byte budget (a total — see
    /// [`McpServerInfo::trimmed_tools`]).
    pub trimmed_tools: usize,
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
