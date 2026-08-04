// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The vitals row: context · cpu · cache · spend, as gauges.
//!
//! # Why gauges and not numbers
//!
//! The statline carries the same four figures as text — `ctx 35k/200k · cpu
//! 73% · cache 85%`. Text is precise and it is the wrong instrument for this
//! job: reading it costs a fixation per figure, and the question a running
//! agent gets glanced at with is not *what is the context* but *is anything
//! about to run out*. That is a question about **proportion**, and a bar
//! answers it without being read.
//!
//! So the row is deliberately redundant with the statline rather than a
//! replacement for it. The bar is for the glance; the figure beside it is for
//! the moment the glance found something.
//!
//! # Health, not fill
//!
//! Each gauge is coloured by what its reading *means*, which is not the same
//! direction for all four. A full context bar is bad and a full cache bar is
//! good, so [`Vital::tone`] takes the direction from the gauge rather than
//! from the number — a cockpit where green means "high" on one dial and "low"
//! on the next is a cockpit that has to be read instead of seen.
//!
//! Pure: this module builds spans. No ratatui layout, no buffer — so the
//! thresholds and the drop order are unit-testable without a terminal, the
//! same discipline as [`crate::proof`] and [`crate::plan`].

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::theme;

/// Cells one gauge's bar spans. Eight is the smallest width where a reader can
/// distinguish thirds at a glance, which is the resolution the question
/// ("fine / getting full / out") actually needs.
pub const GAUGE_W: usize = 8;

/// Which way a rising reading points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sense {
    /// More is worse — context consumed, cpu load, budget spent.
    Consumption,
    /// More is better — cache hit rate.
    Efficiency,
}

/// One gauge: its label, where it sits on 0–100, and the figure that spells the
/// reading out.
#[derive(Debug, Clone, PartialEq)]
pub struct Vital {
    /// Short lowercase label — `ctx`, `cpu`, `cache`, `spend`.
    pub key: &'static str,
    /// The reading, 0–100.
    pub pct: u32,
    /// The exact figure, rendered beside the bar (`35k/200k`, `73%`).
    pub figure: String,
    pub sense: Sense,
}

impl Vital {
    /// The gauge's health colour.
    ///
    /// The bands are two thirds and six sevenths of the dial, chosen so a
    /// gauge turns amber while there is still room to do something about it —
    /// a warning that arrives at 95% is an obituary.
    pub fn tone(&self) -> ratatui::style::Color {
        let severity = match self.sense {
            Sense::Consumption => self.pct,
            // An efficiency dial is read from the other end: a 20% cache hit
            // rate is the same amount of bad news as 80% context used.
            Sense::Efficiency => 100u32.saturating_sub(self.pct),
        };
        match severity {
            0..=66 => theme::OK,
            67..=85 => theme::WARN,
            _ => theme::BAD,
        }
    }

    /// `label ▰▰▰▰▱▱▱▱ figure`, as styled spans.
    pub fn spans(&self) -> Vec<Span<'static>> {
        let dim = Style::new().fg(theme::TEXT_TERTIARY);
        let color = self.tone();
        let filled = ((self.pct as usize * GAUGE_W) / 100).min(GAUGE_W);
        vec![
            Span::styled(format!("{} ", self.key), dim),
            Span::styled(
                "▰".repeat(filled),
                Style::new().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled("▱".repeat(GAUGE_W - filled), dim),
            Span::styled(format!(" {}", self.figure), Style::new().fg(color)),
        ]
    }

    /// Display columns this gauge occupies, `spans()` included.
    pub fn cols(&self) -> usize {
        self.key.chars().count() + 1 + GAUGE_W + 1 + self.figure.chars().count()
    }
}

/// The four readings for one frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Vitals {
    pub context_tokens: u64,
    pub context_window: u64,
    pub cpu_pct: u32,
    /// `None` before any model call has reported input tokens — an unknown
    /// cache rate is not a zero one, and a dial pinned at empty would claim a
    /// catastrophe that has not happened.
    pub cache_hit_pct: Option<u32>,
    pub spent_usd: f64,
    pub limit_usd: Option<f64>,
}

