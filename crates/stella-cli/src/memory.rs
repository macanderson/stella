//! The self-improvement loop (user requirement): after every turn that did
//! real work — chat, `run`, `goal`, and the Command Deck alike, on success
//! AND on failure — the agent reflects on its own performance and records
//! improvement memories; before every turn, relevant memories and skills are
//! recalled into context; and when a lesson recurs enough times it is
//! automatically promoted to a durable skill (`.stella/skills/<slug>/SKILL.md`).
//! A failed turn is the highest-value learning signal, so it gets a
//! root-cause "why did this fail" reflection prompt (see [`reflect_on_turn`]).
//!
//! Data flow per turn:
//!
//! ```text
//! prompt ──> recall_block_reported(): registry-routed recall (crate::contextgraph) + select_skills()
//!            └─ volatile message AFTER the byte-stable system prefix (L-E8)
//! turn runs …
//! outcome ─> reflect_routed(): the turn failed, or its ledger recorded friction? (#2155)
//!            └─ if so, one model call on the cheap (triage) tier -> 0-3 lessons
//!            ├─ MemoryInput::reflection(...) -> context.db (domain-tagged)
//!            ├─ appended to .stella/private/reflections.jsonl (the mining log)
//!            └─ mine_skill_candidates over the log -> decide_auto_creation
//!               -> new SKILL.md files (capped per session, no-clobber)
//! ```
//!
//! Everything here is best-effort by contract: a failed reflection, a
//! malformed store, or a broken skills dir must NEVER fail or slow the
//! user's actual turn — degraded means "no memory this turn", not an error.

use std::path::{Path, PathBuf};

use colored::Colorize;
use serde::{Deserialize, Serialize};
use stella_context::{
    Clock, ContextDelta, ContextStore, DomainInput, EpisodeInput, EpisodeOutcome, FactAssertion,
    HashEmbedder, NodeInput, NodeKind, RecallTier, SystemClock, format_rfc3339,
};
use stella_core::skills::{self, SelectionConfig, Skill};

use crate::domains::Domains;

// Reached only through the glob import in the test modules
// (`memory/tests.rs` and its children): the code that used these moved to
// `memory/recall.rs` and `memory/learning.rs`, the tests deliberately did
// not.
#[cfg(test)]
use stella_context::MemoryInput;
#[cfg(test)]
use stella_protocol::{CompletionMessage, ContextRecallPort, MessageRole, RecalledFrame};

// Which files a memory is about — shared by the reflection write path and by
// `stella memory validate`, which must agree on what counts as an anchor.
pub(crate) mod anchors;
// #1067/#1068: the durable evidence behind the measured skill promote/retire
// gate — the held-candidate queue, the selection→outcome join, and the
// verdicts the creation gate reads.
pub(crate) mod appraisals;
// The policy half: which anchors have gone stale, and recording that they did.
pub(crate) mod anchor_scan;
// Phase 4 (#715): the citation and tool-outcome evidence sources, which turn
// explicit citation from *the* evidence source into one of several.
mod evidence;
mod learning;
// Phase 3 (#714): typed observation extraction and proposal induction
// into the lifecycle ledger.
pub(crate) mod observations;
mod private_state;
mod projection;
pub(crate) mod proposals;
mod recall;
mod records_refresh;
// #2304: the trace-replay learning harness — the learning machinery driven from
// recorded traces with zero model calls. Test-only by construction: the crate is
// bin-only, so an in-crate module is the only place this can reach
// `SessionMemory` at all (`doc:trace-replay-learning-harness` §4).
#[cfg(test)]
pub(crate) mod replay;
// Phase 4 (#715): reversible retirement of context that stops helping.
pub(crate) mod retirement;
pub(crate) mod rules_mining;
pub(crate) mod self_tuning;
pub(crate) mod skill_files;
// #3349: the SteeringPlane implementation — the frame adapter and the one
// packing pass behind `recall_block_reported`.
mod steering;
/// Reachable by name only from tests: production drives the adapter through
/// [`requery_for_turn`], which cannot forget to wire its telemetry (#3366).
#[cfg(test)]
pub(crate) use steering::SessionRequery;
pub(crate) use steering::requery_for_turn;
mod suppression;
pub(crate) mod tuning;
// Phase 4 (#715): context-use extraction — what a finished turn's frame
// carried, and what the turn then said about it.
pub(crate) mod uses;
// #753: deterministic validation — the first pruning-eligible evidence source,
// so the retirement sweep fires without a human.
pub(crate) mod validation;
use private_state::resolve_context_db_path;
#[cfg(test)]
use projection::{is_suppressed_local_frame, project_recalled_frame};
// Every driver reaches the turn through `inject_opening_recall` below, which
// carries this turn's skill scopes and re-query seed as well as its text —
// so the bare injection has no caller left outside the tests that pin its own
// marker rule.
#[cfg(test)]
pub(crate) use recall::inject_recall_block;
pub(crate) use recall::{OpeningRecall, inject_opening_recall};
/// Test-side imports of the recall renderer's internals — one renderer, so
/// every consumer of a recalled frame reads exactly the same rendering,
/// `[nod_…]` citation handles included.
#[cfg(test)]
use recall::{
    ab_control_turn, goal_path_anchors, render_context_section, render_today_section,
    turn_path_tokens,
};
#[cfg(test)]
pub(crate) use skill_files::load_workspace_skills;
pub(crate) use skill_files::{
    load_workspace_skills_with_authority, skill_paths_on_disk, workspace_skills_dir,
};
// Phase 2 (#713): the engine-config builder reads the lifecycle switch through
// here, so exactly one place in the crate resolves a `context.*` sub-block.
pub use tuning::session_lifecycle_enabled;

