//! Per-role request shaping: what each pipeline role sends on the wire, over
//! and above the engine-wide base.
//!
//! A sibling module rather than more lines in [the parent](super), which is a
//! grandfathered god file closed to growth (AGENTS.md § "God files"). These
//! two types are a self-contained vocabulary — plain data, no behaviour, and
//! nothing here reads the pipeline's state — so they carry their own file
//! cleanly.
//!
//! # Who resolves these
//!
//! Not this crate. Every row arrives already resolved by the caller from
//! `agent_engine_config` (`stella-cli`'s `resolve_engine_wiring`), including
//! the "rides the worker" fallbacks — so a row that is `None` here means the
//! operator set nothing *and* nothing was inherited, never "ask again later".
//! Keeping the precedence in one place is what stops the report
//! `stella config` prints from disagreeing with what the pipeline sends.

/// Per-role request overrides for the pipeline's raw completion calls
/// (triage / plan / verifier / guidance), resolved by the caller from
/// `agent_engine_config`. Every field is optional and falls through to the
/// engine config's value — the worker's settings are the pipeline-wide
/// base; these refine one role. `prompt`, when set, is prepended as a
/// system message to the role's built-in task prompt (the task prompt
/// carries the output contract — `PASS`/`FAIL`, the triage token — so it
/// is never replaced outright).
#[derive(Debug, Clone, Default)]
pub struct RoleCallOverrides {
    pub prompt: Option<String>,
    pub effort: Option<stella_protocol::ReasoningEffort>,
    pub reasoning: Option<bool>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub params: Option<stella_protocol::GenerationParams>,
}

/// The pipeline's per-role override set.
///
/// The witness author/repair engines ride the verifier's model, so they take
/// the `verifier` row's shaping too (#1785) — everything except `prompt`,
/// which stays scoped to the raw verdict/guidance calls
/// (`Pipeline::witness_engine_config` says why).
#[derive(Debug, Clone, Default)]
pub struct PipelineRoleOverrides {
    pub triage: RoleCallOverrides,
    pub verifier: RoleCallOverrides,
    /// The worker row (`agents.worker`), which shapes every execute turn.
    ///
    /// It used to shape the **plan** call too, because plan had no row of its
    /// own; `plan` below now carries that, resolved by the caller so that an
    /// unconfigured `agents.plan` still inherits every worker field — the
    /// #2416 property (operator prose reaching the planner, which an
    /// [`EngineConfig`](stella_core::EngineConfig) structurally cannot carry)
    /// is preserved by that inheritance, not by the two sharing a row.
    ///
    /// Deliberately NOT consumed by the conversational fast path — see
    /// `Pipeline::run_conversational`.
    pub worker: RoleCallOverrides,
    /// The planner's row (`agents.plan`), which shapes the plan and
    /// plan-repair calls. Falls back to `worker` field by field at the
    /// caller, so an absent `agents.plan` leaves the planner exactly where it
    /// has always run.
    pub plan: RoleCallOverrides,
    /// The research sub-agents' row (`agents.research`).
    ///
    /// The one row that shapes an [`EngineConfig`](stella_core::EngineConfig)
    /// rather than a raw call:
    /// research children are engine sub-agent turns, so the stage applies this
    /// through `apply_role_shaping` instead of `metered_raw_call`. `prompt` is
    /// therefore not honoured here and a research child keeps
    /// `RESEARCH_SYSTEM_PROMPT` — a sub-agent's system prompt is the contract
    /// that makes it read-only, not a preference (#2374).
    pub research: RoleCallOverrides,
}
