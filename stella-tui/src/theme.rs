//! The one place the deck's look is defined — colors, semantic styles, and
//! glyphs. Every view pulls from here so the deck reads as one system in both
//! the Stella brand palette and its status semantics. No view hard-codes a
//! color; that is what keeps a 12-panel TUI feeling designed rather than
//! assembled.

use ratatui::style::{Color, Modifier, Style};

use crate::deck::TraceKind;
use crate::envelope::AgentStatus;

// ── Oxagen palette — "aurora light on navy black" ───────────────────────────
//
// The brand system, recut cool: a navy-black ground framing an electric
// aurora mark. Aurora (cyan→azure→violet) is the *light* — reserved for
// live / active / brand, never body text. Violet stays the interactive
// chrome (keybind glyphs, links, focus). Cool blue-grey neutrals carry the
// text. No ember, no amber, no tequila sunrise — the only warm hue left is
// the semantic WARNING pair, which means "warning" and nothing else. Every
// token here is 24-bit; [`degrade_buffer`] narrows it to 256- or 16-color,
// or strips it for `NO_COLOR`, once per frame for terminals that can't
// render truecolor.

// Grounds (dark → light lift) — navy blacks, not violet-blacks.
/// App background — navy black. Applied as a real frame fill by
/// `render_deck`, not just assumed from the terminal.
pub const GROUND: Color = Color::Rgb(0x05, 0x0A, 0x18);
/// Card / panel surface.
pub const SURFACE: Color = Color::Rgb(0x0A, 0x12, 0x26);
/// Raised panel (one step above surface).
pub const RAISED: Color = Color::Rgb(0x10, 0x1A, 0x33);
/// Hairline border / rule — a navy seam, not a grey line.
pub const HAIRLINE: Color = Color::Rgb(0x1B, 0x2A, 0x4A);

// Text tiers (primary → dim) — cool, blue-leaning neutrals.
/// Primary text.
pub const TEXT_PRIMARY: Color = Color::Rgb(0xF2, 0xF6, 0xFF);
/// Secondary text.
pub const TEXT_SECONDARY: Color = Color::Rgb(0xA9, 0xB7, 0xD6);
/// Tertiary text (labels, captions).
pub const TEXT_TERTIARY: Color = Color::Rgb(0x72, 0x85, 0xA8);
/// Dim text (the quietest legible tier).
pub const TEXT_DIM: Color = Color::Rgb(0x5D, 0x6C, 0x8A);

// Aurora — the light. Live / active / brand only; never body text.
/// Aurora cyan — the brightest, most electric stop; the prompt `>>>` and
/// the brand accent.
pub const AURORA_CYAN: Color = Color::Rgb(0x3F, 0xE0, 0xFF);
/// Aurora azure — the mid stop; the active-stage label / live status.
pub const AURORA_AZURE: Color = Color::Rgb(0x4D, 0x9F, 0xFF);
/// Aurora magenta — the hot-pink far stop; the failure frontier.
pub const AURORA_MAGENTA: Color = Color::Rgb(0xE4, 0x40, 0x8F);

/// Violet accent — interactive chrome, keybind glyphs, links, focus.
pub const VIOLET: Color = Color::Rgb(0xA7, 0x8B, 0xFA);

// Semantic (base + bright).
/// Success (base).
pub const SUCCESS: Color = Color::Rgb(0x1D, 0x9E, 0x75);
/// Success (bright — text / completed fills). A cool spring mint.
pub const SUCCESS_BRIGHT: Color = Color::Rgb(0x3F, 0xD6, 0x9B);
/// Warning (base). The one deliberately warm survivor — semantic only.
pub const WARNING: Color = Color::Rgb(0xBA, 0x75, 0x17);
/// Warning (bright — text).
pub const WARNING_BRIGHT: Color = Color::Rgb(0xF4, 0xB2, 0x4A);
/// Danger — reuses aurora magenta.
pub const DANGER: Color = AURORA_MAGENTA;
/// Danger (bright — legible removed-line / error text on the dark backdrop).
pub const DANGER_BRIGHT: Color = Color::Rgb(0xFF, 0x5C, 0x8A);

