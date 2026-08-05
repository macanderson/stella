//! The verifier escalation seam: the two Role::Verifier model calls the pipeline
//! makes — the verdict on inconclusive evidence and the distress-guidance
//! steering call — plus the once-per-run surfacing of the router's
//! same-family degradation caveat. Split out of `pipeline.rs` for the same
//! reason `witness_stage.rs` was: one nameable concern, and `pipeline.rs` is
//! already the crate's largest file.
//!
//! Everything decision-shaped stays in [`crate::verify`] (prompts, response
//! parsing, the heuristic fallback); this module is only the I/O seam that
//! needs `&self`.

use super::*;

impl<'a> Pipeline<'a> {
    /// One distress-guidance call ([`guidance_prompt`]): best-effort and
    /// never a verdict — the failure it reacts to is already deterministic,
    /// so the verifier's job here is *steering*, not re-judging. A failed call
    /// (or an unresolvable verifier) degrades to evidence-only revision.
    pub(super) async fn verifier_guidance(
        &self,
        goal: &str,
        diff: &str,
        evidence_summary: &str,
        diff_ctx: &DiffContext<'_>,
        budget: &mut BudgetGuard,
        total: &mut f64,
    ) -> Result<Option<String>, PipelineBudgetAbort> {
        let resolved = match self.resolve_provider(Role::Verifier) {
            Ok(resolved) => resolved,
            Err(_) => return Ok(None),
        };
        if let Some(fb) = &resolved.fallback {
            self.emit_fallback(fb);
        }
        self.warn_verifier_caveat(&resolved);
        self.emit(AgentEvent::Stage {
            name: StageKind::Verdict,
        });
        let prompt = guidance_prompt(goal, diff, evidence_summary, diff_ctx);
        let messages = prompt.into_messages();
        match self
            .metered_raw_call(
                RawCall {
                    role: ModelCallRole::DistressGuidance,
                    resolved: &resolved,
                    messages,
                    policy: RetryPolicy::deterministic(),
                    overrides: &self.config.role_overrides.verifier,
                    timeout: None,
                },
                budget,
                total,
            )
            .await
        {
            Ok(result) => {
                let text = result.text.trim().to_string();
                if text.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(text))
                }
            }
            Err(RawCallError::Budget(abort)) => Err(abort),
            Err(RawCallError::Provider | RawCallError::Timeout) => Ok(None),
        }
    }

    /// Best-effort distress-guidance course-correction, appended to `reason`
    /// when the verifier returns one — the second-consecutive-failure trigger
    /// in `Pipeline::verify_candidate`. Same witness exclusion as the verdict
    /// call (#1433): guidance reads the change under correction, and the
    /// verifier's own test is not part of it. Guidance never carries a delta
    /// baseline (#1431/#1432) — its render is already evidence-scoped, and
    /// "unchanged since a verdict round" is a verdict-shaped claim.
    ///
    /// The arguments are the guidance call's inherent inputs (prompt shaping,
    /// leak scrubbing, call metering); grouping them into a request struct is
    /// a larger refactor than this arity warrants.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn distress_guidance_reason(
        &self,
        goal: &str,
        diff_text: &str,
        witness_paths: &[String],
        evidence_summary: &str,
        sealed: &SealedFailure<'_>,
        reason: &mut String,
        budget: &mut BudgetGuard,
        total: &mut f64,
    ) -> Result<(), PipelineBudgetAbort> {
        let stripped = crate::verify::strip_witness_hunks(diff_text, witness_paths);
        let guidance = self
            .verifier_guidance(
                goal,
                &stripped.diff,
                evidence_summary,
                &DiffContext {
                    witness_paths,
                    previous: None,
                },
                budget,
                total,
            )
            .await?;
        if let Some(guidance) = guidance
            && let Some(text) = self.airlock_forward(&guidance, "distress_guidance", sealed)
        {
            reason.push_str("\n\nIndependent reviewer course-correction:\n");
            reason.push_str(&text);
        }
        Ok(())
    }

    // `diff_ctx` (delta framing #1431 + witness exclusion #1433) and
    // `inputs`/`budget`/`total` (ladder inputs + call metering) are separate
    // concerns threaded through the one escalation call; grouping them into a
    // request struct is a larger refactor than this arity warrants.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn verifier(
        &self,
        goal: &str,
        diff: &str,
        evidence_summary: &str,
        diff_ctx: &DiffContext<'_>,
        inputs: &LadderInputs,
        budget: &mut BudgetGuard,
        total: &mut f64,
    ) -> Result<ModelVerifierVerdict, PipelineBudgetAbort> {
        self.emit(AgentEvent::Stage {
            name: StageKind::Verdict,
        });
        let resolved = match self.resolve_provider(Role::Verifier) {
            Ok(r) => r,
            // Verifier unresolvable → conservative heuristic verdict (L-E11).
            Err(_) => {
                self.warn_verifier_fallback(
                    "the verifier role is unresolvable (no routable provider); check the \
                     `pipeline_verifier_model` provider and its credential",
                );
                return Ok(heuristic_fallback(inputs));
            }
        };
        if let Some(fb) = &resolved.fallback {
            self.emit_fallback(fb);
        }
        self.warn_verifier_caveat(&resolved);

        let prompt = verifier_prompt(goal, diff, evidence_summary, diff_ctx);
        let messages = prompt.into_messages();
        // Deterministic policy: a verifier call that fails must not hang; it falls
        // back to the heuristic verdict rather than retrying.
        match self
            .metered_raw_call(
                RawCall {
                    role: ModelCallRole::Verdict,
                    resolved: &resolved,
                    messages,
                    policy: RetryPolicy::deterministic(),
                    overrides: &self.config.role_overrides.verifier,
                    timeout: None,
                },
                budget,
                total,
            )
            .await
        {
            Ok(result) => match parse_verifier_response(&result.text) {
                Some(verdict) => Ok(verdict),
                None => {
                    self.warn_verifier_fallback(
                        "the verifier's response did not follow the verdict protocol",
                    );
                    Ok(heuristic_fallback(inputs))
                }
            },
            Err(RawCallError::Budget(abort)) => Err(abort),
            Err(RawCallError::Provider | RawCallError::Timeout) => {
                self.warn_verifier_fallback("the verifier call failed or timed out");
                Ok(heuristic_fallback(inputs))
            }
        }
    }

    /// Resolve this round's model verdict for the escalated `ModelVerdict`
    /// arm — witness exclusion (#1433), verdict reuse (#1431 step 1), and the
    /// delta-framing baseline (#1431 remaining scope) all live here, split out
    /// of `pipeline.rs` for the same reason the rest of this module is.
    ///
    /// The pipeline-authored witness's own hunks are stripped from the diff
    /// before it can ride into the paid prompt as "worker-authored data"; the
    /// omission is named in `evidence_summary` (the trusted zone), never
    /// in-band in the diff. A revision that changed nothing the verdict
    /// depends on — goal, stripped diff, evidence, byte for byte — reuses
    /// `state.last_verdict` instead of paying for sampling noise. A fresh
    /// call renders under the delta-framing baseline
    /// (`state.last_verdict_diff`), and only a real model verdict — never a
    /// heuristic fallback, which read nothing — advances either baseline.
    ///
    /// Same arity rationale as [`Self::verifier`]: witness exclusion, verdict
    /// reuse, and call metering are separate concerns threaded through one
    /// escalation call.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn resolve_verdict(
        &self,
        goal: &str,
        evidence_summary: &mut String,
        state: &mut CandidateState,
        witness_paths: &[String],
        inputs: &LadderInputs,
        budget: &mut BudgetGuard,
        total: &mut f64,
    ) -> Result<ModelVerifierVerdict, PipelineBudgetAbort> {
        let stripped = crate::verify::strip_witness_hunks(&state.diff_text, witness_paths);
        if !stripped.omitted.is_empty() {
            evidence_summary.push_str(&format!(
                "; witness_files_omitted_from_diff=[{}] (verifier-authored test, not part of \
                 the change under review)",
                stripped.omitted.join(", ")
            ));
        }
        // Reuse before re-buying (#1431): a revision that changed nothing the
        // verdict depends on — same goal, same diff, same evidence, byte for
        // byte — would re-ask the same question and pay full price for
        // sampling noise. The cached verdict is the same opinion at zero
        // cost, and the appended note steers the worker better than a fresh
        // reading of an unchanged tree ever did.
        let inputs_digest = verdict_inputs_digest(goal, &stripped.diff, evidence_summary);
        let cached = state
            .last_verdict
            .as_ref()
            .filter(|(digest, _)| *digest == inputs_digest)
            .map(|(_, verdict)| verdict.clone());
        let verdict = match cached {
            Some(mut verdict) => {
                verdict.reasoning.push_str(
                    "\n(verdict reused: the goal, diff, and evidence are unchanged since the \
                     previous review round — no new model call was made)",
                );
                verdict
            }
            None => {
                // Delta framing (#1431 remaining scope): a fresh call still
                // renders under the witness exclusion above, plus the
                // previous round's diff as the baseline so unchanged file
                // sections stat-line instead of re-buying their bodies.
                let diff_ctx = DiffContext {
                    witness_paths,
                    previous: state.last_verdict_diff.as_deref(),
                };
                let verdict = self
                    .verifier(
                        goal,
                        &stripped.diff,
                        evidence_summary,
                        &diff_ctx,
                        inputs,
                        budget,
                        total,
                    )
                    .await?;
                // Only pin a real model verdict for reuse. A heuristic
                // fallback is a transient-outage stand-in (unresolvable
                // provider, unparseable response, failed/timed-out call), not
                // the opinion this candidate bought: caching it would
                // suppress recovery on the next round (the verifier may have
                // come back) and graft the "no new model call was made" reuse
                // note onto a fallback that never made one.
                if !verdict.heuristic {
                    state.last_verdict = Some((inputs_digest, verdict.clone()));
                }
                verdict
            }
        };
        if !verdict.heuristic {
            // The delta-framing baseline (#1431) advances only on a verdict a
            // model actually answered: a heuristic fallback read nothing, and
            // must not let the next round stat-line text no model ever saw.
            state.last_verdict_diff = Some(state.diff_text.clone());
        }
        Ok(verdict)
    }

    /// Surface the verifier's resolution-quality caveat — same-family
    /// degradation (L-M8) — as a warning, once per run.
    ///
    /// Once, because the verifier role is resolved on every escalation and every
    /// guidance call, and the caveat describes the *configuration*, not the
    /// call: repeating it each round would bury the transcript in copies of a
    /// fact that cannot change mid-run. Emitted at the verdict/guidance call
    /// sites rather than inside `resolve_provider`, which independence
    /// *checks* also call and which must stay silent (its doc contract).
    fn warn_verifier_caveat(&self, resolved: &ResolvedRole<'_>) {
        if let Some(caveat) = &resolved.caveat
            && !self.verifier_caveat_warned.swap(true, Ordering::Relaxed)
        {
            self.warn(caveat.clone());
        }
    }

    /// Surface a verdict's degradation to the deterministic heuristic — once
    /// per run, like the caveat above, because the escalation loop can hit
    /// the same dead verifier several times and the transcript needs the fact,
    /// not an echo. The ladder rung (`HeuristicFallback`) records *that* it
    /// happened on every round either way; this is the prose account of *why*,
    /// which used to be silent (a configured-on pipeline must never degrade
    /// without saying so and naming a way out).
    fn warn_verifier_fallback(&self, why: &str) {
        if !self.verifier_fallback_warned.swap(true, Ordering::Relaxed) {
            self.warn(format!(
                "the verifier could not render a model verdict — {why}; this round's verdict \
                 falls back to a deterministic heuristic"
            ));
        }
    }
}

/// One digest over everything a model verdict depends on — goal, the (witness
/// -stripped) diff, and the evidence summary. Byte-identical inputs are the
/// same question; [`Pipeline::resolve_verdict`] reuses its previous answer
/// rather than paying for sampling noise (#1431). Within-run only, so the std
/// hasher's stability across versions is irrelevant.
fn verdict_inputs_digest(goal: &str, diff: &str, evidence_summary: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    goal.hash(&mut hasher);
    diff.hash(&mut hasher);
    evidence_summary.hash(&mut hasher);
    hasher.finish()
}
