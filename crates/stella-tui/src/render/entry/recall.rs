//! The context-recall table.
//!
//! A recall is a handful of records with four attributes each — a small table —
//! and it used to render as its labels comma-joined into a paragraph that
//! wrapped mid-word at the pane edge. The reader could not tell where one record
//! ended and the next began, could not tell an 800-token episodic memory from a
//! 60-token graph symbol, and had no way to ask for more: `ctrl+o` did not apply
//! to the entry, and there was nothing left in the read-model for it to reveal.
//!
//! So: a header row that summarises, and one aligned row per frame. Alignment is
//! the whole point — with the per-frame cost in a right-hand column, the one
//! frame eating two thirds of the budget is visible by scanning a strip of
//! digits, which is a finding no paragraph can carry.
//!
//! # Alignment is a property of the construction, not a convention
//!
//! Every cell on every row goes through [`cell`], which returns a string of
//! *exactly* its column's display width — never fewer columns, never more. That
//! is the difference between a table and a run of `format!` calls that usually
//! line up: a cell physically cannot displace the cell to its right, so no call
//! site has to remember to pad, and no future field can quietly break the grid.
//!
//! The measurement is in **display columns** throughout ([`unicode_width`]),
//! because that is the unit a terminal lays out in. Sizing in `char`s is the
//! trap this module is built to avoid, and it is invisible in ASCII: a citation
//! label of 33 CJK characters is 33 `char`s and 66 columns, so a `char`-budgeted
//! elision fits the budget, doubles the cell, and pushes the location column
//! thirty columns to the right on that row alone. Recalled labels are model- and
//! user-authored text, so this is a real input, not a hypothetical one.
//!
//! # The parts that are deliberately not a grid
//!
//! Deliberately **no rail glyph** on a frame row. [`Rail::Result`]'s `⎿` means
//! "the outcome of the call above", and borrowing it here would put five false
//! hits per turn into the margin a reader scans for "what ran and how did it
//! go". Recall is bookkeeping — the same judgement [`quiet`] documents — so it
//! takes the subordinate body column and recedes.
//!
//! The column header and its rule are emitted **only under `ctrl+o`**. Collapsed,
//! the block is held to the height of the paragraph it replaced (see
//! [`RECALL_PREVIEW`]), and two rows of chrome would spend that budget on
//! headings instead of on frames; expanded, there is no height contract and the
//! columns are worth naming.
//!
//! The `expanded` half of this is one contract in two places: the arms below and
//! `deck_ui::is_expandable`, which decides whether `ctrl+o` even reaches an
//! entry. They had already drifted — recall was missing from that list *and*
//! ignored the flag here, so the transcript's "there is more behind this"
//! affordance did nothing on the row with the most behind it. Teaching a new
//! variant means teaching both halves.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{plural, quiet, value};
use crate::model::{RecallBudget, RecalledFrameRow};
use crate::render::row::*;
use crate::theme;

/// Frames shown before the fold. Sized so the collapsed block is no taller
/// than the wrapped paragraph it replaces (four rows, in the pane that
/// prompted this) while being scannable rather than run together.
const RECALL_PREVIEW: usize = 3;

/// Widest a citation label grows before it is elided. Labels are heterogeneous
/// by nature — `fn review` beside a whole recalled user prompt — so the column
/// is capped rather than sized to the widest, which one episodic memory would
/// otherwise push past the pane edge on its own.
const RECALL_LABEL_MAX: usize = 34;

/// Narrowest a label column shrinks to before the location is dropped instead.
/// Below this a label is not a citation, it is an initial.
const RECALL_LABEL_MIN: usize = 12;

/// Widest a location grows before it is left-elided.
const RECALL_LOCATION_MAX: usize = 38;

/// Narrowest a location column is worth keeping. A `path:line` shorter than
/// this is all ellipsis and no path.
const RECALL_LOCATION_MIN: usize = 14;

/// Widest the kind column grows. Sized for the longest kind the protocol ships
/// (`snippet`) plus room to tell a longer one apart; a kind past this is elided
/// rather than allowed to shift the row, because `kind` is wire text and
/// nothing bounds it upstream.
const RECALL_KIND_MAX: usize = 9;

/// Narrowest the kind column shrinks to. A block of `fact` frames should not
/// render a four-column header under a six-column heading.
const RECALL_KIND_MIN: usize = 6;

