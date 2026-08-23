// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `run_test` — the host runs the candidate's own test invocation again, and
//! the plugin never names a directory.
//!
//! The re-run half of the host-call channel (#3580). #3498 solved the *first*
//! run narrowly and correctly, by putting the invocation in the point's request
//! ([`CandidateGrant::test`](stella_plugin::CandidateGrant)) — a plugin holding
//! a root and a plan can run it itself. What that cannot do is ask the host to
//! run it **again against the same opaque handle**, which is why
//! [`RunTestArgs`] carries a [`CandidateHandle`] and nothing else: no path, no
//! program, no argv, no timeout. The plan is the one the grant already carried,
//! so a plugin that wanted to know what ran already knows.
//!
//! # The handle is re-resolved host-side, every time
//!
//! That is the whole security shape of this capability and it is one sentence:
//! **a plugin says which workspace, never where it is.** A handle this host
//! does not hold is [`HostCallRefusal::Unavailable`] — not `Unsupported`, and
//! the difference is where it sends a plugin author. `Unsupported` says "this
//! capability does not exist here, stop asking"; `Unavailable` says "it exists
//! and this host has nothing behind it for you", which is what a handle from
//! another run, or a run with no candidate workspaces at all, actually is.
//!
//! # The port is the host's, not this crate's
//!
//! [`TestRunHost`] is implemented by whatever holds the workspaces —
//! `stella-cli`'s candidate substrate, a server, a fixture — exactly as
//! [`RecallHost`](super::RecallHost) is, and for the harder reason:
//! `crates/stella-runtime/tests/no_pipeline_edge.rs` asserts this assembly
//! declares no edge to a candidate crate, and running a process is I/O that
//! belongs above the engine either way.
//!
//! # Two bounds, and neither is the child-turn bound
//!
//! A test run spends **CPU and wall clock**, not the user's model budget, so
//! this plane does not carve a dollar allowance and has no seat to resolve. It
//! bounds what it can:
//!
//! - **How many runs.** [`DEFAULT_HOST_MAX_TEST_RUNS`], clamping the manifest's
//!   `[loop] max_calls` ask, and **per plugin for the whole run** rather than
//!   per point — the [`ChildTurns`](super::ChildTurns) argument unchanged: a
//!   ceiling that refreshes every point is no ceiling at all across N rounds.
//! - **How much output.** [`DEFAULT_TEST_OUTPUT_CHARS`], clamped here rather
//!   than trusted from the host, because a test suite's output is not this
//!   crate's to size and the wire carries it into a plugin's memory.
//!
//! There is deliberately no `[loop] max_test_runs` key. `max_calls` is the ask
//! a human already consented to at install, and a manifest key exists to say
//! something the existing ones cannot — this one would only restate a number,
//! and the split `max_child_turns` needed was about *money*, which this is not.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use stella_plugin::{
    HostCallFailure, HostCallRefusal, PluginManifest, RunTestArgs, TestBaseline, TestRunResult,
};
use stella_protocol::candidate::CandidateHandle;

/// The host's own ceiling on test runs for one plugin, when a caller states
/// none.
///
/// Four. A flip needs two observations (red, then green) and
/// `plugins/stella-witness`'s `witness-stable` requirement needs a third — the
/// same invocation passing twice on the same tree. Four leaves one and stops a
/// confused plugin from turning a point into a build farm.
pub const DEFAULT_HOST_MAX_TEST_RUNS: u32 = 4;

/// The host's ceiling on how much of a run's output crosses to the plugin.
///
/// Generous enough for a failing suite's tail — which is the part a verifier
/// reads — and bounded, because the alternative is a plugin's memory sized by
/// whatever a test runner felt like printing.
pub const DEFAULT_TEST_OUTPUT_CHARS: usize = 16_384;

/// What one performed test run observed, before the plane names the handle it
/// ran in.
///
/// The host answers the question the wire asks and nothing else: what the
/// assertions said, and what the invocation printed. Exit codes, argv and
/// working directories stay on the host's side of the seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestObservation {
    /// What this run says about the invocation's assertions. Never
    /// [`TestBaseline::NotRun`] — a host with nothing to run refuses with
    /// [`TestRunDenial::NoTestPlan`] instead, so the plugin is told there is
    /// nothing there rather than handed an observation that observed nothing.
    pub assertions: TestBaseline,
    /// What the invocation printed. Clamped by the plane before it crosses.
    pub output: String,
}

