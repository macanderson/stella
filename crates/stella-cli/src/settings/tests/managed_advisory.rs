// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Tests for the managed-scope unknown-key advisory.
//!
//! Kept off `settings/tests.rs`, which is at its line limit.

use super::*;

/// A typo in a managed root key parses fine and denies nothing. The
/// advisory names it. `STELLA_HOME` keeps this test off the real home.
#[test]
fn a_managed_root_key_typo_denies_nothing_but_the_advisory_names_it() {
    let _env = crate::test_env::lock();
    let _restore = crate::test_env::EnvRestore::capture(&crate::test_env::home_env_names(&[
        "STELLA_MANAGED_SETTINGS",
    ]));
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let managed = dir.path().join("managed.json");
    let workspace = dir.path().join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(workspace.join(".stella")).unwrap();
    std::fs::write(&managed, r#"{"toosl": {"bash": "off"}}"#).unwrap();
    // SAFETY: the env lock above covers this.
    unsafe {
        crate::test_env::point_home_at(&home);
        std::env::set_var("STELLA_MANAGED_SETTINGS", &managed);
    }

    let merged = Settings::load(&workspace).unwrap();
    assert!(
        merged.tool_policy().allows("bash"),
        "a root-key typo must deny nothing — the advisory below is how it gets noticed instead"
    );

    let advisory = Settings::managed_advisory();
    assert_eq!(advisory.len(), 1, "{advisory:?}");
    assert!(
        advisory[0].contains("toosl"),
        "the advisory must name the misspelled root key: {advisory:?}"
    );
    assert!(
        advisory[0].contains(&managed.display().to_string()),
        "the advisory must name the file: {advisory:?}"
    );
}

/// A second typo, in a different key, to show the check is general.
#[test]
fn a_second_managed_root_key_typo_is_also_named() {
    let _env = crate::test_env::lock();
    let _restore = crate::test_env::EnvRestore::capture(&crate::test_env::home_env_names(&[
        "STELLA_MANAGED_SETTINGS",
    ]));
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let managed = dir.path().join("managed.json");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(&managed, r#"{"authroity": {"project_prompts": "off"}}"#).unwrap();
    // SAFETY: the env lock above covers this.
    unsafe {
        crate::test_env::point_home_at(&home);
        std::env::set_var("STELLA_MANAGED_SETTINGS", &managed);
    }

    let advisory = Settings::managed_advisory();
    assert_eq!(advisory.len(), 1, "{advisory:?}");
    assert!(
        advisory[0].contains("authroity"),
        "the advisory must name the misspelled root key: {advisory:?}"
    );
}
