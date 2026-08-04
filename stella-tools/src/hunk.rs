//! Hunk decomposition and recomposition (#1265) — the pure core under
//! per-hunk approval.
//!
//! [`crate::file_touch::changed_region_diff`] renders a *display* diff and is
//! deliberately coarse: one blob spanning everything between the common prefix
//! and suffix. That is the right answer for "show me what changed" and the
//! wrong one for "apply half of it", because a reviewer cannot accept part of a
//! single undivided region. This module is the other half of that pair — a real
//! line-level edit script, grouped into standard unified-diff hunks, with the
//! inverse operation ([`compose`]) that rebuilds a file from an arbitrary
//! accept/reject mask.
//!
//! Three properties the rest of the feature leans on:
//!
//! 1. **Lines keep their terminators.** [`split_lines`] yields slices that
//!    include their own `\n`, so [`compose`] is plain concatenation and a file
//!    with no trailing newline round-trips byte-for-byte. A `lines()`-based
//!    split cannot do that — it would silently add or drop a final newline,
//!    which on the one code path that mutates the user's files is not a
//!    cosmetic difference.
//! 2. **Hunks are disjoint and ascending** in both the old and the new
//!    coordinate space, which is what makes composing a mask a single forward
//!    walk with no index arithmetic between hunks.
//! 3. **Accepting every hunk reproduces `new` exactly, rejecting every hunk
//!    reproduces `old` exactly.** Pinned by tests, because a decomposition that
//!    loses a byte at the extremes would corrupt files in the case the reviewer
//!    thought was a no-op.

/// Lines of unchanged context rendered on each side of a hunk, and — doubled —
/// the gap at which two changed regions coalesce into one hunk.
///
/// Three is the unified-diff default, and the choice is about approval as much
/// as display: two edits within six lines of each other are near-certainly one
/// intent, and offering them as separately-rejectable hunks invites a reviewer
/// to keep half of a change that only makes sense whole.
pub const CONTEXT_LINES: usize = 3;

/// Beyond this many DP cells the exact edit script falls back to a single
/// whole-region hunk. The table is materialized for backtracking (unlike
/// [`crate::file_touch::line_diff`], which only needs two rows), so the cap is
/// lower than that function's: at `u32` a cell, this is a 4 MiB allocation.
const LCS_AREA_CAP: usize = 1_000_000;

/// Cap on the body lines one rendered hunk keeps per side, so a hunk that is
/// really a whole-file rewrite cannot balloon the event stream that carries it
/// to the reviewer. Matches `file_touch`'s ceiling for the same reason.
const RENDER_MAX_LINES: usize = 200;

/// One contiguous region where two contents disagree — the unit of approval.
///
/// The spans cover the **changed core only**: the surrounding context lines are
/// a rendering concern ([`render`]) and never part of what accepting a hunk
/// applies. A pure insertion has `old_len == 0`, a pure deletion `new_len == 0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hunk {
    /// 0-based line index into the old content where the changed core starts.
    pub old_start: usize,
    /// Lines of the old content this hunk replaces.
    pub old_len: usize,
    /// 0-based line index into the new content.
    pub new_start: usize,
    /// Lines of the new content this hunk introduces.
    pub new_len: usize,
}

impl Hunk {
    /// One past the last old line this hunk covers.
    #[must_use]
    pub fn old_end(&self) -> usize {
        self.old_start + self.old_len
    }

    /// One past the last new line this hunk covers.
    #[must_use]
    pub fn new_end(&self) -> usize {
        self.new_start + self.new_len
    }
}

/// Split `content` into lines that **keep** their trailing `\n`.
///
/// A final line with no terminator comes back without one, which is exactly
/// what makes `split_lines(x).concat() == x` for every input — including `""`,
/// which yields no lines at all.
///
/// Slicing on `\n` byte offsets is UTF-8-safe unconditionally: `\n` is ASCII
/// and cannot occur inside a multi-byte sequence.
#[must_use]
pub fn split_lines(content: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            out.push(&content[start..=i]);
            start = i + 1;
        }
    }
    if start < content.len() {
        out.push(&content[start..]);
    }
    out
}

