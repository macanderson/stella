// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella run --resume <id>`: continue one named session in non-interactive
//! mode.
//!
//! A cloud sandbox can hit its time limit, or its process can exit, with a
//! run half done. The launcher starts Stella again with the run's id. The run
//! goes on from the last step it finished, or takes a new prompt as its next
//! user turn.
//!
//! The id names one session. A missing one is refused before anything is
//! built or paid for, and nothing here falls back to the latest session,
//! because two runs can share one folder.

use std::path::{Path, PathBuf};

use stella_protocol::{CompletionMessage, MessageRole};
use stella_store::{SessionRecord, SessionRegistry};

use crate::OutputFormat;
use crate::config::Config;
use crate::failure::CliFailure;
use crate::prompt_source;

/// `stella run --resume <id>`: continue that session, and add `prompt` as
/// its next user turn when the launcher gave one.
///
/// Every refusal lands before a provider is built or a call is paid for.
#[expect(
    clippy::too_many_arguments,
    reason = "it mirrors `run_one_shot`, with the session id beside the prompt"
)]
pub(crate) async fn run(
    cfg: &Config,
    id: &str,
    prompt: Option<String>,
    budget_limit: Option<f64>,
    format: OutputFormat,
    pipeline: crate::wrapper_plugin::PipelineChoice<'_>,
    test_command: Option<&str>,
    require_verdict: bool,
) -> Result<(), CliFailure> {
    super::preflight_durable_stream(format)?;
    crate::enterprise_telemetry::authorize_one_shot(!pipeline.is_raw())?;
    let prompt = prompt_source::resolve_follow_up(prompt, prompt_source::read_stdin_to_string)?;
    let resumed = ResumedSession::find(cfg, id, prompt)?;
    let label = resumed.label().to_string();
    super::run_raw_one_shot(
        cfg,
        &label,
        budget_limit,
        format,
        pipeline,
        test_command,
        require_verdict,
        None,
        Some(resumed),
    )
    .await
}

/// A session `stella run --resume` continues, found before the run starts.
#[derive(Debug)]
pub(crate) struct ResumedSession {
    record: SessionRecord,
    /// The resume point a turn killed mid-step left, when one did.
    checkpoint: Option<String>,
    /// The conversation saved at the last turn boundary.
    history: Option<Vec<CompletionMessage>>,
    /// The new prompt, when the launcher gave one.
    prompt: Option<String>,
}

impl ResumedSession {
    /// Find session `id` in this machine's registry, ready to go on here,
    /// and bind its durable record so the run saves into the same one.
    ///
    /// It fails, naming the id, when no session has that id, when another
    /// process runs it, when it belongs to another workspace, or when it
    /// saved nothing to continue. With no `prompt` it also fails when the
    /// last turn finished, because nothing is left to run.
    pub(crate) fn find(cfg: &Config, id: &str, prompt: Option<String>) -> Result<Self, String> {
        let registry = SessionRegistry::open_default();
        let record = registry.get(id).ok_or_else(|| unknown_session(id))?;
        check_owner(&record, &cfg.workspace_root)?;
        if let Some(warning) =
            crate::durability::bind_session(&cfg.durability, &cfg.workspace_root, &record.id)
        {
            eprintln!("  {warning}");
        }
        let checkpoint = cfg.durability.checkpoint();
        let history = stella_store::journal::read_history(&registry.sidecar_dir(&record.id));
        Self::from_parts(record, checkpoint, history, prompt)
    }

    /// The checks [`Self::find`] makes once the saved state is read.
    fn from_parts(
        record: SessionRecord,
        checkpoint: Option<String>,
        history: Option<Vec<CompletionMessage>>,
        prompt: Option<String>,
    ) -> Result<Self, String> {
        let interrupted = checkpoint
            .as_deref()
            .is_some_and(|json| stella_core::step::Checkpoint::from_json(json).is_ok());
        let saved = history.as_deref().is_some_and(|turns| !turns.is_empty());
        if !interrupted && !saved {
            return Err(format!(
                "session `{}` saved no conversation, so there is nothing to continue",
                record.id
            ));
        }
        if prompt.is_none() && !interrupted && history.as_deref().is_some_and(finished) {
            return Err(format!(
                "session `{id}` finished its last turn, so there is nothing to continue. Give \
                 it a next turn with `stella run --resume {id} -- \"<prompt>\"`",
                id = record.id
            ));
        }
        Ok(Self {
            record,
            checkpoint,
            history,
            prompt,
        })
    }

    /// The registry record this run takes over.
    pub(crate) fn record(&self) -> &SessionRecord {
        &self.record
    }

    /// The new prompt, when the launcher gave one.
    pub(crate) fn prompt(&self) -> Option<&str> {
        self.prompt.as_deref()
    }