/// Widest a provider id grows in the budget breakdown before it is elided,
/// sized for the two legs that ship (`workspace-memory`, `code-graph`) with
/// room for a third.
const RECALL_LEG_MAX: usize = 20;

/// Column for a recall's second-level detail — a frame's provenance chain, a
/// budget leg. One step past [`BODY`], so the hierarchy reads without a rule.
const RECALL_DETAIL: usize = BODY + 2;

/// The gutter between two columns. Two columns rather than one, because a
/// single space between an elided label ending in `…` and a location starting
/// in `…` reads as one token rather than two cells.
const COL_GAP: &str = "  ";

/// [`COL_GAP`] as a width, for the fitting arithmetic.
const GAP_W: usize = 2;

/// The transcript rows for one context recall.
//
// The lint is wrong here, and the alternative it asks for is the defect this
// entry kind already suffered once. These nine parameters are not an
// accumulation: they are exactly the fields of `TranscriptEntry::ContextRecall`
// plus the two render inputs and the sink, destructured by the one match arm
// that knows the variant's shape. Bundling them into a parameter struct would
// put a *second* shape between the read-model and the renderer — a third place
// for a field to be dropped, which is precisely how `latency_ms` and
// `used_ann_index` reached no surface at all. `model/recall.rs`'s `Projected`
// alias declines the same invitation for the same reason.
#[allow(clippy::too_many_arguments)]
pub(super) fn recall_lines(
    frames: &[RecalledFrameRow],
    tokens: u32,
    latency_ms: u32,
    used_ann_index: Option<bool>,
    providers: &[(String, u32)],
    budget: Option<&RecallBudget>,
    expanded: bool,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    let dim = Style::new().fg(theme::MUTED);

    // Header: the two numbers that are always worth having, then the two that
    // say whether recall was the reason the turn felt slow.
    let mut head = vec![Span::styled(
        format!(
            "{} · {tokens} tok",
            plural(frames.len() as u64, "frame", "frames")
        ),
        value(),
    )];
    // `0` means *not measured* on the wire, never "instant", so printing `0ms`
    // would invent a measurement. Omitting it says only what is known.
    if latency_ms > 0 {
        head.push(Span::styled(
            format!("  ·  {}", human_duration(u64::from(latency_ms))),
            quiet(),
        ));
    }
    // Tri-state, and rendered tri-state: `ann` (the index fired), `scan` (it
    // did not), nothing at all (no recall path reported). A `bool` here would
    // print `scan` on every real turn and read as "the index never fires".
    if let Some(ann) = used_ann_index {
        head.push(Span::styled(
            if ann { "  ·  ann" } else { "  ·  scan" }.to_string(),
            quiet(),
        ));
    }
    push_note("◉ recalled", quiet(), head, width, out);

    let shown = if expanded {
        frames.len()
    } else {
        RECALL_PREVIEW.min(frames.len())
    };
    // One column layout for the whole block, computed once. Per-row layout
    // would let each row pick a different label width and the table would stop
    // being a table — alignment is the entire reason this is not a paragraph.
    let cols = RecallColumns::fit(&frames[..shown], width);
    if expanded {
        push_recall_head(&cols, width, out);
    }
    for frame in &frames[..shown] {
        push_recall_row(
            justify(
                recall_frame_spans(frame, &cols),
                vec![Span::styled(cols.metric_cell(frame.tokens), dim)],
                width,
                BODY,
            ),
            BODY,
            width,
            out,
        );
        // Expanded only: the provenance chain, which is the whole reason a
        // detail view exists here. `provider ← source` is deliberately two
        // fields — an adapter fronting another store (`workspace-memory` over
        // `stella-context`) is exactly the case a single field would hide.
        //
        // Prose, not a row of cells: a chain is read left to right in one go,
        // and padding its four heterogeneous parts into columns would align
        // fields nobody compares down a column.
        if expanded {
            push_recall_row(
                vec![Span::styled(recall_provenance(frame), dim)],
                RECALL_DETAIL,
                width,
                out,
            );
        }
    }

    if !expanded {
        let hidden = frames.len() - shown;
        // `⋯` is this UI's one glyph for "there is more behind this", and it
        // carries the ctrl+o affordance everywhere else in the file. The row
        // is emitted even at zero hidden frames, because provenance and the
        // budget report are *always* behind the fold and an affordance nobody
        // knows about is the same as no affordance.
        //
        // The hidden frames' token cost rides on it, and that is the whole
        // reason the fold can afford to keep the host's render order rather
        // than promoting the expensive frames into the preview. In the recall
        // that prompted this work the two folded frames carried 908 of 1155
        // tokens — reordering would have shown the outlier while lying about
        // what the model actually saw; naming the number shows it and does not.
        let more = if hidden > 0 {
            let cost: u32 = frames[shown..].iter().map(|f| f.tokens).sum();
            format!("⋯ {hidden} more · {cost} tok · ctrl+o for provenance and budget")
        } else {
            "⋯ ctrl+o for provenance and budget".to_string()
        };
        push_recall_row(
            vec![Span::styled(more, Style::new().fg(theme::TEXT_TERTIARY))],
            BODY,
            width,
            out,
        );
        return;
    }

    // The bill. One row per leg rather than one long joined line: the legs are
    // the same shape as the frame rows above them, so the same scan works on
    // both, and a joined line wrapped at any ordinary pane width — splitting
    // `2 rejected` across two rows, which is precisely the phrase this exists
    // to surface.
    //
    // When a budget report is present it *subsumes* the provider mix: the mix
    // counts frames that won fusion and reached the prompt, the report counts
    // what each leg served and what the host rejected. Showing both puts two
    // similar-looking counts side by side that mean different things, and the
    // report is the strict superset.
    match budget {
        Some(b) => {
            push_recall_row(
                vec![Span::styled(
                    format!("budget {} of {} tok", b.consumed, b.requested),
                    dim,
                )],
                BODY,
                width,
                out,
            );
            let legs = LegColumns::fit(&b.providers);
            for (provider, served, rejected, tok) in &b.providers {
                push_recall_row(
                    vec![Span::styled(
                        legs.leg_row(provider, *served, *rejected, *tok),
                        dim,
                    )],
                    RECALL_DETAIL,
                    width,
                    out,
                );
            }
        }
        // No report — say which legs the frames came from, which is all the
        // event carries when no CGP host produced a usage report.
        None if !providers.is_empty() => {
            let mix = providers
                .iter()
                .map(|(p, n)| format!("{p} {n}"))
                .collect::<Vec<_>>()
                .join(" · ");
            push_recall_row(
                vec![Span::styled(format!("via {mix}"), dim)],
                BODY,
                width,
                out,
            );
        }
        None => {}
    }
}

