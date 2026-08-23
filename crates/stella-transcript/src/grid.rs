//! The character-grid renderer — TUI parity for the same model.
//!
//! Emits styled cells rather than a terminal's escape sequences, and encodes at
//! the very end. That seam is what makes this testable: a golden test asserts
//! the *plain* grid, so a snapshot diff is readable by a human, while the ANSI
//! encoders are checked separately and independently.
//!
//! ## Why the gutters are constants
//!
//! Every width below is a `const`, and every row is assembled by padding to
//! those constants. A TUI gets fixed-width columns for free only if nobody
//! writes a variable-width prefix; the moment a `▸` is one cell and a `▾` is
//! two on some font, the whole column walks. So the fold marker is asserted to
//! be one cell wide and the row builder pads from the left edge every time,
//! never by appending to what the previous branch happened to emit.
//!
//! That only holds if "width" means what the terminal means by it. A CJK
//! ideograph or an emoji occupies two columns and one `char`, so every measure
//! here goes through [`cells`] — never `chars().count()`, which under-counts
//! exactly the text a fixed column is least able to absorb (#3740).
//!
//! ## The width contract
//!
//! [`render`] never returns a row wider than the `width` it was given, for any
//! [`Run`]. Fixed columns are worth nothing if a row can still run past the
//! right edge: the terminal then decides what happens to the excess, and it
//! either soft-wraps — shifting every row below and walking the very columns
//! this module pads to hold — or truncates, silently. Either way the frame the
//! rails draw stops being a rectangle.
//!
//! Three kinds of row keep it, in this order of preference (`digest.rs`'s rule
//! that a digest is a summary rather than cut content, applied here):
//!
//! - **A digest row elides at a named boundary.** A turn name, a prose head, a
//!   note summary and a step's object are cut by [`digest::elide`] or by this
//!   module's own `head_cells` against what the row has left, so the rails and
//!   chips they sit beside stay where the constants put them.
//! - **A framed row reserves before it fills.** Every `─` fill is
//!   `width - used` with no floor: a fill that floors at one cell pushes a row
//!   that exactly fits one cell past the edge (#3769).
//! - **A content row is cut by `clamp_line`.** A result body line, a diff row
//!   or an argument value is as wide as the tool made it, and there is no
//!   named boundary inside it to prefer. It is cut at a cell boundary, last,
//!   after every digest row has already fitted itself.

use crate::digest::{self, Chip};
use crate::file_diff::{FileDiff, RowKind};
use crate::fold::FoldState;
use crate::model::{Call, FileStatus, NodeId, Run, Status, Step, Turn};
use crate::syntax;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Width of the elapsed-offset gutter, including its trailing space.
pub const OFFSET_W: usize = 5;
/// Width of the fold marker gutter.
pub const MARK_W: usize = 2;
/// Width of the tool-name column, so objects start at the same cell on every
/// row regardless of whether the tool is `bash` or `delete_file`.
pub const TOOL_W: usize = 12;
/// Width of the spine gutter (`│ `).
pub const SPINE_W: usize = 2;

/// A colour role. Mapped to a 256-colour index or a 16-colour ANSI code at
/// encode time, never held as a raw escape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// Body text.
    Ink,
    /// Secondary text.
    Dim,
    /// Structure — frames, spines, rules.
    Faint,
    /// bash and tool identity.
    Cyan,
    /// Success and additions.
    Green,
    /// Errors and deletions.
    Red,
    /// Warnings.
    Amber,
    /// Agent and turn identity.
    Violet,
    /// Hunk headers.
    Blue,
    /// Function and method names in highlighted source.
    ///
    /// The deck spends `theme::MAGENTA` here, and this is the entry that lets
    /// the grid say the same thing — `Tok::Function` arrived with the
    /// grammar-backed lexer (#4283) and had no hue in this palette's
    /// vocabulary.
    Magenta,
}

impl Color {
    /// The xterm-256 index.
    #[must_use]
    pub fn ansi256(self) -> u8 {
        match self {
            Color::Ink => 253,
            Color::Dim => 245,
            Color::Faint => 239,
            Color::Cyan => 44,
            Color::Green => 78,
            Color::Red => 203,
            Color::Amber => 179,
            Color::Violet => 141,
            Color::Blue => 111,
            // 168 is the cube entry `theme::MAGENTA` already falls back to.
            Color::Magenta => 168,
        }
    }

    /// The basic 16-colour foreground code.
    #[must_use]
    pub fn ansi16(self) -> u8 {
        match self {
            Color::Ink => 37,
            Color::Dim | Color::Faint => 90,
            Color::Cyan => 36,
            Color::Green => 32,
            Color::Red => 31,
            Color::Amber => 33,
            Color::Violet => 35,
            Color::Blue => 34,
            Color::Magenta => 35,
        }
    }
}

/// A background tint. The four diff tints are the only backgrounds this
/// renderer draws — a terminal with a busy background is unreadable, and the
/// tints exist to answer exactly one question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tint {
    /// No background.
    None,
    /// A removed line.
    DelLine,
    /// The changed tokens inside a removed line.
    DelWord,
    /// An added line.
    AddLine,
    /// The changed tokens inside an added line.
    AddWord,
    /// An inverse-video role badge.
    Badge(Color),
}

