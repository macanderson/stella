// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Counting the store's extension-authored rules without opening the store.
//!
//! [`crate::Store::open`] is not a read: it creates and hardens
//! `.stella/private/`, runs every migration, and writes through
//! `reconcile_interrupted_executions`. That is right for a session and wrong
//! for the settings loader, which runs on every `stella` invocation including
//! `stella --version` — so the trust-gate survey could not ask how many rules
//! a workspace was withholding and stayed silent about them instead (#3617).
//!
//! This answers that one question and nothing else:
//!
//! - **No creation.** The path is resolved by looking, never by
//!   `private::ensure_workspace_state_dir`, which would make a
//!   directory appear as a side effect of a count. Deliberately *not*
//!   [`crate::existing_workspace_private_sqlite_path`], which is the right
//!   guard for a caller that is about to open the store and the wrong one
//!   here for exactly that reason.
//! - **No migration**, of the schema or of the legacy layout. A store still
//!   sitting at the pre-`private/` path is read where it lies; moving it is
//!   the session's job, on a path that can report a failure.
//! - **Immutable at the SQLite level** (`open_private_sqlite_read_only`),
//!   so a count cannot create a journal, checkpoint a WAL, or leave a lock
//!   behind — and it inherits that path's owner-and-regular-file validation.
//!   The documented cost is that an uncheckpointed `-wal`'s pages are
//!   invisible, so a rule published moments ago by a still-running session may
//!   not be counted yet. For a notice that says how much steering was withheld,
//!   undercounting by one is the harmless direction.
//!
//! Best-effort in the same way [`crate::Store::list_rules`]'s caller is: every
//! failure resolves to zero, because the only consumer is a notice that names
//! how much steering was withheld, and a settings load must not fail over a
//! database it was only counting. Zero is the safe direction — it undercounts
//! a notice, where an error would break the load.

use std::path::{Path, PathBuf};

use crate::private::WORKSPACE_PRIVATE_DIR;

/// The workspace store, if one already exists — the current path first, then
/// the pre-`private/` layout, and `None` when neither is there.
///
/// Pure enough to be the whole guard: it stats two paths and creates nothing.
fn existing_store_db(workspace_root: &Path) -> Option<PathBuf> {
    let dot = workspace_root.join(".stella");
    let current = dot.join(WORKSPACE_PRIVATE_DIR).join("store.db");
    if current.is_file() {
        return Some(current);
    }
    let legacy = dot.join("store.db");
    legacy.is_file().then_some(legacy)
}

/// How many extension-authored rules `<root>/.stella/private/store.db` holds.
///
/// Zero for a workspace that has never run Stella, whose store predates the
/// `rules` table, or whose store cannot be read — see the module doc for why
/// every failure resolves that way rather than to an error.
#[must_use]
pub fn published_rule_count(workspace_root: &Path) -> usize {
    let Some(path) = existing_store_db(workspace_root) else {
        return 0;
    };
    let Ok(conn) = crate::open_private_sqlite_read_only(&path) else {
        return 0;
    };
    conn.query_row("SELECT count(*) FROM rules", [], |row| row.get::<_, i64>(0))
        .ok()
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workspace_that_never_ran_stella_counts_zero_and_creates_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("repo");
        std::fs::create_dir_all(root.join(".stella")).expect("workspace");

        assert_eq!(published_rule_count(&root), 0);
        assert!(
            !root.join(".stella").join(WORKSPACE_PRIVATE_DIR).exists(),
            "counting must not create the private state directory"
        );
        // A workspace with no `.stella` at all is the same answer.
        let bare = dir.path().join("bare");
        std::fs::create_dir_all(&bare).expect("bare");
        assert_eq!(published_rule_count(&bare), 0);
        assert!(!bare.join(".stella").exists());
    }

    #[test]
    fn published_rules_are_counted_without_opening_the_store_for_writing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let store = crate::Store::open(&root).expect("open");
        store
            .upsert_rule("house-style", "# house style", "ext")
            .unwrap();
        store.upsert_rule("review", "# review", "ext").unwrap();
        drop(store);

        assert_eq!(published_rule_count(&root), 2);
    }

    /// A store whose schema predates the `rules` table answers zero rather
    /// than failing the settings load that asked.
    #[test]
    fn a_store_without_the_rules_table_counts_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let private = root.join(".stella").join(WORKSPACE_PRIVATE_DIR);
        std::fs::create_dir_all(&private).expect("private dir");
        rusqlite::Connection::open(private.join("store.db")).expect("create");

        assert_eq!(published_rule_count(&root), 0);
    }
}
