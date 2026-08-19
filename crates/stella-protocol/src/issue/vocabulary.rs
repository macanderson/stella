//! How one tracker spells the concepts every tracker has.
//!
//! Every issue system has the same handful of ideas — a title, a body, labels,
//! a parent edge, whether the issue is open, and how it was resolved — and
//! every one of them spells those differently. GitHub says `completed` and
//! `not planned`; Jira says `Done` and `Won't Do`; the next one will say
//! something else. Hardcoding one tracker's words is how a product becomes
//! single-tracker without anyone deciding that it should.
//!
//! So the words are configuration and this is their model. **The concepts are
//! Stella's and fixed; the spellings are the customer's and are not.**
//!
//! # The two questions this answers
//!
//! 1. **What marks an issue closed?** [`Vocabulary::state_of`] maps a
//!    tracker's raw status string onto open or closed.
//! 2. **How was it resolved?** [`Vocabulary::resolution`] maps Stella's
//!    canonical resolution onto the tracker's own spelling for it.
//!
//! # Three canonical resolutions, and passthrough for the rest
//!
//! Stella branches on exactly three — *completed*, *not planned*, *duplicate* —
//! which are GitHub's own close reasons. That is not a simplification of
//! trackers, it is the honest size of what the loop **decides** with: a
//! regression sweep re-checks what was completed, and re-running a witness for
//! something declined as stale or folded into another issue would measure
//! nothing.
//!
//! A customer with twenty close reasons therefore maps the three Stella acts
//! on and leaves the rest alone: [`Vocabulary::resolution`] passes an unmapped
//! name through unchanged, so a tracker's own vocabulary reaches it verbatim
//! without every value having to be declared here first.
//!
//! # An unknown status is an error, not a guess
//!
//! [`Vocabulary::state_of`] returns `None` for a status in neither list.
//! `doc:agent-native-delivery` §4.1 requires exactly this, and the direction
//! matters: guessing *open* floods the queue with things nobody meant to
//! offer, and guessing *closed* silently drops work. A tracker that grew a
//! status is a thing a human should be told about once, not a thing the loop
//! should quietly decide.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::IssueState;

/// Stella's canonical resolution: the work was done.
pub const RESOLUTION_COMPLETED: &str = "completed";

/// Stella's canonical resolution: it will not be done.
pub const RESOLUTION_NOT_PLANNED: &str = "not_planned";

/// Stella's canonical resolution: another issue already covers it.
pub const RESOLUTION_DUPLICATE: &str = "duplicate";

/// Every resolution Stella itself branches on.
///
/// Three, matching GitHub's own close reasons, because those are the
/// distinctions the loop actually acts on: a regression sweep re-checks what
/// was **completed**, and re-running a witness for something declined as stale
/// or folded into another issue would measure nothing. A tracker offering
/// twenty reasons maps these three and passes the rest through.
pub const CANONICAL_RESOLUTIONS: &[&str] = &[
    RESOLUTION_COMPLETED,
    RESOLUTION_NOT_PLANNED,
    RESOLUTION_DUPLICATE,
];

/// One tracker's spellings for the concepts every tracker has.
///
/// Loaded from the active provider's manifest, which `stella.toml`'s
/// `[issues]` section names. Absent, [`Vocabulary::github`] applies — so a
/// workspace that has configured nothing still works against GitHub, which is
/// the tracker it is most likely to be pointed at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Vocabulary {
    /// Raw status strings that mean the issue is open.
    ///
    /// Compared case-insensitively, because trackers are inconsistent about
    /// it between their API and their UI and a customer should not have to
    /// discover which one their export used.
    pub open: Vec<String>,

    /// Raw status strings that mean the issue is closed.
    pub closed: Vec<String>,

    /// Stella's canonical resolution names mapped to this tracker's spelling.
    ///
    /// Only the two Stella branches on need entries. Anything else is passed
    /// through as written — see the module docs.
    pub resolutions: BTreeMap<String, String>,

    /// What this tracker calls each field of an issue.
    ///
    /// Carried so a provider adapter reads one declaration rather than
    /// hardcoding a query, and so a customer whose tracker calls the body a
    /// `description` can say so.
    pub fields: FieldNames,
}

/// What one tracker calls each part of an issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FieldNames {
    /// The one-line summary — `title`, `summary`, `name`.
    pub title: String,
    /// The long form — `body`, `description`.
    pub body: String,
    /// The classification list — `labels`, `tags`, `components`.
    pub labels: String,
    /// The edge to the issue this hangs off — `parent`, `epic`, `parentKey`.
    pub parent: String,
}

impl Default for FieldNames {
    /// GitHub's names, which are also the most common.
    fn default() -> Self {
        Self {
            title: "title".to_owned(),
            body: "body".to_owned(),
            labels: "labels".to_owned(),
            parent: "parent".to_owned(),
        }
    }
}

impl Default for Vocabulary {
    fn default() -> Self {
        Self::github()
    }
}

impl Vocabulary {
    /// GitHub's vocabulary — the default a workspace gets for free.
    ///
    /// Written out rather than left implicit so a customer can read what they
    /// are overriding, and so the defaults are one reviewable list rather than
    /// scattered `unwrap_or` calls at the call sites.
    #[must_use]
    pub fn github() -> Self {
        let mut resolutions = BTreeMap::new();
        resolutions.insert(RESOLUTION_COMPLETED.to_owned(), "completed".to_owned());
        resolutions.insert(RESOLUTION_NOT_PLANNED.to_owned(), "not planned".to_owned());
        resolutions.insert(RESOLUTION_DUPLICATE.to_owned(), "duplicate".to_owned());

        Self {
            open: vec!["open".to_owned()],
            closed: vec!["closed".to_owned()],
            resolutions,
            fields: FieldNames::default(),
        }
    }

