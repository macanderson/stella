use super::*;
// Straight from the table's own module rather than a `config` re-export: it
// is read only here, and a `pub(crate) use` for a test-only name is an
// unused import in every non-test build.
use super::providers::COMMON_KEY_ENV_VARS;

/// Every provider's `default_model` here must resolve against
/// `stella_model::catalog::Catalog::seed()`, or `build_provider`'s
/// catalog check (`agent.rs`) would hard-error on first use of a
/// provider whose default was never added to the seed. Uses the
/// provider-scoped resolver, same as `build_provider`, so a default that
/// only exists under a *different* provider's row still fails here.
#[test]
fn every_provider_default_model_resolves_against_the_catalog_seed() {
    let catalog = stella_model::catalog::Catalog::seed();
    for provider in PROVIDERS {
        catalog
            .resolve_for(provider.id, provider.default_model)
            .unwrap_or_else(|e| {
                panic!(
                    "provider `{}`'s default_model `{}` is not in the catalog seed: {e}",
                    provider.id, provider.default_model
                )
            });
    }
}

#[test]
fn provider_ids_are_unique() {
    let mut ids: Vec<&str> = PROVIDERS.iter().map(|p| p.id).collect();
    ids.push(LOCAL_PROVIDER.id);
    let before = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), before, "duplicate provider id in PROVIDERS");
}

/// Every seeded provider must declare its prompt-cache posture in
/// stella-model's parity matrix — the guard born from OpenRouter
/// silently running Claude with zero caching. A new provider cannot
/// land without stating how caching is engaged and naming the witness
/// test that proves it.
#[test]
fn every_seeded_provider_declares_a_cache_posture() {
    for provider in PROVIDERS.iter().chain(std::iter::once(&LOCAL_PROVIDER)) {
        assert!(
            stella_model::provider_parity::cache_posture(provider.id).is_some(),
            "provider `{}` has no row in stella-model/src/provider_parity.rs — \
             add its CachePosture (with a witness test) in this PR",
            provider.id
        );
    }
}

/// The reasoning-axis sibling of the cache-posture guard: every seeded
/// provider must declare how its reasoning/thinking budget is controlled (or
/// that the shared adapter deliberately drops it). Born from the same silent
/// per-provider divergence — a pinned effort reaching only Z.ai and OpenRouter
/// and being dropped everywhere else with nothing enforcing the omission stays
/// deliberate. A new provider cannot land without stating its reasoning
/// posture and naming the witness that proves a `Controllable` control on the
/// wire.
#[test]
fn every_seeded_provider_declares_a_reasoning_posture() {
    for provider in PROVIDERS.iter().chain(std::iter::once(&LOCAL_PROVIDER)) {
        assert!(
            stella_model::provider_parity::reasoning_posture(provider.id).is_some(),
            "provider `{}` has no ReasoningPosture row in \
             stella-model/src/provider_parity.rs — add it (with a witness test for a \
             Controllable control, or a note for a no-control posture) in this PR",
            provider.id
        );
    }
}

#[test]
fn alias_env_var_resolves_when_the_primary_is_unset() {
    // Synthetic provider with unique var names so parallel tests can't
    // race on shared env state (the convention credential.rs's own
    // tests follow).
    let provider = ProviderConfig {
        id: "alias-test",
        env_var: "STELLA_TEST_ALIAS_PRIMARY_KEY",
        env_var_aliases: &["STELLA_TEST_ALIAS_SECONDARY_KEY"],
        display_name: "Alias Test",
        default_model: "m",
        base_url: "",
        dialect: Dialect::OpenaiCompatible,
        seeded: false,
    };
    // SAFETY: test-only env mutation, unique var names per test — and
    // serialized behind the binary-wide env lock, because setenv racing
    // any concurrent getenv is UB on POSIX regardless of var names.
    let _env = crate::test_env::lock();
    unsafe {
        std::env::remove_var("STELLA_TEST_ALIAS_PRIMARY_KEY");
        std::env::set_var("STELLA_TEST_ALIAS_SECONDARY_KEY", "sk-from-alias");
    }
    let file = CredentialsFile::load(std::env::temp_dir().join(format!(
        "stella-test-alias-credentials-{}.toml",
        std::process::id()
    )))
    .unwrap();

    let (key, source) = resolve_provider_key(&provider, None, None, &file, false).unwrap();
    assert_eq!(key.reveal(), "sk-from-alias");
    assert_eq!(source, stella_model::credential::CredentialSource::EnvVar);

    unsafe {
        std::env::remove_var("STELLA_TEST_ALIAS_SECONDARY_KEY");
    }
}

/// A set-but-empty alias env var must hard-error even when a lower-precedence
/// source (here the credentials file) resolves — the same posture
/// `ApiKey::resolve` takes for the primary var, which errors before it ever
/// consults the file. This path used to silently return the file hit, so the
/// two resolution paths disagreed about the same user mistake.
#[test]
fn empty_alias_env_var_errors_even_when_the_credentials_file_resolves() {
    let provider = ProviderConfig {
        id: "alias-empty-file-test",
        env_var: "STELLA_TEST_ALIAS_EMPTY_FILE_PRIMARY",
        env_var_aliases: &["STELLA_TEST_ALIAS_EMPTY_FILE_SECONDARY"],
        display_name: "Alias Empty Test",
        default_model: "m",
        base_url: "",
        dialect: Dialect::OpenaiCompatible,
        seeded: false,
    };
    // SAFETY: test-only env mutation, unique var names per test — and
    // serialized behind the binary-wide env lock, because setenv racing
    // any concurrent getenv is UB on POSIX regardless of var names.
    let _env = crate::test_env::lock();
    unsafe {
        std::env::remove_var("STELLA_TEST_ALIAS_EMPTY_FILE_PRIMARY");
        std::env::set_var("STELLA_TEST_ALIAS_EMPTY_FILE_SECONDARY", "");
    }
    let mut file = CredentialsFile::load(std::env::temp_dir().join(format!(
        "stella-test-alias-empty-file-credentials-{}.toml",
        std::process::id()
    )))
    .unwrap();
    file.set("alias-empty-file-test", "sk-from-file");

    let err = resolve_provider_key(&provider, None, None, &file, false).unwrap_err();
    assert!(
        matches!(
            &err,
            stella_model::credential::CredentialError::Empty { env_var }
                if env_var == "STELLA_TEST_ALIAS_EMPTY_FILE_SECONDARY"
        ),
        "expected Empty for the alias, got: {err:?}"
    );

    unsafe {
        std::env::remove_var("STELLA_TEST_ALIAS_EMPTY_FILE_SECONDARY");
    }
}

