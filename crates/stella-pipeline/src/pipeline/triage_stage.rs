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
        let resolved = match self.resolve_provider(Role::Triage) {
            Ok(r) => r,
            // Triage resolution failure is soft: fall through to the full path
            // via the deterministic floor. Never fail the run on triage.
            // The conversational route is still resolved deterministically here
            // (`resolve_conversational(false, goal)`) — a bare greeting must
            // route to chat even when the triage provider can't be resolved,
            // since it never depends on a model answer.
            Err(_) => {
                return Ok((
                    TaskAssessment {
                        conversational: resolve_conversational(false, goal),
                        ..TaskAssessment::from_class(resolve_task_class(None, goal))
                    },
                    Vec::new(),
                ));
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
            Err(RawCallError::Budget(abort)) => return Err(abort),
            Err(RawCallError::Provider | RawCallError::Timeout) => None,
        };
        let assessment = response.as_deref().and_then(parse_triage_response);
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
}
