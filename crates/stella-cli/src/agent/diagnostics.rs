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

#[cfg(test)]
mod tests {
    use super::*;

    /// Real git, not a double.
    ///
    /// `stella-pipeline`'s `patch_body` strips everything above the first `@@`
    /// and keeps git's binary sentence verbatim. Both rules are assertions
    /// about what *this* argv prints, and every test of them over there runs
    /// against a scripted string — so without this, the two could drift and
    /// only a live run would notice.
    #[tokio::test]
    async fn an_untracked_text_file_yields_a_hunk_under_a_strippable_preamble() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("regex.txt"), "^\\d{4}-\\d{2}-\\d{2}$\n").unwrap();
        let runner = GitDiagnosticRunner::new(dir.path().to_path_buf());

        let out = runner
            .run_diagnostic(&DiagnosticInvocation::UntrackedPatch {
                path: "regex.txt".to_string(),
            })
            .await;

        // The preamble `patch_body` drops...
        assert!(out.stdout_tail.contains("+++ b/regex.txt"), "{out:?}");
        // ...sits above the first hunk, so dropping it keeps the content.
        let hunk = out.stdout_tail.find("\n@@ ").expect("a hunk header");
        let header = out.stdout_tail.find("+++ b/").expect("the preamble");
        assert!(header < hunk, "{out:?}");
        assert!(
            out.stdout_tail[hunk..].contains("+^\\d{4}-\\d{2}-\\d{2}$"),
            "the content survives below the hunk header: {out:?}"
        );
    }

    /// The bytes of a database sidecar must never reach a prompt. Git decides
    /// that for us — this pins that it still does, and in the wording
    /// `patch_body` matches on.
    #[tokio::test]
    async fn an_untracked_binary_file_yields_gits_sentence_and_no_bytes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("store.db"), [0u8, 159, 146, 150, 0, 1, 2]).unwrap();
        let runner = GitDiagnosticRunner::new(dir.path().to_path_buf());

        let out = runner
            .run_diagnostic(&DiagnosticInvocation::UntrackedPatch {
                path: "store.db".to_string(),
            })
            .await;

        let sentence = out
            .stdout_tail
            .lines()
            .find(|l| l.starts_with("Binary files ") && l.ends_with(" differ"))
            .unwrap_or_else(|| panic!("git did not report a binary file: {out:?}"));
        assert!(sentence.contains("store.db"), "{sentence}");
        assert!(
            !out.stdout_tail.contains("@@ "),
            "a binary file must produce no hunk to render: {out:?}"
        );
    }
}
