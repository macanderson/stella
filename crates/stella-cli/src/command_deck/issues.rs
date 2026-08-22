//! ISSUES-tab driver half: the deck's requests, served by the workspace's
//! issue provider.
//!
//! Every function here is generic over [`IssueProvider`] rather than reaching
//! for [`crate::issue_provider::GhIssueProvider`] itself (invariant 1 — the
//! tracker is a port, and GitHub is one adapter behind it). That is also what
//! makes the mapping testable without a network, a `gh` binary, or a
//! repository: the tests below drive a recording fake and assert on the exact
//! arguments the port received.
//!
//! Split from `command_deck.rs` (#629's 1500-line ratchet).

use stella_protocol::issue::{Issue, IssueDraft, IssueKey, IssueLabel, IssueProvider, IssueState};
use stella_tui::{EntityField, Inbound, IssueAction, IssueRow, WorkspaceInput};
use tokio::sync::mpsc::UnboundedSender;

use super::{agent_entity_hits, local_assignee_hits, merge_assignee_hits};
use crate::config::Config;
use crate::issue_provider::GhIssueProvider;

/// The page size the ISSUES tab browses by — one `gh issue list` read.
///
/// The TUI's `ISSUES_PAGE_SIZE` must agree: the tab decides "this was the last
/// page" by seeing fewer rows come back than it asked for, so a driver reading
/// a different number would either hide a page or offer one that is empty.
pub(super) const ISSUES_PAGE: usize = 30;

/// Run one provider call to completion on the calling thread.
///
/// The caller is already inside `spawn_blocking` (`gh` is a subprocess), so
/// this builds the same fresh current-thread runtime the self-driving commands
/// use rather than borrowing a handle on the deck's — blocking a deck worker
/// on a subprocess is what would stall the driver loop.
fn block_on<F: std::future::Future>(future: F) -> Result<F::Output, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start a runtime for the issue provider: {error}"))
        .map(|runtime| runtime.block_on(future))
}

/// One page of the ISSUES tab's browse list.
///
/// `page` is 0-based and comes from the tab, which is the only thing that
/// knows which page the human is looking at; it becomes the port's `offset`
/// here. Ordering is the tracker's, so paging is only as stable as the
/// tracker's own order — the same caveat [`IssueProvider::search`] carries.
pub(super) fn issues_list<P: IssueProvider + ?Sized>(
    provider: &P,
    query: Option<&str>,
    state: Option<String>,
    page: usize,
) -> Result<Vec<IssueRow>, String> {
    let state = match state.as_deref() {
        Some("open") => Some(IssueState::Open),
        Some("closed") => Some(IssueState::Closed),
        _ => None,
    };
    let offset = page.saturating_mul(ISSUES_PAGE);
    let issues = block_on(provider.search(query.unwrap_or(""), state, ISSUES_PAGE, offset))?
        .map_err(|error| error.to_string())?;
    Ok(issues.iter().map(issue_row).collect())
}

/// File one issue through the provider; answer with the created key and a
/// human status line.
pub(super) fn issues_create<P: IssueProvider + ?Sized>(
    provider: &P,
    title: &str,
    body: &str,
    labels: &[String],
    assignee: Option<&str>,
) -> (String, Result<String, String>) {
    let draft = IssueDraft {
        title: title.to_owned(),
        body: body.to_owned(),
        labels: labels
            .iter()
            .map(|l| IssueLabel::from(l.as_str()))
            .collect(),
        parent: None,
        // Empty is the create form's "left blank", which is not the same
        // request as assigning someone named "".
        assignee: assignee
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_owned),
    };
    match block_on(provider.file(&draft)) {
        Err(error) => (String::new(), Err(error)),
        Ok(Ok(key)) => {
            let key = key.as_str().to_owned();
            (key.clone(), Ok(format!("created #{key}")))
        }
        Ok(Err(error)) => (String::new(), Err(error.to_string())),
    }
}

