//! The trusted-launcher engine seam: `STELLA_ENGINE_CONFIG_JSON`.
//!
//! A sibling module rather than more lines in [the parent](super) — these
//! tests grew past what `config/tests.rs` could hold under the 1500-line
//! ratchet, and they are one subject: what a benchmark harness may freeze into
//! a run, and what the seam refuses.
//!
//! The seam fails **closed** on any key outside its vocabulary
//! (`trusted_engine_config_shape_is_strict`), which is why the posture's role
//! vocabulary is tested here at all: a key the seam has never heard of does
//! not degrade to an ignored key, it refuses the run.

use super::*;

#[test]
fn trusted_engine_json_replaces_adversarial_project_engine_settings() {
    use stella_protocol::ReasoningEffort;

    let settings = settings_from(
        r#"{
            "providers": {"openrouter": {"api_key": "sk-or-test"}},
            "agent_engine_config": {
                "default_model": "anthropic/task-chosen-model",
                "auto_mode": "on",
                "effort_auto": "on",
                "reasoning_auto": "on",
                "agents": {
                    "default": {"provider": "anthropic", "model": "task-default"}
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
            "default":{"effort":"high","reasoning":"on"}
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
    assert!(!engine.auto_mode_on());
    assert!(!engine.effort_auto_on());
    assert!(!engine.reasoning_auto_on());

    let spec = crate::engine_config::model_spec_for(&engine, &|id| id == "openrouter")
        .expect("the role runs the trusted default model");
    assert_eq!(spec.provider, "openrouter");
    assert_eq!(spec.model, "deepseek/deepseek-v4-pro");

    let tuning = crate::engine_config::tuning_for(&engine);
    assert_eq!(tuning.effort, Some(ReasoningEffort::High));
    assert_eq!(tuning.reasoning, Some(true));

    let agent = engine.agent().expect("the named tuning row is pinned");
    assert!(agent.model.is_none());
    assert!(agent.provider.is_none());
    assert!(agent.prompt.is_none());
    assert!(agent.params.is_none());
}

/// **Witness (#3908), trusted-seam half.** A posture carrying every key the
/// role collapse retired is **recognized, ignored, and non-fatal** — it still
/// launches, and it changes nothing.
///
/// This is the half that is easy to get wrong in the expensive direction.
/// `ENGINE_ROOT_FIELDS` is shared with `trusted_engine_config_shape_is_strict`,
/// which fails CLOSED, and `bench/harbor_adapter/stella_harbor/posture.py` and
/// `arenabench/arenabench/harbor_agent.py` still WRITE these keys into hashed
/// benchmark postures. Had #3908 simply dropped them from the allowlist — the
/// shape `pipeline_max_revisions` took — every benchmark launch would be
/// refused at start and every digest registered in `bench/READINESS.md` §8.4
/// would need re-hashing, which is the published-numbers decision #3870
/// reserves for a maintainer.
///
/// So the keys are recognized here and reported everywhere a human reads
/// settings (`settings::unknown`). Tightening this door to a refusal belongs
/// with slice 6 (#3910), once the Python stops writing them.
#[test]
fn a_posture_naming_every_retired_role_key_still_launches_and_changes_nothing() {
    let settings = settings_from(r#"{"providers": {"openrouter": {"api_key": "sk-or-test"}}}"#);
    // Every retired flat key AND every retired persona block, together.
    let trusted = r#"{
        "default_model":"openrouter/deepseek/deepseek-v4-pro",
        "pipeline_worker_model":"openrouter/z-ai/glm-5.2",
        "pipeline_verifier_model":"openrouter/anthropic/claude-opus-5",
        "pipeline_triage_model":"openrouter/z-ai/glm-5.2",
        "pipeline_research_model":"openrouter/z-ai/glm-5.2",
        "pipeline_plan_model":"openrouter/z-ai/glm-5.2",
        "agents":{
            "default":{"effort":"high"},
            "worker":{"effort":"xhigh","reasoning":"on"},
            "verifier":{"effort":"xhigh","reasoning":"on"},
            "triage":{"effort":"low","reasoning":"off"},
            "research":{"effort":"low"},
            "plan":{"effort":"medium"}
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
    .expect("a posture naming retired keys is RECOGNIZED, not refused");
    unsafe { std::env::remove_var(TRUSTED_ENGINE_CONFIG_ENV) };

    let engine = cfg
        .engine_settings
        .expect("trusted engine config is stamped");

    // Recognized — and then ignored. The session runs the one model the one
    // role names; none of the five retired pins reaches the wire.
    let spec = crate::engine_config::model_spec_for(&engine, &|id| id == "openrouter")
        .expect("the role resolves");
    assert_eq!(
        spec.model, "deepseek/deepseek-v4-pro",
        "a retired pin must not decide what runs"
    );
    // `agents.default` is the one persona block that still parses into
    // anything; the other five parsed into nothing at all.
    assert_eq!(
        crate::engine_config::tuning_for(&engine).effort,
        Some(stella_protocol::ReasoningEffort::High),
        "the surviving row still applies"
    );

    // And the strict shape check answers the same way on its own, so this is a
    // property of the seam rather than an accident of this one launch.
    assert!(
        super::super::trusted_engine_config_shape_is_strict(
            &serde_json::from_str(trusted).unwrap()
        ),
        "the retired keys are inside the seam's vocabulary"
    );
}

/// The other direction, so the test above cannot be read as "the seam accepts
/// anything now": a key that was removed outright — rather than retired — is
/// still refused. `pipeline_max_revisions` bought *attempts*, so an arm naming
/// it is being charged for something it does not receive, and a refusal is the
/// honest answer (#1211 §6.7).
#[test]
fn a_removed_knob_is_still_refused_while_a_retired_role_key_is_not() {
    let removed = serde_json::json!({
        "default_model": "openrouter/z-ai/glm-5.2",
        "pipeline_max_revisions": 4,
    });
    assert!(
        !super::super::trusted_engine_config_shape_is_strict(&removed),
        "a removed knob must still fail closed"
    );

    let retired = serde_json::json!({
        "default_model": "openrouter/z-ai/glm-5.2",
        "pipeline_verifier_model": "openrouter/anthropic/claude-opus-5",
    });
    assert!(
        super::super::trusted_engine_config_shape_is_strict(&retired),
        "a retired role key is recognized so benchmark launches keep working"
    );
}

/// The same seam, for the posture that turns the authored witness back on.
///
/// The witness arm (#1007) pinned a second model for the verifier role, which
/// is what the authored-witness tier resolved its independent author from. It
/// reaches the CLI through this same fail-closed seam, so the field it uses
/// must be part of the seam's vocabulary — a key the seam does not know would
/// not mislabel the run, it would refuse it.
///
/// **#3908 inverted the second half of this test, and the split is the point.**
/// `pipeline_verifier_model` is still *recognized* by the seam, so this
/// posture still launches and every registered digest still hashes the same
/// bytes. It is no longer *read* into `AgentEngineConfig`, because the role it
/// pinned does not exist — the staged pipeline that resolved an independent
/// witness author went in #3865, and the key had been resolving to nothing
/// ever since. The assertion below therefore states the current truth rather
/// than the old one: the arm launches, and the second model it names is not
/// bought. An arm that needs a second model again gets it as a plugin-declared
/// seat (`[seats]`), which is what slice 6 (#3910) repoints the harness onto.
#[test]
fn the_benchmark_witness_arm_posture_survives_the_trusted_launcher_seam() {
    let posture = serde_json::json!({
        "default_model": "openrouter/z-ai/glm-5.1",
        "pipeline_verifier_model": "openrouter/deepseek/deepseek-v4-pro",
        "allowed_models": [
            "openrouter/z-ai/glm-5.1",
            "openrouter/deepseek/deepseek-v4-pro",
        ],
        "auto_mode": "off",
        "effort_auto": "off",
        "reasoning_auto": "off",
        "headless_scope_bypass": "on",
        "agents": {
            "default": {"effort": "high", "reasoning": "on"},
            "worker": {"effort": "high", "reasoning": "on"},
            "verifier": {"effort": "high", "reasoning": "on"},
            "triage": {"effort": "low", "reasoning": "off"},
        },
    });
    assert!(
        super::super::trusted_engine_config_shape_is_strict(&posture),
        "the witness-arm posture must pass the strict seam"
    );
    let parsed: crate::settings::AgentEngineConfig =
        serde_json::from_value(posture).expect("and deserialize into the settings type");
    assert_eq!(
        parsed.default_model.as_deref(),
        Some("openrouter/z-ai/glm-5.1"),
        "the arm's own model must survive the round trip"
    );
    assert!(parsed.headless_scope_bypass_on());
    // Recognized above, and read into nothing here — the two halves of a
    // retirement. `seat_models` is where a second model lives now, and this
    // posture names none.
    assert!(parsed.seat_models.is_none());
}

/// The same seam again, for the attempt-count arms (#1211 §6.7, §6.8) — now
/// inverted. `pipeline_max_revisions` and `pipeline_candidates` were the two
/// knobs that decided how many times a task may be attempted; the staged
/// pipeline that read them is gone (#3865), and an adversarial audit found
/// zero remaining behavioral consumers on `AgentEngineConfig`. They were
/// removed from `settings::ENGINE_ROOT_FIELDS` rather than left as a no-op, so
/// the trusted-launcher seam — which shares that exact allowlist — must now
/// FAIL CLOSED on a posture naming either: a benchmark arm that thinks it is
/// still buying revisions or candidates must be refused at launch, not
/// silently charged for nothing.
#[test]
fn the_dead_attempt_count_posture_is_refused_by_the_trusted_launcher_seam() {
    let posture = serde_json::json!({
        "default_model": "openrouter/z-ai/glm-5.2",
        "allowed_models": ["openrouter/z-ai/glm-5.2"],
        "auto_mode": "off",
        "effort_auto": "off",
        "reasoning_auto": "off",
        "headless_scope_bypass": "on",
        "pipeline_max_revisions": 4,
        "pipeline_candidates": 2,
        "agents": {
            "default": {"effort": "xhigh", "reasoning": "on"},
            "worker": {"effort": "xhigh", "reasoning": "on"},
            "verifier": {"effort": "xhigh", "reasoning": "on"},
            "triage": {"effort": "low", "reasoning": "off"},
        },
    });
    assert!(
        !super::super::trusted_engine_config_shape_is_strict(&posture),
        "a posture naming a removed pipeline knob must be refused, not silently accepted"
    );
}
