// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a turn dispatched and what it changed, observed once as the turn's own
//! events go past (#3552).
//!
//! # The hole this closes
//!
//! `crates/stella-cli/src/wrapper_plugin.rs` hands an installed plugin a
//! [`stella_plugin::TurnOutcome`], and two of its four fields were **always
//! empty** on that path: the engine's own `TurnOutcome` carries the answer and
//! the cost, and nothing else. A plugin reading `tools` or `changed_files`
//! therefore could not tell "the turn dispatched nothing and changed nothing"
//! from "this host does not report either", and a wrapper that gates its
//! evidence on "did the turn touch anything" graded every run as untouched.
//!
//! Both facts do exist, and both exist in exactly one place: **the turn's event
//! stream**. Tool names ride [`AgentEvent::ToolStart`]; the tree delta rides
//! [`AgentEvent::FileChange`], emitted at the turn boundary by
//! [`crate::turn_files`] from git's own `--numstat`. Neither survives the turn
//! in any other form — the renderer collects events only for
//! [`OutputFormat::Json`](crate::OutputFormat), and
//! `Durability::snapshot_worktree` is *consumed* by that boundary emit, so a
//! second call afterwards measures a tree that has already been snapshotted and
//! honestly reports nothing.
//!
//! So this is a **tap on the sending side**, installed by whoever wants the
//! facts and folded synchronously as each event is admitted. By the time
//! `run_turn` returns, every event it will ever send has crossed this fold.
//!
//! # Why a tap and not a re-measurement
//!
//! Re-measuring would need a second producer for each fact, and both would be
//! wrong in the way `crate::turn_files`'s own docs already argue at length:
//! inferring changed files from tool arguments is #2290, and a snapshot taken
//! after the boundary one is empty by construction. One producer, read twice.
//!
//! # What these facts claim
//!
//! Exactly what their events claim, and no more. `tools` is the calls the
//! engine dispatched, in call order — including a call that failed, because a
//! wrapper judging a turn is entitled to know it was attempted.
//! `changed_files` inherits [`AgentEvent::FileChange`]'s contract verbatim:
//! **observability, never evidence**. It answers *what changed during this
//! turn*, not *what the agent changed* — a user editing a file in another
//! window mid-turn is inside it and is indistinguishable from the agent's own
//! writes (#2873). A plugin is told the same thing the Files tab is told.
//!
//! It lives beside `agent.rs` rather than inside it because `agent.rs` sits
//! close to the 1500-line ratchet (AGENTS.md § "God files") — new logic lands
//! in a sibling.

use std::sync::{Arc, Mutex, PoisonError};

use stella_core::EventSender;
use stella_protocol::event::AgentEvent;

/// The turn facts gathered so far, behind one lock.
#[derive(Debug, Default)]
struct Gathered {
    tools: Vec<String>,
    changed_files: Vec<String>,
}

/// A fold over one turn's event stream, shared with the sender that feeds it.
///
/// Cheap to clone (it is an `Arc` inside): the observer has to outlive the
/// closure [`EventSender::from_fn`] holds, which is `'static`, while the caller
/// keeps its own handle to read afterwards.
#[derive(Debug, Clone, Default)]
pub(crate) struct TurnFacts {
    gathered: Arc<Mutex<Gathered>>,
}

impl TurnFacts {
    /// A fresh, empty observer.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Wrap `inner` so every event it carries is folded into these facts on its
    /// way past.
    ///
    /// The fold is synchronous and runs on the sending thread, which is what
    /// makes "the turn returned, so the facts are complete" true: an
    /// [`EventSender`] is synchronous by construction (invariant 2 keeps
    /// `stella-core` I/O-free), so there is no queue between the engine's send
    /// and this fold to still be draining.
    ///
    /// It never intercepts: the event is folded and then forwarded unchanged,
    /// and the wrapper reports the inner sender's own result. A fold that
    /// swallowed a send failure would make the renderer's exit invisible to the
    /// engine.
    pub(crate) fn observing(&self, inner: EventSender) -> EventSender {
        let facts = self.clone();
        EventSender::from_fn(move |event| {
            facts.observe(&event);
            inner.send(event)
        })
    }

