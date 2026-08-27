//! The composer: the input-line model, paste-chip collapse (L-T3), and the
//! slash-command menu.
//!
//! A paste above a small line threshold never floods the input buffer or the
//! model context (the TS bug L-T3 fixed): it collapses to a
//! `[pasted: N lines]` chip in the composer while the **full payload stays
//! attached** to the pending message. On submit, chips expand back to their
//! full text — the model sees everything, the screen sees a chip.
//!
//! The slash menu is deliberately generic: it filters a caller-supplied
//! command list by the typed prefix, so `/help /clear /models /diff /files`
//! are an *input*, not a hard-coded set — the CLI owns the real command
//! vocabulary.
//!
//! [`handle_slash_popup_key`] is the one implementation of slash-popup key
//! handling, shared by every composer-driven surface (
//! the deck's [`crate::deck_ui`]) so a future fix to
//! selection clamping, Esc semantics, or completion behavior can't land on
//! one surface and drift from the other.
//!
//! ## Textarea semantics
//!
//! The live buffer is a real multi-line editor: a **modified** `⏎` (`⌘⏎`/`⌃⏎`,
//! or the universally-safe `⌥⏎`) inserts a line break that survives verbatim
//! into the submitted prompt, the cursor moves freely (arrows, Home/End,
//! `⌥[`/`⌥]` to the very start/end), and [`layout`] soft-wraps the content to
//! the viewport width so everything typed stays visible before submitting. A
//! **bare** `⏎` always submits (never blocks) — see [`classify_enter`].

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use stella_protocol::{Attachment, AttachmentKind};
use unicode_width::UnicodeWidthChar;

pub mod args;
pub mod fuzzy;
pub mod palette;
pub mod recent;

pub use palette::{PaletteState, RelevantNow, SlashDomain};
pub use recent::Recents;

/// Below this many lines a paste is inserted inline; at or above it, the
/// paste collapses to a chip. Small on purpose (L-T3).
pub const DEFAULT_PASTE_LINE_THRESHOLD: usize = 6;

/// The deck composer's paste threshold. The deck's input box grows to a few
/// lines and then *scrolls* (see `DECK_COMPOSER_MAX_ROWS`), so a normal
/// multi-line prompt should render inline — one `>>>` per line — rather than
/// collapse to a chip; only a genuinely huge blob (a whole file) is worth
/// chipping to protect the model context. This sits well past the visible cap.
pub const DECK_PASTE_LINE_THRESHOLD: usize = 48;

/// One piece of composer content: typed text, a collapsed paste, or a
/// multimodal attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerEntry {
    /// Literal typed text.
    Text(String),
    /// A collapsed paste. `full_text` is what the model receives; the display
    /// shows `[pasted: line_count lines]`.
    Chip {
        full_text: String,
        line_count: usize,
    },
    /// A multimodal attachment (pasted image, attached media/document file).
    /// Displays as a chip; the payload rides the submission's attachment
    /// list, not its text.
    Attachment(Attachment),
}

impl ComposerEntry {
    /// The on-screen representation — the full payload never renders raw
    /// (L-T3).
    pub fn display(&self) -> String {
        match self {
            ComposerEntry::Text(t) => t.clone(),
            ComposerEntry::Chip { line_count, .. } => format!("[pasted: {line_count} lines]"),
            ComposerEntry::Attachment(att) => {
                let noun = match att.kind() {
                    AttachmentKind::Image => "image",
                    AttachmentKind::Audio => "audio",
                    AttachmentKind::Video => "video",
                    AttachmentKind::Pdf => "pdf",
                    AttachmentKind::Text => "file",
                    AttachmentKind::Binary => "file",
                };
                format!("[{noun}: {}]", att.label())
            }
        }
    }

    /// The text the model receives — paste chips expand to their full
    /// payload; attachments contribute no text (their payload rides the
    /// attachment list).
    pub fn expanded(&self) -> &str {
        match self {
            ComposerEntry::Text(t) => t,
            ComposerEntry::Chip { full_text, .. } => full_text,
            ComposerEntry::Attachment(_) => "",
        }
    }
}

