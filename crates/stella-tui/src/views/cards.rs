//! Shared chrome for the floating cards (`/plan` · `/models` · `/budget`):
//! one `BorderType::Rounded` block in `token::BORDER`, floated above the
//! composer, max-width ~56 cells — the same border every other overlay
//! draws (`crate::views::approval`, `views::picker`, `views::queue`, …), so these
//! three cards are not a second chrome family. Title row = gold name +
//! dimmed context; the key hints ride the BOTTOM border, right-aligned, the
//! way every hand-rolled overlay already places them. Implemented once so
//! the three cards read as one system, and so the selection convention — a
//! `▸` marker glyph **plus** the background tint — is structural: the golden
//! suite strips style, so a style-only selection would be invisible to it.

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget};
use stella_tui_theme::token;

/// Widest card, in columns (matches the panic boundary's error card).
pub(crate) const CARD_MAX_W: u16 = 56;

/// Rows of deck chrome under the content area the card floats above
/// (trace strip 2 + progress 1 + composer 1 + footer 1 + statline 1). The
/// statline is really 2 rows (3 with a cache diagnosis), so this is one short
/// and a card's bottom border lands on the trace strip — as the goldens pin it.
const CHROME_BELOW: u16 = 6;

/// Where a card floats: horizontally centered, bottom-anchored just above
/// the composer chrome, tall enough for `body_rows` + the two border rows,
/// clamped to the frame on small terminals. Never a zero-size rect on a
/// nonzero frame — the guard's scratch discipline handles the rest.
///
/// In accessible mode (`full_width`) the card spans the frame instead: the
/// float is a visual affordance, and a labeled record clipped at a float's
/// right border is a record a reader never hears the end of. `max_w` is the
/// card's own width cap — [`CARD_MAX_W`] for most; the `/models` card runs
/// wider because a model slug and the key that chose it may not elide.
pub(crate) fn card_area(frame: Rect, body_rows: u16, max_w: u16, full_width: bool) -> Rect {
    let w = if full_width {
        frame.width
    } else {
        frame.width.min(max_w)
    };
    let h = (body_rows + 2).min(frame.height);
    let bottom_gap = CHROME_BELOW.min(frame.height.saturating_sub(h));
    Rect {
        x: frame.x + (frame.width.saturating_sub(w)) / 2,
        y: frame.y + frame.height.saturating_sub(h + bottom_gap),
        width: w,
        height: h,
    }
}

/// Draw the card's frame and title row, returning the inner body rect.
///
/// The title row rides the top border: `name` in the accent, the `context`
/// spans (dim by convention; the task card's fraction bar keeps its own
/// success color) beside it. `hints` rides the BOTTOM border, right-aligned —
/// `crate::views::approval`, `views::picker`, `views::queue` and every other
/// hand-rolled overlay already draws hints there rather than on the top
/// title; this shared helper used to be the one holdout, so `/plan`,
/// `/models` and `/budget` (its only callers) read as a different chrome
/// family from the rest of the deck's floating cards. Everything meaningful
/// in it is still a glyph/word (style-blind goldens can pin it).
pub(crate) fn card_frame(
    area: Rect,
    name: &str,
    context: Vec<Span<'static>>,
    hints: &str,
    buf: &mut Buffer,
) -> Rect {
    Clear.render(area, buf);
    let mut title = vec![Span::styled(
        format!(" {name} "),
        Style::new().fg(token::GOLD).add_modifier(Modifier::BOLD),
    )];
    if !context.is_empty() {
        title.push(Span::raw(" "));
        title.extend(context);
        title.push(Span::raw(" "));
    }
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(token::BORDER))
        .title(Line::from(title));
    if !hints.is_empty() {
        block = block.title_bottom(
            Line::from(Span::styled(
                format!(" {hints} "),
                Style::new().fg(token::DIM),
            ))
            .alignment(Alignment::Right),
        );
    }
    let inner = block.inner(area);
    block.render(area, buf);
    inner
}

/// The selection affordance every card row uses: the mandatory `▸ ` marker
/// (unselected rows get two spaces so columns hold), and the row tint via
/// [`token::HL`] layered by the caller. Returned as a span so the caller
/// controls the rest of the row.
pub(crate) fn marker(selected: bool) -> Span<'static> {
    if selected {
        Span::styled("▸ ", Style::new().fg(token::GOLD))
    } else {
        Span::raw("  ")
    }
}

