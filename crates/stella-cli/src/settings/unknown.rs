//! Unrecognized-key detection for `settings.json`.
//!
//! Every settings type in this crate is deliberately *tolerant* of unknown
//! fields — `#[serde(deny_unknown_fields)]` would turn a forward-compatible
//! key written by a newer stella into a hard launch failure on an older one,
//! which is the wrong trade for a file three scopes deep in a user's home
//! directory. The cost of that tolerance is that a **typo** is indistinguishable
//! from a future key: `"provider"` for `"providers"`, `"enable_recapp"` for
//! `"enable_recap"`, `"default_modle"` for `"default_model"` all parsed
//! successfully and configured exactly nothing, with no output whatsoever.
//!
//! So this module keeps the parse tolerant and adds the missing half: a
//! *warning*. It walks the raw JSON of a scope file against the same closed
//! key sets the typed structs declare and reports the keys nothing will ever
//! read — the posture `cargo` takes with "unused manifest key". Never an
//! error, never a launch gate.
//!
//! Only genuinely CLOSED objects are checked. `tools`, `providers`,
//! `context_providers` and the hook matcher lists are open maps whose keys are
//! user-chosen names (a tool name, a provider id), so an unrecognized key there
//! is data, not a mistake — those are descended into, not flagged.

use std::path::Path;

use serde_json::Value;

/// Top-level `settings.json` keys the merged [`super::Settings`] reads.
const ROOT_FIELDS: &[&str] = &[
    "providers",
    "hooks",
    "mcp",
    "agent_engine_config",
    "tools",
    "enable_recap",
    "trace_capture",
    "create_worktrees",
    "ui",
    "reward",
    "context",
    "context_providers",
    "authority",
    "enterprise_telemetry",
];

/// `providers.<id>` — [`super::ProviderSettings`].
const PROVIDER_FIELDS: &[&str] = &[
    "id",
    "name",
    "base_url",
    "api_key",
    "api_key_env",
    "default_model",
    "dialect",
];

/// `mcp` — [`super::McpSettings`].
const MCP_FIELDS: &[&str] = &["registry_url"];

/// `ui` — [`super::UiSettings`].
const UI_FIELDS: &[&str] = &["theme"];

/// `reward` — [`super::RewardSettings`]. Closed: a mistyped weight key is the
/// exact failure this walker exists for, because the typo and the correct key
/// produce identically-shaped labels that differ only in a number nobody reads
/// until they pool the traces.
const REWARD_FIELDS: &[&str] = &[
    "deterministic_weight",
    "verifier_weight",
    "per_step",
    "per_usd",
    "per_revision",
];

/// `hooks` — the PascalCase lifecycle-event keys `stella_core::hooks::Hooks`
/// renames its fields to. A misspelled event name is the highest-consequence
/// typo in the whole file: the hook silently never runs.
const HOOK_EVENTS: &[&str] = &["SessionStart", "PreToolUse", "PostToolUse"];

/// `agent_engine_config` — [`super::AgentEngineConfig`].
///
/// Shared with `config.rs`'s trusted-launcher strictness gate
/// (`STELLA_ENGINE_CONFIG_JSON`), which needs the identical vocabulary: two
/// hand-maintained copies of the same field list would drift the moment a
/// knob is added, and the launcher seam fails *closed* on a name it does not
/// know — so a drifted copy there is a refused benchmark run.
pub(crate) const ENGINE_ROOT_FIELDS: &[&str] = &[
    "default_model",
    "pipeline_verifier_model",
    "pipeline_worker_model",
    "pipeline_triage_model",
    "allowed_models",
    "model_output_caps",
    "auto_mode",
    "effort_auto",
    "reasoning_auto",
    "headless_scope_bypass",
    "hunk_review",
    "pipeline_max_revisions",
    "pipeline_candidates",
    "pipeline_verifier_evidence_demand",
    "pipeline_require_diff_coverage",
    "model_timeout_secs",
    "compaction_budget_tokens",
    "tool_result_horizon_steps",
    "agents",
];

