// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `run_test`, performed — the CLI's [`TestRunHost`] over the grants a door
//! already holds (#4536).
//!
//! # What was missing
//!
//! #3580 shipped the `TestRuns` plane and deleted the last `Unsupported` arm,
//! so a plugin asking the host to re-run a candidate's tests was answered
//! `Unavailable` on every real door. True, and still a refusal: no door bound a
//! [`TestRunHost`], so the capability existed and nothing was behind it. This
//! module is what is behind it.
//!
//! # A plugin says which workspace, never where it is
//!
//! That is the whole security shape of the capability
//! (`stella_runtime::wrapper`'s `test_run` module), and this module is the half that
//! makes it true rather than aspirational: [`GrantedTestRuns`] is built from
//! the grants the door minted, and the only thing a plugin can say is a handle.
//! A handle this door did not grant resolves to nothing —
//! [`TestRunDenial::UnknownCandidate`] — and no path, program or argument a
//! plugin sends can reach the filesystem, because none crosses.
//!
//! It re-runs **the invocation the grant already carried**
//! ([`stella_plugin::TestPlan`]), which the user gave as `--test-command` and
//! the host parsed into a program and an argv before anything reached a plugin
//! (`crate::wrapper_candidate`). Nothing here re-parses, and nothing here ever
//! meets a shell.
//!
//! # Why a door with one shared tree still needs this
//!
//! The goal door's grant is over the tree the turn actually runs in, under
//! [`stella_plugin::HOST_TREE_HANDLE`], and a plugin holding that grant already
//! has the root and the plan — it could run the invocation itself, and
//! `plugins/stella-witness` does. What it cannot do is run it *without* the
//! authority to execute a process, which is exactly what a plugin sandboxed by
//! its `[runtime] env` allowlist may not have, and what a plugin under
//! `stella-serve` or an embedded host never has. The host call is the portable
//! way to ask, and this is the door answering it.
//!
//! # The environment is not scrubbed, and that is a decision
//!
//! `stella_tools::exec::scrub_sensitive_env` strips host credentials from a
//! **model-controlled** child, which is what the `bash` tool and a custom tool
//! spawn. This one is not model-controlled: it is the user's own
//! `--test-command`, given on their own command line, run in their own tree.
//! Scrubbing it would make `run_test` behave differently from the user typing
//! the same command — an integration suite that legitimately reads a key would
//! pass by hand and fail here, and the verifier reading that red would be
//! reading an artifact of this host rather than of the work.
//!
//! # The three bounds, and which one is this module's
//!
//! [`TestRuns`](stella_runtime::wrapper::TestRuns) bounds **how many** runs and
//! **how much output** crosses. Neither is re-decided here. What that plane
//! cannot bound is **how long one run may take**, because it does not perform
//! the run — so the deadline is this module's, as
//! [`DEFAULT_TEST_RUN_TIMEOUT`].

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use stella_plugin::{CandidateGrant, TestBaseline};
use stella_protocol::candidate::CandidateHandle;
use stella_runtime::wrapper::{TestObservation, TestRunDenial, TestRunHost};

/// How long one performed test run may take before this host kills it.
///
/// Ten minutes. A test suite worth verifying against is not a unit test, and a
/// verifier that gave up at sixty seconds would report `Unobserved` for every
/// real repository — which reads as "the assertions said nothing" and would
/// quietly cost every flip. It is a deadline rather than a budget: the plane
/// above already bounds how many runs a plugin gets, so the worst case is
/// bounded by that number times this, and a wedged runner cannot hold a turn
/// open forever.
pub(crate) const DEFAULT_TEST_RUN_TIMEOUT: Duration = Duration::from_secs(600);

/// What one granted candidate's tests are, for a host that will run them.
#[derive(Debug, Clone)]
struct GrantedTests {
    root: PathBuf,
    program: String,
    args: Vec<String>,
}

