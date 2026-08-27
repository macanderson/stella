//! The panel channel — a plugin that **draws** a rectangle of the screen,
//! and the frame it returns for each one it is leased.
//!
//! `design/tui-v2/SPEC.md` §12 is the design and this module is its wire half,
//! built on [`crate::driver`]'s shape: a third dispatch context, with its own
//! point, its own grant, and no rung on the [`crate::Participation`] ladder,
//! because drawing is not influence over a turn.
//!
//! Two rules from that section are held by the types instead of by a host's
//! care, and they are the reason this is a contract at all:
//!
//! - **A panel never emits an escape sequence.** [`PanelText`] wraps a private
//!   `String` whose only constructor refuses every control character, so
//!   `\x1b[2J` is a decode error on the frame that carries it. The host draws
//!   the border, the title and every escape byte the terminal ever sees.
//! - **A panel cannot address a cell it was not leased.** [`PanelRect`] carries
//!   an extent and no origin, so the coordinates a plugin writes are its own
//!   rectangle's; there is no number it could put in a frame that names a cell
//!   of Stella's chrome. [`PanelFrame::fits`] refuses one that starts or runs
//!   past the lease's own edge, naming the row and column that did it.
//!
//! The `[panel]` block is the consent half: [`PanelGrant`], whose `denies` list
//! must name every [`PanelDenial`] before the manifest loads, so the two limits
//! §12's handshake shows a human ride in the signed document they consent to.

use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::ManifestError;
use crate::runtime::{ProcessBlock, Runtime};
use crate::wire::PROTOCOL_VERSION;

/// What a `[panel]` block gives up, and must say it gives up.
///
/// Closed, and the closure is the point: the set is Stella's, not the author's,
/// so a plugin cannot ship a panel that denies less. An unknown word here is a
/// parse error for [`crate::DriverCall`]'s reason — a limit a host does not
/// recognise must refuse the manifest, never read as a limit that is absent.
///
/// Both are §12's own: a panel is a renderer, and a renderer that reaches the
/// network or writes outside its sandbox is asking for authority its drawing
/// does not need.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PanelDenial {
    /// No socket, no request, no name lookup, for the panel's own process.
    Network,
    /// No write outside the sandbox directory the host hands the panel.
    WriteOutsideSandbox,
}

impl PanelDenial {
    /// Every denial a `[panel]` block must name, in the order a prompt prints
    /// them.
    ///
    /// Exhaustive by construction, as [`crate::DriverCall::all`] is: the
    /// `match` in [`PanelDenial::as_str`] stops compiling when a case is added
    /// and left out of this list, so a new limit reaches the consent rendering
    /// and the manifest check in the change that introduces it.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[Self::Network, Self::WriteOutsideSandbox]
    }

    /// The name this denial is written as in `[panel] denies`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::WriteOutsideSandbox => "write-outside-sandbox",
        }
    }

    /// The sentence a human reads at install for this denial.
    ///
    /// Written as what the panel **may not do to their machine**, which is
    /// [`crate::DriverFamily::consent_sentence`]'s rule with the sign flipped:
    /// a grant and a refusal are both things a reader is agreeing to, and only
    /// one of them was previously spellable.
    #[must_use]
    pub fn consent_sentence(self) -> &'static str {
        match self {
            Self::Network => "may not reach the network from its panel process",
            Self::WriteOutsideSandbox => {
                "may not write anywhere but the sandbox directory Stella hands it"
            }
        }
    }
}

