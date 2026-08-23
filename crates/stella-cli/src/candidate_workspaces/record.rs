// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What survives the process that made a candidate (#2813).
//!
//! Every candidate this substrate mints is registered in the plane's live
//! table, and that table is process memory. A `stella` killed mid-fan-out —
//! SIGKILL, OOM, a container teardown, a laptop lid — takes the only record of
//! its checkouts with it: the directories under
//! [`CANDIDATES_DIR`](super::CANDIDATES_DIR) and the `candidate/*` branches
//! beside them are then disk nobody can name. `stella fleet gc` cannot reclaim
//! them either, because this substrate deliberately sits outside the fleet's
//! namespace.
//!
//! So each candidate writes a small JSON record beside its checkout at
//! creation and deletes it at removal, and a later run in the same workspace
//! reads what is left.
//!
//! # It reports; it does not reclaim
//!
//! A leftover record is either **a crash's residue** or **a live sibling
//! run's**, and the difference is a fact about a process rather than about the
//! file. Deleting the second would destroy a concurrent fan-out's work — so
//! the sweep names what it found and names the command that reclaims it, and
//! the person reading decides. That is the same posture
//! [`ref_guard`](super::ref_guard) takes with an escaped ref: detection is the
//! host's business, repair is the user's.
//!
//! # What "the owner is gone" is decided by
//!
//! The recorded pid, probed with [`stella_store::sessions::pid_alive`] — the
//! predicate the session registry already downgrades a crashed session on, so
//! this workspace has one answer to "is that process still there" rather than
//! two.
//!
//! Its one boundary is pid reuse: a recycled pid makes a dead owner read as
//! live, and the record is then **not** reported. That is the safe direction
//! and the deliberate one — the failure it buys is a leak that stays until the
//! next sweep, against a false report that would invite a user to delete a
//! running candidate's tree. `started_at_ms` is the record's own write time,
//! so a report can say how long the residue has been there; it is not a
//! process start time, which nothing portable here can read.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The extension a record is written under, beside the checkout it names.
///
/// Beside rather than inside: `git worktree remove` and the recursive delete
/// under it take the checkout directory whole, so a record kept inside would
/// be destroyed by the one event it exists to describe.
const RECORD_EXT: &str = "candidate.json";

/// One candidate, as the filesystem remembers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct CandidateRecord {
    /// The handle the plugin addresses this candidate by, within the run that
    /// minted it. Unique per run, never across runs — which is why a record is
    /// named for the checkout's slug instead.
    pub(super) handle: String,
    /// The checkout, absolute.
    pub(super) checkout: PathBuf,
    /// The branch the checkout is cut onto — the second thing a reclaim has to
    /// remove, and the one a `rm -rf` would leave behind.
    pub(super) branch: String,
    /// The repository top level, so a reclaim command works from anywhere.
    pub(super) top: PathBuf,
    /// The process that owns it, for as long as that process exists.
    pub(super) pid: u32,
    /// When this record was written, in milliseconds since the epoch.
    pub(super) started_at_ms: u64,
}

/// Where the record for the checkout at `checkout` is written.
///
/// `None` for a checkout with no final component, which no worktree this
/// substrate creates has.
fn path_for(checkout: &Path) -> Option<PathBuf> {
    let slug = checkout.file_name()?.to_str()?;
    Some(checkout.with_file_name(format!("{slug}.{RECORD_EXT}")))
}

impl CandidateRecord {
    /// The record this process would write for `worktree`, now.
    pub(super) fn of(handle: &str, worktree: &stella_fleet::git::Worktree, top: &Path) -> Self {
        Self {
            handle: handle.to_string(),
            checkout: worktree.path.clone(),
            branch: worktree.branch.clone(),
            top: top.to_path_buf(),
            pid: std::process::id(),
            started_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(0)),
        }
    }

    /// Write it beside its checkout.
    ///
    /// # Errors
    ///
    /// Whatever the write failed with. Best-effort at the call site, for the
    /// reason every baseline in this substrate is: a candidate that could not
    /// be *described* is still a candidate that should run.
    pub(super) fn write(&self) -> std::io::Result<()> {
        let Some(path) = path_for(&self.checkout) else {
            return Err(std::io::Error::other(
                "a candidate checkout with no final path component",
            ));
        };
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, json)
    }
}

/// Forget the record for the checkout at `checkout`, if there is one.
///
/// Silent about a record that is already gone: removal is what the end-of-run
/// sweep does to everything, and a second pass must not report a failure no
/// one can act on ([`super::SessionCandidateWorkspaces::remove`]'s contract).
pub(super) fn forget(checkout: &Path) {
    if let Some(path) = path_for(checkout) {
        let _ = std::fs::remove_file(path);
    }
}

