// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Who does what, and whether it happens at all — the pipeline's
//! responsibility roster (#2381).
//!
//! # The two axes, and the one this crate deliberately does not offer
//!
//! A staged pipeline has three things an operator might want to change:
//!
//! 1. **Whether a responsibility runs** — the ablation axis. Turning triage
//!    off and leaving everything else running is how a measurement attributes
//!    an effect to triage rather than to "the pipeline".
//! 2. **Who performs it** — the assignment axis. Nothing about *triaging* a
//!    task requires one model specifically; which one is a deployment choice.
//! 3. **What order they run in** — the topology axis.
//!
//! This module offers the first two and **deliberately refuses the third**,
//! because in this pipeline the order is not a workflow, it is a proof
//! protocol: a flip is credited only against a baseline observed before
//! execution, and only when the worker never touched the oracle's files
//! (tamper exclusion, [`crate::witness`]). Reorder those and the flip oracle
//! still *reports a flip* — it has simply stopped meaning anything. A
//! configuration file that can silently convert a proof into a false proof is
//! not a feature. [`crate::replay::stage_transition_legal`] encodes the same
//! ordering for recorded streams, and an operator-authored graph would make
//! replay validation undecidable rather than merely stricter.
//!
//! So: **the set of responsibilities and their order are code; the assignment
//! and the enablement are configuration.**
//!
//! # Judgement is not on either axis
//!
//! The one thing this module does *not* make configurable is whether a model
//! **judges** the work. None does, and no key here can make one:
//! [`default_agent`] answers `None` for [`ModelCallRole::Verdict`] and
//! [`ModelCallRole::DistressGuidance`], which removes their rows from every
//! roster rather than defaulting them off — so there is no row to enable and
//! no agent to rebind. The reasoning is at that match arm; the short form is
//! that verification's only value is that its answer cannot be talked into
//! existence, so the ablation axis must not reach it in either direction.
//!
//! [`ModelCallRole::WitnessAuthor`] stays assignable, and the distinction is
//! the whole point: it *creates* an oracle, and what it produces is judged by
//! being run. Whether that call should exist at all is a live question with a
//! measured cost, but it is a different question from this one.
//!
//! # Why [`ModelCallRole`] is the responsibility vocabulary
//!
//! It already is one. Every paid call in this crate names a
//! [`ModelCallRole`] (what the call is *for*) beside a
//! [`Role`] (who *serves* it) — `ModelCallRole::Research` beside
//! `Role::Research`, `ModelCallRole::Triage` beside `Role::Triage`. That pair
//! is the binding this module makes configurable; before it, the pair was a
//! literal at each call site.
//!
//! Introducing a fourth "responsibility" enum beside `Role`, `ModelCallRole`
//! and `StageKind` would have been the wrong move for exactly the reason
//! AGENTS.md keeps a glossary: this workspace already has six identifiers that
//! read as "one thing the agent did", and the cost of another is paid forever
//! by every reader. [`ModelCallRole`] is total by construction
//! ([`ModelCallRole::ALL`] is macro-derived and compiler-checked), is already
//! the unit the paid-call ledger attributes spend to, and is already the
//! finest grain at which "who does this" is a real question — research and
//! planning are distinct responsibilities that happen to share the worker's
//! tier by default, and their costs are not alike.
//!
//! # The agent vocabulary is open; resolving it is not (#3472)
//!
//! The responsibilities are code (above). The *agents* are not: [`RoleTable`]
//! is an open set of names, seeded with the five built-ins and extended by
//! whatever a host contributed for this process, so a role can appear because
//! something was installed and disappear when it is removed. What stays closed
//! is **resolution**: a name outside the table is a named
//! [`RosterError::UnknownAgent`] before any spend, never a quiet fallback to
//! the session default, and a contributed row can carry only a routing intent
//! — a tier the host resolves against the operator's own providers — never a
//! model, an endpoint or a credential.
//!
//! The one property that had to be re-derived rather than inherited is
//! independence. While every agent name was built-in, comparing names *was*
//! comparing tiers; with the table open it is not, so
//! [`Roster::independence_losses`] resolves both sides. Otherwise a
//! contributor could nominate itself as its own independent verifier by
//! spelling the worker's tier under another name.
//!
//! # What a new responsibility costs
//!
//! Adding a [`ModelCallRole`] variant fails [`default_agent`]'s exhaustive
//! match with `E0004` until its default binding is declared. That is the whole
//! maintenance contract: a responsibility cannot enter the pipeline without
//! someone stating who owns it.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use stella_protocol::{ModelCallRole, Role};

