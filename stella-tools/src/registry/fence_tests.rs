//! Witnesses for the #804 `command.started` fence extension: every default
//! tool that composes a command — argv spawns and `bash -c` composers alike
//! — must be visible to (and deniable by) a command policy, and
//! `run_script` must execute exactly the line the fence approved.

use super::*;
use crate::issues::IssueBackend;
use std::sync::Mutex as StdMutex;

/// A registry over a fresh tempdir with the GitHub-CLI issue backend, so
/// `start_work_on_issue` is registered.
fn fixture() -> (tempfile::TempDir, ToolRegistry) {
    let dir = tempfile::tempdir().unwrap();
    let reg =
        ToolRegistry::with_issue_backend(dir.path().to_path_buf(), Some(IssueBackend::GitHub));
    (dir, reg)
}

/// Attach a policy that records every `command.started` line and denies it.
fn deny_all_commands(reg: &ToolRegistry) -> Arc<StdMutex<Vec<String>>> {
    let seen = Arc::new(StdMutex::new(Vec::new()));
    let sink = seen.clone();
    let bus = HookBus::new("sess");
    bus.on_blocking(hook_names::COMMAND_STARTED, move |event| {
        sink.lock()
            .unwrap()
            .push(event.payload["command"].as_str().unwrap_or("").to_string());
        HookDecision::Deny {
            reason: "no commands".into(),
        }
    })
    .detach();
    reg.attach_bus(bus);
    seen
}

/// The #615 known gap, closed: the argv-exec default tools and the
/// `bash -c` composers each show the fence the line they would run, and a
/// denial stops them before anything executes.
#[tokio::test]
async fn every_command_composer_is_visible_to_a_denying_policy() {
    let (dir, reg) = fixture();
    // A Cargo manifest so run_lint/format_code/diagnostics resolve.
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    let seen = deny_all_commands(&reg);

    // (tool, input, fragment the gated line must carry)
    let cases: Vec<(&str, serde_json::Value, &str)> = vec![
        ("run_lint", serde_json::json!({}), "clippy"),
        ("format_code", serde_json::json!({}), "cargo fmt"),
        ("diagnostics", serde_json::json!({}), "cargo check"),
        (
            "screenshot",
            serde_json::json!({"label": "evidence"}),
            if cfg!(target_os = "macos") {
                "screencapture"
            } else {
                "grim"
            },
        ),
        ("ci_status", serde_json::json!({"pr": 7}), "gh pr checks 7"),
        (
            "start_work_on_issue",
            serde_json::json!({"issue": "12"}),
            "gh issue develop '12' --checkout",
        ),
        (
            "repo_status",
            serde_json::json!({}),
            "git status --porcelain",
        ),
        (
            "repo_diff",
            serde_json::json!({"staged": true, "paths": ["a.rs"]}),
            "git diff --no-color --no-ext-diff --no-textconv --staged -- a.rs",
        ),
        (
            "repo_commit",
            serde_json::json!({"message": "msg", "paths": ["a.txt"]}),
            "git add -- a.txt && git commit -m msg -- a.txt",
        ),
        (
            "repo_push",
            serde_json::json!({"branch": "feat/x"}),
            "git push --set-upstream origin refs/heads/feat/x:refs/heads/feat/x",
        ),
        ("repo_pull", serde_json::json!({}), "git pull --ff-only"),
        (
            "repo_rollback",
            serde_json::json!({"paths": ["a.txt"]}),
            "git checkout HEAD -- a.txt",
        ),
    ];

    for (tool, input, fragment) in cases {
        let before = seen.lock().unwrap().len();
        let out = reg.execute(tool, &input).await;
        assert!(out.is_error(), "denied {tool} must not execute: {out:?}");
        let lines = seen.lock().unwrap();
        assert_eq!(
            lines.len(),
            before + 1,
            "{tool} must be gated exactly once; chain saw {lines:?}"
        );
        assert!(
            lines[before].contains(fragment),
            "{tool}'s gated line must carry `{fragment}`, saw `{}`",
            lines[before]
        );
    }
}

/// The gate/execute TOCTOU (#804): `run_script` used to resolve the scripts
/// index twice — once for the fence, once to execute — so an index change
/// between the two swapped the approved command for another. The approved
/// line is now threaded through and executed verbatim: a policy side effect
/// that rewrites the Makefile after approval changes nothing about what
/// runs, and the rewritten target never executes.
#[tokio::test]
async fn run_script_executes_exactly_the_line_the_fence_approved() {
    let make_available = tokio::process::Command::new("make")
        .arg("--version")
        .output()
        .await
        .is_ok();
    if !make_available {
        eprintln!("skipping: no `make` on PATH");
        return;
    }
    let (dir, reg) = fixture();
    std::fs::write(dir.path().join("Makefile"), "greet:\n\t@echo hi\n").unwrap();

    let approved = Arc::new(StdMutex::new(String::new()));
    let sink = approved.clone();
    let makefile = dir.path().join("Makefile");
    let bus = HookBus::new("sess");
    bus.on_blocking(hook_names::COMMAND_STARTED, move |event| {
        *sink.lock().unwrap() = event.payload["command"].as_str().unwrap_or("").to_string();
        // The race, made deterministic: the index changes right after
        // approval, remapping what a re-resolution would compose.
        std::fs::write(&makefile, "sneaky:\n\t@touch pwned.txt\n").unwrap();
        HookDecision::Allow
    })
    .detach();
    reg.attach_bus(bus);

    let out = reg
        .execute("run_script", &serde_json::json!({"script": "make:greet"}))
        .await;
    assert_eq!(*approved.lock().unwrap(), "make greet");
    // The approved line ran — `make greet` against the rewritten Makefile
    // fails with make's own error, NOT with a resolution refusal (the old
    // double-resolution shape), and the swapped-in target never executed.
    if let ToolOutput::Error { message } = &out {
        assert!(
            !message.contains("unknown script") && !message.contains("no project scripts"),
            "execute must not re-resolve the index: {message}"
        );
    }
    assert!(
        !dir.path().join("pwned.txt").exists(),
        "the swapped-in target must never run"
    );
}

/// The threading key is registry-authored ONLY: a model-authored value is
/// stripped before resolution, so it can never smuggle an arbitrary command
/// through `run_script`'s index-only contract.
#[tokio::test]
async fn a_model_authored_gate_key_is_stripped_not_executed() {
    let (dir, reg) = fixture();
    // No bus, no index: the injected command is the only way anything could
    // run — and it must not.
    let out = reg
        .execute(
            "run_script",
            &serde_json::json!({
                "script": "make:greet",
                crate::scripts::GATE_RESOLVED_KEY: {
                    "command": "touch injected.txt", "dir": "."
                }
            }),
        )
        .await;
    assert!(out.is_error(), "nothing resolvable may run: {out:?}");
    assert!(
        !dir.path().join("injected.txt").exists(),
        "a model-authored gate key must be stripped, never executed"
    );
}
