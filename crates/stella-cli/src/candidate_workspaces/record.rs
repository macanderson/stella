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
//! two — **and** the start token of whatever is wearing that pid now
//! ([`proc_start`](super::proc_start)).
//!
//! The pid alone was not enough, and the gap was documented rather than
//! closed: a recycled pid made a dead owner read as live, so its record went
//! unreported and its checkout leaked (#4511). `(pid, start token)` closes it,
//! because the kernel hands the number out again but not with the same start
//! instant.
//!
//! Where a start token cannot be read — a platform with neither `/proc` nor
//! `sysctl`, a record written by a build older than the field, a container
//! that did not mount `/proc` — the pre-existing rule stands unchanged: pid
//! alive means owner alive. `None` is "this host cannot tell", never "the
//! owner is gone", and inventing the second would invite a user to delete a
//! running candidate's tree.
//!
//! `started_at_ms` is a different number: the record's own **write** time, so a
//! report can say how long residue has been there. Nothing compares the two.
//!
//! # What the sweep names besides checkouts
//!
//! A run that ends before anything scores its candidates writes each one's work
//! out as a `.patch` beside the checkout and prints where (#2651). Removal then
//! takes the checkout *and its record*, so from the next run on there was
//! nothing left to re-name that patch — a user who missed one stderr line had
//! to go and find it. So the sweep also lists the patches themselves, from the
//! directory rather than from a record, which is what makes them keep being
//! named until somebody removes them.

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
    /// remove, and the one a `rm -rf` would leave behind. `None` for a
    /// copy-tree candidate ([`super::copy_tree`]), which is a directory and
    /// nothing else, so its reclaim is a directory's.
    #[serde(default)]
    pub(super) branch: Option<String>,
    /// The repository top level a reclaim command runs at, so it works from
    /// anywhere. `None` alongside a `None` branch, for the same reason.
    #[serde(default)]
    pub(super) top: Option<PathBuf>,
    /// The process that owns it, for as long as that process exists.
    pub(super) pid: u32,
    /// The start token of that process, where this host could read one — the
    /// half of the owner's identity that a recycled pid does not carry over
    /// (#4511). See [`proc_start`](super::proc_start) for what the number is.
    ///
    /// `default` rather than required: a record written before this field
    /// existed parses, and reads as the "cannot tell" case, which is exactly
    /// what it is.
    #[serde(default)]
    pub(super) pid_start: Option<u64>,
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
    /// The record this process would write for a plain directory, now — the
    /// copy-tree substrate's shape, where the reclaim is a directory's.
    pub(super) fn of_directory(handle: &str, checkout: &Path) -> Self {
        Self {
            handle: handle.to_string(),
            checkout: checkout.to_path_buf(),
            branch: None,
            top: None,
            pid: std::process::id(),
            pid_start: super::proc_start::of(std::process::id()),
            started_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(0)),
        }
    }

    /// The record this process would write for `worktree`, now.
    pub(super) fn of(handle: &str, worktree: &stella_fleet::git::Worktree, top: &Path) -> Self {
        Self {
            branch: Some(worktree.branch.clone()),
            top: Some(top.to_path_buf()),
            ..Self::of_directory(handle, &worktree.path)
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
/// one can act on ([`CandidateWorkspaces::remove`]'s contract).
///
/// [`CandidateWorkspaces::remove`]: stella_runtime::wrapper::CandidateWorkspaces::remove
pub(super) fn forget(checkout: &Path) {
    if let Some(path) = path_for(checkout) {
        let _ = std::fs::remove_file(path);
    }
}

/// Whether the process that wrote `record` is gone.
///
/// Two questions, in the order that makes the second cheap: a pid nothing is
/// wearing settles it, and only a live pid is worth asking whose it is now.
///
/// A start token on both sides that **differs** is a different process wearing
/// the recorded number — the recycled-pid case, and the one this decides that
/// `alive` alone could not. A token missing on either side leaves the
/// pre-existing rule in place: alive means owner alive. See the module doc for
/// why the absent reading is deliberately not the interesting answer.
fn owner_is_gone(
    record: &CandidateRecord,
    alive: &dyn Fn(u32) -> bool,
    started: &dyn Fn(u32) -> Option<u64>,
) -> bool {
    if !alive(record.pid) {
        return true;
    }
    match (record.pid_start, started(record.pid)) {
        (Some(recorded), Some(current)) => recorded != current,
        _ => false,
    }
}

/// Every record under `dir` whose owning process is gone, oldest first.
///
/// `alive` and `started` are injected rather than called directly so the sweep
/// is witnessable against a chosen answer — a test cannot kill a process and
/// then be sure the pid was not reused before it looked, which is the very
/// race the pair exists to survive.
pub(super) fn orphans(
    dir: &Path,
    alive: &dyn Fn(u32) -> bool,
    started: &dyn Fn(u32) -> Option<u64>,
) -> Vec<CandidateRecord> {
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
        .filter(|record| owner_is_gone(record, alive, started))
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
            let checkout = record.checkout.display();
            let how = match (&record.branch, &record.top) {
                (Some(branch), Some(top)) => format!(
                    "`git -C {top} worktree remove --force {checkout} && \
                     git -C {top} branch -D {branch}`",
                    top = top.display()
                ),
                // A copy-tree candidate is a directory and nothing else, so a
                // directory's reclaim is the whole of it.
                _ => format!("`rm -rf {checkout}`"),
            };
            format!(
                "a candidate workspace outlived the run that made it (pid {}, gone): \
                 {checkout}. Nothing was deleted — copy any work out, then reclaim it \
                 with {how}",
                record.pid,
            )
        })
        .collect()
}

