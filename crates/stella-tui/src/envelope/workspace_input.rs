//! Deck → driver: the [`WorkspaceInput`] envelope and the types only it carries.
//!
//! The other half of the deck protocol, and the half that grows: a new deck
//! affordance is a new [`WorkspaceInput`] verb. The driver answers on the
//! other half, [`Inbound`].
//!
//! [`Secret`] and [`AgentControl`] live here because one [`WorkspaceInput`]
//! case is the only thing that carries either — and a `Secret` must reach
//! the driver without ever being logged on the way.

use std::collections::BTreeMap;

use crate::input::UserInput;

use super::*;

/// How hard a bang-persistence sigil asks the record it saves to steer.
///
/// The two sigils differ by this field alone, so one write path serves both.
/// `stella_tui::deck_ui::sigil` parses them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeepStrength {
    /// `!!` — a `should` record. It steers, and a later explicit instruction
    /// can still override it.
    Guidance,
    /// `!!!` — a `must` record at the precedence ceiling. It holds until
    /// somebody retires it.
    Rule,
}

/// What the deck sends back to the caller / engine. The single-session
/// [`UserInput`] is wrapped with the target agent; new verbs cover the deck's
/// workspace-level affordances (queueing, agent control, quit).
#[derive(Clone, Debug, PartialEq)]
pub enum WorkspaceInput {
    /// Route a `UserInput` (prompt, scope decision, ask-user answer) to one agent.
    ToAgent { agent: AgentId, input: UserInput },
    /// Queue a brand-new prompt without blocking on any busy agent — the
    /// router picks the model/agent. The deck never gates input on agent state.
    Enqueue { text: String },
    /// Queue a prompt at the FRONT: the first submission after a
    /// [`WorkspaceInput::StopAndHold`]. The deck sends this instead of
    /// [`WorkspaceInput::Enqueue`] while dispatch is held, so the user's new
    /// prompt runs before the prompt the hold returned to the queue — and
    /// receiving it is what releases the hold.
    EnqueueFront { text: String },
    /// Queue a prompt as the lead's NEXT turn, behind whatever is already
    /// waiting, and never fork it to a sidecar lane. The deck sends this for
    /// a plain prompt typed while the lead is running under the default
    /// [`MidTurnPrompt::Queue`](crate::deck_ui::MidTurnPrompt) policy, so the
    /// backlog is exactly what an Esc then steers into the running turn
    /// ([`WorkspaceInput::Steer`]). At rest it dispatches like
    /// [`WorkspaceInput::Enqueue`].
    ///
    /// A third verb rather than a flag on `Enqueue` because the driver's
    /// mid-turn arm routes a plain `Enqueue` to a sidecar (`route_mid_turn`)
    /// and `EnqueueFront` jumps the backlog; neither is "wait your turn".
    EnqueueNext { text: String },
    /// Remove one not-yet-dispatched prompt from the queue (0 = oldest). The
    /// deck's queue editor sends this for `ctrl+x` delete and for pulling a
    /// prompt back into the composer to edit it.
    QueueRemove { index: usize },
    /// Drop every not-yet-dispatched prompt (the deck confirms with a second
    /// `ctrl+d` before sending this).
    QueueClear,
    /// `/clear`, as an event rather than a queued prompt: reset the session to
    /// its seq-0 state NOW — even mid-turn, where the driver cancels the
    /// in-flight turn first. The deck sends this instead of enqueueing the
    /// text, because a reset that waits behind the backlog is not a reset (the
    /// user watched `/clear` sit in the queue popup as "pending"). The driver
    /// answers with [`Inbound::SessionReset`] once its own history is blanked;
    /// the deck's only optimistic mirror is dropping its queue view, since a
    /// session reset to seq-0 has no backlog by definition.
    SessionClear,
    /// A slash command whose declared class is **queue-free**
    /// ([`crate::composer::SlashCommand::sideband`]): the deck sends this
    /// instead of [`WorkspaceInput::Enqueue`] so the command executes at
    /// once — mid-turn included — beside the prompt queue rather than in it.
    /// The driver runs it without touching the in-flight turn, the backlog,
    /// or the conversation, and answers into the transcript over
    /// [`Inbound::ShellEvent`] (no status flip — the turn's own events keep
    /// telling the truth about the turn). `/export` is the canonical case:
    /// on this route it answers while the lead runs, rather than sitting in
    /// the queue popup as a "pending prompt" until the lead finishes. The
    /// turn-coupled exceptions (`/clear`, `/init`, `/reload`, custom
    /// expansions) never ride this route.
    Command { text: String },
    /// The agents page's "describe a task for a new session": start a NEW
    /// sub-agent lane running `text`, whatever the lead is doing. The driver
    /// answers with the ordinary lane birth — [`Inbound::Register`], a
    /// [`Inbound::PromptStarted`] on that lane, a lead-transcript notice —
    /// or, when every worker slot is taken, a [`Inbound::ShellEvent`] notice
    /// saying so. Distinct from [`WorkspaceInput::Enqueue`], whose idle-arm
    /// meaning is "the lead's next turn": this one is a lane in both arms.
    SpawnLane { text: String },
    /// How the driver settled the parked question the overlay was showing
    /// — the return leg of [`Inbound::QuestionAsked`].
    ///
    /// The whole outcome travels, not just the answers: `Deferred` ("the
    /// options are the wrong shape, let's talk") and `Declined` (cancelled)
    /// are real answers the asking model acts on differently, and collapsing
    /// them into "no answers" would tell it the same thing three ways.
    ///
    /// Boxed to match [`Inbound::QuestionAsked`]: the answer set carries a
    /// note and free text per question.
    QuestionAnswered(Box<stella_protocol::QuestionOutcome>),
    /// How the driver decided the parked approval the card was showing
    /// — the return leg of [`Inbound::ApprovalAsked`].
    ///
    /// A `Deny` carries the driver's own words when they gave any, and the
    /// broker forwards them to the model verbatim: "no, use the staging
    /// bucket" is a redirection the turn can act on, where a bare refusal is
    /// a wall it has to guess its way around.
    ApprovalAnswered(Box<stella_protocol::approval::ApprovalResponse>),
    /// Pause / resume / stop / restart a specific agent.
    Control {
        agent: AgentId,
        control: AgentControl,
    },
    /// Double-Esc: cancel `agent`'s in-flight turn, return that turn's
    /// prompt to the FRONT of the queue, and HOLD dispatch until the user's
    /// next submission — "full stop; what I type next runs first". A single
    /// Esc is the plain [`AgentControl::Stop`]: the lead SOFT-stops at the
    /// next step boundary (completed steps kept); worker lanes cancel
    /// immediately, and the next queued prompt dispatches automatically.
    StopAndHold { agent: AgentId },
    /// Esc with something queued: inject **every** waiting prompt into
    /// `agent`'s running turn at its next step boundary, in order, and keep
    /// the turn going. See [`steering`] for why this is not a cancel, and why
    /// the whole queue travels in one message.
    Steer { agent: AgentId, texts: Vec<String> },
    /// The composer's red mode: soft-stop `agent`'s running turn (completed
    /// steps kept, exactly the first-Esc stop) and run `texts` next, ahead of
    /// the parked backlog. With no running turn it degrades to "run this
    /// now", and at a worker lane it lands as a steer — a lane's next prompt
    /// is the lead's, so there is nothing for the words to outrank there.
    ///
    /// `keep` carries the bang-persistence sigils (`!!`, `!!!`): the same
    /// stop, plus the words saved as a context record. It rides this message
    /// rather than a second one so the stop and the save cannot separate — a
    /// save whose stop was dropped, or a stop whose save was, is a failure the
    /// person who typed it never sees. `None` at every other door.
    Interrupt {
        agent: AgentId,
        texts: Vec<String>,
        keep: Option<KeepStrength>,
    },
    /// The composer's broadcast address (`>@all …`, `>>> @agents !!! …`):
    /// send the words to the live sessions on this machine over their
    /// whistle sockets. [`Broadcast`] holds who hears it, how deep it
    /// reaches, whether their turns stop, and what is kept. Queue-free like a
    /// sideband command: it is about other sessions, so it never waits on
    /// this one's turn. The driver answers with a chrome note of per-session
    /// outcomes. A plain `>@all` leaves this session alone; a `>>> @agents`
    /// interrupt reaches it too, in process, because the room includes the
    /// person who spoke.
    Whistle(Broadcast),
    /// Re-root the Graph tab on `file`: the deck's file picker sends this when
    /// the user selects a file, and the driver answers with a fresh
    /// [`Inbound::GraphSnapshot`] centered on it (the same out-of-band refresh
    /// path `/init` uses). `stella-tui` cannot query the graph store itself, so
    /// re-rooting is a round-trip rather than a local recompute — the picker
    /// only knows the file *names* (shipped in [`GraphSnapshot::files`](crate::graph::GraphSnapshot::files)), never
    /// their neighborhoods. `file` is a root-relative path from that list.
    FocusGraphFile { file: String },
    /// Re-root the Graph tab on a free-form query — a symbol name, or any
    /// text the driver's index can resolve to definitions. Sent by the tab's
    /// `q` box; answered, like [`Self::FocusGraphFile`], with a fresh
    /// [`Inbound::GraphSnapshot`], whose
    /// [`query`](crate::graph::GraphSnapshot::query) echoes `text` back so the bar shows
    /// what the neighborhood on screen actually answers.
    ///
    /// A separate verb rather than a mode flag on `FocusGraphFile`: the two
    /// resolve differently on the driver side (a path lookup versus a symbol
    /// lookup), and a query that matches nothing has to be able to say so
    /// without being mistaken for a missing file.
    GraphQuery { text: String },
    /// The INSTALLED AGENTS pane opened (or wants a reload): enumerate the
    /// agent definitions installed at both scopes and answer with
    /// [`Inbound::AgentsList`].
    AgentsRefresh,
    /// Save an edited agent definition as a NEW version and pin it — the
    /// prior version is preserved on disk (see `stella-cli`'s
    /// `agents_installed` module for the on-disk scheme). The driver
    /// answers with a fresh [`Inbound::AgentsList`].
    AgentSave {
        name: String,
        scope: AgentScope,
        content: String,
    },
    /// Re-pin an existing version WITHOUT editing — the version count never
    /// changes on a pin (increments happen only on [`WorkspaceInput::AgentSave`]).
    AgentPin {
        name: String,
        scope: AgentScope,
        version: u32,
    },
    /// INSTALLED AGENTS `x x`: delete the definition — its canonical file
    /// and its archived versions. Answered with a fresh
    /// [`Inbound::AgentsList`].
    AgentDelete { name: String, scope: AgentScope },
    /// INSTALLED AGENTS `a`, or the `/agent` picker: the lead assumes this
    /// agent's identity for the rest of the session — its definition becomes
    /// the persona the system prompt carries, its `tools:` grant narrows the
    /// session tool policy, and its declared `model:` (if any) switches the
    /// session model, all from the next turn on. Between turns only;
    /// mid-turn the driver answers with a transcript notice. Answered with
    /// [`Inbound::AgentAssumed`].
    AgentAssume { name: String, scope: AgentScope },
    /// The `/model` picker's selection (or `/model <spec>` typed): switch
    /// THIS session's model to `spec` (`provider/slug`). Session-only — the
    /// driver never writes settings for it, so future sessions keep the
    /// configured default. Between turns only, like
    /// [`WorkspaceInput::AgentAssume`]; the driver answers with a fresh
    /// [`Inbound::Register`] + [`Inbound::ConfiguredRoles`] and a chrome
    /// note (on refusal, the note alone).
    ModelOverride { spec: String },
    /// Create a new agent from a short description with LLM assistance: the
    /// driver drafts the definition through the session's provider, installs
    /// it at `scope`, and answers with a fresh [`Inbound::AgentsList`].
    AgentCreate {
        description: String,
        scope: AgentScope,
    },
    /// A SKILLS-tab operation (list / enable / uninstall / search / install /
    /// create / edit / pin). The driver owns the skills on disk + npx + model
    /// and answers with a refreshed [`Inbound::Skills`] / [`Inbound::SkillSearch`].
    Skill(SkillOp),
    /// MCP tab: flip a configured server's session enable/disable state. The
    /// driver toggles the shared disabled-servers set (hiding/showing the
    /// server's tools on the next model call) and pushes a fresh
    /// [`Inbound::McpServers`] snapshot.
    McpToggle { name: String },
    /// MCP tab: search the configured registry for `query`. The driver runs
    /// the async search and replies with [`Inbound::McpSearchResults`].
    McpSearch { query: String },
    /// MCP tab: install the registry server named `name` into `.stella/mcp.toml`
    /// (then refresh the snapshot).
    McpInstall { name: String },
    /// MCP tab: remove a configured server from `.stella/mcp.toml`.
    McpRemove { name: String },
    /// MCP tab: set an auth credential (env var for stdio, header for http) on a
    /// configured server. The value is a [`Secret`] — its `Debug` is redacted,
    /// so it never reaches the deck's debug log.
    McpAuth {
        server: String,
        field: String,
        value: Secret,
    },
    /// MCP tab: record the operator's capability grant for a configured
    /// server, after the first-enable handshake showed what it declares
    /// (SPEC §9.3).
    ///
    /// The driver writes `granted = true` into `.stella/mcp.toml`, adds the
    /// name to the session's shared grant set — so the server becomes usable
    /// in the session it was reviewed in, without a reconnect — enables it,
    /// and pushes a fresh [`Inbound::McpServers`] snapshot.
    ///
    /// Only ever sent for a grant. There is no `McpGrant { granted: false }`:
    /// declining is not answering, the gate's default is withheld, and a
    /// parameter that selects between granting and revoking would be two verbs
    /// wearing one name — the single-purpose rule AGENTS.md #9 states.
    McpGrant { name: String },
    /// MCP tab: rebuild and re-push the [`Inbound::McpServers`] snapshot.
    McpRefresh,
    /// MCP tab: assemble the ctrl+o inspector detail for one configured server
    /// and answer with [`Inbound::McpDetail`].
    ///
    /// `lookup` asks the driver to consult the registry when the server has no
    /// recorded description, backfilling `mcp.toml` with what it finds. Opt-in
    /// per request rather than automatic: the registry is a third-party
    /// service, and merely looking at a tab must not talk to one.
    McpInspect { name: String, lookup: bool },
    /// MCP tab: start the browser OAuth login for a configured **http**
    /// server. The driver runs the flow in the background and streams
    /// [`Inbound::McpOauthStatus`] updates (including the authorize URL).
    McpOauthLogin { server: String },
    /// SESSIONS overlay opened (or `r`): read the machine-wide session
    /// registry and answer with [`Inbound::Sessions`].
    SessionsRefresh,
    /// SESSIONS overlay / inbox: open a session in a replay lane. The driver
    /// loads that session's stored event journal, keyed by `session_id`. It
    /// then answers with a normal [`Inbound::Register`] for a `replay:<id>`
    /// lane, every stored event as an ordinary [`Inbound::Event`], and a
    /// closing [`Inbound::Status`].
    ///
    /// Replay IS the fold. A session dead for twelve hours comes back in the
    /// state it died in, and no second rendering path draws it.
    SessionOpen { id: String },
    /// SESSIONS overlay: tuck a session record away (status → Archived).
    /// Answered with a fresh [`Inbound::Sessions`].
    SessionArchive { id: String },
    /// SESSIONS overlay: delete a session record from the registry.
    /// Answered with a fresh [`Inbound::Sessions`].
    SessionDelete { id: String },
    /// SESSIONS overlay: reopen a resumable session (⏎ on a
    /// [`SessionInfo::resumable`] row) — the deck-native "navigate back
    /// into a session". The driver parks the current session (its durable
    /// state is already on disk; its record flips to Paused), replays the
    /// chosen session's journal through the fold, restores its conversation
    /// and prompt backlog, and re-owns its registry record. Only serviced
    /// between turns — mid-turn the driver answers with a transcript notice
    /// instead of tearing down live work.
    SessionResume { id: String },
    /// SESSIONS overlay `n`: park this session and open a fresh, empty one
    /// — the same hand-over [`WorkspaceInput::SessionResume`] performs, with
    /// a new record in place of a stored one. Between turns only; mid-turn
    /// the driver answers with a transcript notice.
    SessionNew,
    /// Inbox overlay: mark one notification read (it may then be pruned —
    /// "persists until read" is the store's contract). Answered with a fresh
    /// [`Inbound::Notifications`].
    NotificationRead { id: String },
    /// Inbox overlay: mark everything read.
    NotificationsReadAll,
    /// ENGINE overlay: persist the edited agent-engine configuration into
    /// `settings.json` at `scope` (project `.stella/settings.json` or the
    /// user's `~/.stella/settings.json`). The driver writes the
    /// `agent_engine_config` object — preserving every other key in the
    /// file — and answers with a fresh [`Inbound::EngineConfig`] carrying
    /// the save outcome in `status`. Saved config applies to runs started
    /// afterwards; in-flight turns keep their resolved models.
    EngineConfigSave {
        state: EngineConfigState,
        scope: AgentScope,
    },
    /// ENGINE overlay opened (or wants a reload): re-read the settings
    /// scope chain and answer with a fresh [`Inbound::EngineConfig`].
    EngineConfigRefresh,
    /// One seated panel wants its next frame, against the rectangle the deck
    /// has just drawn it into.
    ///
    /// **This is what makes the panel loop run off the draw path** (SPEC 12.4).
    /// The deck cannot ask a plugin for a frame itself — a repaint that waits
    /// on somebody else's process is a frozen terminal — so it asks the
    /// driver, which spawns the exchange as a task and answers with
    /// [`Inbound::PanelFrame`], [`Inbound::PanelThrottled`] or
    /// [`Inbound::PanelSilent`]. Exactly one of the three comes back for every
    /// one of these, which is how the seat knows to ask again.
    ///
    /// The geometry rides on the request because the lease is the deck's to
    /// state: a plugin addresses cells inside a rectangle the host measured,
    /// and the driver has never seen the terminal.
    PanelFrameWanted {
        /// The seating this request belongs to, echoed from
        /// [`Inbound::PanelsSeated`].
        ///
        /// The driver refuses a request naming any other, which is what stops
        /// a request minted before a reseat from starting the plugin that now
        /// holds its slot. `PanelSlot::settle` is the same rule on the return
        /// leg and does not subsume this one: that one keeps a stale *frame*
        /// off the screen, and by the time a frame exists somebody's program
        /// has already run.
        generation: u64,
        /// Which seat, as an index into [`Inbound::PanelsSeated`].
        slot: usize,
        /// The host's counter for this frame, echoed on the frame that
        /// answers so a late one is discardable.
        tick: u64,
        /// The columns the deck leased it, inside the host's chrome.
        cols: u16,
        /// The rows the deck leased it, inside the host's chrome.
        rows: u16,
    },
    /// TOOLS panel: persist the operator's tool switches into `settings.json`
    /// at `scope`, answered with a fresh [`Inbound::ToolPolicy`].
    ///
    /// `switches` carries only the keys the panel actually changed — a tool
    /// name, a group name, or `"*"` — and the driver merges them into that
    /// scope's own `"tools"` object rather than replacing it. Sending the
    /// whole merged map instead would copy the OTHER scopes' switches into
    /// the file being written and freeze them there.
    ToolsSave {
        switches: BTreeMap<String, bool>,
        scope: AgentScope,
    },
    /// TOOLS panel opened (or wants a reload): re-read the settings scope
    /// chain, re-enumerate the session's tools, and answer with a fresh
    /// [`Inbound::ToolPolicy`].
    ToolsRefresh,
    /// ISSUES tab: list (or tracker-search) issues. `query`/`state` are the
    /// tracker-side filters; the driver answers with [`Inbound::IssuesList`]
    /// echoing `seq` so stale replies can be dropped.
    ///
    /// `page` is 0-based and is what the tab's `]`/`[` move. It rides the
    /// request rather than being held driver-side because the panel is the
    /// only thing that knows which page the human is looking at — and a
    /// paging key that could not say so would re-fetch page one under a
    /// notice claiming otherwise.
    IssuesRefresh {
        query: Option<String>,
        state: Option<String>,
        page: usize,
        seq: u64,
    },
    /// ISSUES tab: create an issue from the `n` form. The driver answers
    /// with [`Inbound::IssueActDone`] (the created key + url on success) and
    /// then a fresh [`Inbound::IssuesList`] under the same `seq`.
    IssueCreate {
        title: String,
        body: String,
        labels: Vec<String>,
        assignee: Option<String>,
        seq: u64,
    },
    /// ISSUES tab: act on one existing issue (comment / set-status / start
    /// work). Answered with [`Inbound::IssueActDone`].
    IssueAct {
        key: String,
        action: IssueAction,
        seq: u64,
    },
    /// ISSUES tab: draft a plan for one issue (`w`), from the issue's text,
    /// the files the code graph couples to it, and the memory RULEs that
    /// apply. Answered with [`Inbound::IssueDraft`].
    ///
    /// Reads only. The draft is what the human approves, so nothing on this
    /// path may create a branch, take a claim, or spend a model call — see
    /// [`WorkspaceInput::IssueStartWork`], which is the one request that does.
    IssueDraftPlan { key: String, seq: u64 },
    /// ISSUES tab: approve a drafted plan and start work on it (`a` in the
    /// start-work overlay). `tasks` is the draft's task list **after** the
    /// human's edits, in plan order.
    ///
    /// This is the only ISSUES-tab request that changes anything outside the
    /// tracker: the driver takes the workspace's `issue:<n>` dispatch claim,
    /// opens the branch under it, and authors the plan's first revision.
    /// Answered with [`Inbound::IssueActDone`].
    IssueStartWork {
        key: String,
        tasks: Vec<String>,
        seq: u64,
    },
    /// ISSUES tab: one per-keystroke type-ahead query from the create form's
    /// Assignee/Labels field. Answered with [`Inbound::EntityHits`] echoing
    /// `seq` — the panel keeps only the newest.
    EntitySearch {
        field: EntityField,
        query: String,
        seq: u64,
    },
    /// INSPECT overlay: list the model calls this execution recorded, so a
    /// human can pick one. Answered with [`Inbound::RecordedCalls`].
    InspectRefresh,
    /// INSPECT overlay: reconstruct one call's context. Answered with
    /// [`Inbound::InspectedCall`]. A blocking SQLite read on the driver side —
    /// it replays the block registry and the event journal — so it is served
    /// off the pump, like [`WorkspaceInput::FocusGraphFile`].
    InspectCall {
        turn_instance: u32,
        step: u64,
        call_seq: u64,
    },
    /// Plan card (`/plan`), `x`: ask the driver to skip one still-open step on
    /// `agent`'s plan. Send-and-forget — the row's state changes only when
    /// the driver's next `TaskUpdate` snapshot folds back, so the card can
    /// never show a skip the engine refused.
    TaskSkip { agent: AgentId, id: String },
    /// `u` on a highlighted delete event: restore what that one `delete_file`
    /// call removed, from git — the row's `· git-backed · u undo` affordance
    /// (SPEC 6.3, SPEC 11). `paths` carries the call's targets, several for a
    /// batch delete, so the input scopes exactly one event. The driver answers
    /// with [`Inbound::Notice`] naming what was restored or why it could not
    /// be — an untracked file has no git reading to restore from.
    UndoDelete { paths: Vec<String> },
    /// `x` on a highlighted memory row: tombstone that memory so it stops
    /// steering the agent and the reflection loop stops re-learning it — the
    /// row's `· x reject` affordance (SPEC 6.3).
    ///
    /// `text` rides along because the tombstone is content-addressed as well
    /// as id-addressed: the loop re-mines paraphrases, so a rejection stored
    /// against the id alone would be undone by the next turn that re-learned
    /// the same lesson under a new one. The driver answers with
    /// [`Inbound::Notice`].
    RejectMemory { memory_id: String, text: String },
    /// `e edit` on a logged memory row (SPEC 6.3): a revision on the memory's
    /// lineage, so the old words stop being served and the id does not change.
    ///
    /// `text` is the replacement only. [`Self::RejectMemory`] carries the old
    /// words too, because a tombstone has to recognise a re-mined paraphrase;
    /// an edit names a lineage the id already identifies.
    EditMemory { memory_id: String, text: String },
    /// `r` on a highlighted gate board: ask for one gate to be evaluated
    /// again — SPEC 8.1's `r rerun gate`.
    ///
    /// A *request*, and the driver's answer is an [`Inbound::Notice`] either
    /// way. Verification belongs to the plugin that reported the evidence and
    /// stella never re-runs a gate itself (AGENTS.md's opening), so a door with
    /// no verification plugin bound has no gate to re-request and says so
    /// rather than pretending. That is the whole of the contract here: the key
    /// reaches a driver, the driver answers in words, and no surface renders a
    /// re-run that did not happen.
    RerunGate {
        /// The gate's name, as its row states it.
        gate: String,
    },
    /// `a` on a highlighted revision proposal: make the plan revision the
    /// failing gate put up — SPEC 8.1's `a approve r4`.
    ///
    /// The whole proposal travels rather than its subject alone, because the
    /// driver re-checks it before writing: `RevisionGate::approve` refuses a
    /// proposal whose revision number the plan has moved past, and a subject
    /// on its own carries no number to check.
    ///
    /// `e edit` is absent from this enum on purpose: it is answered inside the
    /// deck, which hands the subject to the composer and stands the proposal
    /// down, so a round trip would be a message whose only effect is latency.
    ApproveRevision {
        agent: AgentId,
        proposal: Box<stella_protocol::RevisionProposal>,
    },
    /// `x` on a highlighted revision proposal: drop it — SPEC 8.1's
    /// `x dismiss`.
    ///
    /// It writes no revision, and it still has to travel. A running turn
    /// withholds its tool calls while the change stands, and only the driver
    /// can reach the gate that withholds them. A dismissal the deck answered
    /// alone would clear the card and leave the turn refused.
    DismissRevision {
        agent: AgentId,
        proposal: Box<stella_protocol::RevisionProposal>,
    },
    /// Plan card (`/plan`), post-approval `e`: ask the driver to open a
    /// scope-change proposal for `agent`'s locked scope. The deck never edits
    /// scope locally — a granted change arrives back as a fresh
    /// `ScopeReview` fold.
    ScopeChangeRequest { agent: AgentId },
    /// Budget editor (`/budget`): set (or with `None` clear) the session
    /// spend cap. The deck renders only the cap the budget stream folds back
    /// (`AgentEvent::BudgetTick::session_limit_usd`), so an ignored or
    /// clamped request never shows a cap that is not in force.
    SetBudget { limit_usd: Option<f64> },
    /// Push-to-talk crossed its warmup: start capturing microphone audio.
    /// The driver answers the eventual [`WorkspaceInput::VoiceStop`] with
    /// [`Inbound::VoiceTranscript`] or [`Inbound::VoiceFailed`]; a start it
    /// cannot honour answers [`Inbound::VoiceFailed`] immediately.
    VoiceStart,
    /// The held key was released: stop capturing and transcribe what was
    /// heard.
    VoiceStop,
    /// Abandon the capture without transcribing (Esc while listening).
    VoiceCancel,
    /// Tear down the deck.
    Quit,
}

