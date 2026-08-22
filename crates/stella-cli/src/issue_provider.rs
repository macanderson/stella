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
    Issue, IssueClass, IssueDraft, IssueError, IssueKey, IssueLabel, IssueProvider, IssueState,
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
    /// Present only when the query asks for it — `list_open` does not, the
    /// deck's browse/search read does, because a list of mixed-state issues
    /// cannot tell the reader which bucket each row is in without it.
    #[serde(default)]
    state: String,
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
            // returns is open by construction. The mixed-state reads
            // (`search` with no state filter) ask `gh` for the field and
            // map what it says; either way this adapter does not invent a
            // status map — the query already is one, or the payload is.
            state: match self.state.to_ascii_lowercase().as_str() {
                "closed" => IssueState::Closed,
                _ => IssueState::Open,
            },
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

    async fn file(&self, draft: &IssueDraft) -> Result<IssueKey, IssueError> {
        let mut args: Vec<String> = vec![
            "issue".into(),
            "create".into(),
            "--title".into(),
            draft.title.clone(),
            "--body".into(),
            draft.body.clone(),
        ];
        for label in &draft.labels {
            args.push("--label".into());
            args.push(label.name.clone());
        }

        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let raw = gh_json(&borrowed)?;

        // `gh issue create` prints the issue URL, not JSON, so the key is the
        // last path segment. Parsed rather than assumed: a `gh` that printed
        // something else must fail loudly here, because the alternative is a
        // filing the loop cannot reference and therefore cannot close.
        let url = raw.trim();
        let key = url
            .rsplit('/')
            .next()
            .filter(|segment| !segment.is_empty() && segment.chars().all(|c| c.is_ascii_digit()))
            .ok_or_else(|| IssueError::Malformed {
                provider: GITHUB.into(),
                reason: format!("`gh issue create` printed no issue number: {url:?}"),
            })?;

        Ok(IssueKey::from(key))
    }

    async fn close(&self, key: &IssueKey, receipt: &str, state: &str) -> Result<(), IssueError> {
        // Two fixes, both from review of #4001, and they compose:
        //
        // `state` is stella's CANONICAL resolution, and `gh_close_reason` is
        // the only place that knows how GitHub spells it. Spelling it in the
        // caller made every declined closure record as `completed`.
        //
        // And an empty receipt attaches no comment: the partial-closure path
        // posts its receipt as a standalone comment first, for durability, so
        // re-attaching it here would leave the identical signed receipt on the
        // issue twice.
        let reason = gh_close_reason(state);
        let mut args = vec!["issue", "close", key.as_str()];
        if !receipt.trim().is_empty() {
            args.push("--comment");
            args.push(receipt);
        }
        args.push("--reason");
        args.push(reason);
        gh_json(&args).map(|_| ())
    }

    async fn comment(&self, key: &IssueKey, body: &str) -> Result<(), IssueError> {
        gh_json(&["issue", "comment", key.as_str(), "--body", body]).map(|_| ())
    }

    async fn relabel(
        &self,
        key: &IssueKey,
        add: &[String],
        remove: &[String],
    ) -> Result<(), IssueError> {
        if add.is_empty() && remove.is_empty() {
            return Ok(());
        }
        let mut args: Vec<String> = vec!["issue".into(), "edit".into(), key.as_str().to_owned()];
        for label in add {
            args.push("--add-label".into());
            args.push(label.clone());
        }
        for label in remove {
            args.push("--remove-label".into());
            args.push(label.clone());
        }
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        gh_json(&borrowed).map(|_| ())
    }

    async fn edit(
        &self,
        key: &IssueKey,
        title: Option<&str>,
        body: Option<&str>,
    ) -> Result<(), IssueError> {
        if title.is_none() && body.is_none() {
            return Ok(());
        }
        let mut args: Vec<String> = vec!["issue".into(), "edit".into(), key.as_str().to_owned()];
        if let Some(title) = title {
            args.push("--title".into());
            args.push(title.to_owned());
        }
        if let Some(body) = body {
            args.push("--body".into());
            args.push(body.to_owned());
        }
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        gh_json(&borrowed).map(|_| ())
    }

    async fn reopen(&self, key: &IssueKey) -> Result<(), IssueError> {
        gh_json(&["issue", "reopen", key.as_str()]).map(|_| ())
    }

    async fn search(
        &self,
        query: &str,
        state: Option<IssueState>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Issue>, IssueError> {
        // `gh issue list` has no server-side offset, so one call fetches
        // offset+limit rows and this slices the page out. That is the cheap
        // correct thing at tracker scale: the alternative (one call per page,
        // each re-fetching the prefix) costs the same bandwidth and adds a
        // failure mode per extra call.
        let fetch = (offset + limit).to_string();
        let state_arg = match state {
            Some(IssueState::Open) | Some(IssueState::InProgress) => "open",
            Some(IssueState::Closed) => "closed",
            None => "all",
        };
        // GitHub has no in-progress state (doc:agent-native-delivery §4.2),
        // so `InProgress` reads as open — the honest approximation, and the
        // payload's own `state` field is what the caller renders.
        let mut args: Vec<String> = vec![
            "issue".into(),
            "list".into(),
            "--state".into(),
            state_arg.into(),
            "--limit".into(),
            fetch,
            "--json".into(),
            "number,title,body,labels,state,createdAt,url".into(),
        ];
        if !query.trim().is_empty() {
            args.push("--search".into());
            args.push(query.trim().to_owned());
        }
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let raw = gh_json(&borrowed)?;
        let rows: Vec<GhIssue> =
            serde_json::from_str(&raw).map_err(|error| IssueError::Malformed {
                provider: GITHUB.into(),
                reason: error.to_string(),
            })?;
        Ok(rows
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(GhIssue::into_issue)
            .collect())
    }
}

