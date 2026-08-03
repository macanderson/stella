//! CLI argument-surface tests: the everything-global invariant on
//! [`GlobalArgs`](super::GlobalArgs) and the parse positions it exists to
//! guarantee. These are the regression fence for "`--budget` is an
//! unexpected argument after `stella fleet`" — the flag was defined at the
//! root without `global = true`, so clap only accepted it *before* the
//! subcommand while README and muscle memory put it after.

use clap::{CommandFactory, Parser};

use super::build_info::{
    BUILD_VERSION_IDENTITY, long_version_static, version_static, version_string,
};
use super::term_policy::{
    PlainReason, accessible_env, animation_disabled, deck_decision, dumb_terminal,
};
use super::{AuthCmd, Cli, Command, ConnectCmd, OutputFormat, TelemetryCmd, error_summary_json};

/// `-V` stays one greppable line; `--version` answers the two questions a bug
/// report otherwise costs a round trip. Both must agree about the version
/// itself — they read the same `build.rs` literal, and this pins that.
#[test]
fn the_long_version_extends_the_short_one_without_contradicting_it() {
    let long = long_version_static();
    assert!(
        long.starts_with(version_string()),
        "the long version must lead with the exact short version: {long}"
    );
    assert!(long.contains("target:"), "{long}");
    assert!(long.contains("profile:"), "{long}");
    // The claim launcher attests the NUL-delimited identity bytes; the long
    // string is a separate literal and must not carry NULs into them.
    assert!(!long.contains('\0'), "{long}");
}

/// Completions are generated from the live `Command` tree, so a new
/// subcommand is completable the day it lands. This proves the tree is
/// actually generatable (clap panics on an inconsistent one) and that the
/// output names the binary rather than the crate.
#[test]
fn completions_generate_for_every_supported_shell() {
    for shell in [
        clap_complete::Shell::Bash,
        clap_complete::Shell::Zsh,
        clap_complete::Shell::Fish,
        clap_complete::Shell::PowerShell,
        clap_complete::Shell::Elvish,
    ] {
        let mut command = Cli::command();
        let mut out: Vec<u8> = Vec::new();
        clap_complete::generate(shell, &mut command, "stella", &mut out);
        let script = String::from_utf8(out).expect("completion scripts are UTF-8");
        assert!(!script.is_empty(), "{shell} produced nothing");
        assert!(
            script.contains("stella"),
            "{shell} script must name the `stella` binary, not the crate"
        );
        // A representative leaf from the real tree — proof the generator saw
        // the subcommands and not just the root.
        assert!(
            script.contains("scoreboard"),
            "{shell} script is missing subcommands"
        );
    }
}

#[test]
fn completions_parse_for_each_shell_name() {
    for name in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let cli = Cli::try_parse_from(["stella", "completions", name])
            .unwrap_or_else(|e| panic!("`stella completions {name}` must parse: {e}"));
        assert!(matches!(cli.command, Some(Command::Completions { .. })));
    }
    assert!(
        Cli::try_parse_from(["stella", "completions", "tcsh"]).is_err(),
        "an unsupported shell must be a usage error, not a silent empty script"
    );
}

/// The build script owns version stamping so both CLI surfaces consume the
/// exact same compile-time literal. In particular, main must not reconstruct
/// `-dev.<sha>` at runtime: the benchmark launcher attests those contiguous
/// bytes in the executable before it permits a paid claim run.
#[test]
fn cli_versions_use_the_build_script_literal() {
    assert!(BUILD_VERSION_IDENTITY.starts_with('\0'));
    assert!(BUILD_VERSION_IDENTITY.ends_with('\0'));
    assert_eq!(version_string(), env!("STELLA_BUILD_VERSION"));
    assert_eq!(version_static(), env!("STELLA_BUILD_VERSION"));

    let expected = match option_env!("STELLA_BUILD_GIT_SHA") {
        Some(sha) if !sha.is_empty() => format!("{}-dev.{sha}", env!("CARGO_PKG_VERSION")),
        _ => env!("CARGO_PKG_VERSION").to_string(),
    };
    assert_eq!(version_string(), expected);
}

#[cfg(unix)]
fn legacy_codegraph_workspace(mode: u32) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let dot = dir.path().join(".stella");
    std::fs::create_dir_all(&dot).unwrap();
    std::fs::set_permissions(&dot, std::fs::Permissions::from_mode(mode)).unwrap();
    let graph = stella_graph::CodeGraph::open(dir.path(), &dot.join("codegraph.db")).unwrap();
    graph.shutdown();
    dir
}

