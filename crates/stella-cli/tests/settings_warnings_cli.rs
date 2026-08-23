//! End-to-end witnesses for the notices `Settings::load` speaks on stderr —
//! the unrecognized-key warning, and the untrusted checkout's withheld
//! steering (#2302).
//!
//! Every settings type in `stella-cli` is deliberately tolerant of unknown
//! fields — `deny_unknown_fields` would turn a key written by a newer stella
//! into a hard launch failure on an older one. The price of that tolerance is
//! that serde cannot tell a typo from a future key, so `"provider"` for
//! `"providers"` parsed cleanly, configured absolutely nothing, and printed
//! nothing at all.
//!
//! Only a process can prove the whole path: the file is read from the real
//! scope chain, the warning reaches stderr, stdout stays clean for whoever is
//! piping it, and the command still succeeds (advisory, never a gate).
//!
//! The same argument covers the stream-json half (#3616): `withheld::event`
//! is pure and would stay green with the emit in `output::open_raw_turn`
//! deleted, so the carrier is witnessed through a real process reading real
//! stdout.
//!
//! That last sentence is why the steering notice is witnessed *here* and not
//! only by `settings::tests`. Those tests exercise `settings::withheld`, which
//! is pure — they would stay green with the `eprintln!` that makes the notice
//! user-visible deleted outright, which is to say they prove the answer and not
//! the announcement.

use std::process::{Command, Output};

/// A workspace with the given `.stella/settings.json`, plus an empty HOME so
/// the developer's own user scope cannot contribute keys to the assertions.
fn workspace(settings: &str) -> (tempfile::TempDir, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("workspace");
    let home = tempfile::tempdir().expect("home");
    std::fs::create_dir_all(dir.path().join(".stella")).expect("dot dir");
    std::fs::write(dir.path().join(".stella/settings.json"), settings).expect("settings");
    (dir, home)
}

/// `stella models` loads the full scope chain and needs no provider or key —
/// the cheapest command that exercises the merge. Untrusted, because that is
/// what a fresh `git clone` is: the trust variables are *removed* rather than
/// merely unset, so a developer who exports `STELLA_TRUST_PROJECT` in their own
/// shell cannot turn these assertions green or red from outside.
fn models(dir: &tempfile::TempDir, home: &tempfile::TempDir) -> Output {
    models_with(dir, home, &[])
}

/// [`models`] with `STELLA_TRUST_PROJECT=1` — the opt-in every trust notice
/// names, so the arm where nothing is withheld is exercised by the flag itself
/// rather than by a stand-in for it.
fn models_trusting_the_project(dir: &tempfile::TempDir, home: &tempfile::TempDir) -> Output {
    models_with(dir, home, &[("STELLA_TRUST_PROJECT", "1")])
}

fn models_with(
    dir: &tempfile::TempDir,
    home: &tempfile::TempDir,
    extra_env: &[(&str, &str)],
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_stella"));
    command
        .arg("models")
        .current_dir(dir.path())
        .env("HOME", home.path())
        .env("NO_COLOR", "1")
        .env("STELLA_NO_ENV_FILE", "1")
        .env("STELLA_CATALOG_AUTO_REFRESH", "0")
        // Point the org-managed scope at a path that does not exist, so a real
        // /etc/stella/settings.json on the build host cannot affect this.
        .env(
            "STELLA_MANAGED_SETTINGS",
            home.path().join("no-managed.json"),
        )
        .env_remove("STELLA_MODEL")
        .env_remove("STELLA_TRUST_PROJECT")
        .env_remove("STELLA_PROJECT_HOOKS");
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command.output().expect("run stella models")
}

/// A workspace whose **steering plane** the trust gate withholds: one memory
/// and one published context record. It deliberately writes no `settings.json`
/// — the steering plane lives outside that file, which is the whole reason its
/// suppression was silent (#2302). The memory body carries a marker the
/// assertions look for by its absence.
fn steering_workspace() -> (tempfile::TempDir, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("workspace");
    let home = tempfile::tempdir().expect("home");
    let stella = dir.path().join(".stella");
    std::fs::create_dir_all(stella.join("memories")).expect("memories dir");
    std::fs::create_dir_all(stella.join("rules")).expect("rules dir");
    std::fs::write(stella.join("memories/00-marker.md"), "SECRET-MARKER-BODY").expect("memory");
    std::fs::write(stella.join("rules/style.toml"), "").expect("record");
    (dir, home)
}

