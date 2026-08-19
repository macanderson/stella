//! The `agent_engine_config` block — the per-role engine agents (models,
//! prompts, effort, params), their JSON schema, and the load/save machinery
//! for the one scope an editor owns.
//!
//! Moved verbatim out of `settings.rs` — no behavior change — when that file
//! crossed the size ratchet (`scripts/check-file-size.sh`): the engine-agent
//! cluster is the largest self-contained concept it carried. Everything here
//! is re-exported by the parent, so every `settings::AgentEngineConfig` path
//! keeps resolving.

use super::*;

/// The configurable engine agents. `Default` is the interactive /
/// step-loop agent; the rest are the staged pipeline's roles, one per
/// `stella_protocol::Role` that serves a pipeline responsibility.
///
/// `Research` and `Plan` were the last two to get their own rows (#2374).
/// Both used to ride the worker's settings, which read as harmless while the
/// worker's posture was the only one anybody tuned — and then a benchmark seat
/// pinned the worker to `xhigh` and silently bought fifteen `xhigh` research
/// calls that emitted a few hundred reasoning tokens between them. A role that
/// cannot be turned down is a role that is always billed at the most expensive
/// setting in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineAgentKind {
    Default,
    Worker,
    Verifier,
    Triage,
    Research,
    Plan,
}

impl EngineAgentKind {
    pub const ALL: [EngineAgentKind; 6] = [
        EngineAgentKind::Default,
        EngineAgentKind::Worker,
        EngineAgentKind::Verifier,
        EngineAgentKind::Triage,
        EngineAgentKind::Research,
        EngineAgentKind::Plan,
    ];
}

