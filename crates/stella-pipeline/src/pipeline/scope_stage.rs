//! The plan → scope-review phase: build a plan, take it through the gate, and
//! plan again when the reviewer asks for a different scope.
//!
//! One module because the loop is the point. `ScopeDecision::Revise` — the
//! reviewer typing "not like that, do this" instead of pressing a key — is a
//! jump back into the planner, so the two stages cannot be read, or bounded,
//! independently. Split out of `pipeline.rs` on the same grounds as
//! `witness_stage`: a stage with its own control flow reads better beside its
//! own doc than buried in `run`.

use super::*;

impl Pipeline<'_> {
    /// Plan the goal and take it through the scope gate, re-planning while the
    /// reviewer keeps asking for a different scope (bounded by
    /// [`MAX_SCOPE_REVISIONS`]).
    ///
    /// Plan and review are one method because [`ScopeDecision::Revise`] is a
    /// loop back into the planner: a reviewer's "not like that — do this" is
    /// only answerable by planning again with what they said. `Err` is still
    /// only the headless-without-bypass case; everything a human can choose
    /// comes back as [`PlannedScope`].
    pub(super) async fn plan_with_review(
        &self,
        goal: &str,
        frames: &[RecalledFrame],
        research: &[ResearchFinding],
        budget: &mut BudgetGuard,
        total: &mut f64,
    ) -> Result<PlannedScope, PipelineError> {
        let repo_structure = self.repo.structure_summary().await;
        let mut revision: Option<String> = None;
        let mut spent_revisions = 0usize;
        let mut spend = Spend { budget, total };

        loop {
            let plan = match self
                .plan_stage(
                    goal,
                    frames,
                    research,
                    &repo_structure,
                    revision.as_deref(),
                    &mut Spend { budget, total },
                )
                .await
            {
                Ok(plan) => plan,
                Err(abort) => {
                    return Ok(PlannedScope::Ended {
                        reason: abort.reason,
                    });
                }
            };

            match self.scope_review(goal, plan).await? {
                ScopeVerdict::Run(steps) => return Ok(PlannedScope::Steps(steps)),
                ScopeVerdict::Stop => {
                    return Ok(PlannedScope::Ended {
                        reason: "aborted at scope review".to_string(),
                    });
                }
                ScopeVerdict::Revise { note } => {
                    // The cap is on *re-planning*, not on the card: the run
                    // that hits it has already shown the reviewer a card they
                    // can still approve, trim, or abort. Only a further note
                    // is refused, and the refusal says what still works rather
                    // than ending on an unexplained abort.
                    if spent_revisions >= MAX_SCOPE_REVISIONS {
                        self.emit(AgentEvent::Error {
                            message: format!(
                                "scope review: {MAX_SCOPE_REVISIONS} re-plans did not settle on a \
                                 scope you accepted, so this note was not sent to the planner \
                                 again — approve, trim, or abort the plan on screen, or start a \
                                 fresh turn with the goal restated"
                            ),
                            retryable: false,
                        });
                        return Ok(PlannedScope::Ended {
                            reason: "aborted at scope review — revision limit reached".to_string(),
                        });
                    }
                    spent_revisions += 1;
                    // Name the loop-back. Without it a second PLAN stage
                    // appears with no stated cause, which reads as the
                    // pipeline having restarted itself.
                    self.emit(AgentEvent::Text {
                        text: format!(
                            "\n[re-planning ({spent_revisions}/{MAX_SCOPE_REVISIONS}) with your \
                             note: {note}]\n"
                        ),
                    });
                    revision = Some(note);
                }
            }
        }
    }

    /// One pass of the scope gate over an already-built `plan`. `Err` is the
    /// headless-without-bypass case only.
    pub(super) async fn scope_review(
        &self,
        goal: &str,
        plan: Vec<PlanStep>,
    ) -> Result<ScopeVerdict, PipelineError> {
        // Coarse blast-radius estimate: one file per step is a deliberately
        // rough proxy until the planner emits per-step file hints (a
        // documented follow-up). Cost estimate is left to the caller/planner.
        let estimate = ScopeEstimate {
            estimated_files: plan.len() as u32,
            estimated_cost_usd: None,
        };
        // Plan mode (#1264) forces the gate: the thresholds decide whether a
        // plan is *big*, and the user has said they want to see it whatever
        // the size. Checked before the threshold test rather than folded into
        // it so `needs_scope_review` stays a pure function of the plan.
        if !self.config.plan_mode
            && !needs_scope_review(&plan, &estimate, &self.config.scope_thresholds)
        {
            return Ok(ScopeVerdict::Run(plan));
        }

        if self.config.plan_mode && self.config.headless {
            // The bypass below exists for plans that merely *tripped* a
            // threshold. Plan mode is an explicit request to be asked, so
            // honouring a bypass here would silently deliver the opposite of
            // what was asked for — worse than refusing.
            self.emit(AgentEvent::Error {
                message: "--plan-mode asks for the plan to be approved before anything runs, and a headless run has nobody to ask; drop --plan-mode or run interactively"
                    .to_string(),
                retryable: false,
            });
            return Ok(ScopeVerdict::Stop);
        }
        if self.config.headless && self.config.headless_bypass_scope_review {
            // Bypass means proceed, not "ask a gate that always says no".
            // The headless approval port is `AlwaysAbortGate`, so running the
            // review anyway would empty the plan and end the turn having done
            // nothing — the same zero-work outcome as the error below, just
            // spelled differently.
            return Ok(ScopeVerdict::Run(plan));
        }
        if self.config.headless {
            // Say why the run is ending. This error leaves through the
            // `Result`, not the event stream, so without this the stream just
            // stops mid-plan: a consumer reading only events sees a run that
            // vanished with no terminal event and no explanation.
            self.emit(AgentEvent::Error {
                message: format!(
                    "this plan needs scope review ({} steps, ~{} files) and a headless \
                     run has nobody to ask; set `headless_scope_bypass: on` where the \
                     working tree is disposable, or raise the scope thresholds",
                    plan.len(),
                    estimate.estimated_files
                ),
                retryable: false,
            });
            return Err(PipelineError::ScopeReviewRequiredHeadless);
        }

        self.emit(AgentEvent::Stage {
            name: StageKind::ScopeReview,
        });
        let proposal = build_proposal(goal, &plan, &estimate);
        self.emit(AgentEvent::ScopeReview {
            proposal: proposal.clone(),
        });

        match self.approvals.review(&proposal).await {
            ScopeDecision::Approve => Ok(ScopeVerdict::Run(plan)),
            ScopeDecision::Trim { keep_steps } => {
                let trimmed = apply_trim(&plan, &keep_steps);
                if trimmed.is_empty() {
                    // Trimmed to nothing is a choice to run nothing, not a
                    // revision — there is no note to re-plan from.
                    Ok(ScopeVerdict::Stop)
                } else {
                    Ok(ScopeVerdict::Run(trimmed))
                }
            }
            // A blank note is an empty submission, not an instruction: it
            // would re-plan from nothing and produce the same rejected plan.
            ScopeDecision::Revise { note } if note.trim().is_empty() => Ok(ScopeVerdict::Stop),
            ScopeDecision::Revise { note } => Ok(ScopeVerdict::Revise { note }),
            ScopeDecision::Abort => Ok(ScopeVerdict::Stop),
        }
    }
}
