//! Extension hook bus integration: policy decisions (deny/modify) around a
//! dispatch, and the command chain that gates every shape of shell-out.

use super::*;

use std::sync::Arc as StdArc;
use std::sync::Mutex as StdMutex;
use stella_core::bus::{HookBus, HookDecision, HookEvent, names as hook_names};

/// Capture every event a `"*"` observer sees on `bus`.
fn record_bus_events(bus: &HookBus) -> StdArc<StdMutex<Vec<HookEvent>>> {
    let seen = StdArc::new(StdMutex::new(Vec::new()));
    let sink = seen.clone();
    bus.on("*", move |event| {
        sink.lock().unwrap().push(event.clone());
        Ok(())
    })
    .detach();
    seen
}

fn event_names(seen: &StdArc<StdMutex<Vec<HookEvent>>>) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .map(|e| e.name.clone())
        .collect()
}

#[tokio::test]
async fn a_deny_policy_blocks_the_tool_and_leaves_no_touch() {
    let (dir, reg) = telemetry_fixture();
    std::fs::write(dir.path().join("f.rs"), "keep me\n").unwrap();
    let bus = HookBus::new("sess");
    bus.on_blocking(hook_names::TOOL_CALL_REQUESTED, |_| HookDecision::Deny {
        reason: "workspace is read-only".into(),
    })
    .detach();
    reg.attach_bus(bus);

    // The delete is refused by the policy; the file survives and the
    // ledger records nothing.
    let out = reg
        .execute("delete_file", &serde_json::json!({"path": "f.rs"}))
        .await;
    assert!(out.is_error(), "deny must surface as a tool error");
    match out {
        ToolOutput::Error { message } => assert!(message.contains("read-only"), "{message}"),
        _ => unreachable!(),
    }
    assert!(
        dir.path().join("f.rs").exists(),
        "denied delete must not run"
    );
    assert!(
        reg.file_touch_telemetry().files_touched.is_empty(),
        "a blocked op records no file touch"
    );
}

#[tokio::test]
async fn a_successful_write_emits_the_file_and_files_touched_events() {
    let (_dir, reg) = telemetry_fixture();
    let bus = HookBus::new("sess");
    let seen = record_bus_events(&bus);
    reg.attach_bus(bus);

    exec_ok(
        &reg,
        "write_file",
        serde_json::json!({"path": "src/new.rs", "content": "a\nb\nc\n", "reason": "scaffold"}),
    )
    .await;

    let names = event_names(&seen);
    // The OBSERVABLE lifecycle for a create. The raw `tool.call.requested`
    // / `file.created` blocking events are delivered only to blocking
    // (privileged) handlers, never to observers — observers see their
    // safe outcomes (`policy.*`), the sanitized `tool.call.started`, the
    // completion, and the `file.created` FACT + aggregate update.
    for expected in [
        hook_names::POLICY_ALLOWED,
        hook_names::TOOL_CALL_STARTED,
        hook_names::TOOL_CALL_COMPLETED,
        hook_names::FILE_CREATED,
        hook_names::FILES_TOUCHED_UPDATED,
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "missing {expected} in {names:?}"
        );
    }
    // The raw blocking event never reaches an observer.
    assert!(
        !names.iter().any(|n| n == hook_names::TOOL_CALL_REQUESTED),
        "raw tool.call.requested must not reach observers: {names:?}"
    );
    // The file.created fact carries the line delta but never the content.
    let events = seen.lock().unwrap();
    let created = events
        .iter()
        .find(|e| e.name == hook_names::FILE_CREATED)
        .unwrap();
    assert_eq!(created.payload["path"], "src/new.rs");
    assert_eq!(created.payload["lines_added"], 3);
    let started = events
        .iter()
        .find(|e| e.name == hook_names::TOOL_CALL_STARTED)
        .unwrap();
    assert!(
        started.payload["input"]["content"]
            .as_str()
            .unwrap()
            .starts_with("<omitted:"),
        "tool.call.started must not leak file content"
    );
}

#[tokio::test]
async fn a_modify_policy_rewrites_the_input_the_tool_runs() {
    let (dir, reg) = telemetry_fixture();
    let bus = HookBus::new("sess");
    bus.on_blocking(hook_names::TOOL_CALL_REQUESTED, |event| {
        // Redirect any write to a quarantine path.
        let mut input = event.payload["input"].clone();
        input["path"] = serde_json::Value::String("quarantine.txt".into());
        let mut payload = event.payload.clone();
        payload["input"] = input;
        HookDecision::Modify { payload }
    })
    .detach();
    reg.attach_bus(bus);

    exec_ok(
        &reg,
        "write_file",
        serde_json::json!({"path": "original.txt", "content": "x\n"}),
    )
    .await;

    assert!(
        dir.path().join("quarantine.txt").exists(),
        "the modified path is what actually got written"
    );
    assert!(!dir.path().join("original.txt").exists());
    assert_eq!(
        reg.file_touch_telemetry().files_touched[0].path,
        "quarantine.txt",
        "the ledger records the path the tool actually touched"
    );
}

