//! What each engine role will *actually* run — the per-role half of
//! `stella config`.
//!
//! `stella config` used to report only the session's provider, model and
//! credential. That is one of four models a pipeline turn uses, and the other
//! three are exactly the ones that go wrong quietly: a judge pinned through a
//! key that a per-agent `provider` outranks, a triage model whose slug is
//! spelled for the wrong gateway, an `effort` the auto-mode is silently
//! replacing. None of it was visible without reading the resolver.
//!
//! Worse, it was not *checkable*. Confirming a settings change meant editing
//! in a deliberate typo to see whether stella complained about the key next to
//! it — an inference from silence. This module makes the answer printable:
//! for every role, the model that will be sent, the effort and thinking that
//! ride with it, and the setting that decided each.
//!
//! It resolves through the same functions the request path uses
//! ([`model_spec_for`], [`tuning_for`], [`AgentEngineConfig::model_for`]'s
//! precedence) rather than re-deriving the rules, because a config report that
//! can disagree with the engine is worse than none.

use crate::engine_config::{ModelSpec, model_spec_for, tuning_for};
use crate::settings::{AgentEngineConfig, EngineAgentKind};
use stella_protocol::ReasoningEffort;

/// One role's resolved wiring.
#[derive(Debug, Clone, PartialEq)]
pub struct RoleWiring {
    pub role: EngineAgentKind,
    /// Provider id and the slug as it goes on the wire.
    pub model: ModelSpec,
    /// The setting that decided `model` — the string a user would edit.
    pub source: ModelSource,
    pub effort: Option<ReasoningEffort>,
    pub reasoning: Option<bool>,
    /// Set when `effort_auto` replaced an effort the user pinned for this
    /// role. The pinned value is carried so the report can name what was
    /// discarded — the failure this whole module exists to surface.
    pub effort_auto_replaced: Option<ReasoningEffort>,
    /// Same, for `reasoning_auto` over a pinned `reasoning`.
    pub reasoning_auto_replaced: Option<bool>,
}

/// Which setting decided a role's model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelSource {
    /// `--model` / `STELLA_MODEL`. Pins the worker for this invocation and
    /// suppresses the worker's settings spec (see `resolve_engine_wiring`).
    Flag,
    /// `agents.<role>.model`, which outranks every flat key.
    PerAgent(EngineAgentKind),
    /// `pipeline_<role>_model`.
    Flat(EngineAgentKind),
    /// `default_model`.
    DefaultModel,
    /// Nothing named it: the role rides the session's resolved model, which
    /// is the provider row's own default when settings are silent too.
    SessionDefault,
}

impl ModelSource {
    /// The settings key a user would edit to change this, as they would type
    /// it. Not a description — a path.
    pub fn label(self) -> String {
        match self {
            Self::Flag => "--model (this invocation)".to_string(),
            Self::PerAgent(kind) => format!("agents.{}.model", role_key(kind)),
            Self::Flat(kind) => format!("pipeline_{}_model", role_key(kind)),
            Self::DefaultModel => "default_model".to_string(),
            Self::SessionDefault => "session default".to_string(),
        }
    }
}

/// The role's name in settings keys and in the report's first column.
pub fn role_key(kind: EngineAgentKind) -> &'static str {
    match kind {
        EngineAgentKind::Default => "default",
        EngineAgentKind::Worker => "worker",
        EngineAgentKind::Judge => "judge",
        EngineAgentKind::Triage => "triage",
    }
}

/// Which setting fed `model_for(kind)`, mirroring its precedence exactly:
/// `agents.<kind>.model` > `pipeline_<kind>_model` > `default_model`.
///
/// `EngineAgentKind::Default` has no flat key of its own — `default_model`
/// *is* its flat key — which is why the middle arm skips it.
fn model_source(engine: &AgentEngineConfig, kind: EngineAgentKind) -> ModelSource {
    if engine
        .agent(kind)
        .and_then(|a| a.model.as_deref())
        .is_some()
    {
        return ModelSource::PerAgent(kind);
    }
    let flat = match kind {
        EngineAgentKind::Default => None,
        EngineAgentKind::Worker => engine.pipeline_worker_model.as_deref(),
        EngineAgentKind::Judge => engine.pipeline_judge_model.as_deref(),
        EngineAgentKind::Triage => engine.pipeline_triage_model.as_deref(),
    };
    if flat.is_some() {
        ModelSource::Flat(kind)
    } else if engine.default_model.is_some() {
        ModelSource::DefaultModel
    } else {
        ModelSource::SessionDefault
    }
}

