// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The half of a candidate's work a git patch cannot carry: permission bits.
//! (#4390, successor to #2935 and #2988.)
//!
//! Adoption delivers by `git apply` of a `git diff --binary` patch, and that
//! is the right mechanism — atomic, every preimage verified before a byte is
//! written, deletions and binary files correct. What it is *not* is a
//! description of a tree. Git records exactly two file modes, `100644` and
//! `100755`, and no directory modes at all. So a candidate that does the job
//! correctly — `chmod 600` on a generated private key, `chmod 700` on the
//! directory holding it — has that half of its work dropped between the
//! worktree where it was verified and the tree that is adopted.
//!
//! # A faithful checkout is what makes the answer symmetric
//!
//! `git worktree add` checks a tracked file out at git's own normalization, so
//! a candidate cut from a tree whose file is `0600` sees `0644` — it cannot
//! observe the mode the user set, and therefore cannot deliberately relax it
//! either. That asymmetry is why the pre-#3865 substrate could tighten and not
//! relax, and why relaxing was still open as #2988.
//!
//! So the mode is put *into* the candidate at creation
//! ([`stamp_unrepresentable`]) and read back *out* at adoption
//! ([`replay_changed`]). The candidate works in a tree that tells it the truth,
//! and delivery asks one question about each path: **is this mode different
//! from the one the candidate was given?** Both directions answer it.
//!
//! # What is recorded, and why it cannot go stale harmfully
//!
//! Only the modes git cannot express ([`is_git_representable`]) are stamped
//! and recorded — in practice a handful of paths, often none. Everything else
//! is `0644`/`0755`, which the patch already carries, and whose baseline is
//! therefore computable without recording anything.
//!
//! The record is a statement about a moment that has passed — what this
//! candidate was handed — not a second description of what the tree *should*
//! be. That is what keeps the user's own tree safe in the two cases that
//! matter: a file the candidate merely edited still reads back at its recorded
//! mode, so nothing is written; and a mode the **user** changed mid-fan-out is
//! not reverted, because the candidate's mode is compared against what the
//! candidate was given rather than against what the real tree holds now.
//!
//! # Best-effort, and deliberately so
//!
//! `git apply` has already written the bytes atomically and they cannot be
//! un-written, so a `chmod` that fails is a partially faithful delivery, never
//! an absent one. Reporting `Err` from here would tell the plugin an adoption
//! did not happen when it did — the one statement that is certainly false.

use std::collections::BTreeMap;
use std::path::Path;

/// Every path whose mode a git patch could not carry, as it stood when a
/// candidate was created.
pub(super) type ModeRecord = BTreeMap<String, u32>;

/// The permission bits a git patch can express: `100644` and `100755`.
///
/// Anything else — `0600`, `0640`, `0700`, a setgid bit — is lost between the
/// candidate worktree and the adopted tree unless it is replayed.
pub(super) fn is_git_representable(mode: u32) -> bool {
    matches!(mode & 0o7777, 0o644 | 0o755)
}

/// Give the fresh checkout at `candidate_top` the modes of the tree it was cut
/// from, for the paths git's own checkout could not reproduce.
///
/// `tracked` is `git ls-files -z` of the real tree — the paths a
/// `git worktree add` materialized. Returns what was stamped, which is the
/// baseline [`replay_changed`] compares against.
#[cfg(unix)]
pub(super) fn stamp_unrepresentable(
    real_top: &Path,
    candidate_top: &Path,
    tracked: &[String],
) -> ModeRecord {
    use std::os::unix::fs::PermissionsExt;

    let mut record = ModeRecord::new();
    for rel in tracked {
        // `symlink_metadata`: a symlink's own mode is meaningless (`0777` on
        // every Unix that has them) and git records it as `120000`, so
        // following the link would read the target's mode and stamp that.
        let Ok(meta) = std::fs::symlink_metadata(real_top.join(rel)) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        let mode = meta.permissions().mode() & 0o7777;
        if is_git_representable(mode) {
            continue;
        }
        let target = candidate_top.join(rel);
        if !target.exists() {
            continue;
        }
        if std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode)).is_ok() {
            record.insert(rel.clone(), mode);
        }
    }
    record
}

/// Give the adopted tree at `real_top` every mode this candidate changed.
///
/// `tracked` is `git ls-files -z` of the **candidate** after its work is
/// staged, so a file the candidate created is included; `record` is what
/// [`stamp_unrepresentable`] handed it. Returns the repository-relative paths
/// whose mode could not be read or written, sorted — a residue to report, not
/// an error to raise (see this module's header).
///
/// `created_dirs` comes from [`directories_adoption_will_create`], evaluated
/// **before** the patch was applied.
#[cfg(unix)]
pub(super) fn replay_changed(
    candidate_top: &Path,
    real_top: &Path,
    tracked: &[String],
    record: &ModeRecord,
    created_dirs: &[String],
) -> Vec<String> {
    use std::os::unix::fs::PermissionsExt;

    let mut unfaithful: Vec<String> = Vec::new();
    let mut deliver = |rel: &str, baseline: Option<u32>| {
        let Ok(meta) = std::fs::symlink_metadata(candidate_top.join(rel)) else {
            // Absent in the candidate: a path it deleted, which has no mode
            // to deliver and whose removal the patch carried.
            return;
        };
        if meta.file_type().is_symlink() {
            return;
        }
        let mode = meta.permissions().mode() & 0o7777;
        let unchanged = match baseline {
            // A path the candidate was handed at a mode git could not carry:
            // it is the candidate's decision only if it differs now.
            Some(given) => mode == given,
            // A path git checked out at its own normalization. The patch
            // already carries `0644`/`0755`, so only a mode git cannot say is
            // this candidate's to deliver.
            None => is_git_representable(mode),
        };
        if unchanged {
            return;
        }
        let target = real_top.join(rel);
        if !target.exists() {
            return;
        }
        if std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode)).is_err() {
            unfaithful.push(rel.to_string());
        }
    };

    for rel in tracked {
        deliver(rel, record.get(rel).copied());
    }
    // Files before directories, and `created_dirs` deepest-first: a directory
    // the candidate tightened to `0500` must not be tightened before the files
    // inside it have been written and chmod'ed.
    for dir in created_dirs {
        deliver(dir, None);
    }
    unfaithful.sort();
    unfaithful.dedup();
    unfaithful
}