/// The `agent_engine_config` root object — issue-style schema:
///
/// ```jsonc
/// {
///   "agent_engine_config": {
///     "default_model": "anthropic/claude-fable-5",
///     "pipeline_worker_model": "zai/glm-5.2",
///     "pipeline_verifier_model": "openrouter/openai/gpt-5.5",
///     "pipeline_triage_model": "deepseek/deepseek-chat",
///     "allowed_models": ["anthropic/claude-fable-5", "zai/glm-5.2"],
///     "auto_mode": "off",
///     "effort_auto": "on",
///     "reasoning_auto": "on",
///     "agents": {
///       "verifier": {
///         "provider": "openrouter",
///         "model": "openai/gpt-5.5",
///         "prompt": "You are a strict code-review verifier…",
///         "effort": "high",
///         "reasoning": "on",
///         "params": {"temperature": 0.2, "top_p": 0.9}
///       }
///     }
///   }
/// }
/// ```
///
/// Model precedence per agent: `agents.<agent>.model` >
/// `pipeline_<agent>_model` (or `default_model` for the default agent) >
/// `default_model` > the provider's own default. A model string is either
/// `provider/slug` (`--model` semantics) or, when the agent's `provider`
/// field is set, a bare slug sent verbatim to THAT provider — which is how
/// an OpenRouter key routes `openai/gpt-5.5` while an Anthropic key serves
/// the worker.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentEngineConfig {
    /// Model for the default (interactive/step-loop) agent, and the base
    /// for every pipeline role that has no more specific setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_verifier_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_worker_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_triage_model: Option<String>,
    /// The read-only research sub-agents' model. Unset, they ride the worker's
    /// — which is the tier they have always run at, not a tier anyone chose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_research_model: Option<String>,
    /// The planner's model. Unset, it rides the worker's, per `Role::Plan`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_plan_model: Option<String>,
    /// The model vocabulary the TUI pickers offer and `auto_mode` selects
    /// from. Entries are `provider/slug` strings. Empty/absent = no
    /// restriction (pickers fall back to the seed catalog).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_models: Option<Vec<String>>,
    /// Per-model output ceilings, overriding what the catalog knows the model
    /// can write. `[models.output_caps]` in TOML; keys are `provider/slug` or
    /// a bare slug, values are token counts.
    ///
    /// The default is, and stays, the model's OWN maximum — the catalog
    /// learns it from the provider and the engine asks for all of it, because
    /// a cap below the model's ceiling decides where work stops rather than
    /// the model doing so. This map exists for the one direction that is a
    /// real request: deliberately spending LESS than the model allows, to
    /// bound cost or latency, or to match a comparator that caps itself
    /// lower. Raising a model above its own ceiling is not a thing this can
    /// do — the provider rejects it — so an entry above the catalog's number
    /// is refused rather than sent (#1290).
    ///
    /// An override, never a definition. Absence means "the catalog wins",
    /// which is what keeps this from becoming the hand-written model table
    /// `[models]` exists to avoid: a model that ships tomorrow needs no entry
    /// here, and one listed here that later changes its real ceiling still
    /// gets a correct default the moment the entry is removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_output_caps: Option<std::collections::BTreeMap<String, u32>>,
    /// `on` = pick the verifier model automatically from `allowed_models`,
    /// preferring a different model family than the worker's and ranking
    /// by catalog list price (the closest objective proxy for capability
    /// tier the seed catalog carries). You never worry about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_mode: Option<Toggle>,
    /// `on` = per-agent reasoning effort is chosen automatically (verifier
    /// high, worker medium, triage low, default medium), overriding any
    /// per-agent `effort`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort_auto: Option<Toggle>,
    /// `on` = thinking mode is chosen automatically (on for verifier/worker/
    /// default, off for triage), overriding any per-agent `reasoning`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_auto: Option<Toggle>,
    /// `on` = a headless `stella run` proceeds past scope review instead of
    /// stopping at `ScopeReviewRequiredHeadless`. Only `stella run` reads it:
    /// `stella goal` and fleet workers keep the hard-off constant.
    ///
    /// Off by default, and deliberately so: scope review is the gate that
    /// keeps a large blast radius from landing unattended, and a headless
    /// surface has nobody to ask. Turn it on only where the working tree is
    /// disposable and the budget cap is the real guard — a benchmark
    /// container, CI on a scratch checkout. Without it, any plan over the
    /// scope thresholds (more than 5 steps by default) ends the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headless_scope_bypass: Option<Toggle>,
    /// Revision turns the pipeline may spend per candidate when verification
    /// fails (`stella_pipeline::PipelineConfig::max_revisions`). Absent keeps
    /// the pipeline's own default, which is what every run used before this
    /// key existed.
    ///
    /// Raising it buys near-misses another attempt and nothing else: a
    /// revision only happens on a *failed* verification, so a run that passes
    /// first time costs the same at 2 as at 4. The ceiling that actually
    /// bounds spend is the budget cap, not this number — but each revision is
    /// a full execute turn, so on a task that cannot be fixed it is the
    /// difference between failing cheaply and failing expensively.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_max_revisions: Option<u32>,
    /// Best-of-N: how many candidate executions the pipeline generates before
    /// selecting one (`stella_pipeline::PipelineConfig::candidates`). Absent
    /// or `1` is single-shot, the default.
    ///
    /// Unlike `pipeline_max_revisions` this is paid *unconditionally* — n
    /// candidates run whether or not the first one would have passed, so `2`
    /// is a straight doubling of execution cost. Opt in where the tail matters
    /// more than the bill.
    ///
    /// Wants candidate isolation to be meaningful: with a
    /// `CandidateWorkspacePort` wired, each candidate runs in its own snapshot
    /// and only the winner is adopted. Without one the pipeline warns and runs
    /// them sequentially in the shared tree, where losing candidates' edits
    /// stay behind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_candidates: Option<u32>,
    /// Escalate to the verifier when the diff-coverage overlap could not be
    /// measured (`stella_pipeline::PipelineConfig::require_diff_coverage`,
    /// #1291). Absent is off.
    ///
    /// This does not decide whether coverage is checked (it always is, where
    /// tooling exists), nor whether an unmeasured overlap is honest about
    /// itself — that is unconditional: such a run is scored UNVERIFIED rather
    /// than as a deterministic pass, whatever this says. What this decides is
    /// whether it also costs a verifier call. Turn it on in a workspace that has
    /// coverage tooling wired and wants the overlap enforced; leaving it off
    /// avoids paying a reviewer per run to be told what the evidence already
    /// said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_require_diff_coverage: Option<Toggle>,
    /// Whether a model verifier's pass with nothing deterministic behind it buys
    /// one revision demanding corroboration
    /// (`stella_pipeline::PipelineConfig::verifier_evidence_demand`, #1295).
    /// Absent keeps the pipeline's own default.
    ///
    /// Reachable as a setting because the question it answers is empirical and
    /// per-workload: the ask is only ever raised where a tracked command could
    /// answer it, so on a workload that has one it converts near-misses, and
    /// on one that does not it costs literally nothing. A benchmark arm that
    /// wants to measure the difference sets it here rather than rebuilding,
    /// which is what makes the two arms one binary and one posture key apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_verifier_evidence_demand: Option<Toggle>,
    /// Seconds of provider silence that end a single generation
    /// (`stella_core::EngineConfig::model_timeout`). Absent keeps the engine's
    /// own default, which is what every run used before this key existed.
    ///
    /// This is the third of the three coupled ceilings, and the only one that
    /// used to be reachable from nothing: the output cap lives in
    /// `agents.<role>.params.max_tokens` and the turn budget is a CLI flag, but
    /// `model_timeout` was an engine constant, so moving it meant recompiling.
    /// For a benchmark arm that made a timeout change a *system-under-test*
    /// change — a re-freeze of the registered commit rather than a line in the
    /// posture (#1211 §6.2).
    ///
    /// The three move together or not at all. Raising the output cap alone
    /// relocates the cliff rather than removing it: a step allowed 128k output
    /// tokens against a timeout sized for 64k stops on the timeout instead, and
    /// the run reports a capability difference that was really a ceiling
    /// nobody scaled. The rule that sets all three is the same — never be the
    /// side that stops first.
    ///
    /// It bounds *idle silence between stream fragments*, not elapsed time, so
    /// a generation that keeps streaming is never cut by it. Size it as a
    /// margin against a provider that stopped answering, above what the output
    /// cap can take to produce at the model's observed throughput.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_timeout_secs: Option<u64>,
    /// Conversation-size ceiling that triggers compaction, in estimated
    /// tokens (`stella_core::EngineConfig::compaction_budget_tokens`). Absent
    /// keeps the engine default, which is what every run used before this key
    /// existed. Whatever is set here is still clamped to 3/4 of the resolved
    /// model's context window — a budget above the window is a provider-side
    /// overflow scheduled in advance, never a legal configuration.
    ///
    /// This existed only as an engine constant (and a `stella-serve`
    /// override): a benchmark arm could not move it without a rebuild, which
    /// made context-handling experiments system-under-test changes rather
    /// than posture keys (#1285).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_budget_tokens: Option<u64>,
    /// Age-based tool-result retention horizon, in tool-bearing steps
    /// (`stella_core::EngineConfig::tool_result_horizon_steps`): results
    /// older than this many steps are middle-out aged on every step,
    /// independent of the compaction budget. Absent keeps the engine
    /// default; `0` disables the pass entirely (the same "0 opts out"
    /// convention as `model_timeout_secs`), restoring pure budget-triggered
    /// compaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result_horizon_steps: Option<u64>,
    /// Seconds a supervised run's parked scope-review approval
    /// (`SidecarApprovalGate::review`, #1585) waits for an attached
    /// terminal's decision before unparking itself as
    /// `ScopeDecision::Abort`. Absent or `0` (the same "0 opts out"
    /// convention as `model_timeout_secs`) parks forever, the shipped
    /// default — bounded only by discoverability (`Needs Input` in `stella
    /// daemon list`) and an explicit `stella daemon stop`.
    ///
    /// The default is deliberate: scope review exists to keep a large blast
    /// radius from landing unattended, so timing it out to `Approve` would
    /// defeat the gate, and a fail-open default nobody asked for is worse
    /// than a run that stays parked until a human looks. This is the knob
    /// for an operator who has decided the opposite trade for their own
    /// workspace — fail CLOSED after N minutes unattended, sacrificing the
    /// run rather than leaving it parked indefinitely (#1616).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_wait_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<AgentEngineAgents>,
    /// Who performs each pipeline responsibility, and whether it runs at all
    /// (`stella_pipeline::Roster`, #2381).
    ///
    /// Keys are `stella_protocol::ModelCallRole` wire tokens, because that enum
    /// is already the vocabulary every paid call in the pipeline names itself
    /// by, so a row here and a row in the paid-call ledger spell the same job
    /// the same way. The assignable set is exactly the calls the pipeline still
    /// issues — `triage`, `research`, `plan`, `worker`, `witness_author`.
    /// `verdict` and `distress_guidance` are **not** assignable: #2584 removed
    /// both calls, and `stella_pipeline::Roster::apply` rejects either key as
    /// `NotAssignable` rather than accepting a row that would steer nothing.
    /// That is structural, not a default — what was removed is authority, and
    /// an authority a config key can restore is one a deployment will restore.
    ///
    /// ```jsonc
    /// "responsibilities": {
    ///   "triage": { "enabled": false },              // ablate the stage
    ///   "witness_author": { "agent": "worker" },     // self-grade, and be told so
    ///   "research": { "agent": "plan" }              // reassign it
    /// }
    /// ```
    ///
    /// Absent — the overwhelmingly common case — is
    /// `stella_pipeline::Roster::default`, which is the pipeline exactly as it
    /// shipped. This is the ablation control the #2374 measurement plan needs
    /// and the reassignment surface that replaced a hard-coded `Role` at each
    /// call site; it deliberately cannot reorder stages, because that ordering
    /// is what makes the witness a proof (`stella_pipeline::roster`'s module
    /// docs carry the argument).
    ///
    /// A `BTreeMap` here where [`AgentEngineAgents`] is a fixed struct, and for
    /// the opposite reason: an unknown *agent* key is harmlessly ignorable, but
    /// an unknown *responsibility* key means an ablation the operator asked for
    /// is not happening — so this map is parsed loosely and then validated
    /// strictly, and a bad key refuses the run rather than being dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responsibilities: Option<std::collections::BTreeMap<String, ResponsibilitySpec>>,
}

