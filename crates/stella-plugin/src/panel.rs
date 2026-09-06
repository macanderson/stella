//! The panel channel — a plugin that **draws** a rectangle of the screen,
//! and the frame it returns for each one it is leased.
//!
//! `design/tui-v2/SPEC.md` §12 is the design and this module is its wire half,
//! built on [`crate::driver`]'s shape: a third dispatch context, with its own
//! point, its own grant, and no rung on the [`crate::Participation`] ladder,
//! because drawing is not influence over a turn.
//!
//! Some of that section's rules are held by the types, not by a host's care.
//! That is why this is a contract at all:
//!
//! - **A panel never emits an escape sequence, and never flips its own text**.
//!   [`PanelText`] wraps a private `String`. Its only constructor turns away
//!   every control `char` and every bidi one. `\x1b[2J` and `U+202E` are both
//!   decode errors on the frame that carries them. The host draws the
//!   border, the title and every escape byte the terminal ever sees.
//! - **A panel cannot address a cell it was not leased**. [`PanelRect`]
//!   carries an extent and no origin. The coordinates a plugin writes are
//!   its own rectangle's. There is no number it could put in a frame that
//!   names a cell of Stella's chrome. [`PanelFrame::fits`] refuses one that
//!   starts or runs past the lease's own edge, naming the row and column
//!   that did it.
//!
//! The `[panel]` block is the consent half: [`PanelGrant`], whose `denies` list
//! must name every [`PanelDenial`] before the manifest loads, so the two limits
//! §12's handshake shows a human ride in the signed document they consent to.
//! It also names its [`PanelSurface`]s — a settings pane, a transcript overlay
//! and a `/name` popup are three separate asks, and a reader agreeing to one
//! should not be agreeing to the other two (#5203).

use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::drawable::{first_bidi_control, first_control_character};
use crate::error::ManifestError;
use crate::runtime::{ProcessBlock, Runtime};
use crate::wire::PROTOCOL_VERSION;

mod error;
#[cfg(test)]
mod tests;

pub use error::{PanelOverflow, PanelRefusal, PanelTextError};

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

/// Where a panel draws.
///
/// Closed, on [`PanelDenial`]'s reasoning: the places Stella has to put a
/// plugin's rectangle are Stella's, so an unknown word is a parse error rather
/// than a placement that reads as absent.
///
/// The kind selects **placement and chrome, never protocol** — all three are
/// leased a [`PanelRect`] and answer with a [`PanelFrame`], so a plugin that
/// draws two of them draws them the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelSurface {
    /// A pane inside the SETTINGS tab (`design/tui-v2/SPEC.md` §9.5), beside
    /// the executions table and the agent editor.
    Settings,
    /// A host-bordered block in the transcript, labelled `◳ panel · <plugin>`
    /// (§12). The placement §12 describes when it says "the host blits it into
    /// the buffer".
    Overlay,
    /// A popup opened by typing the panel's slash name, centered and treated
    /// like the command palette (§10).
    Command,
}

impl PanelSurface {
    /// Every surface a `[panel]` block may name, in the order a prompt prints
    /// them.
    ///
    /// Exhaustive by construction for [`PanelDenial::all`]'s reason: the
    /// `match` in [`PanelSurface::as_str`] stops compiling when a case is added
    /// and left out here.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[Self::Settings, Self::Overlay, Self::Command]
    }

    /// The name this surface is written as in `[panel] surfaces`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::Overlay => "overlay",
            Self::Command => "command",
        }
    }

    /// The sentence a human reads at install for this placement.
    ///
    /// Written as what appears **on their screen**, because that is what a
    /// reader is agreeing to see — [`PanelDenial::consent_sentence`]'s rule
    /// pointed at a grant rather than a refusal.
    #[must_use]
    pub fn consent_sentence(self) -> &'static str {
        match self {
            Self::Settings => "draws a pane inside your SETTINGS tab",
            Self::Overlay => "draws a bordered block in your transcript",
            Self::Command => "adds a popup you open by typing its name",
        }
    }
}

