//! Reload completeness — the guard that makes adding a `Config` field safe.
//!
//! # The defect this exists to prevent
//!
//! [`Config::load`] and [`Config::reload_from_disk`] both derive from the
//! settings scope chain, and only the first of them is exercised by the
//! startup path everyone runs. A field derived in `load` and forgotten in
//! `reload_from_disk` keeps its session-start value for the rest of the
//! session while `/reload` reports success — no parse error, no warning, and
//! no failing test, because every field's own accessor test calls it on a
//! freshly *loaded* `Config` and never on a reloaded one.
//!
//! That has shipped: `ignore_gitignore` was absent from both blocks of
//! `reload_from_disk` from the day the reload was written, so a workspace
//! that switched the gitignore filter off and reloaded kept walking ignored
//! paths until it restarted (#3895).
//!
//! # Why this is a compile error and not a list
//!
//! A hand-maintained list of field names is the same class of object as the
//! thing it is guarding, and fails the same way. So [`ledger`] destructures
//! [`Config`] **exhaustively, with no `..` rest pattern**: adding a field
//! stops this module compiling until its author says what `/reload` does with
//! it. The answer may be [`Posture::StartupOnly`], but it may not be silence.
//! That is the discipline `settings::completeness` applies to the scope merge
//! one layer down, and `stella-protocol`'s event-consumer table applies to
//! event variants.

use super::super::Config;

