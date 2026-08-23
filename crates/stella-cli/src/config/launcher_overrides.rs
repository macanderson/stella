//! Process-wide invocation overrides: the trusted-launcher
//! `agent_engine_config` JSON, `--upstream-pin`, `--allow-dir`, and the
//! interactive-credential-prompt latch.
//!
//! Split out of `config.rs` (#3566, #4494): every item here is a fact about
//! *this invocation* that `main` establishes once, before `Config::load`
//! runs — not part of the resolution chain that file's own module doc
//! describes. `config.rs`'s resolve chain and `config/reload.rs` both still
//! read these overrides (re-exported at the old paths, same as
//! `aux_credentials`/`providers`), so nothing about WHEN they apply moved —
//! only where the process-global state and its accessors live.

use std::env;

/// Trusted-launcher override for the complete `agent_engine_config` object.
///
/// This is intentionally one JSON object rather than a collection of
/// independently overridable variables: a benchmark launcher can replace the
/// repository/user engine posture atomically after the normal settings scopes
/// merge. The value is never included in an error message.
pub(super) const TRUSTED_ENGINE_CONFIG_ENV: &str = "STELLA_ENGINE_CONFIG_JSON";

fn invalid_trusted_engine_config() -> String {
    format!(
        "{TRUSTED_ENGINE_CONFIG_ENV} is invalid; refusing to start with a partial engine override"
    )
}

/// Reject unknown fields before deserializing through the normal settings
/// type. `AgentEngineConfig` deliberately tolerates forward-compatible fields
/// in ordinary settings files; the trusted launcher seam is stricter because
/// a misspelled benchmark control must fail closed instead of silently using a
/// provider default.
pub(super) fn trusted_engine_config_shape_is_strict(value: &serde_json::Value) -> bool {
    fn object_has_only(value: &serde_json::Value, allowed: &[&str]) -> bool {
        value
            .as_object()
            .is_some_and(|object| object.keys().all(|key| allowed.contains(&key.as_str())))
    }

    /// `allowed` plus the retired-but-recognized names, as one slice.
    ///
    /// A retired key is **recognized here and reported everywhere else**: it
    /// parses into nothing, and both the settings walker and the launcher say
    /// so by name. See `settings::unknown`'s `RETIRED_ENGINE_ROOT` for why
    /// #3908's role keys take this path while `pipeline_max_revisions` was
    /// dropped outright and is refused.
    fn tolerating(
        allowed: &'static [&'static str],
        retired: &'static [&'static str],
    ) -> Vec<&'static str> {
        allowed.iter().chain(retired).copied().collect()
    }

    // The ONE vocabulary, shared with the settings.json unknown-key warning
    // (`settings::unknown`). A second hand-maintained copy here would drift the
    // moment a knob is added — and because this gate fails CLOSED, a drifted
    // copy is a refused benchmark run rather than a missing warning.
    use crate::settings::{
        ENGINE_AGENT_FIELDS as AGENT_FIELDS, ENGINE_AGENT_NAMES as AGENT_NAMES,
        ENGINE_PARAM_FIELDS as PARAM_FIELDS, ENGINE_ROOT_FIELDS as ROOT_FIELDS,
        RETIRED_ENGINE_AGENT_NAMES, RETIRED_ENGINE_ROOT,
    };

    if !object_has_only(value, &tolerating(ROOT_FIELDS, RETIRED_ENGINE_ROOT)) {
        return false;
    }
    let Some(agents) = value.get("agents") else {
        return true;
    };
    if agents.is_null() {
        return true;
    }
    if !object_has_only(agents, &tolerating(AGENT_NAMES, RETIRED_ENGINE_AGENT_NAMES)) {
        return false;
    }
    let Some(agent_map) = agents.as_object() else {
        return false;
    };
    agent_map.values().all(|agent| {
        if agent.is_null() {
            return true;
        }
        if !object_has_only(agent, AGENT_FIELDS) {
            return false;
        }
        match agent.get("params") {
            None => true,
            Some(params) if params.is_null() => true,
            Some(params) => object_has_only(params, PARAM_FIELDS),
        }
    })
}

pub(super) fn trusted_engine_config_override()
-> Result<Option<crate::settings::AgentEngineConfig>, String> {
    let Some(raw) = env::var_os(TRUSTED_ENGINE_CONFIG_ENV) else {
        return Ok(None);
    };
    let raw = raw
        .into_string()
        .map_err(|_| invalid_trusted_engine_config())?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|_| invalid_trusted_engine_config())?;
    if !trusted_engine_config_shape_is_strict(&value) {
        return Err(invalid_trusted_engine_config());
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(|_| invalid_trusted_engine_config())
}

