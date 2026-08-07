//! The workspace model — the command deck's derived state.
//!
//! Where [`SessionModel`] folds one agent's
//! `AgentEvent` log, [`WorkspaceModel`] folds the multi-agent [`Inbound`]
//! stream: it keeps one `SessionModel` per agent (so per-agent purity is
//! untouched) and layers cross-agent read-models on top — the file ledger, the
//! route log, the prompt queue, and the unified trace.
//!
//! ## Purity boundary (L-T1)
//!
//! Everything here is a deterministic fold of the `Inbound` stream **except**
//! a small set of labeled out-of-band fields stamped from outside the event
//! log: [`AgentEntry::res`] and [`WorkspaceModel::global_cpu_pct`] (sampled
//! from the OS by the resource monitor), [`WorkspaceModel::now_ms`] (the
//! deck's clock, stamped by the shell tick), [`WorkspaceModel::queue`]
//! (mutated by the shell when the *user* submits and when the dispatcher
//! drains — a fold of outbound input, not of `Inbound`), and the code-graph
//! snapshot (queried from `stella-graph`, held by the graph view). Those are
//! the only exceptions; naming them is what keeps the boundary honest instead
//! of quietly eroded.

use std::collections::{BTreeMap, VecDeque};

use stella_protocol::{
    AgentEvent, CiStatus, FileChangeKind, PrStatus, ProofStep, ProofTree, StageKind, TaskStatus,
};

use crate::envelope::{AgentId, AgentMeta, AgentStatus, Inbound};
use crate::model::SessionModel;

/// The top-level tabs of the deck.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeckTab {
    Session,
    Agents,
    Traces,
    Graph,
    Files,
    Skills,
    Mcp,
    Issues,
    Settings,
}

impl DeckTab {
    pub const ALL: [DeckTab; 9] = [
        DeckTab::Session,
        DeckTab::Agents,
        DeckTab::Traces,
        DeckTab::Graph,
        DeckTab::Files,
        DeckTab::Skills,
        DeckTab::Mcp,
        DeckTab::Issues,
        DeckTab::Settings,
    ];

    /// The tab-bar label. Deck tab labels are UPPERCASE by convention —
    /// every tab added later must follow (e.g. `SKILLS`, `MCP`).
    /// `Agents` renders as AGENTS: the executions dashboard paired with the
    /// installed-agents view. `Settings` is the home of all config — it hosts
    /// the `agent_engine_config` editor.
    pub fn title(self) -> &'static str {
        match self {
            DeckTab::Session => "SESSION",
            DeckTab::Agents => "AGENTS",
            DeckTab::Traces => "TRACES",
            DeckTab::Graph => "GRAPH",
            DeckTab::Files => "FILES",
            DeckTab::Skills => "SKILLS",
            DeckTab::Mcp => "MCP",
            DeckTab::Issues => "ISSUES",
            DeckTab::Settings => "SETTINGS",
        }
    }

    pub fn index(self) -> usize {
        DeckTab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    pub fn from_index(i: usize) -> DeckTab {
        DeckTab::ALL[i % DeckTab::ALL.len()]
    }

    pub fn next(self) -> DeckTab {
        DeckTab::from_index(self.index() + 1)
    }

    pub fn prev(self) -> DeckTab {
        DeckTab::from_index(self.index() + DeckTab::ALL.len() - 1)
    }
}

/// A sampled resource reading for one agent — the one out-of-band field on
/// [`AgentEntry`]. Produced by [`crate::resource::ResourceMonitor`], never
/// folded from events.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ResourceSample {
    /// CPU utilization percent (can exceed 100 across cores).
    pub cpu_pct: f32,
    /// Resident memory in bytes.
    pub mem_bytes: u64,
}

/// One agent's slot in the workspace: its pure per-agent fold plus the derived
/// dashboard counters.
#[derive(Clone, Debug)]
pub struct AgentEntry {
    pub meta: AgentMeta,
    /// The existing pure event fold for this agent (Session tab renders it).
    pub model: SessionModel,
    pub status: AgentStatus,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// Cumulative prompt-cache *read* (hit) tokens — the `cached_input_tokens`
    /// sum from `StepUsage`. A subset of `tokens_in` by the `CompletionUsage`
    /// contract (`cached_input_tokens ⊆ input_tokens`), so the session
    /// cache-hit rate `cache_read_tokens / tokens_in` is always in `[0, 1]`.
    pub cache_read_tokens: u64,
    /// Cumulative prompt-cache *write* tokens — the `cache_write_tokens` sum
    /// from `StepUsage`. NOT a subset of `tokens_in` (writes bill separately),
    /// so this is the raw write volume the cache panel shows next to the reads.
    pub cache_write_tokens: u64,
    /// Cumulative estimated USD saved by prompt caching, summed from the signed
    /// per-call [`crate::envelope::Inbound::CacheInsight`] deltas the
    /// pricing-aware producer computes. Signed: negative when the write premium
    /// outran the reads (the low-hit incident worth surfacing).
    pub cache_savings_usd: f64,
    /// The agent provider's prompt-cache TTL in seconds, from the latest
    /// `CacheInsight` (`0` = no prompt cache / no TTL). Paired with
    /// [`Self::last_provider_call_ms`] for the deck's warmth countdown.
    pub cache_ttl_secs: u64,
    /// Whether the agent's current provider only caches behind an explicit
    /// opt-in marker, from the latest `CacheInsight` — see
    /// [`crate::envelope::Inbound::CacheInsight`]. Feeds
    /// [`Self::cache_diagnosis`]'s `OptInNeverEngaged` case.
    pub cache_is_opt_in_provider: bool,
    /// Metered model calls this agent has made (`StepUsage` count) — the
    /// `turns` a low-hit-rate diagnosis needs enough of before a 0% hit rate
    /// is meaningful (turn 1 always writes, never reads).
    pub cache_call_count: u64,
    /// Wall-clock ms of the agent's most recent metered model call (a
    /// `StepUsage`) — the anchor the cache-warmth countdown measures idle from.
    /// `None` before any call has landed.
    pub last_provider_call_ms: Option<u64>,
    /// Longest observed idle between two metered calls, in seconds — folded
    /// where `last_provider_call_ms` moves. Lets
    /// [`Self::cache_diagnosis`] tell a prefix that expired while the
    /// session sat idle (`CacheCause::IdleBeyondTtl`) from one that churns
    /// between back-to-back turns, mirroring
    /// `stella_model::cache_economics::diagnose_cache_with_idle` (#1525).
    pub max_idle_gap_secs: u64,
    /// The **current** context-window occupancy: the `input_tokens` of the most
    /// recent `StepUsage` (the prompt size the last call actually sent), NOT the
    /// running sum. This is what the Ctx% gauge divides by the window — using
    /// the cumulative `tokens_in` pinned the meter at 100% after a few turns,
    /// since the total input across a session dwarfs any single window.
    pub context_tokens: u64,
    /// Live spend. Authoritative once a `BudgetTick` has been seen (its
    /// `spent_usd` already covers step costs — mirrors the HUD accounting in
    /// `SessionModel`); until then, `StepUsage.cost_usd` accumulates here as
    /// a fallback so a stream without budget ticks still shows real spend.
    pub cost_usd: f64,
    /// True once a `BudgetTick` arrived — from then on the budget stream owns
    /// `cost_usd` and `StepUsage` no longer adds to it (that would
    /// double-count).
    pub budget_ticked: bool,
    pub last_activity_ms: u64,
    /// Sampled CPU/MEM — the out-of-band field.
    pub res: ResourceSample,
    /// Recent activity intensity, one sample per event, for the sparkline.
    pub activity: ActivitySpark,
    /// Wall-clock ms at which the in-flight chat turn started — set the instant
    /// the prompt is dispatched (`Inbound::PromptStarted`) and cleared when the
    /// turn ends. `Some` means a turn is live and the header clock counts up
    /// from here; `None` means it holds `last_turn_ms` (see [`Self::turn_clock_ms`]).
    pub turn_started_ms: Option<u64>,
    /// Duration in ms of the most recently completed turn, held until the next
    /// turn begins. `None` before any turn has finished, so the header clock
    /// reads zero at rest.
    pub last_turn_ms: Option<u64>,
    /// [`Self::tokens_out`] as it stood when the live turn began — snapshotted
    /// with `turn_started_ms` so the progress bar's tok/s divides only the
    /// turn's own output by the turn's own elapsed (cumulative session tokens
    /// over agent lifetime is an average, not a rate). The token twin of
    /// [`crate::model::Hud::turn_start_spent_usd`].
    pub turn_start_tokens_out: u64,
    /// The in-progress task the board most recently flipped active, stamped
    /// `(id, started_ms, cost_usd at that moment)` when a `TaskUpdate` fold
    /// moves the active task. The task card's `elapsed · $cost` row divides
    /// against these — per-task cost does not arrive on the wire, but the
    /// spend delta since the task went active is a fact this fold owns.
    /// Derived from `Inbound` + [`WorkspaceModel::now_ms`], so replay
    /// reconstructs it.
    pub active_task: Option<ActiveTaskStamp>,
    /// Wall-clock ms each witness phase was entered, stamped as the proof
    /// events fold through: author on `Stage::Witness` / `WitnessAuthored`,
    /// execute on the first candidate oracle run, result when the flip
    /// resolves. Folded from `Inbound` + `now_ms` like [`Self::active_task`];
    /// the witness panel derives per-phase elapsed from consecutive stamps.
    pub witness_phase_ms: WitnessPhaseStamps,
}