impl Tint {
    /// The xterm-256 background index, if any.
    #[must_use]
    pub fn ansi256(self) -> Option<u8> {
        Some(match self {
            Tint::None => return None,
            Tint::DelLine => 52,
            Tint::DelWord => 88,
            Tint::AddLine => 22,
            Tint::AddWord => 28,
            Tint::Badge(color) => color.ansi256(),
        })
    }
}

/// One styled run of text on the grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// The text.
    pub text: String,
    /// Foreground role.
    pub fg: Color,
    /// Background tint.
    pub tint: Tint,
    /// Whether the run is bold.
    pub bold: bool,
}

impl Cell {
    /// A plain run in the given colour.
    #[must_use]
    pub fn new(text: impl Into<String>, fg: Color) -> Self {
        Self {
            text: text.into(),
            fg,
            tint: Tint::None,
            bold: false,
        }
    }

    /// Make this run bold.
    #[must_use]
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// Give this run a background tint.
    #[must_use]
    pub fn tinted(mut self, tint: Tint) -> Self {
        self.tint = tint;
        self
    }

    /// Display width in terminal cells.
    #[must_use]
    pub fn width(&self) -> usize {
        cells(&self.text)
    }
}

/// One rendered row.
pub type Line = Vec<Cell>;

/// Total display width of a row.
#[must_use]
pub fn line_width(line: &Line) -> usize {
    line.iter().map(Cell::width).sum()
}

/// How many terminal columns `text` occupies.
///
/// The one measure in this module. A `char` is not a column: CJK and emoji take
/// two, combining marks take none, and a row padded by character count is a row
/// whose columns move as soon as either appears.
#[must_use]
pub fn cells(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Everything a row builder needs that is the same for every row.
///
/// Bundled rather than threaded as three parameters through eight functions:
/// the alternative grew a nine-argument signature whose call sites were
/// impossible to read, and silencing the lint would have hidden the fact that
/// the three always travel together.
struct Ctx<'a> {
    run: &'a Run,
    state: &'a FoldState,
    width: usize,
}

impl Ctx<'_> {
    /// Whether a node renders expanded.
    fn open(&self, node: NodeId) -> bool {
        self.state.is_open(self.run, node)
    }
}

/// Render the whole run to a character grid `width` columns wide.
#[must_use]
pub fn render(run: &Run, state: &FoldState, width: usize) -> Vec<Line> {
    let ctx = Ctx { run, state, width };
    let mut lines = Vec::new();
    for (index, turn) in run.turns.iter().enumerate() {
        render_turn(&mut lines, &ctx, turn, index);
    }
    lines.push(status_line(run, state, width));
    lines
        .into_iter()
        .map(|line| clamp_line(line, width))
        .collect()
}

/// One turn's frame, top rail to bottom rail, with no whole-run footer.
///
/// # The streaming contract
///
/// Every line this returns is **final the moment it is produced**, and the
/// sequence only ever grows at the tail as the turn does. That is what lets an
/// append-only surface — a terminal's scrollback, a log file — print a turn as
/// it happens by re-rendering and emitting the suffix it has not printed yet,
/// instead of inventing a second, terser rendering to show in the meantime
/// (`stella-cli`'s `plain::transcript` does exactly this).
///
/// The contract is why the turn's status and accounting sit on the **bottom**
/// rail rather than the top: neither exists when the first line has to be
/// printed, and a top rail that claimed them would freeze `running` and a
/// partial cost into scrollback forever. It is a real constraint on this
/// function, not a layout preference — moving either back onto
/// `turn_frame_top` makes the frame unstreamable, and
/// `render_turn_is_append_only_as_a_turn_grows` fails.
#[must_use]
pub fn render_turn_lines(run: &Run, state: &FoldState, index: usize, width: usize) -> Vec<Line> {
    let ctx = Ctx { run, state, width };
    let mut lines = Vec::new();
    if let Some(turn) = run.turns.get(index) {
        render_turn(&mut lines, &ctx, turn, index);
    }
    lines
        .into_iter()
        .map(|line| clamp_line(line, width))
        .collect()
}