    /// What names this run: the new prompt, or what the session was doing.
    pub(crate) fn label(&self) -> &str {
        match (&self.prompt, self.record.summary.is_empty()) {
            (Some(prompt), _) => prompt,
            (None, false) => &self.record.summary,
            (None, true) => &self.record.title,
        }
    }

    /// The conversation the resumed turn runs on, and one line saying where
    /// it came from.
    ///
    /// The saved turns come back under today's `system_prompt`. A
    /// `follow_up` joins them as the next user turn.
    pub(crate) fn conversation(
        &self,
        system_prompt: &str,
        follow_up: Option<&str>,
        workspace_root: &Path,
    ) -> (Vec<CompletionMessage>, String) {
        let restored = crate::session_persist::restore_conversation(
            self.checkpoint.as_deref(),
            self.history.clone(),
            system_prompt,
        );
        let mut messages = restored.messages;
        if let Some(text) = follow_up {
            messages.push(crate::attachments::user_message_in(text, workspace_root));
        }
        (messages, restored.note)
    }
}

/// The refusal for an id no session has.
fn unknown_session(id: &str) -> String {
    format!(
        "no session `{id}` on this machine, so nothing was started. `stella resume --list` \
         shows the sessions there are"
    )
}

/// Refuse a session another process runs, or one from another workspace.
///
/// A record with this process's own pid is not another process: a restarted
/// container often hands the new process the pid the old one had.
fn check_owner(record: &SessionRecord, workspace_root: &Path) -> Result<(), String> {
    let ours = record.pid == std::process::id();
    if !ours && SessionRegistry::presented_status(record).is_live() {
        return Err(format!(
            "session `{}` is running in another stella process (pid {}). Stop it before you \
             continue it here",
            record.id, record.pid
        ));
    }
    if canonical(Path::new(&record.workspace)) != canonical(workspace_root) {
        return Err(format!(
            "session `{id}` belongs to {}. Run `stella run --resume {id}` from there",
            record.workspace,
            id = record.id
        ));
    }
    Ok(())
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Whether a saved conversation ends on a final answer.
fn finished(messages: &[CompletionMessage]) -> bool {
    messages
        .last()
        .is_some_and(|last| last.role == MessageRole::Assistant && last.tool_calls.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session that ended, as a killed or finished run's record reads.
    fn record(workspace: &Path) -> SessionRecord {
        let mut record = SessionRecord::new(workspace.display().to_string(), "stella");
        record.status = stella_store::SessionStatus::Stopped;
        record
    }

    fn answered() -> Vec<CompletionMessage> {
        vec![
            CompletionMessage::system("sys"),
            CompletionMessage::user("fix the flaky test"),
            CompletionMessage::assistant("fixed"),
        ]
    }

    #[test]
    fn an_unknown_id_is_named_in_its_refusal() {
        let message = unknown_session("ses-missing");
        assert!(message.contains("`ses-missing`"), "{message}");
        assert!(message.contains("nothing was started"), "{message}");
    }

    #[test]
    fn a_session_that_saved_nothing_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let refused = ResumedSession::from_parts(record(dir.path()), None, None, None)
            .expect_err("nothing saved means nothing to continue");
        assert!(refused.contains("saved no conversation"), "{refused}");
    }

    /// A finished session takes a prompt. With none, there is no turn left
    /// to run.
    #[test]
    fn a_finished_session_needs_a_prompt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let refused = ResumedSession::from_parts(record(dir.path()), None, Some(answered()), None)
            .expect_err("no turn is left to run");
        assert!(refused.contains("finished its last turn"), "{refused}");

        let next = Some("now add a test".to_string());
        let resumed = ResumedSession::from_parts(record(dir.path()), None, Some(answered()), next)
            .expect("a prompt gives it a next turn");
        let (messages, _) = resumed.conversation("today's prompt", resumed.prompt(), dir.path());
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].content, "today's prompt");
        assert_eq!(messages[3].role, MessageRole::User);
        assert_eq!(messages[3].content, "now add a test");
        assert_eq!(resumed.label(), "now add a test");
    }

    /// A turn cut off before its answer goes on with no prompt.
    #[test]
    fn an_unanswered_session_continues_without_a_prompt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut history = answered();
        history.pop();
        let resumed = ResumedSession::from_parts(record(dir.path()), None, Some(history), None)
            .expect("an unanswered turn continues");
        let (messages, _) = resumed.conversation("today's prompt", None, dir.path());
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].content, "fix the flaky test");
    }

    #[test]
    fn a_session_from_another_workspace_is_refused() {
        let here = tempfile::tempdir().expect("tempdir");
        let there = tempfile::tempdir().expect("tempdir");
        let refused = check_owner(&record(there.path()), here.path()).expect_err("another folder");
        assert!(refused.contains("belongs to"), "{refused}");
        check_owner(&record(here.path()), here.path()).expect("its own folder");
    }
}
