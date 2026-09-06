// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella-core` — the step driver. One model call per step. It gathers
//! messages, retries with backoff, compacts context, holds a tool-output
//! budget, evicts, detects loops, and meters spend in USD.
//!
//! NO I/O of its own. The engine drives through the `Provider` and
//! `ToolExecutor` traits, and sends `AgentEvent`s over a channel. Every
//! decision it makes — how to compact, what to evict, when a loop started,
//! what a turn has spent — is a plain function over owned data. That is
//! what makes it easy to test against fakes, with no runtime and no files.
//! The crate README has the module map and the port list.

pub mod accounted_call;
pub mod budget;
pub mod bus;
pub mod compaction;
pub mod driver;
pub mod engine_markers;
pub mod estimator;
pub mod event_sender;
pub mod extensions;
pub mod goal;
pub mod hooks;
pub mod loop_detect;
pub mod ports;
pub mod receipts;
pub mod repair;
pub mod restore;
pub mod retry;
pub mod router;
pub mod scoreboard;
pub mod search;
pub mod shell_text;
pub mod skill_invocation;
pub(crate) mod speculation;
pub mod starvation;
pub mod steering;
pub mod step;
pub mod subagent;
mod summarize;
pub mod tasks;
pub mod tool_foundry;
pub mod waiting;

pub use budget::{BudgetGuard, BudgetOutcome};
// `bus::HookEvent` (the extension-bus envelope) stays module-qualified: the
// crate root already exports `hooks::HookEvent` (the shell-hook lifecycle
// enum) and the two must never be confused at a glance.
pub use accounted_call::{AccountedCall, AccountedCallError, ReceiptContext, run_accounted_call};
pub use bus::{
    ExtensionFailure, HookBus, HookDecision, HookEventDraft, HookSubscription, PolicyOutcome,
};
pub use driver::capabilities::{OwnedTurnCapabilities, TurnCapabilities};
pub use driver::{Engine, EngineConfig, SOFT_STOP_REASON, TurnOutcome};
pub use estimator::{Calibration, CalibrationMap};
pub use event_sender::{EventSendError, EventSender};
pub use extensions::{
    AgentDef, CommandDef, ExistingTargets, ExtensionDiagnostic, ExtensionKind, ExtensionProblem,
    PlannedLink, SyncEntry, SyncPlan, SyncSkip, SyncSkipReason, SyncSource, agent_from_file,
    command_from_file, command_from_toml, expand_command, merge_by_name, plan_extension_sync,
};
pub use goal::{GoalAssessError, GoalConfig, GoalOutcome, GoalVerifierVerdict};
pub use hooks::{HookEvent, HookPayload, HookRunOutcome, HookRunner, Hooks, run_hooks};
pub use loop_detect::{
    CallRecord, LoopDetectionConfig, LoopIdentity, LoopVerdict, ToolOrigin, detect_loop,
};
pub use ports::{Clock, LiveService, ToolExecutor};
pub use repair::{RepairCost, RepairHeadroom, RepairPlan, RepairRefusal, plan_repair};
pub use retry::{RetryOutcome, RetryPolicy, retry_with_backoff};
pub use router::{RoleTable, Router};
pub use step::{
    AbortKind, BudgetSnapshot, CANCELLED_REASON, CHECKPOINT_VERSION, CancelToken, Checkpoint,
    CheckpointError, StepOutcome, TurnState,
};
pub use subagent::{
    AgentAttribution, ChildSteering, MAX_SUB_AGENT_DEPTH, SubAgentDispatcher, SubAgentHost,
    SubAgentOutcome, SubAgentReport, SubAgentSpec, SubAgentSpendLedger, drain_sub_agent_spend,
    forwards_to_parent, push_sub_agent_spend,
};
pub use tasks::{RunningTask, SpawnRequest, TaskBoard, TaskBoardError};
pub use tool_foundry::{
    GapDetectionConfig, ParamKind, ProposedTool, ShellInvocation, ToolParameter, detect_tool_gaps,
};
pub use waiting::{WaitCall, WaitRequest};