fn render_turn(out: &mut Vec<Line>, ctx: &Ctx<'_>, turn: &Turn, index: usize) {
    let width = ctx.width;
    let open = ctx.open(NodeId::Turn(index));
    out.push(turn_frame_top(turn, open, width));
    if !open {
        out.push(vec![
            Cell::new("│ ", Color::Faint),
            Cell::new("…", Color::Dim),
        ]);
        out.push(turn_frame_bottom(turn, width));
        return;
    }

    role_lines(out, "you", Color::Cyan, &turn.prompt, width);
    out.push(spine_blank());

    let offsets = digest::offsets(&turn.steps);
    for (si, step) in turn.steps.iter().enumerate() {
        for (ni, note) in turn.notes.iter().enumerate() {
            if note.before_step == si {
                note_lines(out, ctx, note, index, ni);
            }
        }
        for (pi, prose) in turn.prose.iter().enumerate() {
            if prose.before_step == si {
                prose_lines(out, ctx, prose, index, pi);
            }
        }
        step_lines(out, ctx, step, index, si, &offsets[si]);
    }
    // Prose the step loop cannot reach: reasoning emitted after the last step,
    // and — the case that has no step at all — a turn that produced only prose.
    // Mirrors the trailing pass in `steps_and_prose` in html.rs; without it the
    // two renderers disagree about which prose exists.
    for (ni, note) in turn.notes.iter().enumerate() {
        if note.before_step >= turn.steps.len() {
            note_lines(out, ctx, note, index, ni);
        }
    }
    for (pi, prose) in turn.prose.iter().enumerate() {
        if prose.before_step >= turn.steps.len() {
            prose_lines(out, ctx, prose, index, pi);
        }
    }

    if let Some(answer) = &turn.answer {
        out.push(spine_blank());
        role_lines(out, "agent", Color::Violet, answer, width);
    }
    out.push(turn_frame_bottom(turn, width));
}

/// The turn's opening rail: everything about a turn that is knowable before it
/// runs, and nothing that is not.
///
/// Deliberately carries no status and no chips. See [`render_turn_lines`]'s
/// streaming contract — this is the line a live surface has to print first,
/// when the only true answers to "how did it go" and "what did it cost" are
/// "unknown".
fn turn_frame_top(turn: &Turn, open: bool, width: usize) -> Line {
    let dig = digest::turn_digest(turn, 40, digest::ChipStyle::Tight);
    // The `╭─ ` opener, the space after the name, and the ` ─╮` cap are
    // reserved before anything variable is measured: a top rail that reserves
    // less than it emits is longer than the bottom one it has to meet, and a
    // turn name is a branch name or a goal slug — as long as whoever named it
    // made it.
    let mut budget = width.saturating_sub(3 + 1 + 3);
    let (name, name_w) = head_cells(&turn.name, budget);
    budget -= name_w;
    let mut line = vec![
        Cell::new("╭─ ", Color::Faint),
        Cell::new(name, Color::Violet).bold(),
        Cell::new(" ", Color::Faint),
    ];
    // An expanded turn shows its content in full, so a fold marker on its own
    // header would be an affordance for a click that does nothing new. Only a
    // collapsed turn — whose header stands in for hidden content — gets one.
    if !open {
        line.push(Cell::new(fold_mark(open), Color::Dim));
        line.push(Cell::new(" ", Color::Faint));
        let quoted = format!("\"{}\" ", dig.prompt_line);
        let (quoted, _) = head_cells(&quoted, budget.saturating_sub(2));
        line.push(Cell::new(quoted, Color::Dim));
    }

    let used = line_width(&line) + 3;
    let fill = width.saturating_sub(used);
    line.push(Cell::new("─".repeat(fill), Color::Faint));
    line.push(Cell::new(" ─╮", Color::Faint));
    line
}

/// The turn's closing rail, carrying the two facts that only exist once the
/// turn is over: how it ended, and what it spent.
fn turn_frame_bottom(turn: &Turn, width: usize) -> Line {
    let dig = digest::turn_digest(turn, 40, digest::ChipStyle::Tight);
    let mut line = vec![
        Cell::new("╰─ ", Color::Faint),
        Cell::new(
            format!("{} {}", turn.status.glyph(), status_word(turn.status)),
            status_color(turn.status),
        ),
        Cell::new(" ", Color::Faint),
    ];
    // The chips are cut against what the rail has left, for the same reason the
    // top rail cuts its name: the ` ─╯` cap has to land on the right edge, and
    // a turn that spent enough to grow an extra chip must not push it off.
    let head = line_width(&line);
    let all_chips = chips_text(&dig.chips);
    let (chips, chips_w) = head_cells(&all_chips, width.saturating_sub(head + 4));
    let chips = chips.to_string();
    let used = head + chips_w + 4;
    let fill = width.saturating_sub(used);
    line.push(Cell::new("─".repeat(fill), Color::Faint));
    line.push(Cell::new(" ", Color::Faint));
    line.push(Cell::new(chips, Color::Dim));
    line.push(Cell::new(" ─╯", Color::Faint));
    line
}

fn spine_blank() -> Line {
    vec![Cell::new("│", Color::Faint)]
}

fn role_lines(out: &mut Vec<Line>, tag: &str, color: Color, text: &str, width: usize) {
    let badge = format!(" {tag} ");
    let indent = SPINE_W + cells(&badge) + 1;
    let measure = width.saturating_sub(indent + 2).max(20);
    for (i, wrapped) in wrap(text, measure).into_iter().enumerate() {
        let mut line = vec![Cell::new("│ ", Color::Faint)];
        if i == 0 {
            line.push(
                Cell::new(&badge, Color::Ink)
                    .tinted(Tint::Badge(color))
                    .bold(),
            );
            line.push(Cell::new(" ", Color::Faint));
        } else {
            line.push(Cell::new(" ".repeat(cells(&badge) + 1), Color::Faint));
        }
        line.push(Cell::new(wrapped, Color::Ink));
        out.push(line);
    }
}