/// The nothing-found path's posture, pinned so the two paths cannot drift
/// apart again: with no other source at all, a set-but-empty alias is the
/// same hard error.
#[test]
fn empty_alias_env_var_errors_when_nothing_else_resolves() {
    let provider = ProviderConfig {
        id: "alias-empty-bare-test",
        env_var: "STELLA_TEST_ALIAS_EMPTY_BARE_PRIMARY",
        env_var_aliases: &["STELLA_TEST_ALIAS_EMPTY_BARE_SECONDARY"],
        display_name: "Alias Empty Test",
        default_model: "m",
        base_url: "",
        dialect: Dialect::OpenaiCompatible,
        seeded: false,
    };
    // SAFETY: test-only env mutation, unique var names per test — and
    // serialized behind the binary-wide env lock, because setenv racing
    // any concurrent getenv is UB on POSIX regardless of var names.
    let _env = crate::test_env::lock();
    unsafe {
        std::env::remove_var("STELLA_TEST_ALIAS_EMPTY_BARE_PRIMARY");
        std::env::set_var("STELLA_TEST_ALIAS_EMPTY_BARE_SECONDARY", "");
    }
    let file = CredentialsFile::load(std::env::temp_dir().join(format!(
        "stella-test-alias-empty-bare-credentials-{}.toml",
        std::process::id()
    )))
    .unwrap();

    let err = resolve_provider_key(&provider, None, None, &file, false).unwrap_err();
    assert!(
        matches!(
            &err,
            stella_model::credential::CredentialError::Empty { env_var }
                if env_var == "STELLA_TEST_ALIAS_EMPTY_BARE_SECONDARY"
        ),
        "expected Empty for the alias, got: {err:?}"
    );

    unsafe {
        std::env::remove_var("STELLA_TEST_ALIAS_EMPTY_BARE_SECONDARY");
    }
}

#[test]
fn config_debug_never_leaks_the_api_key() {
    // H3: with `api_key: ApiKey`, the whole Config's derived Debug must
    // redact the secret — no `{:?}` (logs, panics, traces) can leak it.
    let secret = "sk-super-secret-do-not-log-XYZ";
    let cfg = Config {
        provider: PROVIDERS[0].clone(),
        model_id: "glm-5.2".to_string(),
        turn_budget: None,
        model_pinned_by_flag: false,
        durability: Default::default(),
        create_worktrees: Default::default(),
        api_key: ApiKey::new(secret),
        workspace_root: std::path::PathBuf::from("/tmp/ws"),
        base_url_override: None,
        hooks: None,
        engine_settings: None,
        engine_settings_trusted: false,
        tool_policy: Default::default(),
        enable_recap: false,
        trace_capture: false,
        reward_policy: Default::default(),
        authority: crate::settings::AuthorityPolicy::default(),
        credential_source: Some(stella_model::credential::CredentialSource::EnvVar),
        credential_advisories: Vec::new(),
    };
    let dbg = format!("{cfg:?}");
    assert!(!dbg.contains(secret), "Config Debug leaked the key: {dbg}");
    assert!(dbg.contains("redacted"));
}

#[test]
fn resolved_config_carries_the_authority_computed_during_settings_load() {
    // `load_with_settings` reads the process-wide trusted-engine-config env
    // var; hold the binary env lock so a concurrent test setting it to a
    // malformed value can't make this load fail (setenv races any getenv).
    let _env = crate::test_env::lock();
    let authority = crate::settings::AuthorityPolicy {
        project_prompts_allowed: true,
        project_custom_tools_allowed: false,
        bash_allowed: false,
        web_allowed: true,
        media_requires_host_approval: true,
    };
    let mut settings = crate::settings::Settings::default();
    settings.authority_policy = authority;

    let cfg = Config::load_with_settings(
        Some("local/test-model"),
        None,
        Some("http://localhost:11434/v1"),
        &settings,
        std::path::PathBuf::from("/tmp/ws"),
    )
    .unwrap();

    assert_eq!(cfg.authority, authority);
}

/// Helper: a Settings value parsed from JSON, as the scope-merge would
/// produce it — the seam for exercising resolution without touching
/// `$HOME`, `/etc`, or a real workspace.
fn settings_from(json: &str) -> crate::settings::Settings {
    serde_json::from_str(json).expect("test settings JSON must parse")
}

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
                    "judge": {"provider": "openai", "model": "task-judge"},
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
            "judge":{"effort":"high","reasoning":"on"},
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
    assert!(engine.pipeline_judge_model.is_none());
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
        if kind == EngineAgentKind::Triage {
            assert_eq!(tuning.effort, Some(ReasoningEffort::Low));
            assert_eq!(tuning.reasoning, Some(false));
        } else {
            assert_eq!(tuning.effort, Some(ReasoningEffort::High));
            assert_eq!(tuning.reasoning, Some(true));
        }
        let agent = engine.agent(kind).expect("all four tuning rows are pinned");
        assert!(agent.model.is_none());
        assert!(agent.provider.is_none());
        assert!(agent.prompt.is_none());
        assert!(agent.params.is_none());
    }
}

