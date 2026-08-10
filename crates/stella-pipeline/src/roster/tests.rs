// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Roster unit tests.
//!
//! The load-bearing one is [`the_default_roster_contains_only_live_model_work`]:
//! every other property here is about a configuration someone wrote, and that
//! one is about the configuration nobody wrote — which is what almost every
//! run uses.

use super::*;

fn override_of(enabled: Option<bool>, agent: Option<&str>) -> AssignmentOverride {
    AssignmentOverride {
        enabled,
        agent: agent.map(AgentId::new),
    }
}

/// **Requirement 4 of #2381, as a test.** The bindings below are transcribed
/// from the `resolve_provider(Role::…)` literal at each call site as it stood
/// before this module existed, so a default roster is provably the pipeline
/// that shipped rather than a fresh set of opinions that happen to look
/// similar.
#[test]
fn the_default_roster_contains_only_live_model_work() {
    let roster = Roster::default();
    for (responsibility, expected) in [
        (ModelCallRole::Triage, Role::Triage),
        // Its own agent since #2374, so a seat can turn the research fan-out
        // down without touching the planner. Still the worker's tier when
        // nothing pins it (`Router::resolve`), so the shipped pipeline is
        // unchanged in what it actually runs.
        (ModelCallRole::Research, Role::Research),
        (ModelCallRole::Plan, Role::Plan),
        (ModelCallRole::Worker, Role::Worker),
    ] {
        assert!(
            roster.enabled(responsibility),
            "{responsibility:?} must be enabled by default"
        );
        assert_eq!(
            roster.role(responsibility),
            Some(expected),
            "{responsibility:?} must keep the agent its call site named literally"
        );
    }
    assert!(
        roster.validate().is_empty(),
        "the default roster must not be able to be invalid"
    );
    assert!(
        roster.independence_losses().is_empty(),
        "the default roster grades nothing with the worker's own agent"
    );
}

/// The assignable set is exactly the calls this crate issues. Pinned as a
/// list because the alternative — trusting `default_agent`'s `None` arm to
/// stay honest — is how a call role issued elsewhere silently acquires a knob
/// that steers nothing.
#[test]
fn only_the_calls_this_pipeline_issues_are_assignable() {
    let assignable: Vec<_> = ModelCallRole::ALL
        .iter()
        .copied()
        .filter(|&role| Roster::is_assignable(role))
        .collect();
    assert_eq!(
        assignable,
        vec![
            ModelCallRole::Triage,
            ModelCallRole::Research,
            ModelCallRole::Plan,
            ModelCallRole::Worker,
        ],
        "assignable responsibilities drifted from the calls the pipeline makes"
    );
}

/// Verification is an oracle boundary, not a configurable set of model jobs.
/// The wire roles remain parseable for old telemetry, but no live roster may
/// resolve them to an agent.
#[test]
fn verifier_owned_work_is_not_assignable() {
    for responsibility in [
        ModelCallRole::WitnessAuthor,
        ModelCallRole::DistressGuidance,
        ModelCallRole::Verdict,
    ] {
        assert!(
            !Roster::is_assignable(responsibility),
            "{responsibility:?} must not be executable"
        );
        assert!(
            Roster::default().assignment(responsibility).is_none(),
            "{responsibility:?} must have no default binding"
        );
    }
}

/// The mitigation users already wrote before this boundary became permanent
/// remains valid. It has no row to change, which is exactly the point.
#[test]
fn an_old_disabled_verifier_row_is_an_accepted_noop() {
    let mut roster = Roster::default();
    let errors = roster.apply([("verdict".to_string(), override_of(Some(false), None))]);
    assert!(
        errors.is_empty(),
        "a safe old mitigation stays valid: {errors:?}"
    );
    assert!(roster.assignment(ModelCallRole::Verdict).is_none());
}

/// Historical settings often kept the old agent binding beside the explicit
/// disable. The row still grants no authority, so it remains a safe no-op.
#[test]
fn an_old_disabled_verifier_row_with_an_agent_is_an_accepted_noop() {
    let mut roster = Roster::default();
    let errors = roster.apply([(
        "verdict".to_string(),
        override_of(Some(false), Some("verifier")),
    )]);
    assert!(
        errors.is_empty(),
        "a disabled historical row cannot revive retired work: {errors:?}"
    );
    assert!(roster.assignment(ModelCallRole::Verdict).is_none());
}

/// Retired execution authority must fail before spend rather than being
/// silently ignored or revived through a settings file.
#[test]
fn retired_verifier_work_cannot_be_enabled_or_reassigned() {
    let mut roster = Roster::default();
    let errors = roster.apply([
        ("witness_author".to_string(), override_of(Some(true), None)),
        ("verdict".to_string(), override_of(None, Some("triage"))),
    ]);
    assert_eq!(errors.len(), 2, "both authority requests are refused");
    for error in errors {
        assert!(
            error.to_string().contains("retired"),
            "the refusal must explain the permanent boundary: {error}"
        );
    }
}