/// The agent a responsibility is assigned to.
///
/// A **string** rather than an enum, and open at the configuration layer on
/// purpose: the vocabulary of agents is a deployment property, whereas the
/// vocabulary of responsibilities is a protocol property. The names that
/// resolve are [`RoleTable`]'s — the five built-ins ([`Self::BUILTIN`]) plus
/// whatever a host contributed for this process. A name outside that table is
/// a [`RosterError::UnknownAgent`] at validation time — named, listed, and
/// before any spend — never a silent fallback to the worker.
///
/// The names are [`Role`]'s own serde tokens, so an operator who has already
/// written `pipeline_verifier_model` reads `actor = "verifier"` without
/// learning a second spelling.
///
/// `#[serde(transparent)]` so a persisted binding is the bare name an operator
/// wrote, not a wrapper object: the stored form and the configured form are
/// then the same text, which is what lets one decoder read both (#2458).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentId(String);

impl AgentId {
    /// Every agent name that resolves to a model today, in the order a
    /// diagnostic should list them.
    ///
    /// Not derived from [`Role`]: that enum also carries `Embed`, `Vision`,
    /// `Image` and `Video`, which serve no pipeline responsibility and must
    /// not become bindable by being adjacent.
    pub const BUILTIN: &'static [&'static str] =
        &["worker", "triage", "plan", "research", "verifier"];

    /// Name an agent. Accepts anything; resolution is [`RoleTable::resolve`]'s
    /// job and unresolvable names are reported by [`Roster::validate`].
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The agent this [`Role`] is named by in configuration.
    #[must_use]
    pub fn from_role(role: Role) -> Self {
        Self(
            match role {
                Role::Worker => "worker",
                Role::Triage => "triage",
                Role::Plan => "plan",
                Role::Research => "research",
                Role::Verifier => "verifier",
                // Unreachable through this module — `default_agent` never
                // yields one and the role table never resolves one — but
                // stated rather than `unreachable!`d, because a panic in
                // library code on a value the type system permits is invariant
                // 5's exact prohibition. A media role named here resolves to a
                // name the table will refuse, which is the reportable outcome.
                Role::Embed => "embed",
                Role::Vision => "vision",
                Role::Image => "image",
                Role::Video => "video",
            }
            .to_string(),
        )
    }

    /// The built-in engine role that serves this agent, if one does.
    ///
    /// Deliberately **private**, and deliberately only half the answer: the
    /// whole answer is [`RoleTable::resolve`], which asks this first and then
    /// the contributed rows. Two public resolvers is how a caller comes to
    /// hold the closed answer while the run holds the open one, and the
    /// direction that would fail is the dangerous one — a contributed role
    /// judged "unknown" by one and "the worker's tier" by the other.
    fn builtin_role(&self) -> Option<Role> {
        match self.0.as_str() {
            "worker" => Some(Role::Worker),
            "triage" => Some(Role::Triage),
            "plan" => Some(Role::Plan),
            "research" => Some(Role::Research),
            "verifier" => Some(Role::Verifier),
            _ => None,
        }
    }

    /// The configured name, exactly as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Which names resolve, and to which engine tier — the role table.
///
/// **Open at the table, closed at resolution.** [`AgentId`] is a bare string
/// precisely so that a host can add a name here without a protocol change;
/// what a name may never do is resolve to *something arbitrary*. A name this
/// table does not hold is [`RosterError::UnknownAgent`] at
/// [`Roster::validate`] — named, listed, and before any spend. That is what
/// makes removal safe as well as addition: a binding written while a
/// contributor was installed refuses the next run once it is gone, instead of
/// quietly falling back to the session default and grading the work with
/// whatever happened to be there.
///
/// A contributed row carries a **routing intent and nothing else** — a name
/// and the [`Role`] tier it rides. No credential, no endpoint, no model slug:
/// the host resolves that tier against the operator's own providers exactly as
/// it does for a built-in name, so contributing a role can never contribute a
/// destination.
///
/// The built-in names ([`AgentId::BUILTIN`]) are always present and cannot be
/// redefined ([`RoleTableError::ShadowsBuiltin`]). A contributor able to
/// repoint `verifier` at the worker's tier would turn every existing binding
/// into self-grading without one line of configuration changing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoleTable {
    /// Contributed name → the tier it rides, ordered so diagnostics and
    /// [`Self::names`] read the same on every run (invariant 7's discipline:
    /// a list whose order varies between runs is a diff nobody can read).
    contributed: BTreeMap<String, Role>,
}

