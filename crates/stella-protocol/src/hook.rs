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
//! *down* to the crate both may depend on, exactly as AGENTS.md #1 says a
//! shared contract should. `stella-core::hooks` and `stella-plugin::manifest`
//! re-export this type, so every existing path still resolves and another
//! event is now one edit rather than two — the drift shape #3310 was filed
//! against is no longer expressible.
//!
//! # Wire shape
//!
//! PascalCase, with no `rename_all`, because these strings are not this
//! crate's to choose: `"PreToolUse"` is already what a user types in
//! `.stella/settings.json`. Per AGENTS.md #4 the type round-trips through
//! `serde_json` byte-for-byte, and this module's `WIRE_STRINGS` test constant
//! pins each spelling so a rename that would break a shipped settings file
//! fails a test instead of a user's session.
//!
//! # Two families, one vocabulary
//!
//! Some events name points **inside a turn** and the rest name points around
//! one, fired by the self-driving loop; [`HookEvent::in_turn`] is the line, and
//! only an in-turn event may be granted to a plugin. They share this enum for
//! the reason this module exists — a user registers both in the same `hooks`
//! block, and a second enum would be a second vocabulary to keep identical.
//!
//! The naming rule for anything added here: **`Pre`/`Post` for a pair that
//! brackets something, a past participle for a thing that happened, and no
//! `ON_`/`BEFORE_` prefixes** — the tense lives in the name. Only a `Pre` can
//! veto, because only a `Pre` names something that has not happened yet.

use serde::{Deserialize, Serialize};

/// Lifecycle events a hook can fire on (TS: `HookEvent`, `HOOK_EVENTS`,
/// plus the #2684 additions `Stop` and `PreCompact`, and the #2836 additions
/// `UserPromptSubmit`, `SubagentStart` and `SubagentStop`).
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
    /// The user's prompt was submitted, before it becomes part of any turn.
    /// Not tool-scoped: the matcher is ignored.
    ///
    /// Fired **host-side**, in `stella-cli`, on [`HookEvent::SessionStart`]'s
    /// own terms: no `Engine` exists yet, so nothing here runs from
    /// `driver::user_hooks`. A `deny` decision rejects the prompt outright,
    /// with the hook's reason shown to the person who typed it and no turn
    /// built at all — the permission-shaped posture `PreToolUse` already
    /// takes, applied one step earlier. A `modify` decision may rewrite the
    /// prompt text the turn actually runs with.
    UserPromptSubmit,
    /// A child turn (`stella_core::subagent`) is about to start. Not
    /// tool-scoped, and **observe-only**: nothing here may veto or rewrite
    /// the child, mirroring [`HookEvent::PostToolUse`]'s posture rather than
    /// `PreToolUse`'s — the parent already decided to delegate, and this is a
    /// subscriber's window into that decision, not a second permission gate
    /// on it.
    SubagentStart,
    /// A child turn (`stella_core::subagent`) has finished, whatever the
    /// outcome. Not tool-scoped, and cannot veto — the child already ran.
    SubagentStop,
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

    /// A self-driving run has begun, before its first cycle. Reports.
    ///
    /// Reports rather than vetoes, with every event below it: the tense is the
    /// rule, and a run-level veto would duplicate [`HookEvent::PreIssueWork`]
    /// at a coarser grain while being unable to say *which* work it withheld —
    /// the one thing an operator reading a skip needs. These were declared once
    /// #4001 gave each somewhere real to fire from (#4017); a hook point
    /// nothing dispatches is a declaration that quietly does nothing.
    DriveRunStart,
    /// A self-driving run has ended, carrying why it stopped. Reports.
    ///
    /// The reason is the field a scheduler acts on: a run that ended because
    /// the operator asked, one that ended on a spent budget and one that
    /// crashed all want different next moves, and a subscriber that could only
    /// see "it ended" would have to guess between them.
    DriveRunEnd,
    /// A cycle has begun. Reports.
    DriveCycleStart,
    /// A cycle has ended and its ledger record is written. Reports.
    DriveCycleEnd,
    /// A cycle produced nothing — the dry-streak advance. Reports.
    ///
    /// The signal that separates *alive but starved* from *dead*, which a
    /// monitor cannot get from silence: a loop with an empty backlog and a
    /// loop whose process died emit the same nothing.
    DriveIdle,

    /// The loop filed a finding as an issue. Reports.
    IssueCreated,
    /// The loop closed an issue, `--partial` included. Reports.
    IssueClosed,
    /// The loop gave up on an issue and handed it to a human. Reports.
    ///
    /// Carries the reason in the loop's own vocabulary, because a subscriber's
    /// next move differs by it: a ceiling that was reached can be raised, and
    /// a review that needs a human cannot.
    IssueEscalated,

    /// The loop opened a pull request. Reports.
    PullRequestOpened,
    /// The loop took a pull request out of draft. Reports.
    PullRequestReadyForReview,
    /// The base moved under a pull request. Reports.
    PullRequestConflicted,
    /// A pull request landed. Reports.
    PullRequestMerged,

    /// A pull request's checks failed **and the base is green**, so the
    /// failure is this change's. Reports.
    ///
    /// Not the same event as [`HookEvent::BaseBroken`]: the whole `deliver`
    /// machine turns on that distinction, and collapsing the two is how a loop
    /// spends its budget fixing somebody else's breakage.
    ChecksFailed,
    /// A pull request's checks failed **and so does the base**, so the failure
    /// is not this change's. Reports.
    ///
    /// The event a `main`-health monitor wants: it is the one that says the
    /// tree is broken for everyone rather than for this branch.
    BaseBroken,
    /// A pull request's checks passed. Reports.
    ChecksGreen,

    /// The loop stopped because it reached its spend ceiling. Reports.
    DriveBudgetExhausted,
    /// The loop declined to start, or to continue. Reports.
    ///
    /// Distinct from an error: nothing broke, and the loop chose not to run —
    /// an operator stop, steering switched off, a `main-red` hold. A
    /// subscriber that read a refusal as a failure would page somebody about a
    /// working system.
    DriveRefused,
}

