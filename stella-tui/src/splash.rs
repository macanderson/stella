//! The launch mark: one still frame, shown only while session init is
//! genuinely still running.
//!
//! This is deliberately **not** a cinematic. A terminal has no
//! `prefers-reduced-motion`, so the only accessible default is not to animate;
//! and a startup animation is time the user did not ask to spend. The mark
//! therefore earns its screen time by standing in for work that is actually
//! happening: the deck holds it open across session init (code-graph build +
//! MCP connect) and drops it the instant [`SplashState::release`] says init is
//! done — even mid-fade. If there is no init to cover, it is gone in well
//! under a second.
//!
//! What it draws is the comet identity's lockup (`docs/brand/BRAND.md`): the
//! chevron, the `stella` wordmark, and the gold block cursor — plus the build
//! version and the resolved model once one is known. Colours come from
//! [`crate::theme`]'s semantic roles, never from raw palette constants, so
//! the mark follows the identity rather than pinning a copy of it.
//!
//! The one presentational concession is a **single** fade step: for the first
//! [`REVEAL`] the mark renders muted, then in its real colours. One step, not
//! a per-cell choreography — and it is a pure function of elapsed time, so two
//! renders at the same `t` produce byte-identical buffers and a dropped frame
//! scrubs correctly instead of stalling a timeline.

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::theme;

/// How long the mark renders muted before stepping to its real colours. One
/// step, and short enough that a fast init cuts through it (see
/// [`SplashState::release`]) rather than being made to wait for it.
pub const REVEAL: Duration = Duration::from_millis(180);

/// How long an **unheld** splash stays up after the reveal step. An unheld
/// splash is covering nothing, so this is the whole of its life past
/// [`REVEAL`] — long enough to read the mark, short enough not to be a wait.
const UNHELD_HOLD: Duration = Duration::from_millis(420);

/// A held splash that never hears `release()` (a driver that died mid-init)
/// still ends: past this cap it hands off regardless. Any key skips long
/// before this matters.
const HOLD_FAILSAFE: Duration = Duration::from_secs(120);

/// `--no-anim` / `STELLA_NO_ANIM` / `NO_COLOR`: the mark is lit from the first
/// frame (no reveal step) and stands for exactly this long. A recording gets
/// one static frame rather than a transition.
const REDUCED_TOTAL: Duration = Duration::from_millis(900);

/// Narrower than this and the version/model line has nowhere to go — the mark
/// renders alone rather than wrapping into nonsense.
const MIN_DETAIL_WIDTH: u16 = 28;

/// The lockup, left to right: chevron · wordmark · block cursor.
const CHEVRON: &str = "› ";
const WORDMARK: &str = "stella";
const CURSOR: &str = "▮";

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
    /// `--no-anim`: no reveal step, one static frame, ignore hold cues.
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

    /// Init finished — hand off to the deck **now**.
    ///
    /// Deliberately not floored by any minimum screen time: a fast init must
    /// cut straight to a usable deck, not be made to sit out the rest of a
    /// reveal. Idempotent; a no-op on an unheld splash (which is covering
    /// nothing and ends on its own).
    pub fn release(&mut self) {
        if self.held && self.released_at.is_none() {
            self.released_at = Some(Instant::now());
        }
    }

    /// `--no-anim` / `STELLA_NO_ANIM` / `NO_COLOR`: light the mark from the
    /// first frame and hold it briefly, with no reveal step.
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
            return Some(REVEAL + UNHELD_HOLD);
        }
        self.released_at.map(|at| at.duration_since(self.start))
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

    /// Whether the reveal step has landed — the mark renders muted before it
    /// and in its real colours after. A reduced splash is lit immediately.
    fn lit(&self) -> bool {
        self.reduced || self.elapsed() >= REVEAL
    }
}

