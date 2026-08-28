// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The composer's own suite: the textarea, paste-chip collapse, Enter
//! classification, and the slash menu's filter, ranking and sections.
//!
//! Split out of `composer.rs` when that file crossed the 1500-line guard —
//! the pattern `render/tests.rs` and `deck_ui/tests/` already follow. The
//! module under test is `super`, glob-imported here so every assertion below
//! reads exactly as it did inside the file.

use super::*;

fn commands() -> Vec<SlashCommand> {
    vec![
        SlashCommand::new("/help", "show help"),
        SlashCommand::new("/clear", "clear the transcript"),
        SlashCommand::new("/models", "list models"),
        SlashCommand::new("/diff", "open the diff viewer"),
        SlashCommand::new("/files", "focus the files panel"),
    ]
}

#[test]
fn small_paste_inserts_inline() {
    let mut c = Composer::with_paste_threshold(6);
    c.paste("one\ntwo\nthree");
    assert!(c.chips().is_empty());
    assert_eq!(c.buffer(), "one\ntwo\nthree");
}

#[test]
fn large_paste_collapses_to_a_chip_but_keeps_the_payload() {
    let mut c = Composer::with_paste_threshold(3);
    let payload = "a\nb\nc\nd\ne";
    c.paste(payload);
    assert_eq!(c.chips().len(), 1);
    // The real display path (what the renderers draw) shows the chip form.
    assert_eq!(layout(&c, 80).rows, vec!["[pasted: 5 lines] ".to_string()]);
    // The full payload survives to submission.
    let msg = c.take_submission().unwrap().text;
    assert_eq!(msg, payload);
}

#[test]
fn typed_text_before_a_chip_keeps_its_order_on_submit() {
    let mut c = Composer::with_paste_threshold(3);
    for ch in "review this: ".chars() {
        c.insert_char(ch);
    }
    c.paste("x\ny\nz\nw");
    for ch in " thanks".chars() {
        c.insert_char(ch);
    }
    let msg = c.take_submission().unwrap().text;
    assert_eq!(msg, "review this: \nx\ny\nz\nw\n thanks");
    assert!(c.is_empty(), "submission clears the composer");
}

#[test]
fn display_never_leaks_the_raw_payload() {
    let mut c = Composer::with_paste_threshold(2);
    c.paste("secret-line-1\nsecret-line-2\nsecret-line-3");
    let shown = layout(&c, 200).rows.join("\n");
    assert!(
        !shown.contains("secret"),
        "chip must hide the payload: {shown}"
    );
}

fn test_attachment(name: &str) -> Attachment {
    Attachment::from_path(name, "image/png", 1024, format!("/tmp/{name}"))
}

#[test]
fn attachments_ride_the_submission_not_its_text() {
    let mut c = Composer::new();
    for ch in "see ".chars() {
        c.insert_char(ch);
    }
    c.attach(test_attachment("shot.png"));
    for ch in "what broke?".chars() {
        c.insert_char(ch);
    }
    let shown = layout(&c, 200).rows.join("\n");
    assert!(shown.contains("[image: shot.png"), "{shown}");
    let submission = c.take_submission().unwrap();
    assert_eq!(submission.text, "see \nwhat broke?");
    assert_eq!(submission.attachments.len(), 1);
    assert_eq!(submission.attachments[0].name, "shot.png");
    assert!(c.is_empty(), "submission clears attachments too");
}

#[test]
fn attachment_only_submission_is_submittable() {
    let mut c = Composer::new();
    c.attach(test_attachment("clip.png"));
    assert!(!c.is_empty());
    let submission = c.take_submission().unwrap();
    assert_eq!(submission.text, "");
    assert_eq!(submission.attachments.len(), 1);
}

#[test]
fn backspace_pops_an_attachment_chip_when_the_buffer_is_empty() {
    let mut c = Composer::new();
    c.attach(test_attachment("oops.png"));
    c.backspace();
    assert!(c.is_blank(), "backspace removes the pending attachment");
}

#[test]
fn backspace_pops_a_chip_when_the_buffer_is_empty() {
    let mut c = Composer::with_paste_threshold(2);
    c.paste("a\nb\nc");
    assert_eq!(c.chips().len(), 1);
    c.backspace(); // buffer empty → removes the chip
    assert!(c.chips().is_empty());
}

