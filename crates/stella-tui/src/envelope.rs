//! Multi-agent wire types — the envelope that turns the single-session
//! `AgentEvent` stream into a workspace of many agents.
//!
//! The [`run_deck`](crate::deck_shell::run_deck)`(events, submissions)` shell speaks
//! one `AgentEvent` stream for one session. The command deck speaks
//! [`Inbound`] (an agent-id-tagged event) in and [`WorkspaceInput`] out, so N
//! agents share one deck. A single-agent session is just one [`AgentId`].
//!
//! This keeps the L-T1 purity per agent: each agent's derived state is still a
//! pure fold of *its* `AgentEvent`s; the envelope only adds the routing tag the
//! deck needs to keep N folds side by side.

use std::collections::BTreeMap;

use stella_protocol::AgentEvent;

mod inspect;

pub use inspect::{InspectMessage, InspectSection, InspectView, JournalEra, RecordedCallInfo};

use stella_tools::search::readiness::IndexReadiness;

use crate::graph::GraphSnapshot;
use crate::input::UserInput;

/// Stable identifier for one agent/run within the workspace. Human-meaningful
/// where possible (`"lead"`, `"sub:auth-refactor"`) — it is shown on screen, so
/// it is never a raw UUID as the primary label (the L-C4 cite-by-label spirit).
pub type AgentId = String;

/// Everything the dashboard needs to introduce an agent before its first event.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentMeta {
    pub id: AgentId,
    /// The project / goal shown in the dashboard row.
    pub title: String,
    /// `"lead"` | `"subagent"` | free-form role label.
    pub role: String,
    /// OS process id for CPU/MEM attribution, once known.
    pub pid: Option<u32>,
    /// The model handling this agent, once routed.
    pub model: Option<String>,
    /// The reasoning effort the agent's calls are pinned to (`low` … `max`),
    /// as the driver resolved it; `None` when nothing pinned one.
    pub effort: Option<String>,
    /// One sentence on what the agent is for — the task it was handed, in
    /// the words it was handed. `None` falls back to `title` on screen.
    pub purpose: Option<String>,
    /// The agent that dispatched this one — its place in the session's agent
    /// tree. `None` for a root (the lead). The deck walks this for Backspace
    /// (back to the dispatcher), the breadcrumb (`lead ▸ sub:2`), and the
    /// SUB-AGENTS overlay's `f` (flag the lane to whoever dispatched it):
    /// all three need the same answer, and a guess (`"lead"`) would be wrong
    /// the day a lane dispatches a lane.
    pub parent: Option<AgentId>,
    /// Wall-clock start (ms since epoch) for elapsed / $-per-hour.
    pub started_ms: u64,
}

impl AgentMeta {
    /// A minimal meta with the free-form defaults filled in.
    pub fn new(id: impl Into<AgentId>, title: impl Into<String>, started_ms: u64) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            role: "agent".to_string(),
            pid: None,
            model: None,
            effort: None,
            purpose: None,
            parent: None,
            started_ms,
        }
    }

    /// Builder: name the agent that dispatched this one.
    pub fn with_parent(mut self, parent: impl Into<AgentId>) -> Self {
        self.parent = Some(parent.into());
        self
    }

    /// Builder: set the pinned reasoning effort.
    pub fn with_effort(mut self, effort: impl Into<String>) -> Self {
        self.effort = Some(effort.into());
        self
    }

    /// Builder: set the one-sentence purpose.
    pub fn with_purpose(mut self, purpose: impl Into<String>) -> Self {
        self.purpose = Some(purpose.into());
        self
    }

    /// Builder: set the role.
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.role = role.into();
        self
    }

    /// Builder: set the OS pid (for resource attribution).
    pub fn with_pid(mut self, pid: u32) -> Self {
        self.pid = Some(pid);
        self
    }
}

/// The lifecycle status of an agent. Most transitions are derivable from the
/// `AgentEvent` stream (a `Stage` means running, `Complete` means done, an
/// `Error` means failed, `AskUser` means waiting) — but `Queued`, `Paused`,
/// and `Killed` are supervisor states that are *not* in the event stream, so
/// they arrive via [`Inbound::Status`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentStatus {
    Queued,
    Running,
    Paused,
    WaitingInput,
    Done,
    Failed,
    Killed,
}

impl AgentStatus {
    /// True while the agent is actively holding resources / dispatchable.
    pub fn is_active(self) -> bool {
        matches!(self, AgentStatus::Running | AgentStatus::WaitingInput)
    }

