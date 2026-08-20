//! The status bar — SPEC 5, one row:
//!
//! ```text
//! kimi-k3 · execute · ctx ████▎░░░░░░░ 35% · $0.45 · saved $0.69 · ✉ 21    ? help
//! ```
//!
//! ## Why one row and not two
//!
//! The v1 statline was two rows: a dim micro-label row stacked over its value
//! row, ten cells wide, carrying MODEL, the stage box, CPU, CONTEXT, SPEND,
//! CACHE, SAVED, WARMTH, ENGINE and INBOX. Its case — stacking the label above
//! the value spends free vertical space instead of scarce horizontal space —
//! was sound for the set of cells it was answering for, and
//! [`crate::statline`]'s module doc keeps it, because a design decision this
//! surface reversed is worth being able to read.
//!
//! v2 changes the set, and the argument does not survive the change. CPU, MEM,
//! WARMTH and ENGINE move behind `?` and the AGENTS tab (SPEC 5) because none
//! of them is a fact about the *work*; CACHE collapses into the one number a
//! reader acts on, `saved`. What is left is six values, five of which are two
//! or three glyphs wide and need no label at all — `$0.45` is money, `✉ 21` is
//! an inbox, and the one that does take a word (`ctx`) fits it inline for four
//! cells. A label row above *that* set would be a row of chrome explaining
//! values that explain themselves, and permanent chrome has to earn its row on
//! every frame.
//!
//! The row it gives back is not free floor space either: it is the row the
//! keybinding hint line and the pipeline line above the prompt now occupy.
//!
//! ## What is deliberately NOT here
//!
//! A `det %` cell — the deterministic/model split. An earlier draft of SPEC 5
//! placed it on this row and it was removed rather than plumbed (owner's call,
//! 2026-08-19). Do not restore it: a permanent row is the wrong home for a
//! number whose meaning changes with scope, and the thesis it served is
//! expressed where work is actually summarized — the turn receipt (SPEC 6.1)
//! and the `$0.00 · det` tag on a deterministic gate (SPEC 6.3). Those tags
//! are booleans, not a computed ratio: a call either reached a model or it did
//! not.
//!
//! ## Where it draws
//!
//! `render_deck`'s bottom band, through [`render_band`] — the deck's only
//! status row since #4129 deleted the two-row wall. [`crate::v2::project`]
//! turns the live `WorkspaceModel` into a [`Status`]; nothing in this module
//! reads the fold itself, which is what keeps every golden below fixture data
//! all the way down.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use stella_tui_theme::{glyph, token};

/// Cells the context meter occupies, track included.
const METER_CELLS: usize = 12;

/// Everything the status bar says, and nothing it can derive.
///
/// Pure input: no clock, no environment, no process table. The bar is a
/// projection of this and the frame is a function of it, which is what makes
/// the golden frames below fixture data all the way down.
#[derive(Clone, Copy, Debug)]
pub struct Status<'a> {
    /// The model actually answering — the vendor's own slug, not the gateway
    /// that proxied it.
    pub worker: &'a str,
    /// Where the run is: `execute`, `verify`, whatever a plugin contributed.
    pub stage: &'a str,
    /// Context window used, `0.0..=1.0`.
    pub ctx_used: f64,
    /// Session spend, USD.
    pub spend_usd: f64,
    /// Saved against the un-cached price of the same work, USD.
    pub saved_usd: f64,
    /// Queued inbound messages.
    pub inbox: u32,
}

/// A ratio computed upstream, made safe to draw with.
///
/// `ctx_used` is a quotient of counts that have been wrong before, and a
/// status bar is the one widget that must never take the screen down.
/// Non-finite reads as zero rather than as a saturating cast nobody chose:
/// `f64::clamp` propagates `NaN`, and every arithmetic path downstream would
/// then land on whatever `as usize` happens to do with it.
fn ratio(frac: f64) -> f64 {
    if frac.is_finite() {
        frac.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// The partial cell at the head of the fill, given the fraction of it filled.
///
/// Eighth-blocks are the only sub-cell precision this design allows itself
/// (SPEC 2), and a meter is what they exist for: at twelve cells, whole-cell
/// rounding moves the bar in 8.3% steps, which reads as a bar that does not
/// track the number printed beside it.
///
/// The clamp to `1..=7` is the load-bearing part. Rounding is allowed to move
/// the head of the bar; it is not allowed to move its *ends*. A partial cell
/// that rounds up to `█` renders a 99.9% context window identically to a full
/// one — and "full" is the reading a user compacts on, so the two must never
/// look alike. Symmetrically, a fill that exists at all shows at least one
/// eighth rather than reading as empty.
fn partial_cell(filled: f64) -> char {
    if filled <= 0.0 {
        return glyph::BLOCK_EIGHTHS[0];
    }
    let eighths = (filled * 8.0).round().clamp(1.0, 7.0) as usize;
    glyph::BLOCK_EIGHTHS[eighths]
}

/// The meter's spans: gold fill on `border` gray (SPEC 5).
fn meter(frac: f64) -> Vec<Span<'static>> {
    let exact = ratio(frac) * METER_CELLS as f64;
    let full = exact.trunc() as usize;
    let mut spans = Vec::with_capacity(3);
    if full > 0 {
        spans.push(Span::styled(
            glyph::BLOCK_EIGHTHS[8]
                .to_string()
                .repeat(full.min(METER_CELLS)),
            Style::new().fg(token::GOLD),
        ));
    }
    if full < METER_CELLS {
        let partial = partial_cell(exact.fract());
        if partial != ' ' {
            spans.push(Span::styled(
                partial.to_string(),
                Style::new().fg(token::GOLD),
            ));
        }
        let track = METER_CELLS - full - usize::from(partial != ' ');
        if track > 0 {
            spans.push(Span::styled(
                glyph::METER_TRACK.to_string().repeat(track),
                Style::new().fg(token::BORDER),
            ));
        }
    }
    spans
}

/// One `·`, in `dim`. The separator is chrome and is priced as chrome.
fn sep() -> Span<'static> {
    Span::styled(" · ", Style::new().fg(token::DIM))
}