#[test]
fn benchmark_mode_skips_malformed_filesystem_credentials_but_keeps_engine_override() {
    let _env = crate::test_env::lock();
    let _restore = crate::test_env::EnvRestore::capture(&["HOME", TRUSTED_ENGINE_CONFIG_ENV]);
    let dir = tempfile::tempdir().unwrap();
    let credential_dir = dir.path().join(".stella");
    std::fs::create_dir_all(&credential_dir).unwrap();
    std::fs::write(
        credential_dir.join("credentials.toml"),
        "this is deliberately [not valid TOML",
    )
    .unwrap();
    let trusted = r#"{
        "default_model":"openrouter/deepseek/deepseek-v4-pro",
        "allowed_models":["openrouter/deepseek/deepseek-v4-pro"],
        "auto_mode":"off",
        "effort_auto":"off",
        "reasoning_auto":"off"
    }"#;

    // SAFETY: the binary-wide environment lock covers mutation, resolution,
    // and restoration. STELLA_NO_SETTINGS is the adapter-pinned benchmark
    // isolation mode; the trusted engine JSON remains a later explicit seam.
    unsafe {
        std::env::set_var("HOME", dir.path());
        std::env::set_var(TRUSTED_ENGINE_CONFIG_ENV, trusted);
    }
    let _isolation = crate::settings::test_filesystem_isolation(true);
    let cfg = Config::load_with_settings(
        Some("openrouter/deepseek/deepseek-v4-pro"),
        Some("test-key-from-trusted-handoff-seam"),
        Some("https://openrouter.ai/api/v1"),
        &crate::settings::Settings::default(),
        dir.path().join("workspace"),
    )
    .expect("benchmark mode must never parse the hostile credential file");

    assert_eq!(cfg.provider.id, "openrouter");
    assert_eq!(
        cfg.engine_settings
            .and_then(|settings| settings.default_model),
        Some("openrouter/deepseek/deepseek-v4-pro".to_string())
    );
}

#[test]
fn model_pinned_by_flag_records_whether_the_flag_or_settings_answered() {
    // `load_with_settings` folds `--model` and the settings-configured
    // default into one `Option<&str>` before resolution. That fold is the
    // last point where the two are distinguishable, and the pipeline wiring
    // needs the distinction to stop `pipeline_worker_model` from overriding
    // an explicit flag. If this provenance is ever dropped, the wiring guard
    // silently becomes unreachable and the flag stops working again.
    let settings = settings_from(
        r#"{
            "providers": {"openrouter": {"api_key": "sk-or-test"}},
            "agent_engine_config": {"default_model": "openrouter/from-settings"}
        }"#,
    );

    let _env = crate::test_env::lock();

    let flagged = Config::load_with_settings(
        Some("openrouter/from-the-flag"),
        None,
        None,
        &settings,
        std::path::PathBuf::from("/tmp/ws"),
    )
    .expect("an explicit --model resolves");
    assert_eq!(flagged.model_id, "from-the-flag");
    assert!(
        flagged.model_pinned_by_flag,
        "an explicit --model must be recorded as a flag pin"
    );

    let unflagged = Config::load_with_settings(
        None,
        None,
        None,
        &settings,
        std::path::PathBuf::from("/tmp/ws"),
    )
    .expect("the settings default resolves");
    assert_eq!(unflagged.model_id, "from-settings");
    assert!(
        !unflagged.model_pinned_by_flag,
        "a settings-provided default is NOT a flag pin — it must stay \
         overridable by pipeline_worker_model (#276)"
    );
}

/// The reward policy reaches the `Config` every consumer reads, resolved once
/// here rather than re-derived at each call site (#1043).
#[test]
fn a_configured_reward_policy_reaches_the_config() {
    let _env = crate::test_env::lock();
    // `Settings` has private fields, so functional-update syntax is out —
    // mutate the one field instead of restating the struct.
    let mut settings = crate::settings::Settings::default();
    settings.reward = Some(crate::settings::RewardSettings {
        judge_weight: Some(0.2),
        ..Default::default()
    });
    let cfg = Config::load_with_settings(
        Some("openrouter/deepseek/deepseek-v4-pro"),
        Some("test-key"),
        Some("https://openrouter.ai/api/v1"),
        &settings,
        std::path::PathBuf::from("/tmp/ws"),
    )
    .expect("a legal weight loads");
    assert_eq!(cfg.reward_policy.outcome.judged, 0.2);
    assert_eq!(
        cfg.reward_policy.outcome.deterministic, 1.0,
        "an unset weight stays at its default"
    );
}

/// A judge weight that outranks a test fails the LAUNCH, by name — it does not
/// get clamped to something legal and then quietly applied.
///
/// This is the whole reason `reward_policy()` returns a `Result`. A substituted
/// weight would produce correctly-shaped labels for every turn thereafter,
/// meaning something the operator never asked for, and nothing downstream could
/// tell that had happened.
#[test]
fn an_impossible_reward_weight_fails_the_launch_instead_of_being_clamped() {
    let _env = crate::test_env::lock();
    // `Settings` has private fields, so functional-update syntax is out —
    // mutate the one field instead of restating the struct.
    let mut settings = crate::settings::Settings::default();
    settings.reward = Some(crate::settings::RewardSettings {
        judge_weight: Some(3.0),
        ..Default::default()
    });
    let error = Config::load_with_settings(
        Some("openrouter/deepseek/deepseek-v4-pro"),
        Some("test-key"),
        Some("https://openrouter.ai/api/v1"),
        &settings,
        std::path::PathBuf::from("/tmp/ws"),
    )
    .expect_err("a judge outranking a test must not launch");
    assert!(error.contains("judge_weight"), "{error}");
    assert!(error.contains("deterministic_weight"), "{error}");
}

