// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Tests for [`super`] — parsing a plugin manifest and the cross-field rules
//! `PluginManifest::validate` enforces.
//!
//! Split out of `manifest.rs` at the gate's 1500-line ceiling (#5228). The
//! tests moved rather than the validation rules: several of them match on
//! *which* error a manifest breaking two rules reports first, so relocating
//! the rules risks the one thing that issue lists as a constraint, while
//! moving the tests cannot change behaviour at all.

use super::*;

fn parse(text: &str) -> Result<PluginManifest, ManifestError> {
    PluginManifest::from_toml_str(text)
}

#[test]
fn undeclared_loop_block_is_grade_none_with_no_hooks() {
    let m = parse("name = \"bundle\"").unwrap();
    assert_eq!(m.loop_grant.participation, Participation::None);
    assert!(m.loop_grant.hooks.is_empty());
}

#[test]
fn the_ladder_is_monotone_and_every_grade_includes_itself() {
    use Participation::*;
    let ladder = [None, Observer, Steering, Arbiter];
    for (i, higher) in ladder.iter().enumerate() {
        for (j, lower) in ladder.iter().enumerate() {
            assert_eq!(higher.includes(*lower), i >= j, "{higher} vs {lower}");
        }
    }
}

#[test]
fn unknown_top_level_key_is_a_load_error() {
    let err = parse("name = \"x\"\nsurprise = 1").unwrap_err();
    assert!(matches!(err, ManifestError::Parse(_)));
}

#[test]
fn unknown_hook_name_is_a_load_error() {
    let err = parse(
        "name = \"x\"\n[loop]\nparticipation = \"steering\"\nhooks = [\"PostToolUse\", \"OnMerge\"]",
    )
    .unwrap_err();
    assert!(matches!(err, ManifestError::Parse(_)));
}

#[test]
fn unknown_grade_is_a_load_error() {
    let err = parse("name = \"x\"\n[loop]\nparticipation = \"root\"").unwrap_err();
    assert!(matches!(err, ManifestError::Parse(_)));
}

#[test]
fn duplicate_hooks_are_rejected() {
    let err = parse(
        "name = \"x\"\n[loop]\nparticipation = \"steering\"\nhooks = [\"PreToolUse\", \"PreToolUse\"]",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        ManifestError::DuplicateHook {
            hook: HookEvent::PreToolUse
        }
    ));
}

/// **The other half of "an undeclared hook is never invoked".** A plugin
/// may spell the loop-lifecycle events — they are one vocabulary — and may
/// not be routed at them: they are dispatched by the self-driving loop
/// from the operator's own hooks settings, outside any turn. Refused by
/// name, so an author learns it here rather than from a grant that
/// silently never fires (#3599).
#[test]
fn a_plugin_may_not_be_routed_at_a_loop_lifecycle_hook() {
    // Every event outside a turn, taken from the vocabulary rather than
    // listed here: a hand-kept list is what this was, and it covered two
    // of them while seventeen more were being added (#4017).
    let outside: Vec<HookEvent> = HookEvent::ALL
        .into_iter()
        .filter(|event| !event.in_turn())
        .collect();
    assert!(
        outside.len() > 2,
        "the loop vocabulary is the subject; a two-event set means this \
         test is asserting about the wrong thing: {outside:?}"
    );
    for hook in outside {
        let err = parse(&format!(
            "name = \"x\"\n[loop]\nparticipation = \"steering\"\nhooks = [\"{hook}\"]"
        ))
        .unwrap_err();
        assert!(
            matches!(err, ManifestError::HookNotAvailableToPlugins { .. }),
            "{hook} must be refused: {err:?}"
        );
        let text = err.to_string();
        assert!(text.contains(hook.as_str()), "{text}");
        assert!(text.contains("outside any turn"), "{text}");
    }

    // And the in-turn five still load, so the refusal above is a line
    // through the vocabulary rather than a refusal of every hook.
    for hook in HookEvent::ALL.into_iter().filter(|event| event.in_turn()) {
        // `Stop` is the arbiter's own hook and drags that grade's own
        // rules with it, which is a different subject from this one.
        let extra = if hook == HookEvent::Stop {
            "\n\n[requirements]\nr = \"the tests pass\""
        } else {
            ""
        };
        let grade = if hook == HookEvent::Stop {
            "arbiter"
        } else {
            "steering"
        };
        parse(&format!(
            "name = \"x\"\n[loop]\nparticipation = \"{grade}\"\nhooks = [\"{hook}\"]{extra}"
        ))
        .unwrap_or_else(|error| panic!("{hook} must load: {error}"));
    }
}

