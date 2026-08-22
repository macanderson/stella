//! The one place the deck's look is defined — colors, semantic styles, and
//! glyphs. Every view pulls from here so the deck reads as one system in both
//! the stella brand palette and its status semantics. No view hard-codes a
//! color; that is what keeps a 12-panel TUI feeling designed rather than
//! assembled.

use ratatui::style::{Color, Modifier, Style};

use crate::deck::TraceKind;
use crate::envelope::AgentStatus;
use crate::palette;

// ── stella palette — "Gold on a cool near-black" ────────────────────────────
//
// One colour, owned. The ground is a four-step neutral ramp from `#0A0A0C`,
// every step of it blue-above-red so the screen never reads warm on a cheap
// panel; Gold is the signal — reserved for brand, the prompt, active/running,
// selection and focus, and for nothing else. Gold is the signal, never the
// surface: if everything is gold, nothing is, so a general-purpose highlight
// is exactly what this hue must not become. The one sanctioned gold *fill* is
// a pill that owns attention (the H1 title bar, a selected tab), and a gold
// fill always carries GROUND-dark text — white on this gold is 1.35:1.
// (Gold is the *default* accent; `stella-light` swaps it for the deep gold
// ramp on cool paper — see [`apply_theme`].)
//
// The gold is **two values and no more**: [`ACCENT`] for every resting mark,
// [`ACCENT_LIVE`] for the small things that are moving right now — the
// running spinner, the progress fill's leading edge. There is no third gold,
// which is why the deep stops the previous palette carried (`ACCENT_DEEP`,
// `GOLD_DEEP`) are gone rather than re-pointed: a gold nobody can name the
// job of is how an owned colour becomes a family.
//
// **Gold never carries a verdict.** [`WARN`] sits 39.1° away in OKLCH hue —
// far enough to tell apart — and the rule holds regardless, because a reader
// must never be asked to tell an outcome from chrome by hue alone: status is
// always glyph-paired, and activity (running) is the only status that takes
// the accent. Enforced by `gold_never_carries_a_verdict`.
//
// The corollary the transcript actually depends on: **prose is neutral.** The
// accent buys attention, so it may only be spent where attention is owed — on
// the deck that means the tool being called, the active tab, and the progress
// fill. Everything else is [`INK`] or [`MUTED`], and a row that wants to be
// scannable does it with a glyph and a column, not with a hue.
//
// Status always pairs colour with a glyph (see [`status_glyph`]) so hue never
// carries meaning alone — active ▶ vs done ✓ — which is also what keeps the
// deck readable under `NO_COLOR` and for red/green colour blindness.
//
// Every value below comes from [`crate::palette`], which carries the measured
// contrast and hue figures for each. This module is the *semantic* layer over
// it: call sites reference roles (accent, ink, rule, ok) rather than hues, so
// a recolour is a palette edit rather than a hunt through the crate. Add a
// colour here only as a role; add a *value* in `palette`.
//
// Every token is 24-bit; [`degrade_buffer`] narrows it to 256- or 16-color, or
// strips it for `NO_COLOR`, once per frame for terminals that can't render
// truecolor. A theme switch is a second per-frame pass ([`apply_theme`]).

// Grounds (dark → light lift) — the four specified blacks, plus the two
// derived steps at either end (see `palette`'s ground section).
/// Deepest ground — full-bleed backdrops and the splash.
pub const VOID: Color = palette::VOID;
/// App background — the canvas. Applied as a real frame fill by
/// `render_deck`, not just assumed from the terminal.
pub const GROUND: Color = palette::GROUND;
/// Card / panel surface, and the ground a code block sits on.
pub const SURFACE: Color = palette::SURFACE;
/// Raised panel — the highlight-row value, one step above surface.
pub const RAISED: Color = palette::RAISED;
/// Hairline border / rule — the border value. Deliberately below 3.0
/// contrast (1.31:1): it may never be the only thing conveying structure.
pub const HAIRLINE: Color = palette::HAIRLINE;
/// The louder seam, for a boundary that must actually read (1.63:1). Still
/// decoration — a stronger rule, not a substitute for a glyph or a gap.
pub const HAIRLINE_STRONG: Color = palette::HAIRLINE_STRONG;

