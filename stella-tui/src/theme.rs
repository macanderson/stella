//! The one place the deck's look is defined — colors, semantic styles, and
//! glyphs. Every view pulls from here so the deck reads as one system in both
//! the Stella brand palette and its status semantics. No view hard-codes a
//! color; that is what keeps a 12-panel TUI feeling designed rather than
//! assembled.

use ratatui::style::{Color, Modifier, Style};

use crate::envelope::AgentStatus;

// ── Brand + neutrals ────────────────────────────────────────────────────────

/// Stella brand amber — the single accent color (`#FFAC26`).
pub const AMBER: Color = Color::Rgb(255, 172, 38);
/// A deeper amber for gradients / pressed states.
pub const AMBER_DEEP: Color = Color::Rgb(214, 137, 16);
/// Near-white primary text.
pub const INK: Color = Color::Rgb(235, 237, 240);
/// Dimmed secondary text.
pub const MUTED: Color = Color::Rgb(140, 146, 156);
/// Panel border / rule.
pub const RULE: Color = Color::Rgb(58, 62, 70);

// ── Semantic ────────────────────────────────────────────────────────────────

/// Success / positive / added lines.
pub const OK: Color = Color::Rgb(126, 211, 128);
/// Warning / needs-input.
pub const WARN: Color = Color::Rgb(240, 189, 79);
/// Error / removed lines / failure.
pub const BAD: Color = Color::Rgb(240, 113, 120);
/// Running accent (cyan) — matches the "Processing" look of the reference UI.
pub const RUN: Color = Color::Rgb(96, 191, 214);
/// Paused / held (violet).
pub const HELD: Color = Color::Rgb(180, 142, 214);

// ── Styles ──────────────────────────────────────────────────────────────────

/// Accent style for headings / the active tab.
pub fn accent() -> Style {
    Style::default().fg(AMBER).add_modifier(Modifier::BOLD)
}
pub fn heading() -> Style {
    Style::default().fg(INK).add_modifier(Modifier::BOLD)
}
pub fn muted() -> Style {
    Style::default().fg(MUTED)
}
pub fn body() -> Style {
    Style::default().fg(INK)
}
pub fn rule() -> Style {
    Style::default().fg(RULE)
}

// ── Status → color / glyph ──────────────────────────────────────────────────

/// A color per agent lifecycle status (dashboard, traces, session HUD).
pub fn status_color(status: AgentStatus) -> Color {
    match status {
        AgentStatus::Queued => MUTED,
        AgentStatus::Running => RUN,
        AgentStatus::Paused => HELD,
        AgentStatus::WaitingInput => WARN,
        AgentStatus::Done => OK,
        AgentStatus::Failed => BAD,
        AgentStatus::Killed => BAD,
    }
}

/// A compact status glyph.
pub fn status_glyph(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Queued => "◦",
        AgentStatus::Running => "▶",
        AgentStatus::Paused => "⏸",
        AgentStatus::WaitingInput => "?",
        AgentStatus::Done => "✓",
        AgentStatus::Failed => "✗",
        AgentStatus::Killed => "◼",
    }
}

// ── Gauges + sparklines ─────────────────────────────────────────────────────

/// A color ramp for a CPU / budget gauge by utilization fraction `[0.0, 1.0]`:
/// green under load, amber approaching the limit, red at/over it.
pub fn gauge_color(fraction: f64) -> Color {
    if fraction >= 0.85 {
        BAD
    } else if fraction >= 0.6 {
        WARN
    } else {
        OK
    }
}

/// Sparkline / bar-gauge glyphs, empty → full (8 levels).
pub const SPARK_BARS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Map an intensity in `[0, 255]` to one of the [`SPARK_BARS`] glyphs.
pub fn spark_glyph(intensity: u8) -> char {
    let idx = ((intensity as usize) * (SPARK_BARS.len() - 1)) / 255;
    SPARK_BARS[idx.min(SPARK_BARS.len() - 1)]
}
