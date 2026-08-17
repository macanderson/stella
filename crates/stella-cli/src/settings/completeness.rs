//! Merge completeness — the guard that makes adding a settings key safe.
//!
//! # The defect this exists to prevent
//!
//! A settings key is not one edit. It is four, in lockstep: the struct field
//! ([`super`]), the scope overlay ([`Settings::overlay_scope`]), the
//! unrecognized-key vocabulary ([`super::unknown`]), and the TOML document
//! ([`super::toml_config`]). Miss the second and the key parses in every
//! scope, merges to `None`, and configures exactly nothing — with no parse
//! error, no warning, and no failing test, because a field's own accessor
//! tests call it on a *directly deserialized* `Settings` and never on a
//! merged one. Miss the third and a correctly-spelled key is reported to the
//! operator as a typo.
//!
//! Both halves have shipped. `enable_recap` was inert for exactly this reason
//! (the apology is still in `merge.rs`), and `merge.rs` carries six comments
//! begging future authors to remember. Prose is not a guard, and the six
//! comments did not stop the next three:
//!
//! - `ProviderSettings::overlay` dropped `upstream_pin`, so
//!   `providers.<id>.upstream_pin` merged to `None` in all three scopes and
//!   the pin a head-to-head bench run depends on was silently unset.
//! - `unknown::PROVIDER_FIELDS` omitted the same key, so writing it correctly
//!   earned a "possible typo" warning.
//! - `concat_hooks` joined three of `Hooks`' five events, so a `Stop` or
//!   `PreCompact` hook declared in **any** settings file never reached the
//!   runtime. The #2684 witnesses missed it because they build `Hooks` with
//!   `serde_json::from_str` and never merge a scope.
//!
//! # Why this is a compile error and not a list
//!
//! A hand-maintained list of field names is the same class of object as the
//! thing it is guarding, and fails the same way. So the ledgers below
//! destructure their struct **exhaustively, with no `..` rest pattern**:
//! adding a field to `Settings`, `Hooks`, or `ProviderSettings` stops this
//! file compiling until its author says what the merge does with it. That is
//! the same discipline `stella-protocol`'s event-consumer table uses (`E0004`
//! over a red test) — the answer may be "nothing", but it may not be silence.

use super::unknown::{PROVIDER_FIELDS, ROOT_FIELDS};
use super::*;

/// What the three-scope merge is expected to do with one field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Posture {
    /// [`Settings::overlay_scope`] must carry it from any scope into the
    /// merged view. A field that claims this and is dropped is the
    /// `enable_recap` defect.
    Merged,
    /// Assigned by [`Settings::merge_captured_scopes`] from the managed
    /// snapshot alone, deliberately bypassing `overlay_scope` so that no
    /// lower scope can supply it. `enterprise_telemetry` is the only one:
    /// the enrollment is what the *org* said, and a project file offering
    /// one would be forging authority.
    ManagedOnly,
    /// Computed by [`Settings::load`] and `#[serde(skip)]`, so no document
    /// can carry it and there is nothing to merge.
    Computed,
}

/// One field of a settings struct, as the merge sees it.
struct Field {
    /// The key as written in `settings.json`, or `None` for a
    /// `#[serde(skip)]` field no document can carry.
    key: Option<&'static str>,
    posture: Posture,
    /// Whether this instance's value differs from the type's default. The
    /// round-trip assertion reads this on both sides: non-default going in,
    /// still non-default coming out.
    populated: bool,
}

fn keyed(key: &'static str, posture: Posture, populated: bool) -> Field {
    Field {
        key: Some(key),
        posture,
        populated,
    }
}

