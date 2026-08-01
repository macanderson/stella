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
//! prompt ──> recall_block(): registry-routed recall (crate::contextgraph) + select_skills()
//!            └─ volatile message AFTER the byte-stable system prefix (L-E8)
//! turn runs …
//! outcome ─> reflect_and_record(): one cheap model call -> 0-3 lessons
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
    ContextDelta, ContextStore, DomainInput, EpisodeInput, EpisodeOutcome, FactAssertion,
    HashEmbedder, NodeInput, NodeKind, RecallTier, SystemClock, format_rfc3339,
};
use stella_core::skills::{self, SelectionConfig, Skill};

use crate::domains::Domains;

// Reached only through `use super::*` in the sibling test modules
// (`memory/tests.rs`, `memory/quarantine_tests.rs`): the code that used
// these moved to `memory/recall.rs` and `memory/learning.rs`, the tests
// deliberately did not.
#[cfg(test)]
use stella_context::MemoryInput;
#[cfg(test)]
use stella_pipeline::{ContextRecallPort, RecalledFrame};
#[cfg(test)]
use stella_protocol::{CompletionMessage, MessageRole};

// Which files a memory is about — shared by the reflection write path and by
// `stella memory validate`, which must agree on what counts as an anchor.
pub(crate) mod anchors;
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
#[cfg(test)]
mod quarantine_tests;
mod recall;
// Phase 4 (#715): reversible retirement of context that stops helping.
pub(crate) mod retirement;
pub(crate) mod rules_mining;
pub(crate) mod self_tuning;
#[path = "memory/skills.rs"]
mod skill_files;
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
pub use recall::inject_recall_block;
#[cfg(test)]
use recall::{ab_control_turn, goal_path_anchors, render_context_section};
#[cfg(test)]
pub(crate) use skill_files::load_workspace_skills;
pub(crate) use skill_files::{
    load_workspace_skills_with_authority, skill_paths_on_disk, workspace_skills_dir,
};
// Phase 2 (#713): the engine-config builder reads the lifecycle switch through
// here, so exactly one place in the crate resolves a `context.*` sub-block.
pub use tuning::session_lifecycle_enabled;

/// Marker prefixing a recalled-context message so [`inject_recall_block`]
/// can find the newest one for dedup. Blocks land at the conversation
/// tail and stay in place as durable history (L-E8: the byte-stable
/// prefix — system prompt AND replayed turns — is never rewritten, which
/// is what preserves prompt-cache hits).
///
/// Phase 2 (#713) moved the definition to `stella-core`, where receipt
/// decomposition reads it to recognize a recall block. This is a re-export,
/// not a second copy: two spellings of one marker is a decomposition that
/// silently stops firing the day either is edited.
pub use stella_core::receipts::RECALL_MARKER;

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
    /// back to the turn (see `memory::observations`). Nothing populates it yet
    /// — it exists so a caller with a real task boundary can supply one without
    /// a log-format change.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub task_id: String,
    /// What sort of lesson this is: a durable fact about the codebase, or a
    /// note about how the agent behaved.
    ///
    /// They are not equally useful and were previously indistinguishable. In a
    /// measured run of ten mined lessons, eight were process self-critique
    /// ("the agent should be more proactive", "when summarizing, list the
    /// files modified") and **none** captured the repository conventions that
    /// actually decided whether a task passed. Process notes describe one
    /// turn; domain facts are still true next week, and only the second kind
    /// can transfer to a task the agent has never seen.
    #[serde(default)]
    pub kind: LessonKind,
}

/// Whether a lesson is a durable fact about the code or a note about the turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LessonKind {
    /// A convention, invariant, location or required step — true independent
    /// of this turn, and the only kind that can help on an unseen task.
    Domain,
    /// How the agent went about the work. Kept, because a repeated failure
    /// mode is worth knowing, but ranked below domain facts at recall.
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
fn default_task_id() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("session:{secs}-{}", std::process::id())
}

