// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The deck's keymap — one table, read by every surface that tells a person
//! what a key does.
//!
//! The `?` sheet (`deck_render::help`), the hint row under the composer
//! (`v2::frame::render_hint_row`) and the README's key list used to be three
//! hand-written copies of the same facts, and the sheet had already drifted:
//! it advertised plan-review keys (`a`/`t`/`x`/`r`) whose handler had been
//! deleted, and never mentioned `ctrl-a`. A key a person cannot find is a
//! key that does not exist, so the copies collapse into this table and the
//! surfaces render it.
//!
//! What this table is **not**: the dispatcher. `deck_ui::handle_deck_key` is
//! a precedence chain (the crate README says why), and a binding here is a
//! claim about it that the witness tests under `deck_ui/tests/` establish —
//! `focus.rs` for the navigation rows, each tab's own file for its rows. A
//! row with no witness is the drift this table exists to stop, so add the
//! test with the row.

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
}

const fn row(keys: &'static str, does: &'static str, scope: Scope) -> Binding {
    Binding {
        keys,
        does,
        scope,
        hinted: false,
    }
}

const fn hinted(keys: &'static str, does: &'static str, scope: Scope) -> Binding {
    Binding {
        keys,
        does,
        scope,
        hinted: true,
    }
}

use DeckTab as T;
use Scope::{Everywhere, Navigation, Tab};