/// The stamp behind the task card's live `elapsed · $cost` readout — see
/// [`AgentEntry::active_task`].
#[derive(Clone, Debug, PartialEq)]
pub struct ActiveTaskStamp {
    /// The `TaskItem::id` currently in progress.
    pub id: String,
    /// When the board flipped it active (deck clock).
    pub started_ms: u64,
    /// The agent's spend at that moment — cost since is the difference.
    pub cost_at_start_usd: f64,
}

/// When each witness phase began — see [`AgentEntry::witness_phase_ms`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WitnessPhaseStamps {
    pub author_ms: Option<u64>,
    pub execute_ms: Option<u64>,
    pub result_ms: Option<u64>,
}

impl AgentEntry {
    fn new(meta: AgentMeta) -> Self {
        let started = meta.started_ms;
        Self {
            meta,
            model: SessionModel::new(),
            status: AgentStatus::Queued,
            tokens_in: 0,
            tokens_out: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cache_savings_usd: 0.0,
            cache_ttl_secs: 0,
            cache_is_opt_in_provider: false,
            cache_call_count: 0,
            last_provider_call_ms: None,
            max_idle_gap_secs: 0,
            context_tokens: 0,
            cost_usd: 0.0,
            budget_ticked: false,
            last_activity_ms: started,
            res: ResourceSample::default(),
            activity: ActivitySpark::new(ACTIVITY_WINDOW),
            turn_started_ms: None,
            last_turn_ms: None,
            turn_start_tokens_out: 0,
            active_task: None,
            witness_phase_ms: WitnessPhaseStamps::default(),
        }
    }

    /// Elapsed wall-clock ms given the deck's current clock.
    pub fn elapsed_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.meta.started_ms)
    }

    /// The value the header turn-clock displays, in ms: the live elapsed time
    /// while a turn is in flight, otherwise the last completed turn's duration
    /// (zero before any turn has run). Always defined — the clock is visible at
    /// all times, even at rest.
    pub fn turn_clock_ms(&self, now_ms: u64) -> u64 {
        match self.turn_started_ms {
            Some(start) => now_ms.saturating_sub(start),
            None => self.last_turn_ms.unwrap_or(0),
        }
    }

    /// Freeze the turn clock at `now_ms` if a turn is in flight — the turn's
    /// elapsed becomes the held `last_turn_ms` and the live clock stops. A
    /// no-op when no turn is running, so double-fires (e.g. a cancel that emits
    /// its own terminal event after the engine already completed) are harmless.
    fn end_turn(&mut self, now_ms: u64) {
        if let Some(start) = self.turn_started_ms.take() {
            self.last_turn_ms = Some(now_ms.saturating_sub(start));
        }
    }

    /// Spend per hour, or `0.0` before any wall-clock has elapsed.
    pub fn usd_per_hour(&self, now_ms: u64) -> f64 {
        let secs = self.elapsed_ms(now_ms) as f64 / 1000.0;
        if secs < 1.0 {
            0.0
        } else {
            self.cost_usd / secs * 3600.0
        }
    }

    /// The LIVE turn's token rate: output tokens since this turn's
    /// `PromptStarted` over the turn's own elapsed. `None` whenever there is
    /// nothing honest to divide — not running, no turn clock, or no tokens
    /// emitted this turn yet (a lifetime average dressed as a rate is exactly
    /// what this refuses to be). One implementation, shared by the progress
    /// row and the statline's collapsed forms.
    pub fn live_tok_per_s(&self, now_ms: u64) -> Option<u64> {
        if self.status != AgentStatus::Running {
            return None;
        }
        let start = self.turn_started_ms?;
        let elapsed_ms = now_ms.saturating_sub(start);
        let turn_tokens = self.tokens_out.saturating_sub(self.turn_start_tokens_out);
        (elapsed_ms > 0 && turn_tokens > 0).then(|| turn_tokens.saturating_mul(1000) / elapsed_ms)
    }

    /// Whether this lane is a subagent (registered with the `subagent` role) —
    /// the split the SESSION tab's nested rows and the statline's
    /// `✦ lead · ◆ sub` counts read.
    pub fn is_subagent(&self) -> bool {
        self.meta.role == "subagent"
    }
}

/// The deck's PR read-model: the latest `AgentEvent::Pr` observation, from
/// whichever agent emitted it. A session tells one PR story at a time — the
/// newest event wins outright, so a CI update on the same PR simply replaces
/// the snapshot in place. Drives the statline's PR cell.
#[derive(Clone, Debug, PartialEq)]
pub struct PrInfo {
    pub url: String,
    /// The PR number (`#183`), when the monitor parsed one from the URL.
    pub number: Option<u64>,
    pub status: PrStatus,
    /// The head commit's aggregate CI verdict — `None` means "not polled
    /// yet", never "passing".
    pub ci: Option<CiStatus>,
}

