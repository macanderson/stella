// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Roster unit tests.
//!
//! The load-bearing one is [`the_default_roster_is_the_pipeline_that_shipped`]:
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
fn the_default_roster_is_the_pipeline_that_shipped() {
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
        (ModelCallRole::WitnessAuthor, Role::Verifier),
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
            ModelCallRole::WitnessAuthor,
            ModelCallRole::Worker,
        ],
        "assignable responsibilities drifted from the calls the pipeline makes"
    );
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
            RoleTable::default().resolve(&agent).is_some(),
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
        ModelCallRole::Worker,
        ModelCallRole::WitnessAuthor,
        ModelCallRole::Research,
    ] {
        assert!(
            roster.enabled(still_on),
            "{still_on:?} must be untouched by a triage ablation"
        );
    }
}

/// Mac's first example: triage authors the witness test. A config change, not
/// a code change.
#[test]
fn a_responsibility_can_be_reassigned_to_another_agent() {
    let mut roster = Roster::default();
    let errors = roster.apply([(
        "witness_author".to_string(),
        override_of(None, Some("triage")),
    )]);

    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        roster.role(ModelCallRole::WitnessAuthor),
        Some(Role::Triage)
    );
    assert_eq!(
        roster.role(ModelCallRole::Plan),
        Some(Role::Plan),
        "reassigning one responsibility must not move another"
    );
}

/// Mac's second example — the worker grading itself. Legal, and *reported*:
/// the posture is a choice someone may want to measure, but it can never be
/// silent.
#[test]
fn binding_a_graded_responsibility_to_the_worker_is_legal_and_reported() {
    let mut roster = Roster::default();
    let errors = roster.apply([(
        "witness_author".to_string(),
        override_of(None, Some("worker")),
    )]);

    assert!(
        errors.is_empty(),
        "self-grading is a posture, not a configuration error: {errors:?}"
    );
    assert_eq!(
        roster.independence_losses(),
        vec![IndependenceLoss {
            responsibility: ModelCallRole::WitnessAuthor,
            agent: AgentId::new("worker"),
        }],
        "a witness the worker wrote for itself must be reported as an independence loss"
    );
}

/// **The witness for the removal.** No configuration can put a model back in
/// the judgement seat.
///
/// This is the shape a removal has to be tested in. Asserting that a default
/// run makes no verdict call would pass just as well if the call were merely
/// defaulted off, and "off by default" is one settings key away from being on
/// — which, for the one stage whose value is that its answer cannot be talked
/// into existence, is not a guarantee at all.
///
/// So it asserts the stronger property: the responsibilities are *unassignable*.
/// `Roster::default` builds its rows by filtering `ModelCallRole::ALL` through
/// `default_agent`, so a `None` there means no row exists to enable; and
/// `Roster::apply` rejects a key `is_assignable` denies, so the configuration
/// surface cannot create one. Both halves are checked, in both spellings an
/// operator might reach for.
#[test]
fn no_configuration_can_put_a_model_back_in_the_judgement_seat() {
    for responsibility in [ModelCallRole::Verdict, ModelCallRole::DistressGuidance] {
        let token = responsibility_token(responsibility);

        assert!(
            !Roster::is_assignable(responsibility),
            "`{token}` must not be assignable"
        );
        assert!(
            Roster::default().assignment(responsibility).is_none(),
            "`{token}` must have no row to enable"
        );
        assert!(
            !Roster::default().enabled(responsibility),
            "`{token}` must never report as enabled"
        );
        assert_eq!(
            Roster::default().role(responsibility),
            None,
            "`{token}` must resolve no role, so no call site can make its call"
        );

        // Both spellings an operator would reach for: turn it on, and point it
        // at an agent. Each is refused by name rather than silently ignored.
        for spec in [
            override_of(Some(true), None),
            override_of(None, Some("verifier")),
        ] {
            let mut roster = Roster::default();
            let errors = roster.apply([(token.clone(), spec)]);
            assert_eq!(
                errors,
                vec![RosterError::NotAssignable {
                    responsibility: token.clone(),
                }],
                "configuring `{token}` must be refused by name"
            );
            assert_eq!(
                roster.role(responsibility),
                None,
                "a refused row must leave `{token}` resolving nothing"
            );
        }
    }
}

