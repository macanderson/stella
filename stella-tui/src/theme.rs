//! The one place the deck's look is defined — colors, semantic styles, and
//! glyphs. Every view pulls from here so the deck reads as one system in both
//! the Stella brand palette and its status semantics. No view hard-codes a
//! color; that is what keeps a 12-panel TUI feeling designed rather than
//! assembled.

use ratatui::style::{Color, Modifier, Style};

use crate::deck::TraceKind;
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

/// Background tint for the transcript entry selected with the arrow keys —
/// a barely-there warm lift so the highlight reads without shouting.
pub const SELECT_BG: Color = Color::Rgb(46, 42, 32);

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

// ── Diff panel ──────────────────────────────────────────────────────────────

/// Subtle background tint behind added diff lines (the GitHub-PR reading —
/// pair with [`OK`] foreground).
pub const DIFF_ADD_BG: Color = Color::Rgb(20, 44, 26);
/// Subtle background tint behind removed diff lines (pair with [`BAD`]).
pub const DIFF_DEL_BG: Color = Color::Rgb(52, 24, 26);

// ── Syntax highlighting (diff bodies) ───────────────────────────────────────
//
// A four-color code palette layered *under* the add/remove diff semantics:
// the `+`/`-` background always wins (add/remove is never lost — see
// `crate::diff`), while a recognized token overrides only the foreground.
// Every color is chosen to read on all three diff backdrops (add green, del
// red, and the plain panel) and to stay inside the amber/ember brand family —
// never pink/purple. Keyword rides the brand amber so code structure pops the
// way the accent does everywhere else; strings take a softer warm sand so they
// separate from keywords without a second saturated hue; numbers take a
// lighter cousin of the cool [`RUN`] cyan used across the deck (brightened to
// read on the diff backdrops); comments dim toward [`MUTED`].

/// Language keyword (`fn`/`let`/`def`/`import`/`return`…).
pub const SYNTAX_KEYWORD: Color = AMBER;
/// String / char literal.
pub const SYNTAX_STRING: Color = Color::Rgb(214, 184, 120);
/// Numeric literal.
pub const SYNTAX_NUMBER: Color = Color::Rgb(126, 197, 214);
/// Line comment (rendered dimmed + italic).
pub const SYNTAX_COMMENT: Color = Color::Rgb(118, 124, 134);

// ── Activity spinner ────────────────────────────────────────────────────────

/// Darkest stop of the ember ramp (see [`EMBER_RAMP`]).
pub const EMBER_LOW: Color = Color::Rgb(178, 72, 20);
/// Brightest stop of the ember ramp (see [`EMBER_RAMP`]).
pub const EMBER_HIGH: Color = Color::Rgb(255, 214, 130);

/// Burnt-sunset ember ramp, dark → bright, for the working-spinner gradient —
/// the brand's amber answer to the pink/purple reference spinner.
pub const EMBER_RAMP: [Color; 4] = [EMBER_LOW, AMBER_DEEP, AMBER, EMBER_HIGH];

// ── 256-color fallback (non-truecolor terminals) ────────────────────────────
//
// Every color above is a `Color::Rgb`, which some terminals — anything
// without `COLORTERM=truecolor`/`24bit` and without a `-direct` terminfo
// entry, which includes the ubiquitous plain `xterm`/`screen`/`linux` console
// and (despite the name suggesting otherwise) `-256color` variants too, since
// that suffix only promises the *indexed* 256-color palette, not 24-bit —
// either render as a terminal-chosen approximation (the amber accent can come
// out brown/grey) or not at all. [`truecolor_supported`] decides once, from
// `COLORTERM`/`TERM`, whether the deck should stay on the truecolor tokens
// above or degrade every one of them to a hand-picked xterm-256 index via
// [`resolve`].
//
// The fallback table sits immediately below the last truecolor token above
// it (deliberately, not scattered per-const) so a newly added `Color::Rgb`
// token with no matching entry here is easy to spot on read; the
// `every_named_token_has_a_256_fallback` test below also checks it
// mechanically. Each index was chosen as the nearest xterm-256 color-cube (or
// grayscale-ramp) entry by Euclidean RGB distance — the same reference
// standard terminals themselves use — not guessed. Two entries
// (`DIFF_ADD_BG`, `DIFF_DEL_BG`) land on nearly the same dark grey once
// degraded: both source colors are deliberately near-black "subtle" tints
// (see their doc comments), so the 256-cube's coarse dark end can't keep them
// visually apart — the add/remove distinction still reads correctly through
// the paired `OK`/`BAD` foreground text, which resolves to clearly different
// indices (114 vs 204).
//
// | token            | xterm-256 | approx. RGB      |
// |------------------|-----------|-------------------|
// | `AMBER`          | 214       | `#ffaf00`         |
// | `AMBER_DEEP`     | 172       | `#d78700`         |
// | `INK`            | 255       | `#eeeeee`         |
// | `MUTED`          | 246       | `#949494`         |
// | `RULE`           | 238       | `#444444`         |
// | `SELECT_BG`      | 235       | `#262626`         |
// | `OK`             | 114       | `#87d787`         |
// | `WARN`           | 215       | `#ffaf5f`         |
// | `BAD`            | 204       | `#ff5f87`         |
// | `RUN`            | 74        | `#5fafd7`         |
// | `HELD`           | 140       | `#af87d7`         |
// | `DIFF_ADD_BG`    | 234       | `#1c1c1c`         |
// | `DIFF_DEL_BG`    | 235       | `#262626`         |
// | `SYNTAX_STRING`  | 180       | `#d7af87`         |
// | `SYNTAX_NUMBER`  | 116       | `#87d7d7`         |
// | `SYNTAX_COMMENT` | 244       | `#808080`         |
// | `EMBER_LOW`      | 130       | `#af5f00`         |
// | `EMBER_HIGH`     | 222       | `#ffd787`         |

