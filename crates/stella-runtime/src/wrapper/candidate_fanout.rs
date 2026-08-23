// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `candidate_fanout` — N isolated workspaces, one writing worker turn each,
//! and the one adoption that lands a winner.
//!
//! `doc:pipeline-as-plugins` §3/§7 item 4 names `plugins/stella-candidates` —
//! best-of-N — as a wrapper plugin to extract, and until this module existed
//! the socket could not express it (#3844). Every other capability on the
//! host-call channel is read-only ([`recall`], [`child_turn`]) or
//! single-tracked ([`run_test`] against the one grant a round carries), so the
//! strongest thing a plugin could build was *bounded retry with correction
//! over the shared tree*: each attempt mutating the one real work tree in
//! place, with no isolation and no rollback of a losing attempt. That is a
//! real plugin and it is not best-of-N, and shipping it under that name is
//! what this module exists to make unnecessary.
//!
//! # It follows `child_turn`'s precedent exactly, and inverts one rule
//!
//! The shape is [`child_turn`]'s and deliberately so: **the plugin names an
//! intent, the host resolves it against machinery it already owns, the host
//! spends the money and does the I/O, and the plugin only steers and reports.**
//! No path, no branch, no base revision, no credential and no subprocess
//! crosses the wire — which is `doc:wrapper-socket`'s "no borrows, no cwd, no
//! credential in any signature" rule, kept at the one capability where the
//! turns write.
//!
//! One rule is inverted, and it is the rule a manifest author must read:
//!
//! | | `child_turn` | `candidate_fanout` |
//! |---|---|---|
//! | resolves to the worker's seat | [`Forbidden`] | **required** |
//! | writes | never ([`SubAgentSpec::read_only`]) | always |
//! | bounded by | `[loop] max_calls` | `[loop] max_fanout_width` |
//!
//! Both compare the **resolved seat, never the spelling** — [`ChildTurns`]'s
//! own lesson, and the reason [`CandidateFanouts::with_seat`] exists here too.
//! The inversion is not a relaxation. A child turn is *evidence about* the
//! work, so spending the worker's own model on it would let a plugin grade the
//! work with the model that did it. A fan-out candidate **is** the work, so
//! booking it anywhere but the worker's seat would put writing spend on the
//! receipt under a responsibility that wrote nothing — the same misattribution
//! read from the other end, and the one this capability could most easily be
//! used to launder ("do not let this become a way for a plugin to spend the
//! worker's seat under another name", #3844). Here it cannot spend anything
//! *else*, and what bounds it is width and budget rather than seat exclusion.
//!
//! # Two ceilings, because a fan-out is not a conversation
//!
//! [`DEFAULT_HOST_MAX_FANOUT_WIDTH`] bounds one call; [`DEFAULT_HOST_MAX_FANOUTS`]
//! bounds the whole run. Neither is [`DEFAULT_HOST_MAX_CALLS`](super::DEFAULT_HOST_MAX_CALLS),
//! which bounds how *chatty* a plugin may be inside one point and whose unit is
//! a cheap read. The unit here is a writing worker turn, so one number for both
//! would mean a plugin that wanted eight recalls had asked for eight
//! candidates.
//!
//! # The budget is carved, then divided
//!
//! [`CandidateFanouts::with_budget_usd`] is the *whole fan-out's* request, and
//! each candidate is asked for `budget / width`. That is the arithmetic
//! `FanOutBudget` did inside the staged pipeline before that crate was deleted
//! (#3865), kept rather than reinvented, and it is why width is clamped
//! **before** the division: dividing by a plugin's unclamped ask would hand
//! every candidate a slice of a budget the host never agreed to. The request
//! is still only a request — the substrate carves it against the parent's
//! remaining headroom exactly as [`ChildTurns`] does, so a plugin cannot spend
//! a session past its ceiling however it divides.
//!
//! # The handle table is the fence
//!
//! Adoption takes a [`CandidateHandle`] and nothing else, and the host resolves
//! it against the table **it** minted ([`CandidateFanouts::live`]). A handle
//! that names no entry is refused, which is what keeps
//! [`HOST_TREE_HANDLE`](stella_plugin::HOST_TREE_HANDLE) un-adoptable for free:
//! it names no entry in any table and never has, so a plugin that spells it
//! back is told the same "unknown" it has always been told. Adopting empties
//! the table, so a second adoption in the same run is refused rather than
//! silently applying a second diff over the first.
//!
//! [`recall`]: stella_plugin::HostCall::Recall
//! [`child_turn`]: super::ChildTurns
//! [`run_test`]: stella_plugin::HostCall::RunTest
//! [`Forbidden`]: stella_plugin::HostCallRefusal::Forbidden
//! [`SubAgentSpec::read_only`]: stella_core::SubAgentSpec::read_only
//! [`ChildTurns`]: super::ChildTurns

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use stella_plugin::{
    AdoptCandidateArgs, AdoptCandidateResult, CandidateFanoutArgs, CandidateFanoutResult,
    FanoutCandidate, HostCallFailure, HostCallRefusal, PluginManifest,
};
use stella_protocol::candidate::CandidateHandle;
use stella_protocol::event::ModelCallRole;