// Text tiers (primary → dim) — four cool neutrals on the same hue ramp as
// the grounds, so nothing on the dark side reads warm.
/// Primary text.
pub const TEXT_PRIMARY: Color = palette::TEXT_PRIMARY;
/// Secondary text, and the tone context events take. The safe small-text tone
/// on every dark ground (8.58:1).
pub const TEXT_SECONDARY: Color = palette::TEXT_SECONDARY;
/// Tertiary text (labels, captions). 4.47:1 on [`GROUND`] — just under the
/// AA body floor, so this is the caption/UI tier and anything a reader must
/// read at 13px takes [`TEXT_SECONDARY`].
pub const TEXT_TERTIARY: Color = palette::TEXT_TERTIARY;
/// The dim tier — 2.30:1, below every text floor. **Chrome only, never
/// words**: the unfilled progress groove. It has its own value now rather
/// than aliasing [`TEXT_TERTIARY`], which is why the three progress call
/// sites that painted *words* with it moved up a tier.
pub const TEXT_DIM: Color = palette::TEXT_DIM;

// Semantic (base + bright). The palette carries one value per status; the
// `_BRIGHT` names remain as roles for call sites that mean "the text tone".
/// Success (base).
pub const SUCCESS: Color = palette::SUCCESS;
/// Success (bright — text / completed fills).
pub const SUCCESS_BRIGHT: Color = palette::SUCCESS;
/// Warning (base). Derived to sit 39.1° from the gold accent and 38.9° from
/// [`DANGER`] — the point that maximises the smaller of the two gaps — so a
/// warning is tellable from the mark *and* from a failure by hue. The glyph
/// stays the status carrier regardless.
pub const WARNING: Color = palette::WARNING;
/// Warning (bright — text).
pub const WARNING_BRIGHT: Color = palette::WARNING;
/// Danger (base).
pub const DANGER: Color = palette::DANGER;
/// Danger (bright — legible removed-line / error text on the dark backdrop).
pub const DANGER_BRIGHT: Color = palette::DANGER;

// `ORACLE_PRE_FLIP` deliberately does not exist. #3890 retired it along with
// its `palette::ORACLE_RED`/`ORACLE_RED_INK` values, and
// `doc:verification-surface` § "Decision 3" is the standing
// argument: the token's only consumer was the witness panel removed in #3791,
// the brand kit at `docs/brand/` never carried an oracle token, and the
// contrast-table case for keeping it was circular. A pre-flip red returns when
// a panel paints one — a token earns its place by being painted.

// ── Categorical hues (deliberately NOT brand) ───────────────────────────────
//
// A few surfaces need more mutually-distinguishable colours than a one-hue
// palette provides: graph node kinds, tool classes, and one colour per
// concurrent agent. Making those the brand hue would violate the reservation
// above, so they are a categorical set. Every one clears **30° of OKLCH hue
// from [`ACCENT`]** — the floor for two hues to be told apart in a single
// terminal cell — and AA body on [`GROUND`]. They carry no brand meaning and
// must never be used for brand, status, or "active".
//
// The amber mark that used to lead this set is **gone**, not stood down: at
// OKLCH hue 85.2 it measured 5.5° from this gold, the same colour at a
// glance. Its one surviving job — the syntax keyword tone inside code
// bodies — went to [`SYNTAX_KEYWORD`], which the palette puts on the bright
// neutral anyway.