/// `agent_engine_config.agents` — [`super::AgentEngineAgents`].
pub(crate) const ENGINE_AGENT_NAMES: &[&str] = &["default", "worker", "verifier", "triage"];

/// `agent_engine_config.agents.<kind>` — [`super::AgentEngineAgent`].
pub(crate) const ENGINE_AGENT_FIELDS: &[&str] = &[
    "model",
    "provider",
    "prompt",
    "effort",
    "reasoning",
    "params",
];

/// `agent_engine_config.agents.<kind>.params` — [`super::AgentEngineParams`].
pub(crate) const ENGINE_PARAM_FIELDS: &[&str] = &[
    "temperature",
    "top_p",
    "top_k",
    "frequency_penalty",
    "presence_penalty",
    "repetition_penalty",
    "max_tokens",
    "seed",
    "verbosity",
    "service_tier",
];

/// The unrecognized keys in the settings file at `path`, in `a.b.c` dotted
/// form, sorted and deduplicated. An absent file, an unreadable one, or one
/// that is not valid JSON yields nothing: the typed load is the authority on
/// those failures and already reports them by name — this pass must never
/// invent a second, worse-worded diagnosis for a problem that already has one.
pub(super) fn unknown_keys_in(path: &Path) -> Vec<String> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(root) = serde_json::from_str::<Value>(&contents) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    scan_root(&root, &mut found);
    found.sort();
    found.dedup();
    found
}

/// Top-level `stella.toml` keys. Deliberately a SEPARATE list from
/// [`ROOT_FIELDS`]: the TOML document renames three of them
/// (`agent_engine_config` → `agents`, `enable_recap` → `run.recap`,
/// `allowed_models` → `models.allowed`) and adds `meta`. Sharing one list
/// would make a JSON-only key look valid in TOML and vice versa, which is the
/// exact confusion this pass exists to prevent.
const TOML_ROOT_FIELDS: &[&str] = &[
    "meta",
    "run",
    "providers",
    "models",
    "agents",
    "tools",
    "hooks",
    "mcp",
    "context",
    "context_providers",
    "ui",
    "reward",
    "authority",
    "enterprise_telemetry",
];

const META_FIELDS: &[&str] = &["schema_version", "scope"];
const RUN_FIELDS: &[&str] = &["recap", "trace_capture", "create_worktrees"];
const MODELS_FIELDS: &[&str] = &["allowed", "output_caps"];
const TOML_MCP_FIELDS: &[&str] = &["registry_url", "servers"];

/// `[agents]` — the flat engine fields plus the four agent tables, which live
/// in the same table because the TOML shape flattens
/// `agent_engine_config.agents.<name>` up one level.
const TOML_AGENTS_FIELDS: &[&str] = &[
    "default_model",
    "pipeline_verifier_model",
    "pipeline_worker_model",
    "pipeline_triage_model",
    "auto_mode",
    "effort_auto",
    "reasoning_auto",
    "headless_scope_bypass",
    "hunk_review",
    "pipeline_max_revisions",
    "pipeline_candidates",
    "pipeline_verifier_evidence_demand",
    "pipeline_require_diff_coverage",
    "default",
    "worker",
    "verifier",
    "triage",
];

/// The unrecognized keys in the `stella.toml` at `path`.
///
/// Converts to `serde_json::Value` and reuses the same [`closed`] walker rather
/// than growing a second traversal over `toml::Value`. Two walkers over two
/// value types would drift the first time a nested block gained a field, and a
/// drifted walker fails SILENTLY — it stops warning about the very typo it
/// exists to catch.
pub(super) fn unknown_toml_keys_in(path: &Path) -> Vec<String> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(parsed) = toml::from_str::<toml::Value>(&contents) else {
        return Vec::new();
    };
    let Ok(root) = serde_json::to_value(parsed) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    scan_toml_root(&root, &mut found);
    found.sort();
    found.dedup();
    found
}