/// The bar's left group, in SPEC 5 order, as a list of cells so the renderer
/// can drop from the right when the row is tight.
///
/// Pure over [`Status`] — THE decision function, unit-testable without a
/// buffer. The v1 status line built its row the same way and for the same
/// reason; that builder is gone now (`crate::statline` keeps only the two
/// pieces the v2 bar does not replace), so this is the only one left.
#[must_use]
pub fn cells(status: &Status<'_>) -> Vec<Vec<Span<'static>>> {
    let text = Style::new().fg(token::TEXT);
    let label = Style::new().fg(token::MUTED);
    // Money is gold, every time it appears (SPEC 5). Spend and savings are the
    // same kind of fact and take the same metal; only their labels differ.
    let money = Style::new().fg(token::GOLD);

    vec![
        vec![Span::styled(status.worker.to_string(), text)],
        vec![Span::styled(status.stage.to_string(), text)],
        {
            let mut ctx = vec![Span::styled("ctx ", label)];
            ctx.extend(meter(status.ctx_used));
            ctx.push(Span::styled(format!(" {}%", pct(status.ctx_used)), text));
            ctx
        },
        vec![Span::styled(format!("${:.2}", status.spend_usd), money)],
        vec![
            Span::styled("saved ", label),
            Span::styled(format!("${:.2}", status.saved_usd), money),
        ],
        vec![
            // The world coming in takes silver (SPEC 2) — a queued message is
            // something that arrived, not something stella did.
            Span::styled("✉ ", Style::new().fg(token::SILVER)),
            Span::styled(status.inbox.to_string(), text),
        ],
    ]
}

/// A fraction as whole percent, saturating at both ends.
fn pct(frac: f64) -> u32 {
    (ratio(frac) * 100.0).round() as u32
}

/// The help affordance, pinned right. A keybinding hint, so `dim` (SPEC 5's
/// hint tier, and the one thing on this row that is an instruction rather than
/// a fact).
fn help() -> Vec<Span<'static>> {
    vec![Span::styled("? help", Style::new().fg(token::DIM))]
}

/// Draw the deck's bottom band: the status bar, and the diagnosis row under it
/// when the caller reserved one.
///
/// Lives here rather than in `deck_render` because `deck_render.rs` is a
/// grandfathered god file and closed to growth (AGENTS.md, "God files — plan
/// around them, never into them"), and because the decision of what the band
/// contains is this module's, not the frame's. The frame's job is to hand it a
/// `Rect`.
pub fn render_band(
    model: &crate::deck::WorkspaceModel,
    ui: &crate::deck_ui::DeckUi,
    area: Rect,
    buf: &mut Buffer,
) {
    let worker = super::project::worker(model);
    StatusBar(super::project::status(model, ui, &worker)).render(Rect { height: 1, ..area }, buf);
    // The low-hit-rate diagnosis is prose, so it keeps a row of its own rather
    // than a cell on a row of glanceable values.
    if let Some(rest) = area.rows().nth(1) {
        crate::statline::render_diagnosis(model, ui, rest, buf);
    }
}

/// The status bar as a `ratatui` widget.
///
/// Draws exactly one row: the top row of `area`, whatever height it is given,
/// so a caller that over-allocates gets a bar rather than a stretched one.
#[derive(Clone, Copy, Debug)]
pub struct StatusBar<'a>(pub Status<'a>);

impl Widget for StatusBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let row = Rect { height: 1, ..area };
        let width = row.width as usize;

        // The ground is painted, never inherited: every contrast figure in the
        // palette is measured against `bg`, and a terminal's own background is
        // not it.
        buf.set_style(row, Style::new().bg(token::BG));

        let help = help();
        let help_width = span_width(&help);
        let mut cells = cells(&self.0);

        // Drop from the right until the left group and the help affordance fit
        // with a gap between them. Worker and stage are never dropped — which
        // pin is answering and where the run is are the two facts a status bar
        // exists for; everything after them is negotiable.
        while cells.len() > 2 && joined_width(&cells) + help_width + 2 > width {
            cells.pop();
        }

        let mut spans = Vec::new();
        for (i, cell) in cells.iter().enumerate() {
            if i > 0 {
                spans.push(sep());
            }
            spans.extend(cell.iter().cloned());
        }
        let left_width = span_width(&spans);
        if left_width + help_width + 2 <= width {
            spans.push(Span::styled(
                " ".repeat(width - left_width - help_width),
                Style::new(),
            ));
            spans.extend(help);
        }
        Line::from(spans).render(row, buf);
    }
}

/// Display width of a span list.
fn span_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(Span::width).sum()
}

/// Display width of the left group once its separators are counted.
fn joined_width(cells: &[Vec<Span<'static>>]) -> usize {
    let content: usize = cells.iter().map(|cell| span_width(cell)).sum();
    content + cells.len().saturating_sub(1) * 3
}
