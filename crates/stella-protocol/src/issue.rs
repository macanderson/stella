//! The issue kernel, and the port a tracker is reached through.
//!
//! `doc:backlog-self-driving` B1, over `doc:agent-native-delivery` §3–§4's
//! model. The self-driving loop's defect queue is read by shelling out to `gh`
//! literally, in the shipping binary
//! (`crates/stella-cli/src/self_driving_cmd.rs`), and the type the ranker
//! consumes is GitHub's wire shape — `stella_autonomy::QueueIssue` carries a
//! `#[serde(rename = "createdAt")]`, which is a `gh --json` field name sitting
//! in a leaf crate that depends on nothing. So "which tracker" is not a
//! decision anything in the tree can express: it is spelled into the reader.
//!
//! This module is the seam. A tracker becomes an implementation of
//! [`IssueProvider`], the loop reads [`Issue`] values, and GitHub stops being
//! the only answer without becoming a special one — invariant 1, for the
//! backlog plane.
//!
//! # The model is deliberately tiny
//!
//! Four states, four classes, a key, a title, a body, labels, a created stamp
//! and a parent edge. That is the whole kernel, and it is
//! `doc:agent-native-delivery` §3's argument rather than a shortcut: everything
//! richer — a Jira workflow scheme, a Linear cycle, a GitHub project field — is
//! the *customer's* vocabulary, and modelling it here would make Stella learn
//! one tracker's data model and then fail to hold the next one. What crosses
//! this boundary is what the loop actually decides on.
//!
//! Two consequences worth stating, because both were tempting:
//!
//! - **There is no `assignee`.** A tracker's assignee field is not a lock — it
//!   has no compare-and-swap and no lease — and treating it as one is how two
//!   workers take the same issue. Claims live in the fleet ledger
//!   (`doc:agent-native-delivery` §10.4), which already has the primitive.
//! - **There is no `priority` field.** Priority is expressed as a label here,
//!   because that is how the trackers this must span actually carry it, and a
//!   dedicated field would force every provider to invent a mapping into it.
//!   Ranking reads labels (`stella_autonomy::rank_defects`).
//!
//! # Why the kernel is here and the ranking is not
//!
//! [`stella_autonomy`] is a leaf that depends on no other workspace crate —
//! that is what lets `stella-cli` and `stella-observatory` share its folds
//! without the observatory linking the machinery it observes. Moving the
//! ranker onto [`Issue`] would cost it that property for no gain, so the
//! ranker keeps its own input type and a caller maps into it. The mapping is
//! one function in the CLI, and it is the *only* place a tracker's shape meets
//! the loop's.
//!
//! [`stella_autonomy`]: https://docs.rs/stella-autonomy

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A tracker-scoped identifier for one issue, as the tracker spells it.
///
/// A string rather than a number, and that is the load-bearing choice: GitHub
/// counts (`1234`), Jira does not (`STELLA-1234`), Linear does not
/// (`ENG-42`). A numeric key would fit exactly one tracker and force every
/// other provider to fabricate one — and a fabricated key cannot be handed
/// back to the tracker it came from.
///
/// Opaque to Stella: it is produced by a provider, stored, compared for
/// equality, and handed back. Nothing here parses it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IssueKey(pub String);

impl IssueKey {
    /// The key as the tracker spells it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for IssueKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for IssueKey {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

/// Where an issue is in its life, collapsed to what the loop decides on.
///
/// Three buckets, not a workflow. `doc:agent-native-delivery` §4.1 is explicit
/// that a provider maps *every* reachable tracker status into one of these and
/// that a status mapping to none of them is a load error rather than a guess —
/// modelling the workflow itself is a stated non-goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueState {
    /// Nobody is working it. This is the queue the loop draws from.
    Open,
    /// Somebody or something holds it. Not a lock — see the module docs.
    InProgress,
    /// Finished, abandoned, or declined. The loop does not draw from here,
    /// but `sweep regress` re-reads it: a closed issue is a claim with an
    /// expiry (`doc:backlog-self-driving` §4.3).
    Closed,
}

/// What kind of work an issue is, which decides what must exist before it can
/// be called done.
///
/// Four, matching `doc:agent-native-delivery` §3. [`IssueClass::Other`] is the
/// honest bucket for a type a provider's manifest does not map, and it exists
/// so that an unmapped type is *visible* rather than silently filed as a
/// feature and given the wrong policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueClass {
    /// Something is broken. A fix wants a regression witness.
    Bug,
    /// Something should exist that does not.
    Feature,
    /// Work that is neither — a migration, a cleanup, a chore.
    Task,
    /// The provider's vocabulary had no mapping for this one.
    Other,
}

