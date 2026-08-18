//! The one place the deck's look is defined — colors, semantic styles, and
//! glyphs. Every view pulls from here so the deck reads as one system in both
//! the stella brand palette and its status semantics. No view hard-codes a
//! color; that is what keeps a 12-panel TUI feeling designed rather than
//! assembled.

use ratatui::style::{Color, Modifier, Style};

use crate::deck::TraceKind;
use crate::envelope::AgentStatus;
use crate::palette;

// ── stella palette — "Ion on Obsidian" (brand kit v3.0, the comet) ─────────
//
// One colour, owned. The ground is Obsidian, a cool blue-black; Ion (an
// electric azure-cyan) is the signal — reserved for brand, the prompt,
// active/running, selection and focus, and for nothing else. Ion is the
// signal, never the surface: if everything is ion, nothing is, so a
// general-purpose highlight is exactly what this hue must not become. The one
// sanctioned ion *fill* is a pill that owns attention (the H1 title bar, a
// selected tab), and an ion fill always carries GROUND text — white on ion is
// 1.83:1. (Ion is the *default* accent; `stella-light` swaps it for the deep
// ion ramp on cool paper — see [`apply_theme`].)
//
// The identity chrome (the logo's block cursor, splash rules, section
// markers) and the interactive accent are one colour, as they were under the
// bronze kit this replaces. What survives of the old law, and what changes:
// **identity never carries a verdict** — but it no longer needs the 4.0°
// hue-collision argument that used to justify it, because the nearest status
// hue is now 35° away and the warning amber 146°. Chrome and verdict stay
// separate because they are
// different jobs, not because they could be confused; status is still always
// glyph-paired, and activity (running) is the only status that takes the
// accent. Enforced by `identity_never_carries_a_verdict`.
//
// The corollary the transcript actually depends on: **prose is paper.** The
// accent buys attention, so it may only be spent where attention is owed — on
// the deck that means the tool being called, the active tab, and the progress
// fill. Everything else is [`INK`] (cool paper) or [`MUTED`], and a row that
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
/// App background — Obsidian, the terminal's cool blue-black. Applied as
/// a real frame fill by `render_deck`, not just assumed from the terminal.
pub const GROUND: Color = palette::GROUND;
/// Card / panel surface.
pub const SURFACE: Color = palette::SURFACE;
/// Raised panel (one step above surface).
pub const RAISED: Color = palette::RAISED;
/// Hairline border / rule — a cool graphite seam. Deliberately below 3.0
/// contrast (1.48:1): it may never be the only thing conveying structure.
pub const HAIRLINE: Color = palette::HAIRLINE;
/// The louder seam, for a boundary that must actually read (1.94:1). Still
/// decoration — a stronger rule, not a substitute for a glyph or a gap.
pub const HAIRLINE_STRONG: Color = palette::HAIRLINE_STRONG;

// Text tiers (primary → dim) — cool paper over the kit's graphite ramp; no
// warm greys anywhere on the dark side.
/// Primary text.
pub const TEXT_PRIMARY: Color = palette::TEXT_PRIMARY;
/// Secondary text. The safe small-text tone on every ground.
pub const TEXT_SECONDARY: Color = palette::TEXT_SECONDARY;
/// Tertiary text (labels, captions). AA body on [`GROUND`]/[`SURFACE`]
/// (5.96:1 / 5.40:1); on [`RAISED`] (4.67:1) it sits right at the floor.
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
/// Warning (base). Amber — 146° from the ion accent, so unlike the bronze
/// era nothing in the chrome can be mistaken for it. A warning still never
/// appears without its glyph: the glyph, not the hue, is the status carrier.
pub const WARNING: Color = palette::WARNING;
/// Warning (bright — text).
pub const WARNING_BRIGHT: Color = palette::WARNING;
/// Danger (base).
pub const DANGER: Color = palette::DANGER;
/// Danger (bright — legible removed-line / error text on the dark backdrop).
pub const DANGER_BRIGHT: Color = palette::DANGER;