/// Whether the credential chain's last step — the masked interactive prompt —
/// may fire in this process.
///
/// `stella_model::credential::ApiKey::resolve` states the contract plainly:
/// "headless/non-interactive callers (CI, `--output-format stream-json`)
/// should pass `false` and get a clean `NotFound` instead of hanging on a read
/// from a stdin that isn't there." Nothing honoured it. Every `--model
/// provider/slug` path passed `true` unconditionally, so
/// `stella --model anthropic/… run --output-format json '…'` launched from an
/// attached terminal with no key stopped dead on a password prompt while the
/// caller waited on a JSON object that could never arrive — the machine
/// interface deadlocked on a human one.
///
/// A process-wide latch rather than a threaded parameter because the two
/// non-`main` entry points into `Config::load` (`agent::run_init`,
/// `ingest_cmd::extract_all`) have no view of the requested output format and
/// have no business growing one; this mirrors [`JSON_SUMMARY_EMITTED`] and
/// `signals::INTERRUPTED`, the other two facts about the invocation that
/// `main` establishes once.
///
/// [`JSON_SUMMARY_EMITTED`]: crate::note_json_summary_emitted
pub(super) static INTERACTIVE_CREDENTIALS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

/// Forbid the interactive credential prompt for the rest of this process.
/// Called by `main` once the requested output format is known.
pub(crate) fn forbid_interactive_credentials() {
    INTERACTIVE_CREDENTIALS.store(false, std::sync::atomic::Ordering::SeqCst);
}

/// `--upstream-pin`: the gateway upstreams this invocation is pinned to.
///
/// A process-wide cell rather than a parameter threaded through `Config::load`
/// for the same reason as [`INTERACTIVE_CREDENTIALS`] above: it is a fact about
/// the *invocation* that `main` establishes once, and the non-`main` entry
/// points into `Config::load` have no view of it and no business growing one.
///
/// It outranks settings deliberately. The benchmark harness runs with
/// settings isolation (`STELLA_NO_SETTINGS`), so an argument is the only
/// authority that reaches a measured trial — the same reason `--base-url` is
/// a validated CLI argument there.
static UPSTREAM_PIN: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

/// Record `--upstream-pin` for the rest of this process. Called by `main`;
/// a second call is ignored, so no later caller can re-route a running session.
pub(crate) fn set_upstream_pin(order: Vec<String>) {
    if !order.is_empty() {
        let _ = UPSTREAM_PIN.set(order);
    }
}

/// The invocation's pin, if one was given.
pub(super) fn upstream_pin_override() -> Option<&'static [String]> {
    UPSTREAM_PIN.get().map(Vec::as_slice)
}

/// `--allow-dir`: extra directories this invocation may write to.
///
/// A process-wide cell for the same reason as [`UPSTREAM_PIN`] above — it is a
/// fact about the invocation `main` establishes once, and the other entry
/// points into `Config::load` have no view of it. Unlike the pin it does not
/// outrank settings: the two lists are UNIONED (`crate::write_dirs::resolve`),
/// because a flag that replaced the committed list would revoke a write
/// permission the operator never asked to revoke.
static ALLOW_DIRS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

/// Record `--allow-dir` for the rest of this process. Called by `main`; a
/// second call is ignored, so no later caller can widen a running session's
/// write scope.
pub(crate) fn set_allow_dirs(dirs: Vec<String>) {
    if !dirs.is_empty() {
        let _ = ALLOW_DIRS.set(dirs);
    }
}

/// The invocation's `--allow-dir` values, empty when none were given.
pub(super) fn allow_dirs_override() -> &'static [String] {
    ALLOW_DIRS.get().map_or(&[][..], Vec::as_slice)
}

/// Which source supplies a provider's upstream pin: the flag outranks the
/// settings entry, and absent both there is no pin.
///
/// Pure, and separated from the two call sites that leak, so the precedence
/// that actually decides a measured run is testable without writing to the
/// process-wide cell above — which is one-shot, and would leak into every
/// sibling test sharing the process.
pub(super) fn pin_source<'a>(
    flag: Option<&'a [String]>,
    entry: Option<&'a Vec<String>>,
) -> Option<&'a [String]> {
    flag.or_else(|| entry.map(Vec::as_slice))
}

/// Whether `interactive` may still be honoured. `ApiKey::resolve` applies its
/// own `stdin().is_terminal()` guard on top of this; this is the *policy*
/// half, which a tty check cannot answer.
pub(super) fn interactive_allowed() -> bool {
    INTERACTIVE_CREDENTIALS.load(std::sync::atomic::Ordering::SeqCst)
}