/// One non-call row — a recall, a compaction, a verdict, a delegation.
///
/// Shares the step row's column geometry on purpose: the offset column carries
/// the kind glyph, the mark column the fold control, and the summary starts
/// where a step's object starts. A note is a peer of a step, not an aside, so
/// it lines up with one.
fn note_lines(out: &mut Vec<Line>, ctx: &Ctx<'_>, note: &crate::model::Note, ti: usize, ni: usize) {
    let width = ctx.width;
    // A note with nothing behind it gets no fold control, rather than a control
    // that opens onto an empty body.
    let foldable = !note.detail.is_empty();
    let open = foldable && ctx.open(NodeId::Note { turn: ti, note: ni });
    let color = note_color(note.kind);

    let mut line = vec![
        Cell::new("│ ", Color::Faint),
        Cell::new(pad(note.kind.glyph(), OFFSET_W), color),
        Cell::new(
            pad(if foldable { fold_mark(open) } else { "" }, MARK_W),
            Color::Dim,
        ),
        // Elided against what the row has left — the spine and the two padded
        // columns — so a long recall or verdict summary cuts at a named
        // boundary rather than being cut raw by `clamp_line` (#3769).
        Cell::new(
            digest::elide(
                &note.summary,
                width.saturating_sub(2 + OFFSET_W + MARK_W).max(5),
            ),
            color,
        ),
    ];
    let used = line_width(&line);
    if used < width {
        line.push(Cell::new(" ".repeat(width - used), Color::Faint));
    }
    out.push(line);

    if !open {
        return;
    }
    for row in &note.detail {
        for wrapped in wrap(row, width.saturating_sub(OFFSET_W + MARK_W + 4).max(20)) {
            out.push(vec![
                Cell::new("│ ", Color::Faint),
                Cell::new(" ".repeat(OFFSET_W + MARK_W), Color::Faint),
                Cell::new(wrapped, Color::Dim),
            ]);
        }
    }
}

/// The colour a note kind paints in. Money stays quiet deliberately — the
/// model's rule is that metadata never carries the weight of the work.
fn note_color(kind: crate::model::NoteKind) -> Color {
    use crate::model::NoteKind;
    match kind {
        NoteKind::Stage => Color::Violet,
        NoteKind::Context => Color::Blue,
        NoteKind::Meter | NoteKind::Other => Color::Dim,
        NoteKind::Wait | NoteKind::Verdict => Color::Amber,
        NoteKind::Handoff => Color::Cyan,
    }
}

fn prose_lines(
    out: &mut Vec<Line>,
    ctx: &Ctx<'_>,
    prose: &crate::model::Prose,
    ti: usize,
    pi: usize,
) {
    let width = ctx.width;
    let open = ctx.open(NodeId::Prose {
        turn: ti,
        prose: pi,
    });
    // `first_sentence` returns the whole sentence whenever a `.`/`!`/`?`
    // boundary exists, and only elides — at a hard-coded 96 cells, unrelated to
    // any caller's width — when there is none. So a prose block opening with a
    // 300-character sentence rendered a 300-cell row into a 100-cell grid.
    // Elide it against what this row actually has: the spine, the badge, the
    // two spaces, the fold marker and the trailing ` …` are 14 cells (#3769).
    const PROSE_HEAD_FIXED: usize = 14;
    let head = digest::elide(
        &digest::first_sentence(&prose.text),
        width.saturating_sub(PROSE_HEAD_FIXED).max(5),
    );
    out.push(vec![
        Cell::new("│ ", Color::Faint),
        Cell::new(" agent ", Color::Ink)
            .tinted(Tint::Badge(Color::Violet))
            .bold(),
        Cell::new(" ", Color::Faint),
        Cell::new(fold_mark(open), Color::Dim),
        Cell::new(" ", Color::Faint),
        Cell::new(head.clone(), Color::Ink),
        Cell::new(" …", Color::Faint),
    ]);
    if !open {
        return;
    }
    // See `prose_block` in html.rs: when `first_sentence` elides rather than finding
    // a sentence boundary, `head` is not a prefix and `strip_prefix` returns `None`,
    // so fall back to the full text to avoid dropping the reasoning body.
    let trimmed = prose.text.trim();
    let rest = trimmed.strip_prefix(head.as_str()).unwrap_or(trimmed);
    for wrapped in wrap(rest.trim(), width.saturating_sub(12).max(20)) {
        out.push(vec![
            Cell::new("│         ", Color::Faint),
            Cell::new(wrapped, Color::Dim),
        ]);
    }
}

