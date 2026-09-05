// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Where a driver session's record goes.
//!
//! `stella plugin drive` opens one session and serves whatever the grant
//! allows. Before this module, it printed the outcome and threw it away.
//! The session id, every refused ask, and how the session ended were all
//! gone once the terminal scrolled.
//!
//! # Which ledger, and why not the other two
//!
//! The issue named three options. The self-driving audit ledger needs a
//! `LoopState`. `stella plugin drive` is a bare CLI verb, not a
//! self-driving loop, so it has no reason to open one. A `store.db` row
//! would need a schema change for three fields nothing reads yet. An
//! `AgentEvent` case needs a declared reader, and a driver session has
//! none today.
//!
//! So this module is the fourth answer. It appends one line of JSON per
//! session to `.stella/private/driver-sessions.jsonl`, through
//! [`stella_store::append_workspace_private_line`] — the same primitive
//! [`crate::memory::self_tuning`]'s own ledger uses. The file is
//! workspace-private and stays out of git, like every other file under
//! `.stella/private/`.
//!
//! # A write failure never fails the session
//!
//! If the record fails to write, that failure is reported. The session
//! still counts as done. An operator has already seen the outcome on
//! screen, so a disk error should not take it away. See
//! [`crate::self_driving_cmd::audit`]'s own docs for the same rule.

use std::path::Path;

use serde::{Deserialize, Serialize};
use stella_plugin::DriveNext;

/// The file every session's record is appended to, under
/// `.stella/private/`.
const LOG_NAME: &str = "driver-sessions.jsonl";

/// How a driver session ended, as the record holds it.
///
/// Mirrors [`DriveNext`], plus one case it cannot hold: a session whose
/// transport, process, or timeout failed before a `next` came back. A
/// separate type, because a plugin-facing wire type should not also carry
/// a host-only failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum DriverSessionOutcome {
    /// The driver asked to be woken again after this many seconds.
    Sleep {
        /// The clamped duration the driver asked for.
        secs: u32,
    },
    /// The driver stopped, and said why.
    Halt {
        /// The sentence a human reads in the record.
        reason: String,
    },
    /// The session never ended: a transport failure, a spawn failure, or
    /// a timeout.
    Error {
        /// [`BoundDriver::open`](super::BoundDriver::open)'s own message,
        /// which already names the program.
        message: String,
    },
}

impl DriverSessionOutcome {
    /// What a session's own result reads as, for the record.
    pub(crate) fn from_result(result: &Result<DriveNext, String>) -> Self {
        match result {
            Ok(DriveNext::Sleep { secs }) => Self::Sleep { secs: *secs },
            Ok(DriveNext::Halt { reason }) => Self::Halt {
                reason: reason.clone(),
            },
            Err(error) => Self::Error {
                message: error.clone(),
            },
        }
    }
}

/// One driver session, as a fact on disk.
///
/// Serde-first, so a test checks a value instead of parsing a sentence —
/// the same rule [`crate::self_driving_cmd::audit::AuditEntry`] follows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DriverSessionRecord {
    /// When the session was opened, RFC3339 UTC.
    pub at: String,
    /// The installed plugin's manifest name.
    pub plugin: String,
    /// The id `plugin_cmd::session_id` made and sent to the driver. This
    /// record and the driver's own logs join on it.
    pub session_id: String,
    /// The program the host resolved and started.
    pub program: String,
    /// Every ask this session could not serve, in order —
    /// [`DriverCallGate::refusals`](stella_runtime::wrapper::DriverCallGate::refusals)'s
    /// own words.
    pub refusals: Vec<String>,
    /// How the session ended.
    pub outcome: DriverSessionOutcome,
}

/// Append one session's record.
///
/// # Errors
///
/// A message naming what failed: the record could not be built, or the
/// append itself failed. Never fatal to the session that produced it —
/// see this module's docs.
pub(crate) fn record(workspace_root: &Path, entry: &DriverSessionRecord) -> Result<(), String> {
    let line = serde_json::to_string(entry)
        .map_err(|error| format!("could not build this driver session's record: {error}"))?;
    stella_store::append_workspace_private_line(workspace_root, LOG_NAME, &line)
        .map(|_| ())
        .map_err(|error| format!("could not record this driver session: {error}"))
}