/// One responsibility's overrides. Both fields optional: an absent one keeps
/// the built-in binding rather than pinning it to today's value.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ResponsibilitySpec {
    /// Whether the responsibility runs. `false` is the ablation switch: the
    /// stage emits no event frame and buys no call.
    ///
    /// A plain `bool` rather than the [`Toggle`] the mode keys use, because
    /// this is a predicate and not a mode — and because a bare `bool` makes
    /// `enabled = "no"` a parse error rather than a silently ignored key,
    /// which is the refusal an ablation control needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Which agent performs it: `worker`, `triage`, `plan`, or `verifier` —
    /// the same names [`AgentEngineAgents`] uses, so one spelling serves both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

/// The `agents` map — fixed keys rather than a `BTreeMap` so per-role
/// access is exhaustive and typed instead of stringly (a misspelled agent
/// key in JSON is simply ignored, the same tolerance every other settings
/// object has for unknown fields).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentEngineAgents {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<AgentEngineAgent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<AgentEngineAgent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier: Option<AgentEngineAgent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triage: Option<AgentEngineAgent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research: Option<AgentEngineAgent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<AgentEngineAgent>,
}

impl AgentEngineAgents {
    pub fn get(&self, kind: EngineAgentKind) -> Option<&AgentEngineAgent> {
        match kind {
            EngineAgentKind::Default => self.default.as_ref(),
            EngineAgentKind::Worker => self.worker.as_ref(),
            EngineAgentKind::Verifier => self.verifier.as_ref(),
            EngineAgentKind::Triage => self.triage.as_ref(),
            EngineAgentKind::Research => self.research.as_ref(),
            EngineAgentKind::Plan => self.plan.as_ref(),
        }
    }

