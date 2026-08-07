// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella` — a fast, BYOK, model-agnostic terminal coding agent.
//!
//! Built on the `stella-*` crate stack: `stella-model` for provider
//! abstraction (Z.ai/GLM 5.2, Anthropic, OpenAI, xAI, DeepSeek, Gemini
//! direct, Vertex AI, Amazon Bedrock, OpenRouter — plus any local
//! OpenAI-compatible endpoint via `--base-url`), `stella-core` for the
//! step-driver engine, `stella-tools` for the built-in tool set, and
//! `stella-protocol` for the shared types.
//!
//! Design goals:
//! - No phone-home requirement — works with zero network calls other than
//!   the user's configured model provider.
//! - BYOK: any provider key, any combination, no account.
//! - Speed: streaming first, prompt-cache-aware system prefix, minimal
//!   overhead between model turns.
//! - Headless one-shot: `stella run --output-format text|json|stream-json`
//!   for scripting (the interactive `chat`/`goal`/`monitor` modes render
//!   human-readable output).

mod accounted_call;
mod agent;
mod agents_installed;
mod arena;
mod attachments;
mod auth_cmd;
mod build_info;
mod cache_insight;
mod candidate_ws;
mod claims;
mod cli;
mod cloud_drain;
mod command_deck;
mod commands_cmd;
mod config;
mod config_wiring;
mod connect_cmd;
mod context_cmd;
mod context_records;
mod contextgraph;
mod credential_handoff;
mod credential_status;
mod daemon;
// #872, the first slice of #836: the redacted training-trajectory exporter.
mod dataset_cmd;
mod deck_mcp;
mod diag_boot;
mod diag_bridge;
mod discovery;
mod doctor;
mod domains;
mod durability;
mod engine_config;
mod enterprise_telemetry;
mod env_files;
mod export;
mod extensions;
mod failure;
mod fleet_claims;
mod fleet_cmd;
mod fleet_commits;
mod fleet_gc;
mod fleet_spend;
mod fleet_verbs;
mod fleet_warmth;
mod ingest_cmd;
mod init_fx;
mod inspect;
mod interactive;
mod mcp_cmd;
mod memory;
mod memory_cmd;
mod memory_compact;
mod memory_index;
mod memory_retire_cmd;
mod model_catalog;
mod paths;
mod scoreboard_cmd;
mod self_driving_cmd;
// The `/profile` posture planner (fast · balanced · pro · ultra).
mod profile;
// Phase 3 (#714): the adaptive-context proposal review surface.
mod prompt_source;
mod proposals_cmd;
mod query_format;
mod resume_frame;
mod rules;
mod runtime;
mod scripts_cmd;
mod session_persist;
mod settings;
mod settings_check;
mod signals;
mod skill_manager;
mod startup;
mod stats;
mod stats_graph;
mod storage_cmd;
mod subagent;
mod subsession;
mod term_policy;
mod timefmt;
mod tool_foundry;
mod tool_policy;
mod tool_switches;
mod trace;
mod tui;
mod tune_cmd;
mod turn_diff;
mod usage_cmd;

/// Serializes tests that mutate process environment variables. `setenv` /
/// `getenv` from concurrent threads is documented UB on POSIX, and the test
/// harness runs this binary's test modules on parallel threads — so every
/// test that calls `std::env::set_var`/`remove_var` (agent.rs provider
/// routing, config.rs key resolution) must hold this lock for its whole
/// mutate-read-cleanup window.
#[cfg(test)]
pub(crate) mod test_env {
    /// Acquire the env lock, recovering from a poisoned mutex (a prior
    /// env-mutating test that panicked mid-hold must not cascade).
    pub(crate) fn lock() -> std::sync::MutexGuard<'static, ()> {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Captures the current value of each named env var and restores it on
    /// drop. Unlike a hand-rolled "read the old value, mutate, restore at
    /// the end of the test" sequence, this also restores on an unwinding
    /// panic (e.g. a failed `assert!` mid-test) — without it, a panicking
    /// test leaves `HOME`/`STELLA_DATA_DIR`/etc. mutated for every test that
    /// runs after it in this binary. Callers must hold [`lock`] for the
    /// entire lifetime of the returned guard.
    #[must_use]
    pub(crate) struct EnvRestore(Vec<(String, Option<std::ffi::OsString>)>);

    impl EnvRestore {
        pub(crate) fn capture(names: &[&str]) -> Self {
            Self(
                names
                    .iter()
                    .map(|name| ((*name).to_string(), std::env::var_os(name)))
                    .collect(),
            )
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            unsafe {
                for (name, value) in self.0.drain(..) {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }

    /// Witness for #911: the hand-rolled "capture previous, mutate, restore
    /// at the end of the function" sequence this replaced across ~42 tests
    /// never runs its restore step when an assertion panics first — this
    /// fails on that pattern and passes on [`EnvRestore`], whose `Drop` runs
    /// during unwinding too.
    #[test]
    fn restore_runs_on_unwind_not_just_normal_return() {
        let _outer = lock();
        let key = "STELLA_TEST_ENV_RESTORE_WITNESS";
        let original = std::env::var_os(key);
        unsafe { std::env::remove_var(key) };

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _restore = EnvRestore::capture(&[key]);
            unsafe { std::env::set_var(key, "mutated-by-panicking-test") };
            panic!("simulated assertion failure mid-test");
        }))
        .is_err();

        assert!(panicked, "the closure must have actually unwound");
        assert_eq!(
            std::env::var_os(key),
            original,
            "EnvRestore must undo the mutation even when the guarded body panics"
        );
    }
}

use std::io::IsTerminal;
use std::process::ExitCode;

use clap::{FromArgMatches, ValueEnum};
use colored::Colorize;

/// How turn output reaches the caller. `stream-json` is a line-per-`AgentEvent`
/// serialization of the exact protocol enum — a stable machine interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Human-oriented interactive rendering (default).
    Text,
    /// One final JSON object summarizing the turn (headless).
    Json,
    /// One JSON line per AgentEvent as it happens (headless streaming).
    StreamJson,
}