impl RoleTable {
    /// Add a contributed role.
    ///
    /// Every refusal is typed and none of them is a warning: a contribution
    /// that cannot be honoured must not half-apply, because the half that
    /// applied would be a name that resolves somewhere its contributor did not
    /// ask for.
    ///
    /// # Errors
    ///
    /// [`RoleTableError::ShadowsBuiltin`] for a built-in name,
    /// [`RoleTableError::Duplicate`] for a name already contributed,
    /// [`RoleTableError::NotAToken`] for a name that cannot be printed in a
    /// diagnostic and typed back, and [`RoleTableError::NotAPipelineTier`] for
    /// a media tier — which is [`AgentId::BUILTIN`]'s own exclusion, and would
    /// otherwise be reachable by contribution rather than by adjacency.
    pub fn contribute(
        &mut self,
        name: impl Into<String>,
        tier: Role,
    ) -> Result<(), RoleTableError> {
        let name = name.into();
        if !is_role_token(&name) {
            return Err(RoleTableError::NotAToken { name });
        }
        if AgentId::BUILTIN.contains(&name.as_str()) {
            return Err(RoleTableError::ShadowsBuiltin { name });
        }
        if self.contributed.contains_key(&name) {
            return Err(RoleTableError::Duplicate { name });
        }
        if !is_pipeline_tier(tier) {
            return Err(RoleTableError::NotAPipelineTier {
                name,
                tier: AgentId::from_role(tier).to_string(),
            });
        }
        self.contributed.insert(name, tier);
        Ok(())
    }

    /// [`Self::contribute`] by value, for a table built inside a struct
    /// literal — the shape [`Roster::with_roles`] is usually written in.
    ///
    /// # Errors
    ///
    /// As [`Self::contribute`].
    pub fn with(mut self, name: impl Into<String>, tier: Role) -> Result<Self, RoleTableError> {
        self.contribute(name, tier)?;
        Ok(self)
    }

    /// Remove a contributed role, answering whether it was there.
    ///
    /// The other half of contribution, and the half that has to work for the
    /// table to be honest: a role is *contributed*, not installed, so it
    /// leaves with whatever contributed it. A built-in name is never removed —
    /// `false` says so rather than pretending to have taken one away.
    pub fn withdraw(&mut self, name: &str) -> bool {
        self.contributed.remove(name).is_some()
    }

    /// The engine tier serving this agent, or `None` when nothing does.
    ///
    /// `None` is not a failure here — it is the answer [`Roster::validate`]
    /// turns into a named error listing [`Self::names`]. Returning it rather
    /// than defaulting is the whole safety property: an operator who typos
    /// `actor = "verifer"`, and a binding whose contributor has been removed,
    /// must not silently get the worker grading its own work.
    #[must_use]
    pub fn resolve(&self, agent: &AgentId) -> Option<Role> {
        agent
            .builtin_role()
            .or_else(|| self.contributed.get(agent.as_str()).copied())
    }

    /// Every name that resolves, built-ins first in [`AgentId::BUILTIN`] order
    /// and contributed names after, in name order.
    ///
    /// This is the list a diagnostic prints, so it is also the list that makes
    /// removal visible: a name absent from it is a name no binding can use.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        AgentId::BUILTIN
            .iter()
            .copied()
            .chain(self.contributed.keys().map(String::as_str))
            .collect()
    }
}

/// A contribution to the [`RoleTable`] that cannot be honoured.
///
/// Typed rather than a `String` for [`RosterError`]'s reason: a host branches
/// on these — a shadowed built-in is a contribution to refuse outright, a
/// duplicate is two contributors claiming one name, and a malformed name is a
/// manifest to fix (invariant 5).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoleTableError {
    /// A contribution named a built-in role. Built-ins are never redefinable:
    /// see [`RoleTable`]'s own docs for what that would cost.
    #[error(
        "`{name}` is a built-in role and cannot be redefined by a contribution — contribute a distinct name and bind it explicitly"
    )]
    ShadowsBuiltin {
        /// The name exactly as contributed.
        name: String,
    },
    /// Two contributions claimed one name. Refused rather than last-wins,
    /// because which of the two won would depend on load order.
    #[error("role `{name}` is already contributed — two contributors cannot share one role name")]
    Duplicate {
        /// The name exactly as contributed.
        name: String,
    },
    /// A name that cannot survive a round trip through a diagnostic: empty, or
    /// carrying whitespace or the comma the known-names list is joined with.
    #[error(
        "`{name}` is not a usable role name — a role name is one word, without whitespace or `,`, because it is printed in diagnostics and typed back into configuration"
    )]
    NotAToken {
        /// The name exactly as contributed.
        name: String,
    },
    /// A contribution asked for a tier the pipeline does not staff.
    #[error(
        "role `{name}` asks for the `{tier}` tier, which serves no pipeline responsibility — contributable tiers are worker, triage, plan, research, verifier"
    )]
    NotAPipelineTier {
        /// The name exactly as contributed.
        name: String,
        /// The tier asked for, as the name it is configured by.
        tier: String,
    },
}

/// Whether a [`Role`] staffs a pipeline responsibility.
///
/// Exhaustive on purpose: a [`Role`] variant added upstream fails to compile
/// here until someone decides whether a contributed role may ride it. The
/// media tiers are excluded for [`AgentId::BUILTIN`]'s reason — they serve no
/// pipeline responsibility, and contribution must not be the back door that
/// adjacency was already denied.
fn is_pipeline_tier(tier: Role) -> bool {
    match tier {
        Role::Worker | Role::Triage | Role::Plan | Role::Research | Role::Verifier => true,
        Role::Embed | Role::Vision | Role::Image | Role::Video => false,
    }
}