/// Paint `lines` into `inner`, tinting the rows in `selected_rows` with the
/// selection background (the style half of the marker+tint convention).
///
/// More rows than `inner` is tall are **folded, and the fold is admitted** —
/// never silently dropped off the bottom. [`card_area`] caps a card's height
/// at the frame, so any card can be handed more rows than it can draw; a
/// twenty-step plan on a short terminal used to render its first few steps
/// and say nothing about the rest (#4776). The window follows the selection,
/// which is the same defect seen from the other side: `plan_sel` could point
/// at a step that was never drawn, and `x skip` then acted on a row the
/// reader could not see.
pub(crate) fn render_body(
    lines: Vec<Line<'static>>,
    selected_row: Option<usize>,
    inner: Rect,
    buf: &mut Buffer,
) {
    let mut lines = lines;
    if let Some(sel) = selected_row
        && let Some(line) = lines.get_mut(sel)
    {
        line.style = line.style.bg(token::HL);
    }
    let height = inner.height as usize;
    if height > 0 && lines.len() > height {
        lines = fold_to(lines, selected_row, height);
    }
    Paragraph::new(lines).render(inner, buf);
}

/// One row of a fold admission, dim so it reads as chrome rather than content.
fn fold_row(text: String) -> Line<'static> {
    Line::from(Span::styled(text, Style::new().fg(token::MUTED)))
}

/// Window `lines` down to exactly `height` rows around `selected`, spending a
/// row at each cut end to say how much was cut there.
///
/// The admission costs a row, which is why the count is taken *after* the
/// window is chosen: the marker replaces a content row, so that row is hidden
/// too and has to be counted among the hidden. Getting this backwards yields
/// an off-by-one that only shows up at the exact boundary, which is the one
/// place nobody looks.
fn fold_to(
    lines: Vec<Line<'static>>,
    selected: Option<usize>,
    height: usize,
) -> Vec<Line<'static>> {
    let total = lines.len();
    // One row cannot hold both a row and the admission that it is hiding
    // others. The admission wins: a single arbitrary row with nothing saying
    // it is arbitrary is the failure this whole function is about.
    if height == 1 {
        return vec![fold_row(format!("… {total} rows, none fit"))];
    }

    let sel = selected.unwrap_or(0).min(total.saturating_sub(1));
    // Centred on the selection, clamped to the ends so the last page is full
    // rather than trailing blank rows.
    let mut offset = sel.saturating_sub(height / 2).min(total - height);

    // Whether an end is cut decides whether that row is a marker, which
    // decides where content can sit, which decides the offset — so the three
    // are settled together rather than in one pass. A single nudge is not
    // enough and gets this exactly wrong at the end of a long list: moving
    // the window down to free the bottom marker can newly cut the top, and
    // the selection lands on the marker it was moved away from.
    //
    // Each step moves `offset` strictly toward the selection, so it settles.
    // The bound is there in case a future edit breaks that.
    for _ in 0..3 {
        let lo = offset + usize::from(offset > 0);
        let hi = offset + height - usize::from(offset + height < total);
        if lo >= hi {
            // No room for content at all — the collapse below owns this.
            break;
        }
        if sel < lo {
            offset -= lo - sel;
        } else if sel >= hi {
            offset = (offset + (sel - hi + 1)).min(total - height);
        } else {
            break;
        }
    }

    let cut_above = offset > 0;
    let cut_below = total > offset + height;
    // Two admissions in a two-row window leave nowhere for the row the reader
    // is looking at, so the window would be nothing but chrome. Below that
    // floor the two collapse into one count and the selection keeps its row:
    // the reader must always be able to see the thing they selected.
    if height <= usize::from(cut_above) + usize::from(cut_below) {
        return vec![
            lines[sel].clone(),
            fold_row(format!("… {} of {total} hidden", total - 1)),
        ];
    }

    let mut out: Vec<Line<'static>> = lines[offset..offset + height].to_vec();
    if cut_above {
        out[0] = fold_row(format!("↑ {} more above", offset + 1));
    }
    if cut_below {
        let below = total - (offset + height);
        out[height - 1] = fold_row(format!("↓ {} more below", below + 1));
    }
    out
}

