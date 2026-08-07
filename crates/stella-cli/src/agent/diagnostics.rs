//! The closed Git diagnostic vocabulary, as argv.
//!
//! Split out of [`super::tools`], which sits against the file-size limit, and
//! coherent on its own terms: this is the one place a
//! [`DiagnosticInvocation`] becomes a process. The runner's state and
//! construction stay beside the rest of the workspace ports in `tools`; only
//! the mapping lives here.
//!
//! Every variant maps to fixed argv. Paths are literal arguments and no shell
//! is involved, which is the property that lets the pipeline hand this
//! model-supplied path without sanitizing it.

use async_trait::async_trait;
use stella_pipeline::{CmdOutcome, DiagnosticInvocation, DiagnosticRunner};

use super::tools::{GitDiagnosticRunner, run_command, scrub_model_subprocess};

#[async_trait]
impl DiagnosticRunner for GitDiagnosticRunner {
    async fn run_diagnostic(&self, invocation: &DiagnosticInvocation) -> CmdOutcome {
        let mut cmd = tokio::process::Command::new("git");
        scrub_model_subprocess(&mut cmd);
        match invocation {
            DiagnosticInvocation::GitDiff => match self.baseline_commit() {
                Some(baseline) => {
                    cmd.args(["diff", baseline]);
                }
                None => {
                    cmd.args(["diff"]);
                }
            },
            DiagnosticInvocation::UntrackedNumstat { path } => {
                cmd.args(["diff", "--no-index", "--numstat", "--", "/dev/null", path]);
            }
            DiagnosticInvocation::UntrackedPatch { path } => {
                // The same probe as the numstat above, minus `--numstat`, so
                // the two answers about one untracked file can never disagree
                // about which file they read. `--no-color` because a
                // configured `color.ui = always` would otherwise paint SGR
                // escapes into a verifier's prompt.
                cmd.args(["diff", "--no-index", "--no-color", "--", "/dev/null", path]);
            }
        }
        cmd.current_dir(&self.root).env("PWD", &self.root);
        for var in stella_tools::exec::GIT_REPO_ENV_VARS {
            cmd.env_remove(var);
        }
        run_command(cmd).await
    }
}