#[cfg(unix)]
#[test]
fn storage_snapshot_preflight_migrates_safe_legacy_codegraph() {
    let dir = legacy_codegraph_workspace(0o700);
    crate::storage_cmd::load_storage_snapshot_checked(dir.path()).unwrap();
    assert!(!dir.path().join(".stella/codegraph.db").exists());
    assert!(dir.path().join(".stella/private/codegraph.db").exists());
}

#[cfg(unix)]
#[test]
fn storage_snapshot_preflight_reports_unsafe_legacy_codegraph() {
    let dir = legacy_codegraph_workspace(0o777);
    let error = crate::storage_cmd::load_storage_snapshot_checked(dir.path())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("legacy") && error.contains("private"),
        "{error}"
    );
    assert!(dir.path().join(".stella/codegraph.db").exists());
}

#[cfg(unix)]
#[test]
fn observatory_preflight_migrates_safe_legacy_sqlite_stores() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let dot = dir.path().join(".stella");
    std::fs::create_dir_all(&dot).unwrap();
    std::fs::set_permissions(&dot, std::fs::Permissions::from_mode(0o700)).unwrap();
    for name in ["store.db", "fleet.db", "context.db", "codegraph.db"] {
        std::fs::write(dot.join(name), b"closed legacy sqlite file").unwrap();
    }

    crate::storage_cmd::preflight_observatory_stores(dir.path()).unwrap();
    for name in ["store.db", "fleet.db", "context.db", "codegraph.db"] {
        assert!(!dot.join(name).exists());
        assert!(dot.join("private").join(name).exists());
    }
}

#[cfg(unix)]
#[test]
fn observatory_preflight_reports_unsafe_legacy_store() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let dot = dir.path().join(".stella");
    std::fs::create_dir_all(&dot).unwrap();
    std::fs::set_permissions(&dot, std::fs::Permissions::from_mode(0o777)).unwrap();
    std::fs::write(dot.join("store.db"), b"unsafe legacy sqlite file").unwrap();

    let error = crate::storage_cmd::preflight_observatory_stores(dir.path()).unwrap_err();
    assert!(
        error.contains("legacy") && error.contains("private"),
        "{error}"
    );
    assert!(dot.join("store.db").exists());
}

/// `colored` honours `NO_COLOR`, `CLICOLOR*`, and a non-tty stream, but not
/// `TERM` — so a dumb terminal (Emacs `M-x shell`, a serial console, an
/// editor's build pane, `TERM=dumb` in CI) rendered every escape sequence
/// literally. Every other Unix tool that colours output treats `dumb` as
/// "this terminal cannot".
#[test]
fn a_dumb_terminal_disables_colour_unless_the_user_forces_it() {
    use std::ffi::OsStr;
    let term = |s: &str| Some(OsStr::new(s)).map(|o| o.to_owned());

    assert!(dumb_terminal(term("dumb").as_deref(), None));
    // Force is the documented "I know what my terminal is" escape hatch and
    // must outrank the heuristic, in both spellings of "off".
    assert!(!dumb_terminal(
        term("dumb").as_deref(),
        term("1").as_deref()
    ));
    assert!(dumb_terminal(term("dumb").as_deref(), term("0").as_deref()));
    assert!(dumb_terminal(term("dumb").as_deref(), term("").as_deref()));

    // Real terminals, and an unset TERM, are left entirely alone — the tty
    // and NO_COLOR checks `colored` already performs stay the authority.
    assert!(!dumb_terminal(term("xterm-256color").as_deref(), None));
    assert!(!dumb_terminal(term("dumb-but-not-really").as_deref(), None));
    assert!(!dumb_terminal(None, None));
}

/// `--accessible` has an env synonym so a screen-reader user can set it once,
/// in their shell profile, instead of remembering a flag on every invocation
/// (#1258). It follows this CLI's house convention for boolean env vars, where
/// an explicit `0` means off — the same rule `STELLA_PLAIN` and
/// `STELLA_NO_ENV_FILE` follow, so there is one convention to learn.
#[test]
fn the_accessible_env_var_follows_the_house_boolean_convention() {
    use std::ffi::OsStr;
    let v = |s: &str| Some(OsStr::new(s)).map(|o| o.to_owned());

    assert!(accessible_env(v("1").as_deref()));
    assert!(accessible_env(v("yes").as_deref()));
    assert!(!accessible_env(v("0").as_deref()));
    assert!(!accessible_env(v("").as_deref()));
    assert!(!accessible_env(None));
}

