//! Driver → deck: the [`Inbound`] envelope and the read models only it carries.
//!
//! One half of the deck protocol. Everything here travels one way — the driver
//! resolves settings, the session registry and the live run into display
//! shapes, and the deck folds them without reaching back. The deck answers
//! on the other half, [`WorkspaceInput`].
//!
//! The row types at the end sit here rather than beside their tab's other read
//! models because each is carried by one [`Inbound`] case and nothing else:
//! [`SessionInfo`] by [`Inbound::Sessions`], [`NotificationInfo`] by
//! [`Inbound::Notifications`], [`InstalledAgentEntry`] by
//! [`Inbound::AgentsList`].

use stella_protocol::AgentEvent;
use stella_tool_facts::readiness::IndexReadiness;

use crate::graph::GraphSnapshot;
use crate::start_work::StartWorkDraft;

use super::*;

/// One item on the workspace inbound channel — the multi-agent envelope the
/// deck folds. `--output-format stream-json` remains one `AgentEvent` per line
/// per agent; the envelope only adds the routing tag the deck needs.
#[derive(Clone, Debug)]
pub enum Inbound {
    /// A new agent joined the workspace — its dashboard row appears.
    Register(AgentMeta),
    /// Remove one agent's dashboard row — the visual-lifecycle inverse of
    /// [`Inbound::Register`]. Presentation only: journaling and model state
    /// are unaffected (the removed lane's history stays in its own session's
    /// journal, and the engine's conversation is untouched). The driver only
    /// ever sends it for lanes with no live process behind them — e.g. the
    /// terminal worker rows of a session the deck is navigating away from,
    /// which must not linger on the next session's dashboard. Folding an
    /// unknown id is a no-op.
    Deregister { agent: AgentId },
    /// An `AgentEvent` belonging to one agent.
    Event { agent: AgentId, event: AgentEvent },
    /// A local `$` shell command's output, folded into an EXISTING lane's
    /// transcript so it reads inline in the session the user is looking at.
    ///
    /// Not [`Inbound::Event`]: that path derives agent status from the
    /// event, and `ToolStart`/`ToolResult` both mean
    /// [`AgentStatus::Running`] — so routing a `$` command through it would
    /// flip an idle agent to Running and leave it spinning forever (nothing
    /// parks a lane the engine never started). This case touches the
    /// transcript ONLY: no status, no token/cost counters, no trace row. A
    /// `$` command is user-initiated local shell, not agent activity.
    ///
    /// Folding an unknown id is a no-op — unlike `Event`, it never
    /// auto-registers, because a lane that does not exist is precisely the
    /// case the synthetic fallback lane already covers.
    ShellEvent { agent: AgentId, event: AgentEvent },
    /// One tick of a long host-side pass's live counter — `/init`'s code-graph
    /// walk and its two embedding passes — rewritten in place by
    /// [`crate::model::SessionModel::set_progress_line`], which carries the
    /// reasoning for why the fold owns the rewrite.
    ///
    /// Neither [`Inbound::Event`] (an `AgentEvent::Text` coalesces
    /// into one fold, which is what buried init's ✓ summaries under a wall of
    /// repeats) nor [`Inbound::Notice`] (a notice dwells and is gone, while a
    /// counter's final value is the part worth keeping).
    ///
    /// Transcript only: no status, no counters, and no trace row — the strip
    /// keeps naming the milestone the pass is inside. Folding an unknown id is a
    /// no-op, exactly like [`Inbound::ShellEvent`].
    Progress { agent: AgentId, text: String },
    /// A supervisor lifecycle transition not carried by the event stream.
    Status { agent: AgentId, status: AgentStatus },
    /// The dispatcher took the oldest queued prompt and handed it to an
    /// agent. The deck's [`PromptQueue`](crate::deck::PromptQueue) is FIFO on
    /// both sides of the channel, so this pops the front entry — the status
    /// bar's "queued" count goes down the moment work actually starts, and a
    /// trace row records which agent picked the prompt up.
    PromptStarted { agent: AgentId, text: String },
    /// The driver cancelled a turn on [`WorkspaceInput::StopAndHold`]
    /// (double-Esc) and returned that turn's prompt to the FRONT of its
    /// dispatch backlog. Folded as a front-insert into the deck's
    /// [`PromptQueue`](crate::deck::PromptQueue) — the exact inverse of
    /// [`Inbound::PromptStarted`]'s front-pop — so the queue view keeps
    /// matching what will actually run.
    PromptRequeued { agent: AgentId, text: String },
    /// Reset one agent's session to its seq-0 state — a `/clear`. Folded like a
    /// core event (it mutates the model, not just view state): the agent's
    /// transcript is blanked, its cost/token counters and the header clock zero
    /// out, and the progress-bar HUD returns to idle. The driver sends this on
    /// `/clear` (alongside clearing its own LLM message history).
    SessionReset { agent: AgentId },
    /// A refreshed code-graph snapshot for the Graph tab. Unlike its
    /// neighbours this is **not** a folded event — the graph is an out-of-band
    /// read-model, since a graph's structure is not in the per-session event
    /// stream. It rides the inbound channel only because that is the
    /// driver→deck path;
    /// [`crate::deck_ui::ingest_inbound`] applies it straight to the view
    /// state (`DeckUi::graph`) and the model fold ignores it. The driver
    /// sends one after `/init` rebuilds the index so the tab reflects it
    /// without a restart.
    GraphSnapshot(GraphSnapshot),
    /// How far behind the workspace's semantic index is, sent by the driver
    /// as its background embedding pass fills it and once more when that pass
    /// stops. Out-of-band view state like
    /// [`Inbound::GraphSnapshot`]: applied straight to
    /// `DeckUi::index_readiness`, ignored by the model fold. It gates one
    /// thing — a first prompt submitted while a cold workspace is still
    /// indexing (`deck_ui::gates::index_hold`).
    IndexReadiness(IndexReadiness),
    /// A refreshed slash-command vocabulary for the `/` popup. Out-of-band
    /// view state exactly like [`Inbound::GraphSnapshot`]: applied straight
    /// to `DeckUi::slash_commands` by [`crate::deck_ui::ingest_inbound`],
    /// ignored by the model fold. The driver sends one after `/init` adopts
    /// custom commands/skills so the menu reflects them without a restart.
    SlashCommands(Vec<crate::composer::SlashCommand>),
    /// The session's resolved triage / worker / verifier pins, sent once at
    /// startup by the driver — which is the only side that can call
    /// `resolve_provider` — so the statline's MODEL cell and the `/models`
    /// dialog's standing column can name all three before any turn has run.
    ///
    /// Without this they stay empty until each role's first
    /// `AgentEvent::StepUsage`, because that is the only event carrying a
    /// role/provider/model triple. A verifier that has not been reached yet
    /// would then read as unconfigured rather than unused.
    ///
    /// Folded as *configured* (`RolePin::served == false`) and drawn dim; a
    /// later `StepUsage` replaces it with what actually served. Sending it is
    /// optional — a driver that never does behaves exactly as before.
    ConfiguredRoles(Vec<(crate::deck::PipelineRole, crate::deck::RolePin)>),
    /// A mid-session re-pin: replace the named roles' pins,
    /// served evidence included. The driver sends this after a session model
    /// switch (`/model`, an assumed agent's `model:`), where
    /// [`Inbound::ConfiguredRoles`]'s never-overwrite fold is the wrong
    /// contract — the old served pin describes calls of a model that no
    /// longer serves, and keeping it would have the statline name the wrong
    /// model until the next `StepUsage`. Two verbs rather than a flag:
    /// startup intent must never clobber evidence, a switch must.
    RolePinsReset(Vec<(crate::deck::PipelineRole, crate::deck::RolePin)>),
    /// Derived prompt-cache economics for one agent's latest model call —
    /// dollars saved and the provider's cache TTL — computed by the
    /// pricing-aware producer (the CLI has the model catalog; the deck does
    /// not) and folded into the agent's [`crate::deck::AgentEntry`]. Paired
    /// with the raw `StepUsage` the same call emits (which carries the token
    /// counts the deck already folds): this adds only the two figures that
    /// need list pricing / the TTL table, keeping the single savings formula
    /// in `stella-model` and the deck free of a model-tier dependency.
    ///
    /// `savings_usd_delta` is this call's signed savings (negative when the
    /// write premium outran the reads it bought — the low-hit incident), added
    /// to the agent's running total. `ttl_secs` is the provider's prompt-cache
    /// TTL in seconds (`0` = no prompt cache / no TTL to preserve); the deck
    /// pairs it with the last provider-call time to render a live warmth
    /// countdown. `is_opt_in_provider` is whether this provider only caches
    /// behind an explicit marker (Anthropic/Bedrock/OpenRouter-Claude) —
    /// resolved once here from `stella-model`'s cache-posture table so
    /// [`crate::deck::AgentEntry::cache_diagnosis`] can name
    /// `CacheCause::OptInNeverEngaged` without the deck itself needing to
    /// know which providers require the marker.
    CacheInsight {
        agent: AgentId,
        savings_usd_delta: f64,
        ttl_secs: u64,
        is_opt_in_provider: bool,
    },
    /// The installed-agents list for the Agents tab's INSTALLED AGENTS pane.
    /// Out-of-band view state (applied straight to `DeckUi::installed` by
    /// [`crate::deck_ui::ingest_inbound`], ignored by the model fold). The
    /// driver — which owns the definitions on disk — sends one when the pane
    /// asks ([`WorkspaceInput::AgentsRefresh`]) and after every save / pin /
    /// create so the list stays live. `status`, when set, replaces the
    /// pane's hint line (op outcomes, errors).
    /// Which installed agent the lead is running as, after a
    /// [`WorkspaceInput::AgentAssume`] — `None` drops back to the plain
    /// persona. The list marks the row.
    AgentAssumed { name: Option<String> },
    AgentsList {
        entries: Vec<InstalledAgentEntry>,
        status: Option<String>,
        /// True while an LLM-assisted agent creation is still in flight
        /// driver-side (e.g. parked behind a running turn). The create
        /// dialog keeps its spinner up until a list arrives with this
        /// `false` — a queued interim snapshot must not read as done.
        creating: bool,
        /// The name of the agent a just-completed
        /// [`WorkspaceInput::AgentCreate`] installed, when this snapshot is
        /// that op's completion. The create dialog transitions into the
        /// detail preview of exactly this entry; `None` on a failed create
        /// (the dialog shows `status` as the error) and on every other
        /// snapshot.
        created: Option<String>,
    },
    /// A refreshed snapshot of the installed skills for the SKILLS tab. The
    /// driver owns the skills on disk (both scopes), their enabled/version/pin
    /// state, and the npx registry; the deck renders this read-model. Applied
    /// straight to `DeckUi::skills` by [`crate::deck_ui::ingest_inbound`],
    /// ignored by the model fold — same out-of-band contract as
    /// [`Inbound::GraphSnapshot`].
    Skills(SkillsView),
    /// The result of a registry search (`npx skills find <query>`). Folded
    /// into the SKILLS tab's search pane; out-of-band like [`Inbound::Skills`].
    SkillSearch {
        query: String,
        hits: Vec<SkillSearchHit>,
        status: Option<String>,
    },
    /// The rendered `SKILL.md` body for the ctrl+o preview overlay, fetched by
    /// the driver (`npx skills use <id>`) for a not-yet-installed search hit.
    /// Out-of-band like [`Inbound::SkillSearch`]; `id` lets the tab drop a
    /// stale reply if the user closed or re-targeted the preview meanwhile.
    SkillPreview {
        id: String,
        body: String,
        status: Option<String>,
    },
    /// A refreshed snapshot of the configured MCP servers for the MCP tab.
    /// Out-of-band view state exactly like [`Inbound::GraphSnapshot`]: applied
    /// straight to `DeckUi::mcp` by [`crate::deck_ui::ingest_inbound`], ignored
    /// by the model fold. The driver sends one at startup and after every MCP
    /// action (install, toggle, auth, remove) so the tab reflects live state.
    McpServers(Vec<McpServerInfo>),
    /// The result of an MCP registry search the tab requested
    /// ([`WorkspaceInput::McpSearch`]) — also out-of-band, applied to
    /// `DeckUi::mcp` search results.
    McpSearchResults(McpSearchOutcome),
    /// One server's assembled inspector detail ([`WorkspaceInput::McpInspect`]).
    /// Out-of-band view state like the two above. The driver may send this
    /// **twice** for one request: once immediately from what it already knows,
    /// and again after an optional registry lookup fills in a description — so
    /// the inspector opens instantly and is never blocked on the network.
    ///
    /// Boxed: the detail carries a dozen optional strings plus the whole tool
    /// table, and inlining it would make every `Inbound` — including the
    /// per-token `Event` that flows thousands of times a turn — as large as
    /// the rarest one.
    McpDetail(Box<McpServerDetail>),
    /// Open the help overlay. Sent by the driver when the user types `/help`
    /// (so the slash command reaches the same rich, scrollable panel the `?`
    /// key opens) and applied straight to `DeckUi::help_open` by
    /// [`crate::deck_ui::ingest_inbound`]. Out-of-band view state, ignored by
    /// the model fold — like [`Inbound::GraphSnapshot`].
    ShowHelp,
    /// An agent asked the driver a question and its turn is **parked** on the
    /// answer. Raises the question overlay
    /// ([`crate::views::question`]); the driver is holding a `QuestionResponder`
    /// open on the other side and unblocks only on
    /// [`WorkspaceInput::QuestionAnswered`] or its own TTL.
    ///
    /// Out-of-band view state like [`Inbound::ShowHelp`] — the model fold
    /// ignores it. A parked tool call is not conversation: nothing is said
    /// until the answer comes back, and folding a pending question into the
    /// transcript would put a question in the history that may never have
    /// been answered.
    ///
    /// Boxed for the same reason as [`Inbound::McpDetail`]: it carries a
    /// whole batch of questions with their option lists, and the per-token
    /// [`Inbound::Event`] must not grow to the size of the rarest case.
    QuestionAsked(Box<stella_protocol::QuestionRequest>),
    /// The parked question has stopped being answerable — its TTL expired, or
    /// the turn holding it was cancelled — so take the card down.
    ///
    /// The half that makes the overlay safe to leave up for the full
    /// thirty-minute TTL: without it a card would outlive its broker and
    /// still offer to resolve, and the driver would type a considered answer
    /// into a oneshot nobody is holding. Sent by the responder's own drop
    /// path, so it fires on every way the wait can end rather than only the
    /// ones somebody remembered to handle.
    QuestionWithdrawn,
    /// A gate demands a human yes/no before a tool call may run, and that
    /// dispatch is **parked** on the answer. Raises the approval card
    /// ([`crate::views::approval`]).
    ///
    /// Separate from [`Inbound::QuestionAsked`] because the two asks are
    /// different decisions: this one is a yes/no over a call already chosen,
    /// where the whole job is showing enough of the request — the tool,
    /// whether it mutates, the gate that stopped it — to make a refusal
    /// defensible. The generic `AskUser` card it replaces flattened all five
    /// fields into one line of prose.
    ///
    /// Out-of-band view state; the model fold ignores it. Boxed like its
    /// siblings so the per-token [`Inbound::Event`] does not grow to the size
    /// of the rarest case.
    ApprovalAsked(Box<stella_protocol::approval::ApprovalRequest>),
    /// The parked approval has stopped being answerable — its TTL expired (the
    /// shorter of the two deadlines; `stella-cli` sets the deck's) or the turn
    /// holding it was cancelled — so take the card down.
    ///
    /// The dispatch is denied on that path, never approved, so this is
    /// strictly about not leaving a card up that offers to decide something
    /// already decided.
    ApprovalWithdrawn,
    /// A launch-mark cue (see [`SplashCue`]): the driver holds the mark
    /// open over a running init (session startup, `/init`) and
    /// releases it when init finishes. Out-of-band view state, applied
    /// straight to `DeckUi::splash` by [`crate::deck_ui::ingest_inbound`],
    /// ignored by the model fold — like [`Inbound::ShowHelp`].
    Splash(SplashCue),
    /// A **system notification** — the deck telling the user something about
    /// the session itself: that a previous session is resumable, what the
    /// code-graph index pass found, that an `mcp.toml` went untrusted.
    ///
    /// Not [`Inbound::Event`] with an [`AgentEvent::Text`]: the
    /// transcript is the home for agent and user messages **only**, and
    /// routing chrome through it made the deck render machine chatter as
    /// though the agent had said it — then kept it in the scrollback for the
    /// rest of the session. This is out-of-band view state, applied straight
    /// to `DeckUi::notice` by [`crate::deck_ui::ingest_inbound`] and ignored
    /// by the model fold, like [`Inbound::ShowHelp`] and [`Inbound::Splash`].
    ///
    /// The deck shows these as a transient dialog ([`crate::notice`]) that any
    /// key or mouse event dismisses and that expires on its own; it never
    /// touches the transcript, so nothing here can be mistaken for the model
    /// speaking.
    Notice(String),
    /// The plugin panels this session may draw, as of one composition of the
    /// roster.
    ///
    /// **A seat is the install grant crossing into the deck.** The driver
    /// composes the roster. It drops every package whose grant is not in
    /// force, and refuses every slash name that collides with a built-in.
    /// The deck seats what survives and can add nothing of its own. An
    /// ungranted plugin gets no rectangle because it has no slot, not
    /// because the renderer declined to draw one.
    ///
    /// Sent at session open, and again whenever the roster could have
    /// changed (`/reload`). Every seat is replaced. A seat is the grant's
    /// shadow, so a plugin retracted between two of these has to lose its
    /// rectangle; merging would leave it drawing from a slot nobody
    /// renewed.
    ///
    /// Out-of-band view state, applied to `DeckUi::panels` and ignored by the
    /// model fold, like [`Inbound::Notice`] above.
    PanelsSeated {
        /// Which composition these seats came from, counted up by the driver.
        ///
        /// Stamped onto every [`WorkspaceInput::PanelFrameWanted`] the deck
        /// raises against them, so a request still in flight when the next
        /// reseat lands names a seating that has gone and starts no
        /// process. Without it a stale request reaches the driver as a bare
        /// slot index, and an index means whatever the *route* list says it
        /// means when it arrives — which is a different plugin's program.
        generation: u64,
        /// The panels that may draw, in seating order.
        seats: Vec<PanelSeat>,
    },
    /// One frame a plugin drew inside its budget, for the seat at `slot`.
    ///
    /// The tick that produced it ran as a task off the draw path
    /// (`stella_runtime::panel_host::ask`), and the lease it answered was
    /// checked with `PanelLease::admits` before this was sent — so the deck
    /// stores the frame rather than re-deciding whether it belongs.
    PanelFrame {
        /// Which seat drew it, as an index into [`Inbound::PanelsSeated`].
        slot: usize,
        /// The frame, boxed because it is much the largest thing an `Inbound`
        /// carries and every other `Inbound` would pay for it in the enum.
        frame: Box<stella_plugin::PanelFrame>,
    },
    /// A tick that produced no frame at all — the process did not start, wrote
    /// nothing readable, or answered a lease the host had moved past.
    ///
    /// Distinct from [`Inbound::PanelThrottled`], which draws a tag: this one
    /// draws nothing new and says nothing, because a plugin that failed is not
    /// a plugin that was slow, and only the second is a fact about the frame on
    /// screen. It exists so the deck can rearm the seat — a request with no
    /// answer would leave the panel waiting on a tick that will never land.
    PanelSilent {
        /// Which seat produced nothing, as an index into
        /// [`Inbound::PanelsSeated`].
        slot: usize,
    },
    /// A tick that overran its budget: the seat keeps whatever frame is on
    /// screen and the host tags it (SPEC 12.4).
    PanelThrottled {
        /// Which seat overran, as an index into [`Inbound::PanelsSeated`].
        slot: usize,
        /// What the tick actually cost.
        elapsed_ms: u64,
        /// What it was leased, so the tag reports rather than complains.
        budget_ms: u32,
    },
    /// A refreshed snapshot of the **cross-process session registry** for the
    /// SESSIONS overlay (empty-prompt `←`). Every running stella session on
    /// this machine, grouped by [`SessionPhase`]. Out-of-band view state like
    /// [`Inbound::GraphSnapshot`]; the driver answers
    /// [`WorkspaceInput::SessionsRefresh`] and every archive/delete with one.
    Sessions(Vec<SessionInfo>),
    /// A refreshed snapshot of the persist-until-read notification store for
    /// the inbox overlay and the footer's unread badge. The driver polls the
    /// store (other sessions produce into it too) and pushes one whenever the
    /// set changes. Out-of-band view state, ignored by the model fold.
    Notifications(Vec<NotificationInfo>),
    /// Progress from an in-flight MCP OAuth login the tab started
    /// ([`WorkspaceInput::McpOauthLogin`]). `outcome` is `None` while running,
    /// `Some(ok)` when finished — success triggers the tab to request a fresh
    /// snapshot so the ⚿ oauth badge flips. Out-of-band view state.
    McpOauthStatus {
        server: String,
        message: String,
        outcome: Option<bool>,
    },
    /// A refreshed snapshot of the agent-engine configuration
    /// (`settings.json` → `agent_engine_config`) for the ENGINE overlay
    /// (the SETTINGS tab's config panel). Out-of-band view state exactly like
    /// [`Inbound::GraphSnapshot`]: applied straight to `DeckUi` by
    /// [`crate::deck_ui::ingest_inbound`], ignored by the model fold. The
    /// driver sends one at startup, after every
    /// [`WorkspaceInput::EngineConfigSave`], and on
    /// [`WorkspaceInput::EngineConfigRefresh`]. `status`, when set,
    /// replaces the overlay's hint line (save outcomes, errors).
    EngineConfig {
        state: EngineConfigState,
        status: Option<String>,
    },
    /// A refreshed snapshot of the session's tool surface and the `"tools"`
    /// switches in force, for the SETTINGS tab's tool editor. Out-of-band view
    /// state exactly like [`Inbound::EngineConfig`].
    ///
    /// The driver is the only thing that can build this: MCP tools and a
    /// customer's own registered tools exist only in the assembled session
    /// stack, so a catalog-driven list would be a list of the tools Stella
    /// ships — not of the tools this operator has. Sent at startup, after
    /// every [`WorkspaceInput::ToolsSave`], and on
    /// [`WorkspaceInput::ToolsRefresh`].
    ToolPolicy {
        state: ToolPolicyState,
        status: Option<String>,
    },
    /// The answer to an ISSUES-tab [`WorkspaceInput::IssuesRefresh`] (and the
    /// follow-up refresh a successful [`WorkspaceInput::IssueCreate`]
    /// triggers): the tracker's issue list, or the error that stopped it —
    /// including the "no tracker connected" hint the tab renders as its
    /// empty state. Out-of-band view state like [`Inbound::McpSearchResults`];
    /// `seq` echoes the request so the panel can drop stale replies.
    IssuesList {
        seq: u64,
        outcome: Result<Vec<IssueRow>, String>,
    },
    /// The outcome of one ISSUES-tab mutation ([`WorkspaceInput::IssueCreate`]
    /// / [`WorkspaceInput::IssueAct`]): a human status line on success (the
    /// created key + url, "comment added", …) or the failure reason. `key` is
    /// the issue acted on (the created key for a create; empty when a create
    /// failed before a key existed). Out-of-band, seq-guarded like
    /// [`Inbound::IssuesList`].
    IssueActDone {
        seq: u64,
        key: String,
        outcome: Result<String, String>,
    },
    /// The answer to an ISSUES-tab [`WorkspaceInput::IssueDraftPlan`]: the
    /// drafted plan the start-work overlay renders, or what stopped it.
    /// Out-of-band and seq-guarded like [`Inbound::IssuesList`]; boxed because
    /// the draft carries the issue's sources and every task's contract, far
    /// more than any sibling payload on this channel.
    IssueDraft {
        seq: u64,
        outcome: Result<Box<StartWorkDraft>, String>,
    },
    /// The answer to a type-ahead [`WorkspaceInput::EntitySearch`]: the merged
    /// hit list for the create form's Assignee/Labels popup. `query` echoes
    /// the text searched (display only); `seq` echoes the request so the
    /// per-keystroke stream can drop out-of-order replies — only the newest
    /// emitted seq is ever applied.
    EntityHits {
        field: EntityField,
        seq: u64,
        query: String,
        hits: Vec<EntityHit>,
    },
    /// INSPECT overlay: every model call this execution recorded a receipt for,
    /// in wire order. Answers [`WorkspaceInput::InspectRefresh`]. An empty vec
    /// is a real answer — an execution whose receipts predate the receipts
    /// plane has none — and the overlay says so rather than hanging.
    RecordedCalls(Vec<RecordedCallInfo>),
    /// INSPECT overlay: the reconstructed context of one call, answering
    /// [`WorkspaceInput::InspectCall`]. Boxed — the message bodies are whole
    /// prompts, far larger than any other `Inbound` payload, and this case
    /// would otherwise set the size of every channel send.
    InspectedCall(Box<InspectView>),
    /// A failing gate has put a plan revision up for `agent` — SPEC 8.1 item 3.
    ///
    /// Out-of-band view-and-model state like [`Inbound::GraphSnapshot`], not
    /// an [`Inbound::Event`]: `stella_store::plan_graph::RevisionGate` authors
    /// it host-side from a `GateBoard` the host already evaluated, so the
    /// journal keeps the board and the proposal is the host's reading of it.
    /// Routing it as an event would put a host-side decision into the durable
    /// stream, where a second producer could then write one.
    ///
    /// Folded by `SessionModel::propose_revision`, which files the scrollback
    /// row and starts withholding the composer in one step.
    RevisionProposed {
        agent: AgentId,
        proposal: Box<stella_protocol::RevisionProposal>,
    },
    /// A finished dictation's text ([`WorkspaceInput::VoiceStop`]'s answer).
    /// Ingest pastes it at the composer's cursor and returns
    /// [`crate::voice::VoiceUi`] to rest — never a model fold: a dictation
    /// is keyboard input that arrived by another route.
    VoiceTranscript { text: String },
    /// A dictation that produced no text (no recorder, no credential, a
    /// refused transcription request). Ingest returns the voice state to
    /// rest and raises `reason` as a transient notice.
    VoiceFailed { reason: String },
    /// Dictation was switched on, off, or between modes by `/voice`, which
    /// runs on the driver side and has just persisted the same values to
    /// settings.
    ///
    /// Without this the deck would keep the enablement and mode it read at
    /// startup, so `/voice tap` would report success and change nothing until
    /// the next session — and "first use is one command" is the whole point
    /// of the command. Any gesture in flight is returned to rest:
    /// the keys mean something different the instant the mode changes, and a
    /// half-finished hold folded by tap-mode rules is a capture nobody can
    /// stop.
    VoiceConfig {
        enabled: bool,
        mode: crate::voice::VoiceMode,
    },
}