#[test]
fn unrecognized_settings_keys_are_named_on_stderr_without_failing_the_command() {
    let (dir, home) = workspace(
        r#"{
             "provider": { "zai": { "api_key": "x" } },
             "enable_recapp": "on",
             "hooks": { "preToolUse": [] },
             "agent_engine_config": { "default_modle": "zai/glm-5.2" }
           }"#,
    );
    let output = models(&dir, &home);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        output.status.code(),
        Some(0),
        "the warning is advisory and must never gate a command: {stderr}"
    );
    assert!(
        stderr.contains("unrecognized key"),
        "the finding must be stated, not merely implied: {stderr}"
    );
    for typo in [
        "provider",
        "enable_recapp",
        "hooks.preToolUse",
        "agent_engine_config.default_modle",
    ] {
        assert!(
            stderr.contains(typo),
            "`{typo}` must be named so it can be fixed: {stderr}"
        );
    }
    assert!(
        stderr.contains(".stella/settings.json"),
        "the warning must say WHICH file: {stderr}"
    );
    // A machine reading the provider table must not have to filter diagnostics
    // out of it.
    assert!(
        !stdout.contains("unrecognized key"),
        "the warning belongs on stderr: {stdout}"
    );
}

#[test]
fn a_correctly_spelled_settings_file_warns_about_nothing() {
    let (dir, home) = workspace(
        r#"{
             "providers": { "zai": { "api_key_env": "SOME_VAR" } },
             "tools": { "bash": "off", "some-server__any_tool": "off" },
             "enable_recap": "on",
             "ui": { "theme": "stella-dark" },
             "agent_engine_config": {
               "default_model": "zai/glm-5.2",
               "agents": { "verifier": { "params": { "temperature": 0.2 } } }
             }
           }"#,
    );
    let output = models(&dir, &home);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        !stderr.contains("unrecognized key"),
        "open maps (tools, providers) and every real key must stay silent: {stderr}"
    );
}

/// The chain is loaded several times over in one launch (`Config::load`, the
/// catalog bootstrap, `validate_at_launch`, every `/models` render). One
/// accurate notice repeated four times reads as a fault rather than a notice,
/// so the announcement is latched — exactly like the project-trust notices it
/// sits beside.
#[test]
fn the_warning_is_printed_once_per_process_not_once_per_settings_load() {
    let (dir, home) = workspace(r#"{ "enable_recapp": "on" }"#);
    let output = models(&dir, &home);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches("unrecognized key").count(),
        1,
        "expected exactly one notice: {stderr}"
    );
}

/// A `git clone` the user has not trusted is told, on stderr, that its
/// memories and records were withheld — in counts, never in content — and how
/// to opt in (#2302).
///
/// This is the witness for the *announcement*, which is the behaviour the issue
/// is about: delete the `eprintln!` in `Settings::load` and only this test goes
/// red. It also pins the two properties that make the notice safe to print at
/// all — the counts-only rule (repository text must not reach stderr via the
/// refusal to load it) and stdout staying clean for whoever is piping it.
#[test]
fn an_untrusted_checkouts_withheld_steering_is_named_on_stderr() {
    let (dir, home) = steering_workspace();
    let output = models(&dir, &home);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        output.status.code(),
        Some(0),
        "the notice is advisory and must never gate a command: {stderr}"
    );
    assert!(
        stderr.contains("project steering in"),
        "the suppression must be stated, not merely implied: {stderr}"
    );
    assert!(
        stderr.contains("(1 memory, 1 context record)"),
        "the notice must say how much was withheld: {stderr}"
    );
    assert!(
        stderr.contains("STELLA_TRUST_PROJECT=1"),
        "a notice that does not name the opt-in leaves the user stuck: {stderr}"
    );
    assert!(
        !stderr.contains("SECRET-MARKER-BODY") && !stderr.contains("00-marker"),
        "counts, never content or filenames — a refusal that echoed repository \
         text would be the exfiltration channel it exists to prevent: {stderr}"
    );
    assert!(
        !stdout.contains("project steering"),
        "the notice belongs on stderr: {stdout}"
    );
    // Latched with the notices it sits beside: one launch loads the chain
    // several times over.
    assert_eq!(
        stderr.matches("project steering in").count(),
        1,
        "expected exactly one notice: {stderr}"
    );
}

