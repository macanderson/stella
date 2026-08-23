//! The `?` overlay: SPEC 11's help sheet and SPEC 5's metric detail view.
//!
//! Split out of `deck_render.rs` because that file is a grandfathered god file
//! and closed to growth (AGENTS.md, "God files — plan around them, never into
//! them"). It sat at 1514 lines against a 1518 ceiling — four lines of
//! headroom, where the metric block below is forty. The move is the enabling
//! step for #4188, not incidental to it: there was no way to add these rows in
//! place.
//!
//! The cut follows the concern. Everything here answers "what does `?` draw",
//! and nothing else in `deck_render` reads any of it.
//!
//! ## Why the overlay carries numbers as well as keys
//!
//! SPEC 5 re-homes five status cells — MODEL detail, CPU, MEM, WARMTH and
//! ENGINE — to **two** places: "behind `?` *and* the AGENTS tab". The AGENTS
//! half shipped in `views::agents`; this is the other half, which did not, so
//! `?` was a keybinding sheet against a spec that called it "help, full metric
//! detail" (#4188).
//!
//! It matters most for **MODEL detail**. The status bar names one slug —
//! whichever pin is answering — and "which pin serves each role" had no
//! surface left at all: the AGENTS tab's per-row model column answers a
//! different question (which model is this *lane* on), not which model each
//! *role* is pinned to.

use super::*;

/// A pipeline role as this overlay names it.
///
/// The statline spends one cell per role and so uses
/// [`crate::deck::PipelineRole::initial`] (`T`/`W`/`J`). An overlay has the
/// width to say the word, and the whole point of this block is that it is the
/// detail view the one-letter cells send you to.
fn role_word(role: crate::deck::PipelineRole) -> &'static str {
    use crate::deck::PipelineRole as R;
    match role {
        R::Triage => "triage",
        R::Worker => "worker",
        R::Verifier => "verifier",
    }
}

/// SPEC 5's five re-homed cells, as `?` renders them.
///
/// # Every row here has a measured source, or it is absent
///
/// Nothing below substitutes a zero for an absent reading. `0%` CPU is a real
/// and different claim from "no sample has been taken", and a cache warmth of
/// `0s` says the prompt cache has just expired rather than that nothing has
/// been cached — the distinction `cache_panel::fmt_warmth` and
/// `AgentEntry::cache_warmth_secs` already draw with an `Option`, and the one
/// #2290 and #4150 each cost a real defect to learn. A value with no source
/// renders no row at all.
///
/// The numbers are the AGENTS tab's own, read the same way from the same
/// fields (`views::agents`), because SPEC 5 sends these to two surfaces and two
/// surfaces disagreeing about one number is worse than one surface missing it.
fn metric_rows(model: &WorkspaceModel) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let now_ms = model.now_ms;

    // MODEL detail: which pin serves *each* role. This is the row the status
    // bar cannot carry — that names one slug, whichever pin is answering, and
    // the AGENTS tab's per-row model column answers "which model is this lane
    // on", a different question.
    if !model.role_pins.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled("  model", theme::heading())));
        for role in crate::deck::PipelineRole::ORDER {
            let Some(pin) = model.role_pins.get(&role) else {
                continue;
            };
            let active = if model.active_role == Some(role) {
                " · answering"
            } else {
                ""
            };
            lines.push(help_row(
                role_word(role),
                &format!("{}{active}", pin.slug()),
            ));
        }
    }

    // ENGINE, CPU, MEM and WARMTH describe the machine and the focused lane.
    let mut machine: Vec<Line<'static>> = Vec::new();
    let total = model.agents.len();
    if total > 0 {
        let active = model.agents.iter().filter(|a| a.status.is_active()).count();
        machine.push(help_row(
            "engine",
            &format!("{active} active · {total} total"),
        ));
    }
    if let Some(entry) = model.agents.first() {
        machine.push(help_row("cpu", &format!("{:.0}%", entry.res.cpu_pct)));
        machine.push(help_row(
            "mem",
            &crate::views::agents::humanize_bytes(entry.res.mem_bytes),
        ));
        // Absent, not zero: no cached prefix at all and a prefix whose warmth
        // has just run out are different facts about the next call's price.
        if let Some(secs) = entry.cache_warmth_secs(now_ms) {
            machine.push(help_row("warmth", &cache_panel::fmt_warmth(Some(secs))));
        }
    }
    if !machine.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled("  machine", theme::heading())));
        lines.extend(machine);
    }

    lines
}