/// Accessible mode is a mode ON the deck, not a third surface beside it — so
/// it takes no part in the deck-or-REPL decision, and `--plain` keeps meaning
/// exactly what it meant. Pinned because the surface it replaced (#1237's
/// `--simple`) *was* a third branch here, and re-introducing one is how the
/// second-class tier comes back.
#[test]
fn accessible_mode_does_not_change_the_deck_or_repl_decision() {
    assert_eq!(deck_decision(false, false, true, true), None);
    assert_eq!(
        deck_decision(true, false, true, true),
        Some(PlainReason::Flag),
        "--plain still wins on its own terms"
    );
}

/// The deck and the line REPL are different programs: the REPL has no prompt
/// queue, no mid-turn steering, and exits at stdin EOF. Downgrading between
/// them without saying so presents as "stella dies when a turn completes",
/// because the surface that would have queued the next prompt is not running
/// and nothing said so. Every fallback path must therefore name a reason —
/// that is what the caller prints.
#[test]
fn every_plain_fallback_names_a_reason_and_a_real_terminal_gets_the_deck() {
    // The only combination that earns the deck.
    assert_eq!(deck_decision(false, false, true, true), None);

    // Explicit opt-outs outrank stream shape: someone who passed `--plain`
    // does not need to be told about their tty.
    assert_eq!(
        deck_decision(true, false, true, true),
        Some(PlainReason::Flag)
    );
    assert_eq!(
        deck_decision(false, true, true, true),
        Some(PlainReason::Env)
    );
    assert_eq!(
        deck_decision(true, false, false, false),
        Some(PlainReason::Flag),
        "the flag is reported ahead of the ttys it makes irrelevant"
    );

    // A piped stdin is the case behind the bug report: the deck never starts,
    // and the REPL exits the moment that pipe ends.
    assert_eq!(
        deck_decision(false, false, false, true),
        Some(PlainReason::StdinNotTty)
    );
    assert_eq!(
        deck_decision(false, false, true, false),
        Some(PlainReason::StdoutNotTty)
    );

    // No fallback is allowed to be silent — an empty explanation would print
    // a notice that says nothing.
    for reason in [
        PlainReason::Flag,
        PlainReason::Env,
        PlainReason::StdinNotTty,
        PlainReason::StdoutNotTty,
    ] {
        assert!(!reason.explain().is_empty(), "{reason:?} explains nothing");
    }
}

/// `--no-anim`'s help promises "Also forced on by STELLA_NO_ANIM or NO_COLOR",
/// and for the Command Deck — the surface that actually shimmers, and the one
/// an asciinema recording or CI log captures — nothing folded either signal
/// in. The flag's own documentation was the bug report.
#[test]
fn the_env_signals_no_anim_advertises_actually_force_it() {
    let _env = crate::test_env::lock();
    let restore = |name: &str, prior: Option<std::ffi::OsString>| unsafe {
        match prior {
            Some(v) => std::env::set_var(name, v),
            None => std::env::remove_var(name),
        }
    };
    let prior_anim = std::env::var_os("STELLA_NO_ANIM");
    let prior_color = std::env::var_os("NO_COLOR");
    // SAFETY: serialized behind the binary-wide env lock, and both names are
    // restored below whatever the assertions do.
    unsafe {
        std::env::remove_var("STELLA_NO_ANIM");
        std::env::remove_var("NO_COLOR");
    }

    assert!(!animation_disabled(false), "clean env animates");
    assert!(animation_disabled(true), "the flag alone suffices");

    unsafe { std::env::set_var("STELLA_NO_ANIM", "1") };
    assert!(animation_disabled(false));
    // House convention for boolean env vars in this CLI (`STELLA_PLAIN`,
    // `STELLA_NO_ENV_FILE`): an explicit `0` means off.
    unsafe { std::env::set_var("STELLA_NO_ANIM", "0") };
    assert!(!animation_disabled(false));
    unsafe { std::env::remove_var("STELLA_NO_ANIM") };

    unsafe { std::env::set_var("NO_COLOR", "1") };
    assert!(animation_disabled(false));
    // NO_COLOR's published rule: present *and non-empty*.
    unsafe { std::env::set_var("NO_COLOR", "") };
    assert!(!animation_disabled(false));

    restore("STELLA_NO_ANIM", prior_anim);
    restore("NO_COLOR", prior_color);
}

