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

    /// One escalated-verdict call. Takes the built [`verifier_prompt`] for
    /// the same reason [`Self::verifier_guidance`] does; `inputs` stays,
    /// because the heuristic fallback is decided here, at the seam where the
    /// call can fail.
    pub(super) async fn verifier(
        &self,
        prompt: ManagementPrompt,
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
