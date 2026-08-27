// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The plan graph — the wire half of "drift is recorded, not hidden"
//! (`design/tui-v2/SPEC.md` §1.3, §7.4). `[:NEXT]` is the order the plan says
//! its tasks go in, `[:THEN]` the order they turned out to go in, and
//! [`PlanEdgeKind`] is the only thing separating the two lanes.
//!
//! Three rules are the types rather than conventions, and each one is stated
//! on the type that carries it. A revision is a [`PlanNode`] of its own, so
//! inserting a task authors `r{n+1}` beside `r1` instead of over it and SPEC
//! 7.3's `planned 6` stays recoverable. A [`DivergenceCause`] can never be
//! blank. A [`Divergence`] is derived by
//! `stella_core::plan_graph::PlanGraph::divergences` and stored nowhere, so no
//! producer can assert drift the graph does not show, or hide drift it does.
//!
//! Types only, no logic (AGENTS.md #1). Every one of them round-trips through
//! `serde_json` byte-for-byte (#4), which the tests below hold: a recorded
//! plan that reads back *slightly* different is a replay that disagrees with
//! the run it claims to reproduce.

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

/// Which revision of a plan something belongs to: `r1`, `r2`, ….
///
/// One-based, and there is no `r0`: the first thing that exists is a plan
/// somebody approved, so zero names nothing. `Deserialize` refuses it rather
/// than accepting a value with no meaning, which is what keeps
/// [`PlanNode::cause`]'s `None`-only-on-the-first rule decidable from the
/// wire alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PlanRevision(u32);

impl PlanRevision {
    /// `r1` — the plan as approved.
    pub const FIRST: Self = Self(1);

    /// A revision by number, or `None` for zero.
    #[must_use]
    pub const fn new(n: u32) -> Option<Self> {
        if n == 0 { None } else { Some(Self(n)) }
    }

    /// The number, for rendering as `r{n}`.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// The revision that supersedes this one.
    ///
    /// Saturating at `u32::MAX`, which is not a real ceiling for a plan: a
    /// session that revised its plan four billion times has a different
    /// problem, and a wrapping increment would silently hand out `r0`.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// The revision this one supersedes, or `None` for [`Self::FIRST`].
    #[must_use]
    pub const fn previous(self) -> Option<Self> {
        Self::new(self.0 - 1)
    }
}

impl std::fmt::Display for PlanRevision {
    /// `r3` — the breadcrumb's spelling (SPEC 5), so a surface never
    /// re-invents the prefix.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "r{}", self.0)
    }
}

impl<'de> Deserialize<'de> for PlanRevision {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let n = u32::deserialize(d)?;
        Self::new(n).ok_or_else(|| {
            de::Error::custom(
                "a plan revision is one-based; there is no r0, because the first thing that \
                 exists is a plan somebody approved",
            )
        })
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for PlanRevision {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "PlanRevision".into()
    }

    /// An integer with `minimum: 1`, so the one-based rule survives the crate
    /// boundary instead of stopping at `Deserialize`.
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = schemars::json_schema!({
            "type": "integer",
            "minimum": 1
        });
        schema.insert(
            "description".to_owned(),
            serde_json::Value::String(
                "Which revision of a plan this is, one-based: 1 is the plan as approved, and \
                 each later number is a revision authored beside its predecessor rather than \
                 over it."
                    .to_owned(),
            ),
        );
        schema
    }
}

/// Why the plan left the path it was approved on — a compiler error code, a
/// failing gate, the change a reviewer asked for.
///
/// **Non-empty, always.** See the module docs: an optional cause is a cause
/// every producer skips, and drift nobody explained is the thing this module
/// exists to stop. On the wire it is a plain JSON string, so a reader sees the
/// text and nothing else; the emptiness rule is enforced on the way in.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct DivergenceCause(String);

impl DivergenceCause {
    /// A cause, or `None` if it says nothing.
    ///
    /// Whitespace counts as nothing: `"   "` is the empty string wearing a
    /// disguise, and a surface that rendered it would print a blank where a
    /// reason belongs.
    #[must_use]
    pub fn new(cause: impl Into<String>) -> Option<Self> {
        let cause = cause.into();
        (!cause.trim().is_empty()).then_some(Self(cause))
    }