/// The vocabulary with domains, for the palette tests.
fn classified_commands() -> Vec<SlashCommand> {
    vec![
        SlashCommand::new("/help", "show help").in_domain(SlashDomain::Session),
        SlashCommand::new("/clear", "clear the transcript").in_domain(SlashDomain::Session),
        SlashCommand::new("/plan", "the plan").in_domain(SlashDomain::Plan),
        SlashCommand::new("/budget", "set the spend cap").in_domain(SlashDomain::Plan),
        SlashCommand::new("/diff", "open the diff viewer").in_domain(SlashDomain::Code),
        SlashCommand::custom("/fix-bug", "fix a bug end to end"),
    ]
}

/// **The witness (#5048).** A query whose letters are scattered through a
/// name matches it, and the menu says where each letter landed so the
/// renderer can light them. `ga` is neither a prefix nor a substring of
/// `graph query`, so before this the row did not appear at all.
#[test]
fn a_scattered_query_matches_and_reports_where_its_letters_landed() {
    let cmds = vec![
        SlashCommand::new("/graph query", "free-form graph query").in_domain(SlashDomain::Code),
    ];
    let mut c = Composer::new();
    for ch in "/ga".chars() {
        c.insert_char(ch);
    }
    let menu = c.slash_menu(&cmds, &PaletteState::default()).expect("menu");
    let m = menu.matches.first().expect("`ga` reaches `/graph query`");
    assert_eq!(m.command.name, "/graph query");
    assert_eq!(
        m.highlights,
        vec![1, 3],
        "the `g` and the `a` of `/graph`, counted from the slash"
    );
}

/// A scattered match is the weakest name match, so it sorts under every
/// prefix and substring one — and a description match sorts under it.
#[test]
fn a_scattered_name_match_sorts_under_the_contiguous_ones() {
    let cmds = vec![
        SlashCommand::new("/graph", "the code graph"),
        SlashCommand::new("/gates", "the gate board"),
        SlashCommand::new("/models", "pick a model, gateway included"),
    ];
    let mut c = Composer::new();
    for ch in "/ga".chars() {
        c.insert_char(ch);
    }
    let menu = c.slash_menu(&cmds, &PaletteState::default()).expect("menu");
    let names: Vec<&str> = menu
        .matches
        .iter()
        .map(|m| m.command.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["/gates", "/graph", "/models"],
        "prefix, then scattered, then description-only: {names:?}"
    );
}

/// **The witness (#5048).** Running a row from the palette records it, so
/// the next browse list can offer it back under `recent`. Tab completes
/// rather than runs, and records nothing.
#[test]
fn running_a_row_records_it_and_completing_one_does_not() {
    use crossterm::event::{KeyCode, KeyEvent};
    let cmds = classified_commands();
    let mut c = Composer::new();
    let names = vec!["/plan".to_string(), "/diff".to_string()];
    let mut selected = 1usize;

    handle_slash_popup_key(KeyEvent::from(KeyCode::Tab), &names, &mut c, &mut selected);
    assert!(c.recent().is_empty(), "completing is not running");

    selected = 1;
    handle_slash_popup_key(
        KeyEvent::from(KeyCode::Enter),
        &names,
        &mut c,
        &mut selected,
    );
    assert_eq!(
        c.recent()
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>(),
        ["/diff"]
    );

    c.insert_char('/');
    let menu = c.slash_menu(&cmds, &PaletteState::default()).expect("menu");
    assert!(
        menu.sections.iter().any(|(_, h)| h == "recent"),
        "the section appears once there is something in it: {:?}",
        menu.sections
    );
    assert_eq!(
        menu.matches.last().map(|m| m.command.name.as_str()),
        Some("/diff"),
        "and it closes the list"
    );
}

/// **The witness (#4338).** The browse list opens on what the session
/// makes relevant, under a heading that says why, then one group per
/// domain — not thirty rows in vocabulary order.
#[test]
fn the_browse_list_leads_with_relevance_then_groups_by_domain() {
    let cmds = classified_commands();
    let state = PaletteState {
        turn_running: true,
        ..PaletteState::default()
    };
    let mut c = Composer::new();
    c.insert_char('/');
    let menu = c.slash_menu(&cmds, &state).expect("slash menu active");

    let names: Vec<&str> = menu
        .matches
        .iter()
        .map(|m| m.command.name.as_str())
        .collect();
    assert_eq!(
        &names[..2],
        &["/plan", "/budget"],
        "the running turn's commands lead: {names:?}"
    );
    assert_eq!(
        menu.sections.first(),
        Some(&(0, "relevant now · a turn is running".to_string())),
        "the heading says why: {:?}",
        menu.sections
    );
    assert_eq!(
        menu.sections[1..].to_vec(),
        vec![
            (2, "session".to_string()),
            (4, "workspace".to_string()),
            (5, "custom".to_string()),
        ],
        "one heading per remaining group, in domain order"
    );
}

