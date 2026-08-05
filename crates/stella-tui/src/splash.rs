//! The launch mark: the comet identity assembling, shown while session init
//! is genuinely still running.
//!
//! When the frame affords it, the mark is the brand's assemble sequence
//! (`docs/brand/spinners/spinner-lockup-*.svg`) translated to cells: the three
//! comet trails slide in staggered, the four-point star pops with a brief
//! bright flash, the wordmark types on letter by letter behind a gold block
//! cursor, and the cursor blinks until init hands off. The star is not stored
//! art — it is the astroid `|x|^(2/3) + |y|^(2/3) <= r^(2/3)` rasterized into
//! half-block pixels at whatever size ~30% of the frame height works out to,
//! so the mark scales to the terminal instead of shipping one bitmap.
//!
//! On a frame too small to carry that, the mark falls back to the original
//! one-line lockup (chevron · wordmark · cursor) with its single reveal step.
//!
//! The engineering contract is unchanged from the one-line era:
//!
//! - **Pure function of elapsed time.** Two renders at the same `t` produce
//!   byte-identical buffers; a dropped frame scrubs instead of stalling.
//!   Animation time is additionally quantized to [`FRAME_MS`] so a recording
//!   repaints on a stable grid.
//! - **Any key skips, immediately.** The assemble is never modal.
//! - **Reduced (`--no-anim` / `STELLA_NO_ANIM` / `NO_COLOR`) is one static
//!   frame** — the finished composition, cursor hidden, standing briefly.
//! - Colours are semantic theme roles only, so the mark follows the identity
//!   and still narrows through `theme::degrade_buffer`.
//!
//! One contract **did** change, deliberately: a held splash whose init
//! finishes early now plays the assemble out to [`ASSEMBLE`] before handing
//! off, instead of cutting mid-build. The launch mark is the one place the
//! brand is allowed a beat of screen time; a key press still cuts it
//! instantly, and reduced mode never waits.

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::theme;

/// Compact mode only: how long the one-line mark renders muted before
/// stepping to its real colours.
pub const REVEAL: Duration = Duration::from_millis(180);

/// Animation time is floored to this grid before any phase math. The deck
/// repaints on a ~33ms tick anyway; quantizing to 40ms makes "two renders at
/// the same elapsed time are byte-identical" robust against sub-millisecond
/// drift between the two clock reads instead of merely likely.
const FRAME_MS: u64 = 40;

// ---- the assemble timeline (large mark), in ms of quantized elapsed time ----

/// When each comet trail starts sliding in: middle (the long leader), top,
/// bottom — the same stagger as the brand spinner.
const TRAIL_START_MS: [u64; 3] = [0, 90, 180];
/// How long one trail takes to ease into place.
const TRAIL_SLIDE_MS: u64 = 240;
/// The star pop: scale 0 -> overshoot -> 1, flashing bright while it moves.
const STAR_POP_START_MS: u64 = 260;
const STAR_POP_MS: u64 = 240;
/// Letter type-on: the first letter lands at `TYPE_START_MS`, one more every
/// `TYPE_STEP_MS` after.
const TYPE_START_MS: u64 = 540;
const TYPE_STEP_MS: u64 = 80;
/// The cursor starts its blink once the last letter has landed.
const BLINK_BASE_MS: u64 = TYPE_START_MS + 5 * TYPE_STEP_MS;
/// Half-period of the cursor blink square wave.
const BLINK_HALF_MS: u64 = 320;

/// The assemble is complete: everything is in place and the cursor has held
/// one solid beat. A held splash released early still plays to here.
const ASSEMBLE: Duration = Duration::from_millis(1_020);

/// How long an **unheld** splash stays up past the assemble — long enough for
/// two cursor blinks, short enough not to be a wait.
const UNHELD_HOLD: Duration = Duration::from_millis(640);

/// A held splash that never hears `release()` (a driver that died mid-init)
/// still ends: past this cap it hands off regardless. Any key skips long
/// before this matters.
const HOLD_FAILSAFE: Duration = Duration::from_secs(120);