/// Ice tint for transcript agent body — a soft glacier blue that keeps the
/// agent voice distinct from plain text without borrowing the accent.
pub const AGENT_ICE: Color = Color::Rgb(0xA8, 0xC7, 0xF0);

// ── Role aliases (what the rest of the crate references) ─────────────────────
// Role names remap onto the palette so call sites read as intent (accent,
// ink, rule) rather than as a hue that a future recolor would falsify.

/// Stella brand accent — aurora cyan.
pub const ACCENT: Color = AURORA_CYAN;
/// A deeper accent (gradient / pressed) — azure.
pub const ACCENT_DEEP: Color = AURORA_AZURE;
/// Near-white primary text.
pub const INK: Color = TEXT_PRIMARY;
/// Dimmed secondary text.
pub const MUTED: Color = TEXT_SECONDARY;
/// Panel border / rule.
pub const RULE: Color = HAIRLINE;

/// Background tint for the transcript entry selected with the arrow keys —
/// a barely-there navy lift so the highlight reads without shouting.
pub const SELECT_BG: Color = Color::Rgb(0x14, 0x22, 0x4A);

/// Success / positive / added lines.
pub const OK: Color = SUCCESS_BRIGHT;
/// Warning / needs-input.
pub const WARN: Color = WARNING_BRIGHT;
/// Error / removed lines / failure.
pub const BAD: Color = DANGER_BRIGHT;
/// Structural / process accent — aliased to [`VIOLET`] (the brand's
/// interactive-chrome / link hue) so process events (links, diff hunk
/// headers, graph relations, trace stage/tool/vcs) stay distinct from the
/// cyan brand accent instead of competing with it.
pub const RUN: Color = VIOLET;
/// Paused / held — violet.
pub const HELD: Color = VIOLET;

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
// red, and the plain panel) and to stay inside the cool aurora family —
// Keyword rides the brand cyan so code structure pops the way the accent
// does everywhere else; strings take a soft spring green so they separate
// from keywords without a second electric hue; numbers take the [`VIOLET`]
// anchor as the counterpoint to the cyan/green stops; comments dim toward
// [`MUTED`].

/// Language keyword (`fn`/`let`/`def`/`import`/`return`…).
pub const SYNTAX_KEYWORD: Color = ACCENT;
/// String / char literal.
pub const SYNTAX_STRING: Color = Color::Rgb(126, 231, 135);
/// Numeric literal — violet, the counterpoint to the cyan keyword stop.
pub const SYNTAX_NUMBER: Color = VIOLET;
/// Line comment (rendered dimmed + italic).
pub const SYNTAX_COMMENT: Color = Color::Rgb(118, 124, 134);

// ── Aurora gradient (retained for other views) ──────────────────────────────

/// The aurora gradient's three stops, left → right: cyan → azure → violet.
pub const AURORA_STOPS: [Color; 3] = [AURORA_CYAN, AURORA_AZURE, VIOLET];

// ── Gold gradient (the progress-bar fill) ───────────────────────────────────
//
// The one deliberately warm run: the run progress bar reads gold end-to-end
// so the deck's single activity indicator is unmistakable against the cool
// aurora chrome everywhere else.

/// Deep gold — the fill's leading (left) stop, a burnished antique gold.
pub const GOLD_DEEP: Color = Color::Rgb(0xA8, 0x7A, 0x0E);
/// Gold — the mid stop; also the non-truecolor fallback fill.
pub const GOLD: Color = Color::Rgb(0xE8, 0xB8, 0x2E);
/// Bright gold — the frontier/sparkle stop, a warm champagne highlight.
pub const GOLD_BRIGHT: Color = Color::Rgb(0xFF, 0xE6, 0x8A);

/// The gold gradient's three stops, left → right: deep → gold → bright.
/// The determinate progress fill interpolates across these per cell (truecolor
/// only; lesser terminals collapse to a solid [`GOLD`] fill).
pub const GOLD_STOPS: [Color; 3] = [GOLD_DEEP, GOLD, GOLD_BRIGHT];