/// A completed composer submission: the expanded prompt text plus any
/// attachments collected while composing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    pub text: String,
    pub attachments: Vec<Attachment>,
}

/// Where a slash command comes from — decides the glyph the menu row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SlashKind {
    /// Productized: shipped by stella itself (🔒).
    #[default]
    Builtin,
    /// Custom: a user-authored command/skill definition loaded from the
    /// workspace or user-global extension directories (⚡).
    Custom,
}

impl SlashKind {
    /// The menu-row glyph: 🔒 for productized commands, ⚡ for custom ones.
    pub fn glyph(self) -> &'static str {
        match self {
            SlashKind::Builtin => "🔒",
            SlashKind::Custom => "⚡",
        }
    }
}

/// A single slash command offered by the menu. The `name` includes the
/// leading slash (e.g. `"/help"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: String,
    pub description: String,
    pub kind: SlashKind,
    /// What the command is about — the palette's browse groups (#4338).
    /// Defaults to [`SlashDomain::Session`], so a caller that has not
    /// classified its vocabulary gets one group rather than a wrong one.
    pub domain: SlashDomain,
    /// Whether a submit of this command runs BESIDE the prompt queue
    /// ([`crate::envelope::WorkspaceInput::Command`]) instead of riding it:
    /// it executes at once — mid-turn included — and never appears as a
    /// queued prompt. Declared by the caller (the CLI knows which of its
    /// commands touch the turn); defaults to `false`, the queueing behavior
    /// every command had before this flag existed.
    pub sideband: bool,
}

impl SlashCommand {
    /// A productized (built-in) command — the 🔒 rows.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            kind: SlashKind::Builtin,
            domain: SlashDomain::default(),
            sideband: false,
        }
    }

    /// The same command, filed under `domain`.
    #[must_use]
    pub fn in_domain(self, domain: SlashDomain) -> Self {
        Self { domain, ..self }
    }

    /// The same command, declared queue-free — see [`Self::sideband`].
    #[must_use]
    pub fn sideband(self) -> Self {
        Self {
            sideband: true,
            ..self
        }
    }

    /// A custom command/skill loaded from a definition file — the ⚡ rows.
    pub fn custom(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            kind: SlashKind::Custom,
            domain: SlashDomain::Custom,
            ..Self::new(name, description)
        }
    }
}

/// The input model. Committed paste chips precede the live text buffer;
/// what the user is currently typing lives in `buffer`, a multi-line
/// textarea with a movable cursor.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Composer {
    /// Chips committed ahead of the live buffer, in order.
    chips: Vec<ComposerEntry>,
    /// The text currently being typed. May contain `\n` — line breaks are
    /// preserved verbatim through [`Composer::take_submission`].
    buffer: String,
    /// Byte offset of the cursor within `buffer` (always on a char boundary).
    cursor: usize,
    /// Paste-collapse threshold in lines.
    paste_threshold: usize,
    /// Commands run from this composer's slash popup, most recent first —
    /// SPEC 10's `recent` section. It rides the composer because the composer
    /// already owns the slash-menu view ([`Composer::slash_menu`]), so the
    /// key handler and the renderer read one list without either surface
    /// having to pass it between them. A surface that hands it no path keeps
    /// an in-session list; see [`recent::Recents`].
    recent: Recents,
}

impl Composer {
    /// A composer with the default paste threshold.
    pub fn new() -> Self {
        Self {
            paste_threshold: DEFAULT_PASTE_LINE_THRESHOLD,
            ..Self::default()
        }
    }

    /// A composer with an explicit paste threshold.
    pub fn with_paste_threshold(threshold: usize) -> Self {
        Self {
            paste_threshold: threshold.max(1),
            ..Self::default()
        }
    }

    /// Keep the `recent` section in `path` from now on, seeded with what is
    /// already there. Called once by the surface that knows which workspace
    /// this composer belongs to (`DeckOptions::recent_path`).
    pub fn keep_recent_in(&mut self, path: impl Into<std::path::PathBuf>) {
        self.recent = Recents::kept_in(path);
    }