impl std::fmt::Display for PanelDenial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The `[panel]` block — a plugin's declaration that it draws a panel, and of
/// the limits it accepts in order to be allowed one.
///
/// **Absent means a plugin has no panel at all**, which is
/// [`crate::DriverGrant`]'s outer half restated for this context: with no
/// `[panel]` block the host leases no rectangle, so there is nothing for a
/// frame to be checked against.
///
/// # There is no title field, and that absence is the anti-spoofing rule
///
/// §12 gives the border and the title to the host, labelled `◳ panel ·
/// <plugin>`. A `title` key here would let a panel call itself `GATES` or
/// `stella*`, which is the one thing chrome a user trusts must not be able to
/// say about itself. The host writes [`crate::PluginManifest::name`] into that
/// label, so the label is the identity a human already consented to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelGrant {
    /// The limits this panel accepts, which must be every [`PanelDenial`].
    ///
    /// Spelled in the block instead of assumed, because the manifest is the
    /// document a signature covers and a human reads: a limit that exists only
    /// inside the host's build is a limit the reader's own copy of the plugin
    /// does not carry. A block naming fewer than all of them is
    /// [`ManifestError::PanelDenialMissing`], so the completeness is checked at
    /// load and never left to the reader to notice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub denies: Vec<PanelDenial>,
    /// The program the host starts to draw this panel, and the exact slice of
    /// the operator's environment it may see.
    ///
    /// **Absent means the host cannot draw this panel**, the state
    /// [`crate::DriverGrant::process`] describes for a driver nobody starts:
    /// the declaration is still a complete consent document, and installing it
    /// still runs nothing.
    ///
    /// Not `[runtime]`, for that field's own reason — `[runtime]` is the
    /// process a plugin is *inside a turn* and the manifest refuses it below
    /// `observer`, while a panel is drawn between turns and on every tick. The
    /// type is shared because the decision is identical: an argv, a timeout,
    /// and an environment allowlist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<Runtime>,
}

impl PanelGrant {
    /// Whether this grant names `denial`.
    #[must_use]
    pub fn denies(&self, denial: PanelDenial) -> bool {
        self.denies.contains(&denial)
    }

    /// The first denial this grant fails to name, in [`PanelDenial::all`]
    /// order, or `None` when it names all of them.
    ///
    /// [`PanelGrant::validate`] reads this, and so may a host that built a
    /// grant in Rust instead of parsing one.
    #[must_use]
    pub fn missing_denial(&self) -> Option<PanelDenial> {
        PanelDenial::all()
            .iter()
            .copied()
            .find(|denial| !self.denies(*denial))
    }

    /// Every load rule a `[panel]` block has to pass.
    ///
    /// Here rather than inside
    /// [`PluginManifest::validate`](crate::PluginManifest) so the rules sit
    /// beside the type they are about: a reader of `PanelGrant` sees what makes
    /// one legal without crossing to another module, and a host that assembles
    /// a grant in Rust can ask the same question the parser asks.
    ///
    /// Two rules are the ones every other block has — a repeated entry is an
    /// editing mistake rather than an emphasis, and a declared process is held
    /// to [`Runtime`]'s rules under its own block name. The third belongs to
    /// this block alone: the denial set is Stella's, so a block may not name a
    /// subset of it and call the result a narrower panel
    /// (`design/tui-v2/SPEC.md` §12's handshake).
    ///
    /// # Errors
    ///
    /// [`ManifestError::DuplicatePanelDenial`] for a repeated limit,
    /// [`ManifestError::PanelDenialMissing`] for one the block never names, and
    /// whatever `[panel.process]` refuses.
    pub fn validate(&self) -> Result<(), ManifestError> {
        let mut seen = HashSet::with_capacity(self.denies.len());
        for denial in &self.denies {
            if !seen.insert(*denial) {
                return Err(ManifestError::DuplicatePanelDenial { denial: *denial });
            }
        }
        if let Some(denial) = self.missing_denial() {
            return Err(ManifestError::PanelDenialMissing { denial });
        }
        if let Some(process) = &self.process {
            process.validate(ProcessBlock::PanelProcess)?;
        }
        Ok(())
    }
}

/// The one point a panel answers.
///
/// A closed single-case enum rather than a bare string, as
/// [`crate::DrivePoint`] is: `{"point": "before_turn"}` written into a panel
/// exchange is a decode error instead of something a reader shrugs at. A second
/// panel point would be a reviewable addition here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelPoint {
    /// The host asks for one frame, and leases the rectangle to draw it in.
    Frame,
}

impl PanelPoint {
    /// The name this point is written as on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Frame => "frame",
        }
    }
}

