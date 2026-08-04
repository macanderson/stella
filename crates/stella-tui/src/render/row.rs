//! The vocabulary of a transcript row: the rails that index the left margin,
//! the wrapping that keeps a wrapped line aligned under its own text, and the
//! small presentation helpers (durations, counts, paths, the right-hand metric
//! column) that decide how a row reads.
//!
//! Split out from `render` because these are the *grammar* of a row, while
//! `render::entry_lines` is the *sentence* each transcript entry composes from
//! it. Keeping them apart means a change to how one entry kind is phrased
//! cannot quietly change the layout every other kind depends on.
//!
//! The governing idea is that a transcript is a list and lists are read down
//! their left edge. Every row therefore opens with a fixed-width rail whose
//! glyph names the row's kind, so the margin can be scanned for shape before
//! anything is read for content.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme;

/// Coalesce adjacent same-styled characters into spans for compact output.
pub(crate) fn styled_chars_to_spans(chars: Vec<(char, Style)>) -> Vec<Span<'static>> {
    if chars.is_empty() {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut buf = String::new();
    let mut style = chars[0].1;
    for (ch, st) in chars {
        if st != style && !buf.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut buf), style));
            style = st;
        }
        if buf.is_empty() {
            style = st;
        }
        buf.push(ch);
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, style));
    }
    spans
}

/// Content column for a top-level row: the rail glyph occupies column 0 and
/// one space separates it from the text.
pub(crate) const LEAD: usize = 2;

/// Content column for a subordinate row (a tool result and its body). Indented
/// one rail-width past its parent call so the hierarchy is visible without a
/// connecting line on every row.
pub(crate) const BODY: usize = 4;

/// Which rail a transcript row rides. The glyph is the row's type signature —
/// a reader scanning the margin sees the shape of the session (calls, their
/// outcomes, prose, prompts) before reading any content.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Rail {
    /// A tool invocation — the thing that happened.
    Call,
    /// A tool result body, subordinate to the call above it.
    Result,
    /// A failed tool result. Distinct glyph *and* column-2 position, so a
    /// failure is findable by margin-scan alone.
    Fail,
    /// A user prompt — the strongest landmark in the scrollback, since it is
    /// where a reader re-orients when scrolling back.
    User,
    /// Assistant prose. Deliberately glyph-less: prose is the default voice of
    /// the transcript, and a marker on every paragraph would be noise.
    Agent,
}

impl Rail {
    /// The literal prefix this rail prints before a row's first line.
    pub(crate) fn prefix(self) -> &'static str {
        match self {
            Rail::Call => "● ",
            Rail::Result => "  ⎿ ",
            Rail::Fail => "  ✗ ",
            Rail::User => "▌ ",
            Rail::Agent => "  ",
        }
    }

    /// The column continuation lines indent to — always the width of
    /// [`Rail::prefix`], so wrapped text lines up under the first line's text
    /// rather than under its glyph.
    pub(crate) fn indent(self) -> usize {
        UnicodeWidthStr::width(self.prefix())
    }

    /// The style the rail glyph itself renders in. Content styling is the
    /// caller's; the glyph is always the rail's own semantic color so the
    /// margin reads consistently even when content colors vary.
    pub(crate) fn style(self) -> Style {
        match self {
            Rail::Call => Style::new().fg(theme::ACCENT_DEEP),
            Rail::Result => Style::new().fg(theme::MUTED),
            Rail::Fail => Style::new().fg(theme::DANGER),
            Rail::User => Style::new().fg(theme::VIOLET).add_modifier(Modifier::BOLD),
            Rail::Agent => Style::new(),
        }
    }
}