    /// The commands this composer has run, most recent first.
    pub fn recent(&self) -> &[String] {
        self.recent.names()
    }

    /// Write the `recent` list back if a dispatch has changed it. Cheap
    /// enough to call on every keystroke — it returns at once when nothing
    /// has moved.
    pub fn flush_recent(&mut self) {
        self.recent.flush();
    }

    /// The live buffer text (what is being typed, chips excluded).
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    /// The committed chips ahead of the buffer.
    pub fn chips(&self) -> &[ComposerEntry] {
        &self.chips
    }

    /// True when there is nothing to submit.
    pub fn is_empty(&self) -> bool {
        self.chips.is_empty() && self.buffer.trim().is_empty()
    }

    /// True when there is nothing at all to edit — no chips and not even
    /// whitespace in the buffer (stricter than [`Composer::is_empty`], which
    /// is about submittability).
    pub fn is_blank(&self) -> bool {
        self.chips.is_empty() && self.buffer.is_empty()
    }

    /// Byte offset of the cursor within [`Composer::buffer`].
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Type one character at the cursor.
    pub fn insert_char(&mut self, c: char) {
        self.buffer.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Insert a line break at the cursor — the modified-`⏎` textarea action
    /// (`⌘⏎`/`⌃⏎`/`⌥⏎`; a bare `⏎` submits instead).
    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    /// Delete the character before the cursor; at the very start of the
    /// buffer, pop the last chip instead (backspacing off the front of the
    /// buffer removes a paste).
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let prev = prev_char_start(&self.buffer, self.cursor);
            self.buffer.replace_range(prev..self.cursor, "");
            self.cursor = prev;
        } else {
            self.chips.pop();
        }
    }

    /// Handle a paste at the cursor. A paste at or above the line threshold
    /// collapses to a chip (the full payload retained); a small paste inserts
    /// inline. Terminal paste streams carry `\r`/`\r\n` line endings in raw
    /// mode — normalized to `\n` so the buffer has one newline convention.
    pub fn paste(&mut self, pasted: &str) {
        let pasted = pasted.replace("\r\n", "\n").replace('\r', "\n");
        let line_count = line_count(&pasted);
        if line_count >= self.paste_threshold {
            // Text before the cursor is committed ahead of the chip so
            // ordering (typed text, then chip) is preserved on submit; text
            // after the cursor stays in the buffer, which follows the chips.
            let before = self.buffer[..self.cursor].to_string();
            let after = self.buffer[self.cursor..].to_string();
            if !before.is_empty() {
                self.chips.push(ComposerEntry::Text(before));
            }
            self.chips.push(ComposerEntry::Chip {
                full_text: pasted,
                line_count,
            });
            self.buffer = after;
            self.cursor = 0;
        } else {
            self.buffer.insert_str(self.cursor, &pasted);
            self.cursor += pasted.len();
        }
    }

    // Cursor motion (textarea semantics)

    /// One character left.
    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = prev_char_start(&self.buffer, self.cursor);
        }
    }

    /// One character right.
    pub fn move_right(&mut self) {
        if let Some(c) = self.buffer[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    /// To the very start of the prompt — the `⌥[` jump (position 0, before
    /// the first character).
    pub fn move_to_start(&mut self) {
        self.cursor = 0;
    }

    /// To one past the last character — the `⌥]` jump.
    pub fn move_to_end(&mut self) {
        self.cursor = self.buffer.len();
    }

    /// To the start of the current logical line.
    pub fn move_line_start(&mut self) {
        self.cursor = line_start(&self.buffer, self.cursor);
    }

    /// To the end of the current logical line (before its `\n`).
    pub fn move_line_end(&mut self) {
        self.cursor = self.buffer[self.cursor..]
            .find('\n')
            .map(|i| self.cursor + i)
            .unwrap_or(self.buffer.len());
    }

    /// Up one logical line, keeping the character column where possible.
    /// On the first line, jumps to the start (matching most editors' clamp).
    pub fn move_up(&mut self) {
        let start = line_start(&self.buffer, self.cursor);
        if start == 0 {
            self.cursor = 0;
            return;
        }
        let col = self.buffer[start..self.cursor].chars().count();
        let prev_start = line_start(&self.buffer, start - 1);
        let prev_line = &self.buffer[prev_start..start - 1];
        self.cursor = prev_start + byte_at_char_col(prev_line, col);
    }

    /// Down one logical line, keeping the character column where possible.
    /// On the last line, jumps to the end.
    pub fn move_down(&mut self) {
        let Some(newline) = self.buffer[self.cursor..].find('\n') else {
            self.cursor = self.buffer.len();
            return;
        };
        let start = line_start(&self.buffer, self.cursor);
        let col = self.buffer[start..self.cursor].chars().count();
        let next_start = self.cursor + newline + 1;
        let next_end = self.buffer[next_start..]
            .find('\n')
            .map(|i| next_start + i)
            .unwrap_or(self.buffer.len());
        let next_line = &self.buffer[next_start..next_end];
        self.cursor = next_start + byte_at_char_col(next_line, col);
    }

    /// Attach a multimodal input as a chip ahead of the live buffer. Text
    /// typed before the attach point is committed first so ordering is
    /// preserved on submit, mirroring [`Composer::paste`]'s chip path.
    pub fn attach(&mut self, attachment: Attachment) {
        let before = self.buffer[..self.cursor].to_string();
        let after = self.buffer[self.cursor..].to_string();
        if !before.is_empty() {
            self.chips.push(ComposerEntry::Text(before));
        }
        self.chips.push(ComposerEntry::Attachment(attachment));
        self.buffer = after;
        self.cursor = 0;
    }

    /// The attachments currently pending in the composer, in order.
    pub fn attachments(&self) -> impl Iterator<Item = &Attachment> {
        self.chips.iter().filter_map(|chip| match chip {
            ComposerEntry::Attachment(att) => Some(att),
            _ => None,
        })
    }

    /// Assemble the full message the model receives — chips expanded to their
    /// payloads, typed line breaks preserved verbatim, attachments collected
    /// alongside — and clear the composer. Returns `None` when empty.
    pub fn take_submission(&mut self) -> Option<Submission> {
        if self.is_empty() {
            return None;
        }
        let attachments: Vec<Attachment> = self.attachments().cloned().collect();
        let mut parts: Vec<String> = self
            .chips
            .iter()
            .map(|c| c.expanded().to_string())
            .filter(|part| !part.is_empty())
            .collect();
        if !self.buffer.is_empty() {
            parts.push(std::mem::take(&mut self.buffer));
        }
        self.chips.clear();
        self.cursor = 0;
        Some(Submission {
            text: parts.join("\n"),
            attachments,
        })
    }

    /// Clear the composer without submitting.
    pub fn clear(&mut self) {
        self.chips.clear();
        self.buffer.clear();
        self.cursor = 0;
    }

    /// Replace the composer's content with `text` — the queue editor uses
    /// this to pull a queued prompt back in for editing. Any in-progress
    /// chips/typing are discarded (the caller decides when that is right).
    /// The cursor lands at the end, ready to keep typing.
    pub fn load(&mut self, text: impl Into<String>) {
        self.chips.clear();
        self.buffer = text.into();
        self.cursor = self.buffer.len();
    }

    /// The slash-menu view over `commands`, or `None` when the buffer is not
    /// a slash query. Active only when the whole buffer is a single `/`-word
    /// (no spaces yet) with no committed chips.
    ///
    /// `state` orders the result ([`SlashMenu::filter_with`]). Pass
    /// [`PaletteState::default`] from a surface with no session to read.
    pub fn slash_menu<'a>(
        &self,
        commands: &'a [SlashCommand],
        state: &PaletteState,
    ) -> Option<SlashMenu<'a>> {
        if !self.chips.is_empty() {
            return None;
        }
        let q = self.buffer.as_str();
        if !q.starts_with('/') || q.contains(char::is_whitespace) {
            return None;
        }
        Some(SlashMenu::filter_with(commands, q, state, self.recent()))
    }
}

