//! Bounded rendering of the worker-authored diff for verifier-facing prompts.
//!
//! The old bound was one blind clamp: keep 40k chars head + tail, elide the
//! middle. Blind three ways (#1433) — char-blind (the budget is paid in
//! tokens, and 40k chars of dense code is not 40k chars of whitespace),
//! relevance-blind (order-of-`git diff` decided what survived, so the hunks
//! the failing evidence names could be exactly the elided middle), and
//! authorship-blind (the pipeline-authored witness artifact rode the budget
//! the worker's change needed, billing the verifier to re-read a test its own
//! side wrote). It also re-bought the whole render on every escalation round
//! (#1431) and again on each distress-guidance call (#1432).
//!
//! This module renders per FILE SEGMENT instead. The joined diff is one
//! unified-diff-shaped string by construction ([`crate::pipeline`]'s authored
//! splice exists precisely so one parser reads it), so it splits on file
//! headers and each segment earns one of three fidelities:
//!
//! - **verbatim** — it fits the remaining token budget;
//! - **clamped middle-out** — the top-ranked segment when even it alone
//!   cannot fit: head + tail with the elision marked in-band, so something
//!   substantive always survives a single-giant-file diff;
//! - **a stat line** — everything the budget, the delta baseline (#1431), the
//!   witness exclusion (#1433), or the guidance scope (#1432) says the prompt
//!   need not re-buy. The file is still *named*, with its line counts, so the
//!   reader keeps global context at stat granularity.
//!
//! The budget is stated and enforced in **estimated tokens** via
//! [`stella_protocol::tokens::estimate_tokens_for_bytes`] — the one shared
//! byte-heuristic (#925), so this bound and the receipts plane cannot
//! disagree about what a token is made of.
//!
//! Stat lines and the summary header start with `#`, the same inert prefix
//! the authored-section header chose and for the same reason: no downstream
//! parser reads a `#` line as a path or a change. It is also not forgeable
//! from inside the diff: both diff channels are machine-rendered, and
//! worker-written content always arrives behind a `+`/`-`/space prefix, so a
//! bare `#` at the start of a line can only have been put there by the
//! pipeline's own renderers.

use std::collections::BTreeMap;

use stella_protocol::tokens::{BYTES_PER_TOKEN, estimate_tokens_for_bytes};

/// Token budget for the diff section of an escalated-verdict prompt (#1433).
///
/// Sized UNDER the recall budget (20k chars ≈ 5.7k tokens at the shared
/// heuristic): recall informs every turn of the run while this diff informs
/// one verdict, and the old 40k-char clamp (~11k tokens) had that inversion
/// backwards — the per-verdict excerpt was twice the always-on context.
pub const VERIFIER_DIFF_BUDGET_TOKENS: u64 = 5_000;

/// Token budget for the diff section of a distress-guidance prompt (#1432).
///
/// Deliberately smaller than the verdict budget: guidance is
/// course-correction, not judgment — its own prompt says "do not re-judge" —
/// so it needs the failing evidence in full and the diff only where the
/// evidence points ([`DiffScope::EvidenceNamed`]).
pub const GUIDANCE_DIFF_BUDGET_TOKENS: u64 = 2_000;

/// What the renderer knows beyond the diff text itself.
#[derive(Default)]
pub struct DiffContext<'a> {
    /// Workspace-relative paths of the pipeline-authored witness artifact —
    /// excluded from full rendering (#1433), exactly the set
    /// [`crate::verify::coverage::changed_lines`] already excludes and for
    /// the same reason: the witness is the reviewing side's own work, not the
    /// worker's change under review.
    pub witness_paths: &'a [String],
    /// The diff text as it stood when a model verifier last answered a
    /// verdict on this same candidate (#1431). File segments byte-identical
    /// to their previous-round text render as stat lines: a prior verdict
    /// already bought their full body, and what this round needs at full
    /// fidelity is what moved since.
    pub previous: Option<&'a str>,
}

/// Which segments are eligible for full-fidelity rendering.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DiffScope {
    /// Every segment, budget permitting — the verdict shape.
    Budgeted,
    /// Only segments the evidence names, budget permitting — the guidance
    /// shape (#1432). Degrades to [`DiffScope::Budgeted`] when the evidence
    /// names no file at all, because an all-stat diff would leave a steering
    /// call with nothing concrete to steer from.
    EvidenceNamed,
}

