// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `sweep` verbs, over a loop state directory a test wrote itself.
//!
//! Split from [`super`] so neither file nears the size ceiling. The two ask
//! different things. That module binds a driver to its process. This one is
//! about what the host draws when one asks for a supply.
//!
//! No test here runs `gh`. The tracker is [`super::FixtureTracker`], which
//! keeps every draft it is handed. Git is real, and the tempdir is not a
//! repository, so "is this fix still on the base" always answers no. That is
//! the answer a lost fix gives.

use std::path::Path;

use stella_autonomy::Citation;
use stella_autonomy::regress::ClosureReceipt;
use stella_autonomy::supply::SupplyPolicy;
use stella_plugin::{SweepSkip, SweepSkipReason, SweptSupply};

use super::*;
use crate::self_driving_cmd::state::LoopState;

/// A workspace whose convention is bound, so a draft can be filed at all.
///
/// A proposed convention refuses every filing. That would hide what these
/// tests ask about.
fn bind_convention(root: &Path) {
    let dir = root.join(".stella").join("issues");
    std::fs::create_dir_all(&dir).expect("issues dir");
    std::fs::write(
        dir.join("convention.json"),
        r#"{"axes":[{"name":"kind","members":["bug"],
             "requirement":"exactly_one","source":"declared"}],
             "reserved":[],"acceptance":"bound"}"#,
    )
    .expect("convention");
}

/// A host over one tempdir, with the supplies `policy` opens.
///
/// Both fields of [`LoopState`] are `pub`. So the state directory is built
/// here, not through `LoopState::open`, which reads the real stella home.
fn host(root: &Path, policy: SupplyPolicy) -> HostDriverCapabilities {
    bind_convention(root);
    let dir = root.join("state");
    std::fs::create_dir_all(&dir).expect("state dir");

    let roster = roster(vec![installed(GRANTS_BASH, "/opt/pkgs/selfdriving")]);
    HostDriverCapabilities::new(
        "stella-selfdriving",
        PluginGates::from_roster(&roster),
        Box::new(FixtureTracker { open: Vec::new() }),
        LoopConfig {
            supply: policy,
            ..LoopConfig::default()
        },
        root.to_path_buf(),
        WorkSlot::new(Box::new(FixtureWorker::answering(changed()))),
        DeliverDesk::new(Box::new(FixtureForge::default())),
    )
    .sweeping(Some(LoopState {
        dir,
        repo_root: root.to_path_buf(),
    }))
}

/// One closure that cited a merged pull request and saw it on the base.
///
/// The only receipt a regress sweep can act on. Anything else is a skip,
/// because *gone* and *never seen* would look alike.
fn receipt(key: &str) -> ClosureReceipt {
    ClosureReceipt {
        key: key.to_owned(),
        closed_at: "2026-09-01T00:00:00Z".to_owned(),
        by: Some(Citation::PullRequest {
            key: "4101".to_owned(),
        }),
        present_at_close: Some(true),
    }
}

/// **The witness.** A granted `sweep_regress` over an open supply answers
/// with the report's counts and the key it filed.
///
/// Before this change the same ask came back `unsupported`. A host driving
/// the loop from outside the binary could not sweep at all.
#[tokio::test]
async fn a_regress_sweep_answers_with_what_it_re_checked_and_filed() {
    let workspace = tempfile::tempdir().expect("a temporary workspace");
    let host = host(
        workspace.path(),
        SupplyPolicy {
            regress: true,
            ..SupplyPolicy::default()
        },
    );
    // One receipt that can regress, and one that cited nothing.
    let state = LoopState {
        dir: workspace.path().join("state"),
        repo_root: workspace.path().to_path_buf(),
    };
    state.append_receipt(&receipt("4100")).expect("a receipt");
    state
        .append_receipt(&ClosureReceipt {
            by: None,
            ..receipt("4200")
        })
        .expect("a second receipt");

    let report = host
        .perform(DriverCall::SweepRegress, None)
        .await
        .expect("an opened supply is swept")
        .sweep
        .expect("a served sweep carries its report");

    assert_eq!(report.supply, SweptSupply::Regress);
    assert_eq!(report.offered, 1);
    assert_eq!(report.fresh, 1);
    assert_eq!(report.filed.len(), 1, "{report:?}");

    let counts = report.receipts.expect("a regress sweep reads receipts");
    assert_eq!(counts.total, 2);
    assert_eq!(counts.checked, 1);
    assert_eq!(
        counts.skipped,
        vec![SweepSkip {
            reason: SweepSkipReason::NoChangeCited,
            count: 1,
        }],
        "the receipt citing nothing is reported as one, with its reason"
    );
}