    /// True once the agent has reached a terminal state.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            AgentStatus::Done | AgentStatus::Failed | AgentStatus::Killed
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            AgentStatus::Queued => "queued",
            AgentStatus::Running => "running",
            AgentStatus::Paused => "paused",
            AgentStatus::WaitingInput => "needs input",
            AgentStatus::Done => "done",
            AgentStatus::Failed => "failed",
            AgentStatus::Killed => "killed",
        }
    }
}

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
    /// A local `!` shell command's output, folded into an EXISTING lane's
    /// transcript so it reads inline in the session the user is looking at.
    ///
    /// Deliberately not [`Inbound::Event`]: that path derives agent status
    /// from the event, and `ToolStart`/`ToolResult` both mean
    /// [`AgentStatus::Running`] — so routing a `!` command through it would
    /// flip an idle agent to Running and leave it spinning forever (nothing
    /// parks a lane the engine never started). This variant touches the
    /// transcript ONLY: no status, no token/cost counters, no trace row. A
    /// `!` command is user-initiated local shell, not agent activity.
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
    /// Deliberately neither [`Inbound::Event`] (an `AgentEvent::Text` coalesces
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
    /// A refreshed code-graph snapshot for the Graph tab. Unlike the other
    /// variants this is **not** a folded event — the graph is an out-of-band
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
    /// stops (#4043). Out-of-band view state like
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
    /// A deliberate mid-session re-pin: replace the named roles' pins,
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
    /// answer (#4220). Raises the question overlay
    /// ([`crate::v2::question`]); the driver is holding a `QuestionResponder`
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
    /// [`Inbound::Event`] must not grow to the size of the rarest variant.
    QuestionAsked(Box<stella_protocol::QuestionRequest>),
    /// The parked question is no longer answerable — its TTL expired, or the
    /// turn holding it was cancelled — so take the card down.
    ///
    /// The half that makes the overlay safe to leave up for the full
    /// thirty-minute TTL: without it a card would outlive its broker and
    /// still offer to resolve, and the driver would type a considered answer
    /// into a oneshot nobody is holding. Sent by the responder's own drop
    /// path, so it fires on every way the wait can end rather than only the
    /// ones somebody remembered to handle.
    QuestionWithdrawn,
    /// A gate demands a human yes/no before a tool call may run, and that
    /// dispatch is **parked** on the answer (#4240). Raises the approval card
    /// ([`crate::views::approval`]).
    ///
    /// Separate from [`Inbound::QuestionAsked`] because the two asks are
    /// different decisions: this one is a yes/no over a call already chosen,
    /// where the whole job is showing enough of the request — the tool,
    /// whether it mutates, the gate that stopped it — to make a refusal
    /// defensible. It rode the generic `AskUser` card until #4240, which
    /// flattened all five fields into one line of prose.
    ///
    /// Out-of-band view state; the model fold ignores it. Boxed like its
    /// siblings so the per-token [`Inbound::Event`] does not grow to the size
    /// of the rarest variant.
    ApprovalAsked(Box<stella_tools::registry::approval::ApprovalRequest>),
    /// The parked approval is no longer answerable — its TTL expired (the
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
    /// Deliberately not [`Inbound::Event`] with an [`AgentEvent::Text`]: the
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
    /// prompts, far larger than any other `Inbound` payload, and this variant
    /// would otherwise set the size of every channel send.
    InspectedCall(Box<InspectView>),
}

/// One row of the ISSUES tab's browse list — tracker-agnostic: the driver
/// maps whatever issue source it carries into this shape, and the TUI never
/// learns which tracker it was.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IssueRow {
    /// `#123` (GitHub) or `ENG-123` (Linear).
    pub key: String,
    pub title: String,
    pub state: String,
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    pub url: String,
    pub updated_at: Option<String>,
}

/// One row of the create form's type-ahead popup. `kind` is a display type
/// label ("Person", "Agent", "Memory", "Symbol", "Label", …) — rows render as
/// `Kind: label — description`. `insert` is what picking the row writes into
/// the field: `@login` or an email for people, the label name for labels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EntityHit {
    pub kind: String,
    pub label: String,
    pub description: String,
    pub insert: String,
}

/// Which create-form field a type-ahead [`WorkspaceInput::EntitySearch`]
/// feeds — each has its own vocabulary (people vs. labels).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntityField {
    Assignee,
    Label,
}

impl EntityField {
    pub fn label(self) -> &'static str {
        match self {
            EntityField::Assignee => "assignee",
            EntityField::Label => "labels",
        }
    }
}

/// An action on one existing issue ([`WorkspaceInput::IssueAct`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IssueAction {
    /// Add a comment (the deck's `c` prompt).
    Comment(String),
    /// Move to a named status (any workflow-state word on Linear; on GitHub
    /// only the two states below exist, and they have their own variants).
    SetStatus(String),
    /// Close the issue (the deck's `x` on an open row).
    Close,
    /// Re-open a closed issue (the deck's `x` on a closed row).
    ///
    /// Its own variant rather than `SetStatus("open")` so the driver selects
    /// the provider call by matching a variant instead of comparing a status
    /// string — the same reason [`IssueAction::Close`] is not
    /// `SetStatus("closed")`, and why the port grew
    /// `IssueProvider::reopen` rather than a status setter.
    Reopen,
    /// Start work: the driver moves the issue to in-progress (`w`).
    StartWork,
}

