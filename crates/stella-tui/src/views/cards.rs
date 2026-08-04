//! Shared chrome for the floating cards (`/tasks` · `/scope` · `/witness` ·
//! `/models` · `/budget`): one bordered block floated above the composer,
//! max-width ~56 cells, title row = accent-colored name + dimmed context +
//! right-aligned dim key hints; body rows below. Implemented once so the five
//! cards read as one system, and so the selection convention — a `▸` marker
//! glyph **plus** the background tint — is structural: the golden suite
//! strips style, so a style-only selection would be invisible to it (the
//! exact gap the FILES list has today).

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

use crate::theme;

/// Widest card, in columns (matches the panic boundary's error card).
pub(crate) const CARD_MAX_W: u16 = 56;

/// Rows of deck chrome under the content area the card floats above
/// (trace strip 2 + progress 1 + composer 1 + footer 1 + statline 1).
const CHROME_BELOW: u16 = 6;

/// Where a card floats: horizontally centered, bottom-anchored just above
/// the composer chrome, tall enough for `body_rows` + the two border rows,
/// clamped to the frame on small terminals. Never a zero-size rect on a
/// nonzero frame — the guard's scratch discipline handles the rest.
///
/// In accessible mode (`full_width`) the card spans the frame instead: the
/// float is a visual affordance, and a labeled record clipped at a float's
/// right border is a record a reader never hears the end of. `max_w` is the
/// card's own width cap — [`CARD_MAX_W`] for most; the witness panel runs
/// wider because its records are fixed product copy that may not elide.
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
/// success color) beside it, `hints` dim and right-aligned. Everything
/// meaningful in it is a glyph/word (style-blind goldens can pin it).
pub(crate) fn card_frame(
    area: Rect,
    name: &str,
    context: Vec<Span<'static>>,
    hints: &str,
    buf: &mut Buffer,
) -> Rect {
    Clear.render(area, buf);
    let mut title = vec![Span::styled(format!(" {name} "), theme::accent())];
    if !context.is_empty() {
        title.push(Span::raw(" "));
        title.extend(context);
        title.push(Span::raw(" "));
    }
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(title));
    if !hints.is_empty() {
        block = block.title(
            Line::from(Span::styled(
                format!(" {hints} "),
                Style::new().fg(theme::TEXT_TERTIARY),
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
/// `SELECT_BG` layered by the caller. Returned as a span so the caller
/// controls the rest of the row.
pub(crate) fn marker(selected: bool) -> Span<'static> {
    if selected {
        Span::styled("▸ ", theme::accent())
    } else {
        Span::raw("  ")
    }
}

/// Paint `lines` into `inner`, tinting the rows in `selected_rows` with the
/// selection background (the style half of the marker+tint convention).
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
        line.style = line.style.bg(theme::SELECT_BG);
    }
    Paragraph::new(lines).render(inner, buf);
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
        Span::styled(
            "▱".repeat(width - filled),
            Style::new().fg(theme::TEXT_TERTIARY),
        ),
    ]
}

/// Format a millisecond span as `m:ss` (the per-record elapsed the witness
/// and task cards show).
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
