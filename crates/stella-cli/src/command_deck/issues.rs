//! ISSUES-tab driver half: the deck's requests, served by the workspace's
//! issue provider.
//!
//! Every function here is generic over `IssueProvider` rather than reaching
//! for `crate::issue_provider::GhIssueProvider` itself (invariant 1 — the
//! tracker is a port, and GitHub is one adapter behind it). That is also what
//! makes the mapping testable without a network, a `gh` binary, or a
//! repository: the tests below drive a recording fake and assert on the exact
//! arguments the port received.
//!
//! Split from `command_deck.rs` (#629's 1500-line ratchet).

use stella_protocol::issue::{Issue, IssueDraft, IssueKey, IssueLabel, IssueProvider, IssueState};
use stella_tui::{EntityField, EntityHit, Inbound, IssueAction, IssueRow, WorkspaceInput};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::Config;
use crate::issue_provider::GhIssueProvider;

/// The page size the ISSUES tab browses by — one `gh issue list` read.
///
/// The TUI's `ISSUES_PAGE_SIZE` must agree: the tab decides "this was the last
/// page" by seeing fewer rows come back than it asked for, so a driver reading
/// a different number would either hide a page or offer one that is empty.
pub(super) const ISSUES_PAGE: usize = 30;

/// How many suggestions one create-form type-ahead popup offers.
///
/// One number for both fields on purpose: the popup is the same widget in the
/// same box either way, so a cap that differed by field would make the list
/// change height as the human tabbed between them.
const TYPEAHEAD_HITS: usize = 20;

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