/// Violet — process/structural events (links, diff hunk headers, graph
/// relations, trace stages, the user's own prompt). Categorical, not the brand
/// accent. `data-1`, 5.32:1 on ground, 158.2° from gold.
pub const VIOLET: Color = palette::DATA_1;
/// Teal — media traces and one slot of the per-agent palette. `data-3`,
/// 10.60:1 on ground, 95.9° from gold. Deep blue-green, emphatically not the
/// retired ice blue — it replaced a glacier blue (`AGENT_ICE`) that drifted
/// confusable with an older accent.
pub const TEAL: Color = palette::DATA_3;
/// Warm rose — file artifacts (trace chips, graph file/module nodes) and the
/// fourth chart series. `data-2`, 5.11:1 on ground, 95.2° from gold. 1.14:1
/// against [`DANGER`]'s red-pink, so it never carries an error meaning and
/// never appears without a neutral label or glyph beside it.
pub const MAGENTA: Color = palette::DATA_2;
/// Citron — the repository/VCS tool class in the transcript. `data-4`,
/// 11.10:1 on ground, and 35.4° from gold: the tightest clearance in the set,
/// and it clears.
pub const CITRON: Color = palette::DATA_4;
/// Orchid — the delegation/orchestration tool class in the transcript.
/// `data-5`, 6.82:1 on ground, 130.0° from gold and 28.2° from the violet.
pub const ORCHID: Color = palette::DATA_5;

// ── Role aliases (what the rest of the crate references) ─────────────────────
// Role names remap onto the palette so call sites read as intent (accent,
// ink, rule) rather than as a hue that a future recolor would falsify.

/// stella's brand accent — Gold `#EFC53F` (11.99:1 on [`GROUND`]). Brand,
/// active/running, focus, selection, and progress only. In the transcript,
/// the tool name and nothing else. The active theme's actual hue is applied
/// per-frame by [`apply_theme`]; this is the canonical dark value every call
/// site renders.
///
/// The same value clears AA on a glyph, a one-cell rule, and a fill on every
/// dark ground. When it is a *fill*, the text on it must be [`GROUND`]-dark —
/// ink on gold is 11.99:1 where white on gold is 1.35:1.
pub const ACCENT: Color = palette::BRAND;
/// The brand hue for *fills* — a pill, a bar body, a selected-tab wash. The
/// same gold: one owned colour means the stroke and the fill agree, and the
/// fill's legibility comes from pairing it with ink text, never from a second
/// hue.
pub const ACCENT_FILL: Color = palette::BRAND;
/// The **live** accent — the one lifted gold, reserved for small things that
/// are moving: the running spinner, the leading edge of the progress fill.
/// 3.5° from [`ACCENT`] in hue, so it reads as the same gold lit up rather
/// than as a second colour. A resting mark never takes it.
pub const ACCENT_LIVE: Color = palette::BRAND_LIVE;

/// The identity gold — the logo's block cursor, splash rules, section
/// markers, and brand chrome generally. The same value as [`ACCENT`]: chrome
/// and accent are one colour. **Never a verdict** —
/// `gold_never_carries_a_verdict` proves no outcome mapping can return it;
/// activity (running/active) is the one status that takes gold.
pub const GOLD: Color = palette::GOLD;
/// The live stop of the identity sweep — the same value as [`ACCENT_LIVE`],
/// under the same reservation: small, and moving.
pub const GOLD_LIVE: Color = palette::GOLD_LIVE;
/// Primary text.
pub const INK: Color = TEXT_PRIMARY;
/// Dimmed secondary text.
pub const MUTED: Color = TEXT_SECONDARY;
/// Panel border / rule.
pub const RULE: Color = HAIRLINE;

/// Background tint for the transcript entry selected with the arrow keys —
/// the highlight-row value, a barely-there lift that reads without shouting.
/// A full gold wash would make the selection a surface, which gold may not
/// be; the gold *pill* treatment is reserved for single-line attention (the
/// H1 bar, an active tab), where ink-on-gold text keeps it legible.
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
/// the statline's `◆ N sub` count. Aliased to [`TEAL`] (categorical): a
/// subagent is not the brand, not a status, and must never be confusable
/// with the lead's gold `✦`.
pub const SUBAGENT: Color = TEAL;

