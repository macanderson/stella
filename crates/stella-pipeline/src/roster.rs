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
//! 2. **Who performs it** — the assignment axis. Nothing about *authoring a
//!    witness test* requires the verifier's model specifically; it requires a
//!    model that is not the worker. Which one is a deployment choice.
//! 3. **What order they run in** — the topology axis.
//!
//! This module offers the first two and **deliberately refuses the third**,
//! because in this pipeline the order is not a workflow, it is a proof
//! protocol. The witness is authored in a pristine snapshot of the
//! *pre-execution* tree by a model that is not the worker, and its fail→pass
//! flip is credited only when the worker never touched the witness files
//! (tamper exclusion, [`crate::witness`]). Move authoring after the worker has
//! seen the diff, or bind it to the worker, and the flip oracle still *reports
//! a flip* — it has simply stopped meaning anything. A configuration file that
//! can silently convert a proof into a false proof is not a feature.
//! [`crate::replay::stage_transition_legal`] encodes the same ordering for
//! recorded streams, and an operator-authored graph would make replay
//! validation undecidable rather than merely stricter.
//!
//! So: **the set of responsibilities and their order are code; the assignment
//! and the enablement are configuration.**
//!
//! # Why [`ModelCallRole`] is the responsibility vocabulary
//!
//! It already is one. Every paid call in this crate names a
//! [`ModelCallRole`] (what the call is *for*) beside a
//! [`Role`] (who *serves* it) — `ModelCallRole::WitnessAuthor` beside
//! `Role::Verifier`, `ModelCallRole::Triage` beside `Role::Triage`. That pair
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
//! finest grain at which "who does this" is a real question — the witness
//! author and the verdict are distinct responsibilities that happen to share
//! an agent today.
//!
//! # What a new responsibility costs
//!
//! Adding a [`ModelCallRole`] variant fails [`default_agent`]'s exhaustive
//! match with `E0004` until its default binding is declared. That is the whole
//! maintenance contract: a responsibility cannot enter the pipeline without
//! someone stating who owns it.

use std::fmt;

use stella_protocol::{ModelCallRole, Role};

/// The agent a responsibility is assigned to.
///
/// A **string** rather than an enum, and open at the configuration layer on
/// purpose: the vocabulary of agents is a deployment property, whereas the
/// vocabulary of responsibilities is a protocol property. Today four names
/// resolve ([`Self::BUILTIN`]); a fifth is a [`RosterError::UnknownAgent`] at
/// validation time — named, listed, and before any spend — never a silent
/// fallback to the worker.
///
/// The names are [`Role`]'s own serde tokens, so an operator who has already
/// written `pipeline_verifier_model` reads `actor = "verifier"` without
/// learning a second spelling.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentId(String);