/// Marker prefixing a recalled-context message so
/// [`recall::inject_recall_block`] can find the newest one for dedup. Blocks
/// land at the conversation tail and stay in place as durable history (L-E8:
/// the byte-stable prefix — system prompt AND replayed turns — is never
/// rewritten, which is what preserves prompt-cache hits).
///
/// Phase 2 (#713) moved the definition to `stella-core`, where receipt
/// decomposition reads it to recognize a recall block. This is a re-export,
/// not a second copy: two spellings of one marker is a decomposition that
/// silently stops firing the day either is edited.
pub use stella_core::receipts::RECALL_MARKER;

/// What an A/B control turn's episode summary carries so the two arms can be
/// told apart offline (#1221). Appended by [`SessionMemory::record_episode`] —
/// the one place every surface's episode is written — because attribution that
/// each driver has to remember is attribution that a new driver silently drops.
/// There is no reporting command yet: the comparison is a query over episode
/// summaries, which is what makes the exact spelling essential.
pub(crate) const AB_CONTROL_TAG: &str = " [ab-control]";

/// One reflection lesson as the model returns it and as persisted to the
/// mining log (`.stella/private/reflections.jsonl`, one JSON object per line).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionLesson {
    pub lesson: String,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub occurred_at: u64,
    /// Phase 3 (#714): the task this lesson belongs to, for the distinct-task
    /// counting spec §7 requires. `#[serde(default)]` so every log line written
    /// before this field existed still parses; when empty, extraction falls
    /// back to the turn (see `memory::observations`).
    ///
    /// Stamped by [`SessionMemory::reflect_and_record`] from the session's own
    /// boundary, which is session-scoped unless a caller that knows better set
    /// one — a fleet attempt writes `fleet:<task id>` here (#3989). See
    /// [`SessionMemory::set_task_id`] for what collides with what.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub task_id: String,
    /// When this lesson applies — the condition a future task has to satisfy
    /// for it to be worth reading.
    ///
    /// Asked for because a lesson whose trigger the model cannot state is one
    /// it has not finished learning: it has noticed something without working
    /// out what the something is evidence *of*. Requiring the condition prunes
    /// those where the transcript is still there to check them against,
    /// instead of leaving them to be judged later by a reader who never saw
    /// the turn.
    ///
    /// Recorded, mined, **and folded into the memory body recall scores on**
    /// (#2459) — which is where it earns its keep a second time: a trigger is
    /// written in the register a *goal* is written in, so it is the half of a
    /// lesson that lives in the space the retrieval query is asked in.
    ///
    /// The fold is why it cannot be done naively: `partition_known`'s
    /// restatement band (#2358) compares candidate lessons against stored
    /// bodies, and moving that text would move every similarity score in the
    /// comparison at once. `memory::learning::applicability` carries the
    /// encoding that keeps the band's input byte-identical, and the argument
    /// for why folding beat a separate scored field and a post-retrieval
    /// filter.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub trigger: String,
    /// What knowing this would have bought *in the turn that produced it* —
    /// the wrong attempt it prevents, the wait it skips, the wrong answer it
    /// stops being shipped.
    ///
    /// This is the field that makes a lesson checkable. A memory justified
    /// only by the model asserting it would help is the same shape of claim
    /// this project refuses everywhere else: a sentence standing in for
    /// evidence. Naming the moment it would have changed is the cheapest proof
    /// available that a lesson is about the turn's actual cost rather than
    /// about how the turn felt from the inside.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub saves: String,
    /// How far this lesson travels: to any task in this repository, or only to
    /// the turn that produced it.
    ///
    /// The distinction is real and measured — in a run of ten mined lessons,
    /// eight were process self-critique ("the agent should be more
    /// proactive") and **none** captured a repository convention that decided
    /// whether a task passed — and it is what [`LessonKind::recall_tier`]
    /// spends the recall budget on.
    ///
    /// But it used to be *asked* as a question about subject matter ("a fact
    /// about the codebase, or a note about the agent") and *read* as a
    /// question about transfer, and those come apart on the useful cases:
    /// "this repo's integration tests need the fixture server up first" is a
    /// note about how to work, and it is true on every task here. Asking about
    /// transfer directly gives the field the property it is actually read for.
    #[serde(default)]
    pub kind: LessonKind,
}

/// How far a lesson travels — to any task in this repository, or only to the
/// turn that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LessonKind {
    /// Still true on a task the agent has never seen — a convention, an
    /// invariant, a required step, a trap. The only kind that can pay off
    /// somewhere other than where it was learned.
    Domain,
    /// True of this turn and not evidently of the next one. Kept, because a
    /// repeated failure mode is worth knowing, but ranked below travelling
    /// lessons at recall.
    #[default]
    Process,
}

impl LessonKind {
    /// The precedence band this lesson's memory competes in once the recall
    /// budget binds.
    ///
    /// Recall budgets are small — `max_frames` defaults to 5, and the measured
    /// token budget is roughly 0.05% of a turn's input — so when the budget
    /// binds, the frames that survive should be the ones that can apply to a
    /// task the agent has not seen.
    ///
    /// Note the asymmetry: a domain fact is `Normal`, not promoted. It competes
    /// on rank with every other memory and every code symbol exactly as it
    /// always has. It is the *process* note that volunteers to yield, because a
    /// note about how one turn went is the thing least likely to be true of the
    /// next one. Ranking commentary *up* would have been a much larger claim —
    /// that a mined lesson outranks the code it was mined from — and this
    /// change does not make it.
    pub fn recall_tier(self) -> RecallTier {
        match self {
            LessonKind::Domain => RecallTier::Normal,
            LessonKind::Process => RecallTier::Deferred,
        }
    }
}