#[test]
fn malformed_trusted_engine_json_fails_closed_without_echoing_value() {
    let secret_marker = "DO-NOT-ECHO-BENCHMARK-CONTENT";
    let malformed = format!(
        r#"{{"default_model":"{secret_marker}","agents":{{"worker":{{"efort":"high"}}}}}}"#
    );
    let _env = crate::test_env::lock();
    // SAFETY: test-only process environment mutation serialized by test_env.
    unsafe { std::env::set_var(TRUSTED_ENGINE_CONFIG_ENV, &malformed) };
    let error = Config::load_with_settings(
        Some("openrouter/deepseek/deepseek-v4-pro"),
        Some("test-key"),
        Some("https://openrouter.ai/api/v1"),
        &crate::settings::Settings::default(),
        std::path::PathBuf::from("/tmp/ws"),
    )
    .unwrap_err();
    unsafe { std::env::remove_var(TRUSTED_ENGINE_CONFIG_ENV) };

    assert!(error.contains(TRUSTED_ENGINE_CONFIG_ENV));
    assert!(!error.contains(secret_marker));
    assert!(!error.contains("efort"));
}

#[test]
fn a_settings_defined_provider_resolves_via_model_flag_with_its_literal_key() {
    let _env = crate::test_env::lock();
    // The issue #44 acceptance criterion: a provider that is NOT
    // built-in, added purely via settings.json, usable via
    // --model <id>/<model> with no code change.
    let settings = settings_from(
        r#"{"providers": {"together": {
            "name": "Together AI",
            "base_url": "https://api.together.xyz/v1",
            "api_key": "sk-together-test",
            "default_model": "meta-llama/Llama-3.3-70B-Instruct-Turbo"
        }}}"#,
    );
    let cfg = Config::load_with_settings(
        Some("together/meta-llama/Llama-3.3-70B-Instruct-Turbo"),
        None,
        None,
        &settings,
        std::path::PathBuf::from("/tmp/ws"),
    )
    .expect("config-defined provider should resolve");
    assert_eq!(cfg.provider.id, "together");
    assert_eq!(cfg.provider.display_name, "Together AI");
    assert_eq!(cfg.model_id, "meta-llama/Llama-3.3-70B-Instruct-Turbo");
    assert_eq!(cfg.effective_base_url(), "https://api.together.xyz/v1");
    assert_eq!(cfg.api_key.reveal(), "sk-together-test");
    assert_eq!(cfg.provider.dialect, Dialect::OpenaiCompatible);
    assert!(
        !cfg.provider.seeded,
        "config-defined providers must bypass the catalog check"
    );
}

#[test]
fn a_custom_provider_without_base_url_is_a_named_error() {
    let _env = crate::test_env::lock();
    let settings = settings_from(r#"{"providers": {"fireworks": {"api_key": "sk-x"}}}"#);
    let err = Config::load_with_settings(
        Some("fireworks/some-model"),
        None,
        None,
        &settings,
        std::path::PathBuf::from("/tmp/ws"),
    )
    .unwrap_err();
    assert!(err.contains("fireworks"), "{err}");
    assert!(err.contains("base_url"), "{err}");
}

#[test]
fn custom_providers_cannot_claim_the_vertex_or_bedrock_dialects() {
    let _env = crate::test_env::lock();
    let settings = settings_from(
        r#"{"providers": {"myvertex": {
            "base_url": "https://example.com",
            "dialect": "vertex"
        }}}"#,
    );
    let err = Config::load_with_settings(
        Some("myvertex/some-model"),
        None,
        None,
        &settings,
        std::path::PathBuf::from("/tmp/ws"),
    )
    .unwrap_err();
    assert!(err.contains("reserved for the built-in provider"), "{err}");
}

#[test]
fn a_settings_override_reshapes_a_builtin_without_changing_its_dialect() {
    let _env = crate::test_env::lock();
    // The pre-#44 override use case (e.g. the Z.ai coding plan): move a
    // built-in's base URL and key out of provider-specific env vars.
    let settings = settings_from(
        r#"{"providers": {"zai": {
            "name": "ZAI Provider",
            "base_url": "https://api.z.ai/api/coding/paas/v4"
        }}}"#,
    );
    // Key via --api-key (outranks everything) so this test can't be
    // perturbed by an ambient ZAI_API_KEY on the host.
    let cfg = Config::load_with_settings(
        Some("zai/glm-5.2"),
        Some("sk-cli-flag"),
        None,
        &settings,
        std::path::PathBuf::from("/tmp/ws"),
    )
    .expect("built-in override should resolve");
    assert_eq!(cfg.provider.id, "zai");
    assert_eq!(cfg.provider.display_name, "ZAI Provider");
    assert_eq!(
        cfg.effective_base_url(),
        "https://api.z.ai/api/coding/paas/v4"
    );
    assert_eq!(cfg.api_key.reveal(), "sk-cli-flag");
    assert_eq!(cfg.provider.dialect, Dialect::OpenaiCompatible);
    assert!(
        cfg.provider.seeded,
        "built-in overrides keep the catalog check"
    );
}

#[test]
fn a_stale_default_pin_does_not_mangle_a_qualified_engine_default_model() {
    let _env = crate::test_env::lock();
    // Regression: `agents.default.provider: "zai"` alongside the flat
    // `default_model: "openrouter/openrouter/auto"` (a provider-qualified
    // spec, the shape every TUI save writes) used to stitch the phantom
    // slug `zai/openrouter/openrouter/auto` and die on the catalog
    // check. The qualified spec's own provider must win over the stale
    // seeded pin.
    let settings = settings_from(
        r#"{
            "providers": {"openrouter": {"api_key": "sk-or-test"}},
            "agent_engine_config": {
                "default_model": "openrouter/openrouter/auto",
                "agents": {"default": {"provider": "zai"}}
            }
        }"#,
    );
    let cfg = Config::load_with_settings(
        None,
        None,
        None,
        &settings,
        std::path::PathBuf::from("/tmp/ws"),
    )
    .expect("the qualified engine default must resolve");
    assert_eq!(cfg.provider.id, "openrouter");
    assert_eq!(cfg.model_id, "openrouter/auto");
}