impl AgentId {
    /// Every agent name that resolves to a model today, in the order a
    /// diagnostic should list them.
    ///
    /// Not derived from [`Role`]: that enum also carries `Embed`, `Vision`,
    /// `Image` and `Video`, which serve no pipeline responsibility and must
    /// not become bindable by being adjacent.
    pub const BUILTIN: &'static [&'static str] = &["worker", "triage", "plan", "verifier"];

    /// Name an agent. Accepts anything; resolution is
    /// [`Self::role`]'s job and unresolvable names are reported by
    /// [`Roster::validate`].
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
                Role::Verifier => "verifier",
                // Unreachable through this module — `default_agent` never
                // yields one and `role()` never resolves to one — but stated
                // rather than `unreachable!`d, because a panic in library code
                // on a value the type system permits is invariant 5's exact
                // prohibition. A media role named here resolves to a name
                // `role()` will refuse, which is the reportable outcome.
                Role::Embed => "embed",
                Role::Vision => "vision",
                Role::Image => "image",
                Role::Video => "video",
            }
            .to_string(),
        )
    }

    /// The engine role that serves this agent, or `None` when nothing does.
    ///
    /// `None` is not a failure here — it is the answer [`Roster::validate`]
    /// turns into a named error listing [`Self::BUILTIN`]. Returning it rather
    /// than defaulting is the whole safety property: an operator who typos
    /// `actor = "verifer"` must not silently get the worker grading its own
    /// work.
    #[must_use]
    pub fn role(&self) -> Option<Role> {
        match self.0.as_str() {
            "worker" => Some(Role::Worker),
            "triage" => Some(Role::Triage),
            "plan" => Some(Role::Plan),
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
        // The planner rides the worker's tier by default (`Role::Plan`'s own
        // docs), and the research children ride the planner: the findings
        // exist to be read by the plan prompt.
        ModelCallRole::Plan | ModelCallRole::Research => Role::Plan,
        ModelCallRole::Worker => Role::Worker,
        // Three halves of the verifier's job (`Role::Verifier`'s docs): author
        // the witness, steer a distressed worker, render the verdict. Distinct
        // responsibilities that share one agent by default — which is
        // precisely why they are separate rows here.
        ModelCallRole::WitnessAuthor | ModelCallRole::DistressGuidance | ModelCallRole::Verdict => {
            Role::Verifier
        }
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssignmentOverride {
    /// Whether the responsibility runs.
    pub enabled: Option<bool>,
    /// Who performs it.
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
}

impl Default for Roster {
    fn default() -> Self {
        Self {
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
        row.enabled.then(|| row.agent.role()).flatten()
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
    /// **Applies what it can and reports what it cannot.** A bad row leaves
    /// its binding at the default rather than aborting the block, so the
    /// returned errors describe the *whole* configuration. The caller decides
    /// what a non-empty result costs — `stella run` refuses before spend,
    /// because a run under a roster the operator did not write is a result
    /// described by the wrong posture (the same reasoning as
    /// [`crate::PipelineError::WitnessAuthorUnavailable`]).
    pub fn apply<I>(&mut self, overrides: I) -> Vec<RosterError>
    where
        I: IntoIterator<Item = (String, AssignmentOverride)>,
    {
        let mut errors = Vec::new();
        for (name, spec) in overrides {
            let Some(responsibility) = parse_responsibility(&name) else {
                errors.push(RosterError::UnknownResponsibility {
                    name,
                    assignable: assignable_tokens().join(", "),
                });
                continue;
            };
            if let Some(principal) = principal_of(responsibility) {
                errors.push(RosterError::FollowsPrincipal {
                    responsibility: responsibility_token(responsibility),
                    principal: responsibility_token(principal),
                });
                continue;
            }
            if !Self::is_assignable(responsibility) {
                errors.push(RosterError::NotAssignable {
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
        errors.extend(self.validate());
        errors
    }

    /// Every problem this roster has, in row order.
    ///
    /// Returns all of them rather than the first: a roster is written by hand
    /// in a settings file, and reporting one typo per run turns a five-minute
    /// fix into five runs. Called once, before any spend.
    #[must_use]
    pub fn validate(&self) -> Vec<RosterError> {
        let mut errors = Vec::new();
        for row in &self.rows {
            if row.responsibility == ModelCallRole::Worker && !row.enabled {
                errors.push(RosterError::WorkerDisabled);
            }
            if row.agent.role().is_none() {
                errors.push(RosterError::UnknownAgent {
                    responsibility: responsibility_token(row.responsibility),
                    agent: row.agent.to_string(),
                    known: AgentId::BUILTIN.join(", "),
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
    /// Compares assigned agents, not resolved models: two distinct agents that
    /// happen to resolve to one model is the *other* independence question,
    /// and `Pipeline::witness_author_independence` already answers it against
    /// the router. This one catches the case that answer cannot see — a
    /// configuration that asked for self-grading outright.
    #[must_use]
    pub fn independence_losses(&self) -> Vec<IndependenceLoss> {
        let Some(worker) = self.assignment(ModelCallRole::Worker) else {
            return Vec::new();
        };
        [ModelCallRole::WitnessAuthor, ModelCallRole::Verdict]
            .into_iter()
            .filter_map(|responsibility| {
                let row = self.assignment(responsibility)?;
                (row.enabled && row.agent == worker.agent).then(|| IndependenceLoss {
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