/// The tracker's own label vocabulary, as create-form type-ahead hits.
///
/// The query goes to the port rather than being matched here: which labels
/// exist, and which of them a prefix means, are the tracker's answers to give
/// (`IssueProvider::labels`). Nothing in this file knows that GitHub is the
/// tracker — invariant 1, which is also why `gh label list` is not spelled
/// here.
///
/// A failed read is an empty popup, not an error the human has to dismiss.
/// The suggestion is the optional half of the field: a tracker that cannot
/// enumerate its labels leaves them typing one by hand, which is exactly what
/// they did before this popup could answer at all.
pub(super) fn label_hits<P: IssueProvider + ?Sized>(
    provider: &P,
    query: &str,
    limit: usize,
) -> Vec<EntityHit> {
    let Ok(Ok(labels)) = block_on(provider.labels(query, limit)) else {
        return Vec::new();
    };
    labels
        .into_iter()
        .map(|label| EntityHit {
            kind: "Label".to_owned(),
            // The port carries a label's name and nothing else, so the popup
            // shows the name and says nothing it cannot know. A tracker's
            // label description would have to cross the port first — see
            // `IssueLabel`'s own docs, which reserve the field for exactly
            // that.
            description: String::new(),
            insert: label.name.clone(),
            label: label.name,
        })
        .collect()
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
                    EntityField::Label => {
                        let query = query.clone();
                        // `gh` is a subprocess — the same reason every other
                        // tracker read on this tab is spawned blocking.
                        tokio::task::spawn_blocking(move || {
                            label_hits(&GhIssueProvider, &query, TYPEAHEAD_HITS)
                        })
                        .await
                        .unwrap_or_default()
                    }
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
                        merge_assignee_hits(agents, local, TYPEAHEAD_HITS)
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

// ── Entity-hit assemblers (the ISSUES-tab create form's type-ahead) ────────

/// Installed agents whose name or description contains `query`
/// (case-insensitive; an empty query matches all) as "Agent" hits.
fn agent_entity_hits(entries: &[stella_tui::InstalledAgentEntry], query: &str) -> Vec<EntityHit> {
    let needle = query.trim().to_lowercase();
    entries
        .iter()
        .filter(|e| {
            needle.is_empty()
                || e.name.to_lowercase().contains(&needle)
                || e.description.to_lowercase().contains(&needle)
        })
        .map(|e| EntityHit {
            kind: "Agent".to_string(),
            label: e.name.clone(),
            description: e.description.clone(),
            insert: e.name.clone(),
        })
        .collect()
}

/// Cap on the content preview a memory hit carries.
const MEMORY_PREVIEW_CHARS: usize = 60;

/// One memory node as a type-ahead hit: a flattened content preview plus a
/// provenance suffix (`· observed …`) and, when the memory has been cited, its
/// citation stats.
///
/// Observation time is the only time a node has. It used to be followed by a
/// `· valid from …` clause reading `NodeRow::valid_from`, which no node writer
/// ever fills — so the clause restated the observation timestamp on every row
/// it has ever rendered (#3136).
fn memory_hit(
    display_name: &str,
    content: &str,
    recorded_at: &str,
    citations: Option<(i64, f64)>,
) -> EntityHit {
    let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let preview: String = if flat.chars().count() > MEMORY_PREVIEW_CHARS {
        let head: String = flat.chars().take(MEMORY_PREVIEW_CHARS - 1).collect();
        format!("{head}…")
    } else {
        flat
    };
    let mut description = format!("{preview} · observed {recorded_at}");
    if let Some((count, avg)) = citations {
        description.push_str(&format!(" · cited {count}× avg {avg:.1}"));
    }
    EntityHit {
        kind: "Memory".to_string(),
        label: display_name.to_string(),
        description,
        insert: display_name.to_string(),
    }
}

/// One code-graph definition frame as a type-ahead hit: the kind is the
/// frame kind capitalized ("Symbol"), the label its human title (`fn foo`),
/// the description its file location (the citation's parenthetical, else
/// the frame uri), and the inserted text the bare symbol name — the title's
/// last token.
fn symbol_hit(frame: &contextgraph_types::ContextFrame) -> EntityHit {
    let label = frame.title.clone();
    let insert = label
        .split_whitespace()
        .last()
        .unwrap_or(label.as_str())
        .to_string();
    let description = frame
        .citation_label
        .as_deref()
        .and_then(|citation| {
            let start = citation.rfind('(')?;
            let end = citation.rfind(')')?;
            (start + 1 < end).then(|| citation[start + 1..end].to_string())
        })
        .or_else(|| frame.uri.clone())
        .unwrap_or_default();
    EntityHit {
        kind: format!("{:?}", frame.kind),
        label,
        description,
        insert,
    }
}

/// The local (non-tracker) assignee sources, read synchronously (call on
/// the blocking pool): memories from `.stella/private/context.db` — with citation
/// stats joined from `store.db` by `public_id` — and code-graph symbol
/// definitions when an index exists. Read-only politeness (the `stella
/// stats` discipline): a missing database reads as "no hits", never a
/// write. Failures of one source never kill another.
fn local_assignee_hits(root: &std::path::Path, query: &str) -> Vec<EntityHit> {
    let needle = query.trim().to_lowercase();
    let mut hits = Vec::new();

    // Memories: substring over display_name/content; empty query lists all.
    let context_db = stella_store::existing_workspace_private_sqlite_path(root, "context.db")
        .ok()
        .flatten();
    if let Some(context_db) = context_db
        && let Ok(context) = stella_context::ContextStore::open(&context_db)
        && let Ok(nodes) = context.memory_nodes()
    {
        let stats: std::collections::HashMap<String, (i64, f64)> = {
            if stella_store::existing_workspace_private_sqlite_path(root, "store.db")
                .ok()
                .flatten()
                .is_some()
            {
                stella_store::Store::open(root)
                    .and_then(|store| store.memory_citation_stats())
                    .map(|rows| {
                        rows.into_iter()
                            .map(|s| (s.memory_id, (s.citations, s.avg_score)))
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                Default::default()
            }
        };
        hits.extend(
            nodes
                .iter()
                .filter(|n| {
                    needle.is_empty()
                        || n.display_name.to_lowercase().contains(&needle)
                        || n.content.to_lowercase().contains(&needle)
                })
                .take(20)
                .map(|n| {
                    memory_hit(
                        &n.display_name,
                        &n.content,
                        &n.recorded_at,
                        stats.get(&n.public_id).copied(),
                    )
                }),
        );
    }

    // Code-graph definitions of the queried name, when an index exists
    // (definitions are an exact-name lookup, so an empty query has nothing
    // to resolve).
    if !needle.is_empty()
        && let Ok(Some(db)) = crate::search_cmd::codegraph::graph_db_path(root)
        && let Ok(graph) = stella_graph::CodeGraph::open(root, &db)
        && let Ok(frames) = graph.definitions(query.trim())
    {
        hits.extend(frames.iter().map(symbol_hit));
    }
    hits
}

/// Merge the assignee sources in priority order — installed agents first,
/// then local memories/symbols — capped at `cap`.
fn merge_assignee_hits(
    agents: Vec<EntityHit>,
    local: Vec<EntityHit>,
    cap: usize,
) -> Vec<EntityHit> {
    let mut merged = agents;
    merged.extend(local);
    merged.truncate(cap);
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use stella_protocol::issue::{IssueClass, IssueError};
    use stella_tui::AgentScope;

    #[test]
    fn agent_entity_hits_filter_by_name_or_description_case_insensitively() {
        let entries = vec![
            stella_tui::InstalledAgentEntry {
                name: "reviewer".into(),
                description: "Reviews diffs".into(),
                tools: None,
                scope: AgentScope::Project,
                source_path: String::new(),
                version: 1,
                versions: vec![],
                content: String::new(),
            },
            stella_tui::InstalledAgentEntry {
                name: "planner".into(),
                description: "Plans work".into(),
                tools: None,
                scope: AgentScope::User,
                source_path: String::new(),
                version: 1,
                versions: vec![],
                content: String::new(),
            },
        ];
        let hits = agent_entity_hits(&entries, "REVIEW");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, "Agent");
        assert_eq!(hits[0].insert, "reviewer");
        // Description text matches too; the empty query matches all.
        assert_eq!(agent_entity_hits(&entries, "plans")[0].label, "planner");
        assert_eq!(agent_entity_hits(&entries, "").len(), 2);
    }

    #[test]
    fn memory_hits_carry_the_preview_provenance_and_citation_suffixes() {
        let hit = memory_hit(
            "naming-convention",
            "Prefer kebab-case for  skill names\nand slugs.",
            "2026-07-01T00:00:00Z",
            Some((12, 0.9)),
        );
        assert_eq!(hit.kind, "Memory");
        assert_eq!(hit.insert, "naming-convention");
        assert_eq!(
            hit.description,
            "Prefer kebab-case for skill names and slugs. · observed \
             2026-07-01T00:00:00Z · cited 12× avg 0.9"
        );
        // Observation time is the only time a node carries: no `valid from`
        // clause restating it (#3136).
        assert!(!hit.description.contains("valid from"));

        // No citations → no suffix; a long content truncates char-safe with an
        // ellipsis.
        let long = "x".repeat(200);
        let hit = memory_hit("m", &long, "2026-07-01", None);
        assert!(
            hit.description
                .starts_with(&"x".repeat(MEMORY_PREVIEW_CHARS - 1))
        );
        assert!(hit.description.ends_with("… · observed 2026-07-01"));
        assert!(!hit.description.contains("cited"));
    }

    #[test]
    fn symbol_hits_take_the_bare_name_and_the_file_location() {
        let frame = contextgraph_types::ContextFrame {
            id: "code-graph:sym:src/lib.rs:12:issue_row".into(),
            kind: contextgraph_types::FrameKind::Symbol,
            title: "fn issue_row".into(),
            content: Some("fn issue_row(...) { ... }".into()),
            uri: Some("file:///repo/src/lib.rs".into()),
            score: 0.9,
            token_cost: 10,
            content_digest: None,
            representation: contextgraph_types::Representation::Full,
            content_fidelity: None,
            canonical_content_hash: None,
            content_ref: None,
            transform: None,
            minimum_content_fidelity: None,
            inline_content_requirement: None,
            canonical_token_cost: None,
            tokenizer_ref: None,

            valid_from: None,
            valid_to: None,
            recorded_at: None,
            provenance: vec![],
            citation_label: Some("fn issue_row (src/lib.rs:12)".into()),
            embedding: None,
            relations: vec![],
        };
        let hit = symbol_hit(&frame);
        assert_eq!(hit.kind, "Symbol");
        assert_eq!(hit.label, "fn issue_row");
        assert_eq!(hit.insert, "issue_row", "the bare name is what inserts");
        assert_eq!(hit.description, "src/lib.rs:12");

        // Without a citation label the frame's uri stands in.
        let mut bare = frame;
        bare.citation_label = None;
        assert_eq!(symbol_hit(&bare).description, "file:///repo/src/lib.rs");
    }

    #[test]
    fn merge_assignee_hits_orders_agents_then_local_and_caps() {
        let person = |l: &str| EntityHit {
            kind: "Person".into(),
            label: l.into(),
            description: String::new(),
            insert: l.into(),
        };
        let agents: Vec<EntityHit> = (0..2).map(|i| person(&format!("a{i}"))).collect();
        let local: Vec<EntityHit> = (0..3).map(|i| person(&format!("m{i}"))).collect();
        let merged = merge_assignee_hits(agents, local, 4);
        let labels: Vec<&str> = merged.iter().map(|h| h.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["a0", "a1", "m0", "m1"],
            "agents first, then local — capped"
        );
    }

    #[test]
    fn local_assignee_hits_read_as_empty_on_a_bare_workspace() {
        // Read-only politeness: no `.stella/` databases → no hits and, above
        // all, no directories/files created as a side effect.
        let dir = tempfile::tempdir().unwrap();
        assert!(local_assignee_hits(dir.path(), "anything").is_empty());
        assert!(
            !dir.path().join(".stella").exists(),
            "a lookup must never create the workspace store"
        );
    }

    /// What the fake was asked for, so a test can assert on the exact call the
    /// port received rather than on a value the caller round-tripped.
    #[derive(Default)]
    struct Calls {
        search: Vec<(String, Option<IssueState>, usize, usize)>,
        filed: Vec<IssueDraft>,
        reopened: Vec<String>,
        closed: Vec<String>,
        labels: Vec<(String, usize)>,
    }

    #[derive(Default)]
    struct FakeTracker {
        calls: Mutex<Calls>,
        rows: Vec<Issue>,
        /// What this tracker answers a label-vocabulary read with.
        vocabulary: Vec<IssueLabel>,
        /// When set, the vocabulary read fails the way an uninstalled `gh`
        /// does — so a test can pin what the popup does with a failed read.
        vocabulary_fails: bool,
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

        async fn labels(&self, query: &str, limit: usize) -> Result<Vec<IssueLabel>, IssueError> {
            self.calls().labels.push((query.to_owned(), limit));
            if self.vocabulary_fails {
                return Err(IssueError::Unavailable {
                    provider: "fake".into(),
                    reason: "no `gh` on PATH".into(),
                });
            }
            Ok(self.vocabulary.clone())
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

    /// **The witness for #4251.** The create form's Labels type-ahead serves
    /// the tracker's own vocabulary through the port. The arm used to answer
    /// `Vec::new()` unconditionally, so the popup could never show a match no
    /// matter what was typed.
    #[test]
    fn the_label_typeahead_serves_the_trackers_vocabulary() {
        let tracker = FakeTracker {
            vocabulary: vec![IssueLabel::from("area:tui"), IssueLabel::from("area:core")],
            ..FakeTracker::default()
        };
        let hits = label_hits(&tracker, "area", TYPEAHEAD_HITS);

        assert_eq!(
            tracker.calls().labels,
            [("area".to_string(), TYPEAHEAD_HITS)],
            "the typed prefix reaches the tracker verbatim — the vocabulary is \
             the tracker's, not a list filtered here"
        );
        assert_eq!(hits.len(), 2, "both labels reach the popup");
        assert!(
            hits.iter().all(|hit| hit.kind == "Label"),
            "the popup groups by kind: {hits:?}"
        );
        assert_eq!(hits[0].label, "area:tui");
        assert_eq!(
            hits[0].insert, "area:tui",
            "picking a hit inserts the label's own spelling"
        );
    }

    /// A tracker that cannot answer leaves the popup empty rather than failing
    /// the form the human was filling in — the suggestion is optional, the
    /// label they typed by hand is not.
    #[test]
    fn a_failed_vocabulary_read_is_an_empty_popup() {
        let tracker = FakeTracker {
            vocabulary_fails: true,
            ..FakeTracker::default()
        };
        assert!(label_hits(&tracker, "area", TYPEAHEAD_HITS).is_empty());
        assert_eq!(tracker.calls().labels.len(), 1, "it did ask");
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