    /// Which of Stella's states this tracker's raw status means.
    ///
    /// `None` for a status in neither list — a fact to report, never a guess.
    /// See the module docs on why guessing in either direction is worse than
    /// refusing.
    #[must_use]
    pub fn state_of(&self, raw: &str) -> Option<IssueState> {
        let raw = raw.trim();
        if self.open.iter().any(|s| s.eq_ignore_ascii_case(raw)) {
            return Some(IssueState::Open);
        }
        if self.closed.iter().any(|s| s.eq_ignore_ascii_case(raw)) {
            return Some(IssueState::Closed);
        }
        None
    }

    /// Whether this tracker's raw status means the issue is open.
    ///
    /// The blunt form of [`Vocabulary::state_of`], for the many callers that
    /// only need the boolean. An unknown status is **not** open: a caller
    /// asking this question has already given up the ability to report the
    /// unmapped status, and quietly queueing something nobody offered is the
    /// worse of the two failures.
    #[must_use]
    pub fn is_open(&self, raw: &str) -> bool {
        self.state_of(raw) == Some(IssueState::Open)
    }

    /// This tracker's spelling for a canonical resolution.
    ///
    /// Unmapped names pass through unchanged, so a tracker's own vocabulary
    /// reaches it verbatim without every value having to be declared here.
    #[must_use]
    pub fn resolution<'a>(&'a self, canonical: &'a str) -> &'a str {
        self.resolutions
            .get(canonical)
            .map_or(canonical, String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults work against GitHub with nothing configured — the tracker
    /// a workspace is most likely pointed at should need no setup.
    #[test]
    fn the_defaults_are_githubs_vocabulary() {
        let v = Vocabulary::default();
        assert_eq!(v.state_of("OPEN"), Some(IssueState::Open));
        assert_eq!(v.state_of("closed"), Some(IssueState::Closed));
        assert_eq!(v.resolution(RESOLUTION_COMPLETED), "completed");
        assert_eq!(v.resolution(RESOLUTION_NOT_PLANNED), "not planned");
        assert_eq!(v.resolution(RESOLUTION_DUPLICATE), "duplicate");
        assert_eq!(v.fields.body, "body");
    }

    /// **The portability witness.** A tracker with entirely different words is
    /// expressed by configuration, not by a code change.
    #[test]
    fn another_tracker_is_a_configuration_not_a_fork() {
        // Expressed as JSON rather than TOML because this crate carries no
        // TOML dependency and does not need one: the type is serde-generic, so
        // the manifest format is the *caller's* choice and the mapping proven
        // here holds for whichever one a provider loads.
        let jira: Vocabulary = serde_json::from_str(
            r#"{
                "open": ["To Do", "In Progress"],
                "closed": ["Done", "Won't Do"],
                "resolutions": {
                    "completed": "Done",
                    "not_planned": "Won't Do",
                    "duplicate": "Duplicate"
                },
                "fields": {
                    "body": "description",
                    "labels": "components",
                    "parent": "epic"
                }
            }"#,
        )
        .expect("parses");

        assert_eq!(jira.state_of("In Progress"), Some(IssueState::Open));
        assert_eq!(jira.state_of("Won't Do"), Some(IssueState::Closed));
        assert_eq!(jira.resolution(RESOLUTION_COMPLETED), "Done");
        assert_eq!(jira.resolution(RESOLUTION_DUPLICATE), "Duplicate");
        assert_eq!(jira.fields.body, "description");
        // Unmentioned fields keep their defaults rather than blanking.
        assert_eq!(jira.fields.title, "title");
    }

    /// **The refusal witness.** A status in neither list is reported, never
    /// guessed: guessing open floods the queue with things nobody offered, and
    /// guessing closed silently drops work.
    #[test]
    fn an_unmapped_status_is_reported_rather_than_guessed() {
        let v = Vocabulary::default();
        assert_eq!(v.state_of("in review"), None);
        assert!(
            !v.is_open("in review"),
            "the boolean form must not queue an unmapped status"
        );
    }

    /// A customer with twenty close reasons maps the two Stella acts on and
    /// leaves the rest alone — they still reach the tracker verbatim.
    #[test]
    fn an_unmapped_resolution_passes_through_unchanged() {
        let v = Vocabulary::default();
        assert_eq!(v.resolution("duplicate"), "duplicate");
        assert_eq!(v.resolution("cannot reproduce"), "cannot reproduce");
    }

    /// Trackers are inconsistent about case between their API and their UI,
    /// and a customer should not have to discover which one their export used.
    #[test]
    fn status_matching_ignores_case() {
        let v = Vocabulary::default();
        for spelling in ["open", "Open", "OPEN", "  open  "] {
            assert_eq!(v.state_of(spelling), Some(IssueState::Open), "{spelling:?}");
        }
    }

    /// The three Stella branches on are all mapped by the defaults, so a
    /// GitHub workspace never hits the passthrough path for one of them.
    #[test]
    fn every_canonical_resolution_has_a_default_spelling() {
        let v = Vocabulary::github();
        for canonical in CANONICAL_RESOLUTIONS {
            assert!(
                v.resolutions.contains_key(*canonical),
                "`{canonical}` has no default spelling"
            );
        }
    }

    #[test]
    fn a_vocabulary_round_trips_through_json() {
        let v = Vocabulary::github();
        let json = serde_json::to_string(&v).expect("serialize");
        let back: Vocabulary = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(v, back);
    }
}
