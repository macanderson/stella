//! The `?` overlay: SPEC 11's help sheet and SPEC 5's metric detail view.
//!
//! The keys come from [`crate::keymap`] — this file renders the table and
//! owns none of its rows, so the sheet cannot drift from the hint row.
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
//! ENGINE — to **two** places: "behind `?` *and* the AGENTS tab". Only one of
//! the two holds them today. The `?` half arrived late (#4188 — until then
//! the overlay drew keybindings against a spec that called it "help, full
//! metric detail"), and the AGENTS half left with the executions dashboard
//! #4342 removed: `views::agents::render` now forwards to `views::installed`,
//! the installed-agents list, which draws no per-lane numbers and no per-row
//! model column. So `?` is the surviving surface for all five, `metric_rows`
//! below is where they render, and
//! `the_help_overlay_carries_the_metrics_spec_5_sends_behind_it`
//! (`tests/deck_render_snapshots.rs`) names each one beside the golden, so a
//! row that stops rendering fails an assertion instead of shifting a frame.
//!
//! MODEL detail has no second surface at all. The status bar names one slug —
//! whichever pin is answering — so with the AGENTS tab's per-row model column
//! gone, "which pin serves each role" is answered here or nowhere.

use super::*;
use crate::keymap::{self, Scope};

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
/// The numbers are read from the same `AgentEntry` fields the lane views read,
/// and `humanize_bytes` is `views::agents`'s own: two surfaces disagreeing
/// about one number is worse than one surface missing it, and that stays true
/// now that the AGENTS tab draws no numbers of its own (#4342).
fn metric_rows(model: &WorkspaceModel) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let now_ms = model.now_ms;

    // MODEL detail: which pin serves *each* role. This is the row the status
    // bar cannot carry — that names one slug, whichever pin is answering — and
    // no other surface carries it either, since the AGENTS tab's per-row model
    // column went with the executions dashboard (#4342).
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

/// The frame width at which the sheet lays its key sections side by side.
///
/// Below it one column of 84 is the whole panel; at or above it the panel
/// widens and splits, which is what puts SPEC 5's metric block on the first
/// screen of a 40-row terminal instead of a scroll away (#4371).
const TWO_COLUMN_MIN_W: u16 = 120;

/// The single column's width, and the cap the panel keeps below the
/// threshold — one sentence per key on a 100-column terminal.
const ONE_COLUMN_W: u16 = 84;

/// Blank cells between the two columns, so a long description on the left
/// does not run into the key on the right.
const GUTTER: usize = 3;

/// One column's rows, clipped and padded to exactly `width` cells.
///
/// Clipping takes the description, never the key: the key column is the
/// scannable one and is 16 cells at the start of every row, so truncating
/// from the right cannot reach it at any width this layout uses. A clipped
/// row ends in `…`, so a sentence that stops reads as cut rather than as
/// written that way.
fn fit(line: &Line<'static>, width: usize) -> Vec<Span<'static>> {
    let clipped = line.width() > width;
    let room = if clipped {
        width.saturating_sub(1)
    } else {
        width
    };
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for span in &line.spans {
        if used >= room {
            break;
        }
        let mut text = String::new();
        for ch in span.content.chars() {
            let w = UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + w > room {
                break;
            }
            used += w;
            text.push(ch);
        }
        if !text.is_empty() {
            spans.push(Span::styled(text, span.style));
        }
    }
    if clipped {
        spans.push(Span::styled("…", theme::muted()));
        used += 1;
    }
    if used < width {
        spans.push(Span::raw(" ".repeat(width - used)));
    }
    spans
}

/// Lay `left` beside `right`, each column `col` cells wide.
///
/// The taller column decides the row count; the shorter one pads out, so the
/// right column's rows stay in the same place whichever side runs out first.
fn side_by_side(left: &[Line<'static>], right: &[Line<'static>], col: usize) -> Vec<Line<'static>> {
    let blank = Line::default();
    (0..left.len().max(right.len()))
        .map(|i| {
            let mut spans = fit(left.get(i).unwrap_or(&blank), col);
            spans.push(Span::raw(" ".repeat(GUTTER)));
            spans.extend(fit(right.get(i).unwrap_or(&blank), col));
            Line::from(spans)
        })
        .collect()
}

/// The help overlay: the active tab's keys first, then the deck-wide keys —
/// one shortcut per line, key column aligned. Context-aware on purpose: only
/// shortcuts that work on the tab the user is looking at are shown, so the
/// overlay stays short enough to read at a glance. Opened by `?` (empty
/// composer) or `/help`; scrolls with ↑/↓/⇞/⇟/Home/End on a short terminal;
/// closes with esc/`q`/`?`.
pub(super) fn render_help(model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    // Three sections, all read from `crate::keymap`: the active tab's own
    // keys, the focus tree that is the same on every tab, then the
    // deck-wide chords. Context-aware on purpose — only keys that work
    // where the reader is, so the sheet stays short enough to read at a
    // glance.
    let section = |heading: String, scope: Scope| {
        let mut out = vec![
            Line::default(),
            Line::from(Span::styled(heading, theme::heading())),
        ];
        out.extend(keymap::in_scope(scope).map(|b| help_row(b.keys, b.does)));
        out
    };
    let mut here = section(format!("  {} tab", ui.tab.title()), Scope::Tab(ui.tab));
    here.extend(section(
        "  moving around — every tab, every overlay".to_string(),
        Scope::Navigation,
    ));
    let everywhere = section("  everywhere".to_string(), Scope::Everywhere);

    // Wide frames get the panel's full width and two columns; the metric
    // block below still spans both. On a narrow frame the sections stack and
    // the block's own `↓ N more` edge says the numbers are there.
    let wide = area.width >= TWO_COLUMN_MIN_W;
    let w = if wide {
        area.width.saturating_sub(4)
    } else {
        area.width.saturating_sub(4).min(ONE_COLUMN_W)
    };
    let mut lines: Vec<Line<'static>> = if wide {
        // Two borders out of `w`, then the gutter, split evenly.
        let col = (w as usize).saturating_sub(2 + GUTTER) / 2;
        side_by_side(&here, &everywhere, col)
    } else {
        here.into_iter().chain(everywhere).collect()
    };
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
    let h = area.height.min(lines.len() as u16 + 2);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    Clear.render(popup, buf);
    let total = lines.len();
    let height = popup.height.saturating_sub(2) as usize;
    // Record viewport metrics for the pure key handler (`handle_help_key`) —
    // when the panel is clipped, ↑/↓/⇞/⇟/Home/End scroll it.
    ui.metrics.help_total = total;
    ui.metrics.help_height = height;
    let window = ui.help_scroll.window(total, height);
    // A clipped sheet says so on its bottom edge: on a short terminal the
    // metric block is below the keys, and a reader who cannot see that it
    // exists will not scroll to it.
    let below = total.saturating_sub(window.end);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::accent())
        .title(format!(" help — {} · esc close ", ui.tab.title()));
    if below > 0 {
        block = block.title_bottom(
            Line::from(Span::styled(
                format!(" ↓ {below} more · ⇟ space · End "),
                theme::muted(),
            ))
            .right_aligned(),
        );
    }
    let inner = block.inner(popup);
    block.render(popup, buf);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    Paragraph::new(lines)
        .scroll((window.start as u16, 0))
        .render(inner, buf);
}
