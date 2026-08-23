//! The host sequence — the code that actually drives the four points around a
//! turn.
//!
//! `doc:wrapper-socket` describes four points and this crate shipped all four
//! (#3380), but nothing called them: `grep -rn "after_turn"` over `stella-cli`
//! and the then-still-present `stella-pipeline` (deleted in #3865) found one
//! port handle and no dispatch, so an
//! installed wrapper plugin participated in nothing (#3494). This module is the
//! missing caller, and it is deliberately **not** in `stella-cli`: §6's
//! acceptance criterion is that the same plugin runs under `stella-cli`,
//! `stella-serve` and an embedded host linking `stella-engine`, and a sequence
//! that lives in the binary is one the other two cannot reach. It lives beside
//! the trait, in the crate that already owns engine assembly and reads no
//! ambient environment ([`crate`]'s `tests/no_ambient_reads.rs`).
//!
//! # The shape: the dispatcher owns the loop, the host owns the turn
//!
//! ```text
//! for each round:
//!     before_turn  per declared stage the resolved StageProgram says runs
//!     ---- the host runs the turn (TurnDriver) ----
//!     after_turn   once, about the round that just ran
//!     judge        host-run, synchronous, total
//!     again?       host-run, synchronous, total  ->  another round, or stop
//! ```
//!
//! [`TurnDriver`] is the one thing a host supplies, and its signature is why
//! `stella-core` never learns plugins exist: the dispatcher hands over a
//! [`TurnPrelude`] of plain messages and gets back a [`DrivenTurn`]. The engine
//! on the other side of that trait is whatever the host has — a step loop, an
//! HTTP round trip, a fixture.
//!
//! # `admissible` is on the path, not beside it
//!
//! [`super::admissible`] had zero non-test callers when it shipped, which made
//! the undeclared-role and mistyped-signal refusals a rule someone had to
//! remember rather than one the code takes. It now **consumes** the response
//! and answers with an [`AdmittedContribution`](super::AdmittedContribution) —
//! the only value this module will apply — so the check is the step that
//! produces the thing you apply rather than a step beside it.
//!
//! # Invariant 7 is spent here, exactly once
//!
//! A contribution reaches the host as an already-built [`CompletionMessage`],
//! because this module calls
//! [`VolatileContext::into_message`](stella_plugin::VolatileContext::into_message)
//! and the host never sees a `VolatileContext` at all. There is therefore no
//! call site in any host where contributed text could be pushed into the
//! byte-stable system prefix: the value that could do it does not cross the
//! seam.
//!
//! # What a failure means
//!
//! A [`WrapperError`] at either point is an **abstention**, never a claim. A
//! `before_turn` that failed contributes nothing and the turn runs anyway — a
//! wrapper that cannot speak has nothing to say, and that is not the user's
//! fault. An `after_turn` that failed yields
//! [`ObservedEvidence::nothing`], which merges into the
//! [`EvidenceSet::unobserved`] shape and makes [`judge`] abstain rather than
//! blame the worker for evidence nobody collected. Every one of them is
//! reported on [`DispatchReport::faults`]; silence is what this whole apparatus
//! exists to refuse.

use std::sync::Arc;

use async_trait::async_trait;
use stella_plugin::{
    AfterTurnRequest, BeforeTurnRequest, CandidateGrant, Continuation, EvidenceProvenance,
    EvidenceSet, FlipObservation, LoopGrant, ObservedEvidence, Outcome, PROTOCOL_VERSION,
    PluginManifest, PublishedSignal, RoundState, SignalValues, StageName, StageProgram,
    TamperFinding, TurnOutcome, Verdict, VerdictRule, WrapperPoint,
};
use stella_protocol::completion::CompletionMessage;

use super::{TurnWrapper, WrapperError, admissible, again, judge};

/// The host's own ceiling on completion holds, when a caller states none.
///
/// Two, and the number is the host's rather than the manifest's on purpose:
/// [`LoopGrant::max_holds`](stella_plugin::LoopGrant) is a plugin's *ask* and
/// [`again`] clamps it against this. Each hold buys a whole extra turn at the
/// user's expense, so the default is the smallest allowance that can still do
/// the thing a hold is for — one correction, and one chance to check it.
pub const DEFAULT_HOST_MAX_HOLDS: u32 = 2;