/// The host's own ceiling on one fan-out's width, when a caller states none.
///
/// Three, and the reasoning is [`DEFAULT_HOST_MAX_CHILD_TURNS`](super::DEFAULT_HOST_MAX_CHILD_TURNS)'s
/// pointed at a costlier unit. A fan-out of one is not a fan-out — there is
/// nothing to be best *of* — and a fan-out of two can only ever break a tie by
/// coin flip when both candidates pass, so three is the smallest width at which
/// selection means anything. It is also the largest that stays defensible
/// unasked: every candidate is a full writing worker turn, so the width is a
/// multiplier on the most expensive thing the agent does, and a host that wants
/// more says so with [`CandidateFanouts::with_max_width`] and owns the claim.
pub const DEFAULT_HOST_MAX_FANOUT_WIDTH: u32 = 3;

/// The host's own ceiling on fan-outs per plugin, for the whole run.
///
/// Two, and it is a **different bound from the width**, in the way
/// [`DEFAULT_HOST_MAX_CHILD_TURNS`](super::DEFAULT_HOST_MAX_CHILD_TURNS) is a
/// different bound from [`DEFAULT_HOST_MAX_CALLS`](super::DEFAULT_HOST_MAX_CALLS):
/// the width bounds one call and this bounds how many calls a plugin gets, so
/// the product is what a fan-out plugin may cost. Per plugin for the whole run
/// rather than per point, because refreshing it per point would make it no
/// bound at all across N rounds — the shape a hold ceiling that resets would
/// have.
///
/// Two rather than one because the second is what a plugin does after reading
/// the first fan-out's evidence and finding every candidate wanting; a plugin
/// that needs a third is a plugin whose selection is not converging, and it
/// should say so rather than keep buying turns.
pub const DEFAULT_HOST_MAX_FANOUTS: u32 = 2;

/// One isolated workspace the host minted for a candidate.
///
/// The pair a plugin is told about ([`FanoutCandidate::candidate`] and
/// [`FanoutCandidate::root`]) and nothing more: the substrate's own bookkeeping
/// — a branch name, a base revision, a lock — stays on the substrate's side,
/// because a plugin that cannot act on a fact has no business holding it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateWorkspace {
    /// The name that re-addresses this workspace. Minted by the substrate;
    /// opaque to everyone else.
    pub handle: CandidateHandle,
    /// The workspace's absolute root, canonical.
    pub root: String,
}

/// What one candidate turn is asked to do.
///
/// Shaped like [`SubAgentSpec`](stella_core::SubAgentSpec)'s plugin-reachable
/// subset on purpose: a substrate that already dispatches sub-agents serves
/// this by filling in the fields the host owns and none the plugin does. There
/// is no `write_access` field because there is no choice — a candidate turn
/// writes, which is the whole capability.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateWork {
    /// A stable id for this candidate's turn, unique within the fan-out.
    pub agent_id: String,
    /// What the candidate is asked to do — the plugin's instruction verbatim.
    pub instruction: String,
    /// The plugin's **own word** for the role it named, passed through
    /// untouched.
    ///
    /// [`Self::seat`] beside it is the receipt label, drawn from a closed enum
    /// this workspace owns; this is the routing key, drawn from the plugin's
    /// manifest and meaningful only to the plugin that wrote it and the user
    /// who assigned a model to it. Carrying both is what
    /// [`ChildTurns`](super::ChildTurns) does at its own dispatch, and for the
    /// same reason: without this a substrate can book the right seat but never
    /// reach the model the *user* assigned to that role, so every candidate
    /// runs on the session's own model however the user configured the plugin.
    /// Nothing downstream may branch on the contents.
    pub role: String,
    /// The responsibility this turn's model calls are attributed with. Always
    /// [`ModelCallRole::Worker`]; carried explicitly so a substrate books the
    /// seat it is told rather than one it assumes.
    pub seat: ModelCallRole,
    /// This candidate's share of the fan-out's requested carve, already
    /// divided by the clamped width. A request, clamped again by the
    /// substrate against the parent's headroom.
    pub budget_usd: Option<f64>,
    /// The receipt turn slot the candidate's manifests are recorded under.
    pub turn_instance: u32,
}

