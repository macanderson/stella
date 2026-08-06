//! Tool trait + registry. The agent loop drives every tool through
//! `ToolRegistry::execute` — no tool-specific code lives outside this crate.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use stella_core::bus::{self, HookBus, HookDecision, HookEventDraft, names as hook_names};
use stella_core::ports::ToolExecutor;
use stella_protocol::tool::{ToolOutput, ToolSchema};

pub use crate::file_touch::FileOp;
use crate::file_touch::{
    FileTouchEvent, FileTouchLedger, FileTouchTelemetry, count_lines, line_diff,
    normalize_workspace_path,
};

mod belief;
mod classify;
mod process_tools;
mod turn_probe;

use belief::{Belief, Coverage};

/// One tool the agent can call. Input arrives as the model-produced JSON;
/// output is always a typed `ToolOutput` (never a bare string).
#[async_trait]
pub trait Tool: Send + Sync {
    /// Schema advertised to the model: name, description, JSON Schema.
    fn schema(&self) -> ToolSchema;

    /// Execute the tool. `root` is the workspace root for path resolution;
    /// tools must never read or write outside it without explicit opt-in.
    async fn execute(&self, input: &Value, root: &std::path::Path) -> ToolOutput;

    /// The command line this call would run, for the registry's blocking
    /// `command.started` policy chain (#804). `None` — the default — means
    /// the tool runs no external command, or carries it in a plain input
    /// field the registry reads itself (`ToolRegistry::command_line_for`'s
    /// explicit arms: `bash`, `verify_done`, `start_process`, `send_stdin`).
    ///
    /// Best-effort mirror of execute-time composition: a tool that cannot
    /// resolve up front (missing index, backend-resolved branch name)
    /// returns `None` and stays ungated for that call, returning its own
    /// named error at execute time — `run_script`'s shipped posture.
    async fn command_for_gate(&self, _input: &Value, _root: &std::path::Path) -> Option<String> {
        None
    }

    /// Whether sibling calls to this tool (and to read-only tools) in one
    /// step may be dispatched concurrently despite the schema not claiming
    /// `read_only`. The registry aggregates this into
    /// `ToolExecutor::parallel_safe_names` for the engine's dispatch
    /// grouping. Defaults to false — the safe direction. Override only when
    /// the tool mutates no workspace state AND its own machinery is built
    /// for concurrent siblings; the shipped case is the sub-agent spawn
    /// tool (`crate::subagent`), whose children are read-only by
    /// construction and whose dispatcher carves budget per child.
    fn parallel_safe(&self) -> bool {
        false
    }
}

/// A file op classified before execution: the normalized path the ledger
/// aggregates by and the CRUD event. Create-vs-update is decided here — it
/// depends on whether the file exists BEFORE the write, not after.
///
/// The path is workspace-relative and stays that way: every pre/post content
/// capture below reads it back through [`crate::rootfd`], so the ledger's
/// digests and line diffs come from the same confined walk the tools use and
/// cannot be re-pointed at a file outside the workspace between the classify
/// and the read (#938).
struct PendingTouch {
    path: String,
    op: FileOp,
}

/// Registry of all built-in tools, keyed by name. Also the session's
/// file-lifecycle ledger: every successful read/create/update/delete is
/// recorded per normalized path as a [`FileTouchEvent`] (CRUD event, reason,
/// line delta) — rendered by the TUI as the "Files Touched" panel and
/// persisted as the session's file-touch telemetry.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    /// Tools enabled AFTER construction. Kept in a separate interior-mutable
    /// overlay so the primary `tools` map stays immutable and lock-free; the
    /// overlay is consulted as a fallback in the three read paths (`schemas`,
    /// dispatch, name listing).
    ///
    /// Currently always EMPTY in practice: its one client,
    /// [`ToolRegistry::enable_code_graph_if_available`], installs
    /// `graph_query` — which the constructor now registers unconditionally
    /// (a graph tool builds its own index on first use), so that call always
    /// takes the already-registered early return. The overlay is kept because
    /// it is the seam a genuinely late-enabled tool would use, and removing
    /// it would be a cross-crate public-API change; it is not live code today.
    late_tools: std::sync::RwLock<HashMap<String, Arc<dyn Tool>>>,
    root: PathBuf,
    touched: std::sync::Mutex<FileTouchLedger>,
    /// `normalized workspace path → sha256 of the content THIS session last
    /// observed`, and the whole of the no-clobber guarantee.
    ///
    /// Every successful touch records what the file looked like when this
    /// agent was done with it — a read records what it read, a write records
    /// what it wrote. Before a later mutation of the same path, the on-disk
    /// content is re-hashed and compared: a mismatch means something outside
    /// this session changed the file since this agent last looked, so the
    /// mutation would silently overwrite work the agent never saw.
    ///
    /// # Why this needs no shared state
    ///
    /// It is deliberately NOT a lock, and there is no registry, daemon, or
    /// lease anywhere. Each agent remembers only what it saw itself, which is
    /// exactly enough: agent B's write is what makes agent A's memory stale,
    /// so A detects it locally with no channel to B and no knowledge that B
    /// exists. Two processes, two containers, or a human editing in an editor
    /// are all the same case.
    ///
    /// A lock would be strictly weaker. It serializes writers but does nothing
    /// about staleness: B waits, takes the lock, and overwrites the file A
    /// just changed using a plan A's content no longer justifies. Waiting is
    /// also the wrong cost model for an agent — being told "that moved, read
    /// it again" is a cheap retry, where blocking burns a turn.
    ///
    /// Each entry also carries HOW MUCH of that content the agent actually
    /// saw ([`Coverage`]), which decides what a later partial read is allowed
    /// to do to it — see [`Self::remember_observed`].
    observed: std::sync::Mutex<HashMap<String, Belief>>,
    /// The session's durable record, once a host attaches one. Absent — the
    /// default — leaves every existing caller behaving exactly as before.
    ///
    /// `RwLock`, not `OnceLock`, because a registry can outlive the session it
    /// was built for. The deck builds one registry and then lets the user
    /// switch sessions under it, re-keying everything that names the session —
    /// claim holder, notification attribution, journal sidecar. The durable
    /// record has to follow, or work done after the switch commits to the ref
    /// of the session the user left. Splitting one *registry's* history across
    /// two stores is not the hazard it first looks like: they are two
    /// sessions, and two histories is the correct answer.
    work_journal: std::sync::RwLock<Option<(stella_store::work_journal::WorkJournal, String)>>,
    /// Paths `read_symbol` resolved and read, shared with the registered
    /// tool instance and drained once per execution into the file-touch
    /// ledger — the symbol's file is resolved through the code graph
    /// mid-execution, so [`ToolRegistry::classify_file_op`] cannot see it in
    /// the input the way it sees `read_file`'s `path`.
    span_reads: crate::read_symbol::SpanReadLedger,
    /// The read-state ledger shared with `read_file`/`read_symbol`/`edit_file`/
    /// `write_file`, held here for one question only: did a read actually show
    /// the model the whole file? That answer is the [`Coverage`] grade
    /// [`Self::record_touch`] hands to [`Self::remember_observed`], which
    /// decides what the read may do to an existing belief.
    read_ledger: Arc<crate::read::ReadLedger>,
    /// The workspace before-image for the turn in flight, when one was taken
    /// ([`ToolRegistry::begin_workspace_probe`]). `None` before the first
    /// bracket, and `None` for every host that never brackets one — which is
    /// why an unbracketed session behaves exactly as it did before this
    /// existed.
    workspace_probe: std::sync::Mutex<Option<crate::shell_touch::WorkspaceProbe>>,
    /// The session's authored-change ledger — what the agent *meant* to change,
    /// as opposed to how the tree now differs from a commit. Fed from
    /// [`Self::record_touch`] because that is the one place holding both images
    /// of a write, and read by the verification pipeline wherever `git diff`
    /// structurally cannot answer (an untracked file, or no repository at all).
    /// See [`crate::authored_diff`] for why the two are different questions.
    authored: std::sync::Mutex<crate::authored_diff::AuthoredDiffLedger>,
    /// Whether this session brackets turns at all, latched by the first
    /// [`ToolRegistry::begin_workspace_probe`] and never cleared.
    ///
    /// This is the switch between the two probe granularities, and only ever
    /// one of them runs. A bracketing host (the pipeline) probes once per
    /// turn; a host that never brackets keeps the per-call probe in
    /// [`Self::execute`]. Running both would walk the tree twice per shell
    /// call *and* twice per turn to learn the same fact.
    ///
    /// Latched rather than tracking arm/disarm because it answers "does this
    /// host bracket?", not "is a bracket open?" — during the window between
    /// settling one turn and arming the next, the per-call probe must stay
    /// off or it would re-attribute what the turn probe just recorded.
    turn_bracketing: std::sync::atomic::AtomicBool,
    /// Opaque (mutating-capable, op-undeclared) calls executed since the
    /// current bracket was armed. [`Self::settle_workspace_probe`] refuses to
    /// attribute the bracket's tree delta when this is zero: with no call
    /// that could have written, an observed change is foreign — a concurrent
    /// edit in a shared worktree — and recording it would fabricate a
    /// `FileChange` for work nothing in this session did (#1553).
    bracket_opaque_calls: std::sync::atomic::AtomicU32,
    /// The session's memory-citation ledger, shared with the registered
    /// `cite_memory` tool instance and drained per execution by
    /// [`ToolRegistry::take_memory_citations`].
    citations: crate::memory::CitationLedger,
    /// Per-session agent-invocation ledger (see [`crate::agent_use`]) —
    /// drained once per execution by the persistence layer, exactly like
    /// the file-touch ledger above but as an event log, never aggregated.
    agent_uses: std::sync::Mutex<crate::agent_use::AgentUseLedger>,
    /// The session's MCP tool-usage ledger. External MCP tools bypass this
    /// registry (they run through `stella-mcp`'s `McpToolSet`), so nothing here
    /// writes to it — the CLI hands a clone ([`ToolRegistry::mcp_usage_ledger`])
    /// to the `McpToolSet` at connect, which appends, and drains it per
    /// execution via [`ToolRegistry::take_mcp_usage`]. The registry is just the
    /// carrier so `record_execution_end` has one handle for every ledger.
    mcp_usage: stella_core::mcp_usage::McpUsageLedger,
    /// The session task board, shared with the six registered `task_*` tool
    /// instances. The CLI snapshots it into `AgentEvent::TaskUpdate` after
    /// executions via [`ToolRegistry::task_board`].
    task_board: crate::tasks::TaskBoardHandle,
    /// Sub-agent spawn requests queued by `task_assign`, drained once per
    /// execution by the session driver via
    /// [`ToolRegistry::take_spawn_requests`] — exactly the citation-ledger
    /// drain discipline, so no request is dispatched twice.
    spawn_queue: crate::tasks::SpawnQueue,
    /// The sub-agent dispatcher (#922), filled by the host after
    /// construction via [`ToolRegistry::attach_sub_agent_dispatcher`] — it
    /// needs this registry as the child's tool set, so taking it as a
    /// constructor argument would be a reference cycle.
    sub_agent_dispatcher: crate::subagent::DispatcherSlot,
    /// Spend by sub-agents the `task` tool dispatched, drained by the engine
    /// at each step-boundary budget check. A tool cannot charge the turn's
    /// guard directly (the engine holds it mutably), so this ledger is how
    /// `--budget` stays a hard ceiling once turns nest.
    sub_agent_spend: stella_core::subagent::SubAgentSpendLedger,
    /// The pause gate and steering tap of the turn currently running, in the
    /// owned form a session-scoped dispatcher can hold — published by the
    /// driver alongside `events` below and read at dispatch time, so a child
    /// pauses and stops with the turn that asked for it.
    turn_controls: crate::subagent::TurnControlsSlot,
    storage_index: std::sync::Mutex<StorageIndex>,
    /// Which workspace paths are covered by a FRESH saved exploration map —
    /// the read-side analogue of `storage_index` (`docs/spec/
    /// exploration-sharing.md` §6). Built once at construction, refreshed
    /// after every `save_exploration`; drives the once-per-session
    /// "this area is already mapped" hints on search-tool results.
    exploration_coverage: std::sync::Mutex<ExplorationCoverage>,
    bus: std::sync::RwLock<Option<HookBus>>,
    /// The live policy-plane bridge subscription (receipts spec §6.4), if
    /// [`ToolRegistry::bridge_policy_plane`] wired one. Held so a re-bridge
    /// (a new turn's event channel) replaces — unsubscribes — the previous
    /// observer instead of accumulating stale senders.
    policy_bridge: std::sync::Mutex<Option<stella_core::bus::HookSubscription>>,
    /// Where [`ToolRegistry::record_touch`] announces file changes, when a host
    /// attached one. This is the SINGLE emission point for
    /// `AgentEvent::FileChange`: the recorder already holds the pre- and
    /// post-images and the authoritative line delta, so anything downstream
    /// that re-derives a file's `+`/`-` from tool inputs is guessing at data
    /// that was exact right here. Absent (`None`) on a registry whose changes
    /// must NOT be announced — a Best-of-N or witness candidate, whose edits
    /// live in a shadow worktree and are discarded unless adopted.
    events: std::sync::RwLock<Option<stella_core::EventSender>>,
    /// Whether *mutating* touches may be announced on `events`. False only on a
    /// candidate registry ([`ToolRegistry::attach_read_events`]).
    ///
    /// The withholding rule is about writes: a candidate's edits live in a
    /// shadow worktree and are not the user's until adoption re-emits them
    /// against the real tree. Reads carry none of that — a read mutates
    /// nothing, so it claims nothing — but they used to be silenced by the
    /// same blanket `events: None`, which is why an isolated run
    /// (`create_worktrees`, Best-of-N, an authored witness) showed a
    /// completely empty Files tab: not one read, and no mutation until a
    /// passing verdict adopted. Splitting the two lets the invariant say what
    /// it actually means.
    announce_mutations: std::sync::atomic::AtomicBool,
    /// How many **mutating** touches [`ToolRegistry::record_touch`] has
    /// recorded, ever, on this registry. Bumped inside the same locked section
    /// that writes the ledger and immediately before the `FileChange` is sent,
    /// so it counts exactly the mutating events the stream carries — read it
    /// with [`ToolRegistry::mutations_recorded`].
    ///
    /// It exists because the event and the count travelled different wires. A
    /// host attaches its session channel with [`ToolRegistry::attach_events`],
    /// and `record_touch` sends there directly; a verifier that wraps the
    /// *engine's* sender to tally `FileChange`s therefore sees none of them and
    /// concludes nothing was touched (#973 — the verifier was told
    /// `file_change_events=0` with six in the stream). A counter on the
    /// recorder is readable no matter which channel the events went down, and
    /// unlike the ledger's length it never falls back when a file is touched
    /// twice: this is a count of *touches*, one per emitted event.
    ///
    /// Monotonic and never reset, so readers take a delta across the window
    /// they care about rather than trusting an absolute value.
    mutations: std::sync::atomic::AtomicU64,
    process_free: bool,
}

