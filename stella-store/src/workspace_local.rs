//! Machine-local workspace identity — the key that survives a `mv`.
//!
//! Stella already had two identities and neither answers "is this the same
//! working copy I saw last time?":
//!
//! - [`crate::identity::workspace_id`] is the *cloud* identity. It is minted
//!   only by explicit registration and lives at `<root>/.stella/workspace.json`
//!   *outside* `private/`, deliberately, so committing it gives one workspace
//!   identity across clones and machines. Keying local state on it would make
//!   every clone of a repository share one store.
//! - `project_id` ([`crate::usage::project_id_for`]) is a hash of the
//!   canonical path. Always present, machine-local — and it changes the moment
//!   the directory is renamed or moved, which is exactly the failure this
//!   module exists to remove.
//!
//! So this is a third identity, and its properties are the complement of both:
//! machine-local, minted implicitly on first sight, durable across a move, and
//! **not** shared with a clone. It lives at
//! `<root>/.stella/private/local-id.json`, and `private/` is gitignored by
//! construction (`WORKSPACE_GENERATED_IGNORE`), so a `git clone` never carries
//! it — a fresh clone correctly gets a fresh identity, because local sessions
//! are local.
//!
//! # Move versus copy
//!
//! The marker alone cannot tell a moved workspace from a copied one: both show
//! up as "my id, at a path that is not where I last was". The record of where
//! it last lived is what separates them.
//!
//! - The old path is gone, or no longer carries this id → the workspace
//!   **moved**. Adopt the new path and keep the identity.
//! - The old path still exists and still carries the *same* id → this is a
//!   **copy**. Mint a fresh identity for it, so a duplicated project cannot
//!   silently write into the original's history.
//!
//! Guessing wrong in the copy direction is the expensive mistake — it merges
//! two projects' state — so the ambiguous case resolves to "copy".
//!
//! # What is deliberately not solved
//!
//! Delete `.stella/private/` *and* move the directory and nothing links the
//! two ends; no scheme can recover that, because nothing survived that both
//! sides share. The data is not lost (session listings are not scoped by
//! workspace), it is only no longer auto-discovered. A heuristic that guessed
//! here would eventually bind two unrelated projects together, which is worse
//! than an honest miss.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Result;

/// The marker filename inside `<root>/.stella/private/`.
pub const LOCAL_ID_FILE: &str = "local-id.json";

/// How [`resolve`] arrived at the identity it returned. Callers that own
/// path-keyed state use this to migrate it; everyone else can ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The marker was already there and the workspace has not moved.
    Existing,
    /// No marker: minted one. `path_key` names the pre-move key any legacy
    /// path-keyed state would be filed under, so a caller can adopt it.
    Minted,
    /// The marker was there but the workspace has moved since. The identity is
    /// unchanged; the recorded path was updated.
    Moved,
    /// A copy of a workspace that still exists elsewhere. A fresh identity was
    /// minted so the two do not share state.
    Copied,
}

/// A workspace's machine-local identity, and how it was reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalWorkspace {
    /// The durable id. Stable across renames and moves.
    pub id: String,
    /// The canonical path this identity is currently bound to.
    pub path: PathBuf,
    /// The path-hash key this workspace would have had before this module
    /// existed — the lookup key for legacy path-keyed state.
    pub path_key: String,
    /// How the identity was resolved.
    pub origin: Origin,
}

/// The on-disk marker. A struct rather than an ad-hoc `Value` so the field
/// order is the wire order and an unknown shape fails to parse instead of
/// half-parsing into a wrong identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Marker {
    id: String,
    /// Where this marker last resolved. The move/copy discriminator.
    path: String,
}

fn marker_path(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join(".stella")
        .join(crate::private::WORKSPACE_PRIVATE_DIR)
        .join(LOCAL_ID_FILE)
}

fn canonical(workspace_root: &Path) -> PathBuf {
    workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf())
}

