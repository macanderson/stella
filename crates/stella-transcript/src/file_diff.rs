//! A file change turned into renderable rows: dual line numbers, per-line
//! tint, and word-level spans on the pairs that deserve them.
//!
//! This sits between [`stella_diff`], which answers "which lines changed", and
//! the two renderers, which need "what goes in each cell of each row". Keeping
//! it in the middle rather than in either renderer is the whole point — the
//! Observatory grew a JavaScript re-implementation of the TUI's diff painter
//! that silently lacked hunks and word highlights, and one shared row builder is
//! what stops that happening a third time.

use crate::model::{FileChange, FileStatus};
use crate::word::{self, Span};

/// Lines of unchanged context kept around each change.
pub const CONTEXT: usize = 3;

/// A hunk larger than this gets its own fold control, because a 200-line hunk
/// inside an open file diff is the same wall of text the fold exists to prevent.
pub const LARGE_HUNK: usize = 12;

/// What a rendered row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// An unchanged context line. **Never tinted** — colouring context is what
    /// makes a diff read as "everything changed".
    Context,
    /// A line present only on the old side.
    Removed,
    /// A line present only on the new side.
    Added,
}

impl RowKind {
    /// The sigil in the sign gutter.
    #[must_use]
    pub fn sigil(self) -> &'static str {
        match self {
            RowKind::Context => " ",
            RowKind::Removed => "−",
            RowKind::Added => "+",
        }
    }

    /// The stable class token: `ctx`, `del`, `add`.
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            RowKind::Context => "ctx",
            RowKind::Removed => "del",
            RowKind::Added => "add",
        }
    }
}

/// One rendered diff row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// What this row is.
    pub kind: RowKind,
    /// 1-based old-side line number, absent on an added line.
    pub old_no: Option<usize>,
    /// 1-based new-side line number, absent on a removed line.
    pub new_no: Option<usize>,
    /// The line's content, split into plain and changed spans. A context line
    /// is always exactly one plain span.
    pub spans: Vec<Span>,
}

impl Row {
    /// The row's text, spans concatenated.
    #[must_use]
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

/// One `@@` group, with its rows and its own fold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderHunk {
    /// The `@@ -a,b +c,d @@` header, in git's exact format.
    pub header: String,
    /// The rows, in script order.
    pub rows: Vec<Row>,
}

impl RenderHunk {
    /// Whether this hunk is big enough to deserve its own fold control.
    #[must_use]
    pub fn is_large(&self) -> bool {
        self.rows.len() > LARGE_HUNK
    }
}

/// A whole file's rendered diff, plus the header bar's accounting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    /// Display path.
    pub path: String,
    /// Status dot.
    pub status: FileStatus,
    /// Lines added.
    pub added: usize,
    /// Lines removed.
    pub removed: usize,
    /// The hunks.
    pub hunks: Vec<RenderHunk>,
    /// Whether the underlying edit script is the minimal one. `false` means
    /// this build compared the two sides, tripped [`stella_diff::LCS_AREA_CAP`]
    /// and fell back to replace-everything; surfaces say so rather than
    /// presenting a blunt diff as a precise answer.
    ///
    /// A build from [`FileChange::patch`] compares nothing, so it can never
    /// have fallen back and is always `true`. Whether the *producer's* differ
    /// fell back is not on the wire — a declared gap, tracked in #3577's
    /// follow-up rather than guessed at here.
    pub minimal: bool,
}