/// The two silent arms, through the real binary: a checkout the user trusted is
/// getting its steering, and a checkout with nothing on disk lost nothing.
///
/// Both matter as much as the arm that speaks. A notice printed in every
/// repository is one nobody reads, and one printed at a workspace that *did*
/// load its memories is simply false.
#[test]
fn a_trusted_checkout_and_one_with_no_steering_say_nothing() {
    let (dir, home) = steering_workspace();
    let trusted = models_trusting_the_project(&dir, &home);
    let stderr = String::from_utf8_lossy(&trusted.stderr);
    assert_eq!(trusted.status.code(), Some(0), "{stderr}");
    assert!(
        !stderr.contains("project steering"),
        "a trusted workspace is getting its steering and is owed no notice: {stderr}"
    );

    let (bare_dir, bare_home) = workspace(r#"{}"#);
    let bare = models(&bare_dir, &bare_home);
    let stderr = String::from_utf8_lossy(&bare.stderr);
    assert_eq!(bare.status.code(), Some(0), "{stderr}");
    assert!(
        !stderr.contains("project steering"),
        "a repo with no steering must not warn about a suppression that cost it \
         nothing: {stderr}"
    );
}

/// A one-shot `stella run` in `dir`, machine-readable, that cannot reach a
/// provider: `--base-url` points at a closed loopback port, so the first model
/// call is refused at once. That is enough for what is under test — the
/// withheld-steering event is emitted when the turn's channel opens, before
/// anything is asked of a provider — and it keeps the case hermetic, with no
/// mock server and no credential.
fn run_stream_json(
    dir: &tempfile::TempDir,
    home: &tempfile::TempDir,
    extra_env: &[(&str, &str)],
) -> Output {
    run_machine_readable(dir, home, "stream-json", extra_env)
}

/// The same run under `--output-format json`, which answers on one summary
/// object rather than a line per event (#4465). The turn aborts on the dead
/// base URL either way; the summary is printed for an abort exactly as it is
/// for a completion, which is the point — a harness that only ever sees failed
/// runs still learns its checkout was not steering them.
fn run_json(
    dir: &tempfile::TempDir,
    home: &tempfile::TempDir,
    extra_env: &[(&str, &str)],
) -> Output {
    run_machine_readable(dir, home, "json", extra_env)
}

fn run_machine_readable(
    dir: &tempfile::TempDir,
    home: &tempfile::TempDir,
    format: &str,
    extra_env: &[(&str, &str)],
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_stella"));
    command
        .args([
            "--model",
            "openrouter/z-ai/glm-5.1",
            "--api-key",
            "sk-not-a-real-key",
            "--base-url",
            "http://127.0.0.1:1",
            "--spend-limit",
            "0.05",
            "run",
            "--output-format",
            format,
            "say hi and stop",
        ])
        .current_dir(dir.path())
        .env("HOME", home.path())
        .env("STELLA_HOME", home.path())
        .env("STELLA_DATA_DIR", home.path())
        .env("NO_COLOR", "1")
        .env("STELLA_NO_ENV_FILE", "1")
        .env("STELLA_CATALOG_AUTO_REFRESH", "0")
        .env(
            "STELLA_MANAGED_SETTINGS",
            home.path().join("no-managed.json"),
        )
        .env_remove("STELLA_MODEL")
        .env_remove("STELLA_TRUST_PROJECT")
        .env_remove("STELLA_PROJECT_HOOKS")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ZAI_API_KEY")
        .env_remove("OPENAI_API_KEY");
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command.output().expect("run stella")
}

/// **Witness (#3616).** A harness reading `--output-format stream-json` learns
/// on **stdout** that this checkout's steering was withheld, without scraping
/// the human channel. Before this change the answer existed only as an
/// `eprintln!`: the stream carried no such event at all, whatever the
/// repository had on disk.
///
/// It pins the same two safety properties the stderr witness does, because the
/// event is a second carrier of one fact and a second carrier is a second
/// chance to leak: counts only, and the authority named, so a consumer can
/// tell the remedy it can act on from the one it cannot.
#[test]
fn an_untrusted_checkouts_withheld_steering_reaches_the_event_stream() {
    let (dir, home) = steering_workspace();
    let output = run_stream_json(&dir, &home, &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    let event = stdout
        .lines()
        .find(|line| line.contains("\"type\":\"steering_withheld\""))
        .unwrap_or_else(|| panic!("no steering_withheld event on stdout — stream was:\n{stdout}"));
    assert!(
        event.contains("\"withheld_by\":\"project_untrusted\""),
        "the event must name the authority, because it decides the remedy: {event}"
    );
    assert!(
        event.contains("\"memories\":1") && event.contains("\"records\":1"),
        "the event must say how much was withheld: {event}"
    );
    assert!(
        !event.contains("SECRET-MARKER-BODY") && !event.contains("00-marker"),
        "counts, never content or filenames — the same rule the stderr line is \
         held to, for the same reason: {event}"
    );
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.contains("\"type\":\"steering_withheld\""))
            .count(),
        1,
        "one notice per run: {stdout}"
    );
}

