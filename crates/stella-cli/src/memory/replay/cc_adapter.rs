//! Claude Code transcripts → the trace format. **Local-only, opt-in, and
//! deliberately half of what the corpus could supply.**
//!
//! `doc:trace-replay-learning-harness` §7. This is the one non-synthetic corpus
//! available: one engineer, a handful of repositories, the same conventions
//! re-encountered across thousands of sessions — recurrence density no synthetic
//! fixture will match.
//!
//! ## What it does not do, and why that is the point
//!
//! **Claude Code transcripts contain no Stella reflection JSON.** There is
//! nothing in them to script a lessons array from, so the adapter must either
//! derive lessons from the transcript text or decline to. §7.2 puts the choice
//! plainly:
//!
//! - **(a) Synthesize lessons.** This fabricates the exact signal under test —
//!   the harness would then be measuring the adapter's lesson-invention
//!   heuristic, not Stella's learning.
//! - **(b) Use the corpus for shell history, session and turn boundaries, and
//!   timing only**, feeding the tool foundry against thousands of real commands
//!   while the reflection path stays on synthetic scripted lessons.
//!
//! **This implements (b).** Every turn it emits carries
//! [`ScriptedReflection::NotRecorded`], which the replayer honours by never
//! reaching the model boundary at all. The alternative spellings are all
//! dishonest: an empty `Lessons` array asserts the model had nothing to say, and
//! `Unreadable` asserts it said something unparseable — the second would also
//! fabricate starvation and corrupt assertion 7's own metric.
//!
//! If (a) is ever built, its lessons must be stamped [`Provenance::Derived`], so
//! no downstream number can silently mix invented lessons with recorded ones.
//! The field exists already; nothing sets it yet.
//!
//! ## The corpus is thirty days, not months
//!
//! Measured 2026-08-08: 3,822 transcripts across 485 project directories,
//! spanning 2026-07-09 → 2026-08-08. The *months* axis does not come from the
//! corpus — it comes from the replayer's clock, which can dilate thirty days of
//! real recurrence across a simulated six months at will. It is also a rolling
//! window under Claude Code's own retention, so the span is not reproducible
//! across machines or across time. Say it that way; claiming a months-long
//! corpus we do not have is the kind of small dishonesty that costs more than it
//! buys.
//!
//! ## The privacy gate (§7.1) — hard, per invariant 3
//!
//! - **Local-only, opt-in.** Reached only by an explicitly-enabled test; never a
//!   default path, never a shipped command, never run by a plain `cargo test`.
//! - **Nothing derived is committed.** The committed CI corpus stays synthetic,
//!   permanently. A derived trace belongs in gitignored scratch, and the
//!   `no-scratch` guard fails the gate on a committed derivative on its own.
//! - **Every command is redacted before it is written.** Transcript text is user
//!   content and model output — exactly what `origin_is_untrusted` classifies —
//!   and a shell command is the highest-risk field in it, because
//!   `export TOKEN=sk-…` and `curl -H "Authorization: Bearer …"` are ordinary
//!   things to type. [`redact_secrets`] runs on every command, and a command
//!   that still looks executable-with-a-credential after redaction is dropped
//!   rather than emitted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use stella_core::redact::redact_secrets;

use super::trace::{
    Provenance, ScriptedReflection, TRACE_VERSION, Trace, TraceMessage, TraceRole, TraceSession,
    TraceShell, TraceTurn, WorkspaceSeed,
};

/// How a transcript line is shaped. Only the fields the adapter reads.
///
/// Observed line types in the corpus: `assistant`, `user`, `attachment`,
/// `last-prompt`, `mode`, `permission-mode`, `agent-setting`, `worktree-state`,
/// `agent-name`, `ai-title`, `relocated`, `file-history-snapshot`,
/// `queue-operation`, `system`, `pr-link`, `file-history-delta`, `started`,
/// `result`, `bridge-session`, `custom-title`. The adapter reads `user` and
/// `assistant`; everything else is harness bookkeeping.
#[derive(Debug, serde::Deserialize)]
struct Line {
    #[serde(default)]
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    message: Option<Message>,
}

#[derive(Debug, serde::Deserialize)]
struct Message {
    #[serde(default)]
    content: serde_json::Value,
}

/// What one transcript line contributed.
#[derive(Debug, Default)]
struct Extracted {
    text: String,
    commands: Vec<String>,
}

/// Why a transcript could not be adapted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AdapterError {
    Unreadable(String),
    /// The file parsed but carried nothing a replay could use.
    Empty(PathBuf),
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterError::Unreadable(why) => write!(f, "transcript unreadable: {why}"),
            AdapterError::Empty(path) => {
                write!(f, "{} carried no usable turns", path.display())
            }
        }
    }
}