/// One aligned `key → description` row of the help overlay. The key column is
/// padded to a fixed width so the descriptions line up into a scannable
/// second column.
fn help_row(key: &str, desc: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {key:<13} "), theme::accent()),
        Span::styled(desc.to_string(), theme::body()),
    ])
}

/// The shortcuts specific to one deck tab, as `(key, description)` pairs.
/// Keyed off [`DeckTab`] so the overlay only ever shows keys that work where
/// the user actually is — the per-tab handlers in `deck_ui` are the behavior
/// these rows must mirror.
fn tab_shortcuts(tab: DeckTab) -> &'static [(&'static str, &'static str)] {
    match tab {
        DeckTab::Session => &[
            ("↑ ↓", "select a message · esc clears the selection"),
            ("⇞ ⇟", "scroll the transcript"),
            ("⌘[ / ⌘]", "jump to transcript start / end (⌃ works too)"),
            ("ctrl-o", "expand/collapse the selected message (none: all)"),
            ("ctrl-r", "expand/collapse all thinking"),
            (
                "ctrl-f",
                "find in the transcript — ⏎ next · ctrl-p previous",
            ),
            ("ctrl-n / ctrl-p", "jump to the next / previous failure"),
            ("ctrl-z", "fold the selected turn to one line (none: all)"),
            ("↑", "with prompts queued: open the queue editor"),
            ("←", "SESSIONS overlay — every session on this machine"),
            ("→", "CONTEXT overlay — active skills + MCP servers"),
            ("ctrl-g", "INSPECT — the context sent on any recorded call"),
            (
                "a / t / x",
                "plan review: approve · trim · abort — one keypress decides",
            ),
            (
                "r",
                "plan review: refine — type what to change, ⏎ re-plans from it",
            ),
            (
                "esc",
                "plan review: abort (from the refine input: back out)",
            ),
        ],
        DeckTab::Agents => &[
            ("↑ ↓", "select an installed agent"),
            ("⏎", "edit the definition — a save is a new pinned version"),
            ("a", "assume its identity: the lead runs as this agent"),
            ("n", "new agent — drafted by the LLM"),
            ("x x", "delete the agent, every version"),
            ("v / r", "versions · reload"),
        ],
        DeckTab::Traces => &[
            ("↑ ↓ ⇞ ⇟", "scroll the event log"),
            ("f", "cycle the per-agent filter"),
        ],
        DeckTab::Graph => &[
            ("← → ↑ ↓", "walk the neighborhood"),
            ("/ or ⏎", "file picker — re-root on any indexed file"),
        ],
        DeckTab::Files => &[("↑ ↓", "select a file"), ("⏎", "open / close the diff")],
        DeckTab::Skills => &[
            ("← →", "switch panes"),
            ("↑ ↓", "select a skill"),
            ("space", "enable / disable"),
            ("e", "edit the selected skill"),
            ("p", "pin / unpin"),
            ("n", "new skill — drafted by the LLM"),
            ("ctrl-o", "preview"),
            ("ctrl-x ×2", "delete (press twice to confirm)"),
            ("type", "search skills"),
        ],
        DeckTab::Mcp => &[
            ("↑ ↓", "select a server"),
            ("space / e", "enable / disable"),
            ("a", "authenticate (env credentials)"),
            ("o", "OAuth login (http servers)"),
            ("s", "search the registry"),
            ("x", "remove the server"),
            ("r", "refresh"),
        ],
        DeckTab::Issues => &[
            ("↑ ↓", "select an issue"),
            ("r", "refresh the list"),
            ("/", "search the tracker"),
            ("n", "new issue — tab cycles fields · ctrl-s creates"),
            ("c", "comment on the selected issue"),
            ("s", "set the selected issue's status"),
            ("w", "start work on the selected issue"),
        ],
        DeckTab::Settings => &[
            ("← →", "switch panes — agents / tools"),
            ("e", "edit the pane you are on"),
            ("t", "jump to the tool switches"),
            (
                "tab",
                "in the editor: switch agent — global / default / worker / …",
            ),
            ("⏎", "in the editor: edit the selected row / pick a model"),
            ("space", "in the editor: toggle the selected row"),
            ("x", "in the editor: clear the selected row"),
            ("s / S", "in the editor: save to user / project settings"),
            ("r", "in the editor: reload from disk"),
            ("esc", "in the editor: hand the keyboard back to the tab"),
        ],
    }
}