// ── Runtime theme (the `/theme` switch) ──────────────────────────────────────
//
// Colours above are compile-time constants: the canonical **dark** palette
// (gold on the cool near-black ramp), which is what every view renders and
// what ships as the default. A theme switch is applied *after* the widgets draw, as a
// per-frame value→value remap over the finished buffer ([`apply_theme`]) —
// the exact mechanism [`degrade_buffer`] already uses for colour-depth. That
// keeps ~700 `theme::TOKEN` call sites untouched: a theme is a substitution
// table, not a parameter threaded through the render tree.
//
// The one thing a value remap can't recolour is the progress bar's *gradient*,
// whose interpolated cells are never equal to a token; so its source
// ([`brand_gradient`] via [`primary_stops`]) is theme-aware directly.

/// The shipped themes. `stella-dark` (gold on the cool near-black ramp) is
/// the default; `stella-light` (the deep gold ramp on cool paper) is its
/// complement. The names match the `/theme` argument and the `ui.theme`
/// settings value verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeName {
    /// Gold on the cool near-black ramp. Default.
    #[default]
    StellaDark,
    /// The same gold darkened onto cool paper.
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

/// The active theme's brand gradient stops, resting → live: `brand` →
/// `brand-live` on `stella-dark`, `brand-ink-deep` → `brand-ink` on
/// `stella-light`. Feeds [`brand_gradient`] so the progress fill and wordmark
/// sweep recolour with the theme.
///
/// The palette allows exactly two golds, so the sweep IS those two: the fill
/// rests on [`ACCENT`] and lights up to [`ACCENT_LIVE`] at its head, which is
/// precisely the job the live stop is reserved for. This also removes the
/// downgrade-path hazard the previous three-gold ramp had to reason around —
/// `crate::progress` paints a solid [`ACCENT`] when [`ColorMode::is_truecolor`]
/// is false, and that is now the gradient's own resting stop rather than a
/// third value.
///
/// There is deliberately no second, wider "identity" sweep any more. It
/// existed to let chrome run quieter than the progress fill across four gold
/// stops; with two, a second gradient over the same pair would be a
/// distinction that renders identically — and `gold_stops`/`gold_gradient`,
/// which had no caller outside this module, are gone with it.
pub fn primary_stops() -> [Color; 2] {
    stops_for(active_theme())
}

/// The pure half of [`primary_stops`]: which pair a *named* theme sweeps
/// between, reading none of the process-global active theme. Split out so the
/// table can be asserted for every theme at once without a test flipping
/// global state the rest of the binary shares.
fn stops_for(theme: ThemeName) -> [Color; 2] {
    match theme {
        ThemeName::StellaDark => [palette::BRAND, palette::BRAND_LIVE],
        ThemeName::StellaLight => [palette::BRAND_INK_DEEP, palette::BRAND_INK],
    }
}

// ── Diff panel ──────────────────────────────────────────────────────────────

/// Subtle background tint behind added diff lines (the GitHub-PR reading —
/// pair with [`OK`] foreground). [`SUCCESS`] mixed 16% over [`GROUND`], so
/// the wash is the semantic hue diluted rather than an unrelated green:
/// 1.31:1 on ground, and [`INK`] on it is 12.40:1.
pub const DIFF_ADD_BG: Color = Color::Rgb(0x1B, 0x29, 0x21);
/// Subtle background tint behind removed diff lines (pair with [`BAD`]) —
/// [`DANGER`] at the same 16%. [`INK`] on it is 13.59:1.
pub const DIFF_DEL_BG: Color = Color::Rgb(0x2C, 0x19, 0x1E);

/// Background behind the bytes of an added line that actually changed, when a
/// `-`/`+` pair is close enough to word-diff. Two steps brighter than
/// [`DIFF_ADD_BG`]: the whole line is already "added", so this has to read as
/// a second level of emphasis *within* it rather than a different category.
pub const DIFF_ADD_BG_EMPH: Color = Color::Rgb(0x2E, 0x4B, 0x39);
/// Background behind a live search match. Warm enough to find by eye while
/// scrolling, muted enough not to outshout the `✗` rail beside it — a match is
/// something you asked for, not something that went wrong.
pub const MATCH_BG: Color = Color::Rgb(0x46, 0x3B, 0x19);

