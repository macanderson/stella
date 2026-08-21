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
//! ## The one conditional cell
//!
//! Everything above is permanent. `deadline` is not: it draws only while a
//! task deadline is armed, sits third — immediately after the stage — and is
//! the last cell the drop rule gives up (SPEC 5).
//!
//! It is the one cell here that says the run is about to *stop* rather than
//! how it is going, and an expiring deadline is a `SIGKILL`. It was on the v1
//! wall, SPEC 5's re-homing sentence did not mention it, and #4123 dropped it
//! along with the row — for a day nothing on any screen counted it down
//! (#4126). What survives from v1 is the distinction that made the cell worth
//! having: `None` is *nobody is timing this run* and draws nothing at all,
//! `Some(0)` is *this run is out of time* and draws a word. A cell reading
//! `0s` for an unarmed run would say the first thing in the second thing's
//! shape.
//!
//! v1 coloured it on a three-rung green/amber/red ladder. That does not
//! survive SPEC 2: green means *pass* here and an unfinished countdown has
//! passed nothing, and the palette has no amber that is not gold, which means
//! stella acting. Two rungs, then — `text` while there is time, `red` inside
//! the last minute, where a kill this close is a destructive event in
//! progress and that is what §2 keeps red for.
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
//! ## Wiring
//!
//! [`render_band`] is the deck's bottom band: `render_deck` reserves one row
//! for it and two only when a cache diagnosis is showing.
//!
//! This section used to say the module was "not wired to the live deck yet",
//! and give the reason — that a v2 bar under v1 chrome would be "the only
//! cool-gray element on a warm screen". That argument was already spent when it
//! was written: the recolour that made v1 cool landed four minutes after the
//! module did, and the two palettes agree on every token this row uses. The
//! note outlived the condition it described by a day, which is the whole reason
//! this repo treats a stale comment as a defect rather than as harmless prose.

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
    /// Wall clock left before the armed task deadline, in milliseconds.
    ///
    /// `None` and `Some(0)` are different facts and render differently: see
    /// the module doc's "The one conditional cell".
    pub deadline_remaining_ms: Option<u64>,
}

/// At or under this much left, the countdown turns red.
///
/// One minute, because that is the horizon on which a human can still do
/// something about a `SIGKILL` — steer the turn, raise the deadline, or take
/// the artifact off the machine before it is taken away.
const DEADLINE_ALARM_MS: u64 = 60_000;

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

    let mut cells = vec![
        vec![Span::styled(status.worker.to_string(), text)],
        vec![Span::styled(status.stage.to_string(), text)],
    ];
    // Third, so the drop rule — which pops from the right — gives it up last
    // of everything it is allowed to give up. The cell that says the run is
    // about to stop must not be the first casualty of a narrow terminal.
    if let Some(remaining_ms) = status.deadline_remaining_ms {
        cells.push(deadline_cell(remaining_ms, label));
    }
    cells.extend([
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
    ]);
    cells
}

/// The CLOCK cell: what is left of an armed task deadline (SPEC 5).
///
/// Only ever called with an armed deadline — an unarmed run has no cell,
/// because "nobody is timing this" is not a duration and must not be drawn as
/// one.
fn deadline_cell(remaining_ms: u64, label: Style) -> Vec<Span<'static>> {
    let value = if remaining_ms <= DEADLINE_ALARM_MS {
        token::RED
    } else {
        token::TEXT
    };
    vec![
        // The word is the state's non-colour carrier (SPEC 13): on a 16-colour
        // terminal, under `NO_COLOR`, or to a red-blind reader, `deadline` is
        // still the thing that distinguishes this cell from a number.
        Span::styled("deadline ", label),
        Span::styled(fmt_remaining(remaining_ms), Style::new().fg(value)),
    ]
}

/// The countdown's text: the coarsest unit that still resolves the wait.
///
/// Seconds are dropped above an hour deliberately. A run with an hour left is
/// not one anybody is watching second by second, and a cell that changes width
/// on every tick costs each of its neighbours a column each time.
///
/// Carried over from the v1 statline unchanged, `expired` included — the one
/// string here that is a word rather than a quantity, because `0s` invites the
/// reading "no deadline", which is the single thing it does not mean.
fn fmt_remaining(remaining_ms: u64) -> String {
    let secs = remaining_ms / 1_000;
    match secs {
        0 if remaining_ms == 0 => "expired".to_string(),
        // Sub-second but not yet crossed: still armed, still counting.
        0 => "<1s".to_string(),
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{}h {:02}m", s / 3_600, (s % 3_600) / 60),
    }
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
        //
        // An armed `deadline` is negotiable *last*, which [`cells`] arranges by
        // placing it third rather than by teaching this loop a priority table.
        // Position is the priority here, and one mechanism that both the reader
        // and the renderer can see beats two that have to agree.
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