    pub fn get_mut(&mut self, kind: EngineAgentKind) -> &mut Option<AgentEngineAgent> {
        match kind {
            EngineAgentKind::Default => &mut self.default,
            EngineAgentKind::Worker => &mut self.worker,
            EngineAgentKind::Verifier => &mut self.verifier,
            EngineAgentKind::Triage => &mut self.triage,
            EngineAgentKind::Research => &mut self.research,
            EngineAgentKind::Plan => &mut self.plan,
        }
    }
}

/// One agent's engine overrides. Every field optional — an absent field
/// means "no opinion", falling through to the flat model fields / engine
/// defaults / the provider's own defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentEngineAgent {
    /// Model slug — `provider/slug`, or a bare slug when `provider` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Gateway/provider id this agent's requests go through (a built-in id
    /// or a settings-defined provider). When set, `model` is sent verbatim
    /// as the slug to this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Custom system prompt replacing the built-in one for this agent.
    /// Workspace memories/rules still append to it (they are additive
    /// context, not part of the base instruction set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Reasoning effort (`low`/`medium`/`high`/`xhigh`/`max`), forwarded
    /// as `CompletionRequest::effort`. Overridden by `effort_auto: on`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    /// Thinking mode on/off, forwarded as `CompletionRequest::reasoning`.
    /// Absent = the provider's default. Overridden by `reasoning_auto: on`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Toggle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<AgentEngineParams>,
}