/// Everything a wrapper's round is opened with.
///
/// Owned, and deliberately the same facts the wire carries: a host that drives
/// this over HTTP fills the identical struct from a request body.
#[derive(Debug, Clone)]
pub struct RoundInput {
    /// The goal, as the user stated it.
    pub goal: String,
    /// The host's published signal values, which decide which declared stages
    /// run. Total by construction — see [`SignalValues`].
    pub signals: SignalValues,
    /// The candidate workspace this run has, when the host made one. `None` is
    /// "this host runs in the shared tree", which a wrapper reads as having no
    /// worktree to act on rather than as permission to pick one.
    pub candidate: Option<CandidateGrant>,
}

/// What the host is asked to run, after the wrapper has contributed.
///
/// **Constructible only by this module**, which is the point: every field on it
/// has been through [`admissible`], and a host holding one is holding a checked
/// value rather than a promise that someone checked.
#[derive(Debug, Clone)]
pub struct TurnPrelude {
    round: u32,
    stages: Vec<StageName>,
    messages: Vec<CompletionMessage>,
    role: Option<String>,
    scope: Vec<String>,
    witness: Vec<String>,
}

impl TurnPrelude {
    /// Which round of the wrapper's loop this turn is; `0` for the first.
    #[must_use]
    pub fn round(&self) -> u32 {
        self.round
    }

    /// The declared stages that contributed to this turn, in execution order.
    #[must_use]
    pub fn stages(&self) -> &[StageName] {
        &self.stages
    }

    /// The role intent the wrapper named, already checked against the roles its
    /// manifest declares. `None` is "run this turn as the host would".
    #[must_use]
    pub fn role(&self) -> Option<&str> {
        self.role.as_deref()
    }

    /// Workspace-relative paths the wrapper believes the turn should stay
    /// within — advisory input to the host's own scoping, never itself a
    /// permission (`stella_protocol::candidate`).
    #[must_use]
    pub fn scope(&self) -> &[String] {
        &self.scope
    }

    /// Workspace-relative paths the wrapper will judge its flip against, for
    /// the host to snapshot **before it runs the turn** and re-check after
    /// (#3587).
    ///
    /// The union across every stage that contributed to this round, in
    /// first-seen order, exactly as [`Self::scope`] is unioned and for the same
    /// reason: two stages naming one artifact are asking for one watch, and a
    /// list that repeats it says nothing extra.
    ///
    /// A host that pins these is answering a question the invocation could not:
    /// `cargo test --test flip` names `flip`, and `tests/flip.rs` is cargo's
    /// convention rather than anything in the argv. A host that ignores them
    /// reports [`TamperFinding::NotChecked`](stella_plugin::TamperFinding),
    /// which is a refusal to credit rather than a pass, so ignoring them is
    /// safe and useless in the same measure.
    #[must_use]
    pub fn witness(&self) -> &[String] {
        &self.witness
    }

    /// The contributions, as the volatile messages they are.
    ///
    /// Every one is a [`MessageRole::User`](stella_protocol::completion::MessageRole)
    /// message built by
    /// [`VolatileContext::into_message`](stella_plugin::VolatileContext::into_message),
    /// so appending them **after** the byte-stable system prefix is the only
    /// thing a host can do with them and still be writing sensible code
    /// (invariant 7). The correction from a held-open round rides last, in the
    /// same form.
    #[must_use]
    pub fn into_messages(self) -> Vec<CompletionMessage> {
        self.messages
    }
}

/// What the host reports back about the turn it ran.
///
/// Note for whoever wires driver #2: `stella-serve` already has a `pub(crate)`
/// type of this name (`crates/stella-serve/src/session.rs`) for the same idea
/// with different fields, so implementing [`TurnDriver`] there will want an
/// `as` rename on one of them rather than a silent shadow.
#[derive(Debug, Clone)]
pub struct DrivenTurn {
    /// What the turn did, in the vocabulary the wire carries.
    pub outcome: TurnOutcome,
    /// What the **host's** tamper check found. It is a parameter rather than
    /// something the plugin reports because snapshotting artifact identity is
    /// host-side (#3499); a host that took no snapshot says
    /// [`TamperFinding::NotChecked`] itself.
    pub tamper: TamperFinding,
}