fn step_lines(out: &mut Vec<Line>, ctx: &Ctx<'_>, step: &Step, ti: usize, si: usize, offset: &str) {
    let width = ctx.width;
    let open = ctx.open(NodeId::Step { turn: ti, step: si });
    let dig = digest::step_digest(step, 40);
    let chips = chips_text(&dig.chips);

    let mut line = vec![
        Cell::new("│ ", Color::Faint),
        Cell::new(pad(offset, OFFSET_W), Color::Dim),
        Cell::new(pad(fold_mark(open), MARK_W), Color::Dim),
        Cell::new(pad(&dig.verb, TOOL_W), tool_color(dig.class)).bold(),
        Cell::new(&dig.object, Color::Ink),
    ];
    if let Some(delta) = &dig.delta {
        let color = match delta.kind {
            FileStatus::Deleted => Color::Red,
            _ => Color::Green,
        };
        line.push(Cell::new(format!("  {}", delta.label()), color));
    }
    if dig.status == Status::Error {
        line.push(Cell::new("  ✗ failed", Color::Red).bold());
    }
    let used = line_width(&line) + cells(&chips);
    line.push(Cell::new(
        " ".repeat(width.saturating_sub(used)),
        Color::Faint,
    ));
    line.push(Cell::new(chips, Color::Dim));
    out.push(line);

    if !open {
        return;
    }
    if let Some(call) = &step.call {
        body_lines(out, ctx, call, &dig.object, ti, si);
    }
}

fn body_lines(out: &mut Vec<Line>, ctx: &Ctx<'_>, call: &Call, shown: &str, ti: usize, si: usize) {
    if let Some(command) = digest::command_bar(call, shown) {
        out.push(vec![
            body_gutter(),
            Cell::new("$ ", Color::Cyan),
            Cell::new(command, Color::Dim),
        ]);
    }
    for row in call.extra_args() {
        out.push(vec![
            body_gutter(),
            Cell::new(format!("{}: ", row.key), Color::Faint),
            Cell::new(&row.value, Color::Dim),
        ]);
    }

    if call.tool.is_mutation() {
        for (fi, change) in call.files.iter().enumerate() {
            diff_lines(out, ctx, &FileDiff::build(change), ti, si, fi);
        }
        return;
    }

    let fold = digest::fold_output(&call.output, &call.header_object);
    if fold.echo_hidden {
        out.push(vec![body_gutter(), Cell::new("echo hidden", Color::Faint)]);
    }
    let output_open = ctx.open(NodeId::Output { turn: ti, step: si });
    // Decided once from the whole body, not per line: a JSON object's inner
    // lines do not start with a delimiter, and a listing's gutter is only
    // legible as one when you know the body is a listing. The same decision the
    // HTML renderer and the Command Deck make, from the same function, over the
    // same inputs (#3644, #4036).
    let paint = syntax::lines_body_paint(call.read_path(), &fold.body);
    let body = if output_open {
        fold.body.clone()
    } else {
        fold.head.clone()
    };
    for text in &body {
        let mut line = vec![body_gutter()];
        push_output_text(&mut line, text, paint);
        out.push(line);
    }
    if !output_open && fold.has_more() {
        out.push(vec![
            body_gutter(),
            Cell::new(fold.more_label(), Color::Blue),
        ]);
        for text in &fold.tail {
            let mut line = vec![body_gutter()];
            push_output_text(&mut line, text, paint);
            out.push(line);
        }
    }
}

/// Append one result-body line to a row, as syntax-coloured cells.
///
/// A terminal cell carries one foreground, so a coloured line becomes several
/// cells rather than one — which is exactly why the *lexer* is what this shares
/// with the HTML renderer and not the paint. Both split a line into the same
/// runs and classify them identically; one wraps each in a class, the other
/// gives each its own cell.
///
/// A plain line stays a single cell, so nothing about the uncoloured path — and
/// nothing about the row widths the grid pads to — changes.
fn push_output_text(line: &mut Vec<Cell>, text: &str, paint: syntax::BodyPaint) {
    let painted = syntax::paint_line(paint, text);
    // The emitter's own gutter *is* this renderer's number column. It used to
    // draw a second one from the line's index in the fold, which both stacked
    // two gutters on one line and stated the wrong number — a read at offset
    // 780 was labelled line 1, because the index is a position in the body and
    // not a position in the file (#4036).
    if paint.numbered {
        let label = painted
            .gutter
            .map_or_else(|| " ".repeat(NUM_COL), |g| format!("{:>4}  ", g.number));
        line.push(Cell::new(label, Color::Faint));
    }
    // Expanded against the running column, before anything measures: a tab is
    // zero columns to `unicode-width` and a real stop to the terminal, so a row
    // holding one is drawn wider than `line_width` reports it (`crate::tabs`).
    let mut col = line_width(line);
    let mut push = |line: &mut Vec<Cell>, text: &str, color: Color| {
        let expanded = crate::tabs::expand(text, col);
        col += cells(&expanded);
        line.push(Cell::new(expanded, color));
    };
    let Some(lang) = painted.lang else {
        push(line, painted.source, output_color(painted.source));
        return;
    };
    for (run, tok) in syntax::tokenize(painted.source, lang) {
        push(line, &run, tok.map_or(Color::Ink, tok_color));
    }
}

/// Width of the line-number column: four digits and two spaces.
///
/// A body line without a gutter — the read footer — still pays for it, so the
/// source above it stays in one straight column instead of stepping left at the
/// end of every read.
const NUM_COL: usize = 6;

