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
//! re-export this type, so every existing path still resolves and another
//! event is now one edit rather than two — the drift shape #3310 was filed
//! against is no longer expressible.
//!
//! # Wire shape
//!
//! PascalCase, with no `rename_all`, because these strings are not this
//! crate's to choose: `"PreToolUse"` is already what a user types in
//! `.stella/settings.json`. Per invariant #4 the type round-trips through
//! `serde_json` byte-for-byte, and this module's `WIRE_STRINGS` test constant
//! pins each spelling so a rename that would break a shipped settings file
//! fails a test instead of a user's session.
//!
//! # Two families, one vocabulary
//!
//! The original five events name points **inside a turn**. [`HookEvent::PreIssueWork`]
//! and [`HookEvent::PostIssueWork`] name points **around a turn**: the
//! self-driving loop deciding to work an issue, and the outcome when it is
//! done (#3599). They share this enum rather than getting one of their own for
//! the reason this module exists at all — a user registers both in the same
//! `hooks` block of the same settings file, and a plugin declares both in the
//! same `[loop] hooks` list. A second enum would be a second vocabulary to
//! keep identical, which is the drift shape #3310 removed.
//!
//! The naming rule for anything added here, so the set stays readable as one:
//! **`Pre`/`Post` for a pair that brackets something, a past participle for a
//! thing that happened, and no `ON_`/`BEFORE_` prefixes** — the tense lives in
//! the name. The rest of the self-driving vocabulary (the tracker, pull-request
//! and check events) is designed and deliberately **not** declared here yet:
//! it needs the `deliver` verbs to have somewhere to fire from, and a hook
//! point nothing dispatches is a declaration that quietly does nothing. See
//! the issue tracking it.

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
    /// The self-driving loop is about to work an issue, before the worktree
    /// exists and before any model call (#3599). Not tool-scoped.
    ///
    /// **A veto point**, on [`HookEvent::PreToolUse`]'s contract: a `deny`
    /// decision means the loop skips this issue and moves to the next one. That
    /// is the point of the event rather than a side effect of it — it is how a
    /// person keeps an agent off work they have not finished thinking about
    /// (an `agent-hold` label, an unresolved question, a release freeze), and
    /// how two agents working one backlog avoid taking the same issue.
    ///
    /// A skip is not a failure: the loop continues. A hook that means "stop the
    /// whole loop" should say so in its reason and let the operator act, since
    /// nothing here can distinguish the two intents.
    PreIssueWork,
    /// The self-driving loop has finished working an issue, whatever the
    /// outcome. Not tool-scoped, and **cannot veto** — the work is done.
    ///
    /// Fires for a failed attempt as well as a successful one, because the
    /// consumers that most want it (a dashboard, a notifier, a second agent
    /// waiting for the branch) need the failure at least as much.
    ///
    /// Does **not** fire when [`HookEvent::PreIssueWork`] denied: nothing was
    /// worked, so there is no outcome to report, and a `Post` that fired
    /// without its `Pre` having been allowed would make the pair useless for
    /// exactly the bookkeeping it exists for.
    PostIssueWork,
}

impl HookEvent {
    /// Every event, in declaration order — the set a consumer must cover.
    ///
    /// Kept beside the enum rather than derived by a macro so that adding a
    /// variant without adding it here fails this module's
    /// `every_variant_is_listed` test, which is what makes "the whole
    /// vocabulary" a value a caller can iterate instead of a set it re-types.
    pub const ALL: [HookEvent; 7] = [
        HookEvent::SessionStart,
        HookEvent::PreToolUse,
        HookEvent::PostToolUse,
        HookEvent::Stop,
        HookEvent::PreCompact,
        HookEvent::PreIssueWork,
        HookEvent::PostIssueWork,
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
            HookEvent::PreIssueWork => "PreIssueWork",
            HookEvent::PostIssueWork => "PostIssueWork",
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
    const WIRE_STRINGS: [&str; 7] = [
        "SessionStart",
        "PreToolUse",
        "PostToolUse",
        "Stop",
        "PreCompact",
        "PreIssueWork",
        "PostIssueWork",
    ];

    /// Its position in [`HookEvent::ALL`], derived by a match so the compiler
    /// forces a new variant to be placed rather than silently omitted.
    fn declared_index(event: HookEvent) -> usize {
        match event {
            HookEvent::SessionStart => 0,
            HookEvent::PreToolUse => 1,
            HookEvent::PostToolUse => 2,
            HookEvent::Stop => 3,
            HookEvent::PreCompact => 4,
            HookEvent::PreIssueWork => 5,
            HookEvent::PostIssueWork => 6,
        }
    }

    #[test]
    fn every_variant_is_listed() {
        for (index, event) in HookEvent::ALL.into_iter().enumerate() {
            assert_eq!(declared_index(event), index, "{event} is out of place");
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