/// The hunks between two contents, in ascending order.
///
/// Empty when the two are identical — an unchanged file has nothing to review.
#[must_use]
pub fn split(old: &str, new: &str) -> Vec<Hunk> {
    split_at(&split_lines(old), &split_lines(new))
}

/// [`split`] over already-split lines, for callers that hold both sides.
#[must_use]
pub fn split_at(old: &[&str], new: &[&str]) -> Vec<Hunk> {
    coalesce(regions(old, new))
}

/// Rebuild the content that applying exactly the accepted hunks produces.
///
/// `accepted[i]` answers hunk `i`; a mask shorter than the hunk list rejects
/// the remainder (a truncated mask must not apply what nobody looked at).
/// Lines between hunks come from `old`, which is sound precisely because the
/// decomposition guarantees they are identical on both sides.
#[must_use]
pub fn compose(old: &[&str], new: &[&str], hunks: &[Hunk], accepted: &[bool]) -> String {
    let mut out = String::new();
    let mut cursor = 0usize;
    for (i, hunk) in hunks.iter().enumerate() {
        out.extend(old[cursor..hunk.old_start].iter().copied());
        if accepted.get(i).copied().unwrap_or(false) {
            out.extend(new[hunk.new_start..hunk.new_end()].iter().copied());
        } else {
            out.extend(old[hunk.old_start..hunk.old_end()].iter().copied());
        }
        cursor = hunk.old_end();
    }
    out.extend(old[cursor..].iter().copied());
    out
}

/// One hunk as unified-diff text: an `@@` header positioned so a viewer's
/// line-number gutter is correct, then leading context, removals, additions,
/// and trailing context.
///
/// A line that carries no terminator (the last line of a file that does not end
/// in a newline) is rendered with one, followed by the standard
/// `\ No newline at end of file` marker — without it the next diff line would
/// run onto the same row and read as different text than the file holds.
#[must_use]
pub fn render(old: &[&str], new: &[&str], hunk: &Hunk) -> String {
    let lead = hunk.old_start.min(CONTEXT_LINES);
    let trail = (old.len() - hunk.old_end()).min(CONTEXT_LINES);
    let old_from = hunk.old_start - lead;
    let new_from = hunk.new_start - lead;
    let old_count = lead + hunk.old_len + trail;
    let new_count = lead + hunk.new_len + trail;

    // Ranges are 1-based, except that a zero-length one names the line it sits
    // *after*, which is `0` at the top of a file. `diff` and `git` both emit
    // `@@ -0,0` for a create, and a strict parser reads `-1,0` as a different
    // position rather than as a typo.
    let start = |from: usize, count: usize| if count == 0 { from } else { from + 1 };
    let mut out = format!(
        "@@ -{},{} +{},{} @@\n",
        start(old_from, old_count),
        old_count,
        start(new_from, new_count),
        new_count
    );
    let mut push = |prefix: char, lines: &[&str]| {
        for line in lines.iter().take(RENDER_MAX_LINES) {
            out.push(prefix);
            match line.strip_suffix('\n') {
                Some(bare) => {
                    out.push_str(bare);
                    out.push('\n');
                }
                None => {
                    out.push_str(line);
                    out.push('\n');
                    out.push_str("\\ No newline at end of file\n");
                }
            }
        }
        if lines.len() > RENDER_MAX_LINES {
            // A leading space keeps the marker out of any consumer counting
            // `+`/`-` body lines (`stella_tui::diff::count_diff_lines`).
            out.push_str(&format!(
                " … ({} more lines)\n",
                lines.len() - RENDER_MAX_LINES
            ));
        }
    };
    push(' ', &old[old_from..hunk.old_start]);
    push('-', &old[hunk.old_start..hunk.old_end()]);
    push('+', &new[hunk.new_start..hunk.new_end()]);
    push(' ', &old[hunk.old_end()..hunk.old_end() + trail]);
    out
}