/// Linear-interpolate two RGB colors at `t ∈ [0, 1]`. Non-RGB inputs return
/// `a` unchanged (the gradient only ever feeds it `Color::Rgb` stops).
pub fn lerp_rgb(a: Color, b: Color, t: f64) -> Color {
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (a, b) else {
        return a;
    };
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (f64::from(x) + (f64::from(y) - f64::from(x)) * t).round() as u8;
    Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb))
}

/// The aurora gradient sampled at `t ∈ [0, 1]`: cyan at 0, azure at ½,
/// violet at 1, linearly interpolated between the two nearest
/// [`AURORA_STOPS`].
pub fn aurora_gradient(t: f64) -> Color {
    gradient_at(&AURORA_STOPS, t)
}

/// The gold gradient sampled at `t ∈ [0, 1]`: deep gold at 0, gold at ½,
/// bright champagne gold at 1, linearly interpolated between the two nearest
/// [`GOLD_STOPS`]. This is the run progress bar's fill.
pub fn gold_gradient(t: f64) -> Color {
    gradient_at(&GOLD_STOPS, t)
}

/// Sample a 3-stop gradient at `t ∈ [0, 1]`.
fn gradient_at(stops: &[Color; 3], t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    let span = (stops.len() - 1) as f64; // 2 segments
    let scaled = t * span;
    let i = (scaled.floor() as usize).min(stops.len() - 2);
    lerp_rgb(stops[i], stops[i + 1], scaled - i as f64)
}

/// Lighten `color` toward white by `amount ∈ [0, 1]` — the shimmer band and the
/// pulsing head ride a lifted copy of the underlying gradient cell.
pub fn lighten(color: Color, amount: f64) -> Color {
    lerp_rgb(color, Color::Rgb(255, 255, 255), amount)
}

// ── Color-depth degradation (truecolor → 256 → 16 → none) ───────────────────
//
// Every palette token above is a `Color::Rgb`, which lesser terminals either
// approximate unpredictably (amber comes out brown/grey) or ignore. The deck
// detects the terminal's real depth once at startup ([`detect_color_mode`])
// and narrows every cell to it once per frame ([`degrade_buffer`]):
//
//   • Truecolor — pass through unchanged (24-bit).
//   • Ansi256   — each token → a hand-picked xterm-256 cube/grayscale index.
//   • Ansi16    → each token → the nearest ANSI base/bright index (0–15), for
//                 the plainest `xterm`/`linux`/16-color consoles.
//   • None      → `NO_COLOR` is set: strip every color to the terminal default
//                 (monochrome), so structure survives with zero color.
//
// The `(token, idx256, idx16)` table sits immediately below the last token so a
// newly added `Color::Rgb` with no entry is easy to spot; the
// `every_named_token_has_a_fallback` test also checks it mechanically. Indices
// are the nearest cube/base entry by Euclidean RGB distance, not guessed.

/// The color depth the deck renders at. Decided once from the environment; a
/// `Copy` value threaded through the draw loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorMode {
    /// 24-bit — tokens render verbatim (per-cell gradients allowed).
    #[default]
    Truecolor,
    /// Indexed 256-color — tokens map to an xterm-256 index.
    Ansi256,
    /// 16-color ANSI — tokens map to a base/bright index (0–15).
    Ansi16,
    /// `NO_COLOR` — no color at all; every cell falls to the terminal default.
    None,
}

impl ColorMode {
    /// True only for the full 24-bit path — the one mode where per-cell
    /// gradient RGB (the progress fill) is legible, so callers emit solid
    /// named tokens instead when this is false.
    pub fn is_truecolor(self) -> bool {
        matches!(self, ColorMode::Truecolor)
    }
}

/// Whether the terminal advertises 24-bit ("truecolor") support, decided purely
/// from the two environment inputs that matter — no `std::env` access here, so
/// this is unit-testable. `COLORTERM in {truecolor, 24bit}` (the de-facto signal
/// set by iTerm2/kitty/alacritty/wezterm/VS Code/…) or a `TERM` containing
/// `direct` (the terminfo direct-color convention) means yes; everything else —
/// including the `-256color` family, which only promises the indexed palette —
/// is conservatively no.
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