impl std::fmt::Display for PanelPoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The host's request for one frame: `{"point": "frame", "body": {…}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelRequest {
    /// Which point is open. Always [`PanelPoint::Frame`]; present so the
    /// request is self-describing on the wire.
    pub point: PanelPoint,
    /// The lease this frame is drawn against.
    pub body: PanelLease,
}

impl PanelRequest {
    /// Ask for one frame against `lease`.
    #[must_use]
    pub fn new(lease: PanelLease) -> Self {
        Self {
            point: PanelPoint::Frame,
            body: lease,
        }
    }
}

/// The rectangle a panel is leased for one tick, and the budget it has to fill
/// it in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelLease {
    /// The version this message is written at.
    pub protocol_version: u32,
    /// Which panel this lease is for — [`crate::PluginManifest::name`], echoed
    /// so a process drawing more than one panel can tell them apart.
    pub panel: String,
    /// The host's counter for this frame. A panel echoes it on
    /// [`PanelFrame::tick`], so a frame that arrives after the host has moved
    /// on is discardable without guessing.
    pub tick: u64,
    /// The cells this panel may address, and the only ones it can name.
    pub rect: PanelRect,
    /// How long the host will wait for the frame before it draws the previous
    /// one and tags the panel as over budget (§12's frame budget).
    ///
    /// A fact about the host, never a promise the panel is asked to keep: a
    /// slow panel is throttled with a visible tag, and is not killed for
    /// missing a number it was told.
    pub budget_ms: u32,
}

impl PanelLease {
    /// A lease at the current [`PROTOCOL_VERSION`].
    #[must_use]
    pub fn new(panel: impl Into<String>, tick: u64, rect: PanelRect, budget_ms: u32) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            panel: panel.into(),
            tick,
            rect,
            budget_ms,
        }
    }

    /// Whether `frame` stays inside this lease.
    ///
    /// # Errors
    ///
    /// [`PanelOverflow`], naming the row or the cell that ran past the edge.
    pub fn admits(&self, frame: &PanelFrame) -> Result<(), PanelOverflow> {
        frame.fits(self.rect)
    }
}

/// The extent of a panel's lease, in terminal cells.
///
/// **An extent and no origin**, which is what makes addressing outside the
/// lease unrepresentable rather than refused after the fact: a panel counts
/// rows and columns from its own top-left corner, so the host's buffer
/// coordinates are a number it is never told and can never write. The host adds
/// its own origin when it blits, and the border it drew is outside the
/// translation entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelRect {
    /// How many columns wide, inside the host's border.
    pub cols: u16,
    /// How many rows tall, inside the host's border and below its title.
    pub rows: u16,
}

impl PanelRect {
    /// A rectangle of `cols` by `rows` cells.
    #[must_use]
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }

    /// Whether `row` and `col` name a cell of this rectangle.
    #[must_use]
    pub const fn holds(self, row: u16, col: u16) -> bool {
        row < self.rows && col < self.cols
    }
}

/// The panel's answer: `{"point": "frame", "body": {…}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelResponse {
    /// Which point this answers. Always [`PanelPoint::Frame`].
    pub point: PanelPoint,
    /// The frame itself.
    pub body: PanelFrame,
}

impl PanelResponse {
    /// Answer a frame request.
    #[must_use]
    pub fn new(frame: PanelFrame) -> Self {
        Self {
            point: PanelPoint::Frame,
            body: frame,
        }
    }
}

/// One frame a panel draws into the rectangle it was leased.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelFrame {
    /// The version this message is written at.
    pub protocol_version: u32,
    /// The [`PanelLease::tick`] this frame answers.
    pub tick: u64,
    /// What to draw.
    pub paint: PanelPaint,
}

impl PanelFrame {
    /// A frame at the current [`PROTOCOL_VERSION`].
    #[must_use]
    pub fn new(tick: u64, paint: PanelPaint) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            tick,
            paint,
        }
    }

    /// Whether every cell this frame addresses is inside `rect`.
    ///
    /// # Errors
    ///
    /// [`PanelOverflow`], naming the first row or cell that ran past the edge.
    pub fn fits(&self, rect: PanelRect) -> Result<(), PanelOverflow> {
        self.paint.fits(rect)
    }
}

