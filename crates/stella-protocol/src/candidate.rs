// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A candidate workspace, named so it can cross a process — #3380,
//! `doc:pipeline-as-plugins` §4 A10, `doc:wrapper-socket` §6.
//!
//! `after_turn` is defined as "author a witness, run the oracle, read the
//! flip", and all three of those need the candidate worktree. Today that
//! worktree is only reachable as `stella_pipeline::ports::CandidateWorkspace`
//! — nineteen methods returning borrowed trait objects — and **an
//! out-of-process plugin cannot be handed a `&dyn` anything**
//! (`doc:wrapper-socket` §6, the no-host-assumed acceptance test). What
//! crosses instead is a [`CandidateHandle`]: a name the host minted, which
//! the host resolves back to the live workspace on every use.
//!
//! # The subset is six operations, and that is a decision
//!
//! [`CandidateOp`] is the whole plugin-facing surface: create, root, run-test,
//! seal, adopt, remove. The other thirteen methods on `CandidateWorkspace`
//! serve the pipeline's own orchestration — seeding a private task board,
//! gating announcements, grafting a witness between snapshots, the post-seal
//! escape probe — and none of them is a question a plugin asks. Every
//! operation added here is one more thing a Python or TypeScript plugin author
//! must understand before writing a line, so the count is the design.
//!
//! # A handle is a capability the host bounds, never a path to be trusted
//!
//! [`CandidateOp::Root`] hands out an absolute path, because a plugin that
//! runs a test suite needs one. That does **not** make a plugin-supplied path
//! trustworthy on the way back in: every path a plugin names is resolved
//! against the handle's root by the host and refused if it lands outside
//! ([`PathDenial`]). The refusal is the host's, on the host's filesystem,
//! after symlinks — never a promise the plugin was asked to keep. The
//! implementation of that fence is `stella_pipeline::ports::handle`.
//!
//! # Why this crate and not `stella-plugin`
//!
//! `doc:wrapper-socket` §2 puts the wrapper's *wire contract* in
//! `stella-plugin`, and this type is wire-facing, so the question is real. It
//! lives here for three reasons:
//!
//! 1. `stella-plugin`'s boundary is the **manifest**: declarations parsed and
//!    validated out of borrowed text at load time. A handle is the opposite
//!    lifetime and the opposite author — a value the *host* mints per
//!    candidate at run time, meaningless outside the registry that minted it.
//! 2. Invariant #4 is this crate's stated contract, and a handle exists for no
//!    other purpose than to round-trip: see the tests below.
//! 3. `stella-plugin` already depends on `stella-protocol`, so the
//!    plugin-facing request/response envelope can name these types without
//!    moving them. The reverse placement would push every host that resolves a
//!    handle — `stella-pipeline`, and later `stella-runtime` and
//!    `stella-serve` — through the manifest-parsing crate to reach a type that
//!    parses no manifests.
//!
//! # Wire shape
//!
//! ```json
//! {"handle": "candidate-1"}
//! {"op": "run_test"}
//! {"path": {"path": "../etc/passwd", "reason": "traversal"}}
//! ```
//!
//! Per invariant #4 every type here round-trips through `serde_json`
//! byte-for-byte.

use serde::{Deserialize, Serialize};

/// A serializable name for one live candidate workspace.
///
/// Opaque by construction: the string is the host's, and nothing but the host
/// that minted it can turn one back into a workspace. It carries no path, no
/// file descriptor and no authority of its own — resolving it is a lookup in
/// the minting registry, so a handle for a workspace that was never created,
/// or one already removed, is [`CandidateDenial::UnknownHandle`].
///
/// A newtype rather than a bare `String` for the reason AGENTS.md's glossary
/// gives: this workspace already has six identifiers that read alike, and an
/// unwrapped `String` is how a seventh joins them.
///
/// **It is a name, not a bearer token.** Ids are minted in order, so a plugin
/// that holds one handle can spell a sibling candidate's. Binding a handle to
/// the principal it was issued to belongs with plugin identity (#3380 A1) —
/// until that exists (#3484), a host must not hand handles to two principals
/// it means to keep apart.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CandidateHandle(String);

impl CandidateHandle {
    /// Wrap an id a host minted.
    ///
    /// There is no validation here and that is deliberate: this crate carries
    /// zero logic by invariant, and the only meaningful check — "does this
    /// name a live workspace?" — is a lookup only the minting host can do.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The id as minted.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CandidateHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The closed set of operations a [`CandidateHandle`] addresses.
///
/// Closed on purpose — see the module docs. A host implements exactly these
/// six; a plugin may ask for exactly these six.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateOp {
    /// Snapshot the tree into a fresh isolated workspace and mint its handle.
    Create,
    /// The workspace's absolute root path.
    Root,
    /// Run one already-parsed test command inside the workspace.
    RunTest,
    /// Commit the workspace's current bytes into its private history, so a
    /// verification observation and an adoption see the same tree.
    Seal,
    /// Apply the workspace's changes to the real tree, all-or-nothing.
    Adopt,
    /// Discard the workspace.
    Remove,
}

impl CandidateOp {
    /// Every operation, in lifecycle order. Exhaustively matched in this
    /// module's tests, so a new variant fails to compile until it is listed.
    pub const ALL: [CandidateOp; 6] = [
        CandidateOp::Create,
        CandidateOp::Root,
        CandidateOp::RunTest,
        CandidateOp::Seal,
        CandidateOp::Adopt,
        CandidateOp::Remove,
    ];

    /// The wire spelling — identical to the serde one, which a test pins.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            CandidateOp::Create => "create",
            CandidateOp::Root => "root",
            CandidateOp::RunTest => "run_test",
            CandidateOp::Seal => "seal",
            CandidateOp::Adopt => "adopt",
            CandidateOp::Remove => "remove",
        }
    }
}