/// The whole derived deck state, folded from the [`Inbound`] stream.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceModel {
    /// Agents in first-registered order; look up by `meta.id`.
    pub agents: Vec<AgentEntry>,
    pub ledger: FileLedger,
    pub routes: RouteLog,
    pub queue: PromptQueue,
    pub trace: TraceLog,
    /// The deck's clock (ms since epoch), advanced by the shell tick. Kept in
    /// the model so elapsed/$-per-hour are computed from one source.
    pub now_ms: u64,
    /// Global system CPU utilization percent — the second labeled out-of-band
    /// field (sampled by [`crate::resource::ResourceMonitor`], not folded from
    /// events). Drives the status-bar gauge (and, later, dispatch backpressure).
    pub global_cpu_pct: f32,
    /// Whether the session drives turns through the staged pipeline (triage →
    /// plan → execute → witness → verify → verdict), not the raw engine loop.
    /// Surfaced as the `PIPELINE` stat box. Seeded from
    /// `DeckOptions::pipeline` and toggled live by [`Inbound::Pipeline`]
    /// (the driver's `/pipeline` command).
    pub pipeline: bool,
    /// The latest PR observation across all agents (`AgentEvent::Pr` from the
    /// fleet PR/CI monitor) — the statline's PR cell. Latest event wins;
    /// `None` until a PR has been seen this session.
    pub pr: Option<PrInfo>,
    /// Last pin observed serving each of the three pipeline roles — the
    /// statline's MODEL cell. Folded from `AgentEvent::StepUsage`, which
    /// carries the provider that actually served the call rather than the
    /// session's configured default, so this names what ran and not what was
    /// asked for.
    ///
    /// A role is absent until it has served once. That is honest rather than
    /// convenient: a verifier that never ran is not a verifier pinned to nothing,
    /// and showing a configured-but-unused pin as if it were live is how the
    /// triage/worker/verifier split gets misread in a head-to-head run.
    pub role_pins: BTreeMap<PipelineRole, RolePin>,
    /// The role that most recently served a call. Named by the MODEL cell and
    /// accented in the `/models` dialog while any agent is active, which
    /// includes a lead only monitoring subagents its own session spawned.
    pub active_role: Option<PipelineRole>,
    /// The session-level spend cap, folded from the newest
    /// `AgentEvent::BudgetTick` that carried a `session_limit_usd`. `None`
    /// until the driver meters one. Drives the statline's `run $X of $Y`
    /// form and the scope card's budget row; `/budget` edits it by sending
    /// [`crate::envelope::WorkspaceInput::SetBudget`] out — the new cap
    /// arrives back here as a fold, never as a local write.
    pub budget_cap_usd: Option<f64>,
}

/// The three roles the statline surfaces.
///
/// Deliberately not every [`stella_protocol::ModelCallRole`]: reflection,
/// summarization and the authoring roles are real calls with real cost, but
/// they are not the pipeline a head-to-head bench run compares, and a cell
/// that listed all fourteen would stop answering the question it exists for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PipelineRole {
    Triage,
    Worker,
    Verifier,
}

impl PipelineRole {
    /// Which slot a call role belongs to, or `None` for one that belongs to
    /// no slot.
    pub fn of(role: stella_protocol::ModelCallRole) -> Option<Self> {
        use stella_protocol::ModelCallRole as R;
        match role {
            R::Triage => Some(Self::Triage),
            // The whole main line of work reads as the worker. Planning,
            // witness authoring and the distress path all run on the worker
            // pin, so splitting them would print the same model three times
            // and imply three configured models where there is one.
            R::Plan
            | R::PlanRepair
            | R::Research
            | R::WitnessAuthor
            | R::WitnessRepair
            | R::Worker
            | R::DistressGuidance => Some(Self::Worker),
            R::Verdict => Some(Self::Verifier),
            R::Unknown
            | R::AgentAuthor
            | R::SkillAuthor
            | R::DomainInference
            | R::Reflection
            | R::Summarization => None,
        }
    }

    /// Single-character label for the statline cell, where horizontal space
    /// is the binding constraint.
    pub fn initial(self) -> char {
        match self {
            Self::Triage => 'T',
            Self::Worker => 'W',
            Self::Verifier => 'J',
        }
    }

    /// Display order: the order the pipeline actually runs them.
    pub const ORDER: [Self; 3] = [Self::Triage, Self::Worker, Self::Verifier];
}

/// One role's pin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RolePin {
    /// The provider that served the call, not the configured default — the
    /// same distinction `StepUsage::provider` draws, and the one that matters
    /// when the same model slug is reachable through more than one upstream.
    pub provider: String,
    pub model: String,
    /// `false` while this is only what the session was *configured* to use,
    /// `true` once a call has actually been served on it.
    ///
    /// The deck draws the two differently on purpose. A configured pin is a
    /// claim about intent and a served pin is evidence, and a scored run is
    /// read against the second. Collapsing them would let a verifier that never
    /// ran look identical to one that did — the same "unverified reads as
    /// verified" failure the ladder exists to prevent, moved into the UI.
    pub served: bool,
}

impl RolePin {
    /// `provider/model`, or just the model when the provider is unknown
    /// (legacy events carry an empty provider).
    pub fn slug(&self) -> String {
        if self.provider.is_empty() {
            self.model.clone()
        } else {
            format!("{}/{}", self.provider, self.model)
        }
    }
}