/// A quiet session has no relevance block at all — the list is the domain
/// groups alone, with no heading claiming a reason that does not exist.
#[test]
fn a_quiet_browse_list_is_groups_only() {
    let cmds = classified_commands();
    let mut c = Composer::new();
    c.insert_char('/');
    let menu = c
        .slash_menu(&cmds, &PaletteState::default())
        .expect("slash menu active");
    assert!(
        !menu.sections.iter().any(|(_, h)| h.starts_with("relevant")),
        "nothing to be relevant about: {:?}",
        menu.sections
    );
    assert_eq!(menu.sections.first().map(|(at, _)| *at), Some(0));
    let names: Vec<&str> = menu
        .matches
        .iter()
        .map(|m| m.command.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["/help", "/clear", "/plan", "/budget", "/diff", "/fix-bug"],
        "domain order, vocabulary order within a group"
    );
}

/// A typed query keeps one flat ranked list — but a relevant command
/// leads its rank, so `/b` mid-turn opens on `/budget` rather than on
/// whatever the vocabulary happened to list first.
#[test]
fn a_typed_query_promotes_the_relevant_match_without_headings() {
    let cmds = classified_commands();
    let state = PaletteState {
        turn_running: true,
        ..PaletteState::default()
    };
    let mut c = Composer::new();
    for ch in "/p".chars() {
        c.insert_char(ch);
    }
    let menu = c.slash_menu(&cmds, &state).expect("slash menu active");
    assert!(menu.sections.is_empty(), "no headings under a query");
    let names: Vec<&str> = menu
        .matches
        .iter()
        .map(|m| m.command.name.as_str())
        .collect();
    assert_eq!(
        names.first(),
        Some(&"/plan"),
        "the prefix match still leads: {names:?}"
    );
    assert_eq!(
        names.iter().position(|n| *n == "/budget"),
        Some(2),
        "and the relevant one leads its own (weaker) rank: {names:?}"
    );

    // Idle, the same query is the plain fuzzy ranking: `/budget` sits
    // where the vocabulary put it, behind the two rows above it.
    let idle = c
        .slash_menu(&cmds, &PaletteState::default())
        .expect("slash menu active");
    let idle_names: Vec<&str> = idle
        .matches
        .iter()
        .map(|m| m.command.name.as_str())
        .collect();
    assert_eq!(idle_names.first(), Some(&"/plan"));
    assert_eq!(
        idle_names.iter().position(|n| *n == "/budget"),
        Some(3),
        "relevance is what moved it: {idle_names:?}"
    );
}

#[test]
fn slash_menu_fuzzy_ranks_name_prefix_over_substring_over_description() {
    let cmds = commands();
    let mut c = Composer::new();
    for ch in "/f".chars() {
        c.insert_char(ch);
    }
    let menu = c
        .slash_menu(&cmds, &PaletteState::default())
        .expect("slash menu active");
    let names: Vec<&str> = menu
        .matches
        .iter()
        .map(|m| m.command.name.as_str())
        .collect();
    // `/files` starts with the query; `/diff` merely contains it — the
    // prefix match must lead.
    assert_eq!(names, vec!["/files", "/diff"]);
}

#[test]
fn slash_menu_falls_back_to_description_matches() {
    let cmds = commands();
    let mut c = Composer::new();
    for ch in "/transcript".chars() {
        c.insert_char(ch);
    }
    let menu = c
        .slash_menu(&cmds, &PaletteState::default())
        .expect("slash menu active");
    let names: Vec<&str> = menu
        .matches
        .iter()
        .map(|m| m.command.name.as_str())
        .collect();
    // No name contains "transcript"; `/clear`'s description does.
    assert_eq!(names, vec!["/clear"]);
}