/// clap's own consistency audit (conflicting ids, broken defaults,
/// mis-typed value parsers) — panics on the first violation. Runs the same
/// checks release builds skip, so a bad arg table fails here instead of at
/// a user's first `--help`.
#[test]
fn clap_command_is_internally_consistent() {
    Cli::command().debug_assert();
}

#[test]
fn telemetry_status_remains_a_distinct_top_level_command() {
    let cli = Cli::try_parse_from(["stella", "telemetry", "status"])
        .expect("`telemetry status` must parse independently of adjacent auth commands");
    assert!(matches!(
        cli.command,
        Some(Command::Telemetry {
            cmd: TelemetryCmd::Status
        })
    ));
}

/// `doctor` runs on local state alone, so it must parse (and dispatch) without
/// a provider or key — bare, with the opt-in repair, and with the crash-dump
/// reader `--last-failure` (docs/design/diagnostics/diagnostics.md §7.4), which is the one
/// subcommand a user reaches for precisely when nothing else works.
#[test]
fn doctor_parses_bare_and_with_repair() {
    let bare = Cli::try_parse_from(["stella", "doctor"]).expect("`doctor` must parse bare");
    assert!(matches!(
        bare.command,
        Some(Command::Doctor {
            repair: false,
            last_failure: false
        })
    ));
    let repair =
        Cli::try_parse_from(["stella", "doctor", "--repair"]).expect("`doctor --repair` parses");
    assert!(matches!(
        repair.command,
        Some(Command::Doctor { repair: true, .. })
    ));
    let last = Cli::try_parse_from(["stella", "doctor", "--last-failure"])
        .expect("`doctor --last-failure` parses");
    assert!(matches!(
        last.command,
        Some(Command::Doctor {
            last_failure: true,
            ..
        })
    ));
}

/// The load-bearing invariant: every flag defined at the root MUST be
/// `global = true`, or it is silently unaccepted after a subcommand token
/// (`stella fleet … --budget 5` → "unexpected argument"). Introspects the
/// built command rather than the source so any future root flag — added to
/// `GlobalArgs` or straight onto `Cli` — is covered without editing this
/// test. `help`/`version` are clap built-ins with their own propagation.
#[test]
fn every_root_flag_is_global() {
    let cmd = Cli::command();
    for arg in cmd.get_arguments() {
        let id = arg.get_id().as_str();
        if id == "help" || id == "version" {
            continue;
        }
        assert!(
            arg.is_global_set(),
            "root-level flag `--{id}` is not `global = true`: it will be rejected \
             when typed after a subcommand (e.g. `stella fleet … --{id}`). Add \
             `global = true` to its #[arg] in GlobalArgs (see the struct doc)."
        );
    }
}

/// The exact invocation shape README advertises for fleets — session flags
/// after the subcommand — must parse, and the values must land at the root.
#[test]
fn session_flags_parse_after_subcommand() {
    let cli = Cli::try_parse_from([
        "stella",
        "fleet",
        "fix the flaky auth test",
        "--max-concurrency",
        "2",
        "--budget",
        "5.0",
        "--model",
        "zai/glm-5.2",
    ])
    .expect("session flags after `fleet` must parse");
    assert_eq!(cli.globals.budget, Some(5.0));
    assert_eq!(cli.globals.model.as_deref(), Some("zai/glm-5.2"));
    match cli.command {
        Some(Command::Fleet {
            tasks,
            max_concurrency,
            ..
        }) => {
            assert_eq!(tasks, vec!["fix the flaky auth test".to_string()]);
            assert_eq!(max_concurrency, 2);
        }
        _ => panic!("expected the fleet subcommand"),
    }
}

/// The pre-subcommand position keeps working — global args are valid in
/// both positions, not moved.
#[test]
fn session_flags_parse_before_subcommand() {
    let cli = Cli::try_parse_from([
        "stella",
        "--budget",
        "2.5",
        "--output-format",
        "json",
        "run",
        "hi",
    ])
    .expect("session flags before the subcommand must keep parsing");
    assert_eq!(cli.globals.budget, Some(2.5));
    assert_eq!(cli.globals.output_format, OutputFormat::Json);
}

