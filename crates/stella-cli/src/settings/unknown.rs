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
//!
//! # Retired keys are not typos
//!
//! A key that shipped in a release and was then removed lands in the same
//! bucket as `"enable_recapp"` — nothing reads it — but "check the spelling" is
//! false advice for it: the operator spelled a real key correctly, and the
//! feature behind it is gone. [`RETIRED`] is the closed list of those, each
//! with the one sentence that makes the difference actionable, and
//! [`retirement`] is what the caller consults to say the right thing.
//!
//! Deliberately separate from the field lists above rather than a variant
//! inside them. Those lists are also the trusted launcher's strict vocabulary
//! (`config::trusted_engine_config_shape_is_strict`), which fails **closed** —
//! a benchmark posture naming a retired knob must be refused at launch, not
//! warned about and run.

use std::path::Path;

use serde_json::Value;

/// Top-level `settings.json` keys the merged [`super::Settings`] reads.
///
/// Checked against the struct in both directions by
/// [`super::completeness`] — a missing entry reports a correct key as a typo,
/// a stale one silences a real typo.
pub(super) const ROOT_FIELDS: &[&str] = &[
    "providers",
    "hooks",
    "mcp",
    "agent_engine_config",
    "tools",
    "enable_recap",
    "trace_capture",
    "ignore_gitignore",
    "create_worktrees",
    "ui",
    "reward",
    "context",
    "context_providers",
    "authority",
    "enterprise_telemetry",
];

/// `providers.<id>` — [`super::ProviderSettings`]. Checked against the struct
/// in both directions by [`super::completeness`].
pub(super) const PROVIDER_FIELDS: &[&str] = &[
    "id",
    "name",
    "base_url",
    "api_key",
    "api_key_env",
    "default_model",
    "upstream_pin",
    "dialect",
    "cache_ttl",
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
    "per_step",
    "per_usd",
    "per_revision",
];

/// Keys that were correct in a shipped release and read nothing now, each with
/// the sentence an operator needs: what it used to do, and why there is no
/// replacement to point them at.
///
/// Both entries are the same removal seen from two angles — the pipeline stopped
/// asking a model for a verdict, so neither the knob that demanded an
/// independent verifier nor the weight that priced its opinion has anything
/// left to steer. A key belongs here only while a settings file in the wild
/// plausibly still carries it; the list is meant to be pruned, not grown.
///
/// Paths are dotted and format-specific, because the two documents name the
/// same knob differently: `agent_engine_config` in JSON is `agents` in TOML.
const RETIRED: &[(&str, &str)] = &[
    (
        "agent_engine_config.pipeline_require_independent_verifier",
        "refused a run whose verdict call would resolve to the worker's own \
         model; verification makes no model call, so there is no self-graded \
         verdict left to refuse",
    ),
    (
        "agents.pipeline_require_independent_verifier",
        "refused a run whose verdict call would resolve to the worker's own \
         model; verification makes no model call, so there is no self-graded \
         verdict left to refuse",
    ),
    (
        "reward.verifier_weight",
        "scaled a model verifier's opinion against a test's observation; no \
         rung carries that magnitude any more, so a reward label is priced by \
         `deterministic_weight` alone",
    ),
];

/// Why `key` is no longer read, when it is a retired key rather than a typo.
///
/// `key` is one of the dotted paths [`unknown_keys_in`] and
/// [`unknown_toml_keys_in`] return; anything else — including a genuine
/// misspelling — is `None`.
pub(super) fn retirement(key: &str) -> Option<&'static str> {
    RETIRED
        .iter()
        .find_map(|(retired, why)| (*retired == key).then_some(*why))
}

/// The lines to print for the keys `found` in the file at `path`, in the order
/// they should appear. Empty when there is nothing to say.
///
/// A pure function over owned data rather than a `for` loop around `eprintln!`
/// at the call site, so the split — a typo wants re-spelling, a retired key is
/// spelled right — is a thing a test can read. The caller's only job is to
/// print what comes back.
pub(super) fn notices(path: &str, found: Vec<String>) -> Vec<String> {
    let (retired, unknown): (Vec<_>, Vec<_>) =
        found.into_iter().partition(|key| retirement(key).is_some());

    let mut lines = Vec::new();
    if !unknown.is_empty() {
        lines.push(format!(
            "  ! {path}: unrecognized key{} ignored ({}) — check the spelling; \
             stella reads none of them",
            if unknown.len() == 1 { "" } else { "s" },
            unknown
                .iter()
                .map(|key| key.escape_debug().to_string())
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    // One line each rather than a joined list: the whole value of this notice
    // is the reason, and reasons do not join.
    lines.extend(retired.iter().map(|key| {
        format!(
            "  ! {path}: `{}` is retired and reads nothing — it {}. Delete the key.",
            key.escape_debug(),
            retirement(key).unwrap_or_default(),
        )
    }));
    lines
}

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
    "pipeline_research_model",
    "pipeline_plan_model",
    "allowed_models",
    "model_output_caps",
    "auto_mode",
    "effort_auto",
    "reasoning_auto",
    "headless_scope_bypass",
    "pipeline_max_revisions",
    "pipeline_candidates",
    "pipeline_verifier_evidence_demand",
    "pipeline_require_diff_coverage",
    "model_timeout_secs",
    "compaction_budget_tokens",
    "tool_result_horizon_steps",
    "approval_wait_secs",
    "agents",
];

/// `agent_engine_config.agents` — [`super::AgentEngineAgents`].
pub(crate) const ENGINE_AGENT_NAMES: &[&str] = &[
    "default", "worker", "verifier", "triage", "research", "plan",
];

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
const RUN_FIELDS: &[&str] = &[
    "recap",
    "trace_capture",
    "create_worktrees",
    "ignore_gitignore",
];
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
    "pipeline_research_model",
    "pipeline_plan_model",
    "auto_mode",
    "effort_auto",
    "reasoning_auto",
    "headless_scope_bypass",
    "pipeline_max_revisions",
    "pipeline_candidates",
    "pipeline_verifier_evidence_demand",
    "pipeline_require_diff_coverage",
    "default",
    "worker",
    "verifier",
    "triage",
    "research",
    "plan",
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

    /// Order is structural, not alphabetical: [`scan_engine`] sweeps the agent
    /// map for unknown NAMES in one pass before descending into the known ones,
    /// so a misspelled agent is always reported ahead of any field typo inside
    /// a correctly-spelled sibling — whatever the two happen to sort as.
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

    /// A **retired** key is reported by exactly the same machinery as a typo,
    /// and that is the point (#2616). Both `reward.verifier_weight` and
    /// `agent_engine_config.pipeline_require_independent_verifier` steered a
    /// verdict call that no longer runs. A retired key kept in the typed struct
    /// is worse than an unknown one — it parses, merges across all three
    /// scopes, and configures nothing, with no output whatsoever — so retiring
    /// it means deleting the field, not keeping a no-op.
    ///
    /// The file still LOADS: this pass warns, it never gates, which is what
    /// lets a settings file written against an older release keep working.
    #[test]
    fn the_retired_verdict_keys_are_named() {
        let found = scan(
            r#"{
                 "reward": { "deterministic_weight": 1.0, "verifier_weight": 0.3 },
                 "agent_engine_config": {
                   "pipeline_require_diff_coverage": "on",
                   "pipeline_require_independent_verifier": "on"
                 }
               }"#,
        );
        assert_eq!(
            found,
            vec![
                "agent_engine_config.pipeline_require_independent_verifier".to_string(),
                "reward.verifier_weight".to_string(),
            ],
            "a retired key must be named, and the keys beside it left alone"
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