/// The removed-line counterpart of [`DIFF_ADD_BG_EMPH`].
pub const DIFF_DEL_BG_EMPH: Color = Color::Rgb(0x53, 0x2A, 0x31);

// ── Syntax highlighting (diff bodies) ───────────────────────────────────────
//
// A code palette layered *under* the add/remove diff semantics: the `+`/`-`
// background always wins (add/remove is never lost — see `crate::diff`),
// while a recognized token overrides only the foreground. Every colour reads
// on all three diff backdrops (add green, del red, and the plain panel), and
// none of them is brand, status, or activity.
//
// The keyword tone is the palette's own instruction for a code body: token
// classes ride the **bright neutral**, not a hue. That replaces an amber that
// measured 5.5° from this gold — a keyword the reader could mistake for the
// running mark, in the one place gold chrome never reaches to disambiguate
// it. Literals keep a hue so a value still stands out from the shape of the
// code around it, and both are pulled into the scheme's own register.

/// Language keyword (`fn`/`let`/`def`/`import`/`return`…), and in JSON an
/// object key — the palette's "syntax types" slot. [`palette::TEXT_EMPHASIS`],
/// 11.04:1 on ground.
pub const SYNTAX_KEYWORD: Color = palette::TEXT_EMPHASIS;
/// String / char literal — a soft green at the scheme's own chroma (0.115,
/// against the retired tone's 0.164), 11.78:1 on ground. 8.4° from
/// [`SUCCESS`] in hue, which is deliberate rather than sloppy: inside a diff
/// body the *background* already says added-or-removed, so a literal that
/// sits near the success hue cannot be read as a verdict.
pub const SYNTAX_STRING: Color = Color::Rgb(0x93, 0xD8, 0x96);
/// Numeric literal — violet, the categorical counterpoint to the neutral
/// keyword stop.
pub const SYNTAX_NUMBER: Color = VIOLET;
/// Line comment (rendered dimmed + italic) — the caption tier, which is
/// "comments dim toward the caption tier" made literal.
pub const SYNTAX_COMMENT: Color = palette::TEXT_TERTIARY;

/// Inline code spans and fenced-code plain runs (`crate::markdown`). A calm
/// sage green — quiet enough that a backticked word reads as *technical*
/// rather than as emphasis, and 67.2° of hue away from [`ACCENT`] so it never
/// reads as *active* (8.99:1 on ground). This replaces the warning-orange the
/// transcript used to paint every `identifier` with: code is not a warning,
/// and an alarm hue on every backticked word was the single loudest thing on
/// the deck. Not a palette value — it is TUI-only.
pub const CODE: Color = Color::Rgb(0x6F, 0xBF, 0x92);

// ── Brand gradient (the wordmark sweep and the progress-bar fill) ───────────
//
// Two stops, and there are only two golds to make them from: the sweep runs
// resting → live, left to right. An earlier generation ran two separate
// gradients — one for brand chrome and a second for the progress bar — which
// is precisely the split this palette collapses. Progress *is* activity,
// activity is the brand hue, so one gradient serves both. The stops track the
// ACTIVE theme via [`primary_stops`]: the progress fill interpolates non-token
// cells the per-frame [`apply_theme`] remap can't see, so its source has to be
// theme-aware directly.

