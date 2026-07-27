//! The one place the deck's look is defined — colors, semantic styles, and
//! glyphs. Every view pulls from here so the deck reads as one system in both
//! the Stella brand palette and its status semantics. No view hard-codes a
//! color; that is what keeps a 12-panel TUI feeling designed rather than
//! assembled.

use ratatui::style::{Color, Modifier, Style};

use crate::deck::TraceKind;
use crate::envelope::AgentStatus;
use crate::palette;

// ── Stella palette — "bright sky on black" ──────────────────────────────────
//
// A signal light in the dark. The ground is true black; the sky blue is the
// light — reserved for brand, active/running, and progress, and for nothing
// else. If everything is blue, nothing is: a general-purpose highlight is
// exactly what this hue must not become.
//
// The corollary the transcript actually depends on: **prose is white.** The
// accent buys attention, so it may only be spent where attention is owed — on
// the deck that means the tool being called, the active tab, and the progress
// fill. Everything else is [`INK`] (white) or [`MUTED`], and a row that wants
// to be scannable does it with a glyph and a column, not with a hue.
//
// Warning is orange and success is green; status always pairs colour with a
// glyph (see [`status_glyph`]) so hue never carries meaning alone — that is
// also what keeps the deck readable under `NO_COLOR` and for red/green colour
// blindness.
//
// Every value below comes from [`crate::palette`] — the same source the docs
// site's tokens.css is cut from. This module is the *semantic* layer over it:
// call sites reference roles (accent, ink, rule, ok) rather than hues, so a
// recolour is a palette edit rather than a hunt through the crate. Add a
// colour here only as a role; add a *value* in `palette`.
//
// Every token is 24-bit; [`degrade_buffer`] narrows it to 256- or 16-color, or
// strips it for `NO_COLOR`, once per frame for terminals that can't render
// truecolor.

// Grounds (dark → light lift).
/// App background — true black. Applied as a real frame fill by `render_deck`,
/// not just assumed from the terminal.
pub const GROUND: Color = palette::GROUND;
/// Card / panel surface.
pub const SURFACE: Color = palette::SURFACE;
/// Raised panel (one step above surface).
pub const RAISED: Color = palette::RAISED;
/// Hairline border / rule — a cool slate seam. Deliberately below 3.0
/// contrast: it may never be the only thing conveying structure.
pub const HAIRLINE: Color = palette::HAIRLINE;

// Text tiers (primary → dim) — cool, blue-leaning neutrals.
/// Primary text.
pub const TEXT_PRIMARY: Color = palette::TEXT_PRIMARY;
/// Secondary text. The safe small-text tone on every ground.
pub const TEXT_SECONDARY: Color = palette::TEXT_SECONDARY;
/// Tertiary text (labels, captions). Clears AA body text on [`GROUND`]
/// (4.59:1); on [`SURFACE`]/[`RAISED`] it is a large-text / UI tone only.
pub const TEXT_TERTIARY: Color = palette::TEXT_TERTIARY;
/// Dim text (the quietest legible tier) — the same tone as [`TEXT_TERTIARY`].
/// Kept as a distinct role name for the progress track and other chrome that
/// means "quietest", not "caption".
pub const TEXT_DIM: Color = palette::TEXT_TERTIARY;

// Semantic (base + bright). The palette carries one value per status; the
// `_BRIGHT` names remain as roles for call sites that mean "the text tone".
/// Success (base).
pub const SUCCESS: Color = palette::SUCCESS;
/// Success (bright — text / completed fills).
pub const SUCCESS_BRIGHT: Color = palette::SUCCESS;
/// Warning (base). Orange — distinct from both the brand sky and danger.
pub const WARNING: Color = palette::WARNING;
/// Warning (bright — text).
pub const WARNING_BRIGHT: Color = palette::WARNING;
/// Danger (base).
pub const DANGER: Color = palette::DANGER;
/// Danger (bright — legible removed-line / error text on the dark backdrop).
pub const DANGER_BRIGHT: Color = palette::DANGER;

