//! Ambient agent attribution: which sub-agent a bus event belongs to (#1853).
//!
//! # Why a stack and not a slot
//!
//! This was one `Option<String>` on the bus's ambient context, saved and
//! restored by a guard. That is correct under strict LIFO nesting — a
//! grandchild entering and leaving inside its parent — and wrong the moment
//! two siblings overlap:
//!
//! ```text
//! A enters   prev = None          ambient = a
//! B enters   prev = Some(a)       ambient = b
//! A drops    restores None        ambient = NONE   ← b is still running
//! B drops    restores Some(a)     ambient = a      ← a finished long ago
//! ```
//!
//! Both wrong answers outlive the mistake: events emitted while B is still
//! working are attributed to nobody, and every one of the parent's remaining
//! events is attributed to a child that has already exited. Nothing ever puts
//! it back, because the slot has no memory of what it lost.
//!
//! A stack keyed by token fixes both, and the key is that a guard removes
//! **its own** entry rather than popping the top:
//!
//! ```text
//! A enters   [(1,a)]              current = a
//! B enters   [(1,a),(2,b)]        current = b
//! A drops    [(2,b)]              current = b      ← still B's work
//! B drops    []                   current = None   ← back to pre-entry
//! ```
//!
//! # Not live today, which is exactly why it is worth fixing now
//!
//! Both dispatchers build their host with no bus, so the guard is a no-op and
//! no ordering can be observed. It arms the instant anyone calls
//! `SubAgentHost::with_bus` — and `stella-serve` already attaches a bus to the
//! engine, so that is the obvious next step for someone. A latent corruption
//! is cheapest to fix while it is still latent: there is no migration, no
//! recorded telemetry to reinterpret, and no bug report to reconcile.

use super::HookBus;

/// Identifies one entry in the attribution stack.
///
/// Opaque and non-`Clone` on purpose: it is a capability to remove exactly the
/// entry that created it, and a second copy would let a guard remove an entry
/// twice — reintroducing, by hand, the pop-the-wrong-entry bug the stack
/// exists to prevent.
#[derive(Debug, PartialEq, Eq)]
pub struct AttributionToken(u64);

/// The ambient agent stack. Entries are `(token, agent id)`; the innermost
/// still-live entry is the current attribution.
#[derive(Debug, Default)]
pub(super) struct AgentStack {
    entries: Vec<(u64, String)>,
    next: u64,
}

impl AgentStack {
    fn push(&mut self, agent_id: String) -> AttributionToken {
        // Monotonic and never reused within a process, so a token cannot
        // address an entry pushed after the one it was minted for.
        self.next += 1;
        self.entries.push((self.next, agent_id));
        AttributionToken(self.next)
    }

    fn remove(&mut self, token: &AttributionToken) {
        self.entries.retain(|(id, _)| *id != token.0);
    }

    /// The current attribution: the most recently entered scope that has not
    /// yet been left. `None` once every scope has exited, which is the value
    /// the parent's own events want.
    pub(super) fn current(&self) -> Option<String> {
        self.entries.last().map(|(_, agent)| agent.clone())
    }
}

impl HookBus {
    /// Enter attribution for `agent_id`, returning the token that leaves it.
    ///
    /// Prefer [`crate::subagent::AgentAttribution`], which pairs this with its
    /// [`Drop`] so the exit survives a cancelled future or a panicking tool.
    /// This is the primitive underneath it.
    pub fn push_agent(&self, agent_id: String) -> AttributionToken {
        self.inner
            .context
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .agents
            .push(agent_id)
    }

    /// Leave the scope `token` names.
    ///
    /// Removes that entry wherever it sits, rather than popping the top: a
    /// sibling that entered later may still be running, and its attribution
    /// must survive this one exiting.
    pub fn drop_agent(&self, token: &AttributionToken) {
        self.inner
            .context
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .agents
            .remove(token);
    }

    /// The ambient agent id an event with no explicit attribution inherits.
    pub fn current_agent(&self) -> Option<String> {
        self.inner
            .context
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .agents
            .current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Witness (#1853).** Two overlapping scopes leave the ambient id at its
    /// pre-entry value, and the still-running one keeps its attribution while
    /// its sibling exits.
    ///
    /// On the save/restore slot this failed twice over: `current` read `None`
    /// between the drops (B was still working), and settled on `Some("a")`
    /// forever afterwards — attributing every later parent event to a child
    /// that had finished.
    #[test]
    fn overlapping_siblings_do_not_corrupt_the_ambient_agent() {
        let bus = HookBus::new("session-1653");
        assert_eq!(bus.current_agent(), None, "nothing entered yet");

        let a = bus.push_agent("a".to_string());
        let b = bus.push_agent("b".to_string());
        assert_eq!(bus.current_agent(), Some("b".to_string()));

        // A exits first — the non-LIFO order a slot cannot represent.
        bus.drop_agent(&a);
        assert_eq!(
            bus.current_agent(),
            Some("b".to_string()),
            "b is still running, so events between the drops are b's"
        );

        bus.drop_agent(&b);
        assert_eq!(
            bus.current_agent(),
            None,
            "both scopes are gone, so the parent's events are the parent's again"
        );
    }

    /// Strict nesting — the only case the slot got right — still behaves.
    #[test]
    fn nested_scopes_restore_their_parent() {
        let bus = HookBus::new("session-1653");
        let parent = bus.push_agent("parent".to_string());
        let child = bus.push_agent("child".to_string());
        assert_eq!(bus.current_agent(), Some("child".to_string()));

        bus.drop_agent(&child);
        assert_eq!(
            bus.current_agent(),
            Some("parent".to_string()),
            "a grandchild's exit must not un-attribute its parent"
        );
        bus.drop_agent(&parent);
        assert_eq!(bus.current_agent(), None);
    }

    /// Leaving a scope twice is inert.
    ///
    /// Reachable through a guard that is dropped after its bus has already
    /// been cleared, and the failure it must not have is removing somebody
    /// else's entry — which a pop-the-top implementation would do.
    #[test]
    fn dropping_a_scope_twice_does_not_disturb_a_live_sibling() {
        let bus = HookBus::new("session-1653");
        let a = bus.push_agent("a".to_string());
        let b = bus.push_agent("b".to_string());

        bus.drop_agent(&a);
        bus.drop_agent(&a);
        assert_eq!(
            bus.current_agent(),
            Some("b".to_string()),
            "a repeated exit must not consume the sibling's entry"
        );
        bus.drop_agent(&b);
        assert_eq!(bus.current_agent(), None);
    }

    /// Three-deep interleaving, exited in entry order — the worst ordering for
    /// a slot, and the one a fan-out of siblings actually produces.
    #[test]
    fn entry_order_exits_leave_the_innermost_survivor_attributed() {
        let bus = HookBus::new("session-1653");
        let a = bus.push_agent("a".to_string());
        let b = bus.push_agent("b".to_string());
        let c = bus.push_agent("c".to_string());

        bus.drop_agent(&a);
        assert_eq!(bus.current_agent(), Some("c".to_string()));
        bus.drop_agent(&b);
        assert_eq!(bus.current_agent(), Some("c".to_string()));
        bus.drop_agent(&c);
        assert_eq!(bus.current_agent(), None);
    }
}