/// Every record under `dir` whose owning process is gone, oldest first.
///
/// `alive` is injected rather than called directly so the sweep is witnessable
/// against a chosen answer — a test cannot kill a process and then be sure the
/// pid was not reused before it looked.
pub(super) fn orphans(dir: &Path, alive: &dyn Fn(u32) -> bool) -> Vec<CandidateRecord> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        // No candidates directory is the ordinary case: a workspace that has
        // never fanned out has nothing to report.
        return Vec::new();
    };
    let mut found: Vec<CandidateRecord> = entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(RECORD_EXT))
        })
        .filter_map(|entry| std::fs::read(entry.path()).ok())
        // A record this host cannot parse is one a future version wrote, or a
        // truncated write from the crash itself. Skipped rather than reported:
        // a reclaim command built from fields that did not parse is a command
        // that names the wrong path.
        .filter_map(|bytes| serde_json::from_slice::<CandidateRecord>(&bytes).ok())
        .filter(|record| !alive(record.pid))
        .collect();
    found.sort_by(|left, right| {
        (left.started_at_ms, &left.checkout).cmp(&(right.started_at_ms, &right.checkout))
    });
    found
}

/// What a run says about the residue it found, one line per orphan.
///
/// Each line carries the reclaim command whole, because the two halves of a
/// reclaim are easy to half-do: removing the checkout and leaving the
/// `candidate/*` branch behind is how a repository accumulates branches nobody
/// can attribute.
pub(super) fn reclaim_lines(orphans: &[CandidateRecord]) -> Vec<String> {
    orphans
        .iter()
        .map(|record| {
            format!(
                "a candidate workspace outlived the run that made it (pid {}, gone): {} on \
                 branch {}. Nothing was deleted — copy any work out, then reclaim it with \
                 `git -C {} worktree remove --force {} && git -C {} branch -D {}`",
                record.pid,
                record.checkout.display(),
                record.branch,
                record.top.display(),
                record.checkout.display(),
                record.top.display(),
                record.branch,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(handle: &str, pid: u32, at: u64, dir: &Path) -> CandidateRecord {
        CandidateRecord {
            handle: handle.to_string(),
            checkout: dir.join(handle),
            branch: format!("candidate/{handle}"),
            top: dir.to_path_buf(),
            pid,
            started_at_ms: at,
        }
    }

    /// The record is beside the checkout, never inside it — the removal it
    /// exists to survive takes the directory whole.
    #[test]
    fn a_record_lives_beside_its_checkout() {
        let path = path_for(Path::new("/w/.stella/private/candidates/p-0-abcd")).unwrap();
        assert_eq!(
            path,
            Path::new("/w/.stella/private/candidates/p-0-abcd.candidate.json")
        );
        assert!(!path.starts_with("/w/.stella/private/candidates/p-0-abcd/"));
    }

    /// **#2813's witness, at the sweep.** A dead owner's record is named; a
    /// live one's is left alone.
    #[test]
    fn the_sweep_names_a_dead_owners_record_and_leaves_a_live_ones() {
        let dir = tempfile::tempdir().unwrap();
        record("dead-0", 4242, 10, dir.path()).write().unwrap();
        record("live-0", 4243, 20, dir.path()).write().unwrap();

        let found = orphans(dir.path(), &|pid| pid == 4243);
        assert_eq!(found.len(), 1, "exactly the dead owner's: {found:?}");
        assert_eq!(found[0].handle, "dead-0");
    }

    #[test]
    fn a_workspace_that_never_fanned_out_reports_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(orphans(&dir.path().join("candidates"), &|_| false).is_empty());
    }

    /// A half-written record from the crash itself is skipped, not reported: a
    /// reclaim command built from fields that did not parse names the wrong
    /// path.
    #[test]
    fn an_unparseable_record_is_not_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(format!("truncated.{RECORD_EXT}")),
            b"{\"handle\":\"trunc",
        )
        .unwrap();
        record("dead-0", 4242, 10, dir.path()).write().unwrap();

        let found = orphans(dir.path(), &|_| false);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].handle, "dead-0");
    }

    /// A record is forgotten by the removal that took its checkout, so the
    /// next run's sweep has nothing to report about a candidate that ended
    /// cleanly.
    #[test]
    fn forgetting_a_record_takes_it_out_of_the_sweep() {
        let dir = tempfile::tempdir().unwrap();
        let kept = record("dead-0", 4242, 10, dir.path());
        kept.write().unwrap();
        assert_eq!(orphans(dir.path(), &|_| false).len(), 1);

        forget(&kept.checkout);
        assert!(orphans(dir.path(), &|_| false).is_empty());
        // Twice is not an error: the sweep removes what adoption already took.
        forget(&kept.checkout);
    }

    #[test]
    fn a_reclaim_line_names_both_halves_of_the_reclaim() {
        let dir = tempfile::tempdir().unwrap();
        let line = reclaim_lines(&[record("dead-0", 4242, 10, dir.path())]).remove(0);
        assert!(line.contains("worktree remove --force"), "{line}");
        assert!(line.contains("branch -D candidate/dead-0"), "{line}");
        assert!(
            line.contains("Nothing was deleted"),
            "the sweep reports; it does not reclaim: {line}"
        );
    }
}