#[tokio::test]
async fn cite_memory_dispatches_through_the_registry_and_drains_once() {
    let (_dir, reg) = telemetry_fixture();
    exec_ok(
        &reg,
        "cite_memory",
        serde_json::json!({
            "memory_id": "nod_0123456789abcdef01234567",
            "useful_score": 5,
            "truthful": true,
            "remark": "named the failing module outright",
        }),
    )
    .await;
    let drained = reg.take_memory_citations();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].memory_id, "nod_0123456789abcdef01234567");
    assert!(
        reg.take_memory_citations().is_empty(),
        "a drained citation must never persist under a second execution"
    );
}

#[tokio::test]
async fn a_denied_command_never_runs_and_a_bus_less_registry_is_unchanged() {
    // Command guards apply to `bash`, which ships registered.
    let dir = tempfile::tempdir().unwrap();
    let reg = ToolRegistry::with_backends_and_options(
        dir.path().to_path_buf(),
        None,
        None,
        RegistryOptions::default(),
    );
    let bus = HookBus::new("sess");
    bus.on_blocking(hook_names::COMMAND_STARTED, |_| HookDecision::Deny {
        reason: "no shell".into(),
    })
    .detach();
    reg.attach_bus(bus);
    let out = reg
        .execute("bash", &serde_json::json!({"command": "echo hi"}))
        .await;
    assert!(out.is_error());

    // A registry with no bus attached behaves exactly as before hooks.
    let (dir2, plain) = telemetry_fixture();
    std::fs::write(dir2.path().join("f.txt"), "hi\n").unwrap();
    exec_ok(&plain, "read_file", serde_json::json!({"path": "f.txt"})).await;
    assert_eq!(plain.file_touch_telemetry().files_touched.len(), 1);
}

#[tokio::test]
async fn the_command_chain_gates_run_script_with_its_resolved_command() {
    // `run_script` composes its command from the scripts index, so the
    // same `command.started` policy that fences `bash` must fence it —
    // with the resolved command line, not the script name.
    let (dir, reg) = telemetry_fixture();
    std::fs::write(dir.path().join("Makefile"), "greet:\n\t@echo hi\n").unwrap();
    let bus = HookBus::new("sess");
    let denied_command = StdArc::new(StdMutex::new(String::new()));
    let sink = denied_command.clone();
    bus.on_blocking(hook_names::COMMAND_STARTED, move |event| {
        *sink.lock().unwrap() = event.payload["command"].as_str().unwrap_or("").to_string();
        HookDecision::Deny {
            reason: "no shell".into(),
        }
    })
    .detach();
    reg.attach_bus(bus);

    let out = reg
        .execute("run_script", &serde_json::json!({"script": "make:greet"}))
        .await;
    assert!(
        out.is_error(),
        "denied run_script must not execute: {out:?}"
    );
    assert_eq!(
        *denied_command.lock().unwrap(),
        "make greet",
        "the chain must see the index-resolved command line"
    );
}

#[tokio::test]
async fn the_command_chain_gates_start_process_with_its_joined_argv() {
    // `start_process` sits in the DEFAULT surface while `bash` is
    // opt-in, and argv[0] may itself be a shell — so the same
    // `command.started` policy that fences `bash` must fence the argv
    // spawn, seeing the joined argv, before anything runs.
    let (_dir, reg) = telemetry_fixture();
    let bus = HookBus::new("sess");
    let denied_command = StdArc::new(StdMutex::new(String::new()));
    let sink = denied_command.clone();
    bus.on_blocking(hook_names::COMMAND_STARTED, move |event| {
        *sink.lock().unwrap() = event.payload["command"].as_str().unwrap_or("").to_string();
        HookDecision::Deny {
            reason: "no shell".into(),
        }
    })
    .detach();
    reg.attach_bus(bus);

    let out = reg
        .execute(
            "start_process",
            &serde_json::json!({"argv": ["bash", "-c", "echo hi"]}),
        )
        .await;
    assert!(
        out.is_error(),
        "denied start_process must not spawn: {out:?}"
    );
    assert_eq!(
        *denied_command.lock().unwrap(),
        "bash -c echo hi",
        "the chain must see the joined argv as the command line"
    );
    // The denial fired before the spawn: no handle may exist.
    let read = reg
        .execute("read_output", &serde_json::json!({"handle": "proc-1"}))
        .await;
    assert!(
        read.is_error(),
        "no process may exist after a denied spawn: {read:?}"
    );
}

