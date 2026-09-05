// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The JSON a hook command reads on stdin, and the shapes inside it.
//!
//! One struct for every event rather than one per event, because a hook is a
//! shell command reading a document: a consumer that had to know which payload
//! type it held before it could find `cwd` would be harder to write than one
//! that reads a field and finds it absent. Every field is optional except
//! [`HookPayload::event`] and [`HookPayload::cwd`], and which of them an event
//! writes is pinned from outside — `stella_plugin::wire::disclosure`'s table
//! names them, and `stella-tools`'s `hook_disclosure` suite asserts the two
//! agree in both directions.

use serde::{Deserialize, Serialize};

use stella_protocol::hook::HookEvent;

/// The tool a `PreToolUse`/`PostToolUse` hook fires for (TS: `HookPayload["tool"]`),
/// plus the advertised metadata a permission-deciding hook needs (#2684).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookToolInfo {
    pub name: String,
    pub input: serde_json::Value,
    /// The tool's advertised `read_only` bit, from its
    /// [`stella_protocol::ToolSchema`] — `false` for a tool the executor
    /// does not advertise, the cautious direction. The one metadata field
    /// that exists today; the #2716 `ToolContract` fields join here when
    /// they land. `default` so a payload written before this field existed
    /// still deserializes.
    #[serde(default)]
    pub read_only: bool,
}