#[test]
fn an_openrouter_pin_over_the_tui_qualified_default_does_not_double_the_wire_slug() {
    let _env = crate::test_env::lock();
    // Regression: the pin naming the qualified spec's OWN provider —
    // `agents.default.provider: "openrouter"` plus the TUI-written
    // `default_model: "openrouter/openrouter/auto"`. OpenRouter is
    // unseeded, so the catalog arbitration that saves a stale seeded
    // pin never ran, verbatim routing kept the doubled slug, and every
    // call died on `openrouter/openrouter/auto is not a valid model ID`
    // (HTTP 400). The wire slug must come out de-qualified.
    let settings = settings_from(
        r#"{
            "providers": {"openrouter": {"api_key": "sk-or-test"}},
            "agent_engine_config": {
                "default_model": "openrouter/openrouter/auto",
                "agents": {"default": {"provider": "openrouter"}}
            }
        }"#,
    );
    let cfg = Config::load_with_settings(
        None,
        None,
        None,
        &settings,
        std::path::PathBuf::from("/tmp/ws"),
    )
    .expect("the pinned qualified default must resolve");
    assert_eq!(cfg.provider.id, "openrouter");
    assert_eq!(cfg.model_id, "openrouter/auto");
}

#[test]
fn env_var_outranks_the_settings_literal_key() {
    // Chain order: env var above settings.json api_key. Unique var name
    // so parallel tests can't race on shared env state.
    let settings = settings_from(
        r#"{"providers": {"envrank": {
            "base_url": "https://envrank.example/v1",
            "api_key": "sk-from-settings",
            "api_key_env": "STELLA_TEST_ENVRANK_KEY",
            "default_model": "m1"
        }}}"#,
    );
    // SAFETY: test-only env mutation, unique var name per test — and
    // serialized behind the binary-wide env lock (setenv racing any
    // concurrent getenv is UB on POSIX regardless of var names).
    let _env = crate::test_env::lock();
    unsafe {
        std::env::set_var("STELLA_TEST_ENVRANK_KEY", "sk-from-env");
    }
    let cfg = Config::load_with_settings(
        Some("envrank/m1"),
        None,
        None,
        &settings,
        std::path::PathBuf::from("/tmp/ws"),
    )
    .unwrap();
    assert_eq!(cfg.api_key.reveal(), "sk-from-env");
    assert!(
        stella_tools::subprocess_env::is_sensitive_env_name(std::ffi::OsStr::new(
            "STELLA_TEST_ENVRANK_KEY"
        )),
        "trusted custom api_key_env must be registered for child-process scrubbing"
    );
    unsafe {
        std::env::remove_var("STELLA_TEST_ENVRANK_KEY");
    }
}

#[test]
fn a_bare_slug_matches_a_custom_providers_default_model() {
    let _env = crate::test_env::lock();
    let settings = settings_from(
        r#"{"providers": {"slugmatch": {
            "base_url": "https://slugmatch.example/v1",
            "api_key": "sk-slug",
            "default_model": "totally-custom-slug"
        }}}"#,
    );
    let cfg = Config::load_with_settings(
        Some("totally-custom-slug"),
        None,
        None,
        &settings,
        std::path::PathBuf::from("/tmp/ws"),
    )
    .unwrap();
    assert_eq!(cfg.provider.id, "slugmatch");
    assert_eq!(cfg.model_id, "totally-custom-slug");
}

/// A settings.json override of a BUILT-IN provider's `default_model` is what
/// auto-detection launches with and what `stella models` prints as that
/// provider's default — but the bare-slug lookup only ever consulted the
/// hard-coded row, so the one slug the UI told you to use came back
/// "model `…` not recognized". Both spellings must resolve.
#[test]
fn a_bare_slug_matches_a_builtin_providers_overridden_default_model() {
    let _env = crate::test_env::lock();
    let settings = settings_from(
        r#"{"providers": {"anthropic": {
            "api_key": "sk-overridden",
            "default_model": "claude-house-blend"
        }}}"#,
    );
    let cfg = Config::load_with_settings(
        Some("claude-house-blend"),
        None,
        None,
        &settings,
        std::path::PathBuf::from("/tmp/ws"),
    )
    .expect("the configured default model must be selectable by its bare slug");
    assert_eq!(cfg.provider.id, "anthropic");
    assert_eq!(cfg.model_id, "claude-house-blend");

    // And the shipped default keeps working: an override must not retire a
    // name a script or muscle memory already has.
    let cfg = Config::load_with_settings(
        Some("claude-fable-5"),
        None,
        None,
        &settings,
        std::path::PathBuf::from("/tmp/ws"),
    )
    .expect("the shipped default must keep resolving after an override");
    assert_eq!(cfg.provider.id, "anthropic");
    assert_eq!(cfg.model_id, "claude-fable-5");
}