/// The hole the `start_process` gate left open. Gating only the spawn
/// fences `["bash", "-c", "cat"]` once, on that one argv; every command
/// pushed into the live shell afterwards used to execute with no policy
/// consultation and no `command.*` audit record, so a session could run
/// arbitrary shell through a channel that is on by default while `bash`
/// is opt-in.
#[tokio::test]
async fn the_command_chain_gates_the_text_written_into_a_live_interpreter() {
    let (_dir, reg) = telemetry_fixture();
    let bus = HookBus::new("sess");
    let seen = StdArc::new(StdMutex::new(Vec::<String>::new()));
    let sink = seen.clone();
    // Allow the spawn, deny what gets written into it — the operator
    // posture that permits a REPL but not arbitrary commands inside it.
    bus.on_blocking(hook_names::COMMAND_STARTED, move |event| {
        let command = event.payload["command"].as_str().unwrap_or("").to_string();
        sink.lock().unwrap().push(command.clone());
        if command.contains("rm -rf") {
            HookDecision::Deny {
                reason: "no destructive commands".into(),
            }
        } else {
            HookDecision::Allow
        }
    })
    .detach();
    reg.attach_bus(bus);

    let spawned = reg
        .execute("start_process", &serde_json::json!({"argv": ["cat"]}))
        .await;
    assert!(!spawned.is_error(), "the spawn is allowed: {spawned:?}");

    let out = reg
        .execute(
            "send_stdin",
            &serde_json::json!({"handle": "proc-1", "text": "rm -rf /\n"}),
        )
        .await;
    assert!(
        out.is_error(),
        "writing a denied command into a live interpreter must not go \
         through: {out:?}"
    );
    let seen = seen.lock().unwrap().clone();
    assert!(
        seen.iter().any(|c| c.contains("rm -rf")),
        "the chain must be shown the stdin text, not just the spawn argv; \
         it saw {seen:?}"
    );
}

/// A `command.started` policy that denies shell execution, capturing
/// every command line the chain was shown — the posture an extension
/// uses to fence command execution.
fn deny_shell(reg: &ToolRegistry) -> StdArc<StdMutex<Vec<String>>> {
    let seen = StdArc::new(StdMutex::new(Vec::new()));
    let sink = seen.clone();
    let bus = HookBus::new("sess");
    bus.on_blocking(hook_names::COMMAND_STARTED, move |event| {
        sink.lock()
            .unwrap()
            .push(event.payload["command"].as_str().unwrap_or("").to_string());
        HookDecision::Deny {
            reason: "no shell".into(),
        }
    })
    .detach();
    reg.attach_bus(bus);
    seen
}

#[tokio::test]
async fn the_command_chain_gates_the_explicit_build_test_and_verify_commands() {
    // `build_project`, `run_tests`, and `verify_done` all reach
    // `bash -c` from the DEFAULT surface while `bash` is opt-in, so the
    // same `command.started` policy that fences `bash` must fence
    // them — seeing the model's own command text, before anything runs.
    let (dir, reg) = telemetry_fixture();
    let commands = deny_shell(&reg);

    for (tool, input) in [
        (
            "build_project",
            serde_json::json!({"command": "touch build-ran.txt"}),
        ),
        (
            "run_tests",
            serde_json::json!({"command": "touch tests-ran.txt"}),
        ),
        (
            "verify_done",
            serde_json::json!({
                "test_cmd": "touch verify-ran.txt",
                "test_files": ["a.rs"],
            }),
        ),
    ] {
        let out = reg.execute(tool, &input).await;
        assert!(out.is_error(), "denied {tool} must not execute: {out:?}");
    }

    assert_eq!(
        *commands.lock().unwrap(),
        vec![
            "touch build-ran.txt",
            "touch tests-ran.txt",
            "touch verify-ran.txt",
        ],
        "the chain must see each tool's own command line"
    );
    // Each denial fired before the shell ran: no marker file exists.
    for marker in ["build-ran.txt", "tests-ran.txt", "verify-ran.txt"] {
        assert!(
            !dir.path().join(marker).exists(),
            "`{marker}` proves a denied command still ran"
        );
    }
}

#[tokio::test]
async fn the_command_chain_gates_the_index_composed_build_and_test_commands() {
    // The common case is the model omitting `command` entirely and the
    // tool composing one from the scripts index — that path must be
    // fenced with the composed line too, or `{}` reopens the bypass.
    let (dir, reg) = telemetry_fixture();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    let commands = deny_shell(&reg);

    let out = reg.execute("build_project", &serde_json::json!({})).await;
    assert!(out.is_error(), "denied build_project must not run: {out:?}");
    let out = reg
        .execute(
            "run_tests",
            &serde_json::json!({"kind": "unit", "filter": "my_test"}),
        )
        .await;
    assert!(out.is_error(), "denied run_tests must not run: {out:?}");

    assert_eq!(
        *commands.lock().unwrap(),
        vec![
            "cargo build --workspace",
            // The filter is shell-quoted per token, so the gated line is
            // still byte-identical to what would have executed — which
            // is the property this test exists to protect.
            "cargo test --workspace --lib --bins 'my_test'",
        ],
        "the chain must see the index-composed command line"
    );
}
