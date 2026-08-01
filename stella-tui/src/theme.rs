//! The one place the deck's look is defined — colors, semantic styles, and
//! glyphs. Every view pulls from here so the deck reads as one system in both
//! the stella brand palette and its status semantics. No view hard-codes a
//! color; that is what keeps a 12-panel TUI feeling designed rather than
//! assembled.

use ratatui::style::{Color, Modifier, Style};

use crate::deck::TraceKind;
use crate::envelope::AgentStatus;
use crate::palette;

// ── stella palette — "Phosphor Gold on Ink" (brand kit v1.0, the comet) ─────
//
// One colour, owned. The ground is Ink, a warm-neutral near-black; Phosphor
// Gold is the signal — reserved for brand, the prompt, active/running,
// selection and focus, and for nothing else. Gold is the signal, never the
// surface: if everything is gold, nothing is, so a general-purpose highlight
// is exactly what this hue must not become. The one sanctioned gold *fill*
// is a pill that owns attention (the H1 title bar, a selected tab), and a
// gold fill always carries INK text — white on gold is 1.83:1. (Gold is the
// *default* accent; `stella-light` swaps it for the deep gold ramp on warm
// paper — see [`apply_theme`].)
//
// The old blue/gold split is collapsed: the identity chrome (the logo's
// block cursor, splash rules, section markers) and the interactive accent
// are the same gold now. What survives of the old law: **gold never carries
// a verdict.** Warning amber sits 4.0° from gold in hue, so an outcome may
// never be told from chrome by hue alone — status is always glyph-paired,
// and activity (running) is the only status that takes the accent. Enforced
// by `gold_never_carries_a_verdict`.
//
// The corollary the transcript actually depends on: **prose is paper.** The
// accent buys attention, so it may only be spent where attention is owed — on
// the deck that means the tool being called, the active tab, and the progress
// fill. Everything else is [`INK`] (warm paper) or [`MUTED`], and a row that
// wants to be scannable does it with a glyph and a column, not with a hue.
//
// Status always pairs colour with a glyph (see [`status_glyph`]) so hue never
// carries meaning alone — active ▶ vs done ✓ — which is also what keeps the
// deck readable under `NO_COLOR` and for red/green colour blindness.
//
// Every value below comes from [`crate::palette`] — the same source the brand
// kit's tokens are cut from. This module is the *semantic* layer over it:
// call sites reference roles (accent, ink, rule, ok) rather than hues, so a
// recolour is a palette edit rather than a hunt through the crate. Add a
// colour here only as a role; add a *value* in `palette`.
//
// Every token is 24-bit; [`degrade_buffer`] narrows it to 256- or 16-color, or
// strips it for `NO_COLOR`, once per frame for terminals that can't render
// truecolor. A theme switch is a second per-frame pass ([`apply_theme`]).

// Grounds (dark → light lift).
/// Deepest ground — full-bleed backdrops and the splash.
pub const VOID: Color = palette::VOID;
/// App background — Ink, the terminal's warm-neutral near-black. Applied as
/// a real frame fill by `render_deck`, not just assumed from the terminal.
pub const GROUND: Color = palette::GROUND;
/// Card / panel surface.
pub const SURFACE: Color = palette::SURFACE;
/// Raised panel (one step above surface).
pub const RAISED: Color = palette::RAISED;
/// Hairline border / rule — a warm charcoal seam. Deliberately below 3.0
/// contrast (1.26:1): it may never be the only thing conveying structure.
pub const HAIRLINE: Color = palette::HAIRLINE;
/// The louder seam, for a boundary that must actually read (1.57:1). Still
/// decoration — a stronger rule, not a substitute for a glyph or a gap.
pub const HAIRLINE_STRONG: Color = palette::HAIRLINE_STRONG;

