// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A `stella doctor` check. It looks for a pile of `stella fleet` worktrees
//! under `.stella/worktrees/`.
//!
//! `stella fleet clean` removes a finished task's worktree and its
//! `fleet/*` branch. Nothing else shows the pile first. A workspace can
//! hold dozens of stale worktrees and several GB with no warning.
//! [`report`] is that warning. It always reports the count and the size on
//! disk. At or under [`SPLIT_THRESHOLD`] worktrees, it also reports how
//! many `stella fleet clean` would reclaim.
//!
//! [`report`] does not redo that work. It calls
//! [`stella_fleet::Gc::sweep`] with `dry_run: true` and counts the verdicts
//! it returns. So "reclaimable" here means what it means to `stella fleet
//! clean`.
//!
//! # When the split runs
//!
//! A dry-run sweep runs `git` several times per worktree. The bigger the
//! pile, the more that costs. And a big pile is what this check looks for.
//! `doctor` has to stay fast. So the sweep runs at or under
//! [`SPLIT_THRESHOLD`] worktrees, and is skipped above it. Above it,
//! [`report`] gives the count and the size alone. `doctor` then points at
//! `stella fleet clean --dry-run` for the rest.

use std::path::Path;

use stella_fleet::{BranchAction, Gc, GcOptions, Ledger, SystemGitCli};

/// Worktree counts at or under this get the (git-spawning) reclaimable/kept
/// split; above it `doctor` reports count and size only. See the module doc
/// for the direction this gate runs.
const SPLIT_THRESHOLD: usize = 8;

/// The reclaimable/kept breakdown one dry-run sweep produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Split {
    pub(crate) worktrees_reclaimable: usize,
    pub(crate) worktrees_kept: usize,
    /// `fleet/*` branches with no worktree of their own.
    pub(crate) branches_reclaimable: usize,
    pub(crate) branches_kept: usize,
}

/// What [`report`] found. Pure data — [`crate::doctor`] turns this into a
/// [`crate::doctor::Check`], the same split [`crate::settings_check`] uses
/// for the model-config check.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FleetWorktreeReport {
    /// How many worktrees live under `.stella/worktrees/`. Zero — the
    /// default — when the directory does not exist: a workspace that has
    /// never run `stella fleet`, which must produce no noise.
    pub(crate) count: usize,
    /// Their total size on disk, in bytes.
    pub(crate) total_bytes: u64,
    /// `Some` when a dry-run sweep ran (`count` at or under
    /// [`SPLIT_THRESHOLD`]) and succeeded.
    pub(crate) split: Option<Split>,
    /// Set when a sweep was attempted but failed — a diagnosable git error,
    /// never a reason to fail the check.
    pub(crate) sweep_error: Option<String>,
}

/// Walk `.stella/worktrees/` and, when the count warrants it, run a
/// `dry_run` sweep. Never mutates a file, a branch, or the ledger.
pub(crate) fn report(workspace_root: &Path) -> FleetWorktreeReport {
    let worktrees_root = workspace_root.join(".stella").join("worktrees");
    let entries: Vec<_> = match std::fs::read_dir(&worktrees_root) {
        Ok(read) => read.filter_map(Result::ok).collect(),
        Err(_) => return FleetWorktreeReport::default(),
    };
    let count = entries.len();
    if count == 0 {
        return FleetWorktreeReport::default();
    }
    let total_bytes = entries.iter().map(|entry| path_size(&entry.path())).sum();

    if count > SPLIT_THRESHOLD {
        return FleetWorktreeReport {
            count,
            total_bytes,
            split: None,
            sweep_error: None,
        };
    }
    match sweep(workspace_root) {
        Ok(split) => FleetWorktreeReport {
            count,
            total_bytes,
            split: Some(split),
            sweep_error: None,
        },
        Err(error) => FleetWorktreeReport {
            count,
            total_bytes,
            split: None,
            sweep_error: Some(error),
        },
    }
}

/// The on-disk size of a file, or the recursive size of a directory.
/// [`std::fs::symlink_metadata`] does not traverse a symlink, so one is
/// counted by its own size rather than its target's — the same reason `du`
/// never follows one by default.
fn path_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| path_size(&entry.path()))
        .sum()
}

/// Runs a `dry_run: true` sweep. It reads the same activity `stella fleet
/// clean` reads (`crate::fleet_gc::clean`): the ledger's worktree activity
/// when a ledger exists, or an empty list when it does not. A workspace with
/// no ledger has nothing in flight.
fn sweep(workspace_root: &Path) -> Result<Split, String> {
    let ledger_path = workspace_root
        .join(".stella")
        .join("private")
        .join("fleet.db");
    let activity = if ledger_path.exists() {
        Ledger::open(&ledger_path)
            .map_err(|e| format!("could not open the fleet ledger: {e}"))?
            .worktree_activity()
            .map_err(|e| format!("could not read the fleet ledger: {e}"))?
    } else {
        Vec::new()
    };

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("system clock is before the Unix epoch: {e}"))?
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;

    // This builds its own runtime instead of reusing one. `stella doctor`
    // runs before the shared runtime in `run()` exists this deep in the call
    // chain. Every other check in `doctor.rs`, and every test that drives
    // it, stays synchronous. One runtime here, for the one async call, costs
    // less than making the whole module async.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to start a runtime for the fleet sweep: {e}"))?;

    let gc_report = rt
        .block_on(Gc::new(SystemGitCli, workspace_root).sweep(
            &activity,
            &GcOptions {
                dry_run: true,
                ..GcOptions::default()
            },
            now_ms,
        ))
        .map_err(|e| e.to_string())?;

    let worktrees_reclaimable = gc_report.reclaimed_worktrees();
    let branches_reclaimable = gc_report
        .branches
        .iter()
        .filter(|v| matches!(v.action, BranchAction::Deleted | BranchAction::WouldDelete))
        .count();
    Ok(Split {
        worktrees_reclaimable,
        worktrees_kept: gc_report
            .worktrees
            .len()
            .saturating_sub(worktrees_reclaimable),
        branches_reclaimable,
        branches_kept: gc_report
            .branches
            .len()
            .saturating_sub(branches_reclaimable),
    })
}