    /// The cause as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DivergenceCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for DivergenceCause {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::new(raw).ok_or_else(|| {
            de::Error::custom(
                "a divergence cause must say something; drift with a blank reason is drift \
                 nobody recorded",
            )
        })
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for DivergenceCause {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "DivergenceCause".into()
    }

    /// A string with `minLength: 1`. The trimmed-emptiness rule is stricter
    /// than JSON Schema can state, so the schema states the half it can and
    /// `Deserialize` holds the rest — a schema that claimed the whole rule
    /// would be describing a check it does not perform.
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = schemars::json_schema!({
            "type": "string",
            "minLength": 1
        });
        schema.insert(
            "description".to_owned(),
            serde_json::Value::String(
                "Why the plan left the path it was approved on — a compiler error code, a \
                 failing gate, a reviewer's change request. Never blank."
                    .to_owned(),
            ),
        );
        schema
    }
}

/// One task in the plan graph: the board row a lane renders.
///
/// The `subject` rides here rather than being joined back to the task board on
/// read. `/clear` deletes a session's
/// board rows (`stella_store::Store::clear_session_tasks`) precisely because
/// they are board *state*; the plan graph is the audit trail of what was
/// planned and what ran, and an audit trail that goes blank when somebody
/// resets their board is not one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TaskNode {
    /// The board's per-session ordinal id (`"1"`, `"2"`, …), as
    /// [`crate::TaskItem::id`] spells it.
    pub id: String,
    /// What the task is, in one line — [`crate::TaskItem::subject`] at the
    /// moment this edge was authored.
    pub subject: String,
}

impl TaskNode {
    /// A node from a board row's id and subject.
    #[must_use]
    pub fn new(id: impl Into<String>, subject: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            subject: subject.into(),
        }
    }
}

/// Which lane an edge belongs to (SPEC 7.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PlanEdgeKind {
    /// `[:NEXT]` — the planned path: the order the plan says the tasks go in,
    /// at one revision.
    Next,
    /// `[:THEN]` — the actual path: the order they turned out to go in.
    Then,
}

impl PlanEdgeKind {
    /// The wire spelling, and the one a query or a log should use.
    #[must_use]
    pub const fn as_wire_str(self) -> &'static str {
        match self {
            Self::Next => "next",
            Self::Then => "then",
        }
    }

    /// Resolve a wire spelling, or `None` if it names no lane.
    #[must_use]
    pub fn from_wire_str(name: &str) -> Option<Self> {
        match name {
            "next" => Some(Self::Next),
            "then" => Some(Self::Then),
            _ => None,
        }
    }
}

/// The tail of an edge.
///
/// An edge always points *at* a task and only sometimes comes *from* one: the
/// first task in a lane follows the plan itself. Two shapes rather than an
/// `Option<String>` because "the plan" is a node in this graph, not the
/// absence of one, and an `Option` would leave a reader guessing which it
/// meant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case", tag = "node", content = "id")]
pub enum PlanEdgeSource {
    /// The head of a lane: the plan node at this edge's revision.
    Plan,
    /// The task this one follows, by board id.
    Task(String),
}

/// One `[:NEXT]` or `[:THEN]` edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PlanEdge {
    /// Which lane this edge is in.
    pub kind: PlanEdgeKind,
    /// Which revision authored it. For a `[:NEXT]` edge that is the revision
    /// whose chain it belongs to; for a `[:THEN]` edge it is the revision in
    /// force when the task ran, which is what lets a reader tell a task that
    /// ran under the approved plan from one that ran under a revision of it.
    pub revision: PlanRevision,
    /// What this edge follows.
    pub from: PlanEdgeSource,
    /// What it points at.
    pub to: TaskNode,
    /// Zero-based position in its lane, so a lane reconstructed from an
    /// unordered store still comes back in order.
    pub position: u32,
}

/// One revision of the plan — a plan node in the graph.
///
/// The graph holds one of these per revision and never replaces one: `r1`
/// survives `r2` being authored, which is the whole of "the prior plan is
/// retained".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PlanNode {
    /// Which revision this is.
    pub revision: PlanRevision,
    /// Why the plan was re-authored here.
    ///
    /// `None` on [`PlanRevision::FIRST`] and `Some` on every later revision —
    /// the approved plan diverged from nothing, and everything after it
    /// diverged from something. `stella_core::plan_graph::PlanGraph` is where
    /// that rule is enforced on construction; on the wire it is a shape a
    /// reader can check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<DivergenceCause>,
}

/// Which way the actual path left the planned one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DivergenceKind {
    /// A task the approved plan did not have. SPEC 7.2's `⌥` state, and the
    /// `+1` in SPEC 7.3's `planned 6 · actual 7`.
    Inserted,
    /// A task the approved plan had and a revision took out.
    Dropped,
}