/// Set once a machine-readable summary object has already reached stdout for
/// this process, so [`emit_error_summary`] never follows it with a second
/// envelope describing the same failure. `agent.rs` prints its summary and
/// then still returns `Err` for a verification failure or a hard pipeline
/// error, which would otherwise land in `main`'s catch-all twice.
static JSON_SUMMARY_EMITTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Record that a `--output-format json|stream-json` summary object has been
/// written to stdout.
pub(crate) fn note_json_summary_emitted() {
    JSON_SUMMARY_EMITTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// The version of the `--output-format json|stream-json` summary envelope this
/// build emits. Every summary object carries it — the pipeline summary, the raw
/// step-loop summary, the pre-flight error envelope, and the detached-launch
/// summary ([`crate::daemon::detach`]) — so a consumer can branch on the shape
/// instead of sniffing for keys.
///
/// All four envelopes are structs with the version declared first, so a derived
/// `Serialize` heads the object — a courtesy, not a promise: key order stays
/// outside the contract and consumers must read by key. Building any with
/// `serde_json::json!` would undo that (a `json!` object is a sorted map that
/// buries `schema_version` mid-envelope).
///
/// # When to bump
///
/// Increment only when a consumer written against the previous version could
/// break: a key removed or renamed, a value's type changed, or a key's *meaning*
/// changed while its name and type stay the same. Do **not** bump for a purely
/// additive key — consumers must ignore keys they do not recognize, so an
/// addition cannot break a correct client and bumping would burn the signal
/// (#644).
///
/// The `events` array is out of scope: the event vocabulary carries its own
/// forward-compatibility contract and never bumps this number. The
/// consumer-facing statement lives in `website/content/docs/scripting.mdx`; keep
/// the two in step.
pub(crate) const SUMMARY_SCHEMA_VERSION: u32 = 1;

/// The stdout envelope for a failure under `--output-format json|stream-json`,
/// or `None` when the caller asked for human output (a text run must keep
/// stdout clean; its diagnostic is the stderr line).
///
/// Pre-configuration failures — no API key, unknown provider, unknown model, a
/// malformed settings file — are returned by `run()` before an agent exists,
/// so they never reach `agent.rs`'s summary. Emitting the same
/// `{"schema_version":…,"status":"error","text":null,"reason":…}` shape here
/// means the single most likely headless failure is no longer answered with
/// empty stdout. `stream-json` gets it compact, so the line-delimited contract
/// holds.
///
/// Built from a struct rather than `serde_json::json!` for the key order: a
/// `json!` object is a sorted map, which would bury `schema_version` in the
/// middle of the envelope, while a derived `Serialize` emits fields in
/// declaration order. Order is not part of the contract — consumers read by key
/// — but a version a human can see at a glance is worth the struct.
#[derive(serde::Serialize)]
struct PreflightErrorSummary<'a> {
    schema_version: u32,
    status: &'static str,
    text: Option<&'a str>,
    reason: &'a str,
}

pub(crate) fn error_summary_json(format: OutputFormat, msg: &str) -> Option<String> {
    let value = PreflightErrorSummary {
        schema_version: SUMMARY_SCHEMA_VERSION,
        status: "error",
        text: None,
        reason: msg,
    };
    // A struct of one integer and three string fields cannot fail to
    // serialize; the fallback keeps the contract rather than proving the point.
    let fallback = || format!(r#"{{"schema_version":{SUMMARY_SCHEMA_VERSION},"status":"error"}}"#);
    match format {
        OutputFormat::Text => None,
        OutputFormat::Json => {
            Some(serde_json::to_string_pretty(&value).unwrap_or_else(|_| fallback()))
        }
        OutputFormat::StreamJson => {
            Some(serde_json::to_string(&value).unwrap_or_else(|_| fallback()))
        }
    }
}

/// Print [`error_summary_json`] unless a summary already went out for this
/// failure.
fn emit_error_summary(format: OutputFormat, msg: &str) {
    if JSON_SUMMARY_EMITTED.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    if let Some(line) = error_summary_json(format, msg) {
        println!("{line}");
    }
}

// The argument tree itself lives in `cli.rs`; re-exported at the crate root so
// the per-command modules keep addressing their own subcommand enum as
// `crate::AuthCmd`, `crate::McpCmd`, … regardless of which file defines it.
pub(crate) use cli::{
    AuthCmd, Cli, Command, ConnectCmd, DaemonCmd, McpCmd, MigrateCmd, ModelsCmd, TelemetryCmd,
};

/// How this invocation meets the supervisor (#1552, #1607).
///
/// [`daemon::detach::Posture::Foreground`] runs the work in this process, exactly as every
/// release before supervision existed. Supervision costs no capability any
/// more: a plan that expands scope parks and asks through the session sidecar
/// (#1585) instead of dying at the headless scope-review error, so there is no
/// longer a downgrade to warn about here. [`daemon::detach::Posture::Detached`] is the same
/// supervised child with the launcher not staying — `--detach`.
///
/// Computed here, once, because it is the only place the parsed flags and
/// the real terminal handle are both in hand; each long-running arm then
/// asks one question instead of re-deriving four.
fn supervision(globals: &cli::GlobalArgs) -> daemon::detach::Posture {
    daemon::detach::posture(
        globals.foreground,
        globals.detach,
        daemon::supervised_id().is_some(),
        daemon::has_controlling_terminal(),
    )
}

/// The registry title for a supervised run: the same
/// `<workspace>: <prompt…>` shape a session announces for itself, so a run
/// reads identically in `stella daemon list` and in the deck's SESSIONS view.
fn supervised_title(cfg: &config::Config, what: &str) -> String {
    let name = cfg
        .workspace_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| cfg.workspace_root.display().to_string());
    format!("{name}: {}", command_deck::prompt_line(what, 48))
}