impl std::fmt::Display for PanelSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The longest a `[panel] command` may be, in `char`s. A slash name a person
/// types, so the bound is what stays typable rather than what fits.
pub const MAX_PANEL_COMMAND_CHARS: usize = 32;

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
/// label, so the label is the identity a human already consented to — and
/// [`PluginManifest::validate`](crate::PluginManifest) refuses a name that is
/// not drawable, so
/// the identity cannot carry an escape sequence into the chrome either.
///
/// A caption field was written and removed while #5203 was in review. Printing
/// it *beside* the host's label rather than instead of it looks safe, and it is
/// not: it makes the guarantee a property of the host's layout, so any surface
/// that later renders the caption alone hands a plugin the label. Nothing here
/// could enforce that ordering, which is the argument for the field not
/// existing rather than for documenting how to use it carefully.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelGrant {
    /// Where this panel draws, and every place it draws.
    ///
    /// Required and non-empty whenever the block is present: a panel naming no
    /// surface draws nowhere, which is not a narrower panel but an unfinished
    /// declaration ([`ManifestError::PanelNoSurface`]). A repeated entry is an
    /// editing mistake rather than an emphasis, as `[panel] denies`'s is
    /// ([`ManifestError::PanelDuplicateSurface`]).
    ///
    /// A set, not one kind. The three placements are independent — a plugin
    /// may reasonably want a settings pane and a `/name` popup and no
    /// transcript block. The consent prompt also reads better as a list of
    /// what appears on screen than as three separate blocks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub surfaces: Vec<PanelSurface>,
    /// The bare slash name this panel's popup opens under, without the leading
    /// `/`.
    ///
    /// **Absent means the plugin's id**, which is the product rule: a plugin is
    /// launched by its own name (`/hello`), and the namespaced
    /// `/plugin:hello` form resolves as an always-available alias.
    ///
    /// Only meaningful alongside [`PanelSurface::Command`]. Set without it, it
    /// is a promise the interface will never keep — there is no popup for the
    /// name to open — so it is [`ManifestError::PanelCommandWithoutSurface`]
    /// rather than a key a reader is left to wonder about.
    ///
    /// # This crate checks the shape and not the collision
    ///
    /// Validated here as a slug: lowercase ASCII opening character, then
    /// lowercase, digits and `-`, non-empty and bounded by
    /// [`MAX_PANEL_COMMAND_CHARS`]. Whether the name collides with a built-in
    /// command is **not** asked here and cannot be: the built-in table is
    /// `stella-cli`'s `DECK_BUILTINS`, and this crate is a near-leaf that takes
    /// `stella-protocol` and nothing else (AGENTS.md § "When a new crate is
    /// justified" is the same argument one boundary over). The host owns that
    /// check, and it must **refuse visibly** — naming the plugin and the name it
    /// wanted — rather than dropping the row in silence the way a colliding
    /// `.stella/commands/*.toml` entry is dropped today. A signed manifest a
    /// human read must not contain a name that quietly does nothing (#5055).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
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

    /// Whether this panel draws on `surface`.
    #[must_use]
    pub fn draws(&self, surface: PanelSurface) -> bool {
        self.surfaces.contains(&surface)
    }

    /// The first surface this grant names twice, in declaration order, or
    /// `None` when every entry is distinct.
    #[must_use]
    pub fn duplicate_surface(&self) -> Option<PanelSurface> {
        let mut seen = HashSet::with_capacity(self.surfaces.len());
        self.surfaces
            .iter()
            .copied()
            .find(|surface| !seen.insert(*surface))
    }

    /// The bare slash name this panel's popup opens under, resolving the
    /// default, or `None` when this panel has no popup at all.
    ///
    /// `plugin_id` is what an undeclared name means. `None` for a panel that
    /// does not draw on [`PanelSurface::Command`], so a caller cannot register
    /// a name for a popup that does not exist.
    #[must_use]
    pub fn command_or<'a>(&'a self, plugin_id: &'a str) -> Option<&'a str> {
        self.draws(PanelSurface::Command)
            .then(|| self.command.as_deref().unwrap_or(plugin_id))
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
    /// Two rules are the ones every other block has. A repeated entry is an
    /// editing mistake, not an emphasis. A declared process is held to
    /// [`Runtime`]'s rules under its own block name.
    ///
    /// Three rules belong to this block alone. The denial set is Stella's, so
    /// a block cannot name a subset of it and call the result a narrower
    /// panel (`design/tui-v2/SPEC.md` §12's handshake). A panel must name at
    /// least one place it draws. A slash name is checked for the shape the
    /// host will have to register (#5203).
    ///
    /// **Where it draws is asked before what it gives up**, because a block
    /// naming no surface is unfinished rather than over-permissive, and telling
    /// its author about a missing denial first would send them to fix the
    /// second-most-wrong thing.
    ///
    /// # Errors
    ///
    /// [`ManifestError::PanelNoSurface`] for a panel that draws nowhere,
    /// [`ManifestError::PanelDuplicateSurface`] for a repeated placement,
    /// [`ManifestError::PanelCommandWithoutSurface`] for a slash name with no
    /// popup to open, the `PanelCommand*` cases for a name the host could not
    /// register, [`ManifestError::DuplicatePanelDenial`] for a repeated limit,
    /// [`ManifestError::PanelDenialMissing`] for one the block never names, and
    /// whatever `[panel.process]` refuses.
    /// `plugin_id` is required because the slash name this grant registers is
    /// [`PanelGrant::command_or`]'s answer, not the `command` field: an
    /// undeclared name resolves to the plugin's own. Checking only a declared
    /// one left the derived name unchecked, so a plugin named `vera:admin`
    /// registered the namespace-shaped slash command
    /// [`ManifestError::PanelCommandCarriesNamespace`] exists to refuse, and a
    /// name with spaces or capitals registered a command nobody can type.
    pub fn validate(&self, plugin_id: &str) -> Result<(), ManifestError> {
        if self.surfaces.is_empty() {
            return Err(ManifestError::PanelNoSurface);
        }
        if let Some(surface) = self.duplicate_surface() {
            return Err(ManifestError::PanelDuplicateSurface { surface });
        }
        // Declaring a name for a popup that does not exist is a mistake only an
        // author can make, so it stays keyed on the declared field.
        if let Some(command) = &self.command
            && !self.draws(PanelSurface::Command)
        {
            return Err(ManifestError::PanelCommandWithoutSurface {
                command: command.clone(),
            });
        }
        if let Some(command) = self.command_or(plugin_id) {
            validate_panel_command(command)?;
        }
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

/// Whether `command` is a slash name a person can type and a host can register.
///
/// The shape check, and not the collision check: this crate cannot see the
/// built-in command table (see [`PanelGrant::command`]).
fn validate_panel_command(command: &str) -> Result<(), ManifestError> {
    if command.is_empty() {
        return Err(ManifestError::PanelCommandBlank);
    }
    let chars = command.chars().count();
    if chars > MAX_PANEL_COMMAND_CHARS {
        return Err(ManifestError::PanelCommandTooLong {
            chars,
            max: MAX_PANEL_COMMAND_CHARS,
        });
    }
    // Its own refusal, ahead of the general one, because the alias makes this
    // the mistake an author is most likely to make on purpose: `/plugin:hello`
    // is a real way to reach the panel, so a reader can reasonably think the
    // namespace is theirs to write. It is derived, never declared.
    if command.contains(':') {
        return Err(ManifestError::PanelCommandCarriesNamespace {
            command: command.to_string(),
        });
    }
    for (index, found) in command.chars().enumerate() {
        let opens = found.is_ascii_lowercase();
        let continues = index > 0 && (found.is_ascii_digit() || found == '-');
        if !opens && !continues {
            return Err(ManifestError::PanelCommandNotASlug { found, index });
        }
    }
    Ok(())
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
    /// Which plugin this lease is for — [`crate::PluginManifest::name`].
    ///
    /// The plugin and **not** the panel: one plugin may declare all three
    /// [`PanelSurface`]s, so this is shared by every lease it is handed and
    /// disambiguates nothing on its own. [`PanelLease::surface`] is what says
    /// which rectangle this is (#5210).
    pub panel: String,
    /// Which of the plugin's panels this lease is for.
    ///
    /// A plugin declaring several surfaces receives several leases per tick,
    /// alike in everything but this and their extents. It is echoed on
    /// [`PanelFrame::surface`] for [`PanelLease::tick`]'s reason: a host that
    /// has several in flight routes the answer instead of guessing.
    pub surface: PanelSurface,
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
    pub fn new(
        panel: impl Into<String>,
        surface: PanelSurface,
        tick: u64,
        rect: PanelRect,
        budget_ms: u32,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            panel: panel.into(),
            surface,
            tick,
            rect,
            budget_ms,
        }
    }

    /// Whether `frame` is the answer to this lease, and stays inside it.
    ///
    /// **Both questions, through one call**, because a host that asked only the
    /// geometric one would blit a settings pane's frame into a command popup
    /// whenever the two happened to be the same size — every cell inside the
    /// lease, and the wrong panel's content. [`PanelFrame::fits`] is still the
    /// geometry alone, for a caller that has already routed.
    ///
    /// # Errors
    ///
    /// [`PanelRefusal`], naming the surface, the tick, or the cell that did it.
    pub fn admits(&self, frame: &PanelFrame) -> Result<(), PanelRefusal> {
        if frame.surface != self.surface {
            return Err(PanelRefusal::Surface {
                leased: self.surface,
                answered: frame.surface,
            });
        }
        if frame.tick != self.tick {
            return Err(PanelRefusal::Tick {
                leased: self.tick,
                answered: frame.tick,
            });
        }
        frame.fits(self.rect).map_err(PanelRefusal::Overflow)
    }
}

/// The extent of a panel's lease, in terminal cells.
///
/// **An extent and no origin.** A panel counts rows and columns from its own
/// top-left corner. It is never told the host's buffer coordinates, so it
/// can never write them — a stronger rule than a check that refuses a bad
/// number after the fact. The host adds its own origin when it blits, and
/// the border it drew sits outside the translation entirely.
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
    /// The [`PanelLease::surface`] this frame answers — which of the plugin's
    /// panels it drew.
    pub surface: PanelSurface,
    /// The [`PanelLease::tick`] this frame answers.
    pub tick: u64,
    /// What to draw.
    pub paint: PanelPaint,
}