/// Emit one recall row at `indent`, wrapping to the same column.
///
/// [`push_detail_line`] would do this but takes a `&str` in one fixed style,
/// and a recall row is a styled composite — a tertiary kind, a white citation,
/// a dim path, a dim metric flushed right.
fn push_recall_row(
    spans: Vec<Span<'static>>,
    indent: usize,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    let mut row = vec![Span::raw(" ".repeat(indent))];
    row.extend(spans);
    wrap_one_indent(Line::from(row), width, indent, out);
}

/// The column heading and its rule, under `ctrl+o` only.
///
/// The rule is drawn per column rather than as one hairline across the pane, so
/// it *shows* the grid instead of merely underlining it: each segment is exactly
/// its column's width, which makes a column that has drifted visible in the rule
/// itself rather than only in the rows below it.
fn push_recall_head(cols: &RecallColumns, width: usize, out: &mut Vec<Line<'static>>) {
    let head = Style::new().fg(theme::TEXT_TERTIARY);
    let hair = Style::new().fg(theme::HAIRLINE);

    let mut labels = vec![Span::styled(cell("kind", cols.kind), head)];
    let mut rules = vec![Span::styled("─".repeat(cols.kind), hair)];
    for (text, w) in cols.text_columns() {
        labels.push(Span::raw(COL_GAP));
        labels.push(Span::styled(cell(text, w), head));
        rules.push(Span::raw(COL_GAP));
        rules.push(Span::styled("─".repeat(w), hair));
    }
    // The heading of a right-aligned column is right-aligned too, or the word
    // names a column its own digits do not sit under.
    push_recall_row(
        justify(
            labels,
            vec![Span::styled(
                format!("{:>w$}", "cost", w = cols.metric),
                head,
            )],
            width,
            BODY,
        ),
        BODY,
        width,
        out,
    );
    push_recall_row(
        justify(
            rules,
            vec![Span::styled("─".repeat(cols.metric), hair)],
            width,
            BODY,
        ),
        BODY,
        width,
        out,
    );
}

