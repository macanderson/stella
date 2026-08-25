// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The structural rules an emitted [`AgentEvent`] stream obeys, and the reader
//! that checks a recording against them.
//!
//! `docs/wire/agentevent.schema.json` describes what one *event* may look like,
//! and `stella-protocol`'s own contract tests prove every event shape fits it.
//! Neither says anything about what a *stream* may look like: JSON Schema has
//! no way to express "a `tool_start` is answered later by a `tool_result` with
//! the same `call_id`", "`run_complete` is last", or "spend never goes
//! backwards". Those four rules are what [`validate_stream`] checks, and
//! between #3865 — which deleted `stella_pipeline::replay` and took the
//! validator with it — and this module, nothing in the tree checked them at
//! all (#4585).
//!
//! ## Why here and not in `stella-protocol`
//!
//! #4585 recommended the protocol crate, on the grounds that these are
//! properties of the event stream it owns. That crate's own boundary rule
//! settles it the other way: it admits "a serde type — or a field on one — …
//! plus at most a total, allocation-light helper over that type's own data",
//! and refuses "a `match` that decides what the program *does* next", naming
//! `stella-core` as the home for "decision logic over events"
//! (`crates/stella-protocol/README.md`). [`validate_stream`] is a fold over a
//! whole stream with a rank table, a legal-back-edge rule and a violation
//! vocabulary of its own — decision logic by that test, not a helper on a
//! type. It lands here, beside [`crate::loop_detect`] and
//! [`crate::compaction`], as what AGENTS.md's `no I/O in the engine` rule asks
//! for: a plain synchronous function over owned data, with no I/O in it.
//!
//! ## The violations are typed
//!
//! The deleted validator reported `StreamViolation { index, reason: String }`,
//! so a caller wanting to branch on *which* rule broke had to match on prose —
//! and its own conformance test did exactly that, asserting on substrings of
//! the reason. [`StreamViolation`] is an enum instead: a caller branches on
//! the case, and the test that proves every rule has a negative case is a
//! `match` the compiler completes rather than a list somebody remembered to
//! extend.

use stella_protocol::AgentEvent;
use stella_protocol::event::{StageKind, StageScope};
use stella_protocol::journal::StampedEvent;

/// A structural rule an event stream broke.
///
/// Reported as a list rather than a first-failure `Result`: one pass over a
/// recording names everything wrong with it, which is what a conformance gate
/// over a fixture wants to print.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum StreamViolation {
    /// Two consecutive run-scoped `Stage` events moved backwards through the
    /// canonical order along an edge that is not one of the revise back-edges.
    #[error("event {index}: illegal stage transition {from:?} -> {to:?}")]
    IllegalStageTransition {
        /// Index of the `Stage` event that moved illegally.
        index: usize,
        /// The stage in effect before it.
        from: StageKind,
        /// The stage it moved to.
        to: StageKind,
    },
    /// A `tool_start` (or an `ask_user` question) that no later `tool_result`
    /// answered. Attributed to the index the call *opened* at, because that is
    /// the event a reader has to go and look at.
    #[error("event {index}: tool_start for `{call_id}` never matched by a tool_result")]
    UnmatchedToolStart {
        /// Index of the unanswered `tool_start`.
        index: usize,
        /// The call id that stayed open.
        call_id: String,
    },
    /// A `tool_result` whose `call_id` no earlier `tool_start` opened.
    #[error("event {index}: tool_result for `{call_id}` with no preceding tool_start")]
    OrphanToolResult {
        /// Index of the orphaned `tool_result`.
        index: usize,
        /// The call id it claims to answer.
        call_id: String,
    },
    /// A second (or later) `run_complete`. A run terminates once.
    #[error("event {index}: more than one run_complete event; a run terminates once")]
    RepeatedRunComplete {
        /// Index of the surplus `run_complete`.
        index: usize,
    },
    /// Something followed the run's `run_complete`.
    #[error("event {index}: run_complete is not the last event; nothing may follow it")]
    EventAfterRunComplete {
        /// Index of the `run_complete` that turned out not to be last.
        index: usize,
    },
    /// A `budget_tick` reported less cumulative spend than an earlier one.
    #[error(
        "event {index}: budget spent went backwards: {spent_usd:.6} < previous {previous_usd:.6}"
    )]
    BudgetWentBackwards {
        /// Index of the tick that went backwards.
        index: usize,
        /// What it reported.
        spent_usd: f64,
        /// What the previous tick reported.
        previous_usd: f64,
    },
}

