//! Process-group tool unit tests: `start_process` / `read_output` /
//! `clear_output` / `send_stdin` / `stop_process` lifecycle, buffering,
//! reaping, and credential/env-hygiene coverage.
//!
//! Split out of `process.rs` (#2699's `clear_output` split pushed the file
//! over the 1500-line gate) so the module that ships the process tools is
//! not dominated by the module that checks them — same rationale as
//! `registry.rs` / `registry/tests.rs`.

use super::*;

fn tools() -> (ProcessTableHandle, std::path::PathBuf) {
    (Arc::default(), std::env::temp_dir())
}

async fn start(table: &ProcessTableHandle, root: &std::path::Path, argv: &[&str]) -> String {
    start_with_scratch(table, root, argv, None).await
}

async fn start_with_scratch(
    table: &ProcessTableHandle,
    root: &std::path::Path,
    argv: &[&str],
    scratch: Option<std::path::PathBuf>,
) -> String {
    let out = StartProcess {
        handle: table.clone(),
        scratch,
    }
    .execute(&serde_json::json!({ "argv": argv }), root)
    .await;
    match out {
        ToolOutput::Ok { content } => content
            .split_whitespace()
            .find(|w| w.starts_with("proc-"))
            .expect("handle in start output")
            .to_string(),
        ToolOutput::Error { message } => panic!("start failed: {message}"),
    }
}

#[test]
fn output_buffer_caps_and_counts_dropped_bytes() {
    let mut buf = OutputBuffer::default();
    buf.push(&vec![b'a'; MAX_BUFFER_BYTES]);
    buf.push(&[b'z'; 10]);
    let (bytes, dropped) = buf.take();
    assert!(bytes.len() <= MAX_BUFFER_BYTES, "capped at the max");
    // An overflow reclaims a slack quantum, so `dropped` exceeds the
    // strict minimum — but nothing vanishes unaccounted: every byte
    // pushed is either still buffered or counted as dropped, which is
    // what makes the gap flag on the next read honest.
    assert_eq!(
        bytes.len() as u64 + dropped,
        MAX_BUFFER_BYTES as u64 + 10,
        "every pushed byte is either kept or counted"
    );
    assert!(dropped >= 10, "the oldest bytes were dropped: {dropped}");
    assert!(bytes.ends_with(b"zzzzzzzzzz"), "newest bytes survive");
    // Take drains: a second take is empty with no drop count.
    assert_eq!(buf.take(), (Vec::new(), 0));
}

/// The point of the slack: a chatty stream at the cap must not memmove
/// the whole buffer per chunk. One drain per quantum, not per push.
#[test]
fn a_buffer_at_the_cap_drains_a_quantum_not_every_chunk() {
    let mut buf = OutputBuffer::default();
    buf.push(&vec![b'a'; MAX_BUFFER_BYTES]);
    let before = buf.data.len();
    buf.push(&[b'z'; 8192]);
    assert!(
        buf.data.len() < before,
        "the overflow reclaimed headroom: {} → {}",
        before,
        buf.data.len()
    );
    // The next several chunks fit in the reclaimed slack and drop nothing.
    let after_first_drain = buf.dropped;
    for _ in 0..4 {
        buf.push(&[b'z'; 8192]);
    }
    assert_eq!(
        buf.dropped, after_first_drain,
        "chunks inside the slack cost no further drops"
    );
}