/// The two shapes a frame comes in — §12's "styled lines or a cell diff".
///
/// Externally tagged, so `{"lines": […]}` and `{"diff": […]}` are the two
/// legal frames and a table carrying both keys is a decode error. A panel that
/// redraws everything writes lines; one that changed four cells writes a diff,
/// and the host blits either into the same leased rectangle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelPaint {
    /// Every row of the lease, from its top, each row a run of styled spans. A
    /// list shorter than the lease leaves the rows below it untouched.
    Lines(Vec<PanelLine>),
    /// Only the cells that changed since the last frame, each patch a run
    /// starting at one addressed cell.
    Diff(Vec<PanelPatch>),
}

impl PanelPaint {
    /// Whether every cell this paint addresses is inside `rect`.
    ///
    /// # Errors
    ///
    /// [`PanelOverflow`], naming the first row or cell that ran past the edge.
    pub fn fits(&self, rect: PanelRect) -> Result<(), PanelOverflow> {
        match self {
            Self::Lines(lines) => {
                if lines.len() > usize::from(rect.rows) {
                    return Err(PanelOverflow::Rows {
                        lines: lines.len(),
                        rows: rect.rows,
                    });
                }
                for (index, line) in lines.iter().enumerate() {
                    let cells = line.cells();
                    if cells > usize::from(rect.cols) {
                        return Err(PanelOverflow::Line {
                            line: index,
                            cells,
                            cols: rect.cols,
                        });
                    }
                }
                Ok(())
            }
            Self::Diff(patches) => {
                for patch in patches {
                    if patch.row >= rect.rows {
                        return Err(PanelOverflow::Row {
                            row: patch.row,
                            rows: rect.rows,
                        });
                    }
                    let cells = patch.text.cells();
                    // A run of *no* glyphs anchored past the right edge
                    // satisfies `col + 0 <= cols` for every column a `u16` can
                    // hold, so the sum on its own admits a patch whose origin
                    // names a column the lease does not have. It writes
                    // nothing, and this function is still the whole guarantee a
                    // host indexes against, so the anchor is checked as well as
                    // the extent. That check is also what makes a zero-column
                    // lease admit no patch rather than every empty one.
                    //
                    // The sum is in `usize` throughout: a `u16` column plus a
                    // run longer than the rest of the row is exactly the
                    // addition that wraps, and a wrapped sum reads as a cell
                    // inside the lease.
                    if patch.col >= rect.cols
                        || usize::from(patch.col) + cells > usize::from(rect.cols)
                    {
                        return Err(PanelOverflow::Patch {
                            row: patch.row,
                            col: patch.col,
                            cells,
                            cols: rect.cols,
                        });
                    }
                }
                Ok(())
            }
        }
    }
}

/// One row of a [`PanelPaint::Lines`] frame.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelLine {
    /// The runs of this row, left to right. Empty clears the row.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<PanelSpan>,
}

impl PanelLine {
    /// A row of one styled run.
    #[must_use]
    pub fn new(spans: Vec<PanelSpan>) -> Self {
        Self { spans }
    }

    /// How many cells this row occupies.
    #[must_use]
    pub fn cells(&self) -> usize {
        self.spans.iter().map(|span| span.text.cells()).sum()
    }
}

/// One run of text in a row, drawn in one style.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelSpan {
    /// The glyphs, which carry no escape sequence — see [`PanelText`].
    pub text: PanelText,
    /// How to draw them. Omitted on the wire when nothing is asked for, which
    /// is the deck's own resting `text` on `bg`.
    #[serde(default, skip_serializing_if = "PanelStyle::is_plain")]
    pub style: PanelStyle,
}

impl PanelSpan {
    /// A run in one style.
    #[must_use]
    pub fn new(text: PanelText, style: PanelStyle) -> Self {
        Self { text, style }
    }
}

/// One run of a [`PanelPaint::Diff`] frame, starting at the cell it addresses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelPatch {
    /// Which row of the lease, counted from its top.
    pub row: u16,
    /// Which column of the lease, counted from its left edge.
    pub col: u16,
    /// The glyphs to write from that cell rightwards.
    pub text: PanelText,
    /// How to draw them.
    #[serde(default, skip_serializing_if = "PanelStyle::is_plain")]
    pub style: PanelStyle,
}

