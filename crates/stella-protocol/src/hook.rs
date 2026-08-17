//! The lifecycle-hook vocabulary — the one home for [`HookEvent`] (#3310).
//!
//! A hook event names a point in the turn loop where something outside the
//! engine gets a say. Three parties spell that vocabulary and, before this
//! module, two of them held their own hand-kept copy of it:
//!
//! - **A user**, in `.stella/settings.json`, registering a shell hook
//!   (`stella-core::hooks`).
//! - **A plugin manifest**, in `[loop] hooks`, declaring which dispatch
//!   points it is granted (`stella-plugin::manifest`).
//! - **The host**, mapping one to the other.
//!
//! The two enums could not be unified where either of them lived:
//! `stella-core` must never learn plugins exist (#3245 open question 3), and
//! `stella-plugin` must never pull in the engine. So the vocabulary moves
//! *down* to the crate both may depend on, exactly as invariant #1 says a
//! shared contract should. `stella-core::hooks` and `stella-plugin::manifest`
//! re-export this type, so every existing path still resolves and a sixth
//! event is now one edit rather than two — the drift shape #3310 was filed
//! against is no longer expressible.
//!
//! # Wire shape
//!
//! PascalCase, with no `rename_all`, because these five strings are not this
//! crate's to choose: `"PreToolUse"` is already what a user types in
//! `.stella/settings.json`. Per invariant #4 the type round-trips through
//! `serde_json` byte-for-byte, and this module's `WIRE_STRINGS` test constant
//! pins each spelling so a rename that would break a shipped settings file
//! fails a test instead of a user's session.

use serde::{Deserialize, Serialize};

/// Lifecycle events a hook can fire on (TS: `HookEvent`, `HOOK_EVENTS`,
/// plus the #2684 additions `Stop` and `PreCompact`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookEvent {
    /// Once, before the turn begins.
    SessionStart,
    /// Before each tool call — input rewriting and permission decisions.
    PreToolUse,
    /// After each tool call.
    PostToolUse,
    /// The turn is about to complete (`driver::user_hooks`). Not
    /// tool-scoped: the matcher is ignored. Declaring this is also what a
    /// plugin arbiter's verdict hook rides on.
    Stop,
    /// An overflow-summarization round is about to run
    /// (`driver::user_hooks`). Not tool-scoped: the matcher is ignored.
    PreCompact,
}

impl HookEvent {
    /// Every event, in declaration order — the set a consumer must cover.
    ///
    /// Kept beside the enum rather than derived by a macro so that adding a
    /// variant without adding it here fails this module's
    /// `every_variant_is_listed` test, which is what makes "the whole
    /// vocabulary" a value a caller can iterate instead of a set it re-types.
    pub const ALL: [HookEvent; 5] = [
        HookEvent::SessionStart,
        HookEvent::PreToolUse,
        HookEvent::PostToolUse,
        HookEvent::Stop,
        HookEvent::PreCompact,
    ];

    /// Whether this event fires for one specific tool call — the events
    /// whose matchers glob over the tool name. The rest ignore the matcher
    /// and run every registered action.
    pub fn tool_scoped(self) -> bool {
        matches!(self, HookEvent::PreToolUse | HookEvent::PostToolUse)
    }

    /// The wire spelling — identical to the serde representation, and to
    /// what a user types in `.stella/settings.json`.
    pub fn as_str(self) -> &'static str {
        match self {
            HookEvent::SessionStart => "SessionStart",
            HookEvent::PreToolUse => "PreToolUse",
            HookEvent::PostToolUse => "PostToolUse",
            HookEvent::Stop => "Stop",
            HookEvent::PreCompact => "PreCompact",
        }
    }
}

impl std::fmt::Display for HookEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pinned wire strings. A rename here is a break of every shipped
    /// `.stella/settings.json` and every plugin manifest, so it has to be a
    /// deliberate edit to this list.
    const WIRE_STRINGS: [&str; 5] = [
        "SessionStart",
        "PreToolUse",
        "PostToolUse",
        "Stop",
        "PreCompact",
    ];

    #[test]
    fn every_variant_is_listed() {
        // Exhaustive by construction: adding a variant makes this match fail
        // to compile until `ALL` grows with it.
        for event in HookEvent::ALL {
            let listed = match event {
                HookEvent::SessionStart => HookEvent::SessionStart,
                HookEvent::PreToolUse => HookEvent::PreToolUse,
                HookEvent::PostToolUse => HookEvent::PostToolUse,
                HookEvent::Stop => HookEvent::Stop,
                HookEvent::PreCompact => HookEvent::PreCompact,
            };
            assert_eq!(event, listed);
        }
        assert_eq!(HookEvent::ALL.len(), WIRE_STRINGS.len());
    }

    #[test]
    fn wire_strings_are_pinned() {
        for (event, expected) in HookEvent::ALL.into_iter().zip(WIRE_STRINGS) {
            assert_eq!(
                serde_json::to_string(&event).unwrap(),
                format!("\"{expected}\"")
            );
            assert_eq!(event.as_str(), expected);
            assert_eq!(event.to_string(), expected);
        }
    }

    #[test]
    fn round_trips_byte_for_byte() {
        for event in HookEvent::ALL {
            let json = serde_json::to_string(&event).unwrap();
            let back: HookEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(event, back);
            assert_eq!(serde_json::to_string(&back).unwrap(), json);
        }
    }

    #[test]
    fn only_the_tool_call_events_are_tool_scoped() {
        for event in HookEvent::ALL {
            let expected = matches!(event, HookEvent::PreToolUse | HookEvent::PostToolUse);
            assert_eq!(event.tool_scoped(), expected, "{event}");
        }
    }
}
