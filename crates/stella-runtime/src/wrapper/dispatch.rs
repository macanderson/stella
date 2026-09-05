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
//! # AGENTS.md #7 is spent here, exactly once
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
//!
//! A fault list on its own is too quiet at the gate. Nothing ties it to the
//! verdict. A run whose arbiter died would leave the same trace as a run
//! whose arbiter was happy. So each fault also becomes a claim that the
//! member stood aside ([`super::ArbiterClaim::did_not_answer`]).
//! [`DispatchReport::arbitration`] holds those, beside the arbiter's own
//! claim.

use std::sync::Arc;

use async_trait::async_trait;
use stella_core::ports::Clock;
use stella_plugin::{
    AfterTurnRequest, BeforeTurnRequest, CandidateGrant, Continuation, EvidenceProvenance,
    EvidenceSet, FlipObservation, LoopGrant, ObservedEvidence, Outcome, PROTOCOL_VERSION,
    Participation, PluginManifest, PublishedSignal, RoundState, SignalValues, StageName,
    StageProgram, TamperFinding, TurnOutcome, Verdict, VerdictRule, WrapperPoint,
};
use stella_protocol::completion::CompletionMessage;
use stella_protocol::{GateBoard, LadderRung, LadderSnapshot, VerdictEvidence};

use super::stamp::{HostClock, StampTiming};
use super::{
    ArbiterClaim, Arbitration, TurnHoldBudget, TurnWrapper, WrapperError, admissible, again,
    fold_stamps, judge, stamp,
};

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
/// Owned, and the same facts the wire carries: a host that drives
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
    /// (AGENTS.md #7). The correction from a held-open round rides last, in the
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
    /// The pipeline id that ran — `StageProgram::variant`, and what
    /// `executions.pipeline_variant` records (#3388).
    pub variant: String,
    /// How many turns the driver was asked for. At least 1.
    pub rounds: u32,
    /// The verdict of the final round.
    pub verdict: Verdict,
    /// The same verdict as SPEC 8.1's gate board — one row per requirement the
    /// rule declares.
    ///
    /// Built here rather than by the caller because the rule is this
    /// dispatcher's own state and a board built from the verdict alone would
    /// have a row only for each *failure* (`super::gate_board`). Its `patch` is
    /// the candidate this round ran against, when there was one.
    ///
    /// It re-decides nothing: every row is read out of `verdict` above, so the
    /// two cannot disagree.
    pub board: GateBoard,
    /// The final round as the ladder's own record, carrying one stamp.
    ///
    /// The board above is for a person to look at. This is for a reader who
    /// comes back later and asks who decided, against what, and when. The
    /// stamp's name is read from the manifest the host loaded, never from
    /// anything the plugin sends, and its hash covers this record with the
    /// stamp list dropped — so a second observer can add a claim without
    /// breaking the first one. See `super::stamp`.
    ///
    /// It decides nothing. The rung here is the same rung the record would
    /// carry with no stamp on it at all.
    pub snapshot: LadderSnapshot,
    /// How the loop ended.
    pub outcome: Outcome,
    /// Every point that failed, in the order it failed. Empty in the ordinary
    /// case; each entry is a round where the wrapper abstained rather than
    /// answered.
    pub faults: Vec<WrapperError>,
    /// What the completion gate recorded: one row per claim, in arrival
    /// order — an abstention for every fault above, attributed to the member
    /// that produced it, and the arbiter's own claim from the final verdict.
    ///
    /// [`Self::faults`] says what broke. This says what the gate made of
    /// it. Failing open says nothing on its own. A run whose arbiter died
    /// would leave the same trace as one whose arbiter was happy.
    ///
    /// The fold does not decide this loop's rounds. [`again`] does, over the
    /// one arbiter a composition may have
    /// ([`WrapperError::TwoArbiters`]). `wrapper_arbitration.rs` proves the
    /// two agree on every single-arbiter input. So this record is the same
    /// law the loop ran under, not a second opinion.
    pub arbitration: Arbitration,
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
    /// Where a stamp's two times come from. A port rather than a call to the
    /// system clock, so a test can pin both numbers and read the whole record
    /// back byte for byte.
    clock: Arc<dyn Clock>,
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

/// A run's faults, each attributed to the member that produced it.
///
/// Two lists, not one. They answer different questions. `errors` is what
/// broke, in the words [`WrapperError`] writes for a person to act on.
/// `claims` is what the gate made of it. One struct, so a push cannot fill
/// one and forget the other.
#[derive(Default)]
struct Faults {
    errors: Vec<WrapperError>,
    claims: Vec<ArbiterClaim>,
}