impl StreamViolation {
    /// The index in the stream this violation is attributed to.
    #[must_use]
    pub fn index(&self) -> usize {
        match self {
            Self::IllegalStageTransition { index, .. }
            | Self::UnmatchedToolStart { index, .. }
            | Self::OrphanToolResult { index, .. }
            | Self::RepeatedRunComplete { index }
            | Self::EventAfterRunComplete { index }
            | Self::BudgetWentBackwards { index, .. } => *index,
        }
    }
}

/// Canonical rank of a stage in the one-run data flow.
///
/// Forward motion is any non-decreasing rank; the only legal backward motion is
/// the revise/best-of-N loop back to `Execute`.
fn stage_rank(stage: StageKind) -> u8 {
    match stage {
        StageKind::Triage => 0,
        StageKind::ContextRecall => 1,
        // Research is demand-driven pre-plan evidence (#1778): triage names the
        // questions, so it can only follow triage, and its findings feed the
        // planner, so it must precede Plan.
        StageKind::Research => 2,
        StageKind::Plan => 3,
        StageKind::ScopeReview => 4,
        StageKind::Execute => 5,
        // Witness authoring is demand-driven: it runs AFTER execution, once the
        // warrant has read the executed diff and found something to prove. The
        // revise back-edges land on Execute below it — re-execution never
        // re-authors.
        StageKind::Witness => 6,
        StageKind::Verify => 7,
        StageKind::Verdict => 8,
        // Reflect is post-verdict self-reflection, before context write-back.
        StageKind::Reflect => 9,
        StageKind::ContextWrite => 10,
        StageKind::Complete => 11,
    }
}

/// Whether a transition between two consecutive run-scoped `Stage` events is
/// legal: a forward (or same-rank) move, or the revise back-edge from
/// `Verify`/`Verdict` to `Execute` — the revision loop and best-of-N both
/// re-execute the work.
#[must_use]
pub fn stage_transition_legal(from: StageKind, to: StageKind) -> bool {
    if stage_rank(to) >= stage_rank(from) {
        return true;
    }
    matches!(
        (from, to),
        (StageKind::Verify, StageKind::Execute) | (StageKind::Verdict, StageKind::Execute)
    )
}

/// Check a stream against the four structural rules.
///
/// 1. **Legal stage ordering** — consecutive run-scoped `Stage` events move
///    forward in the canonical order or take a known revise back-edge
///    (`Verify`/`Verdict` → `Execute`).
/// 2. **Tool pairing** — every `tool_start` has a later matching `tool_result`
///    (same `call_id`), and no `tool_result` appears without one.
/// 3. **A single terminal `run_complete`** — at most one, and if present it is
///    the last event.
/// 4. **Monotonic budget** — `budget_tick.spent_usd` never decreases.
///
/// An empty result means the stream is well-formed.
#[must_use]
pub fn validate_stream(events: &[AgentEvent]) -> Vec<StreamViolation> {
    let mut violations = Vec::new();
    validate_stage_ordering(events, &mut violations);
    validate_tool_pairing(events, &mut violations);
    validate_terminal(events, &mut violations);
    validate_budget_monotonic(events, &mut violations);
    violations
}

/// The run's stages walk one legal order.
///
/// Two kinds of `Stage` event are not judged here. Turn-scoped
/// ones are the engine's own phases, several per run, and folding them into
/// this single order would report a violation at every turn boundary (#3398).
/// A stage whose name the host does not know — a name an installed plugin
/// contributed, which [`stella_protocol::StageName`] admits by design — has no
/// rank in the table above, so it is neither a violation nor a new baseline:
/// judging it would mean inventing an order nobody declared.
fn validate_stage_ordering(events: &[AgentEvent], out: &mut Vec<StreamViolation>) {
    let mut last_stage: Option<StageKind> = None;
    for (index, event) in events.iter().enumerate() {
        let AgentEvent::Stage {
            name,
            scope: StageScope::Run,
        } = event
        else {
            continue;
        };
        let Some(kind) = name.kind() else {
            continue;
        };
        if let Some(from) = last_stage
            && !stage_transition_legal(from, kind)
        {
            out.push(StreamViolation::IllegalStageTransition {
                index,
                from,
                to: kind,
            });
        }
        last_stage = Some(kind);
    }
}