/// `stella resume --list`: every session in the machine-wide registry,
/// newest activity first, with the rows resumable from THIS directory
/// marked `↩`. Local reads only — works with zero API keys.
fn run_resume_list() -> Result<(), String> {
    let registry = stella_store::SessionRegistry::open_default();
    let mut sessions = registry.list();
    if sessions.is_empty() {
        println!("no stella sessions recorded on this machine yet");
        return Ok(());
    }
    sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at_ms));
    let cwd = std::env::current_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    println!("{:2} {:<24} {:<12} SESSION", "", "ID", "STATUS");
    for s in &sessions {
        let resumable = registry.resumable(&s.id);
        let marker = if resumable && s.workspace == cwd {
            "↩"
        } else if resumable {
            "·"
        } else {
            " "
        };
        let title = if s.title.is_empty() {
            s.workspace.clone()
        } else {
            s.title.clone()
        };
        println!(
            "{marker:2} {:<24} {:<12} {title}",
            s.id,
            stella_store::SessionRegistry::presented_status(s).label(),
        );
    }
    println!(
        "\n↩ resumable here (`stella resume [ID]`) · resumable from its own workspace\n\
         inside the deck: `←` on an empty prompt opens SESSIONS, `⏎` reopens a session"
    );
    Ok(())
}

fn main() -> ExitCode {
    // Restore the default SIGPIPE disposition. Rust masks SIGPIPE at startup, so
    // writing to a closed stdout (`stella tools | head`, `… | grep -q`, a piped
    // reader that quits) surfaces as an EPIPE that `println!` *panics* on — and
    // with panic=abort that's a SIGABRT + a scary panic dump on a routine pipe.
    // Resetting to SIG_DFL makes the process exit quietly on a broken pipe, the
    // way every other Unix CLI does.
    #[cfg(unix)]
    // SAFETY: single-threaded process startup, before any threads/runtime exist;
    // installing a default signal disposition here races nothing.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    // Before the first byte reaches a terminal: a dumb terminal renders ANSI
    // literally, so every diagnostic below (credential handoff failures
    // included) has to already know that.
    term_policy::apply_dumb_terminal_policy();

    // Resolve every user-global anchor — home, XDG state home, the user-tier
    // data dir, the filesystem-isolation boundary — ONCE, here, before any
    // loader can reach for one (#1139). Nothing downstream reads `HOME` out of
    // `std::env`; it asks `paths`. Placed above env-file loading on purpose,
    // and safely: every name this resolves is on `env_files::DENIED_EXACT`,
    // so no project `.env` can move it afterwards.
    paths::install(paths::UserPaths::from_environment());

    // Everything user-global lives at ~/.stella; move data from the legacy
    // split layout (platform data dir + ~/.config/stella) before any store,
    // settings, or extension loader resolves a path.
    stella_store::home::migrate_legacy_global_dirs();

    // The process's one license to write the process environment. Three
    // production paths do — dotenv loading, the credential-handoff scrub, and
    // the privileged-value rollback — and all three demand this token, so the
    // window in which they are reachable is bounded by the lifetime of a local
    // in `main` rather than by a comment (#1140). `None` is unreachable here:
    // nothing else in the binary claims it.
    let Some(startup) = startup::StartupPhase::claim() else {
        eprintln!(
            "{} startup phase was already claimed",
            "stella:".red().bold()
        );
        return ExitCode::FAILURE;
    };

    // A trusted benchmark launcher may provide the selected provider key on
    // an inherited anonymous FD. Consume and close it before project env-file
    // loading, clap, a runtime, or any model/repository-controlled process.
    // The raw key is retained only in the credential module's in-memory slot;
    // it is never installed into this process's environment.
    // Same window, same reason: if this is a supervised child, take its
    // session id out of the environment before anything can inherit it. A
    // tool subprocess that still saw it would be one `stella` invocation away
    // from stamping a terminal status on its parent's live record (#1552).
    daemon::consume_supervised_env(&startup);

    if let Err(error) = credential_handoff::consume_at_startup(&startup) {
        eprintln!(
            "{} secure credential handoff failed: {error}",
            "stella:".red().bold()
        );
        return ExitCode::FAILURE;
    }

    // Load project-scoped `.env`/`.env.local`/`.env.<mode>.local` before
    // parsing, so both clap's `env = …` fields and downstream credential
    // resolution see project keys. Runs here at single-threaded startup where
    // mutating the process environment is safe. The live shell always wins;
    // `STELLA_NO_ENV_FILE=1` opts out entirely.
    let managed_snapshot = settings::Settings::load_managed_telemetry_snapshot()
        .ok()
        .flatten();
    let authority_snapshot =
        enterprise_telemetry::StartupAuthoritySnapshot::capture(managed_snapshot.as_ref());
    let mut loaded_env = env_files::maybe_load(&startup);
    // The snapshot rolls back any privileged name a dotenv file did manage to
    // set (the second-loader backstop behind `env_files`' own deny-list). It
    // returns the names it clawed back — fold them into the load record so the
    // rollback is REPORTED like every other refusal rather than swallowed, and
    // so the diagnostics can't go on claiming a variable was loaded when its
    // host value was put straight back (#553).
    let rejected_privileged =
        authority_snapshot.restore_after_project_env(&startup, &loaded_env.names);
    for name in rejected_privileged {
        loaded_env.names.retain(|loaded| loaded != &name);
        loaded_env.name_files.remove(&name);
        if !loaded_env.refused.contains(&name) {
            loaded_env.refused.push(name);
        }
    }

    // Not `Cli::parse()`: the root help is grouped by `cli::help::command()`,
    // and clap renders `--help` from whichever `Command` does the parsing. The
    // derived `Cli` on the other side is byte-for-byte the same value.
    let cli = match Cli::from_arg_matches(&cli::help::command().get_matches()) {
        Ok(cli) => cli,
        Err(err) => err.exit(),
    };

    // A machine-readable run has nobody to answer a masked password prompt,
    // and its caller is blocked reading stdout for an object that would never
    // come. Decided here, once, from the command's own declaration (#1493) —
    // which is also what finally covers `arena`, whose fixed stream-json
    // contract never passed through the old global flag.
    if matches!(
        cli.output_format(),
        OutputFormat::Json | OutputFormat::StreamJson
    ) {
        config::forbid_interactive_credentials();
    }

    // The diagnostic plane, before anything that can fail interestingly. From
    // here on a record explains a decision instead of being discarded, and the
    // panic hook is armed — so a crash after this line leaves an artifact a
    // user can attach (docs/spec/diagnostics.md §7.4). It is installed after
    // clap so `-v`/`--log-level`/`--log-file` are known, and after env-file
    // loading so `STELLA_LOG` from a project `.env` is honoured.
    let diag_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let dx = diag_boot::install(&cli.globals, &diag_root);

    // The startup window closes HERE, and `close` consumes the token, so this
    // is the only place it can: the next line detaches a delivery thread, and
    // everything past it (the Tokio runtimes in `run`, the tool and hook
    // subprocesses) assumes a multi-threaded process. Below this line the three
    // environment writers are unreachable — no token — and a caller that got
    // one some other way trips the assertion each of them carries (#1140).
    //
    // Note this is the first *thread*, not the first runtime. That is the
    // boundary that matters: `setenv` races a `getenv` on any thread, Tokio's
    // or not.
    startup.close();
    enterprise_telemetry::start_best_effort_flush();
    loaded_env
        .names
        .retain(|name| !stella_tools::exec::is_sensitive_env_name(name));

    // A supervised child's console becomes bounded and indexed from here on
    // (#1588): everything this process prints below flows through the pump
    // threads, which enforce the byte budget and write the ordering index.
    // Installed after `startup.close()` — the pumps are threads, and threads
    // before that boundary would race the env mutations it fences — and
    // drained as this function's last act so the final lines land.
    //
    // A panic would unwind (or, under the release profile's
    // `panic = "abort"`, not unwind at all) past that last act and strand up
    // to a pipe buffer — usually including the panic message itself — so
    // `arm_panic_drain` chains a hook that performs the same idempotent
    // drain (#1616). Whichever of the two arrives first does the one real
    // drain; the other finds the streams already taken.
    let console = daemon::supervised_id().and_then(|id| {
        daemon::console::install_bounded(
            &stella_store::SessionRegistry::open_default().sidecar_dir(&id),
        )
    });
    if let Some(guard) = &console {
        daemon::console::arm_panic_drain(guard);
    }

    // Value-free confirmation (names only), gated on STELLA_ENV_DEBUG + a TTY +
    // a human output format so it never pollutes json/stream-json.
    env_files::announce(&loaded_env, cli.output_format());

    // Captured before `cli` moves into `run`: the catch-all below needs the
    // requested format to honour the machine-readable error contract.
    let output_format = cli.output_format();

    let code = match run(cli, &loaded_env) {
        Ok(()) => {
            daemon::record_outcome_if_supervised(Ok(()));
            // A supervisor's own exit code says only whether it managed to
            // stream a log. What a script wrapping `stella run` is asking
            // about is the run, so the child's code is forwarded verbatim.
            match daemon::forwarded_exit_code() {
                Some(code) => ExitCode::from(code),
                None => ExitCode::SUCCESS,
            }
        }
        Err(e) => {
            // The failure itself, not a bool: a deliberate stop must reach
            // the registry as a stop, not age into it as a crash (#1653).
            daemon::record_outcome_if_supervised(Err(&e));
            eprintln!("{} {}", "stella:".red().bold(), e);
            emit_error_summary(output_format, e.message());
            // §7.4's second trigger, and the one that fires more often: most
            // failures are a returned `Err`, not a panic, and those are exactly
            // the runs a user is about to open an issue about. Naming the file
            // is what makes "attach the log" a sentence stella can say.
            let interrupted = signals::interrupted_exit_code().is_some();
            if let Some(path) = diag_boot::dump_on_failure(&dx, &diag_root, interrupted) {
                eprintln!(
                    "{} diagnostics: {} ({})",
                    "stella:".dimmed(),
                    path.display(),
                    "safe to attach — no prompts, paths, or model output".dimmed()
                );
            }
            // A turn cut short by SIGINT/SIGTERM exits 128 + the signal
            // number, the shell convention, so a script wrapping `stella
            // run` can tell "the user stopped this" from "this failed".
            // Otherwise the failure itself decides: a deliberate stop exits
            // distinctly from a crash (`failure::DELIBERATE_STOP_EXIT_CODE`).
            match signals::interrupted_exit_code() {
                Some(code) => ExitCode::from(code),
                None => e.exit_code(),
            }
        }
    };
    // After the last print of every path above: restore the raw fds and join
    // the pumps, so the console files carry this process's final lines. A
    // no-op if the panic hook installed above already won that race.
    if let Some(guard) = console {
        guard.drain();
    }
    code
}