/// Why a host did not run a candidate's tests.
///
/// Three, because each one sends a plugin author somewhere different and
/// collapsing any two would lose that (invariant 5's rule at the value level).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TestRunDenial {
    /// No workspace this host holds answers to that handle — including a
    /// handle minted by another run.
    #[error("this host holds no candidate workspace for that handle")]
    UnknownCandidate,

    /// The workspace is this host's, and its grant carried no test invocation.
    ///
    /// Distinct from [`Self::UnknownCandidate`] because the fix is different:
    /// the handle was right and the run has no oracle, which is a
    /// `--test-command` the user did not give rather than a handle the plugin
    /// got wrong.
    #[error("that candidate's grant carries no test invocation, so there is nothing to re-run")]
    NoTestPlan,

    /// It was attempted and the host could not carry it out.
    ///
    /// The `String` is a leaf explanation for a human, not something a caller
    /// branches on — the branch a caller needs is which of these three it is,
    /// which the variant already answers.
    #[error("{0}")]
    Failed(String),
}

/// A host that can re-run a candidate workspace's declared test invocation.
///
/// Object-safe and one method, so [`HostPlanes`](super::HostPlanes) can hold a
/// plane over it without becoming generic over the whole assembly.
#[async_trait]
pub trait TestRunHost: Send + Sync {
    /// Run the invocation the named candidate's grant carried, or say why not.
    ///
    /// # Errors
    ///
    /// [`TestRunDenial`], which the plane maps onto the refusal codes a plugin
    /// reads.
    async fn run_test(&self, candidate: &CandidateHandle)
    -> Result<TestObservation, TestRunDenial>;
}

/// A host that can serve `run_test` to a plugin.
///
/// [`TestRuns`] is the shipped implementation over a [`TestRunHost`]; a host
/// that answers the capability some other way implements this instead of
/// re-implementing the gate above it.
#[async_trait]
pub trait TestRunPlane: Send + Sync {
    /// Serve one `run_test`, or say why not.
    ///
    /// # Errors
    ///
    /// [`HostCallFailure`] for every refusal this module's header names, plus
    /// [`HostCallRefusal::AllowanceSpent`] once the ceiling is reached. Each
    /// one reaches the plugin as an `err` answer rather than ending the point.
    async fn run_test(&self, args: RunTestArgs) -> Result<TestRunResult, HostCallFailure>;
}

/// One test run this plane performed, for the host to report.
///
/// The `run_test` half of [`ChildTurnSpend`](super::ChildTurnSpend)'s argument,
/// with the money taken out: nothing here is spend, but "what did the plugin
/// make my machine do" is still a question a user gets to have answered, and a
/// record only this plane could read would be a silence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRunRecord {
    /// The workspace it ran in.
    pub candidate: String,
    /// What the assertions reported.
    pub assertions: TestBaseline,
}

/// The test-run plane: a ceiling, an output clamp, a ledger, and the host that
/// actually runs the invocation.
pub struct TestRuns<H> {
    plugin: String,
    host: H,
    declared_max: Option<u32>,
    ceiling: u32,
    output_chars: usize,
    spent: AtomicU32,
    ledger: Mutex<Vec<TestRunRecord>>,
}

impl<H> std::fmt::Debug for TestRuns<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestRuns")
            .field("plugin", &self.plugin)
            .field("max_runs", &self.max_runs())
            .finish_non_exhaustive()
    }
}

// Unbounded on `H`, for [`ChildTurns`](super::ChildTurns)' reason: every method
// here decides, clamps or reports, and not one of them runs anything. Requiring
// the port to read `max_runs` would make the `Debug` above — the one thing a
// host prints when it cannot work out why a plugin was refused — unavailable
// exactly when `H` is a type parameter.
impl<H> TestRuns<H> {
    /// Bind a plugin's grant to the host that will perform its test runs.
    ///
    /// The manifest is taken whole rather than a number, for
    /// [`HostCallGate::declare`](super::HostCallGate::declare)'s reason: the
    /// grant a human consented to at install is the authority, and a plane
    /// assembled from a hand-built value is one nobody consented to.
    #[must_use]
    pub fn declare(manifest: &PluginManifest, host: H) -> Self {
        Self {
            plugin: manifest.name.clone(),
            host,
            declared_max: manifest.loop_grant.max_calls,
            ceiling: DEFAULT_HOST_MAX_TEST_RUNS,
            output_chars: DEFAULT_TEST_OUTPUT_CHARS,
            spent: AtomicU32::new(0),
            ledger: Mutex::new(Vec::new()),
        }
    }

    /// Set this host's ceiling on test runs, whatever the manifest asks for.
    #[must_use]
    pub fn with_max_runs(mut self, runs: u32) -> Self {
        self.ceiling = runs;
        self
    }

    /// Set how much of a run's output crosses to the plugin.
    #[must_use]
    pub fn with_output_chars(mut self, chars: usize) -> Self {
        self.output_chars = chars;
        self
    }