/// Create a label if the repository does not already carry one by that name.
///
/// Called once at the start of a driving session. A label that does not exist
/// cannot be applied, so without this the loop would fail at its **first**
/// escalation — the moment it most needs to record something — rather than at
/// setup, where a failure is cheap and obvious.
///
/// Already-exists is success, not an error: two sessions starting at once must
/// not race each other into a failure, and `gh` reports the collision on
/// stderr rather than distinguishing it in an exit code worth branching on.
pub(crate) fn ensure_label(name: &str, description: &str) -> Result<(), IssueError> {
    match gh_json(&[
        "label",
        "create",
        name,
        "--description",
        description,
        "--color",
        "B60205",
    ]) {
        Ok(_) => Ok(()),
        Err(IssueError::Failed { reason, .. }) if reason.contains("already exists") => Ok(()),
        Err(other) => Err(other),
    }
}

/// Map stella's canonical resolution onto the reason `gh issue close` accepts.
///
/// **`close` receives the CANONICAL resolution** — `"completed"`,
/// `"not_planned"`, `"duplicate"` — never a tracker's own spelling. This
/// adapter is the only place in the tree that carries GitHub's words, exactly
/// as the module docs claim, and a caller that spelled the value first would
/// leave this function comparing against something it does not recognise.
///
/// That is not hypothetical: it was the bug. The caller mapped through the
/// configured vocabulary and passed `"not planned"`, this compared it to
/// `"not_planned"`, the match never fired, and **every declined and duplicate
/// closure was recorded as `completed`** — declined work labelled as done,
/// which is the "looks productive" failure `stella_autonomy::closure` exists to
/// prevent. Caught in review on #4001.
///
/// `duplicate` maps to `not planned` because `gh issue close --reason` accepts
/// only two values, and of the two that is the truthful one: a duplicate is not
/// work that was done. The receipt says which it was either way.
///
/// Anything unrecognised closes as `completed` rather than failing — an
/// unclosed issue is worse than one closed under a slightly wrong reason.
fn gh_close_reason(canonical: &str) -> &'static str {
    match canonical {
        "not_planned" | "duplicate" => "not planned",
        _ => "completed",
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
    /// **The regression witness.** `close` receives stella's CANONICAL
    /// resolution, and every one that is not work-that-was-done must reach
    /// GitHub as `not planned`.
    ///
    /// The bug this pins: the caller used to spell the value through the
    /// configured vocabulary first, so this comparison saw `"not planned"`,
    /// never matched `"not_planned"`, and **every declined and duplicate
    /// closure was recorded as `completed`** — declined work labelled as done,
    /// which is the "looks productive" failure `stella_autonomy::closure`
    /// exists to prevent. Caught in review on #4001.
    #[test]
    fn a_declined_closure_never_reaches_github_as_completed() {
        assert_eq!(gh_close_reason("not_planned"), "not planned");
        assert_eq!(gh_close_reason("duplicate"), "not planned");
        assert_eq!(gh_close_reason("completed"), "completed");
    }

    /// The canonical vocabulary and this mapping cannot drift apart silently:
    /// every resolution stella branches on has an answer here, and only
    /// `completed` is allowed to mean work was done.
    #[test]
    fn every_canonical_resolution_maps_and_only_completed_means_done() {
        for canonical in stella_protocol::issue::CANONICAL_RESOLUTIONS {
            let reason = gh_close_reason(canonical);
            assert!(
                reason == "completed" || reason == "not planned",
                "`{canonical}` mapped to `{reason}`, which `gh issue close` does not accept"
            );
            assert_eq!(
                reason == "completed",
                *canonical == "completed",
                "only `completed` may record as work that was done, but `{canonical}` did"
            );
        }
    }

    /// An unrecognised value still closes: an unclosed issue is worse than one
    /// closed under a slightly wrong reason, and the receipt says what actually
    /// happened either way.
    #[test]
    fn an_unrecognised_resolution_still_closes() {
        assert_eq!(gh_close_reason("cannot_reproduce"), "completed");
    }

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