/// Resolve all four roles.
///
/// `session` is the model the session actually resolved to (`Config`'s own
/// provider + model), which is what a role falls back to when no setting names
/// one — including the case where settings are absent entirely.
///
/// `model_pinned_by_flag` reproduces the one asymmetry in
/// `resolve_engine_wiring`: an explicit `--model` is a per-invocation pin of
/// the WORKER, so it suppresses the worker's settings spec while leaving
/// judge and triage pins alone. Reporting the settings value there would name
/// a model that is not going to run.
pub fn resolve(
    engine: Option<&AgentEngineConfig>,
    session: &ModelSpec,
    model_pinned_by_flag: bool,
    is_provider: &dyn Fn(&str) -> bool,
) -> Vec<RoleWiring> {
    EngineAgentKind::ALL
        .iter()
        .map(|&role| {
            let Some(engine) = engine else {
                return RoleWiring {
                    role,
                    model: session.clone(),
                    source: ModelSource::SessionDefault,
                    effort: None,
                    reasoning: None,
                    effort_auto_replaced: None,
                    reasoning_auto_replaced: None,
                };
            };
            // `--model` owns the worker and the default agent it is resolved
            // from; the judge and triage keep their own pins.
            let flag_owns = model_pinned_by_flag
                && matches!(role, EngineAgentKind::Worker | EngineAgentKind::Default);
            let spec = if flag_owns {
                None
            } else {
                model_spec_for(engine, role, is_provider)
            };
            let (model, source) = match spec {
                Some(spec) => (spec, model_source(engine, role)),
                None if flag_owns => (session.clone(), ModelSource::Flag),
                None => (session.clone(), ModelSource::SessionDefault),
            };
            let tuning = tuning_for(engine, role);
            // What the user pinned, to compare against what auto resolved.
            let pinned = engine.agent(role);
            let effort_auto_replaced = pinned
                .and_then(|a| a.effort)
                .filter(|_| engine.effort_auto_on());
            let reasoning_auto_replaced = pinned
                .and_then(|a| a.reasoning)
                .map(|t| t.is_on())
                .filter(|_| engine.reasoning_auto_on());
            RoleWiring {
                role,
                model,
                source,
                effort: tuning.effort,
                reasoning: tuning.reasoning,
                effort_auto_replaced,
                reasoning_auto_replaced,
            }
        })
        .collect()
}

/// How an effort renders, including the auto-mode's theft when it happened.
fn effort_cell(row: &RoleWiring) -> String {
    let resolved = row
        .effort
        .map(|e| effort_word(e).to_string())
        .unwrap_or_else(|| "provider default".to_string());
    match row.effort_auto_replaced {
        // Only worth saying when auto actually CHANGED the answer — an
        // `effort_auto` that happens to agree with the pin is not a surprise
        // anyone needs warning about.
        Some(pinned) if Some(pinned) != row.effort => {
            format!(
                "{resolved}  (effort_auto replaced \"{}\")",
                effort_word(pinned)
            )
        }
        _ => resolved,
    }
}

/// Same for thinking.
fn reasoning_cell(row: &RoleWiring) -> String {
    let resolved = match row.reasoning {
        Some(true) => "thinking on",
        Some(false) => "thinking off",
        None => "thinking default",
    };
    match row.reasoning_auto_replaced {
        Some(pinned) if Some(pinned) != row.reasoning => {
            format!(
                "{resolved}  (reasoning_auto replaced \"{}\")",
                if pinned { "on" } else { "off" }
            )
        }
        _ => resolved.to_string(),
    }
}

fn effort_word(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Xhigh => "xhigh",
        ReasoningEffort::Max => "max",
    }
}

/// The report as plain rows (`role`, `provider/model`, `effort`, `thinking`,
/// `source`), column-aligned. Returned rather than printed so the same text is
/// what tests assert on.
pub fn rows(wiring: &[RoleWiring]) -> Vec<[String; 5]> {
    wiring
        .iter()
        .map(|row| {
            [
                role_key(row.role).to_string(),
                format!("{}/{}", row.model.provider, row.model.model),
                effort_cell(row),
                reasoning_cell(row),
                row.source.label(),
            ]
        })
        .collect()
}