/// A session-scoped task identity, distinct per process.
///
/// One `stella run` is one task, so for the headless path this is exactly the
/// right boundary. For a long REPL session it is an approximation, but a
/// strictly better one than per-turn.
///
/// The pid stays (#2320). It is not a time source and it is not a task
/// boundary — its only job is to keep two processes that start in the same
/// second from minting one id, which governance would then count as a single
/// task. Dropping it would buy reproducibility on a path that is never
/// replayed, at the price of under-counting concurrent work. The deterministic
/// half lives in `deterministic_task_id` instead (not a doc link: it is
/// test-gated, so it does not exist in the documented build), so the
/// nondeterminism is
/// confined to the constructor that wants it rather than shared by both.
fn default_task_id(clock: &dyn Clock) -> String {
    format!("session:{}-{}", clock.now_unix_secs(), std::process::id())
}

/// The task identity [`SessionMemory::open_with_clock`] mints: derived from the
/// injected clock alone, so two sessions opened at the same instant agree.
///
/// Deliberately carries no pid. A caller reaching for the deterministic
/// constructor is replaying a recorded engagement, where "which process am I"
/// is not a question with an answer — and where two sessions minting the same
/// id at the same instant is the property under test, not a collision.
///
/// Gated alongside its only caller, [`SessionMemory::open_with_clock`] — drop
/// both gates together when the trace-replay harness lands (#2304).
#[cfg(test)]
fn deterministic_task_id(clock: &dyn Clock) -> String {
    format!("session:{}", clock.now_unix_secs())
}

mod reflection;
pub(crate) use reflection::{ReflectionPosture, reflect_routed};
pub use reflection::{
    ReflectionReport, TurnEvidence, TurnFriction, reflect_on_turn, should_reflect_on,
    turn_warrants_reflection,
};

/// Session-scoped memory state: the context store, the CGP host that
/// routes every recall (workspace memory + code graph as in-process CGP
/// providers — see `crate::contextgraph`), the domain taxonomy, and the skills
/// auto-creation accounting.
pub struct SessionMemory {
    store: std::sync::Arc<ContextStore>,
    host: contextgraph_host::Host,
    domains: Domains,
    /// The retrieval knobs in force for this session, read once at open.
    /// Per-query budgets live here; the store-level ranking knobs were already
    /// handed to `ContextStore::with_tuning`.
    retrieval: crate::settings::RetrievalSettings,
    workspace_root: PathBuf,
    include_workspace_skills: bool,
    skills_created: usize,
    /// The task boundary stamped onto every lesson this session mines.
    ///
    /// Governance counts *distinct tasks* before promoting anything, and with
    /// no boundary the count fell back to `turn:<timestamp>`. That is wrong in
    /// the unsafe direction twice over: three turns spent on one task read as
    /// three tasks, and three lessons emitted by one reflection call — sharing
    /// one timestamp — read as one. A session is not a perfect task boundary
    /// either, but turns within a session are at least plausibly one task,
    /// which is strictly closer than per-turn.
    /// [`SessionMemory::set_task_id`] lets a caller that genuinely knows the
    /// boundary supply it, which the fleet door does (#3989).
    task_id: String,
    /// The execution this turn is writing under, when the caller knows it —
    /// what lets a mined lesson and a self-review be traced back to the turn
    /// that produced them. `None` on any path that has not adopted
    /// [`SessionMemory::set_execution_id`], which files id-less rows exactly
    /// as every row was filed before.
    ///
    /// The post-turn self-review is stored 1:1 with an execution, so without
    /// this the loop has nothing to key the write on — which is why
    /// `execution_reflection.self_rating` was NULL on every row ever written,
    /// and the Observatory's self-improve panels had no data to show. `None`
    /// degrades exactly as before: lessons still mine, the self-review is
    /// dropped rather than written against a guessed row.
    execution_id: Option<i64>,
    /// A/B recall control (Proposal 4): when true, recall is suppressed
    /// entirely on this turn so the outcome can be compared against recalled
    /// turns. Set by `arm_recall_control()`, which every driver calls once per
    /// turn before it recalls anything.
    ab_suppressed: bool,
    /// The steering plane's master switch, resolved once by `Settings::load`
    /// and carried on [`crate::settings::AuthorityPolicy`] (#3243).
    ///
    /// Distinct from `ab_suppressed`, which withholds injection for ONE turn
    /// to build a control arm; this withholds it for the session because an
    /// operator, or their org, asked for it. Both gate the same sites, and
    /// keeping them separate is what lets a control turn stay attributable as
    /// an experiment rather than looking like a disabled feature.
    ///
    /// Defaults **on**: a `SessionMemory` opened without an authority (the
    /// `open`/`open_with_workspace_skills` paths) behaves exactly as before.
    steering_enabled: bool,
    /// The turn number this session last claimed from the control's schedule —
    /// normally the durable, workspace-wide count held in the context store
    /// (#1221), and a bare in-session tally only when that store could not hand
    /// one out. Kept on the session because a fallback tally has to live
    /// somewhere and because it is what the arm is derived from
    /// (`recall::ab_control_turn`).
    ab_turn: u64,
    /// Phase 3 (#714): `context.lifecycle.enabled`, read once at open. While
    /// this is off the learning loop runs exactly the lexical path that ships
    /// today and writes nothing to the lifecycle ledger.
    lifecycle_enabled: bool,
    /// The resolved record registry behind the volatile context-record channel —
    /// `may`/`info` records and anything the truth sweep demoted (epic #897).
    ///
    /// The channel rides the recall block rather than the cached system prefix
    /// because that is what `force` means: `must`/`should` are unconditional and
    /// cacheable, facts are only worth tokens when they apply. The *registry* is
    /// stored rather than a pre-rendered string because rendering is per turn:
    /// `applies_to` selection needs the turn's prompt to decide which scoped
    /// records are worth their tokens right now. Seeded per session by
    /// [`Self::open_for_session`] from the already-resolved rule registry, and
    /// refreshed mid-session when the rule files' bytes move — see
    /// [`records_refresh`]. Behind a lock because the freshness check runs at
    /// the re-query boundary, where the turn holds this memory by `&`.
    record_registry: std::sync::RwLock<Option<stella_records::records::Registry>>,
    /// Bumped on every mid-session registry swap and folded into the
    /// re-query fingerprint, so a swap forces exactly one re-query at the
    /// next boundary — path drift alone would never notice it.
    records_generation: std::sync::atomic::AtomicU64,
    /// Content digest of the rule files behind the loaded registry
    /// ([`records_refresh::rules_digest`]), so the per-boundary freshness
    /// check is a read-and-hash and a reload runs only when bytes moved.
    records_fingerprint: std::sync::Mutex<u64>,
    /// A one-shot section the next recall block leads with — what a
    /// mid-session swap could not fully apply (a pinned change that binds
    /// next session, a file that stopped parsing).
    records_note: std::sync::Mutex<Option<String>>,
    /// The turn's selection→offer join, noted at turn start by
    /// [`Self::note_turn_skills`] and consumed at turn end by the trial
    /// recorder inside [`Self::record_episode`].
    ///
    /// The trigger-matched set rides with the injected one because a trial is
    /// a *measurement*, not a usage count: the matched-but-uninjected skills
    /// are the control arm, and neither set can be reconstructed after the
    /// turn — both are functions of the prompt and the A/B control's coin,
    /// gone by the time the outcome is known. A `Mutex` rather
    /// than a plain field because the consumer runs behind `&self` on the
    /// async episode path; it is touched twice a turn and never contended.
    turn_skill_join: std::sync::Mutex<Option<TurnSkillJoin>>,
    /// The session's time source (#2320) — the only one the learning loop is
    /// allowed to read.
    ///
    /// `stella-context` has shipped this port since the bi-temporal store
    /// landed, and [`ContextStore::open_and_warm`] has always taken one; the
    /// learning loop simply routed around it and called `SystemTime::now()` in
    /// five places. That made every write here non-replayable, and one of the
    /// five was [`SessionMemory::retire_failing_context`], whose `now` is the
    /// instant the truth sweep judges TTLs against — so retirement could not be
    /// tested at all, because the sweep always saw today.
    ///
    /// Production passes [`SystemClock`] through every existing constructor, so
    /// this changes no shipped behaviour; it is the seam that lets a test
    /// advance time on purpose.
    clock: std::sync::Arc<dyn Clock>,
}