impl Faults {
    /// Record one member's failure at one point.
    ///
    /// The author is the member's own `[wrapper] id`, not the whole
    /// composition's. The grant is that member's own. A line that says
    /// "arbiter X did not answer" must name the plugin that fell silent. It
    /// must say "arbiter" only of a plugin that had a say to lose.
    fn push(&mut self, member: &Member, error: WrapperError) {
        self.claims.push(ArbiterClaim::did_not_answer(
            member.id(),
            &error,
            &member.manifest.loop_grant,
        ));
        self.errors.push(error);
    }

    /// Record a failure that is the host's own, not a member's.
    ///
    /// No claim rides with it, and that is the point of a second method. A
    /// member that fell silent lost its say. The host that could not hash a
    /// record kept its answer and lost only the name on it, so a row reading
    /// "did not answer" would report a silence that never happened.
    fn push_host(&mut self, error: WrapperError) {
        self.errors.push(error);
    }
}

impl Member {
    /// This member's own `[wrapper] id`.
    fn id(&self) -> &str {
        // `bind_composed` refused a manifest without one, so the fallback is
        // unreachable — written as a fallback rather than an `expect` because
        // AGENTS.md #5 does not make an exception for "I checked earlier".
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
            wrapper: self.id().to_string(),
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
    /// no pipeline id to record, so there is nothing here to drive.
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
            clock: Arc::new(HostClock),
        })
    }

    /// Set this host's ceiling on completion holds, whatever the manifest asks
    /// for.
    #[must_use]
    pub fn with_host_max_holds(mut self, holds: u32) -> Self {
        self.host_max_holds = holds;
        self
    }

    /// Read the stamp's times from this clock instead of the host's own.
    ///
    /// The default counts from the Unix epoch, so two stamps from two runs can
    /// be compared. A test passes a clock it controls and gets a record whose
    /// every field it can name.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The pipeline id this wrapper runs under.
    ///
    /// For a composition it is the members' ids joined with `,` — the same
    /// text the selection named — so the store's `pipeline_variant` column
    /// records what actually ran rather than whichever member happened to be
    /// first (#3801). A single member is unchanged.
    #[must_use]
    pub fn variant(&self) -> String {
        self.members
            .iter()
            .map(Member::id)
            .collect::<Vec<_>>()
            .join(",")
    }

    /// The id this run's verdict is filed under at the gate.
    ///
    /// The arbiter's own `[wrapper] id`, when the composition has one. That
    /// is the plugin whose terms decided what done means. With no arbiter it
    /// falls back to the composition's id. Nothing held anything open, and
    /// naming a steering member would read as a grade it lacks.
    fn arbiter_id(&self) -> String {
        self.members
            .iter()
            .find(|member| member.manifest.loop_grant.participation == Participation::Arbiter)
            .map_or_else(|| self.variant(), |member| member.id().to_string())
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
        let pipeline_id = self.variant();
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

        let mut faults = Faults::default();
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
            // The clock starts where this round's observer starts work, so the
            // stamp's duration covers gathering the evidence and settling the
            // answer. The turn itself is the worker's time, not the observer's.
            let observing_from_ms = self.clock.now_ms();
            let faults_before = faults.errors.len();
            let observed = self
                .close_round(round, &input, &program, driven.outcome, &mut faults)
                .await;
            // `None` is the host's own conclusion about a plugin that did not
            // answer — an undeclared point, or a fault already on `faults` —
            // and must not be dressed as the plugin's report of nothing
            // (#3513). Either way the flip is `Unobservable` and `judge`
            // abstains; what differs is whose silence it is.
            // Taken before the merge, because it does not survive
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
            let decided_at_ms = self.clock.now_ms();
            let timing = StampTiming {
                decided_at_ms,
                duration_ms: decided_at_ms.saturating_sub(observing_from_ms),
                // Only this round's faults. An earlier round that timed out
                // says nothing about the observer that answered this one.
                timed_out: faults.errors[faults_before..]
                    .iter()
                    .any(|fault| matches!(fault, WrapperError::Timeout { .. })),
            };
            let state = RoundState {
                holds_spent,
                host_max_holds: self.host_max_holds,
            };
            match again(&verdict, &state, &self.hold_grant) {
                Continuation::Stop { outcome } => {
                    let board = super::gate_board(
                        &self.rule,
                        &verdict,
                        input.candidate.as_ref().map(|c| c.handle.to_string()),
                    );
                    let snapshot = self.stamp_round(
                        &evidence,
                        &verdict,
                        &pipeline_id,
                        input.candidate.as_ref(),
                        timing,
                        &mut faults,
                    );
                    // The record the gate leaves. Every claim this run
                    // stood aside on, in the order it happened. Then the
                    // arbiter's own claim, from the verdict that stopped the
                    // loop.
                    let mut claims = faults.claims.clone();
                    claims.push(ArbiterClaim::from_verdict(
                        self.arbiter_id(),
                        &verdict,
                        &self.hold_grant,
                        holds_spent,
                    ));
                    // The rung stays `None`, as it was before the stamp
                    // producer landed. `snapshot.rung` is an answer this
                    // dispatch now holds, and feeding it here would arm
                    // `Arbitration::refutes_done` on the live path for the
                    // first time. That is a decision of its own, with its
                    // own tests, and it is not what this change is.
                    let arbitration = fold_stamps(
                        None,
                        &claims,
                        TurnHoldBudget {
                            turn_holds_spent: holds_spent,
                            host_max_holds: self.host_max_holds,
                        },
                    );
                    return Ok(DispatchReport {
                        variant: pipeline_id,
                        rounds,
                        verdict,
                        board,
                        snapshot,
                        outcome,
                        faults: std::mem::take(&mut faults.errors),
                        arbitration,
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
        faults: &mut Faults,
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
                    wrapper: member.id().to_string(),
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
                            faults.push(member, error);
                            continue;
                        }
                    },
                    Err(error) => {
                        faults.push(member, error);
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
        faults: &mut Faults,
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
                wrapper: member.id().to_string(),
                stage: program.stages.last().cloned(),
                round,
                goal: input.goal.clone(),
                candidate: input.candidate.clone(),
                turn: outcome.clone(),
            };
            match member.wrapper.after_turn(request).await {
                Ok(response) => merged = Some(fold_evidence(merged, response.evidence)),
                Err(error) => faults.push(member, error),
            }
        }
        merged
    }

    /// The round as the ladder's own record, with one stamp on it.
    ///
    /// The name on the stamp comes from the manifests this dispatch was bound
    /// to, so a plugin cannot sign another one's name. `super::stamp` holds the
    /// rest of the rule.
    ///
    /// A record that cannot be hashed keeps its answer and loses its stamp.
    /// The hash is taken after the verdict is settled and can change nothing
    /// about it, so failing the whole run over one would throw away a good
    /// answer. The failure joins `faults`, where every other silence here is
    /// reported.
    fn stamp_round(
        &self,
        evidence: &EvidenceSet,
        verdict: &Verdict,
        pipeline_id: &str,
        candidate: Option<&CandidateGrant>,
        timing: StampTiming,
        faults: &mut Faults,
    ) -> LadderSnapshot {
        // The workspace the evidence was gathered in is the one thing here a
        // reader can go and look at, so it is the one pointer the stamp
        // carries. The board names the same handle as its patch.
        let refs: Vec<String> = candidate
            .iter()
            .map(|grant| format!("candidate:{}", grant.handle))
            .collect();
        match stamp::stamped(&self.rule, evidence, verdict, pipeline_id, refs, timing) {
            Ok(snapshot) => snapshot,
            Err(source) => {
                faults.push_host(WrapperError::Unstampable {
                    wrapper: pipeline_id.to_string(),
                    source,
                });
                stamp::snapshot(&self.rule, evidence, verdict)
            }
        }
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
                format!("{}: nothing decided it either way — {reason}", self.variant)
            }
        }
    }

    /// This report, as the evidence an `AgentEvent::Verdict` carries.
    ///
    /// This crate sends no events (module doc, "the dispatcher owns the
    /// loop, the host owns the turn"). The host builds the event and sends
    /// it. But the evidence *inside* the event comes from this report, so it
    /// is built once, here.
    ///
    /// `deterministic` is [`VerdictEvidence`]'s own distinction: real oracle
    /// checks versus a model verifier's opinion. `judge` never calls a model
    /// (`super::judge`'s module doc), so nothing here is ever a model's
    /// opinion. But not every rung is oracle evidence either.
    /// [`LadderRung::SubmitFast`] and [`LadderRung::Revise`] are the two
    /// rungs `stamp`'s `rung` reaches when the oracle's checks actually
    /// decided every requirement, met or unmet. Every other rung — `Waived`
    /// (nothing was declared to check), `Unverified`/`Unverifiable`/
    /// `WitnessUnsatisfiable` (the oracle could not decide) — carries no such
    /// evidence, and says so.
    ///
    /// `evidence_refs` is left empty. The oracle wire contract names no
    /// artifact today (`stella_plugin::ObservedEvidence`), so there is
    /// nothing here to point at.
    #[must_use]
    pub fn verdict_evidence(&self) -> VerdictEvidence {
        let deterministic = matches!(
            self.snapshot.rung,
            Some(LadderRung::SubmitFast | LadderRung::Revise)
        );
        VerdictEvidence {
            summary: self.summary(),
            deterministic,
            evidence_refs: Vec::new(),
            ladder: Some(Box::new(self.snapshot.clone())),
        }
    }
}