/// `ApiKey::resolve`'s documented contract: a headless caller must get a clean
/// `NotFound` rather than block on a masked prompt. Nothing honoured it — the
/// `--model provider/slug` path passed `interactive: true` unconditionally, so
/// a `--output-format json` run from an attached terminal with no key stopped
/// dead on a password prompt while its caller waited on stdout for an object
/// that could never arrive.
///
/// The prompt itself cannot be exercised here (it needs a tty), so this pins
/// the policy latch that gates it: once `main` forbids interactive
/// credentials, `resolve` must not be able to reach the prompt step whatever
/// the call site asked for.
#[test]
fn forbidding_interactive_credentials_is_process_wide_and_survives_an_interactive_call_site() {
    let _env = crate::test_env::lock();
    assert!(
        interactive_allowed(),
        "the default posture is interactive — a human at a terminal is the common case"
    );
    forbid_interactive_credentials();
    assert!(!interactive_allowed());

    // The `--model provider/slug` path (the one that asks for `interactive:
    // true`) with no credential anywhere still returns a named error rather
    // than reaching the prompt.
    let settings = settings_from(
        r#"{"providers": {"headlessprobe": {
            "base_url": "https://headlessprobe.example/v1",
            "api_key_env": "STELLA_TEST_HEADLESS_PROBE_KEY"
        }}}"#,
    );
    unsafe {
        std::env::remove_var("STELLA_TEST_HEADLESS_PROBE_KEY");
    }
    let err = Config::load_with_settings(
        Some("headlessprobe/m"),
        None,
        None,
        &settings,
        std::path::PathBuf::from("/tmp/ws"),
    )
    .unwrap_err();
    assert!(err.contains("could not resolve a credential"), "{err}");

    // Restore the default for every other test in this binary.
    INTERACTIVE_CREDENTIALS.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn discovery_style_resolution_accepts_the_settings_literal_key() {
    let _env = crate::test_env::lock();
    // resolve_provider_key with a settings literal and nothing else:
    // resolves non-interactively as SettingsJson — this is what puts
    // config-defined providers into auto-detection and judge discovery.
    let provider = ProviderConfig {
        id: "settings-key-test",
        env_var: "STELLA_TEST_SETTINGS_KEY_UNSET",
        env_var_aliases: &[],
        display_name: "Settings Key Test",
        default_model: "m",
        base_url: "https://x.example/v1",
        dialect: Dialect::OpenaiCompatible,
        seeded: false,
    };
    let file = CredentialsFile::load(std::env::temp_dir().join(format!(
        "stella-test-settings-key-credentials-{}.toml",
        std::process::id()
    )))
    .unwrap();
    let (key, source) =
        resolve_provider_key(&provider, None, Some("sk-settings"), &file, false).unwrap();
    assert_eq!(key.reveal(), "sk-settings");
    assert_eq!(
        source,
        stella_model::credential::CredentialSource::SettingsJson
    );
}

/// Issue #249's source-display requirement: a settings.json literal and a
/// real credentials.toml entry are two DIFFERENT stores, and a caller
/// showing "where did this come from" must be able to tell them apart. A
/// single `CredentialSource::ConfigFile` for both would conflate "declared
/// in settings.json" with "stored in credentials.toml". This constructs a
/// provider with BOTH present (the settings literal must win per the
/// documented precedence) and asserts the resolved source is the
/// settings-specific variant, not the file one — the file's differing value
/// proves which one actually won.
#[test]
fn settings_json_literal_is_reported_distinctly_from_a_real_credentials_toml_entry() {
    let provider = ProviderConfig {
        id: "settings-vs-file-test",
        env_var: "STELLA_TEST_SETTINGS_VS_FILE_UNSET",
        env_var_aliases: &[],
        display_name: "Settings vs File Test",
        default_model: "m",
        base_url: "https://x.example/v1",
        dialect: Dialect::OpenaiCompatible,
        seeded: false,
    };
    let mut file = CredentialsFile::load(std::env::temp_dir().join(format!(
        "stella-test-settings-vs-file-credentials-{}.toml",
        std::process::id()
    )))
    .unwrap();
    file.set("settings-vs-file-test", "sk-from-credentials-file");

    let (key, source) =
        resolve_provider_key(&provider, None, Some("sk-from-settings-json"), &file, false).unwrap();
    assert_eq!(key.reveal(), "sk-from-settings-json");
    assert_eq!(
        source,
        stella_model::credential::CredentialSource::SettingsJson,
        "a settings.json literal must be reported as SettingsJson, distinct from ConfigFile"
    );
}

#[test]
fn derived_env_var_uppercases_and_folds_punctuation() {
    assert_eq!(derived_env_var("together"), "TOGETHER_API_KEY");
    assert_eq!(derived_env_var("my-gateway"), "MY_GATEWAY_API_KEY");
}

#[test]
fn local_provider_requires_base_url_and_defaults_its_key() {
    let _env = crate::test_env::lock();
    let err = Config::load(Some("local/llama3.3"), None, None).unwrap_err();
    assert!(err.contains("--base-url"), "{err}");

    let cfg = Config::load(
        Some("local/llama3.3"),
        None,
        Some("http://localhost:11434/v1"),
    )
    .expect("local provider with --base-url should resolve");
    assert_eq!(cfg.provider.id, "local");
    assert_eq!(cfg.model_id, "llama3.3");
    assert_eq!(cfg.effective_base_url(), "http://localhost:11434/v1");
    // No LOCAL_API_KEY set: the placeholder key is used (local servers
    // generally ignore auth, but the OpenAI-compatible client always
    // sends a bearer token).
    assert_eq!(cfg.api_key.reveal(), "local");
}

/// The trusted seam rejects the WHOLE override on one unknown key — that is
/// its point ("a misspelled benchmark control must fail closed"). The cost is
/// that adding a field to `AgentEngineConfig` without also allowing it here
/// stops the benchmark booting, with a message that deliberately does not
/// echo the value, so the offending key is invisible. That happened once:
/// `headless_scope_bypass` shipped in the struct and the posture, and every
/// trial died at startup with a single line of output.
///
/// So: the exact shape the harbor adapter emits must survive the seam.
#[test]
fn the_benchmark_engine_posture_survives_the_trusted_launcher_seam() {
    let posture = serde_json::json!({
        "default_model": "openrouter/z-ai/glm-5.2",
        "allowed_models": ["openrouter/z-ai/glm-5.2"],
        "auto_mode": "off",
        "effort_auto": "off",
        "reasoning_auto": "off",
        "headless_scope_bypass": "on",
        "agents": {
            "default": {"effort": "high", "reasoning": "on"},
            "worker": {"effort": "high", "reasoning": "on"},
            "judge": {"effort": "high", "reasoning": "on"},
            "triage": {"effort": "low", "reasoning": "off"},
        },
    });
    assert!(
        super::trusted_engine_config_shape_is_strict(&posture),
        "bench/harbor_adapter's posture must pass the strict seam"
    );
    let parsed: crate::settings::AgentEngineConfig =
        serde_json::from_value(posture).expect("and deserialize into the settings type");
    assert!(
        parsed.headless_scope_bypass_on(),
        "the flag must survive the round trip, not just be tolerated"
    );
}