/// Render the rows as the indented block `stella config` prints, padded so the
/// columns line up. Plain text — the caller colorizes.
pub fn render(wiring: &[RoleWiring]) -> Vec<String> {
    let rows = rows(wiring);
    let widths = (0..4).map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0));
    let widths: Vec<usize> = widths.collect();
    rows.iter()
        .map(|r| {
            let mut line = String::new();
            for (i, cell) in r.iter().take(4).enumerate() {
                line.push_str(cell);
                line.push_str(&" ".repeat(widths[i].saturating_sub(cell.chars().count()) + 2));
            }
            line.push_str(&r[4]);
            line.trim_end().to_string()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{AgentEngineAgent, AgentEngineAgents, Toggle};

    fn spec(provider: &str, model: &str) -> ModelSpec {
        ModelSpec {
            provider: provider.to_string(),
            model: model.to_string(),
        }
    }

    fn known(id: &str) -> bool {
        matches!(id, "openrouter" | "anthropic" | "zai")
    }

    fn find(wiring: &[RoleWiring], role: EngineAgentKind) -> &RoleWiring {
        wiring.iter().find(|r| r.role == role).unwrap()
    }

    /// With no engine settings at all, every role rides the session model and
    /// says so — the report must never imply a pin that does not exist.
    #[test]
    fn absent_settings_report_the_session_model_for_every_role() {
        let session = spec("openrouter", "moonshotai/kimi-k3");
        let wiring = resolve(None, &session, false, &known);
        assert_eq!(wiring.len(), 4);
        for row in &wiring {
            assert_eq!(row.model, session);
            assert_eq!(row.source, ModelSource::SessionDefault);
        }
    }

    /// The posture this feature was written to make checkable: worker rides
    /// `default_model`, judge and triage have their own flat keys, and each
    /// row names the key that decided it.
    #[test]
    fn each_role_names_the_setting_that_decided_its_model() {
        let engine = AgentEngineConfig {
            default_model: Some("openrouter/moonshotai/kimi-k3".into()),
            pipeline_judge_model: Some("openrouter/anthropic/claude-opus-5".into()),
            pipeline_triage_model: Some("openrouter/z-ai/glm-5.2".into()),
            ..AgentEngineConfig::default()
        };
        let session = spec("openrouter", "moonshotai/kimi-k3");
        let wiring = resolve(Some(&engine), &session, false, &known);

        let worker = find(&wiring, EngineAgentKind::Worker);
        assert_eq!(worker.model.model, "moonshotai/kimi-k3");
        assert_eq!(worker.source, ModelSource::DefaultModel);

        let judge = find(&wiring, EngineAgentKind::Judge);
        assert_eq!(judge.model.model, "anthropic/claude-opus-5");
        assert_eq!(
            judge.source,
            ModelSource::Flat(EngineAgentKind::Judge),
            "the judge's flat key is what a user would edit"
        );
        assert_eq!(judge.source.label(), "pipeline_judge_model");

        let triage = find(&wiring, EngineAgentKind::Triage);
        assert_eq!(triage.model.model, "z-ai/glm-5.2");
        assert_eq!(triage.source.label(), "pipeline_triage_model");
    }

    /// `agents.<role>.model` outranks the flat key, and the report has to say
    /// which one won — editing the flat key when the per-agent one is set is
    /// the change that appears to do nothing.
    #[test]
    fn a_per_agent_model_outranks_the_flat_key_in_the_report() {
        let engine = AgentEngineConfig {
            default_model: Some("openrouter/moonshotai/kimi-k3".into()),
            pipeline_judge_model: Some("openrouter/anthropic/claude-opus-5".into()),
            agents: Some(AgentEngineAgents {
                judge: Some(AgentEngineAgent {
                    model: Some("openrouter/anthropic/claude-fable-5".into()),
                    ..AgentEngineAgent::default()
                }),
                ..AgentEngineAgents::default()
            }),
            ..AgentEngineConfig::default()
        };
        let session = spec("openrouter", "moonshotai/kimi-k3");
        let wiring = resolve(Some(&engine), &session, false, &known);
        let judge = find(&wiring, EngineAgentKind::Judge);
        assert_eq!(judge.model.model, "anthropic/claude-fable-5");
        assert_eq!(judge.source.label(), "agents.judge.model");
    }

    /// The bug this exists to catch. `effort_auto: on` silently replaces a
    /// pinned effort with its own ladder (worker → medium), so a config that
    /// reads `"effort": "max"` runs at medium. The row must show the resolved
    /// value AND name what was discarded.
    #[test]
    fn effort_auto_replacing_a_pin_is_named_in_the_row() {
        let engine = AgentEngineConfig {
            default_model: Some("openrouter/moonshotai/kimi-k3".into()),
            effort_auto: Some(Toggle::On),
            agents: Some(AgentEngineAgents {
                worker: Some(AgentEngineAgent {
                    effort: Some(ReasoningEffort::Max),
                    ..AgentEngineAgent::default()
                }),
                ..AgentEngineAgents::default()
            }),
            ..AgentEngineConfig::default()
        };
        let session = spec("openrouter", "moonshotai/kimi-k3");
        let wiring = resolve(Some(&engine), &session, false, &known);
        let worker = find(&wiring, EngineAgentKind::Worker);
        assert_eq!(
            worker.effort,
            Some(ReasoningEffort::Medium),
            "effort_auto's worker rung is what actually runs"
        );
        assert_eq!(worker.effort_auto_replaced, Some(ReasoningEffort::Max));
        let cell = effort_cell(worker);
        assert!(
            cell.contains("medium") && cell.contains("effort_auto replaced \"max\""),
            "the row states both what runs and what was thrown away: {cell}"
        );
    }

    /// An auto mode that happens to agree with the pin is not a surprise, and
    /// saying so on every row would train people to ignore the warning.
    #[test]
    fn an_auto_mode_that_agrees_with_the_pin_says_nothing() {
        let engine = AgentEngineConfig {
            default_model: Some("openrouter/moonshotai/kimi-k3".into()),
            effort_auto: Some(Toggle::On),
            agents: Some(AgentEngineAgents {
                judge: Some(AgentEngineAgent {
                    // effort_auto's judge rung is High — the same value.
                    effort: Some(ReasoningEffort::High),
                    ..AgentEngineAgent::default()
                }),
                ..AgentEngineAgents::default()
            }),
            ..AgentEngineConfig::default()
        };
        let session = spec("openrouter", "moonshotai/kimi-k3");
        let wiring = resolve(Some(&engine), &session, false, &known);
        let cell = effort_cell(find(&wiring, EngineAgentKind::Judge));
        assert_eq!(cell, "high", "no noise when nothing was replaced: {cell}");
    }

    /// `--model` pins the worker for one invocation and suppresses the
    /// worker's settings spec — but not the judge's. Reporting the settings
    /// value for the worker there would name a model that will not run.
    #[test]
    fn an_explicit_model_flag_owns_the_worker_but_not_the_judge() {
        let engine = AgentEngineConfig {
            default_model: Some("openrouter/moonshotai/kimi-k3".into()),
            pipeline_worker_model: Some("openrouter/z-ai/glm-5.2".into()),
            pipeline_judge_model: Some("openrouter/anthropic/claude-opus-5".into()),
            ..AgentEngineConfig::default()
        };
        let session = spec("anthropic", "claude-sonnet-5");
        let wiring = resolve(Some(&engine), &session, true, &known);

        let worker = find(&wiring, EngineAgentKind::Worker);
        assert_eq!(worker.model, session, "the flag's model is what runs");
        assert_eq!(worker.source, ModelSource::Flag);

        let judge = find(&wiring, EngineAgentKind::Judge);
        assert_eq!(
            judge.model.model, "anthropic/claude-opus-5",
            "--model says nothing about the judge"
        );
    }

    /// The rendered block is column-aligned and every row carries its source.
    #[test]
    fn the_rendered_block_aligns_and_keeps_the_source_on_every_row() {
        let engine = AgentEngineConfig {
            default_model: Some("openrouter/moonshotai/kimi-k3".into()),
            pipeline_judge_model: Some("openrouter/anthropic/claude-opus-5".into()),
            ..AgentEngineConfig::default()
        };
        let session = spec("openrouter", "moonshotai/kimi-k3");
        let lines = render(&resolve(Some(&engine), &session, false, &known));
        assert_eq!(lines.len(), 4);
        for line in &lines {
            assert!(
                line.contains("default_model") || line.contains("pipeline_judge_model"),
                "every row names its source: {line}"
            );
        }
        // The model column starts at the same offset on every row.
        let offsets: Vec<_> = lines.iter().map(|l| l.find("openrouter/")).collect();
        assert!(
            offsets.iter().all(|o| *o == offsets[0]),
            "columns align: {lines:#?}"
        );
    }
}
