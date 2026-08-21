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

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme;
use crate::v2::transcript::RAIL_W;

/// The cells every row of a **tool-call block** opens with, so a call's head,
/// its result, its body and its inline diff read as one unbroken vertical.
///
/// The glyph is [`crate::v2::transcript::rail_span`]'s and not a second copy of
/// it (`render::tests::block_rail` is what says so): the head of every
/// call renders through the v2 event renderer (SPEC 6.2) while the result below
/// it still renders here, and the two must agree on the column or the rail dies
/// one row into the block — which is exactly what it did.
///
/// The *metal* is shared too, as of #4127. It was not: a v2 head wore its
/// event's colour while the result under it receded to a fixed muted tone, so
/// the rail changed colour one row into the block it exists to hold together.
/// SPEC 6.2 makes the rail a property of the **event**, and a call and its
/// result are one event — the transcript records them as two entries only
/// because the head has to draw before the result exists. So the metal rides
/// [`Rail::Result`] from the call's own kind; see [`Rail::style`] and
/// [`block_margin`], which had already reached this conclusion for body rows.
pub(crate) const RAIL: &str = " │";

// The rail is two cells wide on both sides of the seam, or every column below
// is arithmetic about a different glyph than the one v2 draws.
const _: () = assert!(RAIL_W == 2);

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

/// Content column for a subordinate row that rides no rail — the context-recall
/// table's frame and budget grids. Indented one rail-width past the note that
/// heads it so the hierarchy is visible without a connecting line on every row.
///
/// Not the tool-block column: everything inside a call block indents to
/// [`Rail::indent`] instead, which is [`RAIL`] plus a glyph cell. The two were
/// one constant until the v2 head renderer moved the block's content column to
/// 5 and left this one behind at 4, so a result's body sat one cell left of the
/// call it belonged to.
pub(crate) const BODY: usize = 4;

/// Content column of every row in a tool-call block: [`RAIL`], a space, one
/// glyph cell, a space.
///
/// The v2 head renderer arrives at the same number from the other side
/// (`rail_span` + `" {glyph} "`), which is what makes a call and its result one
/// block; `render::tests::block_rail` is what holds the two to it.
pub(crate) const BLOCK_BODY: usize = RAIL_W + 3;

/// The margin every row of a tool-call block reproduces *after* its glyph row:
/// the rail in `metal`, then blanks out to [`BLOCK_BODY`].
///
/// Takes the metal rather than a [`Rail`] because the head of a block is drawn
/// by the v2 event renderer, whose rail wears its **event kind's** colour —
/// silver-dim for a read, gold for a mutation — and not one of `Rail`'s three
/// tones. A body row that hard-coded the result's muted tone would leave a
/// `read_file`'s block with a muted head above a gold body: one block, two
/// metals, which is exactly the thing the rail exists to stop.
pub(crate) fn block_margin(metal: Style) -> Vec<Span<'static>> {
    vec![
        Span::styled(RAIL, metal),
        Span::raw(" ".repeat(BLOCK_BODY - RAIL_W)),
    ]
}

/// Which rail a transcript row rides. The glyph is the row's type signature —
/// a reader scanning the margin sees the shape of the session (calls, their
/// outcomes, prose, prompts) before reading any content.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Rail {
    /// A tool result, in the metal of the **call** above it
    /// ([`crate::v2::transcript_source::head_metal`]) — one event, one rail.
    ///
    /// The colour is carried rather than chosen in [`Rail::style`] for the
    /// reason [`block_margin`] already gives about body rows: the head is drawn
    /// by the v2 event renderer and wears its event kind's metal, and this
    /// function cannot see which tool ran. It applied to the result's own glyph
    /// row too, and did not reach it — so an `edit_file` block drew a gold rail
    /// for exactly one row and a fixed muted one for every row beneath (#4127).
    Result(Color),
    /// A failed tool result. Distinct glyph *and* column-2 position, so a
    /// failure is findable by margin-scan alone — and legible with the colour
    /// switched off, which SPEC 13 requires and a red-only rail would not give.
    /// The one metal that overrides its call's: a call that failed has stopped
    /// being the kind of thing its verb claims.
    Fail,
    /// A user prompt — the strongest landmark in the scrollback, since it is
    /// where a reader re-orients when scrolling back.
    User,
    /// Assistant prose. Deliberately glyph-less: prose is the default voice of
    /// the transcript, and a marker on every paragraph would be noise.
    Agent,
}

impl Rail {
    /// Whether this rail belongs to a tool-call block, and so opens every one
    /// of its rows with [`RAIL`].
    pub(crate) fn railed(self) -> bool {
        matches!(self, Rail::Result(_) | Rail::Fail)
    }