impl std::fmt::Display for CandidateOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a path a plugin named was not resolved inside a candidate root.
///
/// Four refusals rather than one, because they are not the same event: the
/// first three are answered without touching the filesystem and mean the
/// request was malformed, while [`PathDenial::EscapesRoot`] means a
/// well-formed request resolved — through a symlink, or a root that moved —
/// to somewhere else, and is the one a host should treat as interesting.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum PathDenial {
    /// The path named nothing — empty, or only `.` components.
    #[error("the path is empty")]
    Empty,
    /// The path was absolute (POSIX `/…`, or a Windows `C:` prefix), so it
    /// addresses the machine rather than the candidate.
    #[error("the path is absolute")]
    Absolute,
    /// The path contained a `..` component. Refused as written rather than
    /// normalised away: a caller that meant a path inside the root never
    /// needs one, and normalising first is how a fence is talked past.
    #[error("the path contains a `..` component")]
    Traversal,
    /// The path is not a portable relative path — it holds a NUL byte, or a
    /// backslash this host cannot read as a separator without guessing.
    #[error("the path is not a portable relative path")]
    Malformed,
    /// The path was well-formed and still resolved outside the candidate
    /// root once the filesystem had its say — the symlink case.
    #[error("the path resolves outside the candidate workspace root")]
    EscapesRoot,
}

/// What a host says when it refuses a handle-addressed request.
///
/// Serializable because the refusal has to reach the plugin that asked, in
/// whatever language it is written in — a denial a consumer cannot branch on
/// is invariant #5's objection restated one process out.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum CandidateDenial {
    /// No live workspace answers to this handle: it was never minted here, it
    /// was already removed, or it was guessed.
    #[error("no candidate workspace answers to handle `{handle}`")]
    UnknownHandle {
        /// The handle as presented.
        handle: CandidateHandle,
    },
    /// A path the caller named does not address anything inside the
    /// workspace.
    #[error("path `{path}` was refused: {reason}")]
    Path {
        /// The path exactly as the caller wrote it, never a normalised form —
        /// a caller must be able to recognise its own input in the refusal.
        path: String,
        /// Which fence stopped it.
        reason: PathDenial,
    },
    /// The workspace's root could not be resolved on this filesystem, so
    /// nothing could be fenced against it. Fails closed: a root that cannot
    /// be established refuses every path rather than admitting them.
    #[error("the candidate workspace root could not be resolved: {reason}")]
    RootUnavailable {
        /// The underlying filesystem error, as prose.
        reason: String,
    },
    /// The test command did not survive the host's strict test-command
    /// parser, so no process was spawned.
    #[error("the test command was refused: {reason}")]
    TestCommandRefused {
        /// The parser's own reason, as prose. The vocabulary it comes from is
        /// the host's (`stella_pipeline::witness::TestInvocationError`) and is
        /// deliberately not mirrored here: a second copy of a closed enum in
        /// this crate is a rule in two places.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_round_trips_byte_for_byte() {
        let handle = CandidateHandle::new("candidate-3");
        let encoded = serde_json::to_string(&handle).expect("serialises");
        assert_eq!(encoded, r#""candidate-3""#);
        let decoded: CandidateHandle = serde_json::from_str(&encoded).expect("deserialises");
        assert_eq!(decoded, handle);
        assert_eq!(
            serde_json::to_string(&decoded).expect("re-serialises"),
            encoded
        );
    }

    #[test]
    fn every_op_round_trips_and_the_set_is_exhaustive() {
        for op in CandidateOp::ALL {
            // Exhaustive match: a new variant fails to compile here until it
            // is added to `ALL` as well.
            match op {
                CandidateOp::Create
                | CandidateOp::Root
                | CandidateOp::RunTest
                | CandidateOp::Seal
                | CandidateOp::Adopt
                | CandidateOp::Remove => {}
            }
            let encoded = serde_json::to_string(&op).expect("serialises");
            assert_eq!(encoded, format!(r#""{}""#, op.as_str()));
            let decoded: CandidateOp = serde_json::from_str(&encoded).expect("deserialises");
            assert_eq!(decoded, op);
        }
        let mut seen = CandidateOp::ALL.to_vec();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), CandidateOp::ALL.len(), "ALL has a duplicate");
    }

    #[test]
    fn denials_round_trip_with_the_path_as_written() {
        let denials = [
            CandidateDenial::UnknownHandle {
                handle: CandidateHandle::new("candidate-9"),
            },
            CandidateDenial::Path {
                path: "../../etc/passwd".into(),
                reason: PathDenial::Traversal,
            },
            CandidateDenial::Path {
                path: "escape/secret".into(),
                reason: PathDenial::EscapesRoot,
            },
            CandidateDenial::RootUnavailable {
                reason: "No such file or directory".into(),
            },
            CandidateDenial::TestCommandRefused {
                reason: "unsupported test command `rm -rf /`".into(),
            },
        ];
        for denial in denials {
            let encoded = serde_json::to_string(&denial).expect("serialises");
            let decoded: CandidateDenial = serde_json::from_str(&encoded).expect("deserialises");
            assert_eq!(decoded, denial);
            assert_eq!(
                serde_json::to_string(&decoded).expect("re-serialises"),
                encoded
            );
        }
    }

    #[test]
    fn a_refused_path_is_reported_exactly_as_written() {
        let denial = CandidateDenial::Path {
            path: "../outside".into(),
            reason: PathDenial::Traversal,
        };
        assert_eq!(
            denial.to_string(),
            "path `../outside` was refused: the path contains a `..` component"
        );
    }
}
