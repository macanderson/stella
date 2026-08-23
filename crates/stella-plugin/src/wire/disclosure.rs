// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What crosses to a plugin's process on the **hook** channel, and what the
//! consent prompt tells a human about it (#4310).
//!
//! [`WIRE_FIELDS`] covers the wrapper socket and only the wrapper
//! socket. A `[runtime]` observer that declares `hooks = [...]` and no wrapper
//! points therefore rendered *"It asks for no tool capabilities."* and no data
//! section at all — while its process was started and fed hook payloads that
//! **carry the user's tool inputs**. The prompt actively said the plugin asked
//! for nothing, and it was the more serious of the two holes #3514 left.
//!
//! # The table is per event, because the payload is
//!
//! One `HookPayload` type serves seven events and populates a different subset
//! of its fields for each: `PreToolUse` carries the tool's raw arguments,
//! `PostToolUse` adds the whole unclipped tool output, `Stop` carries the
//! model's whole final reply, the issue events carry the tracker's identifiers,
//! and `SessionStart`/`PreCompact` carry the workspace path and nothing else.
//! A per-*type* table would have to disclose the union to everyone, which
//! over-states what a `SessionStart` observer receives by the widest margin in
//! the set — so the rows are keyed on the event that carries them.
//!
//! # What holds this to the truth
//!
//! `HookPayload` lives in `stella-core`, which this crate may not depend on
//! (README § Boundary: `stella-protocol` is the only workspace edge). The
//! exhaustive destructure that makes a new field a compile error therefore
//! lives in the crate that owns *both* — `stella-tools`, which is also the
//! crate whose `hook_runner` actually writes these bytes to a process's stdin.
//! See `crates/stella-tools/tests/hook_disclosure.rs`: it destructures every
//! payload type, and it builds each event's payload with the real constructor
//! and compares the keys that actually serialize against the rows below.

use stella_protocol::hook::HookEvent;

use super::WrapperPoint;

/// One field of a request that crosses to a plugin's own process, and what a
/// consent prompt tells a human about it.
///
/// [`crate::consent_text`] renders [`Self::disclosure`] verbatim (#3514). A
/// user deciding whether to install a plugin that runs as a process is deciding
/// what of their work that process gets to see, and the answer is a property of
/// *this* module: writing the sentences out beside the prompt instead would put
/// a copy of the request shape in a file that does not change when the request
/// does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireField {
    /// The point whose request carries it.
    pub point: WrapperPoint,
    /// The field's path, as serde writes it — dotted for a field of the nested
    /// [`TurnOutcome`](super::TurnOutcome).
    pub path: &'static str,
    /// What crosses, in the words a human reads before granting the point, or
    /// `None` for a field carrying nothing of the user's.
    ///
    /// `None` is a **decision, not an omission**: every field of the two
    /// requests has a row here, so a field that crosses the process boundary
    /// and is disclosed to nobody is a line somebody wrote rather than one
    /// nobody noticed.
    pub disclosure: Option<&'static str>,
}

