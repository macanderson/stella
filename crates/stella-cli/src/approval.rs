//! The CLI's approval responder (#2676): a `RequireApproval` gate decision
//! becomes an interactive yes/no question through the same io the
//! `ask_user` tool uses ([`crate::interactive::AskUserIo`]), so the parked
//! tool call proceeds or fails on a real human decision.
//!
//! Wiring: [`attach_interactive_approvals`] is called by
//! `crate::rules::enforce_workspace_rules` — the same session-wiring
//! shorthand that arms the rule guards able to *produce* a
//! `RequireApproval` — with each driver declaring whether its surface is
//! interactive. Headless surfaces (fleet workers, sub-agent lanes,
//! machine-output runs, the deck until it grows an approval card) attach
//! nothing, so the registry's broker stays headless and refuses with the
//! structured grant-path message instead of hanging on stdin that will
//! never answer.
//!
//! Known hazard, tracked rather than hidden: [`crate::interactive::TtyAskUserIo`]
//! reads stdin on the blocking pool, and the broker's TTL cannot cancel
//! that read once it starts — a timed-out approval leaves the read pending,
//! and the user's next line answers the dead prompt instead of the REPL.
//! The `ask_user` tool has the same shape (it has no TTL at all); fixing
//! both means a cancellable line reader, which is its own change.

use std::io::IsTerminal;
use std::sync::Arc;

use async_trait::async_trait;
use stella_tools::ToolRegistry;
use stella_tools::registry::approval::{
    ApprovalRequest, ApprovalResponder, ApprovalResponse, DEFAULT_APPROVAL_TTL,
};

use crate::interactive::{AskUserIo, TtyAskUserIo};

/// [`ApprovalResponder`] over any [`AskUserIo`] — the same injectable io
/// seam the `ask_user` tool uses, so tests script it and the production
/// wiring hands it a TTY.
pub(crate) struct AskUserApprovalResponder {
    io: Box<dyn AskUserIo>,
}

impl AskUserApprovalResponder {
    pub(crate) fn new(io: Box<dyn AskUserIo>) -> Self {
        Self { io }
    }

    /// The question card for one request: names the tool, the narrower
    /// subject when the gate had one (a path, a command line), and the
    /// gate's reason — everything the human needs to answer without
    /// digging.
    fn question(request: &ApprovalRequest) -> String {
        let subject = request
            .subject
            .as_deref()
            .map(|s| format!(" ({s})"))
            .unwrap_or_default();
        format!(
            "`{}`{subject} requires approval: {} — allow this call?",
            request.tool, request.reason
        )
    }
}

#[async_trait]
impl ApprovalResponder for AskUserApprovalResponder {
    async fn respond(&self, request: &ApprovalRequest) -> ApprovalResponse {
        let options = vec![
            format!("Yes — allow `{}` this once", request.tool),
            "No — deny this call".to_string(),
        ];
        match self.io.prompt(&Self::question(request), &options).await {
            // "1"/"yes"/"y" approves; "2"/"no"/"n" (or an empty line) is a
            // bare deny; anything else is a deny carrying the human's own
            // words, which the broker forwards to the model verbatim.
            Ok(raw) => {
                let answer = raw.trim();
                if answer == "1"
                    || answer.eq_ignore_ascii_case("yes")
                    || answer.eq_ignore_ascii_case("y")
                {
                    ApprovalResponse::Approve
                } else if answer.is_empty()
                    || answer == "2"
                    || answer.eq_ignore_ascii_case("no")
                    || answer.eq_ignore_ascii_case("n")
                {
                    ApprovalResponse::Deny {
                        reason: String::new(),
                    }
                } else {
                    ApprovalResponse::Deny {
                        reason: answer.to_string(),
                    }
                }
            }
            // The io itself failed (surface went away mid-prompt): deny
            // with the named cause, never approve on silence.
            Err(error) => ApprovalResponse::Deny {
                reason: format!("no interactive answer available: {error}"),
            },
        }
    }
}

