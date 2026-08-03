//! The diff-coverage check's host half (#1291): run the tracked test command
//! under the workspace's own coverage tool and report which lines executed.
//!
//! The engine decides what the overlap *means*
//! ([`stella_pipeline::verify::coverage`]); this decides whether the
//! measurement can be made at all, and makes it. Two dialects are supported,
//! and the boundary is deliberate rather than arbitrary: they are the two this
//! codebase already knows how to *read test output* for
//! (`verify::fingerprint` parses libtest and pytest), so "we can measure what
//! we can already interpret" is one rule instead of a growing list.
//!
//! | Test command | Instrumented as | Report read |
//! |---|---|---|
//! | `cargo test …` | `cargo llvm-cov --lcov --output-path …` | LCOV |
//! | `pytest …` / `python -m pytest …` | `pytest --cov --cov-report=json:…` | coverage.py JSON |
//! | anything else | — | none (`unmeasured`) |
//!
//! # Every failure is `None`, and that is the honest answer
//!
//! No coverage tool installed, a subcommand that does not exist, a run that
//! wrote no report, a report that will not parse — all of them return `None`,
//! which the ladder reads as
//! [`DiffCoverage::Unmeasured`](stella_pipeline::verify::coverage::DiffCoverage::Unmeasured):
//! *nobody could tell*. None of them is a finding that the test failed to
//! cover the change, and none may cost the candidate its verdict. The one
//! thing this must never do is guess.
//!
//! # Cost
//!
//! One extra, instrumented run of the tracked command, paid only inside the
//! pre-submit audit — where a deterministic pass is about to be credited — and
//! only when a probe is wired. Instrumented runs are slower than plain ones
//! (llvm-cov roughly doubles a Rust suite), which is the whole reason this is
//! a probe with its own timeout rather than something folded into the ordinary
//! test runner.

use std::path::{Path, PathBuf};

use stella_pipeline::TestInvocation;
use stella_pipeline::verify::coverage::{CoverageReport, parse_coverage_json, parse_lcov};

/// Ceiling on one instrumented run. Above the plain test runner's 300s
/// because instrumentation is genuinely slower, and bounded for the same
/// reason everything else here is: an unbounded probe would let a coverage
/// tool hold a verified candidate hostage.
const COVERAGE_TIMEOUT_SECS: u64 = 600;

/// Which coverage dialect a test invocation can be instrumented with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialect {
    /// `cargo llvm-cov`, reporting LCOV.
    CargoLlvmCov,
    /// `pytest --cov`, reporting coverage.py JSON.
    PytestCov,
}

/// The workspace-rooted coverage probe. Rooted per call like every other
/// probe, so one instance serves the session tree and each isolated candidate
/// worktree.
pub(crate) struct ToolchainCoverageProbe {
    pub(crate) root: PathBuf,
}

#[async_trait::async_trait]
impl stella_pipeline::CoverageProbe for ToolchainCoverageProbe {
    async fn covered_lines(
        &self,
        root: Option<&str>,
        invocation: &TestInvocation,
    ) -> Option<CoverageReport> {
        let root = root.map(PathBuf::from).unwrap_or_else(|| self.root.clone());
        let dialect = dialect_for(invocation)?;
        // A scratch directory beside the tree rather than inside it: the
        // candidate is SEALED at this point, and `sealed_is_unchanged` would
        // rightly refuse a candidate whose verification had written a report
        // file into it.
        let scratch = tempfile::tempdir().ok()?;
        let report_path = scratch.path().join(match dialect {
            Dialect::CargoLlvmCov => "coverage.lcov",
            Dialect::PytestCov => "coverage.json",
        });
        let command = instrumented_command(dialect, invocation, &root, &report_path)?;
        // The run's own exit status is deliberately ignored. The pipeline has
        // already observed this command's assertions through the ordinary
        // runner; what is wanted here is only the report, and a suite that
        // fails under instrumentation for an unrelated reason (a coverage
        // tool that cannot attach, a plugin that errors late) should read as
        // "unmeasured" rather than contradict an observation already made.
        let _ = run_with_timeout(command).await;
        let text = tokio::fs::read_to_string(&report_path).await.ok()?;
        let report = match dialect {
            Dialect::CargoLlvmCov => parse_lcov(&text),
            Dialect::PytestCov => parse_coverage_json(&text),
        };
        // An empty report is no report: it means the tool ran and attributed
        // nothing, which cannot distinguish "nothing executed" from "the
        // instrumentation never attached". `None` says so.
        (!report.is_empty()).then_some(report)
    }
}

/// Which dialect, if any, can instrument this invocation.
///
/// Matched on the program and its first argument only — the same closed
/// reading `witness::parse_test_invocation` already performs on the way in, so
/// nothing model-authored decides which tool is spawned here.
fn dialect_for(invocation: &TestInvocation) -> Option<Dialect> {
    let program = Path::new(&invocation.program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&invocation.program);
    match program {
        "cargo" if invocation.args.first().is_some_and(|arg| arg == "test") => {
            Some(Dialect::CargoLlvmCov)
        }
        "pytest" => Some(Dialect::PytestCov),
        // `python -m pytest …` — the spelling a project without a `pytest`
        // shim on PATH uses, and common enough that missing it would leave
        // most Python workspaces silently unmeasured.
        "python" | "python3" => (invocation.args.first().is_some_and(|arg| arg == "-m")
            && invocation.args.get(1).is_some_and(|arg| arg == "pytest"))
        .then_some(Dialect::PytestCov),
        _ => None,
    }
}