/// A mini fraction bar over `width` cells: `filled/total` in the given
/// color, the rest a dim groove. Deliberately painted with `▰`/`▱` — never
/// the `█` glyph, which belongs exclusively to the brand-gradient run bar so
/// the brand-fill witness (`tests/progress_brand_fill.rs`) stays unambiguous.
pub(crate) fn mini_fraction_bar(
    done: usize,
    total: usize,
    width: usize,
    color: ratatui::style::Color,
) -> Vec<Span<'static>> {
    if total == 0 || width == 0 {
        return Vec::new();
    }
    let filled = (done * width).div_ceil(total).min(width);
    vec![
        Span::styled("▰".repeat(filled), Style::new().fg(color)),
        Span::styled("▱".repeat(width - filled), Style::new().fg(token::MUTED)),
    ]
}

/// Format a millisecond span as `m:ss` (the clock on each lane in the
/// SUB-AGENTS overlay — `crate::views::subagents`).
pub(crate) fn fmt_mss(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Push `right` right-aligned onto `row` given the card's inner width —
/// column arithmetic on display width, grapheme-safe because it only ever
/// pads (never slices).
pub(crate) fn pad_right(
    mut row: Vec<Span<'static>>,
    right: Span<'static>,
    inner_w: usize,
) -> Vec<Span<'static>> {
    let used: usize = row.iter().map(|s| span_w(s)).sum();
    let rw = span_w(&right);
    if used + rw < inner_w {
        row.push(Span::raw(" ".repeat(inner_w - used - rw)));
        row.push(right);
    } else if rw < inner_w {
        row.push(right);
    }
    row
}

/// Display width of one span, unicode-width aware.
fn span_w(span: &Span<'_>) -> usize {
    unicode_width::UnicodeWidthStr::width(span.content.as_ref())
}