/// `--no-anim` / `STELLA_NO_ANIM` / `NO_COLOR`: the finished mark stands for
/// exactly this long. A recording gets one static frame, no transitions.
const REDUCED_TOTAL: Duration = Duration::from_millis(900);

/// Narrower than this and the version/model line has nowhere to go — the mark
/// renders alone rather than wrapping into nonsense.
const MIN_DETAIL_WIDTH: u16 = 28;

/// Below either of these the large mark cannot land and the one-line lockup
/// renders instead.
const MIN_LARGE_WIDTH: u16 = 30;
const MIN_LARGE_HEIGHT: u16 = 12;
/// The star art's height as a share of the frame, and its clamp. 3/10 is the
/// brief: the mark owns about 30% of the screen.
const MARK_ROWS_MIN: u16 = 5;
const MARK_ROWS_MAX: u16 = 15;

// ---- brand geometry, in the logo kit's canvas units (docs/brand/logo) ----
// The lockup's comet region: trails at x 10..30, star centred at (64, 48)
// with half-diagonal 22. The canvas is the bounding box with a little air.

const CANVAS_X0: f64 = 4.0;
const CANVAS_W: f64 = 88.0;
const CANVAS_Y0: f64 = 24.0;
const CANVAS_H: f64 = 48.0;
const STAR_CX: f64 = 64.0;
const STAR_CY: f64 = 48.0;
const STAR_R: f64 = 22.0;
/// Each trail: (centre y, x start, x end). Stroke width 7 in canvas units.
const TRAILS: [(f64, f64, f64); 3] = [(48.0, 10.0, 30.0), (34.0, 18.0, 30.0), (62.0, 18.0, 30.0)];
const TRAIL_HALF: f64 = 3.5;
/// Trails slide in from this x offset, easing to 0 — the spinner's
/// `translateX(-14px)`.
const TRAIL_FROM: f64 = -14.0;

/// The compact lockup, left to right: chevron · wordmark · block cursor.
const CHEVRON: &str = "› ";
const WORDMARK: &str = "stella";
const CURSOR: &str = "▮";

/// The large mark's wordmark, tracked out one space per letter to sit under
/// the star at scale (the kit's wide letterspacing, in cells).
const LETTERS: [char; 6] = ['s', 't', 'e', 'l', 'l', 'a'];
/// `s t e l l a` (11 cells) + ` ▮` (2 cells).
const WORDMARK_FIELD_W: u16 = 13;

/// Ephemeral splash timing. Not part of the model — pure presentation.
#[derive(Debug, Clone)]
pub struct SplashState {
    start: Instant,
    /// When `skip()` dismissed the mark early, if it did.
    skipped_at: Option<Instant>,
    /// Held open over a running init: the mark stands until `release()`.
    held: bool,
    /// When `release()` ended the hold (init finished).
    released_at: Option<Instant>,
    /// `--no-anim`: one static finished frame, ignore hold cues.
    reduced: bool,
}

impl Default for SplashState {
    fn default() -> Self {
        Self::new()
    }
}