/// What `/reload` is expected to do with one field of [`Config`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Posture {
    /// [`Config::reload_from_disk`] must re-derive it from the scope chain on
    /// disk. A field that claims this and is dropped is the
    /// `ignore_gitignore` defect.
    Reloaded,
    /// Re-derived by the reload, but from the process environment rather than
    /// a settings document, so [`EVERY_RELOADABLE_KEY`] cannot move it. The
    /// string names the test that does.
    ReloadedFromEnv(&'static str),
    /// Deliberately left at its session-start value, for the reason carried
    /// beside it. Not "unimportant" — a row here is a claim that re-deriving
    /// it mid-session would be *wrong*, not merely unimplemented.
    StartupOnly(&'static str),
}

/// One field of [`Config`], as `/reload` sees it.
struct Field {
    /// The field's name in Rust, for the assertion message.
    name: &'static str,
    posture: Posture,
    /// Whether this instance's value differs from the pre-reload fixture. The
    /// assertion reads this after the reload: a `Reloaded` field the document
    /// moved must differ, a `StartupOnly` field must not.
    moved: bool,
}

/// One row per field of [`Config`], comparing `after` against `before`.
///
/// The destructure is exhaustive and deliberately carries no `..` rest
/// pattern — see the module docs.
fn ledger(before: &Config, after: &Config) -> Vec<Field> {
    let Config {
        provider,
        model_id,
        model_pinned_by_flag,
        api_key,
        turn_timeout,
        max_output_tokens,
        plan_mode,
        minimal_prompt,
        workspace_root,
        durability: _,
        output_ceilings: _,
        base_url_override,
        hooks,
        engine_settings,
        engine_settings_trusted,
        tool_policy,
        ignore_gitignore,
        reward_policy,
        create_worktrees,
        allowed_write_dirs,
        authority,
        credential_source,
        credential_advisories,
        aux_credentials: _,
        cache_ttl,
    } = after;

    // The provider/model/credential half. `reload_from_disk`'s own doc
    // comment states why: resolving them needs the full startup chain, the
    // interactive credential prompt included, and a mid-session change of
    // provider is a larger step than a config refresh.
    const RESOLUTION: &str = "provider/model/credential resolution needs the full startup chain (interactive \
         prompt included); a mid-session provider change is not a config refresh";
    // The per-invocation half: stamped in `main` from the parsed CLI, which
    // no file on disk can restate.
    const INVOCATION: &str = "stamped from the command line, not from the settings chain";

    vec![
        Field {
            name: "provider",
            posture: Posture::StartupOnly(RESOLUTION),
            moved: provider.id != before.provider.id,
        },
        Field {
            name: "model_id",
            posture: Posture::StartupOnly(RESOLUTION),
            moved: model_id != &before.model_id,
        },
        Field {
            name: "model_pinned_by_flag",
            posture: Posture::StartupOnly(INVOCATION),
            moved: model_pinned_by_flag != &before.model_pinned_by_flag,
        },
        Field {
            name: "api_key",
            posture: Posture::StartupOnly(RESOLUTION),
            moved: api_key.reveal() != before.api_key.reveal(),
        },
        Field {
            name: "turn_timeout",
            posture: Posture::StartupOnly(INVOCATION),
            moved: turn_timeout != &before.turn_timeout,
        },
        Field {
            name: "max_output_tokens",
            posture: Posture::StartupOnly(INVOCATION),
            moved: max_output_tokens != &before.max_output_tokens,
        },
        Field {
            name: "plan_mode",
            posture: Posture::StartupOnly(INVOCATION),
            moved: plan_mode != &before.plan_mode,
        },
        Field {
            name: "minimal_prompt",
            // Only the FLAG half lives here; the settings spelling rides
            // `engine_settings`, which does reload — but the assembled prompt
            // is fixed at session start either way (L-E8).
            posture: Posture::StartupOnly(INVOCATION),
            moved: minimal_prompt != &before.minimal_prompt,
        },
        Field {
            name: "workspace_root",
            posture: Posture::StartupOnly(
                "the reload's own input — it is where the chain is read from",
            ),
            moved: workspace_root != &before.workspace_root,
        },
        Field {
            name: "durability",
            posture: Posture::StartupOnly(
                "a shared cell bound by the driver, deliberately outliving every \
                 `EngineConfig` this session builds",
            ),
            // A handle, not a value: `SessionDurability` has no equality and
            // re-pointing it is the deck's job, not the reload's.
            moved: false,
        },
        Field {
            name: "output_ceilings",
            posture: Posture::StartupOnly(
                "a shared cell holding what this session PAID to learn; discarding it on a \
                 config refresh would re-buy every refused ceiling",
            ),
            moved: false,
        },
        Field {
            name: "base_url_override",
            posture: Posture::StartupOnly(RESOLUTION),
            moved: base_url_override != &before.base_url_override,
        },
        Field {
            name: "hooks",
            posture: Posture::Reloaded,
            moved: hooks != &before.hooks,
        },
        Field {
            name: "engine_settings",
            posture: Posture::Reloaded,
            moved: engine_settings != &before.engine_settings,
        },
        Field {
            name: "engine_settings_trusted",
            posture: Posture::ReloadedFromEnv("a_reload_re_reads_the_trusted_engine_seam"),
            moved: engine_settings_trusted != &before.engine_settings_trusted,
        },
        Field {
            name: "tool_policy",
            posture: Posture::Reloaded,
            moved: tool_policy.allows("bash") != before.tool_policy.allows("bash"),
        },
        Field {
            name: "ignore_gitignore",
            posture: Posture::Reloaded,
            moved: ignore_gitignore != &before.ignore_gitignore,
        },
        Field {
            name: "reward_policy",
            posture: Posture::Reloaded,
            moved: reward_policy != &before.reward_policy,
        },
        Field {
            name: "create_worktrees",
            posture: Posture::Reloaded,
            moved: create_worktrees != &before.create_worktrees,
        },
        Field {
            name: "allowed_write_dirs",
            posture: Posture::StartupOnly(
                "the grant takes effect ONLY in `ToolRegistry::allow_write_dirs`, at assembly \
                 time; re-deriving the field would leave `Config` describing a scope the live \
                 registry does not enforce. Widening it mid-session is `/add-dir`'s job \
                 (`command_deck::add_dir`), which reaches the registry; the reload has only \
                 `&mut Config`",
            ),
            moved: allowed_write_dirs != &before.allowed_write_dirs,
        },
        Field {
            name: "authority",
            posture: Posture::Reloaded,
            moved: authority != &before.authority,
        },
        Field {
            name: "credential_source",
            posture: Posture::StartupOnly(RESOLUTION),
            moved: credential_source != &before.credential_source,
        },
        Field {
            name: "credential_advisories",
            posture: Posture::StartupOnly(RESOLUTION),
            moved: credential_advisories.len() != before.credential_advisories.len(),
        },
        Field {
            name: "aux_credentials",
            posture: Posture::StartupOnly(RESOLUTION),
            moved: false,
        },
        Field {
            name: "cache_ttl",
            posture: Posture::StartupOnly(
                "the interactive surfaces STAMP this after load \
                 (`Config::adopt_interactive_cache_ttl`); re-deriving it would silently return \
                 a deck session to the 5-minute window mid-run",
            ),
            moved: cache_ttl != &before.cache_ttl,
        },
    ]
}

/// A scope document that moves **every** [`Posture::Reloaded`] field the
/// settings chain can reach.
///
/// Written as the operator-facing document rather than built with a struct
/// literal, for the reason `settings::completeness::EVERY_KEY` is: this is
/// what a person types, so the test takes the same path their file does. The
/// `moved` assertion is what keeps it honest — a key spelled wrong here, or
/// set to a value that happens to be the default, fails as a test bug rather
/// than passing vacuously.
const EVERY_RELOADABLE_KEY: &str = r#"{
  "hooks": { "Stop": [ { "hooks": [{ "command": "stop" }] } ] },
  "agent_engine_config": { "default_model": "local/reloaded" },
  "tools": { "bash": "off" },
  "ignore_gitignore": "off",
  "create_worktrees": "never",
  "reward": { "deterministic_weight": 2.0 }
}"#;