/// **Witness (#4465).** The other machine format answers too. #3616 put the
/// counts on `stream-json` and left `--output-format json` with no `withheld`
/// key at all, so a harness reading the summary alone — the shape most scripts
/// use — could not tell an untrusted checkout from a trusted one.
///
/// A process test rather than a unit one for the reason this file exists:
/// `summary::withheld` is pure over a slice of events, and would stay green
/// with the emit in `output::open_raw_turn` deleted. What is proved here is
/// that the event this fold reads is actually in the journal the envelope is
/// built from.
#[test]
fn an_untrusted_checkouts_withheld_steering_reaches_the_json_summary() {
    let (dir, home) = steering_workspace();
    let output = run_json(&dir, &home, &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let summary: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|err| panic!("summary did not parse ({err}) — stdout was:\n{stdout}"));

    let withheld = &summary["withheld"];
    assert_eq!(
        withheld["withheld_by"], "project_untrusted",
        "the summary must name the authority, because it decides the remedy: {summary}"
    );
    assert_eq!(withheld["memories"], 1, "counts, in the summary: {summary}");
    assert_eq!(withheld["records"], 1, "counts, in the summary: {summary}");
    assert!(
        !withheld.to_string().contains("SECRET-MARKER-BODY")
            && !withheld.to_string().contains("00-marker"),
        "counts, never content or filenames — the same rule the other two carriers are \
         held to, for the same reason: {withheld}"
    );
}

/// The silent arm on the summary: nothing was suppressed, so the key is
/// `null` rather than a zeroed object claiming a suppression that cost
/// nothing.
#[test]
fn a_trusted_checkouts_json_summary_reports_no_withholding() {
    let (dir, home) = steering_workspace();
    let output = run_json(&dir, &home, &[("STELLA_TRUST_PROJECT", "1")]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let summary: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|err| panic!("summary did not parse ({err}) — stdout was:\n{stdout}"));
    assert!(
        summary["withheld"].is_null(),
        "a trusted workspace loaded its steering and is owed no notice: {summary}"
    );
}

