//! The judge escalation seam: the two Role::Judge model calls the pipeline
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
    /// so the judge's job here is *steering*, not re-judging. A failed call
    /// (or an unresolvable judge) degrades to evidence-only revision.
    pub(super) async fn judge_guidance(
        &self,
        goal: &str,
        diff: &str,
        evidence_summary: &str,
        budget: &mut BudgetGuard,
        total: &mut f64,
    ) -> Result<Option<String>, PipelineBudgetAbort> {
        let resolved = match self.resolve_provider(Role::Judge) {
            Ok(resolved) => resolved,
            Err(_) => return Ok(None),
        };
        if let Some(fb) = &resolved.fallback {
            self.emit_fallback(fb);
        }
        self.warn_judge_caveat(&resolved);
        self.emit(AgentEvent::Stage {
            name: StageKind::Judge,
        });
        let prompt = guidance_prompt(goal, diff, evidence_summary);
        match self
            .metered_raw_call(
                RawCall {
                    role: ModelCallRole::DistressGuidance,
                    resolved: &resolved,
                    messages: vec![CompletionMessage::user(prompt)],
                    policy: RetryPolicy::deterministic(),
                    overrides: &self.config.role_overrides.judge,
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

    pub(super) async fn judge(
        &self,
        goal: &str,
        diff: &str,
        evidence_summary: &str,
        inputs: &LadderInputs,
        budget: &mut BudgetGuard,
        total: &mut f64,
    ) -> Result<ModelJudgeVerdict, PipelineBudgetAbort> {
        self.emit(AgentEvent::Stage {
            name: StageKind::Judge,
        });
        let resolved = match self.resolve_provider(Role::Judge) {
            Ok(r) => r,
            // Judge unresolvable → conservative heuristic verdict (L-E11).
            Err(_) => return Ok(heuristic_fallback(inputs)),
        };
        if let Some(fb) = &resolved.fallback {
            self.emit_fallback(fb);
        }
        self.warn_judge_caveat(&resolved);

        let prompt = judge_prompt(goal, diff, evidence_summary);
        // Deterministic policy: a judge call that fails must not hang; it falls
        // back to the heuristic verdict rather than retrying.
        match self
            .metered_raw_call(
                RawCall {
                    role: ModelCallRole::Judge,
                    resolved: &resolved,
                    messages: vec![CompletionMessage::user(prompt)],
                    policy: RetryPolicy::deterministic(),
                    overrides: &self.config.role_overrides.judge,
                    timeout: None,
                },
                budget,
                total,
            )
            .await
        {
            Ok(result) => {
                let verdict = parse_judge_response(&result.text)
                    .unwrap_or_else(|| heuristic_fallback(inputs));
                Ok(verdict)
            }
            Err(RawCallError::Budget(abort)) => Err(abort),
            Err(RawCallError::Provider | RawCallError::Timeout) => Ok(heuristic_fallback(inputs)),
        }
    }

    /// Surface the judge's resolution-quality caveat — same-family
    /// degradation (L-M8) — as a warning, once per run.
    ///
    /// Once, because the judge role is resolved on every escalation and every
    /// guidance call, and the caveat describes the *configuration*, not the
    /// call: repeating it each round would bury the transcript in copies of a
    /// fact that cannot change mid-run. Emitted at the verdict/guidance call
    /// sites rather than inside `resolve_provider`, which independence
    /// *checks* also call and which must stay silent (its doc contract).
    fn warn_judge_caveat(&self, resolved: &ResolvedRole<'_>) {
        if let Some(caveat) = &resolved.caveat
            && !self.judge_caveat_warned.swap(true, Ordering::Relaxed)
        {
            self.warn(caveat.clone());
        }
    }
}
