//! The learning loop — the write side of session memory: post-turn
//! reflection, the forgetting filter every recorded lesson passes through,
//! and the skill auto-creation that mines the reflection log.
//!
//! Split verbatim out of `memory.rs` — no behavior change — so the learning
//! loop and the recall plane (`memory/recall.rs`) can evolve independently
//! instead of contending for one file.

use std::path::{Path, PathBuf};

use colored::Colorize;
use stella_context::{ContextDelta, MemoryInput};
use stella_core::skills::{
    self, AutoCreateConfig, AutoCreateDecision, SkillMineConfig, SkillObservation,
};
use stella_protocol::{CompletionMessage, Provider};

use super::{
    ReflectionLesson, ReflectionReport, SessionMemory, reflect_on_turn, skill_paths_on_disk,
};

/// Spec §8 behavior-compatibility tests, written against the pre-migration
/// loop. A child module rather than a sibling so it can reach
/// [`SessionMemory::auto_create_skills`] without widening its visibility for
/// tests' sake.
#[cfg(test)]
mod guarantees;

impl SessionMemory {
    /// Post-turn self-reflection: one cheap model call producing 0-3
    /// durable lessons, stored as domain-tagged reflection memories AND
    /// appended to the skill-mining log; recurring lessons auto-promote to
    /// SKILL.md files. Best-effort throughout — a failed reflection must never
    /// fail the turn it describes. Returns a [`ReflectionReport`] so the caller
    /// can surface the outcome (a model-call error, or how many lessons landed)
    /// in whichever output format it speaks; the report distinguishes a genuine
    /// model-call failure from the common, correct "nothing worth recording."
    ///
    /// `succeeded` controls the reflection prompt template (Proposal 1):
    /// a failed turn gets a failure-analysis prompt that asks the model to
    /// identify the root cause — the highest-value learning signal in the
    /// system. A succeeded turn gets the conventional "what worked?" prompt.
    pub async fn reflect_and_record(
        &mut self,
        provider: &dyn Provider,
        model_hint: &str,
        transcript: &[CompletionMessage],
        quiet: bool,
        succeeded: bool,
        budget_limit: Option<f64>,
    ) -> ReflectionReport {
        let lessons = match reflect_on_turn(
            provider,
            model_hint,
            &self.workspace_root,
            transcript,
            &self.domains.names(),
            succeeded,
            budget_limit,
        )
        .await
        {
            Ok((lessons, cost_usd, events)) => (lessons, cost_usd, events),
            // The single reflection model call errored. Report it up so the
            // caller can warn (text) or emit an event (stream-json) — this
            // is the fix for the previously-silent reflection failure. Never
            // fatal: the turn already stands on its own.
            Err(model_error) => {
                return ReflectionReport {
                    recorded: 0,
                    model_error: Some(model_error.message),
                    cost_usd: model_error.cost_usd,
                    events: model_error.events,
                };
            }
        };
        let (lessons, reflection_cost_usd, reflection_events) = lessons;

        // Drop anything the user has already forgotten, BEFORE it reaches any
        // of the three places this function persists to. Matching is by
        // restatement, not equality: the loop re-learns paraphrases, so a
        // lesson mined today can be the same lesson the user deleted last
        // week wearing slightly different words. Suppressing here rather than
        // at recall is what makes forgetting durable — an unsuppressed lesson
        // would land in the log and stay re-mineable forever.
        let lessons = self.retain_unforgotten(lessons);

        if lessons.is_empty() {
            return ReflectionReport {
                cost_usd: reflection_cost_usd,
                events: reflection_events,
                ..ReflectionReport::default()
            };
        }

        // 1. Store as recallable, domain-tagged reflection memories. Still
        // best-effort (a failed reflection never fails the turn), but the
        // outcome is kept so the "remembered" line below can't claim success
        // for lessons that never landed in the store.
        let delta = ContextDelta {
            memories: lessons
                .iter()
                .map(|l| MemoryInput::reflection(&l.lesson, l.domains.iter().cloned()))
                .collect(),
            ..Default::default()
        };
        let stored = self.store.upsert(delta).await.is_ok();

        // 2. Append to the mining log and mine for auto-creatable skills.
        // Count how many lessons actually reached the log so the message below
        // reports partial persistence accurately (some serialize/append writes
        // may fail while others succeed).
        let log_path =
            stella_store::workspace_private_state_path(&self.workspace_root, "reflections.jsonl")
                .ok();
        let mut logged_count = 0usize;
        for lesson in &lessons {
            if let Ok(line) = serde_json::to_string(lesson)
                && stella_store::append_workspace_private_line(
                    &self.workspace_root,
                    "reflections.jsonl",
                    &line,
                )
                .is_ok()
            {
                logged_count += 1;
            }
        }
        if let Some(log_path) = &log_path {
            self.auto_create_skills(log_path, quiet);
        }

        if !quiet {
            let n = lessons.len();
            if stored {
                println!(
                    "  {} remembered {n} lesson(s) from this turn",
                    "✦".magenta()
                );
            } else if logged_count == n {
                println!(
                    "  {} could not persist {n} lesson(s) to the context store \
                     (logged to reflections.jsonl only)",
                    "!".yellow()
                );
            } else if logged_count > 0 {
                println!(
                    "  {} could not persist {n} lesson(s) to the context store; \
                     {logged_count} of {n} reached reflections.jsonl",
                    "!".yellow()
                );
            } else {
                println!(
                    "  {} could not persist {n} lesson(s) — both the context store \
                     and reflections.jsonl writes failed",
                    "!".yellow()
                );
            }
        }
        ReflectionReport {
            recorded: if stored { lessons.len() } else { 0 },
            model_error: None,
            cost_usd: reflection_cost_usd,
            events: reflection_events,
        }
    }