/// The org-managed half of the same document.
///
/// `authority` is the one [`Posture::Reloaded`] field a user-scope file cannot
/// move: [`crate::settings::AuthorityPolicy::compute`] reads the **managed**
/// block alone, because a repository that could grant itself authority would
/// be forging it. So the ledger test points `STELLA_MANAGED_SETTINGS` at this
/// document as well.
const EVERY_MANAGED_RELOADABLE_KEY: &str = r#"{
  "authority": { "media_requires_host_approval": "off" }
}"#;

/// **Witness (#3895).** `/reload` re-derives every field it declares
/// `Reloaded`, and leaves every `StartupOnly` field alone.
///
/// Fails on the base commit at `ignore_gitignore`: the field is derived by
/// `Config::load` and absent from both blocks of `reload_from_disk`, so a
/// session told its reload succeeded kept the startup filter.
#[test]
fn a_reload_re_derives_every_field_it_claims_to() {
    // `reload_from_disk` reads the process-wide trusted-engine-config env
    // var; hold the binary env lock so a concurrent test setting it to a
    // malformed value cannot make this load fail (setenv races any getenv).
    let _env = crate::test_env::lock();
    // The trusted seam is the one Reloaded field a settings document cannot
    // reach; clear it so an ambient value cannot move `engine_settings_trusted`
    // and make this test report a dirty environment as a dropped field.
    let _restore = crate::test_env::EnvRestore::capture(&[
        super::super::TRUSTED_ENGINE_CONFIG_ENV,
        "STELLA_MANAGED_SETTINGS",
    ]);
    // SAFETY: guarded by `test_env::lock`, and restored by `_restore` even on
    // an unwinding panic.
    unsafe {
        std::env::remove_var(super::super::TRUSTED_ENGINE_CONFIG_ENV);
    }
    let (home, _paths, mut cfg) =
        crate::config::tests::resolution::reload_fixture("reload-completeness");
    let before = cfg.clone();

    std::fs::write(
        home.join(".stella").join("settings.json"),
        EVERY_RELOADABLE_KEY,
    )
    .unwrap();
    let managed = home.join(".stella").join("managed.json");
    std::fs::write(&managed, EVERY_MANAGED_RELOADABLE_KEY).unwrap();
    // SAFETY: as above.
    unsafe {
        std::env::set_var("STELLA_MANAGED_SETTINGS", &managed);
    }

    cfg.reload_from_disk().unwrap();

    for field in ledger(&before, &cfg) {
        let name = field.name;
        match field.posture {
            Posture::Reloaded => assert!(
                field.moved,
                "`{name}` is declared Reloaded and `Config::reload_from_disk` did not move it. \
                 Either it is missing from the derive/commit blocks in reload.rs — in which \
                 case a session is told the reload succeeded and keeps its startup value — or \
                 EVERY_RELOADABLE_KEY does not actually set it, which is a test bug."
            ),
            Posture::ReloadedFromEnv(witness) => assert!(
                !field.moved,
                "`{name}` is declared ReloadedFromEnv and moved under a settings document \
                 alone — either it now reads a file too (declare it Reloaded) or this test's \
                 environment is dirty. Its own witness is `{witness}`."
            ),
            Posture::StartupOnly(why) => assert!(
                !field.moved,
                "`{name}` is declared StartupOnly ({why}) and the reload moved it anyway"
            ),
        }
    }

    let _ = std::fs::remove_dir_all(&home);
}

/// The trusted-launcher seam is re-read too: the one `Reloaded` field no
/// settings document can reach, because it is a process environment variable
/// rather than a file.
///
/// Its own test because it mutates the environment, which the ledger test
/// deliberately does not — and because the seam's contract is that the object
/// replaces the merged engine config ATOMICALLY, so both fields move together
/// or the posture is misdescribed (#1147).
#[test]
fn a_reload_re_reads_the_trusted_engine_seam() {
    let _env = crate::test_env::lock();
    let _restore = crate::test_env::EnvRestore::capture(&[super::super::TRUSTED_ENGINE_CONFIG_ENV]);
    let (home, _paths, mut cfg) =
        crate::config::tests::resolution::reload_fixture("reload-trusted-seam");
    assert!(
        !cfg.engine_settings_trusted,
        "premise: the fixture is not trusted"
    );

    // SAFETY: guarded by `test_env::lock`, and restored by `_restore` even on
    // an unwinding panic.
    unsafe {
        std::env::set_var(
            super::super::TRUSTED_ENGINE_CONFIG_ENV,
            r#"{"default_model":"local/frozen"}"#,
        );
    }
    cfg.reload_from_disk().unwrap();

    assert!(
        cfg.engine_settings_trusted,
        "the reload must re-read the trusted seam — a posture that claims a frozen, \
         disclosed engine config and reports numbers against a stale one is the wrong \
         published number #1147 exists to prevent"
    );
    assert_eq!(
        cfg.engine_settings
            .as_ref()
            .and_then(|e| e.model_for())
            .unwrap_or_default(),
        "local/frozen",
        "and the object it names is the one that landed, un-layered"
    );

    let _ = std::fs::remove_dir_all(&home);
}