impl FileDiff {
    /// Build the rendered diff for one file change.
    ///
    /// Prefers [`FileChange::patch`] when the producer supplied one: it was
    /// computed against the real file, so its line numbers are the file's.
    /// Falls back to comparing `before`/`after`, which is exact whenever the
    /// caller holds both whole sides and fragment-relative when it does not.
    #[must_use]
    pub fn build(change: &FileChange) -> Self {
        if let Some(patch) = &change.patch {
            return Self {
                path: change.path.clone(),
                status: change.status,
                added: patch.added,
                removed: patch.removed,
                hunks: stella_diff::parse::hunks(&patch.text)
                    .iter()
                    .map(render_hunk)
                    .collect(),
                minimal: true,
            };
        }
        let diff = stella_diff::unified_diff(&change.before, &change.after, CONTEXT);
        let hunks = diff.hunks.iter().map(render_hunk).collect();
        Self {
            path: change.path.clone(),
            status: change.status,
            added: diff.added,
            removed: diff.removed,
            hunks,
            minimal: diff.minimal,
        }
    }

    /// The `+N −M` counter string for the header bar.
    #[must_use]
    pub fn counts(&self) -> (usize, usize) {
        (self.added, self.removed)
    }
}

/// Turn one `stella-diff` hunk into rows, pairing removals with additions so a
/// modified line gets word-level spans.
fn render_hunk(hunk: &stella_diff::Hunk) -> RenderHunk {
    let mut rows = Vec::with_capacity(hunk.lines.len());
    let mut old_no = hunk.old_start;
    let mut new_no = hunk.new_start;

    let lines = &hunk.lines;
    let mut i = 0;
    while i < lines.len() {
        match lines[i].op {
            stella_diff::Op::Equal => {
                rows.push(Row {
                    kind: RowKind::Context,
                    old_no: Some(old_no),
                    new_no: Some(new_no),
                    spans: vec![Span {
                        text: lines[i].text.clone(),
                        changed: false,
                    }],
                });
                old_no += 1;
                new_no += 1;
                i += 1;
            }
            stella_diff::Op::Remove | stella_diff::Op::Add => {
                let start = i;
                while i < lines.len() && lines[i].op == stella_diff::Op::Remove {
                    i += 1;
                }
                let removals = &lines[start..i];
                let add_start = i;
                while i < lines.len() && lines[i].op == stella_diff::Op::Add {
                    i += 1;
                }
                let additions = &lines[add_start..i];
                emit_change_block(removals, additions, &mut old_no, &mut new_no, &mut rows);
            }
        }
    }

    RenderHunk {
        header: hunk.header(),
        rows,
    }
}

/// Emit one run of removals followed by its run of additions.
///
/// Pairing is positional: the `k`th removal is compared with the `k`th addition.
/// A smarter similarity match was considered and rejected — positional pairing
/// is what a reader already assumes when they read a `−`/`+` block top to
/// bottom, and a clever re-ordering that highlighted line 1 against line 3 would
/// be correct and unreadable. Unpaired lines carry no word spans, which is the
/// honest answer: there is nothing to compare them against.
fn emit_change_block(
    removals: &[stella_diff::DiffLine],
    additions: &[stella_diff::DiffLine],
    old_no: &mut usize,
    new_no: &mut usize,
    rows: &mut Vec<Row>,
) {
    let paired = removals.len().min(additions.len());
    let mut old_spans: Vec<Vec<Span>> = Vec::with_capacity(removals.len());
    let mut new_spans: Vec<Vec<Span>> = Vec::with_capacity(additions.len());

    for k in 0..paired {
        let (o, n) = word::highlight(&removals[k].text, &additions[k].text);
        old_spans.push(o);
        new_spans.push(n);
    }
    for line in &removals[paired..] {
        old_spans.push(vec![Span {
            text: line.text.clone(),
            changed: false,
        }]);
    }
    for line in &additions[paired..] {
        new_spans.push(vec![Span {
            text: line.text.clone(),
            changed: false,
        }]);
    }

    for spans in old_spans {
        rows.push(Row {
            kind: RowKind::Removed,
            old_no: Some(*old_no),
            new_no: None,
            spans,
        });
        *old_no += 1;
    }
    for spans in new_spans {
        rows.push(Row {
            kind: RowKind::Added,
            old_no: None,
            new_no: Some(*new_no),
            spans,
        });
        *new_no += 1;
    }
}
