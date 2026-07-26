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
    HashEmbedder, NodeInput, NodeKind, SystemClock, format_rfc3339,
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

mod learning;
mod private_state;
mod projection;
#[cfg(test)]
mod quarantine_tests;
mod recall;
#[path = "memory/skills.rs"]
mod skill_files;
mod suppression;
mod tuning;
use private_state::resolve_context_db_path;
#[cfg(test)]
use projection::{is_suppressed_local_frame, project_recalled_frame};
pub use recall::inject_recall_block;
#[cfg(test)]
use recall::{ab_control_turn, goal_path_anchors, render_context_section};
#[cfg(test)]
pub(crate) use skill_files::load_workspace_skills;
pub(crate) use skill_files::{load_workspace_skills_with_authority, workspace_skills_dir};

/// Marker prefixing a recalled-context message so [`inject_recall_block`]
/// can find the newest one for dedup. Blocks land at the conversation
/// tail and stay in place as durable history (L-E8: the byte-stable
/// prefix — system prompt AND replayed turns — is never rewritten, which
/// is what preserves prompt-cache hits).
pub const RECALL_MARKER: &str = "[auto-recalled context]";

/// One reflection lesson as the model returns it and as persisted to the
/// mining log (`.stella/private/reflections.jsonl`, one JSON object per line).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionLesson {
    pub lesson: String,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub occurred_at: u64,
}

mod reflection;
#[cfg(test)]
use reflection::parse_lessons;
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
    /// A/B recall control (Proposal 4): when true, recall is suppressed
    /// entirely on this turn so the outcome can be compared against recalled
    /// turns. Set by `maybe_suppress_recall()` from the turn counter below.
    ab_suppressed: bool,
    /// Count of turns that have consulted the A/B control, used to make
    /// every `rate`-th turn a deterministic control turn (see
    /// [`SessionMemory::maybe_suppress_recall`]).
    ab_turn: u32,
}

impl SessionMemory {
    /// Open the workspace's memory. `None` (with a one-line warning) when
    /// the store can't open — a session without memory beats no session.
    pub fn open(workspace_root: &Path, warn: bool) -> Option<Self> {
        Self::open_with_workspace_skills(workspace_root, warn, false)
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
                    suppression::suppression_reader(workspace_root),
                );
                Some(Self {
                    store,
                    host,
                    domains,
                    retrieval,
                    workspace_root: workspace_root.to_path_buf(),
                    include_workspace_skills,
                    skills_created: 0,
                    ab_suppressed: false,
                    ab_turn: 0,
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
    /// as [`Self::recall_block`], so this reports exactly what was applied.
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
    ) {
        let mut summary: String = prompt.chars().take(240).collect();
        if prompt.chars().count() > 240 {
            summary.push('…');
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

/// Seconds since the Unix epoch — the episode timestamps' primitive.
pub(crate) fn unix_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
