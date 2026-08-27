//! The draft plan the ISSUES tab shows before anything runs (SPEC 8.2).
//!
//! `w` on an issue asks the driver for a **draft**: a plan derived from the
//! issue's own text, the files the code graph couples to it, and the memory
//! RULEs that apply. The overlay renders that draft, and the plan stays a
//! draft until the human presses `a`. Nothing here can start work — this
//! module holds data and a cursor, and the only key that emits an approval is
//! `deck_ui`'s `handle_start_work_key`.
//!
//! # Why the draft carries its own sources
//!
//! SPEC 8.2's sources line names *exactly what was used*, which only the
//! producer knows. A renderer that re-derived the list would be describing its
//! own guess rather than the plan's inputs, and the two would diverge the
//! first time the driver learned a new source. So the driver ships the list it
//! actually read and the overlay prints it.
//!
//! # Why a contract is optional
//!
//! A read-only task changes no file, so no diff can settle it and it declares
//! `read only · no contract` rather than a check nothing can run. Everything
//! else carries one `done means` line naming the mechanism that settles it —
//! and the mechanism is what defines the `det` tag (SPEC §1): a check either
//! reaches a model or it does not. There is no `det %` here, on the estimate
//! line or anywhere else.

use std::collections::BTreeSet;

/// One memory RULE the draft applied, with the text that applied.
///
/// The text rides along because the sources line quotes it: a rule id alone
/// tells a reader that *something* steered the plan without telling them what.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DraftRule {
    pub id: String,
    pub text: String,
}

/// Everything the draft was built from, as the producer read it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DraftSources {
    /// Root-relative paths the code graph couples to the issue.
    pub coupled_files: Vec<String>,
    /// Memory RULEs that applied, in the order the rules engine resolved them.
    pub rules: Vec<DraftRule>,
}

/// What settles one diff-producing task.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DraftContract {
    /// The `done means:` line, as one sentence.
    pub done_means: String,
    /// The check that settles it (`unit`, `gate`, `graph`, `build`).
    pub mechanism: String,
    /// Whether that mechanism reaches a model. A boolean, never a ratio.
    pub deterministic: bool,
}

/// One drafted task.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DraftTask {
    pub subject: String,
    /// `None` for a read-only task — see the module docs.
    pub contract: Option<DraftContract>,
}

/// The estimate line's three terms, all derived from measured numbers.
///
/// `tokens` is the drafted plan's input floor: the bytes of the issue and of
/// every coupled file, counted once per task, through the same byte heuristic
/// the engine budgets with. `usd` and `minutes` are that figure priced and
/// timed by **this workspace's own recorded calls** for the session's model —
/// so an estimate exists only where a measurement does, and the overlay says
/// so rather than inventing one where it does not.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DraftEstimate {
    pub usd: f64,
    pub tokens: u64,
    pub minutes: u64,
}

/// A drafted plan awaiting approval.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StartWorkDraft {
    /// The issue's display key (`#151`), as the browse row spells it.
    pub issue_key: String,
    pub issue_title: String,
    pub sources: DraftSources,
    pub tasks: Vec<DraftTask>,
    /// How many gates the final `verify` task blocks the merge on.
    pub gates: usize,
    /// `None` when this workspace has recorded no calls to price against.
    pub estimate: Option<DraftEstimate>,
}

/// The ISSUES tab's start-work overlay state.
///
/// Open exactly while [`crate::deck_ui::IssuesMode::StartWork`] is the mode.
/// The panel holds the request in flight, the draft that answered it, and the
/// human's edits to it — never a branch, a claim, or anything that runs.
#[derive(Clone, Debug, Default)]
pub struct StartWork {
    /// The issue `w` was pressed on, display spelling. Empty when closed.
    pub issue_key: String,
    /// The draft, once it arrives. `None` while the request is in flight.
    pub draft: Option<StartWorkDraft>,
    /// What stopped the draft, when something did.
    pub error: Option<String>,
    /// Tasks the human took out with `e`, as indices into `draft.tasks`.
    pub dropped: BTreeSet<usize>,
    /// The edit cursor's row.
    pub sel: usize,
    /// Whether `e` has opened the task list for editing.
    pub editing: bool,
    /// The seq of the draft request in flight, so a stale reply is dropped.
    pub wait: u64,
}