/// A one-shot `stella goal` in `dir`, against the same closed loopback port
/// [`run_stream_json`] uses and for the same reason: the notice is emitted
/// when the run's channel opens, long before a provider is asked for
/// anything.
///
/// `stella goal` takes no `--output-format` (goal and monitor are text-mode
/// by construction), so the machine carrier to read is the journal the
/// renderer persists — `<workspace>/.stella/private/store.db`, the `events`
/// table.
fn goal_run(dir: &tempfile::TempDir, home: &tempfile::TempDir, extra_env: &[(&str, &str)]) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_stella"));
    command
        .args([
            "--model",
            "openrouter/z-ai/glm-5.1",
            "--api-key",
            "sk-not-a-real-key",
            "--base-url",
            "http://127.0.0.1:1",
            "--spend-limit",
            "0.05",
            "goal",
            "say hi and stop",
        ])
        .current_dir(dir.path())
        .env("HOME", home.path())
        .env("STELLA_HOME", home.path())
        .env("STELLA_DATA_DIR", home.path())
        .env("NO_COLOR", "1")
        .env("STELLA_NO_ENV_FILE", "1")
        .env("STELLA_CATALOG_AUTO_REFRESH", "0")
        .env(
            "STELLA_MANAGED_SETTINGS",
            home.path().join("no-managed.json"),
        )
        .env_remove("STELLA_MODEL")
        .env_remove("STELLA_TRUST_PROJECT")
        .env_remove("STELLA_PROJECT_HOOKS")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ZAI_API_KEY")
        .env_remove("OPENAI_API_KEY");
    for (key, value) in extra_env {
        command.env(key, value);
    }
    // The run fails — there is nothing on that port — and that is fine: what
    // is under test is an event sent before the first model call.
    let _ = command.output().expect("run stella goal");
}

