//! Tests for the `stella.toml` format port.
//!
//! The load-bearing one is
//! [`a_toml_config_and_its_json_equivalent_produce_identical_settings`]: the
//! whole premise of Phase 1 is that the port changes SPELLING and nothing else.
//! If that test can be made to pass only by weakening it, the port has changed
//! behavior and the migration is no longer safe to run unattended.

use crate::settings::toml_config::{ConfigScope, TomlConfig, load_toml_scope};
use crate::settings::*;

fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

fn load_toml(path: &Path, scope: ConfigScope) -> Result<Settings, String> {
    load_toml_scope(path, scope, |p| std::fs::read_to_string(p)).map(|loaded| loaded.settings)
}

/// THE port contract. Two files, same meaning, byte-different formats.
#[test]
fn a_toml_config_and_its_json_equivalent_produce_identical_settings() {
    let dir = tempfile::tempdir().unwrap();

    let json = write(
        dir.path(),
        "settings.json",
        r#"{
          "providers": {
            "anthropic": {"api_key_env": "ANTHROPIC_API_KEY", "default_model": "claude-fable-5"},
            "vllm": {"base_url": "http://x/v1", "dialect": "openai-compatible"}
          },
          "agent_engine_config": {
            "default_model": "anthropic/claude-fable-5",
            "pipeline_verifier_model": "openrouter/openai/gpt-5.5",
            "allowed_models": ["anthropic/claude-fable-5", "zai/glm-5.2"],
            "auto_mode": "off",
            "effort_auto": "on",
            "headless_scope_bypass": "off",
            "pipeline_max_revisions": 4,
            "pipeline_candidates": 2,
            "agents": {
              "verifier": {
                "provider": "openrouter",
                "model": "openai/gpt-5.5",
                "effort": "high",
                "reasoning": "on",
                "params": {"temperature": 0.2, "max_tokens": 4096, "service_tier": "priority"}
              },
              "triage": {"reasoning": "off"}
            }
          },
          "tools": {"bash": "off", "process": "off"},
          "enable_recap": "off",
          "trace_capture": "on",
          "ui": {"theme": "stella-dark"},
          "reward": {"verifier_weight": 0.25, "per_usd": 0.75},
          "mcp": {"registry_url": "https://registry.example"}
        }"#,
    );

    let toml_path = write(
        dir.path(),
        "stella.toml",
        r#"
[meta]
schema_version = 1

[run]
recap = "off"
trace_capture = "on"

[reward]
verifier_weight = 0.25
per_usd = 0.75

[providers.anthropic]
api_key_env = "ANTHROPIC_API_KEY"
default_model = "claude-fable-5"

[providers.vllm]
base_url = "http://x/v1"
dialect = "openai-compatible"

[models]
allowed = ["anthropic/claude-fable-5", "zai/glm-5.2"]

[agents]
default_model = "anthropic/claude-fable-5"
pipeline_verifier_model = "openrouter/openai/gpt-5.5"
auto_mode = "off"
effort_auto = "on"
headless_scope_bypass = "off"
pipeline_max_revisions = 4
pipeline_candidates = 2

[agents.verifier]
provider = "openrouter"
model = "openai/gpt-5.5"
effort = "high"
reasoning = "on"

[agents.verifier.params]
temperature = 0.2
max_tokens = 4096
service_tier = "priority"

[agents.triage]
reasoning = "off"

[tools]
bash = "off"
process = "off"

[ui]
theme = "stella-dark"

[mcp]
registry_url = "https://registry.example"
"#,
    );

    let from_json = Settings::load_from(&[json]).unwrap();
    let from_toml = load_toml(&toml_path, ConfigScope::User).unwrap();

    assert_eq!(from_json.providers, from_toml.providers, "providers");
    assert_eq!(
        from_json.agent_engine_config, from_toml.agent_engine_config,
        "agent_engine_config — the block the TOML shape reorganizes most"
    );
    assert_eq!(from_json.tools, from_toml.tools, "tools");
    assert_eq!(from_json.enable_recap, from_toml.enable_recap, "recap");
    assert_eq!(
        from_json.trace_capture, from_toml.trace_capture,
        "trace_capture"
    );
    assert!(
        from_toml.trace_capture_enabled(),
        "[run].trace_capture lowers into the flag"
    );
    assert_eq!(from_json.ui, from_toml.ui, "ui");
    assert_eq!(from_json.reward, from_toml.reward, "reward");
    assert_eq!(
        from_toml.reward_policy().unwrap().outcome.judged,
        0.25,
        "[reward].verifier_weight resolves into the policy"
    );
    assert_eq!(
        from_toml.reward_policy().unwrap().outcome.deterministic,
        1.0,
        "an unset weight is the default, not zero"
    );
    assert_eq!(from_json.mcp, from_toml.mcp, "mcp");
}