/// The grid colour for a token class.
///
/// The Command Deck's own mapping, restated in this crate's vocabulary:
/// `stella-tui`'s `theme::SYNTAX_KEYWORD` is amber, `SYNTAX_NUMBER` violet, and
/// `SYNTAX_COMMENT` the tertiary text step this crate calls `Faint`. So a JSON
/// body in an exported grid reads in the same hues as the live deck, which is
/// the whole point — a shared lexer that painted different colours would have
/// closed half the gap.
fn tok_color(t: syntax::Tok) -> Color {
    match t {
        syntax::Tok::Keyword => Color::Amber,
        syntax::Tok::Str => Color::Green,
        syntax::Tok::Number => Color::Violet,
        syntax::Tok::Comment => Color::Faint,
        // The two classes the grammar-backed lexer added (#4283), on the deck's
        // own hues for them: `theme::TEAL` for a type, `theme::MAGENTA` for a
        // function name.
        syntax::Tok::Type => Color::Cyan,
        syntax::Tok::Function => Color::Magenta,
    }
}

fn diff_lines(
    out: &mut Vec<Line>,
    ctx: &Ctx<'_>,
    diff: &FileDiff,
    ti: usize,
    si: usize,
    fi: usize,
) {
    let width = ctx.width;
    let open = ctx.open(NodeId::File {
        turn: ti,
        step: si,
        file: fi,
    });
    let (marker, color) = match diff.status {
        FileStatus::Modified => ("±", Color::Amber),
        FileStatus::New => ("◆", Color::Green),
        FileStatus::Deleted => ("✗", Color::Red),
    };
    out.push(vec![
        body_gutter(),
        Cell::new(format!("{marker} {} ", diff.path), color),
        Cell::new(format!("+{} ", diff.added), Color::Green),
        Cell::new(format!("−{}  ", diff.removed), Color::Red),
        Cell::new(fold_mark(open), Color::Blue),
    ]);
    if !open {
        return;
    }

    for (hi, hunk) in diff.hunks.iter().enumerate() {
        out.push(vec![body_gutter(), Cell::new(&hunk.header, Color::Blue)]);
        let hunk_open = !hunk.is_large()
            || ctx.open(NodeId::Hunk {
                turn: ti,
                step: si,
                file: fi,
                hunk: hi,
            });
        if !hunk_open {
            out.push(vec![
                body_gutter(),
                Cell::new(format!("▸ {} lines", hunk.rows.len()), Color::Blue),
            ]);
            continue;
        }
        for row in &hunk.rows {
            out.push(diff_row(row, width));
        }
    }
}

fn diff_row(row: &crate::file_diff::Row, width: usize) -> Line {
    let (line_tint, word_tint, sigil_color) = match row.kind {
        RowKind::Removed => (Tint::DelLine, Tint::DelWord, Color::Red),
        RowKind::Added => (Tint::AddLine, Tint::AddWord, Color::Green),
        RowKind::Context => (Tint::None, Tint::None, Color::Faint),
    };
    let numbers = format!(
        "{:>3} {:>3} ",
        row.old_no.map(|n| n.to_string()).unwrap_or_default(),
        row.new_no.map(|n| n.to_string()).unwrap_or_default(),
    );
    let mut line = vec![
        body_gutter(),
        Cell::new(numbers, Color::Faint),
        Cell::new(format!("{} ", row.kind.sigil()), sigil_color).tinted(line_tint),
    ];
    let text_color = if row.kind == RowKind::Context {
        Color::Dim
    } else {
        Color::Ink
    };
    for span in &row.spans {
        let tint = if span.changed { word_tint } else { line_tint };
        line.push(Cell::new(&span.text, text_color).tinted(tint));
    }
    // Pad the tinted run to the right edge so the background block is a
    // rectangle rather than a ragged flag — the same reason the web renderer
    // tints the whole table cell.
    if line_tint != Tint::None {
        let pad = width.saturating_sub(line_width(&line));
        if pad > 0 {
            line.push(Cell::new(" ".repeat(pad), text_color).tinted(line_tint));
        }
    }
    line
}

fn status_line(run: &Run, state: &FoldState, width: usize) -> Line {
    let acc = run.rollup();
    let left = format!(
        "{} turns · {} steps · {}",
        run.turns.len(),
        run.step_count(),
        digest::format_cost(acc.micros)
    );
    let right = format!(
        "j/k step  ←/→ fold  z zoom [{}]  e outputs  c copy",
        state.zoom().label()
    );
    let gap = width.saturating_sub(cells(&left) + cells(&right)).max(2);
    vec![
        Cell::new(left, Color::Ink).bold(),
        Cell::new(" ".repeat(gap), Color::Faint),
        Cell::new(right, Color::Cyan),
    ]
}

fn body_gutter() -> Cell {
    Cell::new("│        │ ", Color::Faint)
}

/// The textual fold marker. One cell wide in both states, which is what keeps
/// every column to its right from moving on a toggle.
#[must_use]
pub fn fold_mark(open: bool) -> &'static str {
    if open { "▾" } else { "▸" }
}