/// A secret string whose `Debug` is redacted, so it can ride the deck's input
/// channel (and any debug log of it) without leaking. The value is readable
/// only via [`Secret::reveal`], used solely to write the credential to config.
///
/// The plaintext is wiped on drop, so a credential typed into the MCP auth
/// prompt does not sit in freed heap for the rest of the deck session. The
/// wipe covers the buffer this value owns at drop time and nothing else —
/// notably not an earlier allocation abandoned when the underlying `String`
/// grew, which no `Drop` impl can reach.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }
    /// The raw value — only for writing the credential into config.
    pub fn reveal(&self) -> &str {
        &self.0
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        use zeroize::Zeroize as _;
        self.0.zeroize();
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Secret").field(&"<redacted>").finish()
    }
}

/// The agent-control verbs the dashboard offers, each sent as a
/// [`WorkspaceInput::Control`]. All five are live. The Agents tab binds `s`
/// to Stop, `r` to Restart, and `p` to Pause or Resume by the row's current
/// status. Esc sends Stop for the focused agent.
///
/// The driver acts on four of them on a worker lane. Pause parks the worker
/// at its next step boundary, never mid-tool. Restart respawns the lane from
/// its retained spec. Delete stops a live worker, then takes the lane's row
/// off the deck for good, spec included. On the lead all four are no-ops:
/// its interrupt is Esc, which sends Stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentControl {
    Pause,
    Resume,
    Stop,
    Restart,
    Delete,
}

impl AgentControl {
    pub fn label(self) -> &'static str {
        match self {
            AgentControl::Pause => "pause",
            AgentControl::Resume => "resume",
            AgentControl::Stop => "stop",
            AgentControl::Restart => "restart",
            AgentControl::Delete => "delete",
        }
    }
}