/// The left half of a frame row: `kind`, then the citation, then where it came
/// from in the tree.
///
/// The kind is the field the old rendering lost and the one that changes how a
/// row is read — a `memory` and a `symbol` cost the prompt the same tokens and
/// mean entirely different things about what retrieval did.
fn recall_frame_spans(frame: &RecalledFrameRow, cols: &RecallColumns) -> Vec<Span<'static>> {
    let dim = Style::new().fg(theme::MUTED);
    let mut spans = vec![Span::styled(
        cell(frame_kind(frame), cols.kind),
        Style::new().fg(theme::TEXT_TERTIARY),
    )];
    spans.push(Span::raw(COL_GAP));
    let Some(uri) = frame.uri.as_deref().filter(|_| cols.location > 0) else {
        // Last cell on the row: elided to its column, but not padded to it.
        // Trailing blanks in a final column are invisible on screen and only
        // cost the row width `justify` measures.
        spans.push(Span::styled(elide(&frame.label, cols.label), value()));
        return spans;
    };
    spans.push(Span::styled(cell(&frame.label, cols.label), value()));
    spans.push(Span::raw(COL_GAP));
    // The location, elided from the *left*: a recall row is actionable because
    // of its filename and line, and clipping from the right — which is what the
    // pane edge did — removes exactly that and keeps the repo prefix every row
    // already shares.
    spans.push(Span::styled(
        cell(&recall_location(uri, cols.location), cols.location),
        dim,
    ));
    spans
}

/// A frame's kind as it renders. An empty kind is a stream recorded before the
/// field existed; `frame` says that honestly, where a blank cell would read as
/// a rendering bug.
fn frame_kind(frame: &RecalledFrameRow) -> &str {
    if frame.kind.is_empty() {
        "frame"
    } else {
        frame.kind.as_str()
    }
}

/// The column widths one recall block renders at, in display columns.
///
/// Computed from the pane rather than fixed, because the two elastic columns
/// fail in opposite directions when space runs out. A citation label is prose
/// and elides gracefully — half of `create a new minor release…` still says
/// what it is. A location does not: it is a `path:line`, and the tail is the
/// entire point. So the location is budgeted **first** and the label absorbs
/// the pressure.
///
/// Leaving this to [`justify`] is the bug this type exists to fix. `justify`
/// truncates its left column from the *right* to make room for the metric,
/// which is exactly backwards here: in a 100-column deck pane it cut
/// `crates/stella-core/src/driver.rs:88` down to
/// `crates/stella-core/src/driver.r…`, deleting the filename and line while
/// keeping the repo prefix every row on screen already shares.
struct RecallColumns {
    /// Fitted to the kinds actually present, so a block of `symbol` frames does
    /// not reserve a column for the longest kind the protocol can name — and
    /// bounded, so a kind from a future provider cannot shift the whole row.
    kind: usize,
    label: usize,
    /// `0` when the pane cannot afford a location column that would still say
    /// something — dropped whole rather than rendered as an ellipsis.
    location: usize,
    /// Width of the whole `{n} tok` cell, so the metric reads as one strip a
    /// reader can run an eye down and its heading sits over its own digits.
    metric: usize,
}

impl RecallColumns {
    fn fit(frames: &[RecalledFrameRow], width: usize) -> Self {
        let digits = frames
            .iter()
            .map(|f| f.tokens.to_string().len())
            .max()
            .unwrap_or(1);
        let metric = digits + " tok".len();
        let kind = frames
            .iter()
            .map(|f| display_width(frame_kind(f)))
            .max()
            .unwrap_or(RECALL_KIND_MIN)
            .clamp(RECALL_KIND_MIN, RECALL_KIND_MAX);

        // What `justify` will leave the left column: the pane, less the body
        // indent, less the metric and the one space it insists on.
        let avail = width.saturating_sub(BODY).saturating_sub(metric + 1);
        let rest = avail.saturating_sub(kind + GAP_W);

        // The widest location any frame in *this* block needs, so a block of
        // short paths does not reserve a column for a long one that is absent.
        let widest = frames
            .iter()
            .filter_map(|f| f.uri.as_deref())
            .map(|u| display_width(&recall_location(u, RECALL_LOCATION_MAX)))
            .max()
            .unwrap_or(0);

        // The label keeps its minimum before the location gets anything; below
        // that the location is dropped entirely rather than both being starved
        // into two columns of ellipsis.
        let for_location = rest.saturating_sub(RECALL_LABEL_MIN + GAP_W);
        let location = if widest == 0 || for_location < RECALL_LOCATION_MIN {
            0
        } else {
            widest.min(for_location)
        };
        let gap = usize::from(location > 0) * GAP_W;
        let label = rest
            .saturating_sub(location + gap)
            .clamp(RECALL_LABEL_MIN, RECALL_LABEL_MAX);
        Self {
            kind,
            label,
            location,
            metric,
        }
    }

