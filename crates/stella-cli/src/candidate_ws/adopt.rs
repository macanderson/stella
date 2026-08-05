//! Reading `git`'s account of what an adoption moved.
//!
//! A candidate's edits happen inside a shadow worktree with its own tool
//! stack, deliberately untapped: a losing candidate's work is discarded, and
//! announcing its edits would claim changes the user's tree never received.
//! Adoption is therefore the *only* honest moment to report a candidate's
//! files — and until this module it reported them without diffs, so every
//! adopted file reached the deck's Files tab as `+0 -0`: a row saying a file
//! was modified and that nothing in it changed.
//!
//! So the adoption asks git for the delta over the same baseline→sealed range
//! three ways: `--name-status` (which files, and how), `--numstat` (how many
//! lines each gained and lost — the authoritative counts, never a recount of
//! the text), and the patch itself, which [`split_patch_per_file`] cuts into the
//! per-file diffs the pane renders.

use stella_pipeline::ports::AdoptedChange;
use stella_protocol::FileChangeKind;

/// Parse `git diff --name-status --no-renames -z` output — `S\0path\0`
/// records with statuses A/M/D (renames disabled, so no two-path records).
pub(crate) fn parse_name_status(raw: &str) -> Vec<AdoptedChange> {
    let mut parts = raw.split('\0').filter(|s| !s.is_empty());
    let mut out = Vec::new();
    while let (Some(status), Some(path)) = (parts.next(), parts.next()) {
        let kind = match status.chars().next() {
            Some('A') => FileChangeKind::Created,
            Some('D') => FileChangeKind::Deleted,
            _ => FileChangeKind::Modified,
        };
        out.push(AdoptedChange {
            path: path.to_string(),
            kind,
            added: 0,
            removed: 0,
            diff: None,
        });
    }
    out
}

/// The paths named in `git apply`'s stderr (`error: patch failed:
/// <path>:<line>`, `error: <path>: <why>`), deduped and sorted. Falls back
/// to every path in the patch when stderr names none — the adoption error
/// must always name paths.
pub(crate) fn conflict_paths_from_stderr(stderr: &str, changes: &[AdoptedChange]) -> Vec<String> {
    let mut paths: Vec<String> = stderr
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("error: ")?;
            if let Some(failed) = rest.strip_prefix("patch failed: ") {
                let path = failed.rsplit_once(':').map(|(p, _)| p).unwrap_or(failed);
                Some(path.to_string())
            } else {
                rest.split_once(':').map(|(p, _)| p.to_string())
            }
        })
        .collect();
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        paths = changes.iter().map(|c| c.path.clone()).collect();
    }
    paths
}

/// Where the real tree stands at one path whose hunk `git apply` refused,
/// relative to the two states adoption already knows: `baseline`, the tree the
/// candidate snapshotted when it started, and `sealed`, the tree it finished
/// with.
///
/// # Why the adoption error needs this at all
///
/// `git apply` verifies every preimage before writing anything, so a rejection
/// always means "the real tree is not what this patch was built against". For a
/// long time the module read that as one thing — the user edited a file
/// mid-run — and said so. It is not one thing, and the other case is the more
/// interesting one: a candidate that wrote *through* to the real tree, `cp`ing
/// its answer to the absolute path the task named instead of leaving it in its
/// snapshot. Then git reports `already exists in working directory` and the
/// error blames a user who did nothing, while the actual defect — an isolated
/// candidate escaping its isolation — goes unnamed and unfixed.
///
/// The two are distinguishable, because the candidate's own output is a
/// signature. Nothing else on the machine produces the exact sealed blob for
/// that path; if the real tree holds those bytes and the baseline did not, the
/// candidate put them there. That is what this enum records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathDivergence {
    /// The real tree already holds the candidate's OWN sealed bytes at a path
    /// whose baseline differed. The candidate wrote outside its workspace.
    CandidateWroteOutside,
    /// The real tree moved off the baseline, and not toward the candidate's
    /// result — something outside this run changed it, most often the user.
    ChangedUnderTheRun,
    /// The real tree still matches what the candidate started from. Not what
    /// the rejection was about; carried so a caller can report it as ruled out
    /// rather than silently dropping it.
    Matches,
}

