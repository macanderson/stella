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
//! (`config::trusted_engine_config_shape_is_strict`), which fails **closed**.
//!
//! That coupling makes retirement a two-way decision rather than one, and the
//! list has both answers in it:
//!
//! - **Dropped from the field list.** The launcher then refuses a posture
//!   naming the key. Right for a knob that bought something — an arm setting
//!   `pipeline_max_revisions` is being charged for attempts it will not get, so
//!   a refusal is the honest answer.
//! - **Kept in the field list, via [`RETIRED_ENGINE_ROOT`].** The launcher
//!   recognizes it and the walker reports it. Right for #3908's role keys,
//!   which name a *model for a role that no longer exists* and had already been
//!   inert for the whole life of the postures that write them — refusing now
//!   would break every benchmark launch and re-hash every registered digest
//!   without un-spending anything.
//!
//! Both paths report the key by name. The difference is only whether a launch
//! survives it.

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
    "ignore_gitignore",
    "create_worktrees",
    "candidate_isolation",
    "allowed_dirs",
    "ui",
    "reward",
    "context",
    "context_providers",
    "plugins",
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
const UI_FIELDS: &[&str] = &["theme", "mid_turn_prompt"];

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
    // The three knobs whose implementations left with `crates/stella-pipeline`
    // (#3865). All three shipped in releases and are plausibly still set in
    // settings files in the wild, which is exactly what this list is for: the
    // operator spelled a real key correctly and the feature behind it is gone,
    // so "check the spelling" would be false advice. Retired rather than
    // deleted silently (#3870, #3872); what each one did, and what rebuilding
    // it on the raw loop would take, is recorded on its own issue.
    (
        "enable_recap",
        "printed a deterministic end-of-run recap in text mode; the renderer \
         read the staged pipeline's verdict types and went with that crate, so \
         nothing assembles a recap now",
    ),
    (
        "run.recap",
        "printed a deterministic end-of-run recap in text mode; the renderer \
         read the staged pipeline's verdict types and went with that crate, so \
         nothing assembles a recap now",
    ),
    (
        "trace_capture",
        "appended a per-execution trajectory trace to \
         `.stella/private/traces.jsonl`; the module that assembled it was \
         orphaned by the staged pipeline's removal and deleted with it",
    ),
    (
        "run.trace_capture",
        "appended a per-execution trajectory trace to \
         `.stella/private/traces.jsonl`; the module that assembled it was \
         orphaned by the staged pipeline's removal and deleted with it",
    ),
    (
        "agent_engine_config.approval_wait_secs",
        "bounded how long a supervised run's parked scope-review approval \
         waited before aborting itself; the scope-review gate it timed is gone, \
         so there is no park for it to bound",
    ),
    (
        "agents.approval_wait_secs",
        "bounded how long a supervised run's parked scope-review approval \
         waited before aborting itself; the scope-review gate it timed is gone, \
         so there is no park for it to bound",
    ),
];

/// The retired role vocabulary (#3908), as `(dotted path, replacement)`.
///
/// Every entry is generated into [`RETIRED`]'s shape by [`retirement`], in all
/// four spellings the same knob has: `agent_engine_config.<key>` and
/// `agents.<key>` for the flat model keys, and `…agents.<persona>` for the
/// per-persona blocks, because the JSON and TOML documents name the same block
/// differently.
///
/// The replacement column is the whole point. These keys read like
/// capabilities — an operator who set `pipeline_verifier_model` believed they
/// had bought a second model — and the four non-worker ones bought nothing for
/// the whole time the key existed on a workspace without the staged pipeline.
/// Deleting them silently would leave every settings file in the wild still
/// reading that way, so each one names the assignment that does the job now.
const RETIRED_ROLES: &[(&str, &str, &str)] = &[
    (
        "worker",
        "pipeline_worker_model",
        "core has one role and `default_model` is its model — set that instead",
    ),
    (
        "verifier",
        "pipeline_verifier_model",
        "assign the seat the verification plugin declares: \
         `[seats] \"<plugin-id>/verifier\" = \"…\"`",
    ),
    (
        "triage",
        "pipeline_triage_model",
        "assign the seat the triage plugin declares: `[seats] \"<plugin-id>/triage\" = \"…\"`",
    ),
    (
        "research",
        "pipeline_research_model",
        "assign the seat the plugin declaring it names: `[seats] \"<plugin-id>/research\" = \"…\"`",
    ),
    (
        "plan",
        "pipeline_plan_model",
        "assign the seat the planning plugin declares: `[seats] \"<plugin-id>/plan\" = \"…\"`",
    ),
];