    /// Drop lessons that restate something the user forgot.
    ///
    /// Best-effort in one direction only: if the tombstone table cannot be
    /// read we keep every lesson rather than silently discarding the turn's
    /// learning. That is the opposite posture from recall — there, an
    /// unreadable suppression set means surface nothing; here it means
    /// persist everything. Both choices fail toward the recoverable outcome:
    /// a memory that should have been suppressed can be forgotten again,
    /// whereas a lesson dropped on a transient read error is gone for good.
    fn retain_unforgotten(&self, lessons: Vec<ReflectionLesson>) -> Vec<ReflectionLesson> {
        let forgotten = match stella_store::Store::open(&self.workspace_root)
            .and_then(|store| store.forgotten_texts(stella_store::ContextSurface::Memory))
        {
            Ok(texts) if !texts.is_empty() => texts,
            _ => return lessons,
        };
        lessons
            .into_iter()
            .filter(|l| {
                !stella_store::is_suppressed(&l.lesson, forgotten.iter().map(String::as_str))
            })
            .collect()
    }

    /// Mine the whole reflection log for recurring lessons and auto-create
    /// skills for any that qualify (threshold + session cap + no-clobber
    /// enforced by `stella_core::skills`).
    ///
    /// The log is append-only and re-read in full every reflection turn, so
    /// lines written before a tombstone existed are still in it. Filtering
    /// the observations here is what stops a forgotten lesson from returning
    /// as an auto-created skill — the second door that made a plain `DELETE`
    /// insufficient.
    pub(super) fn auto_create_skills(&mut self, log_path: &Path, quiet: bool) {
        // Reading and writing the workspace skills directory are one authority.
        // Without `include_workspace_skills` the loader is handed an empty
        // workspace dir, so a skill written there would never be read back —
        // and creating it anyway used to compute the target from
        // `workspace_skills_dir()` unconditionally, writing into a directory
        // the session had just been told it may not read (#737). The narrower
        // alternative — redirect creation to the user-global dir — is worse:
        // it would escalate prose mined under an untrusted workspace into the
        // scope that applies to every other workspace.
        if !self.include_workspace_skills {
            return;
        }
        let Ok(log) = std::fs::read_to_string(log_path) else {
            return;
        };
        // Every surface a background loop can regenerate; `ContextSurface` owns which.
        let forgotten: Vec<String> = stella_store::Store::open(&self.workspace_root)
            .and_then(|store| {
                let mut texts = Vec::new();
                for surface in stella_store::ContextSurface::restatement_suppressing() {
                    texts.extend(store.forgotten_texts(surface)?);
                }
                Ok(texts)
            })
            .unwrap_or_default();

        let observations: Vec<SkillObservation> = log
            .lines()
            .filter_map(|line| serde_json::from_str::<ReflectionLesson>(line).ok())
            .filter(|l| {
                !stella_store::is_suppressed(&l.lesson, forgotten.iter().map(String::as_str))
            })
            .map(|l| SkillObservation {
                reference: format!("reflection:{}", l.occurred_at),
                text: l.lesson,
                domains: l.domains,
                occurred_at: l.occurred_at,
                salient: false,
            })
            .collect();
        if observations.is_empty() {
            return;
        }

        let existing = self.load_skills();
        let candidates =
            skills::mine_skill_candidates(observations, &existing, &SkillMineConfig::default());

        let skills_dir = self.workspace_skills_dir();
        // What the no-clobber guard needs is the set of paths that are
        // OCCUPIED, which is a filesystem question — not the set that loaded,
        // which is what this used to pass. A skill disabled from the SKILLS tab
        // keeps its file by design and drops out of `load_skills()`, so a
        // re-mined candidate (identity is a stable `{slug}-{hash8}`, so it
        // re-targets the very same path) sailed past the guard and
        // `std::fs::write` destroyed the user's edits (#737). The loaded paths
        // are still unioned in: they cost nothing, they carry the "known skill"
        // signal, and they keep the guard armed for already-loaded skills if
        // the directory read ever fails.
        let mut occupied_paths = skill_paths_on_disk(&skills_dir);
        occupied_paths.extend(existing.iter().map(|s| s.source_path.clone()));
        let config = AutoCreateConfig::default();
        for candidate in candidates {
            match skills::decide_auto_creation(
                &candidate,
                &skills_dir,
                &occupied_paths,
                self.skills_created,
                &config,
            ) {
                AutoCreateDecision::Create { path } => {
                    let markdown = skills::render_skill_markdown(&candidate);
                    let path = PathBuf::from(path);
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if std::fs::write(&path, markdown).is_ok() {
                        self.skills_created += 1;
                        if !quiet {
                            println!(
                                "  {} new skill auto-created from recurring observations: {} ({})",
                                "✦".magenta().bold(),
                                candidate.name.bright_magenta(),
                                path.display()
                            );
                        }
                    }
                }
                AutoCreateDecision::Skip { .. } => {}
            }
        }
    }
}
