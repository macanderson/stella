//! The animated branded splash shown on deck launch.
//!
//! Timing/state lives here; the wordmark (tui-big-text) and the dissolve
//! (tachyonfx) are drawn in [`render`]. The splash is **time-boxed** and
//! **skippable** on any key so it can never block getting to work.

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Widget};

use crate::theme;

/// Total on-screen time before the splash auto-dismisses.
pub const SPLASH_DURATION: Duration = Duration::from_millis(1600);

/// Ephemeral splash timing. Not part of the model — pure presentation.
#[derive(Debug, Clone)]
pub struct SplashState {
    start: Instant,
    skipped: bool,
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
            skipped: false,
        }
    }

    /// Dismiss immediately (any key).
    pub fn skip(&mut self) {
        self.skipped = true;
    }

    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// Progress `0.0..=1.0` through the splash timeline.
    pub fn progress(&self) -> f32 {
        if self.skipped {
            return 1.0;
        }
        (self.elapsed().as_secs_f32() / SPLASH_DURATION.as_secs_f32()).clamp(0.0, 1.0)
    }

    /// True once the splash should hand off to the deck.
    pub fn is_done(&self) -> bool {
        self.skipped || self.elapsed() >= SPLASH_DURATION
    }
}

/// Draw the splash. Placeholder implementation — a centered branded wordmark;
/// upgraded to a `tui-big-text` STELLA wordmark with a `tachyonfx` coalesce/
/// dissolve keyed off [`SplashState::progress`].
pub fn render(state: &SplashState, area: Rect, buf: &mut Buffer) {
    let p = state.progress();
    // Simple fade proxy until tachyonfx lands: brighten the accent as we go.
    let title = Line::from(vec![Span::styled("✦ STELLA", theme::accent())]).alignment(Alignment::Center);
    let subtitle =
        Line::from(vec![Span::styled("command deck", theme::muted())]).alignment(Alignment::Center);
    let dots = ((p * 4.0) as usize).min(3);
    let warming = Line::from(vec![Span::styled(
        format!("warming up{}", ".".repeat(dots)),
        theme::muted(),
    )])
    .alignment(Alignment::Center);

    let mid = area.height / 2;
    let body = Text::from(vec![
        Line::default(),
        title,
        subtitle,
        Line::default(),
        warming,
    ]);
    let inner = Rect {
        x: area.x,
        y: area.y + mid.saturating_sub(2),
        width: area.width,
        height: area.height.saturating_sub(mid.saturating_sub(2)),
    };
    Paragraph::new(body).render(inner, buf);
}
