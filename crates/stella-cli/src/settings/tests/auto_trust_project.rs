//! `run.auto_trust_project`: the config-file default for
//! [`crate::settings::project_trust`], and the two guarantees it must hold —
//! an env var always overrides it, and the project scope can never supply it.
//!
//! A sibling file rather than more lines in `settings/tests.rs`, which sits at
//! the 1500-line ratchet (the same reason `tests/trust.rs` is one).

use crate::settings::{ProjectTrust, Settings, resolve_project_trust};

/// **Witness.** On the base commit `resolve_project_trust` does not exist —
/// `project_trust` reads only `STELLA_TRUST_PROJECT`/`STELLA_PROJECT_HOOKS`,
/// so a config default has nowhere to feed in and this file fails to compile.
///
/// Five arms, because the merge of "env present or absent" against "config
/// true or false" has that many distinct cells worth naming: an env var
/// wins whichever way it points, even down against a `true` config default,
/// and `STELLA_PROJECT_HOOKS` keeps opening hooks alone when nothing else
/// grants full trust.
#[test]
fn an_env_var_always_overrides_the_config_default_either_way() {
    // No env at all, config off: unchanged legacy behavior.
    assert!(!resolve_project_trust(None, false, false).code_execution_trusted());

    // No env at all, config on: the new capability this exists for.
    let trusted = resolve_project_trust(None, false, true);
    assert!(trusted.code_execution_trusted());
    assert!(trusted.credentials);

    // Env explicitly OFF, config ON: the env var closes trust the config
    // opened — the "override downward" half of "always holds authority".
    let closed = resolve_project_trust(Some(false), false, true);
    assert!(!closed.code_execution_trusted());
    assert!(!closed.credentials);

    // Env explicitly ON, config OFF: the one-off override upward — trust an
    // otherwise-untrusted project for a single launch.
    let opened = resolve_project_trust(Some(true), false, false);
    assert!(opened.code_execution_trusted());
    assert!(opened.credentials);

    // The legacy hooks-only flag still opens hooks alone, config and
    // STELLA_TRUST_PROJECT both silent.
    let hooks_only = resolve_project_trust(None, true, false);
    assert!(hooks_only.code_execution_trusted());
    assert!(!hooks_only.credentials);
}

/// **Witness.** A project's own `stella.toml` cannot vote on its own trust.
///
/// Simulates the shape `Settings::load` produces without touching real env
/// vars or `~/.stella` — `merge_captured_scopes` takes three already-parsed
/// snapshots, so the project scope here declares `auto_trust_project = true`
/// directly and the assertion is that the merged view still reports the
/// TRUSTED-ONLY snapshot's answer, not the project's.
#[test]
fn merge_captured_scopes_never_honors_the_projects_own_auto_trust_project() {
    let trusted_only_false = Settings::default();
    let project_claims_trusted: Settings =
        serde_json::from_str(r#"{"auto_trust_project": true}"#).expect("project scope parses");

    let merged = Settings::merge_captured_scopes(
        &trusted_only_false,
        &Settings::default(),
        &project_claims_trusted,
        // Trust is passed in already resolved, exactly as `Settings::load`
        // would derive it from `resolve_project_trust` — untrusted here,
        // because a project self-declaring `auto_trust_project` must not
        // even manage to flip this bit in the first place.
        ProjectTrust {
            hooks: false,
            credentials: false,
        },
    );

    assert_eq!(
        merged.auto_trust_project,
        None,
        "the merged view must report what the TRUSTED scopes said (nothing, here), \
         never the project's own claim"
    );

    // The control: when the USER scope (a trusted one) says it, the value
    // survives — proving the field is not simply dropped everywhere, only
    // withheld from the one scope that must never supply it.
    let user_grants_it: Settings =
        serde_json::from_str(r#"{"auto_trust_project": true}"#).expect("user scope parses");
    let merged_from_user = Settings::merge_captured_scopes(
        &user_grants_it,
        &Settings::default(),
        &Settings::default(),
        ProjectTrust {
            hooks: true,
            credentials: true,
        },
    );
    assert_eq!(merged_from_user.auto_trust_project, Some(true));
}
