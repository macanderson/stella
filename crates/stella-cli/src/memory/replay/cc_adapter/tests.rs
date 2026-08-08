//! The adapter's tests, and the privacy gate's witness.
//!
//! Everything here runs against **synthetic transcripts written by the test**,
//! never against the developer's real `~/.claude/projects`. That is not
//! squeamishness: a test that read the real corpus would pass or fail based on
//! what its author happened to have typed that month, and would put real user
//! content in front of CI. The one test that touches the real corpus is
//! [`the_real_corpus_adapts_when_opted_in`], which no-ops unless
//! `STELLA_REPLAY_CC_CORPUS` is set.

use super::*;
use crate::memory::replay::Replayer;

/// One transcript line, in the shape Claude Code writes.
fn user_line(at: &str, text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "timestamp": at,
        "message": {"content": text},
    })
    .to_string()
}

fn assistant_bash(at: &str, commands: &[&str]) -> String {
    let content: Vec<serde_json::Value> = commands
        .iter()
        .map(|command| {
            serde_json::json!({
                "type": "tool_use",
                "name": "Bash",
                "input": {"command": command},
            })
        })
        .collect();
    serde_json::json!({
        "type": "assistant",
        "timestamp": at,
        "message": {"content": content},
    })
    .to_string()
}

fn transcript(lines: &[String]) -> String {
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

// ---- the privacy witness (§7.1) --------------------------------------------

/// **THE privacy witness.** A secret-shaped string in a transcript never reaches
/// the trace.
///
/// The spec asks for "quarantined rather than stored". Under option (b) the
/// adapter derives no *statements* at all, so there is no proposal to quarantine
/// — the highest-risk field it touches is the shell command, and
/// `export ANTHROPIC_API_KEY=sk-ant-…` is an ordinary thing to have typed. The
/// gate is therefore applied where the risk actually is, and it is stricter than
/// quarantine: the command is **dropped**, not redacted-and-kept.
#[test]
fn a_secret_in_a_transcript_never_reaches_the_trace() {
    let secret = "sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let contents = transcript(&[
        user_line("2026-07-10T09:00:00Z", "wire up the provider"),
        assistant_bash(
            "2026-07-10T09:00:05Z",
            &[
                &format!("export ANTHROPIC_API_KEY={secret}"),
                "rg -n \"Provider\" crates/stella-model/src/anthropic.rs",
            ],
        ),
    ]);

    let session = session_from_transcript(std::path::Path::new("s1.jsonl"), &contents)
        .expect("the transcript adapts");
    let commands: Vec<&str> = session
        .turns
        .iter()
        .flat_map(|turn| turn.shell.iter())
        .map(|shell| shell.command.as_str())
        .collect();

    assert!(
        !commands.iter().any(|command| command.contains(secret)),
        "a credential reached the trace: {commands:?}"
    );
    assert!(
        !commands
            .iter()
            .any(|command| command.starts_with("export ANTHROPIC_API_KEY")),
        "a command that carried a credential must be dropped, not kept with a \
         hole in it: {commands:?}"
    );
    assert_eq!(
        commands,
        vec!["rg -n \"Provider\" crates/stella-model/src/anthropic.rs"],
        "the innocent command beside it must survive"
    );
}

/// The redactor's prefix list is a good filter, not a complete one, so the gate
/// is a drop rather than a substitution. Pinned as a property of `safe_command`
/// itself, because it is one line and it is the whole gate.
#[test]
fn a_command_whose_redaction_fired_is_dropped_entirely() {
    assert_eq!(
        safe_command("rg -n \"unwrap\" crates/stella-core/src/driver.rs"),
        Some("rg -n \"unwrap\" crates/stella-core/src/driver.rs".to_string())
    );
    assert_eq!(
        safe_command("curl -H 'Authorization: Bearer sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAA'"),
        None
    );
    assert_eq!(safe_command("   "), None);
}

/// The corpus root is unreachable without the opt-in, whatever else is true.
#[test]
fn the_real_corpus_is_unreachable_without_the_opt_in() {
    // SAFETY: single-threaded assertion on this test's own view of the
    // environment; the variable is restored before the test returns.
    let previous = std::env::var_os(OPT_IN);
    unsafe { std::env::remove_var(OPT_IN) };
    assert_eq!(
        opted_in_corpus_root(),
        None,
        "no default path may read the developer's transcripts"
    );
    if let Some(previous) = previous {
        unsafe { std::env::set_var(OPT_IN, previous) };
    }
}

// ---- adaptation ------------------------------------------------------------

