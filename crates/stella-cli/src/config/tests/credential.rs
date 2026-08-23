use super::*;

#[test]
fn alias_env_var_resolves_when_the_primary_is_unset() {
    // Synthetic provider with unique var names so parallel tests can't
    // race on shared env state (the convention credential.rs's own
    // tests follow).
    let provider = ProviderConfig {
        upstream_pin: &[],
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
        upstream_pin: &[],
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
        upstream_pin: &[],
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
    // config-defined providers into auto-detection and verifier discovery.
    let provider = ProviderConfig {
        upstream_pin: &[],
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
        upstream_pin: &[],
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