#[tokio::test]
async fn empty_argv_is_a_named_error() {
    let (table, root) = tools();
    let out = StartProcess {
        handle: table,
        scratch: None,
    }
    .execute(&serde_json::json!({"argv": []}), &root)
    .await;
    match out {
        ToolOutput::Error { message } => assert!(message.contains("argv"), "{message}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn start_process_cannot_inherit_registered_host_credentials() {
    let secret_name = "STELLA_TEST_PROCESS_VERIFY_SECRET";
    let token_name = "STELLA_TEST_PROCESS_BEARER";
    crate::exec::register_sensitive_env_names([secret_name, token_name]);
    unsafe {
        std::env::set_var(secret_name, "process-verification-secret");
        std::env::set_var(token_name, "process-bearer-secret");
    }
    let (table, root) = tools();
    let handle = start(&table, &root, &["env"]).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let output = ReadOutput(table)
        .execute(&serde_json::json!({"handle": handle}), &root)
        .await;
    unsafe {
        std::env::remove_var(secret_name);
        std::env::remove_var(token_name);
    }
    let ToolOutput::Ok { content } = output else {
        panic!("cannot read process output: {output:?}");
    };
    for forbidden in [
        secret_name,
        token_name,
        "process-verification-secret",
        "process-bearer-secret",
    ] {
        assert!(!content.contains(forbidden), "credential leaked: {content}");
    }
}

/// Witness for #2699: `clear_output` did not exist before this split —
/// `read_output`'s only way to discard buffered output was its `clear`
/// mode flag. This proves the new single-purpose tool actually
/// discards: write output via a started process, `clear_output` it,
/// then `read_output` sees nothing.
#[cfg(unix)]
#[tokio::test]
async fn clear_output_discards_buffered_output_before_the_next_read() {
    let (table, root) = tools();
    let handle = start(
        &table,
        &root,
        &["sh", "-c", "echo written_before_clear; sleep 30"],
    )
    .await;

    // Wait until the write has actually landed in the buffer, or
    // clearing it would prove nothing.
    let mut buffered = false;
    for _ in 0..250 {
        if table
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entries
            .get(&handle)
            .is_some_and(|e| {
                !e.output
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .data
                    .is_empty()
            })
        {
            buffered = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(buffered, "the process never produced buffered output");

    let cleared = ClearOutput(table.clone())
        .execute(&serde_json::json!({"handle": handle}), &root)
        .await;
    let ToolOutput::Ok { content } = cleared else {
        panic!("clear_output failed: {cleared:?}");
    };
    assert!(content.contains("cleared"), "{content}");
    assert!(content.contains("buffered bytes"), "{content}");

    let after = ReadOutput(table.clone())
        .execute(&serde_json::json!({"handle": handle}), &root)
        .await;
    let ToolOutput::Ok { content } = after else {
        panic!("read_output after clear failed: {after:?}");
    };
    assert!(
        content.contains("[no new output]"),
        "read_output must see nothing after clear_output drained the buffer: {content}"
    );

    let _ = StopProcess(table)
        .execute(&serde_json::json!({"handle": handle}), &root)
        .await;
}

/// #2699: `read_output`'s `clear` field is removed, not silently
/// ignored. A caller still sending `clear: true` gets a named error
/// pointing at the replacement tool, and the buffer it would have
/// discarded is left untouched.
#[cfg(unix)]
#[tokio::test]
async fn read_output_clear_true_is_a_named_deprecation_error() {
    let (table, root) = tools();
    let handle = start(&table, &root, &["sh", "-c", "echo still_here; sleep 30"]).await;

    let mut buffered = false;
    for _ in 0..250 {
        if table
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entries
            .get(&handle)
            .is_some_and(|e| {
                !e.output
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .data
                    .is_empty()
            })
        {
            buffered = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(buffered, "the process never produced buffered output");

    let out = ReadOutput(table.clone())
        .execute(&serde_json::json!({"handle": handle, "clear": true}), &root)
        .await;
    match out {
        ToolOutput::Error { message } => {
            assert!(message.contains("clear_output"), "{message}");
            assert!(message.contains("removed"), "{message}");
        }
        other => panic!("clear: true must be a named error, not: {other:?}"),
    }

    // The deprecated call must not have consumed the buffer it refused
    // to act on — a plain read still sees the output.
    let read = ReadOutput(table.clone())
        .execute(&serde_json::json!({"handle": handle}), &root)
        .await;
    let ToolOutput::Ok { content } = read else {
        panic!("read_output failed: {read:?}");
    };
    assert!(content.contains("still_here"), "{content}");

    let _ = StopProcess(table)
        .execute(&serde_json::json!({"handle": handle}), &root)
        .await;
}

/// `start_process` used to apply only the credential scrub — a
/// hook-exported GIT_DIR or forced-color override reached the child.
#[cfg(unix)]
#[tokio::test]
async fn start_process_scrubs_git_repo_and_forced_color_env() {
    let _fixture = crate::subprocess_env::test_support::SpawnHygieneFixture::install();
    let (table, root) = tools();
    let command = format!(
        "{}; echo; sleep 30",
        crate::subprocess_env::test_support::SPAWN_HYGIENE_PROBE_COMMAND
    );
    let handle = start(&table, &root, &["sh", "-c", &command]).await;

    let mut observed = String::new();
    for _ in 0..50 {
        let out = ReadOutput(table.clone())
            .execute(&serde_json::json!({"handle": handle}), &root)
            .await;
        let ToolOutput::Ok { content } = out else {
            panic!("read_output failed");
        };
        // Exact-line match: the header echoes the probe command itself,
        // which also contains `|`.
        if let Some(line) = content.lines().find(|line| *line == "unset|unset") {
            observed = line.to_string();
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    crate::subprocess_env::test_support::assert_spawn_hygiene_scrubbed(&observed);

    let stopped = StopProcess(table)
        .execute(&serde_json::json!({"handle": handle}), &root)
        .await;
    assert!(!stopped.is_error(), "{stopped:?}");
}

#[tokio::test]
async fn unknown_handles_are_named_errors_on_every_tool() {
    let (table, root) = tools();
    for out in [
        ReadOutput(table.clone())
            .execute(&serde_json::json!({"handle": "proc-9"}), &root)
            .await,
        ClearOutput(table.clone())
            .execute(&serde_json::json!({"handle": "proc-9"}), &root)
            .await,
        SendStdin(table.clone())
            .execute(&serde_json::json!({"handle": "proc-9", "text": "x"}), &root)
            .await,
        StopProcess(table.clone())
            .execute(&serde_json::json!({"handle": "proc-9"}), &root)
            .await,
    ] {
        match out {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("unknown process handle `proc-9`"),
                    "{message}"
                )
            }
            other => panic!("{other:?}"),
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn cat_echoes_stdin_and_stop_reports_the_lifecycle() {
    let (table, root) = tools();
    let handle = start(&table, &root, &["cat"]).await;

    let write = SendStdin(table.clone())
        .execute(
            &serde_json::json!({"handle": handle, "text": "hello_process\n"}),
            &root,
        )
        .await;
    assert!(!write.is_error(), "{write:?}");

    // Poll until the pump has delivered the echo (bounded).
    let mut echoed = String::new();
    for _ in 0..50 {
        let out = ReadOutput(table.clone())
            .execute(&serde_json::json!({"handle": handle}), &root)
            .await;
        let ToolOutput::Ok { content } = out else {
            panic!("read_output failed");
        };
        echoed.push_str(&content);
        if echoed.contains("hello_process") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(echoed.contains("hello_process"), "{echoed}");
    assert!(echoed.contains("running"), "{echoed}");

    // Stop: cat exits on stdin EOF/SIGTERM; afterwards the state is
    // exited and stdin writes are refused.
    let stopped = StopProcess(table.clone())
        .execute(&serde_json::json!({"handle": handle}), &root)
        .await;
    assert!(!stopped.is_error(), "{stopped:?}");
    let after = SendStdin(table.clone())
        .execute(&serde_json::json!({"handle": handle, "text": "x"}), &root)
        .await;
    match after {
        ToolOutput::Error { message } => {
            assert!(
                message.contains("exited") || message.contains("stdin is closed"),
                "{message}"
            )
        }
        other => panic!("stdin to a stopped process must fail: {other:?}"),
    }
    let status = ReadOutput(table)
        .execute(&serde_json::json!({"handle": handle}), &root)
        .await;
    let ToolOutput::Ok { content } = status else {
        panic!("read after stop must still work");
    };
    assert!(content.contains("exited"), "{content}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_naturally_exited_process_reports_its_code() {
    let (table, root) = tools();
    let handle = start(&table, &root, &["true"]).await;
    // Wait for the exit to be observable.
    for _ in 0..50 {
        let out = ReadOutput(table.clone())
            .execute(&serde_json::json!({"handle": handle}), &root)
            .await;
        let ToolOutput::Ok { content } = out else {
            panic!("read_output failed");
        };
        if content.contains("exited (code 0)") {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("`true` never reported exit");
}

/// An exited process used to keep its `Child` and up to 200 KB of buffer
/// until the whole registry dropped. Once its output has reached the
/// model there is nothing left to hold — but the handle must keep
/// answering, or a model polling a finished server sees "unknown handle"
/// and concludes something is wrong.
#[cfg(unix)]
#[tokio::test]
async fn a_drained_exited_process_is_reaped_but_its_handle_still_answers() {
    let (table, root) = tools();
    let handle = start(&table, &root, &["true"]).await;

    let mut reaped = false;
    for _ in 0..100 {
        let out = ReadOutput(table.clone())
            .execute(&serde_json::json!({"handle": handle}), &root)
            .await;
        assert!(!out.is_error(), "{out:?}");
        if table
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entries
            .is_empty()
        {
            reaped = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(reaped, "the exited process was never reaped");

    let after = ReadOutput(table.clone())
        .execute(&serde_json::json!({"handle": handle}), &root)
        .await;
    let ToolOutput::Ok { content } = after else {
        panic!("a reaped handle must still answer: {after:?}");
    };
    assert!(content.contains("exited (code 0)"), "{content}");
    assert!(content.contains("reaped"), "{content}");

    // stop_process on a reaped handle is a no-op, not an error.
    let stop = StopProcess(table)
        .execute(&serde_json::json!({"handle": handle}), &root)
        .await;
    match stop {
        ToolOutput::Ok { content } => assert!(content.contains("already exited"), "{content}"),
        other => panic!("{other:?}"),
    }
}

/// Nothing used to cap how many children `start_process` could create.
#[cfg(unix)]
#[tokio::test]
async fn the_live_process_cap_refuses_a_runaway_and_a_stop_frees_a_slot() {
    let (table, root) = tools();
    let mut handles = Vec::new();
    for _ in 0..MAX_LIVE_PROCESSES {
        handles.push(start(&table, &root, &["cat"]).await);
    }

    let over = StartProcess {
        handle: table.clone(),
        scratch: None,
    }
    .execute(&serde_json::json!({"argv": ["cat"]}), &root)
    .await;
    match over {
        ToolOutput::Error { message } => {
            assert!(
                message.contains(&MAX_LIVE_PROCESSES.to_string()),
                "{message}"
            );
            assert!(message.contains("stop_process"), "{message}");
        }
        other => panic!(
            "the {}th start must be refused: {other:?}",
            MAX_LIVE_PROCESSES + 1
        ),
    }

    let stopped = StopProcess(table.clone())
        .execute(&serde_json::json!({"handle": handles[0]}), &root)
        .await;
    assert!(!stopped.is_error(), "{stopped:?}");
    let again = StartProcess {
        handle: table.clone(),
        scratch: None,
    }
    .execute(&serde_json::json!({"argv": ["cat"]}), &root)
    .await;
    assert!(
        !again.is_error(),
        "a freed slot admits a new process: {again:?}"
    );

    for handle in handles.iter().skip(1) {
        let _ = StopProcess(table.clone())
            .execute(&serde_json::json!({"handle": handle}), &root)
            .await;
    }
}

/// Exited entries are exempt from the live cap, so before the exited-entry
/// bound a start/exit loop that never called read_output pinned a `Child`
/// and up to 200 KB of buffer per iteration for the rest of the session.
#[cfg(unix)]
#[tokio::test]
async fn exited_unread_entries_are_evicted_oldest_first_never_running_ones() {
    let (table, root) = tools();
    // A process that stays alive across every eviction below.
    let keeper = start(&table, &root, &["cat"]).await;

    let mut echo_handles = Vec::new();
    for _ in 0..MAX_EXITED_ENTRIES + 2 {
        let handle = start(&table, &root, &["sh", "-c", "echo unread_output"]).await;
        // Wait until the exit is observable and the pump has delivered
        // the output, so an eviction demonstrably discards unread bytes.
        for _ in 0..250 {
            let ready = {
                let mut t = table.lock().unwrap_or_else(|p| p.into_inner());
                match t.entries.get_mut(&handle) {
                    Some(e) => {
                        e.poll_exit().is_some()
                            && !e
                                .output
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .data
                                .is_empty()
                    }
                    None => true,
                }
            };
            if ready {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        echo_handles.push(handle);
    }

    // The next start enforces the cap at the table's growth point.
    let extra = start(&table, &root, &["cat"]).await;
    {
        let mut t = table.lock().unwrap_or_else(|p| p.into_inner());
        let mut exited = 0;
        for entry in t.entries.values_mut() {
            if entry.poll_exit().is_some() {
                exited += 1;
            }
        }
        assert!(
            exited <= MAX_EXITED_ENTRIES,
            "{exited} exited entries retained beyond the {MAX_EXITED_ENTRIES} cap"
        );
        assert!(
            t.entries.contains_key(&keeper),
            "a still-running process must never be evicted"
        );
        assert!(
            !t.entries.contains_key(&echo_handles[0]),
            "the oldest exited entry must be evicted first"
        );
        assert!(
            t.entries.contains_key(echo_handles.last().unwrap()),
            "the newest exited entry survives"
        );
    }

    // The evicted handle still answers, and the discard is reported.
    let evicted = ReadOutput(table.clone())
        .execute(&serde_json::json!({"handle": echo_handles[0]}), &root)
        .await;
    let ToolOutput::Ok { content } = evicted else {
        panic!("an evicted handle must still answer: {evicted:?}");
    };
    assert!(content.contains("exited (code 0)"), "{content}");
    assert!(content.contains("discarded"), "{content}");

    for handle in [keeper, extra] {
        let _ = StopProcess(table.clone())
            .execute(&serde_json::json!({"handle": handle}), &root)
            .await;
    }
}

/// Witness test: STELLA_SCRATCH is injected into spawned processes.
/// This test must FAIL on a version that doesn't wire the scratch injection,
/// and PASS once the fix is in place.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_process_injects_stella_scratch_into_spawned_process() {
    let (table, root) = tools();
    // Create a scratch dir for injection.
    let scratch = crate::scratch::ScratchDir::new().expect("scratch dir creation");
    let scratch_path = scratch.path().to_path_buf();
    let expected_path = scratch_path.to_string_lossy().to_string();

    // Start a process that outputs the STELLA_SCRATCH environment variable.
    // Use printf to ensure output is flushed immediately.
    let command = format!("printf 'STELLA_SCRATCH=%s\\n' \"$STELLA_SCRATCH\"; sleep 30");
    let handle = start_with_scratch(&table, &root, &["sh", "-c", &command], Some(scratch_path.clone()))
        .await;

    // Poll read_output until we see the STELLA_SCRATCH value in the output.
    let mut observed = String::new();
    for attempt in 0..100 {
        let out = ReadOutput(table.clone())
            .execute(&serde_json::json!({"handle": handle}), &root)
            .await;
        let ToolOutput::Ok { content } = out else {
            panic!("read_output failed on attempt {attempt}");
        };
        observed.push_str(&content);
        // The injected scratch path should appear in the output.
        if observed.contains(&expected_path) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // Verify the scratch path was actually injected.
    assert!(
        observed.contains(&expected_path),
        "STELLA_SCRATCH should be set to the scratch path. Expected substring '{}' in output:\n{}",
        expected_path,
        observed
    );

    // Stop the process.
    let stopped = StopProcess(table)
        .execute(&serde_json::json!({"handle": handle}), &root)
        .await;
    assert!(!stopped.is_error(), "{stopped:?}");
}

/// A send_stdin whose write was in flight when stop_process closed stdin
/// used to put the handle back afterwards — resurrecting the pipe and
/// revoking the EOF the stop had just delivered.
#[cfg(unix)]
#[tokio::test]
async fn a_concurrent_send_stdin_cannot_resurrect_a_stopped_stdin() {
    let (table, root) = tools();
    // `sleep` never reads stdin: a payload well past the OS pipe buffer
    // parks the write in flight, with the stdin handle taken out.
    let handle = start(&table, &root, &["sleep", "30"]).await;
    let text = "x".repeat(4 * 1024 * 1024);
    let send_table = table.clone();
    let send_handle = handle.clone();
    let send_root = root.clone();
    let writer = tokio::spawn(async move {
        SendStdin(send_table)
            .execute(
                &serde_json::json!({"handle": send_handle, "text": text}),
                &send_root,
            )
            .await
    });
    // The write is in flight once the entry's stdin slot is empty.
    let mut taken = false;
    for _ in 0..250 {
        if table
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entries
            .get(&handle)
            .is_some_and(|e| e.stdin.is_none())
        {
            taken = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(taken, "send_stdin never took the stdin handle out");

    let stopped = StopProcess(table.clone())
        .execute(&serde_json::json!({"handle": handle}), &root)
        .await;
    assert!(!stopped.is_error(), "{stopped:?}");
    // The killed process closes the pipe's read end, failing the write.
    let write_result = writer.await.expect("send task");
    assert!(write_result.is_error(), "{write_result:?}");

    let resurrected = table
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entries
        .get(&handle)
        .is_some_and(|e| e.stdin.is_some());
    assert!(
        !resurrected,
        "send_stdin re-inserted a stdin handle stop_process had closed"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn long_running_process_scrubs_inherited_credentials() {
    let _fixture = crate::subprocess_env::test_support::InheritedCredentialFixture::install();
    let (table, root) = tools();
    let command = format!(
        "{}; sleep 30",
        crate::subprocess_env::test_support::PROBE_COMMAND
    );
    let handle = start(&table, &root, &["sh", "-c", &command]).await;

    let mut observed = String::new();
    for _ in 0..50 {
        let out = ReadOutput(table.clone())
            .execute(&serde_json::json!({"handle": handle}), &root)
            .await;
        let ToolOutput::Ok { content } = out else {
            panic!("read_output failed");
        };
        if let Some(line) = content.lines().find(|line| *line == "|||visible|present") {
            observed = line.to_string();
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    crate::subprocess_env::test_support::assert_scrubbed(&observed);

    let stopped = StopProcess(table)
        .execute(&serde_json::json!({"handle": handle}), &root)
        .await;
    assert!(!stopped.is_error(), "{stopped:?}");
}
