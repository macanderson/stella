//! Word-level highlighting inside a modified line pair.
//!
//! A line-level diff answers "did this line change". It does not answer the
//! question a reader actually has, which is *what* in it changed — and on a
//! long line the difference between those two answers is the whole value of
//! looking at the diff. `\setlength{\parindent}{15pt}` against
//! `\setlength{\parindent}{12pt}` is one changed token in thirty; a renderer
//! that tints the whole line has told the reader nothing they did not already
//! know from the `−`/`+` sigil.
//!
//! ## Tokenisation
//!
//! Split on word boundaries: a token is either a maximal run of alphanumerics
//! and `_`, or a single other character. Whitespace is its own token so that
//! re-indentation shows up rather than being absorbed into a neighbour.
//!
//! Punctuation-as-single-tokens matters more than it looks in this codebase's
//! actual inputs — LaTeX, JSON and Rust are all mostly punctuation, and a
//! tokeniser that lumped `}{` together would report a change in
//! `{\parindent}{15pt}` as spanning the brace run.
//!
//! ## Character-level fallback
//!
//! Token granularity over-reports when the real change is a fragment of a word:
//! `--fast` → `--fast2` is one token replaced, and tinting the whole flag hides
//! which end moved. So when the token-level result puts a changed run of fewer
//! than three characters against a same-length neighbour, that pair is re-diffed
//! at character granularity.
//!
//! ## The all-changed guard
//!
//! If word highlighting would tint essentially the whole line, it is dropped:
//! the line tint already carries "this line changed", and a second tint over all
//! of it is noise that makes the genuinely-partial changes elsewhere harder to
//! spot. The threshold is [`SATURATION`].
//!
//! ## The length cap
//!
//! Both diffs above are exact LCS, so both are quadratic in time *and* space,
//! and a line has no length limit: one minified bundle or one long JSON object
//! is a single line that tokenises to tens of thousands of tokens. Nothing
//! upstream bounds that — `stella_diff::LCS_AREA_CAP` bounds the *line-count*
//! product of a file diff, which says nothing about what is inside one line —
//! so this module bounds it itself with [`LCS_CELL_CAP`]. Over the cap, the
//! pair degrades to the same plain spans a too-dissimilar pair returns and the
//! line tint carries the signal alone.

/// Above this fraction of a line being "changed", word highlights are dropped
/// as noise and the line tint carries the signal alone.
pub const SATURATION: f64 = 0.8;

/// A changed run shorter than this many characters triggers the character-level
/// re-diff, because at that size a whole-token tint is mostly wrong.
pub const SHORT_RUN: usize = 3;

/// Beyond this many LCS table cells, word highlighting is abandoned for the
/// line tint alone: a pathological line must degrade to a coarse-but-instant
/// answer rather than allocate gigabytes and stall whatever is rendering it.
///
/// 250,000 cells is 500 tokens against 500 tokens — a 1 MiB `u32` table, and
/// about 1.4 ms of fill at the ~5.7 ns per cell this loop measures in release.
/// The number is a legibility boundary before it is a performance one: for the
/// punctuation-dense text this tokeniser is tuned for, 500 tokens is roughly a
/// 500-character line, which already wraps four times in a terminal and is the
/// point past which nobody is reading word by word — so precision there is
/// worth less than the guarantee that one machine-generated line cannot wedge
/// a transcript.
///
/// This is **not** `stella_diff::LCS_AREA_CAP`, which bounds the product of two
/// *line counts* across a file. This one bounds the product of two *token (or
/// character) counts* inside a single changed pair; a file diff can pass that
/// cap and still hand one 50 kB line to this module.
///
/// Public so a caller sizing its inputs can reason about the boundary; the
/// value is not tunable per call by design — one bound, one behaviour.
pub const LCS_CELL_CAP: usize = 250_000;

/// One rendered span of a line: text, plus whether it is the *changed* part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// The span's text.
    pub text: String,
    /// Whether this span gets the stronger tint.
    pub changed: bool,
}