impl SplashState {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            skipped_at: None,
            held: false,
            released_at: None,
            reduced: false,
        }
    }

    /// A splash held open over a running init: the mark stands until
    /// [`Self::release`]. The driver opens one this way at session start and
    /// on `/init`.
    pub fn new_held() -> Self {
        Self {
            held: true,
            ..Self::new()
        }
    }

    /// Init finished — hand off once the assemble completes.
    ///
    /// A release after [`ASSEMBLE`] hands off immediately. A release before it
    /// lets the mark finish assembling first: the launch mark is the one
    /// deliberate beat of brand screen time, and it is still never modal —
    /// any key skips it at once, and reduced mode never waits. Idempotent;
    /// a no-op on an unheld splash (which ends on its own).
    pub fn release(&mut self) {
        if self.held && self.released_at.is_none() {
            self.released_at = Some(Instant::now());
        }
    }

    /// `--no-anim` / `STELLA_NO_ANIM` / `NO_COLOR`: one static finished frame,
    /// standing briefly.
    pub fn set_reduced(&mut self, reduced: bool) {
        self.reduced = reduced;
    }

    /// Dismiss immediately (any key). Idempotent — the first skip wins.
    pub fn skip(&mut self) {
        if self.skipped_at.is_none() {
            self.skipped_at = Some(Instant::now());
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// The mark's total screen time, when it is knowable — `None` only while a
    /// held splash is still waiting on init.
    fn total(&self) -> Option<Duration> {
        if self.reduced {
            return Some(REDUCED_TOTAL);
        }
        if !self.held {
            return Some(ASSEMBLE + UNHELD_HOLD);
        }
        // Released: hand off, but not before the assemble has landed.
        self.released_at
            .map(|at| at.duration_since(self.start).max(ASSEMBLE))
    }

    /// True once the mark should hand off to the deck.
    pub fn is_done(&self) -> bool {
        if self.skipped_at.is_some() {
            return true;
        }
        match self.total() {
            Some(total) => self.elapsed() >= total,
            // Held and un-released: standing, unless the driver died mid-init.
            None => self.elapsed() >= HOLD_FAILSAFE,
        }
    }

    /// Compact mode's single reveal step — muted before, real colours after.
    /// A reduced splash is lit immediately.
    fn lit(&self) -> bool {
        self.reduced || self.elapsed() >= REVEAL
    }

    /// Animation time: elapsed ms floored to the [`FRAME_MS`] grid, or pinned
    /// to the finished frame when reduced.
    fn anim_ms(&self) -> u64 {
        if self.reduced {
            return ASSEMBLE.as_millis() as u64;
        }
        let ms = self.elapsed().as_millis() as u64;
        (ms / FRAME_MS) * FRAME_MS
    }
}

/// A state whose clock reads `elapsed_ms` — for the `splash_frames` example
/// and any other offline preview of the timeline. Not for the deck: live
/// splashes start at zero.
#[doc(hidden)]
pub fn state_backdated_for_preview(elapsed_ms: u64) -> SplashState {
    SplashState {
        start: Instant::now() - Duration::from_millis(elapsed_ms),
        ..SplashState::new()
    }
}

/// Draw the launch mark. `model` is the resolved model id once the session
/// knows one; it is omitted rather than guessed at when it does not.
///
/// A pure function of `(state.elapsed(), state flags, area, model)` — two
/// calls at the same elapsed time write byte-identical buffers.
pub fn render(state: &SplashState, model: Option<&str>, area: Rect, buf: &mut Buffer) {
    if state.is_done() || area.width == 0 || area.height == 0 {
        return;
    }
    match mark_rows_for(area) {
        Some(rows) => render_large(state, model, rows, area, buf),
        None => render_compact(state, model, area, buf),
    }
}

/// The star art's height in rows for this frame, or `None` when the frame is
/// too small for the large mark and the one-line lockup should render.
fn mark_rows_for(area: Rect) -> Option<u16> {
    if area.height < MIN_LARGE_HEIGHT || area.width < MIN_LARGE_WIDTH {
        return None;
    }
    // ~30% of the frame height, floored at the smallest star that still
    // reads as one — a frame tall enough for the large mark at all gets at
    // least that...
    let wanted = (((u32::from(area.height) * 3) / 10) as u16).max(MARK_ROWS_MIN);
    // ...but never wider than the frame: art width = CANVAS_W * 2rows/CANVAS_H.
    let fit_by_width = ((f64::from(area.width) * CANVAS_H) / (2.0 * CANVAS_W)).floor() as u16;
    let rows = wanted.min(fit_by_width).min(MARK_ROWS_MAX);
    (rows >= MARK_ROWS_MIN).then_some(rows)
}

/// The assembled comet: star art over a typed-on wordmark over the detail
/// line, all centred as one composition.
fn render_large(state: &SplashState, model: Option<&str>, mark_rows: u16, area: Rect, buf: &mut Buffer) {
    let t = state.anim_ms();
    let px_per_unit = f64::from(2 * mark_rows) / CANVAS_H;
    let art_w = ((CANVAS_W * px_per_unit).ceil() as u16).min(area.width);

    let show_detail = area.width >= MIN_DETAIL_WIDTH;
    // art + blank + wordmark, then blank + detail when there is room.
    let comp_h = (mark_rows + 2 + if show_detail { 2 } else { 0 }).min(area.height);
    let top = area.y + (area.height - comp_h) / 2;
    // Centre the *star* over the wordmark — it is the optical anchor; the
    // trails hang off to its left the way a comet's tail does. Clamped so a
    // tight frame slides the art right rather than clipping the trails.
    let star_px = ((STAR_CX - CANVAS_X0) * px_per_unit).round() as u16;
    let art_x = (area.x + area.width / 2)
        .saturating_sub(star_px)
        .max(area.x)
        .min(area.x + area.width.saturating_sub(art_w));

    // The star and trails, rasterized as half-block pixels: each cell row is
    // two pixel rows, which at a ~1:2 cell aspect makes pixels near-square.
    for cy in 0..mark_rows.min(comp_h) {
        let y = top + cy;
        for cx in 0..art_w {
            let x = art_x + cx;
            let upper = sample(t, canvas_x(cx, px_per_unit), canvas_y(2 * cy, px_per_unit));
            let lower = sample(t, canvas_x(cx, px_per_unit), canvas_y(2 * cy + 1, px_per_unit));
            let Some(cell) = buf.cell_mut((x, y)) else {
                continue;
            };
            match (upper, lower) {
                (Some(a), Some(b)) if a == b => {
                    cell.set_symbol("█").set_fg(a);
                }
                (Some(a), Some(b)) => {
                    cell.set_symbol("▀").set_fg(a).set_bg(b);
                }
                (Some(a), None) => {
                    cell.set_symbol("▀").set_fg(a);
                }
                (None, Some(b)) => {
                    cell.set_symbol("▄").set_fg(b);
                }
                (None, None) => {}
            }
        }
    }

    // The wordmark types on from a fixed left edge — the line must not
    // re-centre itself as letters land.
    let word_y = top + mark_rows + 1;
    if word_y < area.y + area.height {
        let revealed = revealed_letters(t);
        let cursor_on = if state.reduced {
            // The spinner hides the cursor under reduced motion; so do we.
            false
        } else if revealed < LETTERS.len() {
            true
        } else {
            ((t.saturating_sub(BLINK_BASE_MS)) / BLINK_HALF_MS) % 2 == 0
        };

        let mut shown = String::new();
        for (i, l) in LETTERS.iter().enumerate().take(revealed) {
            if i > 0 {
                shown.push(' ');
            }
            shown.push(*l);
        }
        let mut spans: Vec<Span<'static>> = vec![Span::styled(
            shown,
            Style::new().fg(theme::INK).add_modifier(Modifier::BOLD),
        )];
        if cursor_on {
            let cursor = if revealed > 0 {
                format!(" {CURSOR}")
            } else {
                CURSOR.to_string()
            };
            spans.push(Span::styled(cursor, Style::new().fg(theme::GOLD)));
        }
        let field_w = WORDMARK_FIELD_W.min(area.width);
        let word_rect = Rect {
            x: area.x + (area.width - field_w) / 2,
            y: word_y,
            width: field_w,
            height: 1,
        };
        Paragraph::new(Line::from(spans)).render(word_rect, buf);
    }

    // The detail line: version · model, centred, muted.
    let detail_y = word_y + 2;
    if show_detail && detail_y < area.y + area.height {
        let mut detail = format!("v{}", env!("CARGO_PKG_VERSION"));
        if let Some(model) = model {
            detail.push_str(" · ");
            detail.push_str(model);
        }
        let detail_rect = Rect {
            x: area.x,
            y: detail_y,
            width: area.width,
            height: 1,
        };
        Paragraph::new(Line::from(Span::styled(detail, theme::muted())).alignment(Alignment::Center))
            .render(detail_rect, buf);
    }
}