/// The oracle's **pre-flip** state — the one place red carries meaning in the
/// deck (D6): the `red` token in the witness panel's author line and its
/// `red ──▸ green` result line. A healthy, *expected* state ("the test fails
/// before the patch — good"), so it deliberately does not share a value with
/// [`DANGER`]: a failure hue on the very state the pipeline is supposed to
/// produce would teach readers to ignore the failure hue. Nothing else may
/// take this role.
pub const ORACLE_PRE_FLIP: Color = palette::ORACLE_RED;

// ── Categorical hues (deliberately NOT brand) ───────────────────────────────
//
// A few surfaces need more mutually-distinguishable colours than a one-hue
// brand palette provides: syntax tokens, graph node kinds, and one colour per
// concurrent agent. Making those the brand hue would violate the reservation
// above, so they are a categorical set — the *same six values* the observatory
// paints its data marks with, so a series in a chart and a chip in the deck
// agree. Every one sits at least 50° of hue from the ion accent and at least
// 34° from every other mark, and every one clears AA body (6.20:1 or better)
// on [`GROUND`]. They carry no brand meaning and must never be used for
// brand, status, or "active".
//
// Under the bronze/gold system this set had to be "complements of gold that
// stay warm on ink". A cool accent inverts that: five of the six now sit in
// the cool half of the wheel, and the one member that used to sit 0.8° from
// the accent (the amber) is 170° from it — still stood down from chips, but
// for a different reason now (see [`AMBER`]).

/// Periwinkle — process/structural events (links, diff hunk headers, graph
/// relations, trace stages, the user's own prompt). Categorical, not the brand
/// accent. `data-2`, 6.20:1 on ground, 50° from [`ACCENT`] — the closest mark
/// in the set — and 1.74:1 against it.
pub const VIOLET: Color = palette::DATA_2;
/// Apricot — syntax keywords inside code bodies and the first chart series.
/// `data-1`, 11.27:1 on ground.
///
/// Still the one stood-down mark, but the reason moved with the hue. Under
/// the gold accent it was 0.8° and 1.12:1 from the brand and could not colour
/// a chip, a node or an agent. It is now 170° from the accent and 24° from
/// [`WARN`] at 1.08:1 — a pastel beside a saturated amber, which is enough
/// inside a code body where no verdict is ever painted, and not enough on a
/// chip standing next to a status glyph. So the confinement to
/// [`SYNTAX_KEYWORD`] survives the recolour on its own merits.
pub const AMBER: Color = palette::DATA_1;
/// Jade — media traces, one slot of the per-agent palette, and the subagent
/// mark. `data-4`, 9.97:1 on ground, 72° from the accent and 37° from [`OK`].
///
/// It replaces the *teal* the bronze kit used here. Teal was a good
/// complement of gold (134° away) and a bad neighbour of ion (24°), which is
/// the whole cost of moving a brand across the wheel — the one categorical
/// value the recolour could not carry over.
pub const JADE: Color = palette::DATA_4;
/// Magenta — file artifacts (trace chips, graph file/module nodes) and the
/// fourth chart series. `data-3`, 6.24:1 on ground. 122° from the accent;
/// 1.17:1 against [`DANGER`]'s pink-red at 31° of hue, so it never carries an
/// error meaning and never appears without a neutral label or glyph beside
/// it.
pub const MAGENTA: Color = palette::DATA_3;
/// Citron — the repository/VCS tool class in the transcript. `data-5`,
/// 12.50:1 on ground, 112° from the accent and 34°/77° from the warning amber
/// and the success green.
pub const CITRON: Color = palette::DATA_5;
/// Orchid — the delegation/orchestration tool class in the transcript.
/// `data-6`, 8.00:1 on ground, 89° from the accent, 38° from the periwinkle
/// and 34° from the magenta.
pub const ORCHID: Color = palette::DATA_6;

// ── Role aliases (what the rest of the crate references) ─────────────────────
// Role names remap onto the palette so call sites read as intent (accent,
// ink, rule) rather than as a hue that a future recolor would falsify.