fn validate_tool_pairing(events: &[AgentEvent], out: &mut Vec<StreamViolation>) {
    // Open calls keyed by call_id → the index they started at.
    let mut open: Vec<(&str, usize)> = Vec::new();
    for (index, event) in events.iter().enumerate() {
        match event {
            AgentEvent::ToolStart { call, .. } => open.push((&call.call_id, index)),
            // `AskUser` is the `ask_user` tool's question; its answer returns as
            // an ordinary `ToolResult` keyed by this `id`, so it opens a pending
            // call exactly like a `ToolStart`.
            AgentEvent::AskUser { id, .. } => open.push((id, index)),
            AgentEvent::ToolResult { call_id, .. } => {
                match open.iter().position(|(id, _)| *id == call_id) {
                    Some(pos) => {
                        open.remove(pos);
                    }
                    None => out.push(StreamViolation::OrphanToolResult {
                        index,
                        call_id: call_id.clone(),
                    }),
                }
            }
            _ => {}
        }
    }
    for (call_id, index) in open {
        out.push(StreamViolation::UnmatchedToolStart {
            index,
            call_id: call_id.to_string(),
        });
    }
}

/// The run terminates exactly once, on `run_complete`, and nothing follows it.
///
/// The terminator is `run_complete` rather than `complete`: the engine emits
/// `TurnComplete` per turn and a wrapped run emits several, so counting those
/// would report a violation for every extra turn — the expected shape rather
/// than a defect (#3379).
fn validate_terminal(events: &[AgentEvent], out: &mut Vec<StreamViolation>) {
    let mut terminators = events
        .iter()
        .enumerate()
        .filter(|(_, event)| matches!(event, AgentEvent::RunComplete { .. }))
        .map(|(index, _)| index);
    let Some(first) = terminators.next() else {
        return;
    };
    if first != events.len() - 1 {
        out.push(StreamViolation::EventAfterRunComplete { index: first });
    }
    for index in terminators {
        out.push(StreamViolation::RepeatedRunComplete { index });
    }
}

fn validate_budget_monotonic(events: &[AgentEvent], out: &mut Vec<StreamViolation>) {
    let mut previous: Option<f64> = None;
    for (index, event) in events.iter().enumerate() {
        let AgentEvent::BudgetTick { spent_usd, .. } = event else {
            continue;
        };
        if let Some(previous_usd) = previous
            && *spent_usd + f64::EPSILON < previous_usd
        {
            out.push(StreamViolation::BudgetWentBackwards {
                index,
                spent_usd: *spent_usd,
                previous_usd,
            });
        }
        previous = Some(*spent_usd);
    }
}

/// A recording that could not be read at all — distinct from one that read
/// cleanly and broke the rules.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JsonlError {
    /// A non-final line failed to parse. Interior corruption is fatal; only a
    /// torn *tail* is tolerated (see [`parse_jsonl`]).
    #[error("malformed event on line {line} (1-indexed): {message}")]
    MalformedLine {
        /// 1-indexed line number, counting blank lines.
        line: usize,
        /// What `serde_json` said about it.
        message: String,
    },
}

/// Read an event-stream JSONL document — one journal line per line — into the
/// events it carries.
///
/// Lines are parsed as [`StampedEvent`], so a recorded `stella-events.jsonl`
/// (whose sink stamps each line with `ts`) and a bare event stream both read;
/// the stamp is a fact about the write and no structural rule is about it, so
/// it is dropped here.
///
/// A single torn *final* line — the signature of a writer killed mid-line — is
/// dropped rather than failing the whole read. A malformed *interior* line is
/// real damage and is a [`JsonlError::MalformedLine`].
///
/// A line carrying an event type this build does not recognize is **not**
/// malformed: it reads as [`AgentEvent::Unknown`] with its payload preserved,
/// which is what lets a recording from a newer Stella be checked by an older
/// one.
///
/// # Errors
///
/// [`JsonlError::MalformedLine`] when an interior line is not a readable event.
pub fn parse_jsonl(input: &str) -> Result<Vec<AgentEvent>, JsonlError> {
    let lines: Vec<(usize, &str)> = input
        .lines()
        .enumerate()
        .map(|(i, line)| (i + 1, line.trim()))
        .filter(|(_, line)| !line.is_empty())
        .collect();

    let mut events = Vec::with_capacity(lines.len());
    let last = lines.len().saturating_sub(1);
    for (position, (line, content)) in lines.iter().enumerate() {
        match serde_json::from_str::<StampedEvent>(content) {
            Ok(stamped) => events.push(stamped.event),
            // A torn tail: return what read cleanly.
            Err(_) if position == last => break,
            Err(err) => {
                return Err(JsonlError::MalformedLine {
                    line: *line,
                    message: err.to_string(),
                });
            }
        }
    }
    Ok(events)
}

