// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The seam between [`crate::roster`] — which is pure, and knows only which
//! agent owns which responsibility — and the router, which knows which model
//! an agent resolves to.
//!
//! Every call site that used to write `resolve_provider(Role::Verifier)`
//! literally now asks [`Pipeline::assigned`] for a *responsibility* instead, so
//! the binding it used to hard-code is read from configuration. The three
//! answers are distinct on purpose: "the operator turned this off" and "this is
//! configured but nothing routes it" are different facts, and collapsing them
//! is how an ablation would come to look like an outage.
//!
//! Split from `pipeline.rs` because that file is a grandfathered god file
//! closed to growth (AGENTS.md § God files) — the same reason `triage_stage`
//! and `witness_stage` live beside it.

use super::run_error::Independence;
use super::*;

/// What the roster says about one responsibility, resolved against the router.
pub(super) enum Assigned<'p> {
    /// Make the call, with this resolution.
    To(ResolvedRole<'p>),
    /// The roster does not run this responsibility. Every call site treats
    /// this as "take the path you would have taken without it" — the
    /// degradation each stage already had for its own unavailability — and
    /// emits no [`StageKind`] frame, so a recorded stream shows the ablation
    /// rather than leaving a reader to infer it from an absent call (#2381,
    /// requirement 2).
    Withheld,
    /// The roster assigns it, but the router or the provider resolver
    /// refused. Kept distinct from [`Self::Withheld`] because the existing
    /// per-stage degradations were written for exactly this case and their
    /// wording ("unresolvable", "check the credential") would be a lie about
    /// a deliberate ablation.
    ///
    /// Carries no [`RoleResolveError`]: every site that reaches this variant
    /// degrades softly and already discarded the cause before this type
    /// existed. A payload nothing reads would be the unwired code AGENTS.md
    /// asks not to ship — the hard-failure path keeps its typed error, via
    /// [`Pipeline::resolve_provider`] and [`PipelineError::Routing`].
    Unresolvable,
}

impl<'p> Assigned<'p> {
    /// The resolution, or `None` for either non-answer.
    ///
    /// For the call sites whose degradation is identical either way — the
    /// witness author, which simply does not author. A site that must tell an
    /// ablation from an outage matches the variants instead.
    pub(super) fn ok(self) -> Option<ResolvedRole<'p>> {
        match self {
            Self::To(resolved) => Some(resolved),
            Self::Withheld | Self::Unresolvable => None,
        }
    }
}