    /// The effective ceiling after the clamp. Absent means the ceiling, and a
    /// modest manifest is taken at its word — `min`, not "the host's number
    /// wins".
    #[must_use]
    pub fn max_runs(&self) -> u32 {
        self.declared_max.unwrap_or(self.ceiling).min(self.ceiling)
    }

    /// Every test run this plane performed, in order.
    #[must_use]
    pub fn runs(&self) -> Vec<TestRunRecord> {
        // A poisoned lock means an unrelated call panicked mid-record; the
        // records are still exactly what they were, and losing the report to
        // someone else's panic would be the silence this apparatus refuses
        // (`HostCallGate::refusals` takes the same line).
        self.ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn record(&self, record: TestRunRecord) {
        self.ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(record);
    }

    /// Trim a run's output to what this host will carry, keeping the **tail**.
    ///
    /// The tail rather than the head, because a test runner puts its summary
    /// and its failures at the end, and a verifier reading the first 16K of a
    /// passing suite's chatter has been handed the least useful half.
    fn clamp(&self, output: String) -> String {
        let chars = output.chars().count();
        if chars <= self.output_chars {
            return output;
        }
        let skip = chars - self.output_chars;
        output.chars().skip(skip).collect()
    }
}

#[async_trait]
impl<H: TestRunHost> TestRunPlane for TestRuns<H> {
    async fn run_test(&self, args: RunTestArgs) -> Result<TestRunResult, HostCallFailure> {
        // `fetch_add` rather than load-then-store: a plugin pipelining two
        // calls must not be able to spend the last unit twice.
        let taken = self.spent.fetch_add(1, Ordering::Relaxed);
        let max_runs = self.max_runs();
        if taken >= max_runs {
            return Err(HostCallFailure::new(
                HostCallRefusal::AllowanceSpent,
                format!(
                    "this plugin's allowance of {max_runs} test run(s) is spent; answer with what \
                     you have"
                ),
            ));
        }

        let observed = self
            .host
            .run_test(&args.candidate)
            .await
            .map_err(|denial| {
                let refusal = match denial {
                    // Both are "the capability is implemented and this host has
                    // nothing behind it for you", which is exactly `Unavailable`.
                    // Never `Unsupported`: that word means the host does not
                    // perform `run_test` at all, and once a plane is installed it
                    // does.
                    TestRunDenial::UnknownCandidate | TestRunDenial::NoTestPlan => {
                        HostCallRefusal::Unavailable
                    }
                    TestRunDenial::Failed(_) => HostCallRefusal::Failed,
                };
                HostCallFailure::new(
                    refusal,
                    format!("plugin \"{}\" asked to re-run tests: {denial}", self.plugin),
                )
            })?;

        self.record(TestRunRecord {
            candidate: args.candidate.as_str().to_string(),
            assertions: observed.assertions,
        });
        Ok(TestRunResult {
            candidate: args.candidate,
            assertions: observed.assertions,
            output: self.clamp(observed.output),
        })
    }
}

/// Serving a plane a host still holds a handle on.
///
/// [`HostPlanes::with_test_runs`](super::HostPlanes::with_test_runs) takes the
/// plane by value, and the plane is also what knows which runs happened
/// ([`TestRuns::runs`]). Without this a host would have to choose between
/// installing the plane and being able to report on it.
#[async_trait]
impl<P: TestRunPlane + ?Sized> TestRunPlane for std::sync::Arc<P> {
    async fn run_test(&self, args: RunTestArgs) -> Result<TestRunResult, HostCallFailure> {
        (**self).run_test(args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A host holding exactly one workspace, so a handle from anywhere else is
    /// the "another run" case without a second fixture.
    struct OneWorkspace {
        handle: &'static str,
        answer: Result<TestObservation, TestRunDenial>,
        asked: Mutex<Vec<String>>,
    }

    impl OneWorkspace {
        fn passing() -> Arc<Self> {
            Arc::new(Self {
                handle: "candidate-1",
                answer: Ok(TestObservation {
                    assertions: TestBaseline::Passed,
                    output: "test tests::flip ... ok".to_string(),
                }),
                asked: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl TestRunHost for Arc<OneWorkspace> {
        async fn run_test(
            &self,
            candidate: &CandidateHandle,
        ) -> Result<TestObservation, TestRunDenial> {
            self.asked
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(candidate.as_str().to_string());
            if candidate.as_str() != self.handle {
                return Err(TestRunDenial::UnknownCandidate);
            }
            self.answer.clone()
        }
    }

    fn manifest(max_calls: &str) -> PluginManifest {
        PluginManifest::from_toml_str(&format!(
            "name = \"verifier\"\n\n[loop]\nparticipation = \"steering\"\npoints = \
             [\"after_turn\"]\ncalls = [\"run_test\"]\n{max_calls}"
        ))
        .expect("the manifest loads")
    }

    fn ask(handle: &str) -> RunTestArgs {
        RunTestArgs {
            candidate: CandidateHandle::new(handle),
        }
    }

    #[tokio::test]
    async fn a_known_candidate_is_re_run_and_the_answer_names_it() {
        let host = OneWorkspace::passing();
        let plane = TestRuns::declare(&manifest(""), Arc::clone(&host));

        let result = plane
            .run_test(ask("candidate-1"))
            .await
            .expect("a handle this host holds");
        assert_eq!(result.candidate.as_str(), "candidate-1");
        assert_eq!(result.assertions, TestBaseline::Passed);
        assert_eq!(result.output, "test tests::flip ... ok");
        assert_eq!(plane.runs().len(), 1);
    }

    /// **The code that sends a plugin author to the right place.** A handle from
    /// another run is `unavailable`, never `unsupported`: the capability exists
    /// here, and what is missing is a workspace for that handle.
    #[tokio::test]
    async fn a_handle_from_another_run_is_unavailable_rather_than_unsupported() {
        let plane = TestRuns::declare(&manifest(""), OneWorkspace::passing());
        let failure = plane
            .run_test(ask("candidate-from-another-run"))
            .await
            .expect_err("this host holds no such workspace");
        assert_eq!(failure.refusal, HostCallRefusal::Unavailable);
        assert_ne!(failure.refusal, HostCallRefusal::Unsupported);
        assert!(plane.runs().is_empty(), "a refusal records no run");
    }

    /// The other `Unavailable`: the handle was right and the run has no oracle.
    #[tokio::test]
    async fn a_candidate_with_no_test_plan_is_unavailable_with_its_own_reason() {
        let host = Arc::new(OneWorkspace {
            handle: "candidate-1",
            answer: Err(TestRunDenial::NoTestPlan),
            asked: Mutex::new(Vec::new()),
        });
        let plane = TestRuns::declare(&manifest(""), host);
        let failure = plane
            .run_test(ask("candidate-1"))
            .await
            .expect_err("nothing to re-run");
        assert_eq!(failure.refusal, HostCallRefusal::Unavailable);
        assert!(failure.detail.contains("no test invocation"), "{failure}");
    }

    /// A host that tried and could not is `failed`, which is a different fix
    /// for the plugin's author than either `unavailable`.
    #[tokio::test]
    async fn a_host_that_could_not_run_it_reports_that_it_tried() {
        let host = Arc::new(OneWorkspace {
            handle: "candidate-1",
            answer: Err(TestRunDenial::Failed("cargo is not installed".to_string())),
            asked: Mutex::new(Vec::new()),
        });
        let plane = TestRuns::declare(&manifest(""), host);
        let failure = plane
            .run_test(ask("candidate-1"))
            .await
            .expect_err("the host tried");
        assert_eq!(failure.refusal, HostCallRefusal::Failed);
        assert!(
            failure.detail.contains("cargo is not installed"),
            "{failure}"
        );
    }

    /// The manifest's number is an ask; the ceiling is the host's. Per plugin
    /// for the whole run, not per point.
    #[tokio::test]
    async fn a_plugin_asking_for_more_runs_than_the_ceiling_is_clamped() {
        let plane = TestRuns::declare(&manifest("max_calls = 100"), OneWorkspace::passing())
            .with_max_runs(2);
        assert_eq!(plane.max_runs(), 2, "100 was an ask, 2 is the answer");
        for _ in 0..2 {
            plane.run_test(ask("candidate-1")).await.expect("within");
        }
        assert_eq!(
            plane
                .run_test(ask("candidate-1"))
                .await
                .expect_err("the third is over the ceiling")
                .refusal,
            HostCallRefusal::AllowanceSpent
        );
        assert_eq!(plane.runs().len(), 2, "the clamp is on spend, not on ask");
    }

    /// Output is clamped to the tail, because a runner puts its failures at the
    /// end and the head of a passing suite is the least useful half.
    #[tokio::test]
    async fn a_long_report_is_clamped_to_its_tail() {
        let host = Arc::new(OneWorkspace {
            handle: "candidate-1",
            answer: Ok(TestObservation {
                assertions: TestBaseline::Failed,
                output: format!("{}FAILED: tests::flip", "chatter\n".repeat(500)),
            }),
            asked: Mutex::new(Vec::new()),
        });
        let plane = TestRuns::declare(&manifest(""), host).with_output_chars(32);
        let result = plane.run_test(ask("candidate-1")).await.expect("ran");
        assert_eq!(result.output.chars().count(), 32);
        assert!(
            result.output.ends_with("FAILED: tests::flip"),
            "{}",
            result.output
        );
    }
}
