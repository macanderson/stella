// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The plan stage: one planner call, a bounded JSON-repair retry (L-V2), and
//! (#2932) a second bounded retry for a plan that parsed cleanly but named
//! an absolute filesystem path — see [`Pipeline::resolve_plan_paths`] in
//! `plan_steps.rs` for that half. Split out of `pipeline.rs` (a god file
//! closed to growth) so the second retry had somewhere to land that was not
//! a baseline exception.

use super::*;

impl<'a> Pipeline<'a> {
    /// `revision` is the reviewer's note from a rejected scope card, or `None`
    /// for a turn's first plan. `spend` bundles budget + total as downstream
    /// does: #1778's `research` param took the pair one over clippy's cap.
    pub(super) async fn plan_stage(
        &self,
        goal: &str,
        recall: &[RecalledFrame],
        research: &[ResearchFinding],
        repo_structure: &str,
        revision: Option<&str>,
        spend: &mut Spend<'_>,
    ) -> Result<Vec<PlanStep>, PipelineStageAbort> {
        self.emit_stage(StageKind::Plan);
        let fallback_plan = || vec![PlanStep::new(goal)];

        let resolved = match self.assigned(ModelCallRole::Plan) {
            Assigned::To(r) => r,
            Assigned::Withheld | Assigned::Unresolvable => return Ok(fallback_plan()),
        };
        if let Some(fb) = &resolved.fallback {
            self.emit_fallback(fb);
        }

        let prompt = build_planner_prompt(goal, recall, research, repo_structure, revision);
        // The planner's own row, which the caller resolves as `agents.plan`
        // over `agents.worker` field by field — so plan still rides the
        // worker's settings whenever nobody has said otherwise (#2416), and
        // an operator who *has* said otherwise is obeyed (#2374). The
        // non-prompt knobs also arrive via `config.engine` (built from the
        // worker's tuning); `prompt` has no seat there and reaches the planner
        // only here, prepended as a system message ahead of
        // `PLANNER_INSTRUCTIONS` so the JSON-array contract `parse_plan` reads
        // is never replaced.
        let plan_overrides = &self.config.role_overrides.plan;
        let result = match self
            .metered_raw_call(
                RawCall {
                    role: ModelCallRole::Plan,
                    resolved: &resolved,
                    messages: prompt.into_messages(),
                    policy: RetryPolicy::standard(),
                    overrides: plan_overrides,
                    timeout: self.config.engine.model_timeout,
                },
                spend.budget,
                spend.total,
            )
            .await
        {
            Ok(r) => r,
            // Still before execute: a fallback plan here would only buy the
            // worker turns the run has no clock left to run.
            Err(RawCallError::Budget(abort) | RawCallError::Deadline(abort)) => return Err(abort),
            Err(RawCallError::Provider | RawCallError::Timeout) => return Ok(fallback_plan()),
        };

        if let Some(steps) = parse_plan(&result.text) {
            return self
                .resolve_plan_paths(steps, &resolved, plan_overrides, spend)
                .await;
        }

        // One bounded JSON-repair retry (L-V2), deterministic (no retry-hang).
        match self
            .metered_raw_call(
                RawCall {
                    role: ModelCallRole::PlanRepair,
                    resolved: &resolved,
                    messages: plan_repair_prompt(&result.text).into_messages(),
                    policy: RetryPolicy::deterministic(),
                    overrides: plan_overrides,
                    timeout: self.config.engine.model_timeout,
                },
                spend.budget,
                spend.total,
            )
            .await
        {
            Ok(repair) => {
                if let Some(steps) = parse_plan(&repair.text) {
                    return self
                        .resolve_plan_paths(steps, &resolved, plan_overrides, spend)
                        .await;
                }
            }
            Err(RawCallError::Budget(abort) | RawCallError::Deadline(abort)) => return Err(abort),
            Err(RawCallError::Provider | RawCallError::Timeout) => {}
        }

        // Degrade to a single-step plan rather than failing — a planner that
        // won't produce a parseable plan must still let the work proceed.
        Ok(fallback_plan())
    }
}