/// One recorded departure of the plan from the plan it was approved as.
///
/// Derived rather than stored — see the module docs. A [`Divergence`] value is
/// an answer somebody computed, never a fact somebody wrote down, so there is
/// no constructor here that lets a producer assert drift that the graph does
/// not show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Divergence {
    /// Which way the path left.
    pub kind: DivergenceKind,
    /// The task that was added or taken out.
    pub task: TaskNode,
    /// The revision that did it.
    pub revision: PlanRevision,
    /// Why — that revision's own cause, carried here so a surface rendering a
    /// divergence never has to go and find it.
    pub cause: DivergenceCause,
}

/// A plan revision stella has **proposed** and not made — SPEC 8.1's
/// `⌥ propose r4: add task "<title>"`.
///
/// A proposal is a different thing from a revision all the way to the surface.
/// [`PlanNode`] is a revision that happened; this is one somebody has been
/// asked about, and until they answer there is no node, no `[:NEXT]` edge and
/// nothing that may run. A revision carrying an `approved: bool` instead would
/// make "nothing runs until it is approved" a flag every producer can set
/// rather than a shape the graph cannot express.
///
/// # What a proposal is allowed to claim
///
/// [`Self::gate`] names a gate an installed verification plugin reported on,
/// and [`Self::cause`] is what that gate's evidence said. Stella re-ran
/// nothing and re-checked nothing (AGENTS.md's opening), so a proposal
/// answers reported evidence and is never a measurement of its own.
///
/// # Why the subject and not a task id
///
/// The proposal names the work in words. Board ids belong to the task board
/// (`stella_core::tasks::TaskBoard::create` never reuses one), so a proposal
/// that minted its own would open a second id space beside it. The id is the
/// board's to assign when the insertion is actually made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RevisionProposal {
    /// The revision this would author if it were approved — `r{n+1}` against
    /// the plan as it stands. A number that has not happened yet, which is why
    /// it rides here rather than being read off a [`PlanNode`] that does not
    /// exist.
    pub revision: PlanRevision,
    /// The task the revision would insert, in one line: SPEC 8.1's `<title>`.
    pub subject: String,
    /// The gate whose failure provoked this — the `[requirements]` key the
    /// verification plugin's manifest wrote, as [`crate::GateRow::name`]
    /// spells it.
    pub gate: String,
    /// What that gate's evidence said, as the linked cause the revision would
    /// carry. Non-blank by construction, like every other cause here.
    pub cause: DivergenceCause,
    /// The issue this repair belongs to, where the evidence named one.
    ///
    /// `None` renders as no cell rather than a blank one, on
    /// [`crate::GateBoard::patch`]'s rule: an invented link points the reader
    /// at something they cannot go and look at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cause(text: &str) -> DivergenceCause {
        DivergenceCause::new(text).expect("a non-empty cause")
    }

    // ── AGENTS.md #4: everything crossing a boundary round-trips ────────────

    #[test]
    fn an_edge_round_trips_byte_for_byte() {
        let edge = PlanEdge {
            kind: PlanEdgeKind::Next,
            revision: PlanRevision::FIRST,
            from: PlanEdgeSource::Task("2".into()),
            to: TaskNode::new("3", "wire the dedup digest"),
            position: 2,
        };
        let json = serde_json::to_string(&edge).expect("serialize");
        let back: PlanEdge = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(edge, back);
        assert_eq!(json, serde_json::to_string(&back).expect("re-serialize"));
    }

    #[test]
    fn a_lane_head_edge_round_trips() {
        let edge = PlanEdge {
            kind: PlanEdgeKind::Then,
            revision: PlanRevision::FIRST.next(),
            from: PlanEdgeSource::Plan,
            to: TaskNode::new("1", "read the routes"),
            position: 0,
        };
        let json = serde_json::to_string(&edge).expect("serialize");
        let back: PlanEdge = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(edge, back);
        assert_eq!(json, serde_json::to_string(&back).expect("re-serialize"));
    }

    #[test]
    fn a_plan_node_round_trips_in_both_of_its_two_shapes() {
        for node in [
            PlanNode {
                revision: PlanRevision::FIRST,
                cause: None,
            },
            PlanNode {
                revision: PlanRevision::FIRST.next(),
                cause: Some(cause("E0432: unresolved import")),
            },
        ] {
            let json = serde_json::to_string(&node).expect("serialize");
            let back: PlanNode = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(node, back);
            assert_eq!(json, serde_json::to_string(&back).expect("re-serialize"));
        }
    }

    /// The approved plan writes no `cause` key at all, so a reader can tell
    /// `r1` from a later revision by shape and not only by number.
    #[test]
    fn the_approved_plan_writes_no_cause_key() {
        let json = serde_json::to_string(&PlanNode {
            revision: PlanRevision::FIRST,
            cause: None,
        })
        .expect("serialize");
        assert_eq!(json, r#"{"revision":1}"#);
    }

    #[test]
    fn a_divergence_round_trips() {
        let divergence = Divergence {
            kind: DivergenceKind::Inserted,
            task: TaskNode::new("3b", "repair the unresolved import"),
            revision: PlanRevision::FIRST.next(),
            cause: cause("E0432"),
        };
        let json = serde_json::to_string(&divergence).expect("serialize");
        let back: Divergence = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(divergence, back);
        assert_eq!(json, serde_json::to_string(&back).expect("re-serialize"));
    }

    #[test]
    fn a_revision_proposal_round_trips() {
        let proposal = RevisionProposal {
            revision: PlanRevision::FIRST.next(),
            subject: "repair the unresolved import".into(),
            gate: "tests".into(),
            cause: cause("E0432: unresolved import `stella_core::plan_graph`"),
            issue: Some("#5043".into()),
        };
        let json = serde_json::to_string(&proposal).expect("serialize");
        let back: RevisionProposal = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(proposal, back);
        assert_eq!(json, serde_json::to_string(&back).expect("re-serialize"));
    }

    /// A proposal that named no issue writes no `issue` key, so a surface
    /// cannot render an empty link cell for a link nobody supplied.
    #[test]
    fn a_proposal_with_no_issue_writes_no_issue_key() {
        let json = serde_json::to_string(&RevisionProposal {
            revision: PlanRevision::FIRST.next(),
            subject: "repair the unresolved import".into(),
            gate: "tests".into(),
            cause: cause("E0432"),
            issue: None,
        })
        .expect("serialize");
        assert!(!json.contains("issue"), "{json}");
    }

    // ── the rules that are the type ────────────────────────────────────────

    /// There is no `r0`. A stream carrying one is refused rather than
    /// normalized, because a revision number nothing can supersede is not a
    /// revision — and silently promoting it to `r1` would let two different
    /// plans claim the same identity.
    #[test]
    fn a_revision_is_one_based() {
        assert_eq!(PlanRevision::new(0), None);
        assert_eq!(PlanRevision::FIRST.previous(), None);
        assert_eq!(
            PlanRevision::FIRST.next().previous(),
            Some(PlanRevision::FIRST)
        );

        let err = serde_json::from_str::<PlanRevision>("0").expect_err("r0 must not deserialize");
        assert!(err.to_string().contains("one-based"), "{err}");
    }

    /// The breadcrumb's spelling lives on the type, so no surface writes the
    /// `r` prefix itself and none of them can disagree about it (#4333).
    #[test]
    fn a_revision_renders_as_the_breadcrumb_spells_it() {
        assert_eq!(PlanRevision::FIRST.to_string(), "r1");
        assert_eq!(PlanRevision::FIRST.next().next().to_string(), "r3");
    }

    /// The acceptance criterion for "drift is recorded, not hidden": a cause
    /// that says nothing is refused by the only constructor there is, and by
    /// the wire.
    #[test]
    fn a_cause_that_says_nothing_is_refused() {
        assert_eq!(DivergenceCause::new(""), None);
        assert_eq!(DivergenceCause::new("   \t "), None);
        assert_eq!(
            DivergenceCause::new("E0432").map(|c| c.as_str().to_owned()),
            Some("E0432".to_owned())
        );

        let err = serde_json::from_str::<DivergenceCause>(r#""  ""#)
            .expect_err("a blank cause must not deserialize");
        assert!(err.to_string().contains("must say something"), "{err}");
    }

    /// A lane's spelling is resolved in one place, so a store column and a
    /// wire field cannot drift apart.
    #[test]
    fn a_lane_resolves_from_its_wire_spelling() {
        for kind in [PlanEdgeKind::Next, PlanEdgeKind::Then] {
            assert_eq!(PlanEdgeKind::from_wire_str(kind.as_wire_str()), Some(kind));
        }
        assert_eq!(PlanEdgeKind::from_wire_str("maybe"), None);
    }
}