/// Path-coverage of fresh exploration maps plus the hint dedup set.
#[derive(Debug, Default)]
struct ExplorationCoverage {
    /// Workspace-relative path → slices of fresh, complete maps covering it.
    by_path: HashMap<String, Vec<String>>,
    /// Slices already hinted this session — each map is suggested at most
    /// once, so the nudge can never become nagging.
    hinted: HashSet<String>,
}

impl ExplorationCoverage {
    /// Scan the exploration store; only fresh, complete maps generate
    /// coverage (a drifted or draft map must not suppress real exploration).
    fn rebuild(root: &std::path::Path) -> Self {
        let mut by_path: HashMap<String, Vec<String>> = HashMap::new();
        for summary in crate::exploration::summaries_sync(root) {
            if summary.status != crate::exploration::ExplorationStatus::Complete
                || !summary.freshness.is_fresh()
            {
                continue;
            }
            for path in &summary.covered {
                by_path
                    .entry(path.clone())
                    .or_default()
                    .push(summary.slice.clone());
            }
        }
        Self {
            by_path,
            hinted: HashSet::new(),
        }
    }
}

/// The session's storage-map state for the pre-write gate
/// (`docs/spec/storage-map.md` §8): a host-seeded baseline snapshot plus
/// the relations created by writes this session — which the on-disk index
/// may not have re-indexed yet. The gate merges both over a fresh read of
/// the persisted index on every gated write.
#[derive(Debug, Clone, Default)]
pub struct StorageIndex {
    baseline: stella_graph::StorageSnapshot,
    session: Vec<stella_graph::storage::RelationEntry>,
}

/// Prerequisites and host attestations the registry needs to decide what it
/// *can* build. Deliberately NOT a policy surface: the `bash`/`web` booleans
/// that used to live here were operator policy welded into construction, and
/// they only ever covered built-ins — an MCP or custom tool never passed
/// through them. That job belongs to [`crate::policy::ToolPolicy`], enforced
/// once above the whole session tool stack.
#[derive(Clone, Default)]
pub struct RegistryOptions {
    pub media_spend_gate: Option<Arc<dyn stella_media::MediaSpendGate>>,
    pub media_operation_ids: Option<Arc<dyn crate::media::MediaOperationIdSource>>,
    pub media_operation_journal: Option<Arc<dyn stella_media::MediaOperationJournal>>,
    pub media_requires_host_approval: bool,
    pub media_host_data_isolation: Option<crate::media::HostDataIsolation>,
}

impl ToolRegistry {
    /// Construct with auto-detected optional backends.
    pub fn new(root: PathBuf, options: RegistryOptions) -> Self {
        let issue_backend = if crate::media::process_free(options.media_host_data_isolation) {
            None
        } else {
            crate::issues::detect_issue_backend()
        };
        Self::with_backends_and_options(
            root,
            issue_backend,
            crate::media::detect_media_backend(),
            options,
        )
    }

    /// [`ToolRegistry::new`] with the process-spawning issue-backend probe
    /// routed through the blocking pool (#64) — the constructor every async
    /// session driver uses.
    pub async fn new_detected(root: PathBuf, options: RegistryOptions) -> Self {
        let issue_backend = if crate::media::process_free(options.media_host_data_isolation) {
            None
        } else {
            crate::issues::detect_issue_backend_async().await
        };
        Self::with_backends_and_options(
            root,
            issue_backend,
            crate::media::detect_media_backend(),
            options,
        )
    }

    /// Construct with an explicit issue backend (or none), no media
    /// backend, and default options — the seam unit tests use so tool
    /// counts depend on neither the host's `gh` auth nor its provider env
    /// keys (nor any bash opt-in).
    pub fn with_issue_backend(
        root: PathBuf,
        issue_backend: Option<crate::issues::IssueBackend>,
    ) -> Self {
        Self::with_backends(root, issue_backend, None)
    }

    /// [`ToolRegistry::with_backends_and_options`] with default options.
    pub fn with_backends(
        root: PathBuf,
        issue_backend: Option<crate::issues::IssueBackend>,
        media_backend: Option<crate::media::MediaBackend>,
    ) -> Self {
        Self::with_backends_and_options(
            root,
            issue_backend,
            media_backend,
            RegistryOptions::default(),
        )
    }

