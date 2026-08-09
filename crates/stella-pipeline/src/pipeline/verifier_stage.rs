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

/// The once-per-run verifier notices, grouped where their emitters live: both
/// flags describe the run's *configuration* (a same-family caveat, a dead or
/// non-compliant verifier), which cannot change mid-run, so each is surfaced
/// to the transcript exactly once however many calls observe it.
#[derive(Default)]
pub(super) struct VerifierNotices {
    /// Whether the router's same-family degradation caveat (L-M8) has been
    /// surfaced — see [`Pipeline::warn_verifier_caveat`].
    caveat_warned: AtomicBool,
    /// Whether a verdict's degradation to the deterministic heuristic has
    /// been surfaced — see [`Pipeline::warn_verifier_fallback`].
    fallback_warned: AtomicBool,
}

/// One candidate's verdict-degradation record: its ordinal (1-based,
/// [`ProofStep::Oracle`]'s `run` convention).
///
/// Per candidate, because a best-of-N fan-out degrading N times used to
/// leave one prose caveat and no record of *which* candidates the heuristic
/// judged (#1787). The proof fact itself is emitted per OCCURRENCE, not once
/// per candidate (#2129): deduplicating it made a round-2 degradation
/// invisible in aggregate telemetry — an `extract-elf` trace held two
/// degraded verdicts and one proof event — and unlike the prose warning
/// below, a proof stream is a record of what happened, not a notice to a
/// reader. Plain candidate-local state, never a shared flag: candidates run
/// concurrently.
pub(super) struct VerdictDegradation {
    candidate: u32,
}

impl VerdictDegradation {
    pub(super) fn new(candidate: u32) -> Self {
        Self { candidate }
    }
}