/// Windows has no Unix permission bits, so there is nothing a patch failed to
/// carry and nothing to stamp or replay.
#[cfg(not(unix))]
pub(super) fn stamp_unrepresentable(
    _real_top: &Path,
    _candidate_top: &Path,
    _tracked: &[String],
) -> ModeRecord {
    ModeRecord::new()
}

/// See [`stamp_unrepresentable`]'s non-Unix half.
#[cfg(not(unix))]
pub(super) fn replay_changed(
    _candidate_top: &Path,
    _real_top: &Path,
    _tracked: &[String],
    _record: &ModeRecord,
    _created_dirs: &[String],
) -> Vec<String> {
    Vec::new()
}

/// Repository-relative directories that applying this patch will have to
/// create, deepest first — the ancestors of every delivered path the real tree
/// does not have yet.
///
/// Must be called **before** `git apply`: afterwards every one of them exists
/// and the question can no longer be asked. Deepest first so a caller
/// `chmod`ping them in order never has to write through a directory it has
/// already tightened.
pub(super) fn directories_adoption_will_create(real_top: &Path, paths: &[String]) -> Vec<String> {
    let mut absent: Vec<String> = Vec::new();
    for path in paths {
        let mut prefix = String::new();
        let mut components = path.split('/').peekable();
        while let Some(component) = components.next() {
            // The last component is the file itself, never a directory.
            if components.peek().is_none() {
                break;
            }
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            if real_top.join(&prefix).exists() || absent.contains(&prefix) {
                continue;
            }
            absent.push(prefix.clone());
        }
    }
    absent.sort_by_key(|path| std::cmp::Reverse(path.matches('/').count()));
    absent
}

/// Split a `-z` path list — `git ls-files -z`, and every other NUL-terminated
/// git output this substrate reads.
pub(super) fn nul_separated(output: &str) -> Vec<String> {
    output
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_two_modes_a_patch_encodes_are_representable() {
        assert!(is_git_representable(0o644));
        assert!(is_git_representable(0o755));
        assert!(!is_git_representable(0o600), "the openssl key mode (#2935)");
        assert!(!is_git_representable(0o700));
        assert!(!is_git_representable(0o640));
        assert!(
            !is_git_representable(0o2755),
            "a setgid bit is not the executable bit"
        );
    }

    #[test]
    fn absent_ancestors_are_reported_deepest_first_and_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("existing")).unwrap();
        let paths = [
            "existing/a.txt".to_string(),
            "ssl/deep/server.key".to_string(),
            "ssl/server.crt".to_string(),
            "top.txt".to_string(),
        ];
        assert_eq!(
            directories_adoption_will_create(dir.path(), &paths),
            vec!["ssl/deep".to_string(), "ssl".to_string()],
            "an existing directory is the user's; a file at the root has no ancestor"
        );
    }

    #[test]
    fn a_nul_list_drops_the_trailing_empty_field() {
        assert_eq!(
            nul_separated("a.txt\0dir/b.txt\0"),
            vec!["a.txt".to_string(), "dir/b.txt".to_string()]
        );
        assert!(nul_separated("").is_empty());
    }

    /// The safety half, at the unit level: a `0600` the **user** set on a file
    /// the candidate only edited is never clobbered, because the candidate was
    /// handed that same `0600` and handed it back unchanged.
    #[cfg(unix)]
    #[test]
    fn a_mode_the_candidate_did_not_change_is_left_alone() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let (candidate, real) = (dir.path().join("candidate"), dir.path().join("real"));
        std::fs::create_dir_all(&candidate).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("key.pem"), "old\n").unwrap();
        std::fs::set_permissions(real.join("key.pem"), PermissionsExt::from_mode(0o600)).unwrap();
        // What `git worktree add` produces, before the stamp.
        std::fs::write(candidate.join("key.pem"), "old\n").unwrap();
        std::fs::set_permissions(candidate.join("key.pem"), PermissionsExt::from_mode(0o644))
            .unwrap();

        let tracked = vec!["key.pem".to_string()];
        let record = stamp_unrepresentable(&real, &candidate, &tracked);
        assert_eq!(record.get("key.pem"), Some(&0o600), "stamped: {record:?}");

        // The candidate edits the bytes and leaves the mode alone.
        std::fs::write(candidate.join("key.pem"), "new\n").unwrap();
        assert!(replay_changed(&candidate, &real, &tracked, &record, &[]).is_empty());
        assert_eq!(
            std::fs::metadata(real.join("key.pem"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o600,
            "the user's own tightening survives an ordinary content edit"
        );
    }
}