/// The set-based duplicate checks must name the same offender the prefix
/// scan they replaced did — the *first element that repeats an earlier
/// one*, in declaration order — and must keep the blank-stage check
/// interleaved with them. Both are order-sensitive, so both are pinned
/// by a list that would answer differently under a re-ordered pass.
#[test]
fn the_first_repeat_in_declaration_order_is_the_one_reported() {
    let hooks = parse(
        "name = \"x\"\n[loop]\nparticipation = \"steering\"\nhooks = [\"PreToolUse\", \"PostToolUse\", \"PostToolUse\", \"PreToolUse\"]",
    )
    .unwrap_err();
    assert!(
        matches!(
            hooks,
            ManifestError::DuplicateHook {
                hook: HookEvent::PostToolUse
            }
        ),
        "the earlier-repeating PreToolUse pair must not preempt the \
         PostToolUse repeat that occurs first, got {hooks:?}"
    );

    let head = "name = \"x\"\n[loop]\nparticipation = \"steering\"\n\n[subloop]\n";

    // A duplicate before a blank: the duplicate wins, which only holds
    // while the two checks share one pass.
    let dupe_first = parse(&format!("{head}stages = [\"plan\", \"plan\", \" \"]")).unwrap_err();
    assert!(
        matches!(dupe_first, ManifestError::DuplicateStage { ref stage } if stage == "plan"),
        "got {dupe_first:?}"
    );

    // A blank before a duplicate: the blank wins, for the same reason.
    let blank_first = parse(&format!("{head}stages = [\"plan\", \" \", \"plan\"]")).unwrap_err();
    assert!(
        matches!(blank_first, ManifestError::EmptyStageName),
        "got {blank_first:?}"
    );
}

/// The casing split documented on [`HookEvent`] is a decision, so it is
/// pinned: `HookEvent` mirrors the PascalCase a user already types in
/// `.stella/settings.json`, `Participation` is lowercase because this
/// crate coined it. A `rename_all` added to either — the tidying this
/// spelling invites — fails here rather than silently invalidating every
/// shipped manifest.
#[test]
fn wire_strings_are_pinned_on_both_sides() {
    for (hook, wire) in [
        (HookEvent::SessionStart, "SessionStart"),
        (HookEvent::PreToolUse, "PreToolUse"),
        (HookEvent::PostToolUse, "PostToolUse"),
        (HookEvent::Stop, "Stop"),
        (HookEvent::PreCompact, "PreCompact"),
    ] {
        assert_eq!(serde_json::to_value(hook).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<HookEvent>(wire.into()).unwrap(),
            hook
        );
        assert_eq!(hook.to_string(), wire, "Display must match the wire string");
    }

    for (grade, wire) in [
        (Participation::None, "none"),
        (Participation::Observer, "observer"),
        (Participation::Steering, "steering"),
        (Participation::Arbiter, "arbiter"),
    ] {
        assert_eq!(serde_json::to_value(grade).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<Participation>(wire.into()).unwrap(),
            grade
        );
        assert_eq!(
            grade.to_string(),
            wire,
            "Display must match the wire string"
        );
    }
}