impl<'a> Pipeline<'a> {
    /// Resolve the agent the roster assigns to `responsibility`.
    ///
    /// This is the only place a responsibility becomes a [`Role`]. Deliberately
    /// silent — it emits nothing at all — because it is called from
    /// independence probes as well as from call sites, and a probe that warned
    /// would warn on a question rather than on an action (the same contract
    /// [`Pipeline::resolve_provider`] carries, and for the same reason).
    pub(super) fn assigned(&self, responsibility: ModelCallRole) -> Assigned<'a> {
        let Some(role) = self.config.roster.role(responsibility) else {
            return Assigned::Withheld;
        };
        match self.resolve_provider(role) {
            Ok(resolved) => Assigned::To(resolved),
            Err(_) => Assigned::Unresolvable,
        }
    }

    /// Whether the roster runs this responsibility at all.
    ///
    /// For the stages whose question is "should I even start" rather than
    /// "which model serves this" — witness authoring, whose enablement is
    /// decided before any resolution happens.
    pub(super) fn responsibility_enabled(&self, responsibility: ModelCallRole) -> bool {
        self.config.roster.enabled(responsibility)
    }

    /// Refuse the run if the roster says anything that cannot be honoured.
    ///
    /// One call over [`Roster::validate`], which is total — key-level
    /// rejections are recorded on the roster itself — so this cannot miss a
    /// problem by consulting the wrong half. Every problem is joined into one
    /// message rather than reported one run at a time, because a hand-written
    /// block usually carries more than one and each round trip costs the
    /// operator a whole run.
    pub(super) fn roster_refusal(&self) -> Result<(), PipelineError> {
        let problems = self.config.roster.validate();
        if problems.is_empty() {
            return Ok(());
        }
        Err(PipelineError::InvalidRoster(
            problems
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        ))
    }

    /// State the run's roster posture, once, before any spend.
    ///
    /// Two kinds of fact, both of which must be *said* rather than inferred:
    ///
    /// - **Ablations.** A responsibility the operator turned off. Silence here
    ///   would make a deliberately shortened pipeline indistinguishable from a
    ///   broken one in a transcript someone reads a week later.
    /// - **Independence losses.** A responsibility that grades the worker's
    ///   work using the worker's own agent. This is the #2381 requirement that
    ///   disabling — or reassigning — verification must never silently downgrade
    ///   a run to a claimed-done, and it matches the pipeline's standing
    ///   posture that degradation warns and never quietly disables.
    ///
    /// Reassignments that keep independence are not reported: `witness_author`
    /// on the triage agent is a configuration choice with no bearing on what
    /// the verdict means, and a notice per row would train operators to skip
    /// the two that matter.
    pub(super) fn report_roster_posture(&self) {
        for assignment in self.config.roster.assignments() {
            if !assignment.enabled {
                self.warn(format!(
                    "the `{}` responsibility is disabled by configuration — this run is an \
                     ablation, not the default pipeline",
                    responsibility_label(assignment.responsibility)
                ));
            }
        }
        for loss in self.config.roster.independence_losses() {
            self.warn(format!(
                "the `{}` responsibility is assigned to the `{}` agent, which is also the \
                 worker's — this run grades its own work, so its verdict is a self-review and \
                 not independent evidence",
                responsibility_label(loss.responsibility),
                loss.agent
            ));
        }
    }

    /// The agent name a responsibility is bound to, for prose that must name
    /// the row an operator would edit.
    ///
    /// Falls back to the responsibility's own token when the pipeline does not
    /// own it — unreachable from the callers below, which have already gone
    /// through the roster, but stated rather than `unreachable!`d because a
    /// panic in library code on a value the type system permits is invariant
    /// 5's exact prohibition.
    fn assigned_agent(&self, responsibility: ModelCallRole) -> String {
        self.config.roster.assignment(responsibility).map_or_else(
            || responsibility_label(responsibility),
            |row| row.agent.to_string(),
        )
    }

    /// Whether the agent the roster assigns to `responsibility` is independent
    /// of the worker — and, when it is not, why.
    ///
    /// **The roster is consulted exactly once, through [`Self::assigned`], so
    /// this answers the same question the call site's own resolution will**
    /// (#2467). It used to resolve [`Role::Verifier`] literally while
    /// [`Self::resolve_witness_author`] went through the roster — which agreed
    /// only for as long as `witness_author` could not be reassigned. Binding it
    /// elsewhere made the gate and the resolution disagree in both directions,
    /// and the false-positive direction was the dangerous one: the gate said
    /// "independent", so `VerificationContract::WorkerTestFirst` was not chosen
    /// and the worker was never told that its own failing test was the run's
    /// only deterministic evidence; then the resolution dropped the author on
    /// its worker-model filter, silently, because this function's contract says
    /// the gate has already spoken. The run finished carrying no deterministic
    /// evidence and said nothing.
    ///
    /// Parameterised by responsibility rather than fixed to the witness author
    /// for the same reason one step out: `require_independent_verifier` asks
    /// about the VERDICT, and that shared an answer with the witness author
    /// only while the two could not be bound separately.
    ///
    /// Pure — it emits nothing, so asking twice costs nothing and says nothing
    /// twice.
    pub(super) fn independence_of(&self, responsibility: ModelCallRole) -> Independence {
        let Ok(worker) = self.resolve_provider(Role::Worker) else {
            return Independence::WorkerUnresolvable;
        };
        match self.assigned(responsibility) {
            Assigned::To(agent) if agent.model_ref != worker.model_ref => Independence::Independent,
            // The conclusion stays role-neutral (#1795) and the parenthetical
            // names the ASSIGNED agent rather than the verifier (#2467), so a
            // reader is pointed at the row that actually holds the job.
            Assigned::To(_) => Independence::Unavailable(format!(
                "no model independent of the worker resolves (the `{}` agent and the worker \
                 both resolved to `{}`)",
                self.assigned_agent(responsibility),
                worker.model_ref
            )),
            Assigned::Withheld => Independence::Withheld,
            Assigned::Unresolvable => Independence::Unavailable(format!(
                "no model independent of the worker resolves (the `{}` agent is unresolvable)",
                self.assigned_agent(responsibility)
            )),
        }
    }

    /// The reason `responsibility` fails an independence *requirement*, or
    /// `None` when it meets one.
    ///
    /// The requirement projection of [`Self::independence_of`], kept separate
    /// from the degradation projection below because the two collapse its four
    /// states differently — which is the whole reason the probe returns four.
    ///
    /// A withheld responsibility is a shortfall, and says so in its own words:
    /// a host that declared an independent verifier and then ablated the
    /// verdict holds a configuration that contradicts its own claim, and
    /// quietly producing the number anyway is exactly what #1147 refuses.
    ///
    /// [`Independence::WorkerUnresolvable`] is deliberately NOT a shortfall.
    /// The run fails on the worker's own routing error a few steps later, and
    /// refusing here would file a routing outage under an independence claim.
    pub(super) fn independence_shortfall(&self, responsibility: ModelCallRole) -> Option<String> {
        match self.independence_of(responsibility) {
            Independence::Independent | Independence::WorkerUnresolvable => None,
            Independence::Withheld => Some(format!(
                "the `{}` responsibility is disabled by configuration, so nothing performs it \
                 at all",
                responsibility_label(responsibility)
            )),
            Independence::Unavailable(reason) => Some(reason),
        }
    }

    /// Whether a witness author independent of the worker can be resolved.
    ///
    /// Losing the author costs the run its authored witness, never the task: a
    /// `false` here routes to the ordinary single-shot path and the
    /// deterministic/verifier verify ladder. Announced once, at the one point
    /// that decides it, so the run never pays for isolation it cannot use.
    ///
    /// A host that cannot afford that degradation sets
    /// [`crate::PipelineConfig::require_independent_witness`], which refuses
    /// the run up front rather than reaching this call at all.
    pub(super) fn can_author_independent_witness(&self) -> bool {
        match self.independence_of(ModelCallRole::WitnessAuthor) {
            Independence::Independent => true,
            // The operator removed the stage, and `report_roster_posture` said
            // so once, before any spend. Saying it again through `unproven`
            // would file a deliberate ablation as a degradation — precisely
            // the distinction #2381 exists to keep legible.
            Independence::Withheld => false,
            // Reported through `unproven`, not a bare `warn`. A witness triage
            // asked for and the wiring cannot supply is precisely
            // `WitnessUnavailable` — routing it to the warning channel alone
            // left the rail's witness row with no statement, so it fell
            // through to the backstop's "not reported" when the real answer
            // was known all along and worth naming.
            //
            // The message names the degradation AND the way out. A same-model
            // posture is legitimate (an auto-routing gateway can serve
            // distinct upstream models behind one id, and this run keeps going
            // either way), but the operator must hear that the flip oracle
            // cannot arm and what config change restores it — and hear it
            // about the agent that actually holds the responsibility (#2467),
            // since advice about `pipeline_verifier_model` is advice about a
            // row an operator may deliberately have moved authoring off.
            Independence::Unavailable(reason) => {
                self.unproven(format!(
                    "{reason}; verification is degraded — the deterministic flip oracle \
                     cannot arm, so the verdict is a model review with no independent test. \
                     Point `agents.{agent}.model` at a model distinct from the worker's, or \
                     move `responsibilities.witness_author.agent` to an agent that already \
                     is, to restore independent verification",
                    agent = self.assigned_agent(ModelCallRole::WitnessAuthor)
                ));
                false
            }
            // A worker that won't resolve fails later, on its own terms — not
            // here, disguised as a witness-independence verdict.
            Independence::WorkerUnresolvable => false,
        }
    }

    /// Resolve the agent that will author this run's witness, or `None` when
    /// nobody may.
    ///
    /// Three conditions, and only the first is the roster's:
    ///
    /// - `wanted` is the caller's already-taken decision (triage asked for a
    ///   witness, no `--test-command` pre-empted it, the roster enables
    ///   authoring). `Pipeline::can_author_independent_witness` announced any
    ///   degradation on the way to it, so this function is **silent** — a
    ///   second warning here would double-report one fact.
    /// - The assigned agent must resolve.
    /// - It must not resolve to the worker's own model. That filter is
    ///   unconditional and deliberately NOT a roster question: a witness the
    ///   worker wrote proves nothing about the worker's diff, so a roster
    ///   binding the two together loses authoring rather than producing a
    ///   false proof — which `report_roster_posture` has already named.
    ///
    /// Returning `Option` rather than mutating a caller's flag is what lets
    /// `run_best_of_n` derive "are we authoring" from "did an author resolve",
    /// so the two can no longer disagree.
    pub(super) fn resolve_witness_author(
        &self,
        wanted: bool,
        worker: &ResolvedRole<'_>,
    ) -> Option<ResolvedRole<'a>> {
        let author = wanted
            .then(|| self.assigned(ModelCallRole::WitnessAuthor).ok())
            .flatten()
            .filter(|author| author.model_ref != worker.model_ref)?;
        if let Some(fallback) = &author.fallback {
            self.emit_fallback(fallback);
        }
        Some(author)
    }

    /// Record that a disabled verdict left the ladder with nothing to escalate
    /// to, and return the conservative deterministic verdict the run completes
    /// on.
    ///
    /// **Not** [`Self::warn_verifier_fallback`]'s path, which says
    /// "unavailable" and records a [`ProofStep::VerdictDegraded`]: nothing
    /// degraded here, the operator removed the stage. This routes to the
    /// abstention rung instead ([`Self::unverifiable`], #973), whose whole
    /// contract is that the resulting `passed: true` means "nothing failed
    /// this" and never "something proved it" — which is exactly what #2381
    /// requires an ablated verifier to report.
    pub(super) fn verdict_withheld(&self, inputs: &LadderInputs) -> ModelVerifierVerdict {
        self.unverifiable(
            "the `verdict` responsibility is disabled by configuration, so no model reviewed \
             this turn",
        );
        heuristic_fallback(inputs)
    }
}

/// A responsibility's configuration key, for prose an operator will act on.
///
/// The roster's own token function, so the name a warning prints is exactly
/// the name that would turn the responsibility back on.
fn responsibility_label(responsibility: ModelCallRole) -> String {
    crate::roster::responsibility_token(responsibility)
}