impl<'a> Pipeline<'a> {
    /// One distress-guidance call: best-effort and never a verdict — the
    /// failure it reacts to is already deterministic, so the verifier's job
    /// here is *steering*, not re-judging. A failed call (or an unresolvable
    /// verifier) degrades to evidence-only revision.
    ///
    /// Takes the built [`guidance_prompt`] rather than its ingredients: the
    /// prompt is decision-shaped and lives in [`crate::verify`]; this module
    /// is only the I/O seam (its own doc contract).
    pub(super) async fn verifier_guidance(
        &self,
        prompt: ManagementPrompt,
        budget: &mut BudgetGuard,
        total: &mut f64,
    ) -> Result<Option<String>, PipelineBudgetAbort> {
        // A withheld or unresolvable guidance agent degrades identically:
        // evidence-only revision. Both land above the stage event, so an
        // ablated guidance call leaves no frame (#2381) — and unlike the
        // verdict below, nothing needs to be *said* about it, because
        // guidance is steering rather than proof and its absence cannot make
        // a run look more verified than it is.
        let Assigned::To(resolved) = self.assigned(ModelCallRole::DistressGuidance) else {
            return Ok(None);
        };
        if let Some(fb) = &resolved.fallback {
            self.emit_fallback(fb);
        }
        self.warn_verifier_caveat(&resolved);
        self.emit(AgentEvent::Stage {
            name: StageKind::Verdict,
        });
        let messages = prompt.into_messages();
        match self
            .metered_raw_call(
                RawCall {
                    role: ModelCallRole::DistressGuidance,
                    resolved: &resolved,
                    messages,
                    policy: RetryPolicy::deterministic(),
                    overrides: &self.config.role_overrides.verifier,
                    // The worker's per-call ceiling (`model_timeout`, #1211/#1277) —
                    // guidance is best-effort (`Ok(None)` on any failure below), so
                    // a wedged call degrades to evidence-only revision instead of
                    // stalling the run on the clock that scores it (#1483).
                    timeout: self.config.engine.model_timeout,
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

    /// One escalated-verdict call. Takes the built [`verifier_prompt`] for
    /// the same reason [`Self::verifier_guidance`] does; `inputs` stays,
    /// because the heuristic fallback is decided here, at the seam where the
    /// call can fail.
    pub(super) async fn verifier(
        &self,
        degradation: &VerdictDegradation,
        prompt: ManagementPrompt,
        inputs: &LadderInputs,
        spend: &mut Spend<'_>,
    ) -> Result<ModelVerifierVerdict, PipelineBudgetAbort> {
        // #2381 requirement 3, and the one ablation that could quietly turn a
        // run into a claimed-done. Checked BEFORE the stage event so the
        // ablation leaves no frame, and routed to the ABSTENTION rung rather
        // than to the degradation rung below: nothing degraded here — the
        // operator removed the stage — so the run must report that no model
        // reviewed it, which is what makes the resulting `passed: true` read
        // as "nothing failed this" instead of as proof.
        if !self.responsibility_enabled(ModelCallRole::Verdict) {
            return Ok(self.verdict_withheld(inputs));
        }
        self.emit(AgentEvent::Stage {
            name: StageKind::Verdict,
        });
        let resolved = match self.assigned(ModelCallRole::Verdict) {
            Assigned::To(r) => r,
            // Verifier unresolvable → conservative heuristic verdict (L-E11).
            Assigned::Withheld | Assigned::Unresolvable => {
                self.warn_verifier_fallback(
                    degradation,
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

        let messages = prompt.into_messages();
        // Deterministic policy: a verifier call that fails must not hang; it falls
        // back to the heuristic verdict rather than retrying. The timeout is the
        // other half of that contract (#1483) — the worker's per-call ceiling
        // (`model_timeout`, #1211/#1277) so a hung verifier is abandoned onto the
        // heuristic fallback instead of stalling the run on the clock that
        // scores it, the same posture `triage_latency_ceiling` gives triage.
        match self
            .metered_raw_call(
                RawCall {
                    role: ModelCallRole::Verdict,
                    resolved: &resolved,
                    messages,
                    policy: RetryPolicy::deterministic(),
                    overrides: &self.config.role_overrides.verifier,
                    timeout: self.config.engine.model_timeout,
                },
                spend.budget,
                spend.total,
            )
            .await
        {
            Ok(result) => match parse_verifier_response(&result.text) {
                Some(mut verdict) => {
                    // The grader-independence fact (#1795), stated where the
                    // resolution that graded is in hand. `None` when the
                    // worker itself will not resolve — nothing to compare —
                    // never a guess in either direction.
                    verdict.verifier_independent = self
                        .resolve_provider(Role::Worker)
                        .ok()
                        .map(|worker| worker.model_ref != resolved.model_ref);
                    Ok(verdict)
                }
                None => {
                    self.warn_verifier_fallback(
                        degradation,
                        "the verifier's response did not follow the verdict protocol",
                    );
                    Ok(heuristic_fallback(inputs))
                }
            },
            Err(RawCallError::Budget(abort)) => Err(abort),
            Err(RawCallError::Provider | RawCallError::Timeout) => {
                self.warn_verifier_fallback(degradation, "the verifier call failed or timed out");
                Ok(heuristic_fallback(inputs))
            }
        }
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
            && !self
                .verifier_notices
                .caveat_warned
                .swap(true, Ordering::Relaxed)
        {
            self.warn(caveat.clone());
        }
    }

    /// Surface a verdict's degradation to the deterministic heuristic — the
    /// prose warning once per run, like the caveat above, because the
    /// escalation loop can hit the same dead verifier several times and the
    /// transcript needs the fact, not an echo; and the structured
    /// [`ProofStep::VerdictDegraded`] fact on EVERY occurrence, carrying the
    /// candidate ordinal the run-wide warning cannot (#1787). Per occurrence
    /// rather than per candidate (#2129): the proof stream is the record
    /// aggregate telemetry reads, and deduplicating it hid every degradation
    /// after a candidate's first.
    fn warn_verifier_fallback(&self, degradation: &VerdictDegradation, why: &str) {
        if !self
            .verifier_notices
            .fallback_warned
            .swap(true, Ordering::Relaxed)
        {
            self.warn(format!(
                "the verifier could not render a model verdict — {why}; this round's verdict \
                 falls back to a deterministic heuristic"
            ));
        }
        self.emit_proof(ProofStep::VerdictDegraded {
            candidate: degradation.candidate,
            reason: why.to_string(),
        });
    }
}