/// Decide the [`ColorMode`] from the three environment inputs, most-restrictive
/// first — pure, so it is unit-testable without touching the real environment.
///
/// 1. `NO_COLOR` set to anything (even empty, per the `no-color.org` spec) →
///    [`ColorMode::None`]. It wins over every color signal.
/// 2. Truecolor (via [`truecolor_supported`]) → [`ColorMode::Truecolor`].
/// 3. A `TERM` promising 256 colors (`-256color`, or `COLORTERM` present at all)
///    → [`ColorMode::Ansi256`].
/// 4. Anything else (bare `xterm`/`screen`/`linux`, or no `TERM`) →
///    [`ColorMode::Ansi16`], the safe floor: 16 ANSI colors exist essentially
///    everywhere, so structure never renders as raw illegible RGB.
pub fn color_mode(no_color: bool, colorterm: Option<&str>, term: Option<&str>) -> ColorMode {
    if no_color {
        return ColorMode::None;
    }
    if truecolor_supported(colorterm, term) {
        return ColorMode::Truecolor;
    }
    let has_256 =
        colorterm.is_some() || term.is_some_and(|t| t.to_ascii_lowercase().contains("256color"));
    if has_256 {
        ColorMode::Ansi256
    } else {
        ColorMode::Ansi16
    }
}

