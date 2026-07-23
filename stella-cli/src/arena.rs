//! `stella arena` — the [arena-bench](https://github.com/macanderson/arena-bench)
//! adapter port.
//!
//! arena-bench invokes an agent with `--task-dir/--journal/--state-dir/
//! [--resume]`, SIGKILLs it mid-episode on purpose, re-invokes it, and judges
//! the [`contextgraph-trace`] journal it recorded with the Context Graph
//! Protocol's replay oracles. This module is Stella's side of that contract:
//!
//! - [`ArenaRecorder`] maps the live [`AgentEvent`] stream onto the trace
//!   vocabulary and appends it to the journal **before** each event is
//!   admitted to the renderer channel — the same persist-first boundary the
//!   Harbor durable sink uses ([`EventSender::from_fn`]), so a SIGKILL at any
//!   byte leaves a truthful prefix. `StepManifest` becomes `prompt_assembled`
//!   (block identities, digests, and token costs are already content-free),
//!   `ToolStart`/`ToolResult` become the tool-loop pairing, mutating
//!   `FileChange`s and `Commit`s become intended-once side effects.
//! - [`run_arena`] wires the contract: workspace = `--task-dir` (prompt in
//!   `TASK.md`), persistent memory rides `--state-dir` via the workspace
//!   `.stella` link, and `--resume` recovers the journal and declares exactly
//!   what it recovered.
//!
//! The recorder is installed process-globally and consulted by
//! `event_sender_for_run` — one-shot runs are one-per-process, exactly like
//! the env-gated Harbor sink this mirrors.
//!
//! [`contextgraph-trace`]: https://github.com/macanderson/context-graph-protocol/blob/main/docs/sketches/host-trace.md

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use contextgraph_trace::{EventBody, RenderedFrame, SessionOutcome, TRACE_FORMAT, TraceEvent};
use contextgraph_types::FrameId;
use stella_core::EventSender;
use stella_protocol::{AgentEvent, BlockKind, ToolOutput};
use tokio::sync::mpsc;

use crate::OutputFormat;
use crate::config::Config;

/// Journal persistence is part of the accounting boundary: an event the
/// journal did not accept must not be admitted to the run. Mirrors the
/// durable stream-json sink's exit discipline.
const ARENA_JOURNAL_FAILURE_EXIT: i32 = 74;

static RECORDER: OnceLock<ArenaRecorder> = OnceLock::new();

/// The recorder installed for this process's run, if `stella arena` set one.
pub(crate) fn installed_recorder() -> Option<ArenaRecorder> {
    RECORDER.get().cloned()
}

/// The persist-first event boundary for an arena run: journal the trace
/// mapping of the event, then enqueue the same event to the renderer.
pub(crate) fn recording_event_sender(
    sender: mpsc::UnboundedSender<AgentEvent>,
    recorder: ArenaRecorder,
) -> EventSender {
    EventSender::from_fn(move |event| {
        recorder.observe(&event);
        sender.send(event).map_err(|_| stella_core::EventSendError)
    })
}

/// CLI arguments of the adapter contract (`Command::Arena`).
pub(crate) struct ArenaArgs {
    pub task_dir: PathBuf,
    pub journal: PathBuf,
    pub state_dir: PathBuf,
    pub resume: bool,
    pub no_pipeline: bool,
    pub test_command: Option<String>,
}

