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
    .serving(nothing_served());
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
    .serving(nothing_served());
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
        .serving(nothing_served())
        .open("drive-test")
        .expect_err("a program that is not there cannot be started");

    assert!(
        error.contains(dir.join("drive.sh").to_string_lossy().as_ref()),
        "{error}"
    );
}

// ---------------------------------------------------------------------------
// The shipped program, and what this host performs for it
// ---------------------------------------------------------------------------

use async_trait::async_trait;
use stella_core::ports::Principal;
use stella_plugin::{DriverCall, HostCallRefusal};
use stella_protocol::issue::{
    Issue, IssueClass, IssueDraft, IssueError, IssueKey, IssueLabel, IssueProvider, IssueState,
};
use stella_runtime::wrapper::{DriverCapabilities, NoDriverCapabilities};

use super::capabilities::HostDriverCapabilities;
use crate::plugin_authz::PluginGates;
use crate::self_driving_cmd::config::LoopConfig;

/// For the gate tests above, which are about what a grant admits rather than
/// what runs behind it.
fn nothing_served() -> Box<dyn DriverCapabilities> {
    Box::new(NoDriverCapabilities)
}

/// The repository root, two levels above this crate's manifest.
fn repo_root() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("the crate sits two levels under the repository root")
        .to_path_buf()
}

/// Where the shipped package lives in this tree.
fn package_dir() -> PathBuf {
    repo_root().join("plugins/stella-selfdriving")
}

/// The operator's real `PATH`, and nothing else. The child is spawned with a
/// cleared environment, so without this `python3` cannot be found.
fn path_only(name: &str) -> Option<String> {
    (name == "PATH").then(|| std::env::var("PATH").unwrap_or_default())
}

/// A tracker that answers from memory, so no test here spawns `gh`.
struct FixtureTracker {
    open: Vec<Issue>,
}

#[async_trait]
impl IssueProvider for FixtureTracker {
    fn id(&self) -> &str {
        "fixture"
    }

    async fn list_open(&self, limit: usize) -> Result<Vec<Issue>, IssueError> {
        Ok(self.open.iter().take(limit).cloned().collect())
    }

    async fn file(&self, _draft: &IssueDraft) -> Result<IssueKey, IssueError> {
        Ok(IssueKey::from("1000"))
    }

    async fn close(&self, _key: &IssueKey, _receipt: &str, _state: &str) -> Result<(), IssueError> {
        Ok(())
    }

    async fn comment(&self, _key: &IssueKey, _body: &str) -> Result<(), IssueError> {
        Ok(())
    }

    async fn relabel(
        &self,
        _key: &IssueKey,
        _add: &[String],
        _remove: &[String],
    ) -> Result<(), IssueError> {
        Ok(())
    }

    async fn edit(
        &self,
        _key: &IssueKey,
        _title: Option<&str>,
        _body: Option<&str>,
    ) -> Result<(), IssueError> {
        Ok(())
    }
}

fn one_open_issue() -> FixtureTracker {
    FixtureTracker {
        open: vec![Issue {
            key: IssueKey::from("41"),
            title: "issue 41".into(),
            body: String::new(),
            state: IssueState::Open,
            class: IssueClass::Bug,
            labels: ["bug", "P1"].into_iter().map(IssueLabel::from).collect(),
            created_at: "2026-08-01T00:00:00Z".into(),
            updated_at: "2026-08-01T00:00:00Z".into(),
            url: "https://example.invalid/41".into(),
            parent: None,
        }],
    }
}

fn capabilities_for(manifest: &str, tracker: FixtureTracker) -> HostDriverCapabilities {
    let roster = roster(vec![installed(manifest, "/opt/pkgs/selfdriving")]);
    HostDriverCapabilities::new(
        "stella-selfdriving",
        PluginGates::from_roster(&roster),
        Box::new(tracker),
        LoopConfig::default(),
    )
}