/// Truncate `text` to at most `max_cols` display columns with an ellipsis —
/// grapheme-safe (never a byte slice).
pub(crate) fn truncate_cols(text: &str, max_cols: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let width: usize = text.chars().map(|c| c.width().unwrap_or(0)).sum();
    if width <= max_cols {
        return text.to_string();
    }
    let budget = max_cols.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(n: usize) -> Vec<Line<'static>> {
        (0..n).map(|i| Line::from(format!("step {i}"))).collect()
    }

    fn texts(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    /// The property `plan_rail.rs::an_overlong_plan_folds_its_tail_and_admits_it`
    /// carried until #4686 deleted it: too many rows fold, and the fold says
    /// so. It came back as #4776, on the card that replaced the rail.
    #[test]
    fn an_overlong_body_folds_its_tail_and_admits_it() {
        let out = fold_to(rows(20), None, 6);
        assert_eq!(out.len(), 6, "the fold fits the height it was given");
        let text = texts(&out);
        assert!(
            text.last().is_some_and(|row| row.contains("more below")),
            "the tail is admitted rather than dropped: {text:?}"
        );
    }

    /// The count includes the row the marker itself displaced. Twenty rows in
    /// a window of six shows rows 0..=4 and spends row 5 on the admission, so
    /// fifteen are hidden — not fourteen.
    #[test]
    fn the_fold_counts_the_row_its_own_marker_displaced() {
        let text = texts(&fold_to(rows(20), None, 6));
        assert_eq!(text[4], "step 4", "five content rows precede the marker");
        assert_eq!(text[5], "↓ 15 more below", "{text:?}");
    }

    /// The other half of #4776: a selection past the fold used to be styled
    /// on a row that was never drawn, so `x skip` acted on something invisible.
    #[test]
    fn the_window_follows_a_selection_past_the_fold() {
        let text = texts(&fold_to(rows(40), Some(30), 7));
        assert!(
            text.iter().any(|row| row == "step 30"),
            "the selected row is inside the window: {text:?}"
        );
        assert!(text[0].contains("more above"), "{text:?}");
        assert!(
            text.last().is_some_and(|row| row.contains("more below")),
            "{text:?}"
        );
    }

    /// A marker on the selected row would show the fold and hide the thing
    /// the reader had chosen, which is the original defect with extra steps.
    #[test]
    fn a_marker_never_lands_on_the_selection() {
        for sel in 0..40 {
            for height in 2..10 {
                let text = texts(&fold_to(rows(40), Some(sel), height));
                let want = format!("step {sel}");
                assert!(
                    text.contains(&want),
                    "selection {sel} fell on a marker at height {height}: {text:?}"
                );
            }
        }
    }

    /// The last page is full rather than trailing blank rows: a window
    /// clamped to the end still draws `height` rows.
    #[test]
    fn a_selection_at_the_end_still_fills_the_window() {
        let out = fold_to(rows(20), Some(19), 6);
        assert_eq!(out.len(), 6);
        let text = texts(&out);
        assert_eq!(text[5], "step 19", "{text:?}");
        assert!(text[0].contains("more above"), "{text:?}");
    }

    /// One row cannot carry both content and the admission that it is hiding
    /// the rest, so it carries the admission.
    #[test]
    fn a_single_row_window_admits_that_nothing_fits() {
        let text = texts(&fold_to(rows(9), None, 1));
        assert_eq!(text, vec!["… 9 rows, none fit".to_string()]);
    }

    /// `fold_to` being correct buys nothing if `render_body` stops calling it,
    /// and every test above would still pass. This one paints.
    #[test]
    fn render_body_paints_the_admission_rather_than_dropping_the_tail() {
        let area = Rect::new(0, 0, 24, 4);
        let mut buf = Buffer::empty(area);
        render_body(rows(20), None, area, &mut buf);
        let painted: String = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .map(|(x, y)| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
            .collect();
        assert!(
            painted.contains("more below"),
            "the card admits what it cut: {painted:?}"
        );
    }

    /// `card_frame` draws the same chrome family as every hand-rolled
    /// overlay: a rounded border, and hints on the bottom rule rather than
    /// the top title. This pins the port against regressing to the old
    /// `Plain` border with top-title hints.
    #[test]
    fn card_frame_draws_the_shared_v2_chrome() {
        let area = Rect::new(0, 0, 30, 6);
        let mut buf = Buffer::empty(area);
        card_frame(area, "plan", Vec::new(), "esc close", &mut buf);
        assert_eq!(
            buf.cell((0, 0)).map(|c| c.symbol()),
            Some("╭"),
            "top-left corner is rounded"
        );
        let bottom_row: String = (0..area.width)
            .map(|x| {
                buf.cell((x, area.height - 1))
                    .map(|c| c.symbol())
                    .unwrap_or(" ")
            })
            .collect();
        assert!(
            bottom_row.contains("esc close"),
            "hints ride the bottom border: {bottom_row:?}"
        );
        let top_row: String = (0..area.width)
            .map(|x| buf.cell((x, 0)).map(|c| c.symbol()).unwrap_or(" "))
            .collect();
        assert!(
            !top_row.contains("esc close"),
            "hints no longer ride the top title: {top_row:?}"
        );
    }

    #[test]
    fn card_area_fits_inside_any_frame() {
        for (w, h) in [(1u16, 1u16), (3, 2), (56, 5), (80, 24), (200, 60)] {
            let frame = Rect::new(0, 0, w, h);
            let card = card_area(frame, 9, CARD_MAX_W, false);
            assert!(card.right() <= frame.right(), "{w}x{h}");
            assert!(card.bottom() <= frame.bottom(), "{w}x{h}");
            assert!(card.width <= CARD_MAX_W);
        }
    }

    #[test]
    fn the_mini_bar_never_paints_the_brand_fill_glyph() {
        // The `█` glyph is the brand-gradient run bar's alone —
        // tests/progress_brand_fill.rs isolates fill cells by that symbol,
        // so a second `█`-painting widget would make its witness ambiguous.
        let spans = mini_fraction_bar(4, 7, 10, crate::theme::OK);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!text.contains('█'), "{text}");
        assert_eq!(text.chars().count(), 10);
    }

    #[test]
    fn truncation_is_grapheme_safe_on_wide_glyphs() {
        let truncated = truncate_cols("日本語のテキスト", 7);
        assert!(truncated.ends_with('…'));
        // Never more than the budget in display columns.
        let w: usize = truncated
            .chars()
            .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0))
            .sum();
        assert!(w <= 7, "{truncated} is {w} cols");
    }

    #[test]
    fn fmt_mss_zero_pads_seconds() {
        assert_eq!(fmt_mss(38_000), "0:38");
        assert_eq!(fmt_mss(72_000), "1:12");
        assert_eq!(fmt_mss(0), "0:00");
    }
}
