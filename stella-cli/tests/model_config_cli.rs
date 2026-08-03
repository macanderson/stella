//! End-to-end witness for `stella doctor`'s `model config` check (#895).
//!
//! A default model the provider cannot serve does not break one command — it
//! breaks *every* one, and before this it surfaced only as a provider `400` in
//! the middle of whatever the user was doing. The whole point of the check is
//! the message, so what is pinned here is not "it noticed" but "it said what
//! to type next": the setting, the value, what is wrong with it, and all three
//! ways out (the valid-slug listing, the refresh, the SETTINGS tab).
//!
//! Every fixture is hermetic on purpose. `openrouter/auto` is provable from
//! the built-in seed alone — every seeded OpenRouter id carries a `vendor/`
//! prefix, so the de-namespaced wire slug `auto` is wrong without asking
//! anyone — and the base-URL finding is pure table lookup. No key, no network,
//! no synced catalog.

use std::process::{Command, Output};

/// A scratch workspace with the given `.stella/settings.json`, plus an empty
/// HOME and data dir so neither the developer's user scope nor their real
/// `~/.stella/catalog.db` can reach the assertions.
fn workspace(settings: &str) -> (tempfile::TempDir, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("workspace");
    let home = tempfile::tempdir().expect("home");
    std::fs::create_dir_all(dir.path().join(".stella")).expect("dot dir");
    std::fs::write(dir.path().join(".stella/settings.json"), settings).expect("settings");
    (dir, home)
}

/// Run `stella <args…> doctor` with every ambient influence cut: no provider
/// key, no env-file, no auto-refresh, no org-managed scope, and none of the
/// `STELLA_*` session vars a developer's shell may be exporting.
fn doctor(dir: &tempfile::TempDir, home: &tempfile::TempDir, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_stella"));
    command
        .args(args)
        .arg("doctor")
        .current_dir(dir.path())
        .env("HOME", home.path())
        .env("STELLA_DATA_DIR", home.path().join("data"))
        .env("NO_COLOR", "1")
        .env("STELLA_NO_ENV_FILE", "1")
        .env("STELLA_CATALOG_AUTO_REFRESH", "0")
        .env(
            "STELLA_MANAGED_SETTINGS",
            home.path().join("no-managed.json"),
        )
        .env_remove("STELLA_HOME")
        .env_remove("STELLA_MODEL")
        .env_remove("STELLA_BASE_URL")
        .env_remove("STELLA_BUDGET");
    // Auto-detection reads ambient provider keys; a developer with one
    // exported must see the same verdict as CI.
    for key in [
        "OPENROUTER_API_KEY",
        "ZAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "XAI_API_KEY",
        "DEEPSEEK_API_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "VERTEX_ACCESS_TOKEN",
        "AWS_ACCESS_KEY_ID",
    ] {
        command.env_remove(key);
    }
    command.output().expect("run stella doctor")
}