/// The turn, as the dispatcher is allowed to know it.
///
/// One method, and nothing about a model, a credential, a terminal or a
/// filesystem in it: `stella-cli` implements this over its step loop,
/// `stella-serve` over an HTTP round trip, a test over a fixture. That is the
/// whole reason the sequence above can be written once.
///
/// # Why the future is not `Send`
///
/// `?Send` is a report of what the engine already is, not a relaxation chosen
/// for convenience: `stella_core`'s own turn future is not `Send` — its
/// speculative-tool machinery holds `Box<dyn Future>` without the bound — so a
/// `Send` requirement here would be one no in-process host could satisfy, and
/// `stella-cli`'s driver does not compile against it. Every host today drives
/// its turn on the thread it owns. Tightening this is a change in
/// `stella-core`, not here.
#[async_trait(?Send)]
pub trait TurnDriver {
    /// Run one turn with the wrapper's contributions in front of it, and report
    /// what it did.
    ///
    /// Infallible on purpose: a turn that aborted is a
    /// [`TurnOutcome`] with `completed: false`, which is evidence a wrapper may
    /// judge. Collapsing it into an error would take the round away from the
    /// wrapper whose job it is to have an opinion about it.
    async fn run_turn(&mut self, prelude: TurnPrelude) -> DrivenTurn;
}

/// What one wrapper's whole loop concluded.
#[derive(Debug)]
pub struct DispatchReport {
    /// The variant id that ran — `StageProgram::variant`, and what
    /// `executions.pipeline_variant` records (#3388).
    pub variant: String,
    /// How many turns the driver was asked for. At least 1.
    pub rounds: u32,
    /// The verdict of the final round.
    pub verdict: Verdict,
    /// How the loop ended.
    pub outcome: Outcome,
    /// Every point that failed, in the order it failed. Empty in the ordinary
    /// case; each entry is a round where the wrapper abstained rather than
    /// answered.
    pub faults: Vec<WrapperError>,
}

/// One installed wrapper plugin, and the sequence that drives it.
///
/// Holds the manifest **and** the transport together because neither decides
/// anything alone: the manifest carries the stage order, the grants and the
/// verdict rule a human consented to at install, and the transport is only the
/// process that answers. A host that held the transport alone could dispatch a
/// point the manifest never declared.
pub struct WrapperDispatch {
    /// The composed members, in the order the selection named them (#3801).
    ///
    /// One entry is the ordinary case and the shape every caller had before
    /// composition existed; several is a `--pipeline` selection naming several
    /// plugins, whose declarations were reconciled once at bind time by
    /// `super::compose`.
    members: Vec<Member>,
    /// Every stage any member declares, in the one order they all agree with.
    stage_order: Vec<StageName>,
    rule: VerdictRule,
    /// The grant `again` consults — the arbiter's, or a non-arbiter's that
    /// cannot hold. See `super::compose::Composition::hold_grant`.
    hold_grant: LoopGrant,
    host_max_holds: u32,
}

/// One plugin inside a composition: what it declared, and the process that
/// answers for it.
///
/// The two travel together for [`WrapperDispatch`]'s own reason — a transport
/// held without its manifest could be asked a point the manifest never
/// declared — and composition does not weaken that, it just means there are
/// several such pairs and each is filtered by its *own* grants.
struct Member {
    manifest: PluginManifest,
    wrapper: Arc<dyn TurnWrapper>,
}

/// What one round actually runs: the merged stage list, plus which member
/// resolved which stage.
///
/// Held together rather than passed as two arguments because the second is
/// only meaningful against the first — "member 2 runs `plan`" is a claim about
/// this round's resolution, not about the manifest.
struct RunningProgram {
    /// Every stage some member resolved this round, in the agreed order.
    stages: Vec<StageName>,
    /// Per member, in selection order, the stages that member resolved.
    per_member: Vec<Vec<StageName>>,
}

impl RunningProgram {
    /// Whether the member at `index` runs `stage` this round.
    fn runs(&self, index: usize, stage: &StageName) -> bool {
        self.per_member
            .get(index)
            .is_some_and(|stages| stages.iter().any(|resolved| resolved == stage))
    }
}