/// The retirement sentence for one of #3908's role keys, or `None`.
///
/// Split out of [`retirement`] because these are generated from
/// [`RETIRED_ROLES`] rather than written out: twenty hand-written rows for
/// five knobs in four spellings is four chances to describe the same
/// retirement differently, and the difference would only ever be visible to
/// the one operator whose file used the spelling nobody proofread.
fn role_retirement(key: &str) -> Option<String> {
    let flat = |key: &str| {
        RETIRED_ROLES
            .iter()
            .find(|(_, flat, _)| *flat == key)
            .map(|(_, _, replacement)| {
                format!(
                    "pinned a model for a role the core loop does not have — it was a pin on the \
                     staged pipeline deleted in #3865 and has read nothing since. {replacement}"
                )
            })
    };
    let persona = |name: &str| {
        RETIRED_ROLES
            .iter()
            .find(|(persona, _, _)| *persona == name)
            .map(|(_, _, replacement)| {
                format!(
                    "configured a model, prompt, effort and params for a role the core loop does \
                     not have — the staged pipeline that ran it was deleted in #3865. \
                     {replacement}"
                )
            })
    };

    for prefix in ["agent_engine_config", "agents"] {
        if let Some(rest) = key.strip_prefix(prefix).and_then(|r| r.strip_prefix('.')) {
            if let Some(name) = rest.strip_prefix("agents.") {
                return persona(name);
            }
            // TOML spells the persona blocks as `agents.<persona>` with no
            // second `agents.` segment, so a bare tail is either a flat key or
            // a persona name depending on which list claims it.
            return flat(rest).or_else(|| persona(rest));
        }
    }
    None
}

/// Why `key` is no longer read, when it is a retired key rather than a typo.
///
/// `key` is one of the dotted paths [`unknown_keys_in`] and
/// [`unknown_toml_keys_in`] return; anything else — including a genuine
/// misspelling — is `None`.
pub(super) fn retirement(key: &str) -> Option<String> {
    RETIRED
        .iter()
        .find_map(|(retired, why)| (*retired == key).then(|| (*why).to_string()))
        .or_else(|| role_retirement(key))
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
    "seat_models",
    "allowed_models",
    "model_output_caps",
    "auto_mode",
    "effort_auto",
    "reasoning_auto",
    "headless_scope_bypass",
    "model_timeout_secs",
    "compaction_budget_tokens",
    "tool_result_horizon_steps",
    "agents",
];

/// The `agent_engine_config` keys #3908 retired, still **recognized** by the
/// trusted-launcher seam.
///
/// Deliberately not simply dropped from [`ENGINE_ROOT_FIELDS`], which is the
/// shape the other two retirements above took. That allowlist is shared with
/// `config::trusted_engine_config_shape_is_strict`, which fails **closed**, and
/// `bench/harbor_adapter/stella_harbor/posture.py` and
/// `arenabench/arenabench/harbor_agent.py` still *write* these keys into hashed
/// benchmark postures. Dropping them here would refuse every benchmark launch
/// and force a re-hash of every digest registered in `bench/READINESS.md`
/// §8.4 — the published-numbers decision #3870 reserves for a maintainer.
///
/// The distinction against `pipeline_max_revisions` (removed outright, and the
/// launcher now refuses it) is real and not a softer standard: those knobs
/// bought *attempts*, so a posture naming one is an arm being charged for
/// something it does not receive. These name a *model for a role that no longer
/// exists*, and they have already been inert for the whole life of the posture
/// that writes them — nothing has read `pipeline_verifier_model` since #3865.
/// Refusing them now would not un-spend anything; naming them in every door
/// they pass through is what actually ends the silence. The fail-closed
/// tightening lands with slice 6 (#3910), once the Python stops writing them.
///
/// Recognized, ignored, reported — never silently accepted, and never
/// silently dropped.
pub(crate) const RETIRED_ENGINE_ROOT: &[&str] = &[
    "pipeline_verifier_model",
    "pipeline_worker_model",
    "pipeline_triage_model",
    "pipeline_research_model",
    "pipeline_plan_model",
];

/// `agent_engine_config.agents` — [`super::AgentEngineAgents`]. One key,
/// because core has one role.
pub(crate) const ENGINE_AGENT_NAMES: &[&str] = &["default"];