    /// The literal prefix this rail prints before a row's first line.
    ///
    /// A block rail is `RAIL` + space + glyph + space, which puts the glyph in
    /// the same cell as the v2 head's and the content in the same column: a
    /// call, its result and its body then share one left edge instead of three.
    pub(crate) fn prefix(self) -> &'static str {
        match self {
            Rail::Result(_) => " │ ⎿ ",
            Rail::Fail => " │ ✗ ",
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

    /// What every line of this rail's row *after* the first opens with: blanks
    /// out to [`Rail::indent`] for an unrailed row, and the rail itself for a
    /// block row.
    ///
    /// A wrapped result whose continuation was blank punched a hole in the rail
    /// at exactly the rows a wide `bash` result produces most of — so the margin
    /// stopped being scannable on the output it was built to index.
    pub(crate) fn continuation(self) -> Vec<Span<'static>> {
        if !self.railed() {
            return vec![Span::raw(" ".repeat(self.indent()))];
        }
        block_margin(self.style())
    }

    /// The style the rail glyph itself renders in. Content styling is the
    /// caller's; the glyph is always the rail's own semantic color so the
    /// margin reads consistently even when content colors vary.
    pub(crate) fn style(self) -> Style {
        match self {
            Rail::Result(metal) => Style::new().fg(metal),
            Rail::Fail => Style::new().fg(theme::DANGER),
            Rail::User => Style::new().fg(theme::VIOLET).add_modifier(Modifier::BOLD),
            Rail::Agent => Style::new(),
        }
    }
}

