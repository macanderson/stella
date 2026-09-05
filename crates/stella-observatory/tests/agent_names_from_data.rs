// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! **The page reads the names it draws. It holds no list of them.**
//!
//! Two surfaces here name the job an agent did. Each takes the word from the
//! data.
//!
//! The settings pane draws one row per key in
//! `agent_engine_config.agents`. Before, a list of six words filtered those
//! keys. A seventh name drew no row. So a settings file that set something
//! read as a settings file that set nothing.
//!
//! That list was also a producer of the four-language role-name contract. A
//! role word spelled in JavaScript is invisible to every Rust and Python test
//! in this tree.
//!
//! The sub-agent rows are the other half. A plugin picks the seat its child
//! runs at. The word rides in on the `sub_agent` bracket. No list built into
//! this page could hold it.
//!
//! Like the sibling asset tests, this reads the served page as text. The Rust
//! side never runs the script. Each check below is false on the old page.

/// The page exactly as the binary embeds and serves it.
const INDEX_HTML: &str = include_str!("../src/assets/index.html");

/// The five words the old list carried besides `default`.
///
/// `default` is left out. It is a live agent name, and the page quotes it for
/// its own reasons — `agents.default.prompt` among them.
const RETIRED_ROLE_WORDS: [&str; 5] = ["worker", "verifier", "triage", "research", "plan"];

/// The settings pane draws a row per key the merged settings carry.
///
/// Fails on the old page. It filtered through a list, so this read of the
/// keys did not exist.
#[test]
fn the_settings_pane_derives_its_agent_rows_from_the_settings() {
    assert!(
        INDEX_HTML.contains("const names = Object.keys(agents)"),
        "the agent rows must come from the settings' own keys"
    );
}

/// No list of role words survives anywhere in the page.
///
/// The old list was made of quoted role words, so this check fails on it. It
/// is also what stops a list growing back somewhere else in the page.
#[test]
fn the_page_quotes_no_role_word_at_all() {
    for word in RETIRED_ROLE_WORDS {
        for quoted in [format!("\"{word}\""), format!("'{word}'")] {
            assert!(
                !INDEX_HTML.contains(&quoted),
                "the page carries the literal {quoted}. Nothing here can hold \
                 a role word in this page to the Rust one. Read the name from \
                 the data instead."
            );
        }
    }
}

/// Both sub-agent surfaces draw the seat the bracket recorded.
///
/// Fails on the old page. `execution_subagents` projected no `seat` at all,
/// so neither surface had a field to read.
#[test]
fn both_sub_agent_surfaces_draw_the_seat_from_the_bracket() {
    assert!(
        INDEX_HTML.contains(r#"a.seat ? ` <span class="badge dim">${esc(a.seat)}</span>` : """#),
        "the sub-agent row must badge the seat the bracket recorded"
    );
    assert!(
        INDEX_HTML.contains("a.seat ? `<span>seat ${esc(a.seat)}</span>"),
        "the focused sub-agent header must name the seat the bracket recorded"
    );
}