impl StartWork {
    /// Open the overlay on `key`, with nothing drafted yet.
    pub fn open(&mut self, key: &str, wait: u64) {
        *self = Self {
            issue_key: key.to_owned(),
            wait,
            ..Self::default()
        };
    }

    /// Close it and forget the draft.
    pub fn close(&mut self) {
        *self = Self::default();
    }

    /// The tasks that survive the human's edits, in draft order.
    ///
    /// This is what an approval sends, and the only way to read the list: a
    /// caller that walked `draft.tasks` directly would approve rows the human
    /// had just taken out.
    #[must_use]
    pub fn kept(&self) -> Vec<&DraftTask> {
        let Some(draft) = &self.draft else {
            return Vec::new();
        };
        draft
            .tasks
            .iter()
            .enumerate()
            .filter(|(i, _)| !self.dropped.contains(i))
            .map(|(_, task)| task)
            .collect()
    }

    /// Toggle the cursor's task in or out of the plan.
    ///
    /// A no-op with no draft, and a no-op past the end — the cursor is bounded
    /// by [`StartWork::move_cursor`], and a toggle that could name a row the
    /// draft does not have would silently drop a *different* row after the
    /// next redraw.
    pub fn toggle(&mut self) {
        let Some(draft) = &self.draft else { return };
        if self.sel >= draft.tasks.len() {
            return;
        }
        if !self.dropped.remove(&self.sel) {
            self.dropped.insert(self.sel);
        }
    }

    /// Move the edit cursor by `delta`, clamped to the drafted tasks.
    pub fn move_cursor(&mut self, delta: isize) {
        let count = self.draft.as_ref().map_or(0, |d| d.tasks.len());
        if count == 0 {
            return;
        }
        let last = count - 1;
        self.sel = match delta {
            d if d < 0 => self.sel.saturating_sub(d.unsigned_abs()),
            d => (self.sel + d.unsigned_abs()).min(last),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft() -> StartWorkDraft {
        StartWorkDraft {
            issue_key: "#151".into(),
            issue_title: "dedup digest persists across CI runs".into(),
            sources: DraftSources::default(),
            tasks: vec![
                DraftTask {
                    subject: "read seen-set write path".into(),
                    contract: None,
                },
                DraftTask {
                    subject: "persist digest set".into(),
                    contract: Some(DraftContract {
                        done_means: "file exists after run".into(),
                        mechanism: "graph".into(),
                        deterministic: true,
                    }),
                },
            ],
            gates: 5,
            estimate: None,
        }
    }

    fn opened() -> StartWork {
        let mut panel = StartWork::default();
        panel.open("#151", 7);
        panel.draft = Some(draft());
        panel
    }

    #[test]
    fn opening_forgets_the_previous_issue_entirely() {
        let mut panel = opened();
        panel.dropped.insert(0);
        panel.sel = 1;
        panel.editing = true;
        panel.open("#874", 9);
        assert_eq!(panel.issue_key, "#874");
        assert_eq!(panel.wait, 9);
        assert!(panel.draft.is_none(), "a new issue draws no old draft");
        assert!(panel.dropped.is_empty(), "edits do not follow the cursor");
        assert_eq!(panel.sel, 0);
        assert!(!panel.editing);
    }

    #[test]
    fn kept_drops_exactly_what_the_human_took_out() {
        let mut panel = opened();
        panel.toggle();
        let kept: Vec<&str> = panel.kept().iter().map(|t| t.subject.as_str()).collect();
        assert_eq!(kept, vec!["persist digest set"]);
        panel.toggle();
        assert_eq!(panel.kept().len(), 2, "toggling back restores the task");
    }

    #[test]
    fn the_cursor_cannot_leave_the_drafted_tasks() {
        let mut panel = opened();
        panel.move_cursor(-1);
        assert_eq!(panel.sel, 0);
        panel.move_cursor(9);
        assert_eq!(panel.sel, 1, "clamped to the last task");
    }

    #[test]
    fn a_toggle_past_the_end_changes_nothing() {
        let mut panel = opened();
        panel.sel = 42;
        panel.toggle();
        assert!(panel.dropped.is_empty());
        assert_eq!(panel.kept().len(), 2);
    }

    #[test]
    fn a_panel_with_no_draft_keeps_nothing_and_moves_nowhere() {
        let mut panel = StartWork::default();
        panel.move_cursor(3);
        panel.toggle();
        assert_eq!(panel.sel, 0);
        assert!(panel.kept().is_empty());
    }
}