impl SessionMemory {
    /// Open the workspace's memory. `None` (with a one-line warning) when
    /// the store can't open — a session without memory beats no session.
    pub fn open(workspace_root: &Path, warn: bool) -> Option<Self> {
        Self::open_with_workspace_skills(workspace_root, warn, false)
    }

    /// Override the task boundary lessons are stamped with.
    ///
    /// The default is session-scoped, which is exactly right for `stella run`
    /// (one process, one task) and an approximation for a long REPL session. A
    /// caller that genuinely knows where one task ends and the next begins —
    /// a benchmark harness, an issue-driven runner — should say so here, which
    /// is what makes governance's distinct-task threshold mean anything.
    ///
    /// A fleet attempt is the first such caller (#3989): it stamps
    /// `fleet:<task id>` from the plan's own task, composed by
    /// `fleet_cmd::attempt_task_boundary`. **Retry semantics:** the task is the
    /// whole boundary. Every attempt at one task inside one run shares it, and
    /// so does the same task id in a later run or in a different plan.
    ///
    /// Both merges are deliberate.
    /// This field exists to stop a lesson clearing a promotion threshold it has
    /// not earned, so between a boundary whose failure is under-counting
    /// distinct tasks (a promotion arrives later than it might have) and one
    /// whose failure is over-counting (a promotion arrives on evidence that
    /// does not support it), take the first. The claim-holder identity
    /// `{run_id}/{task id}` fails the other way: it is unique per run, so one
    /// task worked in three nightly runs would promote on a single task's worth
    /// of evidence — the same over-counting the session default already has,
    /// and the reason a fleet attempt cannot just keep the default. `task.id`
    /// is plan-local, so two unrelated plans that both name a task `t1` merge
    /// into one boundary; that merges evidence and delays a promotion.
    pub(crate) fn set_task_id(&mut self, task_id: impl Into<String>) {
        self.task_id = task_id.into();
    }

    #[cfg(test)]
    pub(crate) fn task_id_for_test(&self) -> &str {
        &self.task_id
    }

    #[cfg(test)]
    pub(crate) fn execution_id_for_test(&self) -> Option<i64> {
        self.execution_id
    }

    /// Tell memory which execution this turn's reflection belongs to, so the
    /// model's self-review can be stored against it.
    ///
    /// Called by every path that begins an execution and later reflects. A path
    /// that forgets to silently loses that turn's self-rating rather than
    /// failing loudly, so the callers are the feature.
    pub fn set_execution_id(&mut self, execution_id: i64) {
        self.execution_id = Some(execution_id);
    }