/// The session-registry lifecycle phase, exactly the grouping the SESSIONS
/// overlay shows. A TUI-local mirror of `stella-store`'s `SessionStatus`
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
    /// budget, ended scope review) — a deliberate ending beside `Cancelled`,
    /// never an error (#1653).
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
    /// the inbox's `Enter` open the session (replaying it if it is no longer
    /// live) via [`WorkspaceInput::SessionOpen`].
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
/// so the TUI crate stays independent of the extensions engine — the driver
/// maps one to the other and adds the version/scope bookkeeping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledAgentEntry {
    /// The agent's loaded definition name.
    pub name: String,
    /// One-line description shown beside the name.
    pub description: String,
    /// The toolbelt grant from the definition's `tools:` frontmatter.
    /// `None` = the definition does not restrict tools — the agent gets
    /// **all** tools, and the pane says so honestly.
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
    /// How the driver settled the parked question the overlay was showing
    /// (#4220) — the return leg of [`Inbound::QuestionAsked`].
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
    /// (#4240) — the return leg of [`Inbound::ApprovalAsked`].
    ///
    /// A `Deny` carries the driver's own words when they gave any, and the
    /// broker forwards them to the model verbatim: "no, use the staging
    /// bucket" is a redirection the turn can act on, where a bare refusal is
    /// a wall it has to guess its way around.
    ApprovalAnswered(Box<stella_tools::registry::approval::ApprovalResponse>),
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
    /// Re-root the Graph tab on `file`: the deck's file picker sends this when
    /// the user selects a file, and the driver answers with a fresh
    /// [`Inbound::GraphSnapshot`] centered on it (the same out-of-band refresh
    /// path `/init` uses). `stella-tui` cannot query the graph store itself, so
    /// re-rooting is a round-trip rather than a local recompute — the picker
    /// only knows the file *names* (shipped in [`GraphSnapshot::files`]), never
    /// their neighborhoods. `file` is a root-relative path from that list.
    FocusGraphFile { file: String },
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
    /// loads the session's persisted event journal from the store (linked by
    /// `session_id` since store schema v8), then answers with a normal
    /// [`Inbound::Register`] (a `replay:<id>` lane) followed by every
    /// persisted event as ordinary [`Inbound::Event`]s and a terminal
    /// [`Inbound::Status`] — replay IS the fold, so a session dead for 12
    /// hours reconstructs to exactly the state it died in, with no second
    /// rendering path.
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
    /// Tear down the deck.
    Quit,
}

/// The `/models` role table: the open role vocabulary and its render order.
pub mod roles;

mod engine_config;
/// The MCP tab's read models (rows, search results, inspector detail).
mod mcp;
mod skills;
/// Why Esc steers rather than cancels — doc-only.
pub mod steering;
mod tool_policy;
pub use engine_config::{EngineAgentState, EngineConfigState, EngineRole, RoleWiringRow, SeatRow};
pub use mcp::{
    McpLiveIdentity, McpLookupState, McpSearchItem, McpSearchOutcome, McpServerDetail,
    McpServerInfo, McpToolRow,
};
pub use roles::{RoleTableEntry, role_table};
pub use skills::{SkillOp, SkillRow, SkillScope, SkillSearchHit, SkillsView};
pub use tool_policy::{ToolDenial, ToolPolicyState, ToolRow, ToolScope};

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

/// The agent-control verbs surfaced by the dashboard, each sent as a
/// [`WorkspaceInput::Control`]. All five are live: the Agents tab binds `s`
/// (Stop), `p` (Pause/Resume, toggled by the row's current status), and `r`
/// (Restart), and Esc sends Stop for the focused agent. The driver honors
/// Pause/Resume/Restart/Delete on worker lanes — pause parks the worker at
/// its next step boundary (never mid-tool), restart respawns the lane from
/// its retained spec, delete stops the worker if one is live and then takes
/// the lane's row off the deck for good, spec included — and treats them as
/// no-ops on the lead, whose interrupt is Esc (Stop).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_and_terminal_are_disjoint() {
        for s in [
            AgentStatus::Queued,
            AgentStatus::Running,
            AgentStatus::Paused,
            AgentStatus::WaitingInput,
            AgentStatus::Done,
            AgentStatus::Failed,
            AgentStatus::Killed,
        ] {
            assert!(!(s.is_active() && s.is_terminal()), "{s:?}");
        }
    }

    #[test]
    fn meta_builder_sets_fields() {
        let m = AgentMeta::new("lead", "acme-api", 1000)
            .with_role("lead")
            .with_pid(4242);
        assert_eq!(m.id, "lead");
        assert_eq!(m.role, "lead");
        assert_eq!(m.pid, Some(4242));
    }
}