/// Act on one existing issue: comment, close, re-open, or move status.
pub(super) fn issues_act<P: IssueProvider + ?Sized>(
    provider: &P,
    key: &str,
    action: &IssueAction,
) -> Result<String, String> {
    // The row's key is the display spelling (`#874`); the tracker takes the
    // bare one. `issue_row` puts the `#` on, this takes it back off — the two
    // halves of one boundary.
    let key = IssueKey::from(key.trim_start_matches('#'));
    let done = |verb: &str| format!("{verb} #{key}");
    match action {
        IssueAction::Comment(body) => block_on(provider.comment(&key, body))?
            .map(|()| done("comment added to"))
            .map_err(|error| error.to_string()),
        IssueAction::Close => block_on(provider.close(&key, "", "completed"))?
            .map(|()| done("closed"))
            .map_err(|error| error.to_string()),
        IssueAction::Reopen => block_on(provider.reopen(&key))?
            .map(|()| done("re-opened"))
            .map_err(|error| error.to_string()),
        IssueAction::SetStatus(status) => Err(format!(
            "unsupported status `{status}` — GitHub issues are open or closed \
             (`x` closes and re-opens)"
        )),
        IssueAction::StartWork => {
            Err("start work is the self-driving loop's claim, not a tracker status".to_string())
        }
    }
}

/// Map the kernel's [`Issue`] into the tab's row.
///
/// The key gets its `#` here, at the boundary: the provider's key is the
/// tracker's bare identifier (what `gh` and the API take), the row's is the
/// display spelling a human reads and the tab echoes back — and [`issues_act`]
/// strips it again on the way out. Everything between renders `{key}` bare;
/// a `#{key}` in the tab would read `##874`.
fn issue_row(issue: &Issue) -> IssueRow {
    IssueRow {
        key: format!("#{}", issue.key.as_str()),
        title: issue.title.clone(),
        state: match issue.state {
            IssueState::Open => "open",
            IssueState::InProgress => "in progress",
            IssueState::Closed => "closed",
        }
        .to_owned(),
        labels: issue.labels.iter().map(|l| l.name.clone()).collect(),
        // `Issue` carries no assignee — the port does not read one, so the row
        // says so rather than inventing a blank. Filling this in means adding
        // the field to the port first, for every tracker.
        assignee: None,
        url: issue.url.clone(),
        updated_at: None,
    }
}