/// `12.3 MB`-style rendering for [`crate::doctor`]'s summary line. Binary
/// (1024) units, because that is what the filesystem this counts is
/// actually laid out in.
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use stella_fleet::{GitCli, WorktreeManager};

    use super::*;

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// This copies the `seed_repo` fixture from `gc/tests.rs`. This crate
    /// cannot import a test helper from another crate's test module. The
    /// shape — init, configure, one commit — is what this witness needs.
    fn seed_repo(dir: &Path) -> String {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let git = SystemGitCli;
            for args in [
                vec!["init", "-q", "-b", "main"],
                vec!["config", "user.email", "fleet@test.local"],
                vec!["config", "user.name", "Fleet Test"],
                vec!["config", "commit.gpgsign", "false"],
            ] {
                let out = git.run(dir, &args).await.expect("git spawn");
                assert!(out.success, "git {args:?} failed: {}", out.stderr);
            }
            std::fs::write(dir.join("README.md"), "seed\n").expect("write seed file");
            for args in [
                vec!["add", "--", "README.md"],
                vec!["commit", "-q", "-m", "seed"],
            ] {
                let out = git.run(dir, &args).await.expect("git spawn");
                assert!(out.success, "git {args:?} failed: {}", out.stderr);
            }
            let out = git
                .run(dir, &["rev-parse", "HEAD"])
                .await
                .expect("rev-parse");
            out.stdout.trim().to_string()
        })
    }

    /// A workspace with neither `.stella/worktrees/` nor a fleet ledger must
    /// produce no noise — the issue's own constraint.
    #[test]
    fn no_worktrees_directory_reports_the_zero_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(report(dir.path()), FleetWorktreeReport::default());
    }

    /// Above [`SPLIT_THRESHOLD`], `doctor` reports count and size only. It
    /// must not spawn `git` for a sweep. This test needs no real
    /// repository — just enough directories to cross the gate.
    ///
    /// It is also where the reported size is pinned to an exact number.
    /// Every worktree it builds holds one 1-byte file, so the total is the
    /// worktree count and nothing else.
    #[test]
    fn above_the_threshold_reports_count_and_size_without_a_sweep() {
        let dir = tempfile::tempdir().unwrap();
        let worktrees_root = dir.path().join(".stella").join("worktrees");
        for i in 0..(SPLIT_THRESHOLD + 1) {
            let wt = worktrees_root.join(format!("wt-{i}"));
            std::fs::create_dir_all(&wt).unwrap();
            std::fs::write(wt.join("f.txt"), "x").unwrap();
        }

        let report = report(dir.path());
        assert_eq!(report.count, SPLIT_THRESHOLD + 1);
        // The total is exact here: `path_size` sums file lengths and never
        // counts the directory inodes holding them, so one 1-byte file per
        // worktree makes the byte total equal the count. A floor (`> 0`)
        // would survive a double-counted directory, a followed symlink, or
        // a dropped file; this does not.
        assert_eq!(
            usize::try_from(report.total_bytes).unwrap(),
            SPLIT_THRESHOLD + 1,
            "one 1-byte file per worktree: {report:?}"
        );
        assert_eq!(
            report.split, None,
            "above the threshold the split must not run: {report:?}"
        );
        assert_eq!(
            report.sweep_error, None,
            "a skipped sweep is not a failed one: {report:?}"
        );
    }

    /// The witness: a real repository with a finished worktree and a dirty
    /// one. `doctor` reports the true count and size, and the dry-run split
    /// names the finished one reclaimable and the dirty one kept — the exact
    /// numbers `stella fleet clean` would act on. This test compiles only
    /// against `crate::doctor_fleet::report`, so it cannot pass on a tree
    /// that lacks this module.
    #[test]
    fn reports_the_true_count_size_and_reclaimable_kept_split() {
        if !git_available() {
            eprintln!("skipping fleet-worktree doctor witness: `git` not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let base = seed_repo(repo);
        let mgr = WorktreeManager::new(SystemGitCli, repo);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let (done, dirty) = rt.block_on(async {
            let done = mgr.create("done", &base).await.expect("create done");
            let dirty = mgr.create("dirty", &base).await.expect("create dirty");
            (done, dirty)
        });
        std::fs::write(dirty.path.join("scratch.txt"), "unsaved work\n").unwrap();

        let report = report(repo);
        assert_eq!(report.count, 2, "{report:?}");
        // A real worktree's `.git` file records an absolute path into the
        // tempdir, and that path's length changes on every run, so there is
        // no fixed byte total to assert against here. The exact size lives
        // in `above_the_threshold_reports_count_and_size_without_a_sweep`,
        // which builds its worktrees by hand.
        assert!(report.total_bytes > 0, "{report:?}");
        assert_eq!(report.sweep_error, None, "{report:?}");
        assert_eq!(
            report.split,
            Some(Split {
                worktrees_reclaimable: 1,
                worktrees_kept: 1,
                branches_reclaimable: 0,
                branches_kept: 0,
            }),
            "one finished worktree is reclaimable, the dirty one is kept: {report:?}"
        );

        assert!(done.path.exists(), "a dry run must remove nothing");
    }

    #[test]
    fn human_bytes_renders_binary_units() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
    }
}