/// Adapt one Claude Code transcript into one trace session.
///
/// `task_id` is the transcript's own identity: one Claude Code session is one
/// sitting, which is the closest honest approximation of one task the source
/// offers. It is an approximation and is labelled as one — a long session
/// genuinely spans several tasks, and the error runs toward *under*-counting
/// distinct tasks, which is the safe direction for a promotion gate.
pub(crate) fn session_from_transcript(
    path: &Path,
    contents: &str,
) -> Result<TraceSession, AdapterError> {
    let id = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string());

    let mut turns: Vec<TraceTurn> = Vec::new();
    let mut pending: Option<TraceTurn> = None;

    for line in contents.lines().filter(|line| !line.trim().is_empty()) {
        // A line the adapter cannot parse is skipped, not fatal: these files are
        // written by a different program at a version we do not control, and one
        // unrecognized line must not discard a whole session's shell history.
        let Ok(parsed) = serde_json::from_str::<Line>(line) else {
            continue;
        };
        let at = parsed.timestamp.as_deref().and_then(parse_rfc3339);

        match parsed.kind.as_str() {
            "user" => {
                if let Some(turn) = pending.take() {
                    turns.push(turn);
                }
                let extracted = extract(&parsed);
                pending = Some(TraceTurn {
                    at: at.unwrap_or_default(),
                    prompt: extracted.text.chars().take(200).collect(),
                    // The transcript stub stays empty under option (b): nothing
                    // reads it, because the turn never reaches the reflection
                    // prompt. Carrying user text into a trace we did not need
                    // would be gratuitous retention of exactly the content the
                    // privacy gate exists to keep local.
                    transcript: Vec::new(),
                    reflection: ScriptedReflection::NotRecorded,
                    shell: Vec::new(),
                    succeeded: true,
                    forget: Vec::new(),
                    removed_files: Vec::new(),
                });
            }
            "assistant" => {
                let extracted = extract(&parsed);
                if let Some(turn) = pending.as_mut() {
                    for command in extracted.commands {
                        if let Some(safe) = safe_command(&command) {
                            turn.shell.push(TraceShell {
                                command: safe,
                                succeeded: true,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(turn) = pending.take() {
        turns.push(turn);
    }

    // A turn with no shell history contributes nothing under option (b) — it has
    // no reflection to replay either — so it is dropped rather than padding the
    // turn count with turns the harness will not act on. A turn count that
    // includes turns nothing can be learned from is a number that flatters.
    turns.retain(|turn| !turn.shell.is_empty());
    if turns.is_empty() {
        return Err(AdapterError::Empty(path.to_path_buf()));
    }

    // Timestamps must be non-decreasing (`Trace::check`). A transcript whose
    // clock jumped backwards is repaired by clamping rather than refused: it is
    // a real file we do not control, and one skewed line should not discard a
    // session. Clamping is visible in the result; refusing would not be.
    let mut previous = 0i64;
    for turn in &mut turns {
        if turn.at < previous {
            turn.at = previous;
        }
        previous = turn.at;
    }

    Ok(TraceSession {
        id: id.clone(),
        task_id: id,
        turns,
    })
}

/// Adapt a directory of transcripts into one trace, oldest session first.
///
/// `description` must name the corpus — a summary that cannot say what it was
/// built from is not reportable (§11).
pub(crate) fn trace_from_directory(root: &Path, description: &str) -> Result<Trace, AdapterError> {
    let entries = std::fs::read_dir(root)
        .map_err(|e| AdapterError::Unreadable(format!("{}: {e}", root.display())))?;
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    // Sorted so the adapter is deterministic: `read_dir` order is filesystem
    // order, which differs between machines and even between runs.
    paths.sort();

    let mut sessions: Vec<TraceSession> = Vec::new();
    for path in &paths {
        let Ok(contents) = std::fs::read_to_string(path) else {
            continue;
        };
        if let Ok(session) = session_from_transcript(path, &contents) {
            sessions.push(session);
        }
    }
    // A trace's turns must be non-decreasing across sessions too, so order
    // sessions by their first instant rather than by filename.
    sessions.sort_by_key(|session| session.turns.first().map(|t| t.at).unwrap_or_default());
    if sessions.is_empty() {
        return Err(AdapterError::Empty(root.to_path_buf()));
    }
    let mut previous = 0i64;
    for session in &mut sessions {
        for turn in &mut session.turns {
            if turn.at < previous {
                turn.at = previous;
            }
            previous = turn.at;
        }
    }

    Ok(Trace {
        version: TRACE_VERSION,
        description: description.to_string(),
        workspace: WorkspaceSeed::default(),
        sessions,
    })
}

/// Pull text and Bash commands out of one line's message content.
fn extract(line: &Line) -> Extracted {
    let mut out = Extracted::default();
    let Some(message) = &line.message else {
        return out;
    };
    match &message.content {
        serde_json::Value::String(text) => out.text = text.clone(),
        serde_json::Value::Array(blocks) => {
            for block in blocks {
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            out.text.push_str(text);
                        }
                    }
                    Some("tool_use") => {
                        if block.get("name").and_then(|n| n.as_str()) == Some("Bash")
                            && let Some(command) =
                                block.pointer("/input/command").and_then(|c| c.as_str())
                        {
                            out.commands.push(command.to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    out
}

/// Redact a command, or drop it entirely.
///
/// Two gates, in order. First [`redact_secrets`] replaces every recognized
/// credential. Then, if the redaction *fired*, the command is dropped rather
/// than emitted with a `[redacted]` hole in it.
///
/// Dropping rather than keeping the redacted form is the conservative choice and
/// costs almost nothing: a command that carried a credential is a one-off by
/// nature (an `export`, a `curl` with a bearer token), so it was never going to
/// be a recurring shape worth minting a tool from. What it *would* have done is
/// put a partly-redacted credential context into a file, and the redactor's
/// prefix list is a good filter rather than a complete one — a token shape it
/// has not seen would survive. The foundry loses nothing; the gate keeps its
/// margin.
fn safe_command(command: &str) -> Option<String> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }
    let redaction = redact_secrets(trimmed);
    if redaction.redacted {
        return None;
    }
    Some(redaction.text)
}

/// Parse an RFC-3339 timestamp to Unix seconds.
///
/// Hand-rolled to the exact shape Claude Code writes
/// (`YYYY-MM-DDTHH:MM:SS[.mmm]Z`) rather than pulling in a date crate for one
/// caller. Anything else returns `None` and the turn falls back to its
/// neighbours' clamp — a wrong timestamp is worse than a missing one, because
/// the trace's whole timeline is derived from these.
fn parse_rfc3339(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let num = |from: usize, to: usize| text.get(from..to)?.parse::<i64>().ok();
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since 1970-01-01, by Howard Hinnant's `days_from_civil` — the exact
/// inverse of the `civil_from_days` `stella-context`'s clock already uses, so
/// the two agree at every boundary by construction rather than by testing.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The environment variable that opts a local run in.
///
/// A variable rather than a flag because there is no shipped command to put a
/// flag on: the adapter is reachable only from a test, deliberately (§7.1).
pub(crate) const OPT_IN: &str = "STELLA_REPLAY_CC_CORPUS";

/// Where Claude Code keeps its transcripts, if the caller opted in.
///
/// `None` unless [`OPT_IN`] is set, so no default path ever reads the user's
/// transcripts — including in CI, where the variable is never set and the
/// directory does not exist anyway.
/// The home anchor comes from `crate::paths`, never from the environment
/// directly — the guard at `paths.rs`'s `nothing_else_in_this_crate_reads_a_home_out_of_the_environment`
/// exists so a test can redirect one without mutating process-global state
/// (#1139), and it caught this on the first run.
pub(crate) fn opted_in_corpus_root() -> Option<PathBuf> {
    if std::env::var_os(OPT_IN).is_none() {
        return None;
    }
    let root = crate::paths::home()?.join(".claude").join("projects");
    root.is_dir().then_some(root)
}

/// Every project directory under the corpus root, sorted.
pub(crate) fn project_directories(root: &Path) -> BTreeMap<String, PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return BTreeMap::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy().into_owned();
            Some((name, path))
        })
        .collect()
}

/// Assert nothing in this trace is labelled as recorded when it was derived.
///
/// A cheap total check rather than a promise: under option (b) the adapter emits
/// no lessons at all, so any `Lessons` arm reaching a trace built here would
/// mean option (a) was added without stamping [`Provenance::Derived`] — the one
/// mistake §7.2 says must be impossible to make quietly.
pub(crate) fn every_lesson_is_labelled(trace: &Trace) -> bool {
    trace.turns().all(|(_, turn)| match &turn.reflection {
        ScriptedReflection::Lessons { provenance, .. } => *provenance == Provenance::Derived,
        _ => true,
    })
}

/// Unused under option (b); kept so a future option (a) has one obvious place to
/// build a labelled lesson rather than inventing a second spelling.
#[allow(dead_code)]
pub(crate) fn derived_message(role: TraceRole, content: &str) -> TraceMessage {
    TraceMessage {
        role,
        content: content.to_string(),
    }
}

#[cfg(test)]
mod tests;