/// One file's slice of the joined diff, or a run of prose between files
/// (probe-blind markers, the authored-section header) that always renders
/// verbatim.
struct Segment {
    /// The path the headers named — new side preferred, `a/`/`b/` stripped —
    /// or `None` for prose.
    path: Option<String>,
    text: String,
    added: u32,
    removed: u32,
    /// Parser state: this file segment has already consumed its `--- `
    /// old-side header, so another `--- ` line starts the NEXT file. What
    /// makes the authored channel's header pairs (no `diff --git` line)
    /// split correctly.
    has_old_header: bool,
    /// Prose (always rendered) vs a file segment (subject to the budget).
    is_file: bool,
}

impl Segment {
    fn prose(first_line: &str) -> Self {
        Segment {
            path: None,
            text: first_line.to_string(),
            added: 0,
            removed: 0,
            has_old_header: false,
            is_file: false,
        }
    }

    fn file(first_line: &str) -> Self {
        Segment {
            path: None,
            text: first_line.to_string(),
            added: 0,
            removed: 0,
            has_old_header: false,
            is_file: true,
        }
    }
}

/// Split the joined diff into file segments and interleaved prose.
///
/// A new file segment starts at every `diff --git ` line, and at every
/// `--- ` old-side header that is not the one its current segment is still
/// waiting for — which is how the authored channel's bare header pairs are
/// told apart from the old-side header inside a `diff --git` stanza.
fn split_segments(diff: &str) -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::new();
    let mut current: Option<Segment> = None;
    for raw in diff.split_inclusive('\n') {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        if line.starts_with("diff --git ") {
            segments.extend(current.take());
            let mut seg = Segment::file(raw);
            // Best-effort path from the header itself; the `+++ ` line below
            // overwrites it when it names a real side (git quotes exotic
            // paths, and those fall back to the header lines).
            if let Some(path) = line.rsplit(' ').next() {
                let path = path.strip_prefix("b/").unwrap_or(path).trim();
                if !path.is_empty() {
                    seg.path = Some(path.to_string());
                }
            }
            current = Some(seg);
            continue;
        }
        if let Some(rest) = line.strip_prefix("--- ") {
            let old_path = rest.strip_prefix("a/").unwrap_or(rest).trim();
            let starts_new_file = current
                .as_ref()
                .is_none_or(|seg| !seg.is_file || seg.has_old_header);
            if starts_new_file {
                segments.extend(current.take());
                current = Some(Segment::file(raw));
            } else if let Some(seg) = current.as_mut() {
                seg.text.push_str(raw);
            }
            let seg = current.as_mut().expect("a segment was just ensured");
            seg.has_old_header = true;
            if seg.path.is_none() && old_path != "/dev/null" {
                seg.path = Some(old_path.to_string());
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("+++ ") {
            let new_path = rest.strip_prefix("b/").unwrap_or(rest).trim();
            let file_open = current.as_ref().is_some_and(|seg| seg.is_file);
            if file_open {
                let seg = current.as_mut().expect("file_open was just checked");
                seg.text.push_str(raw);
                if new_path != "/dev/null" {
                    seg.path = Some(new_path.to_string());
                }
            } else {
                // A `+++ ` with no file open is degenerate; keep it as prose
                // rather than inventing a file around it.
                append_prose(&mut current, &mut segments, raw);
            }
            continue;
        }
        if line.starts_with('#') && current.as_ref().is_none_or(|seg| seg.is_file) {
            // A joining marker between segments (the authored-section
            // header). Flush the file it closed; the marker itself is prose.
            segments.extend(current.take());
            current = Some(Segment::prose(raw));
            continue;
        }
        if let Some(seg) = current.as_mut() {
            seg.text.push_str(raw);
            if seg.is_file {
                if line.starts_with('+') {
                    seg.added += 1;
                } else if line.starts_with('-') {
                    seg.removed += 1;
                }
            }
        } else {
            current = Some(Segment::prose(raw));
        }
    }
    segments.extend(current);
    segments
}

fn append_prose(current: &mut Option<Segment>, segments: &mut Vec<Segment>, raw: &str) {
    match current {
        Some(seg) if !seg.is_file => seg.text.push_str(raw),
        _ => {
            segments.extend(current.take());
            *current = Some(Segment::prose(raw));
        }
    }
}

/// Whether the evidence text names this path — the full path, or a basename
/// long enough (≥ 4 chars) that its appearance is a mention rather than a
/// coincidence. Test tails and lint samples print paths in both shapes.
fn evidence_names(evidence: &str, path: &str) -> bool {
    if evidence.contains(path) {
        return true;
    }
    let base = path.rsplit('/').next().unwrap_or(path);
    base.len() >= 4 && evidence.contains(base)
}

/// The renderer's per-segment verdict.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fidelity {
    Verbatim,
    /// Middle-out clamp to this many bytes (char-boundary safe).
    Clamp(usize),
    Stat(StatReason),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StatReason {
    Witness,
    Unchanged,
    OverBudget,
    OutOfScope,
}

/// Bytes the remaining token budget is worth under the shared heuristic.
fn budget_bytes(tokens: u64) -> usize {
    (tokens as f64 * BYTES_PER_TOKEN) as usize
}

/// The largest prefix of `s` at most `max` bytes long that ends on a char
/// boundary — the clamp must never split a code point.
fn prefix_at_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The largest suffix of `s` at most `max` bytes long that starts on a char
/// boundary.
fn suffix_at_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut start = s.len() - max;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// Head-weighted middle-out clamp with the elision marked in-band, so the
/// reader knows it has an excerpt: the head carries the file headers and the
/// intent, the tail the most recent hunks — the same aging shape the
/// compactor uses.
fn clamp_middle_out(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let head_bytes = max_bytes * 2 / 3;
    let tail_bytes = max_bytes - head_bytes;
    let head = prefix_at_boundary(text, head_bytes);
    let tail = suffix_at_boundary(text, tail_bytes);
    let elided = text.len() - head.len() - tail.len();
    format!(
        "{head}\n[… {elided} bytes elided from the middle of this section — the head and tail \
         are shown; judge from what is visible and weigh that the middle is not …]\n{tail}"
    )
}

fn stat_line(seg: &Segment, reason: StatReason) -> String {
    let path = seg.path.as_deref().unwrap_or("(unnamed diff section)");
    let (added, removed) = (seg.added, seg.removed);
    match reason {
        StatReason::Witness => format!(
            "# {path}: pipeline-authored witness artifact (+{added}/-{removed} lines) — written \
             by the reviewing side, not part of the worker's change under review"
        ),
        StatReason::Unchanged => format!(
            "# {path}: unchanged since the previous verdict round (+{added}/-{removed} lines) — \
             body omitted; a prior round reviewed it in full"
        ),
        StatReason::OverBudget => {
            format!("# {path}: +{added}/-{removed} line(s) elided under the diff token budget")
        }
        StatReason::OutOfScope => format!(
            "# {path}: +{added}/-{removed} line(s) omitted — not named in the failing evidence"
        ),
    }
}

/// Render the worker-authored diff under `budget_tokens`, at full fidelity
/// where it matters and stat granularity everywhere else. Returns the input
/// byte-identical when nothing had to be reduced, so a small ordinary diff
/// rides exactly as it always did.
pub fn bounded_worker_diff(
    diff: &str,
    evidence: &str,
    ctx: &DiffContext<'_>,
    budget_tokens: u64,
    scope: DiffScope,
) -> String {
    // Fast path: nothing to exclude, no delta baseline, whole thing within
    // budget — verbatim, no parse.
    if ctx.witness_paths.is_empty()
        && ctx.previous.is_none()
        && scope == DiffScope::Budgeted
        && estimate_tokens_for_bytes(diff.len() as u64) <= budget_tokens
    {
        return diff.to_string();
    }
    let segments = split_segments(diff);
    // Pure prose (a probe-blind marker, an honest-diff note): nothing here is
    // file-shaped enough to summarize, so it rides whole — clamped middle-out
    // only if it somehow outgrows the budget.
    if segments.iter().all(|seg| !seg.is_file) {
        return clamp_middle_out(diff, budget_bytes(budget_tokens));
    }

    let previous_segments = ctx.previous.map(split_segments).unwrap_or_default();
    let previous_by_path: BTreeMap<&str, &str> = previous_segments
        .iter()
        .filter_map(|seg| seg.path.as_deref().map(|path| (path, seg.text.as_str())))
        .collect();

    let mut fidelity: Vec<Option<Fidelity>> = segments
        .iter()
        .map(|seg| (!seg.is_file).then_some(Fidelity::Verbatim))
        .collect();

    // The exclusions that hold regardless of budget: the witness artifact is
    // never re-bought (#1433), and neither is a file the previous verdict
    // round already read in this exact form (#1431).
    for (seg, slot) in segments.iter().zip(fidelity.iter_mut()) {
        if slot.is_some() {
            continue;
        }
        let Some(path) = seg.path.as_deref() else {
            continue;
        };
        if ctx.witness_paths.iter().any(|w| w == path) {
            *slot = Some(Fidelity::Stat(StatReason::Witness));
        } else if previous_by_path.get(path) == Some(&seg.text.as_str()) {
            *slot = Some(Fidelity::Stat(StatReason::Unchanged));
        }
    }

    // Eligibility and ranking (#1433): evidence-named segments first, then
    // smallest-first within each group — so one giant generated file can no
    // longer starve the hunks the verdict is actually about.
    let named: Vec<bool> = segments
        .iter()
        .map(|seg| {
            seg.path
                .as_deref()
                .map(|path| evidence_names(evidence, path))
                .unwrap_or(false)
        })
        .collect();
    let any_named = segments
        .iter()
        .zip(fidelity.iter())
        .zip(named.iter())
        .any(|((_, slot), named)| slot.is_none() && *named);
    if scope == DiffScope::EvidenceNamed && any_named {
        for (slot, named) in fidelity.iter_mut().zip(named.iter()) {
            if slot.is_none() && !named {
                *slot = Some(Fidelity::Stat(StatReason::OutOfScope));
            }
        }
    }
    let mut ranked: Vec<usize> = (0..segments.len())
        .filter(|&i| fidelity[i].is_none())
        .collect();
    ranked.sort_by_key(|&i| (!named[i], segments[i].text.len()));

    let mut remaining = budget_tokens;
    let mut any_full = false;
    for i in ranked {
        let cost = estimate_tokens_for_bytes(segments[i].text.len() as u64);
        if cost <= remaining {
            fidelity[i] = Some(Fidelity::Verbatim);
            remaining -= cost;
            any_full = true;
        } else if !any_full {
            // Even the top-ranked segment alone cannot fit: clamp it rather
            // than reducing the entire diff to stat lines.
            fidelity[i] = Some(Fidelity::Clamp(budget_bytes(remaining)));
            remaining = 0;
            any_full = true;
        } else {
            fidelity[i] = Some(Fidelity::Stat(StatReason::OverBudget));
        }
    }

    // Paths the previous round reviewed that are no longer in the diff at
    // all — a revert is a change the verdict should know about (#1431).
    let current_paths: Vec<&str> = segments.iter().filter_map(|s| s.path.as_deref()).collect();
    let removed: Vec<&str> = previous_by_path
        .keys()
        .copied()
        .filter(|path| !current_paths.iter().any(|p| p == path))
        .collect();

    let untouched = removed.is_empty()
        && fidelity
            .iter()
            .all(|slot| matches!(slot, Some(Fidelity::Verbatim)));
    if untouched {
        return diff.to_string();
    }

    let mut full = 0usize;
    let mut clamped = 0usize;
    let mut witness = 0usize;
    let mut unchanged = 0usize;
    let mut over = 0usize;
    let mut out_of_scope = 0usize;
    for slot in &fidelity {
        match slot.expect("every segment was assigned a fidelity") {
            Fidelity::Verbatim => full += 1,
            Fidelity::Clamp(_) => clamped += 1,
            Fidelity::Stat(StatReason::Witness) => witness += 1,
            Fidelity::Stat(StatReason::Unchanged) => unchanged += 1,
            Fidelity::Stat(StatReason::OverBudget) => over += 1,
            Fidelity::Stat(StatReason::OutOfScope) => out_of_scope += 1,
        }
    }
    let mut out = vec![format!(
        "# [diff bounded to ~{budget_tokens} tokens: {full} section(s) in full, {clamped} \
         clamped, {unchanged} unchanged since the previous verdict round, {witness} \
         pipeline-authored witness file(s) excluded, {over} elided by the budget, \
         {out_of_scope} not named in the failing evidence]"
    )];
    for (seg, slot) in segments.iter().zip(fidelity.iter()) {
        let block = match slot.expect("every segment was assigned a fidelity") {
            Fidelity::Verbatim => seg.text.trim_end_matches('\n').to_string(),
            Fidelity::Clamp(max) => clamp_middle_out(seg.text.trim_end_matches('\n'), max),
            Fidelity::Stat(reason) => stat_line(seg, reason),
        };
        out.push(block);
    }
    for path in removed {
        out.push(format!(
            "# {path}: present in the previous verdict round's diff, no longer in this one"
        ));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_segment(path: &str, body_lines: usize) -> String {
        let mut text =
            format!("--- a/{path}\n+++ b/{path}\n@@ -1,{body_lines} +1,{body_lines} @@\n");
        for i in 0..body_lines {
            text.push_str(&format!("+line {i} of {path}\n"));
        }
        text
    }

    fn ctx<'a>() -> DiffContext<'a> {
        DiffContext::default()
    }

    /// The identity contract: a small ordinary diff arrives byte-identical,
    /// exactly as the old clamp promised.
    #[test]
    fn a_small_diff_rides_verbatim() {
        let diff = file_segment("src/lib.rs", 5);
        let rendered = bounded_worker_diff(
            &diff,
            "",
            &ctx(),
            VERIFIER_DIFF_BUDGET_TOKENS,
            DiffScope::Budgeted,
        );
        assert_eq!(rendered, diff);
    }

    /// #1433 problem 3: the witness artifact is excluded even when the whole
    /// diff would fit the budget — the verifier must not be billed to re-read
    /// a test its own side authored.
    #[test]
    fn the_witness_segment_is_always_a_stat_line() {
        let diff = format!(
            "{}{}",
            file_segment("src/lib.rs", 5),
            file_segment("tests/witness_check.py", 8)
        );
        let witness = vec!["tests/witness_check.py".to_string()];
        let rendered = bounded_worker_diff(
            &diff,
            "",
            &DiffContext {
                witness_paths: &witness,
                previous: None,
            },
            VERIFIER_DIFF_BUDGET_TOKENS,
            DiffScope::Budgeted,
        );
        assert!(rendered.contains("+line 3 of src/lib.rs"), "{rendered}");
        assert!(
            !rendered.contains("+line 3 of tests/witness_check.py"),
            "the witness body must not be re-bought: {rendered}"
        );
        assert!(
            rendered.contains("# tests/witness_check.py: pipeline-authored witness artifact"),
            "{rendered}"
        );
        assert!(
            rendered.contains("1 pipeline-authored witness file(s) excluded"),
            "the summary must account for the exclusion: {rendered}"
        );
    }

    /// #1431: a segment byte-identical to the previous verdict round's text
    /// renders as a stat line; the segment that moved renders in full.
    #[test]
    fn delta_framing_keeps_only_what_moved() {
        let stable = file_segment("src/stable.rs", 6);
        let before = format!("{stable}{}", file_segment("src/hot.rs", 4));
        let after =
            format!("{stable}--- a/src/hot.rs\n+++ b/src/hot.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n");
        let rendered = bounded_worker_diff(
            &after,
            "",
            &DiffContext {
                witness_paths: &[],
                previous: Some(&before),
            },
            VERIFIER_DIFF_BUDGET_TOKENS,
            DiffScope::Budgeted,
        );
        assert!(
            rendered.contains("# src/stable.rs: unchanged since the previous verdict round"),
            "{rendered}"
        );
        assert!(!rendered.contains("+line 2 of src/stable.rs"), "{rendered}");
        assert!(
            rendered.contains("+new"),
            "the moved hunk must survive: {rendered}"
        );
    }

    /// #1431 option 1: a diff byte-identical to the previous round reduces to
    /// stat lines that say so — the worker revised nothing file-shaped, and
    /// the prompt stops re-buying ~10k tokens to state that.
    #[test]
    fn an_identical_diff_reduces_to_stat_lines() {
        let diff = format!(
            "{}{}",
            file_segment("src/a.rs", 5),
            file_segment("src/b.rs", 5)
        );
        let rendered = bounded_worker_diff(
            &diff,
            "",
            &DiffContext {
                witness_paths: &[],
                previous: Some(&diff),
            },
            VERIFIER_DIFF_BUDGET_TOKENS,
            DiffScope::Budgeted,
        );
        assert!(
            rendered.contains("2 unchanged since the previous verdict round"),
            "{rendered}"
        );
        assert!(!rendered.contains("+line 1 of src/a.rs"), "{rendered}");
        assert!(!rendered.contains("+line 1 of src/b.rs"), "{rendered}");
    }

    /// A path the previous round reviewed that has since left the diff is
    /// named — a revert is a change the verdict should know about.
    #[test]
    fn a_reverted_path_is_reported() {
        let kept = file_segment("src/kept.rs", 3);
        let before = format!("{kept}{}", file_segment("src/gone.rs", 3));
        let rendered = bounded_worker_diff(
            &kept,
            "",
            &DiffContext {
                witness_paths: &[],
                previous: Some(&before),
            },
            VERIFIER_DIFF_BUDGET_TOKENS,
            DiffScope::Budgeted,
        );
        assert!(
            rendered.contains("# src/gone.rs: present in the previous verdict round's diff"),
            "{rendered}"
        );
    }

    /// #1433 problem 2: with the budget too small for everything, the
    /// evidence-named file survives in full and the giant unnamed one is
    /// reduced to a stat line — the old head+tail clamp did the opposite
    /// whenever the named file sat in the middle.
    #[test]
    fn evidence_named_segments_outrank_a_giant_unnamed_one() {
        let giant = file_segment("vendor/generated.rs", 3_000);
        let named = file_segment("src/decode.rs", 10);
        let diff = format!("{giant}{named}");
        let rendered = bounded_worker_diff(
            &diff,
            "test failed in src/decode.rs: assertion left != right",
            &ctx(),
            1_000,
            DiffScope::Budgeted,
        );
        assert!(rendered.contains("+line 5 of src/decode.rs"), "{rendered}");
        assert!(
            rendered.contains("# vendor/generated.rs:"),
            "the giant must fall to a stat line: {rendered}"
        );
        assert!(
            !rendered.contains("+line 2999 of vendor/generated.rs"),
            "{rendered}"
        );
    }

    /// When even the top-ranked segment cannot fit the budget alone, it is
    /// clamped middle-out with the elision marked in-band rather than the
    /// diff reducing to nothing but stat lines.
    #[test]
    fn a_single_oversized_segment_is_clamped_not_dropped() {
        let giant = file_segment("src/only.rs", 3_000);
        let rendered = bounded_worker_diff(&giant, "", &ctx(), 1_000, DiffScope::Budgeted);
        assert!(
            rendered.contains("bytes elided from the middle of this section"),
            "{rendered}"
        );
        assert!(
            rendered.contains("+line 0 of src/only.rs"),
            "the head survives: {rendered}"
        );
        assert!(
            rendered.contains("+line 2999 of src/only.rs"),
            "the tail survives: {rendered}"
        );
        let budget = budget_bytes(1_000);
        assert!(
            rendered.len() < budget + 600,
            "clamped ({}) must sit near the byte budget ({budget})",
            rendered.len()
        );
    }

    /// #1432: under the guidance scope, files the failing evidence does not
    /// name reduce to stat lines even when the budget could afford them.
    #[test]
    fn guidance_scope_keeps_only_evidence_named_files() {
        let diff = format!(
            "{}{}",
            file_segment("src/named.rs", 5),
            file_segment("src/other.rs", 5)
        );
        let rendered = bounded_worker_diff(
            &diff,
            "FAILED src/named.rs::test_roundtrip",
            &ctx(),
            GUIDANCE_DIFF_BUDGET_TOKENS,
            DiffScope::EvidenceNamed,
        );
        assert!(rendered.contains("+line 1 of src/named.rs"), "{rendered}");
        assert!(!rendered.contains("+line 1 of src/other.rs"), "{rendered}");
        assert!(
            rendered.contains("# src/other.rs:")
                && rendered.contains("not named in the failing evidence"),
            "{rendered}"
        );
    }

    /// The guidance scope must not go blind when the evidence names no file:
    /// it degrades to ordinary budgeted rendering.
    #[test]
    fn guidance_scope_degrades_to_budgeted_when_nothing_is_named() {
        let diff = file_segment("src/lib.rs", 5);
        let rendered = bounded_worker_diff(
            &diff,
            "exit status 1",
            &ctx(),
            GUIDANCE_DIFF_BUDGET_TOKENS,
            DiffScope::EvidenceNamed,
        );
        assert_eq!(rendered, diff, "an unnamed small diff still rides whole");
    }

    /// Prose-only diffs (the probe-blind marker) ride whole — there is no
    /// file segment to summarize, and the marker text is the evidence.
    #[test]
    fn a_prose_only_diff_rides_whole() {
        let marker = "[the diff probe failed; the working tree could not be read.]";
        let rendered = bounded_worker_diff(
            marker,
            "",
            &ctx(),
            VERIFIER_DIFF_BUDGET_TOKENS,
            DiffScope::Budgeted,
        );
        assert_eq!(rendered, marker);
    }

    /// The authored-section joining marker (`# --- changes recorded …`)
    /// survives rendering in place: it labels the half that follows, and the
    /// halves it separates are still split into their own segments.
    #[test]
    fn the_authored_joining_marker_is_preserved_as_prose() {
        let header = "# --- changes recorded by the file tools at write time (authorship, not tree state) ---";
        let diff = format!(
            "{}{header}\n--- /dev/null\n+++ b/solution.py\n@@ -0,0 +1,1 @@\n+print('x')\n",
            file_segment("src/lib.rs", 2)
        );
        let witness = vec!["src/lib.rs".to_string()];
        let rendered = bounded_worker_diff(
            &diff,
            "",
            &DiffContext {
                witness_paths: &witness,
                previous: None,
            },
            VERIFIER_DIFF_BUDGET_TOKENS,
            DiffScope::Budgeted,
        );
        assert!(rendered.contains(header), "{rendered}");
        assert!(rendered.contains("+print('x')"), "{rendered}");
        assert!(
            rendered.contains("# src/lib.rs: pipeline-authored witness artifact"),
            "{rendered}"
        );
    }

    /// Segment splitting handles the authored channel's bare header pairs —
    /// two files with no `diff --git` stanza between them.
    #[test]
    fn bare_header_pairs_split_into_two_segments() {
        let diff = "--- /dev/null\n+++ b/a.py\n@@ -0,0 +1,1 @@\n+a\n--- /dev/null\n+++ b/b.py\n@@ -0,0 +1,1 @@\n+b\n";
        let segments = split_segments(diff);
        let paths: Vec<_> = segments.iter().filter_map(|s| s.path.as_deref()).collect();
        assert_eq!(paths, vec!["a.py", "b.py"]);
    }

    /// The token budget is enforced in the shared heuristic's units: a diff
    /// just over the budget loses its largest segment to a stat line and the
    /// summary header says so.
    #[test]
    fn the_budget_is_paid_in_estimated_tokens() {
        let small = file_segment("src/small.rs", 4);
        let big = file_segment("src/big.rs", 400);
        let diff = format!("{big}{small}");
        let total_tokens = estimate_tokens_for_bytes(diff.len() as u64);
        let budget = total_tokens / 2;
        let rendered = bounded_worker_diff(&diff, "", &ctx(), budget, DiffScope::Budgeted);
        assert!(rendered.contains("+line 1 of src/small.rs"), "{rendered}");
        assert!(rendered.contains("# src/big.rs:"), "{rendered}");
        assert!(rendered.starts_with("# [diff bounded to"), "{rendered}");
    }
}
