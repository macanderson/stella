// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Tests for [`crate::driver_plugin`] — the CLI-side construction of a driver
//! session (#3783).
//!
//! Every test here drives the **real** binder, not a hand-built
//! [`SubprocessDriver`]: the thing #3783 says is missing is not the transport
//! (which `crates/stella-runtime/tests/driver_socket.rs` already covers) but a
//! path from an installed package to one, so a test that constructed the
//! transport itself would be testing the half that already worked.

use std::collections::BTreeMap;
use std::path::PathBuf;

use stella_plugin::PluginManifest;

use super::*;
use crate::plugin_cmd::roster::{InstalledPlugin, PluginScope};

/// A driver Stella can start: a grant, and a process to hold it.
///
/// `participation = "none"` is the only grade this can carry, and the reason
/// this manifest
/// could not have carried a `[runtime]` block — see
/// `stella_plugin::manifest`'s
/// `a_driver_declares_its_own_process_and_runtime_could_not_have_carried_it`.
const DRIVING_MANIFEST: &str = r#"
name = "selfdriving"
[loop]
participation = "none"
[driver]
calls = ["backlog_next", "backlog_file"]
max_calls = 4
[driver.process]
argv = ["${plugin_dir}/drive.sh"]
timeout_secs = 30
env = ["PATH", "ANTHROPIC_API_KEY"]
"#;

/// The same grant with no process — `plugins/stella-selfdriving`'s shape: a
/// consent document for a loop a person starts by hand.
const DECLARED_ONLY_MANIFEST: &str = r#"
name = "selfdriving"
[loop]
participation = "none"
[driver]
calls = ["backlog_next"]
"#;

fn installed(text: &str, dir: &str) -> InstalledPlugin {
    InstalledPlugin {
        manifest: PluginManifest::from_toml_str(text).expect("fixture must load"),
        dir: PathBuf::from(dir),
        scope: PluginScope::User,
        consent: crate::plugin_cmd::receipt::ConsentState::Receipted,
        panel_grant: crate::plugin_cmd::panel_grant::PanelGrantState::Undecided,
    }
}

fn roster(plugins: Vec<InstalledPlugin>) -> PluginRoster {
    PluginRoster::compose(plugins, Vec::new(), &BTreeMap::new())
}

/// Nothing in the operator's environment is set, so a test never depends on
/// what the machine running it happens to export.
fn nothing_set(_: &str) -> Option<String> {
    None
}

/// **The witness.** Before this module nothing outside
/// `crates/stella-runtime/tests/` built a [`SubprocessDriver`], so an installed
/// `[driver]` block was a declaration with no path into a running program.
///
/// The assertion is the interpolated argv: the host is the only party that
/// knows where the package was installed, so a program resolved against the
/// package directory is proof the *host* bound it, not that a manifest was
/// parsed.
#[test]
fn an_installed_driver_is_bound_to_the_program_its_package_declares() {
    let roster = roster(vec![installed(DRIVING_MANIFEST, "/opt/pkgs/selfdriving")]);
    let mut warnings = Vec::new();

    let resolved = bind_with(
        &roster,
        "selfdriving",
        &mut |line| warnings.push(line),
        &mut nothing_set,
    )
    .expect("an installed driver with a process binds");

    assert_eq!(resolved.program(), "/opt/pkgs/selfdriving/drive.sh");
}

/// The grant is what the gate is built from, so a declared capability is
/// offered a channel and a grant with none is not — the difference the
/// transport reads before it decides whether to keep the driver's stdin open.
#[test]
fn the_manifests_grant_is_what_the_session_gate_holds() {
    let declared = bind_with(
        &roster(vec![installed(DRIVING_MANIFEST, "/opt/pkgs/selfdriving")]),
        "selfdriving",
        &mut |_| {},
        &mut nothing_set,
    )
    .expect("binds")
    .serving();
    assert!(declared.offers_calls());
    // Nothing was asked, so nothing was refused: a refusal list that filled
    // itself at construction would make the report meaningless.
    assert!(declared.refusals().is_empty());

    const ASKS_NOTHING: &str = r#"
name = "quiet"
[driver]
[driver.process]
argv = ["/bin/sh"]
timeout_secs = 5
"#;
    let quiet = bind_with(
        &roster(vec![installed(ASKS_NOTHING, "/opt/pkgs/quiet")]),
        "quiet",
        &mut |_| {},
        &mut nothing_set,
    )
    .expect("binds")
    .serving();
    assert!(!quiet.offers_calls());
}

