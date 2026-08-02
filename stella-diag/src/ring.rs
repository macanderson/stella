// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The crash ring — `docs/design/diagnostics.md` §7.4.
//!
//! A bounded in-memory ring holding **every** record at **every** level,
//! filter notwithstanding. It is written to disk only on a panic or a non-zero
//! exit, landing at `.stella/private/crash-<ts>.jsonl`.
//!
//! This is what lets stella say the sentence it currently cannot:
//!
//! > Attach `.stella/private/crash-*.jsonl` to the issue.
//!
//! And here §5.2 pays off completely. Because content in a record does not
//! compile, that file is content-free *by construction* — so a maintainer can
//! ask a stranger on the internet to attach it without a privacy review, and
//! the stranger can send it without reading it first. A crash dump you can
//! safely ask for is worth more than a verbose flag nobody can rerun, which for
//! a *coding agent* is the whole argument: runs are nondeterministic and
//! expensive, so "reproduce it with verbose on" is frequently not a request the
//! user can fulfil (§1.2).
//!
//! What was dropped is recorded rather than hidden. A ring that silently ate
//! the beginning of a run would make a crash file read as a complete history
//! when it is a tail, and a reader who cannot tell the difference will
//! misdiagnose.

use std::collections::VecDeque;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::private;
use crate::record::Record;
use crate::sink::SinkError;

/// Records held, at most. §7.4's first bound.
pub const MAX_RECORDS: usize = 2_000;

/// Estimated bytes held, at most. §7.4's second bound, whichever binds first.
pub const MAX_BYTES: usize = 1024 * 1024;

/// The `.stella/private/` filename prefix a crash dump lands under. `stella
/// doctor --last-failure` and any "attach the log" instruction both key off it.
pub const CRASH_PREFIX: &str = "crash-";

/// What a ring holds, and what it had to drop to stay bounded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RingStats {
    pub held: usize,
    pub bytes: usize,
    /// Records evicted to stay within the bounds. Non-zero means the file is a
    /// tail, and the dump says so in its own first record.
    pub dropped: u64,
}

/// A bounded in-memory ring of records.
///
/// Everything reaches it regardless of filter, which is what keeps a crash
/// dump complete for a user who ran with the default `warn` — the case that
/// matters, because nobody reproduces a crash on purpose with `-vvv` first.
#[derive(Debug)]
pub struct Ring {
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    records: VecDeque<(Record, usize)>,
    bytes: usize,
    dropped: u64,
    max_records: usize,
    max_bytes: usize,
}

impl Default for Ring {
    fn default() -> Self {
        Self::new(MAX_RECORDS, MAX_BYTES)
    }
}

impl Ring {
    #[must_use]
    pub fn new(max_records: usize, max_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                records: VecDeque::new(),
                bytes: 0,
                dropped: 0,
                // A zero bound would make `push` spin evicting a record it just
                // added; one is the smallest ring that terminates.
                max_records: max_records.max(1),
                max_bytes: max_bytes.max(1),
            }),
        }
    }

    /// Add a record, evicting the oldest until both bounds hold.
    pub fn push(&self, record: Record) {
        let size = record.estimated_bytes();
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.records.push_back((record, size));
        inner.bytes = inner.bytes.saturating_add(size);
        while inner.records.len() > inner.max_records
            || (inner.bytes > inner.max_bytes && inner.records.len() > 1)
        {
            let Some((_, evicted)) = inner.records.pop_front() else {
                break;
            };
            inner.bytes = inner.bytes.saturating_sub(evicted);
            inner.dropped = inner.dropped.saturating_add(1);
        }
    }

    /// Every record still held, oldest first.
    #[must_use]
    pub fn records(&self) -> Vec<Record> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .records
            .iter()
            .map(|(record, _)| record.clone())
            .collect()
    }

    #[must_use]
    pub fn stats(&self) -> RingStats {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        RingStats {
            held: inner.records.len(),
            bytes: inner.bytes,
            dropped: inner.dropped,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .records
            .is_empty()
    }

    /// Write every held record to `path` as JSONL, owner-only.
    ///
    /// The file opens with a `diag.ring.dumped` record of its own, carrying
    /// what the ring held and what it dropped — so a reader can tell a complete
    /// history from a tail without counting lines.
    pub fn dump_to(&self, path: &Path) -> Result<usize, SinkError> {
        let stats = self.stats();
        let records = self.records();
        let mut file = private::open_append(path)?;

        let header = Record::new(
            crate::Level::Info,
            "diag.ring.dumped",
            module_path!(),
            crate::Cx::EMPTY,
            crate::Fields::new()
                .with("held", stats.held)
                .with("dropped", stats.dropped)
                .with("bytes", stats.bytes),
        );
        let mut written = 0;
        for record in std::iter::once(&header).chain(records.iter()) {
            let mut line = serde_json::to_vec(record).map_err(|_| SinkError::Encode)?;
            line.push(b'\n');
            file.write_all(&line)?;
            written += 1;
        }
        file.flush()?;
        private::tighten(path);
        Ok(written)
    }

    /// Dump into `dir` under a timestamped `crash-<ts>.jsonl` name, returning
    /// the path written.
    ///
    /// Nothing is written when the ring is empty: a zero-record crash file is
    /// an artifact that wastes a maintainer's time and teaches a user that the
    /// file is not worth attaching.
    pub fn dump_into(&self, dir: &Path) -> Result<Option<PathBuf>, SinkError> {
        if self.is_empty() {
            return Ok(None);
        }
        let path = dir.join(format!(
            "{CRASH_PREFIX}{}.jsonl",
            crate::record::now_millis()
        ));
        self.dump_to(&path)?;
        Ok(Some(path))
    }
}

