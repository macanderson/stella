//! A real line-oriented unified diff — the shape a developer already knows how
//! to read, because it is the shape `git diff` produces.
//!
//! The tree had no unified-diff generator when this landed. `stella-tools`'
//! [`changed_region_diff`] is the display companion to a *line-count* helper and
//! says so in its own docs: it trims the common prefix and suffix and then
//! prints the entire remaining span twice, once as `-` and once as `+`. That is
//! honest and cheap for a file edit, where the changed region is small and the
//! viewer already has the file open. It is useless for a system prompt, where
//! the interesting change is one inserted paragraph inside four hundred stable
//! lines — the coarse renderer would print all four hundred lines as removed
//! and all four hundred and one as added, which is exactly the "I cannot see
//! what actually changed" problem this module exists to end.
//!
//! So: trim the common prefix/suffix (cheap, and it shrinks the quadratic
//! region a lot on append-mostly prompts), run an exact longest-common-
//! subsequence over what is left, and walk the table to an edit script.
//! Group the script into `@@` hunks with context lines.
//!
//! [`changed_region_diff`]: stella_tools::file_touch::changed_region_diff

/// Beyond this many DP cells the exact LCS is abandoned for the
/// replace-everything bound. Mirrors `stella_tools::file_touch`'s cap for the
/// same reason: a pathological input must degrade to a coarse-but-instant
/// answer rather than allocate a gigabyte and stall the terminal.
///
/// The fallback is still a correct diff — every old line removed, every new one
/// added — just not a minimal one, and [`Diff::minimal`] says which you got
/// rather than letting a caller assume.
const LCS_AREA_CAP: usize = 4_000_000;

/// What happened to one line between the two sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    /// Present in both, unchanged — printed as a context line.
    Equal,
    /// Present only on the new side.
    Add,
    /// Present only on the old side.
    Remove,
}

impl Op {
    /// The single-character gutter git puts in front of the line.
    pub(crate) fn sigil(self) -> char {
        match self {
            Op::Equal => ' ',
            Op::Add => '+',
            Op::Remove => '-',
        }
    }

    /// The stable lowercase tag the JSON format emits.
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Op::Equal => "equal",
            Op::Add => "add",
            Op::Remove => "remove",
        }
    }
}

/// One line of the edit script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffLine {
    pub(crate) op: Op,
    pub(crate) text: String,
}

/// One `@@ -old_start,old_count +new_start,new_count @@` group: a run of
/// changes plus the context lines framing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hunk {
    pub(crate) old_start: usize,
    pub(crate) old_count: usize,
    pub(crate) new_start: usize,
    pub(crate) new_count: usize,
    pub(crate) lines: Vec<DiffLine>,
}

impl Hunk {
    /// The `@@ … @@` header, in git's exact format so an editor's diff mode and
    /// a human's muscle memory both parse it.
    pub(crate) fn header(&self) -> String {
        format!(
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_count, self.new_start, self.new_count
        )
    }
}

/// A complete comparison: the hunks, plus the accounting a caller would
/// otherwise be tempted to re-derive by counting sigils in rendered text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Diff {
    pub(crate) hunks: Vec<Hunk>,
    pub(crate) added: usize,
    pub(crate) removed: usize,
    /// Whether the edit script is the minimal one. `false` means the input
    /// exceeded [`LCS_AREA_CAP`] and fell back to replace-everything — the diff
    /// is still correct, just blunt, and surfaces are expected to say so rather
    /// than present it as a precise answer.
    pub(crate) minimal: bool,
}

impl Diff {
    /// Whether the two sides differ at all. An empty hunk list is the honest
    /// "byte-identical" answer, not an error.
    pub(crate) fn changed(&self) -> bool {
        !self.hunks.is_empty()
    }
}

/// Diff `old` against `new` line-wise, framing each change with `context`
/// unchanged lines on either side.
///
/// Line semantics follow `str::lines()`, matching `count_lines` in
/// `stella-tools`: `""` is zero lines and a trailing newline adds none. Two
/// empty inputs therefore produce no hunks rather than one empty hunk.
pub(crate) fn unified_diff(old: &str, new: &str, context: usize) -> Diff {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let (start, old_end, new_end) = trimmed_middle(&old_lines, &new_lines);
    let old_mid = &old_lines[start..old_end];
    let new_mid = &new_lines[start..new_end];

    let (middle, minimal) = edit_script(old_mid, new_mid);

    // Reassemble the full script: the trimmed prefix and suffix are equal by
    // construction, and the hunk grouping below needs them present to have
    // context lines to draw from.
    let mut script: Vec<DiffLine> =
        Vec::with_capacity(middle.len() + start + (old_lines.len() - old_end));
    for line in &old_lines[..start] {
        script.push(equal(line));
    }
    script.extend(middle);
    for line in &old_lines[old_end..] {
        script.push(equal(line));
    }

    let added = script.iter().filter(|l| l.op == Op::Add).count();
    let removed = script.iter().filter(|l| l.op == Op::Remove).count();

    Diff {
        hunks: hunkify(&script, context),
        added,
        removed,
        minimal,
    }
}