/// Service one ISSUES-tab request. The tab's tracker is the workspace's
/// configured issue provider (GitHub by default, reached through the `gh`
/// CLI — see [`crate::issue_provider`]); entity search serves the local
/// sources (installed agents, memories, code-graph symbols). Spawned where
/// work is real (the `spawn_mcp_oauth_login` shape) so a slow `gh` call,
/// SQLite read, or grammar load never stalls the driver loop. Returns `true`
/// when the input was one of the tab's.
pub(super) fn handle_issues_input(
    input: &WorkspaceInput,
    cfg: &Config,
    in_tx: &UnboundedSender<Inbound>,
) -> bool {
    match input {
        WorkspaceInput::IssuesRefresh {
            query,
            state,
            page,
            seq,
        } => {
            let (in_tx, seq, page) = (in_tx.clone(), *seq, *page);
            let query = query.clone();
            let state = state.clone();
            tokio::task::spawn_blocking(move || {
                let outcome = issues_list(&GhIssueProvider, query.as_deref(), state, page);
                let _ = in_tx.send(Inbound::IssuesList { seq, outcome });
            });
            true
        }
        WorkspaceInput::IssueCreate {
            title,
            body,
            labels,
            assignee,
            seq,
        } => {
            let (in_tx, seq) = (in_tx.clone(), *seq);
            let (title, body, labels, assignee) = (
                title.clone(),
                body.clone(),
                labels.clone(),
                assignee.clone(),
            );
            tokio::task::spawn_blocking(move || {
                let (key, outcome) = issues_create(
                    &GhIssueProvider,
                    &title,
                    &body,
                    &labels,
                    assignee.as_deref(),
                );
                let _ = in_tx.send(Inbound::IssueActDone { seq, key, outcome });
            });
            true
        }
        WorkspaceInput::IssueAct { key, action, seq } => {
            let (in_tx, seq) = (in_tx.clone(), *seq);
            let (key, action) = (key.clone(), action.clone());
            tokio::task::spawn_blocking(move || {
                let outcome = issues_act(&GhIssueProvider, &key, &action);
                let _ = in_tx.send(Inbound::IssueActDone { seq, key, outcome });
            });
            true
        }
        WorkspaceInput::EntitySearch { field, query, seq } => {
            let (in_tx, seq, field) = (in_tx.clone(), *seq, *field);
            let query = query.clone();
            let root = cfg.workspace_root.clone();
            tokio::spawn(async move {
                let hits = match field {
                    // The tracker has a label vocabulary now (the ISSUES tab
                    // reaches it), but nothing reads it into this popup yet —
                    // it would be a `gh label list` behind a port method the
                    // `IssueProvider` trait does not have. Declared gap, not a
                    // silence: the popup shows "no matches" until #4246 adds
                    // the read.
                    EntityField::Label => Vec::new(),
                    EntityField::Assignee => {
                        // Independent sources — a failure of one must not
                        // kill the others; collect what succeeds.
                        let agents = {
                            let project = crate::agents_installed::project_agents_dir(&root);
                            let user = crate::agents_installed::user_agents_dir();
                            agent_entity_hits(
                                &crate::agents_installed::discover(user.as_deref(), &project),
                                &query,
                            )
                        };
                        let local = {
                            let root = root.clone();
                            let query = query.clone();
                            // SQLite opens + tree-sitter grammar loading are
                            // synchronous — keep them off the async workers.
                            tokio::task::spawn_blocking(move || local_assignee_hits(&root, &query))
                                .await
                                .unwrap_or_default()
                        };
                        merge_assignee_hits(agents, local, 20)
                    }
                };
                let _ = in_tx.send(Inbound::EntityHits {
                    field,
                    seq,
                    query,
                    hits,
                });
            });
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use stella_protocol::issue::{IssueClass, IssueError};

    /// What the fake was asked for, so a test can assert on the exact call the
    /// port received rather than on a value the caller round-tripped.
    #[derive(Default)]
    struct Calls {
        search: Vec<(String, Option<IssueState>, usize, usize)>,
        filed: Vec<IssueDraft>,
        reopened: Vec<String>,
        closed: Vec<String>,
    }

    #[derive(Default)]
    struct FakeTracker {
        calls: Mutex<Calls>,
        rows: Vec<Issue>,
    }

    impl FakeTracker {
        fn calls(&self) -> std::sync::MutexGuard<'_, Calls> {
            self.calls.lock().expect("test mutex")
        }
    }

    fn an_issue(number: u32, state: IssueState) -> Issue {
        Issue {
            key: IssueKey::from(number.to_string().as_str()),
            title: format!("issue {number}"),
            body: String::new(),
            state,
            class: IssueClass::Bug,
            labels: vec![IssueLabel::from("bug")],
            created_at: String::new(),
            url: format!("https://github.com/o/r/issues/{number}"),
            parent: None,
        }
    }

    #[async_trait::async_trait]
    impl IssueProvider for FakeTracker {
        fn id(&self) -> &str {
            "fake"
        }

        async fn list_open(&self, _limit: usize) -> Result<Vec<Issue>, IssueError> {
            Ok(self.rows.clone())
        }

        async fn search(
            &self,
            query: &str,
            state: Option<IssueState>,
            limit: usize,
            offset: usize,
        ) -> Result<Vec<Issue>, IssueError> {
            self.calls()
                .search
                .push((query.to_owned(), state, limit, offset));
            Ok(self.rows.clone())
        }

        async fn file(&self, draft: &IssueDraft) -> Result<IssueKey, IssueError> {
            self.calls().filed.push(draft.clone());
            Ok(IssueKey::from("874"))
        }

        async fn comment(&self, _key: &IssueKey, _body: &str) -> Result<(), IssueError> {
            Ok(())
        }

        async fn close(
            &self,
            key: &IssueKey,
            _evidence: &str,
            _reason: &str,
        ) -> Result<(), IssueError> {
            self.calls().closed.push(key.as_str().to_owned());
            Ok(())
        }

        async fn reopen(&self, key: &IssueKey) -> Result<(), IssueError> {
            self.calls().reopened.push(key.as_str().to_owned());
            Ok(())
        }

        // Not reachable from the ISSUES tab's verbs; the trait requires them.
        async fn relabel(
            &self,
            _key: &IssueKey,
            _add: &[String],
            _remove: &[String],
        ) -> Result<(), IssueError> {
            Ok(())
        }

        async fn edit(
            &self,
            _key: &IssueKey,
            _title: Option<&str>,
            _body: Option<&str>,
        ) -> Result<(), IssueError> {
            Ok(())
        }
    }

    /// The witness for the paging defect: page 2 must reach the port as
    /// `offset = 30`. Before `IssuesRefresh` carried a page the driver passed
    /// a literal `0`, so `]` re-fetched page one under a "page 2" notice.
    #[test]
    fn a_page_becomes_the_ports_offset() {
        let tracker = FakeTracker::default();
        for (page, want_offset) in [(0, 0), (1, 30), (2, 60)] {
            issues_list(&tracker, Some("flaky"), None, page).expect("list");
            let calls = tracker.calls();
            let (query, _, limit, offset) = calls.search.last().expect("a search call");
            assert_eq!(query, "flaky", "the query rides the page request");
            assert_eq!(*limit, ISSUES_PAGE);
            assert_eq!(*offset, want_offset, "page {page}");
        }
    }

    /// A row's key is the display spelling; the tracker gets the bare one.
    #[test]
    fn the_row_key_carries_a_hash_and_the_tracker_never_sees_it() {
        let tracker = FakeTracker {
            rows: vec![an_issue(874, IssueState::Closed)],
            ..FakeTracker::default()
        };
        let rows = issues_list(&tracker, None, None, 0).expect("list");
        assert_eq!(rows[0].key, "#874", "the row is what a human reads");
        assert_eq!(rows[0].state, "closed", "a mixed-state page renders state");

        // Feeding that row's key straight back strips the `#` again.
        issues_act(&tracker, &rows[0].key, &IssueAction::Reopen).expect("reopen");
        assert_eq!(tracker.calls().reopened, ["874"]);
    }

    /// The create form's Assignee field reaches the draft. It used to be
    /// dropped with a `let _ = assignee`, so a filled-in assignee filed an
    /// unassigned issue and said nothing.
    #[test]
    fn the_create_forms_assignee_reaches_the_draft() {
        let tracker = FakeTracker::default();
        let (key, outcome) = issues_create(
            &tracker,
            "a title",
            "a body",
            &["bug".to_string()],
            Some("octocat"),
        );
        assert_eq!(key, "874");
        assert_eq!(outcome.as_deref(), Ok("created #874"));
        let calls = tracker.calls();
        let draft = calls.filed.last().expect("a filed draft");
        assert_eq!(draft.assignee.as_deref(), Some("octocat"));
        assert_eq!(draft.labels, vec![IssueLabel::from("bug")]);
    }

    /// A blank Assignee field is "unassigned", not an empty-named user.
    #[test]
    fn a_blank_assignee_files_unassigned() {
        let tracker = FakeTracker::default();
        for blank in [None, Some(""), Some("   ")] {
            let (_, outcome) = issues_create(&tracker, "t", "b", &[], blank);
            outcome.expect("the fake files successfully");
        }
        let calls = tracker.calls();
        assert!(
            calls.filed.iter().all(|d| d.assignee.is_none()),
            "blank must not become Some(\"\")"
        );
    }

    /// `x` routes both directions to a distinct port call — never a status
    /// string the driver has to compare.
    #[test]
    fn close_and_reopen_reach_their_own_port_calls() {
        let tracker = FakeTracker::default();
        assert_eq!(
            issues_act(&tracker, "#874", &IssueAction::Close).as_deref(),
            Ok("closed #874")
        );
        assert_eq!(
            issues_act(&tracker, "#875", &IssueAction::Reopen).as_deref(),
            Ok("re-opened #875")
        );
        let calls = tracker.calls();
        assert_eq!(calls.closed, ["874"]);
        assert_eq!(calls.reopened, ["875"]);
    }

    /// A status GitHub does not have is refused by name, not silently ignored.
    #[test]
    fn an_unsupported_status_is_refused_by_name() {
        let tracker = FakeTracker::default();
        let err = issues_act(&tracker, "#874", &IssueAction::SetStatus("triaged".into()))
            .expect_err("triaged is not a GitHub state");
        assert!(err.contains("triaged"), "{err}");
        assert!(tracker.calls().reopened.is_empty(), "nothing was called");
    }
}