// Text tiers (primary → dim) — warm paper over the kit's warm neutral ramp;
// no cool grays anywhere on the dark side.
/// Primary text.
pub const TEXT_PRIMARY: Color = palette::TEXT_PRIMARY;
/// Secondary text. The safe small-text tone on every ground.
pub const TEXT_SECONDARY: Color = palette::TEXT_SECONDARY;
/// Tertiary text (labels, captions). AA body on [`GROUND`]/[`SURFACE`]
/// (5.71:1 / 5.38:1); on [`RAISED`] (4.98:1) it sits right at the floor.
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
/// Warning (base). Amber — a hue-neighbour of the gold accent (4.0° apart),
/// which is exactly why a warning never appears without its glyph: the
/// glyph, not the hue, is the status carrier.
pub const WARNING: Color = palette::WARNING;
/// Warning (bright — text).
pub const WARNING_BRIGHT: Color = palette::WARNING;
/// Danger (base).
pub const DANGER: Color = palette::DANGER;
/// Danger (bright — legible removed-line / error text on the dark backdrop).
pub const DANGER_BRIGHT: Color = palette::DANGER;

// ── Categorical hues (deliberately NOT brand) ───────────────────────────────
//
// A few surfaces need more mutually-distinguishable colours than a one-hue
// brand palette provides: syntax tokens, graph node kinds, and one colour per
// concurrent agent. Making those the brand hue would violate the reservation
// above, so they are a categorical set — the *same four values* the
// observatory paints its data marks with, so a series in a chart and a chip in
// the deck agree. All are complements of gold that stay warm/rich on ink —
// muted violet (215° from gold), deep teal (134°), warm rose (70°) — and every
// one clears AA body (5.09:1 or better) on [`GROUND`]. They carry no brand
// meaning and must never be used for brand, status, or "active".

/// Violet — process/structural events (links, diff hunk headers, graph
/// relations, trace stages, the user's own prompt). Categorical, not the brand
/// accent. `data-2`, 5.29:1 on ground.
pub const VIOLET: Color = palette::DATA_2;
/// Amber — the stood-down categorical hue. `data-1` measures 1.06:1 against
/// the gold accent — the same brass at a glance — so with gold as THE accent
/// it may no longer colour a chip, a node, or an agent (the observatory
/// stands the same mark down from every plotted surface). It survives in one
/// context only: syntax keywords inside code bodies, where gold chrome never
/// reaches and the neighbouring tones are the diff washes and the other
/// syntax hues. 10.11:1 on ground.
pub const AMBER: Color = palette::DATA_1;
/// Teal — media traces and one slot of the per-agent palette. `data-4`,
/// 10.54:1 on ground. Deep blue-green: 134° from the gold accent and far
/// from the success green, and emphatically not the retired ice blue — it
/// replaced a glacier blue (`AGENT_ICE`) that drifted confusable with the
/// old accent, and it reads rich, not icy, on ink.
pub const TEAL: Color = palette::DATA_4;
/// Warm rose — file artifacts (trace chips, graph file/module nodes) and the
/// fourth chart series. `data-3`, 5.09:1 on ground. 70° from gold; 1.30:1
/// against [`DANGER`]'s red-pink, so it never carries an error meaning and
/// never appears without a neutral label or glyph beside it.
pub const MAGENTA: Color = palette::DATA_3;

// ── Role aliases (what the rest of the crate references) ─────────────────────
// Role names remap onto the palette so call sites read as intent (accent,
// ink, rule) rather than as a hue that a future recolor would falsify.

/// stella's brand accent — Phosphor Gold `#FFB81A`, the kit value lifted 10%
/// in lightness (11.36:1 on [`GROUND`]). Brand, active/running, focus, selection, and
/// progress only. In the transcript, the tool name and nothing else. The
/// active theme's actual hue is applied per-frame by [`apply_theme`]; this is
/// the canonical dark value every call site renders.
///
/// Unlike the blue it replaced, gold needs no separate text tone: the same
/// value clears AA on a glyph, a one-cell rule, and a fill on every dark
/// ground. When it is a *fill*, the text on it must be [`GROUND`]-dark —
/// ink on gold is 11.36:1 where white on gold is 1.73:1.
pub const ACCENT: Color = palette::BRAND;
/// The brand hue for *fills* — a pill, a bar body, a selected-tab wash. The
/// same Phosphor Gold: one owned colour means the stroke and the fill agree,
/// and the fill's legibility comes from pairing it with ink text, never from
/// a second hue.
pub const ACCENT_FILL: Color = palette::BRAND;
/// A deeper accent (gradient / pressed) — kit `gold-600` lifted 10% with the
/// brand, the leading stop of the progress fill (8.77:1 on ground, so the
/// fill's tail clears AA).
pub const ACCENT_DEEP: Color = palette::BRAND_DEEP;

