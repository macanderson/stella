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
mod deck_mcp;
mod discovery;
mod doctor;
mod domains;
mod engine_config;
mod enterprise_telemetry;
#[cfg(test)]
mod enterprise_telemetry_tests;
mod env_files;
mod export;
mod extensions;
mod fleet_cmd;
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
mod scoreboard_cmd;
// The `/profile` posture planner (fast · balanced · pro · ultra).
mod profile;
// Phase 3 (#714): the adaptive-context proposal review surface.
mod proposals_cmd;
mod rules;
mod runtime;
mod scripts_cmd;
mod session_persist;
mod settings;
mod settings_check;
mod signals;
mod skill_manager;
mod stats;
mod storage_cmd;
mod subagent;
mod subsession;
mod term_policy;
mod tool_foundry;
mod tool_policy;
mod tool_switches;
mod trace;
mod tui;
mod tune_cmd;
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
/// step-loop summary, and the pre-flight error envelope — so a consumer can
/// branch on the shape instead of sniffing for keys.
///
/// All three envelopes are structs with the version declared first, so a derived
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
    AuthCmd, Cli, Command, ConnectCmd, McpCmd, MigrateCmd, ModelsCmd, TelemetryCmd,
};

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

    // Everything user-global lives at ~/.stella; move data from the legacy
    // split layout (platform data dir + ~/.config/stella) before any store,
    // settings, or extension loader resolves a path.
    stella_store::home::migrate_legacy_global_dirs();

    // A trusted benchmark launcher may provide the selected provider key on
    // an inherited anonymous FD. Consume and close it before project env-file
    // loading, clap, a runtime, or any model/repository-controlled process.
    // The raw key is retained only in the credential module's in-memory slot;
    // it is never installed into this process's environment.
    if let Err(error) = credential_handoff::consume_at_startup() {
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
    let mut loaded_env = env_files::maybe_load();
    // The snapshot rolls back any privileged name a dotenv file did manage to
    // set (the second-loader backstop behind `env_files`' own deny-list). It
    // returns the names it clawed back — fold them into the load record so the
    // rollback is REPORTED like every other refusal rather than swallowed, and
    // so the diagnostics can't go on claiming a variable was loaded when its
    // host value was put straight back (#553).
    let rejected_privileged = authority_snapshot.restore_after_project_env(&loaded_env.names);
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
    // come. Decided here, once, while the requested format is in hand.
    if matches!(
        cli.globals.output_format,
        OutputFormat::Json | OutputFormat::StreamJson
    ) {
        config::forbid_interactive_credentials();
    }

    enterprise_telemetry::start_best_effort_flush();
    loaded_env
        .names
        .retain(|name| !stella_tools::exec::is_sensitive_env_name(name));

    // Value-free confirmation (names only), gated on STELLA_ENV_DEBUG + a TTY +
    // a human output format so it never pollutes json/stream-json.
    env_files::announce(&loaded_env, cli.globals.output_format);

    // Captured before `cli` moves into `run`: the catch-all below needs the
    // requested format to honour the machine-readable error contract.
    let output_format = cli.globals.output_format;

    match run(cli, &loaded_env) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{} {}", "stella:".red().bold(), e);
            emit_error_summary(output_format, &e);
            // A turn cut short by SIGINT/SIGTERM exits 128 + the signal
            // number, the shell convention, so a script wrapping `stella
            // run` can tell "the user stopped this" from "this failed".
            match signals::interrupted_exit_code() {
                Some(code) => ExitCode::from(code),
                None => ExitCode::FAILURE,
            }
        }
    }
}