/// Merge one member's observed evidence into what the composition has so far.
///
/// Three rules, each following from what the piece means rather than from
/// convenience:
///
/// - **Measurements union, and a later member does not overwrite an earlier
///   one's number.** A measurement name belongs to the oracle that declared
///   it, and `compose` already refused a second oracle — so two members
///   reporting the same name means one of them is reporting a number nothing
///   asked it for, and the declaring member's is the one the check reads.
/// - **The flip is whichever member actually observed one.** A flip is a
///   statement about a witness that ran; `NotAttempted` is the honest answer
///   from every member that has no witness, and a composition of "no witness"
///   and "red before, green after" observed a flip. Two members both
///   observing a flip cannot happen while only one oracle may exist, and if it
///   somehow does, the first observation stands rather than the last — a later
///   silence must never erase an earlier observation.
/// - **The advisory detail is the first member's that had one.** The same rule
///   as the flip, for the same reason (#3840): a member with nothing to say
///   about the round must not silence one that did.
fn fold_evidence(into: Option<ObservedEvidence>, next: ObservedEvidence) -> ObservedEvidence {
    let Some(mut merged) = into else {
        return next;
    };
    for (name, value) in next.measurements {
        merged.measurements.entry(name).or_insert(value);
    }
    if merged.flip == FlipObservation::NotAttempted {
        merged.flip = next.flip;
    }
    // The measurements rule, pointed at the one free-text field: whoever spoke
    // first keeps the floor, so a later member's silence cannot erase an
    // earlier member's note (#3840).
    if merged.detail.is_none() {
        merged.detail = next.detail;
    }
    merged
}

impl Member {
    /// This member's own `[wrapper] id`.
    fn variant(&self) -> &str {
        // `bind_composed` refused a manifest without one, so the fallback is
        // unreachable — written as a fallback rather than an `expect` because
        // invariant 5 does not make an exception for "I checked earlier".
        self.manifest
            .wrapper
            .as_ref()
            .map_or(self.manifest.name.as_str(), |wrapper| wrapper.id.as_str())
    }

    /// The stages this member runs *this* round, its own conditions answered
    /// against its own signals.
    ///
    /// Resolved per member rather than once for the composition because a
    /// condition is a statement about the manifest that declared it — two
    /// members can name the same stage and disagree about when it runs, and
    /// each is right about itself.
    fn program(&self, signals: &SignalValues) -> Result<StageProgram, WrapperError> {
        match &self.manifest.wrapper {
            Some(wrapper) => wrapper.resolve(signals),
            None => {
                return Err(WrapperError::NotAWrapper {
                    plugin: self.manifest.name.clone(),
                });
            }
        }
        .map_err(|source| WrapperError::Unresolvable {
            wrapper: self.variant().to_string(),
            source: Box::new(source),
        })
    }
}

impl std::fmt::Debug for WrapperDispatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WrapperDispatch")
            .field("variant", &self.variant())
            .field("host_max_holds", &self.host_max_holds)
            .finish_non_exhaustive()
    }
}

impl WrapperDispatch {
    /// Bind a validated manifest to the transport that answers for it.
    ///
    /// # Errors
    ///
    /// [`WrapperError::NotAWrapper`] when the manifest declares no `[wrapper]`
    /// block. That is not a defect in the plugin — a manifest may declare hooks
    /// and nothing else — it just means there is no stage order to resolve and
    /// no variant id to record, so there is nothing here to drive.
    pub fn bind(
        manifest: PluginManifest,
        wrapper: Arc<dyn TurnWrapper>,
    ) -> Result<Self, WrapperError> {
        Self::bind_composed(vec![(manifest, wrapper)])
    }

    /// Bind several validated manifests to serve one selection together
    /// (#3801).
    ///
    /// The members are asked in the order given, which is the order the
    /// selection named them — a user writing `--pipeline research-v1,plan-v1`
    /// is stating that grounding comes before planning, and nothing else in
    /// the system knows that. Within one stage their contributions concatenate
    /// in that order; across stages they follow the merged stage order
    /// `super::compose` computed.
    ///
    /// Each member keeps its **own** grants. A member that did not declare
    /// `before_turn` is not asked it because another member did; composition
    /// unions what plugins *contribute*, never what they are *permitted*.
    ///
    /// # Errors
    ///
    /// [`WrapperError::NotAWrapper`] when any member declares no `[wrapper]`
    /// block — a composition of a wrapper and a non-wrapper is a caller
    /// mistake, not a degraded composition. [`WrapperError::EmptyComposition`]
    /// for no members. Otherwise one of the two conflicts `super::compose`
    /// documents: a contradictory stage order, or two arbiters.
    pub fn bind_composed(
        members: Vec<(PluginManifest, Arc<dyn TurnWrapper>)>,
    ) -> Result<Self, WrapperError> {
        if members.is_empty() {
            return Err(WrapperError::EmptyComposition);
        }
        for (manifest, _) in &members {
            if manifest.wrapper.is_none() {
                return Err(WrapperError::NotAWrapper {
                    plugin: manifest.name.clone(),
                });
            }
        }
        let manifests: Vec<PluginManifest> = members
            .iter()
            .map(|(manifest, _)| manifest.clone())
            .collect();
        let composition = super::compose::compose(&manifests)?;
        Ok(Self {
            members: members
                .into_iter()
                .map(|(manifest, wrapper)| Member { manifest, wrapper })
                .collect(),
            stage_order: composition.stage_order,
            rule: composition.rule,
            hold_grant: composition.hold_grant,
            host_max_holds: DEFAULT_HOST_MAX_HOLDS,
        })
    }