impl PathDivergence {
    /// Classify one path from three blob identities: what the real tree holds
    /// now, what the candidate started from, and what it sealed. `None` means
    /// the file is absent in that state.
    ///
    /// A path the real tree does not have cannot be the thing blocking a patch
    /// that writes it, so absence is always [`Self::Matches`] — the interesting
    /// asymmetry is the other direction, where a path absent from the baseline
    /// and present in the real tree with the sealed bytes is the escape.
    pub(crate) fn classify(
        real: Option<&str>,
        baseline: Option<&str>,
        sealed: Option<&str>,
    ) -> Self {
        let Some(real) = real else {
            return Self::Matches;
        };
        if Some(real) == baseline {
            return Self::Matches;
        }
        // Ordered before the generic divergence on purpose: a real tree holding
        // the sealed bytes satisfies BOTH conditions, and only the specific
        // reading is actionable.
        if Some(real) == sealed {
            return Self::CandidateWroteOutside;
        }
        Self::ChangedUnderTheRun
    }
}

/// The sentence that leads an adoption failure, given what each conflicting
/// path turned out to be. `None` when nothing diverged — then git's own stderr
/// is the better account and this must not talk over it.
///
/// The escape is reported first and unconditionally when present, even
/// alongside ordinary user edits: it is a defect in the run rather than a fact
/// about the user's tree, and it is the one of the two that will recur on every
/// future turn until it is fixed.
pub(crate) fn describe_divergence(
    classified: &[(String, PathDivergence)],
    truncated: bool,
) -> Option<String> {
    let named = |want: PathDivergence| -> Vec<&str> {
        classified
            .iter()
            .filter(|(_, divergence)| *divergence == want)
            .map(|(path, _)| path.as_str())
            .collect()
    };
    let escaped = named(PathDivergence::CandidateWroteOutside);
    let edited = named(PathDivergence::ChangedUnderTheRun);
    if escaped.is_empty() && edited.is_empty() {
        return None;
    }
    let mut reason = String::new();
    if !escaped.is_empty() {
        reason.push_str(&format!(
            "the candidate wrote outside its workspace: the real tree already holds this \
             candidate's own final bytes at {} — so it did not confine its work to its \
             snapshot, and the patch cannot re-create what is already there",
            render_paths(&escaped)
        ));
    }
    if !edited.is_empty() {
        if !reason.is_empty() {
            reason.push_str("; additionally, ");
        }
        reason.push_str(&format!(
            "the real tree changed under the run at {} (content that is neither the state \
             the candidate started from nor the state it produced)",
            render_paths(&edited)
        ));
    }
    if truncated {
        reason.push_str(" (only the first conflicting paths were examined)");
    }
    Some(reason)
}

/// Up to four paths inline, then a count — an adoption that conflicts on a
/// hundred files must not paste a hundred paths into one error line.
fn render_paths(paths: &[&str]) -> String {
    const INLINE: usize = 4;
    let shown = paths
        .iter()
        .take(INLINE)
        .map(|path| format!("`{path}`"))
        .collect::<Vec<_>>()
        .join(", ");
    match paths.len().checked_sub(INLINE) {
        Some(rest) if rest > 0 => format!("{shown} (+{rest} more)"),
        _ => shown,
    }
}

/// Attach each file's own slice of a multi-file `git diff` to its change.
///
/// Splitting on `diff --git` is what makes the per-file attribution possible,
/// and it is also the one thing that can go wrong quietly: a *content* line
/// inside a hunk can read exactly like a header (this repository's own diff
/// machinery hits it — a removed line of Rust that itself contained
/// `diff --git` would open a phantom file). Hunk bodies are prefixed by
/// git (` `, `+`, `-`), so a real header is only ever a header when it starts
/// at column zero; that is the discriminator used here.
///
/// A path git did not describe keeps `diff: None` rather than borrowing
/// another file's text — the ledger then reports it as uncounted instead of
/// misattributing lines, and `views::files` already renders that honestly.
pub(crate) fn attach_diffs(changes: &mut [AdoptedChange], patch: &str) {
    for (path, diff) in split_patch_per_file(patch) {
        if let Some(change) = changes.iter_mut().find(|c| c.path == path) {
            change.diff = Some(diff);
        }
    }
}