/// Read every session record that parses (bad lines skipped), oldest
/// first.
///
/// The read side of [`record`]. A record nothing reads back is a write
/// nobody can check.
#[cfg(test)]
pub(crate) fn read_sessions(workspace_root: &Path) -> Vec<DriverSessionRecord> {
    let Ok(path) = stella_store::workspace_private_state_path(workspace_root, LOG_NAME) else {
        return Vec::new();
    };
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<DriverSessionRecord>(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "stella-driver-session-log-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp root");
        root
    }

    /// Every field comes back the same. The outcome is a tagged word, not
    /// a sentence a reader has to parse.
    #[test]
    fn a_record_round_trips_through_json() {
        let entry = DriverSessionRecord {
            at: "2026-09-02T21:00:00Z".into(),
            plugin: "watcher".into(),
            session_id: "drive-1-abcd".into(),
            program: "/opt/pkgs/watcher/drive.sh".into(),
            refusals: vec!["backlog_next: unsupported".into()],
            outcome: DriverSessionOutcome::Sleep { secs: 30 },
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        let back: DriverSessionRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(entry, back);
        assert!(json.contains(r#""kind":"sleep""#), "{json}");
        assert!(json.contains(r#""plugin":"watcher""#), "{json}");
    }

    /// Every ending a driver can send, plus the host's own failure path,
    /// maps to the outcome the record holds.
    #[test]
    fn every_ending_converts_to_an_outcome() {
        assert_eq!(
            DriverSessionOutcome::from_result(&Ok(DriveNext::Sleep { secs: 5 })),
            DriverSessionOutcome::Sleep { secs: 5 }
        );
        assert_eq!(
            DriverSessionOutcome::from_result(&Ok(DriveNext::Halt {
                reason: "budget spent".into()
            })),
            DriverSessionOutcome::Halt {
                reason: "budget spent".into()
            }
        );
        assert_eq!(
            DriverSessionOutcome::from_result(&Err("could not start driver: enoent".into())),
            DriverSessionOutcome::Error {
                message: "could not start driver: enoent".into()
            }
        );
    }

    /// **The witness.** Before this module, nothing wrote a driver
    /// session down anywhere. `record` is the first writer, and
    /// `read_sessions` reads back exactly what it wrote.
    #[test]
    fn a_recorded_session_reads_back() {
        let root = temp_root("roundtrip");
        assert!(
            read_sessions(&root).is_empty(),
            "nothing recorded before the first append"
        );

        let entry = DriverSessionRecord {
            at: "2026-09-02T21:00:00Z".into(),
            plugin: "watcher".into(),
            session_id: "drive-1-abcd".into(),
            program: "/opt/pkgs/watcher/drive.sh".into(),
            refusals: Vec::new(),
            outcome: DriverSessionOutcome::Halt {
                reason: "operator stop".into(),
            },
        };
        record(&root, &entry).expect("append must succeed");

        let sessions = read_sessions(&root);
        assert_eq!(sessions, vec![entry]);
    }

    /// One bad line does not lose the rest of the file — the same rule
    /// `self_tuning::read_ledger` gives its own log, for a file a crashed
    /// process could half-write.
    #[test]
    fn a_bad_line_is_skipped_not_fatal() {
        let root = temp_root("bad-line");
        let good = DriverSessionRecord {
            at: "2026-09-02T21:00:00Z".into(),
            plugin: "watcher".into(),
            session_id: "drive-2-efgh".into(),
            program: "/opt/pkgs/watcher/drive.sh".into(),
            refusals: Vec::new(),
            outcome: DriverSessionOutcome::Sleep { secs: 60 },
        };
        record(&root, &good).expect("first append must succeed");
        stella_store::append_workspace_private_line(&root, LOG_NAME, "not json")
            .expect("second append must succeed");

        let sessions = read_sessions(&root);
        assert_eq!(sessions, vec![good]);
    }
}
