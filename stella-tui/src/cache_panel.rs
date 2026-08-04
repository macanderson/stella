//! The deck's cache-economics panel: the derived accessors and the pure
//! text formatters behind the statline's cache hit-rate / saved items and
//! the context overlay's cache detail (issues #267 and #269).
//!
//! Presentation only — the pricing and TTL math already happened upstream in
//! the pricing-aware CLI producer (see [`crate::envelope::Inbound::CacheInsight`])
//! and was folded into each [`AgentEntry`]; this module reads those folded
//! aggregates and turns them into the compact strings the deck renders. Kept
//! out of `deck.rs` to keep that file small, and out of
//! `deck_render.rs` so the formatting is unit-testable without a full frame.

use ratatui::style::Style;
use ratatui::text::Span;
use stella_protocol::CacheCause;

use crate::deck::{AgentEntry, WorkspaceModel};
use crate::theme;

/// Below this hit rate (with enough calls to have established a cache to
/// hit) the deck names a probable cause — matches
/// `stella_model::cache_economics::diagnose_cache`'s own "~20%" acceptance
/// bar and `stella-cli`'s `stats.rs::LOW_HIT_RATE_THRESHOLD`, so a session
/// reads the same diagnosis live in the deck as it does afterward in
/// `stella stats`.
pub const LOW_HIT_RATE_THRESHOLD: f64 = 0.20;

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

    /// The probable cause of this agent's abnormally low cache hit rate, or
    /// `None` when there's nothing to diagnose (too few calls yet, or a
    /// healthy hit rate) — the motivating incident is a session sitting at
    /// CACHE 0% with no hint anywhere on screen. Mirrors
    /// `stella_model::cache_economics::diagnose_cache`'s selection logic
    /// rather than calling it (the deck cannot link that model-tier crate),
    /// using only locally-tracked aggregates plus
    /// [`Self::cache_is_opt_in_provider`], which the pricing-aware CLI
    /// producer resolves once from the provider's cache-posture table and
    /// folds in via `CacheInsight` — the one piece of real domain knowledge
    /// this re-derivation would otherwise have to duplicate.
    pub fn cache_diagnosis(&self, threshold: f64) -> Option<CacheCause> {
        const MIN_TURNS: u64 = 3;
        if self.cache_call_count <= MIN_TURNS {
            return None;
        }
        let hit_rate = if self.tokens_in == 0 {
            0.0
        } else {
            (self.cache_read_tokens as f64 / self.tokens_in as f64).clamp(0.0, 1.0)
        };
        if hit_rate >= threshold {
            return None;
        }
        // A READ is proof the cache engaged: nothing can be read that was
        // never written. Writes and reads land on different turns — once a
        // prefix is cached, every later turn reads it and writes nothing — so
        // `cache_write_tokens == 0` is the NORMAL shape of a warm cache, not
        // evidence of a missing marker.
        //
        // Diagnosing off writes alone put "opt-in never engaged — likely a
        // bug" on a statline that was simultaneously reporting 9.6K read
        // tokens and real savings. A warning that contradicts the number
        // printed beside it is worse than silence: it sends the reader
        // hunting for a defect that is not there, and teaches them to
        // disregard the next one.
        if self.cache_is_opt_in_provider
            && self.cache_write_tokens == 0
            && self.cache_read_tokens == 0
        {
            return Some(CacheCause::OptInNeverEngaged);
        }
        Some(CacheCause::PrefixInstability)
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

// ── Detail formatters ───────────────────────────────────────────────────────
//
// The statline carries the hit rate only (D1); the read/write volumes and
// the warmth countdown are diagnostics and live in the context overlay's
// SESSION VITALS section, built from these.

/// The cache detail line: hit rate plus the compact read/write volumes —
/// `50% hit · 105.3M rd · 40.0K wr` — or the no-data dash before any input
/// is metered.
pub fn cache_volumes(cache_read: u64, cache_write: u64, total_input: u64) -> String {
    match hit_pct(cache_read, total_input) {
        None => "—".to_string(),
        Some(pct) => format!(
            "{pct}% hit · {} rd · {} wr",
            fmt_tokens(cache_read),
            fmt_tokens(cache_write)
        ),
    }
}

/// Format a token count with uppercase scale suffixes and one decimal:
/// `105.3M`, `211.4K`, `950`. Cumulative cache counts reach the millions, so
/// this carries an `M` tier.
pub(crate) fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// The whole SESSION VITALS cache row: `cache 50% hit · 105.3M rd · 40.0K wr
/// ·  warmth 4:12  ·  saved $1.23`, for the lane at `focused`.
///
/// Built here rather than inline in `deck_render` for the reason this module
/// exists: the formatting is then unit-testable without a frame, and the
/// overlay renderer stays a layout function. `saved` joined this row when the
/// statline's zone C narrowed to `turn` + `run` — it is a cumulative session
/// total, so it answers a question asked at the end of a run, not one glanced
/// at mid-turn.
pub fn vitals_spans(model: &WorkspaceModel, focused: usize) -> Vec<Span<'static>> {
    let dim = Style::default().fg(theme::TEXT_TERTIARY);
    let ink = Style::default().fg(theme::INK);
    let savings = model.total_cache_savings_usd();
    vec![
        Span::styled("  cache ", dim),
        Span::styled(
            cache_volumes(
                model.cache_hit_tokens(),
                model.total_cache_write_tokens(),
                model.total_input_tokens(),
            ),
            ink,
        ),
        Span::styled("  ·  warmth ", dim),
        Span::styled(
            fmt_warmth(
                model
                    .agents
                    .get(focused)
                    .and_then(|a| a.cache_warmth_secs(model.now_ms)),
            ),
            ink,
        ),
        Span::styled("  ·  saved ", dim),
        Span::styled(
            fmt_savings(savings),
            // Negative savings — the write premium outran the reads it bought
            // — is the incident worth surfacing, so it keeps its own color
            // rather than reading as an ordinary total.
            if savings < 0.0 {
                Style::default().fg(theme::DANGER_BRIGHT)
            } else {
                Style::default().fg(theme::SUCCESS_BRIGHT)
            },
        ),
    ]
}