fn read_marker(workspace_root: &Path) -> Option<Marker> {
    let text = crate::read_private_to_string(&marker_path(workspace_root)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_marker(workspace_root: &Path, marker: &Marker) -> Result<()> {
    let dir = workspace_root
        .join(".stella")
        .join(crate::private::WORKSPACE_PRIVATE_DIR);
    crate::ensure_private_dir(&dir)?;
    let body = serde_json::to_string(marker)
        .map_err(|e| crate::StoreError(format!("cannot serialize {LOCAL_ID_FILE}: {e}")))?;
    crate::private::write_private_atomic(&dir.join(LOCAL_ID_FILE), body.as_bytes())
}

/// Resolve (and, when absent, mint) this workspace's machine-local identity.
///
/// Infallible in spirit — the only errors are "cannot write into your own
/// `.stella/private/`", which every other durable path reports too. A caller
/// that cannot tolerate failure can fall back to [`crate::usage::project_id_for`]
/// and behave exactly as it did before this existed.
pub fn resolve(workspace_root: &Path) -> Result<LocalWorkspace> {
    let here = canonical(workspace_root);
    let path_key = crate::usage::project_id_for(&here);

    let Some(marker) = read_marker(workspace_root) else {
        let minted = Marker {
            id: crate::enterprise_telemetry::random_uuid_v4(),
            path: here.to_string_lossy().into_owned(),
        };
        write_marker(workspace_root, &minted)?;
        return Ok(LocalWorkspace {
            id: minted.id,
            path: here,
            path_key,
            origin: Origin::Minted,
        });
    };

    if marker.path == here.to_string_lossy() {
        return Ok(LocalWorkspace {
            id: marker.id,
            path: here,
            path_key,
            origin: Origin::Existing,
        });
    }

    // The recorded path differs. If the workspace it names still exists AND
    // still claims this same id, both are live and this one is a copy.
    let previous = PathBuf::from(&marker.path);
    let previous_still_claims_id = read_marker(&previous).is_some_and(|m| m.id == marker.id);

    if previous_still_claims_id {
        let minted = Marker {
            id: crate::enterprise_telemetry::random_uuid_v4(),
            path: here.to_string_lossy().into_owned(),
        };
        write_marker(workspace_root, &minted)?;
        return Ok(LocalWorkspace {
            id: minted.id,
            path: here,
            path_key,
            origin: Origin::Copied,
        });
    }

    let moved = Marker {
        id: marker.id,
        path: here.to_string_lossy().into_owned(),
    };
    write_marker(workspace_root, &moved)?;
    Ok(LocalWorkspace {
        id: moved.id,
        path: here,
        path_key,
        origin: Origin::Moved,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(name: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        (dir, root)
    }

    #[test]
    fn first_sight_mints_and_is_stable_afterwards() {
        let (_guard, root) = ws("proj");
        let first = resolve(&root).unwrap();
        assert_eq!(first.origin, Origin::Minted);
        assert!(!first.id.is_empty());

        let again = resolve(&root).unwrap();
        assert_eq!(again.origin, Origin::Existing);
        assert_eq!(again.id, first.id, "identity must not fork on re-resolve");
    }

    #[test]
    fn a_moved_workspace_keeps_its_identity() {
        // The whole point: `stella resume` keys on this, and today it keys on
        // the path string, so renaming a project orphans every session.
        let guard = tempfile::tempdir().unwrap();
        let before = guard.path().join("old-name");
        std::fs::create_dir_all(&before).unwrap();
        let original = resolve(&before).unwrap();

        let after = guard.path().join("new-name");
        std::fs::rename(&before, &after).unwrap();

        let moved = resolve(&after).unwrap();
        assert_eq!(moved.origin, Origin::Moved);
        assert_eq!(
            moved.id, original.id,
            "a rename is not a new workspace — the identity survives it"
        );
        assert_ne!(
            moved.path_key, original.path_key,
            "the path hash DID change, which is exactly why it cannot be the key"
        );
    }

    #[test]
    fn a_copy_gets_its_own_identity_so_histories_never_merge() {
        // Both copies are live. Sharing an id would let one project's agents
        // write into the other's durable history — the expensive mistake, so
        // the ambiguous case must resolve this way.
        let guard = tempfile::tempdir().unwrap();
        let original_root = guard.path().join("original");
        std::fs::create_dir_all(&original_root).unwrap();
        let original = resolve(&original_root).unwrap();

        let copy_root = guard.path().join("copy");
        std::fs::create_dir_all(copy_root.join(".stella").join("private")).unwrap();
        std::fs::copy(marker_path(&original_root), marker_path(&copy_root)).unwrap();

        let copied = resolve(&copy_root).unwrap();
        assert_eq!(copied.origin, Origin::Copied);
        assert_ne!(
            copied.id, original.id,
            "a copy must not inherit the original's identity"
        );
        assert_eq!(
            resolve(&original_root).unwrap().id,
            original.id,
            "and the original is undisturbed by having been copied"
        );
    }

    #[test]
    fn the_marker_lives_where_git_will_not_carry_it() {
        // `private/` is in WORKSPACE_GENERATED_IGNORE, so a clone gets a fresh
        // identity rather than sharing the origin's. If this path ever moves
        // out of `private/`, clones start fighting over one store.
        let (_guard, root) = ws("proj");
        resolve(&root).unwrap();
        let path = marker_path(&root);
        assert!(path.exists());
        assert!(
            path.components()
                .any(|c| c.as_os_str() == crate::private::WORKSPACE_PRIVATE_DIR),
            "the local id must live under the gitignored private dir: {path:?}"
        );
    }
}