/// Whether a contributed name is one word a human can read out of an error and
/// type back into a settings file.
fn is_role_token(name: &str) -> bool {
    !name.is_empty() && !name.chars().any(|c| c.is_whitespace() || c == ',')
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The default agent for one responsibility, or `None` when the responsibility
/// is not the pipeline's to assign.
///
/// **This match is the maintenance contract.** It is exhaustive over
/// [`ModelCallRole`], so a variant added to that enum fails to compile here
/// until someone decides whether the pipeline owns it and who performs it. The
/// `None` arm is a deliberate statement, not a leftover: those calls are made
/// by `stella-cli` and `stella-core` outside this pipeline, so binding them
/// here would advertise a knob that steers nothing.
///
/// The bindings below are exactly what the call sites hard-coded before this
/// module existed — [`Roster::default`] therefore reproduces today's pipeline
/// byte for byte.
#[must_use]
pub fn default_agent(responsibility: ModelCallRole) -> Option<AgentId> {
    let role = match responsibility {
        ModelCallRole::Triage => Role::Triage,
        // Both ride the worker's tier by default (`Role::Plan`'s own docs), so
        // an unconfigured run resolves them exactly where it always did. They
        // are separate agents rather than one because their costs are not
        // alike: research is a fan-out of many short read-only calls, planning
        // is one call that writes the work order (#2374).
        ModelCallRole::Plan => Role::Plan,
        ModelCallRole::Research => Role::Research,
        ModelCallRole::Worker => Role::Worker,
        // Authoring a witness is the one verifier call that survives, and it
        // survives because it is categorically different from the two below:
        // it *creates the oracle* rather than substituting for one. Its output
        // is a test that either goes fail→pass or does not, and that judgement
        // is made by running it, not by reading it.
        //
        // It is nonetheless the most expensive call in the pipeline and its
        // removal is the tracked next step (see the issue referenced in this
        // PR): one measured run spent 10 calls, 267k input tokens and $0.67
        // arguing with itself about where to put a test and never wrote one.
        // The replacement is the worker's own `verify_done` shadow run, whose
        // fail-on-baseline / pass-on-candidate check is already deterministic;
        // that is a separable change with its own test surface, and folding it
        // in here would bury the decision this module makes.
        ModelCallRole::WitnessAuthor => Role::Verifier,
        // **Judgement buys no model call.** Rendering a verdict and steering a
        // distressed worker were the verifier's other two jobs, and the
        // pipeline no longer issues either.
        //
        // `None` here is not "off by default"; it is the structural lock.
        // `Roster::default` builds its rows by filtering `ModelCallRole::ALL`
        // through this function, and `Roster::apply` rejects any configured key
        // whose responsibility `is_assignable` denies. So an unassignable
        // responsibility has no row to enable, no agent to rebind, and no
        // settings spelling that reaches it — there is nothing for an operator
        // or a future call site to switch back on.
        //
        // Why it had to be structural rather than a default: the thing being
        // removed is *authority*, and an authority that a config key can
        // restore is one a deployment will restore. Verification is the one
        // stage whose whole value is that its answer cannot be talked into
        // existence, and a model in that seam could — measurably: the verdict
        // agreed with Terminal-Bench's grader 46% of the time, its false passes
        // cost 5 tasks outright, and its prose fed back as instruction made a
        // worker destroy a correctly-recovered commit on `fix-git`. What
        // replaces it is not a weaker reviewer but a narrower claim: a
        // fail→pass flip of one normalized command, or `LadderRung::Unverified`.
        //
        // The deterministic machinery is untouched and is what now carries the
        // whole load: the flip oracle, the tamper exclusion, the mutation
        // guard, and the worker's own `verify_done` shadow run.
        ModelCallRole::DistressGuidance | ModelCallRole::Verdict => return None,
        // Repairs follow their principal — see `principal_of`.
        ModelCallRole::PlanRepair | ModelCallRole::WitnessRepair => return None,
        // Not this pipeline's calls. `Unknown` is the legacy-event default and
        // names no call at all; the rest are made by other drivers.
        ModelCallRole::Unknown
        | ModelCallRole::AgentAuthor
        | ModelCallRole::SkillAuthor
        | ModelCallRole::DomainInference
        | ModelCallRole::Reflection
        | ModelCallRole::Summarization => return None,
    };
    Some(AgentId::from_role(role))
}

/// The responsibility a repair call belongs to, when it is one.
///
/// A repair is a second attempt at the *same* call — re-authoring a plan the
/// parser rejected, fixing a witness that did not fail — so it is issued to
/// whichever agent produced the output being repaired. That is not a policy
/// choice this module gets to make configurable: asking a second agent to
/// repair a first agent's malformed JSON is a different operation, not a
/// rebinding of this one.
///
/// So repairs carry no roster row, and naming one in configuration is
/// [`RosterError::FollowsPrincipal`] rather than a silently ignored key. This
/// function exists to make that error name the row the operator actually
/// wanted.
#[must_use]
pub fn principal_of(responsibility: ModelCallRole) -> Option<ModelCallRole> {
    match responsibility {
        ModelCallRole::PlanRepair => Some(ModelCallRole::Plan),
        ModelCallRole::WitnessRepair => Some(ModelCallRole::WitnessAuthor),
        _ => None,
    }
}

/// One responsibility's binding: whether it runs, and who performs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    /// The responsibility this binds.
    pub responsibility: ModelCallRole,
    /// Whether the responsibility runs at all. `false` is the ablation switch
    /// (#2381): the stage emits no [`stella_protocol::StageKind`] frame and
    /// buys no call, so a recorded stream shows the ablation rather than
    /// requiring a reader to infer it.
    pub enabled: bool,
    /// The agent that performs it.
    pub agent: AgentId,
}

