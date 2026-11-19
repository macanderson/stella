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
//! because two runs can share one folder. Two runs can also be handed one id,
//! so a run locks the session before it goes on with it.

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};

use stella_protocol::{CompletionMessage, MessageRole};
use stella_store::sessions::RESUME_LOCK;
use stella_store::{SessionRecord, SessionRegistry, StoreError};

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
    /// The lock that keeps a second run off this session ([`claim`]), held
    /// while this value lives. `None` where the folder cannot be locked.
    _claim: Option<File>,
}

impl ResumedSession {
    /// Find session `id` in this machine's registry, ready to go on here,
    /// and bind its durable record so the run saves into the same one.
    ///
    /// It fails, naming the id, when no session has that id, when another
    /// process runs it or is continuing it, when it belongs to another
    /// workspace, or when it saved nothing to continue. With no `prompt` it
    /// also fails when the last turn finished, because nothing is left to
    /// run. With a `prompt` it fails when a turn stopped partway, because
    /// that turn has to finish first.
    pub(crate) fn find(cfg: &Config, id: &str, prompt: Option<String>) -> Result<Self, String> {
        let registry = SessionRegistry::open_default();
        let found = registry.get(id).ok_or_else(|| unknown_session(id))?;
        check_owner(&found, &cfg.workspace_root)?;
        let lock = claim(&registry, &found.id)?;
        // Read again under the lock. A run that held it and has just ended
        // left a newer record than the one read above.
        let record = registry.get(id).ok_or_else(|| unknown_session(id))?;
        check_owner(&record, &cfg.workspace_root)?;
        if let Some(warning) =
            crate::durability::bind_session(&cfg.durability, &cfg.workspace_root, &record.id)
        {
            eprintln!("  {warning}");
        }
        let checkpoint = cfg.durability.checkpoint();
        let history = stella_store::journal::load_history(&registry.sidecar_dir(&record.id));
        let mut session = Self::from_parts(record, checkpoint, history, prompt)?;
        session._claim = lock;
        Ok(session)
    }