/// Byte index of the char boundary immediately before `idx`.
fn prev_char_start(s: &str, idx: usize) -> usize {
    s[..idx].char_indices().last().map(|(i, _)| i).unwrap_or(0)
}

/// Byte index where the logical line containing `idx` starts.
fn line_start(s: &str, idx: usize) -> usize {
    s[..idx].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

/// Byte offset of character column `col` within `line`, clamped to its end.
fn byte_at_char_col(line: &str, col: usize) -> usize {
    line.char_indices()
        .nth(col)
        .map(|(i, _)| i)
        .unwrap_or(line.len())
}

// Enter classification + shared textarea key handling

/// What an `⏎` keypress means for the composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnterAction {
    /// Dispatch the composer's content.
    Submit,
    /// Insert a line break at the cursor.
    Newline,
    /// The key is not Enter at all.
    NotEnter,
}

/// Classify an Enter keypress for a textarea composer.
///
/// One rule, honest across every terminal: a **bare** `⏎` submits, and `⏎` with
/// any newline modifier — `⌘⏎`/`⌃⏎` (macOS Cmd reports as SUPER/META) or `⌥⏎` —
/// inserts a line break. With the kitty keyboard protocol all three modifiers
/// are reportable; on a legacy terminal only `⌥⏎` survives (its ESC prefix
/// does), and an unreportable `⌘⏎`/`⌃⏎` harmlessly folds into a plain `⏎` and
/// submits — the best a legacy terminal can do. This is the inverse of the old
/// chord-to-submit mapping: Enter now always dispatches, so the queue is one
/// keystroke away and never blocks.
pub fn classify_enter(key: &KeyEvent) -> EnterAction {
    if !matches!(key.code, KeyCode::Enter) {
        return EnterAction::NotEnter;
    }
    let newline = key.modifiers.intersects(
        KeyModifiers::SUPER | KeyModifiers::META | KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    if newline {
        EnterAction::Newline
    } else {
        EnterAction::Submit
    }
}

/// Textarea cursor-motion keys shared by every composer surface. Returns
/// `true` when the key was consumed. Motion that would collide with a
/// surface's own navigation (transcript scroll, tab views) is gated on the
/// buffer actually having something to move through: ←/→ need text, ↑/↓ need
/// a second line — so an empty composer leaves every arrow to its surface.
pub fn handle_edit_key(key: KeyEvent, composer: &mut Composer) -> bool {
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let cmd = key
        .modifiers
        .intersects(KeyModifiers::SUPER | KeyModifiers::META);
    let has_text = !composer.buffer().is_empty();
    let multiline = composer.buffer().contains('\n');
    match key.code {
        // ⌥[ / ⌥] — cursor (and the wrapped view with it) to the very start /
        // one past the last character.
        KeyCode::Char('[') if alt => composer.move_to_start(),
        KeyCode::Char(']') if alt => composer.move_to_end(),
        // ⌘↑ / ⌘↓ — the macOS-native start/end-of-document synonyms.
        KeyCode::Up if cmd => composer.move_to_start(),
        KeyCode::Down if cmd => composer.move_to_end(),
        KeyCode::Left if has_text => composer.move_left(),
        KeyCode::Right if has_text => composer.move_right(),
        KeyCode::Up if multiline => composer.move_up(),
        KeyCode::Down if multiline => composer.move_down(),
        KeyCode::Home if has_text => composer.move_line_start(),
        KeyCode::End if has_text => composer.move_line_end(),
        _ => return false,
    }
    true
}

// Soft-wrap layout (pure, so both renderers and the tests share one truth)

/// The composer soft-wrapped to a viewport width: every visual row plus the
/// cursor's position among them. Hard breaks (`\n`) and soft wraps both
/// produce rows, so `rows.len()` is the height the composer wants and the
/// caller can scroll a capped window to `cursor_row`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerLayout {
    /// The wrapped display rows (chips rendered as their `[pasted: …]` form).
    pub rows: Vec<String>,
    /// Row index the cursor sits on.
    pub cursor_row: usize,
    /// Display-width column of the cursor within `rows[cursor_row]`.
    pub cursor_col: usize,
}