fn equal(text: &str) -> DiffLine {
    DiffLine {
        op: Op::Equal,
        text: text.to_string(),
    }
}

/// The half-open span `[start, old_end)` / `[start, new_end)` left after
/// trimming the common prefix and suffix — the only region the quadratic LCS
/// has to look at.
fn trimmed_middle(old_lines: &[&str], new_lines: &[&str]) -> (usize, usize, usize) {
    let mut start = 0;
    while start < old_lines.len() && start < new_lines.len() && old_lines[start] == new_lines[start]
    {
        start += 1;
    }
    let mut old_end = old_lines.len();
    let mut new_end = new_lines.len();
    while old_end > start && new_end > start && old_lines[old_end - 1] == new_lines[new_end - 1] {
        old_end -= 1;
        new_end -= 1;
    }
    (start, old_end, new_end)
}

/// The minimal edit script over the trimmed middle, as `(script, minimal)`.
///
/// Removals are emitted before additions at a change point, which is what git
/// does and therefore what a reader expects: the old text, then what replaced
/// it.
fn edit_script(old: &[&str], new: &[&str]) -> (Vec<DiffLine>, bool) {
    if old.is_empty() || new.is_empty() {
        return (replace_all(old, new), true);
    }
    if old.len().saturating_mul(new.len()) > LCS_AREA_CAP {
        return (replace_all(old, new), false);
    }

    // dp[i][j] = length of the LCS of old[i..] and new[j..]. Filled backwards
    // so the forward walk below can pick the branch that preserves the most
    // future matches without re-deriving anything.
    let (m, n) = (old.len(), new.len());
    let width = n + 1;
    let mut dp = vec![0u32; (m + 1) * width];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            dp[i * width + j] = if old[i] == new[j] {
                dp[(i + 1) * width + j + 1] + 1
            } else {
                dp[(i + 1) * width + j].max(dp[i * width + j + 1])
            };
        }
    }

    let mut script = Vec::with_capacity(m.max(n));
    let (mut i, mut j) = (0usize, 0usize);
    while i < m && j < n {
        if old[i] == new[j] {
            script.push(equal(old[i]));
            i += 1;
            j += 1;
        } else if dp[(i + 1) * width + j] >= dp[i * width + j + 1] {
            script.push(DiffLine {
                op: Op::Remove,
                text: old[i].to_string(),
            });
            i += 1;
        } else {
            script.push(DiffLine {
                op: Op::Add,
                text: new[j].to_string(),
            });
            j += 1;
        }
    }
    for line in &old[i..] {
        script.push(DiffLine {
            op: Op::Remove,
            text: line.to_string(),
        });
    }
    for line in &new[j..] {
        script.push(DiffLine {
            op: Op::Add,
            text: line.to_string(),
        });
    }
    (script, true)
}

/// Every old line removed, every new line added — the correct-but-blunt script
/// used when one side is empty (where it is also minimal) and when the input is
/// too large for the exact table.
fn replace_all(old: &[&str], new: &[&str]) -> Vec<DiffLine> {
    let mut script = Vec::with_capacity(old.len() + new.len());
    for line in old {
        script.push(DiffLine {
            op: Op::Remove,
            text: line.to_string(),
        });
    }
    for line in new {
        script.push(DiffLine {
            op: Op::Add,
            text: line.to_string(),
        });
    }
    script
}

