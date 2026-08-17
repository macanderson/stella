// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

use std::path::PathBuf;

use super::*;

fn scratch() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let guard = tempfile::tempdir().unwrap();
    let ws = guard.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let store = guard.path().join("store");
    (guard, ws, store)
}

/// The witness for #3413(a): a turn that mutates a file through *no
/// path-naming tool at all* — the only kind of turn the 12-tool surface can
/// produce — is measurable, with git's own counts rather than `0/0`.
#[test]
fn a_turn_that_named_no_paths_still_reports_what_it_changed_with_real_counts() {
    let (_guard, ws, store) = scratch();
    std::fs::write(ws.join("kept.txt"), "same\n").unwrap();
    std::fs::write(ws.join("edited.rs"), "one\ntwo\nthree\n").unwrap();
    let journal = WorkJournal::open_in(&store, &ws, "ses-witness").unwrap();

    // Turn 1 establishes the baseline and deliberately reports nothing:
    // "every file in the workspace" is not what a turn changed.
    assert_eq!(
        journal.snapshot_worktree().unwrap(),
        Vec::new(),
        "the first snapshot is a baseline, not a delta"
    );

    // Turn 2: the tree changes, and nothing anywhere named a path.
    std::fs::write(ws.join("edited.rs"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
    std::fs::write(ws.join("born.rs"), "new\n").unwrap();
    std::fs::remove_file(ws.join("kept.txt")).unwrap();

    let mut changes = journal.snapshot_worktree().unwrap();
    changes.sort_by(|a, b| a.path.cmp(&b.path));

    assert_eq!(
        changes.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
        vec!["born.rs", "edited.rs", "kept.txt"]
    );
    assert_eq!(changes[0].kind, JournalChangeKind::Created);
    assert_eq!(changes[1].kind, JournalChangeKind::Modified);
    assert_eq!(changes[2].kind, JournalChangeKind::Deleted);

    // The half that makes this a fix rather than a listing: real counts.
    assert_eq!((changes[1].added, changes[1].removed), (2, 0));
    assert_eq!((changes[0].added, changes[0].removed), (1, 0));
    assert_eq!((changes[2].added, changes[2].removed), (0, 1));
    assert!(
        changes.iter().any(|c| c.added > 0),
        "a real edit must never report +0 -0 (#2290)"
    );
    assert!(
        changes[1]
            .diff
            .as_deref()
            .is_some_and(|d| d.contains("four")),
        "each change carries its own slice of the patch"
    );
}

#[test]
fn a_read_only_turn_writes_no_commit_and_reports_nothing() {
    let (_guard, ws, store) = scratch();
    std::fs::write(ws.join("a.txt"), "v1\n").unwrap();
    let journal = WorkJournal::open_in(&store, &ws, "ses-quiet").unwrap();
    journal.snapshot_worktree().unwrap();
    let tip = journal.snapshot_tip();

    assert_eq!(journal.snapshot_worktree().unwrap(), Vec::new());
    assert_eq!(
        journal.snapshot_tip(),
        tip,
        "an unchanged tree leaves the ref naming the moment content last moved"
    );
}

#[test]
fn ignored_paths_stay_out_of_the_answer() {
    // `.stella/private/` is rewritten by the session itself on every turn.
    // Reporting it as the agent's work would drown every real change.
    let (_guard, ws, store) = scratch();
    std::fs::write(ws.join(".gitignore"), "ignored/\n").unwrap();
    std::fs::create_dir_all(ws.join("ignored")).unwrap();
    let journal = WorkJournal::open_in(&store, &ws, "ses-ignore").unwrap();
    journal.snapshot_worktree().unwrap();

    std::fs::write(ws.join("ignored/noise.db"), "churn\n").unwrap();
    std::fs::write(ws.join("real.rs"), "work\n").unwrap();

    let changes = journal.snapshot_worktree().unwrap();
    assert_eq!(
        changes.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
        vec!["real.rs"]
    );
}

#[test]
fn the_snapshot_lineage_leaves_the_checkpoint_blobs_alone() {
    // The two producers share a git dir and must not fight over one tree:
    // a whole-tree snapshot on the session head would delete every reserved
    // blob it did not itself write.
    let (_guard, ws, store) = scratch();
    std::fs::write(ws.join("a.txt"), "v1\n").unwrap();
    let journal = WorkJournal::open_in(&store, &ws, "ses-both").unwrap();
    journal
        .record_checkpoint(r#"{"step":1}"#, None, None)
        .unwrap();
    journal.snapshot_worktree().unwrap();
    std::fs::write(ws.join("a.txt"), "v2\n").unwrap();
    journal.snapshot_worktree().unwrap();

    assert_eq!(
        journal.checkpoint().as_deref(),
        Some(r#"{"step":1}"#),
        "snapshotting the work tree must not retract the resume point"
    );
}

#[test]
fn a_real_file_merely_starting_with_the_reserved_name_survives() {
    let (_guard, ws, store) = scratch();
    let journal = WorkJournal::open_in(&store, &ws, "ses-reserved").unwrap();
    journal.snapshot_worktree().unwrap();
    std::fs::write(ws.join(".stella-journal-notes"), "mine\n").unwrap();

    let changes = journal.snapshot_worktree().unwrap();
    assert_eq!(
        changes.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
        vec![".stella-journal-notes"],
        "the prefix filter carries its slash"
    );
}

#[test]
fn a_binary_file_keeps_zero_counts_rather_than_vanishing() {
    let (_guard, ws, store) = scratch();
    let journal = WorkJournal::open_in(&store, &ws, "ses-binary").unwrap();
    journal.snapshot_worktree().unwrap();
    std::fs::write(ws.join("blob.bin"), [0u8, 159, 146, 150]).unwrap();

    let changes = journal.snapshot_worktree().unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].path, "blob.bin");
    assert_eq!(
        (changes[0].added, changes[0].removed),
        (0, 0),
        "git reports `-`/`-` for a binary; it has no lines to count"
    );
}

#[test]
fn a_non_git_workspace_is_still_measurable() {
    // Same contract as the rest of the journal: durability must not depend on
    // the user having run `git init`.
    let (_guard, ws, store) = scratch();
    assert!(!ws.join(".git").exists());
    let journal = WorkJournal::open_in(&store, &ws, "ses-nogit").unwrap();
    journal.snapshot_worktree().unwrap();
    std::fs::write(ws.join("a.txt"), "v1\n").unwrap();

    let changes = journal.snapshot_worktree().unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].kind, JournalChangeKind::Created);
}

