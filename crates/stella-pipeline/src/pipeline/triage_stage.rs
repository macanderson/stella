//! The triage stage: one latency-capped management call that classifies the
//! goal, decides what assurance its result warrants, and — for multi-step
//! work — names the research questions the pre-plan stage will answer
//! (#1778). Split out of `pipeline.rs` on the same grounds as `witness_stage`
//! and `scope_stage`: a stage with its own protocol reads better beside its
//! own doc than buried in `run`, and `pipeline.rs` is closed to growth.
//!
//! The *decision* logic — the classification vocabulary, the parsers, the
//! deterministic floor and vetoes — is pure and lives in [`crate::triage`];
//! this module owns only the paid call and the resolution ordering around it.

use super::*;

use crate::triage::parse_research_questions;

impl Pipeline<'_> {
    /// Record that this turn's class came from the deterministic keyword floor
    /// rather than from a model (#2414).
    ///
    /// Both channels, for the same reason [`Pipeline::unproven`] uses both:
    /// the warning is the prose account a watching human reads, and the proof
    /// step is the structured record a bench census can count. The fallback
    /// this reports is correct and load-bearing — triage must never fail a
    /// run — but it silently became the common case, and every downstream
    /// decision keyed off the class then looked like a decision while being a
    /// default.
    fn triage_degraded(&self, reason: &str) {
        self.warn(format!(
            "triage did not answer, so this turn's class came from the deterministic keyword \
             floor: {reason}"
        ));
        self.emit_proof(ProofStep::TriageDegraded {
            reason: reason.to_string(),
        });
    }

    /// Classify the goal and resolve the turn's assurance plan.
    ///
    /// Returns the resolved assessment together with the research questions
    /// triage named (#1778) — empty on every fast path: a conversational
    /// route, a failed or unparseable triage call, and any class below
    /// [`TaskClass::MultiStep`]. Gating on the *resolved* class rather than
    /// the model's keeps the deterministic floor authoritative: a response
    /// whose class was vetoed down cannot smuggle a research fan-out in.
    pub(super) async fn triage(
        &self,
        goal: &str,
        budget: &mut BudgetGuard,
        total: &mut f64,
    ) -> Result<(TaskAssessment, Vec<String>), PipelineBudgetAbort> {
        // #2381: the roster may withhold triage entirely. Returning BEFORE the
        // stage event is the point — an ablated stage must leave no frame in
        // `stella-events.jsonl`, so a reader sees the ablation rather than
        // inferring it from a stage that emitted but bought nothing. The
        // fall-through is `triage_ablated`, NOT the plain floor an outage
        // takes: see that function for why an ablation must not also strip
        // verification.
        if !self.responsibility_enabled(ModelCallRole::Triage) {
            return Ok((self.triage_ablated(goal), Vec::new()));
        }
        self.emit(AgentEvent::Stage {
            name: StageKind::Triage,
        });
        // Deterministic short-circuit, BEFORE the paid call.
        //
        // `resolve_conversational` is a disjunction whose first term ignores
        // the model entirely, so a `true` here with `model_says_chat = false`
        // means the greeting arm fired — and no triage answer could change the
        // outcome. Classifying `hi` used to cost a full round-trip plus, on a
        // wedged provider, up to `triage_latency_ceiling` of dead air, for a
        // route the module docs already describe as never depending on a model
        // answer. This is the same assessment the resolution-failure arm below
        // builds; it just stops paying for it first.
        if resolve_conversational(false, goal) {
            return Ok((
                TaskAssessment {
                    conversational: true,
                    ..TaskAssessment::from_class(resolve_task_class(None, goal))
                },
                Vec::new(),
            ));
        }
        let resolved = match self.assigned(ModelCallRole::Triage) {
            Assigned::To(r) => r,
            // Triage resolution failure is soft: fall through to the full path
            // via the deterministic floor. Never fail the run on triage.
            // The conversational route is still resolved deterministically
            // there — a bare greeting must route to chat even when the triage
            // provider can't be resolved, since it never depends on a model
            // answer.
            Assigned::Withheld | Assigned::Unresolvable => {
                return Ok((self.triage_floor(goal), Vec::new()));
            }
        };
        if let Some(fb) = &resolved.fallback {
            self.emit_fallback(fb);
        }

        let response = match self
            .metered_raw_call(
                RawCall {
                    role: ModelCallRole::Triage,
                    resolved: &resolved,
                    messages: triage_prompt(goal, &self.repo.structure_summary().await)
                        .into_messages(),
                    policy: RetryPolicy::deterministic(),
                    overrides: &self.config.role_overrides.triage,
                    timeout: Some(self.config.triage_latency_ceiling),
                },
                budget,
                total,
            )
            .await
        {
            Ok(result) => Some(result.text),
            // Triage is the run's first paid call: an expired clock here means
            // nothing has been produced, so stopping is honest — there is no
            // partial work a pivot could settle with.
            Err(RawCallError::Budget(abort) | RawCallError::Deadline(abort)) => return Err(abort),
            // Timeout and provider error are named apart, because the two cost
            // the run very different things and only one of them is a bill for
            // dead air. A census of these records is what turns "triage is
            // slow" into a number (#2414).
            Err(RawCallError::Timeout) => {
                self.triage_degraded(&format!(
                    "the triage call timed out at its {:?} ceiling",
                    self.config.triage_latency_ceiling
                ));
                None
            }
            Err(RawCallError::Provider) => {
                self.triage_degraded("the triage call failed at the provider");
                None
            }
        };
        let assessment = response.as_deref().and_then(parse_triage_response);
        // A response that arrived and said nothing the protocol recognizes is
        // the same outcome as no response — the class comes from the keyword
        // floor either way — so it is the same record. Only reported when the
        // call itself succeeded; a failure already reported above.
        if response.is_some() && assessment.is_none() {
            self.triage_degraded("the triage response did not follow the classification protocol");
        }
        // The class still goes through `resolve_task_class` so a failed or
        // unparseable triage lands on the deterministic floor exactly as
        // before; a real assessment keeps its own assurance flags.
        // Resolve the conversational route once, up front: it must hold even
        // when the triage model call failed/was unparseable (the `None` arm),
        // because a bare greeting is deterministic and should never depend on a
        // model answer. `resolve_conversational` also applies the floor veto to
        // an over-eager model `chat` — a goal with real task signal is work.
        //
        // A headless run never routes to chat on the model's opinion: its goal
        // arrived from a script, a CI job, or a benchmark harness, so there is
        // nobody chatting, and the chat path is terminal no-work — a misroute
        // there silently drops the task with no revision possible. The
        // deterministic greeting arm above stays (`stella run "thanks"` is
        // still not a task); only the model's say is withheld.
        let model_says_chat =
            !self.config.headless && assessment.map(|a| a.conversational).unwrap_or(false);
        let conversational = resolve_conversational(model_says_chat, goal);
        // The witness decision is resolved here for the same reason as the
        // conversational one: it must hold even when the triage call failed or
        // was unparseable. `resolve_witness` is the deterministic *ceiling* —
        // the mirror of the floor above, and the only thing allowed to move
        // assurance down. It fires on one shape (a bare deletion of a named
        // artifact) where an authored witness has nothing to fail against and
        // the author can only invent something vacuous.
        let resolved = match assessment {
            Some(assessment) => {
                let class = resolve_task_class(Some(assessment.class), goal);
                TaskAssessment {
                    class,
                    conversational,
                    require_witness: Some(resolve_witness(assessment.require_witness, class, goal)),
                    ..assessment
                }
            }
            None => {
                let class = resolve_task_class(None, goal);
                TaskAssessment {
                    conversational,
                    require_witness: Some(resolve_witness(None, class, goal)),
                    ..TaskAssessment::from_class(class)
                }
            }
        };
        // Research questions ride only on the full path (#1778): the resolved
        // class must genuinely plan, and a conversational turn does no work.
        // Everything cheaper degrades to the empty set — the stage that reads
        // this treats empty as "skip", so the fast paths (L-E2) never pay.
        //
        // #2415 asked whether `single` should be allowed to ask them too, and
        // the answer here is **not yet**, deliberately. The case for widening
        // is real — a `single` turn has no plan and, until #2415's other half,
        // had no findings either, which is exactly where a worker is most
        // exposed. But the ask is not free where it is made: the `RESEARCH:`
        // line is what moved triage from ~17 output tokens to 330–778, and 27
        // of 34 triage calls in the runs censused for #2414 then burned the
        // full latency ceiling and returned nothing at all. Widening the class
        // that pays for the ask would buy more of that before triage's own
        // latency is back inside its ceiling — and a triage that times out
        // supplies neither research questions nor a class.
        //
        // So the ordering is: fix what research already costs us (its findings
        // now reach the worker, not the planner alone), then re-measure triage,
        // then revisit this gate. Tracked as the follow-up on #2415.
        let research = if resolved.class.plans() && !resolved.conversational {
            response
                .as_deref()
                .map(parse_research_questions)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        // The turn's assurance PLAN, published the moment it is decided and
        // before any later stage can fail, abort, or decline to run.
        //
        // Every other proof step reports something that happened, so the most
        // common outcome by far — triage deciding this change does not warrant
        // a test — used to produce no steps at all and leave the surface with
        // nothing to say about the thing it exists to say. A declared plan
        // makes "we chose not to" a statement rather than an absence.
        //
        // Not emitted for a conversational turn: there is no work, so there is
        // no assurance question, and answering an unasked one is noise.
        if !resolved.conversational {
            self.emit_proof(ProofStep::Assurance {
                witness: resolved.wants_witness(),
                verifier: resolved.wants_verifier(),
            });
        }
        Ok((resolved, research))
    }

    /// The assessment a turn gets when no triage answer is available — the
    /// deterministic floor, and nothing else.
    ///
    /// One function because three paths reach it and they must agree: triage
    /// disabled by the roster (#2381), triage unresolvable, and — historically —
    /// each written out separately, which is how the conversational resolution
    /// came to be spelled two ways. `resolve_conversational(false, goal)`
    /// rather than `false`, because a bare greeting routes to chat on the
    /// deterministic arm alone and must not depend on a model answer that was
    /// never bought.
    fn triage_floor(&self, goal: &str) -> TaskAssessment {
        TaskAssessment {
            conversational: resolve_conversational(false, goal),
            ..TaskAssessment::from_class(resolve_task_class(None, goal))
        }
    }

    /// The assessment a turn gets when triage was **deliberately ablated**
    /// (#2381) — which is not the same thing as triage having failed, and the
    /// difference is the whole point of the roster.
    ///
    /// [`Self::triage_floor`] is right for an outage: triage broke, so spend
    /// as little as the evidence justifies. It is wrong for an ablation,
    /// because the floor's cheapest class is [`TaskClass::SimpleLookup`],
    /// which skips the verification ladder — so turning triage off would
    /// silently turn *verification* off too on any goal whose keywords the
    /// floor did not recognise. An operator ablating one stage to measure it
    /// would have quietly ablated two, and the measurement would attribute
    /// both effects to triage (#2374 F6).
    ///
    /// So the fast path — the thing triage *buys* — is what disappears with
    /// triage, and the floor's own upward evidence is what survives it:
    /// clamped at [`TaskClass::SingleTask`], a genuinely multi-step goal still
    /// plans, and nothing routes down to the lookup path without a
    /// classification that earned it.
    ///
    /// The conversational route is deliberately still resolved: it is
    /// deterministic (`resolve_conversational(false, goal)` consults no model),
    /// so a bare `hi` must not enter the work pipeline merely because triage
    /// was ablated.
    fn triage_ablated(&self, goal: &str) -> TaskAssessment {
        let floor = self.triage_floor(goal);
        TaskAssessment {
            class: floor.class.max(TaskClass::SingleTask),
            ..floor
        }
    }
}