/// Group the script into `@@` hunks: every changed line, plus up to `context`
/// unchanged lines on each side, with runs that would overlap merged into one.
fn hunkify(script: &[DiffLine], context: usize) -> Vec<Hunk> {
    let changed: Vec<usize> = script
        .iter()
        .enumerate()
        .filter(|(_, l)| l.op != Op::Equal)
        .map(|(i, _)| i)
        .collect();
    if changed.is_empty() {
        return Vec::new();
    }

    // Per-script-index line numbers on each side, 1-based, naming the line this
    // entry occupies (or, for an entry absent from that side, the line it sits
    // in front of — which is the number git's header wants).
    let mut old_no = Vec::with_capacity(script.len());
    let mut new_no = Vec::with_capacity(script.len());
    let (mut o, mut n) = (1usize, 1usize);
    for line in script {
        old_no.push(o);
        new_no.push(n);
        match line.op {
            Op::Equal => {
                o += 1;
                n += 1;
            }
            Op::Remove => o += 1,
            Op::Add => n += 1,
        }
    }

    // Merge the context windows around each changed line. Two windows that
    // touch or overlap become one hunk, so a reader never sees the same context
    // line printed twice under two headers.
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for &index in &changed {
        let lo = index.saturating_sub(context);
        let hi = (index + context).min(script.len() - 1);
        match ranges.last_mut() {
            Some((_, prev_hi)) if *prev_hi + 1 >= lo => *prev_hi = (*prev_hi).max(hi),
            _ => ranges.push((lo, hi)),
        }
    }

    ranges
        .into_iter()
        .map(|(lo, hi)| {
            let lines = script[lo..=hi].to_vec();
            let old_count = lines.iter().filter(|l| l.op != Op::Add).count();
            let new_count = lines.iter().filter(|l| l.op != Op::Remove).count();
            Hunk {
                // git renders a zero-length side as the line *before* the
                // insertion point, so `+0,0` never claims a line that isn't
                // there.
                old_start: if old_count == 0 {
                    old_no[lo].saturating_sub(1)
                } else {
                    old_no[lo]
                },
                old_count,
                new_start: if new_count == 0 {
                    new_no[lo].saturating_sub(1)
                } else {
                    new_no[lo]
                },
                new_count,
                lines,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ops(diff: &Diff) -> Vec<(char, &str)> {
        diff.hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .map(|l| (l.op.sigil(), l.text.as_str()))
            .collect()
    }

    #[test]
    fn identical_input_produces_no_hunks_at_all() {
        let diff = unified_diff("a\nb\nc", "a\nb\nc", 3);
        assert!(!diff.changed(), "byte-identical is not a change");
        assert_eq!((diff.added, diff.removed), (0, 0));
    }

    #[test]
    fn an_insertion_in_a_stable_body_shows_only_the_inserted_line() {
        // The case the coarse `changed_region_diff` cannot express, and the
        // reason this module exists: one line lands in the middle of a long
        // stable body, and everything else must stay context.
        let old = (1..=20)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let mut new_lines: Vec<String> = (1..=20).map(|i| i.to_string()).collect();
        new_lines.insert(10, "INSERTED".into());
        let diff = unified_diff(&old, &new_lines.join("\n"), 2);

        assert_eq!((diff.added, diff.removed), (1, 0));
        assert_eq!(diff.hunks.len(), 1, "one contiguous change, one hunk");
        assert_eq!(
            ops(&diff),
            vec![
                (' ', "9"),
                (' ', "10"),
                ('+', "INSERTED"),
                (' ', "11"),
                (' ', "12"),
            ],
            "only the insertion and its context are printed"
        );
    }

    #[test]
    fn hunk_header_positions_the_gutter_the_way_git_does() {
        let diff = unified_diff("a\nb\nc\nd", "a\nB\nc\nd", 1);
        let hunk = &diff.hunks[0];
        // One line replaced at position 2: two lines of old (context + removal
        // is 3 entries, but old_count counts non-Add entries).
        assert_eq!(hunk.header(), "@@ -1,3 +1,3 @@", "{hunk:?}");
    }

    #[test]
    fn separated_changes_become_separate_hunks_but_near_ones_merge() {
        let old: String = (1..=30)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let mut new_lines: Vec<String> = (1..=30).map(|i| i.to_string()).collect();
        new_lines[2] = "X".into();
        new_lines[25] = "Y".into();
        let far = unified_diff(&old, &new_lines.join("\n"), 2);
        assert_eq!(far.hunks.len(), 2, "distant changes do not share a hunk");

        let mut near: Vec<String> = (1..=30).map(|i| i.to_string()).collect();
        near[2] = "X".into();
        near[4] = "Y".into();
        let merged = unified_diff(&old, &near.join("\n"), 3);
        assert_eq!(
            merged.hunks.len(),
            1,
            "overlapping context windows merge so no line prints twice"
        );
    }

    #[test]
    fn an_empty_old_side_is_a_pure_addition_against_line_zero() {
        // The turn-0 shape: nothing on the left, the whole assembled context on
        // the right. The old side must report `-0,0`, never `-1,0`.
        let diff = unified_diff("", "system\nuser", 3);
        assert_eq!((diff.added, diff.removed), (2, 0));
        assert_eq!(diff.hunks[0].header(), "@@ -0,0 +1,2 @@");
    }

    #[test]
    fn removals_precede_additions_at_a_change_point() {
        let diff = unified_diff("keep\nold\ntail", "keep\nnew\ntail", 0);
        assert_eq!(ops(&diff), vec![('-', "old"), ('+', "new")]);
    }

    #[test]
    fn the_exact_script_is_minimal_not_a_wholesale_replacement() {
        // A prompt that gained a paragraph must not report every line as
        // rewritten — the counts are what a caller reports, so a blunt script
        // here would be a lie in the summary line, not just in the rendering.
        let old = "alpha\nbeta\ngamma";
        let new = "alpha\nbeta\nINSERT\ngamma";
        let diff = unified_diff(old, new, 3);
        assert!(diff.minimal);
        assert_eq!((diff.added, diff.removed), (1, 0));
    }

    #[test]
    fn line_semantics_match_str_lines_so_a_trailing_newline_is_not_a_change() {
        let diff = unified_diff("a\nb", "a\nb\n", 3);
        assert!(
            !diff.changed(),
            "a trailing newline adds no line, so it is not a diff"
        );
    }
}