/// A workspace that distrusts its verifier writes one key and inherits the rest.
/// The alternative — an absent key meaning `0.0` — would silently discard every
/// judged turn for anyone who set only `per_step`.
#[test]
fn an_absent_reward_key_is_the_default_not_zero() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "stella.toml",
        "[reward]\nverifier_weight = 0.1\n",
    );
    let settings = load_toml(&path, ConfigScope::User).unwrap();
    let policy = settings.reward_policy().unwrap();
    assert_eq!(policy.outcome.judged, 0.1);
    assert_eq!(policy.outcome.deterministic, 1.0);
    assert_eq!(policy.shaping.per_step, 0.02);
    assert_eq!(policy.shaping.per_usd, 0.5);
    assert_eq!(policy.shaping.per_revision, 0.1);
}

/// No `[reward]` block at all is exactly the defaults — the shipped behavior is
/// unchanged for every workspace that never opts in.
#[test]
fn no_reward_block_is_exactly_the_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "stella.toml", "[run]\nrecap = \"on\"\n");
    let settings = load_toml(&path, ConfigScope::User).unwrap();
    assert_eq!(settings.reward, None);
    assert_eq!(
        settings.reward_policy().unwrap(),
        stella_pipeline::reward::RewardPolicy::default()
    );
}

/// A verifier weight above the deterministic one is refused at load, by name.
/// Loud beats clamping: a silently-substituted weight produces correctly-shaped
/// labels that mean something the operator never asked for.
#[test]
fn a_verifier_weight_above_the_ceiling_fails_the_load_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "stella.toml",
        "[reward]\nverifier_weight = 2.0\n",
    );
    let settings = load_toml(&path, ConfigScope::User).unwrap();
    let error = settings
        .reward_policy()
        .expect_err("a verifier outranking a test must not load");
    assert!(error.contains("verifier_weight"), "{error}");
    assert!(error.contains("deterministic_weight"), "{error}");
    // The message has to say what to do, not just that something is wrong.
    assert!(error.contains("Lower it"), "{error}");
}

#[test]
fn models_allowed_lands_in_allowed_models() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "stella.toml",
        "[models]\nallowed = [\"a/b\", \"c/d\"]\n",
    );
    let settings = load_toml(&path, ConfigScope::User).unwrap();
    let engine = settings.agent_engine_config.unwrap();
    assert_eq!(engine.allowed_models.unwrap(), vec!["a/b", "c/d"]);
}

/// An absent block must stay absent. An all-`None` `AgentEngineConfig` would
/// read to the scope merge as "this scope has an opinion about the engine",
/// letting an empty file outrank a lower scope that actually configured one.
#[test]
fn an_empty_document_produces_no_engine_config_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "stella.toml", "[ui]\ntheme = \"stella-dark\"\n");
    let settings = load_toml(&path, ConfigScope::User).unwrap();
    assert!(
        settings.agent_engine_config.is_none(),
        "no [agents]/[models] means no engine block"
    );
}

/// Same reasoning for `[mcp]`: a block that only carried servers must not
/// synthesize an `McpSettings` whose `registry_url` is None, because the merge
/// cannot tell that apart from a real setting.
#[test]
fn an_mcp_block_without_a_registry_url_contributes_no_mcp_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "stella.toml",
        "[mcp.servers.fs]\ntransport = \"stdio\"\ncmd = \"x\"\n",
    );
    let settings = load_toml(&path, ConfigScope::User).unwrap();
    assert!(settings.mcp.is_none());
}