/// What one candidate turn produced.
///
/// The two size numbers are the substrate's because only it can see the
/// workspace's diff, and they are deliberately crude: this is the evidence a
/// plugin *scores*, and a host that shipped the score would be the thing the
/// plugin was extracted to replace.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateReport {
    /// The turn's own answer, already clamped by the substrate's report
    /// ceiling.
    pub report: String,
    /// Whether the turn reached a final answer.
    pub completed: bool,
    /// What it spent, in USD, as the substrate settled it.
    pub cost_usd: f64,
    /// Files changed in the workspace.
    pub files_changed: u32,
    /// Lines added and removed, summed.
    pub lines_changed: u32,
}

/// Why a candidate workspace operation did not happen.
///
/// Typed and named rather than a `String` (invariant 5), because the four are
/// different events with different remedies: a workspace that could not be
/// created is an isolation substrate problem, a turn that could not be run is
/// an engine problem, an adoption that could not be applied has left the user's
/// tree untouched and their work in a shadow workspace, and a removal that
/// failed has left disk behind. Only the third is anything like an emergency.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CandidateFanoutError {
    /// No isolated workspace could be created.
    #[error("a candidate workspace could not be created: {reason}")]
    NotCreated {
        /// The substrate's own reason, as prose.
        reason: String,
    },
    /// The workspace exists; the turn inside it could not be run.
    #[error("a candidate turn could not be run: {reason}")]
    NotRun {
        /// The substrate's own reason, as prose.
        reason: String,
    },
    /// The candidate's changes were not applied to the real tree.
    #[error("candidate `{handle}` could not be adopted: {reason}")]
    NotAdopted {
        /// Which candidate was being adopted.
        handle: CandidateHandle,
        /// The substrate's own reason, as prose.
        reason: String,
    },
    /// The workspace could not be discarded, so it is still on disk.
    #[error("candidate `{handle}` could not be discarded: {reason}")]
    NotRemoved {
        /// Which workspace is still there.
        handle: CandidateHandle,
        /// The substrate's own reason, as prose.
        reason: String,
    },
}

/// The isolation substrate a fan-out runs on.
///
/// Four operations, and the count is the design — the same argument
/// [`CandidateOp`](stella_protocol::candidate::CandidateOp) makes for its six.
/// A host that can snapshot its tree, run a rooted writing turn, apply one
/// snapshot back and throw the rest away can serve `candidate_fanout`; a host
/// that cannot installs no plane and every ask is answered
/// [`HostCallRefusal::Unavailable`], which is a declared gap rather than a
/// silence.
///
/// It is deliberately **not** [`SubAgentDispatcher`](stella_core::SubAgentDispatcher)
/// with a root added. A dispatcher that ignored such a field would run every
/// candidate in the shared tree while this module reported isolation, and
/// nothing in [`SubAgentOutcome`](stella_core::SubAgentOutcome) could tell the
/// two apart — a silent correctness failure at the one capability where the
/// turns write. A separate port makes "this host cannot root a turn" a type
/// error at assembly instead.
#[async_trait]
pub trait CandidateWorkspaces: Send + Sync {
    /// Snapshot the live tree into a fresh isolated workspace.
    ///
    /// # Errors
    ///
    /// [`CandidateFanoutError::NotCreated`] when no workspace could be made.
    async fn create(&self, label: &str) -> Result<CandidateWorkspace, CandidateFanoutError>;

    /// Run one writing turn rooted at `workspace`.
    ///
    /// # Errors
    ///
    /// [`CandidateFanoutError::NotRun`] when the turn could not be started or
    /// the substrate could not report on it. A turn that ran and did not finish
    /// is an ordinary [`CandidateReport`] with `completed: false`, not an error.
    async fn work(
        &self,
        workspace: &CandidateWorkspace,
        work: CandidateWork,
    ) -> Result<CandidateReport, CandidateFanoutError>;