/// One row per field of [`Settings`].
///
/// The destructure is exhaustive and deliberately carries no `..` rest
/// pattern — see the module docs.
fn settings_ledger(s: &Settings) -> Vec<Field> {
    let d = Settings::default();
    let Settings {
        providers,
        hooks,
        mcp,
        agent_engine_config,
        tools,
        enable_recap,
        trace_capture,
        ignore_gitignore,
        create_worktrees,
        allowed_dirs,
        ui,
        reward,
        context,
        context_providers,
        managed_authority,
        enterprise_telemetry,
        authority_policy,
        steering_ceiling,
    } = s;
    vec![
        keyed("providers", Posture::Merged, providers != &d.providers),
        keyed("hooks", Posture::Merged, hooks != &d.hooks),
        keyed("mcp", Posture::Merged, mcp != &d.mcp),
        keyed(
            "agent_engine_config",
            Posture::Merged,
            agent_engine_config != &d.agent_engine_config,
        ),
        keyed("tools", Posture::Merged, tools != &d.tools),
        keyed(
            "enable_recap",
            Posture::Merged,
            enable_recap != &d.enable_recap,
        ),
        keyed(
            "trace_capture",
            Posture::Merged,
            trace_capture != &d.trace_capture,
        ),
        keyed(
            "ignore_gitignore",
            Posture::Merged,
            ignore_gitignore != &d.ignore_gitignore,
        ),
        keyed(
            "create_worktrees",
            Posture::Merged,
            create_worktrees != &d.create_worktrees,
        ),
        keyed(
            "allowed_dirs",
            Posture::Merged,
            allowed_dirs != &d.allowed_dirs,
        ),
        keyed("ui", Posture::Merged, ui != &d.ui),
        keyed("reward", Posture::Merged, reward != &d.reward),
        keyed("context", Posture::Merged, context != &d.context),
        keyed(
            "context_providers",
            Posture::Merged,
            context_providers != &d.context_providers,
        ),
        keyed(
            "authority",
            Posture::Merged,
            managed_authority != &d.managed_authority,
        ),
        keyed(
            "enterprise_telemetry",
            Posture::ManagedOnly,
            enterprise_telemetry != &d.enterprise_telemetry,
        ),
        Field {
            key: None,
            posture: Posture::Computed,
            populated: authority_policy != &d.authority_policy,
        },
        // Derived from the MANAGED snapshot by `merge_captured_scopes`, not
        // from the merged view — `context` is honored from an untrusted
        // project scope, so a ceiling that rode the ordinary chain could be
        // lifted by the repository it constrains (#3243 §5).
        Field {
            key: None,
            posture: Posture::Computed,
            populated: steering_ceiling != &d.steering_ceiling,
        },
    ]
}

/// One row per field of [`Hooks`]. Every event concatenates across scopes —
/// any scope may add a gate, none may remove another's — so all five are
/// [`Posture::Merged`].
fn hooks_ledger(h: &Hooks) -> Vec<Field> {
    let d = Hooks::default();
    let Hooks {
        session_start,
        pre_tool_use,
        post_tool_use,
        stop,
        pre_compact,
    } = h;
    vec![
        keyed(
            "SessionStart",
            Posture::Merged,
            session_start != &d.session_start,
        ),
        keyed(
            "PreToolUse",
            Posture::Merged,
            pre_tool_use != &d.pre_tool_use,
        ),
        keyed(
            "PostToolUse",
            Posture::Merged,
            post_tool_use != &d.post_tool_use,
        ),
        keyed("Stop", Posture::Merged, stop != &d.stop),
        keyed("PreCompact", Posture::Merged, pre_compact != &d.pre_compact),
    ]
}

/// One row per field of [`ProviderSettings`]. Entries overlay per field so an
/// org-managed `base_url` and a user-scope `api_key_env` compose, which means
/// every field must be listed in `ProviderSettings::overlay`.
fn provider_ledger(p: &ProviderSettings) -> Vec<Field> {
    let d = ProviderSettings::default();
    let ProviderSettings {
        id,
        name,
        base_url,
        api_key,
        api_key_env,
        default_model,
        upstream_pin,
        dialect,
        cache_ttl,
    } = p;
    vec![
        keyed("id", Posture::Merged, id != &d.id),
        keyed("name", Posture::Merged, name != &d.name),
        keyed("base_url", Posture::Merged, base_url != &d.base_url),
        keyed("api_key", Posture::Merged, api_key != &d.api_key),
        keyed(
            "api_key_env",
            Posture::Merged,
            api_key_env != &d.api_key_env,
        ),
        keyed(
            "default_model",
            Posture::Merged,
            default_model != &d.default_model,
        ),
        keyed(
            "upstream_pin",
            Posture::Merged,
            upstream_pin != &d.upstream_pin,
        ),
        keyed("dialect", Posture::Merged, dialect != &d.dialect),
        keyed("cache_ttl", Posture::Merged, cache_ttl != &d.cache_ttl),
    ]
}