fn scan_toml_root(root: &Value, found: &mut Vec<String>) {
    let Some(object) = root.as_object() else {
        return;
    };
    for (key, value) in object {
        if !TOML_ROOT_FIELDS.contains(&key.as_str()) {
            found.push(key.clone());
            continue;
        }
        match key.as_str() {
            "meta" => closed("meta", value, META_FIELDS, found),
            "run" => closed("run", value, RUN_FIELDS, found),
            "models" => closed("models", value, MODELS_FIELDS, found),
            "mcp" => closed("mcp", value, TOML_MCP_FIELDS, found),
            "ui" => closed("ui", value, UI_FIELDS, found),
            "reward" => closed("reward", value, REWARD_FIELDS, found),
            "hooks" => closed("hooks", value, HOOK_EVENTS, found),
            "providers" => {
                if let Some(entries) = value.as_object() {
                    for (id, entry) in entries {
                        closed(&format!("providers.{id}"), entry, PROVIDER_FIELDS, found);
                    }
                }
            }
            "agents" => scan_toml_agents(value, found),
            // `tools`, `context_providers`, `context`, `authority`, and
            // `enterprise_telemetry` are open maps or types owned elsewhere —
            // same treatment as the JSON walker gives them.
            _ => {}
        }
    }
}

fn scan_toml_agents(agents: &Value, found: &mut Vec<String>) {
    closed("agents", agents, TOML_AGENTS_FIELDS, found);
    let Some(map) = agents.as_object() else {
        return;
    };
    for name in ENGINE_AGENT_NAMES {
        let Some(agent) = map.get(*name) else {
            continue;
        };
        let prefix = format!("agents.{name}");
        closed(&prefix, agent, ENGINE_AGENT_FIELDS, found);
        if let Some(params) = agent.get("params") {
            closed(
                &format!("{prefix}.params"),
                params,
                ENGINE_PARAM_FIELDS,
                found,
            );
        }
    }
}

/// Flag every key of `value` (when it is an object) outside `allowed`,
/// prefixed with `prefix`.
fn closed(prefix: &str, value: &Value, allowed: &[&str], found: &mut Vec<String>) {
    let Some(object) = value.as_object() else {
        return;
    };
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            found.push(join(prefix, key));
        }
    }
}

fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

fn scan_root(root: &Value, found: &mut Vec<String>) {
    let Some(object) = root.as_object() else {
        return;
    };
    for (key, value) in object {
        if !ROOT_FIELDS.contains(&key.as_str()) {
            found.push(key.clone());
            continue;
        }
        match key.as_str() {
            // An open map of provider ids; each ENTRY is closed.
            "providers" => {
                if let Some(entries) = value.as_object() {
                    for (id, entry) in entries {
                        closed(&format!("providers.{id}"), entry, PROVIDER_FIELDS, found);
                    }
                }
            }
            "mcp" => closed("mcp", value, MCP_FIELDS, found),
            "ui" => closed("ui", value, UI_FIELDS, found),
            "reward" => closed("reward", value, REWARD_FIELDS, found),
            "hooks" => closed("hooks", value, HOOK_EVENTS, found),
            "agent_engine_config" => scan_engine(value, found),
            // `tools`, `context_providers`, `context`, `authority`, and
            // `enterprise_telemetry` are open maps or types owned elsewhere.
            _ => {}
        }
    }
}