    /// Construct with every optional backend and feature switch explicit —
    /// the full seam.
    pub fn with_backends_and_options(
        root: PathBuf,
        issue_backend: Option<crate::issues::IssueBackend>,
        media_backend: Option<crate::media::MediaBackend>,
        options: RegistryOptions,
    ) -> Self {
        let process_free = crate::media::process_free(options.media_host_data_isolation);
        let media_host_context = options
            .media_spend_gate
            .clone()
            .zip(options.media_operation_ids.clone())
            .zip(options.media_operation_journal.clone())
            .zip(options.media_host_data_isolation)
            .filter(|_| options.media_requires_host_approval && process_free)
            .map(|(((gate, ids), journal), _)| (gate, ids, journal));
        let citations: crate::memory::CitationLedger = Arc::default();
        let mcp_usage: stella_core::mcp_usage::McpUsageLedger = Arc::default();
        let task_board: crate::tasks::TaskBoardHandle = Arc::default();
        let spawn_queue: crate::tasks::SpawnQueue = Arc::default();
        let sub_agent_dispatcher: crate::subagent::DispatcherSlot = Arc::default();
        let sub_agent_spend: stella_core::subagent::SubAgentSpendLedger = Arc::default();
        // One read-state ledger per registry (#331): `read_file` and
        // `read_symbol` (which reads through the same instance) record what
        // the model saw; `edit_file` attributes match failures against it and
        // `write_file`/`edit_file` record the content the model produced.
        let read_ledger: Arc<crate::read::ReadLedger> = Arc::default();
        let read_file = Arc::new(crate::read::ReadFile::with_ledger(read_ledger.clone()));
        let span_reads: crate::read_symbol::SpanReadLedger = Arc::default();
        let mut tools: HashMap<String, Arc<dyn Tool>> = HashMap::new();
        let mut entries: Vec<Arc<dyn Tool>> = vec![
            read_file.clone(),
            Arc::new(crate::write::WriteFile::with_ledger(read_ledger.clone())),
            Arc::new(crate::edit::EditFile::with_ledger(read_ledger.clone())),
            Arc::new(crate::apply_edits::ApplyEdits::with_ledger(
                read_ledger.clone(),
            )),
            Arc::new(crate::delete::DeleteFile),
            Arc::new(crate::memory::SaveMemory),
            Arc::new(crate::memory::CiteMemory(citations.clone())),
            Arc::new(crate::scripts::ListScripts),
            Arc::new(crate::tasks::TaskCreate(task_board.clone())),
            Arc::new(crate::tasks::TaskList(task_board.clone())),
            Arc::new(crate::tasks::TaskStart(task_board.clone())),
            Arc::new(crate::tasks::TaskComplete(task_board.clone())),
            Arc::new(crate::tasks::TaskCancel(task_board.clone())),
            // Registered unconditionally — see `attach_sub_agent_dispatcher`
            // on why an unattached dispatcher is still the honest shape.
            Arc::new(crate::subagent::SpawnSubAgent::new(
                sub_agent_dispatcher.clone(),
            )),
            // SVG generation is client-side (the model authors the SVG, the
            // pipeline validates/sanitizes) — no provider key needed, so
            // unlike image/video it is always registered.
            Arc::new(crate::media::GenerateSvg),
        ];
        // `grep`, `glob`, and exploration staleness checks also launch fixed
        // child processes, so the isolation mode keeps this omission literal.
        if !process_free {
            entries.push(Arc::new(crate::gather::GatherContext));
            // Search results carry a code-map footer; a symbol-shaped grep
            // pattern additionally carries the graph_query pointer, decided
            // per-call from the pattern (crate::code_map::is_symbol_shaped).
            entries.push(Arc::new(crate::grep::Grep::with_code_map()));
            entries.push(Arc::new(crate::glob::Glob::with_code_map()));
            entries.push(Arc::new(crate::exploration::Explorations));
            entries.push(Arc::new(crate::exploration::SaveExploration));
            entries.extend(process_tools::builtins(
                task_board.clone(),
                spawn_queue.clone(),
            ));
            // The web family registers by default, like every other built-in.
            // A fetched page is untrusted input and an egress channel, which is
            // a reason to be able to switch it off (`"tools": {"web": "off"}`),
            // not a reason to ship it dark — an agent that cannot read a doc
            // page is worse at the job for every operator who never found the
            // opt-in. It sits inside the process-free branch because the
            // isolation mode omits the whole host-reaching authority class.
            // `web_search` additionally needs a BYOK search key — no key, no
            // dead schema, mirroring the media tools' conditional registration.
            let auth: Arc<crate::web::WebAuthState> =
                Arc::new(crate::web::WebAuthConfig::load_default());
            entries.push(Arc::new(crate::web::WebFetch(auth.clone())));
            entries.push(Arc::new(crate::web::WebExtractAssets(auth.clone())));
            entries.push(Arc::new(crate::web::WebDownload(auth)));
            if let Some(search) = crate::web::detect_search_backend() {
                entries.push(Arc::new(crate::web::WebSearch(search)));
            }
        }
        // Both advertised unconditionally: a graph tool builds its own
        // index on first use (`crate::graph::open_or_build`), so gating
        // registration on the index already existing would hide exactly the
        // tool meant to CREATE it — and leave the agent to grep instead. The
        // background session build still runs; this just removes the race and
        // the chicken-and-egg gate.
        entries.push(Arc::new(crate::overview::ProjectOverview));
        entries.push(Arc::new(crate::graph::CodeGraphQuery));
        entries.push(Arc::new(crate::read_symbol::ReadSymbol::new(
            read_file,
            span_reads.clone(),
        )));
        // The launch-posture ruling (#785): the PAID media tools surface only
        // when an approving host context exists. Without one they would pair
        // with the deny-by-default gate — registering, being offered to the
        // model, consuming model calls, and refusing every time. Fail-closed
        // stays the architecture; a tool that cannot ever succeed is simply
        // not advertised, the same "no key, no dead schema" posture as
        // `web_search`. (`GenerateSvg` above is client-side and free, so it
        // registers regardless.)
        if let (Some(media), Some((gate, ids, journal))) = (media_backend, &media_host_context) {
            entries.push(Arc::new(crate::media::GenerateImage::with_host_context(
                media.image,
                gate.clone(),
                ids.clone(),
                journal.clone(),
            )) as Arc<dyn Tool>);
            if let Some(video) = media.video {
                entries.push(Arc::new(crate::media::GenerateVideo::with_host_context(
                    video.clone(),
                    gate.clone(),
                    ids.clone(),
                    journal.clone(),
                )) as Arc<dyn Tool>);
                entries.push(Arc::new(crate::media::PollVideo::with_operation_journal(
                    video,
                    journal.clone(),
                )) as Arc<dyn Tool>);
            }
        }
        if let Some(backend) = issue_backend.filter(|_| !process_free) {
            let backend = Arc::new(backend);
            entries.push(Arc::new(crate::issues::CreateIssue(backend.clone())));
            entries.push(Arc::new(crate::issues::UpdateIssue(backend.clone())));
            entries.push(Arc::new(crate::issues::CloseIssue(backend.clone())));
            entries.push(Arc::new(crate::issues::SearchIssues(backend.clone())));
            entries.push(Arc::new(crate::issues::GetIssue(backend.clone())));
            entries.push(Arc::new(crate::issues::ListLabels(backend.clone())));
            entries.push(Arc::new(crate::issues::ListMembers(backend.clone())));
            entries.push(Arc::new(crate::issues::StartWorkOnIssue(backend)));
        }
        for tool in entries {
            let name = tool.schema().name;
            tools.insert(name, tool);
        }
        let exploration_coverage = std::sync::Mutex::new(if process_free {
            ExplorationCoverage::default()
        } else {
            ExplorationCoverage::rebuild(&root)
        });
        Self {
            tools,
            late_tools: std::sync::RwLock::new(HashMap::new()),
            root,
            touched: std::sync::Mutex::new(FileTouchLedger::default()),
            observed: std::sync::Mutex::new(HashMap::new()),
            work_journal: std::sync::RwLock::new(None),
            span_reads,
            read_ledger,
            workspace_probe: std::sync::Mutex::new(None),
            authored: std::sync::Mutex::new(crate::authored_diff::AuthoredDiffLedger::default()),
            turn_bracketing: std::sync::atomic::AtomicBool::new(false),
            bracket_opaque_calls: std::sync::atomic::AtomicU32::new(0),
            citations,
            agent_uses: std::sync::Mutex::new(crate::agent_use::AgentUseLedger::default()),
            mcp_usage,
            task_board,
            sub_agent_dispatcher,
            sub_agent_spend,
            turn_controls: crate::subagent::TurnControlsSlot::default(),
            spawn_queue,
            storage_index: std::sync::Mutex::new(StorageIndex::default()),
            exploration_coverage,
            bus: std::sync::RwLock::new(None),
            policy_bridge: std::sync::Mutex::new(None),
            events: std::sync::RwLock::new(None),
            announce_mutations: std::sync::atomic::AtomicBool::new(true),
            mutations: std::sync::atomic::AtomicU64::new(0),
            process_free,
        }
    }

    pub fn is_process_free(&self) -> bool {
        self.process_free
    }

    /// Attach the session's extension hook bus. From this point every tool
    /// call runs the blocking policy chains (`tool.call.requested`, then
    /// `file.created`/`file.updated`/`file.deleted` or `command.started`)
    /// before executing, and emits the observer events documented in
    /// `website/content/docs/agent-tools/hooks.mdx`. Also emits one `tool.registered` per registered
    /// tool, name-sorted, so extensions see the tool surface up front.
    pub fn attach_bus(&self, bus: HookBus) {
        let mut schemas = ToolRegistry::schemas(self);
        schemas.sort_by(|a, b| a.name.cmp(&b.name));
        for schema in schemas {
            bus.emit_named(
                hook_names::TOOL_REGISTERED,
                serde_json::json!({
                    "tool": schema.name,
                    "read_only": schema.read_only,
                    "speculation_safe": schema.speculation_safe,
                }),
            );
        }
        *self.bus.write().unwrap_or_else(|p| p.into_inner()) = Some(bus);
    }

    /// Bridge the attached bus's policy/extension audit plane into an
    /// `AgentEvent` stream (receipts spec §6.4, #364 gap 6): every
    /// `policy.evaluated` / `policy.blocked` / `approval.requested` /
    /// `secret.detected` the bus emits lands as a content-free
    /// [`stella_protocol::AgentEvent::PolicyDecision`] on `events`, so
    /// whatever journal the host hangs off that stream carries the policy
    /// plane too — previously a process-ephemeral ring that evaporated at
    /// exit. Returns `false` (registering nothing) when no bus is attached:
    /// the plane doesn't exist without one. Call after [`Self::attach_bus`];
    /// calling again (a new turn's event channel) replaces the previous
    /// bridge, so stale senders never accumulate as observer failures.
    pub fn bridge_policy_plane(&self, events: stella_core::EventSender) -> bool {
        let Some(bus) = self.bus() else {
            return false;
        };
        let subscription = stella_core::bus::bridge_policy_plane(&bus, events);
        // Dropping the previous subscription (if any) unsubscribes it.
        *self.policy_bridge.lock().unwrap_or_else(|p| p.into_inner()) = Some(subscription);
        true
    }