/// Build the instrumented command for `invocation`, writing its report to
/// `report_path`.
///
/// The original argv is preserved verbatim after the tool's own flags, so the
/// instrumented run executes *the same tests* the ordinary run did. A
/// narrowed or widened selection here would measure a different suite than the
/// one whose flip is being credited — which is the coincidence this check
/// exists to catch, reintroduced one level up.
fn instrumented_command(
    dialect: Dialect,
    invocation: &TestInvocation,
    root: &Path,
    report_path: &Path,
) -> Option<tokio::process::Command> {
    let mut cmd = match dialect {
        Dialect::CargoLlvmCov => {
            let mut cmd = tokio::process::Command::new(&invocation.program);
            cmd.arg("llvm-cov")
                .arg("--lcov")
                .arg("--output-path")
                .arg(report_path);
            // Everything after `test` — the subcommand llvm-cov supplies
            // itself. `cargo llvm-cov --lcov … -- <libtest args>` is not the
            // same shape, so the filters ride as `cargo llvm-cov … test …`.
            cmd.arg("test");
            cmd.args(invocation.args.iter().skip(1));
            cmd
        }
        Dialect::PytestCov => {
            let mut cmd = tokio::process::Command::new(&invocation.program);
            cmd.args(&invocation.args);
            cmd.arg("--cov")
                .arg(format!("--cov-report=json:{}", report_path.display()));
            cmd
        }
    };
    cmd.current_dir(root).env("PWD", root);
    for var in stella_tools::exec::GIT_REPO_ENV_VARS {
        cmd.env_remove(var);
    }
    stella_tools::exec::scrub_sensitive_env(&mut cmd);
    Some(cmd)
}

/// Run one instrumented command to completion, bounded by
/// [`COVERAGE_TIMEOUT_SECS`]. Output is discarded — the report file is the
/// only thing read — and every failure mode collapses to `None`, which is
/// what makes an absent coverage tool cost nothing but the spawn attempt.
async fn run_with_timeout(mut cmd: tokio::process::Command) -> Option<()> {
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    cmd.kill_on_drop(true);
    let mut child = cmd.spawn().ok()?;
    let timeout = std::time::Duration::from_secs(COVERAGE_TIMEOUT_SECS);
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(_)) => Some(()),
        // A tool that timed out has written nothing usable; the report read
        // that follows will fail and the overlap stays unmeasured.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invocation(program: &str, args: &[&str]) -> TestInvocation {
        TestInvocation {
            program: program.to_string(),
            args: args.iter().map(|a| (*a).to_string()).collect(),
        }
    }

    /// The supported dialects, and the honest `None` for everything else —
    /// which is what "languages we cannot measure are handled explicitly
    /// rather than by accident" means in mechanism.
    #[test]
    fn only_the_dialects_we_can_read_are_instrumented() {
        assert_eq!(
            dialect_for(&invocation("cargo", &["test", "-p", "x"])),
            Some(Dialect::CargoLlvmCov)
        );
        assert_eq!(
            dialect_for(&invocation("/usr/bin/pytest", &["-k", "thing"])),
            Some(Dialect::PytestCov)
        );
        assert_eq!(
            dialect_for(&invocation("python3", &["-m", "pytest", "tests/"])),
            Some(Dialect::PytestCov)
        );
        for unsupported in [
            invocation("cargo", &["build"]),
            invocation("go", &["test", "./..."]),
            invocation("npm", &["test"]),
            invocation("python", &["-m", "unittest"]),
            invocation("make", &["check"]),
        ] {
            assert_eq!(
                dialect_for(&unsupported),
                None,
                "unsupported dialect must be unmeasured, never guessed: {unsupported:?}"
            );
        }
    }

    /// The instrumented run must execute the SAME selection the plain run
    /// did — a narrowed or widened suite would measure something other than
    /// the flip being credited.
    #[test]
    fn the_original_selection_rides_into_the_instrumented_command() {
        let root = Path::new("/tmp/x");
        let report = Path::new("/tmp/x/report");
        let cargo = instrumented_command(
            Dialect::CargoLlvmCov,
            &invocation("cargo", &["test", "-p", "stella-core", "--", "--exact"]),
            root,
            report,
        )
        .expect("cargo is instrumentable");
        let args: Vec<String> = cargo
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec![
                "llvm-cov",
                "--lcov",
                "--output-path",
                "/tmp/x/report",
                "test",
                "-p",
                "stella-core",
                "--",
                "--exact"
            ]
        );

        let pytest = instrumented_command(
            Dialect::PytestCov,
            &invocation("pytest", &["tests/test_thing.py::test_case"]),
            root,
            report,
        )
        .expect("pytest is instrumentable");
        let args: Vec<String> = pytest
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec![
                "tests/test_thing.py::test_case",
                "--cov",
                "--cov-report=json:/tmp/x/report"
            ]
        );
    }
}