    /// Open this session's memory with its volatile record channel already
    /// attached — the one constructor every session surface goes through.
    ///
    /// It takes `active_rules` because opening and attaching used to be two
    /// calls, and that split had no failure mode a driver could notice: a
    /// surface that made only the first got a memory that recalled perfectly
    /// well, just without the `may`/`info` records and anything the truth sweep
    /// demoted. No error, no empty block — only facts that never arrived.
    ///
    /// Three of the five surfaces forgot the second call that way — the Command
    /// Deck (`run_deck_session`, the surface most users actually touch), `/goal`
    /// (`run_goal_cmd`), and `stella run`'s non-pipeline path
    /// (`run_raw_one_shot`) — so the pair is now one call that cannot be
    /// half-made. [`Self::open_with_workspace_skills`] is private for the same
    /// reason: there is no authority-scoped door that skips the channel.
    ///
    /// The channel rides the per-turn recall block rather than the system
    /// prefix because that is what `force` means: `must`/`should` are
    /// unconditional and cacheable, facts are only worth tokens when they
    /// apply. Rendering it into the byte-stable prefix would also be a
    /// guaranteed cache miss on every call, since its text may differ per turn.
    pub fn open_for_session(
        workspace_root: &Path,
        warn: bool,
        authority: &crate::settings::AuthorityPolicy,
        active_rules: &crate::rules::ResolvedRules,
    ) -> Option<Self> {
        let mut memory = Self::open_with_workspace_skills(
            workspace_root,
            warn,
            authority.project_prompts_allowed,
        )?;
        memory.set_steering_enabled(authority.steering_allowed);
        memory.set_record_registry(active_rules.registry().clone());
        Some(memory)
    }

    fn open_with_workspace_skills(
        workspace_root: &Path,
        warn: bool,
        include_workspace_skills: bool,
    ) -> Option<Self> {
        let clock: std::sync::Arc<dyn Clock> = std::sync::Arc::new(SystemClock);
        let task_id = default_task_id(clock.as_ref());
        Self::open_inner(
            workspace_root,
            warn,
            include_workspace_skills,
            clock,
            task_id,
        )
    }

    /// Open a session whose every timestamp — the stamps on mined lessons, the
    /// episode clock, the instant the truth sweep judges TTLs against, and the
    /// default task id — comes from `clock` rather than the wall clock (#2320).
    ///
    /// Two sessions opened this way at the same instant write byte-identical
    /// logs, which is what makes the learning loop replayable and what lets a
    /// test place itself on either side of a TTL.
    ///
    /// `include_workspace_skills` decides whether this session may write into
    /// the workspace's own skills and rules directories — a *trusted project*,
    /// in `open_for_session`'s terms. The replayer needs it true, and that is
    /// not a convenience: with it false, `write_candidates` and `induce_rules`
    /// both record their proposals and then decline to write a FILE (#737). A
    /// replay of a whole corpus would behave perfectly correctly and build
    /// nothing, which reads as a broken learner rather than as a session that
    /// was never trusted to publish. Measured, not theorised — it is the
    /// difference between the committed corpus producing two skills and a rule,
    /// and producing zero.
    ///
    /// **Test-gated until the trace-replay harness lands** (epic #2304,
    /// `doc:trace-replay-learning-harness` §4). `stella-cli` is a bin-only
    /// crate, so no external consumer could call this even in principle, and
    /// leaving it on the production build would be dead code. The harness is
    /// itself `#[cfg(test)]` for exactly that reason, so the gate stays.
    /// [`SessionMemory::set_task_id`] carried the same gate under the same rule
    /// and dropped it when the fleet door became its first shipped caller
    /// (#3989); this one drops when the replayer lands.
    #[cfg(test)]
    pub(crate) fn open_with_clock(
        workspace_root: &Path,
        warn: bool,
        include_workspace_skills: bool,
        clock: std::sync::Arc<dyn Clock>,
    ) -> Option<Self> {
        let task_id = deterministic_task_id(clock.as_ref());
        Self::open_inner(
            workspace_root,
            warn,
            include_workspace_skills,
            clock,
            task_id,
        )
    }

    fn open_inner(
        workspace_root: &Path,
        warn: bool,
        include_workspace_skills: bool,
        clock: std::sync::Arc<dyn Clock>,
        task_id: String,
    ) -> Option<Self> {
        // Ephemeral benchmark trials must neither recall task/user-planted
        // learning state nor create or migrate a context database that can
        // perturb the task under test. Reflection is separately pinned off
        // by the launcher; this closes the pre-turn recall side of the same
        // boundary before the private-state resolver performs any I/O.
        if crate::settings::filesystem_settings_disabled() {
            return None;
        }
        let db_path = resolve_context_db_path(workspace_root, warn, |message| {
            eprintln!("  {} {message}", "!".yellow());
        })?;
        let retrieval = tuning::session_retrieval_settings(workspace_root);
        match ContextStore::open_and_warm(
            &db_path,
            std::sync::Arc::new(HashEmbedder::default()),
            clock.clone(),
        )
        .map(|store| store.with_tuning(retrieval.tuning()))
        {
            Ok(store) => {
                // Run the stale-anchor scan here too, right next to warm,
                // on this same open store. This ends a deleted file's
                // anchor with no manual `--end-stale` run. See
                // `anchor_scan::scan_stale_anchors_at_mount` for the cap.
                anchor_scan::scan_stale_anchors_at_mount(&store, workspace_root);
                let domains = Domains::load(workspace_root)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                let store = std::sync::Arc::new(store);
                let host = crate::contextgraph::session_host(
                    store.clone(),
                    // The whole taxonomy, not just its names: the provider
                    // derives each query's own domain scope from the goal's
                    // anchors, which needs the path prefixes (#2333).
                    domains.clone(),
                    workspace_root.to_path_buf(),
                    suppression::suppression_reader(workspace_root, store.clone()),
                );
                Some(Self {
                    record_registry: std::sync::RwLock::new(None),
                    records_generation: std::sync::atomic::AtomicU64::new(0),
                    records_fingerprint: std::sync::Mutex::new(0),
                    records_note: std::sync::Mutex::new(None),
                    store,
                    host,
                    domains,
                    retrieval,
                    workspace_root: workspace_root.to_path_buf(),
                    include_workspace_skills,
                    skills_created: 0,
                    ab_suppressed: false,
                    steering_enabled: true,
                    ab_turn: 0,
                    // Phase 3 (#714)
                    lifecycle_enabled: tuning::session_lifecycle_enabled(workspace_root),
                    task_id,
                    execution_id: None,
                    turn_skill_join: std::sync::Mutex::new(None),
                    clock,
                })
            }
            Err(e) => {
                if warn {
                    eprintln!("  {} memory disabled this session: {e}", "!".yellow());
                }
                None
            }
        }
    }