/// Wrap a single styled line into multiple lines of at most `max_width`,
/// with continuation lines indented by `indent` spaces. The first line
/// passes through unchanged (it already has its label prefix).
pub(crate) fn wrap_one_indent(
    line: Line<'static>,
    max_width: usize,
    indent: usize,
    out: &mut Vec<Line<'static>>,
) {
    let line_width = line.width();
    // The last clause is the narrow-terminal guard: with `indent >= max_width`
    // the content column has zero width, and the loop below would then flush a
    // row per character — a 60-character reply exploding into 60 rows on a
    // ≤40-column terminal (the transcript pane is 60% of the frame, so its
    // inner width drops under LABEL_COL there). Clip at the pane edge instead,
    // exactly as the un-wrapped diff rows already do.
    if line_width <= max_width || max_width == 0 || max_width <= indent {
        out.push(line);
        return;
    }
    let styled: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
        .collect();

    let content_width = max_width.saturating_sub(indent);
    let mut current: Vec<(char, Style)> = Vec::new();
    let mut current_w = 0usize;

    let flush = |cur: &mut Vec<(char, Style)>, first: bool, out: &mut Vec<Line<'static>>| {
        if !cur.is_empty() {
            let pairs = std::mem::take(cur);
            if first {
                out.push(Line::from(styled_chars_to_spans(pairs)));
            } else {
                let mut spans = vec![Span::raw(" ".repeat(indent))];
                spans.extend(styled_chars_to_spans(pairs));
                out.push(Line::from(spans));
            }
        }
    };

    let mut is_first = true;
    for (ch, style) in styled {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_w + cw > content_width && !current.is_empty() {
            if let Some(space_idx) = current.iter().rposition(|(c, _)| *c == ' ') {
                let mut remainder: Vec<(char, Style)> = current.split_off(space_idx);
                // Consume the wrap-boundary whitespace so the continuation line
                // starts flush at the indent column. Left in place, the leading
                // space stacks on top of `indent` and pushes every wrapped row
                // one column right of the clean left edge — the "extra blank
                // space after the colon" bug.
                let lead = remainder.iter().take_while(|(c, _)| *c == ' ').count();
                remainder.drain(..lead);
                flush(&mut current, is_first, out);
                is_first = false;
                current = remainder;
                current_w = current
                    .iter()
                    .map(|(c, _)| UnicodeWidthChar::width(*c).unwrap_or(0))
                    .sum();
            } else {
                flush(&mut current, is_first, out);
                is_first = false;
                current_w = 0;
            }
        }
        current.push((ch, style));
        current_w += cw;
    }
    flush(&mut current, is_first, out);
}

/// Emit one transcript row on `rail`: the rail glyph, then `content`, with
/// wrap continuations indented to the rail's content column.
///
/// Every `entry_lines` arm MUST route its rows through this (or
/// [`push_row_block`] for multi-line content) — no transcript row renders at
/// the left margin without a rail. The
/// `every_transcript_entry_renders_on_a_rail` test enforces it, with exactly
/// one deliberate exception: [`TranscriptEntry::Evicted`] is a system note
/// *about* the transcript rather than an entry in it, so it renders untagged
/// and full-bleed.
pub(crate) fn push_row(
    rail: Rail,
    content: Vec<Span<'static>>,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    push_row_block(rail, vec![Line::from(content)], width, out);
}

/// Emit one expanded-detail row (a ctrl+o body line) at the subordinate body
/// column. Detail rows sit directly under their parent result's content —
/// aligned to [`BODY`] — so an expanded body reads as part of the same block
/// rather than as a new top-level event.
pub(crate) fn push_detail_line(text: &str, width: usize, out: &mut Vec<Line<'static>>) {
    wrap_one_indent(
        Line::from(vec![
            Span::raw(" ".repeat(BODY)),
            Span::styled(text.to_owned(), Style::new().fg(theme::MUTED)),
        ]),
        width,
        BODY,
        out,
    );
}

/// Emit a system-note row: `↻ retry`, `⇣ compacted`, `✗ error` and friends.
///
/// These rows *are* their own rail — each label already opens with a glyph in
/// column 0, so it indexes the margin exactly like [`Rail::Call`] does without
/// needing a second marker in front of it. Content follows two spaces later;
/// wrapped lines indent to [`LEAD`].
pub(crate) fn push_note(
    label: &str,
    label_style: Style,
    content: Vec<Span<'static>>,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    push_note_block(label, label_style, vec![Line::from(content)], width, out);
}

/// Multi-line form of [`push_note`].
pub(crate) fn push_note_block(
    label: &str,
    label_style: Style,
    lines: Vec<Line<'static>>,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    for (i, line) in lines.into_iter().enumerate() {
        let mut spans = if i == 0 {
            vec![Span::styled(format!("{label}  "), label_style)]
        } else {
            vec![Span::raw(" ".repeat(LEAD))]
        };
        spans.extend(line.spans);
        wrap_one_indent(Line::from(spans), width, LEAD, out);
    }
}

/// Emit a section rule: a hairline across the pane with a label set into it.
///
/// This is what a *stage* renders as. A stage is not an event — nothing
/// happened at it — it is the boundary between two kinds of work, and a
/// boundary should look like a boundary. Rendered as one more note row
/// (`◇ stage  execute`) it competed for attention with the events on either
/// side of it while carrying a fraction of their information, and a transcript
/// with four of them in twenty rows reads as noise. As a rule it recedes until
/// you are scrolling for it — which is the only time a phase marker is useful.
///
/// The label is upper-cased because a rule is chrome, and upper-case is how
/// this deck already distinguishes chrome from content (see the statline).
pub(crate) fn push_rule(
    label: &str,
    label_style: Style,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    let hair = Style::new().fg(theme::HAIRLINE);
    let text = label.to_uppercase();
    // `── ` + label + ` ` — the lead is fixed so successive rules stack into a
    // straight left edge, and only the trailing fill is elastic.
    const LEAD_RULE: &str = "── ";
    let used = UnicodeWidthStr::width(LEAD_RULE) + UnicodeWidthStr::width(text.as_str()) + 1;
    let mut spans = vec![
        Span::styled(LEAD_RULE, hair),
        Span::styled(text, label_style),
        Span::styled(" ", hair),
    ];
    // A pane too narrow for any trailing fill still gets the lead and label;
    // the rule simply stops being a rule and becomes a heading.
    if width > used {
        spans.push(Span::styled("─".repeat(width - used), hair));
    }
    out.push(Line::from(spans));
}