    /// The checks [`Self::find`] makes once the saved state is read.
    fn from_parts(
        record: SessionRecord,
        checkpoint: Option<String>,
        history: Result<Option<Vec<CompletionMessage>>, StoreError>,
        prompt: Option<String>,
    ) -> Result<Self, String> {
        let interrupted = checkpoint
            .as_deref()
            .is_some_and(|json| stella_core::step::Checkpoint::from_json(json).is_ok());
        let history = match history {
            Ok(history) => history,
            // A resume point holds the whole conversation, so the turn can go
            // on without the file.
            Err(_) if interrupted => None,
            Err(error) => {
                return Err(format!(
                    "session `{}` saved a conversation that could not be read ({error}), so \
                     nothing was started",
                    record.id
                ));
            }
        };
        let saved = history.as_deref().is_some_and(|turns| !turns.is_empty());
        if !interrupted && !saved {
            return Err(format!(
                "session `{}` saved no conversation, so there is nothing to continue",
                record.id
            ));
        }
        if interrupted && prompt.is_some() {
            return Err(format!(
                "session `{id}` stopped partway through a turn, and a new prompt would skip \
                 the rest of it. Run `stella run --resume {id}` with no prompt to finish that \
                 turn, then give the next prompt",
                id = record.id
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
            _claim: None,
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

/// Take the lock that says this process continues session `id`
/// ([`RESUME_LOCK`]).
///
/// Two launchers can read one stopped record at the same moment, and both
/// pass [`check_owner`]. Only one of them gets this lock, so only one runs the
/// session. The lock is held until the returned file drops, when the run
/// ends, and the kernel drops it if the process dies first.
///
/// A held lock refuses the run. A folder or file system that cannot be locked
/// at all is said on stderr, and the run goes on without the lock.
fn claim(registry: &SessionRegistry, id: &str) -> Result<Option<File>, String> {
    let lock = match open_lock(registry, id) {
        Ok(lock) => lock,
        Err(reason) => {
            eprintln!("  session `{id}` could not be locked for this run ({reason})");
            return Ok(None);
        }
    };
    match lock.try_lock() {
        Ok(()) => Ok(Some(lock)),
        Err(TryLockError::WouldBlock) => Err(format!(
            "session `{id}` is being continued by another stella process, so nothing was \
             started here. Run `stella run --resume {id}` again once that run ends"
        )),
        Err(TryLockError::Error(error)) => {
            eprintln!("  session `{id}` could not be locked for this run ({error})");
            Ok(None)
        }
    }
}

/// Open the session's lock file, making its folder when it has none.
fn open_lock(registry: &SessionRegistry, id: &str) -> std::io::Result<File> {
    let sidecar = registry
        .prepare_sidecar(id)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(sidecar.join(RESUME_LOCK))
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

    /// What [`ResumedSession::find`] reads from the history file.
    type Saved = Result<Option<Vec<CompletionMessage>>, StoreError>;

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

    /// [`ResumedSession::from_parts`] for a stopped session in `/w`.
    fn resume(
        checkpoint: Option<String>,
        history: Saved,
        prompt: Option<&str>,
    ) -> Result<ResumedSession, String> {
        let prompt = prompt.map(str::to_string);
        ResumedSession::from_parts(record(Path::new("/w")), checkpoint, history, prompt)
    }

    /// The resume point a killed turn left: the prompt, then one step done.
    fn killed_mid_turn() -> String {
        let mut mid_turn = answered();
        mid_turn.pop();
        mid_turn.push(CompletionMessage::assistant("reading parser.rs"));
        let checkpoint = stella_core::step::Checkpoint {
            version: stella_core::step::CHECKPOINT_VERSION,
            step: 1,
            messages: mid_turn,
            budget: stella_core::step::BudgetSnapshot {
                mode: stella_protocol::BudgetMode::Observed,
                turn_limit_usd: None,
                session_limit_usd: None,
                turn_spent_usd: 0.0,
                session_spent_usd: 0.0,
                rejected_spend_figures: 0,
            },
            total_cost_usd: 0.0,
            calibration_model: None,
            loop_steered: false,
            loop_steered_pattern: Vec::new(),
            loop_steered_inputs: None,
            transcript_rewrites: 0,
            loop_steers_spent: 0,
        };
        checkpoint.to_json().expect("a checkpoint encodes")
    }

    #[test]
    fn an_unknown_id_is_named_in_its_refusal() {
        let message = unknown_session("ses-missing");
        assert!(message.contains("`ses-missing`"), "{message}");
        assert!(message.contains("nothing was started"), "{message}");
    }

    #[test]
    fn a_session_that_saved_nothing_is_refused() {
        let refused = resume(None, Ok(None), None).expect_err("nothing was saved");
        assert!(refused.contains("saved no conversation"), "{refused}");
    }

    /// A history file that cannot be read names its cause, rather than
    /// saying the session saved nothing.
    #[test]
    fn an_unreadable_history_names_its_cause() {
        let torn = Err(StoreError::Other("history.json is torn".into()));
        let refused = resume(None, torn, None).expect_err("the history is torn");
        assert!(refused.contains("history.json is torn"), "{refused}");
        assert!(!refused.contains("saved no conversation"), "{refused}");
    }

    /// A resume point holds the turn, so an unreadable history file does not
    /// stop it.
    #[test]
    fn a_resume_point_carries_a_turn_past_an_unreadable_history() {
        let torn = Err(StoreError::Other("history.json is torn".into()));
        let resumed = resume(Some(killed_mid_turn()), torn, None).expect("the point holds it");
        let (messages, _) = resumed.conversation("today's prompt", None, Path::new("/w"));
        let last = messages.last().expect("the restored turn");
        assert_eq!(last.content, "reading parser.rs");
    }

    /// A finished session takes a prompt. With none, there is no turn left
    /// to run.
    #[test]
    fn a_finished_session_needs_a_prompt() {
        let refused = resume(None, Ok(Some(answered())), None).expect_err("no turn is left");
        assert!(refused.contains("finished its last turn"), "{refused}");

        let next = Some("now add a test");
        let resumed = resume(None, Ok(Some(answered())), next).expect("a prompt is a turn");
        let (messages, _) = resumed.conversation("today", resumed.prompt(), Path::new("/w"));
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].content, "today");
        assert_eq!(messages[3].role, MessageRole::User);
        assert_eq!(messages[3].content, "now add a test");
        assert_eq!(resumed.label(), "now add a test");
    }

    /// A turn cut off before its answer goes on with no prompt.
    #[test]
    fn an_unanswered_session_continues_without_a_prompt() {
        let mut history = answered();
        history.pop();
        let resumed = resume(None, Ok(Some(history)), None).expect("an unanswered turn goes on");
        let (messages, _) = resumed.conversation("today's prompt", None, Path::new("/w"));
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].content, "fix the flaky test");
    }

    /// A turn that stopped partway finishes before a new prompt can follow
    /// it. With no prompt, it goes on.
    #[test]
    fn a_prompt_waits_for_a_turn_that_stopped_partway() {
        let point = Some(killed_mid_turn());
        let saved = Ok(Some(answered()));
        let refused = resume(point.clone(), saved, Some("now add a test")).expect_err("partway");
        assert!(refused.contains("stopped partway"), "{refused}");
        assert!(refused.contains("with no prompt"), "{refused}");

        resume(point, Ok(Some(answered())), None).expect("with no prompt the turn goes on");
    }

    /// One run holds a session's lock, and a second run is refused until the
    /// first one lets go.
    #[test]
    fn a_second_claim_on_one_session_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = SessionRegistry::open(dir.path());
        let first = claim(&registry, "ses-1-2").expect("the first claim");
        assert!(first.is_some(), "a temp folder can be locked");

        let refused = claim(&registry, "ses-1-2").expect_err("the session is held");
        assert!(refused.contains("another stella process"), "{refused}");

        drop(first);
        let again = claim(&registry, "ses-1-2").expect("the lock is free again");
        assert!(again.is_some());
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
