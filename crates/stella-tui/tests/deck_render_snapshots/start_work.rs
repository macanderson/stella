// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The start-work overlay's golden frames — SPEC 8.2, rendering
//! `10-start-work` (#5044).
//!
//! A submodule of `deck_render_snapshots` rather than more lines in it: the
//! parent sits at the 1500-line ceiling, so a new golden goes here.
//!
//! Every helper comes from the parent, so both frames are blessed by one
//! command: `BLESS=1 cargo test -p stella-tui --test deck_render_snapshots`.

use stella_tui::start_work::{
    DraftContract, DraftEstimate, DraftRule, DraftSources, DraftTask, StartWorkDraft,
};

use super::*;
use stella_tui::deck_ui::IssuesMode;

/// The scripted issue the overlay is drafted over.
///
/// Written to exercise every branch of the card at once: a read-only task with
/// no contract, two diff-producing tasks whose mechanisms differ, a sources
/// line with both a graph clause and a memory RULE, and an estimate that
/// exists. Anything the fixture omits is a shape no golden covers.
fn fixture_draft() -> StartWorkDraft {
    StartWorkDraft {
        issue_key: "#151".into(),
        issue_title: "dedup digest persists across CI runs".into(),
        sources: DraftSources {
            coupled_files: vec![
                "crates/stella-store/src/seen.rs".into(),
                "crates/stella-cli/src/ci.rs".into(),
            ],
            rules: vec![DraftRule {
                id: "dedup-keys".into(),
                text: "dedup keys must be stable across runs".into(),
            }],
        },
        tasks: vec![
            DraftTask {
                subject: "read the seen-set write path".into(),
                contract: None,
            },
            DraftTask {
                subject: "persist the digest set to .stella/seen".into(),
                contract: Some(DraftContract {
                    done_means: "crates/stella-store/src/seen.rs is changed on the branch".into(),
                    mechanism: "graph".into(),
                    deterministic: true,
                }),
            },
            DraftTask {
                subject: "restore the digest set on start".into(),
                contract: Some(DraftContract {
                    done_means: "a test over this fails before the change and passes after".into(),
                    mechanism: "unit".into(),
                    deterministic: true,
                }),
            },
        ],
        gates: 5,
        estimate: Some(DraftEstimate {
            usd: 0.40,
            tokens: 60_000,
            minutes: 8,
        }),
    }
}

/// The ISSUES tab with the overlay open over the scripted backlog.
fn start_work_ui(editing: bool) -> DeckUi {
    let mut ui = ui_for(DeckTab::Issues);
    ui.issues.start_work.open("#151", 1);
    ui.issues.start_work.draft = Some(fixture_draft());
    ui.issues.start_work.editing = editing;
    if editing {
        // Cursor on the first task with the second taken out, so one frame
        // pins both marks the editor draws.
        ui.issues.start_work.sel = 1;
        ui.issues.start_work.toggle();
        ui.issues.start_work.sel = 0;
    }
    ui.issues.mode = IssuesMode::StartWork;
    ui
}

/// SPEC 8.2's card, whole, over a scripted issue.
///
/// This is the frame the issue calls "the single best demo moment", so the
/// golden is what holds it to the spec: the header and its subtitle, a sources
/// line naming the issue, the coupled files and the RULE with its text, a
/// read-only task marked `read only · no contract`, two contract previews with
/// their mechanism and `det` tag, the `◇ verify · 5 gates · blocks merge` row,
/// the estimate line, the action row and the footer.
///
/// It also pins an **absence**: SPEC §1 struck `det est %` from this line, and
/// the SVG rendering still carries it. A count-based test cannot see a
/// percentage creeping back onto a card; a golden can.
#[test]
fn deck_render_snapshots_pin_the_start_work_draft() {
    let model = fixture_model();
    let mut ui = start_work_ui(false);
    let frame = render_frame(&model, &mut ui, W, H);
    assert!(
        !frame.contains("det est"),
        "SPEC §1 forbids a det ratio on the estimate line:\n{frame}"
    );
    assert_golden(
        "overlay_start_work",
        "the start-work draft (SPEC 8.2) over a scripted issue",
        W,
        H,
        &frame,
    );
}

/// The same card with `e` held open: the edit cursor on one task and another
/// struck out.
///
/// Its own frame because the editor is the only way an approval can carry
/// fewer tasks than the draft, and "taken out" is a *sentence* on a row — the
/// failure mode a golden exists to catch is it quietly becoming a blank.
#[test]
fn deck_render_snapshots_pin_the_start_work_editor() {
    let model = fixture_model();
    let mut ui = start_work_ui(true);
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "overlay_start_work_editing",
        "the start-work draft with the task list open for editing",
        W,
        H,
        &frame,
    );
}