/// Emit a blank spacer row.
///
/// Rhythm is what turns a wall of rows into a readable page, but a blank line
/// between *every* pair of rows would halve the visible history — so the gap
/// is structural, not uniform: it precedes a block-level entry (a call, a
/// prompt, a paragraph of prose) and never separates a result from the call
/// it belongs to. Dense runs of one-line reads stay dense; the substantial
/// things get air around them. This is paragraph spacing, not line spacing.
pub(crate) fn push_gap(out: &mut Vec<Line<'static>>) {
    out.push(Line::default());
}

/// Emit one inline-diff row at the body column. Diff lines are pushed
/// un-wrapped — the transcript renders without wrap (one logical line per row
/// keeps the scroll math line-exact), so overflow clips at the pane edge like
/// the diff viewer, and the line-number gutter never mis-aligns mid-diff.
pub(crate) fn push_diff_line(line: Line<'static>, out: &mut Vec<Line<'static>>) {
    let mut spans = vec![Span::raw(" ".repeat(BODY))];
    spans.extend(line.spans);
    out.push(Line::from(spans));
}

/// Multi-line form of [`push_row`]: the rail glyph prefixes the first line and
/// every following line indents to the rail's content column.
pub(crate) fn push_row_block(
    rail: Rail,
    lines: Vec<Line<'static>>,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    let indent = rail.indent();
    for (i, line) in lines.into_iter().enumerate() {
        let mut spans = if i == 0 {
            vec![Span::styled(rail.prefix().to_string(), rail.style())]
        } else {
            vec![Span::raw(" ".repeat(indent))]
        };
        spans.extend(line.spans);
        wrap_one_indent(Line::from(spans), width, indent, out);
    }
}

/// Widest the `content … metric` span grows before the metric stops tracking
/// the pane edge. Chosen above any ordinary transcript width so normal panes
/// stay flush-right and only genuinely wide ones are reined in.
pub(crate) const METRIC_SPAN: usize = 140;

/// Soft column the tool-name field pads to, so arguments align down a run of
/// calls. A longer name overruns it rather than truncating — identity beats
/// alignment — and the argument simply starts one space later on that row.
pub(crate) const NAME_COL: usize = 13;

/// Lines of a *failed* result shown without expanding. Enough for a compiler
/// error with its location and caret line, or the top of a panic backtrace.
pub(crate) const FAIL_PREVIEW: usize = 6;

/// Pad a tool name to [`NAME_COL`], display-width aware.
pub(crate) fn pad_name(name: &str) -> String {
    let w = UnicodeWidthStr::width(name);
    format!("{name}{} ", " ".repeat(NAME_COL.saturating_sub(w + 1)))
}

/// A duration at human scale. Raw milliseconds are the wrong unit above a
/// second — `4210ms` forces the reader to count digits to learn "about four
/// seconds" — so the unit follows the magnitude, always at two significant
/// figures of useful precision.
pub(crate) fn human_duration(ms: u64) -> String {
    match ms {
        0..=999 => format!("{ms}ms"),
        1_000..=59_999 => {
            let s = ms as f64 / 1000.0;
            // Sub-10s keeps a decimal (4.2s reads as distinct from 4.9s);
            // above that the tenth is noise next to the whole seconds.
            if s < 10.0 {
                format!("{s:.1}s")
            } else {
                format!("{}s", ms / 1000)
            }
        }
        _ => {
            let total = ms / 1000;
            format!("{}m{:02}s", total / 60, total % 60)
        }
    }
}

/// `1 line` / `2 lines` — a count that reads as English rather than as a
/// template with a number substituted into it.
pub(crate) fn plural_lines(n: usize) -> String {
    if n == 1 {
        "1 line".to_string()
    } else {
        format!("{} lines", thousands(n))
    }
}

/// A count with thousands separators — `1,584 lines` is legible at a glance
/// where `1584 lines` needs a beat of digit-counting.
pub(crate) fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Split a path into (directory, basename) spans so the basename carries the
/// emphasis. In a scan, the file *identity* is what the eye is hunting; the
/// directory is context that only matters once you've found the file, so it
/// recedes. Non-path text renders as a single unemphasised span.
pub(crate) fn path_spans(text: &str, is_path: bool) -> Vec<Span<'static>> {
    let dim = Style::new().fg(theme::MUTED);
    let bright = Style::new().fg(theme::INK);
    match text.rfind('/').filter(|_| is_path) {
        Some(cut) => vec![
            Span::styled(text[..=cut].to_owned(), dim),
            Span::styled(text[cut + 1..].to_owned(), bright),
        ],
        None if is_path => vec![Span::styled(text.to_owned(), bright)],
        None => vec![Span::styled(text.to_owned(), dim)],
    }
}

