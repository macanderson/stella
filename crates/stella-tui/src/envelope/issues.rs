//! The ISSUES tab's tracker-facing wire types.
//!
//! Split out of `envelope.rs` under #629's 1500-line ratchet, the same move
//! `skills.rs` made (#3493): pure type definitions the deck emits and the
//! driver interprets, with no behaviour beyond display labels.

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
