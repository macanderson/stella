// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The deck's keymap — one table, read by every surface that tells a person
//! what a key does.
//!
//! The `?` sheet (`deck_render::help`), the hint row under the composer
//! (`views::frame::render_hint_row`) and the README's key list used to be three
//! hand-written copies of the same facts, and the sheet had already drifted:
//! it advertised plan-review keys (`a`/`t`/`x`/`r`) whose handler had been
//! deleted, and never mentioned `ctrl-a`. A key a person cannot find is a
//! key that does not exist, so the copies collapse into this table and the
//! surfaces render it.
//!
//! What this table is **not**: the dispatcher. `deck_ui::handle_deck_key` is
//! a precedence chain (the crate README says why), and a binding here is a
//! claim about it. Every row therefore **names the test that presses its
//! keys** ([`Binding::witness`]), and `every_binding_names_a_witness_that_exists`
//! below fails if that test is not in the swept sources — the shape
//! `stella-parity` uses for its cross-surface matrix. A row added without a
//! witness is a red test, not a sheet that quietly lies (#4368).
//!
//! The guard checks that the named test **exists**, not that it still presses
//! that chord: that half is prose for a reviewer, the same trade
//! `stella-protocol`'s consumer ledger documents. What it buys is that a row
//! and a test are added together, and that deleting the test breaks the build
//! rather than the sheet.

use unicode_width::UnicodeWidthStr;

use crate::deck::DeckTab;

/// Where a binding applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The focus tree — the same four keys at every level (`deck_ui::focus`).
    Navigation,
    /// On one tab, from an empty composer unless the row says otherwise.
    Tab(DeckTab),
    /// On every tab.
    Everywhere,
}

/// One row of the sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub keys: &'static str,
    pub does: &'static str,
    pub scope: Scope,
    /// Whether the hint row under the composer carries this binding. Kept to
    /// a handful: the hint row is one line and names what a hand reaches for
    /// mid-turn, not everything.
    pub hinted: bool,
    /// The test functions that press these keys and assert the effect, by
    /// name. Never empty — a row with no witness is the drift this table
    /// exists to stop. Several names where one key is really two verbs
    /// (`v / r`, `o / p`), so each half is proved rather than one standing in
    /// for the pair.
    pub witness: &'static [&'static str],
}

const fn row(
    keys: &'static str,
    does: &'static str,
    scope: Scope,
    witness: &'static [&'static str],
) -> Binding {
    Binding {
        keys,
        does,
        scope,
        hinted: false,
        witness,
    }
}

const fn hinted(
    keys: &'static str,
    does: &'static str,
    scope: Scope,
    witness: &'static [&'static str],
) -> Binding {
    Binding {
        keys,
        does,
        scope,
        hinted: true,
        witness,
    }
}

use DeckTab as T;
use Scope::{Everywhere, Navigation, Tab};

/// The chord that opens the plan card, spelled as the `?` sheet prints it.
///
/// Named rather than written three times. SPEC 11 bound `tab` to the plan
/// panel while the deck had `tab` on the tab strip and the plan card on
/// `⌃S`; the tab row's breadcrumb hint agreed with the deck, but as a literal
/// that no rebind would have moved (#4341). The deck wins — `tab`/`⇧tab` are
/// deck-global navigation, and a hand reaches for them in every other terminal
/// application — so the spec was amended and both surfaces now read this.
pub const PLAN_CARD_KEYS: &str = "ctrl-s";

/// `ctrl-s` as the chrome prints it: `⌃S`.
///
/// The `?` sheet spells a chord out and the one-row surfaces have no cells to
/// spare, so the two forms are one fact rendered twice. Anything that is not a
/// single-letter control chord has no caret form and comes back unchanged.
fn caret_form(keys: &str) -> String {
    match keys.strip_prefix("ctrl-") {
        Some(letter) if letter.chars().count() == 1 => format!("⌃{}", letter.to_uppercase()),
        _ => keys.to_string(),
    }
}

/// [`PLAN_CARD_KEYS`] in caret form, for the surfaces that print a chord in
/// one cell of chrome.
#[must_use]
pub fn plan_card_chord() -> String {
    caret_form(PLAN_CARD_KEYS)
}