#[test]
fn observer_may_not_declare_hooks() {
    let err =
        parse("name = \"x\"\n[loop]\nparticipation = \"observer\"\nhooks = [\"PostToolUse\"]")
            .unwrap_err();
    assert!(matches!(err, ManifestError::HooksRequireSteering { .. }));
}

#[test]
fn steering_may_not_declare_the_stop_hook() {
    let err = parse("name = \"x\"\n[loop]\nparticipation = \"steering\"\nhooks = [\"Stop\"]")
        .unwrap_err();
    assert!(matches!(err, ManifestError::StopHookRequiresArbiter { .. }));
}

#[test]
fn an_arbiter_must_declare_stop() {
    let err = parse(
        "name = \"x\"\n[loop]\nparticipation = \"arbiter\"\nhooks = [\"PreToolUse\"]\n\n[requirements]\nr = \"a requirement\"",
    )
    .unwrap_err();
    assert!(matches!(err, ManifestError::ArbiterMustDeclareStop));
}

#[test]
fn max_holds_below_arbiter_is_rejected_and_zero_is_rejected_at_arbiter() {
    let below =
        parse("name = \"x\"\n[loop]\nparticipation = \"steering\"\nmax_holds = 2").unwrap_err();
    assert!(matches!(
        below,
        ManifestError::MaxHoldsRequiresArbiter { .. }
    ));

    let zero = parse(
        "name = \"x\"\n[loop]\nparticipation = \"arbiter\"\nhooks = [\"Stop\"]\nmax_holds = 0\n\n[requirements]\nr = \"a requirement\"",
    )
    .unwrap_err();
    assert!(matches!(zero, ManifestError::ZeroMaxHolds));
}

#[test]
fn requirements_below_arbiter_are_rejected() {
    let err = parse(
        "name = \"x\"\n[loop]\nparticipation = \"steering\"\n\n[requirements]\nr = \"a requirement\"",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        ManifestError::RequirementsRequireArbiter { .. }
    ));
}

#[test]
fn an_arbiter_without_requirements_is_rejected_in_both_shapes() {
    let absent =
        parse("name = \"x\"\n[loop]\nparticipation = \"arbiter\"\nhooks = [\"Stop\"]").unwrap_err();
    assert!(matches!(absent, ManifestError::ArbiterRequiresRequirements));

    let empty = parse(
        "name = \"x\"\n[loop]\nparticipation = \"arbiter\"\nhooks = [\"Stop\"]\n\n[requirements]",
    )
    .unwrap_err();
    assert!(matches!(empty, ManifestError::ArbiterRequiresRequirements));
}

#[test]
fn an_empty_requirement_description_is_rejected() {
    let err = parse(
        "name = \"x\"\n[loop]\nparticipation = \"arbiter\"\nhooks = [\"Stop\"]\n\n[requirements]\nr = \"  \"",
    )
    .unwrap_err();
    assert!(matches!(err, ManifestError::EmptyRequirement { .. }));
}
#[test]
fn a_subloop_below_steering_is_rejected() {
    let err = parse(
        "name = \"x\"\n[loop]\nparticipation = \"observer\"\n\n[subloop]\nstages = [\"triage\"]",
    )
    .unwrap_err();
    assert!(matches!(err, ManifestError::SubloopRequiresSteering { .. }));
}

#[test]
fn subloop_stage_lists_are_validated() {
    let head = "name = \"x\"\n[loop]\nparticipation = \"steering\"\n\n[subloop]\n";

    let empty = parse(&format!("{head}stages = []")).unwrap_err();
    assert!(matches!(empty, ManifestError::EmptyStages));

    let dupe = parse(&format!("{head}stages = [\"plan\", \"plan\"]")).unwrap_err();
    assert!(matches!(dupe, ManifestError::DuplicateStage { .. }));

    let blank = parse(&format!("{head}stages = [\"plan\", \" \"]")).unwrap_err();
    assert!(matches!(blank, ManifestError::EmptyStageName));
}