// ── Categorical hues (deliberately NOT brand) ───────────────────────────────
//
// A few surfaces need more mutually-distinguishable colours than a four-hue
// brand palette provides: syntax tokens, graph node kinds, and one colour per
// concurrent agent. Making those the brand hue would violate the reservation
// above, so they are a categorical set — the same role the observatory's `--c1..--c4`
// data-mark palette plays. They carry no brand meaning and must never be used
// for brand, status, or "active".

/// Violet — process/structural events (links, diff hunk headers, graph
/// relations, trace stages, the user's own prompt). Categorical, not the brand
/// accent.
pub const VIOLET: Color = Color::Rgb(0xA7, 0x8B, 0xFA);
/// Amber — the second categorical hue: syntax keywords, file/module graph
/// nodes. Warm on purpose. Its predecessor was an azure that sat a few dozen
/// RGB units from the brand sky, which the palette-law test now rejects
/// outright; retiring gold as the brand freed the warm end of the spectrum,
/// and a categorical set is only useful to the extent its members cannot be
/// mistaken for one another or for the accent.
pub const AMBER: Color = Color::Rgb(0xE3, 0xB3, 0x41);
/// Teal — the third categorical hue: media traces and one slot of the
/// per-agent palette. Blue-green, so it reads apart from both the pale brand
/// sky and the success green. It replaces a glacier blue (`AGENT_ICE`) and
/// then a lilac, each of which drifted within confusable range of the accent.
pub const TEAL: Color = Color::Rgb(0x2D, 0xD4, 0xBF);

// ── Role aliases (what the rest of the crate references) ─────────────────────
// Role names remap onto the palette so call sites read as intent (accent,
// ink, rule) rather than as a hue that a future recolor would falsify.

/// Stella brand accent — bright sky blue. Brand, active/running, and progress
/// only. In the transcript, the tool name and nothing else.
pub const ACCENT: Color = palette::SKY;
/// A deeper accent (gradient / pressed).
pub const ACCENT_DEEP: Color = palette::SKY_DEEP;
/// Near-white primary text.
pub const INK: Color = TEXT_PRIMARY;
/// Dimmed secondary text.
pub const MUTED: Color = TEXT_SECONDARY;
/// Panel border / rule.
pub const RULE: Color = HAIRLINE;

/// Background tint for the transcript entry selected with the arrow keys —
/// a barely-there slate lift so the highlight reads without shouting.
pub const SELECT_BG: Color = palette::RAISED;

/// Success / positive / added lines.
pub const OK: Color = SUCCESS_BRIGHT;
/// Warning / needs-input.
pub const WARN: Color = WARNING_BRIGHT;
/// Error / removed lines / failure.
pub const BAD: Color = DANGER_BRIGHT;
/// Structural / process accent — aliased to [`VIOLET`], a categorical hue, so
/// process events (links, diff hunk headers, graph relations, trace
/// stage/tool/vcs) stay distinct from the brand accent instead of
/// competing with it. Process is not "active"; only [`ACCENT`] means that.
pub const RUN: Color = VIOLET;
/// Paused / held — violet.
pub const HELD: Color = VIOLET;

// ── Diff panel ──────────────────────────────────────────────────────────────

/// Subtle background tint behind added diff lines (the GitHub-PR reading —
/// pair with [`OK`] foreground).
pub const DIFF_ADD_BG: Color = Color::Rgb(20, 44, 26);
/// Subtle background tint behind removed diff lines (pair with [`BAD`]).
pub const DIFF_DEL_BG: Color = Color::Rgb(52, 24, 26);

/// Background behind the bytes of an added line that actually changed, when a
/// `-`/`+` pair is close enough to word-diff. Two steps brighter than
/// [`DIFF_ADD_BG`]: the whole line is already "added", so this has to read as
/// a second level of emphasis *within* it rather than a different category.
pub const DIFF_ADD_BG_EMPH: Color = Color::Rgb(26, 82, 44);
/// Background behind a live search match. Warm enough to find by eye while
/// scrolling, muted enough not to outshout the `✗` rail beside it — a match is
/// something you asked for, not something that went wrong.
pub const MATCH_BG: Color = Color::Rgb(92, 74, 0);

/// The removed-line counterpart of [`DIFF_ADD_BG_EMPH`].
pub const DIFF_DEL_BG_EMPH: Color = Color::Rgb(102, 34, 42);