impl Span {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            changed: false,
        }
    }

    fn hot(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            changed: true,
        }
    }
}

/// Split a line into diffable tokens.
#[must_use]
pub fn tokenize(line: &str) -> Vec<&str> {
    let bytes: Vec<(usize, char)> = line.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let (start, ch) = bytes[i];
        if is_word(ch) {
            let mut end = line.len();
            let mut j = i + 1;
            while j < bytes.len() {
                if is_word(bytes[j].1) {
                    j += 1;
                } else {
                    end = bytes[j].0;
                    break;
                }
            }
            out.push(&line[start..end]);
            i = j;
        } else {
            let end = bytes.get(i + 1).map_or(line.len(), |&(idx, _)| idx);
            out.push(&line[start..end]);
            i += 1;
        }
    }
    out
}

fn is_word(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// Highlight one removed/added line pair.
///
/// Returns `(old_spans, new_spans)`. When the pair is too dissimilar to be worth
/// annotating, both sides come back as a single unchanged span and the caller
/// renders line tint alone.
#[must_use]
pub fn highlight(old: &str, new: &str) -> (Vec<Span>, Vec<Span>) {
    if old == new {
        return (vec![Span::plain(old)], vec![Span::plain(new)]);
    }

    let old_tokens = tokenize(old);
    let new_tokens = tokenize(new);
    if over_cap(old_tokens.len(), new_tokens.len()) {
        return (vec![Span::plain(old)], vec![Span::plain(new)]);
    }
    let script = lcs_script(&old_tokens, &new_tokens);

    let (mut old_spans, mut new_spans) = spans_from_script(&script);
    refine_short_runs(&mut old_spans, &mut new_spans);

    if saturated(&old_spans, old) || saturated(&new_spans, new) {
        return (vec![Span::plain(old)], vec![Span::plain(new)]);
    }
    (merge(old_spans), merge(new_spans))
}

/// One entry of the token edit script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edit<'a> {
    Keep(&'a str),
    Remove(&'a str),
    Add(&'a str),
}

/// Whether an LCS table over these two sequence lengths would exceed
/// [`LCS_CELL_CAP`].
///
/// Measures the table [`lcs_script`] actually allocates, `(n + 1) * (m + 1)`,
/// so the cap bounds the allocation exactly rather than approximately. The
/// multiply saturates because overflowing it is the very thing being guarded
/// against: two 5-billion-token sides would wrap to a small product and pass.
fn over_cap(n: usize, m: usize) -> bool {
    (n + 1).saturating_mul(m + 1) > LCS_CELL_CAP
}

/// Exact LCS over tokens — quadratic in both time and space.
///
/// **Every caller must first put its two lengths through [`over_cap`].** Nothing
/// upstream does it for them: [`crate::file_diff`] bounds how many pairs reach
/// this module and `stella_diff::LCS_AREA_CAP` bounds a file's line-count
/// product, but a single minified line is one pair of one line, and its table
/// is billions of cells (#3739).
fn lcs_script<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<Edit<'a>> {
    let (n, m) = (old.len(), new.len());
    let mut table = vec![0u32; (n + 1) * (m + 1)];
    let idx = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[idx(i, j)] = if old[i] == new[j] {
                table[idx(i + 1, j + 1)] + 1
            } else {
                table[idx(i + 1, j)].max(table[idx(i, j + 1)])
            };
        }
    }

    let mut script = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[i] == new[j] {
            script.push(Edit::Keep(old[i]));
            i += 1;
            j += 1;
        } else if table[idx(i + 1, j)] >= table[idx(i, j + 1)] {
            script.push(Edit::Remove(old[i]));
            i += 1;
        } else {
            script.push(Edit::Add(new[j]));
            j += 1;
        }
    }
    script.extend(old[i..].iter().map(|t| Edit::Remove(t)));
    script.extend(new[j..].iter().map(|t| Edit::Add(t)));
    script
}