/// Soft-wrap the composer's display content to `width` columns
/// (unicode-width aware; `\n` is a hard break) and locate the cursor.
pub fn layout(composer: &Composer, width: usize) -> ComposerLayout {
    let width = width.max(1);
    let mut display = String::new();
    for chip in &composer.chips {
        display.push_str(&chip.display());
        display.push(' ');
    }
    let cursor_at = display.len() + composer.cursor();
    display.push_str(composer.buffer());

    let mut rows: Vec<String> = vec![String::new()];
    let mut col = 0usize;
    let (mut cursor_row, mut cursor_col) = (0usize, 0usize);
    for (idx, ch) in display.char_indices() {
        if ch == '\n' {
            // A cursor on the newline itself renders at this row's end.
            if idx == cursor_at {
                (cursor_row, cursor_col) = (rows.len() - 1, col);
            }
            rows.push(String::new());
            col = 0;
            continue;
        }
        let w = ch.width().unwrap_or(0);
        if col + w > width && col > 0 {
            rows.push(String::new());
            col = 0;
        }
        if idx == cursor_at {
            (cursor_row, cursor_col) = (rows.len() - 1, col);
        }
        rows.last_mut().expect("rows is never empty").push(ch);
        col += w;
    }
    if cursor_at == display.len() {
        // Cursor past the last character; if that row is exactly full the
        // insertion point visually lives on a fresh row.
        if col >= width {
            rows.push(String::new());
            col = 0;
        }
        (cursor_row, cursor_col) = (rows.len() - 1, col);
    }
    ComposerLayout {
        rows,
        cursor_row,
        cursor_col,
    }
}