    /// The text columns after `kind`, paired with their headings. The location
    /// is absent at narrow widths, and everything that lays the table out —
    /// rows, heading, rule — reads the grid from here rather than re-deciding
    /// which columns exist.
    fn text_columns(&self) -> Vec<(&'static str, usize)> {
        let mut cols = vec![("citation", self.label)];
        if self.location > 0 {
            cols.push(("location", self.location));
        }
        cols
    }

    /// `  82 tok` — the metric cell, right-aligned within its column.
    fn metric_cell(&self, tokens: u32) -> String {
        format!("{tokens:>w$} tok", w = self.metric - " tok".len())
    }
}

/// The column widths the budget breakdown renders at.
///
/// A separate grid from the frame table above it on purpose: these are legs,
/// not frames, and forcing one set of columns onto both would size a provider
/// id against a citation label.
///
/// It is also the one table here that does **not** flush its metric to the pane
/// edge. Two or three legs of short text with their cost sixty columns away
/// stop reading as rows at all — a scan column only works while the eye can
/// hold both ends in one saccade, which is the same limit [`METRIC_SPAN`]
/// encodes for the transcript at large. So the legs close up into a compact
/// grid, and the gutter between their last column and the pane is dead space
/// rather than a column edge.
struct LegColumns {
    provider: usize,
    served: usize,
    /// `0` when no leg rejected anything: the column is dropped whole rather
    /// than drawn as a row of zeroes, the same judgement the location column
    /// makes. A number here is the only visible evidence that a provider
    /// misdeclared its cost — a rejected frame never reaches the frame list —
    /// so the column earns its width exactly when it is not empty.
    rejected: usize,
    metric: usize,
}

impl LegColumns {
    fn fit(legs: &[(String, u32, u32, u64)]) -> Self {
        Self {
            provider: legs
                .iter()
                .map(|(p, ..)| display_width(p))
                .max()
                .unwrap_or(0)
                .min(RECALL_LEG_MAX),
            served: legs
                .iter()
                .map(|(_, served, _, _)| served.to_string().len())
                .max()
                .unwrap_or(1),
            rejected: legs
                .iter()
                .filter(|(_, _, rejected, _)| *rejected > 0)
                .map(|(_, _, rejected, _)| rejected.to_string().len())
                .max()
                .unwrap_or(0),
            metric: legs
                .iter()
                .map(|(_, _, _, tok)| tok.to_string().len())
                .max()
                .unwrap_or(1)
                + " tok".len(),
        }
    }

    /// One leg row. Each count carries its own noun rather than leaning on a
    /// heading, because two legs do not make a table worth two rows of chrome —
    /// but the digits are still right-aligned in their own column, which is the
    /// half a `·`-joined line could not do: `4 served · 343 tok` beside
    /// `1 served · 2 rejected · 812 tok` put the two costs in different places
    /// on the one row where comparing them is the entire point.
    fn leg_row(&self, provider: &str, served: u32, rejected: u32, tokens: u64) -> String {
        let mut row = format!(
            "{}{COL_GAP}{served:>w$} served",
            cell(provider, self.provider),
            w = self.served
        );
        if self.rejected > 0 {
            // A leg that rejected nothing leaves the cell blank rather than
            // writing `0 rejected`: the column exists to be scanned for the
            // rows that have one, and a column of zeroes hides them again.
            let cell_w = self.rejected + " rejected".len();
            match rejected {
                0 => row.push_str(&" ".repeat(GAP_W + cell_w)),
                n => row.push_str(&format!("{COL_GAP}{n:>w$} rejected", w = self.rejected)),
            }
        }
        row.push_str(COL_GAP);
        row.push_str(&format!("{tokens:>w$} tok", w = self.metric - " tok".len()));
        row
    }
}