impl PanelPatch {
    /// A run starting at one cell of the lease.
    #[must_use]
    pub fn new(row: u16, col: u16, text: PanelText, style: PanelStyle) -> Self {
        Self {
            row,
            col,
            text,
            style,
        }
    }
}

/// Glyphs a panel writes into cells it was leased.
///
/// **The no-raw-escapes rule, held by the type.** The body is a private
/// `String` whose only constructor is [`PanelText::new`], and that constructor
/// refuses every control character — `\x1b`, and with it every CSI, OSC and SGR
/// sequence a plugin could use to move the cursor out of its rectangle, clear
/// the screen, repaint Stella's own chrome, or set a window title. `\n`, `\r`
/// and `\t` go with them: a row's structure is the frame's, and a tab's width
/// is the terminal's rather than this contract's.
///
/// The privacy is what makes it a rule and not a request, exactly as
/// [`crate::VolatileContext`]'s is. A public field would let a host write
/// `PanelText("\x1b[2J".into())` in one line, change no type, and stay green:
///
/// ```compile_fail
/// let text = stella_plugin::PanelText("\u{1b}[2J".to_string());
/// ```
///
/// The sanctioned reads, and all of them — the glyphs, and how many cells they
/// take:
///
/// ```
/// use stella_plugin::PanelText;
/// let text = PanelText::new("gates: 3 green").expect("plain glyphs");
/// assert_eq!(text.as_str(), "gates: 3 green");
/// assert_eq!(text.cells(), 14);
/// assert!(PanelText::new("\u{1b}[31mred").is_err());
/// ```
///
/// # What a cell is here, and what it is not
///
/// [`PanelText::cells`] counts `char`s. A double-width glyph therefore draws
/// wider than this contract counts, and the frame that carries it misaligns
/// **its own interior** — never anything outside it, because the host clips
/// every blit at the leased rectangle whatever a frame claims. Measuring
/// display width would mean a Unicode width table, and this crate is a
/// near-leaf that takes one workspace dependency on argument (see its
/// `Cargo.toml`); the host already owns the clip that makes the difference
/// safe.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PanelText(String);

impl PanelText {
    /// Build a run of glyphs.
    ///
    /// # Errors
    ///
    /// [`PanelTextError::ControlCharacter`] when the text carries a control
    /// character, naming the character and where it sits.
    pub fn new(text: impl Into<String>) -> Result<Self, PanelTextError> {
        let text = text.into();
        if let Some((index, found)) = text
            .chars()
            .enumerate()
            .find(|(_, found)| found.is_control())
        {
            return Err(PanelTextError::ControlCharacter {
                index,
                code: found as u32,
            });
        }
        Ok(Self(text))
    }

    /// The glyphs.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// How many cells these glyphs occupy — see the type docs on what a cell
    /// counts as here.
    #[must_use]
    pub fn cells(&self) -> usize {
        self.0.chars().count()
    }

    /// Whether this run draws nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Display for PanelText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for PanelText {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for PanelText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Through the same constructor the Rust side uses, so a frame carrying
        // an escape sequence fails to decode instead of arriving as a value the
        // host is trusted to inspect.
        let text = String::deserialize(deserializer)?;
        Self::new(text).map_err(serde::de::Error::custom)
    }
}

/// Why a run of glyphs is not drawable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PanelTextError {
    /// The text carries a control character. A panel writes glyphs into cells;
    /// every escape byte the terminal sees is the host's.
    #[error(
        "panel text carries the control character U+{code:04X} at position {index}: a panel \
         writes glyphs into the cells it was leased, and Stella writes every escape sequence \
         the terminal sees"
    )]
    ControlCharacter {
        /// Which character of the text, counted in `char`s from zero.
        index: usize,
        /// The Unicode scalar value that was refused.
        code: u32,
    },
}

/// How a run of glyphs is drawn.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelStyle {
    /// The ink. `None` is the deck's resting `text`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<PanelInk>,
    /// The ground. `None` is whatever the host already painted there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<PanelInk>,
    /// Everything else, from a closed list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emphasis: Vec<PanelEmphasis>,
}