impl PanelFrame {
    /// A frame at the current [`PROTOCOL_VERSION`].
    #[must_use]
    pub fn new(surface: PanelSurface, tick: u64, paint: PanelPaint) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            surface,
            tick,
            paint,
        }
    }

    /// Whether every cell this frame addresses is inside `rect`.
    ///
    /// The geometry alone. [`PanelLease::admits`] is the whole question — it
    /// asks this one after it has established that the frame answers the lease
    /// at all.
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
/// `String`, and [`PanelText::new`] is its only door. That constructor
/// refuses every control character — `\x1b`, and with it every CSI, OSC and
/// SGR run. A plugin cannot use one to move the cursor out of its rectangle,
/// clear the screen, repaint Stella's own chrome, or set a window title.
/// `\n`, `\r` and `\t` go with them: a row's shape is the frame's, and a
/// tab's width is the terminal's, not this contract's.
///
/// **And the no-flip rule, held the same way.** The same constructor turns
/// away the bidi `char`s — `U+061C`, `U+200E`, `U+200F`, `U+202A`–`U+202E`,
/// `U+2066`–`U+2069`. Each one flips the text after it. A run can then read in
/// an order its bytes do not have. That is the Trojan Source shape
/// (`CVE-2021-42574`). None of it leaves the leased box, since the host clips
/// each blit. It still counts. The box is chrome a person chose to trust. The
/// rest of `Cf` is fine: `U+200D` builds the emoji runs, and `U+200C` is plain
/// Persian and `Indic` text. This crate's `drawable.rs` has the case.
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
/// assert!(PanelText::new("gates: 3 \u{202e}neerg").is_err());
/// ```
///
/// # What a cell is here, and what it is not
///
/// [`PanelText::cells`] counts `char`s, so a cell here is a glyph and not a
/// terminal column. `あ` is one glyph and two columns; `e` followed by a
/// combining acute is two glyphs and one column. A frame this contract admits
/// can therefore need more columns than its lease has, or fewer.
///
/// The host measures the columns. `stella_tui::plugin_panel`'s `write_run`
/// places a glyph only when every column it needs is inside the lease, so a
/// row of wide glyphs is cut at that edge rather than drawn over the border.
/// Two tests in that module hold it: `a_wide_glyph_cannot_reach_past_the_lease_into_the_border`
/// draws the chrome first and asserts the border cell survives, and
/// `a_frame_the_contract_admits_by_glyph_count_is_cut_at_the_lease` pins the
/// seam between the two counts.
///
/// Keeping the `char` count is a recorded decision, with its costs and what
/// would reopen it: `doc:adr/0028-panel-cells-are-glyphs-in-the-contract`.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PanelText(String);

impl PanelText {
    /// Build a run of glyphs.
    ///
    /// # Errors
    ///
    /// [`PanelTextError::ControlCharacter`] for a control `char`, and
    /// [`PanelTextError::BidiControl`] for a bidi one. Each names the `char`
    /// and where it sits.
    pub fn new(text: impl Into<String>) -> Result<Self, PanelTextError> {
        let text = text.into();
        if let Some((index, found)) = first_control_character(&text) {
            return Err(PanelTextError::ControlCharacter {
                index,
                code: found as u32,
            });
        }
        if let Some((index, found)) = first_bidi_control(&text) {
            return Err(PanelTextError::BidiControl {
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

    /// How many glyphs these are — the count this contract calls cells. See
    /// the type docs for what it does and does not measure.
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
/// Token names, never an RGB triple. That keeps §2's two-metal rule and
/// §3.2's hue clamp true of a plugin's pixels, not just Stella's own. The
/// host resolves each name against the live theme, so a panel follows a
/// degraded sixteen-colour terminal (§3.5) with no idea one exists, and no
/// plugin can author the warm hue the clamp exists to refuse.
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