fn spans_from_script(script: &[Edit<'_>]) -> (Vec<Span>, Vec<Span>) {
    let mut old_spans = Vec::new();
    let mut new_spans = Vec::new();
    for edit in script {
        match *edit {
            Edit::Keep(text) => {
                old_spans.push(Span::plain(text));
                new_spans.push(Span::plain(text));
            }
            Edit::Remove(text) => old_spans.push(Span::hot(text)),
            Edit::Add(text) => new_spans.push(Span::hot(text)),
        }
    }
    (old_spans, new_spans)
}

/// Re-diff a short changed run at character granularity.
///
/// Only fires when both sides have exactly one changed run and at least one of
/// them is under [`SHORT_RUN`] characters — the case where a whole-token tint is
/// demonstrably coarser than the truth. A multi-run pair is left alone: character
/// diffing there produces the speckled highlight this module exists to avoid.
fn refine_short_runs(old_spans: &mut Vec<Span>, new_spans: &mut Vec<Span>) {
    let (Some(old_run), Some(new_run)) = (sole_changed(old_spans), sole_changed(new_spans)) else {
        return;
    };
    let old_text = old_spans[old_run].text.clone();
    let new_text = new_spans[new_run].text.clone();
    let short = old_text.chars().count() < SHORT_RUN || new_text.chars().count() < SHORT_RUN;
    if !short && !shares_affix(&old_text, &new_text) {
        return;
    }

    // A sole changed run can be the whole line — `--fast` → `--fast2` shares an
    // affix, and so does one 50 kB minified line against the next revision of
    // itself. Leaving the token-level spans in place is the right fallback:
    // whatever the line-level pass decided still stands.
    if over_cap(old_text.chars().count(), new_text.chars().count()) {
        return;
    }

    let old_chars: Vec<String> = old_text.chars().map(|c| c.to_string()).collect();
    let new_chars: Vec<String> = new_text.chars().map(|c| c.to_string()).collect();
    let old_refs: Vec<&str> = old_chars.iter().map(String::as_str).collect();
    let new_refs: Vec<&str> = new_chars.iter().map(String::as_str).collect();
    let (refined_old, refined_new) = spans_from_script(&lcs_script(&old_refs, &new_refs));

    old_spans.splice(old_run..=old_run, refined_old);
    new_spans.splice(new_run..=new_run, refined_new);
}

/// Whether two token runs share a leading or trailing character run long enough
/// that character granularity would say something token granularity cannot.
///
/// `--fast` → `--fast2` is the motivating case: both tokens are longer than
/// [`SHORT_RUN`], so the length rule alone leaves the whole flag tinted, when
/// the honest answer is that one character was appended.
fn shares_affix(old: &str, new: &str) -> bool {
    if old.is_empty() || new.is_empty() {
        return false;
    }
    let common_prefix = old
        .chars()
        .zip(new.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let common_suffix = old
        .chars()
        .rev()
        .zip(new.chars().rev())
        .take_while(|(a, b)| a == b)
        .count();
    common_prefix >= SHORT_RUN || common_suffix >= SHORT_RUN
}

/// The index of the only changed span, or `None` when there are zero or many.
fn sole_changed(spans: &[Span]) -> Option<usize> {
    let mut found = None;
    for (i, span) in spans.iter().enumerate() {
        if span.changed {
            if found.is_some() {
                return None;
            }
            found = Some(i);
        }
    }
    found
}

fn saturated(spans: &[Span], line: &str) -> bool {
    let total = line.chars().count();
    if total == 0 {
        return false;
    }
    let changed: usize = spans
        .iter()
        .filter(|s| s.changed)
        .map(|s| s.text.chars().count())
        .sum();
    #[allow(clippy::cast_precision_loss)] // Line lengths are far below f64's exact-integer range.
    let fraction = changed as f64 / total as f64;
    fraction > SATURATION
}

/// Collapse adjacent spans of the same kind so the renderer emits one element
/// per run rather than one per token.
fn merge(spans: Vec<Span>) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for span in spans {
        match out.last_mut() {
            Some(last) if last.changed == span.changed => last.text.push_str(&span.text),
            _ => out.push(span),
        }
    }
    out
}