impl WorkspaceModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Index of an agent by id.
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.agents.iter().position(|a| a.meta.id == id)
    }

    /// Count of agents currently active (running / awaiting input).
    pub fn active_count(&self) -> usize {
        self.agents.iter().filter(|a| a.status.is_active()).count()
    }

    /// Total live spend across all agents.
    pub fn total_cost(&self) -> f64 {
        self.agents.iter().map(|a| a.cost_usd).sum()
    }

    /// The most-recently-routed model, if any route has been observed.
    pub fn latest_model(&self) -> Option<&str> {
        self.routes.entries.back().map(|e| e.model.as_str())
    }

    /// Cumulative prompt-cache hit tokens across all agents — the numerator of
    /// the session cache-hit rate (a subset of [`Self::total_input_tokens`]).
    pub fn cache_hit_tokens(&self) -> u64 {
        self.agents.iter().map(|a| a.cache_read_tokens).sum()
    }

    /// How many lanes are registered as subagents.
    pub fn subagent_count(&self) -> usize {
        self.agents.iter().filter(|a| a.is_subagent()).count()
    }

    /// How many lanes are NOT subagents (the lead count the statline shows
    /// beside the subagent count).
    pub fn lead_count(&self) -> usize {
        self.agents.len() - self.subagent_count()
    }

    /// The summed live-turn token rate across every running lane, plus how
    /// many lanes contributed. More than one contributor means the honest
    /// figure is the workspace's combined rate, labelled `combined` (D5) —
    /// a single lane keeps its own unlabelled rate.
    pub fn combined_tok_per_s(&self) -> (Option<u64>, usize) {
        let rates: Vec<u64> = self
            .agents
            .iter()
            .filter_map(|a| a.live_tok_per_s(self.now_ms))
            .collect();
        if rates.is_empty() {
            (None, 0)
        } else {
            (Some(rates.iter().sum()), rates.len())
        }
    }

    /// Cumulative input tokens across all agents — the denominator of the
    /// session cache-hit rate. `cache_hit_tokens ⊆ total_input_tokens` by the
    /// `CompletionUsage` contract, so the ratio never exceeds 1.
    pub fn total_input_tokens(&self) -> u64 {
        self.agents.iter().map(|a| a.tokens_in).sum()
    }

    /// The **sole** mutator: fold one inbound envelope into the deck.
    pub fn apply_inbound(&mut self, inbound: &Inbound) {
        match inbound {
            Inbound::Register(meta) => self.register(meta.clone()),
            // The visual-lifecycle inverse of `Register`: the row disappears.
            // An unknown id is a no-op — a stale deregister (row already
            // gone, or never seen) must never disturb the fold.
            Inbound::Deregister { agent } => {
                if let Some(idx) = self.index_of(agent) {
                    self.agents.remove(idx);
                }
            }
            Inbound::Event { agent, event } => self.apply_event(agent, event),
            // Local `!` output: transcript only. See `Inbound::ShellEvent` for
            // why this must not go through `apply_event` — the status it would
            // derive (`Running`) has nothing to park it. An unknown id is a
            // no-op rather than an auto-register: the caller only ever names a
            // lane it already resolved, and the no-lane case takes the
            // synthetic fallback instead.
            Inbound::ShellEvent { agent, event } => {
                if let Some(idx) = self.index_of(agent) {
                    self.agents[idx].model.apply(event);
                    self.agents[idx].last_activity_ms = self.now_ms;
                }
            }
            Inbound::Status { agent, status } => {
                // Auto-register an unknown id, exactly like `Event`:
                // supervisor states (`Paused`, `Killed`, …) are not
                // recoverable from the event stream, so a status arriving
                // before registration must never be dropped.
                let i = match self.index_of(agent) {
                    Some(i) => i,
                    None => {
                        self.agents.push(AgentEntry::new(AgentMeta::new(
                            agent.clone(),
                            agent.clone(),
                            self.now_ms,
                        )));
                        self.agents.len() - 1
                    }
                };
                // Killed is terminal-terminal; the supervisor owns it and
                // nothing walks it back.
                if self.agents[i].status != AgentStatus::Killed {
                    self.agents[i].status = *status;
                }
                // `WaitingInput` via `Status` is the host's "back to idle"
                // signal — it arrives after handled commands (`/init`, MCP
                // connect, startup) that skip the model turn, so no turn is in
                // flight. Freeze the header clock; unlike `AskUser` (which
                // reaches `WaitingInput` through the event stream mid-turn),
                // this path means the turn is genuinely over.
                if *status == AgentStatus::WaitingInput {
                    self.agents[i].end_turn(self.now_ms);
                }
            }
            Inbound::PromptStarted { agent, text } => {
                // The dispatcher drained the oldest queued prompt. Both sides
                // are FIFO over one ordered channel, so the front entry is the
                // one that started; `text` is carried for the trace row (and
                // as a guard against a front entry the shell never saw).
                if self
                    .queue
                    .items
                    .front()
                    .is_none_or(|queued| queued.text == *text)
                {
                    let _ = self.queue.take_next();
                }
                let ts = self.now_ms;
                self.trace.push(TraceRow {
                    ts,
                    agent: agent.clone(),
                    kind: TraceKind::Stage,
                    summary: format!("▶ {}", snip(text)),
                });
                // Show the user's prompt inline in the agent's transcript so
                // the conversational scrollback is self-contained, matching
                // the Crush-style layout where user messages are visible.
                if let Some(idx) = self.index_of(agent) {
                    self.agents[idx].model.push_user_prompt(text);
                    // A new chat turn begins the instant the prompt is
                    // dispatched: start the header clock here and drop the
                    // prior turn's held time so the readout switches straight
                    // to the live count.
                    self.agents[idx].turn_started_ms = Some(ts);
                    self.agents[idx].last_turn_ms = None;
                    self.agents[idx].turn_start_tokens_out = self.agents[idx].tokens_out;
                    // The witness clocks belong to one turn, like the proof
                    // rail they annotate (`push_user_prompt` just reset it).
                    self.agents[idx].witness_phase_ms = WitnessPhaseStamps::default();
                    // Flip to Running now so the progress bar reads in-progress
                    // from the instant of submission — a driver command (e.g.
                    // `/init`) emits no stage events, and the prior turn may have
                    // left the status at `Done`, which would otherwise keep the
                    // bar frozen at full-green until the engine spoke.
                    self.agents[idx].status = AgentStatus::Running;
                }
            }
            Inbound::PromptRequeued { agent, text } => {
                // The driver cancelled a turn (double-Esc) and returned its
                // prompt to the front of its backlog. Front-insert the mirror
                // — the exact inverse of `PromptStarted`'s front-pop — so the
                // queue view keeps matching what will actually run next.
                let ts = self.now_ms;
                self.queue.enqueue_front(text.clone(), ts);
                self.trace.push(TraceRow {
                    ts,
                    agent: agent.clone(),
                    kind: TraceKind::Stage,
                    summary: format!("↩ {}", snip(text)),
                });
            }
            // `/clear`: reset the agent's session to seq-0 — blank the
            // transcript, zero the counters and header clock, return the HUD to
            // idle, wipe the prompt echo. The file-touch half and
            // [`Self::ledger`] survive; why is on `SessionModel::reset_conversation`.
            Inbound::SessionReset { agent } => {
                if let Some(idx) = self.index_of(agent) {
                    let entry = &mut self.agents[idx];
                    entry.model.reset_conversation();
                    entry.status = AgentStatus::WaitingInput;
                    entry.tokens_in = 0;
                    entry.tokens_out = 0;
                    entry.cache_read_tokens = 0;
                    entry.cache_write_tokens = 0;
                    entry.cache_savings_usd = 0.0;
                    entry.cache_ttl_secs = 0;
                    entry.cache_is_opt_in_provider = false;
                    entry.cache_call_count = 0;
                    entry.last_provider_call_ms = None;
                    entry.max_idle_gap_secs = 0;
                    entry.context_tokens = 0;
                    entry.cost_usd = 0.0;
                    entry.budget_ticked = false;
                    entry.turn_started_ms = None;
                    entry.last_turn_ms = None;
                    entry.turn_start_tokens_out = 0;
                    entry.active_task = None;
                    entry.witness_phase_ms = WitnessPhaseStamps::default();
                }
            }
            // The driver flipped staged-pipeline routing (`/pipeline`) — the
            // PIPELINE stat box tracks it live.
            Inbound::Pipeline(on) => self.pipeline = *on,
            // The driver's resolved role pins, sent once at startup. Never
            // overwrites a pin that has already served: this says what the
            // session intends to use, and a role that has run has already
            // answered that question with evidence.
            Inbound::ConfiguredRoles(pins) => {
                for (role, pin) in pins {
                    self.role_pins
                        .entry(*role)
                        .or_insert_with(|| pin.clone());
                }
            }
            // Derived cache economics for the agent's latest call: accumulate
            // the signed savings and adopt the provider's TTL. Follows the
            // paired `StepUsage` (which auto-registers the lane), so an unknown
            // id here is a stale/out-of-order envelope — a safe no-op.
            Inbound::CacheInsight {
                agent,
                savings_usd_delta,
                ttl_secs,
                is_opt_in_provider,
            } => {
                if let Some(idx) = self.index_of(agent) {
                    let entry = &mut self.agents[idx];
                    entry.cache_savings_usd += *savings_usd_delta;
                    entry.cache_ttl_secs = *ttl_secs;
                    entry.cache_is_opt_in_provider = *is_opt_in_provider;
                }
            }
            // The graph snapshot, the slash vocabulary, the installed-agents
            // list, and the MCP snapshots are out-of-band read-models, not part
            // of the event-log fold — the view state owns them, applied in
            // `ingest_inbound`, so the model deliberately ignores them here.
            Inbound::GraphSnapshot(_)
            | Inbound::SlashCommands(_)
            | Inbound::AgentsList { .. }
            | Inbound::Skills(_)
            | Inbound::SkillSearch { .. }
            | Inbound::SkillPreview { .. }
            | Inbound::McpServers(_)
            | Inbound::McpSearchResults(_)
            | Inbound::McpDetail(_)
            | Inbound::Sessions(_)
            | Inbound::Notifications(_)
            | Inbound::McpOauthStatus { .. }
            | Inbound::EngineConfig { .. }
            | Inbound::ToolPolicy { .. }
            | Inbound::IssuesList { .. }
            | Inbound::IssueActDone { .. }
            | Inbound::EntityHits { .. }
            | Inbound::RecordedCalls(_)
            | Inbound::InspectedCall(_)
            | Inbound::ShowHelp
            | Inbound::Splash(_)
            // The whole point of `Notice`: a system notification is not agent
            // or user speech, so the fold must NOT give it a transcript row.
            // It is view state only (`DeckUi::notice`).
            | Inbound::Notice(_) => {}
        }
    }

    fn register(&mut self, meta: AgentMeta) {
        match self.index_of(&meta.id) {
            Some(i) => self.agents[i].meta = meta, // re-register updates meta
            None => self.agents.push(AgentEntry::new(meta)),
        }
    }

    fn apply_event(&mut self, agent: &AgentId, event: &AgentEvent) {
        // Auto-register an agent we've never seen so a stray event is never
        // dropped — the dashboard row appears with what we know.
        let idx = match self.index_of(agent) {
            Some(i) => i,
            None => {
                self.agents.push(AgentEntry::new(AgentMeta::new(
                    agent.clone(),
                    agent.clone(),
                    self.now_ms,
                )));
                self.agents.len() - 1
            }
        };
        let now = self.now_ms;

        // Per-agent pure fold — untouched.
        self.agents[idx].model.apply(event);

        // Derived counters.
        {
            let entry = &mut self.agents[idx];
            entry.last_activity_ms = now;
            entry.activity.push(event_intensity(event));
            if let Some(status) = status_from_event(event)
                && entry.status != AgentStatus::Killed
                && entry.status != AgentStatus::Paused
            {
                entry.status = status;
            }
            match event {
                AgentEvent::StepUsage {
                    input_tokens,
                    output_tokens,
                    cached_input_tokens,
                    cache_write_tokens,
                    model,
                    cost_usd,
                    ..
                } => {
                    entry.tokens_in += input_tokens;
                    entry.tokens_out += output_tokens;
                    entry.cache_read_tokens += cached_input_tokens;
                    entry.cache_write_tokens += cache_write_tokens;
                    entry.cache_call_count += 1;
                    // A metered call just landed — fold the idle it closes
                    // (the gap the diagnosis reads), then anchor the
                    // cache-warmth countdown here (the prefix is warmest
                    // right now).
                    if let Some(last) = entry.last_provider_call_ms {
                        entry.max_idle_gap_secs =
                            entry.max_idle_gap_secs.max(now.saturating_sub(last) / 1000);
                    }
                    entry.last_provider_call_ms = Some(now);
                    // Occupancy is the LATEST call's prompt size, not the sum.
                    entry.context_tokens = *input_tokens;
                    entry.meta.model = Some(model.clone());
                    // Fallback accounting: a stream that never emits
                    // `BudgetTick` (scenario feeds, minimal drivers) still
                    // shows real spend. Once a tick has been seen it owns
                    // `cost_usd` outright — adding steps on top of it would
                    // double-count.
                    if !entry.budget_ticked {
                        entry.cost_usd += cost_usd;
                    }
                }
                AgentEvent::BudgetTick { spent_usd, .. } => {
                    entry.budget_ticked = true;
                    entry.cost_usd = *spent_usd;
                }
                AgentEvent::Complete { model, cost_usd } => {
                    entry.meta.model = Some(model.clone());
                    entry.cost_usd = entry.cost_usd.max(*cost_usd);
                    // The turn-completion event: freeze the header clock at its
                    // final elapsed so it holds the last turn's duration.
                    entry.end_turn(now);
                }
                // A non-retryable error also ends the turn — an aborted turn,
                // a user Stop, or a double-Esc hold all fold to one of these
                // (see `command_deck`). Retryable errors mean the turn
                // continues (they fold to `Running`), so the clock keeps
                // ticking; only the terminal kind stops it.
                AgentEvent::Error {
                    retryable: false, ..
                } => entry.end_turn(now),
                // The board's active task moved: restamp the elapsed/cost
                // anchors the task card divides against. Same-id snapshots
                // (a re-emitted board) keep the original stamp.
                AgentEvent::TaskUpdate { tasks } => {
                    let active = tasks.iter().find(|t| t.status == TaskStatus::InProgress);
                    match (active, entry.active_task.as_ref()) {
                        (Some(t), Some(stamp)) if stamp.id == t.id => {}
                        (Some(t), _) => {
                            entry.active_task = Some(ActiveTaskStamp {
                                id: t.id.clone(),
                                started_ms: now,
                                cost_at_start_usd: entry.cost_usd,
                            });
                        }
                        (None, _) => entry.active_task = None,
                    }
                }
                // Witness phase entry stamps (the witness panel's per-phase
                // clocks). `get_or_insert` keeps the FIRST observation — a
                // re-emitted step must not restart a phase clock.
                AgentEvent::Stage {
                    name: StageKind::Witness,
                } => {
                    entry.witness_phase_ms.author_ms.get_or_insert(now);
                }
                AgentEvent::Proof { step } => match step {
                    ProofStep::WitnessAuthored { .. } => {
                        entry.witness_phase_ms.author_ms.get_or_insert(now);
                    }
                    ProofStep::Oracle {
                        tree: ProofTree::Candidate,
                        passed,
                        ..
                    } => {
                        entry.witness_phase_ms.execute_ms.get_or_insert(now);
                        if *passed {
                            entry.witness_phase_ms.result_ms.get_or_insert(now);
                        }
                    }
                    _ => {}
                },
                // A verdict closes the witness run whichever way it went.
                AgentEvent::Verdict { .. } if entry.witness_phase_ms.execute_ms.is_some() => {
                    entry.witness_phase_ms.result_ms.get_or_insert(now);
                }
                _ => {}
            }
        }
        // The session-level spend cap rides the budget stream; the newest
        // tick that names one wins (a tick without one leaves the cap alone —
        // most ticks only meter the turn).
        if let AgentEvent::BudgetTick {
            session_limit_usd: Some(cap),
            ..
        } = event
        {
            self.budget_cap_usd = Some(*cap);
        }

        // Cross-agent read-models.
        if let AgentEvent::FileChange {
            path,
            kind,
            added,
            removed,
            ..
        } = event
        {
            self.ledger.record(agent, path, *kind, *added, *removed);
        }
        if let AgentEvent::Pr {
            url,
            status,
            number,
            ci,
        } = event
        {
            // Latest wins, any agent — the statline tells one PR story, and a
            // CI re-poll on the same PR replaces the snapshot in place.
            self.pr = Some(PrInfo {
                url: url.clone(),
                number: *number,
                status: *status,
                ci: *ci,
            });
        }
        if let AgentEvent::StepUsage {
            model,
            provider,
            role,
            ..
        } = event
        {
            self.routes.record(now, agent.clone(), model.clone());
            // `StepUsage` already carries the provider that actually served
            // the call and the role it served — the route log kept only the
            // model, which is why the statline could name one model and never
            // say which of the three pipeline roles it belonged to.
            if let Some(slot) = PipelineRole::of(*role) {
                // Overwrites a configured pin rather than merging with it: the
                // provider that served may differ from the one configured
                // (a gateway falling through to a different upstream), and the
                // one that ran is the true answer.
                self.role_pins.insert(
                    slot,
                    RolePin {
                        provider: provider.clone(),
                        model: model.clone(),
                        served: true,
                    },
                );
                self.active_role = Some(slot);
            }
        }
        // Streaming previews never reach the trace: one row per token would
        // churn the whole capped ring during a single answer, and the
        // authoritative `Text` event lands the same content as one row. Context
        // receipts (spec §4/§5) are excluded for the same reason — one
        // BlockRegistered per block per step would swamp the ring — and are
        // consumed by the store/inspector, not the live deck.
        if !matches!(
            event,
            AgentEvent::TextDelta { .. }
                | AgentEvent::BlockRegistered { .. }
                | AgentEvent::StepManifest { .. }
        ) {
            let (kind, summary) = trace_of(event);
            self.trace.push(TraceRow {
                ts: now,
                agent: agent.clone(),
                kind,
                summary,
            });
        }
    }
}

