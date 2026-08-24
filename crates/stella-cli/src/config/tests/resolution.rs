use super::*;

#[test]
fn config_debug_never_leaks_the_api_key() {
    // H3: with `api_key: ApiKey`, the whole Config's derived Debug must
    // redact the secret — no `{:?}` (logs, panics, traces) can leak it.
    let secret = "sk-super-secret-do-not-log-XYZ";
    let cfg = Config {
        provider: PROVIDERS[0].clone(),
        model_id: "glm-5.2".to_string(),
        turn_timeout: None,
        max_output_tokens: None,
        plan_mode: false,
        minimal_prompt: false,
        model_pinned_by_flag: false,
        durability: Default::default(),
        output_ceilings: Default::default(),
        create_worktrees: Default::default(),
        allowed_write_dirs: Vec::new(),
        api_key: ApiKey::new(secret),
        workspace_root: std::path::PathBuf::from("/tmp/ws"),
        base_url_override: None,
        hooks: None,
        engine_settings: None,
        engine_settings_trusted: false,
        tool_policy: Default::default(),
        ignore_gitignore: true,
        reward_policy: Default::default(),
        authority: crate::settings::AuthorityPolicy::default(),
        credential_source: Some(stella_model::credential::CredentialSource::EnvVar),
        credential_advisories: Vec::new(),
        aux_credentials: Default::default(),
        cache_ttl: None,
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
        steering_allowed: true,
        project_prompts_allowed: true,
        project_custom_tools_allowed: false,
        media_requires_host_approval: true,
        withheld: None,
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

/// A redirected user home plus a `Config` whose reloadable fields all sit at
/// their defaults, so any one of them moving is visible to a witness.
///
/// The user scope is redirected with the thread-local paths seam (#1139), NOT
/// by setting `$HOME`: no environment mutation, no `unsafe`, and no race with
/// a test on another thread. Without it `UserPaths::test_default` keeps the
/// developer's real home and the test reads their actual
/// `~/.stella/settings.json`.
///
/// `tag` keeps concurrent tests off each other's directory. The returned guard
/// must be held for as long as the `Config` is used — dropping it restores the
/// real home mid-test.
pub(in crate::config) fn reload_fixture(
    tag: &str,
) -> (std::path::PathBuf, crate::paths::TestPathsGuard, Config) {
    let home = std::env::temp_dir().join(format!("stella-test-{tag}-{}", std::process::id()));
    let workspace = home.join("ws");
    std::fs::create_dir_all(home.join(".stella")).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let paths = crate::paths::test_user_home(home.clone());

    let cfg = Config {
        provider: PROVIDERS[0].clone(),
        model_id: "glm-5.2".to_string(),
        turn_timeout: None,
        max_output_tokens: None,
        plan_mode: false,
        minimal_prompt: false,
        model_pinned_by_flag: false,
        durability: Default::default(),
        output_ceilings: Default::default(),
        create_worktrees: Default::default(),
        allowed_write_dirs: Vec::new(),
        api_key: ApiKey::new("k"),
        workspace_root: workspace,
        base_url_override: None,
        cache_ttl: None,
        hooks: None,
        engine_settings: None,
        engine_settings_trusted: false,
        tool_policy: Default::default(),
        ignore_gitignore: true,
        reward_policy: Default::default(),
        authority: crate::settings::AuthorityPolicy::default(),
        credential_source: Some(stella_model::credential::CredentialSource::EnvVar),
        credential_advisories: Vec::new(),
        aux_credentials: Default::default(),
    };
    assert!(
        cfg.tool_policy.allows("bash"),
        "premise: the default policy allows bash"
    );
    use crate::settings::CreateWorktrees as CW;
    assert_eq!(cfg.create_worktrees, CW::Ask, "premise: worktrees default");
    (home, paths, cfg)
}

/// Witness for `/reload` (`Config::reload_from_disk`): a settings edit made
/// *after* the session's `Config` was resolved is re-applied to the live
/// value — the fields the scope chain derives (the worktree policy, the tool
/// switches) flip without a restart. Fails to compile on a build without
/// `reload_from_disk`.
///
/// The vehicle was `enable_recap` until #3870 retired it. `ignore_gitignore`
/// is back beside the other two now that #3895 wired it: it is the field whose
/// default is ON, so the edit flips in the direction a forgotten merge would
/// not catch. Which fields `/reload` owes is settled exhaustively in
/// `config::reload::completeness`; this stays the readable three-assertion
/// witness.
#[test]
fn reload_from_disk_reapplies_the_settings_scope_chain() {
    // `reload_from_disk` reads the process-wide trusted-engine-config env
    // var, so hold the binary env lock — read-only, exactly as
    // `resolved_config_carries_the_authority_computed_during_settings_load`
    // does: a concurrent test setting that var malformed would otherwise make
    // this load fail.
    let _env = crate::test_env::lock();
    let (home, _paths, mut cfg) = reload_fixture("reload-from-disk");

    // The edit a running session would previously only see after a restart.
    std::fs::write(
        home.join(".stella").join("settings.json"),
        r#"{"create_worktrees": "never", "tools": {"bash": "off"}, "ignore_gitignore": "off"}"#,
    )
    .unwrap();

    cfg.reload_from_disk().unwrap();

    assert_eq!(
        cfg.create_worktrees,
        crate::settings::CreateWorktrees::Never,
        "reload must re-derive the worktree policy from the scope chain"
    );
    assert!(
        !cfg.tool_policy.allows("bash"),
        "reload must re-derive the tool switches from the scope chain on disk"
    );
    assert!(
        !cfg.ignore_gitignore,
        "reload must re-derive the gitignore filter — a session told the reload \
         succeeded kept walking ignored paths until it restarted (#3895)"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// Witness for the all-or-nothing half of `Config::reload_from_disk`: a scope
/// chain that loads but does not *resolve* leaves the live `Config` exactly as
/// it was.
///
/// The settings file below is well-formed — it parses, and every switch in it
/// is individually legal — but `deterministic_weight: 0.0` gives the reward
/// scale a zero unit, which `reward_policy()` refuses by name rather than
/// clamping. That is the only fallible step downstream of the load, so it
/// is the lever that separates "derive, then commit" from "assign as you go":
/// with the assignments interleaved, `create_worktrees` and the `bash` switch
/// are already written by the time the reward weights are rejected, and the
/// session runs its next turn on a posture no scope chain ever produced —
/// while both callers in `command_deck::settings_io` tell the user the reload
/// failed and the previous values were kept.
#[test]
fn a_failed_reload_leaves_every_field_untouched() {
    let _env = crate::test_env::lock();
    let (home, _paths, mut cfg) = reload_fixture("reload-atomicity");

    std::fs::write(
        home.join(".stella").join("settings.json"),
        r#"{"create_worktrees": "never", "tools": {"bash": "off"}, "reward": {"deterministic_weight": 0.0}}"#,
    )
    .unwrap();

    let error = cfg
        .reload_from_disk()
        .expect_err("a scale with no unit must not resolve");
    assert!(error.contains("deterministic_weight"), "{error}");

    assert_eq!(
        cfg.create_worktrees,
        crate::settings::CreateWorktrees::Ask,
        "a failed reload must not leave the worktree policy applied — the \
         callers report the previous values were kept",
    );
    assert!(
        cfg.tool_policy.allows("bash"),
        "a failed reload must not leave the tool switches applied — a turn \
         would run under a policy the operator was told was not adopted"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// **Witness (#1839).** `providers.<id>.cache_ttl` reaches the resolved
/// config, a pin outranks the interactive-surface stamp, and an unpinned
/// config widens to the 1-hour window only when a surface asks.
#[test]
fn cache_ttl_pin_resolves_and_the_interactive_default_never_overrides_it() {
    use stella_model::CacheTtl;
    let _env = crate::test_env::lock();
    let settings = settings_from(r#"{"providers": {"local": {"cache_ttl": "5m"}}}"#);
    let mut pinned = Config::load_with_settings(
        Some("local/test-model"),
        None,
        Some("http://localhost:11434/v1"),
        &settings,
        std::path::PathBuf::from("/tmp/ws"),
    )
    .unwrap();
    assert_eq!(pinned.cache_ttl, Some(CacheTtl::FiveMinutes));
    pinned.adopt_interactive_cache_ttl();
    assert_eq!(
        pinned.effective_cache_ttl(),
        CacheTtl::FiveMinutes,
        "a settings pin outranks the surface default"
    );

    let mut unpinned = Config::load_with_settings(
        Some("local/test-model"),
        None,
        Some("http://localhost:11434/v1"),
        &crate::settings::Settings::default(),
        std::path::PathBuf::from("/tmp/ws"),
    )
    .unwrap();
    assert_eq!(unpinned.cache_ttl, None);
    assert_eq!(
        unpinned.effective_cache_ttl(),
        CacheTtl::FiveMinutes,
        "headless paths never widen the provider default"
    );
    unpinned.adopt_interactive_cache_ttl();
    assert_eq!(unpinned.effective_cache_ttl(), CacheTtl::OneHour);
}

#[test]
fn benchmark_mode_skips_malformed_filesystem_credentials_but_keeps_engine_override() {
    let _env = crate::test_env::lock();
    let _restore = crate::test_env::EnvRestore::capture(&[TRUSTED_ENGINE_CONFIG_ENV]);
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
    let _home = crate::test_env::home_sandbox(dir.path());
    unsafe {
        std::env::set_var(TRUSTED_ENGINE_CONFIG_ENV, trusted);
    }
    let _isolation = crate::paths::test_filesystem_isolation(true);
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
        deterministic_weight: Some(0.2),
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
    assert_eq!(cfg.reward_policy.outcome.deterministic, 0.2);
    assert_eq!(
        cfg.reward_policy.shaping.per_usd, 0.5,
        "an unset weight stays at its default"
    );
}

/// A reward weight with no usable scale fails the LAUNCH, by name — it does not
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
        deterministic_weight: Some(0.0),
        ..Default::default()
    });
    let error = Config::load_with_settings(
        Some("openrouter/deepseek/deepseek-v4-pro"),
        Some("test-key"),
        Some("https://openrouter.ai/api/v1"),
        &settings,
        std::path::PathBuf::from("/tmp/ws"),
    )
    .expect_err("a scale with no unit must not launch");
    assert!(error.contains("deterministic_weight"), "{error}");
    assert!(error.contains("greater than zero"), "{error}");
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
            "verifier": {"effort": "high", "reasoning": "on"},
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

/// `--upstream-pin` outranks a settings entry.
///
/// The harness that most needs a pin runs with settings isolation
/// (`STELLA_NO_SETTINGS`), so an argument is the only authority that reaches a
/// measured trial. If a project file could override it, a run could be
/// re-pointed to a different upstream without the operator who passed the flag
/// ever seeing it — which is the uncontrolled variable the pin exists to
/// remove.
#[test]
fn the_upstream_pin_flag_outranks_a_settings_entry() {
    let flag = vec!["z-ai".to_string()];
    let entry = vec!["anthropic".to_string()];

    assert_eq!(
        pin_source(Some(&flag), Some(&entry)),
        Some(&flag[..]),
        "the flag wins when both are present"
    );
    assert_eq!(
        pin_source(None, Some(&entry)),
        Some(&entry[..]),
        "the settings entry applies when no flag was passed"
    );
    assert_eq!(
        pin_source(Some(&flag), None),
        Some(&flag[..]),
        "the flag applies with no settings entry — the isolated-harness case"
    );
    assert_eq!(
        pin_source(None, None),
        None,
        "unpinned by default: routing stays the gateway's choice"
    );
}

/// The flag parses a comma-separated order into a preference list, and stays
/// empty when absent — an unpinned run must send no `provider` field at all,
/// since the request body is the prompt-cache key.
#[test]
fn the_upstream_pin_flag_parses_an_order() {
    use clap::Parser;

    let cli = crate::cli::Cli::parse_from(["stella", "--upstream-pin", "z-ai,anthropic", "models"]);
    assert_eq!(cli.globals.upstream_pin, vec!["z-ai", "anthropic"]);

    let bare = crate::cli::Cli::parse_from(["stella", "models"]);
    assert!(bare.globals.upstream_pin.is_empty());
}

/// **The wiring witness for #4417.** An installed plugin's declared `Stop`
/// hook reaches the *resolved config* — the object every door reads its hook
/// plane from — and not merely the function that folds it.
///
/// This is the assertion that fails on a build where `crate::plugin_hooks`
/// exists and `load_with_settings` still stamps `settings.hooks.clone()`: a
/// fold nothing calls is the same unwired code the issue is about, one layer
/// up. It lives here rather than beside the fold because `load_with_settings`
/// is private to `config`.
#[test]
fn an_installed_plugins_hook_route_reaches_the_resolved_config() {
    use crate::plugin_hooks::tests::{plant, stop_actions, temp_root};

    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let root = temp_root("resolved-config");
    let _paths = crate::paths::test_user_home(root.join("home"));
    plant(&stella_home::resolve_project_plugins_dir(&root), "vera");
    // SAFETY: the env lock is held for the whole mutate-read-restore window.
    unsafe { std::env::set_var("STELLA_TRUST_PROJECT", "1") };

    let cfg = Config::load_with_settings(
        Some("local/test-model"),
        None,
        Some("http://localhost:11434/v1"),
        &crate::settings::Settings::default(),
        root.clone(),
    )
    .expect("an offline local-provider config");
    let plane = cfg
        .hooks
        .as_ref()
        .expect("the plugin plane reached the config");
    assert_eq!(
        stop_actions(plane)
            .iter()
            .filter_map(|action| action.plugin.as_ref().map(|origin| origin.plugin.clone()))
            .collect::<Vec<_>>(),
        vec!["vera".to_string()],
    );

    let _ = std::fs::remove_dir_all(&root);
}