/// A manifest that accepts shelling out, which is what reading a tracker
/// spends.
const GRANTS_BASH: &str = r#"
name = "stella-selfdriving"
[loop]
participation = "none"
[[capabilities]]
tool = "bash"
risk = "destructive"
purpose = "read the defect queue"
[driver]
calls = ["backlog_next"]
[driver.process]
argv = ["/bin/sh"]
timeout_secs = 5
"#;

/// The same plugin with a grant that does not carry `bash`. Not an empty
/// capability list: a plugin that declares nothing is narrowed by nothing
/// (`crate::plugin_authz`'s module doc says why), so the negative case has to
/// be a plugin that declared something else.
const GRANTS_NO_SHELL: &str = r#"
name = "stella-selfdriving"
[loop]
participation = "none"
[[capabilities]]
tool = "write_file"
risk = "medium"
purpose = "remember what a cycle learned"
[driver]
calls = ["backlog_next"]
[driver.process]
argv = ["/bin/sh"]
timeout_secs = 5
"#;

/// **The attribution witness.** The host reads the tracker *for* the plugin,
/// *as* the plugin: the same install-time rule a tool call is held to decides
/// this one, under `Principal::Plugin("stella-selfdriving")`. A manifest that
/// granted `bash` is served; one that did not is refused, and the refusal
/// names the plugin and the capability.
///
/// Without a host that serves a driver call there is no call to attribute to
/// anybody, so nothing weaker than a served ask can pass this.
#[tokio::test]
async fn a_served_call_is_attributed_to_the_plugin_and_held_to_its_grant() {
    let granted = capabilities_for(GRANTS_BASH, one_open_issue());
    assert_eq!(
        granted.principal(),
        &Principal::Plugin("stella-selfdriving".to_string())
    );

    let ok = granted
        .perform(DriverCall::BacklogNext)
        .await
        .expect("a granted plugin is served the queue");
    let page = ok.backlog.expect("a served read carries its page");
    let keys: Vec<&str> = page.issues.iter().map(|issue| issue.key.as_str()).collect();
    assert_eq!(keys, ["41"]);
    assert_eq!(page.issues[0].labels, ["bug", "P1"]);

    let refused = capabilities_for(GRANTS_NO_SHELL, one_open_issue())
        .perform(DriverCall::BacklogNext)
        .await
        .expect_err("a plugin that was not granted the shell is refused the read");
    assert_eq!(refused.refusal, HostCallRefusal::Forbidden);
    assert!(refused.detail.contains("stella-selfdriving"), "{refused}");
    assert!(refused.detail.contains("bash"), "{refused}");
}

/// A verb this host has not built names its family, so a driver author reading
/// the log knows which phase they are waiting on rather than believing the ask
/// was malformed.
#[tokio::test]
async fn an_unbuilt_verb_names_its_family() {
    let refused = capabilities_for(GRANTS_BASH, FixtureTracker { open: Vec::new() })
        .perform(DriverCall::DeliverMerge)
        .await
        .expect_err("nothing here serves a merge");
    assert_eq!(refused.refusal, HostCallRefusal::Unsupported);
    assert!(refused.detail.contains("deliver"), "{refused}");
}

/// The shipped package names a program, and the host resolves it against the
/// directory the package was installed into.
///
/// A manifest carrying a `[driver]` grant and no `[driver.process]` cannot
/// pass this: the binder refuses it with "one you start yourself", which is
/// what [`a_driver_with_no_process_says_it_is_one_you_start_yourself`] pins.
#[test]
fn the_shipped_package_names_a_program_stella_starts() {
    let dir = package_dir();
    assert!(dir.join("main.py").is_file(), "the program is in the tree");
    let text = std::fs::read_to_string(dir.join("plugin.toml"))
        .expect("the shipped manifest is in the tree");
    let shipped = InstalledPlugin {
        manifest: PluginManifest::from_toml_str(&text).expect("the shipped manifest loads"),
        dir,
        scope: PluginScope::User,
        consent: crate::plugin_cmd::receipt::ConsentState::Receipted,
        panel_grant: crate::plugin_cmd::panel_grant::PanelGrantState::Undecided,
    };

    let resolved = bind_with(
        &roster(vec![shipped]),
        "stella-selfdriving",
        &mut |_| {},
        &mut nothing_set,
    )
    .expect("the shipped driver binds to its program");
    assert_eq!(resolved.program(), "python3");
}