/// Read `NO_COLOR`/`COLORTERM`/`TERM` from the real process environment once and
/// decide the [`ColorMode`] via [`color_mode`]. Call once at startup (see
/// `shell::run` / `deck_shell::run_deck`) and thread the result through — never
/// per-frame or per-token.
pub fn detect_color_mode() -> ColorMode {
    color_mode(
        std::env::var_os("NO_COLOR").is_some(),
        std::env::var("COLORTERM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
    )
}

/// `(token, xterm-256 index, ANSI-16 index)` for every distinct `Color::Rgb`
/// value in the palette. Role aliases share a value with a palette token, so
/// one entry covers both — the table is keyed by value, first match wins.
const FALLBACKS: &[(Color, u8, u8)] = &[
    (GROUND, 232, 0),
    (SURFACE, 233, 0),
    (RAISED, 234, 8),
    (HAIRLINE, 236, 8),
    (TEXT_PRIMARY, 231, 15),
    (TEXT_SECONDARY, 146, 7),
    (TEXT_TERTIARY, 103, 8),
    (TEXT_DIM, 60, 8),
    (AURORA_CYAN, 81, 14),
    (AURORA_AZURE, 75, 12),
    (AURORA_MAGENTA, 168, 5),
    (VIOLET, 141, 13),
    (SUCCESS, 36, 2),
    (SUCCESS_BRIGHT, 79, 10),
    (WARNING, 136, 3),
    (WARNING_BRIGHT, 215, 11),
    (DANGER_BRIGHT, 204, 9),
    (AGENT_ICE, 153, 12),
    (SELECT_BG, 235, 0),
    (DIFF_ADD_BG, 22, 2),
    (DIFF_DEL_BG, 52, 1),
    (SYNTAX_STRING, 114, 10),
    (SYNTAX_COMMENT, 244, 8),
    (GOLD_DEEP, 136, 3),
    (GOLD, 220, 11),
    (GOLD_BRIGHT, 228, 11),
];

/// Resolve one color for the mode actually in use. Truecolor passes through;
/// `None` (NO_COLOR) drops every RGB to `Reset` (terminal default); 256/16 map
/// via [`FALLBACKS`]. A color with no matching entry (already-indexed, named,
/// `Reset`, or an interpolated gradient cell) passes through unchanged — this
/// only ever narrows the palette tokens, never anything else.
pub fn resolve(color: Color, mode: ColorMode) -> Color {
    match mode {
        ColorMode::Truecolor => color,
        ColorMode::None => match color {
            Color::Rgb(..) | Color::Indexed(_) => Color::Reset,
            other => other,
        },
        ColorMode::Ansi256 => FALLBACKS
            .iter()
            .find_map(|(rgb, i256, _)| (*rgb == color).then_some(Color::Indexed(*i256)))
            .unwrap_or(color),
        ColorMode::Ansi16 => FALLBACKS
            .iter()
            .find_map(|(rgb, _, i16)| (*rgb == color).then_some(Color::Indexed(*i16)))
            .unwrap_or(color),
    }
}

/// Degrade every cell's colors in `buf` in place via [`resolve`]. A no-op in
/// [`ColorMode::Truecolor`].
///
/// This is the *only* place a fallback is applied, once per frame right after
/// the widgets render — which lets every other call site in the crate keep
/// referencing `theme::TOKEN` directly, unaware a lesser terminal is watching.
/// See `shell::run` / `deck_shell::run_deck` for the call sites.
pub fn degrade_buffer(buf: &mut ratatui::buffer::Buffer, mode: ColorMode) {
    if mode.is_truecolor() {
        return;
    }
    for cell in buf.content.iter_mut() {
        cell.fg = resolve(cell.fg, mode);
        cell.bg = resolve(cell.bg, mode);
        cell.underline_color = resolve(cell.underline_color, mode);
    }
}

// ── Styles ──────────────────────────────────────────────────────────────────

/// Accent style for headings / the active tab.
pub fn accent() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
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
        // Live work is aurora — the one place the light means "running now".
        AgentStatus::Running => AURORA_AZURE,
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
/// function/method violet, struct/enum/trait green, file/module azure —
/// three distinct on-brand hues, none of them the cyan that carries the
/// graph chrome.
pub fn graph_kind_color(kind: &str) -> Color {
    match kind {
        "function" | "method" => RUN,
        "struct" | "enum" | "trait" => OK,
        "file" | "module" => ACCENT_DEEP,
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
/// Five distinct on-brand cool hues — violet, cyan, mint, ice, azure — with
/// no magenta (magenta reads as failure elsewhere, so it never brands a
/// healthy agent) and nothing warm.
const AGENT_PALETTE: [Color; 5] = [HELD, ACCENT, OK, AGENT_ICE, ACCENT_DEEP];

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
/// meaning: `RUN` (violet) for process/action events (stage, tool, vcs),
/// `ACCENT`/`ACCENT_DEEP` (cyan/azure) for produced artifacts (file, media),
/// a dim neutral for quiet memory/context events, and the shared
/// `OK`/`WARN`/`BAD` semantics for verdicts, spend, and errors. Memory drops
/// to `TEXT_TERTIARY` rather than reuse violet — the process group already
/// owns that anchor.
pub fn trace_kind_color(kind: TraceKind) -> Color {
    match kind {
        TraceKind::Stage => RUN,
        TraceKind::Text => INK,
        TraceKind::Reasoning => MUTED,
        TraceKind::Tool => RUN,
        TraceKind::File => ACCENT,
        TraceKind::Budget => WARN,
        TraceKind::Context => TEXT_TERTIARY,
        TraceKind::Verdict => OK,
        TraceKind::Media => ACCENT_DEEP,
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

    /// Every distinct `Color::Rgb` value in the palette — kept explicit (not
    /// derived) so this and [`every_named_token_has_a_fallback`] fail loudly the
    /// moment a new truecolor token lands without a [`FALLBACKS`] entry. Role
    /// aliases (`INK`, `OK`, `DANGER`, …) share a value with a palette token, so
    /// they are intentionally not re-listed.
    const ALL_RGB_TOKENS: &[Color] = &[
        GROUND,
        SURFACE,
        RAISED,
        HAIRLINE,
        TEXT_PRIMARY,
        TEXT_SECONDARY,
        TEXT_TERTIARY,
        TEXT_DIM,
        AURORA_CYAN,
        AURORA_AZURE,
        AURORA_MAGENTA,
        VIOLET,
        SUCCESS,
        SUCCESS_BRIGHT,
        WARNING,
        WARNING_BRIGHT,
        DANGER_BRIGHT,
        AGENT_ICE,
        SELECT_BG,
        DIFF_ADD_BG,
        DIFF_DEL_BG,
        SYNTAX_STRING,
        SYNTAX_COMMENT,
        GOLD_DEEP,
        GOLD,
        GOLD_BRIGHT,
    ];

    #[test]
    fn every_named_token_has_a_fallback() {
        for token in ALL_RGB_TOKENS {
            assert!(
                FALLBACKS.iter().any(|(rgb, ..)| rgb == token),
                "token {token:?} has no FALLBACKS entry (256 + 16 index)"
            );
        }
        // No duplicate values in the table (aliases share one entry by value).
        for (i, (rgb, ..)) in FALLBACKS.iter().enumerate() {
            assert!(
                !FALLBACKS[..i].iter().any(|(other, ..)| other == rgb),
                "duplicate FALLBACKS entry for {rgb:?}"
            );
        }
        assert_eq!(
            FALLBACKS.len(),
            ALL_RGB_TOKENS.len(),
            "one FALLBACKS entry per distinct palette token"
        );
    }

    #[test]
    fn role_aliases_track_their_palette_token() {
        assert_eq!(ACCENT, AURORA_CYAN);
        assert_eq!(INK, TEXT_PRIMARY);
        assert_eq!(MUTED, TEXT_SECONDARY);
        assert_eq!(RULE, HAIRLINE);
        assert_eq!(OK, SUCCESS_BRIGHT);
        assert_eq!(WARN, WARNING_BRIGHT);
        assert_eq!(BAD, DANGER_BRIGHT);
        assert_eq!(DANGER, AURORA_MAGENTA);
        assert_eq!(HELD, VIOLET);
        assert_eq!(RUN, VIOLET);
        assert_eq!(SYNTAX_NUMBER, VIOLET);
    }

    /// Regression guard: the ember/amber "tequila sunrise" tones the deck
    /// moved off (`EMBER_GOLD #F9D423`, `EMBER_FLAME #FF7E5F`,
    /// `EMBER_CRIMSON #C2185B`, `AGENT_AMBER #E8A24A`) must never reappear
    /// on any palette token or its aliases. The only deliberately warm
    /// survivors are the semantic WARNING pair — warm because warnings are,
    /// not because the brand is.
    #[test]
    fn no_ember_amber_survives_anywhere() {
        const OLD_EMBER_GOLD: Color = Color::Rgb(0xF9, 0xD4, 0x23);
        const OLD_EMBER_FLAME: Color = Color::Rgb(0xFF, 0x7E, 0x5F);
        const OLD_EMBER_CRIMSON: Color = Color::Rgb(0xC2, 0x18, 0x5B);
        const OLD_AGENT_AMBER: Color = Color::Rgb(0xE8, 0xA2, 0x4A);
        let mut all: Vec<Color> = ALL_RGB_TOKENS.to_vec();
        all.extend([
            RUN,
            SYNTAX_NUMBER,
            SYNTAX_KEYWORD,
            HELD,
            ACCENT,
            OK,
            ACCENT_DEEP,
        ]);
        all.extend(AURORA_STOPS);
        for token in all {
            for (old, name) in [
                (OLD_EMBER_GOLD, "ember gold"),
                (OLD_EMBER_FLAME, "ember flame"),
                (OLD_EMBER_CRIMSON, "ember crimson"),
                (OLD_AGENT_AMBER, "agent amber"),
            ] {
                assert_ne!(token, old, "a token still holds {name}");
            }
        }
    }

    #[test]
    fn truecolor_supported_reads_colorterm_first() {
        assert!(truecolor_supported(Some("truecolor"), None));
        assert!(truecolor_supported(Some("24bit"), Some("xterm")));
        assert!(truecolor_supported(Some("TrueColor"), None)); // case-insensitive
    }

    #[test]
    fn truecolor_supported_falls_back_to_term_direct_suffix() {
        assert!(truecolor_supported(None, Some("xterm-direct")));
        assert!(truecolor_supported(None, Some("st-direct")));
    }

    #[test]
    fn truecolor_supported_is_false_for_known_limited_terms() {
        assert!(!truecolor_supported(None, Some("xterm")));
        assert!(!truecolor_supported(None, Some("xterm-256color")));
        assert!(!truecolor_supported(None, Some("screen")));
        assert!(!truecolor_supported(None, Some("linux")));
        assert!(!truecolor_supported(None, Some("tmux-256color")));
        assert!(!truecolor_supported(None, None));
    }

    #[test]
    fn color_mode_no_color_beats_every_color_signal() {
        // NO_COLOR wins even on a truecolor terminal.
        assert_eq!(color_mode(true, Some("truecolor"), None), ColorMode::None);
        assert_eq!(
            color_mode(true, None, Some("xterm-256color")),
            ColorMode::None
        );
    }

    #[test]
    fn color_mode_detects_each_depth() {
        assert_eq!(
            color_mode(false, Some("truecolor"), None),
            ColorMode::Truecolor
        );
        assert_eq!(
            color_mode(false, None, Some("xterm-256color")),
            ColorMode::Ansi256
        );
        // Bare legacy terminals, and no environment at all, floor at 16 colors.
        assert_eq!(color_mode(false, None, Some("xterm")), ColorMode::Ansi16);
        assert_eq!(color_mode(false, None, Some("linux")), ColorMode::Ansi16);
        assert_eq!(color_mode(false, None, None), ColorMode::Ansi16);
    }

    #[test]
    fn resolve_passes_through_when_truecolor() {
        assert_eq!(resolve(AURORA_CYAN, ColorMode::Truecolor), AURORA_CYAN);
    }

    #[test]
    fn resolve_maps_every_token_to_its_index_when_degraded() {
        for (rgb, i256, i16) in FALLBACKS {
            assert_eq!(resolve(*rgb, ColorMode::Ansi256), Color::Indexed(*i256));
            assert_eq!(resolve(*rgb, ColorMode::Ansi16), Color::Indexed(*i16));
        }
    }

    #[test]
    fn resolve_strips_color_under_no_color() {
        assert_eq!(resolve(AURORA_CYAN, ColorMode::None), Color::Reset);
        assert_eq!(resolve(Color::Indexed(9), ColorMode::None), Color::Reset);
        // A non-color (Reset) stays put — nothing to strip.
        assert_eq!(resolve(Color::Reset, ColorMode::None), Color::Reset);
    }

    #[test]
    fn resolve_leaves_unmapped_colors_unchanged_when_indexed() {
        assert_eq!(
            resolve(Color::Indexed(9), ColorMode::Ansi256),
            Color::Indexed(9)
        );
        assert_eq!(resolve(Color::Reset, ColorMode::Ansi16), Color::Reset);
    }

    #[test]
    fn degrade_buffer_is_noop_when_truecolor() {
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 1, 1));
        buf.content[0].fg = AURORA_CYAN;
        degrade_buffer(&mut buf, ColorMode::Truecolor);
        assert_eq!(buf.content[0].fg, AURORA_CYAN);
    }

    #[test]
    fn degrade_buffer_resolves_every_cell_when_degraded() {
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 1, 1));
        buf.content[0].fg = AURORA_CYAN; // → 81 (256) / 14 (16)
        buf.content[0].bg = VIOLET; // → 141 (256) / 13 (16)
        degrade_buffer(&mut buf, ColorMode::Ansi256);
        assert_eq!(buf.content[0].fg, Color::Indexed(81));
        assert_eq!(buf.content[0].bg, Color::Indexed(141));
    }

    #[test]
    fn degrade_buffer_strips_color_under_no_color() {
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 1, 1));
        buf.content[0].fg = AURORA_CYAN;
        buf.content[0].bg = GROUND;
        degrade_buffer(&mut buf, ColorMode::None);
        assert_eq!(buf.content[0].fg, Color::Reset);
        assert_eq!(buf.content[0].bg, Color::Reset);
    }

    #[test]
    fn aurora_gradient_spans_cyan_to_violet() {
        assert_eq!(aurora_gradient(0.0), AURORA_CYAN);
        assert_eq!(aurora_gradient(1.0), VIOLET);
        assert_eq!(aurora_gradient(0.5), AURORA_AZURE);
        // Monotonic, clamped, never panics across the range.
        for i in 0..=20 {
            let _ = aurora_gradient(f64::from(i) / 20.0);
        }
        assert_eq!(aurora_gradient(-1.0), AURORA_CYAN);
        assert_eq!(aurora_gradient(2.0), VIOLET);
    }

    #[test]
    fn lighten_moves_toward_white() {
        assert_eq!(lighten(AURORA_CYAN, 0.0), AURORA_CYAN);
        assert_eq!(lighten(AURORA_CYAN, 1.0), Color::Rgb(255, 255, 255));
    }
}