fn canvas_x(px: u16, px_per_unit: f64) -> f64 {
    CANVAS_X0 + (f64::from(px) + 0.5) / px_per_unit
}

fn canvas_y(py: u16, px_per_unit: f64) -> f64 {
    CANVAS_Y0 + (f64::from(py) + 0.5) / px_per_unit
}

/// How many wordmark letters have landed by `t`.
fn revealed_letters(t: u64) -> usize {
    if t < TYPE_START_MS {
        0
    } else {
        (1 + (t - TYPE_START_MS) / TYPE_STEP_MS).min(LETTERS.len() as u64) as usize
    }
}

/// The star pop's scale: 0 -> 1.16 overshoot -> 1, the spinner's back-out.
fn pop_scale(u: f64) -> f64 {
    if u < 0.6 {
        1.16 * (u / 0.6)
    } else {
        1.16 - 0.16 * ((u - 0.6) / 0.4)
    }
}

fn ease_out_cubic(u: f64) -> f64 {
    1.0 - (1.0 - u).powi(3)
}

fn clamp01(u: f64) -> f64 {
    u.clamp(0.0, 1.0)
}

/// Membership test for one brand-canvas point at animation time `t`: the
/// colour it carries, or `None` for ground. Star wins; the geometries never
/// overlap anyway (the trails end left of the star's reach).
fn sample(t: u64, x: f64, y: f64) -> Option<Color> {
    if t >= STAR_POP_START_MS {
        let pop = clamp01((t - STAR_POP_START_MS) as f64 / STAR_POP_MS as f64);
        let r = STAR_R * pop_scale(pop);
        if r > 0.5 {
            let dx = (x - STAR_CX).abs() / r;
            let dy = (y - STAR_CY).abs() / r;
            // The four-point star is the astroid |x|^(2/3) + |y|^(2/3) <= 1.
            if dx.powf(2.0 / 3.0) + dy.powf(2.0 / 3.0) <= 1.0 {
                // Flash bright while the pop is still moving, settle to gold.
                return Some(if pop < 1.0 { theme::GOLD_BRIGHT } else { theme::GOLD });
            }
        }
    }
    for (i, (ty, x0, x1)) in TRAILS.iter().enumerate() {
        let start = TRAIL_START_MS[i];
        if t < start {
            continue;
        }
        let slid = ease_out_cubic(clamp01((t - start) as f64 / TRAIL_SLIDE_MS as f64));
        let off = TRAIL_FROM * (1.0 - slid);
        if (y - ty).abs() <= TRAIL_HALF && x >= x0 + off && x <= x1 + off {
            return Some(theme::GOLD);
        }
    }
    None
}

