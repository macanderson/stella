// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The AGENTS page's golden frames, and the one fixture helper only they use.
//!
//! Split out of `deck_render_snapshots.rs` when that file crossed the
//! 1500-line ceiling (#5143's witness was the line that did it). A subject
//! rather than an arbitrary cut: these are the only tests that render the
//! full-frame page, and `fixture_lead` has no other caller.

use super::*;

/// Golden frame for the full-frame AGENTS page (`←` twice on SESSION): its
/// header row, the working lanes with status, clock, model and spend, the
/// resumable sessions under COMPLETED, and the page's own new-task prompt.
///
/// The header row is the reason this golden reaches to column 160: the
/// `stella*` wordmark holds the right edge here as it does on every deck
/// screen (SPEC 3.3), and a style-stripped golden is what pins the *column*
/// it holds — the half of #5051 no `contains` assertion can state.
#[test]
fn deck_render_snapshots_pin_the_agents_page() {
    let mut model = fixture_model();
    let mut meta = stella_tui::AgentMeta::new("req:1", "fix the parser panic", 0)
        .with_role("subagent")
        .with_purpose("Fix the parser panic on empty input.");
    meta.model = Some("glm-5.2".into());
    model.apply_inbound(&Inbound::Register(meta));
    let mut ui = ui_for(DeckTab::Session);
    ui.agents_page.open = true;
    ui.sessions = vec![stella_tui::envelope::SessionInfo {
        id: "s-1".into(),
        title: "stella: wire the dedup digest".into(),
        summary: "Wired a dedup digest into the finding store.".into(),
        description: None,
        workspace: "/w/stella".into(),
        phase: stella_tui::envelope::SessionPhase::Complete,
        started_ms: 0,
        updated_ms: 0,
        mine: false,
        resumable: true,
        turns: 14,
        spend_micros: 450_000,
        model: Some("glm-5.2".into()),
    }];
    let frame = render_frame(&model, &mut ui, W, H);
    assert!(
        frame.contains("describe a task for a new session"),
        "the page's own composer placeholder is its point:\n{frame}"
    );
    assert_golden(
        "page_agents",
        "the full-frame AGENTS page: wordmark, counts, working lanes, resumable sessions, and the new-task prompt",
        W,
        H,
        &frame,
    );
}

/// The AGENTS page mid-conversation: a task half-typed into its composer and
/// the answer to the command before it standing in the page's own reply pane
/// (#4626).
///
/// A second golden rather than a change to the one above, because the two pin
/// different claims and folding them together would lose one. `page_agents` is
/// the page at rest — blank composer, nothing asked — and it is *unchanged* by
/// #4626, which is itself worth pinning: the caret and the reply pane must cost
/// the resting page no rows. This one is the page with both features engaged,
/// so the `REPLY` heading, the quoted transcript row and the typed composer are
/// all in a frame a reviewer can read.
#[test]
fn deck_render_snapshots_pin_the_agents_page_reply_pane() {
    let mut model = fixture_model();
    model.apply_inbound(&Inbound::ShellEvent {
        agent: fixture_lead(&model),
        event: AgentEvent::Text {
            text: "worker · zai/glm-5.2 · effort high".into(),
        },
    });
    let mut ui = ui_for(DeckTab::Session);
    ui.agents_page.open = true;
    // The mark the page sets when it sends a command: everything after it is
    // the answer. One short of the end, so exactly the reply above is quoted.
    ui.agents_page.reply_from = Some(
        model.agents[ui.focused]
            .model
            .transcript
            .len()
            .saturating_sub(1),
    );
    ui.agents_page.notice = Some("/model sent".to_string());
    for c in "rewrite the parser".chars() {
        ui.agents_page.composer.insert_char(c);
    }
    let frame = render_frame(&model, &mut ui, W, H);
    assert!(
        frame.contains("worker · zai/glm-5.2"),
        "the reply pane must quote the session's own answer:\n{frame}"
    );
    assert!(
        frame.contains("rewrite the parser"),
        "…above the composer that is still being typed into:\n{frame}"
    );
    assert_golden(
        "page_agents_reply",
        "the AGENTS page with a command's reply quoted in its own pane and a task half-typed",
        W,
        H,
        &frame,
    );
}

/// The lead lane's id in the demo fixture — the session a page-submitted
/// command is answered in.
fn fixture_lead(model: &WorkspaceModel) -> String {
    model
        .agents
        .first()
        .map(|a| a.meta.id.clone())
        .expect("the demo fixture registers at least one lane")
}

/// **The witness (#5143).** The page's command popup starts in the composer's
/// own text column.
///
/// #5143 read the popup's `x = area.x + 3` against `PROMPT_PREFIX_W == 4` and
/// concluded #5051 had left it one column adrift. Measured, it had not: each
/// popup row is drawn `" {name} "`, and that leading space is the fourth
/// column. The two numbers agreed by coincidence, though — one a literal `3`,
/// the other the prefix's width — and nothing anywhere asserted the agreement,
/// which is the half of #5143 that was real. This is that assertion.
///
/// It compares the columns rather than pinning either one, so it survives a
/// change to the prefix and fails only when the two stop tracking each other.
#[test]
fn the_page_menu_starts_in_the_composers_text_column() {
    let mut model = fixture_model();
    model.apply_inbound(&Inbound::ShellEvent {
        agent: fixture_lead(&model),
        event: AgentEvent::Text {
            text: "hello".into(),
        },
    });
    let mut ui = ui_for(DeckTab::Session);
    ui.agents_page.open = true;
    ui.slash_commands = vec![
        stella_tui::SlashCommand::custom("/model".to_string(), "pick a model".to_string()),
        stella_tui::SlashCommand::custom("/models".to_string(), "list models".to_string()),
    ];
    for c in "/mod".chars() {
        ui.agents_page.composer.insert_char(c);
    }
    let frame = render_frame(&model, &mut ui, W, H);

    let col_of = |needle: &str| -> usize {
        frame
            .lines()
            .find(|l| l.trim_end().contains(needle))
            .map(|l| l.find(needle.trim_start()).expect("the needle"))
            .unwrap_or_else(|| panic!("no row carrying {needle:?}:\n{frame}"))
    };

    // The composer's text: everything after the shared prompt prefix.
    let composer_col = col_of(">>> ") + stella_tui::views::frame::PROMPT_PREFIX_W;
    // The popup's first name. `/models` is the longer match, so `/model` is
    // unambiguous as a row start only with the trailing space the row adds.
    let menu_col = col_of("/models");

    assert_eq!(
        menu_col, composer_col,
        "the popup's names and the composer's text are in different columns \
         ({menu_col} vs {composer_col}):\n{frame}"
    );
}