/// The identity gold — the logo's block cursor, splash rules, section
/// markers, and brand chrome generally. The same Phosphor Gold as [`ACCENT`]:
/// under the comet kit, chrome and accent are one colour. **Never a
/// verdict**: it sits 4.0° from [`WARN`] in hue, and
/// `gold_never_carries_a_verdict` proves no outcome mapping can return it —
/// activity (running/active) is the one status that takes gold, by kit rule.
pub const GOLD: Color = palette::GOLD;
/// The bright stop of the gold sweep (headlines, the splash rule's leading
/// edge). Kit `gold-400`.
pub const GOLD_BRIGHT: Color = palette::GOLD_BRIGHT;
/// The trailing stop of the gold sweep. Kit `gold-700` — the identity sweep
/// runs wider and quieter than the progress fill's ([`ACCENT_DEEP`]).
pub const GOLD_DEEP: Color = palette::GOLD_DEEP;
/// Warm-paper primary text.
pub const INK: Color = TEXT_PRIMARY;
/// Dimmed secondary text.
pub const MUTED: Color = TEXT_SECONDARY;
/// Panel border / rule.
pub const RULE: Color = HAIRLINE;

/// Background tint for the transcript entry selected with the arrow keys —
/// a barely-there warm lift so the highlight reads without shouting. A full
/// gold wash would make the selection a surface, which gold may not be; the
/// gold *pill* treatment is reserved for single-line attention (the H1 bar,
/// an active tab), where ink-on-gold text keeps it legible.
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

// ── Runtime theme (the `/theme` switch) ──────────────────────────────────────
//
// Colours above are compile-time constants: the canonical **dark** palette
// (Phosphor Gold on Ink), which is what every view renders and
// what ships as the default. A theme switch is applied *after* the widgets draw, as a
// per-frame value→value remap over the finished buffer ([`apply_theme`]) —
// the exact mechanism [`degrade_buffer`] already uses for colour-depth. That
// keeps ~700 `theme::TOKEN` call sites untouched: a theme is a substitution
// table, not a parameter threaded through the render tree.
//
// The one thing a value remap can't recolour is the progress bar's *gradient*,
// whose interpolated cells are never equal to a token; so its source
// ([`brand_gradient`] via [`primary_stops`]) is theme-aware directly.

/// The shipped themes. `stella-dark` (Phosphor Gold on Ink) is the default;
/// `stella-light` (the deep gold ramp on warm paper) is its complement. The
/// names match the `/theme` argument and the `ui.theme` settings value
/// verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeName {
    /// Phosphor Gold on Ink. Default.
    #[default]
    StellaDark,
    /// The same gold darkened onto warm paper.
    StellaLight,
}

impl ThemeName {
    /// The stable slug used by `/theme <slug>` and persisted in settings.
    pub fn slug(self) -> &'static str {
        match self {
            ThemeName::StellaDark => "stella-dark",
            ThemeName::StellaLight => "stella-light",
        }
    }

    /// Parse a slug (case-insensitive; `_`/space tolerated for `-`). Returns
    /// `None` for anything that isn't a shipped theme, so the command surface
    /// can report the valid set rather than silently no-op.
    pub fn parse(s: &str) -> Option<Self> {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace([' ', '_'], "-")
            .as_str()
        {
            "stella-dark" | "dark" => Some(ThemeName::StellaDark),
            "stella-light" | "light" => Some(ThemeName::StellaLight),
            _ => None,
        }
    }

    /// Every shipped theme, in menu order.
    pub const ALL: [ThemeName; 2] = [ThemeName::StellaDark, ThemeName::StellaLight];
}

/// The active theme, as a plain index so it is lock-free to read on the render
/// hot path (`0` = dark, `1` = light). Mirrors the single-session REPL's
/// `ACCENT` atomic. Read via [`active_theme`], set via [`set_active_theme`].
static ACTIVE_THEME: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// The theme every frame renders through. Defaults to [`ThemeName::StellaDark`].
pub fn active_theme() -> ThemeName {
    match ACTIVE_THEME.load(std::sync::atomic::Ordering::Relaxed) {
        1 => ThemeName::StellaLight,
        _ => ThemeName::StellaDark,
    }
}