/// stella's brand accent — Ion `#00D1F9`, the kit's `brand-500` verbatim
/// (10.79:1 on [`GROUND`]). Brand, active/running, focus, selection, and
/// progress only. In the transcript, the tool name and nothing else. The
/// active theme's actual hue is applied per-frame by [`apply_theme`]; this is
/// the canonical dark value every call site renders.
///
/// Like the gold it replaced, ion needs no separate text tone: the same value
/// clears AA on a glyph, a one-cell rule, and a fill on every dark ground.
/// When it is a *fill*, the text on it must be [`GROUND`]-dark — obsidian on
/// ion is 10.79:1 where white on ion is 1.83:1.
pub const ACCENT: Color = palette::BRAND;
/// The brand hue for *fills* — a pill, a bar body, a selected-tab wash. The
/// same Ion: one owned colour means the stroke and the fill agree, and the
/// fill's legibility comes from pairing it with obsidian text, never from a
/// second hue.
pub const ACCENT_FILL: Color = palette::BRAND;
/// A deeper accent (gradient / pressed) — kit `brand-600`, the leading stop
/// of the progress fill (7.65:1 on ground, so the fill's tail clears AA).
pub const ACCENT_DEEP: Color = palette::BRAND_DEEP;

/// The identity ion — the logo's block cursor, splash rules, section markers,
/// and brand chrome generally. The same Ion as [`ACCENT`]: under the comet
/// kit, chrome and accent are one colour. **Never a verdict** —
/// `identity_never_carries_a_verdict` proves no outcome mapping can return
/// it. The old justification (it sat 4.0° from [`WARN`]) no longer applies at
/// 136° of separation; the rule stands because chrome and verdict are
/// different jobs. Activity (running/active) is the one status that takes the
/// identity, by kit rule.
pub const IDENTITY: Color = palette::IDENTITY;
/// The bright stop of the identity sweep (headlines, the splash rule's
/// leading edge). Kit `brand-300`.
pub const IDENTITY_BRIGHT: Color = palette::IDENTITY_BRIGHT;
/// The trailing stop of the identity sweep. Kit `brand-700` — the sweep runs
/// wider and quieter than the progress fill's ([`ACCENT_DEEP`]).
pub const IDENTITY_DEEP: Color = palette::IDENTITY_DEEP;
/// Cool-paper primary text.
pub const INK: Color = TEXT_PRIMARY;
/// Dimmed secondary text.
pub const MUTED: Color = TEXT_SECONDARY;
/// Panel border / rule.
pub const RULE: Color = HAIRLINE;

/// Background tint for the transcript entry selected with the arrow keys —
/// a barely-there cool lift so the highlight reads without shouting. A full
/// ion wash would make the selection a surface, which ion may not be; the ion
/// *pill* treatment is reserved for single-line attention (the H1 bar, an
/// active tab), where obsidian-on-ion text keeps it legible.
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
/// A subagent's identity mark — the `◆` beside a nested lane in SESSION and
/// the statline's `◆ N sub` count. Aliased to [`JADE`] (categorical): a
/// subagent is not the brand, not a status, and must never be confusable
/// with the lead's ion `✦`.
pub const SUBAGENT: Color = JADE;

// ── Runtime theme (the `/theme` switch) ──────────────────────────────────────
//
// Colours above are compile-time constants: the canonical **dark** palette
// (Ion on Obsidian), which is what every view renders and
// what ships as the default. A theme switch is applied *after* the widgets draw, as a
// per-frame value→value remap over the finished buffer ([`apply_theme`]) —
// the exact mechanism [`degrade_buffer`] already uses for colour-depth. That
// keeps ~700 `theme::TOKEN` call sites untouched: a theme is a substitution
// table, not a parameter threaded through the render tree.
//
// The one thing a value remap can't recolour is the progress bar's *gradient*,
// whose interpolated cells are never equal to a token; so its source
// ([`brand_gradient`] via [`primary_stops`]) is theme-aware directly.