#[test]
fn a_future_schema_version_is_refused_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "stella.toml", "[meta]\nschema_version = 99\n");
    let err = load_toml(&path, ConfigScope::User).unwrap_err();
    assert!(err.contains("schema_version 99"), "{err}");
    assert!(err.contains("upgrade stella"), "says what to do: {err}");
}

#[test]
fn a_file_with_no_meta_block_is_version_one_by_definition() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "stella.toml",
        "[ui]\ntheme = \"stella-light\"\n",
    );
    let settings = load_toml(&path, ConfigScope::User).unwrap();
    assert_eq!(settings.ui_theme(), Some("stella-light"));
}

/// A project file must not be able to claim managed authority by asserting it.
#[test]
fn a_declared_scope_that_disagrees_with_the_location_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "stella.toml", "[meta]\nscope = \"managed\"\n");
    let err = load_toml(&path, ConfigScope::Project).unwrap_err();
    assert!(err.contains("scope = \"managed\""), "{err}");
    assert!(err.contains("loaded as the project config"), "{err}");
}

#[test]
fn a_matching_declared_scope_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "stella.toml", "[meta]\nscope = \"project\"\n");
    assert!(load_toml(&path, ConfigScope::Project).is_ok());
}

/// A root `stella.toml` gets committed. The one field that was tolerable only
/// through obscurity stops being tolerable.
#[test]
fn an_inline_api_key_is_refused_at_project_scope_with_both_alternatives_named() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "stella.toml",
        "[providers.anthropic]\napi_key = \"sk-secret\"\n",
    );
    let err = load_toml(&path, ConfigScope::Project).unwrap_err();
    assert!(err.contains("api_key"), "{err}");
    assert!(
        err.contains("api_key_env"),
        "names the env alternative: {err}"
    );
    assert!(
        err.contains("credentials.toml"),
        "names the file alternative: {err}"
    );
    // The secret itself must never appear in the message.
    assert!(!err.contains("sk-secret"), "the key is not echoed: {err}");
}

#[test]
fn an_inline_api_key_still_works_at_user_scope() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "stella.toml",
        "[providers.anthropic]\napi_key = \"sk-secret\"\n",
    );
    let settings = load_toml(&path, ConfigScope::User).unwrap();
    assert_eq!(
        settings.providers["anthropic"].api_key.as_deref(),
        Some("sk-secret")
    );
}

#[test]
fn a_restated_provider_id_must_match_its_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "stella.toml",
        "[providers.anthropic]\nid = \"openai\"\n",
    );
    let err = load_toml(&path, ConfigScope::User).unwrap_err();
    assert!(err.contains("must match its key"), "{err}");
}

#[test]
fn a_malformed_document_is_a_named_error_not_an_empty_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "stella.toml", "[providers\nbroken");
    let err = load_toml(&path, ConfigScope::User).unwrap_err();
    assert!(err.contains("invalid config file"), "{err}");
    assert!(err.contains("stella.toml"), "{err}");
}

#[test]
fn a_missing_file_is_an_empty_scope_never_an_error() {
    let settings = load_toml(Path::new("/nonexistent/stella.toml"), ConfigScope::User).unwrap();
    assert_eq!(settings, Settings::default());
}

/// `[mcp.servers]` is parsed so a migrated file is never rejected, but it is
/// not consumed yet — and that must be SAID, not silently dropped.
#[test]
fn declared_mcp_servers_are_announced_as_not_yet_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "stella.toml",
        "[mcp.servers.fs]\ntransport = \"stdio\"\ncmd = \"mcp-fs\"\n",
    );
    let loaded =
        load_toml_scope(&path, ConfigScope::Project, |p| std::fs::read_to_string(p)).unwrap();
    assert_eq!(loaded.mcp.servers.len(), 1, "still parsed");
    assert_eq!(loaded.warnings.len(), 1, "and announced");
    assert!(
        loaded.warnings[0].contains("does not read yet"),
        "{:?}",
        loaded.warnings
    );
    assert!(
        loaded.warnings[0].contains(".stella/mcp.toml"),
        "points at where they DO work: {:?}",
        loaded.warnings
    );
}

// ── The unrecognized-key pass ───────────────────────────────────────────────

