//! The trusted-launcher engine seam: `STELLA_ENGINE_CONFIG_JSON`.
//!
//! A sibling module rather than more lines in [the parent](super) — these
//! tests grew past what `config/tests.rs` could hold under the 1500-line
//! ratchet, and they are one subject: what a benchmark harness may freeze into
//! a run, and what the seam refuses.
//!
//! The seam fails **closed** on any key outside its vocabulary
//! (`trusted_engine_config_shape_is_strict`), which is why the posture's role
//! vocabulary is tested here at all: a role the seam has never heard of does
//! not degrade to an ignored key, it refuses the run.

use super::*;

#[test]
fn trusted_engine_json_replaces_adversarial_project_engine_settings() {
    use crate::settings::EngineAgentKind;
    use stella_protocol::ReasoningEffort;

    let settings = settings_from(
        r#"{
            "providers": {"openrouter": {"api_key": "sk-or-test"}},
            "agent_engine_config": {
                "default_model": "anthropic/task-chosen-model",
                "pipeline_worker_model": "zai/task-worker",
                "auto_mode": "on",
                "effort_auto": "on",
                "reasoning_auto": "on",
                "agents": {
                    "default": {"provider": "anthropic", "model": "task-default"},
                    "worker": {"provider": "zai", "model": "task-worker"},
                    "verifier": {"provider": "openai", "model": "task-verifier"},
                    "triage": {"provider": "deepseek", "model": "task-triage"}
                }
            }
        }"#,
    );
    let trusted = r#"{
        "default_model":"openrouter/deepseek/deepseek-v4-pro",
        "allowed_models":["openrouter/deepseek/deepseek-v4-pro"],
        "auto_mode":"off",
        "effort_auto":"off",
        "reasoning_auto":"off",
        "agents":{
            "default":{"effort":"high","reasoning":"on"},
            "worker":{"effort":"high","reasoning":"on"},
            "verifier":{"effort":"high","reasoning":"on"},
            "triage":{"effort":"low","reasoning":"off"}
        }
    }"#;

    let _env = crate::test_env::lock();
    // SAFETY: test-only process environment mutation serialized by test_env.
    unsafe { std::env::set_var(TRUSTED_ENGINE_CONFIG_ENV, trusted) };
    let cfg = Config::load_with_settings(
        Some("openrouter/deepseek/deepseek-v4-pro"),
        None,
        Some("https://openrouter.ai/api/v1"),
        &settings,
        std::path::PathBuf::from("/tmp/ws"),
    )
    .expect("trusted engine posture should replace project settings");
    unsafe { std::env::remove_var(TRUSTED_ENGINE_CONFIG_ENV) };

    let engine = cfg
        .engine_settings
        .expect("trusted engine config is stamped");
    assert_eq!(
        engine.default_model.as_deref(),
        Some("openrouter/deepseek/deepseek-v4-pro")
    );
    assert!(engine.pipeline_worker_model.is_none());
    assert!(engine.pipeline_verifier_model.is_none());
    assert!(engine.pipeline_triage_model.is_none());
    assert!(!engine.auto_mode_on());
    assert!(!engine.effort_auto_on());
    assert!(!engine.reasoning_auto_on());
    for kind in EngineAgentKind::ALL {
        let spec = crate::engine_config::model_spec_for(&engine, kind, &|id| id == "openrouter")
            .expect("every role inherits the trusted default model");
        assert_eq!(spec.provider, "openrouter");
        assert_eq!(spec.model, "deepseek/deepseek-v4-pro");
        let tuning = crate::engine_config::tuning_for(&engine, kind);
        match kind {
            EngineAgentKind::Triage => {
                assert_eq!(tuning.effort, Some(ReasoningEffort::Low));
                assert_eq!(tuning.reasoning, Some(false));
            }
            // The posture above names four rows, so these two carry no tuning
            // of their own — and that is the #2374 defect stated as an
            // assertion rather than left implicit. A seat writing this exact
            // JSON has said nothing about research or plan; what they end up
            // running is decided downstream, by `resolve_engine_wiring`
            // inheriting the worker's row field by field.
            EngineAgentKind::Research | EngineAgentKind::Plan => {
                assert!(
                    engine.agent(kind).is_none(),
                    "{kind:?} is not in the posture"
                );
                assert_eq!(tuning.effort, None);
                assert_eq!(tuning.reasoning, None);
                continue;
            }
            _ => {
                assert_eq!(tuning.effort, Some(ReasoningEffort::High));
                assert_eq!(tuning.reasoning, Some(true));
            }
        }
        let agent = engine
            .agent(kind)
            .expect("every named tuning row is pinned");
        assert!(agent.model.is_none());
        assert!(agent.provider.is_none());
        assert!(agent.prompt.is_none());
        assert!(agent.params.is_none());
    }
}

/// **Witness (#2374), trusted-seam half.** A benchmark seat must be able to
/// write research and plan into the frozen posture and have them survive.
///
/// The seam fails CLOSED on any key outside its vocabulary
/// (`trusted_engine_config_shape_is_strict`), so before the vocabulary grew,
/// a seat that tried this did not get a turned-down research role — it got a
/// refused run. That is the whole reason the knob has to exist on both sides
/// of the container boundary in the same change.
#[test]
fn a_trusted_posture_may_pin_research_and_plan() {
    use crate::settings::EngineAgentKind;
    use stella_protocol::ReasoningEffort;

    let settings = settings_from(r#"{"providers": {"openrouter": {"api_key": "sk-or-test"}}}"#);
    let trusted = r#"{
        "default_model":"openrouter/deepseek/deepseek-v4-pro",
        "pipeline_research_model":"openrouter/z-ai/glm-5.2",
        "effort_auto":"off",
        "reasoning_auto":"off",
        "agents":{
            "worker":{"effort":"xhigh","reasoning":"on"},
            "research":{"effort":"low","reasoning":"off"},
            "plan":{"effort":"medium","reasoning":"on"}
        }
    }"#;

    let _env = crate::test_env::lock();
    // SAFETY: test-only process environment mutation serialized by test_env.
    unsafe { std::env::set_var(TRUSTED_ENGINE_CONFIG_ENV, trusted) };
    let cfg = Config::load_with_settings(
        Some("openrouter/deepseek/deepseek-v4-pro"),
        None,
        Some("https://openrouter.ai/api/v1"),
        &settings,
        std::path::PathBuf::from("/tmp/ws"),
    )
    .expect("a posture naming research and plan is accepted, not refused");
    unsafe { std::env::remove_var(TRUSTED_ENGINE_CONFIG_ENV) };

    let engine = cfg
        .engine_settings
        .expect("trusted engine config is stamped");
    let research = crate::engine_config::tuning_for(&engine, EngineAgentKind::Research);
    assert_eq!(
        (research.effort, research.reasoning),
        (Some(ReasoningEffort::Low), Some(false)),
        "the seat's research posture must reach the engine, not be dropped at the seam"
    );
    assert_eq!(
        crate::engine_config::tuning_for(&engine, EngineAgentKind::Plan).effort,
        Some(ReasoningEffort::Medium)
    );
    assert_eq!(
        crate::engine_config::tuning_for(&engine, EngineAgentKind::Worker).effort,
        Some(ReasoningEffort::Xhigh),
        "and must not disturb the worker's"
    );
    let spec = crate::engine_config::model_spec_for(&engine, EngineAgentKind::Research, &|id| {
        id == "openrouter"
    })
    .expect("the flat research key resolves");
    assert_eq!(spec.model, "z-ai/glm-5.2");
}