impl HookEvent {
    /// Every event, in declaration order — the set a consumer must cover.
    ///
    /// Kept beside the enum rather than derived by a macro so that adding a
    /// case without adding it here fails this module's
    /// `every_variant_is_listed` test, which is what makes "the whole
    /// vocabulary" a value a caller can iterate instead of a set it re-types.
    pub const ALL: [HookEvent; 27] = [
        HookEvent::SessionStart,
        HookEvent::PreToolUse,
        HookEvent::PostToolUse,
        HookEvent::Stop,
        HookEvent::PreCompact,
        HookEvent::UserPromptSubmit,
        HookEvent::SubagentStart,
        HookEvent::SubagentStop,
        HookEvent::PreIssueWork,
        HookEvent::PostIssueWork,
        HookEvent::DriveRunStart,
        HookEvent::DriveRunEnd,
        HookEvent::DriveCycleStart,
        HookEvent::DriveCycleEnd,
        HookEvent::DriveIdle,
        HookEvent::IssueCreated,
        HookEvent::IssueClosed,
        HookEvent::IssueEscalated,
        HookEvent::PullRequestOpened,
        HookEvent::PullRequestReadyForReview,
        HookEvent::PullRequestConflicted,
        HookEvent::PullRequestMerged,
        HookEvent::ChecksFailed,
        HookEvent::BaseBroken,
        HookEvent::ChecksGreen,
        HookEvent::DriveBudgetExhausted,
        HookEvent::DriveRefused,
    ];

    /// Whether this event fires for one specific tool call — the events
    /// whose matchers glob over the tool name. The rest ignore the matcher
    /// and run every registered action.
    pub fn tool_scoped(self) -> bool {
        matches!(self, HookEvent::PreToolUse | HookEvent::PostToolUse)
    }