/// Write a stream as JSONL, one event per line — the inverse of
/// [`parse_jsonl`], for building a fixture or corrupting one in a test.
///
/// Unstamped: the stamp belongs to a sink that owns a clock, and this crate
/// owns none (`stella_protocol::journal::stamped_line` is the write side that
/// does).
#[must_use]
pub fn to_jsonl(events: &[AgentEvent]) -> String {
    let mut out = String::new();
    for event in events {
        // `AgentEvent` carries no non-string map key and no non-finite float
        // this crate can introduce, so this cannot fail; the `expect` states
        // that rule rather than hiding a real fallible path.
        let line = serde_json::to_string(event).expect("AgentEvent is always serializable");
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Read a recording and check it — [`parse_jsonl`] then [`validate_stream`].
///
/// The three outcomes stay distinct: `Ok(vec![])` is a conforming
/// recording, `Ok(violations)` is one that read cleanly and breaks the rules,
/// and `Err` is one that is not a readable recording at all. A checker that
/// collapsed the third into the first would report a clean bill of health for a
/// file it never managed to look at.
///
/// # Errors
///
/// [`JsonlError::MalformedLine`] when an interior line is not a readable event.
pub fn conform_jsonl(input: &str) -> Result<Vec<StreamViolation>, JsonlError> {
    Ok(validate_stream(&parse_jsonl(input)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::{BudgetMode, ToolCall, ToolOutput};

    /// `AgentEvent` carries no `PartialEq` — it holds a
    /// `serde_json::Value` payload on its forward-compatible arm — so two
    /// streams are compared as the bytes they serialize to, which is the
    /// equality a recording actually has.
    fn same_stream(left: &[AgentEvent], right: &[AgentEvent]) {
        assert_eq!(to_jsonl(left), to_jsonl(right));
    }

    fn stage(kind: StageKind) -> AgentEvent {
        AgentEvent::Stage {
            name: kind.into(),
            scope: StageScope::Run,
        }
    }

    fn tick(spent_usd: f64) -> AgentEvent {
        AgentEvent::BudgetTick {
            spent_usd,
            limit_usd: None,
            mode: BudgetMode::Observed,
            session_spent_usd: None,
            session_limit_usd: None,
            deadline_remaining_ms: None,
        }
    }

    fn tool_start(call_id: &str) -> AgentEvent {
        AgentEvent::ToolStart {
            call: ToolCall {
                call_id: call_id.to_string(),
                name: "edit_file".to_string(),
                input: serde_json::json!({}),
            },
            sub_agent_id: None,
        }
    }

    fn tool_result(call_id: &str) -> AgentEvent {
        AgentEvent::ToolResult {
            call_id: call_id.to_string(),
            output: ToolOutput::Ok {
                content: "edited".to_string(),
                data: None,
            },
            duration_ms: 1,
            speculated: false,
            sub_agent_id: None,
        }
    }

    fn run_complete() -> AgentEvent {
        AgentEvent::RunComplete {
            model: "m".to_string(),
            cost_usd: 0.002,
        }
    }

    /// The shape every negative case below is a single mutation of.
    fn conforming() -> Vec<AgentEvent> {
        vec![
            stage(StageKind::Triage),
            tick(0.0001),
            stage(StageKind::Execute),
            tool_start("call_1"),
            tool_result("call_1"),
            tick(0.0002),
            stage(StageKind::Complete),
            run_complete(),
        ]
    }

    #[test]
    fn a_well_formed_stream_has_no_violations() {
        assert_eq!(validate_stream(&conforming()), vec![]);
    }

    #[test]
    fn the_revise_back_edge_is_legal_and_every_other_backward_edge_is_not() {
        assert!(stage_transition_legal(
            StageKind::Verify,
            StageKind::Execute
        ));
        assert!(stage_transition_legal(
            StageKind::Verdict,
            StageKind::Execute
        ));
        assert!(stage_transition_legal(StageKind::Plan, StageKind::Plan));
        assert!(!stage_transition_legal(
            StageKind::Complete,
            StageKind::Triage
        ));
        assert!(!stage_transition_legal(StageKind::Verify, StageKind::Plan));
    }

    #[test]
    fn a_backward_stage_jump_is_a_violation() {
        let mut events = conforming();
        events.insert(events.len() - 1, stage(StageKind::Triage));
        assert!(matches!(
            validate_stream(&events).as_slice(),
            [StreamViolation::IllegalStageTransition {
                from: StageKind::Complete,
                to: StageKind::Triage,
                ..
            }]
        ));
    }

    /// The open stage vocabulary (#3398) means a plugin may contribute a name
    /// this build has never heard of. It has no rank, so it must be passed
    /// over — judged, it would violate the order on the way in and again on the
    /// way out, and the rank it was compared against would be invented.
    #[test]
    fn a_contributed_stage_name_is_passed_over_rather_than_ranked() {
        let mut events = conforming();
        events.insert(
            2,
            AgentEvent::Stage {
                name: stella_protocol::StageName::new("quantum_reticulation"),
                scope: StageScope::Run,
            },
        );
        assert_eq!(validate_stream(&events), vec![]);
    }

    /// Turn-scoped stages are the engine's own phases and repeat per turn;
    /// folding them into the run order would flag every turn boundary.
    #[test]
    fn turn_scoped_stages_are_not_judged_against_the_run_order() {
        let mut events = conforming();
        events.insert(
            2,
            AgentEvent::Stage {
                name: StageKind::Complete.into(),
                scope: StageScope::Turn,
            },
        );
        assert_eq!(validate_stream(&events), vec![]);
    }

    #[test]
    fn a_dropped_tool_result_leaves_the_call_open() {
        let mut events = conforming();
        events.remove(4);
        assert!(matches!(
            validate_stream(&events).as_slice(),
            [StreamViolation::UnmatchedToolStart { call_id, .. }] if call_id == "call_1"
        ));
    }

    #[test]
    fn a_tool_result_with_no_start_is_an_orphan() {
        let mut events = conforming();
        events.remove(3);
        assert!(matches!(
            validate_stream(&events).as_slice(),
            [StreamViolation::OrphanToolResult { call_id, .. }] if call_id == "call_1"
        ));
    }

    /// `ask_user` opens a pending call the same way `tool_start` does — its
    /// answer comes back as an ordinary `tool_result` keyed by the question id.
    #[test]
    fn an_answered_ask_user_question_pairs_like_a_tool_call() {
        let mut events = conforming();
        events.insert(
            3,
            AgentEvent::AskUser {
                id: "q_1".to_string(),
                question: "which?".to_string(),
                options: vec![],
            },
        );
        events.insert(4, tool_result("q_1"));
        assert_eq!(validate_stream(&events), vec![]);
    }

    #[test]
    fn a_second_run_complete_is_a_violation() {
        let mut events = conforming();
        events.push(run_complete());
        let violations = validate_stream(&events);
        assert!(
            violations
                .iter()
                .any(|v| matches!(v, StreamViolation::RepeatedRunComplete { .. })),
            "{violations:?}"
        );
    }

    #[test]
    fn an_event_after_run_complete_is_a_violation() {
        let mut events = conforming();
        events.push(AgentEvent::Text {
            text: "one more thing".to_string(),
        });
        assert!(matches!(
            validate_stream(&events).as_slice(),
            [StreamViolation::EventAfterRunComplete { .. }]
        ));
    }

    #[test]
    fn spend_going_backwards_is_a_violation() {
        let mut events = conforming();
        events[5] = tick(0.00001);
        assert!(matches!(
            validate_stream(&events).as_slice(),
            [StreamViolation::BudgetWentBackwards { .. }]
        ));
    }

    #[test]
    fn a_stream_round_trips_through_jsonl() {
        let events = conforming();
        same_stream(&parse_jsonl(&to_jsonl(&events)).unwrap(), &events);
    }

    /// A stamped recording is the shape that actually exists on disk.
    #[test]
    fn a_stamped_recording_reads_as_the_events_it_carries() {
        let line = stella_protocol::journal::stamped_line(&run_complete(), 1_754_582_400_123)
            .expect("a run_complete serializes");
        same_stream(&parse_jsonl(&line).unwrap(), &[run_complete()]);
    }

    #[test]
    fn a_torn_final_line_is_dropped_rather_than_failing_the_read() {
        let mut recording = to_jsonl(&conforming());
        recording.push_str(r#"{"type":"run_complete","model":"m","cost_"#);
        same_stream(&parse_jsonl(&recording).unwrap(), &conforming());
    }

    #[test]
    fn a_malformed_interior_line_is_an_error_not_a_clean_bill_of_health() {
        let recording = to_jsonl(&conforming());
        let half = recording.len() / 2;
        let split = recording[..half].rfind('\n').unwrap() + 1;
        let mut torn = recording.clone();
        torn.insert_str(split, "{ not valid json }\n");
        assert!(matches!(
            conform_jsonl(&torn),
            Err(JsonlError::MalformedLine { .. })
        ));
    }

    #[test]
    fn an_unrecognized_event_type_is_read_rather_than_rejected() {
        let recording = "{\"type\":\"quantum_reticulation\",\"splines\":[\"alpha\"]}\n\
                         {\"type\":\"run_complete\",\"model\":\"m\",\"cost_usd\":0.002}\n";
        let events = parse_jsonl(recording).expect("a newer stella's event is not malformed");
        assert!(events[0].is_unknown());
        assert_eq!(validate_stream(&events), vec![]);
    }
}