/// The JSON payload fed to a hook command on stdin (TS: `HookPayload`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookPayload {
    pub event: HookEvent,
    pub cwd: String,
    /// Present for `PreToolUse` / `PostToolUse`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<HookToolInfo>,
    /// Present for `PostToolUse`: the result string the tool returned —
    /// `ToolOutput::Ok`'s content or `ToolOutput::Error`'s message, whole.
    /// Nothing clips it, so a hook matching a tool that returns a large body
    /// receives that whole body on stdin; a hook author who only wants a
    /// summary must truncate on their own side.
    #[serde(rename = "toolResult", skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<String>,
    /// Present for `Stop`: the text the turn is about to complete with,
    /// whole — the same unclipped posture as `tool_result`, and for the
    /// same reason: what a hook can afford to read is the hook's call.
    #[serde(rename = "finalText", default, skip_serializing_if = "Option::is_none")]
    pub final_text: Option<String>,
    /// Present for `UserPromptSubmit`: the prompt the user just typed,
    /// whole — the same unclipped posture as `final_text`, before any turn
    /// exists to clip it against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Present for `SubagentStart` / `SubagentStop`: which child turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<HookSubAgentInfo>,
    /// Present for `SubagentStop`: how that child's turn ended.
    #[serde(
        rename = "subagentResult",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub subagent_result: Option<HookSubAgentResult>,
    /// Present for `PreIssueWork` / `PostIssueWork`: which issue the
    /// self-driving loop is about to work, or has just worked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<HookIssueInfo>,
    /// Present for `PostIssueWork`: how the work unit ended.
    #[serde(
        rename = "issueOutcome",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub issue_outcome: Option<HookIssueOutcome>,
    /// Present for the loop-lifecycle events: which run, and which cycle
    /// inside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<HookRunInfo>,
    /// Present for the pull-request and check events: which pull request.
    #[serde(
        rename = "pullRequest",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub pull_request: Option<HookPullRequestInfo>,
    /// Why, in the loop's own words — why a run ended, why the loop is idle,
    /// why it refused to run, why an issue was escalated or closed.
    ///
    /// Prose for a person, and the *event name* is what a subscriber branches
    /// on. That split is why `ChecksFailed` and `BaseBroken` are two events
    /// rather than one carrying a reason: a decision a consumer has to make
    /// belongs in the vocabulary, not in a string it has to parse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl HookPayload {
    /// A payload carrying only the event and the workspace — the shape
    /// every non-tool event shares.
    fn bare(event: HookEvent, cwd: String) -> Self {
        Self {
            event,
            cwd,
            tool: None,
            tool_result: None,
            final_text: None,
            prompt: None,
            subagent: None,
            subagent_result: None,
            issue: None,
            issue_outcome: None,
            run: None,
            pull_request: None,
            reason: None,
        }
    }

    /// A `SessionStart` payload — no tool, no result.
    pub fn session_start(cwd: impl Into<String>) -> Self {
        Self::bare(HookEvent::SessionStart, cwd.into())
    }

    /// A `PreToolUse` payload for the given tool call. `read_only` is the
    /// tool's advertised bit from its schema (#2684).
    pub fn pre_tool_use(
        cwd: impl Into<String>,
        name: impl Into<String>,
        input: serde_json::Value,
        read_only: bool,
    ) -> Self {
        Self {
            tool: Some(HookToolInfo {
                name: name.into(),
                input,
                read_only,
            }),
            ..Self::bare(HookEvent::PreToolUse, cwd.into())
        }
    }

    /// A `PostToolUse` payload for the given tool call and its result.
    pub fn post_tool_use(
        cwd: impl Into<String>,
        name: impl Into<String>,
        input: serde_json::Value,
        read_only: bool,
        tool_result: impl Into<String>,
    ) -> Self {
        Self {
            tool: Some(HookToolInfo {
                name: name.into(),
                input,
                read_only,
            }),
            tool_result: Some(tool_result.into()),
            ..Self::bare(HookEvent::PostToolUse, cwd.into())
        }
    }

    /// A `Stop` payload carrying the text the turn is about to complete
    /// with.
    pub fn stop(cwd: impl Into<String>, final_text: impl Into<String>) -> Self {
        Self {
            final_text: Some(final_text.into()),
            ..Self::bare(HookEvent::Stop, cwd.into())
        }
    }

    /// A `PreCompact` payload — the event and the workspace, nothing else.
    pub fn pre_compact(cwd: impl Into<String>) -> Self {
        Self::bare(HookEvent::PreCompact, cwd.into())
    }

    /// A `UserPromptSubmit` payload carrying the prompt the user just typed.
    pub fn user_prompt_submit(cwd: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            prompt: Some(prompt.into()),
            ..Self::bare(HookEvent::UserPromptSubmit, cwd.into())
        }
    }

    /// A `SubagentStart` payload — the child about to run.
    pub fn subagent_start(cwd: impl Into<String>, subagent: HookSubAgentInfo) -> Self {
        Self {
            subagent: Some(subagent),
            ..Self::bare(HookEvent::SubagentStart, cwd.into())
        }
    }

    /// A `SubagentStop` payload — the same child, plus how its turn ended.
    pub fn subagent_stop(
        cwd: impl Into<String>,
        subagent: HookSubAgentInfo,
        result: HookSubAgentResult,
    ) -> Self {
        Self {
            subagent: Some(subagent),
            subagent_result: Some(result),
            ..Self::bare(HookEvent::SubagentStop, cwd.into())
        }
    }

    /// A `PreIssueWork` payload — the issue the loop is about to work.
    ///
    /// No outcome, because nothing has happened yet: this is the payload a
    /// hook reads to decide whether the work should happen at all.
    pub fn pre_issue_work(cwd: impl Into<String>, issue: HookIssueInfo) -> Self {
        Self {
            issue: Some(issue),
            ..Self::bare(HookEvent::PreIssueWork, cwd.into())
        }
    }

    /// A `PostIssueWork` payload — the same issue, plus how it went.
    pub fn post_issue_work(
        cwd: impl Into<String>,
        issue: HookIssueInfo,
        outcome: HookIssueOutcome,
    ) -> Self {
        Self {
            issue: Some(issue),
            issue_outcome: Some(outcome),
            ..Self::bare(HookEvent::PostIssueWork, cwd.into())
        }
    }

    /// A loop-lifecycle payload: which run, which cycle, and why where there
    /// is a why.
    ///
    /// One constructor for the five, taking the event, because they differ in
    /// nothing else — five near-identical functions would be five places for
    /// the `run` field to be forgotten. The caller names the event, and
    /// [`HookEvent::in_turn`] is what stops an in-turn one being passed here:
    /// a `SessionStart` carrying a run id is a payload no disclosure row
    /// describes, and the census test in `stella-tools` fails on it.
    pub fn drive(
        event: HookEvent,
        cwd: impl Into<String>,
        run: HookRunInfo,
        reason: Option<String>,
    ) -> Self {
        Self {
            run: Some(run),
            reason,
            ..Self::bare(event, cwd.into())
        }
    }

    /// A tracker payload: which issue, and why where there is a why.
    ///
    /// `IssueCreated` has no reason — the issue's own body is the reason, and
    /// it is one tracker read away — while `IssueClosed` and `IssueEscalated`
    /// carry the loop's own words for what it decided.
    pub fn tracker(
        event: HookEvent,
        cwd: impl Into<String>,
        issue: HookIssueInfo,
        reason: Option<String>,
    ) -> Self {
        Self {
            issue: Some(issue),
            reason,
            ..Self::bare(event, cwd.into())
        }
    }

    /// A pull-request or check payload: which pull request, and why where
    /// there is a why.
    pub fn pull_request(
        event: HookEvent,
        cwd: impl Into<String>,
        pull_request: HookPullRequestInfo,
        reason: Option<String>,
    ) -> Self {
        Self {
            pull_request: Some(pull_request),
            reason,
            ..Self::bare(event, cwd.into())
        }
    }
}

