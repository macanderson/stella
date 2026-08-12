//! The half of a candidate's work a git patch cannot carry: permission bits.
//!
//! Adoption delivers by `git apply` of a `git diff --binary --no-renames`
//! patch, and that is the right mechanism — it is atomic, it verifies every
//! preimage before writing a byte, and it gets deletions and binary files
//! right (#2927 cleared it of both suspicions). What it is *not* is a
//! description of a tree. Git records exactly two file modes, `100644` and
//! `100755`, and no directory modes at all. So a candidate that does the job
//! correctly — `chmod 600` on a generated private key, `chmod 700` on the
//! directory holding it — has that half of its work silently dropped between
//! the worktree where it was verified and the tree that is graded (#2935,
//! observed on `openssl-selfsigned-cert`).
//!
//! # Why a replay rather than a manifest
//!
//! The obvious fix — record every path's `st_mode` at seal time and apply the
//! recording after `git apply` — creates a second description of the tree that
//! can disagree with the tree, and the disagreement would be undetectable: a
//! stale manifest `chmod`s a delivered file to a mode the candidate no longer
//! holds, and nothing in the run can tell that from the truth.
//!
//! So nothing is recorded. The modes are read from the candidate worktree at
//! the moment of delivery and applied immediately, in the same call, from the
//! same tree the patch was built against — which the drift check
//! (`sealed_unchanged_inner`) has just certified byte-identical to the sealed
//! commit. There is one description of the tree, and it is the tree.
//!
//! # What is replayed, and what deliberately is not
//!
//! Only modes git **cannot** express: [`is_git_representable`] passes `0644`
//! and `0755` straight through to the patch, which already carries the
//! executable bit correctly. The asymmetry is deliberate and it is the whole
//! safety argument. A tracked file enters the candidate through
//! `git worktree add`, so its mode there is git's normalization (`0644`) and
//! not the mode the user's own file carries. Forcing the candidate's mode onto
//! every delivered path would therefore *destroy* a `0600` the user set on a
//! file the candidate merely edited the contents of. Adding what git cannot
//! say can only ever be information the patch lost; removing what git cannot
//! say would be information the patch never had. The residue — a candidate
//! that deliberately relaxes a `0600` back to `0644` — is unrepresentable in
//! either direction and is tracked in #2988.
//!
//! Directory modes are replayed only for directories **adoption itself
//! created**, computed before the patch is applied. A directory the real tree
//! already had is the user's, not the candidate's, and its mode is not the
//! candidate's to change.

use std::path::Path;

use stella_pipeline::ports::AdoptedChange;
use stella_protocol::FileChangeKind;

/// The permission bits a git patch can express: `100644` and `100755`.
///
/// Anything else — `0600`, `0640`, `0700`, a setgid bit — is lost between the
/// candidate worktree and the graded tree unless it is replayed.
pub(crate) fn is_git_representable(mode: u32) -> bool {
    matches!(mode & 0o7777, 0o644 | 0o755)
}

/// Repository-relative directories that applying this patch will have to
/// create, deepest first — the ancestors of every delivered path that the real
/// tree does not have yet.
///
/// Must be called **before** `git apply`: afterwards every one of them exists
/// and the question can no longer be asked. Deepest first so a caller
/// `chmod`ping them in order never has to write through a directory it has
/// already tightened.
pub(crate) fn directories_adoption_will_create(
    real_toplevel: &Path,
    changes: &[AdoptedChange],
) -> Vec<String> {
    let mut absent: Vec<String> = Vec::new();
    for change in changes {
        if change.kind == FileChangeKind::Deleted {
            continue;
        }
        let mut prefix = String::new();
        let mut components = change.path.split('/').peekable();
        while let Some(component) = components.next() {
            // The last component is the file itself, never a directory.
            if components.peek().is_none() {
                break;
            }
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            if real_toplevel.join(&prefix).exists() || absent.contains(&prefix) {
                continue;
            }
            absent.push(prefix.clone());
        }
    }
    // Deepest first: `a/b/c` before `a/b` before `a`.
    absent.sort_by_key(|path| std::cmp::Reverse(path.matches('/').count()));
    absent
}

