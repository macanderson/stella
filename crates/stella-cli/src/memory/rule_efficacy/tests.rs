// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The two pure halves: which records the loop may retract, and what a
//! demotion reads as. The end-to-end arc is
//! `memory::learning::rule_lifecycle`.

use stella_learn::skills::appraisal::{DemotionDecision, DemotionReason, KeepSkillReason};
use stella_records::records::registry;
use stella_records::records::{Registry, Trust};

use super::{lineages, origins, retraction_reason};

/// One published file holding every record a sweep has to sort out.
const RECORDS: &str = r#"schema = "context-record/v0.1"
set_id = "acme"

[[record]]
lineage_id = "ctx.acme.mined"
kind = "rule"
statement = "Regenerate gen/ rather than editing it by hand."
status = "active"
origin = "inferred"

[[record]]
lineage_id = "ctx.acme.hand-written"
kind = "rule"
statement = "Retry the billing webhook three times."
status = "active"
origin = "user"

[[record]]
lineage_id = "ctx.acme.already-retracted"
kind = "rule"
statement = "Prefer the pnpm lockfile."
status = "retracted"
origin = "inferred"
"#;

fn file(path: &str, contributed_by: Option<String>) -> stella_learn::rules::RuleFile {
    stella_learn::rules::RuleFile {
        path: path.to_string(),
        contents: RECORDS.to_string(),
        contributed_by,
    }
}

fn project_registry() -> Registry {
    registry::load(
        &[],
        &[file(".stella/rules/acme.toml", None)],
        &stella_records::records::Facts::default(),
    )
}

#[test]
fn only_a_mined_active_project_record_is_the_loops_to_retract() {
    let registry = project_registry();
    let origins = origins(&registry);

    assert_eq!(
        origins.get("mined"),
        Some(&stella_learn::skills::SkillOrigin::AutoCreated),
        "the loop wrote this one, so the loop may take it back"
    );
    assert!(
        !origins.contains_key("hand-written"),
        "a person wrote it; an absent origin is what keeps it"
    );
    assert!(
        !origins.contains_key("already-retracted"),
        "retracting it again would write one event per sweep, forever"
    );
}

#[test]
fn a_user_tier_record_is_never_the_loops_to_retract() {
    let registry = registry::load(
        &[file("~/.stella/rules/acme.toml", None)],
        &[],
        &stella_records::records::Facts::default(),
    );
    assert!(
        registry
            .entries
            .iter()
            .any(|entry| entry.record.trust == Trust::User),
        "the fixture must load as the user tier"
    );
    assert!(
        origins(&registry).is_empty(),
        "this workspace has no standing to rewrite the user's own rules"
    );
}

#[test]
fn a_plugins_record_is_never_the_loops_to_retract() {
    let registry = registry::load(
        &[],
        &[file(
            ".stella/plugins/vera/rules/acme.toml",
            Some("vera".to_string()),
        )],
        &stella_records::records::Facts::default(),
    );
    assert!(
        origins(&registry).is_empty(),
        "`stella plugin remove` is that door, not this sweep"
    );
}

#[test]
fn every_handle_joins_back_to_its_lineage() {
    let lineages = lineages(&project_registry());
    assert_eq!(
        lineages.get("mined").map(String::as_str),
        Some("ctx.acme.mined"),
        "the trial ledger names a handle and the retraction door takes a lineage"
    );
}

#[test]
fn a_retraction_reason_names_the_measurement_and_the_way_back() {
    let harmful = retraction_reason(&DemotionDecision::Demote {
        reason: DemotionReason::Harmful { lift: -0.25 },
    })
    .expect("a demotion earns a reason");
    assert!(
        harmful.contains("lift") && harmful.contains("active"),
        "it must say what measured it and how to put it back: {harmful}"
    );

    let inert = retraction_reason(&DemotionDecision::Demote {
        reason: DemotionReason::Inert { trials: 40 },
    })
    .expect("a demotion earns a reason");
    assert!(inert.contains("40"), "the window is the evidence: {inert}");

    assert_eq!(
        retraction_reason(&DemotionDecision::Keep {
            reason: KeepSkillReason::HandAuthored,
        }),
        None,
        "a kept record has nothing to explain"
    );
}