/// Every binding the deck teaches, in the order the sheet prints them
/// within each scope.
pub const BINDINGS: &[Binding] = &[
    // ── the focus tree ──────────────────────────────────────────────────
    row(
        "← →",
        "the sibling: the pane beside this one, else the tab beside it",
        Navigation,
        &["left_and_right_walk_the_tab_strip_from_every_tab"],
    ),
    row(
        "← ←",
        "the AGENTS page: every lane and session, and a prompt that starts a new one",
        Tab(T::Session),
        &["left_left_opens_the_agents_page_from_the_session_tab"],
    ),
    row(
        "↑ ↓ · k j",
        "the item above / below",
        Navigation,
        &[
            "a_list_takes_arrows_and_jk_alike_and_clamps",
            "the_engine_panel_takes_j_and_end",
        ],
    ),
    row(
        "⏎",
        "open it — a diff, a lane, a message's full text",
        Navigation,
        &["enter_opens_the_highlighted_message"],
    ),
    row(
        "⌫",
        "back up one level; never reaches a running turn",
        Navigation,
        &["backspace_peels_one_layer_at_a_time"],
    ),
    row(
        "⇞ ⇟ · Home End",
        "a page · the ends",
        Navigation,
        &["a_body_scrolls_pages_and_jumps"],
    ),
    row(
        "tab / ⇧tab",
        "the next / previous tab",
        Navigation,
        &["tab_and_backtab_walk_the_tab_bar"],
    ),
    row(
        "q / esc",
        "close any overlay",
        Navigation,
        &["help_overlay_closes_with_esc_or_q"],
    ),
    // ── SESSION ─────────────────────────────────────────────────────────
    row(
        "↑",
        "walk the transcript — with prompts queued, the queue editor first",
        Tab(T::Session),
        &["up_arrow_on_session_opens_the_queue_editor_on_the_newest_prompt"],
    ),
    row(
        "↓",
        "the SUB-AGENTS overlay — every lane and delegate, with controls",
        Tab(T::Session),
        &["down_opens_the_overlay_and_the_arrows_walk_in_and_out"],
    ),
    row(
        "⇞ ⇟",
        "scroll the transcript",
        Tab(T::Session),
        &["page_keys_still_scroll_the_transcript_while_a_hunk_card_is_up"],
    ),
    row(
        "⌘[ / ⌘]",
        "jump to transcript start / end (⌃ works too)",
        Tab(T::Session),
        &["bracket_jumps_reach_both_ends_of_the_transcript"],
    ),
    hinted(
        "ctrl-n / ctrl-p",
        "jump to the next / previous failure",
        Tab(T::Session),
        &["ctrl_n_with_the_bar_closed_seeks_the_next_failure"],
    ),
    hinted(
        "ctrl-z",
        "fold the highlighted turn to one line (none: all)",
        Tab(T::Session),
        &["ctrl_z_with_no_selection_toggles_the_fold_all_overlay"],
    ),
    hinted(
        "ctrl-o",
        "expand / collapse the highlighted message (none: all)",
        Tab(T::Session),
        &["up_selects_the_last_message_and_ctrl_o_toggles_it"],
    ),
    row(
        "ctrl-r",
        "expand / collapse all thinking",
        Tab(T::Session),
        &["ctrl_r_toggles_thinking_from_any_tab"],
    ),
    row(
        "ctrl-f",
        "find in the transcript — ⏎ next · ctrl-p previous",
        Tab(T::Session),
        &["the_search_bar_is_modal_over_the_composer"],
    ),
    row(
        "⌫ / esc",
        "at an opened lane: back to the agent that dispatched it",
        Tab(T::Session),
        &["backspace_backs_out_of_a_lane_and_never_stops_it"],
    ),
    // ── AGENTS ──────────────────────────────────────────────────────────
    row(
        "⏎",
        "edit the definition — a save is a new pinned version",
        Tab(T::Agents),
        &["installed_enter_opens_the_editor_and_ctrl_s_saves_a_new_version"],
    ),
    row(
        "a",
        "assume its identity: the lead runs as this agent",
        Tab(T::Agents),
        &["installed_x_twice_deletes_and_a_assumes"],
    ),
    row(
        "n",
        "new agent — drafted by the LLM",
        Tab(T::Agents),
        &["create_flow_describes_picks_scope_and_dispatches_the_llm_draft"],
    ),
    row(
        "x x",
        "delete the agent, every version",
        Tab(T::Agents),
        &["installed_x_twice_deletes_and_a_assumes"],
    ),
    row(
        "v / r",
        "versions · reload",
        Tab(T::Agents),
        &[
            "version_picker_pins_an_older_version_without_editing",
            "installed_r_reloads_the_list_from_disk",
        ],
    ),
    // ── TRACES ──────────────────────────────────────────────────────────
    row(
        "f",
        "cycle the per-agent filter",
        Tab(T::Traces),
        &["traces_filter_cycles_through_agents_and_back"],
    ),
    // ── GRAPH ───────────────────────────────────────────────────────────
    row(
        "↑ ↓",
        "walk the neighborhood",
        Tab(T::Graph),
        &["arrows_walk_the_neighborhood_and_clamp_at_both_ends"],
    ),
    row(
        "/ or ⏎",
        "file picker — re-root on any indexed file",
        Tab(T::Graph),
        &[
            "slash_opens_the_picker_defaulting_to_the_current_focus",
            "enter_also_opens_the_picker",
        ],
    ),
    // ── FILES ───────────────────────────────────────────────────────────
    row(
        "⏎",
        "open / close the diff",
        Tab(T::Files),
        &["files_enter_opens_and_closes_the_diff"],
    ),
    // ── SKILLS ──────────────────────────────────────────────────────────
    row(
        "← →",
        "installed ↔ registry search (→ past the search leaves the tab)",
        Tab(T::Skills),
        &["skills_left_and_right_cross_the_panes_and_then_leave_the_tab"],
    ),
    row(
        "space",
        "enable / disable",
        Tab(T::Skills),
        &["skills_space_toggles_and_two_ctrl_x_uninstalls"],
    ),
    row(
        "e",
        "edit the selected skill",
        Tab(T::Skills),
        &["skills_e_opens_edit_overlay_and_ctrl_s_saves"],
    ),
    row(
        "p",
        "pin / unpin",
        Tab(T::Skills),
        &["skills_p_opens_pin_picker_and_enter_pins"],
    ),
    row(
        "n",
        "new skill — drafted by the LLM",
        Tab(T::Skills),
        &["skills_n_creates_via_description_then_scope"],
    ),
    row(
        "ctrl-o",
        "preview",
        Tab(T::Skills),
        &["skills_ctrl_o_on_installed_opens_preview_with_local_body"],
    ),
    row(
        "ctrl-x ×2",
        "delete (press twice to confirm)",
        Tab(T::Skills),
        &["skills_space_toggles_and_two_ctrl_x_uninstalls"],
    ),
    row(
        "type",
        "search skills",
        Tab(T::Skills),
        &["skills_search_pane_types_a_query_and_dispatches_a_search"],
    ),
    // ── MCP ─────────────────────────────────────────────────────────────
    row(
        "space / e",
        "enable / disable",
        Tab(T::Mcp),
        &["mcp_tab_navigates_toggles_and_enters_search"],
    ),
    row(
        "ctrl-o",
        "inspect the server",
        Tab(T::Mcp),
        &["ctrl_o_opens_the_mcp_inspector_for_the_highlighted_server"],
    ),
    row(
        "a",
        "authenticate (env credentials)",
        Tab(T::Mcp),
        &["mcp_auth_prompt_captures_a_masked_value_as_a_redacted_secret"],
    ),
    row(
        "o",
        "OAuth login (http servers)",
        Tab(T::Mcp),
        &["mcp_o_starts_an_oauth_login_and_explains_itself_on_a_stdio_server"],
    ),
    row(
        "s",
        "search the registry",
        Tab(T::Mcp),
        &["mcp_tab_navigates_toggles_and_enters_search"],
    ),
    row(
        "x",
        "remove the server",
        Tab(T::Mcp),
        &["mcp_x_removes_the_selected_server_and_r_refreshes"],
    ),
    row(
        "r",
        "refresh",
        Tab(T::Mcp),
        &["mcp_x_removes_the_selected_server_and_r_refreshes"],
    ),
    // ── ISSUES ──────────────────────────────────────────────────────────
    row(
        "space",
        "select several",
        Tab(T::Issues),
        &["issues_space_toggles_a_pick_and_the_pick_follows_the_key"],
    ),
    row(
        "r",
        "refresh the list",
        Tab(T::Issues),
        &["issues_browse_keys_refresh_and_start_work"],
    ),
    row(
        "/",
        "search the tracker",
        Tab(T::Issues),
        &["issues_tracker_search_fires_on_enter_and_esc_returns"],
    ),
    row(
        "] [",
        "next / previous page",
        Tab(T::Issues),
        &["issues_brackets_page_the_active_query"],
    ),
    row(
        "o / p",
        "open in the browser · push the pick to the prompt",
        Tab(T::Issues),
        &[
            "issues_o_opens_the_selection_in_the_browser",
            "issues_p_submits_the_picked_issues_as_a_prompt",
        ],
    ),
    row(
        "n",
        "new issue — tab cycles fields · ctrl-s creates",
        Tab(T::Issues),
        &["issue_form_ctrl_s_submits_the_parsed_fields"],
    ),
    row(
        "c",
        "comment on the selected issue",
        Tab(T::Issues),
        &["issues_comment_prompt_sends_the_act_for_the_selected_issue"],
    ),
    row(
        "s",
        "set the selected issue's status",
        Tab(T::Issues),
        &["issues_s_opens_the_status_prompt_for_the_selection"],
    ),
    row(
        "w",
        "start work on the selected issue",
        Tab(T::Issues),
        &["issues_browse_keys_refresh_and_start_work"],
    ),
    // ── SETTINGS ────────────────────────────────────────────────────────
    row(
        "← →",
        "agents ↔ tools ↔ seats (past the ends leaves the tab)",
        Tab(T::Settings),
        &["left_right_walk_the_panes_then_rise_to_the_tab_strip"],
    ),
    row(
        "e",
        "edit the pane you are on",
        Tab(T::Settings),
        &["e_focuses_the_editor_of_the_highlighted_pane"],
    ),
    row(
        "t",
        "jump to the tool switches",
        Tab(T::Settings),
        &["t_jumps_to_the_tools_pane_and_focuses_it"],
    ),
    row(
        "tab",
        "in the editor: switch agent — global / default / worker / …",
        Tab(T::Settings),
        &["tabs_cycle_and_rows_clamp"],
    ),
    row(
        "⏎",
        "in the editor: edit the selected row / pick a model",
        Tab(T::Settings),
        &[
            "enter_cycles_reasoning_none_on_off",
            "picker_filters_by_substring_and_enter_sets_the_model",
        ],
    ),
    row(
        "space",
        "in the editor: toggle the selected row",
        Tab(T::Settings),
        &["enter_cycles_reasoning_none_on_off"],
    ),
    row(
        "x",
        "in the editor: clear the selected row",
        Tab(T::Settings),
        &["enter_cycles_reasoning_none_on_off"],
    ),
    row(
        "s / S",
        "in the editor: save to user / project settings",
        Tab(T::Settings),
        &["s_saves_the_working_copy_at_user_scope"],
    ),
    row(
        "r",
        "in the editor: reload from disk",
        Tab(T::Settings),
        &["settings_r_reloads_the_engine_config_from_disk"],
    ),
    row(
        "esc",
        "in the editor: hand the keyboard back to the tab",
        Tab(T::Settings),
        &["e_on_the_settings_tab_focuses_the_panel_and_esc_unfocuses"],
    ),
    // ── everywhere ──────────────────────────────────────────────────────
    // Enter semantics are the inverse of the old chord-to-submit mapping (see
    // `composer::classify_enter`): a bare ⏎ dispatches, a *modified* ⏎ breaks
    // the line. The parenthetical is the fact a reviewer missed while a scope
    // card was waiting — the sidecar it describes swallowed their answer.
    row(
        "⏎",
        "queue the prompt — mid-turn it waits its turn (unless a card asks)",
        Everywhere,
        &["bare_enter_always_enqueues_a_prompt_without_blocking"],
    ),
    row(
        "⌘⏎ / ⌃⏎ / ⌥⏎",
        "insert a line break in the prompt",
        Everywhere,
        &["a_modified_enter_inserts_a_line_break_preserved_through_submit"],
    ),
    hinted(
        "!cmd",
        "run a shell command NOW (skips the queue)",
        Everywhere,
        &["bang_prefix_runs_a_shell_command_immediately_never_enqueued"],
    ),
    hinted(
        "/",
        "slash commands — ↑↓ pick · tab completes · ⏎ runs",
        Everywhere,
        &["deck_slash_popup_selects_completes_and_dispatches"],
    ),
    row(
        "ctrl-v",
        "paste — a copied image is attached to the prompt",
        Everywhere,
        &["ctrl_v_is_the_clipboard_pull_and_a_bare_v_is_not"],
    ),
    row(
        "ctrl-a",
        "SUB-AGENTS — o open · n nudge · f flag · x stop · xx delete · r resume · rr restart",
        Everywhere,
        &[
            "ctrl_a_opens_the_sub_agents_overlay_from_any_tab",
            "the_keys_control_the_selected_lane",
            "nudge_steers_the_lane_and_flag_tells_the_dispatcher",
            "ctrl_x_twice_kills_the_selected_lane_and_any_other_key_disarms",
            "x_stops_the_lane_and_a_second_x_deletes_it",
            "o_opens_the_selected_lane_like_enter",
        ],
    ),
    row(
        "ctrl-t",
        "the queue editor",
        Everywhere,
        &["ctrl_t_toggles_the_queue_editor"],
    ),
    row(
        "ctrl-e",
        "SESSIONS — every session on this machine",
        Everywhere,
        &["ctrl_e_and_ctrl_k_open_the_sessions_and_context_overlays"],
    ),
    row(
        "ctrl-k",
        "CONTEXT — active skills + MCP servers",
        Everywhere,
        &["ctrl_e_and_ctrl_k_open_the_sessions_and_context_overlays"],
    ),
    row(
        "ctrl-g",
        "INSPECT — the context sent on any recorded call",
        Everywhere,
        &["ctrl_g_opens_inspect_and_asks_the_driver_for_the_call_index"],
    ),
    row(
        PLAN_CARD_KEYS,
        "PLAN — every step of the approved plan, in full",
        Everywhere,
        &["ctrl_s_opens_and_closes_the_plan_card"],
    ),
    row(
        ">text",
        "steer the running turn — lands at the next step boundary",
        Everywhere,
        &["steering_sends_the_marker_the_driver_already_reads"],
    ),
    row(
        "esc",
        "steer: your draft + queue go INTO the running turn",
        Everywhere,
        &["esc_with_a_typed_draft_steers_the_running_turn_instead_of_cancelling_it"],
    ),
    row(
        "esc esc",
        "cancel NOW & hold — nothing runs until your next prompt",
        Everywhere,
        &["double_esc_inside_the_window_escalates_to_stop_and_hold"],
    ),
    row(
        "?",
        "this sheet",
        Everywhere,
        &["question_mark_opens_help_overlay"],
    ),
    row(
        "ctrl-c",
        "quit stella",
        Everywhere,
        &["ctrl_c_quits_from_any_tab"],
    ),
];