/// The deck's presentation, resolved from the flags and their env synonyms.
///
/// One place, so `chat` and `resume` cannot drift: a user who set
/// `STELLA_ACCESSIBLE` in their profile must get the accessible deck from
/// both, and finding that out by discovering `resume` ignores it is exactly
/// the failure this centralization prevents.
fn deck_presentation(globals: &cli::GlobalArgs) -> term_policy::DeckPresentation {
    term_policy::DeckPresentation {
        no_anim: term_policy::animation_disabled(globals.no_anim),
        accessible: term_policy::accessible_mode(globals.accessible),
    }
}

fn run(cli: Cli, loaded_env: &env_files::Loaded) -> Result<(), failure::CliFailure> {
    // A supervised child holds its liveness lock from `pre_exec` onwards; this
    // is the backstop for a child started some other way (by hand with
    // STELLA_SUPERVISED set, or by a future launchd/systemd unit). Without it
    // such a run reads as already-finished to every other process from the
    // moment it starts.
    if let Some(id) = daemon::supervised_id() {
        daemon::hold_liveness_lock(&stella_store::SessionRegistry::open_default(), &id);
    }

    // Above the keyless dispatch because `daemon resume`'s parent half needs
    // a runtime (it streams the child it spawns) while staying keyless.
    let rt = || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("failed to start runtime: {e}"))
    };

    // Models and Version don't need a configured provider/key.
    match &cli.command {
        Some(Command::Models { cmd }) => {
            return match cmd {
                None => {
                    config::Config::print_available_models(Some(loaded_env));
                    model_catalog::print_catalog_status();
                    Ok(())
                }
                // Needs a runtime for the HTTP fetch — built here because
                // this arm runs before the shared `rt` closure exists.
                Some(ModelsCmd::Refresh { force }) => tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| format!("failed to start runtime: {e}"))?
                    .block_on(model_catalog::run_refresh(*force)),
                Some(ModelsCmd::List { provider, all }) => {
                    model_catalog::run_list(provider.as_deref(), *all)
                }
            }
            .map_err(failure::CliFailure::from);
        }
        Some(Command::Tools {
            validate,
            author,
            adopt,
            enable,
            disable,
            foundry,
        }) => {
            // The tool-foundry protocol's three decisions are three flags, in
            // the order a tool travels through them: author -> adopt (prove)
            // -> enable (approve). `clap` makes them mutually exclusive, so
            // this is a first-match chain rather than a state machine.
            return match (validate, author) {
                _ if *foundry => tool_foundry::adopt::run_tools_foundry_report(),
                _ if adopt.is_some() => {
                    tool_foundry::adopt::run_tools_adopt(adopt.as_deref().unwrap_or_default())
                }
                _ if enable.is_some() => tool_foundry::adopt::run_tools_enable(
                    enable.as_deref().unwrap_or_default(),
                    true,
                ),
                _ if disable.is_some() => tool_foundry::adopt::run_tools_enable(
                    disable.as_deref().unwrap_or_default(),
                    false,
                ),
                // `--author` (name optional) stages a tool-foundry proposal
                // as a reviewable manifest+script pair — or lists proposals.
                (_, Some(name)) => tool_foundry::run_tools_author(name.as_deref()),
                // `--validate` (dir optional) is the strict pre-flight path;
                // a plain `stella tools` stays the lenient listing.
                (Some(dir), None) => agent::run_tools_validation(dir.as_deref()),
                (None, None) => agent::run_tools_listing(),
            }
            .map_err(failure::CliFailure::from);
        }
        Some(Command::Graph { op, target }) => {
            // Reads the local index only — works with zero API keys.
            return contextgraph::run_graph(*op, target).map_err(failure::CliFailure::from);
        }
        Some(Command::Scripts { cmd }) => {
            // Static manifest parsing plus a local subprocess — works with
            // zero API keys.
            return scripts_cmd::run_scripts(cmd).map_err(failure::CliFailure::from);
        }
        Some(Command::Storage { cmd }) => {
            // Reads the local index + manifest only — zero API keys.
            return storage_cmd::run_storage(cmd).map_err(failure::CliFailure::from);
        }
        Some(Command::Commands { cmd }) => {
            // Reads (and, for convert, writes) definition files only.
            return commands_cmd::run_commands(cmd).map_err(failure::CliFailure::from);
        }
        // Reads context-record TOML and the tree, and appends to the local
        // lifecycle ledger on the review actions (Phase 3, #714). `propose
        // --commit` writes a local branch and commit. No store, model, or
        // API key on any path.
        Some(Command::Context { cmd }) => {
            return context_cmd::run_context(cmd).map_err(failure::CliFailure::from);
        }
        Some(Command::Proposals { cmd }) => {
            return proposals_cmd::run_proposals(cmd).map_err(failure::CliFailure::from);
        }
        // #831 first slice. Reads loop-bench result files + the local ledger;
        // writes settings only on `--promote`. No provider, no API key.
        Some(Command::Tune { cmd }) => {
            return tune_cmd::run_tune(cmd).map_err(failure::CliFailure::from);
        }
        // #872. Folds .stella/private/store.db into a redacted trajectory
        // dataset and writes it owner-only. No provider, no API key.
        Some(Command::Dataset { cmd }) => {
            return dataset_cmd::run_dataset(cmd).map_err(failure::CliFailure::from);
        }
        Some(Command::Calibration { format }) => {
            // Reads the local event journal only — no provider, no API key.
            return inspect::run_calibration(*format).map_err(failure::CliFailure::from);
        }
        Some(Command::Inspect {
            execution_id,
            turn,
            step,
            call_seq,
            format,
            full,
            diff,
            context,
            only,
        }) => {
            // Reads the local receipt tables only — no provider, no API key.
            return inspect::run_inspect(&inspect::InspectArgs {
                execution_id: *execution_id,
                turn: *turn,
                step: *step,
                call_seq: *call_seq,
                format: *format,
                full: *full,
                diff: *diff,
                context: *context,
                only: *only,
            })
            .map_err(failure::CliFailure::from);
        }
        Some(Command::Stats {
            format,
            provider,
            cmd,
        }) => {
            // Reads local telemetry only — works with zero API keys.
            // `*format`: this match borrows `&cli.command` (the Tools arm
            // needs `validate` by ref), so `format` binds as `&StatsFormat`;
            // it is `Copy`, so deref rather than move.
            return match cmd {
                None => stats::run_stats(*format, provider.as_deref()),
                Some(stats::StatsCmd::Prune(args)) => stats::run_stats_prune(args),
                Some(stats::StatsCmd::Graph(args)) => stats_graph::run_stats_graph(args),
            }
            .map_err(failure::CliFailure::from);
        }
        Some(Command::Usage { cmd }) => {
            // Hub-only reads/writes — no provider, no API keys.
            return usage_cmd::run_usage(cmd.clone()).map_err(failure::CliFailure::from);
        }
        Some(Command::Cloud { cmd }) => {
            return usage_cmd::run_cloud(cmd.clone()).map_err(failure::CliFailure::from);
        }
        Some(Command::Telemetry { cmd }) => {
            // Managed operational export is independent of model/provider
            // configuration. Community/default status constructs no client.
            return enterprise_telemetry::run_command(*cmd).map_err(failure::CliFailure::from);
        }
        Some(Command::Ingest(args)) => {
            // Scanning (no paths) reads local markdown only; extracting from
            // named files resolves a provider and makes a model call, so the
            // global model / key / base-url overrides are threaded through.
            return ingest_cmd::run(
                args,
                cli.globals.model.as_deref(),
                cli.globals.api_key.as_deref(),
                cli.globals.base_url.as_deref(),
            )
            .map_err(failure::CliFailure::from);
        }
        Some(Command::Scoreboard) => {
            // Reads .stella/private/store.db only.
            return scoreboard_cmd::run().map_err(failure::CliFailure::from);
        }
        Some(Command::SelfDriving { cmd }) => {
            // Reads and writes ~/.stella/self-driving/<slug>/ (plus `gh` reads
            // of the defect queue) — works with zero API keys.
            return self_driving_cmd::run(cmd).map_err(failure::CliFailure::from);
        }
        Some(Command::Memory { cmd }) => {
            // Reads local stores only (list) / writes one rule file
            // (promote) — works with zero API keys.
            return match cmd {
                memory_cmd::MemoryCmd::List { format } => memory_cmd::run_memory_list(*format),
                memory_cmd::MemoryCmd::Promote { id } => memory_cmd::run_memory_promote(id),
                memory_cmd::MemoryCmd::Validate { end_stale } => {
                    memory_cmd::run_memory_validate(*end_stale)
                }
                memory_cmd::MemoryCmd::Forget { id, reason } => {
                    memory_cmd::run_memory_forget(id, reason)
                }
                memory_cmd::MemoryCmd::Edit { id, text } => memory_cmd::run_memory_edit(id, text),
                memory_cmd::MemoryCmd::Restore { id } => memory_cmd::run_memory_restore(id),
                memory_cmd::MemoryCmd::Forgotten => memory_cmd::run_memory_forgotten(),
                memory_cmd::MemoryCmd::Retired => memory_retire_cmd::run_memory_retired(),
                memory_cmd::MemoryCmd::Retire { id, reason } => {
                    memory_retire_cmd::run_memory_retire(id, reason)
                }
                memory_cmd::MemoryCmd::Reaffirm { id, reason } => {
                    memory_retire_cmd::run_memory_reaffirm(id, reason)
                }
                memory_cmd::MemoryCmd::Compact(args) => memory_compact::run_memory_compact(args),
                memory_cmd::MemoryCmd::Index(args) => memory_index::run_memory_index(args),
            }
            .map_err(failure::CliFailure::from);
        }
        Some(Command::Mcp { cmd }) => {
            // MCP management reads/writes local config + the registry over
            // HTTP — no provider or API key required.
            return mcp_cmd::run(cmd).map_err(failure::CliFailure::from);
        }
        Some(Command::Connect { cmd }) => {
            // Tracker OAuth talks only to the tracker the user is connecting
            // — no provider or API key required. A `--api-key` here is
            // almost always muscle memory from when `connect linear` had a
            // flag by that name (now `--paste-key`): say so instead of
            // silently running the OAuth path.
            if cli.globals.api_key.is_some() {
                eprintln!(
                    "⚠ --api-key is the model-provider credential and is unused by \
                     `stella connect`; to paste a Linear personal API key, use \
                     `stella connect linear --paste-key`"
                );
            }
            return connect_cmd::run(cmd).map_err(failure::CliFailure::from);
        }
        Some(Command::Auth { cmd }) => {
            // Reads/writes ~/.stella/credentials.toml directly — no
            // provider needs to already resolve (this is often how the
            // FIRST key gets configured), so this short-circuits before
            // `Config::load` like `Connect`/`Mcp` do.
            return auth_cmd::run(cmd).map_err(failure::CliFailure::from);
        }
        Some(Command::Observe { port, open }) => {
            // Loopback-only dashboard over local telemetry — no provider or
            // API key required; the stores are opened strictly read-only.
            return storage_cmd::run_observe(*port, *open).map_err(failure::CliFailure::from);
        }
        Some(Command::Migrate { cmd }) => {
            // Deliberately BEFORE `Config::load`, for the same reason as
            // `doctor`: the config being migrated may be the reason stella
            // cannot start. Requiring a resolvable provider here would make the
            // fix-it command unreachable exactly when it is needed — a
            // settings.json naming a model whose provider no longer exists is
            // one of the likeliest things a user runs this to escape.
            let MigrateCmd::Config { dry_run } = cmd;
            let root = std::env::current_dir()
                .map_err(|e| format!("cannot determine workspace root: {e}"))?;
            println!("Migrating settings.json -> stella.toml\n");
            return settings::migrate::run(&root, *dry_run).map_err(failure::CliFailure::from);
        }
        Some(Command::Doctor {
            repair,
            last_failure,
        }) => {
            // Reads local state only — and with --repair renames files inside
            // .stella/private/. No provider, no API key, and deliberately
            // before `Config::load`: a workspace whose store is corrupt must be
            // diagnosable without a working model configuration. The session
            // flags are threaded in so the `model config` check diagnoses the
            // model the next run would actually send (#895).
            return doctor::run_doctor(
                *repair,
                *last_failure,
                cli.globals.model.as_deref(),
                cli.globals.base_url.as_deref(),
            )
            .map_err(failure::CliFailure::from);
        }
        Some(Command::Completions { shell }) => {
            // Generated from the live `Command` tree, so a new subcommand or
            // flag is completable the day it lands — a hand-written script
            // would be stale by the next release.
            let mut command = cli::help::command();
            clap_complete::generate(*shell, &mut command, "stella", &mut std::io::stdout());
            return Ok(());
        }
        Some(Command::Version) => {
            println!("stella v{}", build_info::version_string());
            return Ok(());
        }
        Some(Command::Resume { list: true, .. }) => {
            // Listing reads the local registry only — no provider required.
            return run_resume_list().map_err(failure::CliFailure::from);
        }
        Some(Command::Daemon { .. }) => {
            // Finding, watching and stopping supervised runs is the local
            // registry, two console files and a signal. Deliberately before
            // provider resolution: a run whose model config has since been
            // broken (a rotated key, a removed provider) is exactly the one an
            // operator needs to be able to find and stop.
            let Some(Command::Daemon { cmd }) = &cli.command else {
                unreachable!("matched Command::Daemon")
            };
            match cmd {
                // `daemon resume` splits (#1586): the parent half below only
                // spawns and streams, so it too works keyless from any
                // directory — while the `--foreground` child half does the
                // interrupted turn's actual work and falls through to
                // provider resolution, its cwd already pinned to the
                // record's workspace by the parent's launch.
                DaemonCmd::Resume { id } if !cli.globals.foreground => {
                    return daemon::resume_supervised(rt()?, id.as_deref(), None)
                        .map(|_| ())
                        .map_err(failure::CliFailure::from);
                }
                DaemonCmd::Resume { .. } => {}
                // The sweep is the parent half N times over — it resolves,
                // spawns and streams each `daemon resume <id> --foreground`
                // child — so it stays keyless here for the same reason.
                DaemonCmd::ResumeAll { dry_run, ceiling } => {
                    return daemon::resume_all(
                        *dry_run,
                        std::time::Duration::from_secs(ceiling.saturating_mul(60)),
                        rt,
                    )
                    .map_err(failure::CliFailure::from);
                }
                _ => return daemon::run(cmd).map_err(failure::CliFailure::from),
            }
        }
        _ => {}
    }

    // Everything past here resolves a provider (and may build one), so the
    // model catalog must be live first: open catalog.db, auto-sync each
    // configured provider's own live model listing (BYOK-clean), re-fetch a
    // stale models.dev master list only after the user's explicit first
    // `stella models refresh` (the no-phone-home rule), and install the
    // runtime catalog that slug validation and pricing resolve against.
    model_catalog::bootstrap();

    // `init` works offline (heuristic fallback), so config resolution
    // failure downgrades rather than aborting.
    if let Some(Command::Init) = cli.command {
        return signals::block_on_interruptible(
            rt()?,
            agent::run_init(
                cli.globals.model.as_deref(),
                cli.globals.api_key.as_deref(),
                cli.globals.base_url.as_deref(),
                term_policy::animation_disabled(cli.globals.no_anim),
            ),
        )
        .map_err(failure::CliFailure::from);
    }

    // Run/Chat/Config need a resolved config (which requires an API key).
    let mut cfg = config::Config::load(
        cli.globals.model.as_deref(),
        cli.globals.api_key.as_deref(),
        cli.globals.base_url.as_deref(),
    )?;
    // Stamped here for the same reason `model_pinned_by_flag` is: `Config::load`
    // resolves provider and credentials and has no view of the parsed CLI, and
    // giving it one for a value it never consults would widen its signature to
    // carry something straight through. `main` is where the flag and the config
    // are both in hand.
    cfg.turn_budget = cli.globals.turn_budget;
    cfg.max_output_tokens = cli.globals.max_output_tokens;
    cfg.plan_mode = cli.globals.plan_mode;
    // `--tools` is the lowest-authority scope (#1263): folded in AFTER
    // settings so it can only narrow what they already allowed. `narrow_with`
    // is the intersection, not a key-level merge, which is what lets the
    // read-only idiom `*:off,read_file:on` mean what it says while still
    // being unable to re-enable anything an org policy denied.
    if let Some(spec) = cli.globals.tools.as_deref() {
        let scope = stella_tools::policy::ToolPolicy::parse_spec(spec)
            .map_err(|e| format!("--tools: {e}"))?;
        cfg.tool_policy.narrow_with(&scope);
    }

    // Correctness pass over the resolved settings — model-slug problems (an
    // unknown provider, a typo, an over-qualified slug that would 400 on the
    // first call) and a `--base-url` pointed at a different provider's host
    // surface here, before the TUI's alternate screen hides stderr, as
    // advisory warnings that never block the run. The printing lives in
    // `settings_check` so `stella ingest`, which resolves its own config,
    // says exactly the same thing (#895).
    settings_check::report_at_launch(&cfg);

    // Same posture, one file over: `~/.stella/credentials.toml` is read even
    // when its mode lets others at it (refusing would lock a user out of their
    // own keys) and is never silently `chmod`ed (its mode is not ours to
    // change) — so the only honest response left is to say so, out loud, once.
    // A check whose finding nothing prints would be worse than no check.
    for advisory in &cfg.credential_advisories {
        eprintln!("⚠ credentials: {}", advisory.line());
    }

    match cli.command.unwrap_or(Command::Chat) {
        Command::Run {
            prompt,
            no_pipeline,
            test_command,
            keep_witness,
            output_format,
        } => {
            let prompt = prompt_source::resolve(
                prompt,
                std::io::stdin().is_terminal(),
                prompt_source::read_stdin_to_string,
            )?;
            // Resolved first on purpose: the prompt may have come from this
            // process's stdin, and a detached child has none to read.
            let posture = supervision(&cli.globals);
            if posture.supervises() {
                return daemon::supervise_this_invocation(
                    rt()?,
                    &cfg.workspace_root,
                    &supervised_title(&cfg, &prompt),
                    prompt.as_bytes(),
                    posture,
                    output_format,
                ).map_err(failure::CliFailure::from);
            }
            signals::block_on_interruptible(
                rt()?,
                agent::run_one_shot(
                    &cfg,
                    &prompt,
                    cli.globals.budget,
                    output_format,
                    !no_pipeline,
                    test_command.as_deref(),
                    keep_witness,
                ),
            )?;
        }
        Command::Arena {
            task_dir,
            journal,
            state_dir,
            resume,
            no_pipeline,
            test_command,
        } => {
            signals::block_on_interruptible(
                rt()?,
                arena::run_arena(
                    cfg,
                    arena::ArenaArgs {
                        task_dir,
                        journal,
                        state_dir,
                        resume,
                        no_pipeline,
                        test_command,
                    },
                ),
            )?;
        }
        Command::Goal { goal, no_pipeline } => {
            let goal = prompt_source::resolve(
                goal,
                std::io::stdin().is_terminal(),
                prompt_source::read_stdin_to_string,
            )?;
            let posture = supervision(&cli.globals);
            if posture.supervises() {
                return daemon::supervise_this_invocation(
                    rt()?,
                    &cfg.workspace_root,
                    &supervised_title(&cfg, &goal),
                    goal.as_bytes(),
                    posture,
                    // `goal` declares no `--output-format`, so there is no
                    // machine-readable stream to name the session on.
                    OutputFormat::Text,
                ).map_err(failure::CliFailure::from);
            }
            signals::block_on_interruptible(
                rt()?,
                agent::run_goal_cmd(&cfg, &goal, cli.globals.budget, !no_pipeline),
            )?;
        }
        Command::Fleet {
            cmd: Some(sub),
            ..
        } => {
            // Maintenance verbs never fan out, never supervise, and never
            // touch a provider — dispatch before any of the run machinery.
            signals::block_on_interruptible(rt()?, fleet_verbs::run(&cfg, &sub))?;
        }
        Command::Fleet {
            cmd: None,
            tasks,
            plan,
            max_concurrency,
            base_ref,
            watch,
            no_pipeline,
            task_timeout,
            output_format,
        } => {
            let posture = supervision(&cli.globals);
            if posture.supervises() {
                return daemon::supervise_this_invocation(
                    rt()?,
                    &cfg.workspace_root,
                    &supervised_title(
                        &cfg,
                        &match plan.as_deref() {
                            Some(file) => format!("fleet {}", file.display()),
                            None => format!("fleet ({} tasks)", tasks.len()),
                        },
                    ),
                    &[],
                    posture,
                    output_format,
                ).map_err(failure::CliFailure::from);
            }
            signals::block_on_interruptible(
                rt()?,
                fleet_cmd::run_fleet(
                    &cfg,
                    &tasks,
                    plan.as_deref(),
                    base_ref.as_deref(),
                    max_concurrency,
                    cli.globals.budget,
                    watch,
                    !no_pipeline,
                    task_timeout.map(std::time::Duration::from_secs),
                    output_format,
                ),
            )?;
        }
        Command::Monitor { target } => {
            let target = target.unwrap_or_else(|| "main".to_string());
            let posture = supervision(&cli.globals);
            if posture.supervises() {
                return daemon::supervise_this_invocation(
                    rt()?,
                    &cfg.workspace_root,
                    &supervised_title(&cfg, &format!("monitor {target}")),
                    &[],
                    posture,
                    // `monitor` declares no `--output-format` either.
                    OutputFormat::Text,
                ).map_err(failure::CliFailure::from);
            }
            // Monitoring IS a goal: the verifier (who can call ci_status
            // itself) ends the loop only on a fully green latest run.
            let goal = format!(
                "Drive CI for `{target}` to fully green. Use ci_status (wait: true) to watch \
                 the latest runs, read the failure logs it returns, fix each root cause in the \
                 code, commit and push the fix, then re-check. The goal is met only when the \
                 latest CI run for `{target}` has completed with every check successful."
            );
            signals::block_on_interruptible(
                rt()?,
                agent::run_goal_cmd(&cfg, &goal, cli.globals.budget, true),
            )?;
        }
        Command::Chat => {
            // The Command Deck (tabbed TUI) is the default chat surface on a
            // real terminal; `--plain` / STELLA_PLAIN=1 / a non-TTY stream
            // falls back to the line-based REPL.
            match term_policy::plain_fallback(cli.globals.plain) {
                None => {
                    signals::block_on_interruptible(
                        rt()?,
                        command_deck::run_deck_session(
                            &cfg,
                            cli.globals.budget,
                            deck_presentation(&cli.globals),
                            None,
                        ),
                    )?;
                }
                Some(reason) => {
                    // Say which surface this is and why, BEFORE the REPL's
                    // banner — which otherwise looks enough like a normal
                    // start that the missing features read as breakage. The
                    // REPL has no prompt queue and no mid-turn steering, and
                    // it exits at stdin EOF, so a silent downgrade presents
                    // as "stella dies when a turn completes" with nothing to
                    // point at. To stderr: stdout may be the pipe that caused
                    // the fallback in the first place.
                    eprintln!(
                        "▸ plain REPL ({}) — no prompt queue, no mid-turn steering; \
                         exits at end of input.",
                        reason.explain()
                    );
                    eprintln!("  The Command Deck needs both stdin and stdout on a terminal.");
                    signals::block_on_interruptible(
                        rt()?,
                        agent::run_interactive(&cfg, cli.globals.budget),
                    )?;
                }
            }
        }
        Command::Resume { id, list } => {
            // `--list` returned before provider resolution; this arm is the
            // actual reopen, which is a full deck session (durable state is
            // a deck feature — the plain REPL has no session to restore).
            debug_assert!(!list, "handled before provider resolution");
            if !term_policy::use_deck(cli.globals.plain) {
                return Err(failure::CliFailure::error(
                    "`stella resume` reopens a Command Deck session and needs a real \
                     terminal (it cannot combine with --plain / STELLA_PLAIN / a piped \
                     stream). `stella resume --list` works anywhere.",
                ));
            }
            let request = match id {
                Some(id) => session_persist::ResumeRequest::Id(id),
                None => session_persist::ResumeRequest::Latest,
            };
            signals::block_on_interruptible(
                rt()?,
                command_deck::run_deck_session(
                    &cfg,
                    cli.globals.budget,
                    deck_presentation(&cli.globals),
                    Some(request),
                ),
            )?;
        }
        // The `--foreground` child half of `daemon resume` (#1586): cfg has
        // resolved from the record's workspace (the parent pinned the cwd),
        // and the interrupted turn continues from its checkpoint. Every other
        // daemon verb returned from the keyless dispatch above.
        Command::Daemon {
            cmd: DaemonCmd::Resume { id },
        } => {
            signals::block_on_interruptible(
                rt()?,
                agent::resume::run_resume(&cfg, id.as_deref()),
            )?;
        }
        // Models/Version (and Tools) short-circuit in the first match at the
        // top of `run` before a provider is resolved; Init is handled by the
        // caller. Reaching any of them here is impossible.
        Command::Init
        | Command::Daemon { .. }
        | Command::Tools { .. }
        | Command::Graph { .. }
        | Command::Scripts { .. }
        | Command::Storage { .. }
        | Command::Commands { .. }
        | Command::Inspect { .. }
        | Command::Calibration { .. }
        // Phase 3 (#714)
        | Command::Proposals { .. }
        // Epic #897
        | Command::Context { .. }
        // #831 first slice
        | Command::Tune { .. }
        // #872
        | Command::Dataset { .. }
        | Command::Stats { .. }
        | Command::Usage { .. }
        | Command::Cloud { .. }
        | Command::Telemetry { .. }
        | Command::Memory { .. }
        | Command::SelfDriving { .. }
        | Command::Scoreboard
        | Command::Ingest(_)
        | Command::Mcp { .. }
        | Command::Connect { .. }
        | Command::Auth { .. }
        | Command::Observe { .. }
        | Command::Models { .. }
        | Command::Doctor { .. }
        | Command::Migrate { .. }
        | Command::Completions { .. }
        | Command::Version => {
            unreachable!("handled before provider resolution")
        }
        Command::Config => {
            cfg.print_config(Some(loaded_env));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