/// The same seam, for the posture that turns the authored witness back on.
///
/// The witness arm (#1007) pins a second model for the judge role, which is
/// what the authored-witness tier resolves its independent author from. It
/// reaches the CLI through this same fail-closed seam, so the field it uses
/// must be part of the seam's vocabulary — and it must arrive as
/// `pipeline_judge_model`, not as a new root key, because a descriptive key
/// would not mislabel the run, it would refuse it.
#[test]
fn the_benchmark_witness_arm_posture_survives_the_trusted_launcher_seam() {
    let posture = serde_json::json!({
        "default_model": "openrouter/z-ai/glm-5.1",
        "pipeline_judge_model": "openrouter/deepseek/deepseek-v4-pro",
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
            "judge": {"effort": "high", "reasoning": "on"},
            "triage": {"effort": "low", "reasoning": "off"},
        },
    });
    assert!(
        super::trusted_engine_config_shape_is_strict(&posture),
        "the witness-arm posture must pass the strict seam"
    );
    let parsed: crate::settings::AgentEngineConfig =
        serde_json::from_value(posture).expect("and deserialize into the settings type");
    assert_eq!(
        parsed.pipeline_judge_model.as_deref(),
        Some("openrouter/deepseek/deepseek-v4-pro"),
        "the judge author must survive the round trip; dropping it here is a \
         benchmark run that reports the witness on and measures it off"
    );
    assert!(parsed.headless_scope_bypass_on());
}

/// The same seam again, for the attempt-count arms (#1211 §6.7, §6.8).
///
/// `pipeline_max_revisions` and `pipeline_candidates` are the two knobs that
/// decide how many times a task may be attempted, and until they joined
/// `settings::ENGINE_ROOT_FIELDS` a posture naming either was not *ignored* — this
/// seam fails closed, so the trial died at launch. Numbers, not strings, and
/// the round trip has to preserve them: a dropped `pipeline_candidates` is a
/// best-of-N arm that costs 2x and measures single-shot.
#[test]
fn the_benchmark_attempt_count_posture_survives_the_trusted_launcher_seam() {
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
            "judge": {"effort": "xhigh", "reasoning": "on"},
            "triage": {"effort": "low", "reasoning": "off"},
        },
    });
    assert!(
        super::trusted_engine_config_shape_is_strict(&posture),
        "the attempt-count posture must pass the strict seam"
    );
    let parsed: crate::settings::AgentEngineConfig =
        serde_json::from_value(posture).expect("and deserialize into the settings type");
    assert_eq!(parsed.pipeline_max_revisions, Some(4));
    assert_eq!(parsed.pipeline_candidates, Some(2));
}

/// Witness for the first-run error rewrite: it used to join all thirteen
/// `PROVIDERS` rows into ~400 characters that hard-wrap into an unreadable
/// block, and pointed the user at hand-editing `~/.stella/credentials.toml`
/// while never naming `stella auth set` (which writes that file safely) or
/// `stella models` (which tabulates key status per provider).
#[test]
fn the_first_run_no_key_error_is_short_and_names_the_remediation_commands() {
    let msg = no_api_key_error();
    assert!(
        msg.contains("stella auth set"),
        "must name the command that writes the credentials file: {msg}"
    );
    assert!(
        msg.contains("stella models"),
        "must name the command that lists providers and key status: {msg}"
    );
    assert!(
        msg.len() < 300,
        "the most-hit error must stay readable on an 80-column terminal, got {} chars:\n{msg}",
        msg.len()
    );
    for line in msg.lines() {
        assert!(
            line.chars().count() <= 80,
            "line wraps on an 80-column terminal ({} chars): {line}",
            line.chars().count()
        );
    }
}

/// The short list in the error must stay a real subset of `PROVIDERS`, so it
/// cannot drift into naming an environment variable no provider reads.
#[test]
fn every_common_key_env_var_belongs_to_a_real_provider() {
    for name in COMMON_KEY_ENV_VARS {
        assert!(
            PROVIDERS
                .iter()
                .any(|p| p.env_var == *name || p.env_var_aliases.contains(name)),
            "`{name}` is not any provider's key variable"
        );
    }
}