fn scan_engine(engine: &Value, found: &mut Vec<String>) {
    const PREFIX: &str = "agent_engine_config";
    closed(PREFIX, engine, ENGINE_ROOT_FIELDS, found);
    let Some(agents) = engine.get("agents") else {
        return;
    };
    let agents_prefix = format!("{PREFIX}.agents");
    closed(&agents_prefix, agents, ENGINE_AGENT_NAMES, found);
    let Some(map) = agents.as_object() else {
        return;
    };
    for (name, agent) in map {
        if !ENGINE_AGENT_NAMES.contains(&name.as_str()) {
            continue; // already reported as an unknown agent name
        }
        let agent_prefix = join(&agents_prefix, name);
        closed(&agent_prefix, agent, ENGINE_AGENT_FIELDS, found);
        if let Some(params) = agent.get("params") {
            closed(
                &join(&agent_prefix, "params"),
                params,
                ENGINE_PARAM_FIELDS,
                found,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(json: &str) -> Vec<String> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, json).unwrap();
        unknown_keys_in(&path)
    }

    /// The witness. Each of these typos parsed cleanly and configured
    /// absolutely nothing, with no output at all — the singular property of
    /// this whole module is that they now have a name.
    #[test]
    fn the_four_silent_typos_are_named() {
        let found = scan(
            r#"{
                 "provider": { "zai": { "api_key": "x" } },
                 "enable_recapp": "on",
                 "toolz": { "bash": "off" },
                 "agent_engine_config": { "default_modle": "zai/glm-5.2" }
               }"#,
        );
        assert_eq!(
            found,
            vec![
                "agent_engine_config.default_modle".to_string(),
                "enable_recapp".to_string(),
                "provider".to_string(),
                "toolz".to_string(),
            ]
        );
    }

    #[test]
    fn a_fully_valid_file_is_silent() {
        let found = scan(
            r#"{
                 "providers": { "zai": { "base_url": "https://x", "api_key_env": "K" } },
                 "hooks": { "PreToolUse": [{ "matcher": "bash", "hooks": [] }] },
                 "mcp": { "registry_url": "https://r" },
                 "ui": { "theme": "stella-dark" },
                 "tools": { "bash": "off", "some-mcp-server__thing": "off" },
                 "enable_recap": "on",
                 "agent_engine_config": {
                   "default_model": "zai/glm-5.2",
                   "agents": { "verifier": { "provider": "openrouter",
                                          "params": { "temperature": 0.2 } } }
                 }
               }"#,
        );
        assert!(found.is_empty(), "{found:?}");
    }

    /// The open maps must stay open: a tool name, a provider id, and a
    /// context-provider id are user-chosen data, not schema.
    #[test]
    fn open_maps_are_descended_into_not_flagged() {
        let found = scan(
            r#"{
                 "tools": { "anything_at_all": "off" },
                 "providers": { "my-private-gateway": { "base_url": "https://x" } },
                 "context_providers": { "whatever": { "enabled": true } }
               }"#,
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_misspelled_hook_event_is_named() {
        // The costliest typo in the file: the hook simply never runs.
        let found = scan(r#"{ "hooks": { "preToolUse": [] } }"#);
        assert_eq!(found, vec!["hooks.preToolUse".to_string()]);
    }

    #[test]
    fn nested_engine_typos_carry_their_full_path() {
        let found = scan(
            r#"{ "agent_engine_config": {
                   "agents": { "verifier": { "modell": "x", "params": { "temperatur": 1 } },
                               "verifer": { "model": "y" } } } }"#,
        );
        assert_eq!(
            found,
            vec![
                "agent_engine_config.agents.verifer".to_string(),
                "agent_engine_config.agents.verifier.modell".to_string(),
                "agent_engine_config.agents.verifier.params.temperatur".to_string(),
            ]
        );
    }

    /// A malformed or missing file is the typed loader's problem, and it
    /// already names it precisely. Reporting a second, vaguer complaint here
    /// would only add noise to a failure the user is already looking at.
    #[test]
    fn malformed_and_missing_files_yield_nothing() {
        assert!(scan("{ not json").is_empty());
        assert!(scan("[]").is_empty());
        assert!(unknown_keys_in(Path::new("/nonexistent/settings.json")).is_empty());
    }
}
