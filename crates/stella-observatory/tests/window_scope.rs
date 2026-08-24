// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! **The time-window control appears only where it filters something.**
//!
//! The nav's Window radiogroup (`24h` / `7d` / `30d` / `All`) drives
//! `state.tf`, and `state.tf` is consulted by exactly two tabs' surfaces:
//! Overview (KPI tiles, the token timeline, the executions table, via
//! `inTf`) and Activity (the daily charts, via `inTfDay`). Every other tab
//! renders all-time aggregates or configuration state.
//!
//! The control nevertheless sat in the global nav on all twelve tabs, and on
//! ten of them clicking it changed nothing — no error, no re-render, no
//! acknowledgement beyond the highlight moving. That is indistinguishable
//! from a broken control, and it was reported as one ("the window time
//! filter is not working"). The fix scopes the control's visibility to the
//! tabs it governs: `switchTab` hides `#tfWrap` unless the destination tab
//! is in `TF_TABS`.
//!
//! Like the sibling asset tests, this reads the served page as text — the
//! Rust side never executes the script — and pins the three pieces the fix
//! is made of, each of which is absent on the code that shipped the defect:
//!
//! 1. the wrapper is addressable (`id="tfWrap"`), so the script can hide it;
//! 2. hiding works at all: `.tf-wrap` is `display:flex`, which overrides the
//!    `hidden` attribute's default styling unless a `[hidden]` rule wins —
//!    delete that rule and the attribute becomes a silent no-op;
//! 3. `switchTab` toggles the wrapper from a `TF_TABS` set whose members are
//!    real tabs, and that set names only tabs whose panels the window
//!    actually re-renders.

/// The page exactly as the binary embeds and serves it.
const INDEX_HTML: &str = include_str!("../src/assets/index.html");

#[test]
fn window_control_is_addressable_and_hideable() {
    assert!(
        INDEX_HTML.contains(r#"<div class="tf-wrap" id="tfWrap">"#),
        "the Window control's wrapper must carry id=\"tfWrap\" so switchTab can hide it"
    );
    assert!(
        INDEX_HTML.contains(".tf-wrap[hidden]{display:none}"),
        ".tf-wrap is display:flex, which beats the hidden attribute's UA styling; \
         without this rule, hiding the control is a silent no-op"
    );
}

#[test]
fn switch_tab_scopes_the_window_to_the_tabs_it_filters() {
    assert!(
        INDEX_HTML.contains(r#"const TF_TABS = new Set(["overview", "activity"]);"#),
        "TF_TABS must name exactly the tabs whose surfaces consult state.tf \
         (inTf / inTfDay); widening it without wiring the window into the new \
         tab's renders recreates the do-nothing control this test exists for"
    );
    assert!(
        INDEX_HTML.contains(r#"$("tfWrap").hidden = !TF_TABS.has(tab);"#),
        "switchTab must hide the Window control on tabs it does not govern"
    );
    // Both governed tabs are real: each has a section panel to filter.
    for tab in ["overview", "activity"] {
        assert!(
            INDEX_HTML.contains(&format!(r#"<section data-tab="{tab}""#)),
            "TF_TABS names {tab:?}, but no <section data-tab={tab:?}> exists"
        );
    }
}