    /// Register every enabled external CGP context provider from settings onto
    /// this session's host (#453).
    ///
    /// Async and separate from [`SessionMemory::open`] because admission does
    /// real I/O — it spawns or connects to each provider and runs the
    /// protocol's conformance suite against it — and `open` is called from
    /// synchronous paths. A refusal is reported, never fatal: the session
    /// continues on its in-tree sources, which is the same crash-isolation
    /// discipline the query fan-out applies, moved to admission time.
    ///
    /// With no `context_providers` configured (the shipping default) this
    /// costs one settings read and registers nothing.
    pub async fn register_external_providers(
        &mut self,
        report: impl Fn(String),
    ) -> Vec<crate::contextgraph::Admission> {
        let configured = match crate::settings::Settings::load(&self.workspace_root) {
            Ok(settings) => settings.context_providers,
            Err(error) => {
                report(format!(
                    "external context providers disabled: settings unreadable: {error}"
                ));
                return Vec::new();
            }
        };
        if configured.is_empty() {
            return Vec::new();
        }
        let admissions =
            crate::contextgraph::register_external_providers(&mut self.host, &configured).await;
        let admitted: Vec<&str> = admissions
            .iter()
            .filter(|a| a.registered())
            .map(|a| a.id())
            .collect();
        if !admitted.is_empty() {
            report(format!(
                "external context providers admitted: {}",
                admitted.join(", ")
            ));
        }
        for refusal in admissions.iter().filter_map(|a| a.refusal()) {
            report(refusal);
        }
        admissions
    }

    fn workspace_skills_dir(&self) -> String {
        workspace_skills_dir(&self.workspace_root)
    }

    /// Set the steering plane's master switch for this session (#3243).
    ///
    /// Takes the already-resolved answer rather than re-deriving it: the
    /// precedence chain (settings → `STELLA_CONTEXT_STEERING` → the org's
    /// ceiling) is settled once in `Settings::load` and carried on
    /// [`crate::settings::AuthorityPolicy`], so a second derivation here could
    /// only ever disagree with the first.
    pub(crate) fn set_steering_enabled(&mut self, enabled: bool) {
        self.steering_enabled = enabled;
    }

    /// Load the workspace's skills fresh (cheap — a handful of file reads;
    /// fresh so a just-installed or just-auto-created skill is live on the
    /// very next turn).
    ///
    /// Demoted skills are excluded here, which is what makes a
    /// demotion mean something: every selection path reads this load, so a
    /// skill the appraisal sweep retired stops being offered everywhere at
    /// once, while its file — and the append-only ledger row saying why —
    /// both survive.
    pub fn load_skills(&self) -> Vec<Skill> {
        let mut skills = load_workspace_skills_with_authority(
            &self.workspace_root,
            self.include_workspace_skills,
        )
        .skills;
        let demoted = appraisals::demoted_skills(&self.store);
        if !demoted.is_empty() {
            skills.retain(|s| !demoted.contains(&s.name));
        }
        skills
    }
}

/// One turn's trigger→injection join. See the field docs on
/// [`SessionMemory::turn_skill_join`].
#[derive(Debug, Clone)]
struct TurnSkillJoin {
    /// Skills whose trigger matched this turn's prompt — the population both
    /// appraisal arms are drawn from.
    trigger_matched: Vec<String>,
    /// The subset selection actually injected. Empty on a suppressed turn.
    selected: Vec<String>,
}

impl SessionMemory {
    /// The skills recall would inject for `prompt`, as `(name, reason)` pairs
    /// — `reason` is the matched domains/terms that selected it.
    ///
    /// Test-gated: [`Self::note_turn_skills`] returns the same pairs and also
    /// arms the turn's trial join, so it is the door every production caller
    /// takes. A production caller of this one would be a caller reporting an
    /// injection without recording the measurement of it, which is the split
    /// that left the appraisal ledger empty. Drop the gate if a surface ever
    /// needs to *preview* a selection rather than take a turn with it.
    #[cfg(test)]
    pub fn selected_skills(&self, prompt: &str) -> Vec<(String, String)> {
        if self.injection_suppressed() {
            return Vec::new();
        }
        Self::describe_selection(self.skill_selection(prompt).selected)
    }

    /// Whether this turn withholds every injected channel — the A/B recall
    /// control's coin, or steering switched off for the workspace.
    ///
    /// One copy, because a turn that injects skills but reports none (or the
    /// reverse) corrupts both appraisal arms at once.
    fn injection_suppressed(&self) -> bool {
        self.ab_suppressed || !self.steering_enabled
    }

    /// One scoring pass over the loaded skills, keeping the top-k tail.
    ///
    /// The A/B control is **not** applied here. Whether this turn's prompt
    /// matched a skill's trigger is a fact about the prompt; whether the match
    /// was then injected is the experiment. Collapsing the two would erase the
    /// control arm, since a suppressed turn would look like a turn the skill
    /// never matched.
    fn skill_selection(&self, prompt: &str) -> skills::SkillSelection {
        skills::select_skills_reporting(
            &self.load_skills(),
            prompt,
            &self.active_domains(prompt),
            &SelectionConfig::default(),
        )
    }