/// The session-registry lifecycle phase, exactly the grouping the SESSIONS
/// overlay shows. A deck-local mirror of `stella-store`'s `SessionStatus`
/// (the deck never links the store crate; the driver maps one to the other).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionPhase {
    InProgress,
    NeedsInput,
    /// Set aside with its durable state intact — quit (or switched away
    /// from) with work still pending; the first thing resume looks for.
    Paused,
    Cancelled,
    /// Ended itself by policy (stuck-loop escalation, step cap, enforced
    /// budget, ended scope review) — an ending the session chose, beside
    /// `Cancelled`, and never an error.
    Stopped,
    Complete,
    Archived,
    Error,
}

impl SessionPhase {
    /// Display/grouping order: attention-worthy first.
    pub const ALL: [SessionPhase; 8] = [
        SessionPhase::InProgress,
        SessionPhase::NeedsInput,
        SessionPhase::Paused,
        SessionPhase::Cancelled,
        SessionPhase::Stopped,
        SessionPhase::Complete,
        SessionPhase::Archived,
        SessionPhase::Error,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SessionPhase::InProgress => "In Progress",
            SessionPhase::NeedsInput => "Needs Input",
            SessionPhase::Paused => "Paused",
            SessionPhase::Cancelled => "Cancelled",
            SessionPhase::Stopped => "Stopped by policy",
            SessionPhase::Complete => "Complete",
            SessionPhase::Archived => "Archived",
            SessionPhase::Error => "Error",
        }
    }
}