/// The extension [`SessionCandidateWorkspaces::write_patch`] keeps unscored
/// work under, beside the checkout it came from.
///
/// [`SessionCandidateWorkspaces::write_patch`]: super::SessionCandidateWorkspaces
const PATCH_EXT: &str = ".patch";

/// Every preserved patch under `dir`, in a stable order.
///
/// Sorted by path rather than by mtime: the order a run reports its residue in
/// must not change between two runs that found the same residue, and a
/// filesystem's directory order is not an order at all.
fn preserved_patches(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(PATCH_EXT))
        })
        .collect();
    found.sort();
    found
}

/// What a run says about the work earlier runs kept, one line per patch.
///
/// Each line names the removal as well as the application, because this list
/// is the one thing that stops repeating: a patch nobody deletes is named by
/// every run from now until somebody does, which is the point — and a user
/// told only how to apply it has no way to make the line go away.
fn preserved_lines(patches: &[PathBuf]) -> Vec<String> {
    patches
        .iter()
        .map(|path| {
            let path = path.display();
            format!(
                "a candidate's work was kept from a run that ended before anything scored it: \
                 {path}. Apply it with `git apply --binary {path}`, then delete it — this line \
                 repeats until you do"
            )
        })
        .collect()
}

/// One line per orphan and one per preserved patch under `dir`, for a substrate
/// to hand its host.
///
/// A free function rather than a method because both substrates keep their
/// checkouts in the same directory and answer this question identically — the
/// records say which shape each one was.
///
/// Records first: a leftover checkout is a live question about disk somebody
/// may still be writing to, and a patch is settled work waiting to be read.
pub(super) fn report(dir: &Path) -> Vec<String> {
    let mut lines = reclaim_lines(&orphans(
        dir,
        &stella_store::sessions::pid_alive,
        &super::proc_start::of,
    ));
    lines.extend(preserved_lines(&preserved_patches(dir)));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(handle: &str, pid: u32, at: u64, dir: &Path) -> CandidateRecord {
        CandidateRecord {
            handle: handle.to_string(),
            checkout: dir.join(handle),
            branch: Some(format!("candidate/{handle}")),
            top: Some(dir.to_path_buf()),
            pid,
            pid_start: None,
            started_at_ms: at,
        }
    }

    /// A host with no start-time interface, which is the pre-#4511 answer and
    /// still the answer wherever neither `/proc` nor `sysctl` exists.
    fn unreadable_start(_pid: u32) -> Option<u64> {
        None
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

        let found = orphans(dir.path(), &|pid| pid == 4243, &unreadable_start);
        assert_eq!(found.len(), 1, "exactly the dead owner's: {found:?}");
        assert_eq!(found[0].handle, "dead-0");
    }

    #[test]
    fn a_workspace_that_never_fanned_out_reports_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            orphans(
                &dir.path().join("candidates"),
                &|_| false,
                &unreadable_start
            )
            .is_empty()
        );
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

        let found = orphans(dir.path(), &|_| false, &unreadable_start);
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
        assert_eq!(orphans(dir.path(), &|_| false, &unreadable_start).len(), 1);

        forget(&kept.checkout);
        assert!(orphans(dir.path(), &|_| false, &unreadable_start).is_empty());
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

    /// A copy-tree candidate is a directory and nothing else, so naming a
    /// branch reclaim for it would send a user to a `git` command against a
    /// tree that may not even be a repository.
    #[test]
    fn a_directory_candidates_reclaim_is_a_directorys() {
        let dir = tempfile::tempdir().unwrap();
        let line = reclaim_lines(&[CandidateRecord::of_directory(
            "candidate-0",
            &dir.path().join("copy-0"),
        )])
        .remove(0);
        assert!(line.contains("rm -rf"), "{line}");
        assert!(!line.contains("git "), "{line}");
    }

    /// **#4511's witness, half one.** A recycled pid makes a dead owner read
    /// live; the start token is what tells the two apart, and without it this
    /// record goes unreported and its checkout leaks.
    #[test]
    fn a_recycled_pid_does_not_make_a_dead_owner_read_live() {
        let dir = tempfile::tempdir().unwrap();
        let mut leaked = record("crashed-0", 4242, 10, dir.path());
        leaked.pid_start = Some(1_000);
        leaked.write().unwrap();

        // The number is in use again — by something that started later.
        let found = orphans(dir.path(), &|_| true, &|_| Some(2_000));
        assert_eq!(
            found.len(),
            1,
            "a different process wears the pid: {found:?}"
        );
        assert_eq!(found[0].handle, "crashed-0");

        // And the owner itself still reads as its own owner.
        assert!(
            orphans(dir.path(), &|_| true, &|_| Some(1_000)).is_empty(),
            "a live owner's own record must never be offered for reclaim"
        );
    }

    /// A host that cannot read a start token keeps the pre-#4511 rule, and a
    /// record written before the field existed is exactly that host's case.
    /// `None` is "cannot tell", never "gone".
    #[test]
    fn an_unreadable_start_leaves_a_live_pid_alone() {
        let dir = tempfile::tempdir().unwrap();
        record("live-0", 4242, 10, dir.path()).write().unwrap();
        assert!(orphans(dir.path(), &|_| true, &|_| Some(2_000)).is_empty());

        let mut recorded = record("live-1", 4243, 20, dir.path());
        recorded.pid_start = Some(1_000);
        recorded.write().unwrap();
        assert!(orphans(dir.path(), &|_| true, &unreadable_start).is_empty());
    }

    /// A record written by a build older than `pid_start` still parses, and
    /// reads as the host that cannot tell — which is what it is.
    #[test]
    fn a_record_from_before_the_field_existed_still_parses() {
        let older = serde_json::json!({
            "handle": "old-0",
            "checkout": "/w/.stella/private/candidates/old-0",
            "pid": 4242,
            "started_at_ms": 10_u64,
        });
        let parsed: CandidateRecord = serde_json::from_value(older).unwrap();
        assert_eq!(parsed.pid_start, None);
    }

    /// **#4511's witness, half two.** The patch outlives the record that named
    /// its checkout, so the sweep has to find it in the directory or it is
    /// named exactly once, on a stderr line the user may have missed.
    #[test]
    fn a_preserved_patch_is_named_by_every_sweep_until_it_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let kept = record("scored-nothing-0", 4242, 10, dir.path());
        kept.write().unwrap();
        let patch = dir.path().join("scored-nothing-0.patch");
        std::fs::write(&patch, b"diff --git a/x b/x\n").unwrap();

        // Removal takes the checkout and the record with it (#2651's ending),
        // and the patch is what is left.
        forget(&kept.checkout);
        let named = report(dir.path());
        assert_eq!(named.len(), 1, "{named:?}");
        assert!(named[0].contains("scored-nothing-0.patch"), "{}", named[0]);
        assert!(
            named[0].contains("git apply --binary"),
            "a path with no way to use it is half a report: {}",
            named[0]
        );
        assert!(
            named[0].contains("delete it"),
            "a line that repeats forever must say how to stop it: {}",
            named[0]
        );

        // A second run finds it again — that is the whole fix.
        assert_eq!(report(dir.path()).len(), 1);

        std::fs::remove_file(&patch).unwrap();
        assert!(report(dir.path()).is_empty(), "and stops once it is gone");
    }

    /// Patches are named in a stable order: two runs that found the same
    /// residue must say the same thing, and a directory's own order is not an
    /// order.
    #[test]
    fn preserved_patches_are_reported_in_a_stable_order() {
        let dir = tempfile::tempdir().unwrap();
        for slug in ["p-2-cccc", "p-0-aaaa", "p-1-bbbb"] {
            std::fs::write(dir.path().join(format!("{slug}.patch")), b"diff\n").unwrap();
        }
        // A record's own file must not be mistaken for a patch.
        record("live-0", 4242, 10, dir.path()).write().unwrap();

        let found = preserved_patches(dir.path());
        let names: Vec<String> = found
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["p-0-aaaa.patch", "p-1-bbbb.patch", "p-2-cccc.patch"]
        );
    }
}