/// A turn's boundary is a `user` line; its shell history is the Bash calls that
/// follow it.
#[test]
fn turns_are_bounded_by_user_lines_and_carry_the_bash_that_followed() {
    let contents = transcript(&[
        user_line("2026-07-10T09:00:00Z", "first ask"),
        assistant_bash("2026-07-10T09:00:05Z", &["rg -n \"a\" src/one.rs"]),
        assistant_bash("2026-07-10T09:00:09Z", &["rg -n \"b\" src/two.rs"]),
        user_line("2026-07-10T10:00:00Z", "second ask"),
        assistant_bash("2026-07-10T10:00:04Z", &["rg -n \"c\" src/three.rs"]),
    ]);
    let session =
        session_from_transcript(std::path::Path::new("abc.jsonl"), &contents).expect("adapts");

    assert_eq!(session.id, "abc");
    assert_eq!(session.task_id, "abc", "one transcript is one task");
    assert_eq!(session.turns.len(), 2);
    assert_eq!(session.turns[0].shell.len(), 2);
    assert_eq!(session.turns[1].shell.len(), 1);
    assert_eq!(session.turns[0].at, 1_783_674_000, "2026-07-10T09:00:00Z");
    assert!(session.turns[0].at < session.turns[1].at);
}

/// **Every adapted turn is `NotRecorded`** — the whole of option (b).
///
/// Claude Code transcripts carry no Stella reflection JSON, so any other arm
/// would be an assertion the source does not support. `Unreadable` in particular
/// would fabricate starvation and corrupt assertion 7's metric.
#[test]
fn every_adapted_turn_declines_to_invent_a_reflection() {
    let contents = transcript(&[
        user_line("2026-07-10T09:00:00Z", "an ask"),
        assistant_bash("2026-07-10T09:00:05Z", &["rg -n \"a\" src/one.rs"]),
    ]);
    let session =
        session_from_transcript(std::path::Path::new("s.jsonl"), &contents).expect("adapts");
    for turn in &session.turns {
        assert_eq!(
            turn.reflection,
            ScriptedReflection::NotRecorded,
            "the adapter must not invent the signal under test"
        );
        assert!(
            turn.transcript.is_empty(),
            "nothing reads the stub under option (b), so retaining user text \
             would be gratuitous"
        );
    }
}

/// A turn that ran no shell contributes nothing and is dropped, so the turn
/// count cannot be padded with turns the harness will never act on.
#[test]
fn a_turn_with_no_shell_history_is_dropped_rather_than_padding_the_count() {
    let contents = transcript(&[
        user_line("2026-07-10T09:00:00Z", "just chatting"),
        user_line("2026-07-10T09:05:00Z", "still chatting"),
    ]);
    assert!(matches!(
        session_from_transcript(std::path::Path::new("s.jsonl"), &contents),
        Err(AdapterError::Empty(_))
    ));
}

/// An unparseable line is skipped, not fatal: these files are written by another
/// program at a version we do not control.
#[test]
fn one_unreadable_line_does_not_discard_the_session() {
    let contents = transcript(&[
        user_line("2026-07-10T09:00:00Z", "an ask"),
        "{ this is not json".to_string(),
        assistant_bash("2026-07-10T09:00:05Z", &["rg -n \"a\" src/one.rs"]),
    ]);
    let session = session_from_transcript(std::path::Path::new("s.jsonl"), &contents)
        .expect("adapts despite the torn line");
    assert_eq!(session.turns.len(), 1);
    assert_eq!(session.turns[0].shell.len(), 1);
}

/// A backwards clock is clamped, not refused — one skewed line must not discard
/// a real session, and the trace loader requires non-decreasing time.
#[test]
fn a_backwards_timestamp_is_clamped_so_the_trace_still_loads() {
    let contents = transcript(&[
        user_line("2026-07-10T10:00:00Z", "later first"),
        assistant_bash("2026-07-10T10:00:01Z", &["rg -n \"a\" src/one.rs"]),
        user_line("2026-07-10T09:00:00Z", "earlier second"),
        assistant_bash("2026-07-10T09:00:01Z", &["rg -n \"b\" src/two.rs"]),
    ]);
    let session =
        session_from_transcript(std::path::Path::new("s.jsonl"), &contents).expect("adapts");
    assert!(
        session.turns[0].at <= session.turns[1].at,
        "clamped: {:?}",
        session.turns.iter().map(|t| t.at).collect::<Vec<_>>()
    );
}

/// The date arithmetic is the exact inverse of the one `stella-context`'s clock
/// uses, so the two agree at every boundary by construction.
#[test]
fn timestamps_round_trip_through_the_clocks_own_formatter() {
    for instant in [0i64, 1_600_000_000, 1_783_501_200, 1_582_934_400] {
        let rendered = stella_context::format_rfc3339(instant);
        assert_eq!(
            parse_rfc3339(&rendered),
            Some(instant),
            "{rendered} did not round trip"
        );
    }
    assert_eq!(parse_rfc3339("not a timestamp"), None);
    assert_eq!(parse_rfc3339("2026-13-01T00:00:00Z"), None);
}