mod reflection;
pub use reflection::{
    ReflectionReport, reflect_on_turn, should_reflect_on, turn_warrants_reflection,
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
    /// which is strictly closer than per-turn. `set_task_id` lets a caller
    /// that genuinely knows the boundary supply it.
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
    /// turns. Set by `maybe_suppress_recall()` from the turn counter below.
    ab_suppressed: bool,
    /// Count of turns that have consulted the A/B control, used to make
    /// every `rate`-th turn a deterministic control turn (see
    /// [`SessionMemory::maybe_suppress_recall`]).
    ab_turn: u32,
    /// Phase 3 (#714): `context.lifecycle.enabled`, read once at open. While
    /// this is off the learning loop runs exactly the lexical path that ships
    /// today and writes nothing to the lifecycle ledger.
    lifecycle_enabled: bool,
    /// The volatile context-record channel — `may`/`info` records and anything the
    /// truth sweep demoted (epic #897).
    ///
    /// It rides the recall block rather than the cached system prefix because that
    /// is what `force` means: `must`/`should` are unconditional and cacheable,
    /// facts are only worth tokens when they apply. Set once per session by
    /// [`Self::set_record_channel`] from the already-resolved rule registry —
    /// re-deriving it here would re-walk the rule directories and re-run the truth
    /// sweep on every turn.
    record_channel: Option<String>,
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
    /// **Test-gated until such a caller exists in this tree.** `stella-cli` is
    /// a bin-only crate, so there is no external consumer that could call this
    /// even in principle; leaving it on the production build would be an
    /// `#[allow(dead_code)]` describing an API nothing can reach. The session
    /// default is what every shipped path uses today. Drop the gate in the same
    /// commit that adds the first real caller.
    #[cfg(test)]
    pub(crate) fn set_task_id(&mut self, task_id: impl Into<String>) {
        self.task_id = task_id.into();
    }

    #[cfg(test)]
    pub(crate) fn task_id_for_test(&self) -> &str {
        &self.task_id
    }

    /// Tell memory which execution this turn's reflection belongs to, so the
    /// model's self-review can be stored against it.
    ///
    /// Called by every path that begins an execution and later reflects. Not
    /// test-gated, unlike `set_task_id` above: the whole point is the shipped
    /// paths calling it, and a path that forgets to silently loses that turn's
    /// self-rating rather than failing loudly, so the callers are the feature.
    ///
    /// (`set_task_id` is deliberately named in prose rather than linked. It is
    /// `#[cfg(test)]`, so it does not exist in a doc build at all, and an
    /// intra-doc link to it fails `-D warnings` rather than resolving.)
    pub fn set_execution_id(&mut self, execution_id: i64) {
        self.execution_id = Some(execution_id);
    }

    /// Open memory with workspace skill injection governed by the session's
    /// immutable authority snapshot. Context recall itself remains evidence.
    pub fn open_with_authority(
        workspace_root: &Path,
        warn: bool,
        authority: &crate::settings::AuthorityPolicy,
    ) -> Option<Self> {
        Self::open_with_workspace_skills(workspace_root, warn, authority.project_prompts_allowed)
    }

    fn open_with_workspace_skills(
        workspace_root: &Path,
        warn: bool,
        include_workspace_skills: bool,
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
            std::sync::Arc::new(SystemClock),
        )
        .map(|store| store.with_tuning(retrieval.tuning()))
        {
            Ok(store) => {
                let domains = Domains::load(workspace_root)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                let store = std::sync::Arc::new(store);
                let host = crate::contextgraph::session_host(
                    store.clone(),
                    domains.names(),
                    workspace_root.to_path_buf(),
                    suppression::suppression_reader(workspace_root, store.clone()),
                );
                Some(Self {
                    record_channel: None,
                    store,
                    host,
                    domains,
                    retrieval,
                    workspace_root: workspace_root.to_path_buf(),
                    include_workspace_skills,
                    skills_created: 0,
                    ab_suppressed: false,
                    ab_turn: 0,
                    // Phase 3 (#714)
                    lifecycle_enabled: tuning::session_lifecycle_enabled(workspace_root),
                    task_id: default_task_id(),
                    execution_id: None,
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

    /// Load the workspace's skills fresh (cheap — a handful of file reads;
    /// fresh so a just-installed or just-auto-created skill is live on the
    /// very next turn).
    pub fn load_skills(&self) -> Vec<Skill> {
        load_workspace_skills_with_authority(&self.workspace_root, self.include_workspace_skills)
            .skills
    }
}

impl SessionMemory {
    /// The skills recall would inject for `prompt`, as `(name, reason)` pairs
    /// for skill-version usage telemetry — `reason` is the matched
    /// domains/terms that selected it. Same enabled-filtered load + selection
    /// as [`Self::recall_block_reported`], so this reports exactly what was applied.
    pub fn selected_skills(&self, prompt: &str) -> Vec<(String, String)> {
        skills::select_skills(
            &self.load_skills(),
            prompt,
            &self.domains.names(),
            &SelectionConfig::default(),
        )
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

    /// Record the turn that just finished as an episodic memory: a summary,
    /// the files it touched, and how it ended. Episodes become retrievable
    /// `Episode` nodes, so future recall can surface "we did something like
    /// this before" alongside reflections — the episodic half of the context
    /// plane (`stella-context` L-C3 neighborhood). Domain tags come from the
    /// touched files' taxonomy prefixes. Best-effort like everything here: a
    /// failed write must never fail the turn it describes.
    pub async fn record_episode(
        &self,
        prompt: &str,
        outcome: EpisodeOutcome,
        files_touched: &[(String, String)],
        started_unix_secs: i64,
        tag: Option<&str>,
    ) {
        let mut summary: String = prompt.chars().take(240).collect();
        if prompt.chars().count() > 240 {
            summary.push('…');
        }
        // A tag (the #1042 trace pointer) lands AFTER truncation: it is a
        // join key, and a key a long prompt silently truncates away is not
        // a key.
        if let Some(tag) = tag {
            summary.push_str(tag);
        }

        let mut domains: Vec<String> = Vec::new();
        for (path, _ops) in files_touched {
            for name in self.domains.domains_for_path(path) {
                if !domains.contains(&name) {
                    domains.push(name);
                }
            }
        }

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(started_unix_secs);
        let mut episode = EpisodeInput::new(
            summary,
            format_rfc3339(started_unix_secs),
            format_rfc3339(now_secs),
        )
        .with_domains(domains);
        episode.outcome = outcome;
        episode.files_touched = files_touched.iter().map(|(path, _)| path.clone()).collect();

        let delta = ContextDelta {
            episodes: vec![episode],
            ..Default::default()
        };
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
pub(crate) fn unix_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