impl Vitals {
    /// Read this frame's vitals off the deck.
    ///
    /// Lives here rather than at the call site because *which* number answers
    /// "how full is the context" is vitals knowledge, not layout knowledge —
    /// the session view should ask for the readings, not know how to take
    /// them.
    pub fn read(model: &crate::deck::WorkspaceModel, agent: &crate::deck::AgentEntry) -> Self {
        Self {
            context_tokens: agent.context_tokens,
            context_window: crate::statline::CTX_WINDOW,
            cpu_pct: model.global_cpu_pct.round().max(0.0) as u32,
            cache_hit_pct: crate::cache_panel::hit_pct(
                model.cache_hit_tokens(),
                model.total_input_tokens(),
            ),
            spent_usd: agent.model.hud.turn_spent_usd(),
            limit_usd: agent.model.hud.limit_usd,
        }
    }

    /// The gauges, in priority order: the ones later in the list are the ones
    /// dropped first when the row runs out of columns.
    ///
    /// Context leads because it is the reading that ends turns. Spend is next
    /// because it is the one that costs money. Cpu and cache are diagnostics —
    /// interesting, never urgent.
    pub fn gauges(&self) -> Vec<Vital> {
        let mut out = Vec::new();
        if self.context_window > 0 {
            let pct = ((self.context_tokens as f64 / self.context_window as f64) * 100.0)
                .round()
                .clamp(0.0, 100.0) as u32;
            out.push(Vital {
                key: "ctx",
                pct,
                figure: format!(
                    "{}/{}",
                    fmt_k(self.context_tokens),
                    fmt_k(self.context_window)
                ),
                sense: Sense::Consumption,
            });
        }
        // Only a *capped* run has a spend proportion. Without a cap there is
        // no dial to draw — a bar against an unknown limit is a made-up
        // number, and the statline already carries the raw figure.
        if let Some(limit) = self.limit_usd.filter(|l| *l > 0.0) {
            let pct = ((self.spent_usd / limit) * 100.0).round().clamp(0.0, 100.0) as u32;
            out.push(Vital {
                key: "spend",
                pct,
                figure: format!("${:.2}/${limit:.2}", self.spent_usd),
                sense: Sense::Consumption,
            });
        }
        if let Some(hit) = self.cache_hit_pct {
            out.push(Vital {
                key: "cache",
                pct: hit,
                figure: format!("{hit}%"),
                sense: Sense::Efficiency,
            });
        }
        out.push(Vital {
            key: "cpu",
            pct: self.cpu_pct.min(100),
            figure: format!("{}%", self.cpu_pct.min(100)),
            sense: Sense::Consumption,
        });
        out
    }

    /// The vitals row for `width` columns: as many gauges as fit, in priority
    /// order, separated by a wide gap.
    ///
    /// Gauges drop **whole**, never elided. Half a bar is not a smaller bar, it
    /// is a bar reading a different number — the same rule the statline's zones
    /// follow.
    pub fn row(&self, width: usize) -> Vec<Span<'static>> {
        const GAP: &str = "   ";
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut used = 0usize;
        for gauge in self.gauges() {
            let cost = gauge.cols() + if spans.is_empty() { 0 } else { GAP.len() };
            if used + cost > width {
                continue;
            }
            if !spans.is_empty() {
                spans.push(Span::raw(GAP));
            }
            spans.extend(gauge.spans());
            used += cost;
        }
        spans
    }
}