#[test]
fn unknown_toml_roots_are_flagged() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "stella.toml",
        "[uii]\ntheme = \"x\"\n\n[agents]\ndefault_modle = \"y\"\n",
    );
    let found = super::unknown::unknown_toml_keys_in(&path);
    assert!(found.contains(&"uii".to_string()), "{found:?}");
    assert!(
        found.contains(&"agents.default_modle".to_string()),
        "{found:?}"
    );
}

/// The two vocabularies are genuinely different, and each must reject the
/// other's spelling — otherwise the warning would bless a key that configures
/// nothing in the format actually being read.
#[test]
fn the_json_spelling_of_a_renamed_key_is_unknown_in_toml() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "stella.toml",
        "enable_recap = \"off\"\n\n[agent_engine_config]\ndefault_model = \"x\"\n",
    );
    let found = super::unknown::unknown_toml_keys_in(&path);
    assert!(
        found.contains(&"agent_engine_config".to_string()),
        "the JSON name is not a TOML root: {found:?}"
    );
    assert!(
        found.contains(&"enable_recap".to_string()),
        "the bare root scalar moved to [run].recap: {found:?}"
    );
}

#[test]
fn a_valid_toml_document_flags_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "stella.toml",
        r#"
[meta]
schema_version = 1
scope = "project"

[run]
recap = "on"

[models]
allowed = ["a/b"]

[agents]
default_model = "a/b"

[agents.verifier]
effort = "high"

[agents.verifier.params]
temperature = 0.1

[providers.anthropic]
api_key_env = "K"

[tools]
bash = "off"

[mcp]
registry_url = "https://x"

[ui]
theme = "stella-dark"
"#,
    );
    assert_eq!(
        super::unknown::unknown_toml_keys_in(&path),
        Vec::<String>::new()
    );
}

#[test]
fn open_maps_are_descended_into_not_flagged() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "stella.toml",
        // Tool names, provider ids, and hook matchers are user-chosen data.
        "[tools]\nsome_mcp_tool = \"off\"\n\n[providers.my-own-gateway]\nbase_url = \"http://x\"\n",
    );
    assert_eq!(
        super::unknown::unknown_toml_keys_in(&path),
        Vec::<String>::new()
    );
}

// ── Parsing details worth pinning ───────────────────────────────────────────

#[test]
fn hook_renames_survive_the_format_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "stella.toml",
        r#"
[[hooks.PreToolUse]]
matcher = "bash"

  [[hooks.PreToolUse.hooks]]
  type = "command"
  command = "./audit.sh"
  timeoutMs = 5000
"#,
    );
    let settings = load_toml(&path, ConfigScope::User).unwrap();
    let hooks = settings.hooks.unwrap();
    let matchers = hooks.pre_tool_use.unwrap();
    assert_eq!(matchers[0].matcher.as_deref(), Some("bash"));
    assert_eq!(matchers[0].hooks[0].kind, "command");
    assert_eq!(matchers[0].hooks[0].timeout_ms, Some(5000));
}

#[test]
fn the_wildcard_tool_key_survives_quoting() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "stella.toml",
        "[tools]\n\"*\" = \"off\"\nread_file = \"on\"\n",
    );
    let settings = load_toml(&path, ConfigScope::User).unwrap();
    let policy = settings.tool_policy();
    assert!(policy.allows("read_file"));
    assert!(!policy.allows("write_file"));
}

#[test]
fn a_typoed_toggle_is_a_loud_parse_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "stella.toml", "[run]\nrecap = \"onn\"\n");
    let err = load_toml(&path, ConfigScope::User).unwrap_err();
    assert!(err.contains("invalid config file"), "{err}");
}

#[test]
fn the_document_round_trips_through_parse_without_losing_a_field() {
    // Guards the lowering: every field that is set must survive into Settings.
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "stella.toml",
        r#"
[agents]
default_model = "a/b"
pipeline_worker_model = "c/d"
pipeline_verifier_model = "e/f"
pipeline_triage_model = "g/h"
auto_mode = "on"
effort_auto = "off"
reasoning_auto = "on"
headless_scope_bypass = "on"
"#,
    );
    let engine = load_toml(&path, ConfigScope::User)
        .unwrap()
        .agent_engine_config
        .unwrap();
    assert_eq!(engine.default_model.as_deref(), Some("a/b"));
    assert_eq!(engine.pipeline_worker_model.as_deref(), Some("c/d"));
    assert_eq!(engine.pipeline_verifier_model.as_deref(), Some("e/f"));
    assert_eq!(engine.pipeline_triage_model.as_deref(), Some("g/h"));
    assert!(engine.auto_mode_on());
    assert!(!engine.effort_auto_on());
    assert!(engine.reasoning_auto_on());
    assert!(engine.headless_scope_bypass_on());
}

