//! ISSUES-tab key handling: the browse surface and its selection model.
//!
//! Non-modal, exactly like the MCP tab's Browse mode — the composer stays
//! live, so every letter verb gates on `composer_empty` and never shadows the
//! first character of a prompt. The tab's other modes (the create form, the
//! comment/status prompts, the tracker search line) are modal and stay in
//! `deck_ui.rs` beside the state they drive.
//!
//! **Row keys carry their `#`.** `IssueRow::key` is the *display* spelling —
//! the driver's `issue_row` puts the `#` on at the boundary and strips it
//! again before the key reaches the tracker. So everything here interpolates
//! `{key}` bare; a `#{key}` would render `##874`.
//!
//! Split from `deck_ui.rs` (#629's 1500-line ratchet), same as `mcp_keys`.

use crossterm::event::{KeyCode, KeyEvent};

use super::{DeckAction, DeckUi, IssuesMode, IssuesPanel, submit_prompt};
use crate::deck::WorkspaceModel;
use crate::envelope::{IssueAction, IssueRow, WorkspaceInput};

/// The page size the ISSUES tab browses by — kept in step with the driver's
/// own page read, so a short page is how the tab knows the list is exhausted.
pub(super) const ISSUES_PAGE_SIZE: usize = 30;

/// The notice every verb answers with when it needs a row and has none.
const NO_SELECTION: &str = "no issue selected — r loads the list";

impl IssuesPanel {
    /// The rows the multiselect has picked, in list order. Falls back to the
    /// cursor row when nothing is picked, so every verb that works on "the
    /// selection" also works on a bare cursor.
    pub fn picked_rows(&self) -> Vec<&IssueRow> {
        if self.picked.is_empty() {
            return self.selected().into_iter().collect();
        }
        self.rows
            .iter()
            .filter(|row| self.picked.contains(&row.key))
            .collect()
    }

    /// Toggle the cursor row in the multiselect.
    pub(super) fn toggle_pick(&mut self) {
        if let Some(row) = self.rows.get(self.sel) {
            let key = row.key.clone();
            if !self.picked.remove(&key) {
                self.picked.insert(key);
            }
        }
    }

    /// Drop picks that no longer name a row (a refresh re-fetched the list).
    pub(super) fn prune_picks(&mut self) {
        let keys: std::collections::BTreeSet<&str> =
            self.rows.iter().map(|r| r.key.as_str()).collect();
        self.picked.retain(|k| keys.contains(k.as_str()));
    }
}

