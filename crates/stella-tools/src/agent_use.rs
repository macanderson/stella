//! Per-session agent-usage telemetry: one [`AgentUseEvent`] per invocation
//! of an installed agent definition, recorded at the moment the definition
//! is applied to a turn and **drained** once per execution into the store's
//! `agent_uses` table (see `stella-cli`'s `record_execution_end`).
//!
//! The registry holds one [`AgentUseLedger`] behind a mutex, invocation
//! sites call `ToolRegistry::record_agent_use`, and the persistence layer
//! takes everything recorded since the previous drain. Agent uses are an
//! **event log**: every invocation is its own row, because the unit of
//! analysis is "agent-version X was invoked by execution Y at time T",
//! never an aggregate.
//!
//! # Two writers, two name spaces
//!
//! [`AgentUseEvent::kind`] records which of them minted `agent` (#3822), and
//! the two are not interchangeable:
//!
//! - [`AgentUseKind::Definition`] — `stella-cli`'s deck, recording an
//!   **installed agent definition** by name (`reviewer`) at its *pinned*
//!   version (see `stella-cli::agents_installed` for the on-disk versioning
//!   scheme), so the telemetry can attribute outcomes to the exact definition
//!   text that ran. The same name recurs across sessions and is worth
//!   counting.
//! - [`AgentUseKind::Delegation`] — the `task` tool ([`crate::subagent`]),
//!   recording the **child id it minted** from the model's own 3-6 word
//!   description (`find-retry-policy-2`), always version 1: a `task` child is
//!   minted at the call site rather than loaded from a versioned file, so
//!   there is no pinned version to carry. Each id is unique by construction.
//!
//! Grouping the two together made an agent invoked eight times and eight
//! separate delegations render identically, which is why the kind is recorded
//! by the writer that knows it rather than guessed downstream from a spelling.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// Which writer minted an [`AgentUseEvent`]'s `agent` name — see the module
/// docs for what each name space contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentUseKind {
    /// An installed agent definition, invoked by name at a pinned version.
    Definition,
    /// One `task` delegation, under the child id minted for it.
    Delegation,
}

impl AgentUseKind {
    /// The stored token, matching the `agent_uses.kind` `CHECK` in
    /// `stella-store`'s DDL. Named here rather than shared as a type because
    /// `stella-tools` and `stella-store` are siblings, neither depending on
    /// the other; `stella-cli` maps one to the other where it drains.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Definition => "definition",
            Self::Delegation => "delegation",
        }
    }
}

/// A shared handle on the session's ledger.
///
/// The registry owns one and hands clones out — to
/// [`crate::ctx::ToolCtx`] per call, so a tool can attribute its own
/// delegations (#3807). Shared rather than borrowed because the context
/// crosses an `async_trait` boundary on every dispatch, exactly like the
/// hook bus beside it.
pub type AgentUseLedgerHandle = Arc<Mutex<AgentUseLedger>>;

/// One agent invocation — an installed definition applied to a turn, or one
/// `task` delegation. Which one is [`AgentUseEvent::kind`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentUseEvent {
    /// The agent's name, in the name space [`AgentUseEvent::kind`] names.
    pub agent: String,
    /// The pinned version of the definition at invocation time (1-based;
    /// an un-versioned agent, and every delegation, is version 1).
    pub version: u32,
    /// Why / how it was invoked — free text, kept short by the recorder
    /// (e.g. a snipped task line). Empty when no reason was available.
    pub reason: String,
    /// Which writer this came from. Defaulted on deserialize so a ledger
    /// serialized by an older build reads as what all of them were.
    #[serde(default = "definition_kind")]
    pub kind: AgentUseKind,
}

/// The `serde` default for [`AgentUseEvent::kind`] — see that field.
fn definition_kind() -> AgentUseKind {
    AgentUseKind::Definition
}

/// The session's agent-use ledger. One instance lives behind the registry's
/// mutex; [`AgentUseLedger::drain`] hands the accumulated events to the
/// per-execution persistence step and resets the ledger, so each execution
/// records exactly the invocations that happened on its watch.
#[derive(Debug, Default)]
pub struct AgentUseLedger {
    events: Vec<AgentUseEvent>,
}

impl AgentUseLedger {
    /// Append one invocation.
    pub fn record(&mut self, event: AgentUseEvent) {
        self.events.push(event);
    }

    /// Take every event recorded since the last drain, in record order,
    /// leaving the ledger empty.
    pub fn drain(&mut self) -> Vec<AgentUseEvent> {
        std::mem::take(&mut self.events)
    }

    /// Events currently pending a drain (test/inspection accessor).
    pub fn pending(&self) -> usize {
        self.events.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(agent: &str, version: u32, reason: &str) -> AgentUseEvent {
        AgentUseEvent {
            agent: agent.to_string(),
            version,
            reason: reason.to_string(),
            kind: AgentUseKind::Definition,
        }
    }

    /// A ledger serialized by a build that predates the discriminator reads as
    /// what all of its rows were — the deck was the only writer then.
    #[test]
    fn an_event_without_a_kind_deserializes_as_a_definition() {
        let event: AgentUseEvent =
            serde_json::from_str(r#"{"agent":"reviewer","version":2,"reason":""}"#).expect("parse");
        assert_eq!(event.kind, AgentUseKind::Definition);
        assert_eq!(AgentUseKind::Delegation.as_str(), "delegation");
    }

    #[test]
    fn drain_returns_events_in_record_order_and_resets() {
        let mut ledger = AgentUseLedger::default();
        ledger.record(ev("reviewer", 2, "review the diff"));
        ledger.record(ev("reviewer", 2, "second pass"));
        ledger.record(ev("planner", 1, ""));
        assert_eq!(ledger.pending(), 3);

        let drained = ledger.drain();
        assert_eq!(
            drained,
            vec![
                ev("reviewer", 2, "review the diff"),
                ev("reviewer", 2, "second pass"),
                ev("planner", 1, ""),
            ],
            "an event log, never aggregated — repeat invocations stay distinct rows"
        );
        assert_eq!(ledger.pending(), 0, "drain resets the ledger");
        assert!(ledger.drain().is_empty(), "a second drain has nothing");
    }
}