#[test]
fn parse_accepts_an_entirely_empty_document() {
    let cfg = TomlConfig::parse("", Path::new("stella.toml")).unwrap();
    assert_eq!(cfg, TomlConfig::default());
}

/// The shipped reference config must actually load, with no unrecognized keys.
///
/// This exists because the doc and the code DID diverge: `stella.today.toml`
/// kept the JSON spellings `enable_recap` (a bare root scalar) and
/// `agents.allowed_models` after the port moved them to `[run].recap` and
/// `[models].allowed`. Both parsed fine and configured nothing, and were caught
/// only by pointing a real binary at the file.
///
/// A reference config that documents keys the loader ignores is worse than no
/// reference at all — it is a wrong answer with the authority of an example.
#[test]
fn the_shipped_reference_config_loads_with_no_unrecognized_keys() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("docs/design/config-system/stella.today.toml");
    assert!(
        path.exists(),
        "reference config missing at {}",
        path.display()
    );

    let unknown = super::unknown::unknown_toml_keys_in(&path);
    assert_eq!(
        unknown,
        Vec::<String>::new(),
        "the documented example uses keys stella does not read"
    );

    // It declares scope = "project", so it must load as one — and refuse to
    // load as anything else.
    load_toml(&path, ConfigScope::Project).expect("reference config should load");
}

// ── Migration ───────────────────────────────────────────────────────────────

const RICH_SETTINGS: &str = r#"{
  "providers": {
    "anthropic": {"api_key_env": "ANTHROPIC_API_KEY", "default_model": "claude-fable-5"},
    "vllm": {"base_url": "http://x/v1", "dialect": "openai-compatible", "name": "Internal"}
  },
  "agent_engine_config": {
    "default_model": "anthropic/claude-fable-5",
    "pipeline_worker_model": "zai/glm-5.2",
    "allowed_models": ["anthropic/claude-fable-5", "zai/glm-5.2"],
    "auto_mode": "off",
    "effort_auto": "on",
    "reasoning_auto": "on",
    "headless_scope_bypass": "off",
    "agents": {
      "verifier": {"effort": "high", "reasoning": "on",
                "params": {"temperature": 0.2, "top_p": 0.9, "max_tokens": 4096}},
      "triage": {"reasoning": "off"}
    }
  },
  "tools": {"bash": "off", "process": "off"},
  "enable_recap": "on",
  "ui": {"theme": "stella-dark"},
  "mcp": {"registry_url": "https://registry.example"},
  "context": {"retrieval": {"rrf_k": 60.0, "mmr_lambda": 0.7, "ann_probes": 12}},
  "hooks": {"PreToolUse": [
    {"matcher": "bash", "hooks": [{"type": "command", "command": "./audit.sh", "timeoutMs": 5000}]}
  ]}
}"#;

/// THE migration contract: what stella reads out of the generated TOML must be
/// what it read out of the JSON. Everything else about the migration is
/// presentation.
#[test]
fn migrating_a_settings_file_preserves_every_value_it_configured() {
    let dir = tempfile::tempdir().unwrap();
    let json = write(dir.path(), "settings.json", RICH_SETTINGS);
    let toml_path = dir.path().join("stella.toml");

    super::migrate::migrate_scope(&json, &toml_path, ConfigScope::User, false).unwrap();

    let before = Settings::load_scope(&json).unwrap();
    let after = load_toml(&toml_path, ConfigScope::User).unwrap();

    assert_eq!(before.providers, after.providers, "providers");
    assert_eq!(
        before.agent_engine_config, after.agent_engine_config,
        "engine config"
    );
    assert_eq!(before.tools, after.tools, "tools");
    assert_eq!(before.enable_recap, after.enable_recap, "recap");
    assert_eq!(before.ui, after.ui, "ui");
    assert_eq!(before.mcp, after.mcp, "mcp");
    assert_eq!(before.context, after.context, "context");
    assert_eq!(before.hooks, after.hooks, "hooks");
}