/// `--budget`'s value parser still guards after a subcommand: a
/// non-positive limit would silently disarm the hard spend cap, so it must
/// be a parse error in every position.
#[test]
fn budget_validation_applies_after_subcommand() {
    let err = Cli::try_parse_from(["stella", "fleet", "task", "--budget", "0"])
        .err()
        .expect("a zero budget must be rejected");
    assert!(
        err.to_string().contains("positive dollar amount"),
        "unexpected error: {err}"
    );
}

/// Global names are reserved CLI-wide. A subcommand flag with the same id
/// as a root global does NOT shadow it cleanly: clap propagates the
/// global's value slot into every subcommand, and the collision panics at
/// match-downcast time in debug builds and misbinds in release —
/// `debug_assert` does not catch it (this is exactly how `connect linear`'s
/// old `--api-key` bool blew up against the global credential string).
/// Walk every subcommand recursively so a reintroduction anywhere fails
/// here, not in a user's terminal.
#[test]
fn no_subcommand_flag_reuses_a_global_name() {
    fn check(cmd: &clap::Command, globals: &[String], path: &str) {
        for sub in cmd.get_subcommands() {
            let sub_path = format!("{path} {}", sub.get_name());
            for arg in sub.get_arguments() {
                let id = arg.get_id().as_str();
                // Propagated copies of the globals themselves are fine —
                // only a *local* redefinition collides.
                if !arg.is_global_set() && globals.iter().any(|g| g == id) {
                    panic!(
                        "`{sub_path}` defines its own `--{id}`, colliding with the \
                         session-wide global of that name — pick a different flag \
                         name (see the GlobalArgs doc)."
                    );
                }
            }
            check(sub, globals, &sub_path);
        }
    }
    let mut cmd = Cli::command();
    cmd.build();
    let globals: Vec<String> = cmd
        .get_arguments()
        .filter(|a| a.is_global_set())
        .map(|a| a.get_id().to_string())
        .collect();
    assert!(!globals.is_empty(), "expected at least one global flag");
    check(&cmd, &globals, "stella");
}

/// `connect linear --paste-key` (the rename that resolved the collision
/// above) binds to the subcommand's bool, and the session-wide `--api-key`
/// stays independently usable on the same command line.
#[test]
fn connect_linear_paste_key_parses() {
    let cli = Cli::try_parse_from(["stella", "connect", "linear", "--paste-key"])
        .expect("`connect linear --paste-key` must parse");
    assert_eq!(cli.globals.api_key, None);
    match cli.command {
        Some(Command::Connect {
            cmd: ConnectCmd::Linear { paste_key },
        }) => assert!(paste_key),
        _ => panic!("expected `connect linear`"),
    }

    let cli = Cli::try_parse_from(["stella", "connect", "linear", "--api-key", "sk-model"])
        .expect("the global --api-key must parse after `connect linear`");
    assert_eq!(cli.globals.api_key.as_deref(), Some("sk-model"));
    match cli.command {
        Some(Command::Connect {
            cmd: ConnectCmd::Linear { paste_key },
        }) => assert!(!paste_key),
        _ => panic!("expected `connect linear`"),
    }
}

/// `stella auth set <provider> --key <k>` parses, and the global `--api-key`
/// (a different secret — the model-provider credential) still parses
/// independently on the same command line, same shape as `connect linear`.
#[test]
fn auth_set_key_parses_and_stays_independent_of_the_global_api_key() {
    let cli = Cli::try_parse_from([
        "stella",
        "auth",
        "set",
        "zai",
        "--key",
        "sk-stored",
        "--api-key",
        "sk-model",
    ])
    .expect("`auth set <provider> --key <k>` must parse alongside the global --api-key");
    assert_eq!(cli.globals.api_key.as_deref(), Some("sk-model"));
    match cli.command {
        Some(Command::Auth {
            cmd:
                AuthCmd::Set {
                    provider,
                    key,
                    stdin,
                },
        }) => {
            assert_eq!(provider, "zai");
            assert_eq!(key.as_deref(), Some("sk-stored"));
            assert!(!stdin);
        }
        _ => panic!("expected `auth set`"),
    }
}

