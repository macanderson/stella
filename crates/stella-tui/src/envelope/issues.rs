// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The ISSUES tab's read models and wire types — tracker-agnostic by
//! construction.
//!
//! The driver maps whatever issue source it carries into these shapes and the
//! deck never learns which tracker it was, which is what lets one tab serve
//! GitHub and Linear without a branch anywhere in this crate.
//!
//! Split out of `envelope.rs` under #629's 1500-line ratchet, the same move
//! `skills.rs` made (#3493). A subject rather than an arbitrary cut: these
//! types are one vocabulary, nothing outside the ISSUES tab reads them, and
//! none carries behaviour beyond a display label. Re-exported from the parent,
//! so every `envelope::IssueRow` path still resolves.

/// One row of the ISSUES tab's browse list — tracker-agnostic: the driver
/// maps whatever issue source it carries into this shape, and the deck never
/// learns which tracker it was.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IssueRow {
    /// `#123` (GitHub) or `ENG-123` (Linear).
    pub key: String,
    pub title: String,
    pub state: String,
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    pub url: String,
    pub updated_at: Option<String>,
    /// The work this session started for the issue, once it has started any.
    pub linked: Option<LinkedWork>,
}

/// The work one session claimed for an issue: what `w start work` opened, and
/// what that claim's evidence ledger has recorded since (SPEC 9.4).
///
/// The ISSUES tab reads this three ways — the inline `plan r3 · task 3 live`
/// tag on a row, the detail pane's `linked` line, and the
/// [`touched_files`](Self::touched_files) the heat sort joins against the code
/// graph (`crate::views::issues`).
///
/// **No producer fills it yet.** The deck's own start-work key is refused by
/// name in `stella-cli`'s `command_deck::issues`'s `issues_act`, because the
/// claim belongs to the self-driving loop's dispatch ledger rather than to a
/// tracker status, and the plan-opening event that would name a round lost its
/// producer when the staged pipeline was removed (#3865). #5197 is the read
/// path from those ledgers to the deck. Until it lands, every row carries
/// `None` here and the tab draws the tracker's own fields alone — which is why
/// each field below is dropped from rendering when it is empty rather than
/// printed as a zero.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkedWork {
    /// The plan's round label, spelled as the plan panel spells it (`r3`).
    /// Empty when the claim opened a branch before any plan existed.
    pub plan: String,
    /// Tasks that plan has finished.
    pub tasks_done: usize,
    /// Tasks that plan holds. Zero means "no plan to count", so the `2/6`
    /// clause is dropped rather than rendered as `0/0`.
    pub tasks_total: usize,
    /// The 1-based task the loop is executing right now, when one is live.
    pub live_task: Option<usize>,
    /// The branch the claim opened. Empty when it has opened none.
    pub branch: String,
    /// Root-relative paths the evidence ledger recorded the work as touching.
    /// The heat sort looks each one up in the code graph.
    pub touched_files: Vec<String>,
    /// Tests the evidence ledger holds passing.
    pub tests_passed: usize,
    /// Tests it ran. Zero drops the `4/4 tests` clause, for the same reason
    /// [`tasks_total`](Self::tasks_total) does.
    pub tests_total: usize,
}

/// One row of the create form's type-ahead popup. `kind` is a display type
/// label ("Person", "Agent", "Memory", "Symbol", "Label", …) — rows render as
/// `Kind: label — description`. `insert` is what picking the row writes into
/// the field: `@login` or an email for people, the label name for labels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EntityHit {
    pub kind: String,
    pub label: String,
    pub description: String,
    pub insert: String,
}

/// Which create-form field a type-ahead [`WorkspaceInput::EntitySearch`]
/// feeds — each has its own vocabulary (people vs. labels).
///
/// [`WorkspaceInput::EntitySearch`]: crate::envelope::WorkspaceInput::EntitySearch
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntityField {
    Assignee,
    Label,
}

impl EntityField {
    pub fn label(self) -> &'static str {
        match self {
            EntityField::Assignee => "assignee",
            EntityField::Label => "labels",
        }
    }
}

/// An action on one existing issue ([`WorkspaceInput::IssueAct`]).
///
/// [`WorkspaceInput::IssueAct`]: crate::envelope::WorkspaceInput::IssueAct
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IssueAction {
    /// Add a comment (the deck's `c` prompt).
    Comment(String),
    /// Move to a named status (any workflow-state word on Linear; on GitHub
    /// only the two states below exist, each with its own option here).
    SetStatus(String),
    /// Close the issue (the deck's `x` on an open row).
    Close,
    /// Re-open a closed issue (the deck's `x` on a closed row).
    ///
    /// Its own option rather than `SetStatus("open")` so the driver selects
    /// the provider call by matching on the enum instead of comparing a
    /// status string — the same reason [`IssueAction::Close`] is not
    /// `SetStatus("closed")`, and why the port grew
    /// `IssueProvider::reopen` rather than a status setter.
    Reopen,
}