/// The child turn a `SubagentStart`/`SubagentStop` hook observes.
///
/// Present on both events, unlike [`HookToolInfo`]/[`HookSubAgentResult`]'s
/// pre/post split: `agent_id` is how a subscriber pairs a `SubagentStop`
/// back to the `SubagentStart` that opened it, so both need to carry it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookSubAgentInfo {
    /// Stable id for this child, unique within the parent turn — the same
    /// identifier [`stella_protocol::SubAgentPhase::agent_id`] carries.
    #[serde(rename = "agentId")]
    pub agent_id: String,
    /// The child's task, truncated for display — never the full prompt, on
    /// the same posture as the `Started` `AgentEvent`.
    #[serde(rename = "instructionPreview")]
    pub instruction_preview: String,
    /// Nesting depth: `1` for a child of the top-level turn.
    pub depth: u8,
}

/// How a child's turn ended, for [`HookEvent::SubagentStop`].
///
/// Mirrors the fields of [`stella_protocol::SubAgentPhase::Finished`] a
/// subscriber can actually act on — not `truncated`/`absorbed_messages`,
/// which describe the parent's bookkeeping rather than the child's outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookSubAgentResult {
    /// Whether the child reached a clean final answer, aborted at a step
    /// boundary, or was refused before its first model call.
    pub status: stella_protocol::SubAgentStatus,
    /// The report handed back to the parent.
    pub summary: String,
    /// The child's total spend, already settled into the parent's budget.
    #[serde(rename = "costUsd")]
    pub cost_usd: f64,
    /// Model calls the child made.
    pub steps: usize,
}

/// The run a loop-lifecycle event belongs to, as a hook sees it.
///
/// [`HookIssueInfo`]'s discipline, for the other identity: the run id is
/// always present because it is what every other record of the run is keyed
/// by, and everything else is what the loop happened to have in hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookRunInfo {
    /// The run's identifier, as `stella self-driving runs` prints it.
    #[serde(rename = "runId")]
    pub run_id: String,
    /// Which cycle of that run, for the cycle events. Absent for the run
    /// events themselves, which bracket every cycle rather than sitting in
    /// one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle: Option<u64>,
}

impl HookRunInfo {
    /// The run alone — the shape a caller outside any cycle can always build.
    pub fn new(run_id: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            cycle: None,
        }
    }

    /// The run, and which cycle of it this is.
    #[must_use]
    pub fn in_cycle(mut self, cycle: u64) -> Self {
        self.cycle = Some(cycle);
        self
    }
}

/// The pull request a delivery event is about, as a hook sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookPullRequestInfo {
    /// The pull request's number, as the forge spells it. Always present.
    pub number: String,
    /// The issue it settles, when the loop already had it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
}

impl HookPullRequestInfo {
    /// The identifier alone.
    pub fn new(number: impl Into<String>) -> Self {
        Self {
            number: number.into(),
            issue: None,
        }
    }

    /// The pull request, and the issue it settles.
    #[must_use]
    pub fn for_issue(mut self, issue: impl Into<String>) -> Self {
        self.issue = Some(issue.into());
        self
    }
}

/// The issue a self-driving work unit is about, as a hook sees it.
///
/// # Why the identifier and nothing else is guaranteed
///
/// Only [`Self::number`] is always present. The rest is what the loop happened
/// to have already read, and the loop reads the tracker lazily — so a hook that
/// needs a field this does not carry should fetch it itself (`gh issue view`
/// is one line in a shell hook) rather than have Stella fetch it on every
/// dispatch for the hooks that do not.
///
/// That splits the cost the right way: an extra tracker round trip per issue,
/// paid by the hooks that want it, beats one paid by every loop whether a hook
/// is registered or not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookIssueInfo {
    /// The issue number, as the tracker spells it. Always present — it is the
    /// identity everything else about the work unit hangs off.
    pub number: String,
    /// The issue's title, when the loop already had it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The branch the work would land on, when it is already decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

impl HookIssueInfo {
    /// The identifier alone — the shape a caller that has read nothing else
    /// can always build.
    pub fn new(number: impl Into<String>) -> Self {
        Self {
            number: number.into(),
            title: None,
            branch: None,
        }
    }
}

/// How a work unit ended, for [`HookEvent::PostIssueWork`].
///
/// Three arms rather than a boolean, because a consumer's next move differs
/// for each: `Changed` means there is a branch to review, `NoChange` means the
/// issue is still open and untouched, and `Failed` means something needs a
/// human. Collapsing the last two into "not successful" is what makes a
/// dashboard unable to tell a starved loop from a broken one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum HookIssueOutcome {
    /// The turn ran and left committed or uncommitted work behind.
    Changed {
        /// What changed, as the loop summarized it.
        summary: String,
    },
    /// The turn ran and changed nothing.
    NoChange,
    /// The work unit could not complete.
    Failed {
        /// Why, in the loop's own words.
        reason: String,
    },
}