/// A directory of transcripts becomes one trace, ordered by time and loadable.
#[tokio::test]
async fn a_directory_of_transcripts_becomes_a_replayable_trace() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("second.jsonl"),
        transcript(&[
            user_line("2026-07-20T09:00:00Z", "later session"),
            assistant_bash("2026-07-20T09:00:05Z", &["rg -n \"b\" src/two.rs"]),
        ]),
    )
    .expect("write");
    std::fs::write(
        dir.path().join("first.jsonl"),
        transcript(&[
            user_line("2026-07-10T09:00:00Z", "earlier session"),
            assistant_bash("2026-07-10T09:00:05Z", &["rg -n \"a\" src/one.rs"]),
        ]),
    )
    .expect("write");

    let trace = trace_from_directory(dir.path(), "two synthetic transcripts").expect("adapts");
    assert_eq!(trace.sessions.len(), 2);
    assert_eq!(
        trace.sessions[0].id, "first",
        "sessions are ordered by time, not by filename"
    );
    assert!(every_lesson_is_labelled(&trace));

    // The real proof: it survives the loader's own contract, then replays.
    let json = serde_json::to_string(&trace).expect("serialize");
    let reloaded = super::super::trace::Trace::parse(&json).expect("an adapted trace must load");
    assert_eq!(reloaded, trace);

    let workspace = tempfile::tempdir().expect("tempdir");
    let summary = Replayer::run(&trace, &workspace.path().join("adapted"))
        .await
        .expect("an adapted trace replays");
    assert_eq!(summary.turns, 2);
    assert_eq!(
        summary.turns_not_recorded, 2,
        "every adapted turn is counted as carrying no reflection, never as an \
         empty one: {summary:?}"
    );
    assert_eq!(
        summary.memories, 0,
        "option (b) feeds the foundry and the timeline, never the reflection path"
    );
    assert!(
        summary.model_errors.is_empty(),
        "a turn the source never reflected on is not a starved turn: {:?}",
        summary.model_errors
    );
}

/// A shape recurring across adapted sessions reaches the foundry — the half of
/// the corpus that is genuinely worth having.
#[tokio::test]
async fn a_recurring_shape_across_adapted_sessions_mints_a_tool() {
    let dir = tempfile::tempdir().expect("tempdir");
    for (index, (day, path)) in [
        ("10", "crates/stella-core/src/driver.rs"),
        ("11", "crates/stella-cli/src/agent.rs"),
        ("12", "crates/stella-model/src/openai.rs"),
    ]
    .iter()
    .enumerate()
    {
        std::fs::write(
            dir.path().join(format!("s{index}.jsonl")),
            transcript(&[
                user_line(&format!("2026-07-{day}T09:00:00Z"), "an ask"),
                assistant_bash(
                    &format!("2026-07-{day}T09:00:05Z"),
                    &[&format!("rg -n \"unwrap\" {path}")],
                ),
            ]),
        )
        .expect("write");
    }

    let trace = trace_from_directory(dir.path(), "three sessions, one shape").expect("adapts");
    let workspace = tempfile::tempdir().expect("tempdir");
    let summary = Replayer::run(&trace, &workspace.path().join("adapted"))
        .await
        .expect("replays");
    assert!(
        !summary.tools.is_empty(),
        "one shape with three argument sets, mined from adapted transcripts: \
         {summary:?}"
    );
}

// ---- the real corpus, opt-in only ------------------------------------------

/// Adapt the developer's real Claude Code corpus. **No-ops unless
/// `STELLA_REPLAY_CC_CORPUS` is set**, so a plain `cargo test` — and every CI
/// run — never touches it.
///
/// Nothing derived here is written to disk, printed in full, or committed. The
/// assertions are counts and the privacy property; the committed CI corpus stays
/// synthetic, permanently.
#[tokio::test]
async fn the_real_corpus_adapts_when_opted_in() {
    let Some(root) = opted_in_corpus_root() else {
        return;
    };
    let projects = project_directories(&root);
    println!("corpus: {} project director(ies)", projects.len());

    let mut adapted = 0usize;
    let mut turns = 0usize;
    let mut commands = 0usize;
    for (name, path) in projects.iter().take(20) {
        let Ok(trace) = trace_from_directory(path, name) else {
            continue;
        };
        assert!(
            every_lesson_is_labelled(&trace),
            "an unlabelled derived lesson reached a trace built from {name}"
        );
        // The loader's contract must hold for a real, uncontrolled source too —
        // this is the assertion that catches a transcript shape the adapter
        // mishandles, rather than a synthetic one it was written against.
        let json = serde_json::to_string(&trace).expect("serialize");
        super::super::trace::Trace::parse(&json)
            .unwrap_or_else(|e| panic!("adapted trace for {name} does not load: {e}"));

        adapted += 1;
        turns += trace.sessions.iter().map(|s| s.turns.len()).sum::<usize>();
        commands += trace
            .turns()
            .map(|(_, turn)| turn.shell.len())
            .sum::<usize>();
    }
    println!("adapted {adapted} project(s): {turns} turn(s), {commands} command(s)");
    assert!(adapted > 0, "the opt-in was set but nothing adapted");
}
