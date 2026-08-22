// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! **The agent-to-agent witness for `ask_question` (#4212).**
//!
//! The design claim this feature rests on is that a delegated sub-agent can
//! ask its driver a question *by construction* — no branch in the tool, no
//! second registration path, no special case. The mechanism is one flag:
//! `stella_core::ports::ReadOnlyTools` filters a child's tool surface on
//! `schema.read_only`, and `ask_question` declares it truthfully.
//!
//! A claim of that shape is exactly the kind this repository refuses to take
//! on a doc comment's word. `stella-tools`' own unit tests exercise the tool
//! against a broker directly, which proves nothing about the wrapper a child
//! actually runs behind: a `read_only: false` here would leave every one of
//! those tests green while the feature was structurally unreachable from the
//! only agent that most needs it.
//!
//! So this drives the real composition — a `ToolRegistry` wrapped in the real
//! `ReadOnlyTools`, dispatched through the real `ToolExecutor` port — and
//! asserts both halves: that the child can see and call the tool, and that
//! the question it raises is attributed to the child rather than to the
//! top-level turn.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use stella_core::bus::HookBus;
use stella_core::ports::{ReadOnlyTools, ToolExecutor};
use stella_core::subagent::AgentAttribution;
use stella_protocol::tool::ToolOutput;
use stella_protocol::{Answer, QuestionOutcome, QuestionRequest};
use stella_tools::ToolRegistry;
use stella_tools::registry::question::QuestionResponder;

/// Records every question it is asked and answers each one the same way.
struct Recording {
    seen: Mutex<Vec<QuestionRequest>>,
}

#[async_trait]
impl QuestionResponder for Recording {
    async fn respond(&self, request: &QuestionRequest) -> QuestionOutcome {
        self.seen.lock().unwrap().push(request.clone());
        QuestionOutcome::Answered {
            answers: request
                .questions
                .iter()
                .map(|q| Answer {
                    header: q.header.clone(),
                    question: q.question.clone(),
                    chosen: vec![q.options[0].label.clone()],
                    note: Some("answered by the driver".into()),
                })
                .collect(),
        }
    }
}

fn call() -> serde_json::Value {
    json!({
        "questions": [{
            "header": "Scope",
            "question": "Should the child widen the search to the tests?",
            "options": [
                { "label": "Yes, include tests" },
                { "label": "No, source only" }
            ]
        }]
    })
}

fn registry_with_driver() -> (ToolRegistry, Arc<Recording>) {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    // The registry outlives the TempDir handle here on purpose: nothing in
    // this test touches the filesystem, and leaking the directory keeps the
    // test from depending on drop order between the two.
    let root = workspace.keep();
    let registry = ToolRegistry::new(root);
    let responder = Arc::new(Recording {
        seen: Mutex::new(Vec::new()),
    });
    registry.attach_question_responder(responder.clone(), Duration::from_secs(5));
    (registry, responder)
}

/// **The witness.** A child running behind `ReadOnlyTools` — the exact
/// wrapper `delegate` puts every sub-agent behind — can both *see* and *call*
/// `ask_question`.
///
/// The `delegate` half of the assertion is the control: it proves the wrapper
/// is genuinely filtering rather than passing everything through, so the
/// first half means what it says.
#[tokio::test]
async fn a_child_behind_read_only_tools_can_see_and_call_ask_question() {
    let (registry, responder) = registry_with_driver();
    let child_surface = ReadOnlyTools::new(&registry);

    let names: Vec<String> = child_surface
        .schemas()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert!(
        names.iter().any(|n| n == "ask_question"),
        "a delegated child cannot ask its driver anything: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "delegate"),
        "the wrapper is not filtering at all, so the assertion above proves nothing: {names:?}"
    );

    let out = child_surface.execute("ask_question", &call()).await;
    let ToolOutput::Ok { content, .. } = out else {
        panic!("a child's question must reach the attached driver: {out:?}");
    };
    assert!(content.contains("Yes, include tests"), "{content}");
    assert!(
        content.contains("note: answered by the driver"),
        "the note must survive the wrapper: {content}"
    );
    assert_eq!(responder.seen.lock().unwrap().len(), 1);
}

/// The second half: the driver is told **whose** question it is reading.
///
/// Attribution comes from the bus's agent stack, which the sub-agent
/// primitive maintains around every child turn — so a child's question
/// carries the child's id and a top-level turn's carries none. Without this
/// a driver answering a fanned-out delegation could not tell which of three
/// children was asking, which is the whole difference between a question and
/// an interruption.
///
/// Note what is *not* asserted: that a model could set this. It cannot —
/// `asker` is stamped by the runtime from state no tool input is consulted
/// for, which is what stops a child claiming to be its parent.
#[tokio::test]
async fn a_childs_question_is_attributed_to_the_child_not_the_turn() {
    let (registry, responder) = registry_with_driver();
    let bus = HookBus::new("ses-a2a-witness");
    registry.attach_bus(bus.clone());

    // Top-level: no agent is entered, so the question is the driver's own.
    let out = registry.execute("ask_question", &call()).await;
    assert!(!out.is_error(), "{out:?}");

    // Inside a child's attribution scope — the guard the sub-agent primitive
    // holds for the life of a child turn.
    {
        let _child = AgentAttribution::enter(Some(&bus), "research-child");
        let out = registry.execute("ask_question", &call()).await;
        assert!(!out.is_error(), "{out:?}");
    }

    // ...and the scope closes when the child's turn ends.
    let out = registry.execute("ask_question", &call()).await;
    assert!(!out.is_error(), "{out:?}");

    let seen = responder.seen.lock().unwrap();
    let askers: Vec<Option<&str>> = seen.iter().map(|r| r.asker.as_deref()).collect();
    assert_eq!(
        askers,
        vec![None, Some("research-child"), None],
        "the question must name the agent that raised it, and only while that agent is running"
    );
}