    /// Apply this workspace's changes to the real tree, all-or-nothing.
    ///
    /// # Errors
    ///
    /// [`CandidateFanoutError::NotAdopted`] when nothing was applied.
    async fn adopt(&self, workspace: &CandidateWorkspace) -> Result<(), CandidateFanoutError>;

    /// Discard this workspace.
    ///
    /// # Errors
    ///
    /// [`CandidateFanoutError::NotRemoved`] when it is still on disk.
    async fn remove(&self, workspace: &CandidateWorkspace) -> Result<(), CandidateFanoutError>;
}

/// What one performed fan-out cost, and what it was booked against.
///
/// The host's half of "the spend is visible", at the capability where the
/// number is largest. **Consumer** (invariant 10's discipline, pointed at a
/// host report): the same place [`ChildTurnSpend`](super::ChildTurnSpend) is
/// read — a driver prints it beside the refusals so a user learns what a plugin
/// did on their money. A fan-out that is not reported is N worker turns nobody
/// can account for.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateFanoutSpend {
    /// The role intent the plugin named.
    pub role: String,
    /// The responsibility the host resolved it to — always
    /// [`ModelCallRole::Worker`], and recorded rather than assumed.
    pub seat: ModelCallRole,
    /// The width the plugin asked for, before the clamp.
    pub requested_width: u32,
    /// The candidates that actually ran.
    pub width: u32,
    /// What they spent in total, in USD.
    pub cost_usd: f64,
    /// How many of them reached a final answer.
    pub completed: u32,
}

/// A host that can fan a turn out across isolated workspaces, and land one.
///
/// Object-safe and two methods, so [`HostPlanes`](super::HostPlanes) can hold
/// one without becoming generic over the whole assembly. [`CandidateFanouts`]
/// is the shipped implementation.
#[async_trait]
pub trait CandidateFanoutPlane: Send + Sync {
    /// Run one fan-out, or say why not.
    ///
    /// # Errors
    ///
    /// [`HostCallFailure`] for every refusal in this module's header, plus
    /// [`HostCallRefusal::Failed`] when the fan-out was attempted and no
    /// candidate could be built. Each one reaches the plugin as an `err`
    /// answer rather than ending the point.
    async fn fanout(
        &self,
        args: CandidateFanoutArgs,
    ) -> Result<CandidateFanoutResult, HostCallFailure>;

    /// Apply one candidate and discard its siblings, or say why not.
    ///
    /// # Errors
    ///
    /// [`HostCallRefusal::Unavailable`] when the handle names no live
    /// candidate of this run, and [`HostCallRefusal::Failed`] when the
    /// substrate could not apply it. In both cases the real tree is untouched.
    async fn adopt(
        &self,
        args: AdoptCandidateArgs,
    ) -> Result<AdoptCandidateResult, HostCallFailure>;
}

/// The fan-out plane: a manifest's declared role intents, this host's seats,
/// its two ceilings, and the substrate that mints and runs the workspaces.
pub struct CandidateFanouts<W> {
    plugin: String,
    workspaces: W,
    roles: BTreeMap<String, String>,
    seats: BTreeMap<String, ModelCallRole>,
    declared_width: Option<u32>,
    width_ceiling: u32,
    fanout_ceiling: u32,
    budget_usd: Option<f64>,
    turn_lane: u32,
    spent: AtomicU32,
    live: Mutex<Vec<CandidateWorkspace>>,
    ledger: Mutex<Vec<CandidateFanoutSpend>>,
}

impl<W> std::fmt::Debug for CandidateFanouts<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CandidateFanouts")
            .field("plugin", &self.plugin)
            .field("roles", &self.roles)
            .field("max_width", &self.max_width())
            .field("max_fanouts", &self.max_fanouts())
            .finish_non_exhaustive()
    }
}

