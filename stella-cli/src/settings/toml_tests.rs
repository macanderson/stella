//! Tests for the `stella.toml` format port.
//!
//! The load-bearing one is
//! [`a_toml_config_and_its_json_equivalent_produce_identical_settings`]: the
//! whole premise of Phase 1 is that the port changes SPELLING and nothing else.
//! If that test can be made to pass only by weakening it, the port has changed
//! behavior and the migration is no longer safe to run unattended.

use super::toml_config::{ConfigScope, TomlConfig, load_toml_scope};
use super::*;

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
            "pipeline_judge_model": "openrouter/openai/gpt-5.5",
            "allowed_models": ["anthropic/claude-fable-5", "zai/glm-5.2"],
            "auto_mode": "off",
            "effort_auto": "on",
            "headless_scope_bypass": "off",
            "agents": {
              "judge": {
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
          "ui": {"theme": "stella-dark"},
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
pipeline_judge_model = "openrouter/openai/gpt-5.5"
auto_mode = "off"
effort_auto = "on"
headless_scope_bypass = "off"

[agents.judge]
provider = "openrouter"
model = "openai/gpt-5.5"
effort = "high"
reasoning = "on"

[agents.judge.params]
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
    assert_eq!(from_json.ui, from_toml.ui, "ui");
    assert_eq!(from_json.mcp, from_toml.mcp, "mcp");
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
    let path = write(dir.path(), "stella.toml", "[ui]\ntheme = \"stella-light\"\n");
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
    assert!(err.contains("api_key_env"), "names the env alternative: {err}");
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
    let loaded = load_toml_scope(&path, ConfigScope::Project, |p| std::fs::read_to_string(p)).unwrap();
    assert_eq!(loaded.mcp.servers.len(), 1, "still parsed");
    assert_eq!(loaded.warnings.len(), 1, "and announced");
    assert!(loaded.warnings[0].contains("does not read yet"), "{:?}", loaded.warnings);
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

[agents.judge]
effort = "high"

[agents.judge.params]
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
pipeline_judge_model = "e/f"
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
    assert_eq!(engine.pipeline_judge_model.as_deref(), Some("e/f"));
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