/// The brand gradient's stops for the *default* (dark) theme, resting → live.
/// [`primary_stops`] returns these for `stella-dark` and the paper pair for
/// `stella-light`; prefer that accessor. The determinate progress fill
/// interpolates across the active stops per cell (truecolor only; lesser
/// terminals collapse to a solid [`ACCENT`] fill).
pub const BRAND_STOPS: [Color; 2] = [ACCENT, ACCENT_LIVE];

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
    (VOID, 232, 0),
    (GROUND, 232, 0),
    (SURFACE, 233, 0),
    (RAISED, 234, 8),
    (HAIRLINE, 235, 8),
    (HAIRLINE_STRONG, 237, 8),
    (TEXT_PRIMARY, 255, 15),
    // The bright neutral shares 15 with primary at 16 colours — the two are
    // one value step apart and there is no third white to give it. It keeps
    // its own 256 entry, which is where the code body actually reads.
    (palette::TEXT_EMPHASIS, 251, 15),
    (TEXT_SECONDARY, 145, 7),
    (TEXT_TERTIARY, 243, 8),
    (TEXT_DIM, 239, 8),
    // The two golds. `#EFC53F` sits on cube entry 221 (255,215,95) almost
    // exactly; the live stop takes 222 (255,215,135) rather than sharing it,
    // because the whole point of that value is being visibly lit. At 16
    // colours both collapse to bright yellow (11): there is no second yellow
    // to lift to, so the live head is a truecolor/256 affordance and the
    // degraded terminal simply sees a solid gold — which `crate::progress`
    // already paints deliberately when truecolor is absent. The family stays
    // on the *yellow* side: the nearest entry to the live stop on the orange
    // side is 215, and orange is the one thing this identity may not render.
    (ACCENT, 221, 11),      // also ACCENT_FILL and GOLD (one value, one entry)
    (ACCENT_LIVE, 222, 11), // also GOLD_LIVE
    (SUCCESS, 114, 10),
    (WARNING, 173, 3),
    // Nearest for danger is 168, which the rose data mark takes below; 204
    // (255,95,135) is a hair further and keeps the two apart, and a red-pink
    // is what an error must still read as.
    (DANGER, 204, 9),
    (VIOLET, 98, 13),
    (TEAL, 44, 6),
    (MAGENTA, 168, 5),
    // The tool-class pair. 149 (175,215,95) and 170 (215,95,215) are the
    // nearest cube entries; at 16 colours they share green/magenta with their
    // hue neighbours, which is the ordinary collapse every categorical hue
    // takes there — the tool NAME is always beside the colour, so the class is
    // never carried by hue alone.
    (CITRON, 149, 10),
    (ORCHID, 170, 13),
    (CODE, 72, 2),
    // The five washes keep green/red/yellow cube entries rather than their
    // nearest neighbours, which are all greys: a wash that no longer reads as
    // added/removed/found has lost the only thing it was doing.
    (DIFF_ADD_BG, 22, 2),
    (DIFF_DEL_BG, 52, 1),
    (DIFF_ADD_BG_EMPH, 28, 2),
    (DIFF_DEL_BG_EMPH, 88, 1),
    (MATCH_BG, 58, 3),
    (SYNTAX_STRING, 157, 10),
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
// the identity. `stella-light` maps every dark token to its cool-paper
// counterpart,
// keyed by value exactly like [`FALLBACKS`]. Role aliases share a value with a
// palette token (INK==TEXT_PRIMARY, OK==SUCCESS, SELECT_BG==RAISED, …), so one
// entry per distinct value covers every alias — the same property that makes
// [`FALLBACKS`] one-entry-per-value. The categorical/syntax/diff targets are
// TUI-only (not brand tokens), so they live here inline rather than in
// `palette`; the brand, ground, text and status targets come from `palette`.