/// How many crash dumps a workspace keeps. See [`prune_crash_files`].
pub const CRASH_KEEP: usize = 5;

/// Delete all but the `keep` newest `crash-*.jsonl` files in `dir`, returning
/// how many were removed.
///
/// A dump is written on **every** non-zero exit, and most non-zero exits are
/// ordinary user errors — a typo'd model id, a missing key. Without a bound,
/// `.stella/private/` would accumulate one file per failed run forever, and
/// nothing else in the workspace would ever clean them up (`stella stats
/// prune` covers the store, not this). A handful is all any bug report needs:
/// the interesting dump is the last one, and the four before it cover "it
/// started happening after…".
///
/// Best-effort, like everything else on the failure path: a file that will not
/// delete is left alone rather than turned into a second failure.
pub fn prune_crash_files(dir: &Path, keep: usize) -> usize {
    let mut stamped: Vec<(u64, PathBuf)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(stamp) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(crash_stamp)
        {
            stamped.push((stamp, path));
        }
    }
    if stamped.len() <= keep {
        return 0;
    }
    // Newest first, then drop everything past the keep window.
    stamped.sort_by_key(|(stamp, _)| std::cmp::Reverse(*stamp));
    stamped
        .drain(keep..)
        .filter(|(_, path)| std::fs::remove_file(path).is_ok())
        .count()
}

/// The timestamp in a `crash-<ts>.jsonl` filename, if it is one.
fn crash_stamp(name: &str) -> Option<u64> {
    name.strip_prefix(CRASH_PREFIX)
        .and_then(|rest| rest.strip_suffix(".jsonl"))
        .and_then(|stamp| stamp.parse::<u64>().ok())
}