/// Something a roster says that cannot be honoured.
///
/// Typed rather than a `String` because callers branch on the kind: an unknown
/// agent is the operator's typo (fix the config), a disabled worker is a
/// configuration that cannot produce a result at all, and the two want
/// different remedies (invariant 5).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RosterError {
    /// A responsibility names an agent nothing resolves.
    #[error(
        "responsibility `{responsibility}` is assigned to unknown agent `{agent}` — known agents are {known}"
    )]
    UnknownAgent {
        /// The responsibility whose binding is unresolvable, as its wire token.
        responsibility: String,
        /// The name exactly as configured.
        agent: String,
        /// The names that would have worked, comma-separated.
        known: String,
    },
    /// A responsibility the pipeline does not own was named in configuration.
    #[error(
        "`{responsibility}` is not a pipeline responsibility — it is issued outside the staged pipeline and assigning it here would steer nothing"
    )]
    NotAssignable {
        /// The responsibility named, as its wire token.
        responsibility: String,
    },
    /// A repair call was named. Repairs follow the agent whose output they
    /// repair; see [`principal_of`].
    #[error(
        "`{responsibility}` is a repair of `{principal}` and is always issued to the same agent — configure `{principal}` instead"
    )]
    FollowsPrincipal {
        /// The repair responsibility named, as its wire token.
        responsibility: String,
        /// The responsibility it repairs, as its wire token.
        principal: String,
    },
    /// Configuration named a responsibility that is not a [`ModelCallRole`] at
    /// all.
    #[error("`{name}` is not a known responsibility — assignable ones are {assignable}")]
    UnknownResponsibility {
        /// The key exactly as configured.
        name: String,
        /// The responsibility tokens that would have worked, comma-separated.
        assignable: String,
    },
    /// The worker was disabled. A pipeline whose worker never runs changes no
    /// files, so every downstream stage would grade an empty diff — an
    /// ablation that measures nothing rather than one that measures less.
    #[error(
        "the `worker` responsibility cannot be disabled — a run with no worker executes nothing, so there is no result for any other stage to observe"
    )]
    WorkerDisabled,
}

/// One responsibility's configured overrides, exactly as written: an absent
/// field means "no opinion" and keeps the built-in binding.
///
/// Deliberately all-`Option` rather than a full [`Assignment`], because the
/// difference between "enabled = true" and "did not mention enabled" is the
/// difference between a deployment that pinned today's default and one that
/// inherits tomorrow's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssignmentOverride {
    /// Whether the responsibility runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Who performs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
}

/// A responsibility whose assigned agent is the worker's, so the independence
/// its stage assumes does not hold.
///
/// Reported, never refused: binding the verdict to the worker is a legitimate
/// posture (and a measurement someone may deliberately want), but it must
/// arrive as a stated fact rather than as an unexplained pass. The pipeline's
/// standing posture is that degradation warns and never silently disables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndependenceLoss {
    /// The responsibility that lost its independence.
    pub responsibility: ModelCallRole,
    /// The agent it shares with the worker.
    pub agent: AgentId,
}