/// Split one display row at `col` display columns for block-cursor drawing:
/// `(before, under, after)`, where `under` is the character the cursor sits
/// on (`None` at end of row — the caller draws a reversed space).
pub fn split_row_at(row: &str, col: usize) -> (String, Option<char>, String) {
    let mut acc = 0usize;
    let mut chars = row.chars();
    let mut before = String::new();
    for ch in chars.by_ref() {
        if acc >= col {
            return (before, Some(ch), chars.collect());
        }
        acc += ch.width().unwrap_or(0);
        before.push(ch);
    }
    (before, None, String::new())
}

/// One palette row: the command, and where the query lit up inside its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashMatch<'a> {
    pub command: &'a SlashCommand,
    /// Char offsets into [`SlashCommand::name`], the leading `/` counted, so
    /// the renderer walks the string it prints instead of re-deriving a bare
    /// slug from it. Empty when the query matched only the description, and
    /// for the browse list's `recent` rows.
    pub highlights: Vec<usize>,
}

/// The filtered slash-command list for the current query. Borrows the
/// caller's command vocabulary — the menu owns no command list of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashMenu<'a> {
    pub query: String,
    pub matches: Vec<SlashMatch<'a>>,
    /// Headings the palette draws above [`Self::matches`], as
    /// `(index of the first match under it, heading)`. Ascending, and only
    /// ever populated for the browse list — see [`Self::filter_with`].
    pub sections: Vec<(usize, String)>,
}

impl<'a> SlashMenu<'a> {
    /// [`Self::filter_with`] against a session with nothing to say.
    ///
    /// The plain REPL's composer has no plan, no lanes and no inbox to read,
    /// so it gets the ranking it always had rather than a relevance block
    /// derived from zeroes.
    pub fn filter(commands: &'a [SlashCommand], query: &str) -> Self {
        Self::filter_with(commands, query, &PaletteState::default(), &[])
    }

