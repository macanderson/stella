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
        //
        // This gate sits on the dispatcher rather than inside either arm, so
        // it covers the typed path and the lexical one by construction. A
        // guard that protected only one of them would be a guarantee that
        // depended on a feature flag.
        if !self.include_workspace_skills {
            return;
        }
        // Phase 3 (#714). While `context.lifecycle.enabled` is off — the
        // shipped default — this is byte-for-byte the loop that ships today:
        // no ledger write, no typed record, no behavior change of any kind.
        // The typed path is a migration with a behavior-compatibility
        // obligation (spec §8), and the only honest way to hold that
        // obligation is to keep the thing it must stay compatible WITH
        // runnable, so both paths are exercised by the same guarantee suite.
        if self.lifecycle_enabled {
            self.auto_create_skills_typed(log_path, quiet);
        } else {
            self.auto_create_skills_lexical(log_path, quiet);
        }
    }

    /// Every surface a background loop can regenerate; `ContextSurface` owns
    /// which. Shared by both paths so a tombstone means the same thing on each
    /// — a second suppression mechanism is a defect (spec §5.7).
    fn forgotten_texts(&self) -> Vec<String> {
        stella_store::Store::open(&self.workspace_root)
            .and_then(|store| {
                let mut texts = Vec::new();
                for surface in stella_store::ContextSurface::restatement_suppressing() {
                    texts.extend(store.forgotten_texts(surface)?);
                }
                Ok(texts)
            })
            .unwrap_or_default()
    }

    /// Write out whichever candidates the caller decided to keep, under the
    /// per-session cap and the no-clobber guard.
    ///
    /// Shared by both paths, so the cap and the guard are literally the same
    /// code on each — asserted by test rather than by inspection, which is what
    /// spec §8 asks for.
    fn write_candidates(&mut self, candidates: Vec<skills::SkillCandidate>, quiet: bool) {
        let existing = self.load_skills();
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

    /// The typed path: extract observations into the ledger, induce durable
    /// proposals over them, and write only the proposals that are *eligible*.
    ///
    /// Two things differ from the lexical path, and both are the point:
    ///
    /// 1. **Distinct tasks gate, not events.** A cluster of thirty repetitions
    ///    inside one task still mines a candidate — the miner is unchanged —
    ///    but its proposal records `distinct_tasks == 1` and is not eligible,
    ///    so no file is written. Spec §7's anti-poisoning rule, enforced where
    ///    it can actually be enforced.
    /// 2. **There is a record of why.** The proposal carries its supporting
    ///    observations and its scored components, so "three separate tasks,
    ///    here they are" is answerable before a skill file exists.
    ///
    /// Everything else is identical, deliberately: the same miner, the same
    /// thresholds, the same tombstone sweep, the same cap, the same no-clobber
    /// guard, the same rendering.
    fn auto_create_skills_typed(&mut self, log_path: &Path, quiet: bool) {
        // 1. Evidence → typed, redacted, replay-idempotent observations.
        super::observations::extract_reflection_observations(&self.store, log_path);

        // 2. Tombstones. Filtered at MINE time rather than at extraction time:
        //    ledger records are immutable, so a lesson forgotten after it was
        //    observed cannot be removed from the ledger — and should not be,
        //    since the observation genuinely happened. What must not happen is
        //    that it induces a proposal or a skill, which is what this stops.
        //    Exactly the same reasoning as the lexical path's log filter, and
        //    exactly the same predicate.
        let forgotten = self.forgotten_texts();
        let observations: Vec<_> = super::observations::all_observations(&self.store, 5_000)
            .into_iter()
            .filter(|o| !stella_store::is_suppressed(&o.text, forgotten.iter().map(String::as_str)))
            .collect();
        if observations.is_empty() {
            return;
        }

        // 3. Induce durable proposals over the unchanged miner.
        let existing = self.load_skills();
        let induced = super::proposals::induce_proposals(
            &self.store,
            &observations,
            &existing,
            &SkillMineConfig::default(),
        );

        // 4. The re-proposal cooldown. A proposal the user declined must not
        //    come back next turn — that is what makes "Ignore" mean anything.
        //    Read from the promotion event log rather than from a status
        //    column on the proposal: the proposal is immutable, and the
        //    decision is a separate fact about it that can itself be revised.
        //    A later Keep overwrites an earlier Ignore because the fold is
        //    last-write-wins, so declining is reversible, not permanent.
        let declined = crate::proposals_cmd::decisions(&self.store);

        // 5. Only eligible proposals become files. An ineligible one stays in
        //    the ledger as a visible "recurring, but not across enough tasks
        //    yet" — which is information, not a failure.
        let promotion = super::tuning::inferred_directive_promotion(&self.workspace_root);
        let eligible: Vec<_> = induced
            .into_iter()
            .filter(|i| {
                declined.get(&i.proposal.lineage_id)
                    != Some(&stella_core::context_record::PromotionAction::Rejected)
            })
            .filter(|i| {
                i.proposal
                    .is_eligible(promotion.min_distinct_tasks, promotion.min_observations)
            })
            .map(|i| i.candidate)
            .collect();
        self.write_candidates(eligible, quiet);

        // 6. The rules half, over the SAME observations (#714 deliverable 5).
        //    Its miner is the structural twin of the skills one and shares
        //    `stella_core::mining`; running both from one observation pool is
        //    what makes that sharing pay off rather than being decorative.
        self.induce_rules(&observations, &declined, quiet);
    }

    /// Mine rule proposals from the same observations, and auto-activate only
    /// the ones whose evidence is strong enough to earn it.
    ///
    /// Directives steer — a rule is injected into the system prefix as an
    /// instruction — so they clear a higher bar than skills:
    ///
    /// * the same distinct-task eligibility gate, plus
    /// * `confidence >= auto_activate_at_confidence` (85 by default). Three
    ///   observations across three tasks score 70, so the common case is that
    ///   a rule proposal is *recorded and waits for an explicit Keep* rather
    ///   than landing on its own. Auto-activation is for evidence that is
    ///   genuinely strong, which is what spec §5.4's "governed before it takes
    ///   effect" means for something with instruction authority.
    ///
    /// Advisory always: the inferred guard is stripped in
    /// `rules_mining::write_rule`, so a mined rule is Tier 1 and can never deny
    /// a tool call.
    fn induce_rules(
        &mut self,
        observations: &[stella_core::context_record::ObservationRecord],
        declined: &std::collections::HashMap<String, stella_core::context_record::PromotionAction>,
        quiet: bool,
    ) {
        let existing = crate::rules::load_workspace_rules_unfiltered(&self.workspace_root);
        let induced = super::rules_mining::induce_rule_proposals(
            &self.store,
            observations,
            &existing,
            &stella_core::rules::MineConfig::default(),
        );
        let promotion = super::tuning::inferred_directive_promotion(&self.workspace_root);

        for rule in induced {
            if declined.get(&rule.proposal.lineage_id)
                == Some(&stella_core::context_record::PromotionAction::Rejected)
            {
                continue;
            }
            if !rule
                .proposal
                .is_eligible(promotion.min_distinct_tasks, promotion.min_observations)
            {
                continue;
            }
            if rule.proposal.confidence.get() < promotion.auto_activate_at_confidence {
                // Recorded, reviewable, not active. `stella proposals list`
                // shows it; `stella proposals keep` activates it.
                continue;
            }
            if let Some(path) =
                super::rules_mining::write_rule(&self.workspace_root, &rule.candidate)
                && !quiet
            {
                println!(
                    "  {} new advisory rule from recurring observations: {} ({})",
                    "✦".magenta().bold(),
                    rule.candidate.id.bright_magenta(),
                    path.display()
                );
            }
        }
    }

    /// The lexical path exactly as it shipped — reached whenever
    /// `context.lifecycle.enabled` is off, which is the default.
    fn auto_create_skills_lexical(&mut self, log_path: &Path, quiet: bool) {
        let Ok(log) = std::fs::read_to_string(log_path) else {
            return;
        };
        let forgotten = self.forgotten_texts();

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
        self.write_candidates(candidates, quiet);
    }
}
