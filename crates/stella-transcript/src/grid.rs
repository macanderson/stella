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

use crate::digest::{self, Chip};
use crate::file_diff::{FileDiff, RowKind};
use crate::fold::FoldState;
use crate::model::{Call, FileStatus, NodeId, Run, Status, Step, ToolKind, Turn};

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

    /// Display width in cells.
    #[must_use]
    pub fn width(&self) -> usize {
        self.text.chars().count()
    }
}

/// One rendered row.
pub type Line = Vec<Cell>;

/// Total display width of a row.
#[must_use]
pub fn line_width(line: &Line) -> usize {
    line.iter().map(Cell::width).sum()
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
        out.push(turn_frame_bottom(width));
        return;
    }

    role_lines(out, "you", Color::Cyan, &turn.prompt, width);
    out.push(spine_blank());

    let offsets = digest::offsets(&turn.steps);
    for (si, step) in turn.steps.iter().enumerate() {
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
    for (pi, prose) in turn.prose.iter().enumerate() {
        if prose.before_step >= turn.steps.len() {
            prose_lines(out, ctx, prose, index, pi);
        }
    }

    if let Some(answer) = &turn.answer {
        out.push(spine_blank());
        role_lines(out, "agent", Color::Violet, answer, width);
    }
    out.push(turn_frame_bottom(width));
}

fn turn_frame_top(turn: &Turn, open: bool, width: usize) -> Line {
    let dig = digest::turn_digest(turn, 40);
    let mut line = vec![
        Cell::new("╭─ ", Color::Faint),
        Cell::new(&turn.name, Color::Violet).bold(),
        Cell::new(" ", Color::Faint),
        Cell::new(
            format!("{} {}", turn.status.glyph(), status_word(turn.status)),
            status_color(turn.status),
        ),
        Cell::new(" ", Color::Faint),
        Cell::new(fold_mark(open), Color::Dim),
        Cell::new(" ", Color::Faint),
    ];
    if !open {
        line.push(Cell::new(format!("\"{}\" ", dig.prompt_line), Color::Dim));
    }

    let chips = chips_text(&dig.chips);
    let used = line_width(&line) + chips.chars().count() + 2;
    let fill = width.saturating_sub(used).max(1);
    line.push(Cell::new("─".repeat(fill), Color::Faint));
    line.push(Cell::new(" ", Color::Faint));
    line.push(Cell::new(chips, Color::Dim));
    line.push(Cell::new(" ─╮", Color::Faint));
    line
}

fn turn_frame_bottom(width: usize) -> Line {
    vec![Cell::new(
        format!("╰{}╯", "─".repeat(width.saturating_sub(2))),
        Color::Faint,
    )]
}

fn spine_blank() -> Line {
    vec![Cell::new("│", Color::Faint)]
}

fn role_lines(out: &mut Vec<Line>, tag: &str, color: Color, text: &str, width: usize) {
    let badge = format!(" {tag} ");
    let indent = SPINE_W + badge.chars().count() + 1;
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
            line.push(Cell::new(
                " ".repeat(badge.chars().count() + 1),
                Color::Faint,
            ));
        }
        line.push(Cell::new(wrapped, Color::Ink));
        out.push(line);
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
    let head = digest::first_sentence(&prose.text);
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
    let used = line_width(&line) + chips.chars().count();
    line.push(Cell::new(
        " ".repeat(width.saturating_sub(used).max(1)),
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

    let numbered = matches!(call.tool, ToolKind::ReadFile);
    let fold = digest::fold_output(&call.output, &call.header_object);
    if fold.echo_hidden {
        out.push(vec![body_gutter(), Cell::new("echo hidden", Color::Faint)]);
    }
    let output_open = ctx.open(NodeId::Output { turn: ti, step: si });
    let body = if output_open {
        fold.body.clone()
    } else {
        fold.head.clone()
    };
    for (i, text) in body.iter().enumerate() {
        let mut line = vec![body_gutter()];
        if numbered {
            line.push(Cell::new(format!("{:>4}  ", i + 1), Color::Faint));
        }
        line.push(Cell::new(text, output_color(text)));
        out.push(line);
    }
    if !output_open && fold.has_more() {
        out.push(vec![
            body_gutter(),
            Cell::new(fold.more_label(), Color::Blue),
        ]);
        for text in &fold.tail {
            out.push(vec![body_gutter(), Cell::new(text, output_color(text))]);
        }
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
    let gap = width
        .saturating_sub(left.chars().count() + right.chars().count())
        .max(2);
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

fn pad(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len >= width {
        return text.chars().take(width).collect();
    }
    format!("{text}{}", " ".repeat(width - len))
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

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.trim().lines() {
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            if !current.is_empty() && current.chars().count() + 1 + word.chars().count() > width {
                out.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
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