/// The shipped themes. `stella-dark` (Ion on Obsidian) is the default;
/// `stella-light` (the deep ion ramp on cool paper) is its complement. The
/// names match the `/theme` argument and the `ui.theme` settings value
/// verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeName {
    /// Ion on Obsidian. Default.
    #[default]
    StellaDark,
    /// The same ion darkened onto cool paper.
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
/// `brand` (Ion) on `stella-dark`, `brand-ink-deep` → `brand-ink` on
/// `stella-light`. Feeds [`brand_gradient`] so the progress fill and wordmark
/// sweep recolour with the theme.
///
/// The bright end is [`ACCENT`] (the ion itself, not `brand-bright`), so
/// that the truecolor gradient lands on the same value a degraded terminal
/// falls back to — `crate::progress` paints a solid [`ACCENT`] when
/// [`ColorMode::is_truecolor`] is false, and a fill whose colour jumped
/// between depths would be a downgrade-path bug. (`brand-bright` belongs to
/// the identity sweep's high end, [`identity_stops`].)
pub fn primary_stops() -> [Color; 2] {
    match active_theme() {
        ThemeName::StellaDark => [palette::BRAND_DEEP, palette::BRAND],
        ThemeName::StellaLight => [palette::BRAND_INK_DEEP, palette::BRAND_INK],
    }
}

/// The active theme's identity gradient stops, deep → bright. The identity is
/// chrome: the splash rule, section markers, the wordmark's cursor block. It
/// is never a verdict and never a data mark, so nothing that resolves an
/// *outcome* may call this.
pub fn identity_stops() -> [Color; 2] {
    match active_theme() {
        ThemeName::StellaDark => [palette::IDENTITY_DEEP, palette::IDENTITY_BRIGHT],
        // One identity tone on paper: `identity-ink` (kit `brand-700`, the
        // kit's one light-ground *graphical* ion) holds the 3:1 graphical
        // floor on paper, so the light sweep is flat by design. Ion *text* on
        // paper is darker still — the flat-token remap sends it to
        // `brand-ink`.
        ThemeName::StellaLight => [palette::IDENTITY_INK, palette::IDENTITY_INK],
    }
}