/// Replace every tab in `line` with the spaces that reach the next tab stop.
///
/// ratatui does not render control characters — `Span::styled_graphemes`
/// filters them out of the grapheme stream entirely — so a tab is not narrowed
/// and not drawn zero-width, it is **deleted**. `read_file` separates its
/// line-number gutter from the source with one (`stella-tools`'s
/// `{line_num:>6}\t{line}`), which is why an unindented source line reached the
/// screen as `780#[test]` with no gap at all, while an indented one looked
/// right by accident — its own leading spaces stood in for the separator
/// (#4020).
///
/// Expanded here rather than at the emitter because that payload is also read
/// by the model and by the export surfaces, where a tab is the cheaper wire
/// form; this is a fact about *drawing* a line, so it belongs to the drawing
/// layer. Expanded before [`wrap_one_indent`] measures, because
/// `UnicodeWidthChar` gives a tab width 0 as well: measuring first would
/// inherit the very hole this closes, and the deck's wrap arithmetic would
/// disagree with what ratatui finally draws.
///
/// The *stop* is [`stella_transcript::tabs`]'s, not this file's: the grid
/// renderer had the same hole in its own width arithmetic (#4036), and one file
/// must not indent differently in the deck and in an exported transcript. The
/// column is counted from the true screen position — rail prefix and body
/// indent included, since both are already spans on the line by the time this
/// runs.
fn expand_tabs(line: &mut Line<'static>) {
    if !line.spans.iter().any(|s| s.content.contains('\t')) {
        return;
    }
    let mut col = 0usize;
    for span in &mut line.spans {
        let expanded = stella_transcript::tabs::expand(span.content.as_ref(), col);
        col += UnicodeWidthStr::width(expanded.as_ref());
        if let std::borrow::Cow::Owned(text) = expanded {
            span.content = text.into();
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
    wrap_one_lead(line, max_width, &[Span::raw(" ".repeat(indent))], out);
}

/// [`wrap_one_indent`] for a continuation margin that is not blank — a
/// tool-block row, whose wrapped lines must reproduce [`RAIL`] rather than the
/// spaces that would break it.
///
/// `lead` is the whole margin, so its display width *is* the indent; the two
/// cannot be passed separately and disagree.
pub(crate) fn wrap_one_lead(
    mut line: Line<'static>,
    max_width: usize,
    lead: &[Span<'static>],
    out: &mut Vec<Line<'static>>,
) {
    let indent: usize = lead
        .iter()
        .map(|s| UnicodeWidthStr::width(&*s.content))
        .sum();
    // Before the width is taken: see [`expand_tabs`] on why measuring a tab
    // and drawing a tab are the same hole.
    expand_tabs(&mut line);
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
                let mut spans = lead.to_vec();
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
/// one deliberate exception: [`TranscriptEntry::Evicted`](crate::model::TranscriptEntry::Evicted) is a system note
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

/// Emit one body row of a tool-call block — a preview line, an expanded
/// (ctrl+o) body line, the `⋯ N lines` affordance.
///
/// Rides `margin` — [`Rail::continuation`] for a row under a v1 rail,
/// [`block_margin`] for one under the v2-drawn head — so the row sits at the
/// block's content column and the rail runs unbroken past it.
///
/// The margin is passed rather than derived because the three things a block
/// body can hang from wear three different metals: a success recedes to muted,
/// a failure stands in danger, and a call's own arguments take the event kind's
/// colour from the v2 head above them. Deriving any one of those here would
/// give one of the three a rail that changes colour halfway down.
pub(crate) fn push_detail_line(
    margin: &[Span<'static>],
    text: &str,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    push_detail_spans(
        margin,
        vec![Span::styled(text.to_owned(), Style::new().fg(theme::MUTED))],
        width,
        out,
    );
}

/// [`push_detail_line`] for content that is already styled — a syntax-colored
/// JSON body, where the muted single style of the plain form would erase the
/// coloring it exists to show. Indent, column, and wrapping are identical, so
/// the two forms interleave in one body without a seam.
pub(crate) fn push_detail_spans(
    margin: &[Span<'static>],
    content: Vec<Span<'static>>,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    let mut spans = margin.to_vec();
    spans.extend(content);
    wrap_one_lead(Line::from(spans), width, margin, out);
}

/// Emit a system-note row: `↻ retry`, `⇣ compacted`, `✗ error` and friends.
///
/// These rows *are* their own rail — each label already opens with a glyph in
/// column 0, so it indexes the margin exactly like [`Rail::Result`] does without
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
    // The louder seam, not the hairline. A stage rule is the transcript's
    // coarsest structure; drawn at 1.26:1 it was invisible on every screen
    // that was not the author's, and the label set into it inherited the
    // dimness of the line it sat on.
    let hair = Style::new().fg(theme::HAIRLINE_STRONG);
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
///
/// A tinted row is padded out to `width` so the add/remove ground reads as a
/// **band** rather than as a shape traced around the ragged end of the code
/// (SPEC 6.4). The tint is read off the row's own marker cell rather than passed
/// in, so a renderer that stops tinting stops padding and the two can never
/// disagree about which rows are changed ones.
///
/// SPEC 6.4 words this as `Line.style` carrying the tint while spans keep their
/// syntax foreground. The deck cannot say it that way: `Line.style` paints the
/// whole row, including the `margin` prepended here — and that margin is the
/// **call's** metal, not the hunk's. Padding a trailing span is the same two
/// layers with the margin left out of the second one (#4195).
pub(crate) fn push_diff_line(
    margin: &[Span<'static>],
    line: Line<'static>,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    // The marker cell (`+`/`-`/` `) is the first span after the gutter, and the
    // one that always carries the row's *base* tint — a word-level emphasis span
    // carries the brighter `*_EMPH` ground instead, so reading the tint off any
    // old span would pad a changed row in the wrong colour.
    let tint = line.spans.get(1).and_then(|s| s.style.bg);
    let mut spans = margin.to_vec();
    spans.extend(line.spans);
    // The one row that reaches the buffer without passing through
    // [`wrap_one_indent`], and so the one that needs [`expand_tabs`] by name: a
    // tab-indented file's diff would otherwise lose its indentation entirely.
    let mut row = Line::from(spans);
    expand_tabs(&mut row);
    if let Some(tint) = tint {
        let used = row.width();
        if used < width {
            row.spans.push(Span::styled(
                " ".repeat(width - used),
                Style::new().bg(tint),
            ));
        }
    }
    out.push(row);
}

/// Multi-line form of [`push_row`]: the rail glyph prefixes the first line and
/// every following line indents to the rail's content column.
pub(crate) fn push_row_block(
    rail: Rail,
    lines: Vec<Line<'static>>,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    let cont = rail.continuation();
    for (i, line) in lines.into_iter().enumerate() {
        let mut spans = if i == 0 {
            vec![Span::styled(rail.prefix().to_string(), rail.style())]
        } else {
            cont.clone()
        };
        spans.extend(line.spans);
        wrap_one_lead(Line::from(spans), width, &cont, out);
    }
}

/// Widest the `content … metric` span grows before the metric stops tracking
/// the pane edge. Chosen above any ordinary transcript width so normal panes
/// stay flush-right and only genuinely wide ones are reined in.
pub(crate) const METRIC_SPAN: usize = 140;

/// Lines of a *failed* result shown without expanding. Enough for a compiler
/// error with its location and caret line, or the top of a panic backtrace.
pub(crate) const FAIL_PREVIEW: usize = 6;

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