/// Attach git's own line counts from `git diff --numstat -z` output:
/// `added\tremoved\tpath\0` per file, where a binary file reports `-\t-`.
///
/// These, not a recount of the patch text, are the numbers the Files tab shows
/// — the same rule the session recorder follows, for the same reason: one
/// measurement, one source. A binary file keeps `0/0` (it has no lines to
/// count) while still carrying its kind.
pub(crate) fn attach_numstat(changes: &mut [AdoptedChange], numstat: &str) {
    for record in numstat.split('\0').filter(|s| !s.is_empty()) {
        let mut fields = record.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if let Some(change) = changes.iter_mut().find(|c| c.path == path) {
            change.added = added.parse().unwrap_or(0);
            change.removed = removed.parse().unwrap_or(0);
        }
    }
}

/// Cut a multi-file patch into `(path, diff-text)` pairs, keyed by the `b/`
/// side (the post-image, which is the path every consumer names) and falling
/// back to the `a/` side for a deletion, whose `b/` side is `/dev/null`.
fn split_patch_per_file(patch: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, String)> = None;

    for line in patch.lines() {
        // Column zero only: an indented or `+`/`-`/` `-prefixed occurrence is
        // hunk *content* that happens to look like a header.
        if let Some(header) = line.strip_prefix("diff --git ") {
            if let Some(done) = current.take() {
                out.push(done);
            }
            current = header_path(header).map(|path| (path, String::new()));
            continue;
        }
        if let Some((_, body)) = current.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    if let Some(done) = current.take() {
        out.push(done);
    }
    out
}