    /// Fold one event.
    fn observe(&self, event: &AgentEvent) {
        let mut gathered = self.lock();
        match event {
            AgentEvent::ToolStart { call, .. } => gathered.tools.push(call.name.clone()),
            // The turn-boundary snapshot emits one event per path, so a repeat
            // would mean two producers measured the same turn. Keep first-seen
            // order and drop the duplicate rather than reporting a path twice:
            // a plugin counting paths must not see a number that depends on how
            // many surfaces reported them.
            AgentEvent::FileChange { path, .. }
                if !gathered.changed_files.iter().any(|seen| seen == path) =>
            {
                gathered.changed_files.push(path.clone());
            }
            _ => {}
        }
    }

    /// The tools this turn dispatched, in call order.
    pub(crate) fn tools(&self) -> Vec<String> {
        self.lock().tools.clone()
    }

    /// The workspace-relative paths this turn changed, in the order they were
    /// measured.
    pub(crate) fn changed_files(&self) -> Vec<String> {
        self.lock().changed_files.clone()
    }

    /// The gathered facts, recovering from a poisoned lock rather than
    /// panicking.
    ///
    /// Invariant 5: a panic while some other event was being folded must not
    /// turn every later read into a second panic. The two vectors' own
    /// invariants cannot be broken by an unwind — a push is atomic with respect
    /// to them — so the contents behind a poisoned guard are exactly as sound
    /// as before, and losing the whole report because one send panicked would
    /// be the silence this module exists to remove.
    fn lock(&self) -> std::sync::MutexGuard<'_, Gathered> {
        self.gathered.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::event::FileChangeKind;
    use stella_protocol::tool::ToolCall;

    fn tool_start(name: &str) -> AgentEvent {
        AgentEvent::ToolStart {
            call: ToolCall {
                call_id: format!("call-{name}"),
                name: name.to_string(),
                input: serde_json::json!({}),
            },
            sub_agent_id: None,
        }
    }

    fn file_change(path: &str) -> AgentEvent {
        AgentEvent::FileChange {
            path: path.to_string(),
            kind: FileChangeKind::Modified,
            added: 2,
            removed: 1,
            diff: None,
            minimal: true,
        }
    }

    /// **Witness.** The two facts a plugin reads are gathered from the turn's
    /// own stream, in call order, and every other event passes through
    /// untouched.
    #[test]
    fn the_turns_tools_and_changed_files_are_folded_as_they_pass() {
        let facts = TurnFacts::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sender = facts.observing(EventSender::new(tx));

        for event in [
            tool_start("read_file"),
            AgentEvent::TextDelta {
                delta: "thinking".into(),
            },
            tool_start("edit_file"),
            file_change("src/lib.rs"),
            file_change("README.md"),
        ] {
            sender.send(event).expect("the receiver is alive");
        }

        assert_eq!(facts.tools(), vec!["read_file", "edit_file"]);
        assert_eq!(facts.changed_files(), vec!["src/lib.rs", "README.md"]);

        drop(sender);
        let mut forwarded = 0;
        while rx.try_recv().is_ok() {
            forwarded += 1;
        }
        assert_eq!(forwarded, 5, "the tap forwards, it never intercepts");
    }

    /// A turn that ran and touched nothing gathers empty lists — which the
    /// driver reports as `Some(vec![])`, the answer that is *not* "unknown".
    #[test]
    fn a_turn_that_touched_nothing_gathers_empty_rather_than_missing() {
        let facts = TurnFacts::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let sender = facts.observing(EventSender::new(tx));
        sender
            .send(AgentEvent::TurnComplete {
                model: "claude-opus-5".into(),
                cost_usd: 0.01,
            })
            .expect("the receiver is alive");

        assert!(facts.tools().is_empty());
        assert!(facts.changed_files().is_empty());
    }

    /// Two producers measuring one turn must not double a path.
    #[test]
    fn a_repeated_path_is_reported_once() {
        let facts = TurnFacts::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let sender = facts.observing(EventSender::new(tx));
        sender.send(file_change("src/lib.rs")).expect("alive");
        sender.send(file_change("src/lib.rs")).expect("alive");
        assert_eq!(facts.changed_files(), vec!["src/lib.rs"]);
    }
}