    /// Fuzzy filter over `commands`, ordered by what the session is doing.
    ///
    /// Matching decides *what appears* and where each row lights up
    /// ([`fuzzy::match_name`]): a name-prefix match ranks first, a
    /// name-substring match second, a name-subsequence match third, and a
    /// description-substring match last. An empty query (just `/`) matches
    /// everything and lights nothing. Case-insensitive on the ASCII fold;
    /// command names are ASCII slugs, so the cheap fold is exact for every
    /// name the CLI actually registers.
    ///
    /// `state` decides *the order*, and the two cases are deliberately
    /// different surfaces rather than one compromise (#4338):
    ///
    /// - **The browse list** (an empty query — the palette just opened) is
    ///   sectioned: [`palette::relevant_now`]'s commands first under one
    ///   heading that says why, then a group per [`SlashDomain`]. Thirty
    ///   rows in vocabulary order is a list you read; six groups is a menu
    ///   you use.
    /// - **A typed query** stays one flat ranked list with no headings —
    ///   grouping a three-row result buries the rows under their own
    ///   captions — but a relevant command still leads *within its rank*, so
    ///   `/pl` mid-turn opens on `/plan`.
    ///
    /// `recent` closes the browse list under its own heading — the commands
    /// this workspace ran last, most recent first. A recent row is a second
    /// appearance of a command a domain group already lists, which is what a
    /// shortcut is, so it is appended rather than lifted out of its group
    /// (SPEC 10 puts the section last).
    pub fn filter_with(
        commands: &'a [SlashCommand],
        query: &str,
        state: &PaletteState,
        recent: &[String],
    ) -> Self {
        let needle = query.trim_start_matches('/').to_ascii_lowercase();
        let matched = |c: &'a SlashCommand| -> Option<(u8, SlashMatch<'a>)> {
            let bare = c.name.trim_start_matches('/');
            // What the slash cost, so the offsets index the printed name.
            let slash = c.name.chars().count() - bare.chars().count();
            match fuzzy::match_name(&bare.to_ascii_lowercase(), &needle) {
                Some(m) => Some((
                    m.kind.rank(),
                    m.indices.iter().map(|i| i + slash).collect::<Vec<_>>(),
                )),
                None => c
                    .description
                    .to_ascii_lowercase()
                    .contains(&needle)
                    .then(|| (fuzzy::DESCRIPTION_RANK, Vec::new())),
            }
            .map(|(rank, highlights)| {
                (
                    rank,
                    SlashMatch {
                        command: c,
                        highlights,
                    },
                )
            })
        };
        let relevant = palette::relevant_now(state);
        // Where a command sits in the relevance block, or past every one of
        // them. `usize::MAX` rather than an `Option` so it sorts last with
        // no second comparison.
        let relevance = |c: &SlashCommand| -> usize {
            relevant
                .as_ref()
                .and_then(|r| r.commands.iter().position(|n| *n == c.name))
                .unwrap_or(usize::MAX)
        };

