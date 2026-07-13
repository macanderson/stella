//! Reusable [`tachyonfx`] animation building blocks for the deck.
//!
//! Every surface that wants motion — a panel's content settling into place,
//! a tab switch, a view being torn down — should reach for a constructor
//! here instead of hand-rolling a `tachyonfx::fx::*` call inline, so the
//! deck's motion language (timing, curves, which colors carry brand meaning)
//! stays consistent in one place. Colors always come from [`crate::theme`].
//!
//! [`crate::splash`] doesn't build its effects here — it needs to scrub to
//! an arbitrary point on an external `f32` timeline (so a skip lands exactly
//! where it should), which these forward-only, real-time constructors don't
//! support — but it shares the same [`apply`] plumbing to drive whatever
//! effect it builds.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use tachyonfx::{fx, Duration as FxDuration, Effect, EffectTimer, Interpolation, Motion};

use crate::theme;

/// A foreground fade-in from muted to each cell's real color, over `ms`.
///
/// Use when a panel's content just became available and should ease into
/// view rather than pop in — e.g. a view's first paint after a tab switch,
/// or a card resolving once its data arrives.
pub fn fade_in(ms: u32) -> Effect {
    fx::fade_from_fg(theme::MUTED, EffectTimer::from_ms(ms, Interpolation::QuadOut))
}

/// Scatters cells to blank over `ms`, accelerating toward empty.
///
/// Use when a panel is being replaced or torn down — a dissolve reads as
/// "this is going away," distinct from a fade which reads as "this is
/// settling in."
pub fn dissolve_out(ms: u32) -> Effect {
    fx::dissolve(EffectTimer::from_ms(ms, Interpolation::QuadIn))
}

/// A brisk amber sweep for tab / view switches in the deck shell: the new
/// content sweeps in left-to-right out of the brand accent color and lands
/// on its real style over `ms`.
pub fn tab_switch(ms: u32) -> Effect {
    fx::sweep_in(
        Motion::LeftToRight,
        10,
        3,
        theme::AMBER_DEEP,
        EffectTimer::from_ms(ms, Interpolation::CircOut),
    )
}

/// Advances `effect` by `dt` and renders the result into `buf` within
/// `area`. Thin wrapper over [`tachyonfx::Effect::process`] so call sites
/// don't need to import `tachyonfx` (or convert its `Duration` type) just to
/// drive one effect forward a frame.
pub fn apply(effect: &mut Effect, dt: std::time::Duration, area: Rect, buf: &mut Buffer) {
    effect.process(FxDuration::from(dt), buf, area);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ratatui::style::{Color, Style};
    use ratatui::text::Line;
    use ratatui::widgets::{Paragraph, Widget};

    use super::*;

    fn painted_buffer(area: Rect) -> Buffer {
        let mut buf = Buffer::empty(area);
        Paragraph::new(Line::from("STELLA COMMAND DECK").style(Style::default().fg(Color::White)))
            .render(area, &mut buf);
        buf
    }

    fn non_space_cells(buf: &Buffer) -> usize {
        let area = *buf.area();
        (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .filter(|&(x, y)| buf.cell((x, y)).is_some_and(|c| c.symbol() != " "))
            .count()
    }

    #[test]
    fn fade_in_runs_to_completion_and_does_not_panic() {
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = painted_buffer(area);
        let mut effect = fade_in(100);

        assert!(!effect.done(), "a fresh 100ms effect has not finished");
        apply(&mut effect, Duration::from_millis(50), area, &mut buf);
        assert!(!effect.done(), "halfway through a 100ms fade should still be running");
        apply(&mut effect, Duration::from_millis(200), area, &mut buf);
        assert!(effect.done(), "overshooting the duration should finish the effect");
    }

    #[test]
    fn dissolve_out_clears_cells_toward_blank() {
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = painted_buffer(area);
        let before = non_space_cells(&buf);
        assert!(before > 0, "fixture text should paint some cells");

        let mut effect = dissolve_out(50);
        // Drive well past the effect's own duration so it settles fully
        // dissolved regardless of its internal random cell ordering.
        apply(&mut effect, Duration::from_millis(500), area, &mut buf);

        assert_eq!(non_space_cells(&buf), 0, "a fully-run dissolve blanks every cell");
    }

    #[test]
    fn tab_switch_processes_without_panicking_on_a_realistic_area() {
        let area = Rect::new(0, 0, 60, 12);
        let mut buf = painted_buffer(area);
        let mut effect = tab_switch(150);

        for _ in 0..5 {
            apply(&mut effect, Duration::from_millis(40), area, &mut buf);
        }
        assert!(effect.done() || effect.running());
    }

    #[test]
    fn apply_is_a_no_op_on_a_zero_area() {
        let area = Rect::new(0, 0, 0, 0);
        let mut buf = Buffer::empty(area);
        let mut effect = fade_in(100);
        apply(&mut effect, Duration::from_millis(10), area, &mut buf);
    }
}