// ── File ledger: CRUD + line +/- per (agent, path) ──────────────────────────

/// One file's cumulative change record within the session.
#[derive(Clone, Debug, PartialEq)]
pub struct FileRecord {
    pub agent: AgentId,
    pub path: String,
    /// The latest *mutation* kind — a read only sets this on a file that has
    /// never been mutated, so an edited file's badge never regresses to `R`
    /// when the agent re-reads it.
    pub kind: FileChangeKind,
    pub added: u32,
    pub removed: u32,
    /// How many *mutating* `FileChange` events have touched this
    /// (agent, path).
    pub changes: u32,
    /// How many times this (agent, path) has been read.
    pub reads: u32,
}

/// Every file touched this session, with CRUD op and the line +/- **carried
/// on** each `FileChange` event.
///
/// These counts are no longer re-derived here. They used to be parsed back out
/// of the event's diff text, which made the panel's numbers a count of whatever
/// the emitter had synthesized — for a bulk edit or a worker lane, nothing at
/// all, rendering as `+0 -0` over real work. The emitter
/// (`ToolRegistry::record_touch`) computes the delta from the actual pre- and
/// post-images and sends it; this fold just accumulates.
#[derive(Clone, Debug, Default)]
pub struct FileLedger {
    pub records: Vec<FileRecord>,
}