/// The raw changed regions, before coalescing — one per maximal run of
/// unmatched lines in the alignment.
fn regions(old: &[&str], new: &[&str]) -> Vec<Hunk> {
    // Common prefix and suffix are cheap to strip and shrink the DP to the
    // part that can actually differ — the same trim `file_touch::line_diff`
    // opens with.
    let mut start = 0usize;
    while start < old.len() && start < new.len() && old[start] == new[start] {
        start += 1;
    }
    let mut old_end = old.len();
    let mut new_end = new.len();
    while old_end > start && new_end > start && old[old_end - 1] == new[new_end - 1] {
        old_end -= 1;
        new_end -= 1;
    }
    let old_mid = &old[start..old_end];
    let new_mid = &new[start..new_end];
    if old_mid.is_empty() && new_mid.is_empty() {
        return Vec::new();
    }
    // A pure insertion or deletion has no interior alignment to find, and an
    // over-large middle is not worth an exact one: both collapse to a single
    // region covering the whole middle. That is honest — "this span was
    // replaced" — rather than a guess at a finer split.
    if old_mid.is_empty() || new_mid.is_empty() || old_mid.len() * new_mid.len() > LCS_AREA_CAP {
        return vec![Hunk {
            old_start: start,
            old_len: old_mid.len(),
            new_start: start,
            new_len: new_mid.len(),
        }];
    }

    let mut out = Vec::new();
    let (mut prev_old, mut prev_new) = (start, start);
    for (oi, ni) in matches(old_mid, new_mid) {
        let (oi, ni) = (oi + start, ni + start);
        if oi > prev_old || ni > prev_new {
            out.push(Hunk {
                old_start: prev_old,
                old_len: oi - prev_old,
                new_start: prev_new,
                new_len: ni - prev_new,
            });
        }
        prev_old = oi + 1;
        prev_new = ni + 1;
    }
    if prev_old < old_end || prev_new < new_end {
        out.push(Hunk {
            old_start: prev_old,
            old_len: old_end - prev_old,
            new_start: prev_new,
            new_len: new_end - prev_new,
        });
    }
    out
}