    /// The attached hook bus, if any (cheap clone — shared inner).
    fn bus(&self) -> Option<HookBus> {
        self.bus.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Announce this registry's file changes on `events`, as
    /// `AgentEvent::FileChange` emitted by `record_touch` — the same call that
    /// writes the ledger, so a surface reading the event stream and a surface
    /// reading the audit log see identical `+`/`-` numbers.
    ///
    /// Call once per turn with that turn's channel; a later call replaces the
    /// previous sender, so a stale channel is dropped rather than accumulated.
    /// Leave it unattached on a registry whose edits must not be claimed as the
    /// user's — a candidate workspace's, until adoption.
    pub fn attach_events(&self, events: stella_core::EventSender) {
        *self.events.write().unwrap_or_else(|p| p.into_inner()) = Some(events);
        self.announce_mutations
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Announce only this registry's **reads** on `events` — the candidate
    /// posture. Mutations stay withheld until adoption re-emits them against
    /// the real tree (`Pipeline::run_best_of_n`).
    ///
    /// The alternative, and what this replaces, was attaching nothing at all.
    /// That did keep a losing candidate's edits out of the user's Files tab —
    /// but it also meant an isolated run announced *nothing whatsoever*, so a
    /// session that answered "yes" to `create_worktrees` watched 40 `read_file`
    /// calls scroll past a Files tab reading "no files touched yet". Reads are
    /// safe to announce the moment they happen precisely because adoption has
    /// no opinion about them: there is nothing to adopt, and nothing claimed.
    ///
    /// Call once per candidate, with the session channel. `detach_event_stream`
    /// is the counterpart, and restores the announcing default.
    pub fn attach_read_events(&self, events: stella_core::EventSender) {
        *self.events.write().unwrap_or_else(|p| p.into_inner()) = Some(events);
        self.announce_mutations
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    /// Release this turn's event channel: the counterpart every turn owes
    /// [`Self::attach_events`] and [`Self::bridge_policy_plane`].
    ///
    /// Both hand the registry an `EventSender`, which is an `Arc<dyn Fn>` over
    /// the renderer's channel — so while the registry holds one, that channel
    /// has a live sender. A one-shot run's registry outlives its turn (the
    /// audit close-out still reads its ledger), so the caller dropping its own
    /// sender never closed the channel: the renderer's `recv()` loop stayed
    /// pending and a *completed* `stella run` hung until something killed it
    /// (#960). Detaching here is what actually ends the stream.
    ///
    /// Idempotent, and safe on a registry that never attached either one.
    pub fn detach_event_stream(&self) {
        *self.events.write().unwrap_or_else(|p| p.into_inner()) = None;
        // Back to the announcing default, so a registry re-attached later (a
        // next turn) is never left silently withholding a real tree's edits.
        self.announce_mutations
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // Dropping the subscription unsubscribes it, releasing the sender the
        // bridge closure captured.
        *self.policy_bridge.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    /// Let the `task` tool run sub-agents through `dispatcher` (#922).
    ///
    /// Late attachment, not a constructor argument: a dispatcher needs this
    /// registry as the child's tool set, so the two would otherwise own each
    /// other. Until it is called the tool reports sub-agents as unavailable —
    /// a truthful result, not a silently missing capability. Once per
    /// session; a later call replaces the previous dispatcher.
    pub fn attach_sub_agent_dispatcher(
        &self,
        dispatcher: std::sync::Arc<dyn stella_core::subagent::SubAgentDispatcher>,
    ) {
        *self
            .sub_agent_dispatcher
            .write()
            .unwrap_or_else(|p| p.into_inner()) = Some(dispatcher);
    }

    /// Publish this turn's pause gate and steering tap, until the returned
    /// guard drops.
    ///
    /// The companion to [`Self::attach_events`], called from the same place
    /// for the same reason: a sub-agent dispatcher is session-scoped, so
    /// anything turn-shaped it needs has to be read from here at dispatch
    /// time. Without this, a child dispatched by a tool call never sees the
    /// pause or the soft stop that governs its parent. A guard rather than a
    /// bare setter — see [`crate::subagent::TurnControlsGuard`].
    pub fn attach_turn_controls(
        &self,
        controls: stella_core::ports::TurnControls,
    ) -> crate::subagent::TurnControlsGuard {
        crate::subagent::TurnControlsGuard::attach(&self.turn_controls, controls)
    }

    /// The ledger a dispatcher charges finished children to (cheap clone —
    /// shared inner), drained by the engine at each step-boundary check.
    ///
    /// `pub` because settling is the *dispatcher's* job, per
    /// [`stella_core::subagent::SubAgentDispatcher`]'s contract: it must
    /// happen the moment a child stops, on the child's own thread, or a
    /// parent cancelled mid-`task` leaves paid-for dollars in no ledger at
    /// all.
    pub fn sub_agent_spend_ledger(&self) -> stella_core::subagent::SubAgentSpendLedger {
        self.sub_agent_spend.clone()
    }

    /// The current turn's boundary controls — empty between turns, and on a
    /// driver that publishes none.
    pub fn turn_controls(&self) -> stella_core::ports::TurnControls {
        self.turn_controls
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// The attached turn event sender, if any (cheap clone — shared inner).
    /// `pub` so a session-scoped sub-agent dispatcher (#922) can read the
    /// *current* turn's sender at dispatch time rather than each driver
    /// having to re-attach a dispatcher of its own.
    pub fn events(&self) -> Option<stella_core::EventSender> {
        self.events
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Enable `graph_query` after a background build or mid-session `/init`;
    /// the shared overlay avoids rebuilding the registry and its MCP wrapper.
    ///
    /// A no-op since `graph_query` became unconditionally registered (see the
    /// `late_tools` field): the `contains_key` early return always fires.
    /// Kept as the host-facing seam — callers outside this crate still invoke
    /// it after an index build, and it must stay correct if registration ever
    /// becomes conditional again.
    pub fn enable_code_graph_if_available(&self, root: &std::path::Path) -> Result<(), String> {
        if !crate::graph::graph_available(root)? {
            return Ok(());
        }
        let tool: Arc<dyn Tool> = Arc::new(crate::graph::CodeGraphQuery);
        let name = tool.schema().name;
        if self.tools.contains_key(&name) {
            return Ok(()); // already registered at construction
        }
        let mut late = self.late_tools.write().unwrap_or_else(|p| p.into_inner());
        late.entry(name).or_insert(tool);
        Ok(())
    }

    /// All tool schemas, for advertising to the model — primary tools plus any
    /// late-enabled overlay tools, sorted by name.
    pub fn schemas(&self) -> Vec<ToolSchema> {
        let mut schemas: Vec<ToolSchema> = self.tools.values().map(|t| t.schema()).collect();
        let late = self.late_tools.read().unwrap_or_else(|p| p.into_inner());
        schemas.extend(late.values().map(|t| t.schema()));
        // Sort by name: the maps iterate in per-process-randomized HashMap
        // order, and this list is serialized verbatim at position 0 of the
        // prompt prefix. Prompt caching is a byte-level prefix match, so a
        // deterministic order lets two processes (or a restart within the
        // cache TTL) share the tools+system cache entry instead of each
        // writing a divergent one.
        schemas.sort_by(|a, b| a.name.cmp(&b.name));
        schemas
    }

    /// Execute a tool by name. Returns an error `ToolOutput` if the name is
    /// unknown or the input is malformed — never panics.
    pub async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        let bus = self.bus();
        let started_at = std::time::Instant::now();

        // Extension policy, stage 1: the `tool.call.requested` blocking
        // chain. Runs FIRST — a `modify` decision replaces the input, and
        // everything downstream (classification, pre-content capture, the
        // schema gate, execution) must see the final input, not the
        // original.
        let mut modified_input: Option<Value> = None;
        if let Some(bus) = &bus {
            match self.gate_tool_call(bus, name, input) {
                Ok(replacement) => modified_input = replacement,
                Err(denied) => return denied,
            }
        }
        let input: &Value = modified_input.as_ref().unwrap_or(input);

        // A save_exploration's staleness manifest is built from the map's
        // actual evidence: pass the session's read paths from the file-touch
        // ledger through the internal input key (spec §3d). The model never
        // authors this field; a value it did author is replaced — and when
        // the session recorded no reads, removed, or it would flow into the
        // manifest as fabricated evidence.
        let mut ledger_augmented: Option<Value> = None;
        if name == "save_exploration"
            && let Value::Object(map) = input
        {
            let read_paths: Vec<String> = self
                .files_touched()
                .into_iter()
                .filter(|(_, letters)| letters.contains('R'))
                .map(|(path, _)| path)
                .collect();
            if !read_paths.is_empty() {
                let mut map = map.clone();
                map.insert(
                    crate::exploration::LEDGER_FILES_KEY.to_string(),
                    serde_json::json!(read_paths),
                );
                ledger_augmented = Some(Value::Object(map));
            } else if map.contains_key(crate::exploration::LEDGER_FILES_KEY) {
                let mut map = map.clone();
                map.remove(crate::exploration::LEDGER_FILES_KEY);
                ledger_augmented = Some(Value::Object(map));
            }
        }
        let input: &Value = ledger_augmented.as_ref().unwrap_or(input);

        // Resolve the tool now rather than at dispatch below: the command
        // fence asks IT for the line it would run. Clone the `Arc` out so
        // the overlay's read lock is released before any `.await`.
        let tool = self.tools.get(name).cloned().or_else(|| {
            self.late_tools
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .get(name)
                .cloned()
        });

        // `run_script` threads its gate resolution into execution through an
        // internal input key, so the line the `command.started` chain
        // approves is the line that runs — closing the gate/execute index
        // TOCTOU (#804; see `scripts::gate`). Any model-authored key is
        // stripped on the way.
        let (gate_threaded, script_resolved) = if name == "run_script" {
            crate::scripts::thread_gate_resolution(&self.root, input, bus.is_some()).await
        } else {
            (None, None)
        };
        let input: &Value = gate_threaded.as_ref().unwrap_or(input);

        // The effective command line for the `command.started` policy chain
        // and the command.* observer events: `run_script`'s threaded
        // resolution, `build_project`/`run_tests`'s index composition (their
        // explicit `command` override is read off the input by
        // `command_line_for` instead), or whatever the tool itself reports
        // it would run ([`Tool::command_for_gate`]). A failed resolution is
        // not gated — the tool returns the named error. Gated on an attached
        // bus so a bus-less registry never pays for index detection.
        let resolved_command: Option<String> = match (&bus, name) {
            (Some(_), "run_script") => script_resolved,
            (Some(_), "build_project" | "run_tests") => {
                crate::project::resolve_command_for_gate(name, &self.root, input).await
            }
            (Some(_), _) => match &tool {
                Some(tool) => tool.command_for_gate(input, &self.root).await,
                None => None,
            },
            _ => None,
        };

        // Classify the file op(s) BEFORE executing: create-vs-update depends
        // on whether the file exists now, not after the write. Single-path
        // tools yield zero or one op; `apply_edits` yields one Update per
        // distinct file in its batch.
        let pending_ops = self.classify_file_ops(name, input);
        // The no-clobber guard. Refuse BEFORE executing: once a write lands,
        // the other agent's work is already gone and the best this could do is
        // narrate the loss. Returned as a tool `Error` rather than a turn
        // abort because it is a recoverable, model-actionable condition — the
        // message names the file and the fix, and re-reading is one cheap step
        // where a failed turn would be a whole one.
        if let Some(refusal) = self.refused_mutation(name, &pending_ops) {
            return ToolOutput::Error {
                message: refusal.message(),
            };
        }
        // Updates need the pre-write content for the line diff; deletes need
        // it for the pre-deletion line count. Lossy UTF-8 so a binary file
        // still yields a deterministic (if approximate) line count.
        let pre_contents: Vec<Option<String>> = pending_ops
            .iter()
            .map(|pending| match pending.op {
                FileOp::Update | FileOp::Delete => {
                    crate::rootfd::read_confined_lossy(&self.root, &pending.path)
                }
                _ => None,
            })
            .collect();

        // Storage gate (docs/spec/storage-map.md §8): if the call targets
        // storage-definition files, parse each proposed post-write content
        // with the SAME adapter extraction the indexer uses and verifier it
        // against the live storage map before anything lands. Objects a
        // target file already defines on disk are exempt — rewriting an
        // existing migration in place is not a duplicate. `write_file` /
        // `edit_file` carry one target; an `apply_edits` batch may carry
        // several (#442), each judged with the same check, so the
        // transactional path is not a way around the gate. `storage_intent`
        // is per CALL: one declaration covers every object the batch lands,
        // exactly as it covers a whole `write_file`.
        let mut pending_storage: Vec<crate::schema_gate::GatePass> = Vec::new();
        let mut declared_intent: Option<String> = None;
        if let Some(extractor) = crate::schema_gate::extractor() {
            // (path, current on-disk content, simulated post-write source).
            // The post-write file is simulated — for edit_file the current
            // content with the replacement applied, for apply_edits the
            // batch's composed edits (`apply_edits::simulate_batch`, the
            // validate phase's own composition) — so whole-file adapters see
            // the resulting definitions; a Prisma model gaining a field is
            // invisible in `new_string` alone. A failed simulation falls
            // back to the fragment / stays unjudged: the tool itself then
            // returns the named error and writes nothing.
            let mut targets: Vec<(String, Option<String>, Option<String>)> = Vec::new();
            match name {
                "write_file" | "edit_file" => {
                    if let Some(path) = input.get("path").and_then(|v| v.as_str())
                        && crate::schema_gate::is_schema_file(path)
                    {
                        let current = crate::rootfd::read_confined(&self.root, path);
                        let proposed_src: Option<String> = match name {
                            "write_file" => input
                                .get("content")
                                .and_then(|v| v.as_str())
                                .map(str::to_string),
                            _ => {
                                let new_string = input.get("new_string").and_then(|v| v.as_str());
                                let old_string = input.get("old_string").and_then(|v| v.as_str());
                                match (new_string, old_string, &current) {
                                    (Some(new), Some(old), Some(cur))
                                        if !old.is_empty() && cur.contains(old) =>
                                    {
                                        let replace_all = input
                                            .get("replace_all")
                                            .and_then(|v| v.as_bool())
                                            .unwrap_or(false);
                                        Some(if replace_all {
                                            cur.replace(old, new)
                                        } else {
                                            cur.replacen(old, new, 1)
                                        })
                                    }
                                    (Some(new), _, _) => Some(new.to_string()),
                                    _ => None,
                                }
                            }
                        };
                        targets.push((path.to_string(), current, proposed_src));
                    }
                }
                "apply_edits" => {
                    let mut seen = HashSet::new();
                    for path in input
                        .get("edits")
                        .and_then(|v| v.as_array())
                        .into_iter()
                        .flatten()
                        .filter_map(|e| e.get("path").and_then(|v| v.as_str()))
                    {
                        if !crate::schema_gate::is_schema_file(path)
                            || !seen.insert(path.to_string())
                        {
                            continue;
                        }
                        let current = crate::rootfd::read_confined(&self.root, path);
                        let proposed_src = current
                            .as_deref()
                            .and_then(|cur| crate::apply_edits::simulate_batch(input, path, cur));
                        targets.push((path.to_string(), current, proposed_src));
                    }
                }
                _ => {}
            }
            // The persisted snapshot opens SQLite, so it goes to the blocking
            // pool (#549) — loaded once for the whole call, and only when a
            // target actually extracts definitions. The `.await` happens
            // BEFORE the overlay merge takes the `storage_index` std mutex —
            // a guard held across an await would make this future non-`Send`.
            let mut snapshot: Option<stella_graph::StorageSnapshot> = None;
            for (path, current, proposed_src) in targets {
                let proposed = proposed_src
                    .as_deref()
                    .map(|src| extractor.extract(&path, src))
                    .unwrap_or_default();
                if proposed.is_empty() {
                    continue;
                }
                if snapshot.is_none() {
                    let persisted = {
                        let root = self.root.clone();
                        tokio::task::spawn_blocking(move || {
                            crate::graph::load_storage_snapshot(&root)
                        })
                        .await
                        .unwrap_or_else(|_| Err("the storage map load was cancelled".into()))
                    };
                    snapshot = match persisted {
                        Ok(persisted) => Some(self.merge_storage_overlay(persisted)),
                        Err(message) => return ToolOutput::Error { message },
                    };
                }
                let own = current
                    .as_deref()
                    .map(|content| extractor.extract(&path, content))
                    .unwrap_or_default();
                let manifest_layer = crate::schema_gate::manifest_layer_for(&self.root, &path);
                let intent = input.get("storage_intent").and_then(|v| v.as_str());
                match crate::schema_gate::check(&crate::schema_gate::GateRequest {
                    path: &path,
                    manifest_layer: manifest_layer.as_deref(),
                    proposed: &proposed,
                    own: &own,
                    snapshot: snapshot.as_ref().expect("loaded above"),
                    storage_intent: intent,
                }) {
                    Ok(pass) => {
                        declared_intent = intent.map(str::to_string);
                        pending_storage.push(pass);
                    }
                    Err(message) => return ToolOutput::Error { message },
                }
            }
        }

        // Extension policy, stage 2: the per-side-effect blocking chains
        // (`file.created`/`file.updated`/`file.deleted`, `command.started`)
        // plus the payload-hygiene detectors, then the observable
        // `tool.call.started` (with content-bearing fields sanitized).
        if let Some(bus) = &bus {
            if let Err(denied) =
                self.gate_side_effects(bus, name, input, &pending_ops, resolved_command.as_deref())
            {
                return denied;
            }
            bus.emit_named(
                hook_names::TOOL_CALL_STARTED,
                serde_json::json!({ "tool": name, "input": bus::sanitize_tool_input(input) }),
            );
        }

        // Opaque calls get a before-image. A tool whose input named no file
        // op but which is allowed to mutate is precisely `bash` and its
        // relatives: the schema cannot say what they will touch, so the tree
        // is asked instead (see [`crate::shell_touch`]). Read-only tools are
        // skipped, and an *unknown* tool counts as mutating for the same
        // reason the ladder counts it so — on Terminal-Bench the shell is how
        // nearly all real work lands, and a name we fail to recognize must
        // never be the reason a change goes unattributed.
        //
        // Skipped entirely when the host brackets turns: the turn probe covers
        // the same ground for one pair of walks instead of one pair per call.
        // The measurement that forces the choice — 757 of 1,063 calls were
        // shell calls against a 900s trial kill — makes per-call probing cost
        // more than the trial it is meant to observe. Hosts that never bracket
        // keep this path, so nothing regresses for them.
        let opaque_call =
            pending_ops.is_empty() && !tool.as_ref().map(|t| t.schema().read_only).unwrap_or(false);
        let bracketing = self
            .turn_bracketing
            .load(std::sync::atomic::Ordering::Relaxed);
        // A bracketing host gets no per-call probe, but the settle still has
        // to know whether any call this bracket COULD have written — that
        // count is what stops a foreign mid-turn edit being attributed to a
        // turn that dispatched nothing opaque (#1553).
        if opaque_call && bracketing {
            self.bracket_opaque_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let shell_probe = (opaque_call && !bracketing)
            .then(|| crate::shell_touch::WorkspaceProbe::capture(&self.root));

        let mut output = match tool {
            Some(tool) => tool.execute(input, &self.root).await,
            None => ToolOutput::Error {
                message: format!(
                    "unknown tool `{name}` — available: {}",
                    self.available_names()
                ),
            },
        };
        if let Some(bus) = &bus {
            let duration_ms = started_at.elapsed().as_millis() as u64;
            // The command line the tool actually ran: bash's own input, an
            // explicit build/test command, verify_done's test_cmd,
            // start_process's joined argv, or the index-resolved command
            // (see [`Self::command_line_for`]).
            let command_line = Self::command_line_for(name, input, resolved_command.as_deref());
            match &output {
                ToolOutput::Error { message } => {
                    bus.emit_named(
                        hook_names::TOOL_CALL_FAILED,
                        serde_json::json!({
                            "tool": name, "error": message, "duration_ms": duration_ms,
                        }),
                    );
                    if let Some(command) = command_line.as_deref() {
                        bus.emit_named(
                            hook_names::COMMAND_FAILED,
                            serde_json::json!({ "command": command, "error": message }),
                        );
                    }
                }
                ToolOutput::Ok { .. } => {
                    bus.emit_named(
                        hook_names::TOOL_CALL_COMPLETED,
                        serde_json::json!({ "tool": name, "duration_ms": duration_ms }),
                    );
                    if let Some(command) = command_line.as_deref() {
                        bus.emit_named(
                            hook_names::COMMAND_COMPLETED,
                            serde_json::json!({ "command": command }),
                        );
                    }
                }
            }
        }
        // Attribute what the opaque call actually did. Deliberately OUTSIDE
        // the success gate below: a shell command that exits non-zero has
        // very often still written something (a build that failed halfway, a
        // patch that applied two hunks of three), and the ledger's job is to
        // describe the tree as it is, not as the exit code implies.
        if let Some(before) = shell_probe {
            let after = crate::shell_touch::WorkspaceProbe::capture(&self.root);
            self.record_probe_delta(
                &before,
                &after,
                name,
                &serde_json::json!({ "reason": format!("attributed to `{name}`") }),
                bus.as_ref(),
            );
        }
        if !output.is_error() {
            // The write landed, so the objects it creates now exist: grow
            // the session overlay so a duplicate later this session
            // conflicts even before any re-index from the code graph. One
            // pass per gated file — a batch records each (#442). A `dry_run`
            // batch returns Ok but writes NOTHING, so its passes must not
            // touch the overlay (or the manifest via a declared intent) — a
            // later real write of the same object would be falsely rejected
            // as a duplicate of a phantom.
            let dry_run = name == "apply_edits"
                && input
                    .get("dry_run")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
            if !dry_run {
                for pass in &pending_storage {
                    self.record_storage_objects(pass, declared_intent.as_deref());
                }
            }
            for (pending, pre_content) in pending_ops.into_iter().zip(pre_contents) {
                self.record_touch(pending, pre_content, name, input, bus.as_ref());
            }
            // `read_symbol` resolves its file through the code graph
            // mid-execution, so its `R` event cannot be classified from the
            // input up front like `read_file`'s: the tool recorded each
            // resolved path and the drain here lands the same
            // one-R-event-per-successful-read in the ledger.
            if name == "read_symbol" {
                let resolved: Vec<String> =
                    std::mem::take(&mut *self.span_reads.lock().unwrap_or_else(|p| p.into_inner()));
                for path in resolved {
                    // `normalize_workspace_path` runs the confinement check
                    // itself, so an escaping span path drops out here.
                    if let Some(normalized) = normalize_workspace_path(&self.root, &path) {
                        self.record_touch(
                            PendingTouch {
                                path: normalized,
                                op: FileOp::Read,
                            },
                            None,
                            name,
                            input,
                            bus.as_ref(),
                        );
                    }
                }
            }
            // A new/updated map changes what is covered — rebuild (also
            // resets nothing: the hinted set survives via re-insertion
            // below being per-slice, and a freshly saved map needs no hint
            // for its own author).
            if name == "save_exploration" {
                let mut refreshed = ExplorationCoverage::rebuild(&self.root);
                let mut coverage = self
                    .exploration_coverage
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                refreshed.hinted = std::mem::take(&mut coverage.hinted);
                // The author of a map never needs a hint about it.
                if let Some(slice) = input.get("slice").and_then(|v| v.as_str()) {
                    refreshed.hinted.insert(slice.to_string());
                }
                *coverage = refreshed;
            }
        }

        // Coverage hints (spec §6b): when a search/read touches territory a
        // FRESH map covers, say so once per (session, slice) — meeting the
        // grep habit where it lives. Appended to the result text, outside
        // the cached prompt prefix, so the hint costs no cache stability.
        if matches!(
            name,
            "grep" | "glob" | "read_file" | "read_symbol" | "graph_query"
        ) && let ToolOutput::Ok { content } = &mut output
        {
            let mut haystack = String::new();
            for key in ["path", "target", "pattern"] {
                if let Some(v) = input.get(key).and_then(|v| v.as_str()) {
                    haystack.push_str(v);
                    haystack.push('\n');
                }
            }
            // Cap the scan window (char-boundary-safe) — a huge grep dump
            // is already summarizing many files; the head names them.
            let mut cap = content.len().min(20_000);
            while cap < content.len() && !content.is_char_boundary(cap) {
                cap += 1;
            }
            haystack.push_str(&content[..cap]);
            let mut fresh_hits: Vec<String> = {
                let mut coverage = self
                    .exploration_coverage
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                let mut hits: Vec<String> = coverage
                    .by_path
                    .iter()
                    .filter(|(path, _)| mentions_path(&haystack, path))
                    .flat_map(|(_, slices)| slices.iter().cloned())
                    .filter(|slice| !coverage.hinted.contains(slice))
                    .collect();
                hits.sort();
                hits.dedup();
                hits.truncate(3);
                for slice in &hits {
                    coverage.hinted.insert(slice.clone());
                }
                hits
            };
            for slice in fresh_hits.drain(..) {
                content.push_str(&format!(
                    "\n\nnote: saved exploration `{slice}` covers this area (fresh) — \
                     explorations({{\"slice\": \"{slice}\"}}) may already answer this"
                ));
            }
        }
        output
    }

    /// Run the `tool.call.requested` blocking chain. `Ok(None)`: allowed
    /// unchanged. `Ok(Some(input))`: allowed with a policy-modified input.
    /// `Err(output)`: denied or pending approval — the error the model sees
    /// instead of a tool result. The chain receives the RAW input (the
    /// interception point is privileged by design); observable events carry
    /// only sanitized inputs.
    fn gate_tool_call(
        &self,
        bus: &HookBus,
        name: &str,
        input: &Value,
    ) -> Result<Option<Value>, ToolOutput> {
        let outcome = bus.emit_blocking(HookEventDraft::new(
            hook_names::TOOL_CALL_REQUESTED,
            serde_json::json!({ "tool": name, "input": input }),
        ));
        match outcome.decision {
            HookDecision::Deny { reason } => Err(ToolOutput::Error {
                message: format!("`{name}` was denied by an extension policy: {reason}"),
            }),
            HookDecision::RequireApproval { reason } => Err(ToolOutput::Error {
                message: format!("`{name}` requires approval before it can run: {reason}"),
            }),
            _ => {
                if !outcome.modified {
                    return Ok(None);
                }
                match outcome.event.payload.get("input") {
                    Some(new_input) => Ok(Some(new_input.clone())),
                    None => {
                        // A `modify` that dropped the `input` field is a
                        // broken policy handler — surface it, keep the
                        // original input rather than executing garbage.
                        bus.emit_named(
                            hook_names::EXTENSION_ERROR,
                            serde_json::json!({
                                "failed_event": hook_names::TOOL_CALL_REQUESTED,
                                "error": "modify decision dropped the `input` field; original input kept",
                            }),
                        );
                        Ok(None)
                    }
                }
            }
        }
    }

    /// Run the per-side-effect blocking chains and payload-hygiene
    /// detectors for one already-final input: `sensitive_operation.detected`
    /// and `secret.detected` observers first (an auditor sees the attempt
    /// even when a policy then denies it), then the `file.*` chain for a
    /// classified C/U/D op and the `command.started` chain for every tool
    /// that runs a command — `bash`, `build_project`, `run_tests`,
    /// `verify_done`, `run_script`, `start_process`, `send_stdin`, and every
    /// [`Tool::command_for_gate`] implementor: `run_lint`, `format_code`,
    /// `diagnostics`, `screenshot`, `ci_status`, `start_work_on_issue`, and
    /// the `repo_*` family (#804; see [`Self::command_line_for`]).
    /// These chains gate — `modify` decisions are recorded but not honored
    /// here, because the input was already final after
    /// `tool.call.requested`. Reads are observable but never interceptable.
    fn gate_side_effects(
        &self,
        bus: &HookBus,
        name: &str,
        input: &Value,
        pending_ops: &[PendingTouch],
        resolved_command: Option<&str>,
    ) -> Result<(), ToolOutput> {
        for pending in pending_ops {
            if bus::is_sensitive_path(&pending.path) {
                bus.emit_named(
                    hook_names::SENSITIVE_OPERATION_DETECTED,
                    serde_json::json!({
                        "path": pending.path,
                        "tool": name,
                        "operation": pending.op.letter().to_string(),
                    }),
                );
            }
            // The content this call proposes to land in THIS file: write's
            // whole content, edit's replacement, or — for a batch — the
            // union of the batch's replacements targeting this path.
            let proposed: Option<String> = match name {
                "write_file" => input
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                "edit_file" => input
                    .get("new_string")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                "apply_edits" => input.get("edits").and_then(|v| v.as_array()).map(|edits| {
                    edits
                        .iter()
                        .filter(|e| {
                            e.get("path")
                                .and_then(|v| v.as_str())
                                .and_then(|p| normalize_workspace_path(&self.root, p))
                                .is_some_and(|p| p == pending.path)
                        })
                        .filter_map(|e| e.get("new_string").and_then(|v| v.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n")
                }),
                _ => None,
            };
            if let Some(proposed) = proposed.as_deref() {
                let kinds = bus::scan_for_secrets(proposed);
                if !kinds.is_empty() {
                    bus.emit_named(
                        hook_names::SECRET_DETECTED,
                        serde_json::json!({
                            "path": pending.path,
                            "kinds": kinds.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
                        }),
                    );
                }
            }
            let event_name = match pending.op {
                FileOp::Create => Some(hook_names::FILE_CREATED),
                FileOp::Update => Some(hook_names::FILE_UPDATED),
                FileOp::Delete => Some(hook_names::FILE_DELETED),
                FileOp::Read => None,
            };
            if let Some(event_name) = event_name {
                let outcome = bus.emit_blocking(HookEventDraft::new(
                    event_name,
                    serde_json::json!({
                        "path": pending.path,
                        "tool": name,
                        "operation": pending.op.letter().to_string(),
                    }),
                ));
                match outcome.decision {
                    HookDecision::Deny { reason } => {
                        return Err(ToolOutput::Error {
                            message: format!(
                                "`{name}` on `{}` was denied by an extension policy: {reason}",
                                pending.path
                            ),
                        });
                    }
                    HookDecision::RequireApproval { reason } => {
                        return Err(ToolOutput::Error {
                            message: format!(
                                "`{name}` on `{}` requires approval: {reason}",
                                pending.path
                            ),
                        });
                    }
                    _ => {}
                }
            }
        }
        let command_line = Self::command_line_for(name, input, resolved_command);
        if let Some(command) = command_line.as_deref() {
            let outcome = bus.emit_blocking(HookEventDraft::new(
                hook_names::COMMAND_STARTED,
                serde_json::json!({ "command": command }),
            ));
            match outcome.decision {
                HookDecision::Deny { reason } => {
                    return Err(ToolOutput::Error {
                        message: format!("command was denied by an extension policy: {reason}"),
                    });
                }
                HookDecision::RequireApproval { reason } => {
                    return Err(ToolOutput::Error {
                        message: format!("command requires approval: {reason}"),
                    });
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Merge the session overlay into a freshly loaded persisted storage map.
    ///
    /// The other half of the old `storage_snapshot` — the persisted load
    /// (`crate::graph::load_storage_snapshot`, which opens SQLite and reads
    /// the storage rows plus the manifest) — is now the caller's, so an async
    /// caller can put it on the blocking pool (#549). This half is pure
    /// in-memory merging and holds a `std::sync::Mutex` guard, so it must
    /// stay synchronous and away from any await.
    fn merge_storage_overlay(
        &self,
        mut snapshot: stella_graph::StorageSnapshot,
    ) -> stella_graph::StorageSnapshot {
        let index = self.storage_index.lock().unwrap_or_else(|p| p.into_inner());
        for rel in index.baseline.relations.iter().chain(index.session.iter()) {
            if let Some(existing) = snapshot
                .relations
                .iter_mut()
                .find(|r| r.address == rel.address)
            {
                // Same relation known from disk: union in any fields the
                // overlay knows about that the index hasn't caught up on.
                for field in &rel.fields {
                    let key = stella_graph::storage::normalize_name(&field.name);
                    if !existing
                        .fields
                        .iter()
                        .any(|f| stella_graph::storage::normalize_name(&f.name) == key)
                    {
                        existing.fields.push(field.clone());
                    }
                }
            } else {
                snapshot.relations.push(rel.clone());
            }
        }
        snapshot
    }

    /// Record what a landed write created: grow the session overlay, and —
    /// when the model declared a `storage_intent` — append it to
    /// `stella.storage.toml` (origin `declared`). The gate's justification
    /// path is what populates the map: every challenged object is born
    /// documented (spec §8 ring 3).
    fn record_storage_objects(&self, pass: &crate::schema_gate::GatePass, intent: Option<&str>) {
        if let Some(intent) = intent {
            for address in &pass.intent_addresses {
                let _ = stella_graph::manifest::append_meaning(
                    &self.root,
                    "relations",
                    address,
                    intent,
                    "declared",
                );
            }
        }
        if pass.created.is_empty() {
            return;
        }
        let mut index = self.storage_index.lock().unwrap_or_else(|p| p.into_inner());
        index.session.extend(pass.created.iter().cloned());
    }

    /// The effective command line of one tool call, feeding both the
    /// blocking `command.started` policy chain and the `command.*`
    /// observer events: `bash`'s own input, `build_project`'s/`run_tests`'s
    /// `command` override or index-composed runner line, `verify_done`'s
    /// `test_cmd`, `run_script`'s index-resolved command, or
    /// `start_process`'s space-joined argv. `resolved_command` is the
    /// index-composed line for the three index-backed tools (`None` when
    /// resolution failed — the tool returns the named error itself,
    /// ungated).
    ///
    /// Every tool that reaches `bash -c` MUST ride the same fence as `bash`:
    /// they stay in the surface when an operator sets `"bash": "off"` (and
    /// `start_process`'s argv[0] may itself be a shell, `["bash", "-c", …]`),
    /// so leaving any out hands ambient shell execution to the very posture
    /// that turned `bash` off. The #615 known gap is closed (#804): the
    /// `bash -c` composers (`screenshot`, `ci_status`, `start_work_on_issue`)
    /// and the argv-exec default tools (`run_lint`, `format_code`,
    /// `diagnostics`, the `repo_*` family) each report the line they would
    /// run via [`Tool::command_for_gate`], which lands here as
    /// `resolved_command` through the catch-all arm — so a policy denying
    /// command execution sees every command the session runs.
    ///
    /// That covers tools which *compose* a command line. `send_stdin` does
    /// not — it writes into an interpreter someone already started — but the
    /// bytes it writes are commands to that interpreter, so it rides the fence
    /// too. Gating only the spawn meant `start_process(["bash", "-i"])` was
    /// fenced once, on that one argv, and every command subsequently pushed
    /// into the live shell executed with no policy consultation and left no
    /// `command.*` audit record at all. An auditor reconstructing what ran
    /// from the command stream saw `bash -i` and nothing after it.
    ///
    /// A REPL fed data rather than commands is gated here too. That direction
    /// is the safe one: the fence is opt-in per operator, and showing a policy
    /// more than it needs beats showing it nothing.
    fn command_line_for(
        name: &str,
        input: &Value,
        resolved_command: Option<&str>,
    ) -> Option<String> {
        match name {
            "bash" => Some(
                input
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ),
            // The override wins exactly as it does at execute time; without
            // one the gate sees the command composed from the scripts
            // index. A missing/non-string field on either path is `None`:
            // ungated, and the tool returns its own named error.
            "build_project" | "run_tests" => input
                .get("command")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| resolved_command.map(str::to_string)),
            "verify_done" => input
                .get("test_cmd")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            "start_process" => Some(
                input
                    .get("argv")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default(),
            ),
            // The text is what the live interpreter will execute, so it is
            // what the policy chain and the `command.*` audit trail see. A
            // missing/non-string `text` is `None` — ungated, and the tool
            // returns its own named error, exactly as the other arms behave.
            "send_stdin" => input
                .get("text")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            // Everything else rides on what it resolved for itself —
            // `run_script`'s threaded index resolution and every
            // [`Tool::command_for_gate`] implementor. `None` (no resolution,
            // or a tool that runs no command) stays ungated.
            _ => resolved_command.map(str::to_string),
        }
    }

    /// This session's staleness map as JSON, for a durable host to persist.
    ///
    /// `BTreeMap`, so the bytes are a pure function of the contents: this is
    /// written into a content-addressed store on every step boundary, and a
    /// `HashMap`'s iteration order would mint a fresh object each time for a
    /// map that had not changed.
    ///
    /// `None` when nothing has been observed yet — there is no state to write,
    /// and an empty object would still cost a commit.
    #[must_use]
    pub fn observed_snapshot(&self) -> Option<String> {
        let observed = self.observed.lock().unwrap_or_else(|p| p.into_inner());
        if observed.is_empty() {
            return None;
        }
        let ordered: std::collections::BTreeMap<&str, String> = observed
            .iter()
            .map(|(k, v)| (k.as_str(), v.encode()))
            .collect();
        serde_json::to_string(&ordered).ok()
    }

    /// Restore a staleness map persisted by [`Self::observed_snapshot`],
    /// returning how many paths came back.
    ///
    /// Restoring is what makes the no-clobber guarantee survive a crash. Without
    /// it a resumed session is *less* safe than a fresh one is honest: the
    /// restored transcript still says the agent read a file, so the model acts
    /// on content it believes it knows, while the guard — having forgotten it
    /// was ever read — waves the overwrite through. That is the one combination
    /// the guard exists to prevent.
    ///
    /// Replaces rather than merges: the restored map is this session's
    /// complete belief about the tree, so a departing session's observations
    /// must not survive into it (the body says why in full).
    pub fn restore_observed(&self, json: &str) -> usize {
        let Ok(restored) = serde_json::from_str::<HashMap<String, String>>(json) else {
            return 0;
        };
        let restored: HashMap<String, Belief> = restored
            .into_iter()
            .map(|(path, encoded)| (path, Belief::decode(&encoded)))
            .collect();
        let mut observed = self.observed.lock().unwrap_or_else(|p| p.into_inner());
        // REPLACE, never merge. The restored map is this session's complete
        // belief about the tree, and the deck reuses one registry across a
        // session switch — so merging would leave the departing session's
        // observations in place and refuse the arriving session's writes to
        // files it never read. `or_insert` compounded it by preferring the
        // stale entry when both sessions had seen the same path, making a
        // disagreement unresolvable in the wrong direction. A guard built to
        // have no false positives cannot inherit another session's memory.
        let count = restored.len();
        *observed = restored;
        count
    }

    /// The paths in `pending` this call would clobber — ones this session has
    /// seen before whose bytes on disk no longer match what it saw.
    ///
    /// Only `Update` and `Delete` can clobber. A `Create` is checked by the
    /// filesystem itself, and a `Read` changes nothing. A path this session
    /// has never observed is NOT flagged: this guard's claim is "you are about
    /// to overwrite a change you never saw", and it can only make that claim
    /// about a file it watched the agent look at. Blind writes to never-read
    /// files are a separate policy question and are deliberately left alone
    /// here, so this check has no false positives.
    ///
    /// An unreadable file is not a conflict either — the tool's own error
    /// path reports that far better than a staleness message would.
    ///
    /// This is the digest half of the guard only. A file whose bytes match but
    /// which the agent has only ever glimpsed is the *other* half, decided by
    /// coverage rather than by hashing — see
    /// [`Self::refused_mutation`], which calls this one first.
    fn clobbered_paths(&self, pending: &[PendingTouch]) -> Vec<String> {
        let observed = self.observed.lock().unwrap_or_else(|p| p.into_inner());
        pending
            .iter()
            .filter(|p| matches!(p.op, FileOp::Update | FileOp::Delete))
            .filter(|p| {
                observed.get(&p.path).is_some_and(|seen| {
                    crate::rootfd::read_confined_bytes(&self.root, &p.path)
                        .is_some_and(|bytes| crate::staleness::hex_sha256(&bytes) != seen.sha256)
                })
            })
            .map(|p| p.path.clone())
            .collect()
    }

    /// Bind this session's durable record, replacing any earlier binding.
    ///
    /// Re-binding is how a host whose registry outlives one session — the deck,
    /// across a session switch — keeps the record attributed to the session
    /// that is actually running. Mutations before the call land on the previous
    /// session's ref and stay there; nothing already committed moves.
    ///
    /// `agent` labels the commits, so a workspace worked by several agents
    /// reads back as an attributable log rather than an anonymous one.
    pub fn attach_work_journal(
        &self,
        journal: stella_store::work_journal::WorkJournal,
        agent: impl Into<String>,
    ) {
        let mut slot = self.work_journal.write().unwrap_or_else(|p| p.into_inner());
        *slot = Some((journal, agent.into()));
    }

    /// Commit one mutation to the session's durable record.
    ///
    /// Best-effort by the same reasoning as the checkpoint sink: a journal
    /// that cannot be written leaves the agent exactly as recoverable as it
    /// was before durability existed, so failing a live tool call to report
    /// the loss of a *safety net* would be strictly worse than not having one.
    /// Reads are skipped — nothing changed, and a commit per read would bury
    /// the record of real work.
    fn journal_mutation(&self, pending: &PendingTouch, reason: &str) {
        // Cloned out from under the lock rather than held across the commit:
        // `record` spawns git, and a read guard held across a subprocess would
        // park a concurrent re-bind for the length of it. The clone is three
        // paths and a string.
        let Some((journal, agent)) = self
            .work_journal
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
        else {
            return;
        };
        let verb = match pending.op {
            FileOp::Read => return,
            FileOp::Create => "create",
            FileOp::Update => "update",
            FileOp::Delete => "delete",
        };
        let _ = journal.record(
            std::slice::from_ref(&pending.path),
            &[],
            &format!("stella({agent}): {verb} {} — {reason}", pending.path),
        );
    }

    /// Record one SUCCESSFUL file touch: derive the event's line delta per
    /// the counting rules (R: zero; C: full new line count; U: line diff of
    /// pre vs post content; D: full pre-deletion line count), attach the
    /// model-supplied `reason` (or a per-op default), and append it to the
    /// ledger. The append and its aggregate updates happen under one lock,
    /// so concurrent tool completions never lose or interleave an event.
    ///
    /// With a bus attached, the touch also emits its `file.*` fact event
    /// (path + reason + line deltas, never contents) and a
    /// `files_touched.updated` carrying the changed record plus the ledger
    /// `revision` assigned under the lock — concurrent touches may deliver
    /// out of order, but consumers keep the highest revision and stay
    /// consistent.
    fn record_touch(
        &self,
        pending: PendingTouch,
        pre_content: Option<String>,
        tool: &str,
        input: &Value,
        bus: Option<&HookBus>,
    ) {
        // Refresh this session's belief about the file before anything else in
        // here can fail or return early: the ledger is telemetry, but this is
        // the guarantee, and a touch whose digest went unrecorded would leave
        // the NEXT write to this path unguarded.
        //
        // A read earns the WHOLE-file grade only if it showed the model every
        // line: `read_file` defaults to a 2000-line window and `read_symbol`
        // reads a span, so the partial case is the common one. A mutation
        // always earns it — the agent authored what it just wrote, so there is
        // no unseen remainder to be wrong about. What a partial read may then
        // do to an existing belief is `remember_observed`'s rule.
        let coverage = if !matches!(pending.op, FileOp::Read)
            || self.read_ledger.saw_whole_file(&self.root, &pending.path)
        {
            Coverage::Whole
        } else {
            Coverage::Partial
        };
        self.remember_observed(&pending.path, coverage);
        // Stella's own state directory is not workspace content: probe-observed
        // churn under `.stella/` (the code-graph DB and its WAL/SHM sidecars)
        // drowned real task edits 60:1 on a single bench trial (#1537). The
        // staleness refresh above still runs — a write there stays guarded —
        // but nothing downstream records, journals, or announces it.
        if crate::file_touch::is_stella_state_path(&pending.path) {
            return;
        }
        let reason = input
            .get("reason")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| crate::file_touch::default_reason(pending.op).to_string());
        // The durable record, written from the same choke point as the ledger
        // and the staleness map, so all three describe one reality. Before the
        // diff work below, which is presentation: if anything here is going to
        // be expensive, the commit that makes the work recoverable goes first.
        self.journal_mutation(&pending, &reason);
        // Counts and rendered diff come out of the SAME pre/post pair, so the
        // ledger, the telemetry payload and the TUI describe one reality.
        let pre = pre_content.as_deref().unwrap_or("");
        // `post` rides alongside the counts so the authored-change ledger can
        // keep BOTH images of this write. It costs nothing extra to produce —
        // every arm below already computes it to render its own diff — and it
        // is the difference between a verifier that can read the agent's change
        // and one that is handed a filename (see [`crate::authored_diff`]).
        let (lines_added, lines_removed, diff, post) = match pending.op {
            FileOp::Read => (0, 0, None, None),
            FileOp::Create => {
                // `write_file` carries the new content in its input. A file
                // created by an opaque call (a shell redirect, a compiler
                // output) carries it only on disk, so fall back to reading
                // it — otherwise every shell-created file would be recorded
                // as zero lines long, understating exactly the work this
                // attribution path exists to see.
                let from_input = input.get("content").and_then(|v| v.as_str());
                let from_disk = from_input.is_none().then(|| {
                    crate::rootfd::read_confined_lossy(&self.root, &pending.path)
                        .unwrap_or_default()
                });
                let content =
                    from_input.unwrap_or_else(|| from_disk.as_deref().unwrap_or_default());
                (
                    count_lines(content),
                    0,
                    crate::file_touch::changed_region_diff("", content),
                    Some(content.to_string()),
                )
            }
            FileOp::Update => {
                // write_file carries the post content in its input; edit_file
                // landed it on disk (mutating tools are never parallelized —
                // see the read_only partition test — so this read-back sees
                // exactly what the edit wrote).
                let post = match tool {
                    "write_file" => input
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    _ => crate::rootfd::read_confined_lossy(&self.root, &pending.path)
                        .unwrap_or_default(),
                };
                let (added, removed) = line_diff(pre, &post);
                let diff = crate::file_touch::changed_region_diff(pre, &post);
                (added, removed, diff, Some(post))
            }
            FileOp::Delete => (
                0,
                count_lines(pre),
                crate::file_touch::changed_region_diff(pre, ""),
                Some(String::new()),
            ),
        };
        // Reads yield `None` above and the ledger refuses them again on its own
        // account — two guards, because this caller has already been the place
        // a read leaked into a change count once.
        if let Some(post) = &post {
            self.authored
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .record(
                    &pending.path,
                    pending.op,
                    crate::authored_diff::Provenance::for_tool(tool),
                    pre,
                    post,
                );
        }
        let path = pending.path;
        let (revision, record, total_files) = {
            let mut touched = self.touched.lock().unwrap_or_else(|p| p.into_inner());
            touched.record(
                path.clone(),
                FileTouchEvent {
                    event: pending.op,
                    reason: reason.clone(),
                    lines_added,
                    lines_removed,
                },
            );
            (
                touched.revision(),
                touched.get(&path).cloned(),
                touched.len(),
            )
        };
        if let Some(bus) = bus {
            let fact = match pending.op {
                FileOp::Read => hook_names::FILE_READ,
                FileOp::Create => hook_names::FILE_CREATED,
                FileOp::Update => hook_names::FILE_UPDATED,
                FileOp::Delete => hook_names::FILE_DELETED,
            };
            bus.emit_named(
                fact,
                serde_json::json!({
                    "path": path,
                    "reason": reason,
                    "lines_added": lines_added,
                    "lines_removed": lines_removed,
                }),
            );
            bus.emit_named(
                hook_names::FILES_TOUCHED_UPDATED,
                serde_json::json!({
                    "revision": revision,
                    "path": path,
                    "record": record,
                    "total_files": total_files,
                }),
            );
        }
        // The one place a `FileChange` is born. A closed channel (the turn
        // ended) is not an error worth surfacing: the ledger above is the
        // durable record, and this is its live projection.
        let kind = match pending.op {
            FileOp::Read => stella_protocol::FileChangeKind::Read,
            FileOp::Create => stella_protocol::FileChangeKind::Created,
            FileOp::Update => stella_protocol::FileChangeKind::Modified,
            FileOp::Delete => stella_protocol::FileChangeKind::Deleted,
        };
        // Counted off the event's OWN `kind`, and unconditionally — the tally
        // must not depend on whether a channel happened to be attached, or a
        // candidate registry (deliberately unattached) would report having
        // touched nothing. Reads are excluded here exactly as every consumer
        // excludes them, so the two agree by construction rather than by two
        // copies of the same `match`.
        if kind.is_mutation() {
            self.mutations
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        // A candidate registry (`attach_read_events`) announces its reads and
        // withholds its mutations: the edits are not the user's tree's until
        // adoption re-emits them. The counter above is deliberately OUTSIDE
        // this gate — `mutations_recorded` must stay true no matter which
        // events were announced, or the verification ladder goes blind exactly
        // where #973 found it blind.
        let announce = !kind.is_mutation()
            || self
                .announce_mutations
                .load(std::sync::atomic::Ordering::Relaxed);
        if announce && let Some(events) = self.events() {
            let _ = events.send(stella_protocol::AgentEvent::FileChange {
                path,
                kind,
                added: lines_added as u32,
                removed: lines_removed as u32,
                diff,
            });
        }
    }

    /// How many **mutating** file touches this registry has recorded since it
    /// was constructed — the count behind every `AgentEvent::FileChange` whose
    /// `kind` is a mutation, taken at the point the event is born.
    ///
    /// Monotonic: callers take a delta across the window they care about (a
    /// turn, a candidate) rather than reading it as an absolute. Unlike
    /// [`Self::files_touched`], re-touching one file counts twice — this
    /// answers "how many changes happened", not "how many files".
    pub fn mutations_recorded(&self) -> u64 {
        self.mutations.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The session's authored change, rendered as a unified diff.
    ///
    /// The companion to [`Self::mutations_recorded`]: that answers *how many*
    /// changes happened, this answers *what they were*. Both are read by the
    /// verification pipeline, and this one exists because the channel it
    /// previously read — `git diff` — cannot answer at all in the two
    /// workspaces that matter most (no repository; a newly created file).
    #[must_use]
    pub fn authored_diff(&self) -> crate::authored_diff::AuthoredDiff {
        self.authored
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .render()
    }

    /// Snapshot of every file touched this session, insertion-ordered,
    /// with its ops as a compact CRUD string (e.g. `"RU"`).
    pub fn files_touched(&self) -> Vec<(String, String)> {
        self.touched
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .compact()
    }

    /// The full session file-touch telemetry payload: per normalized path,
    /// deduplicated CRUD events (first-occurrence order), line-delta totals,
    /// and the complete ordered audit log.
    pub fn file_touch_telemetry(&self) -> FileTouchTelemetry {
        self.touched
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .snapshot()
    }

    /// Drain the memory citations the `cite_memory` tool recorded since the
    /// last drain. Draining (unlike the cumulative file-touch snapshot) is
    /// what lets the CLI persist each citation under exactly one execution —
    /// re-persisting under later executions would inflate the promotion
    /// eligibility count.
    pub fn take_memory_citations(&self) -> Vec<crate::memory::MemoryCitation> {
        std::mem::take(&mut *self.citations.lock().unwrap_or_else(|p| p.into_inner()))
    }

    /// A clone of the session task-board handle, shared with the registered
    /// `task_*` tool instances — the CLI snapshots it into
    /// `AgentEvent::TaskUpdate` after each execution.
    pub fn task_board(&self) -> crate::tasks::TaskBoardHandle {
        self.task_board.clone()
    }

    /// A clone of the `task_assign` spawn-queue handle — for hosts that want
    /// to observe queued spawns without draining them.
    pub fn spawn_queue(&self) -> crate::tasks::SpawnQueue {
        self.spawn_queue.clone()
    }

    /// Drain the sub-agent spawn requests `task_assign` queued since the
    /// last drain — the session driver calls this exactly once per
    /// execution and dispatches each request through the fleet seam, so
    /// (like [`ToolRegistry::take_memory_citations`]) no request is ever
    /// handled twice.
    pub fn take_spawn_requests(&self) -> Vec<stella_core::tasks::SpawnRequest> {
        std::mem::take(&mut *self.spawn_queue.lock().unwrap_or_else(|p| p.into_inner()))
    }

    /// Record one invocation of an installed agent definition (see
    /// [`crate::agent_use`]). `version` is the definition's pinned version at
    /// invocation time; `reason` is a short free-text why/how (may be empty).
    pub fn record_agent_use(&self, agent: &str, version: u32, reason: &str) {
        self.agent_uses
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record(crate::agent_use::AgentUseEvent {
                agent: agent.to_string(),
                version,
                reason: reason.to_string(),
            });
    }

    /// Take every agent invocation recorded since the last drain — the
    /// per-execution persistence step calls this exactly once per execution
    /// so each invocation lands in the store attributed to the execution it
    /// happened under.
    pub fn drain_agent_uses(&self) -> Vec<crate::agent_use::AgentUseEvent> {
        self.agent_uses
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain()
    }

    /// A clone of the MCP usage-ledger handle, for handing to the `McpToolSet`
    /// at connect so its successful calls are recorded against this registry's
    /// ledger (which [`ToolRegistry::take_mcp_usage`] later drains).
    pub fn mcp_usage_ledger(&self) -> stella_core::mcp_usage::McpUsageLedger {
        self.mcp_usage.clone()
    }

    /// Drain the MCP tool calls recorded since the last drain — the counterpart
    /// of [`ToolRegistry::take_memory_citations`], persisted under exactly one
    /// execution id so per-call counts never inflate.
    pub fn take_mcp_usage(&self) -> Vec<stella_core::mcp_usage::McpUsageRecord> {
        stella_core::mcp_usage::drain_usage(&self.mcp_usage)
    }

    /// Comma-separated sorted list of registered tool names, for error
    /// messages.
    pub fn available_names(&self) -> String {
        let late = self.late_tools.read().unwrap_or_else(|p| p.into_inner());
        let mut names: Vec<String> = self
            .tools
            .keys()
            .cloned()
            .chain(late.keys().cloned())
            .collect();
        names.sort();
        names.join(", ")
    }

    /// The workspace root this registry resolves paths against.
    pub fn root(&self) -> &PathBuf {
        &self.root
    }

    /// Seed the gate's baseline storage map. Called at session start with
    /// the assembled snapshot; the gate additionally re-reads the persisted
    /// index on every gated write, so the baseline mainly serves hosts and
    /// tests that inject a map without an on-disk index.
    pub fn update_storage_index(&self, snapshot: stella_graph::StorageSnapshot) {
        let mut index = self.storage_index.lock().unwrap_or_else(|p| p.into_inner());
        index.baseline = snapshot;
    }
}

/// True when `haystack` mentions `path` as a whole path token: the hit may
/// not extend into path characters on either side, so a map covering
/// `lib.rs` never fires on `mylib.rs` or `graphlib.rs` — a false positive
/// would permanently consume that map's once-per-session coverage hint.
fn mentions_path(haystack: &str, path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let is_path_char = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/');
    let mut from = 0;
    while let Some(pos) = haystack[from..].find(path) {
        let start = from + pos;
        let end = start + path.len();
        // A preceding `/` is a component boundary, not an embedding: search
        // results routinely print the workspace-relative map path with an
        // absolute prefix (`/tmp/ws/covered.rs` covers `covered.rs`).
        let clear_before = !haystack[..start]
            .chars()
            .next_back()
            .is_some_and(|c| is_path_char(c) && c != '/');
        let clear_after = !haystack[end..].chars().next().is_some_and(is_path_char);
        if clear_before && clear_after {
            return true;
        }
        from = end;
    }
    false
}

/// `ToolRegistry` is the production implementation of `stella-core`'s
/// `ToolExecutor` port — the engine drives every tool call through this
/// impl, never through `stella-tools` types directly.
#[async_trait]
impl ToolExecutor for ToolRegistry {
    fn schemas(&self) -> Vec<ToolSchema> {
        ToolRegistry::schemas(self)
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        ToolRegistry::execute(self, name, input).await
    }

    /// Take what the `task` tool's children cost since the last step
    /// boundary. Destructive by the port's contract: the engine charges
    /// whatever this returns, so reporting twice would bill twice.
    fn drain_sub_agent_spend_usd(&self) -> f64 {
        stella_core::subagent::drain_sub_agent_spend(&self.sub_agent_spend)
    }

    /// Aggregate each registered tool's [`Tool::parallel_safe`] claim for the
    /// engine's dispatch grouping. Consults the primary map and the
    /// late-enabled overlay — the same two sources every other read path
    /// (`schemas`, dispatch) reads, so a late-enabled tool's claim cannot be
    /// lost.
    fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
        let mut names: std::collections::HashSet<String> = self
            .tools
            .iter()
            .filter(|(_, tool)| tool.parallel_safe())
            .map(|(name, _)| name.clone())
            .collect();
        if let Ok(late) = self.late_tools.read() {
            names.extend(
                late.iter()
                    .filter(|(_, tool)| tool.parallel_safe())
                    .map(|(name, _)| name.clone()),
            );
        }
        names
    }
}

#[cfg(test)]
mod tests;