/// Cap on distinct (agent, path) records the ledger keeps. Unlike
/// [`RouteLog`]/[`TraceLog`], which are always capped, this one had no bound
/// at all: a long multi-agent session that touches (or repeatedly re-reads)
/// many thousands of files grew this vector — and the O(n) `find` scan
/// [`FileLedger::record`] runs on every single `FileChange` event — without
/// limit for the life of the deck. Oldest-first eviction on overflow, mirroring
/// the other capped logs; the type stays `Vec` (not `VecDeque`) because
/// `views::files` range-slices `ledger.records` directly.
const MAX_LEDGER_RECORDS: usize = 4096;

impl FileLedger {
    fn record(&mut self, agent: &str, path: &str, kind: FileChangeKind, added: u32, removed: u32) {
        if let Some(rec) = self
            .records
            .iter_mut()
            .find(|r| r.agent == agent && r.path == path)
        {
            if kind.is_mutation() {
                rec.kind = kind;
                rec.added += added;
                rec.removed += removed;
                rec.changes += 1;
            } else {
                rec.reads += 1;
            }
        } else {
            if self.records.len() >= MAX_LEDGER_RECORDS {
                self.records.remove(0);
            }
            let mutation = kind.is_mutation();
            self.records.push(FileRecord {
                agent: agent.to_string(),
                path: path.to_string(),
                kind,
                added,
                removed,
                changes: mutation as u32,
                reads: !mutation as u32,
            });
        }
    }

    pub fn total_added(&self) -> u32 {
        self.records.iter().map(|r| r.added).sum()
    }
    pub fn total_removed(&self) -> u32 {
        self.records.iter().map(|r| r.removed).sum()
    }
    pub fn file_count(&self) -> usize {
        self.records.len()
    }
    pub fn total_reads(&self) -> u32 {
        self.records.iter().map(|r| r.reads).sum()
    }
}

// The diff-counting fold moved to `crate::diff` (one module owns the whole
// "how a diff reads" story); re-exported here so existing call sites hold.
pub use crate::diff::count_diff_lines;

// ── Route log: which model handled what ─────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct RouteEntry {
    pub ts: u64,
    pub agent: AgentId,
    pub model: String,
}

/// A capped log of model-routing observations (one per committed step).
#[derive(Clone, Debug, Default)]
pub struct RouteLog {
    pub entries: VecDeque<RouteEntry>,
}

impl RouteLog {
    const CAP: usize = 256;
    fn record(&mut self, ts: u64, agent: AgentId, model: String) {
        self.entries.push_back(RouteEntry { ts, agent, model });
        while self.entries.len() > Self::CAP {
            self.entries.pop_front();
        }
    }
}

// The prompt queue moved to its own module when #1742's cross-store comment
// pushed this file past its size ceiling; re-exported so call sites hold.
mod prompt_queue;
pub use prompt_queue::{PromptQueue, QueuedPrompt};

// ── Trace log: unified cross-agent timeline ─────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceKind {
    Stage,
    Text,
    Reasoning,
    Tool,
    File,
    Budget,
    Context,
    Verdict,
    Media,
    Vcs,
    Error,
    Complete,
    Other,
}

