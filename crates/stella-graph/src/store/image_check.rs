// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Which store images this process has already walked with `PRAGMA
//! quick_check`, so an unchanged image is walked once instead of once per
//! open.
//!
//! # Why the memo exists
//!
//! The check itself is not optional and its argument is in
//! [`super::open_verified`]: the 2026-08-08 field corruption sailed through
//! migration and left the graph dead for four days, and `quick_check` catches
//! that shape precisely *because* it walks every page. What was wrong was the
//! assumption under which it was priced — "the writer's open pays it once,
//! ahead of a tree walk that dwarfs it". The read path does not open once.
//! `search::engine::report_with` calls `open_or_build` on **every `search`
//! call**, so a session that searched sixty-one times walked every page of a
//! 180 MB `codegraph.db` sixty-one times, against a hash-diff catch-up
//! measured at 78-97 ms warm. #4385 is what that cost looked like from the
//! outside.
//!
//! # Why the key is the image's own stamp, not the path
//!
//! A path-keyed memo would answer "this process verified this file once" and
//! that is the wrong question, because a store can be damaged **after** it was
//! verified — by a disk fault, or by another writer — and the next open is
//! exactly where that must be caught. `store::tests::
//! a_store_corrupt_only_in_its_data_pages_is_quarantined_at_open` scrambles a
//! page between two opens in one process and is the test that says so.
//!
//! So the memo records `(length, modification time)` of the main database file
//! and re-walks whenever either moves. That is both correct and effective
//! here, because of what WAL mode does: ordinary writes land in the `-wal`
//! sidecar and leave the main image untouched, and the main image is what
//! `quick_check` walks. A burst of searches over a settled index therefore
//! hits the memo every time, while a checkpoint — which does rewrite the main
//! file — costs one more walk, which is the conservative direction.
//!
//! **What it still gives up:** damage written inside the same `(length,
//! mtime)` stamp as the verification is invisible to the memo until the stamp
//! moves. A filesystem whose timestamps are coarse widens that window. The
//! first open in a process always walks, which is the case #4370 was about,
//! and a store damaged mid-session still surfaces through the catch-up scan's
//! error reaching the caller as an index warning
//! (`search::codegraph::open_or_build`) — the path that was silent in
//! 2026-08-08 and is not any more.
//!
//! The path is used as the caller spelled it, not canonicalized: a second
//! spelling of one file walks it again, which is the conservative direction,
//! and it keeps this module free of its own I/O beyond the stat.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

/// What an image looked like when it was walked and found intact. Equal
/// stamps mean the bytes `quick_check` read are still the bytes on disk, as
/// far as the filesystem is willing to say.
type Stamp = (u64, Option<SystemTime>);

/// The images this process has walked, and the stamp each carried at the time.
static VERIFIED: Mutex<BTreeMap<PathBuf, Stamp>> = Mutex::new(BTreeMap::new());

/// How many full-image walks this process has performed. See [`image_walks`].
static WALKS: AtomicU64 = AtomicU64::new(0);

/// How many times this process has walked a store image with `PRAGMA
/// quick_check`.
///
/// Always compiled rather than `#[cfg(test)]`, for the reason
/// `search::cache::GatherCache::gathered` is: the count is the **only**
/// observable difference between a memo that works and a memo that silently
/// never hits, and a counter that exists only under `cfg(test)` cannot be
/// asserted from an integration test in another crate. It is monotonic within
/// a process and carries no meaning across processes.
#[must_use]
pub fn image_walks() -> u64 {
    WALKS.load(Ordering::Relaxed)
}

/// The image's current stamp. `None` when it cannot be read at all, which is
/// never treated as a match: an image whose stamp is unknown is walked.
fn stamp(db_path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(db_path).ok()?;
    Some((meta.len(), meta.modified().ok()))
}

/// Whether `db_path` was walked, found intact, and has not changed since.
pub(crate) fn already_walked(db_path: &Path) -> bool {
    let Some(current) = stamp(db_path) else {
        return false;
    };
    VERIFIED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(db_path)
        .is_some_and(|recorded| *recorded == current)
}

/// Record that `db_path`'s image is intact as it stands right now, so a later
/// open of the same bytes skips the walk.
///
/// A stamp that cannot be read **forgets** the path rather than storing a
/// placeholder: a remembered entry must mean "these exact bytes were checked",
/// and an entry that cannot say which bytes it refers to is worse than none.
pub(crate) fn remember_intact(db_path: &Path) {
    let mut verified = VERIFIED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match stamp(db_path) {
        Some(current) => {
            verified.insert(db_path.to_path_buf(), current);
        }
        None => {
            verified.remove(db_path);
        }
    }
}

/// Record that a walk is about to happen. Counted before the walk rather than
/// after it, so a walk that ends in a corruption verdict is still counted —
/// the cost was paid either way.
pub(crate) fn count_walk() {
    WALKS.fetch_add(1, Ordering::Relaxed);
}