impl PanelStyle {
    /// The style that asks for nothing.
    #[must_use]
    pub fn plain() -> Self {
        Self::default()
    }

    /// A style in one ink.
    #[must_use]
    pub fn ink(fg: PanelInk) -> Self {
        Self {
            fg: Some(fg),
            ..Self::default()
        }
    }

    /// Whether this style asks for nothing — the predicate the wire's
    /// `skip_serializing_if` needs, so an unstyled span omits the key.
    #[must_use]
    pub fn is_plain(&self) -> bool {
        *self == Self::default()
    }
}

/// The colours a panel may paint with.
///
/// Token names and never an RGB triple, which is what keeps §2's two-metal rule
/// and §3.2's hue clamp true of a plugin's pixels as well as Stella's: the host
/// resolves each name against the live theme, so a panel follows a degraded
/// sixteen-colour terminal (§3.5) without knowing one exists, and no plugin can
/// author the warm hue the clamp exists to refuse.
///
/// The set is `design/tui-v2/SPEC.md` §3.1's `tui` surface exactly. This crate
/// spells its own copy because it is a near-leaf that may not depend on the
/// interactive-mode crates; the copy is checkable against the SPEC table by
/// eye, and a name that is not in it is a decode error rather than a colour
/// nobody chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelInk {
    /// The canvas.
    Bg,
    /// Code blocks, panels, tables.
    Panel,
    /// Selected and highlighted rows.
    Hl,
    /// Panel borders and dividers.
    Border,
    /// Turn boundary rules.
    Rule,
    /// Stella acting on the world: edit, write, gate, brand, money.
    Gold,
    /// Live indicators only: spinner, hot marker, drift glyph.
    GoldBright,
    /// The world coming in: read, skill, memory, secondary emphasis.
    Silver,
    /// Syntax types.
    SilverType,
    /// Primary text.
    Text,
    /// Secondary text.
    Muted,
    /// Hints, keybinding rows, line numbers.
    Dim,
    /// Pass, and the `+` diff sign.
    Green,
    /// Fail, the `-` diff sign, delete and destructive events. §2 makes this
    /// the rarest colour on screen; a panel that paints it everywhere spends
    /// the alarm the whole deck depends on.
    Red,
    /// Added diff row background.
    DiffAddBg,
    /// Removed diff row background.
    DiffDelBg,
}

/// Everything a style may ask for beyond its two colours.
///
/// Closed, and two familiar attributes are absent. `reverse` swaps a span's ink
/// and ground, which is how a panel would paint a bar the same shape as the
/// host's own selected row; `blink` is not a property of a character cell at
/// all, and §14 rules out motion that carries meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelEmphasis {
    /// Heavier weight.
    Bold,
    /// Lighter weight.
    Dim,
    /// Slanted.
    Italic,
    /// Ruled underneath.
    Underline,
}