/// Per-agent generation parameters — the "Include" checkbox model: a
/// `Some` value goes on the wire (where the provider's dialect supports
/// it), `None` leaves the provider default untouched. `temperature` and
/// `max_tokens` land on `CompletionRequest` directly; the rest ride
/// `CompletionRequest::params`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentEngineParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repetition_penalty: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<Verbosity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,
}

impl AgentEngineParams {
    /// Overlay `other` (higher precedence) onto `self`, per field.
    pub(super) fn overlay(&mut self, other: &AgentEngineParams) {
        macro_rules! take {
            ($field:ident) => {
                if other.$field.is_some() {
                    self.$field = other.$field;
                }
            };
        }
        take!(temperature);
        take!(top_p);
        take!(top_k);
        take!(frequency_penalty);
        take!(presence_penalty);
        take!(repetition_penalty);
        take!(max_tokens);
        take!(seed);
        take!(verbosity);
        take!(service_tier);
    }
}

impl AgentEngineAgent {
    /// Overlay `other` (higher precedence) onto `self`, per field —
    /// `params` composes recursively so a project scope can set one knob
    /// without clobbering the user scope's others.
    pub(super) fn overlay(&mut self, other: &AgentEngineAgent) {
        macro_rules! take {
            ($field:ident) => {
                if other.$field.is_some() {
                    self.$field = other.$field.clone();
                }
            };
        }
        take!(model);
        take!(provider);
        take!(prompt);
        take!(effort);
        take!(reasoning);
        if let Some(params) = &other.params {
            self.params
                .get_or_insert_with(AgentEngineParams::default)
                .overlay(params);
        }
    }
}

impl AgentEngineConfig {
    /// Overlay `other` (higher precedence) onto `self`, per field, per
    /// agent — the same composition rule as `ProviderSettings::overlay`.
    /// `allowed_models` REPLACES wholesale (it is one vocabulary, not a
    /// set of independent knobs — concatenating scopes would make it
    /// impossible for a project to narrow the user's list).
    pub(super) fn overlay(&mut self, other: &AgentEngineConfig) {
        macro_rules! take {
            ($field:ident) => {
                if other.$field.is_some() {
                    self.$field = other.$field.clone();
                }
            };
        }
        take!(default_model);
        take!(responsibilities);
        take!(pipeline_verifier_model);
        take!(pipeline_worker_model);
        take!(pipeline_triage_model);
        take!(pipeline_research_model);
        take!(pipeline_plan_model);
        take!(allowed_models);
        // Per KEY, not wholesale — the deliberate opposite of
        // `allowed_models` two lines up, and the contrast is the reason
        // either rule is right. `allowed_models` is one vocabulary, so a
        // project narrowing it must be able to replace the user's list
        // entirely. These are independent per-model knobs: a project pinning
        // one model's ceiling has said nothing about any other model, and
        // replacing the map wholesale would silently drop the user's pins on
        // models the project never mentioned.
        if let Some(caps) = &other.model_output_caps {
            let target = self.model_output_caps.get_or_insert_with(Default::default);
            for (model, cap) in caps {
                target.insert(model.clone(), *cap);
            }
        }
        take!(auto_mode);
        take!(effort_auto);
        take!(reasoning_auto);
        // Was omitted from this list: a higher-precedence scope setting it
        // had its value discarded, so the LOWER scope silently won the one
        // knob deciding whether an unattended run may proceed past scope
        // review. Latent while nothing layered underneath the user's file;
        // load-bearing now that `provider_engine_baseline` does.
        take!(headless_scope_bypass);
        take!(pipeline_max_revisions);
        take!(pipeline_candidates);
        take!(pipeline_verifier_evidence_demand);
        take!(pipeline_require_diff_coverage);
        take!(model_timeout_secs);
        take!(compaction_budget_tokens);
        take!(tool_result_horizon_steps);
        take!(approval_wait_secs);
        if let Some(agents) = &other.agents {
            let target = self.agents.get_or_insert_with(AgentEngineAgents::default);
            for kind in EngineAgentKind::ALL {
                if let Some(agent) = agents.get(kind) {
                    target
                        .get_mut(kind)
                        .get_or_insert_with(AgentEngineAgent::default)
                        .overlay(agent);
                }
            }
        }
    }