/// Whether the terminal advertises 24-bit ("truecolor") support, decided
/// purely from the two environment inputs that matter — no `std::env` access
/// here, so this is unit-testable without touching the process environment.
/// [`detect_truecolor_support`] is the real caller that reads the actual
/// environment once at startup.
///
/// Detection order:
/// 1. `COLORTERM` is `truecolor` or `24bit` (case-insensitive) — the de facto
///    standard signal, set by iTerm2, kitty, alacritty, wezterm, VS Code's
///    integrated terminal, gnome-terminal, konsole, and most other modern
///    terminals that actually support 24-bit color.
/// 2. Otherwise, a `TERM` whose name contains `direct` (e.g. `xterm-direct`,
///    `st-direct`) — the one `TERM`-only terminfo convention for advertising
///    direct (24-bit) color.
/// 3. Anything else is treated as non-truecolor, deliberately conservative:
///    this covers bare legacy entries (`xterm`, `screen`, `linux`) *and* the
///    very common `-256color` family (`xterm-256color`, `tmux-256color`,
///    `screen-256color`) which only promise the 256-color palette this
///    fallback table targets, plus the no-`TERM`-at-all case (cron/CI/piped
///    output). Erring toward "degrade" is the safe failure mode: a 256-color
///    fallback on a truecolor terminal is a harmless slight color shift, but
///    raw truecolor RGB sent to a non-supporting terminal is the illegible
///    approximation this fix exists to avoid.
pub fn truecolor_supported(colorterm: Option<&str>, term: Option<&str>) -> bool {
    if let Some(colorterm) = colorterm {
        let colorterm = colorterm.trim();
        if colorterm.eq_ignore_ascii_case("truecolor") || colorterm.eq_ignore_ascii_case("24bit") {
            return true;
        }
    }

    match term {
        Some(term) => term.to_ascii_lowercase().contains("direct"),
        None => false,
    }
}