/// One label, as the tracker spells it.
///
/// Deliberately a struct with a single `name` rather than a bare `String`:
/// every tracker this must span carries more per label (a colour, a
/// description, an id), and a caller that wants one later should find a field
/// added here rather than a `Vec<String>` widened everywhere it is passed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IssueLabel {
    /// The label text — `"P1"`, `"area:core"`, `"bug"`.
    pub name: String,
}

impl From<&str> for IssueLabel {
    fn from(name: &str) -> Self {
        Self {
            name: name.to_owned(),
        }
    }
}

/// One issue, in the only shape that crosses this boundary.
///
/// Round-trips through `serde_json` byte-for-byte (invariant 4); the test is
/// beside the type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    /// The tracker's own identifier.
    pub key: IssueKey,
    /// One line. Never empty in practice; not enforced here, because a
    /// provider faithfully reporting an empty title is more useful than one
    /// that refuses to.
    pub title: String,
    /// The body, as the tracker holds it.
    ///
    /// **Untrusted input, always.** Issue text is written by whoever can file
    /// an issue, and it reaches a prompt as *data, never instruction*
    /// (`doc:agent-native-delivery` §10.2). Of the context-record kinds only
    /// `directive` carries instruction authority, and issue text is never one.
    #[serde(default)]
    pub body: String,
    /// Which of the three buckets the tracker's status maps to.
    pub state: IssueState,
    /// Which policy applies to it.
    pub class: IssueClass,
    /// Labels, verbatim. Priority and area live here — see the module docs.
    #[serde(default)]
    pub labels: Vec<IssueLabel>,
    /// When the tracker says it was created, RFC3339.
    ///
    /// A string rather than a timestamp type: this crate holds no clock and no
    /// date dependency, the value is compared lexically for age ordering
    /// (RFC3339 sorts correctly as text), and a provider that cannot produce
    /// one supplies an empty string rather than a fabricated instant.
    #[serde(default)]
    pub created_at: String,
    /// Where a human would go to read it.
    #[serde(default)]
    pub url: String,
    /// The epic or parent this hangs off, when the tracker has one.
    ///
    /// One edge, not a tree: `doc:agent-native-delivery` §5 resolves a spec by
    /// walking *up* this edge, and nothing in the loop needs to walk down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<IssueKey>,
}

/// Why a tracker read or write did not happen.
///
/// Typed rather than a `String` (invariant 5), and the variants are chosen by
/// what a **caller has to do differently**, which is the test that rule states:
/// an unavailable tracker is retried or degraded, an unauthenticated one needs
/// a human, a missing issue is a permanent answer, and a malformed payload is
/// a bug in the provider.
#[derive(Debug, thiserror::Error)]
pub enum IssueError {
    /// The provider's transport is not installed or not on `PATH`.
    #[error("issue provider `{provider}` is unavailable: {reason}")]
    Unavailable {
        /// The provider id that could not be reached.
        provider: String,
        /// What was missing, in terms a human can act on.
        reason: String,
    },
    /// The transport is present but the caller is not authenticated to it.
    ///
    /// Distinct from [`IssueError::Unavailable`] because the remedy differs and
    /// a caller should say which: installing a tool and logging into it are
    /// different instructions, and the loop cannot fix either itself.
    #[error("issue provider `{provider}` is not authenticated: {reason}")]
    Unauthenticated {
        /// The provider id that refused.
        provider: String,
        /// What the tracker said.
        reason: String,
    },
    /// No issue with that key.
    #[error("no such issue: {key}")]
    NotFound {
        /// The key that resolved to nothing.
        key: IssueKey,
    },
    /// The tracker answered, and the answer did not parse.
    #[error("issue provider `{provider}` returned a payload this build cannot read: {reason}")]
    Malformed {
        /// The provider id whose payload was rejected.
        provider: String,
        /// The parse failure.
        reason: String,
    },
    /// The tracker answered with a failure of its own.
    #[error("issue provider `{provider}` failed: {reason}")]
    Failed {
        /// The provider id that failed.
        provider: String,
        /// What it said.
        reason: String,
    },
}