/// Switch the active theme. Takes effect on the next frame — the switch is a
/// buffer remap, so nothing needs re-laying-out.
pub fn set_active_theme(theme: ThemeName) {
    let v = match theme {
        ThemeName::StellaDark => 0,
        ThemeName::StellaLight => 1,
    };
    ACTIVE_THEME.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// The active theme's brand gradient stops, deep → bright: `brand-deep` →
/// `brand` (Phosphor Gold) on `stella-dark`, `brand-ink-deep` → `brand-ink`
/// on `stella-light`. Feeds [`brand_gradient`] so the progress fill and
/// wordmark sweep recolour with the theme.
///
/// The bright end is [`ACCENT`] (the gold itself, not `brand-bright`), so
/// that the truecolor gradient lands on the same value a degraded terminal
/// falls back to — `crate::progress` paints a solid [`ACCENT`] when
/// [`ColorMode::is_truecolor`] is false, and a fill whose colour jumped
/// between depths would be a downgrade-path bug. (`brand-bright` belongs to
/// the identity sweep's high end, [`gold_stops`].)
pub fn primary_stops() -> [Color; 2] {
    match active_theme() {
        ThemeName::StellaDark => [palette::BRAND_DEEP, palette::BRAND],
        ThemeName::StellaLight => [palette::BRAND_INK_DEEP, palette::BRAND_INK],
    }
}

/// The active theme's gold gradient stops, deep → bright. Gold is identity
/// chrome: the splash rule, section markers, the wordmark's cursor block. It
/// is never a verdict and never a data mark, so nothing that resolves an
/// *outcome* may call this.
pub fn gold_stops() -> [Color; 2] {
    match active_theme() {
        ThemeName::StellaDark => [palette::GOLD_DEEP, palette::GOLD_BRIGHT],
        // One gold on paper: `gold-ink` (the kit's `gold-deep`, its one
        // light-ground gold) holds the 3:1 graphical floor on paper, so the
        // light sweep is flat by design. Gold *text* on paper is darker
        // still — the flat-token remap sends it to `brand-ink`.
        ThemeName::StellaLight => [palette::GOLD_INK, palette::GOLD_INK],
    }
}

/// The gold gradient sampled at `t ∈ [0, 1]` — the counterpart of
/// [`brand_gradient`] for identity chrome.
pub fn gold_gradient(t: f64) -> Color {
    gradient_at(&gold_stops(), t)
}

/// The active theme's bright primary. Same value the flat
/// [`ACCENT`] remaps to, but resolved eagerly — use this only where a colour is
/// *interpolated* (lightened, swept), since [`apply_theme`]'s value remap can't
/// reach an interpolated cell. Flat fills should keep using [`ACCENT`].
pub fn primary() -> Color {
    primary_stops()[1]
}

/// The active theme's deep primary — the interpolation counterpart of
/// [`primary`] (see its note on when to reach for these over [`ACCENT_DEEP`]).
pub fn primary_deep() -> Color {
    primary_stops()[0]
}

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
// brand, not status, and not activity. Keyword takes [`AMBER`] — the one
// surviving home of the amber mark, legitimate here because gold chrome
// never enters a code body, so nothing nearby can be confused for "active" —
// strings a soft spring green, numbers the [`VIOLET`] anchor, and comments
// dim toward the caption tier.

/// Language keyword (`fn`/`let`/`def`/`import`/`return`…).
pub const SYNTAX_KEYWORD: Color = AMBER;
/// String / char literal.
pub const SYNTAX_STRING: Color = Color::Rgb(126, 231, 135);
/// Numeric literal — violet, the counterpoint to the amber keyword stop.
pub const SYNTAX_NUMBER: Color = VIOLET;
/// Line comment (rendered dimmed + italic). The same warm neutral as
/// [`TEXT_TERTIARY`] — "comments dim toward the caption tier" made literal,
/// and the cool gray it replaces is banned from the dark side outright.
pub const SYNTAX_COMMENT: Color = palette::TEXT_TERTIARY;

/// Inline code spans and fenced-code plain runs (`crate::markdown`). A calm
/// sage green — quiet enough that a backticked word reads as *technical*
/// rather than as emphasis, and 105° of hue away from [`ACCENT`] so it never
/// reads as *active* (8.94:1 on ground). This replaces the warning-orange the
/// transcript used to paint every `identifier` with: code is not a warning,
/// and an alarm hue on every backticked word was the single loudest thing on
/// the deck. Not a brand value — it is TUI-only and has no entry in
/// `palette`.
pub const CODE: Color = Color::Rgb(0x6F, 0xBF, 0x92);

// ── Brand gradient (the wordmark sweep and the progress-bar fill) ───────────
//
// Two stops, not three: the sweep runs deep → bright, left to right. An
// earlier generation ran two separate gradients — one for brand chrome and a
// second for the progress bar — which is precisely the split this palette
// collapses. Progress *is* activity, activity is the brand hue, so one
// gradient serves both. The stops track the ACTIVE theme (gold on ink, the
// deep gold ramp on paper) via [`primary_stops`]: the progress fill
// interpolates non-token cells the per-frame [`apply_theme`] remap can't see,
// so its source has to be theme-aware directly.
//
// The identity chrome keeps its own wider two-stop sweep ([`gold_stops`] /
// [`gold_gradient`]) — the splash rule and section markers. Same family now
// (gold-700 → gold-400 around the same hue), but a separate gradient: chrome
// may sweep quiet-to-bright while the progress fill's tail must hold AA on
// its own, and the progress bar's degraded fallback is a solid [`ACCENT`].

/// The brand gradient's stops for the *default* (dark) theme, deep → bright.
/// [`primary_stops`] returns these for `stella-dark` and the paper pair for
/// `stella-light`; prefer that accessor. The determinate progress fill
/// interpolates across the active stops per cell (truecolor only; lesser
/// terminals collapse to a solid [`ACCENT`] fill).
pub const BRAND_STOPS: [Color; 2] = [ACCENT_DEEP, ACCENT];

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

/// The brand gradient sampled at `t ∈ [0, 1]`: deep at 0, bright at 1, linearly
/// interpolated across the ACTIVE theme's [`primary_stops`]. This is the run
/// progress bar's fill and the wordmark sweep — recolours with the theme.
pub fn brand_gradient(t: f64) -> Color {
    gradient_at(&primary_stops(), t)
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
    (VOID, 232, 0),
    (GROUND, 232, 0),
    (SURFACE, 233, 0),
    (RAISED, 234, 8),
    (HAIRLINE, 235, 8),
    (HAIRLINE_STRONG, 236, 8),
    (TEXT_PRIMARY, 255, 15),
    (TEXT_SECONDARY, 246, 7),
    (TEXT_TERTIARY, 245, 8),
    // Phosphor Gold `#FFB81A` sits one unit from cube entry 214 (255,175,0) —
    // for once the terminal cube holds the brand almost exactly. The family
    // stays on the *yellow* side everywhere: the nearest entry to
    // `gold-bright` is 215 (255,175,95), an orange, and orange is the one
    // thing this identity may not render, so it takes 221 (255,215,95)
    // instead; `gold-deep` likewise skips orange 130 for 136 (175,135,0).
    // The progress ramp's deep stop takes 178 rather than nearest-entry 172
    // (215,135,0, again orange) — it shares that dark yellow with `warning`,
    // which is acceptable because a degraded terminal never paints the
    // gradient at all (`crate::progress` collapses to solid ACCENT), while
    // ACCENT vs ACCENT_DEEP staying distinct keeps the head-to-tail ramp
    // honest. At 16 colours the accent family takes bright yellow (11) and
    // the deep stop plain yellow's darker sibling is unavailable, so it
    // shares 3 with warning under the same never-painted reasoning; GOLD
    // family values stay on 11 so chrome never collapses onto `warning`'s
    // plain yellow — gold is chrome, warning is a verdict, and the two may
    // not become the same cell.
    (ACCENT, 214, 11), // also ACCENT_FILL and GOLD (one value, one entry)
    (GOLD_BRIGHT, 221, 11),
    (ACCENT_DEEP, 178, 3),
    (GOLD_DEEP, 136, 11),
    (SUCCESS, 78, 10),
    (WARNING, 178, 3),
    (DANGER, 204, 9),
    (VIOLET, 98, 13),
    (AMBER, 179, 3),
    (TEAL, 44, 6),
    (MAGENTA, 168, 5),
    (CODE, 72, 2),
    (DIFF_ADD_BG, 22, 2),
    (DIFF_DEL_BG, 52, 1),
    (DIFF_ADD_BG_EMPH, 28, 2),
    (DIFF_DEL_BG_EMPH, 88, 1),
    (MATCH_BG, 58, 3),
    (SYNTAX_STRING, 114, 10),
];

/// Resolve one color for the mode actually in use. Truecolor passes through;
/// `None` (NO_COLOR) drops **every** color to `Reset` (terminal default);
/// 256/16 map via `FALLBACKS`. A color with no matching entry
/// (already-indexed, named, `Reset`, or an interpolated gradient cell) passes
/// through unchanged in the two indexed modes — this only ever narrows the
/// palette tokens, never anything else. That pass-through is a known gap for
/// interpolated cells (animated sweeps, `brand_gradient`): on a 256- or
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

// ── Theme remap (the `stella-light` value substitution) ──────────────────────
//
// `stella-dark` is the canonical palette the widgets render, so its remap is
// the identity. `stella-light` maps every dark token to its paper counterpart,
// keyed by value exactly like [`FALLBACKS`]. Role aliases share a value with a
// palette token (INK==TEXT_PRIMARY, OK==SUCCESS, SELECT_BG==RAISED, …), so one
// entry per distinct value covers every alias — the same property that makes
// [`FALLBACKS`] one-entry-per-value. The categorical/syntax/diff targets are
// TUI-only (not brand tokens), so they live here inline rather than in
// `palette`; the brand, ground, text and status targets come from `palette`.

/// `stella-light`: every canonical (dark) token value → its paper counterpart.
const LIGHT_REMAP: &[(Color, Color)] = &[
    // Grounds → paper.
    (VOID, palette::PAPER),
    (GROUND, palette::PAPER),
    (SURFACE, palette::SNOW),
    (RAISED, palette::PAPER_RAISED),            // also SELECT_BG
    (HAIRLINE, palette::PAPER_HAIRLINE),        // also RULE
    (HAIRLINE_STRONG, palette::PAPER_HAIRLINE), // one paper seam serves both
    // Text → ink (INK==TEXT_PRIMARY, MUTED==TEXT_SECONDARY, DIM==TERTIARY;
    // SYNTAX_COMMENT rides the tertiary entry too).
    (TEXT_PRIMARY, palette::INK),
    (TEXT_SECONDARY, palette::MUTED),
    (TEXT_TERTIARY, palette::INK_DIM),
    // Brand → the deep gold ramp. ACCENT, ACCENT_FILL and GOLD are one
    // Phosphor Gold value, so one entry sends every flat gold cell to
    // `brand-ink` (kit gold-800 lifted 10%, 5.22:1 on paper) — the kit's `gold-deep`
    // is reserved for *graphical* chrome via `gold_stops`, because at
    // 3.79:1 it cannot carry terminal-cell text on paper.
    (ACCENT, palette::BRAND_INK),
    (GOLD_BRIGHT, palette::GOLD_INK),
    (ACCENT_DEEP, palette::BRAND_INK_DEEP),
    // `gold-deep` already IS the kit's light-ground gold; its entry is the
    // identity so the sweep's trailing stop needs no second value.
    (GOLD_DEEP, palette::GOLD_INK),
    // Status → ink variants (OK/BRIGHT share the base value).
    (SUCCESS, palette::SUCCESS_INK),
    (WARNING, palette::WARNING_INK),
    (DANGER, palette::DANGER_INK),
    // Inline code — a darker sage, 5.26:1 on paper.
    (CODE, Color::Rgb(0x2A, 0x71, 0x50)),
    // Categorical hues, darkened for AA on the warm paper (RUN/HELD/NUMBER==
    // VIOLET, KEYWORD==AMBER). Teal drops to teal-700: the old teal-600
    // measured 3.35:1 on the kit's paper, under the 4.5:1 text floor.
    (VIOLET, Color::Rgb(0x6D, 0x28, 0xD9)),
    (AMBER, Color::Rgb(0x92, 0x40, 0x0E)),
    (TEAL, Color::Rgb(0x0F, 0x76, 0x6E)),
    (MAGENTA, Color::Rgb(0xA6, 0x18, 0x5C)),
    // Syntax bodies. Strings take green-800 (6.38:1 — green-700 sat at
    // 4.49:1 on the warm paper).
    (SYNTAX_STRING, Color::Rgb(0x16, 0x65, 0x34)),
    // Diff tints → light washes.
    (DIFF_ADD_BG, Color::Rgb(0xDC, 0xFC, 0xE7)),
    (DIFF_DEL_BG, Color::Rgb(0xFE, 0xE2, 0xE2)),
    (DIFF_ADD_BG_EMPH, Color::Rgb(0xA7, 0xF3, 0xC6)),
    (DIFF_DEL_BG_EMPH, Color::Rgb(0xFB, 0xB4, 0xBD)),
    (MATCH_BG, Color::Rgb(0xFE, 0xF0, 0x8A)),
];

/// Remap one colour through a theme table; unmapped colours pass through (the
/// same contract as [`resolve`], so gradient/interpolated cells are untouched).
fn remap_theme(color: Color, table: &[(Color, Color)]) -> Color {
    table
        .iter()
        .find_map(|(from, to)| (*from == color).then_some(*to))
        .unwrap_or(color)
}

/// Recolour the finished frame to the active theme, in place — run once per
/// frame *before* [`degrade_buffer`]. `stella-dark` is the canonical identity
/// (no-op). `stella-light` is a truecolor experience: on a degraded terminal we
/// leave the canonical dark palette so `degrade_buffer` can map it to indices
/// (the paper hues have no 256/16 fallback). See `deck_shell` / `shell` /
/// `fleet_dashboard` for the call sites.
pub fn apply_theme(buf: &mut ratatui::buffer::Buffer, mode: ColorMode) {
    if !mode.is_truecolor() {
        return;
    }
    let table = match active_theme() {
        ThemeName::StellaDark => return,
        ThemeName::StellaLight => LIGHT_REMAP,
    };
    for cell in buf.content.iter_mut() {
        cell.fg = remap_theme(cell.fg, table);
        cell.bg = remap_theme(cell.bg, table);
        cell.underline_color = remap_theme(cell.underline_color, table);
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
/// function/method violet, struct/enum/trait green, file/module warm rose —
/// three distinct categorical hues, none of them the reserved brand accent
/// (the rose replaces an amber that became indistinguishable from the gold
/// accent, and it matches the Traces tab's file chip so "file" is one colour
/// everywhere). A node kind is a category, not an activity.
pub fn graph_kind_color(kind: &str) -> Color {
    match kind {
        "function" | "method" => RUN,
        "struct" | "enum" | "trait" => OK,
        "file" | "module" => MAGENTA,
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
/// Five distinct categorical hues — violet, rose, green, teal, secondary —
/// none of them the reserved brand hue (an agent is not "the brand"; the
/// amber slot the rose replaces was 1.06:1 against the gold accent, and a
/// chip that can be mistaken for "running" is worse than no chip) and none
/// of them danger (which reads as failure elsewhere, so it never brands a
/// healthy agent).
const AGENT_PALETTE: [Color; 5] = [HELD, MAGENTA, OK, TEXT_SECONDARY, TEAL];

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
/// `MAGENTA`/`TEAL` for produced artifacts (file, media) — categorical, so
/// an artifact never reads as "running"; the file chip's warm rose sits
/// 1.30:1 from [`BAD`], which is tolerable exactly because a chip is always
/// a coloured *word* and an error is always glyph-paired — a dim neutral for
/// quiet memory/context events, and the shared `OK`/`WARN`/`BAD` semantics
/// for verdicts, spend, and errors. Memory drops to `TEXT_TERTIARY` rather
/// than reuse violet — the process group already owns that anchor.
pub fn trace_kind_color(kind: TraceKind) -> Color {
    match kind {
        TraceKind::Stage => RUN,
        TraceKind::Text => INK,
        TraceKind::Reasoning => MUTED,
        TraceKind::Tool => RUN,
        TraceKind::File => MAGENTA,
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
#[path = "theme/tests.rs"]
mod tests;