/// **The witness.** A supply the workspace never opened is refused, by the
/// name of the switch that opens it.
///
/// Granting the verb does not open the supply. Only an operator's
/// `stella.toml` does.
#[tokio::test]
async fn a_shut_supply_is_refused_by_the_name_of_its_switch() {
    let workspace = tempfile::tempdir().expect("a temporary workspace");
    let refused = host(workspace.path(), SupplyPolicy::default())
        .perform(DriverCall::SweepRegress, None)
        .await
        .expect_err("a shut supply is not swept");

    assert_eq!(refused.refusal, HostCallRefusal::Unavailable);
    assert!(refused.detail.contains("regress"), "{refused}");
    assert!(refused.detail.contains("self_driving.supply"), "{refused}");
}

/// **The witness.** `sweep_meta` folds the loop's own ledger. It answers with
/// how many cycles it read.
///
/// An empty ledger offers nothing, and that is a real answer. The counts say
/// the supply is dry, not that the verb is missing.
#[tokio::test]
async fn a_meta_sweep_answers_over_the_ledger_it_read() {
    let workspace = tempfile::tempdir().expect("a temporary workspace");
    let report = host(
        workspace.path(),
        SupplyPolicy {
            meta: true,
            ..SupplyPolicy::default()
        },
    )
    .perform(DriverCall::SweepMeta, None)
    .await
    .expect("an opened supply is swept")
    .sweep
    .expect("a served sweep carries its report");

    assert_eq!(report.supply, SweptSupply::Meta);
    assert_eq!(report.offered, 0, "{report:?}");
    assert!(
        report.receipts.is_none(),
        "the meta supply folds the cycle ledger and reads no receipts"
    );
    assert!(report.filed.is_empty(), "{report:?}");
}

/// A sweep verb reads no arguments. An ask carrying a table is refused, never
/// read past.
#[tokio::test]
async fn a_sweep_ask_carrying_arguments_is_refused() {
    let workspace = tempfile::tempdir().expect("a temporary workspace");
    let refused = host(
        workspace.path(),
        SupplyPolicy {
            meta: true,
            ..SupplyPolicy::default()
        },
    )
    .perform(
        DriverCall::SweepMeta,
        Some(DriverArgs {
            work_start: Some(UnitArgs {
                issue: "41".to_string(),
            }),
            ..DriverArgs::default()
        }),
    )
    .await
    .expect_err("a table this verb does not read is a refusal");

    assert!(refused.detail.contains("reads no arguments"), "{refused}");
}

/// A host with no loop state says so. It does not report an empty ledger as a
/// swept one.
#[tokio::test]
async fn a_host_with_no_state_directory_refuses_rather_than_reporting_nothing() {
    let workspace = tempfile::tempdir().expect("a temporary workspace");
    let roster = roster(vec![installed(GRANTS_BASH, "/opt/pkgs/selfdriving")]);
    let refused = HostDriverCapabilities::new(
        "stella-selfdriving",
        PluginGates::from_roster(&roster),
        Box::new(FixtureTracker { open: Vec::new() }),
        LoopConfig {
            supply: SupplyPolicy {
                meta: true,
                ..SupplyPolicy::default()
            },
            ..LoopConfig::default()
        },
        workspace.path().to_path_buf(),
        WorkSlot::new(Box::new(FixtureWorker::answering(changed()))),
        DeliverDesk::new(Box::new(FixtureForge::default())),
    )
    .perform(DriverCall::SweepMeta, None)
    .await
    .expect_err("nothing was bound to sweep");

    assert_eq!(refused.refusal, HostCallRefusal::Unavailable);
    assert!(refused.detail.contains("state directory"), "{refused}");
}