#[test]
fn bare_slash_lists_every_command() {
    let cmds = commands();
    let mut c = Composer::new();
    c.insert_char('/');
    let menu = c.slash_menu(&cmds, &PaletteState::default()).unwrap();
    assert_eq!(menu.matches.len(), cmds.len());
}

#[test]
fn slash_menu_is_inactive_once_a_space_is_typed() {
    let cmds = commands();
    let mut c = Composer::new();
    for ch in "/models ".chars() {
        c.insert_char(ch);
    }
    assert!(c.slash_menu(&cmds, &PaletteState::default()).is_none());
}

#[test]
fn slash_command_constructors_set_the_kind() {
    assert_eq!(SlashCommand::new("/help", "d").kind, SlashKind::Builtin);
    assert_eq!(SlashCommand::custom("/x", "d").kind, SlashKind::Custom);
}

#[test]
fn slash_menu_is_inactive_when_chips_are_present() {
    let cmds = commands();
    let mut c = Composer::with_paste_threshold(2);
    c.paste("a\nb\nc");
    c.insert_char('/');
    assert!(c.slash_menu(&cmds, &PaletteState::default()).is_none());
}

#[test]
fn line_count_ignores_a_trailing_newline() {
    assert_eq!(line_count("a\nb\n"), 2);
    assert_eq!(line_count("a\nb"), 2);
    assert_eq!(line_count(""), 0);
    assert_eq!(line_count("solo"), 1);
}

// Textarea semantics

fn typed(text: &str) -> Composer {
    let mut c = Composer::new();
    for ch in text.chars() {
        c.insert_char(ch);
    }
    c
}

#[test]
fn newlines_typed_into_the_buffer_survive_submission_verbatim() {
    let mut c = typed("first line");
    c.insert_newline();
    for ch in "second line".chars() {
        c.insert_char(ch);
    }
    assert_eq!(c.take_submission().unwrap().text, "first line\nsecond line");
}

#[test]
fn insert_and_backspace_act_at_the_cursor() {
    let mut c = typed("hello");
    c.move_left();
    c.move_left();
    c.insert_char('X'); // hel X lo
    assert_eq!(c.buffer(), "helXlo");
    c.backspace(); // removes the X, not the tail
    assert_eq!(c.buffer(), "hello");
    assert_eq!(c.cursor(), 3);
}

#[test]
fn move_to_start_and_end_bound_the_whole_buffer() {
    let mut c = typed("a\nb\nc");
    c.move_to_start();
    assert_eq!(c.cursor(), 0, "before the first character");
    c.move_to_end();
    assert_eq!(c.cursor(), c.buffer().len(), "one past the last character");
}

#[test]
fn vertical_motion_keeps_the_column_and_clamps_to_short_lines() {
    let mut c = typed("long line\nab\nlonger line");
    // Cursor at end of "longer line"; up lands clamped to "ab"'s end.
    c.move_up();
    assert_eq!(&c.buffer()[..c.cursor()], "long line\nab");
    // Up again: column carried from the clamp point (2) into "long line".
    c.move_up();
    assert_eq!(&c.buffer()[..c.cursor()], "lo");
    // Down from the first line's column 2 → "ab" clamps to its end again.
    c.move_down();
    assert_eq!(&c.buffer()[..c.cursor()], "long line\nab");
    // Down on the last line jumps to the very end.
    c.move_down();
    c.move_down();
    assert_eq!(c.cursor(), c.buffer().len());
}

#[test]
fn line_start_and_end_stay_within_the_logical_line() {
    let mut c = typed("one\ntwo three");
    // Cursor at end; Home goes to the start of "two three", not offset 0.
    c.move_line_start();
    assert_eq!(&c.buffer()[..c.cursor()], "one\n");
    c.move_line_end();
    assert_eq!(c.cursor(), c.buffer().len());
}

#[test]
fn paste_lands_at_the_cursor_and_normalizes_line_endings() {
    let mut c = typed("ac");
    c.move_left();
    c.paste("b\r\nB"); // small paste: inline, CRLF → LF
    assert_eq!(c.buffer(), "ab\nBc");
}

#[test]
fn big_paste_mid_buffer_keeps_the_tail_after_the_chip() {
    let mut c = Composer::with_paste_threshold(2);
    for ch in "headtail".chars() {
        c.insert_char(ch);
    }
    for _ in 0..4 {
        c.move_left(); // cursor between "head" and "tail"
    }
    c.paste("x\ny\nz");
    let msg = c.take_submission().unwrap().text;
    assert_eq!(msg, "head\nx\ny\nz\ntail", "order: before, chip, after");
}