/// A repair is not a rebinding: naming one must send the operator to the row
/// they actually meant rather than silently doing nothing.
#[test]
fn a_repair_call_points_at_its_principal_instead_of_carrying_a_row() {
    for (repair, principal) in [
        ("plan_repair", "plan"),
        ("witness_repair", "witness_author"),
    ] {
        let mut roster = Roster::default();
        let errors = roster.apply([(repair.to_string(), override_of(None, Some("worker")))]);
        assert_eq!(
            errors,
            vec![RosterError::FollowsPrincipal {
                responsibility: repair.to_string(),
                principal: principal.to_string(),
            }],
            "`{repair}` must name `{principal}` rather than be ignored"
        );
    }
}

/// Every default binding resolves. A default that did not would make
/// `Roster::role` return `None` on an untouched configuration, which every
/// call site reads as "do not make this call".
#[test]
fn every_default_binding_resolves_to_a_role() {
    for &responsibility in ModelCallRole::ALL {
        let Some(agent) = default_agent(responsibility) else {
            continue;
        };
        assert!(
            agent.role().is_some(),
            "{responsibility:?} defaults to `{agent}`, which resolves to no role"
        );
    }
}

/// The wire tokens an error names must be the tokens the parser accepts —
/// both directions, over the whole family.
#[test]
fn responsibility_tokens_round_trip() {
    for &responsibility in ModelCallRole::ALL {
        let token = responsibility_token(responsibility);
        assert_eq!(
            parse_responsibility(&token),
            Some(responsibility),
            "token `{token}` did not parse back to {responsibility:?}"
        );
    }
}

/// The #2381 ablation switch, at the type level: one responsibility off leaves
/// every other one exactly as it was.
#[test]
fn disabling_one_responsibility_leaves_the_rest_alone() {
    let mut roster = Roster::default();
    let errors = roster.apply([("triage".to_string(), override_of(Some(false), None))]);

    assert!(errors.is_empty(), "disabling triage is legal: {errors:?}");
    assert!(!roster.enabled(ModelCallRole::Triage));
    assert_eq!(
        roster.role(ModelCallRole::Triage),
        None,
        "a disabled responsibility resolves no role, so its call site cannot make its call"
    );
    for still_on in [
        ModelCallRole::Plan,
        ModelCallRole::Research,
        ModelCallRole::Worker,
    ] {
        assert!(
            roster.enabled(still_on),
            "{still_on:?} must be untouched by a triage ablation"
        );
    }
}

/// A live responsibility can still move between model agents.
#[test]
fn a_responsibility_can_be_reassigned_to_another_agent() {
    let mut roster = Roster::default();
    let errors = roster.apply([("research".to_string(), override_of(None, Some("triage")))]);

    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(roster.role(ModelCallRole::Research), Some(Role::Triage));
    assert_eq!(
        roster.role(ModelCallRole::Plan),
        Some(Role::Plan),
        "reassigning research must not move planning"
    );
}

/// The typo case, and the reason `role()` returns `Option` rather than falling
/// back: `actor = "verifer"` must not quietly become the worker.
#[test]
fn an_unknown_agent_is_named_and_never_silently_resolved() {
    let mut roster = Roster::default();
    let errors = roster.apply([("research".to_string(), override_of(None, Some("verifer")))]);

    assert_eq!(
        errors,
        vec![RosterError::UnknownAgent {
            responsibility: "research".to_string(),
            agent: "verifer".to_string(),
            known: "worker, triage, plan, research, verifier".to_string(),
        }]
    );
    assert_eq!(
        roster.role(ModelCallRole::Research),
        None,
        "an unresolvable binding must resolve to nothing, never to a default"
    );
}

/// A responsibility that exists but that this pipeline does not issue gets a
/// different diagnosis from one that does not exist at all — the two send an
/// operator to different places.
#[test]
fn a_real_but_unissued_responsibility_is_distinguished_from_a_typo() {
    let mut roster = Roster::default();
    let errors = roster.apply([
        ("reflection".to_string(), override_of(Some(false), None)),
        ("triarge".to_string(), override_of(Some(false), None)),
    ]);

    assert_eq!(
        errors,
        vec![
            RosterError::NotAssignable {
                responsibility: "reflection".to_string(),
            },
            RosterError::UnknownResponsibility {
                name: "triarge".to_string(),
                assignable: assignable_tokens().join(", "),
            },
        ]
    );
}