    /// Render selected skills as `(name, why)` — the matched domains and terms
    /// that put each one in the prompt.
    fn describe_selection(selected: Vec<skills::SelectedSkill>) -> Vec<(String, String)> {
        selected
            .into_iter()
            .map(|s| {
                let mut why: Vec<String> = Vec::new();
                if !s.matched_domains.is_empty() {
                    why.push(format!("domains: {}", s.matched_domains.join(", ")));
                }
                if !s.matched_terms.is_empty() {
                    why.push(format!("terms: {}", s.matched_terms.join(", ")));
                }
                (s.skill.name, why.join("; "))
            })
            .collect()
    }

    /// `selected_skills`, and additionally note this turn's
    /// trigger→injection join for the trial recorder. The turn-start seam
    /// (`agent::stamp_and_record_skill_usage`) calls this instead of the plain
    /// query, so a turn that records usage also arms its own trial.
    ///
    /// **The join is scoped to the skills this prompt's trigger matched**, not
    /// to every skill on disk. A trial is a with/without comparison, and both
    /// arms have to be drawn from the same population or the baseline is not
    /// one: a skill about SQL migrations that is offered on a turn about CSS
    /// was never going to help that turn, and counting it as a without-skill
    /// success measures the CSS turn, not the skill. Scoring every loaded skill
    /// against every turn is how a skill's baseline fills with turns it could
    /// not have affected, and an appraisal over that population answers a
    /// question nobody asked.
    ///
    /// Matched-but-not-injected turns are what the without-skill arm is made
    /// of, and there are three ways to be one: the A/B control's coin, steering
    /// switched off, and losing the last top-k seat to a higher-scoring
    /// sibling. All three are recorded, which is why this reads the selection
    /// pass rather than `selected_skills` — that one returns nothing at
    /// all on a suppressed turn, by design.
    pub(crate) fn note_turn_skills(&self, prompt: &str) -> Vec<(String, String)> {
        let selection = self.skill_selection(prompt);
        // The trigger-matched population: the survivors plus the ones the
        // top-k cut threw away. Both cleared the same score floor, which is
        // what "the trigger matched" means here.
        let trigger_matched: Vec<String> = selection
            .selected
            .iter()
            .chain(selection.over_top_k.iter())
            .map(|s| s.skill.name.clone())
            .collect();
        let injected = if self.injection_suppressed() {
            Vec::new()
        } else {
            Self::describe_selection(selection.selected)
        };
        if let Ok(mut join) = self.turn_skill_join.lock() {
            *join = Some(TurnSkillJoin {
                trigger_matched,
                selected: injected.iter().map(|(name, _)| name.clone()).collect(),
            });
        }
        injected
    }

    /// Append this turn's skill trials — one per skill whose trigger matched
    /// this turn, `selected` set from the join [`Self::note_turn_skills`]
    /// armed — to the trial ledger `appraisals::sweep` appraises.
    ///
    /// A turn that matched no trigger records nothing, which is the whole
    /// point: it is not evidence about any skill.
    ///
    /// Takes the join rather than reading it, so one episode records one
    /// turn's trials exactly once; a path that never armed a join (a
    /// sub-session, a sweep with no turn) records nothing. Best-effort like
    /// every ledger write here.
    fn record_skill_trials(&self, succeeded: bool) {
        let Ok(mut guard) = self.turn_skill_join.lock() else {
            return;
        };
        let Some(join) = guard.take() else {
            return;
        };
        drop(guard);
        if join.trigger_matched.is_empty() {
            return;
        }
        appraisals::record_turn(
            &self.workspace_root,
            &join.trigger_matched,
            &join.selected,
            &stella_core::skills::appraisal::SkillTrial {
                task: appraisals::LIVE_WINDOW_TASK.to_string(),
                // Overwritten per skill by `record_turn`; the value here is
                // never read.
                selected: false,
                outcome: stella_core::self_tuning::TaskOutcome {
                    succeeded,
                    cost_usd: 0.0,
                    tokens: 0,
                    retries: 0,
                },
                turns: 1,
            },
        );
    }

    /// Record the turn that just finished as an episodic memory: a summary,
    /// the files it touched, and how it ended. Episodes become retrievable
    /// `Episode` nodes, so future recall can surface "we did something like
    /// this before" alongside reflections — the episodic half of the context
    /// plane (`stella-context` L-C3 neighborhood). Domain tags come from the
    /// touched files' taxonomy prefixes. Best-effort like everything here: a
    /// failed write must never fail the turn it describes.
    ///
    /// **A/B attribution rides here, not at the call sites** (#1221). A turn
    /// the control suppressed is tagged `[ab-control]` by this method, because
    /// the tag is the entire readout of the experiment and a driver that
    /// forgets it does not fail — it silently files a control turn as an
    /// ordinary one, which corrupts both arms at once. `self` already knows
    /// whether this turn was suppressed, so nothing is gained by asking four
    /// callers to remember to say so.
    pub async fn record_episode(
        &self,
        prompt: &str,
        outcome: EpisodeOutcome,
        files_touched: &[(String, String)],
        started_unix_secs: i64,
        tag: Option<&str>,
    ) {
        // The turn-end half of the skill promote/retire join: every
        // episode-writing surface — chat, `run`, `goal`, the deck — funnels
        // through here with the settled outcome, which makes this the one
        // seam that can turn the turn-start selection note into a trial.
        self.record_skill_trials(outcome == EpisodeOutcome::Success);

        let mut summary: String = prompt.chars().take(240).collect();
        if prompt.chars().count() > 240 {
            summary.push('…');
        }
        // A tag (the #1042 trace pointer) lands AFTER truncation: it is a
        // join key, and a key a long prompt silently truncates away is not
        // a key. The A/B marker is the same kind of key and lands the same
        // way — appended to the *prompt* (as it was) it was the first thing a
        // 240-character prompt threw away, which lost exactly the control
        // turns the experiment is made of.
        if let Some(tag) = tag {
            summary.push_str(tag);
        }
        if self.ab_suppressed {
            summary.push_str(AB_CONTROL_TAG);
        }

        let mut domains: Vec<String> = Vec::new();
        for (path, _ops) in files_touched {
            for name in self.domains.domains_for_path(path) {
                if !domains.contains(&name) {
                    domains.push(name);
                }
            }
        }

        // The episode's end instant, from the session clock (#2320). `started`
        // arrives as a parameter and always did, so a caller that knows when
        // the turn began — a replayer, a test — now controls both ends.
        let now_secs = self.clock.now_unix_secs();
        let mut episode = EpisodeInput::new(
            summary,
            format_rfc3339(started_unix_secs),
            format_rfc3339(now_secs),
        )
        .with_domains(domains);
        // The turn's own execution row is what separates two turns that share a
        // prompt and land in the same second — both timestamps are
        // second-resolution, so without it the second write lands on top of the
        // first and the earlier turn's outcome and files are gone. A turn that
        // is recorded twice passes the same id and still updates one row. A
        // path that never adopted `set_execution_id` keeps the older identity
        // and the collision with it, which is tracked on its own issue.
        if let Some(execution) = self.execution_id {
            episode = episode.with_occurrence(execution.to_string());
        }
        episode.outcome = outcome;
        episode.files_touched = files_touched.iter().map(|(path, _)| path.clone()).collect();

        let delta = ContextDelta {
            episodes: vec![episode],
            ..Default::default()
        };
        // Ignored on purpose, and it is the weakest link on this path: a
        // failed episode must not fail the turn it describes, the deck calls
        // this while it owns the terminal so stderr is not available, and the
        // workspace has no logger to route the failure to. Reporting it is
        // tracked on its own issue. The delta holds one record, so nothing
        // else is lost with it.
        let _ = self.store.upsert(delta).await;
    }