/// The `agents.<persona>` names #3908 retired, still recognized by the
/// trusted-launcher seam for [`RETIRED_ENGINE_ROOT`]'s reason.
pub(crate) const RETIRED_ENGINE_AGENT_NAMES: &[&str] =
    &["worker", "verifier", "triage", "research", "plan"];

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
pub(super) const TOML_ROOT_FIELDS: &[&str] = &[
    "meta",
    "run",
    "workspace",
    "providers",
    "models",
    "agents",
    // `[seats]`, a top-level section rather than a key under `[agents]`
    // (`toml_config::SeatsSection`). Absent from this list until #3908, which
    // meant a `stella.toml` using the seat plane slice 0 shipped had its
    // `[seats]` table reported as an unrecognized key — a warning telling the
    // operator to check the spelling of a section that was working.
    "seats",
    "tools",
    "hooks",
    "mcp",
    "context",
    "context_providers",
    "ui",
    "reward",
    "authority",
    "enterprise_telemetry",
    // `[self_driving]` and `[issues]` — the autonomous loop's own
    // configuration (`toml_config::SelfDrivingSection`, `IssuesSection`).
    //
    // The identical omission `[seats]` had above, with the identical symptom:
    // a `stella.toml` configuring the loop had its whole block reported as an
    // unrecognized key, advising the operator to check the spelling of a
    // section serde was reading correctly the entire time. Twice is a pattern —
    // a root section added to `TomlConfig` must be added here in the same
    // change, or this walker tells people their working config is a typo.
    "self_driving",
    "issues",
    // `[plugins]` — the per-plugin retraction switches
    // (`TomlConfig::plugins`). The THIRD instance of the omission the two
    // comments above record, and the one that bites hardest: the operator most
    // likely to write this section is the one switching a plugin OFF, and the
    // walker told them the section they used to do it was a typo while the
    // loader was reading it correctly the whole time.
    //
    // Twice was a pattern; three times is a missing guard. See
    // `toml_root_vocabulary_is_total` in `super::completeness`, which now
    // destructures `TomlConfig` exhaustively so a root section added without an
    // entry here stops the crate compiling instead of shipping this warning.
    "plugins",
];

const META_FIELDS: &[&str] = &["schema_version", "scope"];
const RUN_FIELDS: &[&str] = &[
    "candidate_isolation",
    "create_worktrees",
    "ignore_gitignore",
];
/// `[workspace]` — closed, like `[run]`: a mistyped `allowed_dir` grants
/// nothing and looks exactly like a granted directory until a tool refuses a
/// write, which is the failure this walker exists to pre-empt.
const WORKSPACE_FIELDS: &[&str] = &["allowed_dirs"];
const MODELS_FIELDS: &[&str] = &["allowed", "output_caps"];
const TOML_MCP_FIELDS: &[&str] = &["registry_url", "servers"];

