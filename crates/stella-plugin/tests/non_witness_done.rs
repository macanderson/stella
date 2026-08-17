//! The D-1 falsifier, executable: a definition of done that is not Vera's,
//! expressed entirely as declarative data (`doc:pipeline-as-plugins` §6.1).
//!
//! D-1 settles that `judge` stays in-process, synchronous and I/O-free, and
//! that plugins participate by supplying evidence out-of-process and their
//! **verdict rule as data**. That claim is only worth as much as the grammar's
//! reach, so this file is the attempt to break it: `fixtures/perf-budget.toml`
//! is a performance-budget arbiter — done means a named benchmark did not
//! regress past a recorded budget — and every assertion below is about whether
//! the manifest can carry it without a line of plugin code deciding anything.
//!
//! Two things did not fit and were widened in the same change:
//! `flip = "not-applicable"` (this oracle observes no red-to-green transition)
//! and `[[oracle.checks]]` (the budget had nowhere to live but inside the
//! oracle binary). What is deliberately still absent is a fractional
//! measurement — see the fixture's header and #3488.

use std::collections::BTreeMap;

use stella_plugin::{
    CompareOp, FlipPolicy, HookEvent, ManifestError, Participation, PluginManifest, TamperPolicy,
    consent_text,
};

const PERF_BUDGET: &str = include_str!("fixtures/perf-budget.toml");

fn perf_budget() -> PluginManifest {
    PluginManifest::from_toml_str(PERF_BUDGET).expect("the falsifier fixture must load")
}

/// Reported measurements for a run that is inside both budgets.
fn within_budget() -> BTreeMap<String, u64> {
    BTreeMap::from([
        ("agent-loop-step-p50-pct-of-baseline".to_string(), 101),
        ("worst-bench-pct-of-baseline".to_string(), 112),
    ])
}

// --- The definition of done loads, and it is not a witness test. ---------

#[test]
fn a_performance_budget_is_expressible_as_a_manifest() {
    let manifest = perf_budget();

    // It is an arbiter: it binds the Stop gate, so it can hold a turn open.
    assert_eq!(manifest.loop_grant.participation, Participation::Arbiter);
    assert!(manifest.loop_grant.permits_hook(HookEvent::Stop));
    assert_eq!(manifest.loop_grant.max_holds, Some(2));

    // Its definition of done is enumerable, and it is about speed, not tests.
    let requirements = manifest.requirements.as_ref().expect("[requirements]");
    assert_eq!(requirements.len(), 2);
    assert!(requirements.contains_key("hot-path-within-budget"));
    assert!(requirements.contains_key("no-new-slow-path"));

    let oracle = manifest.oracle.as_ref().expect("[oracle]");
    assert_eq!(
        oracle.flip,
        FlipPolicy::NotApplicable,
        "a benchmark passes before and after; there is no flip to observe"
    );
    // The half of the witness contract that carried over unchanged: the
    // recorded baseline is the "before" of every comparison, so tampering
    // with it wins the budget without touching the code.
    assert_eq!(oracle.tamper, TamperPolicy::ArtifactIdentity);

    // And every requirement is decided by a rule the HOST evaluates.
    for name in requirements.keys() {
        assert!(
            oracle.decides(name),
            "{name} must be decided by a declared check, not by the oracle's opinion"
        );
    }
}

#[test]
fn the_verdict_rule_is_data_the_host_evaluates() {
    let oracle = perf_budget().oracle.expect("[oracle]");

    assert!(
        oracle.unmet(&within_budget()).unwrap().is_empty(),
        "a run inside both budgets meets the definition of done"
    );

    let mut regressed = within_budget();
    regressed.insert("agent-loop-step-p50-pct-of-baseline".to_string(), 118);
    let unmet = oracle.unmet(&regressed).unwrap();
    assert_eq!(unmet.len(), 1, "exactly the hot-path budget is blown");
    assert_eq!(unmet[0].requirement, "hot-path-within-budget");
    assert_eq!(unmet[0].reported, 118);
    assert_eq!(unmet[0].rule.op, CompareOp::LessOrEqual);
    assert_eq!(unmet[0].rule.value, 105);
    assert_eq!(
        unmet[0].to_string(),
        "hot-path-within-budget: agent-loop-step-p50-pct-of-baseline was 118, budget <= 105",
        "a hold must be attributable to a named requirement and a stated budget"
    );
}

/// The number the oracle failed to report is the dangerous case: read as
/// zero it would satisfy every `<=` budget in the file.
#[test]
fn a_measurement_the_oracle_did_not_report_is_never_a_pass() {
    let oracle = perf_budget().oracle.expect("[oracle]");
    let mut partial = within_budget();
    partial.remove("worst-bench-pct-of-baseline");

    let err = oracle.unmet(&partial).unwrap_err();
    assert!(
        matches!(
            err,
            ManifestError::MeasurementNotReported { ref measurement, .. }
                if measurement == "worst-bench-pct-of-baseline"
        ),
        "got {err:?}"
    );
}

// --- The grammar stayed closed, and stayed honest. -----------------------

/// `not-applicable` must not become the way to hold a turn open on vibes: it
/// removes the flip, so it must add the obligation that every requirement is
/// decided by a check.
#[test]
fn dropping_the_flip_does_not_drop_the_obligation() {
    let weakened = PERF_BUDGET.replace(
        "[[oracle.checks]]\nrequirement = \"no-new-slow-path\"\ncheck = \"worst-bench-pct-of-baseline <= 120\"\n",
        "",
    );
    assert_ne!(weakened, PERF_BUDGET, "the splice must have matched");

    let err = PluginManifest::from_toml_str(&weakened)
        .expect_err("a requirement with neither a flip nor a check decides nothing");
    assert!(
        matches!(
            err,
            ManifestError::UndecidableRequirement { ref requirement }
                if requirement == "no-new-slow-path"
        ),
        "got {err:?}"
    );
}

/// Vera's definition of done is untouched by the widening: the witness
/// arbiter still loads, still declares `flip = "required"`, and still needs
/// no checks at all.
#[test]
fn the_witness_definition_of_done_is_unchanged() {
    let vera = PluginManifest::from_toml_str(include_str!("fixtures/arbiter.toml")).unwrap();
    let oracle = vera.oracle.expect("[oracle]");
    assert_eq!(oracle.flip, FlipPolicy::Required);
    assert!(oracle.checks.is_empty());
    assert!(oracle.measurements.is_empty());
}

/// Invariant 4: both new blocks cross a crate boundary byte-for-byte, in the
/// manifest's own format and in the JSON an install prompt will render from.
#[test]
fn the_falsifier_fixture_round_trips_through_both_wire_shapes() {
    let parsed = perf_budget();

    let toml_text = toml::to_string(&parsed).unwrap();
    let from_toml = PluginManifest::from_toml_str(&toml_text).unwrap();
    assert_eq!(parsed, from_toml);

    let json = serde_json::to_string(&parsed).unwrap();
    let from_json: PluginManifest = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, from_json);
}

/// The budget is only meaningfully "declared" if a human sees it before
/// installing: a plugin that could change what counts as done without a
/// visible manifest change would have kept the verdict to itself after all.
#[test]
fn the_budget_is_visible_at_install() {
    let text = consent_text(&perf_budget());
    assert!(
        text.contains("agent-loop-step-p50-pct-of-baseline <= 105"),
        "the consent text must state the budget verbatim:\n{text}"
    );
    assert!(
        text.contains("hot-path-within-budget"),
        "and name the requirement it decides:\n{text}"
    );
}