/// `35k`, `1.2M`, `842` — the same compact form the statline uses.
fn fmt_k(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{}k", n / 1_000),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(spans: &[Span<'static>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn full() -> Vitals {
        Vitals {
            context_tokens: 35_000,
            context_window: 200_000,
            cpu_pct: 38,
            cache_hit_pct: Some(85),
            spent_usd: 0.42,
            limit_usd: Some(2.50),
        }
    }

    /// The row as a reader sees it. A golden, because the value of this surface
    /// is that four readings land in one glance — a change that makes it wordy
    /// is a regression no per-gauge assertion would catch.
    #[test]
    fn the_row_reads_as_one_glance() {
        assert_eq!(
            text(&full().row(120)),
            "ctx ▰▱▱▱▱▱▱▱ 35k/200k   \
             spend ▰▱▱▱▱▱▱▱ $0.42/$2.50   \
             cache ▰▰▰▰▰▰▱▱ 85%   \
             cpu ▰▰▰▱▱▱▱▱ 38%"
        );
    }

    /// **The direction rule.** A full cache bar is good news and a full context
    /// bar is bad news, so the same fill cannot mean the same thing.
    #[test]
    fn a_rising_reading_is_not_always_a_worsening_one() {
        let hot_context = Vital {
            key: "ctx",
            pct: 92,
            figure: "184k/200k".into(),
            sense: Sense::Consumption,
        };
        let hot_cache = Vital {
            key: "cache",
            pct: 92,
            figure: "92%".into(),
            sense: Sense::Efficiency,
        };
        assert_eq!(hot_context.tone(), theme::BAD);
        assert_eq!(
            hot_cache.tone(),
            theme::OK,
            "a full cache dial is good news"
        );

        let cold_cache = Vital {
            pct: 8,
            ..hot_cache
        };
        assert_eq!(cold_cache.tone(), theme::BAD, "…and an empty one is not");
    }

    /// The warning has to arrive while there is still room to act on it.
    #[test]
    fn a_gauge_turns_amber_before_it_turns_red() {
        let at = |pct| {
            Vital {
                key: "ctx",
                pct,
                figure: String::new(),
                sense: Sense::Consumption,
            }
            .tone()
        };
        assert_eq!(at(50), theme::OK);
        assert_eq!(at(70), theme::WARN, "amber with a third of the dial left");
        assert_eq!(at(95), theme::BAD);
    }

    /// Gauges drop whole. A clipped bar is not a shorter bar, it is a bar
    /// reading a different number.
    #[test]
    fn a_narrow_row_drops_whole_gauges_in_priority_order() {
        let v = full();
        let wide = text(&v.row(120));
        assert!(wide.contains("cpu"), "{wide}");

        let narrow = text(&v.row(30));
        assert!(
            narrow.contains("ctx"),
            "context is the reading that ends turns, so it survives: {narrow}"
        );
        assert!(!narrow.contains("cpu"), "cpu is a diagnostic: {narrow}");
        assert!(
            narrow.chars().count() <= 30,
            "the row overflowed: {narrow:?}"
        );
    }

    #[test]
    fn every_width_fits_inside_itself() {
        let v = full();
        for width in 0..140usize {
            let row = text(&v.row(width));
            assert!(
                row.chars().count() <= width,
                "width {width} overflowed by {}: {row:?}",
                row.chars().count() - width
            );
        }
    }

    /// An uncapped run has no spend *proportion*, so it gets no spend dial — a
    /// bar against an unknown limit is a made-up number.
    #[test]
    fn an_uncapped_run_draws_no_spend_gauge() {
        let v = Vitals {
            limit_usd: None,
            ..full()
        };
        assert!(!text(&v.row(120)).contains("spend"));
    }

    /// An unreported cache rate is unknown, not zero — and a dial pinned at
    /// empty would claim a catastrophe that has not happened.
    #[test]
    fn an_unreported_cache_rate_draws_no_gauge_rather_than_an_empty_one() {
        let v = Vitals {
            cache_hit_pct: None,
            ..full()
        };
        let row = text(&v.row(120));
        assert!(!row.contains("cache"), "{row}");
        assert!(row.contains("ctx"), "the others are unaffected: {row}");
    }

    #[test]
    fn a_session_with_no_readings_yet_draws_only_what_it_knows() {
        let row = text(&Vitals::default().row(120));
        // cpu is always known (it is sampled, not reported), the rest are not.
        assert!(row.contains("cpu"), "{row}");
        assert!(!row.contains("ctx"), "{row}");
    }
}