/// The original one-line lockup, for frames the large mark cannot land on.
fn render_compact(state: &SplashState, model: Option<&str>, area: Rect, buf: &mut Buffer) {
    // The lockup's three parts carry three different roles: the chevron is the
    // interactive/brand signal, the wordmark is ink, the cursor block is the
    // identity gold. All three are semantic theme tokens, so they follow the
    // identity and — unlike an interpolated RGB — still degrade through
    // `theme::degrade_buffer` on a 256- or 16-colour terminal.
    let (chev_style, ink_style, cursor_style, dim_style) = if state.lit() {
        (
            theme::accent(),
            Style::new().fg(theme::INK).add_modifier(Modifier::BOLD),
            Style::new().fg(theme::GOLD),
            theme::muted(),
        )
    } else {
        // The single reveal step: before it lands, the whole lockup is one
        // muted tone.
        let muted = theme::muted();
        (muted, muted, muted, muted)
    };

    let mut lines: Vec<Line<'static>> = vec![
        Line::from(vec![
            Span::styled(CHEVRON, chev_style),
            Span::styled(WORDMARK, ink_style),
            Span::styled(CURSOR, cursor_style),
        ])
        .alignment(Alignment::Center),
    ];
    // The detail line is the only part that needs room; drop it rather than
    // wrap the version across two rows on a narrow terminal.
    if area.width >= MIN_DETAIL_WIDTH && area.height >= 2 {
        let mut detail = format!("v{}", env!("CARGO_PKG_VERSION"));
        if let Some(model) = model {
            detail.push_str(" · ");
            detail.push_str(model);
        }
        lines.push(Line::from(Span::styled(detail, dim_style)).alignment(Alignment::Center));
    }

    let height = (lines.len() as u16).min(area.height);
    let block = Rect {
        x: area.x,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width: area.width,
        height,
    };
    Paragraph::new(lines).render(block, buf);
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    /// Flatten a `TestBackend` buffer to one `String` per row (content, not
    /// raw ANSI — matches the rest of the crate's render tests).
    fn buffer_rows(buf: &Buffer) -> Vec<String> {
        let area = *buf.area();
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect()
    }

    fn has_non_space(rows: &[String]) -> bool {
        rows.iter().any(|row| row.chars().any(|c| c != ' '))
    }

    fn has_block_art(rows: &[String]) -> bool {
        rows.iter()
            .any(|row| row.chars().any(|c| matches!(c, '█' | '▀' | '▄')))
    }

    fn draw(state: &SplashState, w: u16, h: u16) -> Vec<String> {
        draw_with(state, None, w, h)
    }

    fn draw_with(state: &SplashState, model: Option<&str>, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render(state, model, area, f.buffer_mut());
            })
            .unwrap();
        buffer_rows(terminal.backend().buffer())
    }

    /// Backdates `start` so `elapsed()` reads as `elapsed_ms` without a real
    /// sleep. Legal only because `mod tests` is a descendant of the module
    /// that defines `SplashState`'s private fields.
    ///
    /// Times passed here should sit a few ms **inside** a [`FRAME_MS`] bucket
    /// (e.g. 645, not 640) so the µs that pass between backdating and
    /// rendering cannot tip `anim_ms()` into the next frame.
    fn state_at(elapsed_ms: u64) -> SplashState {
        SplashState {
            start: Instant::now() - Duration::from_millis(elapsed_ms),
            ..SplashState::new()
        }
    }

    fn unheld_total_ms() -> u64 {
        (ASSEMBLE + UNHELD_HOLD).as_millis() as u64
    }

    fn assemble_ms() -> u64 {
        ASSEMBLE.as_millis() as u64
    }

    #[test]
    fn an_unheld_mark_ends_on_its_own_promptly() {
        // It covers no work: one assemble, a couple of blinks, gone.
        assert!(
            unheld_total_ms() < 2_000,
            "unheld splash runs {}ms",
            unheld_total_ms()
        );
        assert!(!state_at(0).is_done());
        assert!(state_at(unheld_total_ms() + 50).is_done());
    }

    #[test]
    fn default_matches_new() {
        let state = SplashState::default();
        assert!(!state.is_done());
        assert!(state.skipped_at.is_none());
        assert!(!state.held);
    }

    #[test]
    fn skip_finishes_the_mark_immediately() {
        let mut state = SplashState::new();
        assert!(!state.is_done());
        state.skip();
        assert!(state.is_done());
    }

    #[test]
    fn a_held_mark_stands_until_init_releases_it() {
        let mut state = SplashState {
            held: true,
            ..state_at(10_000)
        };
        assert!(!state.is_done(), "held over a running init");
        state.release();
        assert!(
            state.is_done(),
            "a release after the assemble hands off at once"
        );
    }

    /// The changed contract, pinned: a release **during** the assemble lets
    /// the mark finish assembling (the brand's one beat of screen time), while
    /// a key press still skips instantly.
    #[test]
    fn an_early_release_floors_at_the_assemble_not_the_reveal() {
        let mut early = SplashState {
            held: true,
            ..state_at(25)
        };
        early.release();
        assert!(
            !early.is_done(),
            "an early release plays the assemble out rather than cutting mid-build"
        );
        assert_eq!(early.total(), Some(ASSEMBLE));

        // A key press during that grace still cuts at once.
        early.skip();
        assert!(early.is_done(), "skip always wins immediately");

        // And once the assemble has passed, a released mark is done.
        let played_out = SplashState {
            held: true,
            released_at: Some(Instant::now() - Duration::from_millis(assemble_ms())),
            ..state_at(assemble_ms() + 200)
        };
        assert!(played_out.is_done());
    }

    #[test]
    fn release_is_a_no_op_on_an_unheld_mark() {
        let mut state = state_at(100);
        state.release();
        assert!(state.released_at.is_none());
        assert!(!state.is_done());
    }

    #[test]
    fn a_stuck_hold_ends_at_the_failsafe() {
        let state = SplashState {
            held: true,
            ..state_at((HOLD_FAILSAFE.as_millis() as u64) + 10_000)
        };
        assert!(
            state.is_done(),
            "a held mark that never hears release() must still end"
        );
    }

    #[test]
    fn the_mark_and_its_build_line_render() {
        // Past the assemble on a large frame: full wordmark, blinking cursor,
        // build line. t sits just inside a frame bucket and just after the
        // blink base, so the cursor is on.
        let rows = draw_with(&state_at(assemble_ms() + 45), Some("glm-4.6"), 60, 16);
        let text = rows.join("\n");
        assert!(
            text.contains("s t e l l a"),
            "the tracked wordmark renders:\n{text}"
        );
        assert!(has_block_art(&rows), "the star art renders:\n{text}");
        assert!(
            text.contains(concat!("v", env!("CARGO_PKG_VERSION"))),
            "the build version renders:\n{text}"
        );
        assert!(
            text.contains("glm-4.6"),
            "the resolved model renders:\n{text}"
        );
    }

    #[test]
    fn the_assemble_builds_in_order_trails_star_letters() {
        // Early: trails only — no star, no letters.
        let early = draw(&state_at(125), 80, 24);
        assert!(has_non_space(&early), "trails render from the first phase");
        assert!(
            !early.join("\n").contains('s'),
            "no letters before the type-on:\n{}",
            early.join("\n")
        );

        // Mid: the star has popped; count the lit cells so we can see it grow.
        let mid = draw(&state_at(645), 80, 24);
        assert!(has_block_art(&mid));
        let lit = |rows: &[String]| {
            rows.iter()
                .flat_map(|r| r.chars())
                .filter(|c| matches!(c, '█' | '▀' | '▄'))
                .count()
        };
        assert!(
            lit(&mid) > lit(&early),
            "the star adds pixels over the trails alone"
        );

        // Late: all six letters have landed.
        let late = draw(&state_at(assemble_ms() + 45), 80, 24);
        assert!(late.join("\n").contains("s t e l l a"));
    }

    #[test]
    fn the_cursor_blinks_after_the_assemble() {
        let on = draw(&state_at(BLINK_BASE_MS + 45), 80, 24);
        let off = draw(&state_at(BLINK_BASE_MS + BLINK_HALF_MS + 45), 80, 24);
        assert!(on.join("\n").contains(CURSOR), "cursor on in the first half");
        assert!(
            !off.join("\n").contains(CURSOR),
            "cursor off in the second half"
        );
    }

    /// The property the whole scrub design rests on: the frame is a function
    /// of elapsed time and nothing else, so a dropped frame lands exactly
    /// where continuous playback would have.
    #[test]
    fn two_renders_at_the_same_elapsed_time_are_byte_identical() {
        for ms in [0, 45, 205, 445, 645, 1_005, assemble_ms() + 45] {
            for &(w, h) in &[(60_u16, 12_u16), (100, 30), (24, 6)] {
                let state = state_at(ms);
                let a = draw_with(&state, Some("glm-4.6"), w, h);
                let b = draw_with(&state, Some("glm-4.6"), w, h);
                assert_eq!(a, b, "frames at t={ms}ms {w}x{h} diverged");
            }
        }
    }

    #[test]
    fn a_small_frame_gets_the_one_line_lockup_not_block_art() {
        let rows = draw_with(&state_at(205), Some("glm-4.6"), 30, 5);
        let text = rows.join("\n");
        assert!(text.contains("stella"), "the compact mark renders:\n{text}");
        assert!(
            !has_block_art(&rows),
            "no half-block art on a small frame:\n{text}"
        );
    }

    #[test]
    fn the_compact_reveal_is_one_step_from_muted_to_lit() {
        assert!(!state_at(0).lit());
        assert!(state_at(REVEAL.as_millis() as u64 + 5).lit());
        // Both steps still paint the mark — the step is colour, not presence,
        // so nothing ever pops into existence.
        assert!(has_non_space(&draw(&state_at(5), 30, 5)));
        assert!(has_non_space(&draw(
            &state_at(REVEAL.as_millis() as u64 + 5),
            30,
            5
        )));
    }

    #[test]
    fn a_narrow_terminal_keeps_the_mark_and_drops_the_detail_line() {
        let rows = draw_with(&state_at(205), Some("glm-4.6"), 12, 4);
        let text = rows.join("\n");
        assert!(text.contains("stella"), "the mark survives:\n{text}");
        assert!(
            !text.contains("glm-4.6"),
            "the detail line is dropped, never wrapped:\n{text}"
        );
    }

    #[test]
    fn a_reduced_mark_is_one_static_finished_frame() {
        let mut state = state_at(5);
        state.set_reduced(true);
        assert!(!state.is_done());

        // Finished composition from the first frame: star, full wordmark —
        // and no cursor, matching the spinner's reduced-motion rule.
        let rows = draw_with(&state, Some("glm-4.6"), 80, 24);
        let text = rows.join("\n");
        assert!(has_block_art(&rows), "the star stands immediately:\n{text}");
        assert!(text.contains("s t e l l a"), "the wordmark stands:\n{text}");
        assert!(!text.contains(CURSOR), "no blinking cursor when reduced");

        // And it is static: later renders are identical.
        let mut later = state_at(700);
        later.set_reduced(true);
        assert_eq!(rows, draw_with(&later, Some("glm-4.6"), 80, 24));

        let mut over = state_at(REDUCED_TOTAL.as_millis() as u64 + 100);
        over.set_reduced(true);
        assert!(over.is_done(), "reduced mark ends after its brief hold");
    }

    #[test]
    fn a_reduced_mark_ignores_hold_cues() {
        let state = SplashState {
            held: true,
            reduced: true,
            ..state_at(REDUCED_TOTAL.as_millis() as u64 + 100)
        };
        assert!(state.is_done(), "reduced marks never wait on init");
    }

    #[test]
    fn render_does_not_panic_at_any_size_or_time() {
        let marks = [
            0,
            90,
            REVEAL.as_millis() as u64 + 5,
            305,
            645,
            assemble_ms() + 45,
            unheld_total_ms() + 200,
        ];
        for &(w, h) in &[
            (80_u16, 24_u16),
            (200, 60),
            (44, 14),
            (30, 12),
            (30, 5),
            (0, 0),
            (1, 1),
            (28, 2),
            (31, 13),
        ] {
            for &ms in &marks {
                let _ = draw_with(&state_at(ms), Some("glm-4.6"), w, h);
            }
            let mut skipped = SplashState::new();
            skipped.skip();
            let _ = draw(&skipped, w, h);
        }
    }

    #[test]
    fn a_finished_mark_renders_nothing() {
        let mut state = SplashState::new();
        state.skip();
        assert!(
            !has_non_space(&draw(&state, 80, 24)),
            "a finished mark must leave the frame to the deck"
        );
        assert!(
            !has_non_space(&draw(&state_at(unheld_total_ms() + 200), 80, 24)),
            "a played-out mark must leave the frame to the deck"
        );
    }
}