/// The total responsibility→assignment table.
///
/// Total by construction: [`Self::assignment`] answers for every
/// [`ModelCallRole`] the pipeline owns, because the rows are built from
/// [`ModelCallRole::ALL`] filtered through [`default_agent`] rather than
/// written out. [`Self::default`] is today's pipeline exactly — every
/// responsibility enabled, every agent the one its call site used to name
/// literally — so a deployment that configures nothing sees no change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roster {
    /// One row per assignable responsibility, in [`ModelCallRole::ALL`] order.
    rows: Vec<Assignment>,
    /// Configuration rows [`Self::apply`] could not turn into bindings.
    ///
    /// Carried on the roster rather than left to the caller so that
    /// [`Self::validate`] is **total**: every reason this roster is
    /// unrunnable — a key that named nothing as much as a binding that
    /// resolves to nothing — is reachable from the value alone. That is what
    /// lets the refusal live in `Pipeline::run`, where every host gets it,
    /// instead of in whichever host remembered to check the return of
    /// `apply`.
    rejected: Vec<RosterError>,
    /// Which agent names resolve, and to which tier.
    ///
    /// Held on the roster rather than passed to each query so that a roster is
    /// answerable on its own: `role()`, `validate()` and
    /// `independence_losses()` must all resolve through the *same* table, and
    /// a table threaded through arguments is a table one of the three
    /// eventually gets called without.
    ///
    /// Deliberately not part of [`Self::overrides`]: the table is the host's
    /// statement about this process, not the operator's about this run, so a
    /// restored binding is re-resolved against whatever is installed now. That
    /// is the point — a binding whose contributor is gone must be refused, not
    /// carried along with a private copy of the table that once resolved it.
    roles: RoleTable,
}

impl Default for Roster {
    fn default() -> Self {
        Self {
            rejected: Vec::new(),
            roles: RoleTable::default(),
            rows: ModelCallRole::ALL
                .iter()
                .filter_map(|&responsibility| {
                    default_agent(responsibility).map(|agent| Assignment {
                        responsibility,
                        enabled: true,
                        agent,
                    })
                })
                .collect(),
        }
    }
}

impl Roster {
    /// Whether the pipeline assigns this responsibility at all.
    #[must_use]
    pub fn is_assignable(responsibility: ModelCallRole) -> bool {
        default_agent(responsibility).is_some()
    }

    /// Resolve this roster's bindings against `roles` instead of the built-ins
    /// alone — how a host hands the run the roles its installed contributors
    /// added.
    ///
    /// Takes the whole table rather than adding one role at a time, because a
    /// half-built table is a roster that would refuse a binding it is about to
    /// be able to honour. The bindings themselves are untouched: contributing
    /// a role makes a name *resolvable*, never *assigned* — an assignment is
    /// still the operator's, through [`Self::apply`].
    #[must_use]
    pub fn with_roles(mut self, roles: RoleTable) -> Self {
        self.roles = roles;
        self
    }

    /// The table this roster resolves agent names against.
    #[must_use]
    pub fn roles(&self) -> &RoleTable {
        &self.roles
    }

    /// This responsibility's binding, or `None` when the pipeline does not own
    /// it.
    #[must_use]
    pub fn assignment(&self, responsibility: ModelCallRole) -> Option<&Assignment> {
        self.rows
            .iter()
            .find(|row| row.responsibility == responsibility)
    }

    /// Every binding, in [`ModelCallRole::ALL`] order.
    ///
    /// Deterministic order because this feeds the run's posture reporting, and
    /// a posture line whose order varies between runs is a diff nobody can
    /// read.
    #[must_use]
    pub fn assignments(&self) -> &[Assignment] {
        &self.rows
    }

    /// Whether this responsibility runs.
    ///
    /// A responsibility the pipeline does not own answers `false`: it does not
    /// run *here*, which is the question every call site is asking.
    #[must_use]
    pub fn enabled(&self, responsibility: ModelCallRole) -> bool {
        self.assignment(responsibility)
            .is_some_and(|row| row.enabled)
    }

    /// The engine role serving this responsibility.
    ///
    /// `None` when the responsibility is unowned, disabled, or bound to an
    /// agent nothing resolves — the three cases in which a call site must not
    /// make its call. Callers that need to tell them apart ask
    /// [`Self::validate`] first, which is where the unresolvable case is
    /// reported; by the time a run is under way an unresolvable binding has
    /// already been refused.
    #[must_use]
    pub fn role(&self, responsibility: ModelCallRole) -> Option<Role> {
        let row = self.assignment(responsibility)?;
        row.enabled
            .then(|| self.roles.resolve(&row.agent))
            .flatten()
    }

    /// Disable a responsibility (the #2381 ablation switch).
    ///
    /// Silently ignores a responsibility the pipeline does not own; naming one
    /// is a configuration error caught by [`Self::validate`], not a reason for
    /// this setter to fail. Keeping the two separate is what lets a caller
    /// apply a whole config block and then report *every* problem in it,
    /// rather than stopping at the first.
    pub fn set_enabled(&mut self, responsibility: ModelCallRole, enabled: bool) {
        if let Some(row) = self
            .rows
            .iter_mut()
            .find(|row| row.responsibility == responsibility)
        {
            row.enabled = enabled;
        }
    }

