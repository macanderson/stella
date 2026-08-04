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
