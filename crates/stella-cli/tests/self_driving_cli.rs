//! End-to-end witness for `stella self-driving` (#1548): drive the real binary
//! against a throwaway `SELF_DRIVING_STATE_DIR` and pin the contracts the
//! `/self-driving` slash commands and `scripts/self-driving.sh` depend on — the
//! digest bytes, the shell-evalable output shapes, the aperture advance, and
//! the run lifecycle.
//!
//! The decision logic itself is covered in `stella-autonomy`; what only
//! a process can prove is that the command is reachable without a provider or
//! API key, that the state files land where the observatory reads them, and
//! that exit codes carry the verdicts (`watch` SLEEP is exit 1, not an
//! error). `PATH` is cleared so the child can find no `gh` and no `git`:
//! hermetic by construction, the same posture as `scripts/test-self-driving.sh`.

use std::path::Path;
use std::process::{Command, Output};

fn stella(workspace: &Path, state: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stella"))
        .current_dir(workspace)
        .env_clear()
        .env("SELF_DRIVING_STATE_DIR", state)
        .env("PATH", "")
        .arg("self-driving")
        .args(args)
        .output()
        .expect("spawn stella")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The digest is a cross-implementation contract: the same bytes the shell
/// pipeline (`tr | sed | shasum -a 256 | cut -c1-16`) wrote into every
/// existing `seen.txt`. See the goldens in `stella-autonomy::tests`.
#[test]
fn seen_digest_prints_the_shell_pipelines_bytes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = stella(
        tmp.path(),
        &tmp.path().join("state"),
        &[
            "seen",
            "--digest",
            "crates/stella-core/src/driver.rs:812 retry counter never reset",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout(&out).trim(), "a9efcba568ece91c");
}

/// The aperture oracle, end to end through real files: two consecutive dry
/// cycles advance the lens, and the fresh lens starts its streak at zero.
#[test]
fn two_dry_cycles_advance_the_lens_and_the_fresh_lens_starts_at_zero() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path().join("state");
    let end = |n: &str, new: &str| {
        let out = stella(
            tmp.path(),
            &state,
            &[
                "cycle",
                "end",
                "--cycle",
                n,
                "--new",
                new,
                "--outcome",
                "ok",
            ],
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let current = || {
        stdout(&stella(tmp.path(), &state, &["aperture", "--current"]))
            .trim()
            .to_string()
    };

    end("1", "1");
    assert_eq!(
        current(),
        "rubric",
        "a discovering cycle holds the lens open"
    );
    end("2", "0");
    assert_eq!(
        current(),
        "rubric",
        "one dry cycle is noise, not convergence"
    );
    end("3", "0");
    assert_eq!(current(), "properties", "two dry cycles open the next lens");

    let streak = stdout(&stella(tmp.path(), &state, &["state", "--dry-streak"]));
    assert_eq!(
        streak.trim(),
        "0",
        "a fresh lens must not inherit the dry tail that opened it"
    );
}

/// `plan` is eval'd by the slash commands, so the KEY=value lines are a wire
/// format. Pin every probe and assert the exact decision.
#[test]
fn plan_emits_the_evalable_decision_for_a_pinned_machine() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_stella"))
        .current_dir(tmp.path())
        .env_clear()
        .env("SELF_DRIVING_STATE_DIR", tmp.path().join("state"))
        .env("PATH", "")
        .env("SELF_DRIVING_PROBE_CPU", "8")
        .env("SELF_DRIVING_PROBE_LOAD1", "1")
        .env("SELF_DRIVING_PROBE_MEM_TOTAL_GB", "16")
        .env("SELF_DRIVING_PROBE_MEM_FREE_GB", "8")
        .env("SELF_DRIVING_PROBE_DISK_FREE_GB", "50")
        .env("SELF_DRIVING_PROBE_ON_BATTERY", "0")
        .env("SELF_DRIVING_PROBE_CONTENTION", "0")
        .args(["self-driving", "plan"])
        .output()
        .expect("spawn stella");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    for expected in [
        "SELF_DRIVING_TIER=normal",
        "SELF_DRIVING_BATCH=20",
        "SELF_DRIVING_PARALLEL=2",
        "SELF_DRIVING_LOCAL_BUILD=1",
        "SELF_DRIVING_SCOPE=impacted",
        "SELF_DRIVING_AUDIT=deep",
        "SELF_DRIVING_BENCH=loop",
        "SELF_DRIVING_APERTURE=rubric",
    ] {
        assert!(
            text.lines().any(|l| l == expected),
            "missing `{expected}` in:\n{text}"
        );
    }
}

/// The run lifecycle through the real files: start records `running`, end
/// records the terminal status, and the fold reports both.
#[test]
fn a_run_starts_running_and_ends_with_its_terminal_status() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path().join("state");

    let start = stella(tmp.path(), &state, &["run", "start"]);
    assert!(start.status.success());
    assert!(
        stdout(&start).contains("SELF_DRIVING_RUN_ID=r-"),
        "the driver needs the run id on stdout:\n{}",
        stdout(&start)
    );

    let listed = stdout(&stella(tmp.path(), &state, &["runs"]));
    assert!(
        listed.contains("running"),
        "expected a running run:\n{listed}"
    );

    let end = stella(
        tmp.path(),
        &state,
        &["run", "end", "--status", "completed", "--reason", "done"],
    );
    assert!(
        end.status.success(),
        "{}",
        String::from_utf8_lossy(&end.stderr)
    );

    let listed = stdout(&stella(tmp.path(), &state, &["runs"]));
    assert!(
        listed.contains("completed") && !listed.contains("running"),
        "the fold must report the terminal status:\n{listed}"
    );
}