#[test]
fn a_migration_never_deletes_the_json_it_read() {
    let dir = tempfile::tempdir().unwrap();
    let json = write(dir.path(), "settings.json", RICH_SETTINGS);
    let toml_path = dir.path().join("stella.toml");
    super::migrate::migrate_scope(&json, &toml_path, ConfigScope::User, false).unwrap();
    assert!(json.exists(), "the input survives");
    assert!(toml_path.exists(), "the output was written");
}

#[test]
fn a_dry_run_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let json = write(dir.path(), "settings.json", RICH_SETTINGS);
    let toml_path = dir.path().join("stella.toml");
    super::migrate::migrate_scope(&json, &toml_path, ConfigScope::User, true).unwrap();
    assert!(!toml_path.exists());
}

#[test]
fn an_existing_stella_toml_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let json = write(dir.path(), "settings.json", RICH_SETTINGS);
    let toml_path = write(
        dir.path(),
        "stella.toml",
        "# hand-written\n[ui]\ntheme = \"x\"\n",
    );
    let outcome =
        super::migrate::migrate_scope(&json, &toml_path, ConfigScope::User, false).unwrap();
    assert!(matches!(
        outcome,
        super::migrate::Outcome::AlreadyMigrated(_)
    ));
    let after = std::fs::read_to_string(&toml_path).unwrap();
    assert!(after.contains("# hand-written"), "untouched: {after}");
}

/// A `temperature` of 0.2 is an f32 that the TOML serializer widens to f64,
/// printing `0.20000000298023224`. Same number, but it reads as corruption in a
/// file a person edits — and invites a hand "fix" that changes the value.
#[test]
fn a_widened_f32_is_written_back_at_f32_precision() {
    let dir = tempfile::tempdir().unwrap();
    let json = write(
        dir.path(),
        "settings.json",
        r#"{"agent_engine_config": {"agents": {"verifier": {"params": {"temperature": 0.2}}}}}"#,
    );
    let toml_path = dir.path().join("stella.toml");
    super::migrate::migrate_scope(&json, &toml_path, ConfigScope::User, false).unwrap();
    let rendered = std::fs::read_to_string(&toml_path).unwrap();
    assert!(rendered.contains("temperature = 0.2"), "{rendered}");
    assert!(!rendered.contains("0.20000000"), "{rendered}");
}

/// `rrf_k` is a genuine f64 whose value happens to be f32-exact. Narrowing it
/// must not drop the decimal point — `60` would parse back as an integer and
/// serde would reject the type.
#[test]
fn a_whole_number_float_keeps_its_decimal_point() {
    let dir = tempfile::tempdir().unwrap();
    let json = write(
        dir.path(),
        "settings.json",
        r#"{"context": {"retrieval": {"rrf_k": 60.0}}}"#,
    );
    let toml_path = dir.path().join("stella.toml");
    super::migrate::migrate_scope(&json, &toml_path, ConfigScope::User, false).unwrap();
    let rendered = std::fs::read_to_string(&toml_path).unwrap();
    assert!(rendered.contains("rrf_k = 60.0"), "{rendered}");
    // And it still loads as a float.
    let after = load_toml(&toml_path, ConfigScope::User).unwrap();
    assert_eq!(after.context.unwrap().retrieval.rrf_k, 60.0);
}

/// Nested structs must render as headered tables, not one enormous inline
/// table — otherwise the generated file looks nothing like the documented
/// shape and teaches the wrong thing to whoever edits it next.
#[test]
fn nested_sections_render_as_tables_and_parents_stay_implicit() {
    let dir = tempfile::tempdir().unwrap();
    let json = write(dir.path(), "settings.json", RICH_SETTINGS);
    let toml_path = dir.path().join("stella.toml");
    super::migrate::migrate_scope(&json, &toml_path, ConfigScope::User, false).unwrap();
    let rendered = std::fs::read_to_string(&toml_path).unwrap();

    assert!(rendered.contains("[providers.anthropic]"), "{rendered}");
    assert!(rendered.contains("[agents.verifier.params]"), "{rendered}");
    assert!(rendered.contains("[[hooks.PreToolUse]]"), "{rendered}");
    assert!(
        !rendered.contains("{ "),
        "no inline tables anywhere:\n{rendered}"
    );
    // A table holding only sub-tables emits no header of its own.
    for empty_parent in ["\n[providers]\n", "\n[context]\n", "\n[hooks]\n"] {
        assert!(
            !rendered.contains(empty_parent),
            "no bare {empty_parent:?} header:\n{rendered}"
        );
    }
}