/// The tiers this host resolves a fan-out's role intent from, before a caller
/// binds any of its own.
///
/// The same four [`ChildTurns`](super::ChildTurns) serves, and shipping the
/// three a fan-out may **not** use is the point — it is that module's own
/// argument for keeping `worker` in *its* table, read from the other end. A
/// tier that resolves to nothing answers [`HostCallRefusal::Unavailable`]
/// ("this host binds it to no responsibility"), which sends a plugin author to
/// ask their host operator for a binding; a tier that resolves to a non-worker
/// seat answers [`HostCallRefusal::Forbidden`] ("a candidate is the work, and
/// this seat did not do it"), which sends them to change one line of their own
/// manifest. Collapsing the second into the first would hide a rule behind the
/// weaker of the two answers, and a rule discovered that way comes to be read
/// as an accident of configuration.
///
/// A host that serves some other tier says so with
/// [`CandidateFanouts::with_seat`]; binding one to anything but
/// [`ModelCallRole::Worker`] only changes which word gets refused, because the
/// check below compares the resolved seat and never the spelling.
fn default_seats() -> BTreeMap<String, ModelCallRole> {
    [
        ("worker", ModelCallRole::Worker),
        ("triage", ModelCallRole::Triage),
        ("research", ModelCallRole::Research),
        ("plan", ModelCallRole::Plan),
    ]
    .into_iter()
    .map(|(tier, seat)| (tier.to_string(), seat))
    .collect()
}

// Unbounded on `W`: every method here decides, clamps or reports, and not one
// of them touches the substrate — the `Debug` impl above included, which is
// what a host prints when it cannot work out why a plugin was refused.
impl<W> CandidateFanouts<W> {
    /// Bind a plugin's declared role intents to the substrate that will run
    /// them.
    ///
    /// The manifest is taken whole rather than its `[roles]` map, for
    /// [`ChildTurns::declare`](super::ChildTurns::declare)'s reason: the grant
    /// a human consented to at install is the authority, and a plane assembled
    /// from a hand-built map is one nobody consented to.
    ///
    /// `[loop] max_fanout_width` is read here as the plugin's **ask** and
    /// clamped against [`DEFAULT_HOST_MAX_FANOUT_WIDTH`]. It is that key and
    /// not `[loop] max_calls`, which bounds a different thing — see the module
    /// header's table.
    #[must_use]
    pub fn declare(manifest: &PluginManifest, workspaces: W) -> Self {
        let roles = manifest.roles.as_ref().map_or_else(BTreeMap::new, |roles| {
            roles
                .iter()
                .map(|(name, role)| (name.clone(), role.tier.clone()))
                .collect()
        });
        Self {
            plugin: manifest.name.clone(),
            workspaces,
            roles,
            seats: default_seats(),
            declared_width: manifest.loop_grant.max_fanout_width,
            width_ceiling: DEFAULT_HOST_MAX_FANOUT_WIDTH,
            fanout_ceiling: DEFAULT_HOST_MAX_FANOUTS,
            budget_usd: None,
            turn_lane: stella_core::turn_slots::FANOUT_LANE,
            spent: AtomicU32::new(0),
            live: Mutex::new(Vec::new()),
            ledger: Mutex::new(Vec::new()),
        }
    }

    /// Set this host's ceiling on one fan-out's width, whatever the manifest
    /// asks for.
    #[must_use]
    pub fn with_max_width(mut self, width: u32) -> Self {
        self.width_ceiling = width;
        self
    }

    /// Set this host's ceiling on fan-outs for the whole run.
    #[must_use]
    pub fn with_max_fanouts(mut self, fanouts: u32) -> Self {
        self.fanout_ceiling = fanouts;
        self
    }

    /// Request a USD carve for one whole fan-out, to be divided by its clamped
    /// width.
    ///
    /// An ask, not an authority — the substrate clamps each share against the
    /// parent's remaining headroom. `None`, the default, requests the whole
    /// headroom, which is still bounded whenever the session is.
    #[must_use]
    pub fn with_budget_usd(mut self, budget_usd: Option<f64>) -> Self {
        self.budget_usd = budget_usd;
        self
    }

    /// Bind one `tier` to the seat this host resolves it from.
    ///
    /// Binding a tier to anything but [`ModelCallRole::Worker`] does not buy a
    /// plugin a cheaper seat for writing work: the refusal below compares the
    /// resolved seat, so renaming only changes which word gets refused.
    #[must_use]
    pub fn with_seat(mut self, tier: impl Into<String>, seat: ModelCallRole) -> Self {
        self.seats.insert(tier.into(), seat);
        self
    }