/// One row of the SESSIONS overlay — a running (or finished) stella session
/// from the machine-wide registry, with the human title and work summary the
/// registry holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    /// Registry id (`ses-…`), the archive/delete handle.
    pub id: String,
    /// Human title: `<workspace basename>: <first prompt…>`.
    pub title: String,
    /// What work is involved — the latest prompt/goal, truncated.
    pub summary: String,
    /// One sentence on what the session did, written by a model from its
    /// prompts (`stella-cli`'s `sessions_view`); `None` until it has been.
    pub description: Option<String>,
    /// Workspace path (dimmed detail line).
    pub workspace: String,
    pub phase: SessionPhase,
    pub started_ms: u64,
    pub updated_ms: u64,
    /// True for the record of THIS deck process (rendered with a marker and
    /// protected from delete).
    pub mine: bool,
    /// True when the session can be reopened here: no live process owns it,
    /// it belongs to this deck's workspace, and its durable state (journal /
    /// history) is on disk. `⏎` on such a row sends
    /// [`WorkspaceInput::SessionResume`].
    pub resumable: bool,
    /// Turns recorded in the store for this session.
    pub turns: u32,
    /// Spend across those turns, in micro-dollars — integral so the row stays
    /// `Eq`, and six decimals is what the store keeps.
    pub spend_micros: u64,
    /// The model the latest turn ran on.
    pub model: Option<String>,
}