    /// Persist the `stella init` taxonomy into the context plane: each domain
    /// as a described domain record, and each of its path prefixes as a
    /// bi-temporal `covers_path` fact. Re-running `init` after the taxonomy
    /// shifts supersedes stale beliefs instead of deleting them, so
    /// "what did we believe at T1" still answers (L-C3).
    ///
    /// Known limitation (deliberately deferred): `covers_path` *facts* are
    /// versioned (a moved path's old fact is superseded), but the File node's
    /// `node_domains` tags are insert-only — re-running `init` after a path
    /// moves from domain A to B adds the B tag without removing A. This does
    /// NOT break recall correctness: the session scopes recall to the *full
    /// current taxonomy*, so the node still passes the scope filter via B; the
    /// residual is only a domain-overlap ranking boost for A, and only while A
    /// itself remains a taxonomy domain.
    ///
    /// Two fixes were considered and both deferred:
    /// - Versioned node-domain associations (mirroring the fact model) — the
    ///   correct design, but a `stella-context` schema change (`node_domains`
    ///   gains validity columns, and every scope query must filter live rows).
    ///   Disproportionate to a ranking-edge, and higher-risk right after the
    ///   store's DuckDB→SQLite migration.
    /// - Retiring taxonomy-owned tags before re-adding (a `node_domains`
    ///   rewrite) — rejected as brittle: it relies on the unenforced invariant
    ///   that only the taxonomy ever tags File nodes, so it would silently wipe
    ///   a tag written by any future source.
    pub async fn record_taxonomy(&self, taxonomy: &crate::domains::Domains) {
        let domains = taxonomy
            .domains
            .iter()
            .map(|d| DomainInput {
                name: d.name.clone(),
                description: (!d.description.is_empty()).then(|| d.description.clone()),
            })
            .collect();
        let facts = taxonomy
            .domains
            .iter()
            .flat_map(|d| {
                d.paths.iter().map(|path| {
                    // Tag the nodes themselves, not just the edge — node-level
                    // tags are what `recall_scoped`'s domain filter and
                    // overlap boost read (`node_domains` rows come from the
                    // subject/object inputs, never from the fact's own tags).
                    let subject = NodeInput::new(NodeKind::Concept, &d.name)
                        .with_uri(format!("domain://{}", d.name))
                        .with_domains([d.name.clone()]);
                    let object = NodeInput::new(NodeKind::File, path)
                        .with_uri(format!("file://{path}"))
                        .with_domains([d.name.clone()]);
                    let mut fact = FactAssertion::new(subject, "covers_path", object)
                        .with_domains([d.name.clone()]);
                    // A domain legitimately covers several paths at once.
                    fact.multivalued = true;
                    fact
                })
            })
            .collect();
        let delta = ContextDelta {
            domains,
            facts,
            ..Default::default()
        };
        let _ = self.store.upsert(delta).await;
    }
}

/// Phase 3 (#714): where this workspace's `context.db` lives — the lifecycle
/// ledger's home, needed by the `stella proposals` review surface, which reads
/// the ledger without opening a whole session.
pub(crate) fn context_db_path(workspace_root: &Path) -> Result<PathBuf, String> {
    stella_store::workspace_private_sqlite_path(workspace_root, "context.db")
        .map_err(|e| format!("cannot resolve private context state: {e}"))
}

/// Seconds since the Unix epoch — the episode timestamps' primitive.
///
/// This is the **ambient** wall clock, for callers outside a session that need
/// a start instant to hand to [`SessionMemory::record_episode`] — the drivers
/// in `agent.rs`, `agent/goal.rs` and `command_deck.rs`, which own no clock.
/// [`SessionMemory`] itself no longer calls it (#2320): every timestamp the
/// learning loop writes comes from [`SessionMemory::clock`], and the one value
/// that still arrives from out here rides in as `started_unix_secs`, a
/// parameter a replayer supplies. Reaching for this from inside a session would
/// re-open the seam #2320 closed.
pub(crate) fn unix_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