/// Why a frame does not fit the rectangle it was leased.
///
/// Four cases because the two frame shapes fail in two ways each, and a host
/// refusing a frame should be able to print which row, which column and which
/// edge without re-deriving any of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PanelOverflow {
    /// A [`PanelPaint::Lines`] frame carries more rows than the lease has.
    #[error("a panel frame of {lines} line(s) does not fit a lease {rows} row(s) tall")]
    Rows {
        /// How many rows the frame carries.
        lines: usize,
        /// How many the lease holds.
        rows: u16,
    },
    /// A row of a [`PanelPaint::Lines`] frame runs past the lease's right edge.
    #[error("line {line} of a panel frame is {cells} cell(s) wide, past a {cols}-column lease")]
    Line {
        /// Which row, counted from the top of the frame.
        line: usize,
        /// How wide it is.
        cells: usize,
        /// How wide the lease is.
        cols: u16,
    },
    /// A [`PanelPaint::Diff`] patch addresses a row the lease does not have.
    #[error("a panel frame patches row {row}, past a lease {rows} row(s) tall")]
    Row {
        /// The row the patch addressed.
        row: u16,
        /// How many rows the lease holds.
        rows: u16,
    },
    /// A [`PanelPaint::Diff`] patch starts past the lease's right edge, or runs
    /// past it. Both are this one case because both are answered by the same
    /// edit — move the patch left or shorten it — and a host printing the
    /// refusal wants the column and the run length either way.
    #[error(
        "a panel frame patches {cells} cell(s) from column {col} of row {row}, past a \
         {cols}-column lease"
    )]
    Patch {
        /// The row the patch addressed.
        row: u16,
        /// The column it started at.
        col: u16,
        /// How many cells it writes.
        cells: usize,
        /// How wide the lease is.
        cols: u16,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(glyphs: &str) -> PanelText {
        PanelText::new(glyphs).expect("plain glyphs")
    }

    #[test]
    fn every_denial_has_a_distinct_wire_name_and_a_sentence() {
        let mut names: Vec<&str> = PanelDenial::all()
            .iter()
            .map(|denial| denial.as_str())
            .collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "two denials share a wire name");
        for denial in PanelDenial::all() {
            assert!(!denial.consent_sentence().trim().is_empty(), "{denial}");
        }
    }

    #[test]
    fn all_is_exhaustive_over_the_denial_set() {
        // Round-tripping every wire name back through the deserializer proves
        // `all()` and the enum agree, without a hand-maintained count.
        for denial in PanelDenial::all() {
            let json = serde_json::to_string(denial).expect("a denial serializes");
            let back: PanelDenial = serde_json::from_str(&json).expect("and reads back");
            assert_eq!(back, *denial);
        }
        assert!(
            serde_json::from_str::<PanelDenial>("\"read-anything\"").is_err(),
            "the denial set is Stella's, so an unknown limit is a refusal"
        );
    }

    #[test]
    fn a_grant_reports_the_first_denial_it_fails_to_name() {
        let partial = PanelGrant {
            denies: vec![PanelDenial::WriteOutsideSandbox],
            process: None,
        };
        assert_eq!(partial.missing_denial(), Some(PanelDenial::Network));
        let complete = PanelGrant {
            denies: PanelDenial::all().to_vec(),
            process: None,
        };
        assert_eq!(complete.missing_denial(), None);
        assert!(complete.denies(PanelDenial::Network));
    }

    #[test]
    fn a_run_of_glyphs_refuses_every_control_character() {
        for hazard in ["\u{1b}[2J", "a\nb", "a\rb", "a\tb", "a\u{9b}c"] {
            assert!(
                PanelText::new(hazard).is_err(),
                "{hazard:?} decoded as drawable glyphs"
            );
        }
        assert_eq!(
            PanelText::new("\u{1b}").unwrap_err(),
            PanelTextError::ControlCharacter { index: 0, code: 27 }
        );
        // The index counts `char`s, so a multi-byte glyph before the hazard
        // does not shift it into a byte offset a reader cannot navigate by.
        assert_eq!(
            PanelText::new("✦\u{1b}").unwrap_err(),
            PanelTextError::ControlCharacter { index: 1, code: 27 }
        );
    }

    #[test]
    fn a_frame_of_lines_that_overruns_its_lease_is_refused() {
        let rect = PanelRect::new(8, 2);
        let one = || PanelLine::new(vec![PanelSpan::new(text("gates"), PanelStyle::plain())]);
        let fits = PanelPaint::Lines(vec![one(), one()]);
        assert_eq!(fits.fits(rect), Ok(()));

        let too_tall = PanelPaint::Lines(vec![one(), one(), one()]);
        assert_eq!(
            too_tall.fits(rect),
            Err(PanelOverflow::Rows { lines: 3, rows: 2 })
        );

        let too_wide = PanelPaint::Lines(vec![PanelLine::new(vec![PanelSpan::new(
            text("nine cells"),
            PanelStyle::plain(),
        )])]);
        assert_eq!(
            too_wide.fits(rect),
            Err(PanelOverflow::Line {
                line: 0,
                cells: 10,
                cols: 8,
            })
        );
    }

    #[test]
    fn a_cell_diff_outside_the_lease_is_refused() {
        let rect = PanelRect::new(8, 2);
        let patch = |row, col, glyphs| PanelPatch::new(row, col, text(glyphs), PanelStyle::plain());

        assert_eq!(PanelPaint::Diff(vec![patch(1, 7, "x")]).fits(rect), Ok(()));
        assert_eq!(
            PanelPaint::Diff(vec![patch(2, 0, "x")]).fits(rect),
            Err(PanelOverflow::Row { row: 2, rows: 2 })
        );
        assert_eq!(
            PanelPaint::Diff(vec![patch(0, 6, "abc")]).fits(rect),
            Err(PanelOverflow::Patch {
                row: 0,
                col: 6,
                cells: 3,
                cols: 8,
            })
        );
    }

    #[test]
    fn a_column_plus_a_long_run_cannot_wrap_back_into_the_lease() {
        // `u16::MAX` cells past the right edge is the addition that wraps in
        // `u16` and lands back inside a small rectangle. The check runs in
        // `usize`, so it refuses.
        let rect = PanelRect::new(4, 1);
        let long = text(&"x".repeat(usize::from(u16::MAX) + 8));
        let paint = PanelPaint::Diff(vec![PanelPatch::new(0, 2, long, PanelStyle::plain())]);
        assert!(matches!(
            paint.fits(rect),
            Err(PanelOverflow::Patch {
                col: 2,
                cols: 4,
                ..
            })
        ));
    }

    #[test]
    fn an_empty_frame_fits_every_lease_including_an_empty_one() {
        let empty = PanelRect::new(0, 0);
        assert_eq!(PanelPaint::Lines(Vec::new()).fits(empty), Ok(()));
        assert_eq!(PanelPaint::Diff(Vec::new()).fits(empty), Ok(()));
        // And a lease with no cells admits no patch at all.
        let paint = PanelPaint::Diff(vec![PanelPatch::new(0, 0, text("x"), PanelStyle::plain())]);
        assert_eq!(
            paint.fits(empty),
            Err(PanelOverflow::Row { row: 0, rows: 0 })
        );
    }

    #[test]
    fn a_patch_of_no_glyphs_is_still_anchored_inside_the_lease() {
        // A run of nothing writes nothing, so the *extent* check has no opinion
        // about where it starts: `col + 0` is inside every lease. A host that
        // reads the column before it measures the run is handed one its buffer
        // does not have, and `fits` is the only thing standing between the two.
        let rect = PanelRect::new(8, 2);
        let empty_run = |col| PanelPatch::new(0, col, text(""), PanelStyle::plain());

        assert_eq!(PanelPaint::Diff(vec![empty_run(7)]).fits(rect), Ok(()));
        assert_eq!(
            PanelPaint::Diff(vec![empty_run(8)]).fits(rect),
            Err(PanelOverflow::Patch {
                row: 0,
                col: 8,
                cells: 0,
                cols: 8,
            })
        );
        assert_eq!(
            PanelPaint::Diff(vec![empty_run(u16::MAX)]).fits(rect),
            Err(PanelOverflow::Patch {
                row: 0,
                col: u16::MAX,
                cells: 0,
                cols: 8,
            })
        );

        // The same rule read from the other end: a lease with rows but no
        // columns is a lease with no cells, so it admits no patch either.
        let columnless = PanelRect::new(0, 2);
        assert_eq!(
            PanelPaint::Diff(vec![empty_run(0)]).fits(columnless),
            Err(PanelOverflow::Patch {
                row: 0,
                col: 0,
                cells: 0,
                cols: 0,
            })
        );
    }

    #[test]
    fn a_lease_admits_the_frame_that_answers_it() {
        let lease = PanelLease::new("gates", 7, PanelRect::new(12, 1), 33);
        let frame = PanelFrame::new(
            7,
            PanelPaint::Lines(vec![PanelLine::new(vec![PanelSpan::new(
                text("3 green"),
                PanelStyle::ink(PanelInk::Green),
            )])]),
        );
        assert_eq!(lease.admits(&frame), Ok(()));
        assert_eq!(frame.protocol_version, PROTOCOL_VERSION);
    }
}