/// One persist-until-read notification as the inbox overlay lists it. A
/// mirror of `stella-store`'s `Notification` minus storage concerns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationInfo {
    pub id: String,
    pub title: String,
    pub body: String,
    /// Origin hint (session id, server name); may be empty.
    pub source: String,
    pub created_ms: u64,
    pub read: bool,
    /// The session this notification is about, when it has one — what lets
    /// the inbox's `Enter` open the session (replaying it if it has ended)
    /// via [`WorkspaceInput::SessionOpen`].
    pub session_id: Option<String>,
}

/// Driver → deck cues for the launch mark ([`crate::splash`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplashCue {
    /// Show the mark **held** open over a running init: it stands until
    /// `Release`. Ignored on `--no-anim` sessions (a static frame is their
    /// contract).
    Replay,
    /// Init finished — hand the frame straight back to the deck. A no-op if
    /// no held mark is showing.
    Release,
}

/// Which config level an installed agent definition lives at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentScope {
    /// The workspace's `.stella/agents/` directory.
    Project,
    /// The user's `~/.stella/agents/` directory.
    User,
}

impl AgentScope {
    pub fn label(self) -> &'static str {
        match self {
            AgentScope::Project => "project",
            AgentScope::User => "user",
        }
    }
}

/// One selectable version of an installed agent (the version picker's row).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentVersionInfo {
    /// 1-based version number.
    pub version: u32,
    /// Short display label (e.g. the version file's modification time), or
    /// empty when none was available.
    pub label: String,
}

/// One installed agent as the Agents tab's INSTALLED AGENTS pane lists it
/// (see [`Inbound::AgentsList`]). Decoupled from `stella-core`'s `AgentDef`
/// so the deck crate stays independent of the extensions engine — the driver
/// maps one to the other and adds the version/scope bookkeeping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledAgentEntry {
    /// The agent's loaded definition name.
    pub name: String,
    /// One-line description shown beside the name.
    pub description: String,
    /// The toolbelt grant from the definition's `tools:` frontmatter.
    /// `None` = the definition does not restrict tools — the agent gets
    /// **all** tools, and the pane says so.
    pub tools: Option<Vec<String>>,
    /// Which config level the definition lives at.
    pub scope: AgentScope,
    /// The definition file the loader reads (display/provenance).
    pub source_path: String,
    /// The pinned (active) version — 1 for a never-versioned agent.
    pub version: u32,
    /// Every version on disk, oldest first. Always contains `version`.
    pub versions: Vec<AgentVersionInfo>,
    /// The pinned version's full file content (frontmatter + body) — what
    /// the editor loads.
    pub content: String,
}