#[test]
fn roles_that_resolve_nowhere_are_rejected_and_tiers_must_be_non_empty() {
    let orphaned = parse(
        "name = \"x\"\n[loop]\nparticipation = \"steering\"\n\n[roles.triage]\ntier = \"cheap\"",
    )
    .unwrap_err();
    assert!(matches!(orphaned, ManifestError::RolesResolveNowhere));

    let blank_tier = parse(
        "name = \"x\"\n[loop]\nparticipation = \"steering\"\n\n[subloop]\nstages = [\"triage\"]\n\n[roles.triage]\ntier = \"\"",
    )
    .unwrap_err();
    assert!(matches!(blank_tier, ManifestError::EmptyRoleTier { .. }));
}

/// **The witness for #3496.** A `[wrapper]` that names a role intent on
/// its `before_turn` response is a second thing that can resolve one, so
/// `[roles]` beside it loads with no `[subloop]` to prop it up — while
/// `[roles]` with neither is still refused, because the rule is "something
/// must be able to spend this", not "declare any table you like".
///
/// Three shipped manifests were declaring a `[subloop]` they never used to
/// get past the old rule (`plugins/stella-plan`, `plugins/stella-goal`,
/// and this crate's own reference fixture in
/// `crates/stella-runtime/tests/wrapper_socket.rs`); all three drop it in
/// the same change.
#[test]
fn a_wrapper_can_name_a_role_intent_without_declaring_a_subloop() {
    let wrapper_only = parse(
        "name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"before_turn\"]\n\n\
         [wrapper]\nid = \"x-v1\"\n\n[[wrapper.stages]]\nname = \"plan\"\n\n\
         [roles.planner]\ntier = \"plan\"",
    )
    .expect("a wrapper naming a role intent needs no subloop to resolve it");
    assert!(wrapper_only.subloop.is_none());
    assert!(wrapper_only.roles.is_some());

    // A tier is still a tier: widening which tables satisfy the rule does
    // not widen what the entries themselves may say.
    let blank_tier = parse(
        "name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"before_turn\"]\n\n\
         [wrapper]\nid = \"x-v1\"\n\n[[wrapper.stages]]\nname = \"plan\"\n\n\
         [roles.planner]\ntier = \" \"",
    )
    .unwrap_err();
    assert!(matches!(blank_tier, ManifestError::EmptyRoleTier { .. }));
}

#[test]
fn an_empty_name_is_rejected() {
    let err = parse("name = \" \"").unwrap_err();
    assert!(matches!(err, ManifestError::EmptyName));
}

#[test]
fn permits_hook_is_declared_and_graded_even_on_a_hand_built_grant() {
    // Through the constructor: only the declared hooks pass.
    let m = parse("name = \"x\"\n[loop]\nparticipation = \"steering\"\nhooks = [\"PreToolUse\"]")
        .unwrap();
    assert!(m.loop_grant.permits_hook(HookEvent::PreToolUse));
    assert!(!m.loop_grant.permits_hook(HookEvent::PostToolUse));
    assert!(!m.loop_grant.permits_hook(HookEvent::Stop));

    // Hand-built below steering with hooks smuggled in: still filtered.
    let smuggled = LoopGrant {
        participation: Participation::Observer,
        hooks: vec![HookEvent::PreToolUse],
        points: vec![WrapperPoint::BeforeTurn],
        before_turn_stages: Vec::new(),
        calls: vec![HostCall::Recall],
        max_calls: None,
        max_child_turns: None,
        max_fanout_width: None,
        max_holds: None,
    };
    assert!(!smuggled.permits_hook(HookEvent::PreToolUse));
    assert!(!smuggled.permits_point(WrapperPoint::BeforeTurn));
    assert!(!smuggled.permits_call(HostCall::Recall));
    // The stage filter is a strengthening of the point filter, so it
    // inherits the grade check rather than reopening it: an empty stage
    // list is "every stage", and a grade below steering still reaches
    // nothing (#3543).
    assert!(!smuggled.permits_stage(WrapperPoint::BeforeTurn, &StageName::new("execute")));
}