/// The most recent `crash-*.jsonl` in `dir`, if any.
///
/// Ordered by the timestamp in the filename rather than by mtime: the name is
/// what the dump chose, and an mtime can be rewritten by a copy or a backup
/// restore.
#[must_use]
pub fn latest_crash_file(dir: &Path) -> Option<PathBuf> {
    let mut newest: Option<(u64, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(stamp) = crash_stamp(name) else {
            continue;
        };
        if newest.as_ref().is_none_or(|(best, _)| stamp > *best) {
            newest = Some((stamp, path));
        }
    }
    newest.map(|(_, path)| path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cx, Fields, Level};

    fn record(n: u64) -> Record {
        Record::at(
            n,
            Level::Debug,
            "test.record",
            "stella_diag::ring",
            Cx::EMPTY,
            Fields::new().with("n", n),
        )
    }

    #[test]
    fn the_ring_keeps_the_most_recent_records_and_counts_the_rest() {
        let ring = Ring::new(3, MAX_BYTES);
        for n in 0..10 {
            ring.push(record(n));
        }
        let held = ring.records();
        assert_eq!(held.len(), 3);
        assert_eq!(
            held.iter().map(|r| r.ts).collect::<Vec<_>>(),
            vec![7, 8, 9],
            "the ring keeps the tail, which is the part next to the crash"
        );
        assert_eq!(ring.stats().dropped, 7);
    }

    /// Whichever bound binds first. A byte ceiling that could be exceeded
    /// would make a crash dump unbounded on a run with wide fields.
    #[test]
    fn the_byte_bound_binds_before_the_record_bound() {
        let ring = Ring::new(MAX_RECORDS, 400);
        for n in 0..50 {
            ring.push(record(n));
        }
        let stats = ring.stats();
        assert!(stats.held < 5, "held {} on a 400-byte ring", stats.held);
        assert!(stats.bytes <= 400 || stats.held == 1);
        assert!(stats.dropped > 0);
    }

    /// The bounds must never evict the record that was just pushed — a ring
    /// smaller than one record still has to hold the last thing that happened.
    #[test]
    fn a_ring_smaller_than_one_record_still_holds_the_last_one() {
        let ring = Ring::new(1, 1);
        ring.push(record(1));
        ring.push(record(2));
        let held = ring.records();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].ts, 2);
    }

    #[test]
    fn a_dump_is_jsonl_owner_only_and_opens_with_its_own_census() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ring = Ring::new(2, MAX_BYTES);
        for n in 0..5 {
            ring.push(record(n));
        }
        let path = ring.dump_into(dir.path()).expect("dump").expect("a path");

        let text = std::fs::read_to_string(&path).expect("read");
        let lines: Vec<Record> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("each line parses"))
            .collect();
        assert_eq!(lines.len(), 3, "a census record plus the two held");
        assert_eq!(lines[0].code.as_str(), "diag.ring.dumped");
        assert_eq!(
            lines[0].fields.get("dropped"),
            Some(&crate::FieldValue::Uint(3))
        );
        assert_eq!(lines[1].ts, 3);
        assert_eq!(lines[2].ts, 4);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "a crash file is owner-only");
        }
    }

    /// A zero-record crash file teaches a user that the artifact is not worth
    /// attaching. Better to write nothing.
    #[test]
    fn an_empty_ring_writes_no_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(Ring::default().dump_into(dir.path()).expect("dump"), None);
        assert_eq!(std::fs::read_dir(dir.path()).expect("read").count(), 0);
    }

    #[test]
    fn the_latest_crash_file_is_the_highest_stamp_not_the_newest_mtime() {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in ["crash-100.jsonl", "crash-9000.jsonl", "crash-3.jsonl"] {
            std::fs::write(dir.path().join(name), "{}\n").expect("write");
        }
        std::fs::write(dir.path().join("traces.jsonl"), "{}\n").expect("write");
        std::fs::write(dir.path().join("crash-nope.jsonl"), "{}\n").expect("write");

        let latest = latest_crash_file(dir.path()).expect("a crash file");
        assert_eq!(latest.file_name().expect("name"), "crash-9000.jsonl");
    }

    /// A dump per failed run, forever, would be an unbounded write to a
    /// directory nothing else prunes. The newest survive, because the newest
    /// are the ones a bug report is about.
    #[test]
    fn pruning_keeps_the_newest_dumps_and_nothing_else() {
        let dir = tempfile::tempdir().expect("tempdir");
        for stamp in [10_u64, 30, 20, 50, 40, 60, 5] {
            std::fs::write(dir.path().join(format!("crash-{stamp}.jsonl")), "{}\n").expect("write");
        }
        // Files this does not own must survive pruning untouched.
        std::fs::write(dir.path().join("traces.jsonl"), "{}\n").expect("write");
        std::fs::write(dir.path().join("store.db"), "x").expect("write");

        assert_eq!(prune_crash_files(dir.path(), 3), 4);

        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .expect("read")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![
                "crash-40.jsonl",
                "crash-50.jsonl",
                "crash-60.jsonl",
                "store.db",
                "traces.jsonl",
            ]
        );
    }

    #[test]
    fn pruning_under_the_limit_deletes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("crash-1.jsonl"), "{}\n").expect("write");
        assert_eq!(prune_crash_files(dir.path(), CRASH_KEEP), 0);
        assert_eq!(prune_crash_files(Path::new("/nonexistent/stella"), 1), 0);
        assert!(dir.path().join("crash-1.jsonl").exists());
    }

    #[test]
    fn no_crash_file_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(latest_crash_file(dir.path()), None);
        assert_eq!(latest_crash_file(Path::new("/nonexistent/stella")), None);
    }
}