/// The docs' provider cards must not drift from [`PROVIDERS`].
///
/// `website/src/components/provider-cards.tsx` holds one typed record per
/// provider, rendered as the card grid on both the API Providers index and the
/// getting-started walkthrough. Before it existed the same facts were typed
/// into a markdown table on each page, and they had already drifted: Bedrock's
/// "default model" cell read `Claude via Converse` — a wire dialect, not a
/// slug. Consolidating the two copies into one record removed the drift
/// *between the pages*; it did nothing about drift from this file, which is
/// the copy that actually decides what the binary does.
///
/// So: `id`, `env`, and `defaultModel` are pinned here. A renamed env var or a
/// bumped default model now fails a test instead of silently sending readers
/// to a variable Stella stopped reading.
///
/// `dialect` and `blurb` are deliberately NOT pinned — those are prose written
/// for a human ("Anthropic Messages", "OpenAI-compatible"), not the
/// kebab-case `Dialect` wire value, and asserting on them would force the
/// docs to speak the enum's language rather than the reader's.
#[test]
fn provider_cards_match_the_registry() {
    let Some(source) = read_provider_cards() else {
        return;
    };
    let cards = parse_provider_cards(&source);
    assert!(
        cards.len() >= PROVIDERS.len(),
        "parsed only {} cards from provider-cards.tsx — the parser has \
         probably gone stale against the file's shape",
        cards.len()
    );

    for provider in PROVIDERS {
        let card = cards
            .iter()
            .find(|c| c.id == provider.id)
            .unwrap_or_else(|| {
                panic!(
                    "provider `{}` has no card in provider-cards.tsx — every \
                     supported provider must appear in the docs grid",
                    provider.id
                )
            });

        assert_eq!(
            card.default_model, provider.default_model,
            "the docs card for `{}` says its default model is `{}`, but \
             PROVIDERS says `{}`",
            provider.id, card.default_model, provider.default_model
        );

        let (primary, aliases) = split_env(&card.env);
        assert_eq!(
            primary, provider.env_var,
            "the docs card for `{}` names `{primary}` as the primary \
             credential variable, but PROVIDERS reads `{}`",
            provider.id, provider.env_var
        );
        assert_eq!(
            aliases, provider.env_var_aliases,
            "the docs card for `{}` lists aliases {aliases:?}, but PROVIDERS \
             has {:?}",
            provider.id, provider.env_var_aliases
        );

        // The link must reach that provider's documentation. Deliberately NOT
        // pinned to one URL shape: the docs moved from ten per-provider pages
        // (`/docs/api-providers/zai`) to one consolidated page with anchors
        // (`/docs/api-providers#zai`), and re-encoding the site's heading slugs
        // here would make every editorial retitle a failing test in the binary
        // — asserting on the website's structure rather than on the fact this
        // file is the authority for. What still cannot drift is what actually
        // breaks a reader: a card pointing outside the API-providers docs, or
        // at the bare index with no anchor to land on.
        assert!(
            card.href.starts_with("/docs/api-providers"),
            "the docs card for `{}` links to `{}`, which is not in the \
             API-providers docs",
            provider.id,
            card.href
        );
        let lands_somewhere = card.href.len() > "/docs/api-providers".len()
            && !card.href.ends_with('#')
            && !card.href.ends_with('/');
        assert!(
            lands_somewhere,
            "the docs card for `{}` links to `{}` — the bare index, with \
             nothing identifying this provider",
            provider.id, card.href
        );
    }

    // …and two providers must never share a destination, which is how a
    // copy-pasted card silently sends readers to the wrong provider's setup.
    let mut hrefs: Vec<&str> = PROVIDERS
        .iter()
        .filter_map(|p| cards.iter().find(|c| c.id == p.id))
        .map(|c| c.href.as_str())
        .collect();
    let total = hrefs.len();
    hrefs.sort_unstable();
    hrefs.dedup();
    assert_eq!(
        hrefs.len(),
        total,
        "two provider cards link to the same page — one of them sends readers \
         to another provider's setup instructions"
    );

    // The reverse direction: a card for a provider that no longer exists sends
    // readers to a page for something they cannot select. `local` is the one
    // legitimate extra — a pseudo-provider that is deliberately absent from
    // PROVIDERS so auto-detection can never pick it.
    for card in &cards {
        assert!(
            PROVIDERS.iter().any(|p| p.id == card.id) || card.id == LOCAL_PROVIDER.id,
            "provider-cards.tsx documents `{}`, which is not a provider \
             Stella supports",
            card.id
        );
    }
}

/// A card's pinned fields, as written in the TSX record.
struct ProviderCard {
    id: String,
    href: String,
    env: String,
    default_model: String,
}

/// The docs tree, or `None` when it isn't checked out — same posture as
/// `stella-tools/tests/docs_in_sync.rs`: a repo-hygiene gate, not a runtime
/// invariant. Note the granularity — a missing *tree* skips, but a missing
/// file inside a present tree panics, so renaming the component cannot
/// silently disable this test.
fn read_provider_cards() -> Option<String> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent()?;
    let website = root.join("website");
    if !website.is_dir() {
        return None;
    }
    let path = website.join("src/components/provider-cards.tsx");
    Some(
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} is missing or unreadable: {e}", path.display())),
    )
}

/// Every `{ id: "…", …, env: "…", defaultModel: "…" }` record inside the
/// `PROVIDER_CATALOG` array.
fn parse_provider_cards(source: &str) -> Vec<ProviderCard> {
    let start = source
        .find("PROVIDER_CATALOG")
        .expect("provider-cards.tsx no longer declares PROVIDER_CATALOG");
    let body = &source[start..];

    let mut out = vec![];
    for (offset, _) in body.match_indices("id: \"") {
        // Each record runs to the start of the next one; the last runs to the
        // end. Bounding the field search this way keeps a missing field in one
        // record from silently picking up the next record's value.
        let rest = &body[offset..];
        let end = rest[1..].find("id: \"").map_or(rest.len(), |next| next + 1);
        let record = &rest[..end];

        let Some(id) = tsx_field(record, "id") else {
            continue;
        };
        out.push(ProviderCard {
            href: tsx_field(record, "href")
                .unwrap_or_else(|| panic!("the `{id}` card has no href")),
            env: tsx_field(record, "env").unwrap_or_else(|| panic!("the `{id}` card has no env")),
            default_model: tsx_field(record, "defaultModel")
                .unwrap_or_else(|| panic!("the `{id}` card has no defaultModel")),
            id,
        });
    }
    out
}

/// The value of a `key: "value"` field in a TSX object literal.
fn tsx_field(record: &str, key: &str) -> Option<String> {
    let needle = format!("{key}: \"");
    let start = record.find(&needle)? + needle.len();
    let rest = &record[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Split a card's `env` string into its primary variable and its aliases.
///
/// The docs render aliases in parentheses — `GEMINI_API_KEY (GOOGLE_API_KEY)`
/// — because that is how a reader wants to see "this one, or that one".
fn split_env(env: &str) -> (&str, Vec<&str>) {
    match env.split_once('(') {
        Some((primary, aliases)) => (
            primary.trim(),
            aliases
                .trim_end_matches(')')
                .split(',')
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .collect(),
        ),
        None => (env.trim(), vec![]),
    }
}