impl TraceKind {
    pub fn label(self) -> &'static str {
        match self {
            TraceKind::Stage => "stage",
            TraceKind::Text => "text",
            TraceKind::Reasoning => "think",
            TraceKind::Tool => "tool",
            TraceKind::File => "file",
            TraceKind::Budget => "spend",
            TraceKind::Context => "ctx",
            TraceKind::Verdict => "verdict",
            TraceKind::Media => "media",
            TraceKind::Vcs => "vcs",
            TraceKind::Error => "error",
            TraceKind::Complete => "done",
            TraceKind::Other => "·",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TraceRow {
    pub ts: u64,
    pub agent: AgentId,
    pub kind: TraceKind,
    pub summary: String,
}

/// A capped ring buffer of trace rows across all agents.
#[derive(Clone, Debug)]
pub struct TraceLog {
    pub rows: VecDeque<TraceRow>,
    cap: usize,
}

impl Default for TraceLog {
    fn default() -> Self {
        Self {
            rows: VecDeque::new(),
            cap: 2000,
        }
    }
}

impl TraceLog {
    pub fn push(&mut self, row: TraceRow) {
        self.rows.push_back(row);
        while self.rows.len() > self.cap {
            self.rows.pop_front();
        }
    }
    /// Rows for one agent (filtered view).
    pub fn for_agent<'a>(&'a self, agent: &'a str) -> impl Iterator<Item = &'a TraceRow> + 'a {
        self.rows.iter().filter(move |r| r.agent == agent)
    }
}

// ── Activity sparkline ring ─────────────────────────────────────────────────

/// How many recent activity samples the dashboard sparkline keeps per agent.
pub const ACTIVITY_WINDOW: usize = 24;

/// A fixed-width ring of activity intensities (one per event), rendered as a
/// sparkline in the dashboard row.
#[derive(Clone, Debug)]
pub struct ActivitySpark {
    samples: VecDeque<u8>,
    cap: usize,
}

impl ActivitySpark {
    pub fn new(cap: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(cap),
            cap,
        }
    }
    pub fn push(&mut self, intensity: u8) {
        self.samples.push_back(intensity);
        while self.samples.len() > self.cap {
            self.samples.pop_front();
        }
    }
    /// Intensities oldest→newest, left-padded to the full width with zeros.
    pub fn padded(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.cap.saturating_sub(self.samples.len())];
        out.extend(self.samples.iter().copied());
        out
    }
}

// ── Event → derived attributes ──────────────────────────────────────────────

/// Activity intensity for the sparkline, by event kind. Edits and tool calls
/// read as "hot"; streaming text as "warm"; metering ticks as "cool".
fn event_intensity(ev: &AgentEvent) -> u8 {
    match ev {
        AgentEvent::FileChange { .. } => 255,
        AgentEvent::ToolStart { .. } | AgentEvent::ToolResult { .. } => 210,
        AgentEvent::Stage { .. } => 170,
        AgentEvent::Text { .. } | AgentEvent::TextDelta { .. } => 130,
        AgentEvent::Reasoning { .. } => 90,
        AgentEvent::Commit { .. } | AgentEvent::Pr { .. } => 230,
        AgentEvent::BudgetTick { .. } | AgentEvent::StepUsage { .. } => 60,
        AgentEvent::Error { .. } => 255,
        // A proof step is a decision the run reached, not work it did. It is
        // real activity on the rail and none on the sparkline — pitched with
        // the stage boundaries it interleaves with, so a well-proven turn does
        // not read as busier than an unproven one doing the same edits.
        AgentEvent::Proof { .. } => 170,
        // A sub-agent bracket is a boundary, pitched with `Stage` for the
        // same reason: the child's real work already registers through its
        // forwarded tool calls and metering, so pricing the bracket as work
        // too would double-count one child as a burst of activity.
        AgentEvent::SubAgent { .. } => 170,
        // Explicit rather than falling through the wildcard: an undecodable
        // event is real activity, so it should register on the sparkline, but
        // this build cannot know whether it was hot (an edit) or cool (a
        // metering tick). Cool-but-present is the honest reading, and it keeps
        // a burst of future events from impersonating heavy edit activity.
        AgentEvent::Unknown { .. } => 60,
        // A park is the turn deliberately idling — the coolest honest signal,
        // pitched with the metering ticks so a long wait never reads as work.
        AgentEvent::TurnParked { .. } | AgentEvent::TurnWoken { .. } => 60,
        _ => 110,
    }
}

/// Lifecycle status implied by an event, or `None` if it doesn't move the
/// agent's lifecycle.
fn status_from_event(ev: &AgentEvent) -> Option<AgentStatus> {
    match ev {
        AgentEvent::Complete { .. } => Some(AgentStatus::Done),
        AgentEvent::Error { retryable, .. } => Some(if *retryable {
            AgentStatus::Running
        } else {
            AgentStatus::Failed
        }),
        // Both user-response gates block the agent until answered — a scope
        // review is just as much "needs input" as an ask-user question.
        AgentEvent::AskUser { .. }
        | AgentEvent::ScopeReview { .. }
        | AgentEvent::HunkReview { .. } => Some(AgentStatus::WaitingInput),
        AgentEvent::Stage { .. }
        | AgentEvent::Text { .. }
        | AgentEvent::TextDelta { .. }
        | AgentEvent::Reasoning { .. }
        | AgentEvent::ToolStart { .. }
        | AgentEvent::ToolResult { .. }
        // A child turn is the parent working, so the lane stays Running
        // rather than falling through to "no lifecycle change". Explicit
        // because the wildcard below would otherwise let a long child run —
        // whose own narration is filtered out — read as an idle agent.
        | AgentEvent::SubAgent { .. }
        // A parked turn is the engine actively probing on its own clock —
        // alive, not waiting on the user — and the wake precedes the next
        // model call. Explicit for the same reason as `SubAgent`: a long
        // park emits nothing else, and the lane must not read as dead.
        | AgentEvent::TurnParked { .. }
        | AgentEvent::TurnWoken { .. } => Some(AgentStatus::Running),
        _ => None,
    }
}