/// A disabled responsibility cannot lose independence it was never going to
/// exercise — otherwise ablating the verifier would report a self-grading
/// verdict that never runs.
#[test]
fn a_disabled_responsibility_reports_no_independence_loss() {
    let mut roster = Roster::default();
    roster.apply([(
        "witness_author".to_string(),
        override_of(Some(false), Some("worker")),
    )]);

    assert!(roster.independence_losses().is_empty());
}

/// The typo case, and the reason `role()` returns `Option` rather than falling
/// back: `actor = "verifer"` must not quietly become the worker.
#[test]
fn an_unknown_agent_is_named_and_never_silently_resolved() {
    let mut roster = Roster::default();
    let errors = roster.apply([(
        "witness_author".to_string(),
        override_of(None, Some("verifer")),
    )]);

    assert_eq!(
        errors,
        vec![RosterError::UnknownAgent {
            responsibility: "witness_author".to_string(),
            agent: "verifer".to_string(),
            known: "worker, triage, plan, research, verifier".to_string(),
        }]
    );
    assert_eq!(
        roster.role(ModelCallRole::WitnessAuthor),
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
        (
            "witness_author".to_string(),
            override_of(None, Some("nobody")),
        ),
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
    roster.apply([("witness_author".to_string(), override_of(Some(false), None))]);

    assert!(!roster.enabled(ModelCallRole::WitnessAuthor));
    assert_eq!(
        roster
            .assignment(ModelCallRole::WitnessAuthor)
            .map(|row| row.agent.clone()),
        Some(AgentId::new("verifier")),
        "disabling a responsibility must not also reset who owns it"
    );
}

/// Media roles are adjacent to the four bindable ones in `Role` and must not
/// become bindable by adjacency.
#[test]
fn media_roles_are_not_bindable_agents() {
    for name in ["embed", "vision", "image", "video"] {
        assert_eq!(
            RoleTable::default().resolve(&AgentId::new(name)),
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
    original.set_enabled(ModelCallRole::WitnessAuthor, false);
    original.set_agent(ModelCallRole::Plan, AgentId::new("triage"));

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
    roster.set_agent(ModelCallRole::WitnessAuthor, AgentId::new("plan"));

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
    let problems = restored.apply([("witness_author".to_string(), override_of(Some(false), None))]);

    assert!(problems.is_empty());
    assert!(!restored.enabled(ModelCallRole::WitnessAuthor));
    for untouched in [
        ModelCallRole::Triage,
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

/// **The #3472 witness, first half.** A role a host contributed resolves, and
/// stops resolving when whatever contributed it is removed.
///
/// Both directions in one test on purpose. That a name can be added is the
/// easy half and proves nothing on its own — a table that only ever grew would
/// let a binding survive the uninstall of the thing it named. The second half
/// is the one that has to hold: the binding is refused, by name, listing what
/// *is* installed, before the run spends anything.
#[test]
fn a_contributed_role_resolves_and_stops_resolving_when_it_is_withdrawn() {
    let mut table = RoleTable::default();
    table
        .contribute("vera-witness", Role::Verifier)
        .expect("a fresh name on a pipeline tier is contributable");

    let mut roster = Roster::default().with_roles(table.clone());
    assert!(
        roster
            .apply([(
                "witness_author".to_string(),
                override_of(None, Some("vera-witness")),
            )])
            .is_empty(),
        "a contributed role must be bindable exactly like a built-in one"
    );
    assert_eq!(
        roster.role(ModelCallRole::WitnessAuthor),
        Some(Role::Verifier)
    );
    assert!(
        roster.roles().names().contains(&"vera-witness"),
        "the role table is what the `/models` row list is drawn from"
    );

    // The contributor is removed. The binding it left behind must not survive
    // it — and must not survive it *loudly*, not by resolving somewhere else.
    assert!(table.withdraw("vera-witness"));
    let orphaned = roster.with_roles(table);
    assert!(!orphaned.roles().names().contains(&"vera-witness"));
    assert_eq!(
        orphaned.role(ModelCallRole::WitnessAuthor),
        None,
        "an unresolvable binding must never fall back to another agent"
    );
    let problems = orphaned.validate();
    assert_eq!(
        problems,
        vec![RosterError::UnknownAgent {
            responsibility: "witness_author".to_string(),
            agent: "vera-witness".to_string(),
            known: AgentId::BUILTIN.join(", "),
        }],
        "the refusal must name the role and list the ones that would work"
    );
}

/// **The #3472 witness, second half — the security one.** A contributed role
/// cannot smuggle the worker back in as its own independent verifier.
///
/// The bypass this closes: independence used to be a comparison of agent
/// *names*, which was the same question as tiers only because every name was
/// built-in. A contributor that could name the worker's tier `vera-judge`
/// would author its own witness, and the roster — the one place that reports
/// self-grading before any spend — would have said nothing at all.
#[test]
fn a_contributed_role_cannot_launder_the_worker_into_the_verifier_seat() {
    let table = RoleTable::default()
        .with("vera-judge", Role::Worker)
        .expect("riding the worker's tier is a legal contribution");
    let mut roster = Roster::default().with_roles(table);
    let problems = roster.apply([(
        "witness_author".to_string(),
        override_of(None, Some("vera-judge")),
    )]);

    assert!(
        problems.is_empty(),
        "the binding resolves — the point is that it is *reported*, not refused: {problems:?}"
    );
    assert_eq!(
        roster.independence_losses(),
        vec![IndependenceLoss {
            responsibility: ModelCallRole::WitnessAuthor,
            agent: AgentId::new("vera-judge"),
        }],
        "a role riding the worker's tier is the worker, whatever it is called"
    );
}

/// A contribution that keeps its distance from the worker is not reported.
///
/// The other side of the test above, and the reason it is not simply "warn on
/// every contributed role": a notice per row would train operators to skip the
/// one that matters.
#[test]
fn a_contributed_role_on_an_independent_tier_reports_no_loss() {
    let table = RoleTable::default()
        .with("vera-witness", Role::Verifier)
        .expect("contributable");
    let mut roster = Roster::default().with_roles(table);
    roster.apply([(
        "witness_author".to_string(),
        override_of(None, Some("vera-witness")),
    )]);

    assert!(roster.independence_losses().is_empty());
}

/// A contribution may never redefine a built-in name.
///
/// This is the quietest of the attacks and the worst: repointing `verifier` at
/// the worker's tier turns every binding already written — including the
/// default roster's own `witness_author` — into self-grading, with no
/// configuration changing and nothing to notice.
#[test]
fn a_contribution_cannot_redefine_a_built_in_role() {
    for name in AgentId::BUILTIN {
        assert_eq!(
            RoleTable::default().contribute(*name, Role::Worker),
            Err(RoleTableError::ShadowsBuiltin {
                name: (*name).to_string()
            }),
            "`{name}` is built in and must not be redefinable"
        );
    }
    assert_eq!(
        Roster::default().role(ModelCallRole::WitnessAuthor),
        Some(Role::Verifier),
        "the default roster's own independence rests on that refusal"
    );
}

/// The remaining contribution refusals, each typed so a host can tell a
/// manifest to fix from a load-order collision to report.
#[test]
fn a_contribution_is_refused_by_name_rather_than_ignored() {
    let mut table = RoleTable::default();
    table
        .contribute("vera-witness", Role::Verifier)
        .expect("ok");
    assert_eq!(
        table.contribute("vera-witness", Role::Triage),
        Err(RoleTableError::Duplicate {
            name: "vera-witness".to_string()
        }),
        "which of two contributors won would otherwise depend on load order"
    );

    for name in ["", " ", "vera judge", "vera,judge"] {
        assert_eq!(
            RoleTable::default().contribute(name, Role::Verifier),
            Err(RoleTableError::NotAToken {
                name: name.to_string()
            }),
            "`{name}` cannot survive a round trip through a diagnostic"
        );
    }

    // Media tiers are excluded from `AgentId::BUILTIN` deliberately;
    // contribution must not be the back door adjacency was denied.
    for tier in [Role::Embed, Role::Vision, Role::Image, Role::Video] {
        assert!(
            matches!(
                RoleTable::default().contribute("vera-embed", tier),
                Err(RoleTableError::NotAPipelineTier { .. })
            ),
            "{tier:?} staffs no pipeline responsibility"
        );
    }
}