/// The longest prefix of `text` that fits in `budget` display cells, and how
/// many cells that prefix occupies.
///
/// The one cut this module makes, so a cut made to fill a fixed gutter and a
/// cut made to break an over-long word cannot disagree about where the
/// boundary is. It counts columns rather than characters and stops *before* a
/// double-width character that would straddle the budget, so the answer is
/// always whole characters and never one cell over. It can come back one cell
/// under, and empty when even the first character is wider than `budget`.
fn head_cells(text: &str, budget: usize) -> (&str, usize) {
    let mut used = 0;
    for (i, ch) in text.char_indices() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > budget {
            return (&text[..i], used);
        }
        used += w;
    }
    (text, used)
}

/// Cut `line` to at most `width` display cells.
///
/// The backstop under the module doc's width contract, and deliberately not the
/// mechanism that keeps it. Every framed row fits itself as it is assembled,
/// because a rail cut here would lose the `─╮` cap that closes it. What
/// actually reaches this is a **content** row — a result body line, a diff row,
/// an argument value, a command bar — whose text is as wide as the tool made it
/// and which holds no named boundary to prefer a cut at.
///
/// Cutting beats the alternative because the alternative is not "the reader
/// sees more": it is the terminal soft-wrapping the excess onto a row of its
/// own, shifting everything below and walking the fixed columns this module
/// exists to hold. The cut goes through [`head_cells`], so it lands on a cell
/// boundary and never straddles a double-width character.
fn clamp_line(line: Line, width: usize) -> Line {
    if line_width(&line) <= width {
        return line;
    }
    let mut out = Vec::with_capacity(line.len());
    let mut used = 0;
    for cell in line {
        let w = cell.width();
        if used + w <= width {
            used += w;
            out.push(cell);
            continue;
        }
        let head = head_cells(&cell.text, width - used).0.to_string();
        if !head.is_empty() {
            out.push(Cell { text: head, ..cell });
        }
        break;
    }
    out
}

/// `text`, in exactly `width` display cells: padded when short, truncated when
/// long.
///
/// Truncation goes through [`head_cells`], so it stops before a double-width
/// character that would straddle the boundary — the cell it would have
/// half-filled becomes a space, and the column after it starts where the
/// constant says it does whether or not the text was cut mid-ideograph.
fn pad(text: &str, width: usize) -> String {
    let (head, used) = head_cells(text, width);
    let mut out = String::with_capacity(head.len() + (width - used));
    out.push_str(head);
    out.push_str(&" ".repeat(width - used));
    out
}

fn chips_text(chips: &[Chip]) -> String {
    chips
        .iter()
        .map(|c| format!("[{}]", c.text))
        .collect::<Vec<_>>()
        .join(" ")
}

fn tool_color(class: &str) -> Color {
    match class {
        "bash" => Color::Cyan,
        "rf" | "wf" => Color::Green,
        "ef" => Color::Amber,
        "df" => Color::Red,
        "sr" => Color::Violet,
        _ => Color::Dim,
    }
}

fn status_color(status: Status) -> Color {
    match status {
        Status::Ok => Color::Green,
        Status::Warn => Color::Amber,
        Status::Error => Color::Red,
        Status::Running => Color::Cyan,
    }
}

fn status_word(status: Status) -> &'static str {
    match status {
        Status::Ok => "done",
        Status::Warn => "warnings",
        Status::Error => "failed",
        Status::Running => "running",
    }
}

/// Tint an output line by what it looks like — a cheap, honest heuristic that
/// only ever colours, never hides.
fn output_color(text: &str) -> Color {
    let lower = text.to_ascii_lowercase();
    if lower.contains("error") || lower.contains("failed") {
        Color::Red
    } else if lower.contains("warn") || lower.contains("overfull") {
        Color::Amber
    } else {
        Color::Dim
    }
}