// ── Syntax highlighting (diff bodies) ───────────────────────────────────────
//
// A four-color code palette layered *under* the add/remove diff semantics:
// the `+`/`-` background always wins (add/remove is never lost — see
// `crate::diff`), while a recognized token overrides only the foreground.
// Every color is chosen to read on all three diff backdrops (add green, del
// red, and the plain panel), and every one is *categorical*: syntax is not
// brand, not status, and not activity, so none of these is the brand hue. Keyword
// takes [`AMBER`], strings a soft spring green, numbers the [`VIOLET`]
// anchor, and comments dim toward [`MUTED`].

/// Language keyword (`fn`/`let`/`def`/`import`/`return`…).
pub const SYNTAX_KEYWORD: Color = AMBER;
/// String / char literal.
pub const SYNTAX_STRING: Color = Color::Rgb(126, 231, 135);
/// Numeric literal — violet, the counterpoint to the azure keyword stop.
pub const SYNTAX_NUMBER: Color = VIOLET;
/// Line comment (rendered dimmed + italic).
pub const SYNTAX_COMMENT: Color = Color::Rgb(118, 124, 134);

// ── Brand gradient (the wordmark sweep and the progress-bar fill) ───────────
//
// Two stops, not three: the sweep runs deep sky → sky, left to right. An
// earlier generation ran two separate gradients — one for brand chrome and a
// second for the progress bar — which is precisely the split this palette
// collapses. Progress *is* activity, activity is the brand hue, so one
// gradient serves both.

/// Deep sky — the sweep's leading (left) stop.
pub const SKY_DEEP: Color = palette::SKY_DEEP;
/// Sky — the trailing stop; also the non-truecolor fallback fill.
pub const SKY: Color = palette::SKY;

/// The brand gradient's stops, left → right: deep sky → sky. The determinate
/// progress fill interpolates across these per cell (truecolor only; lesser
/// terminals collapse to a solid [`SKY`] fill).
pub const BRAND_STOPS: [Color; 2] = [SKY_DEEP, SKY];

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

/// The brand gradient sampled at `t ∈ [0, 1]`: deep sky at 0, sky at 1,
/// linearly interpolated across [`BRAND_STOPS`]. This is the run progress
/// bar's fill and the wordmark sweep.
pub fn brand_gradient(t: f64) -> Color {
    gradient_at(&BRAND_STOPS, t)
}