/// **Witness for #3501 item 2.** A manifest declares the socket points it
/// implements, and a point it did not declare is never dispatched — the
/// filter [`LoopGrant::permits_hook`] already is for hooks. Before this,
/// `[loop]` could not express the answer at all, so a host learned that a
/// wrapper refuses `before_turn` by asking and being refused at run time.
#[test]
fn an_undeclared_point_is_never_dispatched() {
    let m = parse("name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"after_turn\"]")
        .expect("a declared point set must load");
    assert_eq!(m.loop_grant.points, vec![WrapperPoint::AfterTurn]);
    assert!(m.loop_grant.permits_point(WrapperPoint::AfterTurn));
    assert!(
        !m.loop_grant.permits_point(WrapperPoint::BeforeTurn),
        "before_turn was never declared, so it is never dispatched"
    );

    // Undeclared entirely: a plugin that answers nowhere.
    let silent = parse("name = \"x\"\n[loop]\nparticipation = \"steering\"").unwrap();
    assert!(silent.loop_grant.points.is_empty());
    for point in [WrapperPoint::BeforeTurn, WrapperPoint::AfterTurn] {
        assert!(!silent.loop_grant.permits_point(point));
    }
}

#[test]
fn point_declarations_are_graded_and_deduplicated_like_hooks() {
    let below =
        parse("name = \"x\"\n[loop]\nparticipation = \"observer\"\npoints = [\"before_turn\"]")
            .unwrap_err();
    assert!(matches!(below, ManifestError::PointsRequireSteering { .. }));

    let dupe = parse(
        "name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"after_turn\", \"after_turn\"]",
    )
    .unwrap_err();
    assert!(matches!(
        dupe,
        ManifestError::DuplicatePoint {
            point: WrapperPoint::AfterTurn
        }
    ));

    let unknown = parse("name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"judge\"]")
        .unwrap_err();
    assert!(
        matches!(unknown, ManifestError::Parse(_)),
        "`judge` is a host function, not a point a plugin can answer; got {unknown:?}"
    );
}

/// **Witness for #3540.** A manifest declares the host capabilities it may
/// ask for, and one it did not declare is refused — the filter
/// [`LoopGrant::permits_hook`] already is for hooks and
/// [`LoopGrant::permits_point`] is for points. Before this the `[loop]`
/// block could not express the answer at all, because there was nothing on
/// the wire to ask with.
#[test]
fn an_undeclared_host_call_is_never_performed() {
    let m = parse(
        "name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"before_turn\"]\ncalls = [\"recall\"]",
    )
    .expect("a declared call set must load");
    assert_eq!(m.loop_grant.calls, vec![HostCall::Recall]);
    assert!(m.loop_grant.permits_call(HostCall::Recall));
    assert!(
        !m.loop_grant.permits_call(HostCall::ChildTurn),
        "child_turn was never declared, so the host never performs it"
    );

    // Undeclared entirely: a plugin that asks for nothing.
    let silent =
        parse("name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"before_turn\"]")
            .unwrap();
    assert!(silent.loop_grant.calls.is_empty());
    for call in [HostCall::Recall, HostCall::ChildTurn, HostCall::RunTest] {
        assert!(!silent.loop_grant.permits_call(call));
    }
}

#[test]
fn call_declarations_are_graded_and_deduplicated_like_hooks() {
    let below = parse("name = \"x\"\n[loop]\nparticipation = \"observer\"\ncalls = [\"recall\"]")
        .unwrap_err();
    assert!(matches!(below, ManifestError::CallsRequireSteering { .. }));

    let dupe = parse(
        "name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"before_turn\"]\ncalls = [\"recall\", \"recall\"]",
    )
    .unwrap_err();
    assert!(matches!(
        dupe,
        ManifestError::DuplicateCall {
            call: HostCall::Recall
        }
    ));

    let unknown = parse(
        "name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"before_turn\"]\ncalls = [\"read_file\"]",
    )
    .unwrap_err();
    assert!(
        matches!(unknown, ManifestError::Parse(_)),
        "the capability set is closed, not an RPC surface; got {unknown:?}"
    );
}