/// The CLI's [`TestRunHost`]: the grants this door minted, and nothing else.
///
/// Built from grants rather than from a workspace substrate, because that is
/// what every door has. `stella run`'s candidate fan-out mints workspaces whose
/// handles this map can hold; `stella goal` and `stella fleet` mint one grant
/// over the tree the turn runs in. Both are "a handle, a root and a plan", and
/// a host that took the substrate instead would serve one door and refuse the
/// two the issue named.
#[derive(Debug, Default)]
pub(crate) struct GrantedTestRuns {
    granted: BTreeMap<String, GrantedTests>,
    timeout: Duration,
}

impl GrantedTestRuns {
    /// A host that can run the tests of every grant in `grants`.
    ///
    /// A grant carrying no [`TestPlan`](stella_plugin::TestPlan) is still
    /// recorded, so a plugin naming it is told
    /// [`TestRunDenial::NoTestPlan`] — "the handle was right and this run has no
    /// oracle" — rather than [`TestRunDenial::UnknownCandidate`], which would
    /// send its author looking for a bug in the handle.
    pub(crate) fn over<'a>(grants: impl IntoIterator<Item = &'a CandidateGrant>) -> Self {
        let mut granted = BTreeMap::new();
        for grant in grants {
            let tests = grant.test.as_ref().map(|plan| GrantedTests {
                root: PathBuf::from(&grant.root),
                program: plan.program.clone(),
                args: plan.args.clone(),
            });
            granted.insert(
                grant.handle.as_str().to_string(),
                tests.unwrap_or(GrantedTests {
                    root: PathBuf::from(&grant.root),
                    program: String::new(),
                    args: Vec::new(),
                }),
            );
        }
        Self {
            granted,
            timeout: DEFAULT_TEST_RUN_TIMEOUT,
        }
    }

    /// Set this host's deadline for one run, whatever [`DEFAULT_TEST_RUN_TIMEOUT`] says.
    #[cfg(test)]
    pub(crate) fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Whether this host holds any grant at all — the question a door asks
    /// before installing a plane that would refuse everything.
    pub(crate) fn is_empty(&self) -> bool {
        self.granted.is_empty()
    }
}

#[async_trait]
impl TestRunHost for GrantedTestRuns {
    async fn run_test(
        &self,
        candidate: &CandidateHandle,
    ) -> Result<TestObservation, TestRunDenial> {
        let tests = self
            .granted
            .get(candidate.as_str())
            .ok_or(TestRunDenial::UnknownCandidate)?;
        if tests.program.is_empty() {
            return Err(TestRunDenial::NoTestPlan);
        }
        run_in(tests, self.timeout).await
    }
}