    /// [`Self::set_enabled`] by value, for a caller building a roster inside a
    /// struct literal.
    ///
    /// [`crate::PipelineConfig`] is almost always written as one literal, and
    /// before #2458 the way to say "this run authors no witness" was a `bool`
    /// field in it. Forcing every such caller to break the literal apart into
    /// a `let mut` and a statement is how a mechanical change acquires a
    /// dozen incidental diffs — and how the next author reaches for a second
    /// field instead.
    #[must_use]
    pub fn with_enabled(mut self, responsibility: ModelCallRole, enabled: bool) -> Self {
        self.set_enabled(responsibility, enabled);
        self
    }

    /// Reassign a responsibility to a different agent.
    ///
    /// Ignores an unowned responsibility for the same reason as
    /// [`Self::set_enabled`], and does **not** check that the agent resolves —
    /// that is [`Self::validate`]'s job, so a config carrying two typos
    /// reports both.
    pub fn set_agent(&mut self, responsibility: ModelCallRole, agent: AgentId) {
        if let Some(row) = self
            .rows
            .iter_mut()
            .find(|row| row.responsibility == responsibility)
        {
            row.agent = agent;
        }
    }

    /// Apply a configured override block, returning every problem it had.
    ///
    /// The translation from configuration to bindings lives here, in the
    /// engine crate, rather than in the CLI: it is a pure function over owned
    /// data, which is where invariant 2 puts decision logic, and it is the only
    /// copy — a second one in a host would be a second answer to "what does
    /// `actor = "verifer"` mean".
    ///
    /// **Applies what it can and records what it cannot.** A bad row leaves
    /// its binding at the default rather than aborting the block, so the
    /// result describes the *whole* configuration rather than stopping at the
    /// first typo. Every rejection is retained on the roster, so a caller that
    /// discards this return value has not lost it: [`Self::validate`] still
    /// reports it, and `Pipeline::run` refuses before spend — a run under a
    /// roster the operator did not write is a result described by the wrong
    /// posture (the same reasoning as
    /// [`crate::PipelineError::WitnessAuthorUnavailable`]).
    pub fn apply<I>(&mut self, overrides: I) -> Vec<RosterError>
    where
        I: IntoIterator<Item = (String, AssignmentOverride)>,
    {
        for (name, spec) in overrides {
            let Some(responsibility) = parse_responsibility(&name) else {
                self.rejected.push(RosterError::UnknownResponsibility {
                    name,
                    assignable: assignable_tokens().join(", "),
                });
                continue;
            };
            if let Some(principal) = principal_of(responsibility) {
                self.rejected.push(RosterError::FollowsPrincipal {
                    responsibility: responsibility_token(responsibility),
                    principal: responsibility_token(principal),
                });
                continue;
            }
            if !Self::is_assignable(responsibility) {
                self.rejected.push(RosterError::NotAssignable {
                    responsibility: responsibility_token(responsibility),
                });
                continue;
            }
            if let Some(enabled) = spec.enabled {
                self.set_enabled(responsibility, enabled);
            }
            if let Some(agent) = spec.agent {
                self.set_agent(responsibility, agent);
            }
        }
        self.validate()
    }

    /// This roster as the smallest override block that reproduces it from
    /// [`Self::default`] — the inverse of [`Self::apply`], in the same
    /// vocabulary.
    ///
    /// Deliberately the same vocabulary, because that is what lets a caller
    /// which must *persist* a roster (the resume frame, #2458) write this and
    /// read it back through `apply`. The decoder for a stored roster is then
    /// literally the decoder for a configured one, and a stored row cannot
    /// come to mean something a settings row does not.
    ///
    /// A diff from the default rather than the whole table, for durability
    /// rather than size: a responsibility added to [`ModelCallRole`] after a
    /// block was written must come back at its new default, not absent. A
    /// roster that configured nothing yields an empty map, so the common case
    /// costs nothing to carry.
    ///
    /// The roster's rejected rows are deliberately not represented. Those
    /// describe configuration text this roster could not turn into bindings,
    /// and text that is not being re-parsed cannot be re-rejected — a run
    /// holding one was refused before spend
    /// ([`crate::PipelineError::InvalidRoster`]), so no checkpoint of it
    /// exists to restore.
    #[must_use]
    pub fn overrides(&self) -> BTreeMap<String, AssignmentOverride> {
        let baseline = Self::default();
        self.rows
            .iter()
            .filter_map(|row| {
                let default = baseline.assignment(row.responsibility);
                let enabled =
                    (default.map(|d| d.enabled) != Some(row.enabled)).then_some(row.enabled);
                let agent =
                    (default.map(|d| &d.agent) != Some(&row.agent)).then(|| row.agent.clone());
                (enabled.is_some() || agent.is_some()).then(|| {
                    (
                        responsibility_token(row.responsibility),
                        AssignmentOverride { enabled, agent },
                    )
                })
            })
            .collect()
    }

