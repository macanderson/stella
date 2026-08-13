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
        ToolOutput::Error { message, .. } => panic!("start failed: {message}"),
    }
}

/// Poll until `read_output` sees the given substring, or panic — bounded so
/// a stuck test fails fast instead of hanging.
async fn wait_for_output(
    table: &ProcessTableHandle,
    root: &std::path::Path,
    handle: &str,
    needle: &str,
) -> String {
    let mut observed = String::new();
    for _ in 0..250 {
        let out = ReadOutput(table.clone())
            .execute(&serde_json::json!({"handle": handle}), root)
            .await;
        let ToolOutput::Ok { content } = out else {
            panic!("read_output failed: {out:?}");
        };
        observed.push_str(&content);
        observed.push('\n');
        if observed.contains(needle) {
            return observed;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("never observed {needle:?} in output: {observed}");
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
        ToolOutput::Error { message, .. } => assert!(message.contains("argv"), "{message}"),
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
    wait_for_output(&table, &root, &handle, "written_before_clear").await;

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

    teardown(&table, &handle).await;
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
    wait_for_output(&table, &root, &handle, "still_here").await;

    let out = ReadOutput(table.clone())
        .execute(&serde_json::json!({"handle": handle, "clear": true}), &root)
        .await;
    match out {
        ToolOutput::Error { message, .. } => {
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

    teardown(&table, &handle).await;
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

    teardown(&table, &handle).await;
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
        RestartProcess {
            handle: table.clone(),
            scratch: None,
        }
        .execute(&serde_json::json!({"handle": "proc-9"}), &root)
        .await,
    ] {
        match out {
            ToolOutput::Error { message, .. } => {
                assert!(
                    message.contains("unknown process handle `proc-9`"),
                    "{message}"
                )
            }
            other => panic!("{other:?}"),
        }
    }
}

/// Take a live process down the way the runtime does.
///
/// No tool verb leaves a service stopped any more (#2864), so a test that
/// needs a process gone reaches for the same internal `terminate` the
/// `restart_process` path uses. Calling `stop_process` here would assert the
/// refusal, not the teardown.
#[cfg(unix)]
async fn teardown(table: &ProcessTableHandle, handle: &str) {
    let _ = super::service::terminate(table, handle).await;
}

#[cfg(unix)]
#[tokio::test]
async fn cat_echoes_stdin_and_the_lifecycle_is_reported_after_it_ends() {
    let (table, root) = tools();
    let handle = start(&table, &root, &["cat"]).await;

    let write = SendStdin(table.clone())
        .execute(
            &serde_json::json!({"handle": handle, "text": "hello_process\n"}),
            &root,
        )
        .await;
    assert!(!write.is_error(), "{write:?}");

    let echoed = wait_for_output(&table, &root, &handle, "hello_process").await;
    assert!(echoed.contains("running"), "{echoed}");

    // cat exits on stdin EOF/SIGTERM; afterwards the state is exited and
    // stdin writes are refused.
    teardown(&table, &handle).await;
    let after = SendStdin(table.clone())
        .execute(&serde_json::json!({"handle": handle, "text": "x"}), &root)
        .await;
    match after {
        ToolOutput::Error { message, .. } => {
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

/// An exited process used to keep its `Child` and its log file until the
/// whole registry dropped. Once its output has reached the model there is
/// nothing left to hold — but the handle must keep answering, or a model
/// polling a finished server sees "unknown handle" and concludes something
/// is wrong.
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
async fn the_live_process_cap_refuses_a_runaway_and_an_exit_frees_a_slot() {
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
        ToolOutput::Error { message, .. } => {
            assert!(
                message.contains(&MAX_LIVE_PROCESSES.to_string()),
                "{message}"
            );
            assert!(message.contains("restart_process"), "{message}");
        }
        other => panic!(
            "the {}th start must be refused: {other:?}",
            MAX_LIVE_PROCESSES + 1
        ),
    }

    teardown(&table, &handles[0]).await;
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
        teardown(&table, handle).await;
    }
    let _ = StopProcess(table)
        .execute(&serde_json::json!({"handle": "proc-999"}), &root)
        .await;
}

/// Exited entries are exempt from the live cap, so before the exited-entry
/// bound a start/exit loop that never called read_output pinned a `Child`
/// and its log file per iteration for the rest of the session.
#[cfg(unix)]
#[tokio::test]
async fn exited_unread_entries_are_evicted_oldest_first_never_running_ones() {
    let (table, root) = tools();
    // A process that stays alive across every eviction below.
    let keeper = start(&table, &root, &["cat"]).await;

    let mut echo_handles = Vec::new();
    for _ in 0..MAX_EXITED_ENTRIES + 2 {
        let handle = start(&table, &root, &["sh", "-c", "echo unread_output"]).await;
        // Wait until the exit is observable and something has landed in
        // the log, so an eviction demonstrably discards unread bytes.
        for _ in 0..250 {
            let ready = {
                let mut t = table.lock().unwrap_or_else(|p| p.into_inner());
                match t.entries.get_mut(&handle) {
                    Some(e) => e.poll_exit().is_some() && e.log_len() > 0,
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
        teardown(&table, &handle).await;
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
    let command = "printf 'STELLA_SCRATCH=%s\\n' \"$STELLA_SCRATCH\"; sleep 30".to_string();
    let handle = start_with_scratch(
        &table,
        &root,
        &["sh", "-c", &command],
        Some(scratch_path.clone()),
    )
    .await;

    let observed = wait_for_output(&table, &root, &handle, &expected_path).await;
    assert!(
        observed.contains(&expected_path),
        "STELLA_SCRATCH should be set to the scratch path. Expected substring '{}' in output:\n{}",
        expected_path,
        observed
    );

    // Stop the process.
    teardown(&table, &handle).await;
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

    teardown(&table, &handle).await;
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

    teardown(&table, &handle).await;
}

/// The witness for #2666: a `start_process`d child used to be SIGKILLed the
/// moment the `ProcessTable` (and with it the whole tool registry) dropped
/// — the exact moment a bench trial's agent finishes and the verifier goes
/// looking for the service it just proved was running. This test spawns a
/// process that logs a line once a second, drops the table (simulating the
/// registry going away at the end of a turn) WITHOUT calling
/// `stop_process`, and then proves the process is still alive by signalling
/// it with `kill(pid, 0)` directly — outside the table entirely, since the
/// table that would normally answer `read_output` is gone.
///
/// This fails against the old pipe-owned-by-us design (verified by hand:
/// the child gets SIGKILLed by `ProcessTable::drop`) and passes once output
/// is file-backed and the table no longer kills on drop.
#[cfg(unix)]
#[tokio::test]
async fn a_started_child_outlives_the_process_tables_drop() {
    let root = std::env::temp_dir();
    let table: ProcessTableHandle = Arc::default();
    let handle = start(
        &table,
        &root,
        &[
            "sh",
            "-c",
            "i=0; while [ $i -lt 30 ]; do echo tick $i; i=$((i+1)); sleep 1; done",
        ],
    )
    .await;
    wait_for_output(&table, &root, &handle, "tick 0").await;

    let pid = {
        let mut t = table.lock().unwrap_or_else(|p| p.into_inner());
        let entry = t.entries.get_mut(&handle).expect("entry still present");
        entry.pid
    };
    assert!(pid > 0, "no pid recorded for the started process");

    // Drop every reference to the table — this is what happens when the
    // registry (and the whole tool set) is torn down at the end of a run,
    // with no explicit `stop_process` call.
    drop(table);

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(
        unsafe { libc::kill(pid, 0) == 0 },
        "the started process (pid {pid}) did not survive the ProcessTable's drop"
    );

    // Clean up: kill the process group directly since nothing in-process
    // holds a handle to it anymore.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
}

/// **The #2764 witness, tool side.** A `start_process` child that nothing
/// stopped is reported through the `Tool::live_services` seam the engine's
/// end-of-turn assertion reads, naming the handle and the label the model
/// gave it; the same query after `stop_process` reports nothing. Before
/// #2764 there was no query at all — the process table's liveness was
/// visible only to the process tools themselves, so the driver could not
/// tell a turn that left a service up from one that did not.
#[tokio::test]
async fn a_started_process_is_reported_live_until_it_ends() {
    let (table, root) = tools();
    let start_tool = StartProcess {
        handle: table.clone(),
        scratch: None,
    };
    let out = start_tool
        .execute(
            &serde_json::json!({
                "argv": ["sh", "-c", "i=0; while [ $i -lt 30 ]; do echo tick; sleep 1; i=$((i+1)); done"],
                "name": "ticker",
            }),
            &root,
        )
        .await;
    let ToolOutput::Ok { content } = &out else {
        panic!("start failed: {out:?}");
    };
    let handle = content
        .split_whitespace()
        .find(|w| w.starts_with("proc-"))
        .expect("handle in start output")
        .to_string();

    let live = start_tool.live_services();
    assert_eq!(
        live.len(),
        1,
        "the running child must be reported to the engine: {live:?}"
    );
    assert_eq!(live[0].handle, handle);
    assert_eq!(
        live[0].name.as_deref(),
        Some("ticker"),
        "the model's own label rides along so the nudge can name it"
    );
    assert!(
        live[0].display.contains("sh"),
        "the command line has to be there too: {:?}",
        live[0].display
    );

    teardown(&table, &handle).await;
    assert!(
        start_tool.live_services().is_empty(),
        "a stopped process must not be reported: {:?}",
        start_tool.live_services()
    );
}

/// A process that exited on its own is not a service anyone left running —
/// the same liveness test `live_count` applies. Without this, every
/// completing turn after a short-lived child would be nudged about a handle
/// whose process is already gone, and the assertion would be noise within
/// one session of shipping.
#[tokio::test]
async fn an_exited_process_is_not_reported_live() {
    let (table, root) = tools();
    let start_tool = StartProcess {
        handle: table.clone(),
        scratch: None,
    };
    let handle = start(&table, &root, &["sh", "-c", "echo bye"]).await;
    wait_for_output(&table, &root, &handle, "bye").await;
    // `wait_for_output` polls until the line lands; the shell may still be
    // reaping, so give `poll_exit` a bounded window to observe the exit.
    for _ in 0..250 {
        if start_tool.live_services().is_empty() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!(
        "an exited process was still reported live: {:?}",
        start_tool.live_services()
    );
}

/// Handles are reported oldest first, not in `HashMap` order. The list
/// reaches the model's context, where an unstable ordering is both a
/// confusing message and a prompt-cache miss (invariant 7) — and `proc-10`
/// must not sort ahead of `proc-2`.
#[tokio::test]
async fn live_services_are_ordered_oldest_handle_first() {
    let (table, root) = tools();
    let start_tool = StartProcess {
        handle: table.clone(),
        scratch: None,
    };
    let mut expected = Vec::new();
    for _ in 0..12 {
        expected.push(start(&table, &root, &["sh", "-c", "sleep 30"]).await);
    }
    let live: Vec<String> = start_tool
        .live_services()
        .into_iter()
        .map(|service| service.handle)
        .collect();
    assert_eq!(live, expected, "start order, every time");

    for handle in expected {
        teardown(&table, &handle).await;
    }
}

/// **The #2864 witness.** On `pypi-server__HRUntZQ` the agent ran
/// `stop_process` on the server it had been asked to build — twice — so that
/// its witness test would fail against an absent service, and restarted it
/// three seconds before the trial ended. The grader found a dead port and
/// correct work scored zero.
///
/// Fails on `main`, where the stop succeeds and the service is gone: here it
/// is refused by name, and the process is still running afterwards.
#[cfg(unix)]
#[tokio::test]
async fn stopping_a_live_service_is_refused_and_leaves_it_running() {
    let (table, root) = tools();
    let handle = start(
        &table,
        &root,
        &[
            "sh",
            "-c",
            "echo serving; i=0; while [ $i -lt 60 ]; do sleep 1; i=$((i+1)); done",
        ],
    )
    .await;
    wait_for_output(&table, &root, &handle, "serving").await;
    let pid = {
        let mut t = table.lock().unwrap_or_else(|p| p.into_inner());
        t.entries.get_mut(&handle).expect("entry").pid
    };
    assert!(pid > 0);

    let refused = StopProcess(table.clone())
        .execute(&serde_json::json!({"handle": handle}), &root)
        .await;
    let ToolOutput::Error { message, .. } = refused else {
        panic!("stopping a live service must be refused: {refused:?}");
    };
    // The refusal has to teach the substitute, or the model finds another
    // route to the same state.
    assert!(message.contains("still running"), "{message}");
    assert!(message.contains("restart_process"), "{message}");
    assert!(message.contains("RUNNING service"), "{message}");

    // The service the run would be judged on is still up.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        unsafe { libc::kill(pid, 0) == 0 },
        "the refused stop still took the service (pid {pid}) down"
    );
    let status = ReadOutput(table.clone())
        .execute(&serde_json::json!({"handle": handle}), &root)
        .await;
    let ToolOutput::Ok { content } = status else {
        panic!("read_output after a refused stop: {status:?}");
    };
    assert!(content.contains("running"), "{content}");

    teardown(&table, &handle).await;
}

/// The other half of the same contract: the legitimate reason to stop a
/// service — pick up an edit, or fix a bad argv — is preserved, and its
/// postcondition is always "something is running under this handle". That is
/// what makes the refusal above structural rather than a blocklist: no
/// sequence of the remaining verbs reaches "the graded service is down".
#[cfg(unix)]
#[tokio::test]
async fn restart_replaces_the_process_and_leaves_the_handle_running() {
    let (table, root) = tools();
    let handle = start(&table, &root, &["sh", "-c", "echo first; sleep 60"]).await;
    wait_for_output(&table, &root, &handle, "first").await;
    let first_pid = {
        let mut t = table.lock().unwrap_or_else(|p| p.into_inner());
        t.entries.get_mut(&handle).expect("entry").pid
    };

    let restarted = RestartProcess {
        handle: table.clone(),
        scratch: None,
    }
    .execute(
        &serde_json::json!({"handle": handle, "argv": ["sh", "-c", "echo second; sleep 60"]}),
        &root,
    )
    .await;
    let ToolOutput::Ok { content } = restarted else {
        panic!("restart_process failed: {restarted:?}");
    };
    assert!(content.contains("restarted"), "{content}");

    let second_pid = {
        let mut t = table.lock().unwrap_or_else(|p| p.into_inner());
        t.entries.get_mut(&handle).expect("entry").pid
    };
    assert_ne!(first_pid, second_pid, "restart reused the old process");
    assert!(
        unsafe { libc::kill(second_pid, 0) == 0 },
        "the handle is not running after a restart"
    );
    assert!(
        unsafe { libc::kill(first_pid, 0) != 0 },
        "the replaced process (pid {first_pid}) is still running"
    );
    // The replacement's own output is what the handle now reports.
    let seen = wait_for_output(&table, &root, &handle, "second").await;
    assert!(seen.contains("running"), "{seen}");

    teardown(&table, &handle).await;
}