/// `--key` and `--stdin` are mutually exclusive — a user meaning to pipe a
/// secret in via `--stdin` must not silently also accept a (likely absent)
/// `--key` value.
#[test]
fn auth_set_rejects_key_and_stdin_together() {
    // `Cli` derives no `Debug` (it carries the model API key), so match
    // rather than `expect_err`/`unwrap_err` (both require `T: Debug`).
    match Cli::try_parse_from(["stella", "auth", "set", "zai", "--key", "sk-x", "--stdin"]) {
        Err(e) => assert_eq!(e.kind(), clap::error::ErrorKind::ArgumentConflict),
        Ok(_) => panic!("--key and --stdin must conflict"),
    }
}

#[test]
fn auth_remove_and_list_parse() {
    let cli = Cli::try_parse_from(["stella", "auth", "remove", "zai"])
        .expect("`auth remove <provider>` must parse");
    match cli.command {
        Some(Command::Auth {
            cmd: AuthCmd::Remove { provider },
        }) => assert_eq!(provider, "zai"),
        _ => panic!("expected `auth remove`"),
    }

    let cli = Cli::try_parse_from(["stella", "auth", "list"]).expect("`auth list` must parse");
    assert!(matches!(
        cli.command,
        Some(Command::Auth { cmd: AuthCmd::List })
    ));
}

/// Witness for the `--output-format json|stream-json` pre-flight contract: a
/// failure returned by `run()` before the agent exists (no API key, unknown
/// provider, unknown model, a malformed settings file) used to leave stdout
/// completely empty, so a headless script got nothing to parse for the single
/// most likely failure it hits.
#[test]
fn pre_flight_failures_emit_a_machine_readable_error_envelope() {
    let msg = "no API key found for provider `zai`";

    let pretty = error_summary_json(OutputFormat::Json, msg).expect("json must emit an envelope");
    let parsed: serde_json::Value = serde_json::from_str(&pretty).unwrap();
    assert_eq!(parsed["status"], "error");
    assert!(parsed["text"].is_null());
    assert_eq!(parsed["reason"], msg);
    // #644: the pre-flight envelope is a summary like any other, so it declares
    // the same contract version. A consumer that branches on `schema_version`
    // must not have to special-case the earliest failure it can hit.
    assert_eq!(
        parsed["schema_version"],
        serde_json::json!(crate::SUMMARY_SCHEMA_VERSION)
    );

    // Leads with the version, in both encodings — a build convention (every
    // envelope is a struct with `schema_version` declared first), not a promise
    // to consumers, who read by key. See `every_summary_envelope_leads_with_its_version`.
    assert!(
        pretty.starts_with("{\n  \"schema_version\":"),
        "pre-flight envelope must lead with its version, got: {pretty}"
    );

    // stream-json is line-delimited: the envelope must be exactly one line.
    let line =
        error_summary_json(OutputFormat::StreamJson, msg).expect("stream-json must emit one");
    assert!(
        line.starts_with(r#"{"schema_version":"#),
        "compact envelope must lead with its version, got: {line}"
    );
    assert!(
        !line.contains('\n'),
        "stream-json envelope must be one line"
    );
    let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(parsed["reason"], msg);
}

/// The human path keeps stdout clean — its diagnostic is the `stella: …`
/// stderr line, and a JSON object printed there would corrupt piped output.
#[test]
fn text_output_never_gets_an_error_envelope_on_stdout() {
    assert!(error_summary_json(OutputFormat::Text, "boom").is_none());
}

/// The docs section every command page lives in.
const COMMANDS_DOCS_DIR: &str = "website/content/docs/commands";

/// Entries in `meta.json` that are not commands. `index` is the section's own
/// landing page.
const NON_COMMAND_PAGES: &[&str] = &["index"];

/// The repository root, from this crate's manifest directory.
fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("stella-cli must have a parent directory")
        .to_path_buf()
}