#[test]
fn the_generated_file_declares_its_own_scope_and_version() {
    let dir = tempfile::tempdir().unwrap();
    let json = write(dir.path(), "settings.json", RICH_SETTINGS);
    let toml_path = dir.path().join("stella.toml");
    super::migrate::migrate_scope(&json, &toml_path, ConfigScope::Project, false).unwrap();
    let rendered = std::fs::read_to_string(&toml_path).unwrap();
    assert!(rendered.contains("schema_version = 1"), "{rendered}");
    assert!(rendered.contains(r#"scope = "project""#), "{rendered}");
    // And that self-declaration must agree with where it was written.
    assert!(load_toml(&toml_path, ConfigScope::Project).is_ok());
    assert!(load_toml(&toml_path, ConfigScope::User).is_err());
}

/// The generated file must survive the editor that will later modify it.
#[test]
fn a_migrated_file_round_trips_through_a_section_save() {
    let dir = tempfile::tempdir().unwrap();
    let json = write(dir.path(), "settings.json", RICH_SETTINGS);
    let toml_path = dir.path().join("stella.toml");
    super::migrate::migrate_scope(&json, &toml_path, ConfigScope::User, false).unwrap();

    let ui = UiSettings {
        theme: Some("stella-light".to_string()),
    };
    ui.save_to(&toml_path).unwrap();

    let after = load_toml(&toml_path, ConfigScope::User).unwrap();
    assert_eq!(after.ui_theme(), Some("stella-light"));
    // Everything else survived the edit…
    assert_eq!(after.providers.len(), 2);
    assert_eq!(
        after.agent_engine_config.unwrap().default_model.as_deref(),
        Some("anthropic/claude-fable-5")
    );
    // …including the generated header comment.
    let rendered = std::fs::read_to_string(&toml_path).unwrap();
    assert!(
        rendered.contains("# stella.toml — generated by"),
        "{rendered}"
    );
}

/// A project `settings.json` holding an inline `api_key`.
const PROJECT_SETTINGS_WITH_SECRET: &str = r#"{
  "providers": {
    "anthropic": {"api_key": "sk-secret", "default_model": "claude-fable-5"}
  }
}"#;

/// The security regression. `render` serializes `providers` verbatim and the
/// loader refuses an inline `api_key` at the project scope, so a migration that
/// skipped that rule would write the secret into the committed repo-root file
/// AND produce a config stella then refuses to load. Both halves are asserted:
/// the refusal, and that nothing reached disk.
#[test]
fn migrating_an_inline_api_key_to_project_scope_is_refused_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let json = write(dir.path(), "settings.json", PROJECT_SETTINGS_WITH_SECRET);
    let toml_path = dir.path().join("stella.toml");

    let err = super::migrate::migrate_scope(&json, &toml_path, ConfigScope::Project, false)
        .expect_err("a committed file must not receive a literal secret");

    // The secret never reaches the filesystem — not even partially written.
    assert!(
        !toml_path.exists(),
        "refusal must leave no file behind: {}",
        toml_path.display()
    );
    // Same two alternatives the loader names, so the advice cannot diverge.
    assert!(err.contains("api_key_env"), "{err}");
    assert!(err.contains("credentials.toml"), "{err}");
    // And the file the user has to actually edit, which is the JSON — the
    // stella.toml the loader's message names does not exist yet.
    assert!(err.contains("settings.json"), "names the source: {err}");
    // The secret itself is never echoed, so the error stays safe to paste.
    assert!(!err.contains("sk-secret"), "the key is not echoed: {err}");
}