/// Fold `text` into rows of at most `width` display cells.
///
/// # An over-wide unit is hard-broken, not allowed to overflow
///
/// The preferred break is between words, and it is tried first. But a single
/// unbreakable unit can be wider than the whole measure — a URL, a minified
/// line, a stack-frame path, or a Japanese or Chinese paragraph, which is
/// normally written with no spaces at all. There is no whitespace to break at,
/// so the choice is to cut inside the unit or to emit a row past the right
/// edge, and this function **cuts** (#3769).
///
/// Overflow is the worse of the two because the terminal, not this renderer,
/// then decides what happens to the excess: it either soft-wraps — shifting
/// every row below it and walking the fixed columns this module exists to hold
/// (see the module doc) — or truncates, silently losing the tail. A cut at a
/// cell boundary keeps every character *and* keeps the frame; nothing is
/// dropped, because the remainder simply starts the next row.
///
/// The cut goes through [`head_cells`], so it counts columns rather than
/// characters and never straddles a double-width character. The one row that
/// can still exceed `width` is a single character wider than the entire
/// measure, which is placed alone rather than dropped or looped on; no caller
/// here can reach it, because each clamps its measure to at least 20 cells.
fn wrap(text: &str, width: usize) -> Vec<String> {
    // The narrowest grid that can make progress. Clamping here is what lets the
    // hard break below terminate: at zero cells no prefix ever fits.
    let width = width.max(1);
    let mut out = Vec::new();
    for paragraph in text.trim().lines() {
        let mut current = String::new();
        // Carried rather than re-measured per word: the running total is the
        // same answer, and re-walking the line for every word it already holds
        // is quadratic in a paragraph's length for no gain.
        let mut used = 0;
        for word in paragraph.split_whitespace() {
            let word_w = cells(word);
            if !current.is_empty() && used + 1 + word_w > width {
                out.push(std::mem::take(&mut current));
                used = 0;
            }
            if word_w > width {
                // Wider than any row, so breaking *between* words cannot help.
                // `current` is necessarily empty: the flush above fires for
                // every non-empty row once the word cannot join it. Emit full
                // rows and carry the remainder, which the next word may join.
                let mut rest = word;
                while cells(rest) > width {
                    let (head, _) = head_cells(rest, width);
                    // Empty only when one character is wider than the whole
                    // measure. Place it alone: the row is a cell over, which
                    // beats dropping the character or never advancing.
                    let cut = if head.is_empty() {
                        rest.chars().next().map_or(rest.len(), char::len_utf8)
                    } else {
                        head.len()
                    };
                    out.push(rest[..cut].to_string());
                    rest = &rest[cut..];
                }
                current.push_str(rest);
                used = cells(rest);
                continue;
            }
            if !current.is_empty() {
                current.push(' ');
                used += 1;
            }
            current.push_str(word);
            used += word_w;
        }
        out.push(current);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Encode to plain text — what a golden test asserts.
#[must_use]
pub fn to_plain(lines: &[Line]) -> String {
    lines
        .iter()
        .map(|line| {
            line.iter()
                .map(|c| c.text.as_str())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Encode with 256-colour escapes, including the `48;5;n` background spans the
/// word-level diff needs.
#[must_use]
pub fn to_ansi256(lines: &[Line]) -> String {
    encode(lines, false)
}

/// Encode for a 16-colour terminal.
///
/// There is no background index to reach for, so a word-level change falls back
/// to bold + underline. That is a real degradation and is stated as one: the
/// information survives, the elegance does not.
#[must_use]
pub fn to_ansi16(lines: &[Line]) -> String {
    encode(lines, true)
}

fn encode(lines: &[Line], basic: bool) -> String {
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        for cell in line {
            let mut codes: Vec<String> = Vec::new();
            if cell.bold {
                codes.push("1".to_string());
            }
            if basic {
                codes.push(cell.fg.ansi16().to_string());
                match cell.tint {
                    Tint::DelWord | Tint::AddWord => {
                        codes.push("1".to_string());
                        codes.push("4".to_string());
                    }
                    Tint::Badge(_) => codes.push("7".to_string()),
                    _ => {}
                }
            } else {
                codes.push(format!("38;5;{}", cell.fg.ansi256()));
                if let Some(bg) = cell.tint.ansi256() {
                    codes.push(format!("48;5;{bg}"));
                }
                if matches!(cell.tint, Tint::Badge(_)) {
                    codes.push("30".to_string());
                }
            }
            out.push_str(&format!(
                "\u{1b}[{}m{}\u{1b}[0m",
                codes.join(";"),
                cell.text
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Run, Status, Turn};

    /// A run of one open turn whose only content is `prompt`.
    fn run_with_prompt(prompt: &str) -> Run {
        Run {
            name: "wrap-probe".to_string(),
            model: "z-ai/glm-5.2".to_string(),
            started_at: "14:02:11".to_string(),
            turns: vec![Turn {
                name: "probe".to_string(),
                prompt: prompt.to_string(),
                prose: Vec::new(),
                notes: Vec::new(),
                steps: Vec::new(),
                answer: None,
                status: Status::Ok,
                duration_ms: 1,
            }],
        }
    }

    #[test]
    fn wrap_hard_breaks_a_unit_wider_than_the_measure() {
        let word = "x".repeat(90);
        let rows = wrap(&word, 20);
        for row in &rows {
            assert!(cells(row) <= 20, "row of {} cells: {row:?}", cells(row));
        }
        assert_eq!(rows.concat(), word, "the hard break dropped characters");
    }

    #[test]
    fn wrap_hard_breaks_without_straddling_a_double_width_character() {
        // No spaces anywhere — the normal way a CJK paragraph is written — and
        // an odd measure, so a naive cell cut would land mid-ideograph.
        let text = "日本語".repeat(10);
        let rows = wrap(&text, 21);
        for row in &rows {
            assert!(cells(row) <= 21, "row of {} cells: {row:?}", cells(row));
        }
        assert_eq!(rows.concat(), text, "the hard break dropped characters");
    }

    #[test]
    fn wrap_still_breaks_between_words_when_it_can() {
        assert_eq!(
            wrap("alpha beta gamma delta", 12),
            vec!["alpha beta".to_string(), "gamma delta".to_string()],
        );
    }

    #[test]
    fn an_unbreakable_prompt_stays_inside_the_grid() {
        const WIDTH: usize = 100;
        let run = run_with_prompt(&"x".repeat(400));
        for line in &render(&run, &FoldState::new(), WIDTH) {
            assert!(
                line_width(line) <= WIDTH,
                "row of {} cells: {:?}",
                line_width(line),
                to_plain(std::slice::from_ref(line)),
            );
        }
    }
}