/// Give every delivered path the candidate's own permission bits, for the
/// modes a patch could not carry. Returns the repository-relative paths whose
/// mode could not be read or set, in sorted order.
///
/// Best-effort by construction, and that is a decision rather than an
/// omission: `git apply` has already written the bytes atomically and they
/// cannot be un-written, so a failure here is a partially-faithful delivery,
/// never an absent one. Returning `Err` would make the caller report an
/// adoption that did not happen — the one thing that is certainly false.
/// Surfacing this residue on the adoption event is #2990.
///
/// `created_dirs` comes from [`directories_adoption_will_create`], evaluated
/// before the apply.
#[cfg(unix)]
pub(crate) fn replay_unrepresentable_modes(
    candidate_toplevel: &Path,
    real_toplevel: &Path,
    changes: &[AdoptedChange],
    created_dirs: &[String],
) -> Vec<String> {
    use std::os::unix::fs::PermissionsExt;

    let mut unfaithful: Vec<String> = Vec::new();
    let mut replay = |rel: &str, representable_only: bool| {
        let source = candidate_toplevel.join(rel);
        // `symlink_metadata`: a symlink's own mode is meaningless (`0777` on
        // every Unix that has them) and git records it as `120000` — following
        // the link would read the target's mode and stamp it on the link.
        let Ok(meta) = std::fs::symlink_metadata(&source) else {
            unfaithful.push(rel.to_string());
            return;
        };
        if meta.file_type().is_symlink() {
            return;
        }
        let mode = meta.permissions().mode() & 0o7777;
        if representable_only && is_git_representable(mode) {
            return;
        }
        let target = real_toplevel.join(rel);
        if !target.exists() {
            return;
        }
        if std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode)).is_err() {
            unfaithful.push(rel.to_string());
        }
    };

    for change in changes {
        if change.kind == FileChangeKind::Deleted {
            continue;
        }
        replay(&change.path, true);
    }
    // Files before directories, and `created_dirs` deepest-first: a directory
    // the candidate tightened to `0500` must not be tightened before the files
    // inside it have been written and chmod'ed.
    for dir in created_dirs {
        replay(dir, false);
    }
    unfaithful.sort();
    unfaithful.dedup();
    unfaithful
}

/// Windows has no Unix permission bits, so there is nothing a patch failed to
/// carry and nothing to replay.
#[cfg(not(unix))]
pub(crate) fn replay_unrepresentable_modes(
    _candidate_toplevel: &Path,
    _real_toplevel: &Path,
    _changes: &[AdoptedChange],
    _created_dirs: &[String],
) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(path: &str, kind: FileChangeKind) -> AdoptedChange {
        AdoptedChange {
            path: path.to_string(),
            kind,
            added: 0,
            removed: 0,
            diff: None,
        }
    }

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
        let root = std::env::temp_dir().join(format!(
            "stella_modes_dirs_{}_{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(root.join("existing")).unwrap();
        let changes = vec![
            change("existing/a.txt", FileChangeKind::Created),
            change("ssl/deep/server.key", FileChangeKind::Created),
            change("ssl/server.crt", FileChangeKind::Created),
            change("top.txt", FileChangeKind::Created),
        ];
        assert_eq!(
            directories_adoption_will_create(&root, &changes),
            vec!["ssl/deep".to_string(), "ssl".to_string()],
            "an existing directory is the user's; a file at the root has no ancestor"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_deletion_creates_no_directories() {
        let root = std::env::temp_dir().join(format!(
            "stella_modes_del_{}_{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let changes = vec![change("gone/x.txt", FileChangeKind::Deleted)];
        assert!(directories_adoption_will_create(&root, &changes).is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    /// The safety half of the asymmetry, at the unit level: a candidate whose
    /// file carries a mode git *can* express is left to the patch, so a `0600`
    /// the user set on a file the candidate only edited is never clobbered.
    #[cfg(unix)]
    #[test]
    fn a_representable_mode_is_left_to_the_patch() {
        use std::os::unix::fs::PermissionsExt;

        let base = std::env::temp_dir().join(format!(
            "stella_modes_keep_{}_{}",
            std::process::id(),
            line!()
        ));
        let (candidate, real) = (base.join("candidate"), base.join("real"));
        std::fs::create_dir_all(&candidate).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(candidate.join("edited.txt"), "new\n").unwrap();
        std::fs::set_permissions(
            candidate.join("edited.txt"),
            PermissionsExt::from_mode(0o644),
        )
        .unwrap();
        std::fs::write(real.join("edited.txt"), "new\n").unwrap();
        std::fs::set_permissions(real.join("edited.txt"), PermissionsExt::from_mode(0o600))
            .unwrap();

        let changes = vec![change("edited.txt", FileChangeKind::Modified)];
        assert!(replay_unrepresentable_modes(&candidate, &real, &changes, &[]).is_empty());
        assert_eq!(
            std::fs::metadata(real.join("edited.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o600,
            "the user's own tightening survives an ordinary content edit"
        );
        std::fs::remove_dir_all(&base).ok();
    }
}