/// A trace kind + short human summary for one event.
/// One trace line for a proof step — the same facts the rail folds, in the
/// order they were observed, for the reader who wants the history the rail
/// deliberately discards.
fn trace_of(ev: &AgentEvent) -> (TraceKind, String) {
    use stella_protocol::ToolOutput;
    match ev {
        // An event from a newer stella: name it, claim nothing about it.
        AgentEvent::Unknown { event_type, .. } => {
            (TraceKind::Other, format!("unrecognized `{event_type}`"))
        }
        AgentEvent::Stage { name } => (TraceKind::Stage, format!("{name:?}").to_lowercase()),
        AgentEvent::Text { text } => (TraceKind::Text, snip(text)),
        // Mapped for completeness; `apply_event` never traces deltas (one
        // row per token would churn the capped ring — see the guard there).
        AgentEvent::TextDelta { delta } => (TraceKind::Text, snip(delta)),
        AgentEvent::Reasoning { delta } => (TraceKind::Reasoning, snip(delta)),
        AgentEvent::ToolStart { call } => (TraceKind::Tool, format!("{}()", call.name)),
        AgentEvent::SpeculationDiscarded { name, reason, .. } => {
            (TraceKind::Tool, format!("discarded {name} ({reason})"))
        }
        AgentEvent::LoopDetected {
            kind,
            repeats,
            aborted,
            ..
        } => (
            TraceKind::Other,
            format!(
                "loop {kind} ×{repeats}{}",
                if *aborted {
                    " — aborted"
                } else {
                    " — steered"
                }
            ),
        ),
        AgentEvent::BudgetDenied {
            spent_usd,
            limit_usd,
            ..
        } => (
            TraceKind::Other,
            format!("budget denied ${spent_usd:.4}/${limit_usd:.2}"),
        ),
        AgentEvent::RetriesExhausted {
            attempts,
            retryable,
            ..
        } => (
            TraceKind::Other,
            if *retryable {
                format!("retries exhausted ({attempts})")
            } else {
                format!(
                    "terminal failure, not retryable ({attempts} attempt{})",
                    if *attempts == 1 { "" } else { "s" }
                )
            },
        ),
        AgentEvent::PolicyDecision { kind, subject, .. } => {
            (TraceKind::Other, format!("policy {kind:?}: {subject}"))
        }
        AgentEvent::ToolResult {
            output,
            duration_ms,
            ..
        } => {
            let ok = matches!(output, ToolOutput::Ok { .. });
            (
                TraceKind::Tool,
                format!("{} in {duration_ms}ms", if ok { "ok" } else { "err" }),
            )
        }
        AgentEvent::FileChange {
            path,
            kind,
            added,
            removed,
            ..
        } => (
            TraceKind::File,
            format!("{kind:?} {path} +{added}/-{removed}").to_lowercase(),
        ),
        AgentEvent::BudgetTick { spent_usd, .. } => (TraceKind::Budget, format!("${spent_usd:.4}")),
        AgentEvent::StepUsage {
            model, cost_usd, ..
        } => (TraceKind::Budget, format!("{model} ${cost_usd:.4}")),
        AgentEvent::ContextRecall { frames, tokens, .. } => (
            TraceKind::Context,
            format!("{} frames, {tokens} tok", frames.len()),
        ),
        AgentEvent::ContextWrite {
            upserts,
            superseded,
            ..
        } => (TraceKind::Context, format!("+{upserts} ~{superseded}")),
        // Receipts are filtered out of the trace ring above (apply_event's
        // guard); these arms exist only to keep this mapping total.
        AgentEvent::BlockRegistered { kind, .. } => {
            (TraceKind::Context, format!("block {kind:?}").to_lowercase())
        }
        AgentEvent::StepManifest { step, blocks, .. } => (
            TraceKind::Context,
            format!("manifest step {step}: {} blocks", blocks.len()),
        ),
        // Traced under Verdict, the kind that already means "what this run
        // established": the steps and the verdict are one story, and the trace
        // log is where a reader reconstructs how the rail got where it is.
        AgentEvent::Proof { step } => (TraceKind::Verdict, crate::proof::proof_trace(step)),
        AgentEvent::Verdict { passed, .. } => (
            TraceKind::Verdict,
            if *passed {
                "passed".into()
            } else {
                "failed".into()
            },
        ),
        AgentEvent::GoalVerdict { met, round, .. } => (
            TraceKind::Verdict,
            format!("round {round} {}", if *met { "met" } else { "unmet" }),
        ),
        AgentEvent::MediaProgress { kind, .. } => {
            (TraceKind::Media, format!("{kind:?}").to_lowercase())
        }
        AgentEvent::MediaComplete { artifact } => (TraceKind::Media, artifact.label.clone()),
        AgentEvent::Commit { message, .. } => (TraceKind::Vcs, snip(message)),
        AgentEvent::Pr { status, .. } => (TraceKind::Vcs, format!("pr {status:?}").to_lowercase()),
        AgentEvent::TaskUpdate { tasks } => {
            let done = tasks.iter().filter(|t| !t.status.is_open()).count();
            (TraceKind::Other, format!("tasks {done}/{}", tasks.len()))
        }
        // A sub-agent bracket is the only trace of a child turn — its own
        // events are filtered at the parent boundary — so it names the child
        // and, on the way out, what the parent saved by not carrying its work.
        AgentEvent::SubAgent { phase } => {
            use stella_protocol::SubAgentPhase;
            (
                TraceKind::Other,
                match phase {
                    SubAgentPhase::Started { agent_id, .. } => format!("sub-agent {agent_id} ↴"),
                    SubAgentPhase::Finished {
                        agent_id,
                        status,
                        absorbed_messages,
                        ..
                    } => format!(
                        "sub-agent {agent_id} {} ({absorbed_messages} msgs absorbed)",
                        format!("{status:?}").to_lowercase()
                    ),
                },
            )
        }
        AgentEvent::ProviderFallback { from, to, .. } => {
            (TraceKind::Other, format!("fallback {from}→{to}"))
        }
        AgentEvent::Retry { attempt, .. } => (TraceKind::Other, format!("retry #{attempt}")),
        AgentEvent::Steered { text } => (
            TraceKind::Other,
            format!("steer: {}", text.chars().take(40).collect::<String>()),
        ),
        AgentEvent::TurnParked {
            description,
            poll_interval_secs,
            deadline_secs,
        } => (
            TraceKind::Other,
            format!(
                "parked: {} (every {poll_interval_secs}s, up to {deadline_secs}s)",
                description.chars().take(40).collect::<String>()
            ),
        ),
        AgentEvent::TurnWoken { reason, polls_used } => (
            TraceKind::Other,
            format!("woke: {reason} after {polls_used} probes"),
        ),
        AgentEvent::Compaction {
            before_tokens,
            after_tokens,
            ..
        } => (
            TraceKind::Other,
            format!("compact {before_tokens}→{after_tokens}"),
        ),
        AgentEvent::UsageIncomplete { reason, .. } => {
            (TraceKind::Other, format!("usage incomplete: {reason:?}"))
        }
        AgentEvent::ScopeReview { proposal } => (TraceKind::Stage, snip(&proposal.summary)),
        AgentEvent::HunkReview { proposal } => (
            TraceKind::Stage,
            format!(
                "review {} hunk{} from {}",
                proposal.hunks.len(),
                if proposal.hunks.len() == 1 { "" } else { "s" },
                proposal.tool
            ),
        ),
        AgentEvent::AskUser { question, .. } => (TraceKind::Other, snip(question)),
        AgentEvent::Error { message, .. } => (TraceKind::Error, snip(message)),
        AgentEvent::Complete { model, cost_usd } => {
            (TraceKind::Complete, format!("{model} ${cost_usd:.4}"))
        }
    }
}

/// A one-line, length-capped snip of free text for a trace row.
fn snip(text: &str) -> String {
    const MAX: usize = 80;
    let flat = text.replace(['\n', '\r'], " ");
    let flat = flat.trim();
    if flat.chars().count() <= MAX {
        flat.to_string()
    } else {
        let head: String = flat.chars().take(MAX - 1).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests;