/// Read `COLORTERM`/`TERM` from the real process environment once and decide
/// truecolor support via [`truecolor_supported`]. Call this once at startup
/// (see `shell::run`, `deck_shell::run_deck`) and thread the result through —
/// don't call it per-frame or per-token.
pub fn detect_truecolor_support() -> bool {
    truecolor_supported(
        std::env::var("COLORTERM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
    )
}

/// `(truecolor token, xterm-256 fallback index)` pairs for every
/// `Color::Rgb` token defined above. See the module-level table for the
/// approximate RGB each index renders as.
const FALLBACKS: &[(Color, u8)] = &[
    (AMBER, 214),
    (AMBER_DEEP, 172),
    (INK, 255),
    (MUTED, 246),
    (RULE, 238),
    (SELECT_BG, 235),
    (OK, 114),
    (WARN, 215),
    (BAD, 204),
    (RUN, 74),
    (HELD, 140),
    (DIFF_ADD_BG, 234),
    (DIFF_DEL_BG, 235),
    (SYNTAX_STRING, 180),
    (SYNTAX_NUMBER, 116),
    (SYNTAX_COMMENT, 244),
    (EMBER_LOW, 130),
    (EMBER_HIGH, 222),
];

/// Resolve one color for the terminal actually in use: unchanged when
/// `truecolor` is `true`, or its [`FALLBACKS`] entry (an indexed 256-color)
/// when `false`. A color with no matching entry (already-indexed, named,
/// `Reset`, …) passes through unchanged either way — this only ever narrows
/// the truecolor tokens defined above, never touches anything else.
pub fn resolve(color: Color, truecolor: bool) -> Color {
    if truecolor {
        return color;
    }
    FALLBACKS
        .iter()
        .find_map(|(rgb, indexed)| (*rgb == color).then_some(Color::Indexed(*indexed)))
        .unwrap_or(color)
}

/// Degrade every cell's colors in `buf` in place via [`resolve`]. A no-op
/// when `truecolor` is `true`.
///
/// This is the *only* place a fallback is actually applied, and it runs once
/// per frame, right after the widgets render into the buffer — which is what
/// lets every other call site in this crate (`render.rs`, `textline.rs`, the
/// view modules, …) keep referencing `theme::TOKEN` directly, unaware that a
/// 256-color terminal might be watching. See `shell::run` /
/// `deck_shell::run_deck` for the two call sites.
pub fn degrade_buffer(buf: &mut ratatui::buffer::Buffer, truecolor: bool) {
    if truecolor {
        return;
    }
    for cell in buf.content.iter_mut() {
        cell.fg = resolve(cell.fg, false);
        cell.bg = resolve(cell.bg, false);
        cell.underline_color = resolve(cell.underline_color, false);
    }
}

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

// ── Graph tab: code-graph node kinds ────────────────────────────────────────

/// Color a [`crate::graph::GraphNode`] by its `kind`, so the Graph tab's node
/// list, detail panel, and node-edge sketch all agree on one palette:
/// function/method one hue, struct/enum/trait another, file/module a third.
pub fn graph_kind_color(kind: &str) -> Color {
    match kind {
        "function" | "method" => RUN,
        "struct" | "enum" | "trait" => OK,
        "file" | "module" => HELD,
        _ => MUTED,
    }
}

/// A compact glyph per node `kind`, paired with [`graph_kind_color`].
pub fn graph_kind_glyph(kind: &str) -> &'static str {
    match kind {
        "function" | "method" => "\u{0192}", // ƒ
        "struct" | "enum" | "trait" => "◆",
        "file" | "module" => "▤",
        _ => "•",
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

// ── Per-agent identity color (Traces tab, multi-agent panels) ──────────────

/// A small rotating palette an agent id is hashed into. The point is
/// stability, not per-color meaning: the same id always lands on the same
/// slot, so an agent reads as one consistent color everywhere it appears.
const AGENT_PALETTE: [Color; 6] = [RUN, HELD, AMBER, OK, WARN, AMBER_DEEP];

/// A deterministic (not randomized — stable across processes and test runs)
/// color for one agent id, picked from [`AGENT_PALETTE`] by hashing the id.
pub fn agent_color(id: &str) -> Color {
    AGENT_PALETTE[(fnv1a(id) as usize) % AGENT_PALETTE.len()]
}

/// FNV-1a: a tiny, deterministic, dependency-free string hash. Unlike
/// `std::collections::hash_map::DefaultHasher` reached via `RandomState`, this
/// never varies by process, which is what makes `agent_color` stable.
fn fnv1a(s: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in s.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

// ── Trace kind → color (Traces tab kind chip) ───────────────────────────────

/// A color per [`TraceKind`], for the Traces tab's kind chip. Grouped by
/// meaning: `RUN` for process/action events (stage, tool, vcs), `AMBER`/
/// `AMBER_DEEP` for produced artifacts (file, media), `HELD` for
/// memory/context events, and the shared `OK`/`WARN`/`BAD` semantics for
/// verdicts, spend, and errors.
pub fn trace_kind_color(kind: TraceKind) -> Color {
    match kind {
        TraceKind::Stage => RUN,
        TraceKind::Text => INK,
        TraceKind::Reasoning => MUTED,
        TraceKind::Tool => RUN,
        TraceKind::File => AMBER,
        TraceKind::Budget => WARN,
        TraceKind::Context => HELD,
        TraceKind::Verdict => OK,
        TraceKind::Media => AMBER_DEEP,
        TraceKind::Vcs => RUN,
        TraceKind::Error => BAD,
        TraceKind::Complete => OK,
        TraceKind::Other => MUTED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_color_is_stable_across_calls() {
        assert_eq!(agent_color("lead"), agent_color("lead"));
        assert_eq!(agent_color("sub:auth"), agent_color("sub:auth"));
    }

    #[test]
    fn agent_color_never_panics_on_empty_or_unicode_ids() {
        let _ = agent_color("");
        let _ = agent_color("agent-🚀-42");
    }

    #[test]
    fn trace_kind_color_covers_every_variant_without_panic() {
        for kind in [
            TraceKind::Stage,
            TraceKind::Text,
            TraceKind::Reasoning,
            TraceKind::Tool,
            TraceKind::File,
            TraceKind::Budget,
            TraceKind::Context,
            TraceKind::Verdict,
            TraceKind::Media,
            TraceKind::Vcs,
            TraceKind::Error,
            TraceKind::Complete,
            TraceKind::Other,
        ] {
            let _ = trace_kind_color(kind);
        }
    }

    /// All eighteen `Color::Rgb` tokens defined in this module — kept as an
    /// explicit list (rather than derived) so this test and
    /// [`every_named_token_has_a_256_fallback`] both fail loudly the moment a
    /// new truecolor token is added without a matching [`FALLBACKS`] entry.
    const ALL_RGB_TOKENS: [Color; 18] = [
        AMBER,
        AMBER_DEEP,
        INK,
        MUTED,
        RULE,
        SELECT_BG,
        OK,
        WARN,
        BAD,
        RUN,
        HELD,
        DIFF_ADD_BG,
        DIFF_DEL_BG,
        SYNTAX_STRING,
        SYNTAX_NUMBER,
        SYNTAX_COMMENT,
        EMBER_LOW,
        EMBER_HIGH,
    ];

    #[test]
    fn every_named_token_has_a_256_fallback() {
        for token in ALL_RGB_TOKENS {
            assert!(
                FALLBACKS.iter().any(|(rgb, _)| *rgb == token),
                "token {token:?} has no xterm-256 fallback entry in FALLBACKS"
            );
        }
        assert_eq!(
            FALLBACKS.len(),
            ALL_RGB_TOKENS.len(),
            "FALLBACKS should have exactly one entry per named RGB token — \
             update both ALL_RGB_TOKENS and FALLBACKS when adding a new token"
        );
    }

    #[test]
    fn truecolor_supported_reads_colorterm_first() {
        assert!(truecolor_supported(Some("truecolor"), None));
        assert!(truecolor_supported(Some("24bit"), Some("xterm")));
        // Case-insensitive.
        assert!(truecolor_supported(Some("TrueColor"), None));
    }

    #[test]
    fn truecolor_supported_falls_back_to_term_direct_suffix() {
        assert!(truecolor_supported(None, Some("xterm-direct")));
        assert!(truecolor_supported(None, Some("st-direct")));
    }

    #[test]
    fn truecolor_supported_is_false_for_known_limited_terms() {
        // No COLORTERM, and TERM values that only promise 16/256-indexed
        // color, not 24-bit — the exact scenario this fix targets.
        assert!(!truecolor_supported(None, Some("xterm")));
        assert!(!truecolor_supported(None, Some("xterm-256color")));
        assert!(!truecolor_supported(None, Some("screen")));
        assert!(!truecolor_supported(None, Some("screen-256color")));
        assert!(!truecolor_supported(None, Some("linux")));
        assert!(!truecolor_supported(None, Some("tmux-256color")));
    }

    #[test]
    fn truecolor_supported_is_false_with_no_environment_at_all() {
        assert!(!truecolor_supported(None, None));
    }

    #[test]
    fn resolve_passes_through_when_truecolor() {
        assert_eq!(resolve(AMBER, true), AMBER);
    }

    #[test]
    fn resolve_maps_every_token_to_its_indexed_fallback_when_degraded() {
        for (rgb, indexed) in FALLBACKS {
            assert_eq!(resolve(*rgb, false), Color::Indexed(*indexed));
        }
    }

    #[test]
    fn resolve_leaves_unmapped_colors_unchanged_when_degraded() {
        assert_eq!(resolve(Color::Indexed(9), false), Color::Indexed(9));
        assert_eq!(resolve(Color::Reset, false), Color::Reset);
    }

    #[test]
    fn degrade_buffer_is_noop_when_truecolor() {
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 1, 1));
        buf.content[0].fg = AMBER;
        degrade_buffer(&mut buf, true);
        assert_eq!(buf.content[0].fg, AMBER);
    }

    #[test]
    fn degrade_buffer_resolves_every_cell_when_degraded() {
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 1, 1));
        buf.content[0].fg = AMBER;
        buf.content[0].bg = HELD;
        degrade_buffer(&mut buf, false);
        assert_eq!(buf.content[0].fg, Color::Indexed(214));
        assert_eq!(buf.content[0].bg, Color::Indexed(140));
    }
}