/// The rows for one scope, in table order.
pub fn in_scope(scope: Scope) -> impl Iterator<Item = &'static Binding> {
    BINDINGS.iter().filter(move |b| b.scope == scope)
}

/// The cells a surface tabulating these rows pads the key column to: the
/// widest [`Binding::keys`] in the table.
///
/// Derived from the table, never spelled beside it. The `?` sheet padded to a
/// literal 13 while `ctrl-n / ctrl-p` is 15 cells and `⇞ ⇟ · Home End` is 14,
/// so those two rows' descriptions began left of every other row's (#4428) —
/// the drift a second copy of a fact always reaches. A row added here now
/// moves the column instead of breaking it.
///
/// Measured in terminal cells rather than `char`s, because that is what the
/// column is: `{key:<N}` pads by `char` count, which is a different number
/// for every wide glyph and happens to agree only for the ASCII rows.
#[must_use]
pub fn key_column_width() -> usize {
    BINDINGS
        .iter()
        .map(|b| UnicodeWidthStr::width(b.keys))
        .max()
        .unwrap_or(0)
}

/// The rows the hint row carries for `tab`: the tab's hinted rows, then the
/// deck-wide hinted ones.
pub fn hints(tab: DeckTab) -> impl Iterator<Item = &'static Binding> {
    in_scope(Tab(tab))
        .chain(in_scope(Everywhere))
        .filter(|b| b.hinted)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every source a keymap witness may live in.
    ///
    /// Source text, not the module tree — the same trade `stella-parity`'s
    /// capability matrix documents: a witness that moves to a file outside
    /// this list fails loudly (a false alarm, fixed by extending the list),
    /// never silently (the rotted proof this guard exists to catch).
    ///
    /// Most of it is `deck_ui/tests/`, where a key witness presses through
    /// `handle_deck_key`. The rest are the four places a row's vocabulary
    /// genuinely lives elsewhere: `deck_ui/list_nav.rs` (the tree keys are
    /// that helper), `deck_ui/dispatch.rs` (the `>` steer marker),
    /// `views/settings.rs` + `views/engine_panel/keys.rs` (the SETTINGS panes and
    /// their modal editor), `views/subagents.rs` (the overlay's own verbs), and
    /// `deck_shell.rs` (`⌃V`, claimed by the run loop above the pure key
    /// layer because the capture is blocking I/O).
    fn witness_sources() -> [&'static str; 29] {
        [
            include_str!("deck_ui/tests/agents.rs"),
            include_str!("deck_ui/tests/composer.rs"),
            include_str!("deck_ui/tests/esc.rs"),
            include_str!("deck_ui/tests/files.rs"),
            include_str!("deck_ui/tests/focus.rs"),
            include_str!("deck_ui/tests/gates.rs"),
            include_str!("deck_ui/tests/graph.rs"),
            include_str!("deck_ui/tests/agents_page.rs"),
            include_str!("deck_ui/tests/help.rs"),
            include_str!("deck_ui/tests/issues.rs"),
            include_str!("deck_ui/tests/list_vocabulary.rs"),
            include_str!("deck_ui/tests/mcp.rs"),
            include_str!("deck_ui/tests/queue.rs"),
            include_str!("deck_ui/tests/routing_card.rs"),
            include_str!("deck_ui/tests/selection.rs"),
            include_str!("deck_ui/tests/sessions.rs"),
            include_str!("deck_ui/tests/settings.rs"),
            include_str!("deck_ui/tests/skills.rs"),
            include_str!("deck_ui/tests/splash.rs"),
            include_str!("deck_ui/tests/tabs.rs"),
            include_str!("deck_ui/tests/traces.rs"),
            include_str!("deck_ui/tests/transcript_nav.rs"),
            include_str!("deck_ui/dispatch.rs"),
            include_str!("deck_ui/list_nav.rs"),
            include_str!("deck_shell.rs"),
            include_str!("views/engine_panel/keys.rs"),
            include_str!("views/agents_page.rs"),
            include_str!("views/subagents.rs"),
            include_str!("views/settings.rs"),
        ]
    }

    fn witness_exists(witness: &str) -> bool {
        let needle = format!("fn {witness}(");
        witness_sources()
            .iter()
            .any(|source| source.contains(&needle))
    }

    /// **The guard (#4368).** Every row names at least one test, and every
    /// name resolves to a function in the swept sources.
    ///
    /// There is no exemption list and no baseline: a row whose witness would
    /// have to be excused is a row whose witness has not been written yet.
    #[test]
    fn every_binding_names_a_witness_that_exists() {
        for b in BINDINGS {
            assert!(
                !b.witness.is_empty(),
                "{:?} `{}` ({}) names no witness — write the test that presses it",
                b.scope,
                b.keys,
                b.does
            );
            for witness in b.witness {
                assert!(
                    witness_exists(witness),
                    "{:?} `{}` ({}) names `{witness}`, which is not a test in the swept sources",
                    b.scope,
                    b.keys,
                    b.does
                );
            }
        }
    }

    /// A witness name that resolves nowhere would fail the guard above; this
    /// asserts the sweep is capable of saying no, so a green run means the
    /// names were found rather than that the search never looked.
    #[test]
    fn the_sweep_can_fail() {
        assert!(!witness_exists("no_such_witness_test_exists"));
        assert!(witness_exists("question_mark_opens_help_overlay"));
    }

    /// `PLAN_CARD_KEYS` is a row of this table, not a constant beside it.
    #[test]
    fn the_plan_card_chord_is_a_binding() {
        assert!(
            BINDINGS.iter().any(|b| b.keys == PLAN_CARD_KEYS),
            "{PLAN_CARD_KEYS} names no row — the surfaces would print a chord the deck does not bind"
        );
    }

    #[test]
    fn a_chord_with_no_caret_form_is_left_alone() {
        assert_eq!(caret_form("ctrl-s"), "⌃S");
        assert_eq!(caret_form("tab / ⇧tab"), "tab / ⇧tab");
        assert_eq!(caret_form("ctrl-x ×2"), "ctrl-x ×2");
    }

    /// **The guard (#4341).** SPEC 11 names the chord the deck actually binds
    /// to the plan panel.
    ///
    /// The spec said `tab` and the deck has said `⌃S` since the focus tree
    /// landed, so a reader who followed the document got the tab strip. The
    /// deck won that call — `tab`/`⇧tab` are deck-global navigation — and this
    /// is what stops the document drifting off it again.
    ///
    /// `design/` is outside ci.yml's prose paths (`scripts/ci-rust-scope.sh`),
    /// so a diff that edits the spec alone still runs the gate that reads it.
    #[test]
    fn spec_11_names_the_chord_the_deck_binds_to_the_plan_panel() {
        const SPEC: &str = include_str!("../../../design/tui-v2/SPEC.md");

        let table = SPEC
            .split("## 11. Keybindings")
            .nth(1)
            .expect("SPEC.md has no section 11")
            .split("\n## ")
            .next()
            .expect("split always yields one");

        let row = table
            .lines()
            .find(|l| l.contains("plan panel"))
            .expect("section 11 names no plan panel row");

        let key = row
            .split('|')
            .nth(1)
            .expect("a table row has cells")
            .trim()
            .trim_matches('`')
            // The spec spells a control chord `^S`; the chrome draws `⌃S`.
            .replace('^', "⌃");

        assert_eq!(
            key,
            plan_card_chord(),
            "SPEC 11 binds `{key}` to the plan panel and the deck binds {}",
            plan_card_chord()
        );
    }

    /// Every tab has at least one row, so no tab's sheet is empty.
    #[test]
    fn every_tab_has_rows() {
        for tab in DeckTab::ALL {
            assert!(in_scope(Tab(tab)).next().is_some(), "{tab:?} has no rows");
        }
    }

    /// A key that is navigation everywhere is not repeated per tab with a
    /// different meaning: the tab rows may refine `↑ ↓`, `← →` and `⏎`
    /// (what the list is, what opens) but never claim `⌫`, `q` or the tab
    /// keys, which mean one thing.
    #[test]
    fn the_tree_keys_are_not_rebound_per_tab() {
        for b in BINDINGS.iter().filter(|b| matches!(b.scope, Tab(_))) {
            for reserved in ["⌫", "tab / ⇧tab", "q / esc", "Home"] {
                assert!(
                    b.keys != reserved,
                    "{:?} rebinds {reserved}: {}",
                    b.scope,
                    b.does
                );
            }
        }
    }

    /// The hint row stays one line: a handful of hinted rows, never the sheet.
    #[test]
    fn the_hint_row_is_a_handful() {
        for tab in DeckTab::ALL {
            let n = hints(tab).count();
            assert!(n <= 6, "{tab:?} hints {n} rows — the hint row is one line");
        }
    }

    /// The rows the old sheet carried for a handler that no longer exists
    /// (plan review on SESSION) are gone, and the overlays every tab can
    /// reach are named with their real chords.
    #[test]
    fn the_sheet_names_real_keys_only() {
        let session: Vec<&str> = in_scope(Tab(T::Session)).map(|b| b.does).collect();
        assert!(
            !session.iter().any(|d| d.contains("plan review")),
            "plan review keys have no handler: {session:?}"
        );
        let everywhere: Vec<&str> = in_scope(Everywhere).map(|b| b.keys).collect();
        for chord in ["ctrl-a", "ctrl-e", "ctrl-k", "ctrl-g", "ctrl-t", "ctrl-s"] {
            assert!(
                everywhere.contains(&chord),
                "{chord} missing: {everywhere:?}"
            );
        }
    }
}