/// The `steering_withheld` rows this run's journal holds.
fn withheld_events_in_journal(dir: &tempfile::TempDir) -> Vec<String> {
    let store = dir.path().join(".stella").join("private").join("store.db");
    let conn = rusqlite::Connection::open(&store)
        .unwrap_or_else(|e| panic!("open {}: {e}", store.display()));
    let mut stmt = conn
        .prepare("SELECT payload FROM events WHERE event_type = 'steering_withheld'")
        .expect("prepare");
    stmt.query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

/// **Witness (#4500).** `stella goal` is a door too: an untrusted checkout's
/// refusal reaches its event stream, not only its stderr.
///
/// The goal loop drives `Engine::run_goal` itself rather than
/// `agent::run_turn`, so it reached neither of the two openers that carry
/// this notice — `agent::output::open_raw_turn` and the deck's boot
/// announcement. The stderr line `Settings::load` prints was the whole of
/// what a `stella goal` user got; the journal, `stella export` and anything
/// reading the run's events had nothing.
#[test]
fn an_untrusted_checkouts_withheld_steering_reaches_a_goal_runs_journal() {
    let (dir, home) = steering_workspace();
    goal_run(&dir, &home, &[]);

    let events = withheld_events_in_journal(&dir);
    assert_eq!(
        events.len(),
        1,
        "a goal run is owed exactly one withheld-steering event: {events:?}"
    );
    let payload = &events[0];
    assert!(
        payload.contains("\"withheld_by\":\"project_untrusted\""),
        "the event must name the authority, because it decides the remedy: {payload}"
    );
    assert!(
        payload.contains("\"memories\":1") && payload.contains("\"records\":1"),
        "the event must say how much was withheld: {payload}"
    );
    assert!(
        !payload.contains("SECRET-MARKER-BODY") && !payload.contains("00-marker"),
        "counts, never content or filenames: {payload}"
    );
}

/// The silent arm of the goal door: a trusted checkout loaded its steering and
/// is owed nothing.
#[test]
fn a_trusted_checkouts_goal_run_records_no_withheld_notice() {
    let (dir, home) = steering_workspace();
    goal_run(&dir, &home, &[("STELLA_TRUST_PROJECT", "1")]);
    assert!(
        withheld_events_in_journal(&dir).is_empty(),
        "a trusted workspace loaded its steering and is owed no event"
    );
}

/// A one-task `stella fleet` fan-out in `dir`, against the same closed
/// loopback port [`run_stream_json`] uses and for the same reason: the notice
/// goes on the attempt's channel when it opens, before a provider is asked
/// for anything. A positional prompt is a SHARED-tree task, so the attempt's
/// journal is the invocation workspace's own store —
/// [`withheld_events_in_journal`] reads it unchanged.
///
/// The fleet pins its base to a sha before dispatching anything, so the
/// workspace has to be a git repository with one commit first.
fn fleet_run(dir: &tempfile::TempDir, home: &tempfile::TempDir, extra_env: &[(&str, &str)]) {
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.email=fleet@test",
            "-c",
            "user.name=fleet",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        let status = Command::new("git")
            .args(&args)
            .current_dir(dir.path())
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_stella"));
    command
        .args([
            "--model",
            "openrouter/z-ai/glm-5.1",
            "--api-key",
            "sk-not-a-real-key",
            "--base-url",
            "http://127.0.0.1:1",
            "--spend-limit",
            "0.05",
            "fleet",
            "--output-format",
            "json",
            "say hi and stop",
        ])
        .current_dir(dir.path())
        .env("HOME", home.path())
        .env("STELLA_HOME", home.path())
        .env("STELLA_DATA_DIR", home.path())
        .env("NO_COLOR", "1")
        .env("STELLA_NO_ENV_FILE", "1")
        .env("STELLA_CATALOG_AUTO_REFRESH", "0")
        .env(
            "STELLA_MANAGED_SETTINGS",
            home.path().join("no-managed.json"),
        )
        .env_remove("STELLA_MODEL")
        .env_remove("STELLA_TRUST_PROJECT")
        .env_remove("STELLA_PROJECT_HOOKS")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ZAI_API_KEY")
        .env_remove("OPENAI_API_KEY");
    for (key, value) in extra_env {
        command.env(key, value);
    }
    // The run fails — there is nothing on that port — and that is fine: what
    // is under test is an event sent before the first model call.
    let _ = command.output().expect("run stella fleet");
}

/// **Witness (#4500, the fleet door).** A fleet attempt's journal records the
/// untrusted checkout's refusal — once per attempt, because each worker is
/// its own session in its own workspace (its own store, its own SessionStart
/// hooks, its own system prompt), so the process-wide latch the goal door
/// spends would let the first lane's journal vouch for every sibling's.
///
/// `stella fleet` routed through neither opener that carries the notice, so
/// an untrusted checkout fanning out N workers had the refusal on stderr and
/// in no attempt's journal at all.
#[test]
fn an_untrusted_checkouts_withheld_steering_reaches_a_fleet_attempts_journal() {
    let (dir, home) = steering_workspace();
    fleet_run(&dir, &home, &[]);

    let events = withheld_events_in_journal(&dir);
    assert_eq!(
        events.len(),
        1,
        "one attempt is owed exactly one withheld-steering event: {events:?}"
    );
    let payload = &events[0];
    assert!(
        payload.contains("\"withheld_by\":\"project_untrusted\""),
        "the event must name the authority, because it decides the remedy: {payload}"
    );
    assert!(
        payload.contains("\"memories\":1") && payload.contains("\"records\":1"),
        "the event must say how much was withheld: {payload}"
    );
    assert!(
        !payload.contains("SECRET-MARKER-BODY") && !payload.contains("00-marker"),
        "counts, never content or filenames: {payload}"
    );
}

/// The silent arm of the fleet door: a trusted checkout loaded its steering
/// and is owed nothing.
#[test]
fn a_trusted_checkouts_fleet_run_records_no_withheld_notice() {
    let (dir, home) = steering_workspace();
    fleet_run(&dir, &home, &[("STELLA_TRUST_PROJECT", "1")]);
    assert!(
        withheld_events_in_journal(&dir).is_empty(),
        "a trusted workspace loaded its steering and is owed no event"
    );
}

/// The silent arm on the machine channel: a trusted checkout is getting its
/// steering, so the stream says nothing about a suppression that did not
/// happen.
#[test]
fn a_trusted_checkouts_event_stream_carries_no_withheld_notice() {
    let (dir, home) = steering_workspace();
    let output = run_stream_json(&dir, &home, &[("STELLA_TRUST_PROJECT", "1")]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("steering_withheld"),
        "a trusted workspace loaded its steering and is owed no event: {stdout}"
    );
}