/// The dim provenance line under an expanded frame row.
fn recall_provenance(frame: &RecalledFrameRow) -> String {
    let mut parts = Vec::new();
    if frame.provider.is_empty() {
        parts.push(frame.source.clone());
    } else if frame.provider == frame.source || frame.source.is_empty() {
        parts.push(frame.provider.clone());
    } else {
        parts.push(format!("{} ← {}", frame.provider, frame.source));
    }
    if let Some(m) = &frame.method {
        parts.push(m.clone());
    }
    if let Some(id) = &frame.id {
        parts.push(id.clone());
    }
    // A missing digest is not nothing: per the context-reuse spec such a frame
    // is *not verifiable* and a host must re-query rather than reuse it. Saying
    // so is the point of showing the field at all.
    match &frame.digest {
        Some(d) => parts.push(short_digest(d)),
        None => parts.push("unverifiable (no digest)".to_string()),
    }
    parts.join(" · ")
}

/// `sha256:9f2c1ab…` — enough to compare two frames by eye, short enough to
/// share a row with the rest of the provenance chain.
fn short_digest(digest: &str) -> String {
    match digest.split_once(':') {
        Some((algo, hex)) => format!("{algo}:{}…", &hex[..hex.len().min(7)]),
        None => format!("{}…", &digest[..digest.len().min(14)]),
    }
}

/// A frame's URI as a location a reader can act on, within `cap` columns.
///
/// Left-elided, which is the opposite of what the pane edge did to it: the tail
/// (`…/command_deck/hunk_gate.rs:32`) is the part that identifies the frame,
/// and the head is a repo prefix every row on screen already shares.
fn recall_location(uri: &str, cap: usize) -> String {
    // Strip a scheme so `file:///…` and a bare path render alike; the scheme
    // is never the discriminating part of a recall row.
    let path = uri.split_once("://").map_or(uri, |(_, rest)| rest);
    if display_width(path) <= cap {
        return path.to_string();
    }
    if cap == 0 {
        return String::new();
    }
    let tail = take_right(path, cap - 1);
    // Cut on a separator so the elision lands between path segments rather
    // than mid-directory-name — but only when that leaves a real path behind.
    // Snapping to a separator that sits near the end would trade a readable
    // `…re/src/driver.rs:88` for a useless `…/driver.rs:88` of the wrong
    // width, so the snap is taken only in the leading third.
    //
    // The leading third is measured in columns, not bytes: `find` returns a
    // byte offset, and comparing that against a column budget silently refuses
    // the snap on any path with a multi-byte segment in front of the cut.
    match tail
        .find('/')
        .filter(|cut| display_width(&tail[..*cut]) <= cap / 3)
    {
        Some(cut) => format!("…{}", &tail[cut..]),
        None => format!("…{tail}"),
    }
}

/// One table cell: `text` in exactly `col` display columns, elided if it does
/// not fit and space-padded if it does.
///
/// This is the invariant the whole table rests on, and it is deliberately the
/// opposite of how a *row* pads. The transcript's call rows used to pad the
/// tool-name field to a soft column that an over-wide name simply overran,
/// because identity outranks alignment on a row nobody reads down a column.
/// (That helper went with the v1 call row in #4127; SPEC 6.2's head sets its
/// own columns.) Here the row *is* read down a column, so a cell that overran
/// would displace every cell to its right on that row and only that row, which
/// is the one failure a table cannot survive.
fn cell(text: &str, col: usize) -> String {
    let text = elide(text, col);
    format!(
        "{text}{}",
        " ".repeat(col.saturating_sub(display_width(&text)))
    )
}

/// Truncate to `cap` display columns with a trailing `…`.
///
/// The `…` is one column, so the kept text is budgeted `cap - 1`. A cut that
/// lands mid-wide-character keeps the narrower text and lets [`cell`] pad the
/// hole, rather than emitting a cell one column over its budget.
fn elide(text: &str, cap: usize) -> String {
    if display_width(text) <= cap {
        return text.to_string();
    }
    if cap == 0 {
        return String::new();
    }
    let kept = take_left(text, cap - 1);
    format!("{}…", kept.trim_end())
}

/// The longest prefix of `text` that fits in `cap` display columns.
fn take_left(text: &str, cap: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > cap {
            break;
        }
        used += w;
        out.push(ch);
    }
    out
}

/// The longest suffix of `text` that fits in `cap` display columns.
fn take_right(text: &str, cap: usize) -> String {
    let mut kept: Vec<char> = Vec::new();
    let mut used = 0;
    for ch in text.chars().rev() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > cap {
            break;
        }
        used += w;
        kept.push(ch);
    }
    kept.iter().rev().collect()
}

/// Columns `text` occupies on a terminal — the unit every width here is in.
fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}