/// The allowance is an ask with a shape: it needs calls to bound, and zero
/// contradicts the calls it is declared beside. And a call with no point to
/// make it from is the manifest that quietly does nothing.
#[test]
fn the_host_call_allowance_must_be_a_coherent_ask() {
    let orphan_calls =
        parse("name = \"x\"\n[loop]\nparticipation = \"steering\"\ncalls = [\"recall\"]")
            .unwrap_err();
    assert!(matches!(orphan_calls, ManifestError::CallsRequirePoints));

    let orphan_allowance =
        parse("name = \"x\"\n[loop]\nparticipation = \"steering\"\nmax_calls = 4").unwrap_err();
    assert!(matches!(
        orphan_allowance,
        ManifestError::MaxCallsRequiresCalls
    ));

    let zero = parse(
        "name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"before_turn\"]\ncalls = [\"recall\"]\nmax_calls = 0",
    )
    .unwrap_err();
    assert!(matches!(zero, ManifestError::ZeroMaxCalls));

    let asked = parse(
        "name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"before_turn\"]\ncalls = [\"recall\"]\nmax_calls = 4",
    )
    .expect("a coherent ask loads");
    assert_eq!(asked.loop_grant.max_calls, Some(4));
}

/// **Witness for #3599 B0, the manifest half.** A driver holds its
/// capabilities through a `[driver]` block that the `Participation` ladder
/// neither grants nor gates.
///
/// The required assertion is the first one: `participation = "none"` —
/// the grade a plugin that never runs inside a turn carries — used to
/// make every capability unreachable, and that is the defect the phase
/// exists to fix. The rest pin the asymmetry: no grade is
/// required, no `points` prerequisite applies (a driver call is made during
/// a driver session, not during a wrapper point), and the `[loop]` rules
/// that *do* transfer — deduplicated, a coherent allowance — still hold.
#[test]
fn a_driver_holds_capabilities_without_a_participation_grade() {
    use crate::driver::DriverCall;
    let driving = parse(
        "name = \"x\"\n[loop]\nparticipation = \"none\"\n\n[driver]\ncalls = [\"backlog_next\", \"deliver_open\"]",
    )
    .expect("a driver at grade `none` loads");
    let grant = driving.driver.expect("the [driver] block is parsed");
    assert!(grant.permits_call(DriverCall::BacklogNext));
    assert!(grant.permits_call(DriverCall::DeliverOpen));
    // Declared is exhaustive, in the driver's context as in the wrapper's.
    assert!(!grant.permits_call(DriverCall::DeliverMerge));
    // And the ladder is untouched: the same manifest still takes no say in
    // any turn.
    assert_eq!(driving.loop_grant.participation, Participation::None);

    // Absent is not empty. "Not a driver" and "a driver that asks for
    // nothing" must not share a representation.
    assert!(
        parse("name = \"x\"")
            .expect("a bare manifest loads")
            .driver
            .is_none()
    );
    assert_eq!(
        parse("name = \"x\"\n[driver]")
            .expect("an empty [driver] block loads")
            .driver,
        Some(DriverGrant::default())
    );

    // The capability set is closed, not an RPC surface, so `release` is not
    // in it (§6.4).
    assert!(matches!(
        parse("name = \"x\"\n[driver]\ncalls = [\"release\"]").unwrap_err(),
        ManifestError::Parse(_)
    ));

    // The `[loop] calls` rules that transfer.
    assert!(matches!(
        parse("name = \"x\"\n[driver]\ncalls = [\"sweep_audit\", \"sweep_audit\"]").unwrap_err(),
        ManifestError::DuplicateDriverCall {
            call: DriverCall::SweepAudit
        }
    ));
    assert!(matches!(
        parse("name = \"x\"\n[driver]\nmax_calls = 4").unwrap_err(),
        ManifestError::DriverMaxCallsRequiresCalls
    ));
    assert!(matches!(
        parse("name = \"x\"\n[driver]\ncalls = [\"sweep_audit\"]\nmax_calls = 0").unwrap_err(),
        ManifestError::ZeroDriverMaxCalls
    ));
}