/// The identity gradient sampled at `t ∈ [0, 1]` — the counterpart of
/// [`brand_gradient`] for identity chrome.
pub fn identity_gradient(t: f64) -> Color {
    gradient_at(&identity_stops(), t)
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
/// pair with [`OK`] foreground, 8.14:1 on this tint).
pub const DIFF_ADD_BG: Color = Color::Rgb(0x0F, 0x36, 0x20);
/// Subtle background tint behind removed diff lines (pair with [`BAD`],
/// 6.09:1 on this tint).
pub const DIFF_DEL_BG: Color = Color::Rgb(0x46, 0x20, 0x2A);

/// Background behind the bytes of an added line that actually changed, when a
/// `-`/`+` pair is close enough to word-diff. Two steps brighter than
/// [`DIFF_ADD_BG`]: the whole line is already "added", so this has to read as
/// a second level of emphasis *within* it rather than a different category.
pub const DIFF_ADD_BG_EMPH: Color = Color::Rgb(0x18, 0x5B, 0x37);
/// Background behind a live search match. Warm enough to find by eye against
/// an entirely cool ground, muted enough not to outshout the `✗` rail beside
/// it — a match is something you asked for, not something that went wrong.
/// [`TEXT_PRIMARY`] measures 7.87:1 on it.
pub const MATCH_BG: Color = Color::Rgb(0x5D, 0x43, 0x04);

/// The removed-line counterpart of [`DIFF_ADD_BG_EMPH`].
pub const DIFF_DEL_BG_EMPH: Color = Color::Rgb(0x75, 0x36, 0x46);

// ── Syntax highlighting (diff bodies) ───────────────────────────────────────
//
// A four-color code palette layered *under* the add/remove diff semantics:
// the `+`/`-` background always wins (add/remove is never lost — see
// `crate::diff`), while a recognized token overrides only the foreground.
// Every color is chosen to read on all three diff backdrops (add green, del
// red, and the plain panel), and every one is *categorical*: syntax is not
// brand, not status, and not activity. Keyword takes [`AMBER`] — a warm
// counterpoint inside an otherwise cool body, and 174° from the accent so
// nothing nearby can be confused for "active" — strings a soft spring green,
// numbers the [`VIOLET`] anchor, and comments dim toward the caption tier.

/// Language keyword (`fn`/`let`/`def`/`import`/`return`…).
pub const SYNTAX_KEYWORD: Color = AMBER;
/// String / char literal.
pub const SYNTAX_STRING: Color = Color::Rgb(0x8F, 0xE4, 0x92);
/// Numeric literal — violet, the counterpoint to the amber keyword stop.
pub const SYNTAX_NUMBER: Color = VIOLET;
/// Line comment (rendered dimmed + italic). The same graphite as
/// [`TEXT_TERTIARY`] — "comments dim toward the caption tier" made literal.
pub const SYNTAX_COMMENT: Color = palette::TEXT_TERTIARY;

/// Inline code spans and fenced-code plain runs (`crate::markdown`). A calm
/// sage green — quiet enough that a backticked word reads as *technical*
/// rather than as emphasis, and 60° of hue away from [`ACCENT`] at 1.42:1
/// against it, so it never reads as *active* (9.61:1 on ground). This
/// replaces the warning-orange the transcript used to paint every
/// `identifier` with: code is not a warning, and an alarm hue on every
/// backticked word was the single loudest thing on the deck. Not a brand
/// value — it is TUI-only and has no entry in `palette`.
pub const CODE: Color = Color::Rgb(0x77, 0xC5, 0x99);

// ── Brand gradient (the wordmark sweep and the progress-bar fill) ───────────
//
// Two stops, not three: the sweep runs deep → bright, left to right. An
// earlier generation ran two separate gradients — one for brand chrome and a
// second for the progress bar — which is precisely the split this palette
// collapses. Progress *is* activity, activity is the brand hue, so one
// gradient serves both. The stops track the ACTIVE theme (ion on obsidian,
// the deep ion ramp on paper) via [`primary_stops`]: the progress fill
// interpolates non-token cells the per-frame [`apply_theme`] remap can't see,
// so its source has to be theme-aware directly.
//
// The identity chrome keeps its own wider two-stop sweep ([`identity_stops`]
// / [`identity_gradient`]) — the splash rule and section markers. Same family
// (brand-700 → brand-300 around one hue), but a separate gradient: chrome
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
// approximate unpredictably (a saturated cyan comes out grey-blue) or ignore. The deck
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
/// `deck_shell::run_deck` / `fleet_dashboard::run`) and thread it through — never
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
    (VOID, 16, 0),
    (GROUND, 232, 0),
    (SURFACE, 234, 0),
    (RAISED, 235, 8),
    (HAIRLINE, 236, 8),
    (HAIRLINE_STRONG, 238, 8),
    (TEXT_PRIMARY, 255, 15),
    (TEXT_SECONDARY, 247, 7),
    (TEXT_TERTIARY, 245, 8),
    // Ion `#00D1F9` lands on cube entry 45 (0,215,255) — a two-unit miss, so
    // the terminal cube holds the brand almost exactly, as it did for the
    // bronze before it. The family stays on the *cyan* side everywhere:
    // `identity-bright` takes 81 (95,215,255) and `identity-deep` 31
    // (0,135,175), each the nearest cube entry, and each distinct from the
    // others so the identity sweep still reads as a sweep at 256 colours. At
    // 16 colours the accent family takes bright cyan (14) and the deep stops
    // plain cyan (6) — nothing else in the palette claims either, which is
    // the one thing the gold era could not have: `warning` now owns bright
    // yellow (11) outright instead of sharing a cell with chrome.
    (ACCENT, 45, 14), // also ACCENT_FILL and IDENTITY (one value, one entry)
    (IDENTITY_BRIGHT, 81, 14),
    (ACCENT_DEEP, 38, 6),
    (IDENTITY_DEEP, 31, 6),
    (SUCCESS, 42, 10),
    (WARNING, 214, 11),
    (DANGER, 204, 9),
    // Nearest cube entry to ORACLE_PRE_FLIP's `#FF3D2A` is 202 (255,95,0) —
    // two steps from DANGER's 204, so the two stay distinct at 256 colours.
    // At 16 colours there is only one red (9) and the pre-flip state shares
    // it with danger; the `red ──▸ green` wording, not the hue, carries the
    // meaning there (the same glyph-over-hue rule every status obeys).
    (ORACLE_PRE_FLIP, 202, 9),
    (VIOLET, 105, 12),
    (AMBER, 216, 3),
    (JADE, 77, 2),
    (MAGENTA, 206, 5),
    // The tool-class pair. 148 (175,215,0) and 177 (215,135,255) are the
    // nearest cube entries; at 16 colours they share green/magenta with their
    // hue neighbours, which is the ordinary collapse every categorical hue
    // takes there — the tool NAME is always beside the colour, so the class is
    // never carried by hue alone.
    (CITRON, 148, 10),
    (ORCHID, 177, 13),
    // CODE's nearest cube entry (114) is also SYNTAX_STRING's, and two syntax
    // tones collapsing onto one cell is exactly what a degraded terminal must
    // not do inside a code body, so CODE takes the next-nearest (108, a duller
    // sage) and the string tone keeps 114.
    (CODE, 108, 2),
    (DIFF_ADD_BG, 22, 2),
    (DIFF_DEL_BG, 52, 1),
    (DIFF_ADD_BG_EMPH, 28, 2),
    (DIFF_DEL_BG_EMPH, 88, 5),
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
/// See `deck_shell::run_deck` / `fleet_dashboard::run` for the call sites.
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
    // Brand → the deep ion ramp. ACCENT, ACCENT_FILL and IDENTITY are one Ion
    // value, so one entry sends every flat ion cell to `brand-ink` (kit
    // `brand-800`, 4.59:1 on paper) — kit `brand-700` is reserved for
    // *graphical* chrome via `identity_stops`, because at 3.16:1 it cannot
    // carry terminal-cell text on paper.
    (ACCENT, palette::BRAND_INK),
    (IDENTITY_BRIGHT, palette::IDENTITY_INK),
    (ACCENT_DEEP, palette::BRAND_INK_DEEP),
    // `identity-deep` already IS the kit's light-ground graphical ion; its
    // entry is the identity so the sweep's trailing stop needs no second
    // value.
    (IDENTITY_DEEP, palette::IDENTITY_INK),
    // Status → ink variants (OK/BRIGHT share the base value).
    (SUCCESS, palette::SUCCESS_INK),
    (WARNING, palette::WARNING_INK),
    (DANGER, palette::DANGER_INK),
    (ORACLE_PRE_FLIP, palette::ORACLE_RED_INK),
    // Inline code — a darker sage, 5.68:1 on paper.
    (CODE, Color::Rgb(0x00, 0x6D, 0x42)),
    // Categorical hues, darkened for AA on the cool paper (RUN/HELD/NUMBER==
    // VIOLET, KEYWORD==AMBER). Every one is the dark-side hue walked down to
    // OKLCH L=0.48, which clears 5.4:1 or better on `paper`.
    (VIOLET, Color::Rgb(0x51, 0x42, 0xC9)),
    (AMBER, Color::Rgb(0x97, 0x3F, 0x00)),
    (JADE, Color::Rgb(0x00, 0x73, 0x01)),
    (MAGENTA, Color::Rgb(0x9D, 0x13, 0x85)),
    // The tool-class pair, darkened for AA on the cool paper: citron drops to
    // an olive (5.58:1), orchid to a deep purple (6.44:1).
    (CITRON, Color::Rgb(0x4B, 0x69, 0x00)),
    (ORCHID, Color::Rgb(0x83, 0x29, 0xAC)),
    // Syntax bodies. Strings take a deep green (5.64:1 on paper).
    (SYNTAX_STRING, Color::Rgb(0x00, 0x6F, 0x1A)),
    // Diff tints → light washes, each the dark tint's own hue lifted to the
    // paper end of the ramp so add/remove keeps its meaning on both grounds.
    (DIFF_ADD_BG, Color::Rgb(0xCC, 0xF8, 0xDA)),
    (DIFF_DEL_BG, Color::Rgb(0xFF, 0xE3, 0xE8)),
    (DIFF_ADD_BG_EMPH, Color::Rgb(0xA3, 0xED, 0xBC)),
    (DIFF_DEL_BG_EMPH, Color::Rgb(0xFF, 0xC8, 0xD3)),
    (MATCH_BG, Color::Rgb(0xFF, 0xE4, 0xB4)),
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

/// The border of a panel that *contains* something a reader works in — the
/// transcript, PLAN, PROOF, the deck's section boxes.
///
/// [`HAIRLINE_STRONG`], not [`RULE`]. The plain hairline is 1.26:1 and is
/// documented as decoration that "may never be the only thing conveying
/// structure" — which is exactly the job a panel border does have. At 1.57:1
/// the seam is still quiet enough to recede and finally strong enough to
/// enclose. Popovers and cards that already sit on a lifted ground keep
/// [`rule`].
pub fn panel_rule() -> Style {
    Style::default().fg(HAIRLINE_STRONG)
}

/// The words set into a panel's border.
///
/// A ratatui `Block` title with no style of its own inherits the border's, so
/// every panel label was being drawn in the seam colour — legible only to
/// someone who already knew what it said. A title is content: it takes the
/// caption tier and its own weight.
pub fn panel_title() -> Style {
    Style::default()
        .fg(TEXT_SECONDARY)
        .add_modifier(Modifier::BOLD)
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

/// The statline stage dot's color, by pipeline stage — the planning stages
/// read as process (periwinkle), execution as live work (the accent, the one
/// status that takes the identity), the verification stages as the jade
/// categorical
/// (checking is neither activity nor a verdict), and the wind-down stages
/// dim. `Complete` is the sole outcome here and takes success — paired with
/// the stage *word* beside the dot, so hue never carries the meaning alone.
pub fn stage_color(stage: stella_protocol::StageKind) -> Color {
    use stella_protocol::StageKind as S;
    match stage {
        S::Triage | S::ContextRecall | S::Research | S::Plan | S::ScopeReview | S::Witness => RUN,
        S::Execute => ACCENT,
        S::Verify | S::Verdict => JADE,
        // The wind-down stages *write*: reflection mines lessons into
        // `.stella/`, context-write upserts facts. They wore the neutral tier
        // and were the only stages a reader could not see at all; the
        // magenta is the hue the transcript paints a mutating tool call, so "this
        // phase changed something durable" is one colour wherever it appears.
        S::Reflect | S::ContextWrite => MAGENTA,
        S::Complete => OK,
    }
}

/// The stage colour a **transcript section rule** renders in — the same phase
/// families [`stage_color`] uses, with one deliberate divergence.
///
/// `Execute` is the accent on the statline, because the statline's dot is
/// *current state* and ion means active — the one status the kit gives it. A
/// transcript rule is the opposite: it is history, written once and scrolled
/// past, and by the time it is read the stage is over. The accent on a
/// settled thing is the reservation's exact failure mode, so the execute rule
/// takes the citron instead: the brightest categorical hue there is
/// (12.50:1), 112° clear of the accent, and — like the stage it marks — the
/// one that says the workspace changed here.
pub fn stage_rule_color(stage: stella_protocol::StageKind) -> Color {
    match stage {
        stella_protocol::StageKind::Execute => CITRON,
        other => stage_color(other),
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
/// function/method periwinkle, struct/enum/trait green, file/module magenta —
/// three distinct categorical hues, none of them the reserved brand accent
/// (the magenta replaces an amber that became indistinguishable from the gold
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
/// Five distinct categorical hues — periwinkle, magenta, green, jade,
/// secondary — none of them the reserved brand hue (an agent is not "the
/// brand"; the amber slot the magenta replaces was 1.12:1 against the gold
/// accent, and a chip that can be mistaken for "running" is worse than no
/// chip) and none of them danger (which reads as failure elsewhere, so it
/// never brands a healthy agent).
const AGENT_PALETTE: [Color; 5] = [HELD, MAGENTA, OK, TEXT_SECONDARY, JADE];

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
/// meaning: `RUN` (periwinkle) for process/action events (stage, tool, vcs),
/// `MAGENTA`/`JADE` for produced artifacts (file, media) — categorical, so
/// an artifact never reads as "running"; the file chip's magenta sits 1.17:1
/// from [`BAD`], which is tolerable exactly because a chip is always a
/// coloured *word* and an error is always glyph-paired — a dim neutral for
/// quiet memory/context events, and the shared `OK`/`WARN`/`BAD` semantics
/// for verdicts, spend, and errors. Memory drops to `TEXT_TERTIARY` rather
/// than reuse the periwinkle — the process group already owns that anchor.
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
        TraceKind::Media => JADE,
        TraceKind::Vcs => RUN,
        TraceKind::Error => BAD,
        TraceKind::Complete => OK,
        TraceKind::Other => MUTED,
    }
}

#[cfg(test)]
mod tests;
