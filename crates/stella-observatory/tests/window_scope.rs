// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! **The time-window control lives on the pages it filters.**
//!
//! The Window radiogroup (`24h` / `7d` / `30d` / `All`) drives `state.tf`,
//! and `state.tf` is consulted by two tabs' surfaces: Overview (KPI tiles,
//! the token timeline, the executions table, via `inTf`) and Activity (the
//! daily charts, via `inTfDay`). Every other tab renders all-time aggregates
//! or configuration state.
//!
//! The control nevertheless sat in the global nav on all twelve tabs, and on
//! ten of them clicking it changed nothing — no error, no re-render, no
//! acknowledgement beyond the highlight moving. That is indistinguishable
//! from a broken control, and it was reported as one ("the window time
//! filter is not working"). Hiding the nav control on the other ten tabs
//! left a control that appears and disappears as the reader moves around
//! the nav; putting one inside each page it filters leaves nothing to hide.
//!
//! Like the sibling asset tests, this reads the served page as text — the
//! Rust side never executes the script — and pins what the fix is made of,
//! each part of which is absent on the code that shipped the defect:
//!
//! 1. the nav carries no window control at all;
//! 2. exactly the two tabs whose renders consult `state.tf` carry one, each
//!    a labelled radiogroup offering all four windows;
//! 3. every instance is driven from the one `state.tf`, so a window chosen
//!    on Overview is the window Activity renders under.

/// The page exactly as the binary embeds and serves it.
const INDEX_HTML: &str = include_str!("../src/assets/index.html");

/// The tabs whose surfaces re-render under the window, in document order.
const FILTERED_TABS: [&str; 2] = ["overview", "activity"];

const PANEL_OPEN: &str = r#"<section data-tab=""#;

/// `(tab, markup)` for every tab panel, each slice running from its opening
/// tag to the next panel's or to the end of `<main>`.
fn panels() -> Vec<(&'static str, &'static str)> {
    let main = INDEX_HTML
        .split_once("</main>")
        .expect("the page wraps its tab panels in <main>")
        .0;
    let starts: Vec<usize> = main.match_indices(PANEL_OPEN).map(|(i, _)| i).collect();
    assert!(!starts.is_empty(), "the page declares no tab panels");
    starts
        .iter()
        .enumerate()
        .map(|(n, &start)| {
            let end = starts.get(n + 1).copied().unwrap_or(main.len());
            let body = &main[start..end];
            let tab = body[PANEL_OPEN.len()..]
                .split('"')
                .next()
                .expect("a panel's data-tab attribute is quoted");
            (tab, body)
        })
        .collect()
}

#[test]
fn the_nav_carries_no_window_control() {
    let nav = INDEX_HTML
        .split_once(r#"<nav class="bar""#)
        .expect("the page has a nav bar")
        .1
        .split_once("</nav>")
        .expect("the nav bar closes")
        .0;
    assert!(
        !nav.contains("data-tf"),
        "the Window control must not sit in the nav: there it stands above \
         ten tabs whose renders ignore state.tf, which reads as broken"
    );
}

#[test]
fn only_the_tabs_the_window_filters_carry_it() {
    let carrying: Vec<&str> = panels()
        .into_iter()
        .filter(|(_, body)| body.contains("data-tf"))
        .map(|(tab, _)| tab)
        .collect();
    assert_eq!(
        carrying, FILTERED_TABS,
        "a Window control belongs on exactly the tabs whose surfaces consult \
         state.tf (inTf / inTfDay); one anywhere else filters nothing, and a \
         missing one on these two leaves their aggregates unnarrowable"
    );
}

#[test]
fn each_control_is_a_labelled_radiogroup_over_all_four_windows() {
    for (tab, body) in panels() {
        if !FILTERED_TABS.contains(&tab) {
            continue;
        }
        assert!(
            body.contains(&format!(r#"aria-labelledby="tfLabel-{tab}""#)),
            "the {tab} Window radiogroup must name its own label: the ids are \
             per-page now, and two elements sharing one id point a reader's \
             group label at whichever came first"
        );
        for window in ["24h", "7d", "30d", "all"] {
            assert!(
                body.contains(&format!(r#"data-tf="{window}""#)),
                "the {tab} Window control is missing the {window} option"
            );
        }
    }
}

#[test]
fn one_selection_drives_every_instance() {
    assert!(
        INDEX_HTML.contains(r#"const TF_GROUPS = [...document.querySelectorAll(".tf")];"#),
        "the script must bind every .tf radiogroup on the page, not one by id; \
         a second instance nothing binds is a control that does nothing"
    );
    assert!(
        INDEX_HTML.contains("state.tf = tf;"),
        "setTf must take the window itself rather than the button clicked — \
         that is what lets it repaint the checked state of the other page's \
         instance from the same value"
    );
}