    /// Every problem this roster has, in row order.
    ///
    /// Returns all of them rather than the first: a roster is written by hand
    /// in a settings file, and reporting one typo per run turns a five-minute
    /// fix into five runs. Called once, before any spend.
    #[must_use]
    pub fn validate(&self) -> Vec<RosterError> {
        let mut errors = self.rejected.clone();
        for row in &self.rows {
            if row.responsibility == ModelCallRole::Worker && !row.enabled {
                errors.push(RosterError::WorkerDisabled);
            }
            if self.roles.resolve(&row.agent).is_none() {
                errors.push(RosterError::UnknownAgent {
                    responsibility: responsibility_token(row.responsibility),
                    agent: row.agent.to_string(),
                    // The table as it stands *now*, not `AgentId::BUILTIN`:
                    // an operator whose binding named a contributed role has
                    // to be able to see, from this one line, whether the role
                    // is gone or the name is misspelled.
                    known: self.roles.names().join(", "),
                });
            }
        }
        errors
    }

    /// The responsibilities whose agent is the worker's, so the independence
    /// their stage assumes does not hold.
    ///
    /// Only the responsibilities that *have* an independence requirement are
    /// considered: authoring a witness and rendering the verdict both mean
    /// something different when the worker performs them. Distress guidance
    /// does not — steering a worker with its own model is weaker advice, not a
    /// broken proof — so it is deliberately absent, and a witness repair
    /// follows its author's binding rather than carrying one.
    ///
    /// Compares assigned agents **and the tiers they resolve to**, not
    /// resolved models: two distinct tiers that happen to resolve to one model
    /// is the *other* independence question, and
    /// `Pipeline::witness_author_independence` already answers it against the
    /// router. This one catches the case that answer cannot see — a
    /// configuration that asked for self-grading outright.
    ///
    /// The tier half is what a name comparison alone cannot do once
    /// [`RoleTable`] is open, and it is the reason this is not merely a
    /// cosmetic generalisation. While every name was built-in, name equality
    /// *was* tier equality — the two were the same table read twice. A
    /// contributed role breaks that: `vera-verifier` riding [`Role::Worker`]
    /// is a different name from `worker` and the very same seat, so a
    /// contributor could otherwise nominate itself as its own independent
    /// verifier and the roster would report nothing. Resolving both sides
    /// closes it, and closes it for a contribution nobody audited as much as
    /// for one somebody did.
    ///
    /// Reporting, not refusal — the posture [`IndependenceLoss`] documents is
    /// unchanged. What changes is that a loss can no longer be *hidden* by
    /// spelling it differently.
    #[must_use]
    pub fn independence_losses(&self) -> Vec<IndependenceLoss> {
        let Some(worker) = self.assignment(ModelCallRole::Worker) else {
            return Vec::new();
        };
        let worker_tier = self.roles.resolve(&worker.agent);
        [ModelCallRole::WitnessAuthor, ModelCallRole::Verdict]
            .into_iter()
            .filter_map(|responsibility| {
                let row = self.assignment(responsibility)?;
                // Names first so an unresolvable pair — two bindings on one
                // typo — is still reported as the self-grading it asks for.
                // Then tiers, which only ever *adds* losses: `worker_tier` is
                // `None` exactly when the worker's own binding is unresolvable,
                // and that run is refused before spend either way.
                let shared = row.agent == worker.agent
                    || (worker_tier.is_some() && self.roles.resolve(&row.agent) == worker_tier);
                (row.enabled && shared).then(|| IndependenceLoss {
                    responsibility,
                    agent: row.agent.clone(),
                })
            })
            .collect()
    }
}

/// A responsibility's wire token, for error text a human will paste back into
/// a settings file.
///
/// Goes through `serde_json` rather than a second hand-written table because
/// the token an error names must be the token the config parser accepts, and
/// two spellings of that is how the last one drifted. The fallback is
/// unreachable for a fieldless enum and exists only to keep this total.
pub(crate) fn responsibility_token(responsibility: ModelCallRole) -> String {
    serde_json::to_value(responsibility)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{responsibility:?}"))
}

/// A responsibility from its wire token — the inverse of
/// [`responsibility_token`], and through the same serde definition for the
/// same reason.
///
/// Accepts every [`ModelCallRole`], assignable or not, so that naming
/// `reflection` is reported as [`RosterError::NotAssignable`] ("this exists
/// but the pipeline does not issue it") rather than as an unknown key. The
/// two diagnoses send an operator to different places.
fn parse_responsibility(name: &str) -> Option<ModelCallRole> {
    serde_json::from_value(serde_json::Value::String(name.to_string())).ok()
}

/// Every responsibility an operator may name, as wire tokens in
/// [`ModelCallRole::ALL`] order.
fn assignable_tokens() -> Vec<String> {
    ModelCallRole::ALL
        .iter()
        .filter(|&&role| Roster::is_assignable(role))
        .map(|&role| responsibility_token(role))
        .collect()
}

#[cfg(test)]
mod tests;