    /// Layer `self` OVER `baseline`, field by field and agent by agent —
    /// `self` (the user's merged settings) wins every field it sets, and
    /// `baseline` answers only where the user had no opinion.
    ///
    /// This is the same composition rule the settings scope chain already
    /// uses; the difference is only what sits underneath. It exists so a
    /// gateway can ship a default posture (see
    /// [`crate::engine_config::provider_engine_baseline`]) that is a genuine
    /// *default* — visible in `stella config`, overridable at any scope, and
    /// never able to displace something the user actually wrote down.
    pub fn layered_over(&self, baseline: &AgentEngineConfig) -> AgentEngineConfig {
        let mut merged = baseline.clone();
        merged.overlay(self);
        merged
    }

    /// The per-agent overrides for `kind`, if any.
    pub fn agent(&self, kind: EngineAgentKind) -> Option<&AgentEngineAgent> {
        self.agents.as_ref().and_then(|a| a.get(kind))
    }

    /// The effective model STRING for `kind` (not yet resolved to a
    /// provider): `agents.<kind>.model` > the flat per-role field >
    /// `default_model`. `None` = no opinion (auto-detect / `--model` /
    /// provider default decide, exactly as before this config existed).
    pub fn model_for(&self, kind: EngineAgentKind) -> Option<&str> {
        if let Some(model) = self.agent(kind).and_then(|a| a.model.as_deref()) {
            return Some(model);
        }
        let flat = match kind {
            EngineAgentKind::Default => None,
            EngineAgentKind::Worker => self.pipeline_worker_model.as_deref(),
            EngineAgentKind::Verifier => self.pipeline_verifier_model.as_deref(),
            EngineAgentKind::Triage => self.pipeline_triage_model.as_deref(),
            // Both fall through to `default_model` below when unset, which is
            // where they have always landed — the worker's flat key is not
            // consulted for them, since a worker pin says nothing about what
            // a read-only research call or a single planning call should run.
            EngineAgentKind::Research => self.pipeline_research_model.as_deref(),
            EngineAgentKind::Plan => self.pipeline_plan_model.as_deref(),
        };
        flat.or(self.default_model.as_deref())
    }

    /// The allowed-model vocabulary, empty when unrestricted.
    pub fn allowed_models(&self) -> &[String] {
        self.allowed_models.as_deref().unwrap_or(&[])
    }

    /// The operator's deliberate output ceiling for one model, if they set
    /// one. `None` means what it should mean everywhere in this feature: use
    /// the model's own maximum.
    ///
    /// Both spellings resolve, qualified first: `anthropic/claude-sonnet-5`
    /// pins that model on that provider, while a bare `claude-sonnet-5` pins
    /// it wherever it is reached from. Both are useful and they are not the
    /// same request — capping a model only on the gateway you pay per-token
    /// for is a real thing to want — so the qualified form wins when both are
    /// present rather than one silently shadowing the other.
    ///
    /// `0` is not admissible and is treated as unset. Elsewhere in this
    /// engine `0` spells "no ceiling" (`model_timeout_secs`), but here that
    /// reading is unavailable: the field's absence already means "the model's
    /// own maximum", so a literal zero can only be a mistake — and honoring
    /// it would ask the provider for an empty completion on every step.
    pub fn model_output_cap(&self, provider: &str, slug: &str) -> Option<u32> {
        let caps = self.model_output_caps.as_ref()?;
        caps.get(&format!("{provider}/{slug}"))
            .or_else(|| caps.get(slug))
            .copied()
            .filter(|cap| *cap > 0)
    }