/// Every problem in one pass. A roster is hand-written; reporting one typo per
/// run turns a five-minute fix into five runs.
#[test]
fn apply_reports_every_problem_rather_than_the_first() {
    let mut roster = Roster::default();
    let errors = roster.apply([
        ("nonsense".to_string(), override_of(Some(false), None)),
        ("verdict".to_string(), override_of(None, Some("nobody"))),
    ]);

    assert_eq!(errors.len(), 2, "both rows must be reported: {errors:?}");
}

/// The one ablation that measures nothing rather than measuring less.
#[test]
fn the_worker_cannot_be_disabled() {
    let mut roster = Roster::default();
    let errors = roster.apply([("worker".to_string(), override_of(Some(false), None))]);

    assert!(
        errors.contains(&RosterError::WorkerDisabled),
        "disabling the worker must be refused: {errors:?}"
    );
}

/// An absent field means "no opinion", which is what lets a deployment pin one
/// axis without freezing the other at today's value.
#[test]
fn an_absent_field_keeps_the_built_in_binding() {
    let mut roster = Roster::default();
    roster.apply([("research".to_string(), override_of(Some(false), None))]);

    assert!(!roster.enabled(ModelCallRole::Research));
    assert_eq!(
        roster
            .assignment(ModelCallRole::Research)
            .map(|row| row.agent.clone()),
        Some(AgentId::new("research")),
        "disabling a responsibility must not also reset who owns it"
    );
}

/// Media roles are adjacent to the four bindable ones in `Role` and must not
/// become bindable by adjacency.
#[test]
fn media_roles_are_not_bindable_agents() {
    for name in ["embed", "vision", "image", "video"] {
        assert_eq!(
            AgentId::new(name).role(),
            None,
            "`{name}` must not resolve as a pipeline agent"
        );
    }
}

/// **#2458's structural guarantee.** A roster written out as an override block
/// and read back through [`Roster::apply`] is the roster that was written.
///
/// This is what makes it safe for the resume frame to carry a roster at all:
/// [`Roster::overrides`] is the only encoder and `apply` is the only decoder —
/// and `apply` is the *same* function the settings path calls, so a stored
/// ablation cannot come to mean something a configured one does not.
#[test]
fn a_roster_round_trips_through_its_own_override_block() {
    let mut original = Roster::default();
    original.set_enabled(ModelCallRole::Triage, false);
    original.set_enabled(ModelCallRole::Plan, false);
    original.set_agent(ModelCallRole::Research, AgentId::new("triage"));

    let mut restored = Roster::default();
    let problems = restored.apply(original.overrides());

    assert!(
        problems.is_empty(),
        "a block this build wrote must read back clean: {problems:?}"
    );
    assert_eq!(
        restored, original,
        "every ablation and every reassignment survives the round trip"
    );
}

/// The common case costs nothing to carry: a run that configured nothing
/// encodes to an empty block, so an ordinary resume frame does not grow.
#[test]
fn a_default_roster_has_nothing_to_override() {
    assert!(
        Roster::default().overrides().is_empty(),
        "the default is the absence of opinions, not a table of them"
    );
}

/// Invariant 4 for the block that crosses into `stella-cli`'s resume frame.
#[test]
fn an_override_block_survives_json() {
    let mut roster = Roster::default();
    roster.set_enabled(ModelCallRole::Triage, false);
    roster.set_agent(ModelCallRole::Research, AgentId::new("plan"));

    let json = serde_json::to_string(&roster.overrides()).expect("the block serializes");
    let parsed: BTreeMap<String, AssignmentOverride> =
        serde_json::from_str(&json).expect("and reads back");
    assert_eq!(parsed, roster.overrides());

    let mut restored = Roster::default();
    assert!(restored.apply(parsed).is_empty());
    assert_eq!(restored, roster);
}

/// A block that names only some rows leaves the rest at their defaults.
///
/// The decoder rebuilds from [`Roster::default`] and applies a diff, so a
/// [`ModelCallRole`] variant added after a block was written comes back at its
/// new default rather than absent — the property that makes a persisted roster
/// total by construction rather than by the writer's diligence.
#[test]
fn a_block_naming_only_some_rows_leaves_the_rest_at_their_defaults() {
    let mut restored = Roster::default();
    let problems = restored.apply([("triage".to_string(), override_of(Some(false), None))]);

    assert!(problems.is_empty());
    assert!(!restored.enabled(ModelCallRole::Triage));
    for untouched in [
        ModelCallRole::Plan,
        ModelCallRole::Research,
        ModelCallRole::Worker,
    ] {
        assert_eq!(
            restored.assignment(untouched),
            Roster::default().assignment(untouched),
            "{untouched:?} was not named, so it must keep the built-in binding"
        );
    }
}