/// One session against the shipped program, under `grant`.
#[cfg(unix)]
fn drive_shipped_program(grant: &str) -> (Result<DriveNext, String>, Vec<String>) {
    let installed = InstalledPlugin {
        manifest: PluginManifest::from_toml_str(grant).expect("fixture must load"),
        dir: package_dir(),
        scope: PluginScope::User,
        consent: crate::plugin_cmd::receipt::ConsentState::Receipted,
        panel_grant: crate::plugin_cmd::panel_grant::PanelGrantState::Undecided,
    };
    let roster = roster(vec![installed]);
    let bound = bind_with(&roster, "stella-selfdriving", &mut |_| {}, &mut path_only)
        .expect("binds to the shipped program")
        .serving(Box::new(HostDriverCapabilities::new(
            "stella-selfdriving",
            PluginGates::from_roster(&roster),
            Box::new(one_open_issue()),
            LoopConfig::default(),
        )));
    let next = bound.open("drive-test");
    (next, bound.refusals())
}

/// **The end-to-end witness.** The shipped program, through the real
/// transport, twice.
///
/// With `backlog_next` declared, the read is served and the program gets as far
/// as asking to claim the issue it found. With it omitted, the host refuses the
/// ask `undeclared`, the session **keeps running**, and the program still ends
/// it with a `next` rather than dying — the property that makes a refusal a
/// value the loop reads instead of a crash.
///
/// The package must name a program for either arm to run at all, and the host
/// must serve the verb for the first arm to reach the claim.
#[cfg(unix)]
#[test]
fn the_shipped_program_is_served_what_it_declared_and_refused_what_it_did_not() {
    const DECLARES_THE_READ: &str = r#"
name = "stella-selfdriving"
[loop]
participation = "none"
[[capabilities]]
tool = "bash"
risk = "destructive"
purpose = "read the defect queue"
[driver]
calls = ["backlog_next", "backlog_claim"]
[driver.process]
argv = ["python3", "${plugin_dir}/main.py"]
timeout_secs = 60
env = ["PATH"]
"#;

    const OMITS_THE_READ: &str = r#"
name = "stella-selfdriving"
[loop]
participation = "none"
[[capabilities]]
tool = "bash"
risk = "destructive"
purpose = "read the defect queue"
[driver]
calls = ["backlog_claim"]
[driver.process]
argv = ["python3", "${plugin_dir}/main.py"]
timeout_secs = 60
env = ["PATH"]
"#;

    let (served, served_refusals) = drive_shipped_program(DECLARES_THE_READ);
    match served.expect("the session ended with a next") {
        DriveNext::Halt { reason } => {
            // The read was served, so the program reached the claim — which
            // this host does not serve, and which it says rather than guessing.
            assert!(reason.contains("41"), "{reason}");
            assert!(reason.contains("backlog_claim"), "{reason}");
        }
        DriveNext::Sleep { secs } => {
            panic!("the queue was not empty, yet the driver slept {secs}s");
        }
    }
    assert!(
        served_refusals
            .iter()
            .all(|line| !line.contains("backlog_next")),
        "a declared, served call must not be refused: {served_refusals:?}"
    );

    let (refused, refusals) = drive_shipped_program(OMITS_THE_READ);
    match refused.expect("a refused ask still ends the session with a next") {
        DriveNext::Halt { reason } => {
            assert!(reason.contains("backlog_next"), "{reason}");
            assert!(reason.contains("undeclared"), "{reason}");
        }
        DriveNext::Sleep { secs } => {
            panic!("a refused read must not read as an empty queue: slept {secs}s");
        }
    }
    let named = refusals
        .iter()
        .find(|line| line.contains("backlog_next"))
        .unwrap_or_else(|| panic!("the refusal must be reported: {refusals:?}"));
    assert!(named.contains("stella-selfdriving"), "{named}");
    assert!(named.contains("undeclared"), "{named}");
}