/// The ISSUES tab's browse keys (non-modal — the composer stays live, so
/// every letter verb is gated on a blank composer, exactly like the MCP
/// tab): ↑/↓ select · Space multiselect · `r` refresh · `/` tracker search ·
/// `]`/`[` page · `o` open in the browser · `p` push the pick to the prompt
/// and submit · `n` create · `c` comment · `x` close / re-open · `s` set
/// status · `w` start work.
pub(super) fn handle_issues_browse_key(
    key: KeyEvent,
    model: &WorkspaceModel,
    ui: &mut DeckUi,
    composer_empty: bool,
) -> Option<DeckAction> {
    let count = ui.issues.rows.len();
    if super::list_nav::select(key, &mut ui.issues.sel, count, composer_empty) {
        return Some(DeckAction::Handled);
    }
    match key.code {
        KeyCode::Char(' ') if composer_empty => {
            ui.issues.toggle_pick();
            Some(DeckAction::Handled)
        }
        KeyCode::Char('r') if composer_empty => {
            // A refresh is the unfiltered list from the top: it drops both the
            // active query and the page, so `r` is the way back out of a
            // search several pages deep.
            ui.issues.page = 0;
            ui.issues.active_query = None;
            ui.issues.notice = Some("refreshing…".into());
            Some(issues_page_request(ui))
        }
        KeyCode::Char('/') if composer_empty => {
            ui.issues.mode = IssuesMode::SearchTracker;
            ui.issues.search_query.clear();
            Some(DeckAction::Handled)
        }
        KeyCode::Char(']') if composer_empty => {
            // A short last page means there is no next one — the tracker
            // answered with fewer rows than a full page.
            if count < ISSUES_PAGE_SIZE {
                ui.issues.notice = Some("no next page".into());
                return Some(DeckAction::Handled);
            }
            ui.issues.page += 1;
            ui.issues.notice = Some(format!("loading page {}…", ui.issues.page + 1));
            Some(issues_page_request(ui))
        }
        KeyCode::Char('[') if composer_empty => {
            if ui.issues.page == 0 {
                ui.issues.notice = Some("already on the first page".into());
                return Some(DeckAction::Handled);
            }
            ui.issues.page -= 1;
            ui.issues.notice = Some(format!("loading page {}…", ui.issues.page + 1));
            Some(issues_page_request(ui))
        }
        KeyCode::Char('o') if composer_empty => {
            let Some(row) = ui.issues.selected() else {
                ui.issues.notice = Some(NO_SELECTION.into());
                return Some(DeckAction::Handled);
            };
            if row.url.is_empty() {
                ui.issues.notice = Some(format!("{} has no url to open", row.key));
                return Some(DeckAction::Handled);
            }
            open_in_browser(&row.url);
            ui.issues.notice = Some(format!("opened {} in the browser", row.key));
            Some(DeckAction::Handled)
        }
        KeyCode::Char('p') if composer_empty => {
            let rows = ui.issues.picked_rows();
            if rows.is_empty() {
                ui.issues.notice = Some(NO_SELECTION.into());
                return Some(DeckAction::Handled);
            }
            let text = rows
                .iter()
                .map(|row| format!("{} {}", row.key, row.title))
                .collect::<Vec<_>>()
                .join("\n");
            ui.issues.picked.clear();
            Some(submit_prompt(ui, model, text))
        }
        KeyCode::Char('n') if composer_empty => {
            ui.issues.clear_form();
            ui.issues.mode = IssuesMode::Create;
            ui.issues.notice = None;
            Some(DeckAction::Handled)
        }
        KeyCode::Char('c') if composer_empty => {
            if ui.issues.selected().is_some() {
                ui.issues.input.clear();
                ui.issues.mode = IssuesMode::Comment;
            } else {
                ui.issues.notice = Some(NO_SELECTION.into());
            }
            Some(DeckAction::Handled)
        }
        KeyCode::Char('x') if composer_empty => {
            let Some(row) = ui.issues.selected() else {
                ui.issues.notice = Some(NO_SELECTION.into());
                return Some(DeckAction::Handled);
            };
            // One key, both directions: the row's own state says which
            // transition applies, so `x` on an open issue closes it and on a
            // closed one re-opens it.
            let (action, verb) = if row.state.eq_ignore_ascii_case("closed") {
                (IssueAction::Reopen, "re-opening")
            } else {
                (IssueAction::Close, "closing")
            };
            let issue_key = row.key.clone();
            let seq = ui.issues.bump_seq();
            ui.issues.act_wait = seq;
            ui.issues.busy = true;
            ui.issues.notice = Some(format!("{verb} {issue_key}…"));
            Some(DeckAction::Send(WorkspaceInput::IssueAct {
                key: issue_key,
                action,
                seq,
            }))
        }
        KeyCode::Char('s') if composer_empty => {
            if ui.issues.selected().is_some() {
                ui.issues.input.clear();
                ui.issues.mode = IssuesMode::SetStatus;
            } else {
                ui.issues.notice = Some(NO_SELECTION.into());
            }
            Some(DeckAction::Handled)
        }
        KeyCode::Char('w') if composer_empty => {
            let Some(row) = ui.issues.selected() else {
                ui.issues.notice = Some(NO_SELECTION.into());
                return Some(DeckAction::Handled);
            };
            let issue_key = row.key.clone();
            let seq = ui.issues.bump_seq();
            ui.issues.act_wait = seq;
            ui.issues.busy = true;
            ui.issues.notice = Some(format!("starting work on {issue_key}…"));
            Some(DeckAction::Send(WorkspaceInput::IssueAct {
                key: issue_key,
                action: IssueAction::StartWork,
                seq,
            }))
        }
        _ => None,
    }
}

/// Fetch the panel's current page — the one read every browse-list request
/// goes through (`r`, `]`, `[`, and the search line's Enter).
///
/// `page` and the active query both ride the request: the panel is the only
/// thing that knows which page the human is looking at, so a paging key that
/// did not send it would re-fetch page one under a notice claiming otherwise.
/// The caller sets `notice` before calling — the reason differs per key and
/// this does not try to guess it.
pub(super) fn issues_page_request(ui: &mut DeckUi) -> DeckAction {
    let seq = ui.issues.bump_seq();
    ui.issues.list_wait = seq;
    ui.issues.busy = true;
    DeckAction::Send(WorkspaceInput::IssuesRefresh {
        query: ui.issues.active_query.clone(),
        state: None,
        page: ui.issues.page,
        seq,
    })
}

/// Open a url in the system browser, fire-and-forget: the deck does not wait
/// on the browser, and a failure surfaces as the OS's own error rather than
/// a deck notice (there is no useful recovery either way).
fn open_in_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "cmd"
    } else {
        "xdg-open"
    };
    let mut command = std::process::Command::new(opener);
    if cfg!(target_os = "windows") {
        command.args(["/c", "start", "", url]);
    } else {
        command.arg(url);
    }
    let _ = command.spawn();
}
