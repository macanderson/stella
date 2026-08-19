//! The GitHub adapter behind [`IssueProvider`], and the one mapping from a
//! tracker's shape into the ranker's.
//!
//! `doc:backlog-self-driving` B1. Until this module existed, `gh` was invoked
//! literally inside `self_driving_cmd::queue` and the ranker's input type
//! carried `#[serde(rename = "createdAt")]` — a `gh --json` field name, in a
//! leaf crate that depends on nothing. "Which tracker" was therefore not a
//! decision anything could express; it was spelled into the reader.
//!
//! What moved here is the I/O half only. The port and the kernel are
//! [`stella_protocol::issue`]; the ranking stays in `stella_autonomy`, on its
//! own input type, because that crate depends on no other workspace crate and
//! that property is what lets the Observatory link it.
//!
//! # `gh`, not the REST API — for now, and for a reason
//!
//! The adapter shells out to `gh` rather than speaking HTTP. That is what the
//! loop already did, so this slice is a relocation rather than a rewrite, and
//! it keeps the one property a token-holding client would have to re-earn:
//! **Stella never holds a GitHub credential.** `gh` owns the auth, the token
//! never enters this process, and `stella auth` has nothing new to store.
//! `doc:agent-native-delivery` §4.4's `kind = "exec"` is the general form of
//! exactly this shape, and a manifest-driven provider is the next slice.

use std::process::Command;

use async_trait::async_trait;
use stella_protocol::issue::{
    Issue, IssueClass, IssueError, IssueKey, IssueLabel, IssueProvider, IssueState,
};

/// The provider id this adapter answers to, and the one an error names.
pub(crate) const GITHUB: &str = "github";

/// Reads the workspace's GitHub issues through the `gh` CLI.
pub(crate) struct GhIssueProvider;

/// What `gh issue list --json …` writes, named separately from [`Issue`] on
/// purpose.
///
/// This is the **only** type in the tree allowed to carry GitHub's field
/// spellings, and it is private to this module. Before B1 the same
/// `createdAt` rename lived on the ranker's public input type in a leaf crate,
/// which is how a tracker's wire format became the loop's vocabulary.
#[derive(serde::Deserialize)]
struct GhIssue {
    number: u64,
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    labels: Vec<GhLabel>,
    #[serde(rename = "createdAt", default)]
    created_at: String,
    #[serde(default)]
    url: String,
}

#[derive(serde::Deserialize)]
struct GhLabel {
    name: String,
}

impl GhIssue {
    /// GitHub has no issue *type* on a plain repository, so the class is read
    /// off the labels this repository actually uses.
    ///
    /// [`IssueClass::Other`] is the honest answer for an issue labelled
    /// neither, and it is deliberately not `Task`: a wrong class silently
    /// applies the wrong completion policy, which is worse than a visible
    /// unmapped one (`doc:agent-native-delivery` §12.6 leaves whether `Other`
    /// is workable open, and this adapter does not pre-empt it).
    fn class(&self) -> IssueClass {
        let has = |name: &str| self.labels.iter().any(|label| label.name == name);
        if has("bug") {
            IssueClass::Bug
        } else if has("feature") {
            IssueClass::Feature
        } else if has("chore") || has("task") || has("refactor") {
            IssueClass::Task
        } else {
            IssueClass::Other
        }
    }

    fn into_issue(self) -> Issue {
        let class = self.class();
        Issue {
            key: IssueKey(self.number.to_string()),
            title: self.title,
            body: self.body,
            // `list_open` asks `gh` for open issues only, so every row it
            // returns is open by construction. This adapter does not invent a
            // status map because the query already is one.
            state: IssueState::Open,
            class,
            labels: self
                .labels
                .into_iter()
                .map(|label| IssueLabel { name: label.name })
                .collect(),
            created_at: self.created_at,
            url: self.url,
            // GitHub's parent edge needs a second (GraphQL) call per issue,
            // which the queue read does not need and should not pay for.
            // Absent here is "not fetched", and the write half fetches it when
            // something reads it (#3599).
            parent: None,
        }
    }
}

#[async_trait]
impl IssueProvider for GhIssueProvider {
    fn id(&self) -> &str {
        GITHUB
    }

    async fn list_open(&self, limit: usize) -> Result<Vec<Issue>, IssueError> {
        let limit = limit.to_string();
        let raw = gh_json(&[
            "issue",
            "list",
            "--state",
            "open",
            "--limit",
            &limit,
            "--json",
            "number,title,body,labels,createdAt,url",
        ])?;
        let rows: Vec<GhIssue> =
            serde_json::from_str(&raw).map_err(|error| IssueError::Malformed {
                provider: GITHUB.into(),
                reason: error.to_string(),
            })?;
        Ok(rows.into_iter().map(GhIssue::into_issue).collect())
    }
}