/// The port every tracker is reached through — invariant 1 for the backlog
/// plane.
///
/// A new tracker is an adapter, never a change to the loop. Nothing above this
/// trait knows whether the issues came from GitHub, Jira, a TOML fixture, or a
/// local journal, which is exactly the property the witness test asserts by
/// ranking a real queue with no `gh` on `PATH` at all.
///
/// # Read-only, on purpose, at this slice
///
/// Filing, claiming and closing are `doc:backlog-self-driving` §3.1's other
/// four `backlog` calls, and they are deliberately not here yet: a write path
/// with nothing calling it is the unwired code AGENTS.md's "nothing left
/// behind" rule exists to prevent. This trait grows the write half in the
/// slice that serves those calls, and #3599 tracks it.
#[async_trait]
pub trait IssueProvider: Send + Sync {
    /// Stable id for this provider, e.g. `"github"` — what an error names and
    /// what a workspace binds to.
    fn id(&self) -> &str;

    /// Every issue currently in [`IssueState::Open`], newest-first or in
    /// whatever order the tracker returns them.
    ///
    /// **Ordering is not part of the contract**: ranking is the loop's job and
    /// is deterministic (`stella_autonomy::rank_defects`), so a provider that
    /// sorted differently could not change which issues a cycle picks. `limit`
    /// bounds the read, because a tracker with ten thousand open issues should
    /// not have all of them cross this boundary to produce a batch of five.
    async fn list_open(&self, limit: usize) -> Result<Vec<Issue>, IssueError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Issue {
        Issue {
            key: IssueKey::from("1234"),
            title: "the retry counter survives a goal-round boundary".into(),
            body: "`RetryHistory` is keyed per turn.".into(),
            state: IssueState::Open,
            class: IssueClass::Bug,
            labels: vec![IssueLabel::from("P1"), IssueLabel::from("area:core")],
            created_at: "2026-08-19T05:00:00Z".into(),
            url: "https://github.com/macanderson/stella/issues/1234".into(),
            parent: Some(IssueKey::from("1200")),
        }
    }

    /// Invariant 4: every type crossing a crate boundary round-trips through
    /// `serde_json` byte-for-byte.
    #[test]
    fn an_issue_round_trips_byte_for_byte() {
        let issue = fixture();
        let json = serde_json::to_string(&issue).expect("serialize");
        let back: Issue = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, issue);
        assert_eq!(serde_json::to_string(&back).expect("re-serialize"), json);
    }

    /// The optional half round-trips too — a `parent`-less issue must not
    /// serialize a null that a stricter reader would reject.
    #[test]
    fn an_issue_with_no_parent_omits_the_field() {
        let issue = Issue {
            parent: None,
            ..fixture()
        };
        let json = serde_json::to_string(&issue).expect("serialize");
        assert!(!json.contains("parent"), "{json}");
        let back: Issue = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, issue);
    }

    /// The wire spellings are pinned. These cross a process boundary into
    /// provider manifests and stored records, so a rename is a breaking change
    /// and should have to edit this test to happen.
    #[test]
    fn every_closed_vocabulary_is_pinned_on_the_wire() {
        let states = [
            (IssueState::Open, "\"open\""),
            (IssueState::InProgress, "\"in_progress\""),
            (IssueState::Closed, "\"closed\""),
        ];
        for (state, wire) in states {
            assert_eq!(serde_json::to_string(&state).expect("serialize"), wire);
        }

        let classes = [
            (IssueClass::Bug, "\"bug\""),
            (IssueClass::Feature, "\"feature\""),
            (IssueClass::Task, "\"task\""),
            (IssueClass::Other, "\"other\""),
        ];
        for (class, wire) in classes {
            assert_eq!(serde_json::to_string(&class).expect("serialize"), wire);
        }
    }

    /// A key is transparent on the wire — a bare string, not `{"0": "..."}`.
    /// A provider in another language writes the tracker's own identifier and
    /// nothing around it.
    #[test]
    fn a_key_is_a_bare_string_on_the_wire() {
        let json = serde_json::to_string(&IssueKey::from("STELLA-42")).expect("serialize");
        assert_eq!(json, "\"STELLA-42\"");
    }

    /// A body absent from the payload is empty, never a parse failure: a
    /// provider that lists issues without fetching each body is doing the
    /// cheap correct thing, and the queue read does not need one.
    #[test]
    fn the_optional_fields_default_rather_than_refuse() {
        let minimal = r#"{"key":"7","title":"t","state":"open","class":"task"}"#;
        let issue: Issue = serde_json::from_str(minimal).expect("deserialize");
        assert_eq!(issue.body, "");
        assert_eq!(issue.created_at, "");
        assert_eq!(issue.url, "");
        assert!(issue.labels.is_empty());
        assert_eq!(issue.parent, None);
    }
}
