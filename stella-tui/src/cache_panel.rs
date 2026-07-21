//! The deck's cache-economics panel: the derived accessors and the pure
//! text formatters behind the statline's CACHE / SAVED / WARMTH cells
//! (issues #267 and #269).
//!
//! Presentation only — the pricing and TTL math already happened upstream in
//! the pricing-aware CLI producer (see [`crate::envelope::Inbound::CacheInsight`])
//! and was folded into each [`AgentEntry`]; this module reads those folded
//! aggregates and turns them into the compact strings the deck renders. Kept
//! out of `deck.rs` to keep that file under its size ratchet, and out of
//! `deck_render.rs` so the formatting is unit-testable without a full frame.

use ratatui::style::Style;
use ratatui::text::Span;

use crate::deck::{AgentEntry, WorkspaceModel};
use crate::deck_render::fmt_tokens;
use crate::theme;

impl AgentEntry {
    /// Seconds of prompt-cache warmth remaining: how long until this agent's
    /// cached prefix expires, from its provider TTL minus the idle since the
    /// last metered call. `None` when the provider has no prompt cache
    /// (`cache_ttl_secs == 0`) or no call has landed yet — nothing to preserve.
    /// `Some(0)` means the prefix has already gone cold (the next turn rewrites
    /// it). Saturating, mirroring `stella_model::CacheWarmth::from_elapsed`,
    /// which the pricing-aware producer computes upstream; the deck cannot link
    /// that model-tier crate, so it re-derives the trivial countdown here.
    pub fn cache_warmth_secs(&self, now_ms: u64) -> Option<u64> {
        let last = self.last_provider_call_ms?;
        if self.cache_ttl_secs == 0 {
            return None;
        }
        let elapsed_secs = now_ms.saturating_sub(last) / 1000;
        Some(self.cache_ttl_secs.saturating_sub(elapsed_secs))
    }
}

impl WorkspaceModel {
    /// Cumulative prompt-cache *write* tokens across all agents — the write
    /// volume the cache panel shows next to the reads.
    pub fn total_cache_write_tokens(&self) -> u64 {
        self.agents.iter().map(|a| a.cache_write_tokens).sum()
    }

    /// Cumulative estimated USD saved by prompt caching across all agents.
    /// Signed: negative when the write premium outran the reads it bought.
    pub fn total_cache_savings_usd(&self) -> f64 {
        self.agents.iter().map(|a| a.cache_savings_usd).sum()
    }
}

/// Cache-hit percentage (0–100, rounded) for the session, or `None` before any
/// input is metered — the panel shows `—` for `None`, never a divide-by-zero.
pub fn hit_pct(cache_read: u64, total_input: u64) -> Option<u32> {
    if total_input == 0 {
        return None;
    }
    Some(
        ((cache_read as f64 / total_input as f64) * 100.0)
            .round()
            .clamp(0.0, 100.0) as u32,
    )
}

/// Format session cache savings as a signed dollar figure: `$1.23` saved, or
/// `-$0.38` when the write premium outran the reads it bought (the low-hit
/// incident worth surfacing — never hidden behind a clamp).
pub fn fmt_savings(savings_usd: f64) -> String {
    if savings_usd < 0.0 {
        format!("-${:.2}", -savings_usd)
    } else {
        format!("${savings_usd:.2}")
    }
}

/// Format remaining cache warmth as a compact countdown: `m:ss` while warm
/// (`4:12`), `cold` once the prefix has expired, `—` when there is no warm
/// prefix to preserve (no TTL, or no call yet).
pub fn fmt_warmth(remaining_secs: Option<u64>) -> String {
    match remaining_secs {
        None => "—".to_string(),
        Some(0) => "cold".to_string(),
        Some(s) => format!("{}:{:02}", s / 60, s % 60),
    }
}

// ── Statline cell span builders ─────────────────────────────────────────────
//
// Each returns the `Span`s for one statline cell, so `deck_render` stays thin
// (and under its size ratchet) and the styling lives next to the formatting it
// dresses. Colors come from [`crate::theme`], matching the surrounding cells.

/// CACHE cell: hit% then the compact read/write token volumes behind it, or the
/// no-data dash before any input is metered.
pub fn cache_cell(cache_read: u64, cache_write: u64, total_input: u64) -> Vec<Span<'static>> {
    let val = Style::default().fg(theme::TEXT_PRIMARY);
    match hit_pct(cache_read, total_input) {
        None => vec![Span::styled("—", val)],
        Some(pct) => vec![
            Span::styled(format!("{pct}%"), val),
            Span::styled(
                format!(
                    " ({} rd · {} wr)",
                    fmt_tokens(cache_read),
                    fmt_tokens(cache_write)
                ),
                Style::default().fg(theme::TEXT_TERTIARY),
            ),
        ],
    }
}

/// SAVED cell: session dollars saved by caching, danger-colored when the write
/// premium outran the reads (the low-hit incident). `metered` gates the dash —
/// `false` (no input yet) shows `—`, never a misleading `$0.00`.
pub fn saved_cell(savings_usd: f64, metered: bool) -> Vec<Span<'static>> {
    if !metered {
        return vec![Span::styled("—", Style::default().fg(theme::TEXT_PRIMARY))];
    }
    let color = if savings_usd < 0.0 {
        theme::DANGER_BRIGHT
    } else {
        theme::SUCCESS_BRIGHT
    };
    vec![Span::styled(
        fmt_savings(savings_usd),
        Style::default().fg(color),
    )]
}

/// WARMTH cell: countdown until the focused agent's cached prefix expires —
/// danger once cold, warning under a minute (about to cool), success while
/// comfortably warm, dim `—` when there is no warm prefix to preserve.
pub fn warmth_cell(remaining_secs: Option<u64>) -> Vec<Span<'static>> {
    let color = match remaining_secs {
        Some(0) => theme::DANGER_BRIGHT,
        Some(s) if s < 60 => theme::WARNING_BRIGHT,
        Some(_) => theme::SUCCESS_BRIGHT,
        None => theme::TEXT_TERTIARY,
    };
    vec![Span::styled(
        fmt_warmth(remaining_secs),
        Style::default().fg(color),
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_pct_is_none_before_input_and_rounds_within_bounds() {
        assert_eq!(hit_pct(0, 0), None);
        assert_eq!(hit_pct(500, 1_000), Some(50));
        assert_eq!(hit_pct(2, 3), Some(67)); // rounds
        // Defensive clamp: cached over total never exceeds 100%.
        assert_eq!(hit_pct(2_000, 1_000), Some(100));
    }

    #[test]
    fn savings_shows_sign_and_two_places() {
        assert_eq!(fmt_savings(1.234), "$1.23");
        assert_eq!(fmt_savings(0.0), "$0.00");
        // The negative case is the whole point — never clamped to $0.00.
        assert_eq!(fmt_savings(-0.375), "-$0.38");
    }

    #[test]
    fn warmth_countdown_reads_cold_at_zero_and_dash_without_a_prefix() {
        assert_eq!(fmt_warmth(None), "—");
        assert_eq!(fmt_warmth(Some(0)), "cold");
        assert_eq!(fmt_warmth(Some(252)), "4:12");
        assert_eq!(fmt_warmth(Some(9)), "0:09"); // zero-padded seconds
    }
}