    /// Whether this event names a point **inside** a turn.
    ///
    /// The two families this enum holds, as a value rather than a paragraph.
    /// An in-turn event fires somewhere between a turn's start and its
    /// completion and is the only kind a plugin may be routed at — most of
    /// them from the engine's driver, `SessionStart`/`UserPromptSubmit` from
    /// the CLI host before any `Engine` exists, and the `Subagent` pair from
    /// `stella_core::subagent` around a child turn. Everything else is
    /// dispatched by the self-driving loop, outside every turn, from the
    /// operator's own `hooks` settings.
    ///
    /// A total `match` rather than a `matches!` allowlist, and that is the
    /// point: the allowlist form would silently place a new event on the
    /// outside, and `stella_plugin`'s refusal and the host's routing table
    /// both read this. Placing it wrongly is a decision somebody has to write
    /// down; forgetting to place it does not compile.
    pub fn in_turn(self) -> bool {
        match self {
            HookEvent::SessionStart
            | HookEvent::PreToolUse
            | HookEvent::PostToolUse
            | HookEvent::Stop
            | HookEvent::PreCompact
            | HookEvent::UserPromptSubmit
            | HookEvent::SubagentStart
            | HookEvent::SubagentStop => true,
            HookEvent::PreIssueWork
            | HookEvent::PostIssueWork
            | HookEvent::DriveRunStart
            | HookEvent::DriveRunEnd
            | HookEvent::DriveCycleStart
            | HookEvent::DriveCycleEnd
            | HookEvent::DriveIdle
            | HookEvent::IssueCreated
            | HookEvent::IssueClosed
            | HookEvent::IssueEscalated
            | HookEvent::PullRequestOpened
            | HookEvent::PullRequestReadyForReview
            | HookEvent::PullRequestConflicted
            | HookEvent::PullRequestMerged
            | HookEvent::ChecksFailed
            | HookEvent::BaseBroken
            | HookEvent::ChecksGreen
            | HookEvent::DriveBudgetExhausted
            | HookEvent::DriveRefused => false,
        }
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
            HookEvent::UserPromptSubmit => "UserPromptSubmit",
            HookEvent::SubagentStart => "SubagentStart",
            HookEvent::SubagentStop => "SubagentStop",
            HookEvent::PreIssueWork => "PreIssueWork",
            HookEvent::PostIssueWork => "PostIssueWork",
            HookEvent::DriveRunStart => "DriveRunStart",
            HookEvent::DriveRunEnd => "DriveRunEnd",
            HookEvent::DriveCycleStart => "DriveCycleStart",
            HookEvent::DriveCycleEnd => "DriveCycleEnd",
            HookEvent::DriveIdle => "DriveIdle",
            HookEvent::IssueCreated => "IssueCreated",
            HookEvent::IssueClosed => "IssueClosed",
            HookEvent::IssueEscalated => "IssueEscalated",
            HookEvent::PullRequestOpened => "PullRequestOpened",
            HookEvent::PullRequestReadyForReview => "PullRequestReadyForReview",
            HookEvent::PullRequestConflicted => "PullRequestConflicted",
            HookEvent::PullRequestMerged => "PullRequestMerged",
            HookEvent::ChecksFailed => "ChecksFailed",
            HookEvent::BaseBroken => "BaseBroken",
            HookEvent::ChecksGreen => "ChecksGreen",
            HookEvent::DriveBudgetExhausted => "DriveBudgetExhausted",
            HookEvent::DriveRefused => "DriveRefused",
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
    /// considered edit to this list.
    const WIRE_STRINGS: [&str; 27] = [
        "SessionStart",
        "PreToolUse",
        "PostToolUse",
        "Stop",
        "PreCompact",
        "UserPromptSubmit",
        "SubagentStart",
        "SubagentStop",
        "PreIssueWork",
        "PostIssueWork",
        "DriveRunStart",
        "DriveRunEnd",
        "DriveCycleStart",
        "DriveCycleEnd",
        "DriveIdle",
        "IssueCreated",
        "IssueClosed",
        "IssueEscalated",
        "PullRequestOpened",
        "PullRequestReadyForReview",
        "PullRequestConflicted",
        "PullRequestMerged",
        "ChecksFailed",
        "BaseBroken",
        "ChecksGreen",
        "DriveBudgetExhausted",
        "DriveRefused",
    ];

    /// Its position in [`HookEvent::ALL`], derived by a match so the compiler
    /// forces a new case to be placed rather than silently omitted.
    fn declared_index(event: HookEvent) -> usize {
        match event {
            HookEvent::SessionStart => 0,
            HookEvent::PreToolUse => 1,
            HookEvent::PostToolUse => 2,
            HookEvent::Stop => 3,
            HookEvent::PreCompact => 4,
            HookEvent::UserPromptSubmit => 5,
            HookEvent::SubagentStart => 6,
            HookEvent::SubagentStop => 7,
            HookEvent::PreIssueWork => 8,
            HookEvent::PostIssueWork => 9,
            HookEvent::DriveRunStart => 10,
            HookEvent::DriveRunEnd => 11,
            HookEvent::DriveCycleStart => 12,
            HookEvent::DriveCycleEnd => 13,
            HookEvent::DriveIdle => 14,
            HookEvent::IssueCreated => 15,
            HookEvent::IssueClosed => 16,
            HookEvent::IssueEscalated => 17,
            HookEvent::PullRequestOpened => 18,
            HookEvent::PullRequestReadyForReview => 19,
            HookEvent::PullRequestConflicted => 20,
            HookEvent::PullRequestMerged => 21,
            HookEvent::ChecksFailed => 22,
            HookEvent::BaseBroken => 23,
            HookEvent::ChecksGreen => 24,
            HookEvent::DriveBudgetExhausted => 25,
            HookEvent::DriveRefused => 26,
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

    /// The two families, pinned from the outside: an in-turn event is one
    /// fired somewhere inside a turn's lifetime — the engine's driver, the
    /// CLI host, or `stella_core::subagent` — and every loop event — the
    /// `Issue` pair included — sits outside a turn, which is what makes it
    /// unroutable to a plugin.
    ///
    /// Spelled as a list here rather than re-deriving [`HookEvent::in_turn`]'s
    /// match, because a test that recomputed the answer would agree with any
    /// mistake the answer contains.
    #[test]
    fn the_in_turn_family_is_exactly_the_events_the_turn_lifecycle_dispatches() {
        let in_turn: Vec<&str> = HookEvent::ALL
            .into_iter()
            .filter(|event| event.in_turn())
            .map(HookEvent::as_str)
            .collect();
        assert_eq!(
            in_turn,
            [
                "SessionStart",
                "PreToolUse",
                "PostToolUse",
                "Stop",
                "PreCompact",
                "UserPromptSubmit",
                "SubagentStart",
                "SubagentStop",
            ]
        );
    }
}