/// Aligned `(old_index, new_index)` pairs of a longest common subsequence,
/// ascending. The table is materialized because the *path* is what this needs,
/// not just its length.
fn matches(old: &[&str], new: &[&str]) -> Vec<(usize, usize)> {
    let width = new.len() + 1;
    let mut dp = vec![0u32; (old.len() + 1) * width];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            dp[i * width + j] = if old[i] == new[j] {
                dp[(i + 1) * width + j + 1] + 1
            } else {
                dp[(i + 1) * width + j].max(dp[i * width + j + 1])
            };
        }
    }
    // Walking forward off a suffix table keeps the result ascending without a
    // reverse, and takes the earliest match on a tie — which biases hunks
    // toward starting late and ending early, i.e. toward smaller hunks.
    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < old.len() && j < new.len() {
        if old[i] == new[j] {
            out.push((i, j));
            i += 1;
            j += 1;
        } else if dp[(i + 1) * width + j] >= dp[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// Merge regions separated by at most `2 * CONTEXT_LINES` unchanged lines, so
/// each surviving hunk renders as one standard unified-diff hunk with
/// non-overlapping context.
fn coalesce(regions: Vec<Hunk>) -> Vec<Hunk> {
    let mut out: Vec<Hunk> = Vec::with_capacity(regions.len());
    for region in regions {
        match out.last_mut() {
            Some(last) if region.old_start - last.old_end() <= CONTEXT_LINES * 2 => {
                // The gap lines are identical on both sides, so extending on
                // the old side and the new side by the same span keeps the two
                // coordinate systems consistent.
                last.old_len = region.old_end() - last.old_start;
                last.new_len = region.new_end() - last.new_start;
            }
            _ => out.push(region),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` numbered lines, the fixture most tests below vary from.
    fn numbered(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    #[test]
    fn split_lines_keeps_terminators_and_round_trips() {
        for content in ["", "a", "a\n", "a\nb", "a\nb\n", "\n\n", "héllo\nwörld"] {
            assert_eq!(
                split_lines(content).concat(),
                content,
                "round trip failed for {content:?}"
            );
        }
        assert_eq!(split_lines("a\nb"), vec!["a\n", "b"]);
        assert_eq!(split_lines(""), Vec::<&str>::new());
    }

    #[test]
    fn identical_content_has_no_hunks() {
        assert!(split(&numbered(10), &numbered(10)).is_empty());
        assert!(split("", "").is_empty());
    }

    /// The shape the issue is about: two independent edits, far enough apart to
    /// stay separate, each rejectable on its own.
    #[test]
    fn two_distant_edits_are_two_hunks() {
        let old = numbered(30);
        let new = old
            .replace("line 2\n", "TWO\n")
            .replace("line 25\n", "TWENTY-FIVE\n");
        let hunks = split(&old, &new);
        assert_eq!(hunks.len(), 2, "{hunks:?}");
        assert_eq!(hunks[0].old_start, 1);
        assert_eq!(hunks[0].old_len, 1);
        assert_eq!(hunks[1].old_start, 24);
        assert_eq!(hunks[1].old_len, 1);
    }

    /// Two edits within `2 * CONTEXT_LINES` of each other are one intent, and
    /// offering them separately would let a reviewer keep half a change.
    #[test]
    fn nearby_edits_coalesce_into_one_hunk() {
        let old = numbered(30);
        let new = old
            .replace("line 10\n", "TEN\n")
            .replace("line 13\n", "THIRTEEN\n");
        assert_eq!(split(&old, &new).len(), 1);
    }

    /// The extremes. A decomposition that loses a byte here would corrupt a
    /// file in exactly the case the reviewer believed was a no-op.
    #[test]
    fn accepting_all_reproduces_new_and_rejecting_all_reproduces_old() {
        let cases = [
            (
                numbered(30),
                numbered(30)
                    .replace("line 2\n", "TWO\n")
                    .replace("line 25\n", "X\n"),
            ),
            ("a\nb\nc".to_string(), "a\nB\nc".to_string()),
            (
                "no trailing newline".to_string(),
                "rewritten, still none".to_string(),
            ),
            ("".to_string(), "created\n".to_string()),
            ("deleted\n".to_string(), "".to_string()),
            ("a\n".to_string(), "a\nb\nc\n".to_string()),
        ];
        for (old, new) in cases {
            let (ol, nl) = (split_lines(&old), split_lines(&new));
            let hunks = split_at(&ol, &nl);
            let all = vec![true; hunks.len()];
            let none = vec![false; hunks.len()];
            assert_eq!(compose(&ol, &nl, &hunks, &all), new, "accept-all: {old:?}");
            assert_eq!(compose(&ol, &nl, &hunks, &none), old, "reject-all: {old:?}");
        }
    }

    /// The issue's acceptance criterion, at the level of the pure core: two
    /// hunks, one rejected, exactly one applied.
    #[test]
    fn rejecting_one_of_two_hunks_applies_exactly_the_other() {
        let old = numbered(30);
        let new = old
            .replace("line 2\n", "TWO\n")
            .replace("line 25\n", "TWENTY-FIVE\n");
        let (ol, nl) = (split_lines(&old), split_lines(&new));
        let hunks = split_at(&ol, &nl);
        assert_eq!(hunks.len(), 2);

        let first_only = compose(&ol, &nl, &hunks, &[true, false]);
        assert!(first_only.contains("TWO\n"));
        assert!(first_only.contains("line 25\n"));
        assert!(!first_only.contains("TWENTY-FIVE"));

        let second_only = compose(&ol, &nl, &hunks, &[false, true]);
        assert!(second_only.contains("line 2\n"));
        assert!(!second_only.contains("TWO\n"));
        assert!(second_only.contains("TWENTY-FIVE\n"));
    }

    /// A mask the caller truncated must not apply the hunks it does not
    /// mention — silence is a rejection, never an approval.
    #[test]
    fn a_short_mask_rejects_the_hunks_it_does_not_cover() {
        let old = numbered(30);
        let new = old.replace("line 2\n", "TWO\n").replace("line 25\n", "X\n");
        let (ol, nl) = (split_lines(&old), split_lines(&new));
        let hunks = split_at(&ol, &nl);
        assert_eq!(compose(&ol, &nl, &hunks, &[]), old);
    }

    #[test]
    fn pure_insertion_and_deletion_are_single_hunks_with_a_zero_side() {
        let insert = split("a\nc\n", "a\nb\nc\n");
        assert_eq!(insert.len(), 1);
        assert_eq!(insert[0].old_len, 0);
        assert_eq!(insert[0].new_len, 1);

        let delete = split("a\nb\nc\n", "a\nc\n");
        assert_eq!(delete.len(), 1);
        assert_eq!(delete[0].old_len, 1);
        assert_eq!(delete[0].new_len, 0);
    }

    #[test]
    fn render_positions_the_header_and_shows_both_sides() {
        let old = numbered(30);
        let new = old.replace("line 10\n", "TEN\n");
        let (ol, nl) = (split_lines(&old), split_lines(&new));
        let hunks = split_at(&ol, &nl);
        let text = render(&ol, &nl, &hunks[0]);
        // Line 10 is index 9; three lines of context start it at index 6,
        // which is 1-based line 7, and the span is 3 + 1 + 3 on each side.
        assert!(text.starts_with("@@ -7,7 +7,7 @@\n"), "{text}");
        assert!(text.contains("\n-line 10\n"), "{text}");
        assert!(text.contains("\n+TEN\n"), "{text}");
        assert!(text.contains("\n line 7\n"), "{text}");
    }

    /// A file with no final newline must not have its last line silently glued
    /// to the next diff row.
    #[test]
    fn render_marks_a_missing_final_newline() {
        let (ol, nl) = (split_lines("a\nb"), split_lines("a\nB"));
        let hunks = split_at(&ol, &nl);
        let text = render(&ol, &nl, &hunks[0]);
        assert!(
            text.contains("-b\n\\ No newline at end of file\n"),
            "{text}"
        );
        assert!(
            text.contains("+B\n\\ No newline at end of file\n"),
            "{text}"
        );
    }

    /// A side with no lines is numbered from the line it sits after — `0` at
    /// the top of a file — which is what `diff` and `git` emit for a create.
    #[test]
    fn render_numbers_an_empty_side_from_zero() {
        let (ol, nl) = (split_lines(""), split_lines("created\n"));
        let hunks = split_at(&ol, &nl);
        let text = render(&ol, &nl, &hunks[0]);
        assert!(text.starts_with("@@ -0,0 +1,1 @@\n"), "{text}");

        // The mirror image: a delete empties the new side.
        let (ol, nl) = (split_lines("deleted\n"), split_lines(""));
        let hunks = split_at(&ol, &nl);
        let text = render(&ol, &nl, &hunks[0]);
        assert!(text.starts_with("@@ -1,1 +0,0 @@\n"), "{text}");
    }

    /// Beyond the DP cap the split degrades to one whole-region hunk rather
    /// than spending unbounded memory on an exact script.
    #[test]
    fn an_oversized_middle_degrades_to_a_single_hunk() {
        let n = 1200; // 1200 * 1200 > LCS_AREA_CAP
        let old: String = (0..n).map(|i| format!("old {i}\n")).collect();
        let new: String = (0..n).map(|i| format!("new {i}\n")).collect();
        let hunks = split(&old, &new);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_len, n);
        assert_eq!(hunks[0].new_len, n);
    }

    /// Hunks must stay disjoint and ascending on both sides — the invariant
    /// `compose`'s single forward walk depends on.
    #[test]
    fn hunks_are_disjoint_and_ascending() {
        let old = numbered(60);
        let new = old
            .replace("line 5\n", "FIVE\n")
            .replace("line 20\n", "")
            .replace("line 40\n", "FORTY\nFORTY-ONE\n");
        let hunks = split(&old, &new);
        assert!(hunks.len() >= 2, "{hunks:?}");
        for pair in hunks.windows(2) {
            assert!(pair[0].old_end() <= pair[1].old_start, "{hunks:?}");
            assert!(pair[0].new_end() <= pair[1].new_start, "{hunks:?}");
        }
    }
}