#[test]
fn doctor_fails_on_a_default_model_the_catalog_rejects() {
    // `openrouter/auto` is the natural-looking form that is wrong: it reaches
    // the gateway as the bare `auto`, which OpenRouter does not serve. It is
    // exactly the class of bug #895 was filed for — configured once, breaks
    // every call after.
    let (dir, home) = workspace(r#"{"agent_engine_config": {"default_model": "openrouter/auto"}}"#);
    let output = doctor(&dir, &home, &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        output.status.code(),
        Some(1),
        "a model that cannot serve a single call must fail the command a \
         script gates on: {stdout}{stderr}"
    );
    assert!(
        stdout.contains("model config"),
        "the check names itself: {stdout}"
    );
    assert!(
        stdout.contains("default_model") && stdout.contains("openrouter/auto"),
        "the report must name the setting AND the value, or the user has to \
         guess which of their pins is the bad one: {stdout}"
    );
    assert!(
        stdout.contains("vendor namespace"),
        "the diagnosis must say what is actually wrong with the slug: {stdout}"
    );
    // The three ways out, which is the whole ask of the issue: valid slugs,
    // `stella models`, and where the default is set.
    for remedy in [
        "stella models list",
        "stella models refresh",
        "SETTINGS tab",
    ] {
        assert!(
            stdout.contains(remedy),
            "`{remedy}` must be offered — \"invalid model\" with no way forward \
             is the failure mode this check replaces: {stdout}"
        );
    }
    assert!(
        stdout.contains("--provider openrouter"),
        "the listing hint must be scoped to the provider that rejected it: {stdout}"
    );
}

#[test]
fn doctor_names_the_provider_a_mismatched_base_url_actually_points_at() {
    // The mislabeled-401 shape: an OpenRouter pin with the endpoint forced to
    // Z.ai. The key goes to Z.ai, Z.ai rejects it, and OpenRouter's adapter
    // reports the rejection — so the error blames the wrong party. Nothing
    // downstream can tell these apart; only the pairing can.
    let (dir, home) = workspace("{}");
    let output = doctor(
        &dir,
        &home,
        &[
            "--model",
            "openrouter/moonshotai/kimi-k3",
            "--base-url",
            "https://api.z.ai/api/paas/v4",
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        output.status.code(),
        Some(1),
        "an incoherent provider/endpoint pairing is a failure, not a note: {stdout}"
    );
    assert!(
        stdout.contains("--base-url"),
        "the report names the flag that caused it: {stdout}"
    );
    assert!(
        stdout.contains("Z.ai") && stdout.contains("OpenRouter"),
        "BOTH sides must be named — the whole bug is that the error blames the \
         wrong one: {stdout}"
    );
    assert!(
        stdout.contains("OPENROUTER_API_KEY"),
        "the credential that would be leaked to the other host is named: {stdout}"
    );
}

#[test]
fn doctor_leaves_a_private_gateway_alone() {
    // A `--base-url` at a host stella does not recognize is a proxy, a tunnel,
    // or a corporate gateway — a legitimate and common use of the flag. The
    // coherence check must never cry wolf at one, or it teaches users to
    // ignore it.
    let (dir, home) = workspace("{}");
    let output = doctor(
        &dir,
        &home,
        &[
            "--model",
            "openrouter/moonshotai/kimi-k3",
            "--base-url",
            "https://llm-gateway.internal.example/v1",
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "an unrecognized host is a private gateway, not a mistake: {stdout}"
    );
}

#[test]
fn doctor_reports_the_model_a_valid_config_resolves_to_and_where_it_came_from() {
    // A pass is not silence: knowing WHICH model the next run will send, and
    // which setting decided that, is half of what people open the doctor for.
    let (dir, home) = workspace(r#"{"agent_engine_config": {"default_model": "zai/glm-5.2"}}"#);
    let output = doctor(&dir, &home, &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(
        stdout.contains("zai/glm-5.2") && stdout.contains("default_model"),
        "the pass states the resolved model and its source: {stdout}"
    );
    assert!(stdout.contains("4 ok, 0 failed"), "{stdout}");
}

#[test]
fn the_flag_outranks_the_setting_and_the_doctor_diagnoses_the_flag() {
    // #895 problem 4 assumed a settings-pinned model silently beats the
    // override. It does not: `--model` and `STELLA_MODEL` are one clap slot,
    // and `Config::load` does `model_override.or(engine_default)`. So a GOOD
    // setting cannot rescue a BAD flag — and the doctor must diagnose the one
    // that will actually be used, naming it as the flag.
    let (dir, home) = workspace(r#"{"agent_engine_config": {"default_model": "zai/glm-5.2"}}"#);
    let output = doctor(&dir, &home, &["--model", "openrouter/auto"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        output.status.code(),
        Some(1),
        "the flag wins, so its bad value must fail the check: {stdout}"
    );
    assert!(
        stdout.contains("--model"),
        "the finding is attributed to the override, not to the innocent \
         setting underneath it: {stdout}"
    );
}