        let mut ranked: Vec<(u8, SlashMatch<'a>)> = commands.iter().filter_map(matched).collect();

        if !needle.is_empty() {
            // Stable within a key, so the vocabulary order survives among
            // commands the session says nothing about.
            ranked.sort_by_key(|(r, m)| (*r, relevance(m.command)));
            return Self {
                query: query.to_string(),
                matches: ranked.into_iter().map(|(_, m)| m).collect(),
                sections: Vec::new(),
            };
        }

        // The browse list: relevance block, then a group per domain.
        ranked.sort_by_key(|(_, m)| (relevance(m.command), m.command.domain.order()));
        let mut matches: Vec<SlashMatch<'a>> = ranked.into_iter().map(|(_, m)| m).collect();

        let mut sections = Vec::new();
        let promoted = relevant.as_ref().map_or(0, |r| {
            matches
                .iter()
                .filter(|m| r.commands.iter().any(|n| *n == m.command.name))
                .count()
        });
        if let Some(relevant) = relevant.as_ref()
            && promoted > 0
        {
            sections.push((0, format!("relevant now · {}", relevant.reason)));
        }
        let mut group = None;
        for (i, m) in matches.iter().enumerate().skip(promoted) {
            if group != Some(m.command.domain) {
                group = Some(m.command.domain);
                sections.push((i, m.command.domain.label().to_string()));
            }
        }
        // `recent` closes the list. A name the vocabulary no longer answers to
        // is skipped rather than drawn: the file outlives any one build's
        // command set.
        let recent_rows: Vec<SlashMatch<'a>> = recent
            .iter()
            .filter_map(|name| commands.iter().find(|c| c.name == *name))
            .map(|command| SlashMatch {
                command,
                highlights: Vec::new(),
            })
            .collect();
        if !recent_rows.is_empty() {
            sections.push((matches.len(), "recent".to_string()));
            matches.extend(recent_rows);
        }
        Self {
            query: query.to_string(),
            matches,
            sections,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }
}

/// The names of the slash commands currently matching the composer, or empty
/// when the popup should be inactive. Owned strings so a caller can keep
/// mutating its own UI state while acting on them.
///
/// `state` must be the same one the frame is drawn with: this list is what
/// the selection index means, so a key handler ordering it differently from
/// the renderer would run the row *above* the one highlighted.
pub fn slash_popup_matches(
    composer: &Composer,
    slash_commands: &[SlashCommand],
    state: &PaletteState,
) -> Vec<String> {
    composer
        .slash_menu(slash_commands, state)
        .map(|m| m.matches.iter().map(|m| m.command.name.clone()).collect())
        .unwrap_or_default()
}

/// What a slash-popup key press should do, abstracted over the caller's own
/// action type — a REPL `Prompt` and a deck `Enqueue` both start from the
/// same submitted text.
pub enum SlashPopupOutcome {
    /// Navigation, completion, or dismiss — fully handled here.
    Handled,
    /// Enter: dispatch this text as a prompt.
    Submit(String),
}

/// Slash-popup navigation shared by every composer-driven surface: ↑/↓
/// choose, Tab completes into the buffer, Enter dispatches the selection,
/// Esc dismisses. Returns `None` for a key the popup doesn't claim, so the
/// caller can fall through to normal composer editing. `matches` must be
/// non-empty — callers only reach this once the popup is confirmed active; an
/// empty slice trips a `debug_assert!` and then claims nothing, so a caller
/// bug degrades to "the popup isn't open" rather than a panic mid-keystroke.
pub fn handle_slash_popup_key(
    key: KeyEvent,
    matches: &[String],
    composer: &mut Composer,
    slash_selected: &mut usize,
) -> Option<SlashPopupOutcome> {
    // The `- 1`s below (and the `matches[selected]` indexing) rest on this.
    // Every caller gates on `slash_popup_matches(..)` being non-empty, so a
    // violation is a caller bug worth surfacing in dev rather than a release
    // panic inside the key handler.
    debug_assert!(
        !matches.is_empty(),
        "handle_slash_popup_key called with no matches — the popup is inactive"
    );
    if matches.is_empty() {
        return None;
    }
    let selected = (*slash_selected).min(matches.len() - 1);
    match key.code {
        KeyCode::Up => {
            *slash_selected = selected.saturating_sub(1);
            Some(SlashPopupOutcome::Handled)
        }
        KeyCode::Down => {
            *slash_selected = (selected + 1).min(matches.len() - 1);
            Some(SlashPopupOutcome::Handled)
        }
        KeyCode::Tab => {
            composer.load(matches[selected].clone());
            *slash_selected = 0;
            Some(SlashPopupOutcome::Handled)
        }
        KeyCode::Enter => {
            let chosen = matches[selected].clone();
            // The one place a palette row is *run*, which is what the `recent`
            // section reports. Tab completes instead of running, so it records
            // nothing.
            composer.recent.record(&chosen);
            composer.clear();
            *slash_selected = 0;
            Some(SlashPopupOutcome::Submit(chosen))
        }
        KeyCode::Esc => {
            composer.clear();
            *slash_selected = 0;
            Some(SlashPopupOutcome::Handled)
        }
        _ => None,
    }
}

/// Count the lines in a pasted payload — the metric the chip threshold uses.
/// A trailing newline does not add a phantom empty line.
fn line_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    text.trim_end_matches('\n').split('\n').count()
}

#[cfg(test)]
mod tests;
