// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The approval question a gate parks, and the answer that comes back.
//!
//! Four places share these types. The flow that parks the call. The engine's
//! route port. The prompt the CLI draws. The card the terminal draws. They
//! cross four crate lines, so they round-trip through `serde_json` byte for
//! byte (AGENTS.md #4).
//!
//! They sit here for the reason the question types next door do. A screen
//! draws the card. Drawing a card must not mean linking the executor that
//! parked it.
//!
//! `stella_core::hooks::decision` and `stella_tools::registry::approval`
//! re-export all three. Every caller keeps the path it has.

use serde::{Deserialize, Serialize};

/// What a parked approval question is *about*.
///
/// A shape of `{ tool, read_only, reason }` makes a tool name a required
/// field. A `Stop` hook that asks at a **turn boundary** has no tool to name.
/// It could only ask by making one up. So it does not ask: the hook gets a
/// note back saying the verb does not apply, and the audit trail keeps no
/// lie. `doc:pipeline-as-plugins` §4 A6 names the case that needs this arm. A
/// paid plugin asks *"budget spent, go on?"* and needs an answer.
///
/// Closed, and two. A third arm is a new *kind* of question. Each screen that
/// asks would need a new drawing for it. So a third arm is a design change,
/// not a new value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "subject", rename_all = "snake_case")]
pub enum ApprovalSubject {
    /// One tool call, parked before it dispatches.
    Tool {
        /// The tool whose dispatch is parked.
        name: String,
        /// The tool's advertised `read_only` bit.
        read_only: bool,
    },
    /// The turn itself, parked at the boundary where it would complete.
    TurnCompletion {
        /// A digest of the text the turn is about to complete with — never
        /// the text.
        ///
        /// A digest rather than the answer because this rides the
        /// `approval.*` audit events, and AGENTS.md #3 governs what those
        /// carry: a model's reply is content. It is still worth carrying, and
        /// is not decoration: a surface can tell two questions about two
        /// different completions apart, and a resolution can be matched to
        /// the completion it was given for.
        final_text_digest: String,
    },
}

impl ApprovalSubject {
    /// The tool this question is about, or `None` at a turn boundary.
    ///
    /// The audit line's `tool` key is built from this. An `Option`, not a
    /// string with a filler in it. A missing key says there was no tool. A key
    /// reading `"<turn>"` says a tool by that name was parked.
    #[must_use]
    pub fn tool(&self) -> Option<&str> {
        match self {
            Self::Tool { name, .. } => Some(name),
            Self::TurnCompletion { .. } => None,
        }
    }

    /// Whether what is parked is advertised read-only. `false` at a turn
    /// boundary: a completion is not a read.
    #[must_use]
    pub fn read_only(&self) -> bool {
        match self {
            Self::Tool { read_only, .. } => *read_only,
            Self::TurnCompletion { .. } => false,
        }
    }

    /// A short stable label for an audit line and for a surface with one line
    /// to spend — never parsed, only displayed.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Tool { name, .. } => format!("`{name}`"),
            Self::TurnCompletion { .. } => "this turn's completion".to_string(),
        }
    }
}

/// One approval question, as the responder and the bus both see it.
///
/// It crosses a crate line: the CLI answers it through
/// `stella_tools::registry::approval::ApprovalResponder`. So it round-trips
/// through `serde_json` byte for byte (AGENTS.md #4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// What is parked: a tool call and its advertised `read_only` bit, or the
    /// turn's own completion.
    ///
    /// One enum, not a tool name plus a `read_only` flag. Those two fields
    /// make a tool required, and a turn boundary has none to give.
    ///
    /// One shared enum, not a second one here. This is
    /// [`RiskLevel`](crate::RiskLevel)'s reason, one plane over. The subject a
    /// screen draws and the subject the engine parked must be one type. Two
    /// spellings of one idea can disagree.
    pub parked: ApprovalSubject,
    /// The gate's reason, verbatim from the `RequireApproval` decision.
    pub reason: String,
    /// The blocking chain that raised the requirement
    /// (`tool.call.requested`, `file.created`/`updated`/`deleted`,
    /// `command.started`, or a bridge's own event name).
    pub gate: String,
    /// The chain's narrower subject when it has one — the workspace path
    /// for a `file.*` gate, the command line for `command.started`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

/// The human's answer, as the responder port returns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum ApprovalResponse {
    /// Run this one call.
    Approve,
    /// Refuse it. `reason` carries the human's words when they gave any;
    /// empty means a bare "no".
    Deny { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both subjects and both answers survive a round trip byte for byte
    /// (AGENTS.md #4). Four crates read these off one wire.
    #[test]
    fn every_shape_round_trips() {
        let shapes = [
            ApprovalRequest {
                parked: ApprovalSubject::Tool {
                    name: "bash".into(),
                    read_only: false,
                },
                reason: "a gate asked".into(),
                gate: "tool.call.requested".into(),
                subject: Some("rm -rf /tmp/x".into()),
            },
            ApprovalRequest {
                parked: ApprovalSubject::TurnCompletion {
                    final_text_digest: "abc123".into(),
                },
                reason: "budget exhausted".into(),
                gate: "stop".into(),
                subject: None,
            },
        ];
        for shape in shapes {
            let wire = serde_json::to_string(&shape).expect("serialize");
            let back: ApprovalRequest = serde_json::from_str(&wire).expect("deserialize");
            assert_eq!(back, shape);
            assert_eq!(serde_json::to_string(&back).expect("re-serialize"), wire);
        }

        for answer in [
            ApprovalResponse::Approve,
            ApprovalResponse::Deny {
                reason: "not that path".into(),
            },
        ] {
            let wire = serde_json::to_string(&answer).expect("serialize");
            let back: ApprovalResponse = serde_json::from_str(&wire).expect("deserialize");
            assert_eq!(back, answer);
        }
    }

    /// A question with no tool answers `None` rather than inventing a name,
    /// which is what the audit payload's `tool` key depends on.
    #[test]
    fn a_turn_boundary_names_no_tool() {
        let turn = ApprovalSubject::TurnCompletion {
            final_text_digest: "d".into(),
        };
        assert_eq!(turn.tool(), None);
        assert!(!turn.read_only());
        assert_eq!(turn.label(), "this turn's completion");
    }
}