/// The path a `diff --git a/x b/x` header describes. Git quotes paths
/// containing spaces, so the halves are split on the ` b/` boundary rather
/// than on whitespace, and the `b/` side wins unless it is `/dev/null`.
fn header_path(header: &str) -> Option<String> {
    let (a, b) = match header.split_once(" b/") {
        Some((a, b)) => (a.trim_start_matches("a/"), b),
        // Not a shape we understand — better no diff than a wrong one.
        None => return None,
    };
    if b == "/dev/null" || b.is_empty() {
        let a = a.trim();
        return (!a.is_empty()).then(|| a.to_string());
    }
    Some(b.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(path: &str) -> AdoptedChange {
        AdoptedChange {
            path: path.to_string(),
            kind: FileChangeKind::Modified,
            added: 0,
            removed: 0,
            diff: None,
        }
    }

    const TWO_FILES: &str = "\
diff --git a/src/a.rs b/src/a.rs
index 111..222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,2 +1,2 @@
-old a
+new a
diff --git a/src/b.rs b/src/b.rs
index 333..444 100644
--- a/src/b.rs
+++ b/src/b.rs
@@ -1,1 +1,2 @@
 kept
+added b
";

    #[test]
    fn each_file_gets_its_own_slice_of_the_patch() {
        let mut changes = vec![change("src/a.rs"), change("src/b.rs")];
        attach_diffs(&mut changes, TWO_FILES);

        let a = changes[0].diff.as_deref().expect("a.rs has a diff");
        assert!(a.contains("-old a") && a.contains("+new a"));
        assert!(!a.contains("added b"), "no bleed from the next file");
        assert_eq!(
            stella_tui::diff::count_diff_lines(a),
            (1, 1),
            "the ledger counts a.rs's real lines"
        );

        let b = changes[1].diff.as_deref().expect("b.rs has a diff");
        assert_eq!(stella_tui::diff::count_diff_lines(b), (1, 0));
    }

    /// The failure mode that would silently corrupt attribution: a removed
    /// source line that itself reads like a patch header. Git prefixes hunk
    /// bodies, so only a column-zero header opens a new file.
    #[test]
    fn a_header_shaped_line_inside_a_hunk_does_not_open_a_phantom_file() {
        let patch = "\
diff --git a/src/x.rs b/src/x.rs
@@ -1,3 +1,3 @@
-let s = \"diff --git a/fake b/fake\";
+let s = \"replaced\";
";
        let mut changes = vec![change("src/x.rs")];
        attach_diffs(&mut changes, patch);
        let x = changes[0].diff.as_deref().expect("x.rs has a diff");
        assert_eq!(
            stella_tui::diff::count_diff_lines(x),
            (1, 1),
            "the quoted header is content, not structure"
        );
        assert_eq!(split_patch_per_file(patch).len(), 1, "one file, not two");
    }

    #[test]
    fn a_deletion_is_keyed_by_the_path_that_existed() {
        let patch = "\
diff --git a/gone.rs b/dev/null
deleted file mode 100644
--- a/gone.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-one
-two
";
        // `b/dev/null` — git writes the b-side as `/dev/null` for a delete.
        let pairs = split_patch_per_file(patch);
        assert_eq!(pairs.len(), 1);
        let mut changes = vec![AdoptedChange {
            path: pairs[0].0.clone(),
            kind: FileChangeKind::Deleted,
            added: 0,
            removed: 0,
            diff: None,
        }];
        attach_diffs(&mut changes, patch);
        assert_eq!(
            stella_tui::diff::count_diff_lines(changes[0].diff.as_deref().unwrap()),
            (0, 2)
        );
    }

    #[test]
    fn a_path_the_patch_never_described_stays_uncounted_not_misattributed() {
        let mut changes = vec![change("src/a.rs"), change("src/untouched.rs")];
        attach_diffs(&mut changes, TWO_FILES);
        assert!(changes[0].diff.is_some());
        assert!(
            changes[1].diff.is_none(),
            "an undescribed path must not borrow another file's lines"
        );
    }

    #[test]
    fn paths_with_spaces_survive_the_header_split() {
        let patch = "diff --git a/my dir/a b.rs b/my dir/a b.rs\n@@ -1 +1 @@\n-x\n+y\n";
        let pairs = split_patch_per_file(patch);
        assert_eq!(pairs[0].0, "my dir/a b.rs");
    }

    #[test]
    fn an_empty_patch_attaches_nothing() {
        let mut changes = vec![change("src/a.rs")];
        attach_diffs(&mut changes, "");
        assert!(changes[0].diff.is_none());
    }

    #[test]
    fn name_status_maps_each_git_status_to_its_kind() {
        let parsed = parse_name_status("A\0new.rs\0M\0mod.rs\0D\0gone.rs\0");
        let described: Vec<(&str, FileChangeKind)> =
            parsed.iter().map(|c| (c.path.as_str(), c.kind)).collect();
        assert_eq!(
            described,
            vec![
                ("new.rs", FileChangeKind::Created),
                ("mod.rs", FileChangeKind::Modified),
                ("gone.rs", FileChangeKind::Deleted),
            ]
        );
        assert!(
            parsed.iter().all(|c| c.diff.is_none()),
            "diffs attach later"
        );
    }

    #[test]
    fn numstat_counts_come_from_git_not_from_recounting_the_patch() {
        let mut changes = vec![change("src/a.rs"), change("src/b.rs")];
        attach_numstat(&mut changes, "12\t3\tsrc/a.rs\0");
        assert_eq!((changes[0].added, changes[0].removed), (12, 3));
        assert_eq!(
            (changes[1].added, changes[1].removed),
            (0, 0),
            "a file numstat did not mention stays uncounted"
        );
    }

    #[test]
    fn a_binary_file_keeps_zero_counts_rather_than_failing() {
        let mut changes = vec![change("logo.png")];
        attach_numstat(&mut changes, "-\t-\tlogo.png\0");
        assert_eq!((changes[0].added, changes[0].removed), (0, 0));
    }

    #[test]
    fn numstat_handles_multiple_records_and_paths_with_spaces() {
        let mut changes = vec![change("a.rs"), change("my dir/b c.rs")];
        attach_numstat(&mut changes, "1\t0\ta.rs\0");
        attach_numstat(&mut changes, "4\t5\tmy dir/b c.rs\0");
        assert_eq!((changes[0].added, changes[0].removed), (1, 0));
        assert_eq!((changes[1].added, changes[1].removed), (4, 5));
    }

    /// The escape, and the discriminator that identifies it: the real tree
    /// holds bytes the candidate sealed at a path the candidate started
    /// without. Nothing else produces that blob.
    #[test]
    fn the_real_tree_holding_the_sealed_bytes_is_the_candidate_writing_outside() {
        assert_eq!(
            // Absent from the baseline, present in the real tree, and equal to
            // what the candidate sealed — the `cp` out of the workspace.
            PathDivergence::classify(Some("sealed"), None, Some("sealed")),
            PathDivergence::CandidateWroteOutside
        );
        assert_eq!(
            // Same reading when the path did exist before and the real tree now
            // carries the candidate's version of it.
            PathDivergence::classify(Some("sealed"), Some("base"), Some("sealed")),
            PathDivergence::CandidateWroteOutside
        );
    }

    /// The case the module used to assume was the only one. It must survive
    /// intact: content that is neither what the candidate started from nor what
    /// it produced came from outside the run.
    #[test]
    fn content_from_neither_known_state_is_an_edit_under_the_run() {
        assert_eq!(
            PathDivergence::classify(Some("other"), Some("base"), Some("sealed")),
            PathDivergence::ChangedUnderTheRun
        );
    }

    #[test]
    fn a_path_still_matching_its_baseline_is_not_the_conflict() {
        assert_eq!(
            PathDivergence::classify(Some("base"), Some("base"), Some("sealed")),
            PathDivergence::Matches
        );
        assert_eq!(
            PathDivergence::classify(None, None, Some("sealed")),
            PathDivergence::Matches,
            "a file the real tree does not have cannot be blocking a patch that writes it"
        );
        assert_eq!(
            PathDivergence::classify(None, Some("base"), Some("sealed")),
            PathDivergence::Matches,
            "an absent real file is still not what a rejected preimage collided with"
        );
    }

    /// A tree whose probes all failed reads as `Matches` everywhere, and then
    /// this must stay silent — git's own stderr is a better account than an
    /// attribution built from nothing.
    #[test]
    fn nothing_diverged_produces_no_attribution() {
        let classified = vec![
            ("a.rs".to_string(), PathDivergence::Matches),
            ("b.rs".to_string(), PathDivergence::Matches),
        ];
        assert_eq!(describe_divergence(&classified, false), None);
        assert_eq!(describe_divergence(&[], false), None);
    }

    #[test]
    fn the_escape_is_named_first_and_never_reported_as_a_user_edit() {
        let classified = vec![
            (
                "src/edited.rs".to_string(),
                PathDivergence::ChangedUnderTheRun,
            ),
            (
                "app/vm.js".to_string(),
                PathDivergence::CandidateWroteOutside,
            ),
        ];
        let reason = describe_divergence(&classified, false).expect("a divergence is described");

        let escape = reason
            .find("wrote outside its workspace")
            .expect("the escape is named");
        let edit = reason
            .find("changed under the run")
            .expect("the user edit is still reported");
        assert!(
            escape < edit,
            "the run's own defect leads; it recurs every turn until fixed: {reason}"
        );
        assert!(reason.contains("`app/vm.js`"), "{reason}");
        assert!(reason.contains("`src/edited.rs`"), "{reason}");
    }

    /// The old message blamed the user unconditionally. A pure escape must not
    /// mention a mid-run edit at all — that sentence is what sent the last
    /// diagnosis down the wrong path.
    #[test]
    fn a_pure_escape_does_not_mention_a_mid_run_edit() {
        let classified = vec![(
            "app/vm.js".to_string(),
            PathDivergence::CandidateWroteOutside,
        )];
        let reason = describe_divergence(&classified, false).expect("described");
        assert!(reason.contains("wrote outside its workspace"), "{reason}");
        assert!(
            !reason.contains("changed under the run"),
            "nothing changed under this run — the candidate did it: {reason}"
        );
    }

    #[test]
    fn many_conflicting_paths_are_summarized_not_pasted() {
        let classified: Vec<_> = (0..9)
            .map(|i| (format!("f{i}.rs"), PathDivergence::CandidateWroteOutside))
            .collect();
        let reason = describe_divergence(&classified, true).expect("described");
        assert!(
            reason.contains("`f0.rs`") && reason.contains("`f3.rs`"),
            "{reason}"
        );
        assert!(
            !reason.contains("`f4.rs`"),
            "only the first four are inlined: {reason}"
        );
        assert!(reason.contains("+5 more"), "{reason}");
        assert!(
            reason.contains("only the first conflicting paths were examined"),
            "a bounded examination must say it was bounded: {reason}"
        );
    }

    #[test]
    fn conflict_paths_fall_back_to_the_whole_patch() {
        let changes = vec![change("a.rs"), change("b.rs")];
        assert_eq!(
            conflict_paths_from_stderr("something unparseable\n", &changes),
            vec!["a.rs".to_string(), "b.rs".to_string()]
        );
        assert_eq!(
            conflict_paths_from_stderr("error: patch failed: a.rs:12\n", &changes),
            vec!["a.rs".to_string()]
        );
    }
}