/// `[agents]` — the flat engine fields plus the four agent tables, which live
/// in the same table because the TOML shape flattens
/// `agent_engine_config.agents.<name>` up one level.
pub(super) const TOML_AGENTS_FIELDS: &[&str] = &[
    "default_model",
    "auto_mode",
    "effort_auto",
    "reasoning_auto",
    "headless_scope_bypass",
    // The three engine budgets. Present in `ENGINE_ROOT_FIELDS` since they
    // shipped, and absent here until the reference config was written against
    // the struct rather than against this list — so the SAME knob was accepted
    // in `settings.json` and reported as a possible typo in `stella.toml`.
    //
    // That is the worst shape this divergence can take. The two vocabularies
    // are deliberately separate (a JSON-only key must not look valid in TOML
    // and vice versa), which makes every intentional difference load-bearing
    // and every accidental one invisible: nothing distinguishes "renamed on
    // purpose" from "forgotten" except a human reading both lists.
    // `AgentsSection` is now destructured against this one in
    // `super::completeness`, so the compiler makes that distinction instead.
    "model_timeout_secs",
    "compaction_budget_tokens",
    "tool_result_horizon_steps",
    "default",
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
            "workspace" => closed("workspace", value, WORKSPACE_FIELDS, found),
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

    /// Witness for the dead-config removal: the staged pipeline is gone
    /// (#3865) and took its four benchmark-posture knobs with it —
    /// `pipeline_max_revisions`, `pipeline_candidates`,
    /// `pipeline_require_diff_coverage`, `pipeline_verifier_evidence_demand`
    /// all had zero behavioral consumers left in `AgentEngineConfig`, so a
    /// deployment setting one silently no-opped instead of being told the
    /// knob does nothing. Deleting the fields (rather than moving them to
    /// [`RETIRED`]) means they now fall through to the same unknown-key path
    /// as any other typo — this is the honest behavior for a benchmark arm
    /// that measured nothing.
    #[test]
    fn the_dead_pipeline_posture_keys_are_named_unknown() {
        let found = scan(r#"{ "agent_engine_config": { "pipeline_max_revisions": 4 } }"#);
        assert_eq!(
            found,
            vec!["agent_engine_config.pipeline_max_revisions".to_string()]
        );
    }

    /// **Witness (#3908), settings-file half.** A file carrying every retired
    /// role key loads, and every one of them is reported BY NAME with the
    /// assignment that replaces it — never as a typo, and never in silence.
    ///
    /// The naming is the whole deliverable. These keys read like capabilities:
    /// an operator who wrote `pipeline_verifier_model` believed they had bought
    /// a second model, and for the entire life of the key on a workspace
    /// without the staged pipeline they had not. Deleting them quietly would
    /// leave every settings file in the wild still reading that way, so the
    /// notice has to say what the key did, that it does nothing, and what to
    /// write instead.
    #[test]
    fn every_retired_role_key_is_reported_by_name_with_its_replacement() {
        let found = scan(
            r#"{ "agent_engine_config": {
                   "default_model": "zai/glm-5.2",
                   "pipeline_worker_model": "a/b",
                   "pipeline_verifier_model": "c/d",
                   "pipeline_triage_model": "e/f",
                   "pipeline_research_model": "g/h",
                   "pipeline_plan_model": "i/j",
                   "agents": { "default": {}, "worker": {}, "verifier": {},
                               "triage": {}, "research": {}, "plan": {} } } }"#,
        );

        // Every retired key is found — the flat five and the persona five.
        for key in [
            "agent_engine_config.pipeline_worker_model",
            "agent_engine_config.pipeline_verifier_model",
            "agent_engine_config.pipeline_triage_model",
            "agent_engine_config.pipeline_research_model",
            "agent_engine_config.pipeline_plan_model",
            "agent_engine_config.agents.worker",
            "agent_engine_config.agents.verifier",
            "agent_engine_config.agents.triage",
            "agent_engine_config.agents.research",
            "agent_engine_config.agents.plan",
        ] {
            assert!(
                found.contains(&key.to_string()),
                "{key} not found: {found:?}"
            );
            assert!(
                retirement(key).is_some(),
                "{key} must carry a retirement reason, not read as a typo"
            );
        }
        // ...and the two live keys are not.
        assert!(!found.contains(&"agent_engine_config.default_model".to_string()));
        assert!(!found.contains(&"agent_engine_config.agents.default".to_string()));

        // Every line is a retirement, none is a spelling complaint, and each
        // one names the assignment that replaces it.
        let lines = notices("settings.json", found);
        assert_eq!(lines.len(), 10, "one line per retired key: {lines:#?}");
        for line in &lines {
            assert!(
                line.contains("is retired and reads nothing"),
                "not phrased as a retirement: {line}"
            );
            assert!(
                !line.contains("check the spelling"),
                "a correctly-spelled retired key must never be called a typo: {line}"
            );
        }
        let joined = lines.join("\n");
        assert!(
            joined.contains("`default_model`"),
            "the worker's replacement is the one model core ships: {joined}"
        );
        assert!(
            joined.contains("[seats]"),
            "the other four point at the seat plane: {joined}"
        );
    }

    /// The same keys spelled the way `stella.toml` spells them. The two
    /// documents name the same block differently (`agent_engine_config` in
    /// JSON is `agents` in TOML), so a retirement that only knew one spelling
    /// would go silent for half the users who have it.
    #[test]
    fn the_toml_spelling_of_a_retired_role_key_is_reported_too() {
        for key in [
            "agents.pipeline_verifier_model",
            "agents.verifier",
            "agents.plan",
        ] {
            assert!(
                retirement(key).is_some(),
                "{key} must be recognized in its TOML spelling"
            );
        }
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
                 "ignore_gitignore": "on",
                 "agent_engine_config": {
                   "default_model": "zai/glm-5.2",
                   "agents": { "default": { "provider": "openrouter",
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
                   "agents": { "default": { "modell": "x", "params": { "temperatur": 1 } },
                               "defalt": { "model": "y" } } } }"#,
        );
        assert_eq!(
            found,
            vec![
                "agent_engine_config.agents.defalt".to_string(),
                "agent_engine_config.agents.default.modell".to_string(),
                "agent_engine_config.agents.default.params.temperatur".to_string(),
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
                   "model_timeout_secs": 60,
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