/// Attach the TTY-backed approval responder to `registry` when this surface
/// is genuinely interactive: the caller wants interactive text output AND
/// both stdio handles are terminals — the condition
/// [`crate::agent::approval_capability_for`] already pins for the
/// pipeline's own approval port (stdin must accept the decision, stdout
/// must present the prompt; a piped text run must never read stdin for a
/// decision no one is there to give). Anything less keeps the broker
/// headless, whose refusal names the grant path (`tools.<name>` policy, or
/// rerunning interactively).
pub(crate) fn attach_interactive_approvals(registry: &ToolRegistry, interactive_output: bool) {
    // `supervised: false` — a supervised child's approvals ride the sidecar
    // transport (#1585), and its staged stdin could never pass the terminal
    // test anyway; this attachment is exclusively for the local-TTY surface.
    let capability = crate::agent::approval_capability_for(
        false,
        interactive_output,
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
    );
    if capability != crate::agent::PipelineApprovalCapability::Stdio {
        return;
    }
    registry.attach_approval_responder(
        Arc::new(AskUserApprovalResponder::new(Box::new(TtyAskUserIo))),
        DEFAULT_APPROVAL_TTL,
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Scripted io: records the prompt, pops answers from a queue.
    struct ScriptedIo {
        answers: Mutex<Vec<&'static str>>,
        questions: Mutex<Vec<String>>,
    }

    impl ScriptedIo {
        fn new(answers: Vec<&'static str>) -> Self {
            Self {
                answers: Mutex::new(answers),
                questions: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl AskUserIo for ScriptedIo {
        async fn prompt(&self, question: &str, _options: &[String]) -> Result<String, String> {
            self.questions.lock().unwrap().push(question.to_string());
            let mut answers = self.answers.lock().unwrap();
            if answers.is_empty() {
                return Err("script exhausted".into());
            }
            Ok(answers.remove(0).to_string())
        }
    }

    fn request() -> ApprovalRequest {
        ApprovalRequest {
            tool: "bash".into(),
            read_only: false,
            reason: "commands need a human".into(),
            gate: "command.started".into(),
            subject: Some("rm -rf build/".into()),
        }
    }

    #[tokio::test]
    async fn numeric_and_word_yes_both_approve() {
        for answer in ["1", "yes", "Y"] {
            let responder = AskUserApprovalResponder::new(Box::new(ScriptedIo::new(vec![answer])));
            assert_eq!(
                responder.respond(&request()).await,
                ApprovalResponse::Approve,
                "{answer} approves"
            );
        }
    }

    #[tokio::test]
    async fn bare_no_denies_without_words() {
        for answer in ["2", "no", "N", ""] {
            let responder = AskUserApprovalResponder::new(Box::new(ScriptedIo::new(vec![answer])));
            assert_eq!(
                responder.respond(&request()).await,
                ApprovalResponse::Deny {
                    reason: String::new()
                },
                "{answer:?} denies bare"
            );
        }
    }

    #[tokio::test]
    async fn free_text_denies_with_the_humans_words() {
        let responder = AskUserApprovalResponder::new(Box::new(ScriptedIo::new(vec![
            "ask the platform team first",
        ])));
        assert_eq!(
            responder.respond(&request()).await,
            ApprovalResponse::Deny {
                reason: "ask the platform team first".into()
            }
        );
    }

    #[tokio::test]
    async fn io_failure_denies_never_approves_on_silence() {
        let responder = AskUserApprovalResponder::new(Box::new(ScriptedIo::new(vec![])));
        match responder.respond(&request()).await {
            ApprovalResponse::Deny { reason } => {
                assert!(reason.contains("script exhausted"), "{reason}")
            }
            other => panic!("io failure must deny: {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_question_names_tool_subject_and_reason() {
        let responder = AskUserApprovalResponder::new(Box::new(ScriptedIo::new(vec!["1"])));
        let _ = responder.respond(&request()).await;
        let question = AskUserApprovalResponder::question(&request());
        assert!(question.contains("bash"));
        assert!(question.contains("rm -rf build/"));
        assert!(question.contains("commands need a human"));
    }
}