/// Every binding the deck teaches, in the order the sheet prints them
/// within each scope.
pub const BINDINGS: &[Binding] = &[
    // ── the focus tree ──────────────────────────────────────────────────
    row(
        "← →",
        "the sibling: the pane beside this one, else the tab beside it",
        Navigation,
    ),
    row("↑ ↓ · k j", "the item above / below", Navigation),
    row(
        "⏎",
        "open it — a diff, a lane, a message's full text",
        Navigation,
    ),
    row(
        "⌫",
        "back up one level; never reaches a running turn",
        Navigation,
    ),
    row("⇞ ⇟ · Home End", "a page · the ends", Navigation),
    row("tab / ⇧tab", "the next / previous tab", Navigation),
    row("q / esc", "close any overlay", Navigation),
    // ── SESSION ─────────────────────────────────────────────────────────
    row(
        "↑",
        "walk the transcript — with prompts queued, the queue editor first",
        Tab(T::Session),
    ),
    row(
        "↓",
        "the SUB-AGENTS overlay — every lane and delegate, with controls",
        Tab(T::Session),
    ),
    row("⇞ ⇟", "scroll the transcript", Tab(T::Session)),
    row(
        "⌘[ / ⌘]",
        "jump to transcript start / end (⌃ works too)",
        Tab(T::Session),
    ),
    hinted(
        "ctrl-n / ctrl-p",
        "jump to the next / previous failure",
        Tab(T::Session),
    ),
    hinted(
        "ctrl-z",
        "fold the highlighted turn to one line (none: all)",
        Tab(T::Session),
    ),
    hinted(
        "ctrl-o",
        "expand / collapse the highlighted message (none: all)",
        Tab(T::Session),
    ),
    row("ctrl-r", "expand / collapse all thinking", Tab(T::Session)),
    row(
        "ctrl-f",
        "find in the transcript — ⏎ next · ctrl-p previous",
        Tab(T::Session),
    ),
    row(
        "⌫ / esc",
        "at an opened lane: back to the agent that dispatched it",
        Tab(T::Session),
    ),
    // ── AGENTS ──────────────────────────────────────────────────────────
    row(
        "⏎",
        "edit the definition — a save is a new pinned version",
        Tab(T::Agents),
    ),
    row(
        "a",
        "assume its identity: the lead runs as this agent",
        Tab(T::Agents),
    ),
    row("n", "new agent — drafted by the LLM", Tab(T::Agents)),
    row("x x", "delete the agent, every version", Tab(T::Agents)),
    row("v / r", "versions · reload", Tab(T::Agents)),
    // ── TRACES ──────────────────────────────────────────────────────────
    row("f", "cycle the per-agent filter", Tab(T::Traces)),
    // ── GRAPH ───────────────────────────────────────────────────────────
    row("↑ ↓", "walk the neighborhood", Tab(T::Graph)),
    row(
        "/ or ⏎",
        "file picker — re-root on any indexed file",
        Tab(T::Graph),
    ),
    // ── FILES ───────────────────────────────────────────────────────────
    row("⏎", "open / close the diff", Tab(T::Files)),
    // ── SKILLS ──────────────────────────────────────────────────────────
    row(
        "← →",
        "installed ↔ registry search (→ past the search leaves the tab)",
        Tab(T::Skills),
    ),
    row("space", "enable / disable", Tab(T::Skills)),
    row("e", "edit the selected skill", Tab(T::Skills)),
    row("p", "pin / unpin", Tab(T::Skills)),
    row("n", "new skill — drafted by the LLM", Tab(T::Skills)),
    row("ctrl-o", "preview", Tab(T::Skills)),
    row(
        "ctrl-x ×2",
        "delete (press twice to confirm)",
        Tab(T::Skills),
    ),
    row("type", "search skills", Tab(T::Skills)),
    // ── MCP ─────────────────────────────────────────────────────────────
    row("space / e", "enable / disable", Tab(T::Mcp)),
    row("ctrl-o", "inspect the server", Tab(T::Mcp)),
    row("a", "authenticate (env credentials)", Tab(T::Mcp)),
    row("o", "OAuth login (http servers)", Tab(T::Mcp)),
    row("s", "search the registry", Tab(T::Mcp)),
    row("x", "remove the server", Tab(T::Mcp)),
    row("r", "refresh", Tab(T::Mcp)),
    // ── ISSUES ──────────────────────────────────────────────────────────
    row("space", "select several", Tab(T::Issues)),
    row("r", "refresh the list", Tab(T::Issues)),
    row("/", "search the tracker", Tab(T::Issues)),
    row("] [", "next / previous page", Tab(T::Issues)),
    row(
        "o / p",
        "open in the browser · push the pick to the prompt",
        Tab(T::Issues),
    ),
    row(
        "n",
        "new issue — tab cycles fields · ctrl-s creates",
        Tab(T::Issues),
    ),
    row("c", "comment on the selected issue", Tab(T::Issues)),
    row("s", "set the selected issue's status", Tab(T::Issues)),
    row("w", "start work on the selected issue", Tab(T::Issues)),
    // ── SETTINGS ────────────────────────────────────────────────────────
    row(
        "← →",
        "agents ↔ tools ↔ seats (past the ends leaves the tab)",
        Tab(T::Settings),
    ),
    row("e", "edit the pane you are on", Tab(T::Settings)),
    row("t", "jump to the tool switches", Tab(T::Settings)),
    row(
        "tab",
        "in the editor: switch agent — global / default / worker / …",
        Tab(T::Settings),
    ),
    row(
        "⏎",
        "in the editor: edit the selected row / pick a model",
        Tab(T::Settings),
    ),
    row(
        "space",
        "in the editor: toggle the selected row",
        Tab(T::Settings),
    ),
    row(
        "x",
        "in the editor: clear the selected row",
        Tab(T::Settings),
    ),
    row(
        "s / S",
        "in the editor: save to user / project settings",
        Tab(T::Settings),
    ),
    row("r", "in the editor: reload from disk", Tab(T::Settings)),
    row(
        "esc",
        "in the editor: hand the keyboard back to the tab",
        Tab(T::Settings),
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
    ),
    row(
        "⌘⏎ / ⌃⏎ / ⌥⏎",
        "insert a line break in the prompt",
        Everywhere,
    ),
    hinted(
        "!cmd",
        "run a shell command NOW (skips the queue)",
        Everywhere,
    ),
    hinted(
        "/",
        "slash commands — ↑↓ pick · tab completes · ⏎ runs",
        Everywhere,
    ),
    row(
        "ctrl-v",
        "paste — a copied image is attached to the prompt",
        Everywhere,
    ),
    row(
        "ctrl-a",
        "SUB-AGENTS — n nudge · f flag · ^x^x kill · p pause · r restart",
        Everywhere,
    ),
    row("ctrl-t", "the queue editor", Everywhere),
    row(
        "ctrl-e",
        "SESSIONS — every session on this machine",
        Everywhere,
    ),
    row(
        "ctrl-k",
        "CONTEXT — active skills + MCP servers",
        Everywhere,
    ),
    row(
        "ctrl-g",
        "INSPECT — the context sent on any recorded call",
        Everywhere,
    ),
    row(
        "ctrl-s",
        "PLAN — every step of the approved plan, in full",
        Everywhere,
    ),
    row(
        ">text",
        "steer the running turn — lands at the next step boundary",
        Everywhere,
    ),
    row(
        "esc",
        "steer: your draft + queue go INTO the running turn",
        Everywhere,
    ),
    row(
        "esc esc",
        "cancel NOW & hold — nothing runs until your next prompt",
        Everywhere,
    ),
    row("?", "this sheet", Everywhere),
    row("ctrl-c", "quit stella", Everywhere),
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