#[test]
fn name_status_maps_each_git_status_to_its_kind() {
    let parsed = parse_name_status("A\0born\0M\0edited\0D\0gone\0T\0retyped\0");
    assert_eq!(
        parsed
            .iter()
            .map(|c| (c.path.as_str(), c.kind))
            .collect::<Vec<_>>(),
        vec![
            ("born", JournalChangeKind::Created),
            ("edited", JournalChangeKind::Modified),
            ("gone", JournalChangeKind::Deleted),
            // A typechange is a modification to an observer, and reporting it
            // beats dropping a real change on the floor.
            ("retyped", JournalChangeKind::Modified),
        ]
    );
}

#[test]
fn counts_come_from_git_not_from_recounting_the_patch() {
    // The #2290 rule, as a test: the numbers are git's, and a patch that
    // disagrees with them does not get to win.
    let mut changes = parse_name_status("M\0a.rs\0M\0b.bin\0");
    attach_numstat(&mut changes, "12\t3\ta.rs\0-\t-\tb.bin\0");
    assert_eq!((changes[0].added, changes[0].removed), (12, 3));
    assert_eq!((changes[1].added, changes[1].removed), (0, 0));
}

#[test]
fn a_header_shaped_line_inside_a_hunk_does_not_open_a_phantom_file() {
    let patch = "diff --git a/real.rs b/real.rs\n\
                 @@ -1 +1,2 @@\n\
                 +diff --git a/fake.rs b/fake.rs\n";
    let cut = split_patch_per_file(patch);
    assert_eq!(cut.len(), 1);
    assert_eq!(cut[0].0, "real.rs");
}

#[test]
fn a_deletion_is_keyed_by_the_path_that_existed() {
    let cut = split_patch_per_file("diff --git a/gone.rs b//dev/null\n@@ -1 +0,0 @@\n-x\n");
    assert_eq!(cut[0].0, "gone.rs");
}

#[test]
fn paths_with_spaces_survive_the_header_split() {
    let cut = split_patch_per_file("diff --git a/my file.rs b/my file.rs\n@@ -1 +1 @@\n");
    assert_eq!(cut[0].0, "my file.rs");
}