/// A scope document that sets **every** key to a non-default value.
///
/// Deserialized rather than built with a struct literal on purpose: this is
/// the operator-facing document, so the test exercises the same path a real
/// `settings.json` takes and the keys below are the keys a person types. The
/// `populated` assertion is what keeps it honest — a key spelled wrong here,
/// or set to its own default, fails as a test bug rather than passing
/// vacuously.
const EVERY_KEY: &str = r#"{
  "providers": {
    "acme": {
      "id": "acme",
      "name": "Acme Models",
      "base_url": "https://acme.test/v1",
      "api_key": "sk-literal",
      "api_key_env": "ACME_API_KEY",
      "default_model": "acme-large",
      "upstream_pin": ["acme-west"],
      "dialect": "anthropic",
      "cache_ttl": "1h"
    }
  },
  "hooks": {
    "SessionStart": [ { "hooks": [{ "command": "session-start" }] } ],
    "PreToolUse":   [ { "hooks": [{ "command": "pre-tool-use" }] } ],
    "PostToolUse":  [ { "hooks": [{ "command": "post-tool-use" }] } ],
    "Stop":         [ { "hooks": [{ "command": "stop" }] } ],
    "PreCompact":   [ { "hooks": [{ "command": "pre-compact" }] } ]
  },
  "mcp": { "registry_url": "https://registry.test/v0.1" },
  "agent_engine_config": { "default_model": "acme/acme-large" },
  "tools": { "bash": "off" },
  "enable_recap": "on",
  "trace_capture": "on",
  "ignore_gitignore": "off",
  "create_worktrees": "always",
  "allowed_dirs": ["/srv/shared"],
  "ui": { "theme": "stella-light" },
  "reward": { "deterministic_weight": 2.0 },
  "context": { "lifecycle": { "enabled": false } },
  "context_providers": {
    "docs": { "transport": "stdio", "command": "docs-provider", "enabled": true }
  },
  "authority": { "project_prompts": "on" },
  "enterprise_telemetry": { "source": "managed" }
}"#;

fn every_key_scope() -> Settings {
    serde_json::from_str(EVERY_KEY).expect("the completeness document must parse")
}

/// Every [`Posture::Merged`] field of [`Settings`] survives `overlay_scope`.
///
/// **Witness.** Fails on the base commit at `hooks`, because `concat_hooks`
/// joined only three of the five events — a `Stop` hook declared in any
/// scope was dropped before the runtime saw it. The `hooks` row cannot
/// distinguish *which* event was lost, which is what
/// [`every_hook_event_survives_the_scope_merge`] is for.
#[test]
fn every_merged_settings_field_survives_the_scope_merge() {
    let scope = every_key_scope();
    let mut merged = Settings::default();
    merged.overlay_scope(&scope);

    for (declared, got) in settings_ledger(&scope)
        .into_iter()
        .zip(settings_ledger(&merged))
    {
        let key = declared.key.unwrap_or("<skipped>");
        match declared.posture {
            Posture::Merged => {
                assert!(
                    declared.populated,
                    "test bug: `{key}` is set to its own default in EVERY_KEY, so this \
                     assertion would pass even if the merge dropped it"
                );
                assert!(
                    got.populated,
                    "`{key}` is declared Merged but `Settings::overlay_scope` drops it: it \
                     parses in every scope and merges to the default, configuring nothing. \
                     Add it to `overlay_scope` in merge.rs (and check the other three sites \
                     — the struct, `unknown::ROOT_FIELDS`, and the TOML lowering)."
                );
            }
            Posture::ManagedOnly => assert!(
                !got.populated,
                "`{key}` is declared ManagedOnly but `overlay_scope` carries it, which lets a \
                 user or project scope supply what only the org may say"
            ),
            Posture::Computed => assert!(
                !got.populated,
                "a Computed field must stay at its default until `Settings::load` derives it"
            ),
        }
    }
}

/// All five hook events concatenate across scopes.
///
/// **Witness.** Fails on the base commit at `Stop` and `PreCompact`.
/// Separate from the ledger test above because `hooks` is one row there:
/// this is the assertion that names the lost event, and #3246's whole
/// plugin story rests on `Stop` being reachable from a settings file.
#[test]
fn every_hook_event_survives_the_scope_merge() {
    let scope = every_key_scope();
    let mut merged = Settings::default();
    merged.overlay_scope(&scope);

    let declared = scope.hooks.expect("EVERY_KEY declares hooks");
    let got = merged.hooks.expect("the merge keeps a hooks block");
    for (declared, got) in hooks_ledger(&declared).into_iter().zip(hooks_ledger(&got)) {
        let key = declared.key.unwrap_or("<skipped>");
        assert!(
            declared.populated,
            "test bug: `hooks.{key}` is unset in EVERY_KEY"
        );
        assert!(
            got.populated,
            "`hooks.{key}` is dropped by `concat_hooks`, so a matcher declared in any \
             settings scope never reaches the runtime. Every event concatenates — add the \
             `join` line in merge.rs."
        );
    }
}