    /// The `turn_instance` lane this plane's fan-outs are recorded in.
    ///
    /// [`ChildTurns::in_turn_lane`](super::ChildTurns::in_turn_lane)'s sibling,
    /// and its argument holds unchanged: the `n`-th fan-out takes the `n`-th
    /// slot of this lane, so it can never collide with a door's own rounds or
    /// with the child-turn plane beside it, whatever either counter reaches.
    /// One slot per fan-out and not per candidate — the candidates of one
    /// fan-out are told apart by `call_seq` within it, because a width the host
    /// clamps is not a number that may reach a database key.
    ///
    /// Defaults to [`stella_core::turn_slots::FANOUT_LANE`].
    #[must_use]
    pub fn in_turn_lane(mut self, lane: u32) -> Self {
        self.turn_lane = lane;
        self
    }

    /// The `turn_instance` the `seq`-th fan-out of this plane lands on.
    fn slot_for(&self, seq: u32) -> u32 {
        stella_core::turn_slots::slot(self.turn_lane, seq)
    }

    /// The effective ceiling on one fan-out's width, after the clamp.
    #[must_use]
    pub fn max_width(&self) -> u32 {
        self.declared_width
            .unwrap_or(self.width_ceiling)
            .min(self.width_ceiling)
    }

    /// The effective ceiling on fan-outs for the whole run.
    #[must_use]
    pub fn max_fanouts(&self) -> u32 {
        self.fanout_ceiling
    }