/// `stella-light`: every canonical (dark) token value → its paper counterpart.
const LIGHT_REMAP: &[(Color, Color)] = &[
    // Grounds → paper. The paper ramp is cooled to the same 285-286° neutral
    // hue the dark ramp holds, so switching theme changes the lightness and
    // not the temperature.
    (VOID, palette::PAPER),
    (GROUND, palette::PAPER),
    (SURFACE, palette::SNOW),
    (RAISED, palette::PAPER_RAISED),            // also SELECT_BG
    (HAIRLINE, palette::PAPER_HAIRLINE),        // also RULE
    (HAIRLINE_STRONG, palette::PAPER_HAIRLINE), // one paper seam serves both
    // Text → ink (INK==TEXT_PRIMARY, MUTED==TEXT_SECONDARY; SYNTAX_COMMENT
    // rides the tertiary entry, SYNTAX_KEYWORD the emphasis one).
    (TEXT_PRIMARY, palette::INK),
    (palette::TEXT_EMPHASIS, palette::INK_EMPHASIS),
    (TEXT_SECONDARY, palette::MUTED),
    (TEXT_TERTIARY, palette::INK_DIM),
    // The dim tier is chrome, and on paper the groove has to darken rather
    // than lighten to stay visible; the tertiary ink is the quietest tone
    // that still reads as texture on `snow`.
    (TEXT_DIM, palette::INK_DIM),
    // Brand → the deep gold ramp. ACCENT, ACCENT_FILL and GOLD are one value,
    // so one entry sends every flat gold cell to `brand-ink` (6.02:1 on
    // paper) — `gold-ink` is reserved for *graphical* chrome, because at
    // 3.37:1 it cannot carry terminal-cell text on paper.
    (ACCENT, palette::BRAND_INK),
    (ACCENT_LIVE, palette::GOLD_INK),
    // Status → ink variants (OK/BRIGHT share the base value).
    (SUCCESS, palette::SUCCESS_INK),
    (WARNING, palette::WARNING_INK),
    (DANGER, palette::DANGER_INK),
    // Inline code — a darker sage, 5.26:1 on paper.
    (CODE, Color::Rgb(0x2A, 0x71, 0x50)),
    // Categorical hues, darkened for AA on the paper ground (RUN/HELD/NUMBER
    // == VIOLET). Teal drops to teal-700: teal-600 measured 3.35:1, under the
    // 4.5:1 text floor.
    (VIOLET, Color::Rgb(0x6D, 0x28, 0xD9)),
    (TEAL, Color::Rgb(0x0F, 0x76, 0x6E)),
    (MAGENTA, Color::Rgb(0xA6, 0x18, 0x5C)),
    // The tool-class pair, darkened for AA on the paper: citron drops to an
    // olive, orchid to a deep purple.
    (CITRON, Color::Rgb(0x4D, 0x6B, 0x12)),
    (ORCHID, Color::Rgb(0x8B, 0x2B, 0xA8)),
    // Syntax bodies. Strings take green-800 (green-700 sat at 4.49:1).
    (SYNTAX_STRING, Color::Rgb(0x16, 0x65, 0x34)),
    // Diff tints → light washes, mixed from the same paper ground and the
    // same status inks the dark washes are mixed from.
    (DIFF_ADD_BG, Color::Rgb(0xD4, 0xE2, 0xDD)),
    (DIFF_DEL_BG, Color::Rgb(0xE8, 0xD9, 0xDE)),
    (DIFF_ADD_BG_EMPH, Color::Rgb(0xB0, 0xCD, 0xBF)),
    (DIFF_DEL_BG_EMPH, Color::Rgb(0xDA, 0xB9, 0xC2)),
    (MATCH_BG, Color::Rgb(0xD7, 0xD2, 0xC0)),
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
/// transcript, PLAN, the deck's section boxes.
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
/// read as process (violet), execution as live work (the accent, the one
/// status that takes gold), the verification stages as the teal categorical
/// (checking is neither activity nor a verdict), and the wind-down stages
/// dim. `Complete` is the sole outcome here and takes success — paired with
/// the stage *word* beside the dot, so hue never carries the meaning alone.
///
/// A **contributed** stage — one a plugin named, which this host has no phase
/// family for — takes [`contributed_stage_color`] instead. See there for why it
/// is a hash rather than a guess.
pub fn stage_color(stage: &stella_protocol::StageName) -> Color {
    use stella_protocol::StageKind as S;
    let Some(kind) = stage.kind() else {
        return contributed_stage_color(stage.as_str());
    };
    match kind {
        S::Triage | S::ContextRecall | S::Research | S::Plan | S::ScopeReview | S::Witness => RUN,
        S::Execute => ACCENT,
        S::Verify | S::Verdict => TEAL,
        // The wind-down stages *write*: reflection mines lessons into
        // `.stella/`, context-write upserts facts. They wore the neutral tier
        // and were the only stages a reader could not see at all; the rose is
        // the same hue the transcript paints a mutating tool call, so "this
        // phase changed something durable" is one colour wherever it appears.
        S::Reflect | S::ContextWrite => MAGENTA,
        S::Complete => OK,
    }
}

/// The categorical hues a contributed stage is hashed into.
///
/// Excludes, deliberately: [`ACCENT`] (brand, and the one hue that means
/// "active" — a plugin's stage is not the brand and is not a status), and
/// [`OK`], [`WARN`] and [`BAD`] (a contributed stage would read as a verdict
/// it never reported). What remains is the whole categorical set — five hues
/// that all clear AA on [`GROUND`] and 30° of hue from the accent, and none
/// of which can be mistaken for an outcome.
const CONTRIBUTED_STAGE_PALETTE: [Color; 5] = [VIOLET, TEAL, MAGENTA, CITRON, ORCHID];

/// A deterministic colour for a stage this host did not define.
///
/// **The point is stability and distinctness, not per-colour meaning** — the
/// same contract [`agent_color`] states, reached for the same reason. The deck
/// cannot know whether a plugin's `triage-lite` is a planning stage or a
/// verification one, so painting it violet because the name resembles `triage`
/// would be a claim about the turn that nothing established. Hashing says only
/// what is true: these are different stages, and each is always the same
/// colour wherever it appears.
///
/// A contributed stage can therefore collide with a host stage's hue. That is
/// acceptable for the reason [`stage_color`]'s own doc gives — the stage *word*
/// renders beside the dot, so hue never carries the meaning alone.
pub fn contributed_stage_color(name: &str) -> Color {
    CONTRIBUTED_STAGE_PALETTE[(fnv1a(name) as usize) % CONTRIBUTED_STAGE_PALETTE.len()]
}

/// The stage colour a **transcript section rule** renders in — the same phase
/// families [`stage_color`] uses, with one deliberate divergence.
///
/// `Execute` is the accent on the statline, because the statline's dot is
/// *current state* and gold means active — the one status the kit gives it. A
/// transcript rule is the opposite: it is history, written once and scrolled
/// past, and by the time it is read the stage is over. Gold on a settled thing
/// is the reservation's exact failure mode, so the execute rule takes the
/// citron instead: the brightest categorical hue there is (11.08:1), 39° clear
/// of gold, and — like the stage it marks — the one that says the workspace
/// changed here.
///
/// A contributed stage has no such divergence to make: it never takes the
/// accent in the first place (see [`contributed_stage_color`]), so there is
/// no gold to move off a settled thing, and it reads the same in both places.
pub fn stage_rule_color(stage: &stella_protocol::StageName) -> Color {
    match stage.kind() {
        Some(stella_protocol::StageKind::Execute) => CITRON,
        _ => stage_color(stage),
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
/// none of them the reserved brand hue (an agent is not "the brand", and a
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
/// a coloured *word* and an error is always glyph-paired — a neutral for
/// quiet memory/context events, and the shared `OK`/`WARN`/`BAD` semantics
/// for verdicts, spend, and errors. Context takes `TEXT_SECONDARY`, which is
/// the palette's own instruction for a context event; reasoning drops to
/// `TEXT_TERTIARY` beneath it, so the two quiet kinds stay one value step
/// apart instead of collapsing onto the same neutral, and neither reuses
/// violet — the process group already owns that anchor.
pub fn trace_kind_color(kind: TraceKind) -> Color {
    match kind {
        TraceKind::Stage => RUN,
        TraceKind::Text => INK,
        TraceKind::Reasoning => TEXT_TERTIARY,
        TraceKind::Tool => RUN,
        TraceKind::File => MAGENTA,
        TraceKind::Budget => WARN,
        TraceKind::Context => TEXT_SECONDARY,
        TraceKind::Verdict => OK,
        TraceKind::Media => TEAL,
        TraceKind::Vcs => RUN,
        TraceKind::Error => BAD,
        TraceKind::Complete => OK,
        TraceKind::Other => MUTED,
    }
}

#[cfg(test)]
mod tests;