/// Why the check above is load-bearing rather than defensive: `render` is a
/// pure serializer with no opinion about secrets, and `api_key` is
/// `skip_serializing_if = "Option::is_none"`, so a key that is present IS
/// written. This pins that — if `render` ever stopped emitting it, the guard
/// would silently become untested rather than unnecessary.
#[test]
fn render_itself_would_write_the_secret_if_nothing_stopped_it() {
    let dir = tempfile::tempdir().unwrap();
    let json = write(dir.path(), "settings.json", PROJECT_SETTINGS_WITH_SECRET);
    let settings = Settings::load_scope(&json).unwrap();

    let rendered = super::migrate::render(&settings, ConfigScope::Project).unwrap();

    assert!(
        rendered.contains("sk-secret"),
        "render emits the key verbatim; only the guard keeps it off disk:\n{rendered}"
    );
}

/// `--dry-run` reports what a real run WOULD do, so it has to fail here too. A
/// dry run that printed "would write" for a config the real run refuses would
/// be the one output the user trusts before committing.
#[test]
fn a_dry_run_refuses_the_same_project_scope_secret() {
    let dir = tempfile::tempdir().unwrap();
    let json = write(dir.path(), "settings.json", PROJECT_SETTINGS_WITH_SECRET);
    let toml_path = dir.path().join("stella.toml");

    assert!(super::migrate::migrate_scope(&json, &toml_path, ConfigScope::Project, true).is_err());
    assert!(!toml_path.exists());
}

/// The rule is scoped to the committed file, not to secrets in general.
/// `~/.stella/stella.toml` is private and 0600, and the loader accepts an
/// inline key there — so migration must keep working, or users with a
/// perfectly legal user-scope key could never migrate at all.
#[test]
fn migrating_an_inline_api_key_to_user_scope_still_works() {
    let dir = tempfile::tempdir().unwrap();
    let json = write(dir.path(), "settings.json", PROJECT_SETTINGS_WITH_SECRET);
    let toml_path = dir.path().join("stella.toml");

    super::migrate::migrate_scope(&json, &toml_path, ConfigScope::User, false).unwrap();

    let settings = load_toml(&toml_path, ConfigScope::User).unwrap();
    assert_eq!(
        settings.providers["anthropic"].api_key.as_deref(),
        Some("sk-secret")
    );
}

/// The general invariant the specific tests above are instances of: whatever
/// migration writes, the loader can read back at the same scope.
#[test]
fn a_migration_that_succeeds_always_produces_a_loadable_file() {
    for (scope, body) in [
        (ConfigScope::User, PROJECT_SETTINGS_WITH_SECRET),
        (ConfigScope::Project, PROJECT_SETTINGS_WITH_SECRET),
        (ConfigScope::User, RICH_SETTINGS),
        (ConfigScope::Project, RICH_SETTINGS),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let json = write(dir.path(), "settings.json", body);
        let toml_path = dir.path().join("stella.toml");

        if super::migrate::migrate_scope(&json, &toml_path, scope, false).is_err() {
            // A refusal is a valid outcome; it just must not have written.
            assert!(!toml_path.exists(), "{scope:?} refused but wrote a file");
            continue;
        }
        load_toml(&toml_path, scope)
            .unwrap_or_else(|e| panic!("{scope:?} migrated to an unloadable file: {e}"));
    }
}

/// The engine config splits across two sections, so clearing it has to remove
/// both — a stale `[models]` left behind would keep narrowing the vocabulary
/// after the user cleared the setting that created it.
#[test]
fn saving_an_empty_engine_config_removes_both_of_its_sections() {
    let dir = tempfile::tempdir().unwrap();
    let json = write(dir.path(), "settings.json", RICH_SETTINGS);
    let toml_path = dir.path().join("stella.toml");
    super::migrate::migrate_scope(&json, &toml_path, ConfigScope::User, false).unwrap();
    assert!(
        std::fs::read_to_string(&toml_path)
            .unwrap()
            .contains("[models]")
    );

    AgentEngineConfig::default().save_to(&toml_path).unwrap();

    let rendered = std::fs::read_to_string(&toml_path).unwrap();
    assert!(!rendered.contains("[models]"), "{rendered}");
    assert!(!rendered.contains("[agents]"), "{rendered}");
    assert!(
        rendered.contains("[providers.anthropic]"),
        "siblings kept:\n{rendered}"
    );
}