/// Run one arena episode invocation: honor the contract, record the journal,
/// drive the ordinary one-shot path.
pub(crate) async fn run_arena(mut cfg: Config, args: ArenaArgs) -> Result<(), String> {
    let task_dir = args
        .task_dir
        .canonicalize()
        .map_err(|error| format!("--task-dir {}: {error}", args.task_dir.display()))?;
    std::env::set_current_dir(&task_dir).map_err(|error| format!("entering task dir: {error}"))?;
    cfg.workspace_root = task_dir.clone();
    wire_state_dir(&task_dir, &args.state_dir)?;

    let prompt = std::fs::read_to_string(task_dir.join("TASK.md"))
        .map_err(|error| format!("reading TASK.md in the task dir: {error}"))?;

    let recorder = ArenaRecorder::open(&args.journal, args.resume, &cfg.model_id)?;
    RECORDER
        .set(recorder.clone())
        .map_err(|_| "arena recorder installed twice in one process".to_string())?;

    let result = crate::agent::run_one_shot(
        &cfg,
        &prompt,
        None,
        OutputFormat::StreamJson,
        !args.no_pipeline,
        args.test_command.as_deref(),
    )
    .await;

    // `Complete` normally closes the recording; close it here too so an
    // early return (or an error) still ends the session honestly.
    match &result {
        Ok(()) => recorder.finish(SessionOutcome::Completed),
        Err(_) => recorder.finish(SessionOutcome::Aborted),
    }
    result
}

/// Persistent agent memory across episodes: the workspace `.stella` store is
/// linked into `--state-dir`, which the runner preserves between episodes
/// (and wipes for the `amnesic` arm). A fixture that ships its own `.stella`
/// wins — the link is only created when the workspace has none.
fn wire_state_dir(task_dir: &Path, state_dir: &Path) -> Result<(), String> {
    let workspace_store = task_dir.join(".stella");
    if workspace_store.exists() {
        return Ok(());
    }
    let backing = state_dir.join("workspace-stella");
    std::fs::create_dir_all(&backing)
        .map_err(|error| format!("creating state dir {}: {error}", backing.display()))?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&backing, &workspace_store)
            .map_err(|error| format!("linking .stella into the state dir: {error}"))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(&workspace_store)
            .map_err(|error| format!("creating {}: {error}", workspace_store.display()))?;
    }
    Ok(())
}

/// What the recorder remembers about a registered context block, so a later
/// manifest entry can be named and verified at its point of use.
struct BlockMeta {
    digest: Option<String>,
    citation_label: Option<String>,
    kind: BlockKind,
}

/// Maps the [`AgentEvent`] stream onto `contextgraph-trace` events, appended
/// crash-safely (one flushed line per event; torn tails truncated on
/// recovery). Cloneable — every [`EventSender`] clone shares this state, and
/// the inner mutex is what makes journal order equal admission order.
#[derive(Clone)]
pub(crate) struct ArenaRecorder {
    inner: Arc<Mutex<RecorderInner>>,
}

impl std::fmt::Debug for ArenaRecorder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ArenaRecorder")
    }
}

struct RecorderInner {
    file: std::fs::File,
    path: String,
    session: String,
    next_seq: u64,
    open_turn: Option<u64>,
    highest_turn: u64,
    blocks: HashMap<String, BlockMeta>,
    pending_calls: HashSet<String>,
    performed_effects: HashSet<String>,
    ended: bool,
}

impl ArenaRecorder {
    /// Open (or recover) the journal. Fresh ⇒ `session_start`; an existing
    /// recording requires `--resume` and gets a `resume` event declaring
    /// exactly the highest `seq` recovered.
    pub(crate) fn open(path: &Path, resume: bool, model: &str) -> Result<Self, String> {
        let display = path.display().to_string();
        let existing = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(format!("journal {display}: {error}")),
        };

        let mut session = None;
        let mut next_seq: u64 = 1;
        let mut highest_turn: u64 = 0;
        let mut performed_effects = HashSet::new();
        let mut good_lines: Vec<&str> = Vec::new();
        let lines: Vec<&str> = existing
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        for (index, line) in lines.iter().enumerate() {
            match serde_json::from_str::<TraceEvent>(line) {
                Ok(event) => {
                    session.get_or_insert(event.session.clone());
                    next_seq = event.seq + 1;
                    if let Some(turn) = event.turn {
                        highest_turn = highest_turn.max(turn);
                    }
                    if let EventBody::SideEffect { effect_id, .. } = &event.body {
                        performed_effects.insert(effect_id.clone());
                    }
                    good_lines.push(line);
                }
                // A torn final line is the signature of the SIGKILL; the
                // events it carried were never durable, which the `resume`
                // event's `last_seq_seen` declares honestly.
                Err(_) if index + 1 == lines.len() => break,
                Err(error) => {
                    return Err(format!("journal {display} line {}: {error}", index + 1));
                }
            }
        }
        if good_lines.len() != lines.len() || !(existing.is_empty() || existing.ends_with('\n')) {
            let mut clean = good_lines.join("\n");
            if !clean.is_empty() {
                clean.push('\n');
            }
            std::fs::write(path, clean).map_err(|error| format!("journal {display}: {error}"))?;
        }