/// Draw the launch mark. `model` is the resolved model id once the session
/// knows one; it is omitted rather than guessed at when it does not.
///
/// A pure function of `(state.elapsed(), state flags, area, model)` — two calls
/// at the same elapsed time write byte-identical buffers.
pub fn render(state: &SplashState, model: Option<&str>, area: Rect, buf: &mut Buffer) {
    if state.is_done() || area.width == 0 || area.height == 0 {
        return;
    }
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
    fn state_at(elapsed_ms: u64) -> SplashState {
        SplashState {
            start: Instant::now() - Duration::from_millis(elapsed_ms),
            ..SplashState::new()
        }
    }

    fn unheld_total_ms() -> u64 {
        (REVEAL + UNHELD_HOLD).as_millis() as u64
    }

    #[test]
    fn an_unheld_mark_is_gone_in_under_a_second() {
        // It covers no work, so it may not cost the user real time.
        assert!(
            unheld_total_ms() < 1_000,
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
            "release hands off at once — a fast init must not wait on the mark"
        );
    }

    /// The behaviour the old cinematic got backwards: an init that finishes
    /// before the reveal step lands must cut straight to the deck rather than
    /// hold the user on an animation with nothing behind it.
    #[test]
    fn an_early_release_cuts_straight_through_the_reveal() {
        let mut state = SplashState {
            held: true,
            ..state_at(20)
        };
        assert!(!state.lit(), "still inside the reveal step");
        state.release();
        assert!(state.is_done());
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
        let rows = draw_with(
            &state_at(REVEAL.as_millis() as u64 + 10),
            Some("glm-4.6"),
            60,
            12,
        );
        let text = rows.join("\n");
        assert!(text.contains("stella"), "the wordmark renders:\n{text}");
        assert!(text.contains('▮'), "the block cursor renders:\n{text}");
        assert!(
            text.contains(concat!("v", env!("CARGO_PKG_VERSION"))),
            "the build version renders:\n{text}"
        );
        assert!(
            text.contains("glm-4.6"),
            "the resolved model renders:\n{text}"
        );
    }

    /// The property the whole scrub design rests on: the frame is a function
    /// of elapsed time and nothing else, so a dropped frame lands exactly
    /// where continuous playback would have.
    #[test]
    fn two_renders_at_the_same_elapsed_time_are_byte_identical() {
        for ms in [0, 50, REVEAL.as_millis() as u64 + 5, 400] {
            let state = state_at(ms);
            let a = draw_with(&state, Some("glm-4.6"), 60, 12);
            let b = draw_with(&state, Some("glm-4.6"), 60, 12);
            assert_eq!(a, b, "frames at t={ms}ms diverged");
        }
    }

    #[test]
    fn the_reveal_is_one_step_from_muted_to_lit() {
        assert!(!state_at(0).lit());
        assert!(state_at(REVEAL.as_millis() as u64 + 1).lit());
        // Both steps still paint the mark — the step is colour, not presence,
        // so nothing ever pops into existence.
        assert!(has_non_space(&draw(&state_at(0), 60, 12)));
        assert!(has_non_space(&draw(
            &state_at(REVEAL.as_millis() as u64 + 1),
            60,
            12
        )));
    }

    #[test]
    fn a_narrow_terminal_keeps_the_mark_and_drops_the_detail_line() {
        let rows = draw_with(&state_at(200), Some("glm-4.6"), 12, 4);
        let text = rows.join("\n");
        assert!(text.contains("stella"), "the mark survives:\n{text}");
        assert!(
            !text.contains("glm-4.6"),
            "the detail line is dropped, never wrapped:\n{text}"
        );
    }

    #[test]
    fn a_reduced_mark_is_lit_from_the_first_frame() {
        let mut state = state_at(0);
        state.set_reduced(true);
        assert!(state.lit(), "no reveal step under --no-anim");
        assert!(!state.is_done());

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
            unheld_total_ms() + 200,
        ];
        for &(w, h) in &[(80_u16, 24_u16), (44, 14), (30, 5), (0, 0), (1, 1), (28, 2)] {
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