/// A driver names its own process, and `[runtime]` could not have carried
/// it: the same argv under `[runtime]` is refused at `participation =
/// "none"`, the only grade a plugin that never stands inside a turn can
/// carry (#3783).
///
/// The second assertion is why the first is not enough: on its own it
/// would pass against a design that reused `[runtime]` and quietly
/// required a driver to overstate its in-turn standing to gain a process.
#[test]
fn a_driver_declares_its_own_process_and_runtime_could_not_have_carried_it() {
    const PROCESS: &str = "argv = [\"python3\", \"${plugin_dir}/main.py\"]\n\
                           timeout_secs = 600\nenv = [\"HOME\"]";

    let driving = parse(&format!(
        "name = \"x\"\n[loop]\nparticipation = \"none\"\n\n[driver]\n\
         calls = [\"backlog_next\"]\n\n[driver.process]\n{PROCESS}"
    ))
    .expect("a driver's process loads at grade `none`");
    let process = driving
        .driver
        .expect("the [driver] block is parsed")
        .process
        .expect("the [driver.process] block is parsed");
    assert_eq!(process.argv[0], "python3");
    assert_eq!(process.timeout_secs, 600);
    assert_eq!(process.env, vec!["HOME".to_string()]);

    // The same program under `[runtime]`, at the same grade, does not load.
    assert!(matches!(
        parse(&format!(
            "name = \"x\"\n[loop]\nparticipation = \"none\"\n\n[runtime]\n{PROCESS}"
        ))
        .unwrap_err(),
        ManifestError::RuntimeRequiresObserver {
            participation: Participation::None
        }
    ));

    // Absent is not empty here either: a grant with no process is a driver
    // Stella cannot start, which `plugins/stella-selfdriving` relies on.
    assert!(
        parse("name = \"x\"\n[driver]\ncalls = [\"backlog_next\"]")
            .expect("a processless driver loads")
            .driver
            .expect("the block is parsed")
            .process
            .is_none()
    );
}

/// `[runtime]`'s rules, reported against the block the author actually
/// wrote. A refusal that named `[runtime]` would send them to a table
/// their manifest does not contain.
#[test]
fn a_driver_process_defect_names_the_driver_block() {
    let err = parse(
        "name = \"x\"\n[driver]\ncalls = [\"backlog_next\"]\n\n\
         [driver.process]\nargv = []\ntimeout_secs = 5",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        ManifestError::EmptyProcessArgv {
            block: ProcessBlock::DriverProcess
        }
    ));
    assert!(err.to_string().starts_with("[driver.process]"), "{err}");

    let dead = parse(
        "name = \"x\"\n[driver]\n\n[driver.process]\n\
         argv = [\"node\"]\ntimeout_secs = 5\nenv = [\" PATH\"]",
    )
    .unwrap_err();
    assert!(matches!(
        dead,
        ManifestError::InvalidProcessEnvName {
            block: ProcessBlock::DriverProcess,
            ..
        }
    ));

    // And `[runtime]`'s own refusals still name `[runtime]`.
    let runtime = parse(
        "name = \"x\"\n[loop]\nparticipation = \"observer\"\n\n\
         [runtime]\nargv = []\ntimeout_secs = 5",
    )
    .unwrap_err();
    assert!(runtime.to_string().starts_with("[runtime]"), "{runtime}");
}