fn run(cli: Cli, loaded_env: &env_files::Loaded) -> Result<(), String> {
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
            };
        }
        Some(Command::Tools { validate, author }) => {
            return match (validate, author) {
                // `--author` (name optional) stages a tool-foundry proposal
                // as a reviewable manifest+script pair — or lists proposals.
                (_, Some(name)) => tool_foundry::run_tools_author(name.as_deref()),
                // `--validate` (dir optional) is the strict pre-flight path;
                // a plain `stella tools` stays the lenient listing.
                (Some(dir), None) => agent::run_tools_validation(dir.as_deref()),
                (None, None) => agent::run_tools_listing(),
            };
        }
        Some(Command::Graph { op, target }) => {
            // Reads the local index only — works with zero API keys.
            return contextgraph::run_graph(*op, target);
        }
        Some(Command::Scripts { cmd }) => {
            // Static manifest parsing plus a local subprocess — works with
            // zero API keys.
            return scripts_cmd::run_scripts(cmd);
        }
        Some(Command::Storage { cmd }) => {
            // Reads the local index + manifest only — zero API keys.
            return storage_cmd::run_storage(cmd);
        }
        Some(Command::Commands { cmd }) => {
            // Reads (and, for convert, writes) definition files only.
            return commands_cmd::run_commands(cmd);
        }
        // Reads context-record TOML and the tree, and appends to the local
        // lifecycle ledger on the review actions (Phase 3, #714). `propose
        // --commit` writes a local branch and commit. No store, model, or
        // API key on any path.
        Some(Command::Context { cmd }) => {
            return context_cmd::run_context(cmd);
        }
        Some(Command::Proposals { cmd }) => {
            return proposals_cmd::run_proposals(cmd);
        }
        // #831 first slice. Reads loop-bench result files + the local ledger;
        // writes settings only on `--promote`. No provider, no API key.
        Some(Command::Tune { cmd }) => {
            return tune_cmd::run_tune(cmd);
        }
        Some(Command::Calibration { format }) => {
            // Reads the local event journal only — no provider, no API key.
            return inspect::run_calibration(*format);
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
            });
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
            };
        }
        Some(Command::Usage { cmd }) => {
            // Hub-only reads/writes — no provider, no API keys.
            return usage_cmd::run_usage(cmd.clone());
        }
        Some(Command::Cloud { cmd }) => {
            return usage_cmd::run_cloud(cmd.clone());
        }
        Some(Command::Telemetry { cmd }) => {
            // Managed operational export is independent of model/provider
            // configuration. Community/default status constructs no client.
            return enterprise_telemetry::run_command(*cmd);
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
            );
        }
        Some(Command::Scoreboard) => {
            // Reads .stella/private/store.db only.
            return scoreboard_cmd::run();
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
            };
        }
        Some(Command::Mcp { cmd }) => {
            // MCP management reads/writes local config + the registry over
            // HTTP — no provider or API key required.
            return mcp_cmd::run(cmd);
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
            return connect_cmd::run(cmd);
        }
        Some(Command::Auth { cmd }) => {
            // Reads/writes ~/.stella/credentials.toml directly — no
            // provider needs to already resolve (this is often how the
            // FIRST key gets configured), so this short-circuits before
            // `Config::load` like `Connect`/`Mcp` do.
            return auth_cmd::run(cmd);
        }
        Some(Command::Observe { port, open }) => {
            // Loopback-only dashboard over local telemetry — no provider or
            // API key required; the stores are opened strictly read-only.
            return storage_cmd::run_observe(*port, *open);
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
            return settings::migrate::run(&root, *dry_run);
        }
        Some(Command::Doctor { repair }) => {
            // Reads local state only — and with --repair renames files inside
            // .stella/private/. No provider, no API key, and deliberately
            // before `Config::load`: a workspace whose store is corrupt must be
            // diagnosable without a working model configuration.
            return doctor::run_doctor(*repair);
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
            return run_resume_list();
        }
        _ => {}
    }

    let rt = || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("failed to start runtime: {e}"))
    };

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
        );
    }

    // Run/Chat/Config need a resolved config (which requires an API key).
    let cfg = config::Config::load(
        cli.globals.model.as_deref(),
        cli.globals.api_key.as_deref(),
        cli.globals.base_url.as_deref(),
    )?;

    // Correctness pass over the resolved settings — model-slug problems (an
    // unknown provider, a typo, an over-qualified slug that would 400 on the
    // first call) surface here, before the TUI's alternate screen hides
    // stderr, as advisory warnings that never block the run.
    for issue in settings_check::validate_at_launch(&cfg) {
        eprintln!("⚠ settings: {}", issue.line());
    }

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
        } => {
            signals::block_on_interruptible(
                rt()?,
                agent::run_one_shot(
                    &cfg,
                    &prompt,
                    cli.globals.budget,
                    cli.globals.output_format,
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
            signals::block_on_interruptible(
                rt()?,
                agent::run_goal_cmd(&cfg, &goal, cli.globals.budget, !no_pipeline),
            )?;
        }
        Command::Fleet {
            tasks,
            plan,
            max_concurrency,
            base_ref,
            watch,
            no_pipeline,
        } => {
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
                    cli.globals.output_format,
                ),
            )?;
        }
        Command::Monitor { target } => {
            let target = target.unwrap_or_else(|| "main".to_string());
            // Monitoring IS a goal: the judge (who can call ci_status
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
            if term_policy::use_deck(cli.globals.plain) {
                signals::block_on_interruptible(
                    rt()?,
                    command_deck::run_deck_session(
                        &cfg,
                        cli.globals.budget,
                        term_policy::animation_disabled(cli.globals.no_anim),
                        None,
                    ),
                )?;
            } else {
                signals::block_on_interruptible(
                    rt()?,
                    agent::run_interactive(&cfg, cli.globals.budget),
                )?;
            }
        }
        Command::Resume { id, list } => {
            // `--list` returned before provider resolution; this arm is the
            // actual reopen, which is a full deck session (durable state is
            // a deck feature — the plain REPL has no session to restore).
            debug_assert!(!list, "handled before provider resolution");
            if !term_policy::use_deck(cli.globals.plain) {
                return Err(
                    "`stella resume` reopens a Command Deck session and needs a real \
                     terminal (it cannot combine with --plain / STELLA_PLAIN / a piped \
                     stream). `stella resume --list` works anywhere."
                        .to_string(),
                );
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
                    term_policy::animation_disabled(cli.globals.no_anim),
                    Some(request),
                ),
            )?;
        }
        // Models/Version (and Tools) short-circuit in the first match at the
        // top of `run` before a provider is resolved; Init is handled by the
        // caller. Reaching any of them here is impossible.
        Command::Init
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
        | Command::Stats { .. }
        | Command::Usage { .. }
        | Command::Cloud { .. }
        | Command::Telemetry { .. }
        | Command::Memory { .. }
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
#[path = "main_tests.rs"]
mod main_tests;