/// The statline's optional second row: a low-hit-rate diagnosis, prefixed
/// with a warning glyph and rendered in `CacheCause::hint`'s full-sentence
/// wording — byte-identical to what `stella stats` prints for the same
/// cause, per `stella-protocol::cache`'s "the CLI and the TUI render
/// identical wording" contract. Always danger-colored: this row only exists
/// when something is actually wrong.
pub fn diagnosis_spans(cause: CacheCause) -> Vec<Span<'static>> {
    let style = Style::default().fg(theme::DANGER_BRIGHT);
    vec![Span::styled("⚠ ", style), Span::styled(cause.hint(), style)]
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

    fn entry(
        tokens_in: u64,
        cache_read_tokens: u64,
        cache_write_tokens: u64,
        cache_call_count: u64,
        is_opt_in: bool,
    ) -> AgentEntry {
        let mut m = WorkspaceModel::new();
        m.apply_inbound(&crate::envelope::Inbound::Register(
            crate::envelope::AgentMeta::new("lead", "goal", 0),
        ));
        let a = &mut m.agents[0];
        a.tokens_in = tokens_in;
        a.cache_read_tokens = cache_read_tokens;
        a.cache_write_tokens = cache_write_tokens;
        a.cache_call_count = cache_call_count;
        a.cache_is_opt_in_provider = is_opt_in;
        m.agents.remove(0)
    }

    #[test]
    fn diagnosis_fires_only_past_min_turns_and_under_threshold() {
        // Too few calls: 0% hit rate but only 3 turns — nothing to say yet.
        assert_eq!(entry(30_000, 0, 0, 3, true).cache_diagnosis(0.20), None);
        // Past MIN_TURNS, still 0% hit — fires.
        assert!(entry(30_000, 0, 0, 4, true).cache_diagnosis(0.20).is_some());
        // Healthy hit rate past MIN_TURNS — quiet.
        assert_eq!(
            entry(30_000, 20_000, 5_000, 6, true).cache_diagnosis(0.20),
            None
        );
    }

    #[test]
    fn diagnosis_names_opt_in_absent_vs_prefix_instability() {
        // Opt-in provider, 0 hits, 0 writes: the marker never engaged.
        assert_eq!(
            entry(30_000, 0, 0, 5, true).cache_diagnosis(0.20),
            Some(CacheCause::OptInNeverEngaged)
        );
        // Opt-in provider that DID write (writes just never got read back):
        // the prefix is unstable, not opt-in-absent.
        assert_eq!(
            entry(30_000, 0, 15_000, 5, true).cache_diagnosis(0.20),
            Some(CacheCause::PrefixInstability)
        );
        // An implicit-cache provider (is_opt_in false) that never wrote
        // still names prefix instability, never opt-in-absent — there is no
        // marker to have missed.
        assert_eq!(
            entry(30_000, 0, 0, 5, false).cache_diagnosis(0.20),
            Some(CacheCause::PrefixInstability)
        );
        // Reported from a live session: 9.6K read against 0 written, and the
        // statline said "opt-in never engaged — likely a bug" beside its own
        // evidence that the cache was working. A read cannot come from a
        // cache that was never written; writes and reads simply land on
        // different turns, so zero writes is the ordinary shape of a warm
        // prefix. Low-but-nonzero hits are instability, never an absent
        // marker.
        assert_eq!(
            entry(96_000, 9_600, 0, 5, true).cache_diagnosis(0.20),
            Some(CacheCause::PrefixInstability),
            "a cache read is proof the cache engaged"
        );
    }

    // ── Detail-line + statline diagnosis integration tests ──────────────────
    //
    // The volumes left the statline for the context overlay (D1); what stays
    // statline-side is the hit rate (asserted in `crate::statline`'s own
    // tests) and the diagnosis row, asserted through the real band renderer
    // here so the formatter and the row reservation cannot drift apart.

    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use stella_protocol::{AgentEvent, StageKind};

    use crate::deck_ui::DeckUi;
    use crate::envelope::{AgentMeta, Inbound};

    /// Flatten a rendered `Buffer` to one string, styling stripped — content
    /// is what these tests assert on, never raw ANSI.
    fn buffer_text(buf: &Buffer) -> String {
        let area = *buf.area();
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_vitals_row_carries_volumes_warmth_and_the_savings_that_left_the_statline() {
        let mut m = WorkspaceModel::new();
        m.now_ms = 1_000;
        m.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        {
            let a = &mut m.agents[0];
            a.tokens_in = 200_000;
            a.cache_read_tokens = 150_000;
            a.cache_write_tokens = 40_000;
            a.cache_savings_usd = 1.23;
        }
        let text: String = vitals_spans(&m, 0)
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(text.contains("75% hit · 150.0K rd · 40.0K wr"), "{text}");
        assert!(text.contains("warmth"), "{text}");
        // The figure zone C stopped carrying has to live somewhere.
        assert!(text.contains("saved $1.23"), "{text}");
    }

    #[test]
    fn cache_volumes_reads_hit_rate_and_compact_counts() {
        assert_eq!(
            cache_volumes(105_300_000, 0, 211_400_000),
            "50% hit · 105.3M rd · 0 wr"
        );
        assert_eq!(
            cache_volumes(150_000, 40_000, 200_000),
            "75% hit · 150.0K rd · 40.0K wr"
        );
        // No input metered yet: the dash, never a divide-by-zero.
        assert_eq!(cache_volumes(0, 0, 0), "—");
    }

    #[test]
    fn a_diagnosed_agent_earns_the_statline_diagnosis_row() {
        // Opt-in provider, past MIN_TURNS, 0% hit, nothing written: the
        // marker never engaged, and the second statline row says so in the
        // full-sentence wording `stella stats` prints for the same cause.
        let mut m = WorkspaceModel::new();
        m.now_ms = 1_000;
        m.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        m.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::Stage {
                name: StageKind::Execute,
            },
        });
        {
            let a = &mut m.agents[0];
            a.tokens_in = 40_000;
            a.cache_call_count = 5;
            a.cache_is_opt_in_provider = true;
        }
        let ui = DeckUi::default();
        let area = Rect::new(0, 0, 200, 2);
        let mut buf = Buffer::empty(area);
        crate::statline::render(&m, &ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(
            text.contains("cache opt-in never engaged"),
            "diagnosis row present:\n{text}"
        );

        // A healthy agent never grows the row.
        let mut healthy = WorkspaceModel::new();
        healthy.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        let mut buf = Buffer::empty(area);
        crate::statline::render(&healthy, &ui, area, &mut buf);
        assert!(!buffer_text(&buf).contains("engaged"));
    }
}