/// Deck-wide shortcuts that work on every tab.
const GLOBAL_SHORTCUTS: &[(&str, &str)] = &[
    ("tab / ⇧tab", "switch tabs"),
    // Enter semantics are the inverse of the old chord-to-submit mapping (see
    // `composer::classify_enter`): a bare ⏎ dispatches, a *modified* ⏎ breaks
    // the line. The composer footer advertises the same pair — these rows must
    // not drift from it.
    // The parenthetical is load-bearing: the unqualified promise ("runs as its
    // own agent") is what a reviewer read while a scope card was waiting, and
    // the sidecar it describes is exactly what swallowed their answer.
    (
        "⏎",
        "queue the prompt — mid-turn it runs as its own agent (unless a card waits)",
    ),
    ("⌘⏎ / ⌃⏎ / ⌥⏎", "insert a line break in the prompt"),
    ("!cmd", "run a shell command NOW (skips the queue)"),
    ("/", "slash commands — ↑↓ pick · tab completes · ⏎ runs"),
    ("ctrl-v", "paste — a copied image is attached to the prompt"),
    ("ctrl-t", "open the queue editor"),
    ("ctrl-s", "PLAN — every step of the approved plan, in full"),
    (
        ">text",
        "steer the running turn — lands at the next step boundary",
    ),
    ("esc", "steer: your draft + queue go INTO the running turn"),
    (
        "esc esc",
        "cancel NOW & hold — nothing runs until your next prompt",
    ),
    ("ctrl-c", "quit stella"),
];

/// The help overlay: the active tab's keys first, then the deck-wide keys —
/// one shortcut per line, key column aligned. Context-aware on purpose: only
/// shortcuts that work on the tab the user is looking at are shown, so the
/// overlay stays short enough to read at a glance. Opened by `?` (empty
/// composer) or `/help`; scrolls with ↑/↓/⇞/⇟/Home/End on a short terminal;
/// closes with esc/`q`/`?`.
pub(super) fn render_help(model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        format!("  {} tab", ui.tab.title()),
        theme::heading(),
    )));
    for (key, desc) in tab_shortcuts(ui.tab) {
        lines.push(help_row(key, desc));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled("  everywhere", theme::heading())));
    for (key, desc) in GLOBAL_SHORTCUTS {
        lines.push(help_row(key, desc));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "  letter & arrow hotkeys apply while the prompt box is empty",
        theme::muted(),
    )));
    // The metric detail last, under the keys. `?` is reached for as a key sheet
    // far more often than as a dashboard, so the keys keep the top of a panel
    // that has to be scrolled on a short terminal; SPEC 5 asks for the numbers
    // to be *here*, not to be first.
    lines.extend(metric_rows(model));

    // Size the panel to its content, capped to the frame.
    let w = area.width.min(68);
    let h = area.height.min(lines.len() as u16 + 2);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    Clear.render(popup, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::accent())
        .title(format!(" help — {} · esc close ", ui.tab.title()));
    let inner = block.inner(popup);
    block.render(popup, buf);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let total = lines.len();
    let height = inner.height as usize;
    // Record viewport metrics for the pure key handler (`handle_help_key`) —
    // when the panel is clipped, ↑/↓/⇞/⇟/Home/End scroll it.
    ui.metrics.help_total = total;
    ui.metrics.help_height = height;
    let window = ui.help_scroll.window(total, height);
    Paragraph::new(lines)
        .scroll((window.start as u16, 0))
        .render(inner, buf);
}