/// The `pages` array of the commands section's `meta.json`.
fn documented_pages() -> Vec<String> {
    let meta_path = repo_root().join(COMMANDS_DOCS_DIR).join("meta.json");
    let raw = std::fs::read_to_string(&meta_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", meta_path.display()));
    let meta: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", meta_path.display()));
    meta["pages"]
        .as_array()
        .expect("meta.json must carry a `pages` array")
        .iter()
        .filter_map(|page| page.as_str())
        // Fumadocs separators (`---Group---`) are layout, not pages.
        .filter(|page| !page.starts_with("---"))
        .map(str::to_owned)
        .collect()
}

/// The top-level subcommand names clap itself would render, minus the ones a
/// reader never types. `help` is clap's own built-in, and a hidden command is
/// hidden from `--help` too, so neither owes the docs a page.
fn top_level_subcommands() -> Vec<String> {
    Cli::command()
        .get_subcommands()
        .filter(|sub| !sub.is_hide_set())
        .map(|sub| sub.get_name().to_owned())
        .filter(|name| name != "help")
        .collect()
}

/// Every command `stella --help` lists must have a reference page, and every
/// page must correspond to a real command.
///
/// # Why this is a test rather than a script over `--help`
///
/// It reads the same `Command` tree `--help` renders, but reads it
/// *structurally*. Parsing help text would make the gate a hostage to terminal
/// width, ANSI color, and clap's wrapping — three things that have nothing to
/// do with whether a command is documented. It also needs no new gate step:
/// `make gate` already runs the test suite.
///
/// # Why both directions
///
/// Forward catches the bug that motivated this (#928: seven surfaces shipped
/// with no page, and nothing noticed). Backward catches its mirror — a page
/// outliving the command it documents, which is worse than no page, because a
/// reader has no way to tell a stale page from a current one.
#[test]
fn every_subcommand_has_a_reference_page() {
    let pages = documented_pages();
    let commands = top_level_subcommands();

    let undocumented: Vec<&String> = commands
        .iter()
        .filter(|name| !pages.iter().any(|page| page == *name))
        .collect();
    assert!(
        undocumented.is_empty(),
        "these subcommands have no page in {COMMANDS_DOCS_DIR}/meta.json: {undocumented:?}\n\
         add `website/content/docs/commands/<name>.mdx` and list it in `meta.json`"
    );

    let orphaned: Vec<&String> = pages
        .iter()
        .filter(|page| !NON_COMMAND_PAGES.contains(&page.as_str()))
        .filter(|page| !commands.iter().any(|name| name == *page))
        .collect();
    assert!(
        orphaned.is_empty(),
        "these pages document no existing subcommand: {orphaned:?}\n\
         remove the page, or add it to NON_COMMAND_PAGES if it is not a command reference"
    );

    // A `meta.json` entry with no file behind it renders as a broken nav link.
    for page in &pages {
        let path = repo_root()
            .join(COMMANDS_DOCS_DIR)
            .join(format!("{page}.mdx"));
        assert!(
            path.exists(),
            "meta.json lists `{page}` but {} does not exist",
            path.display()
        );
    }
}

/// A command's own page must name every subcommand that command has.
///
/// This is the check that would have caught the sharpest half of #928:
/// `stella memory` had grown to twelve subcommands while its page documented
/// six, so the reference was provably behind the CLI — and behind the project's
/// own release notes, which described the missing six in prose.
///
/// Nested subcommands do not get their own pages; they are documented inside
/// the parent's. So the assertion is the same one
/// `stella-tools/tests/docs_in_sync.rs` makes about the tool registry: the
/// exact invocation a reader would type, `stella <parent> <child>`, must appear
/// literally in the parent's page.
#[test]
fn every_page_documents_all_of_its_commands_subcommands() {
    let mut missing: Vec<String> = Vec::new();

    for parent in Cli::command()
        .get_subcommands()
        .filter(|s| !s.is_hide_set())
    {
        let parent_name = parent.get_name();
        if parent_name == "help" {
            continue;
        }
        let children: Vec<&str> = parent
            .get_subcommands()
            .filter(|child| !child.is_hide_set())
            .map(clap::Command::get_name)
            .filter(|name| *name != "help")
            .collect();
        if children.is_empty() {
            continue;
        }

        let path = repo_root()
            .join(COMMANDS_DOCS_DIR)
            .join(format!("{parent_name}.mdx"));
        let Ok(page) = std::fs::read_to_string(&path) else {
            // The other test owns "this page is missing entirely"; reporting it
            // twice would make one omission look like two problems.
            continue;
        };

        for child in children {
            if !page.contains(&format!("stella {parent_name} {child}")) {
                missing.push(format!("stella {parent_name} {child}"));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "these subcommands are not documented on their command's page: {missing:#?}\n\
         each must appear literally as `stella <parent> <child>` in \
         {COMMANDS_DOCS_DIR}/<parent>.mdx"
    );
}