/// Every field of the two request messages, and what the consent prompt says
/// about each.
///
/// In wire order, grouped by point, because the prompt renders it in table
/// order and is showing a reader a message rather than a set.
///
/// The destructure is **transitive**: `every_wire_field_is_in_the_table` takes
/// apart every type reachable from a dispatched request — the two requests,
/// [`TurnOutcome`](super::TurnOutcome), [`CandidateGrant`](super::CandidateGrant), [`TestPlan`](super::TestPlan) and [`PublishedSignal`](super::PublishedSignal) —
/// so a field added to any of them stops that test compiling until it has a
/// row here (#4310). It used to stop at the first three, and the `candidate`
/// row's sentence named `root` and `test` by hand, which is exactly the
/// hand-maintained list this table exists to abolish.
pub const WIRE_FIELDS: &[WireField] = &[
    WireField {
        point: WrapperPoint::BeforeTurn,
        path: "protocol_version",
        disclosure: None,
    },
    WireField {
        point: WrapperPoint::BeforeTurn,
        path: "wrapper",
        disclosure: None,
    },
    WireField {
        point: WrapperPoint::BeforeTurn,
        path: "stage",
        disclosure: None,
    },
    WireField {
        point: WrapperPoint::BeforeTurn,
        path: "round",
        disclosure: None,
    },
    WireField {
        point: WrapperPoint::BeforeTurn,
        path: "goal",
        disclosure: Some("the goal you typed for this turn, in full"),
    },
    WireField {
        point: WrapperPoint::BeforeTurn,
        path: "candidate.handle",
        disclosure: None,
    },
    WireField {
        point: WrapperPoint::BeforeTurn,
        path: "candidate.root",
        disclosure: Some(
            "when the host has made one, the absolute path of the scratch workspace the turn \
             will run in",
        ),
    },
    WireField {
        point: WrapperPoint::BeforeTurn,
        path: "candidate.test.program",
        disclosure: Some("and the test command it would run there, with its arguments"),
    },
    WireField {
        point: WrapperPoint::BeforeTurn,
        path: "candidate.test.args",
        disclosure: None,
    },
    WireField {
        point: WrapperPoint::BeforeTurn,
        path: "candidate.test.baseline",
        disclosure: None,
    },
    // The plugin's own words handed back: `published` carries what an earlier
    // stage of this same plugin published this turn, never something the host
    // measured of the user's.
    WireField {
        point: WrapperPoint::BeforeTurn,
        path: "published.signal",
        disclosure: None,
    },
    WireField {
        point: WrapperPoint::BeforeTurn,
        path: "published.value",
        disclosure: None,
    },
    WireField {
        point: WrapperPoint::AfterTurn,
        path: "protocol_version",
        disclosure: None,
    },
    WireField {
        point: WrapperPoint::AfterTurn,
        path: "wrapper",
        disclosure: None,
    },
    WireField {
        point: WrapperPoint::AfterTurn,
        path: "stage",
        disclosure: None,
    },
    WireField {
        point: WrapperPoint::AfterTurn,
        path: "round",
        disclosure: None,
    },
    WireField {
        point: WrapperPoint::AfterTurn,
        path: "goal",
        disclosure: Some("the goal you typed for this turn, in full"),
    },
    WireField {
        point: WrapperPoint::AfterTurn,
        path: "candidate.handle",
        disclosure: None,
    },
    WireField {
        point: WrapperPoint::AfterTurn,
        path: "candidate.root",
        disclosure: Some(
            "when the host has made one, the absolute path of the scratch workspace the turn \
             ran in",
        ),
    },
    WireField {
        point: WrapperPoint::AfterTurn,
        path: "candidate.test.program",
        disclosure: Some("and the test command it would run there, with its arguments"),
    },
    WireField {
        point: WrapperPoint::AfterTurn,
        path: "candidate.test.args",
        disclosure: None,
    },
    WireField {
        point: WrapperPoint::AfterTurn,
        path: "candidate.test.baseline",
        disclosure: None,
    },
    WireField {
        point: WrapperPoint::AfterTurn,
        path: "turn.completed",
        disclosure: Some("whether the turn finished or was aborted"),
    },
    WireField {
        point: WrapperPoint::AfterTurn,
        path: "turn.answer",
        disclosure: Some("the model's full reply for the turn — the same text you were shown"),
    },
    WireField {
        point: WrapperPoint::AfterTurn,
        path: "turn.tools",
        disclosure: Some("the name of every tool the turn ran, in call order"),
    },
    WireField {
        point: WrapperPoint::AfterTurn,
        path: "turn.changed_files",
        disclosure: Some("the workspace-relative path of every file the turn changed"),
    },
];