        let fresh = session.is_none();
        if !fresh && !resume {
            return Err(format!(
                "journal {display} already holds a recording; re-invocations must pass --resume"
            ));
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|error| format!("journal {display}: {error}"))?;

        let session_id = session.unwrap_or_else(|| format!("stella-arena-{:x}", epoch_millis()));
        let recorder = Self {
            inner: Arc::new(Mutex::new(RecorderInner {
                file,
                path: display,
                session: session_id,
                next_seq,
                open_turn: None,
                highest_turn,
                blocks: HashMap::new(),
                pending_calls: HashSet::new(),
                performed_effects,
                ended: false,
            })),
        };
        {
            let mut inner = recorder.lock();
            if fresh {
                inner.emit(
                    None,
                    EventBody::SessionStart {
                        agent: "stella".to_string(),
                        harness: format!("stella/{}", env!("CARGO_PKG_VERSION")),
                        model: Some(model.to_string()),
                        trace_format: Some(TRACE_FORMAT.to_string()),
                    },
                );
            } else {
                let last_seq_seen = inner.next_seq - 1;
                inner.emit(None, EventBody::Resume { last_seq_seen });
            }
        }
        Ok(recorder)
    }

    /// Map and journal one agent event. Runs inside the event admission
    /// boundary: this MUST complete before the event reaches the renderer.
    pub(crate) fn observe(&self, event: &AgentEvent) {
        let mut inner = self.lock();
        if inner.ended {
            return;
        }
        match event {
            AgentEvent::BlockRegistered {
                block_id,
                kind,
                content_digest,
                citation_label,
                ..
            } => {
                inner.blocks.insert(
                    block_id.clone(),
                    BlockMeta {
                        digest: Some(content_digest.clone()),
                        citation_label: citation_label.clone(),
                        kind: *kind,
                    },
                );
            }
            AgentEvent::StepManifest {
                blocks,
                effective_budget_tokens,
                ..
            } => {
                inner.ensure_turn_open();
                let frames: Vec<RenderedFrame> = blocks
                    .iter()
                    .map(|entry| {
                        let meta = inner.blocks.get(&entry.block_id);
                        RenderedFrame {
                            frame: FrameId::new(
                                "stella",
                                entry.block_id.clone(),
                                meta.and_then(|meta| meta.digest.clone()),
                            ),
                            representation: Default::default(),
                            token_cost: entry.token_cost,
                            citation_label: Some(block_label(meta)),
                        }
                    })
                    .collect();
                let declared_total_tokens: u64 =
                    frames.iter().map(|frame| u64::from(frame.token_cost)).sum();
                let turn = inner.open_turn;
                inner.emit_at(
                    turn,
                    EventBody::PromptAssembled {
                        budget_tokens: (*effective_budget_tokens).min(u64::from(u32::MAX)) as u32,
                        declared_total_tokens,
                        // Deliberately absent: the manifest is content-free,
                        // and a digest derived from the block-id sequence
                        // would make the deterministic-composition oracle
                        // judge a tautology instead of the rendered bytes.
                        composition_digest: None,
                        frames,
                    },
                );
            }
            AgentEvent::ToolStart { call } => {
                inner.ensure_turn_open();
                inner.pending_calls.insert(call.call_id.clone());
                let turn = inner.open_turn;
                inner.emit_at(
                    turn,
                    EventBody::ModelResponse {
                        tool_calls: vec![call.call_id.clone()],
                    },
                );
                inner.emit_at(
                    turn,
                    EventBody::ToolCall {
                        call_id: call.call_id.clone(),
                        tool: call.name.clone(),
                    },
                );
            }
            AgentEvent::ToolResult {
                call_id, output, ..
            } => {
                if inner.pending_calls.remove(call_id) {
                    let status = match output {
                        ToolOutput::Ok { .. } => contextgraph_trace::ToolStatus::Ok,
                        ToolOutput::Error { .. } => contextgraph_trace::ToolStatus::Error,
                    };
                    let turn = inner.open_turn;
                    inner.emit_at(
                        turn,
                        EventBody::ToolResult {
                            call_id: call_id.clone(),
                            status,
                        },
                    );
                }
            }
            // A speculative call whose result never reached the transcript:
            // resolve it as declined so the loop pairing stays truthful —
            // the I/O ran, the model never saw it.
            AgentEvent::SpeculationDiscarded { call_id, .. } => {
                if inner.pending_calls.remove(call_id) {
                    let turn = inner.open_turn;
                    inner.emit_at(
                        turn,
                        EventBody::ToolResult {
                            call_id: call_id.clone(),
                            status: contextgraph_trace::ToolStatus::Rejected,
                        },
                    );
                }
            }
            AgentEvent::FileChange { path, kind, diff } if kind.is_mutation() => {
                let effect_id = file_effect_id(path, *kind, diff.as_deref());
                inner.record_effect(effect_id, "file_write", None);
            }
            AgentEvent::Commit { sha, .. } => {
                inner.record_effect(format!("git-commit:{sha}"), "git_commit", None);
            }
            AgentEvent::Complete { .. } => inner.finish(SessionOutcome::Completed),
            _ => {}
        }
    }

    /// Close the recording if the stream's `Complete` event has not already.
    pub(crate) fn finish(&self, outcome: SessionOutcome) {
        self.lock().finish(outcome);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RecorderInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl RecorderInner {
    fn ensure_turn_open(&mut self) {
        if self.open_turn.is_none() {
            let turn = self.highest_turn + 1;
            self.highest_turn = turn;
            self.emit_at(Some(turn), EventBody::TurnStart);
            self.open_turn = Some(turn);
        }
    }

    fn record_effect(&mut self, effect_id: String, kind: &str, call_id: Option<String>) {
        // The recorder records reality: a re-performed intended-once effect
        // is journaled again and convicted by the oracle, never laundered.
        self.performed_effects.insert(effect_id.clone());
        let turn = self.open_turn;
        self.emit_at(
            turn,
            EventBody::SideEffect {
                effect_id,
                kind: kind.to_string(),
                call_id,
            },
        );
    }

    fn finish(&mut self, outcome: SessionOutcome) {
        if self.ended {
            return;
        }
        if self.open_turn.is_some() && outcome == SessionOutcome::Completed {
            let turn = self.open_turn.take();
            self.emit_at(turn, EventBody::TurnEnd);
        }
        self.emit(None, EventBody::SessionEnd { outcome });
        self.ended = true;
    }

    fn emit(&mut self, turn: Option<u64>, body: EventBody) {
        self.emit_at(turn, body)
    }

    fn emit_at(&mut self, turn: Option<u64>, body: EventBody) {
        let event = TraceEvent {
            seq: self.next_seq,
            at: rfc3339_utc_now(),
            session: self.session.clone(),
            turn,
            body,
        };
        let line = serde_json::to_string(&event).expect("trace events serialize");
        if let Err(error) = writeln!(self.file, "{line}").and_then(|()| self.file.flush()) {
            // Same discipline as the durable stream sink: an event the
            // journal did not accept must not be admitted to the run.
            eprintln!("arena journal write failed ({}): {error}", self.path);
            std::process::exit(ARENA_JOURNAL_FAILURE_EXIT);
        }
        self.next_seq += 1;
    }
}

/// The human label a block is cited under at its point of use. Recall frames
/// carry their own label; structural blocks get a stable kind-derived one —
/// never a raw id (§F3).
fn block_label(meta: Option<&BlockMeta>) -> String {
    let Some(meta) = meta else {
        return "context block".to_string();
    };
    if let Some(label) = &meta.citation_label
        && !label.trim().is_empty()
    {
        return label.clone();
    }
    match meta.kind {
        BlockKind::SystemPrefix => "system prefix",
        BlockKind::UserGoal => "task prompt (TASK.md)",
        BlockKind::RecalledFrame => "recalled context frame",
        BlockKind::AssistantText => "assistant text",
        BlockKind::ToolCall => "tool call",
        BlockKind::ToolResult => "tool result",
        BlockKind::Steered => "user steering",
        BlockKind::Summary => "history summary",
        BlockKind::Attachment => "attachment",
        BlockKind::Other => "context block",
    }
    .to_string()
}

/// An intended-once id for a file mutation: the same logical edit (same
/// path, same kind, same diff) replayed after a crash-resume collides — the
/// bug the effect-exactly-once oracle exists for — while a genuinely new
/// edit to the same file carries a new diff and a new id.
fn file_effect_id(path: &str, kind: stella_protocol::FileChangeKind, diff: Option<&str>) -> String {
    let kind = match kind {
        stella_protocol::FileChangeKind::Created => "created",
        stella_protocol::FileChangeKind::Modified => "modified",
        stella_protocol::FileChangeKind::Deleted => "deleted",
        stella_protocol::FileChangeKind::Read => "read",
    };
    match diff {
        Some(diff) => format!("file:{kind}:{path}#{:016x}", fnv1a(diff.as_bytes())),
        None => format!("file:{kind}:{path}"),
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

fn epoch_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

/// RFC 3339 UTC now — the trace timestamp profile (`SPEC.md` §F4), from the
/// civil-from-days algorithm so no clock dependency is added.
fn rfc3339_utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;
    use contextgraph_trace::{Journal, run_oracles};
    use stella_protocol::{CacheZone, ManifestEntry, ModelCallRole, ToolCall};

    fn manifest_event() -> AgentEvent {
        AgentEvent::StepManifest {
            turn_instance: 1,
            step: 0,
            role: ModelCallRole::default(),
            provider: "test".into(),
            model: "test-model".into(),
            blocks: vec![ManifestEntry {
                block_id: "blk_aaaaaaaaaaaaaaaaaaaaaaaa".into(),
                cache_zone: CacheZone::default(),
                token_cost: 120,
                resident_since_step: 0,
                message_index: 0,
            }],
            effective_budget_tokens: 4096,
            calibration_factor: 1.0,
            estimated_input_tokens: 120,
        }
    }

    fn registered_event() -> AgentEvent {
        AgentEvent::BlockRegistered {
            block_id: "blk_aaaaaaaaaaaaaaaaaaaaaaaa".into(),
            kind: BlockKind::SystemPrefix,
            origin: stella_protocol::BlockOrigin {
                turn_instance: 1,
                step: 0,
                call_id: None,
                memory_id: None,
            },
            token_cost: 120,
            content_digest: format!("sha256:{}", "a".repeat(64)),
            citation_label: None,
            content: None,
        }
    }

    fn drive_happy_path(recorder: &ArenaRecorder) {
        recorder.observe(&registered_event());
        recorder.observe(&manifest_event());
        recorder.observe(&AgentEvent::ToolStart {
            call: ToolCall {
                call_id: "call_1".into(),
                name: "write_file".into(),
                input: serde_json::json!({}),
            },
        });
        recorder.observe(&AgentEvent::FileChange {
            path: "src/main.rs".into(),
            kind: stella_protocol::FileChangeKind::Modified,
            diff: Some("+hello".into()),
        });
        recorder.observe(&AgentEvent::ToolResult {
            call_id: "call_1".into(),
            output: ToolOutput::Ok {
                content: "ok".into(),
            },
            duration_ms: 3,
            speculated: false,
        });
        recorder.observe(&AgentEvent::Complete {
            model: "test-model".into(),
            cost_usd: 0.0,
        });
    }

    fn parse(path: &Path) -> Journal {
        Journal::from_ndjson(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn a_recorded_run_passes_every_trace_oracle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.ndjson");
        let recorder = ArenaRecorder::open(&path, false, "test-model").unwrap();
        drive_happy_path(&recorder);

        let report = run_oracles(&parse(&path));
        assert!(report.passed(), "{report:?}");
    }

    #[test]
    fn a_crash_resume_continues_the_session_and_passes_the_oracles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.ndjson");
        {
            // First invocation, killed after the side effect: no result, no
            // turn end, no session end.
            let recorder = ArenaRecorder::open(&path, false, "test-model").unwrap();
            recorder.observe(&registered_event());
            recorder.observe(&manifest_event());
            recorder.observe(&AgentEvent::ToolStart {
                call: ToolCall {
                    call_id: "call_1".into(),
                    name: "write_file".into(),
                    input: serde_json::json!({}),
                },
            });
            recorder.observe(&AgentEvent::FileChange {
                path: "src/main.rs".into(),
                kind: stella_protocol::FileChangeKind::Modified,
                diff: Some("+hello".into()),
            });
        }
        {
            // The resumed invocation: a fresh model exchange (new call id),
            // and the workspace already holds the first edit, so the second
            // pass produces a *different* diff — a different intended-once
            // id. Blindly replaying the identical edit would collide instead
            // (asserted below) and be convicted by effect-exactly-once.
            let recorder = ArenaRecorder::open(&path, true, "test-model").unwrap();
            recorder.observe(&registered_event());
            recorder.observe(&manifest_event());
            recorder.observe(&AgentEvent::ToolStart {
                call: ToolCall {
                    call_id: "call_2".into(),
                    name: "write_file".into(),
                    input: serde_json::json!({}),
                },
            });
            recorder.observe(&AgentEvent::FileChange {
                path: "src/main.rs".into(),
                kind: stella_protocol::FileChangeKind::Modified,
                diff: Some("+hello world".into()),
            });
            recorder.observe(&AgentEvent::ToolResult {
                call_id: "call_2".into(),
                output: ToolOutput::Ok {
                    content: "ok".into(),
                },
                duration_ms: 3,
                speculated: false,
            });
            recorder.observe(&AgentEvent::Complete {
                model: "test-model".into(),
                cost_usd: 0.0,
            });
        }

        let journal = parse(&path);
        let report = run_oracles(&journal);
        assert!(report.passed(), "{report:?}");
        // Identical (path, kind, diff) ⇒ identical effect id — the collision
        // the oracle convicts when an agent replays instead of resuming.
        assert_eq!(
            file_effect_id(
                "src/main.rs",
                stella_protocol::FileChangeKind::Modified,
                Some("+hello")
            ),
            file_effect_id(
                "src/main.rs",
                stella_protocol::FileChangeKind::Modified,
                Some("+hello")
            ),
        );
    }

    #[test]
    fn an_existing_recording_without_resume_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.ndjson");
        {
            let _ = ArenaRecorder::open(&path, false, "m").unwrap();
        }
        let error = ArenaRecorder::open(&path, false, "m").unwrap_err();
        assert!(error.contains("--resume"), "{error}");
    }

    #[test]
    fn a_discarded_speculation_resolves_its_call_as_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.ndjson");
        let recorder = ArenaRecorder::open(&path, false, "m").unwrap();
        recorder.observe(&manifest_event());
        recorder.observe(&AgentEvent::ToolStart {
            call: ToolCall {
                call_id: "call_spec".into(),
                name: "grep".into(),
                input: serde_json::json!({}),
            },
        });
        recorder.observe(&AgentEvent::SpeculationDiscarded {
            call_id: "call_spec".into(),
            name: "grep".into(),
            reason: "attempt_failed".into(),
        });
        recorder.observe(&AgentEvent::Complete {
            model: "m".into(),
            cost_usd: 0.0,
        });
        let report = run_oracles(&parse(&path));
        assert!(report.passed(), "{report:?}");
    }
}