    /// Every fan-out this plane performed, in the order it performed them.
    #[must_use]
    pub fn spends(&self) -> Vec<CandidateFanoutSpend> {
        self.ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// What this plane spent in total, in USD.
    #[must_use]
    pub fn spent_usd(&self) -> f64 {
        self.spends().iter().map(|spend| spend.cost_usd).sum()
    }

    /// The candidates this run still holds, in the order they were minted.
    ///
    /// Public because it is the table adoption is fenced against, and a host
    /// that wants to answer "what is still on disk?" — at the end of a run, or
    /// in a failure report — should be able to ask without adopting anything.
    #[must_use]
    pub fn live(&self) -> Vec<CandidateWorkspace> {
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Resolve one role intent to the seat the candidates will be spent at, or
    /// say why they will not be.
    ///
    /// Public for [`ChildTurns::resolve`](super::ChildTurns::resolve)'s reason:
    /// a host that wants to answer "what would this plugin be allowed to
    /// spend?" before spending anything should be able to ask.
    ///
    /// # Errors
    ///
    /// [`HostCallRefusal::Undeclared`] when the manifest declares no such role,
    /// [`HostCallRefusal::Unavailable`] when its tier names no seat this host
    /// serves, and [`HostCallRefusal::Forbidden`] when it resolves anywhere but
    /// the worker's seat.
    pub fn resolve(&self, role: &str) -> Result<ModelCallRole, HostCallFailure> {
        let Some(tier) = self.roles.get(role) else {
            let declared = if self.roles.is_empty() {
                "none".to_string()
            } else {
                self.roles.keys().cloned().collect::<Vec<_>>().join(", ")
            };
            return Err(HostCallFailure::new(
                HostCallRefusal::Undeclared,
                format!(
                    "plugin \"{}\" declares no [roles.{role}], so the host will not fan a turn \
                     out at it; declared role intents: {declared}",
                    self.plugin
                ),
            ));
        };
        let Some(&seat) = self.seats.get(tier) else {
            return Err(HostCallFailure::new(
                HostCallRefusal::Unavailable,
                format!(
                    "role intent \"{role}\" asks for the \"{tier}\" tier, which this host binds \
                     to no responsibility; served tiers: {}",
                    self.seats.keys().cloned().collect::<Vec<_>>().join(", ")
                ),
            ));
        };
        if seat != ModelCallRole::Worker {
            return Err(HostCallFailure::new(
                HostCallRefusal::Forbidden,
                format!(
                    "role intent \"{role}\" resolves to the \"{}\" seat, and a candidate is the \
                     work rather than evidence about it — booking a writing turn anywhere but \
                     the worker's seat would attribute it to a responsibility that wrote \
                     nothing; name a tier that resolves to the worker",
                    seat_token(seat)
                ),
            ));
        }
        Ok(seat)
    }

    fn record(&self, spend: CandidateFanoutSpend) {
        self.ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(spend);
    }

    fn register(&self, workspaces: Vec<CandidateWorkspace>) {
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(workspaces);
    }

    /// Take the whole live table, leaving it empty.
    fn take_live(&self) -> Vec<CandidateWorkspace> {
        std::mem::take(
            &mut *self
                .live
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

impl<W: CandidateWorkspaces> CandidateFanouts<W> {
    /// Discard every candidate this run still holds, and report what would not
    /// go.
    ///
    /// The end-of-run half of the capability, and the reason it returns the
    /// failures rather than swallowing them: an un-removed workspace is disk
    /// left behind under a name only this table knew, and a host that cannot
    /// be told is a host whose user finds out with `df`.
    pub async fn discard_all(&self) -> Vec<CandidateFanoutError> {
        let mut failures = Vec::new();
        for workspace in self.take_live() {
            if let Err(error) = self.workspaces.remove(&workspace).await {
                failures.push(error);
            }
        }
        failures
    }
}

#[async_trait]
impl<W: CandidateWorkspaces> CandidateFanoutPlane for CandidateFanouts<W> {
    async fn fanout(
        &self,
        args: CandidateFanoutArgs,
    ) -> Result<CandidateFanoutResult, HostCallFailure> {
        // Resolution first, and before anything is spent or created: a refusal
        // the plugin could never have bought must not cost it an allowance.
        let seat = self.resolve(&args.role)?;

        // `fetch_add` rather than load-then-store: a plugin pipelining two
        // calls must not be able to spend the last fan-out twice.
        let taken = self.spent.fetch_add(1, Ordering::Relaxed);
        let max_fanouts = self.max_fanouts();
        if taken >= max_fanouts {
            return Err(HostCallFailure::new(
                HostCallRefusal::AllowanceSpent,
                format!(
                    "this plugin's allowance of {max_fanouts} candidate fan-out(s) is spent; \
                     score what you have"
                ),
            ));
        }

        // Clamped *before* the division below, so each candidate's share is a
        // slice of a budget the host agreed to rather than of the plugin's ask.
        // `max(1)` rather than a refusal for a zero width: a plugin asking for
        // no candidates has asked for nothing, and the honest answer to an ask
        // is the clamp, not an error (`RecallArgs::limit`'s discipline).
        let width = args.width.clamp(1, self.max_width());
        let share = self.budget_usd.map(|budget| budget / f64::from(width));

        // Creation is sequential and the turns are not. Two isolation
        // substrates racing to snapshot the same tree is the substrate's
        // hardest case (a git worktree registry is one writer), while N turns
        // in N disjoint roots is the easy one — and the concurrency that
        // matters is the second, since a turn is minutes and a snapshot is
        // milliseconds.
        let mut workspaces = Vec::with_capacity(width as usize);
        let mut creation_failure = None;
        for index in 0..width {
            match self
                .workspaces
                .create(&format!("plugin:{}/{}#{index}", self.plugin, args.role))
                .await
            {
                Ok(workspace) => workspaces.push(workspace),
                Err(error) => {
                    creation_failure = Some(error);
                    break;
                }
            }
        }
        if workspaces.is_empty() {
            let detail = creation_failure.map_or_else(
                || "the host created no candidate workspaces".to_string(),
                |error| error.to_string(),
            );
            return Err(HostCallFailure::new(HostCallRefusal::Failed, detail));
        }
        let ran = u32::try_from(workspaces.len()).unwrap_or(u32::MAX);

        let reports = futures_util::future::join_all(workspaces.iter().enumerate().map(
            |(index, workspace)| {
                self.workspaces.work(
                    workspace,
                    CandidateWork {
                        agent_id: format!("plugin:{}/{}#{index}", self.plugin, args.role),
                        instruction: args.instruction.clone(),
                        role: args.role.clone(),
                        seat,
                        budget_usd: share,
                        turn_instance: self.slot_for(taken),
                    },
                )
            },
        ))
        .await;

        // A workspace whose turn would not run is still a workspace: it goes
        // into the table so it can be discarded at the end of the run, and it
        // is reported to the plugin with an empty report and `completed:
        // false` rather than dropped. A candidate silently missing from the
        // answer is a plugin scoring a set it cannot see the shape of.
        let mut candidates = Vec::with_capacity(workspaces.len());
        let mut cost_usd = 0.0;
        let mut completed = 0;
        for (workspace, report) in workspaces.iter().zip(reports) {
            let report = report.unwrap_or_else(|error| CandidateReport {
                report: error.to_string(),
                completed: false,
                cost_usd: 0.0,
                files_changed: 0,
                lines_changed: 0,
            });
            cost_usd += report.cost_usd;
            completed += u32::from(report.completed);
            candidates.push(FanoutCandidate {
                candidate: workspace.handle.clone(),
                root: workspace.root.clone(),
                report: report.report,
                completed: report.completed,
                files_changed: report.files_changed,
                lines_changed: report.lines_changed,
            });
        }

        self.record(CandidateFanoutSpend {
            role: args.role.clone(),
            seat,
            requested_width: args.width,
            width: ran,
            cost_usd,
            completed,
        });
        self.register(workspaces);

        Ok(CandidateFanoutResult {
            requested: args.width,
            candidates,
        })
    }

    async fn adopt(
        &self,
        args: AdoptCandidateArgs,
    ) -> Result<AdoptCandidateResult, HostCallFailure> {
        // The table is taken whole, so a refusal below has to put it back:
        // taking first is what makes "exactly one adoption per run" true
        // without a second lock round-trip, and a plugin that named a handle
        // this host does not hold must not thereby have discarded the run's
        // candidates.
        let held = self.take_live();
        let Some(index) = held
            .iter()
            .position(|workspace| workspace.handle == args.candidate)
        else {
            let names = if held.is_empty() {
                "none".to_string()
            } else {
                held.iter()
                    .map(|workspace| workspace.handle.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            self.register(held);
            return Err(HostCallFailure::new(
                HostCallRefusal::Unavailable,
                format!(
                    "no candidate of this run answers to handle `{}`; live candidates: {names}",
                    args.candidate
                ),
            ));
        };

        let winner = &held[index];
        if let Err(error) = self.workspaces.adopt(winner).await {
            // Nothing was applied, so nothing is discarded either: the losing
            // candidates are the only copies of work that might still be
            // wanted, and throwing them away because the winner would not
            // apply is the one irreversible move available here.
            self.register(held);
            return Err(HostCallFailure::new(
                HostCallRefusal::Failed,
                error.to_string(),
            ));
        }

        // Every workspace goes, the winner's included: its bytes are on the
        // real tree now, so the copy is garbage that nothing can reach — the
        // table it was addressed through has just been emptied, so leaving it
        // would leak a worktree `discard_all` can no longer see. Only the
        // *siblings* are reported as discarded, because "thrown away without
        // landing" is a different fact from "landed, and its scratch copy
        // cleaned up".
        //
        // A removal that fails is left to `discard_all`'s contract rather than
        // raised here: the adoption succeeded, and failing the call now would
        // tell the plugin its winner did not land when it did.
        let mut discarded = Vec::with_capacity(held.len().saturating_sub(1));
        for (at, workspace) in held.iter().enumerate() {
            let _ = self.workspaces.remove(workspace).await;
            if at != index {
                discarded.push(workspace.handle.clone());
            }
        }

        Ok(AdoptCandidateResult {
            adopted: args.candidate,
            discarded,
        })
    }
}

/// Serving a plane a host still holds a handle on — [`ChildTurnPlane`]'s
/// reason exactly: [`HostPlanes::with_candidate_fanout`] takes the plane by
/// value, and the plane is also what knows what was spent and what is still on
/// disk.
///
/// [`ChildTurnPlane`]: super::ChildTurnPlane
/// [`HostPlanes::with_candidate_fanout`]: super::HostPlanes::with_candidate_fanout
#[async_trait]
impl<P: CandidateFanoutPlane + ?Sized> CandidateFanoutPlane for std::sync::Arc<P> {
    async fn fanout(
        &self,
        args: CandidateFanoutArgs,
    ) -> Result<CandidateFanoutResult, HostCallFailure> {
        (**self).fanout(args).await
    }

    async fn adopt(
        &self,
        args: AdoptCandidateArgs,
    ) -> Result<AdoptCandidateResult, HostCallFailure> {
        (**self).adopt(args).await
    }
}

/// A seat's wire token, as `step_usage` records it — through `serde_json`
/// rather than a second hand-written table, for [`super::child_turn`]'s reason:
/// a second copy of a closed enum's spellings is a rule in two places.
fn seat_token(seat: ModelCallRole) -> String {
    serde_json::to_value(seat)
        .ok()
        .and_then(|value| value.as_str().map(ToString::to_string))
        .unwrap_or_else(|| format!("{seat:?}").to_lowercase())
}