#[test]
fn classify_enter_submits_bare_and_breaks_on_a_modifier() {
    let plain = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let cmd = KeyEvent::new(KeyCode::Enter, KeyModifiers::SUPER);
    let meta = KeyEvent::new(KeyCode::Enter, KeyModifiers::META);
    let ctrl = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);
    let alt = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
    // Bare Enter always submits (never blocks).
    assert_eq!(classify_enter(&plain), EnterAction::Submit);
    // Every newline modifier inserts a line break instead.
    assert_eq!(classify_enter(&cmd), EnterAction::Newline);
    assert_eq!(classify_enter(&meta), EnterAction::Newline);
    assert_eq!(classify_enter(&ctrl), EnterAction::Newline);
    assert_eq!(classify_enter(&alt), EnterAction::Newline);
    // A non-Enter key is not this function's concern.
    let other = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
    assert_eq!(classify_enter(&other), EnterAction::NotEnter);
}

#[test]
fn edit_keys_leave_an_empty_composer_to_the_surface() {
    // Arrows on an empty buffer must fall through (they scroll views).
    let mut c = Composer::new();
    for code in [KeyCode::Left, KeyCode::Right, KeyCode::Up, KeyCode::Down] {
        assert!(!handle_edit_key(
            KeyEvent::new(code, KeyModifiers::NONE),
            &mut c
        ));
    }
    // ↑/↓ on a single-line buffer also fall through (transcript scroll).
    let mut single = typed("one line");
    assert!(!handle_edit_key(
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        &mut single
    ));
    assert!(handle_edit_key(
        KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
        &mut single
    ));
}

// Soft-wrap layout

#[test]
fn layout_soft_wraps_long_lines_and_hard_breaks_newlines() {
    let c = typed("abcdef\ngh");
    let l = layout(&c, 4);
    assert_eq!(l.rows, vec!["abcd", "ef", "gh"]);
    // Cursor at the end: one past 'h' on the last row.
    assert_eq!((l.cursor_row, l.cursor_col), (2, 2));
}

#[test]
fn layout_places_the_cursor_mid_text() {
    let mut c = typed("abcdef");
    for _ in 0..2 {
        c.move_left(); // cursor before 'e' (offset 4)
    }
    let l = layout(&c, 4);
    assert_eq!(l.rows, vec!["abcd", "ef"]);
    assert_eq!((l.cursor_row, l.cursor_col), (1, 0), "'e' starts row 1");
}

#[test]
fn layout_gives_the_cursor_a_fresh_row_when_the_last_row_is_full() {
    let c = typed("abcd");
    let l = layout(&c, 4);
    assert_eq!(l.rows, vec!["abcd", ""]);
    assert_eq!((l.cursor_row, l.cursor_col), (1, 0));
}

#[test]
fn layout_shows_chips_as_their_display_form() {
    let mut c = Composer::with_paste_threshold(2);
    c.paste("a\nb\nc");
    for ch in "ok".chars() {
        c.insert_char(ch);
    }
    let l = layout(&c, 40);
    assert_eq!(l.rows, vec!["[pasted: 3 lines] ok"]);
    assert_eq!((l.cursor_row, l.cursor_col), (0, 20));
}

#[test]
fn layout_is_wide_char_aware() {
    let c = typed("日本語"); // width 2 each
    let l = layout(&c, 4);
    assert_eq!(l.rows, vec!["日本", "語"]);
    assert_eq!((l.cursor_row, l.cursor_col), (1, 2));
}

#[test]
fn split_row_at_returns_the_char_under_the_cursor() {
    assert_eq!(split_row_at("abc", 1), ("a".into(), Some('b'), "c".into()));
    assert_eq!(split_row_at("abc", 3), ("abc".into(), None, String::new()));
    assert_eq!(
        split_row_at("日本", 2),
        ("日".into(), Some('本'), String::new())
    );
}

#[test]
fn empty_composer_lays_out_as_one_empty_row_with_the_cursor_home() {
    let c = Composer::new();
    let l = layout(&c, 10);
    assert_eq!(l.rows, vec![""]);
    assert_eq!((l.cursor_row, l.cursor_col), (0, 0));
}