/// Every field of a `providers.<id>` entry survives the per-field overlay.
///
/// **Witness.** Fails on the base commit at `upstream_pin`.
#[test]
fn every_provider_field_survives_the_entry_overlay() {
    let scope = every_key_scope();
    let declared = scope
        .providers
        .get("acme")
        .expect("EVERY_KEY declares providers.acme")
        .clone();

    let mut merged = ProviderSettings::default();
    merged.overlay(&declared);

    for (declared, got) in provider_ledger(&declared)
        .into_iter()
        .zip(provider_ledger(&merged))
    {
        let key = declared.key.unwrap_or("<skipped>");
        assert!(
            declared.populated,
            "test bug: `providers.<id>.{key}` is unset in EVERY_KEY"
        );
        assert!(
            got.populated,
            "`providers.<id>.{key}` is dropped by `ProviderSettings::overlay`, so it merges \
             to `None` in all three scopes. Add a `take!({key})` line in settings.rs."
        );
    }
}

/// **Witness (#3243 Phase 2).** The org's "off" is final: a managed scope
/// that disables steering cannot be overridden by the project scope, and the
/// two halves of the precedence rule compose in one direction only.
///
/// Deliberately exercises `merge_captured_scopes` rather than the env var —
/// `STELLA_CONTEXT_STEERING` is process-global state and the concurrent test
/// runner makes `set_var` a race (POSIX setenv/getenv is undefined under
/// threads, which is why `truthy_flag` exists as a pure predicate). The env
/// leg is a one-line `env_toggle` read over that same predicate; what is
/// worth a test is the ceiling, which is the part with a trust boundary in
/// it.
#[test]
fn a_managed_steering_ceiling_cannot_be_lifted_by_the_project() {
    let off: Settings = serde_json::from_str(r#"{"context":{"steering":{"enabled":false}}}"#)
        .expect("managed scope parses");
    let on: Settings = serde_json::from_str(r#"{"context":{"steering":{"enabled":true}}}"#)
        .expect("project scope parses");

    let merged = Settings::merge_captured_scopes(
        &Settings::default(),
        &off,
        &on,
        ProjectTrust {
            credentials: true,
            hooks: true,
        },
    );

    assert!(
        !merged.steering_enabled(),
        "the project re-declared the block and must not have lifted the org's ceiling"
    );
    assert!(
        !merged.authority_policy.steering_allowed,
        "and the resolved authority every injection site reads agrees"
    );

    // The control: with no managed objection, the project's own answer stands.
    let permitted = Settings::merge_captured_scopes(
        &Settings::default(),
        &Settings::default(),
        &on,
        ProjectTrust {
            credentials: true,
            hooks: true,
        },
    );
    assert!(permitted.steering_enabled());
}

/// The unrecognized-key vocabulary agrees with the structs, both ways.
///
/// A key missing from the vocabulary is reported to the operator as a
/// possible typo when they spelled it correctly; a key in the vocabulary that
/// no field claims is a stale entry that silences a real typo. Both
/// directions are checked because both have shipped.
///
/// **Witness.** Fails on the base commit: `PROVIDER_FIELDS` omits
/// `upstream_pin`.
#[test]
fn the_unknown_key_vocabulary_matches_the_structs() {
    let check = |what: &str, ledger: Vec<Field>, vocabulary: &[&str]| {
        let declared: Vec<&str> = ledger.iter().filter_map(|f| f.key).collect();
        for key in &declared {
            assert!(
                vocabulary.contains(key),
                "`{what}` has a field `{key}` that the unrecognized-key vocabulary does not \
                 list, so writing it correctly is reported as a possible typo"
            );
        }
        for key in vocabulary {
            assert!(
                declared.contains(key),
                "the unrecognized-key vocabulary lists `{key}`, which no `{what}` field \
                 claims — a stale entry silences a real typo. Remove it, or move it to \
                 `unknown::RETIRED` with the sentence an operator needs."
            );
        }
    };

    check("Settings", settings_ledger(&every_key_scope()), ROOT_FIELDS);
    check(
        "ProviderSettings",
        provider_ledger(&ProviderSettings::default()),
        PROVIDER_FIELDS,
    );
}