/// Sample an n-stop gradient at `t ∈ [0, 1]`. Panics on fewer than two stops,
/// which is a programming error rather than a runtime condition.
fn gradient_at(stops: &[Color], t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    let span = (stops.len() - 1) as f64;
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
/// 1. `NO_COLOR` present → [`ColorMode::None`]. It wins over every color
///    signal. Note this is *presence*, not the `no-color.org` letter, which
///    disables color only when the variable is present **and non-empty**;
///    `NO_COLOR=` therefore strips color here where the spec would keep it.
///    The looser reading is shared with `stella-cli`'s animation gate, so
///    tightening it belongs in one change across both, not here alone.
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
/// The 256-colour index is the nearest xterm cube/greyscale entry by Euclidean
/// RGB distance. The 16-colour index is *not*: at 16 colours the nearest entry
/// is frequently the wrong one semantically — `success` is nearest to cyan,
/// `danger` to grey — and a status that no longer reads as its meaning is worse
/// than one slightly off in hue. So the 16-colour column is chosen for meaning
/// (success green, danger red, warning yellow) and the diff tints keep their
/// green/red cube entries for the same reason. Truecolor terminals never see
/// either column.
const FALLBACKS: &[(Color, u8, u8)] = &[
    (GROUND, 16, 0),
    (SURFACE, 232, 0),
    (RAISED, 234, 8),
    (HAIRLINE, 236, 8),
    (TEXT_PRIMARY, 255, 15),
    (TEXT_SECONDARY, 109, 7),
    (TEXT_TERTIARY, 66, 8),
    // Sky and deep sky take *different* 256 indices (117/81) and different
    // ANSI-16 indices (bright cyan / cyan) so the progress fill keeps a
    // readable head-to-tail ramp even where the gradient collapses.
    (SKY, 117, 14),
    (SKY_DEEP, 81, 6),
    (SUCCESS, 78, 10),
    (WARNING, 208, 3),
    (DANGER, 204, 9),
    (VIOLET, 141, 13),
    (AMBER, 179, 3),
    (TEAL, 43, 2),
    (DIFF_ADD_BG, 22, 2),
    (DIFF_DEL_BG, 52, 1),
    (DIFF_ADD_BG_EMPH, 28, 2),
    (DIFF_DEL_BG_EMPH, 88, 1),
    (MATCH_BG, 58, 3),
    (SYNTAX_STRING, 114, 10),
    (SYNTAX_COMMENT, 244, 8),
];

/// Resolve one color for the mode actually in use. Truecolor passes through;
/// `None` (NO_COLOR) drops **every** color to `Reset` (terminal default);
/// 256/16 map via `FALLBACKS`. A color with no matching entry
/// (already-indexed, named, `Reset`, or an interpolated gradient cell) passes
/// through unchanged in the two indexed modes — this only ever narrows the
/// palette tokens, never anything else. That pass-through is a known gap for
/// interpolated cells (the `crate::fx` sweeps, `brand_gradient`): on a 256- or
/// 16-color terminal they still emit 24-bit SGR. Surfaces that care collapse
/// to a solid named token themselves when [`ColorMode::is_truecolor`] is false
/// (see `crate::progress`).
///
/// `None` must catch the *named* ANSI colors too (`Color::Green`,
/// `Color::DarkGray`, …), not just RGB: the single-session REPL still styles
/// its HUD, composer and cards with those, and leaving them intact left color
/// on screen under `NO_COLOR` — precisely what the spec forbids.
pub fn resolve(color: Color, mode: ColorMode) -> Color {
    match mode {
        ColorMode::Truecolor => color,
        ColorMode::None => Color::Reset,
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
        // Live work takes the brand hue — "active/running" is one of the three
        // things it is reserved for, and the only status that gets it.
        AgentStatus::Running => ACCENT,
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
/// three distinct categorical hues, none of them the reserved brand hue.
/// A node kind is a category, not an activity.
pub fn graph_kind_color(kind: &str) -> Color {
    match kind {
        "function" | "method" => RUN,
        "struct" | "enum" | "trait" => OK,
        "file" | "module" => AMBER,
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
/// Five distinct categorical hues — violet, amber, green, teal, secondary — none
/// of them the reserved brand hue (an agent is not "the brand") and none of
/// them danger (which reads as failure elsewhere, so it never brands a healthy
/// agent).
const AGENT_PALETTE: [Color; 5] = [HELD, AMBER, OK, TEXT_SECONDARY, TEAL];

/// A deterministic (not randomized — stable across processes and test runs)
/// color for one agent id, picked from `AGENT_PALETTE` by hashing the id.
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
/// `AMBER`/`TEAL` for produced artifacts (file, media) — categorical, so
/// an artifact never reads as "running" — a dim neutral for quiet
/// memory/context events, and the shared
/// `OK`/`WARN`/`BAD` semantics for verdicts, spend, and errors. Memory drops
/// to `TEXT_TERTIARY` rather than reuse violet — the process group already
/// owns that anchor.
pub fn trace_kind_color(kind: TraceKind) -> Color {
    match kind {
        TraceKind::Stage => RUN,
        TraceKind::Text => INK,
        TraceKind::Reasoning => MUTED,
        TraceKind::Tool => RUN,
        TraceKind::File => AMBER,
        TraceKind::Budget => WARN,
        TraceKind::Context => TEXT_TERTIARY,
        TraceKind::Verdict => OK,
        TraceKind::Media => TEAL,
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
        SKY,
        SKY_DEEP,
        SUCCESS,
        WARNING,
        DANGER,
        VIOLET,
        AMBER,
        TEAL,
        DIFF_ADD_BG,
        DIFF_DEL_BG,
        DIFF_ADD_BG_EMPH,
        DIFF_DEL_BG_EMPH,
        MATCH_BG,
        SYNTAX_STRING,
        SYNTAX_COMMENT,
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
        // Brand roles resolve to the generated palette, not to a local literal.
        assert_eq!(ACCENT, palette::SKY);
        assert_eq!(ACCENT_DEEP, palette::SKY_DEEP);
        assert_eq!(GROUND, palette::GROUND);
        assert_eq!(INK, TEXT_PRIMARY);
        assert_eq!(MUTED, TEXT_SECONDARY);
        assert_eq!(RULE, HAIRLINE);
        assert_eq!(OK, SUCCESS);
        assert_eq!(WARN, WARNING);
        assert_eq!(BAD, DANGER);
        assert_eq!(HELD, VIOLET);
        assert_eq!(RUN, VIOLET);
        // Syntax and process hues are categorical -- never the brand accent.
        assert_eq!(SYNTAX_NUMBER, VIOLET);
        assert_eq!(SYNTAX_KEYWORD, AMBER);
    }

    /// The palette law, in the form a future edit would actually break.
    ///
    /// The guard has outlived two recolours now (aurora → gold → sky), which is
    /// the point: each time, the *shape* of the law survived and only the hue
    /// it names changed. Keep rewriting it rather than deleting it.
    ///
    /// What must hold now:
    ///   1. The accent is the generated brand sky — not a local literal that
    ///      drifted, and not one of the retired hues.
    ///   2. The ground is neutral. A blue accent on a blue-tinted ground loses
    ///      the separation that makes it read as a signal, so the canvas may
    ///      not carry a colour cast.
    ///   3. Status hues stay clear of the brand: warning ramps orange and
    ///      success ramps green, so neither can be mistaken for "active".
    ///   4. The brand hue is reserved. No categorical or status role may hold
    ///      it, and no other role may sit close enough to be confused with it.
    #[test]
    fn palette_law_sky_is_the_brand_and_nothing_else() {
        const RETIRED_AURORA_CYAN: Color = Color::Rgb(0x3F, 0xE0, 0xFF);
        const RETIRED_EMBER_FLAME: Color = Color::Rgb(0xFF, 0x7E, 0x5F);
        const RETIRED_EMBER_CRIMSON: Color = Color::Rgb(0xC2, 0x18, 0x5B);
        const RETIRED_GOLD: Color = Color::Rgb(0xFF, 0xDD, 0x00);
        const RETIRED_GOLD_DEEP: Color = Color::Rgb(0xE0, 0xB8, 0x00);
        /// The old glacier blue, retired *because* it collided with the sky
        /// accent — the exact failure this test's clause 4 now prevents.
        const RETIRED_AGENT_ICE: Color = Color::Rgb(0xA8, 0xC7, 0xF0);

        // 1. The accent comes from the palette, not a drifted local literal.
        assert_eq!(ACCENT, palette::SKY, "the accent must be the brand sky");

        // The retired hues are gone from every token and alias.
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
        all.extend(BRAND_STOPS);
        for token in &all {
            for (retired, name) in [
                (RETIRED_AURORA_CYAN, "aurora cyan"),
                (RETIRED_EMBER_FLAME, "ember flame"),
                (RETIRED_EMBER_CRIMSON, "ember crimson"),
                (RETIRED_GOLD, "gold"),
                (RETIRED_GOLD_DEEP, "deep gold"),
                (RETIRED_AGENT_ICE, "agent ice"),
            ] {
                assert_ne!(*token, retired, "a token still holds {name}");
            }
        }

        // 2. The ground is neutral — no channel may outrun another, or the
        //    canvas takes on a cast that competes with the accent.
        for (ground, name) in [(GROUND, "GROUND"), (SURFACE, "SURFACE")] {
            let Color::Rgb(r, _g, b) = ground else {
                panic!("{name} must be a truecolor token");
            };
            assert!(
                u32::from(b) - u32::from(r) <= 0x14,
                "{name} carries a colour cast ({ground:?}); the canvas must stay neutral"
            );
        }

        // 3. Warning ramps orange (r > g > b) and success ramps green
        //    (g dominant), so status never reads as the blue "active".
        let Color::Rgb(wr, wg, wb) = WARNING else {
            panic!("WARNING must be a truecolor token");
        };
        assert!(
            wr > wg && wg > wb,
            "warning must ramp red > green > blue (orange)"
        );
        let Color::Rgb(sr, sg, sb) = SUCCESS else {
            panic!("SUCCESS must be a truecolor token");
        };
        assert!(sg > sr && sg > sb, "success must be green-dominant");

        // 4. The brand hue is reserved for brand / active / progress. No status
        //    or categorical role may quietly become it — nor creep close enough
        //    to be confused with it at a glance, which is how `AGENT_ICE` died.
        let Color::Rgb(ar, ag, ab) = ACCENT else {
            panic!("ACCENT must be a truecolor token");
        };
        for (role, name) in [
            (OK, "OK"),
            (WARN, "WARN"),
            (BAD, "BAD"),
            (RUN, "RUN"),
            (HELD, "HELD"),
            (VIOLET, "VIOLET"),
            (AMBER, "AMBER"),
            (TEAL, "TEAL"),
            (SYNTAX_KEYWORD, "SYNTAX_KEYWORD"),
            (SYNTAX_STRING, "SYNTAX_STRING"),
            (SYNTAX_NUMBER, "SYNTAX_NUMBER"),
        ] {
            assert_ne!(role, ACCENT, "{name} must not be the reserved brand sky");
            let Color::Rgb(r, g, b) = role else {
                panic!("{name} must be a truecolor token");
            };
            // Squared euclidean distance in RGB. 0x30 per channel is roughly
            // where two tones stop being tellable apart in a terminal cell.
            let d = (i32::from(r) - i32::from(ar)).pow(2)
                + (i32::from(g) - i32::from(ag)).pow(2)
                + (i32::from(b) - i32::from(ab)).pow(2);
            assert!(
                d > 3 * 0x30 * 0x30,
                "{name} ({role:?}) sits too close to the brand accent to be told apart"
            );
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
        assert_eq!(resolve(ACCENT, ColorMode::Truecolor), ACCENT);
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
        assert_eq!(resolve(ACCENT, ColorMode::None), Color::Reset);
        assert_eq!(resolve(Color::Indexed(9), ColorMode::None), Color::Reset);
        // A non-color (Reset) stays put — nothing to strip.
        assert_eq!(resolve(Color::Reset, ColorMode::None), Color::Reset);
        // The named ANSI colors are colors too: the single-session REPL styles
        // its HUD/composer/cards with them, so `NO_COLOR` must strip them as
        // surely as it strips a palette token.
        for named in [
            Color::Green,
            Color::Red,
            Color::Yellow,
            Color::Cyan,
            Color::DarkGray,
        ] {
            assert_eq!(
                resolve(named, ColorMode::None),
                Color::Reset,
                "NO_COLOR must strip the named ANSI colors too ({named:?})"
            );
        }
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
        buf.content[0].fg = ACCENT;
        degrade_buffer(&mut buf, ColorMode::Truecolor);
        assert_eq!(buf.content[0].fg, ACCENT);
    }

    #[test]
    fn degrade_buffer_resolves_every_cell_when_degraded() {
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 1, 1));
        buf.content[0].fg = ACCENT; // → 117 (256) / 14 (16)
        buf.content[0].bg = VIOLET; // → 141 (256) / 13 (16)
        degrade_buffer(&mut buf, ColorMode::Ansi256);
        assert_eq!(buf.content[0].fg, Color::Indexed(117));
        assert_eq!(buf.content[0].bg, Color::Indexed(141));
    }

    #[test]
    fn degrade_buffer_strips_color_under_no_color() {
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 1, 1));
        buf.content[0].fg = ACCENT;
        buf.content[0].bg = GROUND;
        degrade_buffer(&mut buf, ColorMode::None);
        assert_eq!(buf.content[0].fg, Color::Reset);
        assert_eq!(buf.content[0].bg, Color::Reset);
    }

    #[test]
    fn brand_gradient_spans_deep_sky_to_sky() {
        assert_eq!(brand_gradient(0.0), SKY_DEEP);
        assert_eq!(brand_gradient(1.0), SKY);
        // Monotonic, clamped, never panics across the range.
        for i in 0..=20 {
            let _ = brand_gradient(f64::from(i) / 20.0);
        }
        assert_eq!(brand_gradient(-1.0), SKY_DEEP);
        assert_eq!(brand_gradient(2.0), SKY);
    }

    #[test]
    fn lighten_moves_toward_white() {
        assert_eq!(lighten(ACCENT, 0.0), ACCENT);
        assert_eq!(lighten(ACCENT, 1.0), Color::Rgb(255, 255, 255));
    }
}