/// #1549's user-facing surface: the ladder listing names each lens's tool
/// backing, and admits the model-only lenses in so many words.
#[test]
fn aperture_list_names_the_tooling_and_admits_the_model_only_lenses() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let text = stdout(&stella(
        tmp.path(),
        &tmp.path().join("state"),
        &["aperture", "--list"],
    ));
    assert!(
        text.contains("* rubric"),
        "the open lens is marked:\n{text}"
    );
    assert!(text.contains("make supply-chain"), "{text}");
    assert!(text.contains("model-only"), "{text}");
    assert!(text.contains("[heavy tier]"), "{text}");
    assert!(text.contains("watch"), "{text}");
}

/// `watch` on a machine where nothing changed sleeps via exit code 1 — the
/// driver loops on the code, and an error envelope would read as a broken
/// sentinel rather than a quiet night.
#[test]
fn watch_sleeps_with_exit_one_when_nothing_changed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = stella(tmp.path(), &tmp.path().join("state"), &["watch"]);
    assert_eq!(out.status.code(), Some(1), "SLEEP is exit 1");
    assert!(stdout(&out).contains("SLEEP"));
}

/// **Witness (#4457).** `stella self-driving stop` writes the flag into the
/// state root the loop itself resolves, and says where it put it.
///
/// The flag had a reader and no writer: #3942 taught the loop to park on the
/// file's presence, and left `touch` — against a path an operator learns from a
/// parked run's audit line, which prints only once the loop is already
/// parked — as the only way to create it. Through the real binary because the
/// resolution is the half that can drift: a verb writing to a root the loop
/// does not read would pass every unit test and stop nothing.
#[test]
fn stop_writes_the_flag_into_the_loops_own_state_root() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path().join("state");

    let out = stella(tmp.path(), &state, &["stop"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let flag = state.join("stop");
    assert!(
        flag.exists(),
        "the verb writes the flag the loop polls: {}",
        flag.display()
    );
    let text = stdout(&out);
    assert!(
        text.contains(&flag.display().to_string()),
        "and names the path, so an operator can drop it again:\n{text}"
    );

    // Idempotent, and it says which case it was — a supervisor re-issuing the
    // stop must not read a successful stop as a failure.
    let again = stella(tmp.path(), &state, &["stop"]);
    assert!(again.status.success());
    assert!(
        stdout(&again).contains("already"),
        "asking a stopping loop to stop reports that it was already asked:\n{}",
        stdout(&again)
    );
}

/// `--root` stops a loop rooted somewhere other than this workspace's own.
#[test]
fn stop_root_targets_another_loops_state_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mine = tmp.path().join("state");
    let theirs = tmp.path().join("other-clone");

    let out = stella(
        tmp.path(),
        &mine,
        &["stop", "--root", &theirs.display().to_string()],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        theirs.join("stop").exists(),
        "the named root is the one that stops"
    );
    assert!(
        !mine.join("stop").exists(),
        "and this workspace's own loop keeps working"
    );
}

/// **#4471's witness, at the process.** `--api-key` is a session global, so
/// clap accepts it on every subcommand; before this it was accepted here and
/// then reached no turn — the silent-drop shape #4352 had just fixed for
/// `--model`.
///
/// The refusal lands before any state directory is touched, which is what
/// makes it a refusal rather than a warning about work already done.
#[test]
fn an_api_key_is_refused_before_the_loop_touches_anything() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path().join("state");

    let out = stella(
        tmp.path(),
        &state,
        &["state", "--api-key", "sk-should-never-reach-a-child"],
    );

    assert!(
        !out.status.success(),
        "a dropped credential is not a success"
    );
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(said.contains("--api-key"), "{said}");
    assert!(
        said.contains("stella auth"),
        "the refusal has to name a route that works: {said}"
    );
    assert!(
        !said.contains("sk-should-never-reach-a-child") && !stdout(&out).contains("sk-should"),
        "and must not echo the secret onto a terminal or a log: {said}"
    );
    assert!(
        !state.exists(),
        "nothing may be written before a flag this command cannot honour is refused"
    );
}
