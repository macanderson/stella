//! The palette — SPEC 3.1, one constant per token, and the [`ALL`] table that
//! makes the set walkable.
//!
//! Every value is [`Color::Rgb`]; there are no indexed or named colours here,
//! because the clamp in [`crate::clamp`] can only speak about channels. The
//! table pairs each token with its [`Role`], and the role is what decides
//! which clamp the token is held to — so adding a token means declaring what
//! kind of colour it is, and the test walks the declaration rather than a
//! hand-maintained second list. That is the whole anti-drift mechanism: a new
//! warm hex cannot enter the palette without picking a role that rejects it.

use ratatui::style::Color;

// ── Grounds ─────────────────────────────────────────────────────────────────

/// Canvas. The deck's frame fill, painted rather than inherited from the
/// terminal, so every contrast figure below is measured against a known
/// ground.
pub const BG: Color = Color::Rgb(0x0A, 0x0A, 0x0C);

/// Code blocks, panels, tables — one step above the canvas.
pub const PANEL: Color = Color::Rgb(0x0F, 0x0F, 0x12);

/// Selected and highlighted rows.
pub const HL: Color = Color::Rgb(0x17, 0x17, 0x1B);

/// Panel borders and dividers, and the unfilled track of every meter
/// (SPEC 5: meters render gold fill on `border` gray).
pub const BORDER: Color = Color::Rgb(0x26, 0x26, 0x2C);

/// Turn boundary rules — the transcript's one structural line (SPEC 6.1),
/// one step louder than [`BORDER`] because a turn boundary is the rhythm of
/// the whole surface.
pub const RULE: Color = Color::Rgb(0x2C, 0x2C, 0x33);

// ── The two metals ──────────────────────────────────────────────────────────
//
// Gold means stella acting on the world; silver means the world coming in
// (SPEC 2). There is no third metal, and neither one ever carries a verdict —
// pass and fail are [`GREEN`] and [`RED`], and both of those are rationed.

/// stella acting: edit, write, gate, brand, money, active tab. The resting
/// gold, and the one held to the full clamp
/// ([`crate::clamp::is_resting_gold`]).
pub const GOLD: Color = Color::Rgb(0xEF, 0xC5, 0x3F);

/// Tiny live indicators only: the spinner, the hot marker, the drift glyph.
/// Single cells that must read as *moving* against resting gold beside them.
///
/// This is the one token SPEC 3.2's blue ceiling does not admit — see
/// [`crate::clamp::GOLD_LIFT_BLUE_PCT`] for the arithmetic and why the value
/// stands rather than the clamp. Because it is a lift, its licence is narrow:
/// a whole row, a bar fill or a border in this colour is a bug, not a style
/// choice.
pub const GOLD_BRIGHT: Color = Color::Rgb(0xF7, 0xD9, 0x6B);

/// The world coming in: read, skill, memory, secondary emphasis.
pub const SILVER: Color = Color::Rgb(0xA9, 0xAA, 0xB5);

/// Syntax types (SPEC 6.4). The lighter silver, so a type reads above an
/// identifier without spending a second hue on it.
pub const SILVER_TYPE: Color = Color::Rgb(0xBF, 0xC1, 0xCC);

// ── Text ramp ───────────────────────────────────────────────────────────────

/// Primary text. Prose is uncoloured on purpose: the metals only mean
/// something because the default voice does not compete with them.
pub const TEXT: Color = Color::Rgb(0xE8, 0xE8, 0xEC);

/// Secondary text.
pub const MUTED: Color = Color::Rgb(0x77, 0x77, 0x82);

/// Hints, keybinding rows, line numbers. The floor: SPEC 13 forbids anything
/// dimmer than this from carrying information a reader needs.
pub const DIM: Color = Color::Rgb(0x4B, 0x4B, 0x56);

/// Code comments.
pub const COMMENT: Color = Color::Rgb(0x56, 0x56, 0x60);

// ── Verdicts ────────────────────────────────────────────────────────────────
//
// Desaturated and cool, and rationed. Red is the rarest colour on screen
// (SPEC 2): because it never appears in a healthy frame, a red gate reads as
// an alarm with no blinking and no bell. Every healthy-frame snapshot asserts
// a red cell count of zero, and that assertion is the feature.

/// Pass, and the `+` diff sign.
pub const GREEN: Color = Color::Rgb(0x74, 0xC9, 0x91);

/// Fail, the `-` diff sign, delete events, destructive actions. Nothing else,
/// ever.
pub const RED: Color = Color::Rgb(0xE0, 0x68, 0x7A);

/// Added diff row background — a tint under the syntax colours, not a wash
/// over them (SPEC 6.4: `Line.style` carries the bg, spans keep their fg).
pub const DIFF_ADD_BG: Color = Color::Rgb(0x10, 0x20, 0x1A);

/// Removed diff row background.
pub const DIFF_DEL_BG: Color = Color::Rgb(0x24, 0x10, 0x19);

/// What kind of colour a token is, and therefore which clamp holds it.
///
/// The variants are not decoration: [`ALL`] pairs every token with one, and
/// `token_roles_are_honoured` in the tests walks that pairing. A token with
/// no honest role has no way into the palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// A neutral or blue-tipped gray: the grounds and the text ramp.
    /// [`crate::clamp::is_neutral_gray`].
    Gray,
    /// The resting gold. [`crate::clamp::is_resting_gold`].
    Gold,
    /// The lifted gold, for single-cell live indicators only.
    /// [`crate::clamp::is_lifted_gold`].
    GoldLift,
    /// The second metal. [`crate::clamp::is_cool_silver`].
    Silver,
    /// A verdict hue — pass or fail. Held to no hue clamp: they are not gold
    /// and not gray, and their job is to be unmistakable rather than on-brand.
    Verdict,
    /// A diff row tint. A background, never a foreground, so it is measured by
    /// darkness rather than hue — see `diff_tints_stay_under_the_panel`.
    Tint,
}

/// Every palette token, paired with its name and role.
///
/// Lets the tests walk the whole palette without a second list to keep in
/// sync — the shape `stella-tui`'s own `palette::ALL` uses, for the same
/// reason.
pub const ALL: [(&str, Color, Role); 17] = [
    ("bg", BG, Role::Gray),
    ("panel", PANEL, Role::Gray),
    ("hl", HL, Role::Gray),
    ("border", BORDER, Role::Gray),
    ("rule", RULE, Role::Gray),
    ("gold", GOLD, Role::Gold),
    ("gold_bright", GOLD_BRIGHT, Role::GoldLift),
    ("silver", SILVER, Role::Silver),
    ("silver_type", SILVER_TYPE, Role::Silver),
    ("text", TEXT, Role::Gray),
    ("muted", MUTED, Role::Gray),
    ("dim", DIM, Role::Gray),
    ("comment", COMMENT, Role::Gray),
    ("green", GREEN, Role::Verdict),
    ("red", RED, Role::Verdict),
    ("diff_add_bg", DIFF_ADD_BG, Role::Tint),
    ("diff_del_bg", DIFF_DEL_BG, Role::Tint),
];