/// Lay a row out as `left … right`, with `right` flush to the pane edge and
/// `left` truncated (not wrapped) if the two would collide.
///
/// The right edge becomes a second scan column: durations and sizes stack into
/// a vertical strip a reader can run down to find "what was slow" or "what was
/// big" without reading a single tool name. Truncation falls on the left
/// because the metric is fixed-width and the argument is the elastic part.
pub(crate) fn justify(
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: usize,
    indent: usize,
) -> Vec<Span<'static>> {
    let left_w: usize = left
        .iter()
        .map(|s| UnicodeWidthStr::width(&*s.content))
        .sum();
    let right_w: usize = right
        .iter()
        .map(|s| UnicodeWidthStr::width(&*s.content))
        .sum();
    let full = width.saturating_sub(indent);
    if right_w == 0 {
        return left;
    }
    // No room to justify: drop the right column rather than let it collide or
    // push the row into a wrap. The metric is recoverable via ctrl+o; a mangled
    // row is not recoverable at all.
    if full <= right_w + 1 {
        return left;
    }
    // One space minimum between the two, or the metric loses its own column.
    if left_w + right_w + 1 > full {
        let budget = full - right_w - 1;
        let mut spans = truncate_spans(left, budget);
        spans.push(Span::raw(" "));
        spans.extend(right);
        return spans;
    }
    // Right-aligned, but only within [`METRIC_SPAN`]. Flush to the pane edge is
    // right at ordinary widths and wrong on an ultrawide terminal, where the
    // metric ends up so far from the row it labels that the two stop reading as
    // one line — a scan column has to stay within a saccade of its content.
    let span = full.min(METRIC_SPAN.max(left_w + right_w + 1));
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(span - left_w - right_w)));
    spans.extend(right);
    spans
}

/// Truncate a styled run to `budget` display columns, ending in `…` when it
/// had to cut. Styling is preserved per span.
pub(crate) fn truncate_spans(spans: Vec<Span<'static>>, budget: usize) -> Vec<Span<'static>> {
    let total: usize = spans
        .iter()
        .map(|s| UnicodeWidthStr::width(&*s.content))
        .sum();
    if total <= budget {
        return spans;
    }
    let keep = budget.saturating_sub(1);
    let mut out = Vec::new();
    let mut used = 0usize;
    for span in spans {
        let w = UnicodeWidthStr::width(&*span.content);
        if used + w <= keep {
            used += w;
            out.push(span);
            continue;
        }
        let room = keep - used;
        let mut text = String::new();
        let mut tw = 0usize;
        for c in span.content.chars() {
            let cw = UnicodeWidthChar::width(c).unwrap_or(0);
            if tw + cw > room {
                break;
            }
            tw += cw;
            text.push(c);
        }
        let style = span.style;
        out.push(Span::styled(text, style));
        out.push(Span::styled("…".to_string(), style));
        return out;
    }
    out
}

/// Markers that make a line of tool output worth surfacing over the line that
/// merely happens to be first. Matched case-insensitively at a word boundary.
pub(crate) const SALIENT: [&str; 8] = [
    "error",
    "warning",
    "failed",
    "failure",
    "panic",
    "assert",
    "fatal",
    "exception",
];

/// Pick the line of `text` a collapsed result should show.
///
/// Showing line 1 is the obvious rule and the wrong one: a build's first line
/// is `Checking foo v0.1.0` while the line that matters — the error — is
/// twenty lines down, so the collapsed row reliably shows the least
/// informative part of the most important output. Instead, find the first
/// line that carries a failure marker and anchor there; fall back to the
/// first non-blank line when nothing stands out. Returns the index into
/// `text.lines()`.
pub(crate) fn salient_line(text: &str) -> usize {
    let mut first_nonblank = 0;
    let mut seen_nonblank = false;
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if !seen_nonblank && !trimmed.is_empty() {
            first_nonblank = i;
            seen_nonblank = true;
        }
        let lower = trimmed.to_ascii_lowercase();
        if SALIENT.iter().any(|m| {
            lower.starts_with(m)
                // `error[E0432]:` / `warning:` / `error --> ` all count; a
                // prose line that merely contains "error" does not, or every
                // log line mentioning error handling would hijack the row.
                || lower
                    .split_once(&format!("{m}:"))
                    .is_some_and(|(head, _)| head.len() <= 12)
        }) {
            return i;
        }
    }
    first_nonblank
}