    /// Set this host's ceiling on completion holds, whatever the manifest asks
    /// for.
    #[must_use]
    pub fn with_host_max_holds(mut self, holds: u32) -> Self {
        self.host_max_holds = holds;
        self
    }

    /// The variant id this wrapper runs under.
    ///
    /// For a composition it is the members' ids joined with `,` — the same
    /// text the selection named — so the store's `pipeline_variant` column
    /// records what actually ran rather than whichever member happened to be
    /// first (#3801). A single member is unchanged.
    #[must_use]
    pub fn variant(&self) -> String {
        self.members
            .iter()
            .map(Member::variant)
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Every composed member's manifest, in the order the selection named them.
    ///
    /// This replaces the old `manifest()` accessor, which returned the one
    /// manifest a dispatch was bound to. A composition has no *one* manifest,
    /// and an accessor handing back the first member's would read as "the
    /// manifest that was consented to" while silently omitting the rest —
    /// which is the precise misreading this whole change exists to prevent.
    /// Callers wanting the single-member case take `.manifests().next()` and
    /// handle the `Option` honestly.
    pub fn manifests(&self) -> impl Iterator<Item = &PluginManifest> {
        self.members.iter().map(|member| &member.manifest)
    }

    /// Drive the four points around as many turns as the verdict asks for.
    ///
    /// # Errors
    ///
    /// [`WrapperError::Unresolvable`] when the declared stage order cannot be
    /// resolved against `input.signals`. A manifest that came from
    /// [`PluginManifest::from_toml_str`](stella_plugin::PluginManifest::from_toml_str)
    /// resolves for every possible signal set — `stella-plugin`'s
    /// `tests/wrapper_program.rs` asserts that as a property — so this is
    /// reachable only for a hand-built manifest, and it is an error rather than
    /// an empty program because "which stages run" is not a question a host may
    /// answer by guessing.
    ///
    /// A failure at either *point* is not an error here: see the module docs.
    pub async fn run(
        &self,
        input: RoundInput,
        driver: &mut dyn TurnDriver,
    ) -> Result<DispatchReport, WrapperError> {
        let variant = self.variant();
        // Every member resolves its own conditions, then the union is walked in
        // the order they all agreed to at bind time. A stage no member resolved
        // this round simply does not appear.
        let mut running: Vec<StageName> = Vec::new();
        let mut per_member: Vec<Vec<StageName>> = Vec::with_capacity(self.members.len());
        for member in &self.members {
            let resolved = member.program(&input.signals)?.stages().to_vec();
            for stage in &resolved {
                if !running.iter().any(|seen| seen == stage) {
                    running.push(stage.clone());
                }
            }
            per_member.push(resolved);
        }
        running.sort_by_key(|stage| {
            self.stage_order
                .iter()
                .position(|declared| declared == stage)
                // A stage no member declared cannot be one a member resolved,
                // so this is unreachable; sorting it last is the inert answer.
                .unwrap_or(usize::MAX)
        });
        let program = RunningProgram {
            stages: running,
            per_member,
        };

        let mut faults = Vec::new();
        let mut holds_spent = 0u32;
        let mut rounds = 0u32;
        let mut correction = None;

        loop {
            let round = rounds;
            rounds += 1;

            let mut prelude = self.open_round(round, &input, &program, &mut faults).await;
            // The correction rides last, after this round's own contributions:
            // it is the most recent thing the host has to say, and it is the
            // same volatile shape everything else here takes.
            if let Some(guidance) = correction.take() {
                prelude.messages.push(guidance);
            }

            let driven = driver.run_turn(prelude).await;
            let observed = self
                .close_round(round, &input, &program, driven.outcome, &mut faults)
                .await;
            // `None` is the host's own conclusion about a plugin that did not
            // answer — an undeclared point, or a fault already on `faults` —
            // and must not be dressed as the plugin's report of nothing
            // (#3513). Either way the flip is `Unobservable` and `judge`
            // abstains; what differs is whose silence it is.
            // Taken before the merge, because it deliberately does not survive
            // it: `EvidenceSet` is the closed vocabulary `judge` is total over,
            // and this is the wrapper's own free text (#3840). It rejoins the
            // verdict afterwards, where nothing can decide anything with it.
            let detail = observed
                .as_ref()
                .and_then(|observed| observed.detail.clone());
            let evidence = match observed {
                Some(observed) => EvidenceSet::from_observed(observed, driven.tamper),
                None => EvidenceSet {
                    tamper: driven.tamper,
                    ..EvidenceSet::unobserved()
                },
            };

            let verdict = judge(&self.rule, &evidence).with_detail(detail);
            let state = RoundState {
                holds_spent,
                host_max_holds: self.host_max_holds,
            };
            match again(&verdict, &state, &self.hold_grant) {
                Continuation::Stop { outcome } => {
                    return Ok(DispatchReport {
                        variant,
                        rounds,
                        verdict,
                        outcome,
                        faults,
                    });
                }
                Continuation::Again { correction: next } => {
                    holds_spent += 1;
                    correction = Some(next.guidance.into_message());
                }
            }
        }
    }

    /// `before_turn` for every stage the resolved program runs, folded into the
    /// one checked value a host may apply.
    ///
    /// Signals a stage publishes are carried forward into the *next* stage's
    /// request, which is what [`BeforeTurnRequest::published`] is for. What they
    /// still cannot do is change which stages run: [`Wrapper::resolve`] takes
    /// one up-front snapshot, so a value published mid-turn cannot reach the
    /// condition that reads it. That gap is #3491 and it is declared here rather
    /// than papered over.
    ///
    /// [`Wrapper::resolve`]: stella_plugin::Wrapper::resolve
    async fn open_round(
        &self,
        round: u32,
        input: &RoundInput,
        program: &RunningProgram,
        faults: &mut Vec<WrapperError>,
    ) -> TurnPrelude {
        let mut prelude = TurnPrelude {
            round,
            stages: program.stages.clone(),
            messages: Vec::new(),
            role: None,
            scope: Vec::new(),
            witness: Vec::new(),
        };

        let mut published: Vec<PublishedSignal> = Vec::new();
        // Stage-major, then member within a stage: a later stage must see what
        // an earlier one published *from every member*, which is the whole
        // reason a composition needs one agreed stage order. Walking
        // member-major instead would hand the second member the first
        // member's whole turn as history and the third member's grounding not
        // at all.
        for stage in &program.stages {
            for (index, member) in self.members.iter().enumerate() {
                // Each member is filtered by its OWN grants. Composition unions
                // what plugins contribute, never what they are permitted: a
                // member that did not declare `before_turn` is not asked it
                // because a sibling did (#3501's filter, per member).
                //
                // `permits_stage` is `permits_point` plus the stage list
                // `[loop] before_turn_stages` declares (#3543), so a plugin
                // that contributes at one stage of an eight-stage order is
                // spawned once per round rather than eight times. Empty is
                // "every stage this program runs", which is what a manifest
                // written before the field existed means.
                if !member
                    .manifest
                    .loop_grant
                    .permits_stage(WrapperPoint::BeforeTurn, stage)
                {
                    continue;
                }
                // Nor is it asked about a stage its own conditions did not
                // resolve this round.
                if !program.runs(index, stage) {
                    continue;
                }
                let request = BeforeTurnRequest {
                    protocol_version: PROTOCOL_VERSION,
                    // Its own id, not the composition's: the plugin is being
                    // asked as itself, and a manifest that keys behaviour on
                    // the wrapper id would otherwise see a name it never
                    // declared.
                    wrapper: member.variant().to_string(),
                    stage: stage.clone(),
                    round,
                    goal: input.goal.clone(),
                    candidate: input.candidate.clone(),
                    published: published.clone(),
                };
                let admitted = match member.wrapper.before_turn(request).await {
                    Ok(response) => match admissible(&member.manifest, response) {
                        Ok(admitted) => admitted,
                        Err(error) => {
                            faults.push(error);
                            continue;
                        }
                    },
                    Err(error) => {
                        faults.push(error);
                        continue;
                    }
                };
                // Last contribution wins the role intent, members included:
                // stages run in the agreed order and members within a stage in
                // selection order, so the nearest declaration to the turn is
                // still the one that meant it.
                if let Some(role) = admitted.role() {
                    prelude.role = Some(role.to_string());
                }
                // The union of what was asked for, in first-seen order. Two
                // contributions naming the same path are asking for one
                // narrowing, and a list that repeats it says nothing extra.
                for path in admitted.scope() {
                    if !prelude.scope.iter().any(|seen| seen == path) {
                        prelude.scope.push(path.clone());
                    }
                }
                // The same union, for the same reason, over the artifacts the
                // host will vouch for (#3587). Two stages naming one witness
                // are asking for one watch.
                for path in admitted.witness() {
                    if !prelude.witness.iter().any(|seen| seen == path) {
                        prelude.witness.push(path.clone());
                    }
                }
                published.extend(admitted.published().iter().copied());
                prelude.messages.extend(admitted.into_messages());
            }
        }
        prelude
    }

    /// `after_turn` once, about the round that just ran.
    ///
    /// The `stage` named is the last one the program ran, because that is the
    /// stage the round ended at; `None` only when the program ran no stage at
    /// all, which is the reading [`AfterTurnRequest::stage`] reserves for a host
    /// with no stages.
    async fn close_round(
        &self,
        round: u32,
        input: &RoundInput,
        program: &RunningProgram,
        outcome: TurnOutcome,
        faults: &mut Vec<WrapperError>,
    ) -> Option<ObservedEvidence> {
        let mut merged: Option<ObservedEvidence> = None;
        for member in &self.members {
            if !member
                .manifest
                .loop_grant
                .permits_point(WrapperPoint::AfterTurn)
            {
                continue;
            }
            let request = AfterTurnRequest {
                protocol_version: PROTOCOL_VERSION,
                wrapper: member.variant().to_string(),
                stage: program.stages.last().cloned(),
                round,
                goal: input.goal.clone(),
                candidate: input.candidate.clone(),
                turn: outcome.clone(),
            };
            match member.wrapper.after_turn(request).await {
                Ok(response) => merged = Some(fold_evidence(merged, response.evidence)),
                Err(error) => faults.push(error),
            }
        }
        merged
    }
}

/// The verdict a report carries, as the one-line answer a surface prints.
impl DispatchReport {
    /// Whether the wrapper's own loop ended with every declared requirement
    /// met. `false` covers both a determinate failure and an abstention — a
    /// caller that needs to tell them apart reads [`Self::outcome`], which is
    /// why this is a convenience and not the answer.
    #[must_use]
    pub fn met(&self) -> bool {
        matches!(self.outcome, Outcome::Met { .. })
    }

    /// The reason a surface prints, in the words the declaration used.
    #[must_use]
    pub fn summary(&self) -> String {
        match &self.outcome {
            // A `Met` the plugin asserted about its own work must not print as
            // an observation. The host does not re-run a plugin's checks
            // (#3511's Option 2), so the only honest report of that verdict
            // names whose word it rests on (#3513).
            Outcome::Met { evidence } => match evidence {
                EvidenceProvenance::HostObserved => {
                    format!("{}: every declared requirement is met", self.variant)
                }
                EvidenceProvenance::PluginReported => format!(
                    "{}: every declared requirement is met, on the plugin's own reported evidence",
                    self.variant
                ),
            },
            Outcome::Unmet { unmet, stopped } => {
                let clauses: Vec<String> = unmet.iter().map(ToString::to_string).collect();
                format!(
                    "{}: {} unmet after {} round(s) ({stopped:?}) — {}",
                    self.variant,
                    unmet.len(),
                    self.rounds,
                    clauses.join("; ")
                )
            }
            Outcome::Undecided { reason } => {
                format!(
                    "{}: nothing decided it either way ({reason:?})",
                    self.variant
                )
            }
        }
    }
}
