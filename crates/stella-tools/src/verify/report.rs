//! Typed execution reports for pipeline-owned final-state replay.

use serde_json::Value;
use stella_protocol::tool::ToolOutput;

use super::{Verdict, VerifyDone};

pub(super) struct CommandRun {
    pub(super) exit_code: i32,
    pub(super) stdout: String,
    pub(super) stderr: String,
}

impl CommandRun {
    pub(super) fn combined(&self) -> String {
        match (self.stdout.is_empty(), self.stderr.is_empty()) {
            (_, true) => self.stdout.clone(),
            (true, false) => self.stderr.clone(),
            (false, false) => format!("{}\n{}", self.stdout, self.stderr),
        }
    }
}

pub(super) struct CandidateFailure {
    pub(super) command: String,
    pub(super) exit_code: i32,
    pub(super) stdout: String,
    pub(super) stderr: String,
}

pub(super) struct VerifyReport {
    pub(super) output: ToolOutput,
    pub(super) verdict: Verdict,
    pub(super) candidate_failure: Option<CandidateFailure>,
}

impl VerifyReport {
    pub(super) fn new(output: ToolOutput, verdict: Verdict) -> Self {
        Self {
            output,
            verdict,
            candidate_failure: None,
        }
    }
}

impl VerifyDone {
    /// Replay one previously confirmed request through the built-in oracle.
    /// This is the pipeline's typed authority path; ordinary tool-result text
    /// never reaches it.
    pub async fn replay_for_pipeline(
        &self,
        input: &Value,
        root: &std::path::Path,
    ) -> stella_core::VerificationOracleResult {
        let report = self.run_report(input, root).await;
        match report.verdict {
            Verdict::Confirmed => match report.output {
                ToolOutput::Ok { content } => {
                    stella_core::VerificationOracleResult::Confirmed { summary: content }
                }
                ToolOutput::Error { message } => {
                    stella_core::VerificationOracleResult::Unverifiable { reason: message }
                }
            },
            Verdict::NotDone => match report.candidate_failure {
                Some(failure) => stella_core::VerificationOracleResult::CandidateFailed {
                    command: failure.command,
                    exit_code: failure.exit_code,
                    stdout: failure.stdout,
                    stderr: failure.stderr,
                },
                None => stella_core::VerificationOracleResult::Unverifiable {
                    reason: "verify_done reported a candidate failure without an execution receipt"
                        .to_string(),
                },
            },
            _ => {
                let reason = match report.output {
                    ToolOutput::Ok { content } => content,
                    ToolOutput::Error { message } => message,
                };
                stella_core::VerificationOracleResult::Unverifiable { reason }
            }
        }
    }
}