/// One field of one hook event's payload, and what the consent prompt says
/// about it.
///
/// [`crate::consent_text`] renders [`Self::disclosure`] verbatim, exactly as
/// it does [`WireField::disclosure`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookField {
    /// The event whose payload carries it.
    pub event: HookEvent,
    /// The field's path, as serde writes it on the wire — dotted for a field
    /// of a nested object, and carrying serde's own renames (`toolResult`,
    /// not `tool_result`).
    pub path: &'static str,
    /// What crosses, in the words a human reads before granting the hook, or
    /// `None` for a field carrying nothing of the user's.
    ///
    /// `None` is a **decision, not an omission**, on the same terms as
    /// [`WireField::disclosure`].
    pub disclosure: Option<&'static str>,
}

/// Every field of every hook payload, and what the consent prompt says about
/// each.
///
/// In event order, then in wire order within an event, because the prompt
/// renders it in table order and is showing a reader a message rather than a
/// set.
pub const HOOK_FIELDS: &[HookField] = &[
    HookField {
        event: HookEvent::SessionStart,
        path: "event",
        disclosure: None,
    },
    HookField {
        event: HookEvent::SessionStart,
        path: "cwd",
        disclosure: Some("the absolute path of the workspace you are running in"),
    },
    HookField {
        event: HookEvent::PreToolUse,
        path: "event",
        disclosure: None,
    },
    HookField {
        event: HookEvent::PreToolUse,
        path: "cwd",
        disclosure: Some("the absolute path of the workspace you are running in"),
    },
    HookField {
        event: HookEvent::PreToolUse,
        path: "tool.name",
        disclosure: Some("the name of every tool the turn is about to run"),
    },
    HookField {
        event: HookEvent::PreToolUse,
        path: "tool.input",
        disclosure: Some(
            "that tool's arguments in full, before it runs — the command line for a shell \
             call, the path and the whole new text for a write, the query for a search",
        ),
    },
    HookField {
        event: HookEvent::PreToolUse,
        path: "tool.read_only",
        disclosure: None,
    },
    HookField {
        event: HookEvent::PostToolUse,
        path: "event",
        disclosure: None,
    },
    HookField {
        event: HookEvent::PostToolUse,
        path: "cwd",
        disclosure: Some("the absolute path of the workspace you are running in"),
    },
    HookField {
        event: HookEvent::PostToolUse,
        path: "tool.name",
        disclosure: Some("the name of every tool the turn ran"),
    },
    HookField {
        event: HookEvent::PostToolUse,
        path: "tool.input",
        disclosure: Some("that tool's arguments in full"),
    },
    HookField {
        event: HookEvent::PostToolUse,
        path: "tool.read_only",
        disclosure: None,
    },
    HookField {
        event: HookEvent::PostToolUse,
        path: "toolResult",
        disclosure: Some(
            "everything that tool returned, unclipped — a file's whole contents for a read, \
             a command's whole output for a shell call",
        ),
    },
    HookField {
        event: HookEvent::Stop,
        path: "event",
        disclosure: None,
    },
    HookField {
        event: HookEvent::Stop,
        path: "cwd",
        disclosure: Some("the absolute path of the workspace you are running in"),
    },
    HookField {
        event: HookEvent::Stop,
        path: "finalText",
        disclosure: Some("the model's full reply for the turn — the same text you were shown"),
    },
    HookField {
        event: HookEvent::PreCompact,
        path: "event",
        disclosure: None,
    },
    HookField {
        event: HookEvent::PreCompact,
        path: "cwd",
        disclosure: Some("the absolute path of the workspace you are running in"),
    },
    HookField {
        event: HookEvent::PreIssueWork,
        path: "event",
        disclosure: None,
    },
    HookField {
        event: HookEvent::PreIssueWork,
        path: "cwd",
        disclosure: Some("the absolute path of the workspace you are running in"),
    },
    HookField {
        event: HookEvent::PreIssueWork,
        path: "issue.number",
        disclosure: Some("which issue the self-driving loop is about to work"),
    },
    HookField {
        event: HookEvent::PreIssueWork,
        path: "issue.title",
        disclosure: Some(
            "that issue's title and target branch, when the loop has already read them",
        ),
    },
    HookField {
        event: HookEvent::PreIssueWork,
        path: "issue.branch",
        disclosure: None,
    },
    HookField {
        event: HookEvent::PostIssueWork,
        path: "event",
        disclosure: None,
    },
    HookField {
        event: HookEvent::PostIssueWork,
        path: "cwd",
        disclosure: Some("the absolute path of the workspace you are running in"),
    },
    HookField {
        event: HookEvent::PostIssueWork,
        path: "issue.number",
        disclosure: Some("which issue the self-driving loop just worked"),
    },
    HookField {
        event: HookEvent::PostIssueWork,
        path: "issue.title",
        disclosure: Some(
            "that issue's title and target branch, when the loop has already read them",
        ),
    },
    HookField {
        event: HookEvent::PostIssueWork,
        path: "issue.branch",
        disclosure: None,
    },
    HookField {
        event: HookEvent::PostIssueWork,
        path: "issueOutcome.status",
        disclosure: None,
    },
    HookField {
        event: HookEvent::PostIssueWork,
        path: "issueOutcome.summary",
        disclosure: Some("how that work unit ended, in the loop's own summary of what changed"),
    },
    HookField {
        event: HookEvent::PostIssueWork,
        path: "issueOutcome.reason",
        disclosure: Some("or, when it could not complete, why"),
    },
];