/// Run a `gh` subcommand whose stdout is parsed, with colour forced off.
///
/// The colour handling is not cosmetic and is inherited from the shell driver:
/// ANSI escapes inside a JSON payload are invisible in a terminal and fatal to
/// every parser, so a `gh` call whose output is read goes through here and one
/// whose output a human reads does not.
fn gh_json(args: &[&str]) -> Result<String, IssueError> {
    let output = Command::new("gh")
        .args(args)
        .env("NO_COLOR", "1")
        .env("CLICOLOR_FORCE", "0")
        .output()
        .map_err(|error| IssueError::Unavailable {
            provider: GITHUB.into(),
            reason: format!("could not run `gh`: {error} — is the GitHub CLI installed?"),
        })?;

    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    // The two failures a caller must tell apart: installing a tool and logging
    // into it are different instructions, and the loop can act on neither
    // itself — so it must at least say which one a human needs to do.
    let reason = stderr.trim().to_owned();
    if stderr.contains("gh auth login") || stderr.contains("authentication") {
        Err(IssueError::Unauthenticated {
            provider: GITHUB.into(),
            reason,
        })
    } else {
        Err(IssueError::Failed {
            provider: GITHUB.into(),
            reason,
        })
    }
}

/// Map the kernel's [`Issue`] into the ranker's input.
///
/// The one place a tracker's shape meets the loop's, and it is a function
/// rather than a trait impl because neither type's crate may depend on the
/// other's: `stella-autonomy` takes no workspace dependency at all, and
/// `stella-protocol` holds no logic. `stella-cli` is where both are already
/// linked, so the mapping lands here and nowhere else.
///
/// `number` is parsed from the key and falls back to `0`. That is lossy for a
/// non-numeric tracker (`STELLA-42`), and deliberately so at this slice: the
/// ranker's field is a `u64` today, the loss shows up only in the display
/// column, and widening a leaf crate's public type is a change that should be
/// made when a non-numeric provider actually exists rather than in advance.
/// Tracked in #3599 with B1's second slice.
pub(crate) fn to_queue_issue(issue: &Issue) -> stella_autonomy::QueueIssue {
    stella_autonomy::QueueIssue {
        number: issue.key.as_str().parse().unwrap_or(0),
        title: issue.title.clone(),
        labels: issue
            .labels
            .iter()
            .map(|label| stella_autonomy::IssueLabel {
                name: label.name.clone(),
            })
            .collect(),
        created_at: issue.created_at.clone(),
        url: issue.url.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gh_payload() -> &'static str {
        r#"[
          {"number":1234,"title":"retry counter survives a round boundary",
           "body":"keyed per turn","labels":[{"name":"bug"},{"name":"P1"}],
           "createdAt":"2026-08-19T05:00:00Z","url":"https://example.test/1234"},
          {"number":7,"title":"add a mistral adapter","body":"",
           "labels":[{"name":"feature"}],"createdAt":"2026-08-01T00:00:00Z",
           "url":"https://example.test/7"},
          {"number":9,"title":"unlabelled","body":"","labels":[],
           "createdAt":"2026-08-02T00:00:00Z","url":"https://example.test/9"}
        ]"#
    }

    /// The GitHub wire shape decodes, and every field lands where the kernel
    /// says it does.
    #[test]
    fn the_gh_payload_maps_onto_the_kernel() {
        let rows: Vec<GhIssue> = serde_json::from_str(gh_payload()).expect("decode");
        let issues: Vec<Issue> = rows.into_iter().map(GhIssue::into_issue).collect();

        assert_eq!(issues[0].key, IssueKey::from("1234"));
        assert_eq!(issues[0].class, IssueClass::Bug);
        assert_eq!(issues[0].state, IssueState::Open);
        assert_eq!(issues[0].created_at, "2026-08-19T05:00:00Z");
        assert_eq!(issues[1].class, IssueClass::Feature);
        // Unlabelled is `Other`, visibly — never silently `Task`.
        assert_eq!(issues[2].class, IssueClass::Other);
    }

    /// The mapping into the ranker keeps exactly what ranking reads: the
    /// labels it sorts on and the stamp it breaks ties with.
    #[test]
    fn the_ranker_mapping_keeps_what_ranking_reads() {
        let rows: Vec<GhIssue> = serde_json::from_str(gh_payload()).expect("decode");
        let issues: Vec<Issue> = rows.into_iter().map(GhIssue::into_issue).collect();
        let mapped = to_queue_issue(&issues[0]);

        assert_eq!(mapped.number, 1234);
        assert_eq!(mapped.created_at, "2026-08-19T05:00:00Z");
        assert!(mapped.labels.iter().any(|label| label.name == "P1"));
    }
}