/// A model credential named in `[driver.process] env` is withheld at the
/// socket and the withholding is **reported**. An author who is never told
/// cannot stop asking, and a user who is never told believes the key was
/// handed over.
#[test]
fn a_model_credential_is_withheld_from_a_driver_and_the_user_is_told() {
    let roster = roster(vec![installed(DRIVING_MANIFEST, "/opt/pkgs/selfdriving")]);
    let mut warnings = Vec::new();

    bind_with(
        &roster,
        "selfdriving",
        &mut |line| warnings.push(line),
        // Both allowlisted names resolve, so the refusal is the socket's
        // decision rather than an absent variable.
        &mut |name| Some(format!("value-of-{name}")),
    )
    .expect("binds");

    let refusal = warnings
        .iter()
        .find(|line| line.contains("ANTHROPIC_API_KEY"))
        .unwrap_or_else(|| panic!("the withheld credential must be reported: {warnings:?}"));
    assert!(refusal.contains("will not get it"), "{refusal}");
    // `PATH` was allowlisted too and is not a credential, so it is not
    // reported — a report that named every variable would name nothing.
    assert!(
        !warnings.iter().any(|line| line.contains("PATH")),
        "{warnings:?}"
    );
}

/// A grant with no process is refused with the sentence that says what it *is*
/// — the state `plugins/stella-selfdriving` is in, and one a user must be able
/// to tell from a broken install.
#[test]
fn a_driver_with_no_process_says_it_is_one_you_start_yourself() {
    let roster = roster(vec![installed(
        DECLARED_ONLY_MANIFEST,
        "/opt/pkgs/selfdriving",
    )]);

    let error = bind_with(&roster, "selfdriving", &mut |_| {}, &mut nothing_set)
        .expect_err("a grant with no process cannot be started");

    assert!(error.contains("[driver.process]"), "{error}");
    assert!(error.contains("one you start yourself"), "{error}");
}

/// A name that is not an installed driver names the ones that are, rather than
/// leaving the user to guess how the plugin they installed is spelled.
#[test]
fn an_unknown_driver_names_the_installed_ones() {
    let roster = roster(vec![installed(DRIVING_MANIFEST, "/opt/pkgs/selfdriving")]);

    let error = bind_with(&roster, "typo", &mut |_| {}, &mut nothing_set)
        .expect_err("an uninstalled name is refused");
    assert!(error.contains("installed drivers: selfdriving"), "{error}");

    let empty = bind_with(
        &PluginRoster::default(),
        "typo",
        &mut |_| {},
        &mut nothing_set,
    )
    .expect_err("an empty roster refuses too");
    assert!(empty.contains("stella plugin list"), "{empty}");
}

/// A ceiling the host will not fund is announced before the session, not
/// discovered when the allowance runs out (#3841's posture).
#[test]
fn a_ceiling_this_host_will_not_fund_is_announced() {
    let greedy = format!(
        "name = \"greedy\"\n[driver]\ncalls = [\"backlog_next\"]\nmax_calls = {}\n\
         [driver.process]\nargv = [\"/bin/sh\"]\ntimeout_secs = 5\n",
        DEFAULT_DRIVER_MAX_CALLS + 1
    );
    let mut warnings = Vec::new();
    bind_with(
        &roster(vec![installed(&greedy, "/opt/pkgs/greedy")]),
        "greedy",
        &mut |line| warnings.push(line),
        &mut nothing_set,
    )
    .expect("binds");
    assert!(
        warnings
            .iter()
            .any(|line| line.contains("this host funds") && line.contains("greedy")),
        "{warnings:?}"
    );

    // And a manifest asking for no more than the host funds says nothing.
    let modest = format!(
        "name = \"modest\"\n[driver]\ncalls = [\"backlog_next\"]\nmax_calls = {DEFAULT_DRIVER_MAX_CALLS}\n\
         [driver.process]\nargv = [\"/bin/sh\"]\ntimeout_secs = 5\n"
    );
    let mut quiet = Vec::new();
    bind_with(
        &roster(vec![installed(&modest, "/opt/pkgs/modest")]),
        "modest",
        &mut |line| quiet.push(line),
        &mut nothing_set,
    )
    .expect("binds");
    assert!(quiet.is_empty(), "{quiet:?}");
}

/// The end of the path: a bound driver is actually spawned, and a program the
/// package does not contain fails **naming the resolved path**. That is the
/// half a construction-only assertion cannot reach — it proves the argv the
/// host resolved is the argv the transport ran.
#[cfg(unix)]
#[test]
fn opening_a_session_spawns_the_resolved_program_and_reports_what_it_was() {
    let dir = std::env::temp_dir().join(format!("stella-driver-{}", std::process::id()));
    let roster = roster(vec![installed(DRIVING_MANIFEST, &dir.to_string_lossy())]);

    let error = bind_with(&roster, "selfdriving", &mut |_| {}, &mut nothing_set)
        .expect("binds")
        .serving()
        .open("drive-test")
        .expect_err("a program that is not there cannot be started");

    assert!(
        error.contains(dir.join("drive.sh").to_string_lossy().as_ref()),
        "{error}"
    );
}