/// The hook-channel disclosure sentences for `events`, in table order, with each event's
/// rows grouped under it.
///
/// Returns an empty vector when nothing declared carries anything of the
/// user's — an event whose every row is `None` renders no heading rather than
/// a heading with nothing under it.
#[must_use]
pub fn hook_disclosures_for(events: &[HookEvent]) -> Vec<(HookEvent, Vec<&'static str>)> {
    let mut grouped: Vec<(HookEvent, Vec<&'static str>)> = Vec::new();
    for field in HOOK_FIELDS {
        if !events.contains(&field.event) {
            continue;
        }
        let Some(sentence) = field.disclosure else {
            continue;
        };
        match grouped.last_mut() {
            Some((event, sentences)) if *event == field.event => sentences.push(sentence),
            _ => grouped.push((field.event, vec![sentence])),
        }
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every declared event has at least one row, so a hook a plugin can
    /// declare is never one the prompt is silent about.
    #[test]
    fn every_hook_event_has_rows() {
        for event in HookEvent::ALL {
            assert!(
                HOOK_FIELDS.iter().any(|field| field.event == event),
                "`{event}` is declarable and has no row in HOOK_FIELDS"
            );
        }
    }

    /// A disclosed row carries a sentence to disclose it in.
    #[test]
    fn every_disclosure_says_something() {
        for field in HOOK_FIELDS {
            assert!(
                field
                    .disclosure
                    .is_none_or(|sentence| !sentence.trim().is_empty()),
                "`{}.{}` is disclosed with no sentence to disclose it in",
                field.event,
                field.path
            );
        }
    }

    /// The grouping preserves table order and drops the events that disclose
    /// nothing of the user's.
    #[test]
    fn disclosures_group_by_event_in_table_order() {
        let grouped = hook_disclosures_for(&[HookEvent::Stop, HookEvent::PreToolUse]);
        let events: Vec<HookEvent> = grouped.iter().map(|(event, _)| *event).collect();
        assert_eq!(events, vec![HookEvent::PreToolUse, HookEvent::Stop]);
        assert!(
            grouped[0].1.iter().any(|s| s.contains("arguments in full")),
            "a PreToolUse observer is told it receives the tool's arguments: {:?}",
            grouped[0].1
        );
        assert!(hook_disclosures_for(&[]).is_empty());
    }
}