/// Run one grant's invocation in its own root, and report what was observed.
///
/// Never a shell: `program` and `args` go to the process builder exactly as
/// `parse_test_invocation` produced them, which is the property that makes a
/// `--test-command` safe to hand to a plugin at all (#1400).
async fn run_in(tests: &GrantedTests, timeout: Duration) -> Result<TestObservation, TestRunDenial> {
    let mut command = tokio::process::Command::new(&tests.program);
    command
        .args(&tests.args)
        .current_dir(&tests.root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // A group of its own, so the deadline below reaches a runner's children —
    // `cargo test` is a parent process whose work is in the binaries it spawns,
    // and killing the parent alone leaves those running.
    stella_tools::exec::detach_into_own_process_group(&mut command);

    let mut child = command.spawn().map_err(|error| {
        TestRunDenial::Failed(format!(
            "`{}` could not be started in {}: {error}",
            tests.program,
            tests.root.display()
        ))
    })?;
    // Cancellation backstop, on the same contract every other spawn site in
    // this workspace takes: a dropped future must not leave the detached group
    // running.
    let mut guard = stella_tools::exec::GroupKillGuard::arm(child.id().unwrap_or(0) as i32);

    let capture = stella_tools::exec::capture(
        &mut child,
        stella_tools::exec::MAX_CAPTURE_BYTES,
        stella_tools::exec::Overflow::Elide,
    );
    let observed = match tokio::time::timeout(timeout, capture).await {
        Ok(Ok(stella_tools::exec::Capture::Exited(output))) => {
            guard.disarm();
            TestObservation {
                // The one place pass/fail is decided, and it is the exit
                // status — never a sniff of the output. Every runner
                // `parse_test_invocation` admits honours its exit code.
                assertions: if output.status.success() {
                    TestBaseline::Passed
                } else {
                    TestBaseline::Failed
                },
                output: combined(&output),
            }
        }
        Ok(Ok(stella_tools::exec::Capture::Refused { stream })) => {
            // Unreachable under `Elide`, which never stops a read. Reported
            // rather than asserted: a binary must not abort a user's run over
            // this crate's own refactor.
            guard.disarm();
            return Err(TestRunDenial::Failed(format!(
                "the eliding capture refused {stream}, which that policy cannot do"
            )));
        }
        Ok(Err(error)) => {
            return Err(TestRunDenial::Failed(format!(
                "`{}` ran and its output could not be read: {error}",
                tests.program
            )));
        }
        // The deadline. `Unobserved` is the wire's own word for "it was
        // attempted and did not complete, so it says nothing about assertions
        // either way" — which is exactly a kill at the deadline, and is not
        // `Failed`: a flip oracle fed a timed-out run as a red baseline would
        // credit the next clean run as a verified fix. The guard stays armed,
        // so the group dies on the drop below.
        Err(_elapsed) => TestObservation {
            assertions: TestBaseline::Unobserved,
            output: String::new(),
        },
    };
    Ok(observed)
}

/// One stream out of two, in the order a reader expects.
///
/// Concatenated rather than reported separately because
/// [`TestObservation::output`] is one field, and separating them would put the
/// summary a runner writes to stdout and the failure it writes to stderr in two
/// places for a verifier to reassemble. The plane above clamps the result to its
/// **tail**, which is where a test runner puts what matters — so stderr going
/// last is deliberate.
fn combined(output: &std::process::Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.stderr.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grant over `root`, with `command` parsed the way a door parses it.
    fn granted(root: &std::path::Path, command: Option<&str>) -> CandidateGrant {
        crate::wrapper_candidate::grant_shared_tree(root, command)
            .expect("the root resolves and the command parses")
            .grant
    }

    fn workspace(script: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temp workspace");
        std::fs::create_dir_all(dir.path().join("tests")).expect("tests dir");
        std::fs::write(dir.path().join("tests/witness_flip.sh"), script).expect("the witness");
        dir
    }

    fn handle(grant: &CandidateGrant) -> CandidateHandle {
        CandidateHandle::new(grant.handle.as_str())
    }

    /// **The witness for #4536.** A granted handle is re-run in its own tree and
    /// the answer is the observation, where every door used to answer
    /// `Unavailable` because nothing was bound behind the capability.
    #[tokio::test]
    async fn a_granted_candidate_is_run_in_its_own_tree() {
        let dir = workspace("#!/bin/sh\necho 'ok: 1 passed'\nexit 0\n");
        let grant = granted(dir.path(), Some("sh tests/witness_flip.sh"));
        let host = GrantedTestRuns::over([&grant]);

        let observed = host
            .run_test(&handle(&grant))
            .await
            .expect("the handle this host granted");
        assert_eq!(observed.assertions, TestBaseline::Passed);
        assert!(observed.output.contains("ok: 1 passed"), "{observed:?}");
    }

    /// A red suite is `Failed` — the assertions genuinely failed, which is the
    /// red half a flip needs and must never be confused with a run that could
    /// not be observed.
    #[tokio::test]
    async fn a_failing_invocation_reports_failed_assertions_and_its_output() {
        let dir = workspace("#!/bin/sh\necho 'FAILED: tests::flip' >&2\nexit 1\n");
        let grant = granted(dir.path(), Some("sh tests/witness_flip.sh"));
        let host = GrantedTestRuns::over([&grant]);

        let observed = host.run_test(&handle(&grant)).await.expect("it ran");
        assert_eq!(observed.assertions, TestBaseline::Failed);
        assert!(
            observed.output.contains("FAILED: tests::flip"),
            "stderr is part of what the invocation printed: {observed:?}"
        );
    }

    /// A plugin says which workspace, never where it is — so a handle this door
    /// did not grant reaches no filesystem at all.
    #[tokio::test]
    async fn a_handle_this_door_did_not_grant_is_unknown() {
        let dir = workspace("#!/bin/sh\nexit 0\n");
        let grant = granted(dir.path(), Some("sh tests/witness_flip.sh"));
        let host = GrantedTestRuns::over([&grant]);

        let denial = host
            .run_test(&CandidateHandle::new("candidate-from-another-run"))
            .await
            .expect_err("this host granted no such workspace");
        assert_eq!(denial, TestRunDenial::UnknownCandidate);
    }

    /// The handle was right and the run has no oracle, which sends a plugin
    /// author somewhere different: a `--test-command` the user did not give.
    #[tokio::test]
    async fn a_grant_with_no_test_command_has_nothing_to_re_run() {
        let dir = workspace("#!/bin/sh\nexit 0\n");
        let grant = granted(dir.path(), None);
        let host = GrantedTestRuns::over([&grant]);

        let denial = host
            .run_test(&handle(&grant))
            .await
            .expect_err("no invocation was ever named");
        assert_eq!(denial, TestRunDenial::NoTestPlan);
    }

    /// A program that is not there is `Failed`: the host attempted it and could
    /// not carry it out, which is a different fix for a plugin's author than
    /// either `Unavailable`.
    #[tokio::test]
    async fn an_unspawnable_program_reports_that_the_host_tried() {
        let dir = workspace("#!/bin/sh\nexit 0\n");
        let mut grant = granted(dir.path(), Some("sh tests/witness_flip.sh"));
        if let Some(plan) = grant.test.as_mut() {
            plan.program = "stella-no-such-runner".to_string();
        }
        let host = GrantedTestRuns::over([&grant]);

        let denial = host
            .run_test(&handle(&grant))
            .await
            .expect_err("no program");
        let TestRunDenial::Failed(reason) = denial else {
            panic!("an unspawnable program is a host that tried");
        };
        assert!(reason.contains("stella-no-such-runner"), "{reason}");
    }

    /// A run killed at the deadline says **nothing** about assertions.
    ///
    /// `Unobserved`, never `Failed`: a flip oracle fed a timed-out run as its
    /// red baseline would credit the next clean run as a verified fix, which is
    /// the manufactured flip `CmdKind` exists to make unrepresentable.
    #[tokio::test]
    async fn a_run_killed_at_the_deadline_observed_no_assertions() {
        let dir = workspace("#!/bin/sh\nsleep 30\n");
        let grant = granted(dir.path(), Some("sh tests/witness_flip.sh"));
        let host = GrantedTestRuns::over([&grant]).with_timeout(Duration::from_millis(150));

        let observed = host
            .run_test(&handle(&grant))
            .await
            .expect("it was attempted");
        assert_eq!(observed.assertions, TestBaseline::Unobserved);
    }

    /// A door with no grant installs no plane rather than one that refuses
    /// everything, and this is the question it asks.
    #[test]
    fn a_host_over_no_grants_is_empty() {
        assert!(GrantedTestRuns::over([]).is_empty());
        let dir = workspace("#!/bin/sh\nexit 0\n");
        let grant = granted(dir.path(), None);
        assert!(!GrantedTestRuns::over([&grant]).is_empty());
    }
}