    pub fn auto_mode_on(&self) -> bool {
        self.auto_mode.is_some_and(Toggle::is_on)
    }

    pub fn effort_auto_on(&self) -> bool {
        self.effort_auto.is_some_and(Toggle::is_on)
    }

    pub fn reasoning_auto_on(&self) -> bool {
        self.reasoning_auto.is_some_and(Toggle::is_on)
    }

    /// Whether a headless run skips the (now-removed) staged pipeline's
    /// scope-review gate rather than refusing outright. The gate itself is
    /// gone with the pipeline (#3846); this accessor and the setting it reads
    /// survive only because settings-merge tests still exercise the
    /// scope-chain precedence rule for `agent_engine_config.headless_scope_bypass`
    /// through it — no production code consults the answer any more.
    #[allow(dead_code)]
    pub fn headless_scope_bypass_on(&self) -> bool {
        self.headless_scope_bypass.is_some_and(Toggle::is_on)
    }

    /// Persist THIS config as the `agent_engine_config` key of the
    /// settings file at `path`, preserving every other key in the file
    /// byte-for-byte at the value level (providers, hooks, mcp, and any
    /// forward-compat keys survive a TUI save untouched). Creates the file
    /// (and parent directories) when absent.
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        // The FORMAT is a property of the target file, not of the caller, so
        // every existing call site keeps working unchanged once its path
        // resolver starts returning a `.toml`.
        if toml_config::path_is_toml(path) {
            let sections = toml_config::agent_sections(self)?;
            let user_private = user_config_path().as_deref() == Some(path);
            return toml_io::save_sections(path, &sections, user_private);
        }
        private::reject_symlink(path)?;
        let mut root: serde_json::Value = match std::fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents)
                .map_err(|e| format!("invalid settings file {}: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        let object = root
            .as_object_mut()
            .ok_or_else(|| format!("settings file {} is not a JSON object", path.display()))?;
        let value = serde_json::to_value(self)
            .map_err(|e| format!("cannot serialize agent_engine_config: {e}"))?;
        object.insert("agent_engine_config".to_string(), value);
        let mut rendered = serde_json::to_string_pretty(&root)
            .map_err(|e| format!("cannot render settings: {e}"))?;
        rendered.push('\n');
        let user_private = user_settings_path().as_deref() == Some(path);
        // Project settings are canonical committable configuration: the
        // persistence layer reserves owner-only atomic writes for user scope.
        private::write_settings(path, rendered.as_bytes(), user_private)
    }
}
/// The `agent_engine_config` recorded in ONE user-scope file, ignoring every
/// other scope.
///
/// [`Settings::load`] returns the MERGED view of user + managed + project. That
/// is the right input for *running* the engine and the wrong input for
/// *editing* one file: [`AgentEngineConfig::save_to`] replaces the whole block,
/// so a caller that loads the merged config, changes one field, and saves to
/// user scope also copies the project's and the org's opinions into the user
/// file — silently promoting a per-repo model pin to a machine-wide default.
/// An editor reads the scope it is about to write.
///
/// A **missing** file is an empty config — there is nothing to preserve, and
/// the caller is about to create it. A file that exists but cannot be parsed is
/// an **error**, and deliberately not a fallback to empty: because the caller
/// then writes the whole block back, degrading here would let one malformed key
/// somewhere else in the file destroy an engine config this editor never owned.
/// A named error the user can act on beats silently discarding their settings.
///
/// User scope specifically, because it is the only scope a slash command
/// writes; a project-scope sibling would need to pass its own
/// [`toml_config::ConfigScope`] so a TOML file's declared scope still validates.
pub fn user_engine_config_at(path: &Path) -> Result<AgentEngineConfig, String> {
    let settings = if toml_config::path_is_toml(path) {
        // A closure, not `std::fs::read_to_string` by name: the free function
        // is generic over `AsRef<Path>`, so inference pins it to one concrete
        // lifetime and it stops satisfying the reader's for-any-lifetime bound.
        toml_config::load_toml_scope(path, toml_config::ConfigScope::User, |p: &Path| {
            std::fs::read_to_string(p)
        })?
        .settings
    } else {
        Settings::load_scope(path)?
    };
    Ok(settings.agent_engine_config.unwrap_or_default())
}
