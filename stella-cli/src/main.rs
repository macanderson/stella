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
mod cache_insight;
mod candidate_ws;
mod claims;
mod cloud_drain;
mod command_deck;
mod commands_cmd;
mod config;
mod connect_cmd;
mod contextgraph;
mod credential_handoff;
mod credential_status;
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
mod subsession;
mod tool_foundry;
mod tool_policy;
mod tool_switches;
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
}

use std::io::IsTerminal;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
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
/// branch on the shape it was handed instead of sniffing for keys. A version
/// stamped on only some summaries would be worse than none: a script could not
/// rely on reading it.
///
/// All three envelopes are built from structs with the version declared first,
/// so a derived `Serialize` puts it at the head of the object. That is a
/// courtesy to whoever reads the output by eye, not a promise: key order stays
/// outside the contract and consumers must read by key. Building any of them
/// with `serde_json::json!` would quietly undo it — a `json!` object is a
/// sorted map, which buries `schema_version` mid-envelope.
///
/// # Why version at all
///
/// The envelope's key set is a contract with every script that parses it, and
/// the cost of adding the stamp rises with the number of consumers. Today it is
/// additive and harmless; after the first external script pins the current
/// shape, it is a compatibility negotiation (#644). This is the same reasoning
/// that versioned the drain wire format ahead of its transport — see
/// `DRAIN_SCHEMA_VERSION` in `stella-store`.
///
/// # When to bump
///
/// Increment when a consumer written against the previous version could break:
///
/// - a key is removed or renamed,
/// - a key's value type changes (`string` → `object`, scalar → array),
/// - a key's *meaning* changes while its name and type stay the same.
///
/// Do **not** increment for purely additive change — a new key appended to the
/// envelope. Consumers are required to ignore keys they do not recognize (the
/// same discipline rule 1 of the event-stream contract imposes), so an addition
/// cannot break a correct client, and bumping for one would burn the signal.
///
/// The `events` array is deliberately **out of scope**: the event vocabulary
/// carries its own forward-compatibility contract
/// (`website/content/docs/event-stream-compatibility.mdx`), and a new event type
/// never bumps this number. Everything else the envelope owns — including the
/// nested `verdict`, `reflection`, and `files_touched` payloads — is covered.
///
/// The consumer-facing statement of this rule lives in
/// `website/content/docs/scripting.mdx`; keep the two in step.
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

#[derive(Parser)]
#[command(
    name = "stella",
    version = version_static(),
    about = "A fast, BYOK, model-agnostic terminal coding agent"
)]
struct Cli {
    #[command(flatten)]
    globals: GlobalArgs,

    #[command(subcommand)]
    command: Option<Command>,
}

/// Session-wide flags shared by every subcommand — model routing,
/// credentials, output shape, spend limit, UI toggles.
///
/// Every field here MUST carry `global = true`. clap accepts a plain
/// root-level flag only *before* the subcommand token, so a non-global
/// field in this struct is silently unreachable in the position users
/// naturally type it (`stella fleet … --budget 5` dies with "unexpected
/// argument"). `global = true` registers the flag with every subcommand,
/// making both positions valid. The invariant is machine-enforced by
/// `every_root_flag_is_global` in main_tests.rs — a new field without the
/// attribute fails the suite, not a user's shell.
///
/// The names here are reserved CLI-wide: a subcommand flag reusing one
/// does not shadow cleanly — clap propagates the global's value slot into
/// every subcommand, and the id collision panics at match time in debug
/// builds and misbinds in release. `no_subcommand_flag_reuses_a_global_name`
/// in main_tests.rs enforces uniqueness (it is why `connect linear` pastes
/// a key via `--paste-key`, not `--api-key`).
#[derive(clap::Args)]
struct GlobalArgs {
    /// Override the worker model for this invocation: provider/model_id
    /// (e.g. zai/glm-5.2, anthropic/claude-fable-5, openai/gpt-5.5)
    #[arg(long, global = true, env = "STELLA_MODEL")]
    model: Option<String>,

    /// API key for the selected provider, highest-precedence step of the
    /// credential chain (CLI flag -> env var -> credentials file ->
    /// interactive prompt). Prefer an env var or
    /// ~/.stella/credentials.toml for anything long-lived — a flag
    /// value is visible in shell history and `ps`.
    #[arg(long, global = true)]
    api_key: Option<String>,

    /// Base URL override. Required with `--model local/<model>` to point at a
    /// local OpenAI-compatible server (Ollama, vLLM, LM Studio, llama.cpp
    /// server — e.g. http://localhost:11434/v1); optional for every other
    /// provider to route through a proxy.
    #[arg(long, global = true, env = "STELLA_BASE_URL")]
    base_url: Option<String>,

    /// Output format: text (interactive), json (one final object), or
    /// stream-json (one line per agent event)
    #[arg(
        long,
        global = true,
        env = "STELLA_OUTPUT_FORMAT",
        value_enum,
        default_value = "text"
    )]
    output_format: OutputFormat,

    /// Hard USD spend limit for the whole run — a session-scoped cap:
    /// enforced mode aborts cleanly (never mid-tool) once cumulative spend
    /// across every turn and goal round exceeds this, not each turn on its
    /// own. Omit to meter spend for the cost summary without ever blocking
    /// (observed mode).
    #[arg(long, global = true, env = "STELLA_BUDGET", value_parser = parse_budget)]
    budget: Option<f64>,

    /// Use the plain line-based REPL for chat instead of the Command Deck
    /// (the tabbed TUI). The deck also steps aside automatically when stdin
    /// or stdout is not a terminal. Env: STELLA_PLAIN=1.
    #[arg(long, global = true)]
    plain: bool,

    /// Freeze all deck animation (the run progress bar's shimmer/pulse and the
    /// caret blink) to a static frame — for CI and asciinema-style recordings.
    /// Also forced on by STELLA_NO_ANIM or NO_COLOR.
    #[arg(long, global = true)]
    no_anim: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Send a one-shot prompt (non-interactive)
    Run {
        /// The prompt to send
        prompt: String,

        /// Use the raw step-loop instead of the staged pipeline (triage, plan,
        /// execute, verify, judge). The pipeline is the default; this flag
        /// falls back to the direct Engine::run_turn path.
        #[arg(long)]
        no_pipeline: bool,

        /// Test command the pipeline's verify stage runs deterministically
        /// (e.g. "cargo test -p my-crate"). Arms the fail→pass flip oracle:
        /// a change that flips a failing test to passing can submit without
        /// a model-judge call. Omitted, verification always escalates to the
        /// judge.
        #[arg(long, value_name = "CMD")]
        test_command: Option<String>,

        /// Keep the authored witness test as a file in your working tree.
        /// By default the witness is scaffolding: it proves this run's goal
        /// inside the candidate workspace and is discarded with it, so an
        /// already-satisfied test is never left behind in your test tree.
        /// Pass this to promote it to a real test you can commit.
        #[arg(long)]
        keep_witness: bool,
    },

    /// arena-bench adapter: run the task in --task-dir (prompt in TASK.md)
    /// while recording a contextgraph-trace journal the arena runner judges
    /// with the protocol's replay oracles. Speaks the adapter contract
    /// (--task-dir/--journal/--state-dir/--resume); see
    /// <https://github.com/macanderson/arena-bench>.
    Arena {
        /// The episode workspace; the prompt is read from TASK.md inside it.
        #[arg(long)]
        task_dir: std::path::PathBuf,

        /// The contextgraph-trace journal to append (crash-safe, per-event).
        #[arg(long)]
        journal: std::path::PathBuf,

        /// Agent state that persists across episodes (memory arm).
        #[arg(long)]
        state_dir: std::path::PathBuf,

        /// Present when re-invoked after a chaos kill: recover the journal,
        /// declare what was recovered, continue the same session.
        #[arg(long)]
        resume: bool,

        /// Use the raw step-loop instead of the staged pipeline.
        #[arg(long)]
        no_pipeline: bool,

        /// Test command for the pipeline's deterministic verify ladder.
        #[arg(long, value_name = "CMD")]
        test_command: Option<String>,
    },

    /// Work in judged rounds until a judge model confirms the goal is met.
    /// Each working round runs through the staged pipeline (triage, plan,
    /// witness, execute, verify) by default; --no-pipeline falls back to the
    /// raw step-loop.
    Goal {
        /// What must be true when done — assessed by the judge each round
        goal: String,

        /// Use the raw step-loop instead of the staged pipeline for each
        /// working round. The pipeline is the default.
        #[arg(long)]
        no_pipeline: bool,
    },

    /// Watch CI for a branch/PR and fix failures until it is fully green
    Monitor {
        /// Branch name or PR number (default: main)
        target: Option<String>,
    },

    /// Start an interactive REPL session
    Chat,

    /// Reopen a previous session exactly where it stood — transcript,
    /// conversation, pending prompts. Sessions are durable (quit, crash, and
    /// power loss included); the deck's SESSIONS overlay (`←` on an empty
    /// prompt, `⏎` on a row) is the same navigation from inside a session.
    Resume {
        /// Registry id (`ses-…`) of the session to reopen. Omitted: the most
        /// recently active resumable session of this workspace.
        id: Option<String>,

        /// List this machine's sessions (resumable ones marked) and exit.
        #[arg(long)]
        list: bool,
    },

    /// Analyze this workspace and infer its domain taxonomy
    /// (.stella/domains.toml) — the tagging vocabulary for memories,
    /// reflections, and every code-graph node/edge
    Init,

    /// List every tool available to the agent this session — built-ins,
    /// developer custom tools (.stella/tools/), and manifest diagnostics
    Tools {
        /// Validate custom tool manifests instead of listing: parse every
        /// `<name>.toml`, check names, required fields, timeouts, and
        /// collisions with built-ins and other manifests, then exit
        /// non-zero if any manifest has errors. Pass a directory to check
        /// (defaults to the dirs discovery scans: .stella/tools/ and
        /// ~/.stella/tools/).
        #[arg(long, value_name = "DIR")]
        validate: Option<Option<std::path::PathBuf>>,
    },

    /// Fan tasks out to a fleet of worker agents in ONE shared tree —
    /// coordinated by cooperative claims (lock-on-first-write, sub-second,
    /// rivals named), wave-scheduled by dependency, every attempt, commit,
    /// and dollar recorded in .stella/private/fleet.db. Tasks opting into
    /// isolation = "isolated" get a dedicated worktree whose `fleet/<task>`
    /// branch is left in place for review.
    Fleet {
        /// Task prompts — each becomes an independent task in the SHARED
        /// tree (cooperative claims coordinate writers; pass a plan file
        /// with `isolation = "isolated"` for per-task worktrees)
        #[arg(required_unless_present = "plan")]
        tasks: Vec<String>,

        /// A plan file instead: `.json` or `.toml` with `[[tasks]]` entries
        /// (id, title, prompt, optional depends_on + isolation + claims —
        /// paths held as cooperative file locks while the task runs)
        #[arg(long, value_name = "FILE", conflicts_with = "tasks")]
        plan: Option<std::path::PathBuf>,

        /// Max tasks dispatched concurrently within one wave
        #[arg(long, default_value_t = 4)]
        max_concurrency: usize,

        /// Git ref `isolation = "isolated"` worktrees branch from
        /// (default: current HEAD); shared-tree tasks ignore it
        #[arg(long)]
        base_ref: Option<String>,

        /// After the fan-out, watch each fleet branch's CI to completion and
        /// reconcile its PR status via `gh` (the fleet PR/CI monitor). Exits
        /// non-zero if any watched branch ends red. Meaningful once the
        /// branches are pushed — e.g. task prompts that push and open PRs.
        #[arg(long)]
        watch: bool,

        /// Use the raw step-loop instead of the staged pipeline (triage,
        /// plan, witness, execute, verify) for each worker. The pipeline is
        /// the default.
        #[arg(long)]
        no_pipeline: bool,
    },

    /// Query the code graph built by `stella init` — symbol definitions and
    /// references, a file's imports/importers, or its graph neighborhood.
    /// Offline: reads .stella/private/codegraph.db, needs no API key.
    Graph {
        /// What to ask the graph
        #[arg(value_enum)]
        op: contextgraph::GraphOp,

        /// Symbol name (definitions/references) or workspace-relative file
        /// path (imports/importers/neighbors)
        target: String,
    },

    /// List or run the project's package-manager scripts — deterministic
    /// static detection (cargo/npm/uv/go/make/just/…) mapped onto canonical
    /// verbs (install/build/check/start/test/lint/format). Offline: manifest
    /// parsing plus a local subprocess, needs no API key.
    Scripts {
        #[command(subcommand)]
        cmd: scripts_cmd::ScriptsCmd,
    },

    /// Custom slash commands: list what this workspace offers, or convert
    /// markdown definitions to TOML. Conversion is deliberate and never part
    /// of `init` — `init` SYMLINKS `.claude/commands/`, so a converted copy
    /// trades the live link for a typed `allowed-tools` and a delimiter-free
    /// `prompt`. Offline: reads and writes local files, needs no API key.
    Commands {
        #[command(subcommand)]
        cmd: commands_cmd::CommandsCmd,
    },

    /// Inspect the storage map — every storage layer, namespace, relation,
    /// and field, with intent/boundaries from stella.storage.toml. Offline:
    /// reads .stella/private/codegraph.db + the manifest, needs no API key.
    Storage {
        #[command(subcommand)]
        cmd: storage_cmd::StorageCmd,
    },

    /// List configured providers and available models
    Models {
        #[command(subcommand)]
        cmd: Option<ModelsCmd>,
    },

    /// Review what the adaptive-context loop wants to make durable, before it
    /// lands: the proposals it induced, the distinct tasks behind each, and
    /// Keep/Edit/Ignore. Every decision is recorded as an immutable event, so
    /// the review history replays exactly. Offline: reads and appends to
    /// .stella/private/context.db only, needs no API key. Phase 3 (#714).
    Proposals {
        #[command(subcommand)]
        cmd: proposals_cmd::ProposalsCmd,
    },

    /// Eval-driven self-tuning of stella's own policy (#831): A/B one knob
    /// (worker reasoning effort) over two loop-bench `--json` result files,
    /// auto-select the winner, and — with `--promote` — write it to settings
    /// with a reversible rollback record. `rollback` reverts the last
    /// promotion; `status` shows the ledger. Offline: reads result files and
    /// the local ledger, writes settings only on `--promote`. No API key.
    Tune {
        #[command(subcommand)]
        cmd: tune_cmd::TuneCmd,
    },

    /// Show the exact context a past model call was sent — the reconstructed
    /// message array, rebuilt from the recorded receipts and verified against
    /// the digests taken at emission. With no arguments, lists executions that
    /// have receipts; with an execution id, lists its model calls. Reads
    /// .stella/private/store.db only; needs no API key and never writes.
    Inspect {
        /// Execution to inspect (omit to list recent executions)
        execution_id: Option<i64>,

        /// Turn instance within the execution
        #[arg(long, default_value_t = 0)]
        turn: u32,

        /// Step to reconstruct (omit to list the execution's calls)
        #[arg(long)]
        step: Option<u64>,

        /// Which model call at that step: 0 the worker, 1 the overflow
        /// summarizer, 2+ a pipeline management role
        #[arg(long = "call-seq", default_value_t = 0)]
        call_seq: u64,

        /// Output format
        #[arg(long, value_enum, default_value = "text")]
        format: inspect::InspectFormat,

        /// Print message bodies in full instead of eliding long ones
        #[arg(long)]
        full: bool,
    },

    /// Summarize cost, tokens, and resolve rate per provider/model from
    /// local telemetry (.stella/private/store.db) — $/resolved-task receipts.
    /// `stella stats prune` bounds that store's growth
    Stats {
        /// Output format: table (aligned, with TOTAL row), json, or csv
        #[arg(long, value_enum, default_value = "table")]
        format: stats::StatsFormat,

        /// Only show executions for this provider id (e.g. zai, anthropic,
        /// local)
        #[arg(long)]
        provider: Option<String>,

        #[command(subcommand)]
        cmd: Option<stats::StatsCmd>,
    },

    /// Cross-project telemetry hub (~/.stella/usage.db): global report,
    /// cursor-based sync, backfill across every known project
    Usage {
        #[command(subcommand)]
        cmd: Option<usage_cmd::UsageCmd>,
    },

    /// Cloud account registration (stub): org / workspace identity that
    /// scopes replicated telemetry; OAuth login attaches here later
    Cloud {
        #[command(subcommand)]
        cmd: usage_cmd::CloudCmd,
    },

    /// Inspect or explicitly flush the managed enterprise operational spool.
    /// Disabled by default; requires a signed org-managed enrollment.
    Telemetry {
        #[command(subcommand)]
        cmd: TelemetryCmd,
    },

    /// Open the Observatory — a local web dashboard over this workspace's
    /// telemetry (.stella/private/store.db + fleet.db): spend, tokens, cache
    /// traffic, tool calls, files touched, memory citations, reflections,
    /// and fleet runs. Binds 127.0.0.1 only and opens the stores strictly
    /// read-only — nothing ever leaves this machine.
    Observe {
        /// Port to bind on 127.0.0.1 (0 picks a free port)
        #[arg(long, default_value_t = 7787)]
        port: u16,

        /// Open the dashboard in the default browser once serving
        #[arg(long)]
        open: bool,
    },

    /// Turn markdown you already wrote — `AGENTS.md`, `CLAUDE.md`, or any file
    /// you name — into steering Stella can check, cite, and retire. With no
    /// arguments, scans the workspace and shows what it found. Reads local
    /// files only; needs no API key.
    Ingest(ingest_cmd::IngestArgs),

    /// What the work cost, and whether anyone said it was good: calls, characters
    /// typed, follow-ups, and the verdict a merged or closed pull request implies.
    /// Reads local state only; needs no API key, and no model judges anything.
    Scoreboard,

    /// Inspect the project's memories through the citation feedback loop —
    /// most-cited first, usefulness scores, truthfulness — and promote an
    /// eligible memory to a project rule (.stella/rules/). Reads local state
    /// only; needs no API key.
    Memory {
        #[command(subcommand)]
        cmd: memory_cmd::MemoryCmd,
    },

    /// Manage MCP servers: search a registry, install into .stella/mcp.toml,
    /// list configured servers, and show tool-usage telemetry. Enable/disable
    /// is per-session and lives in the deck's MCP tab (`/mcp`). Reads/writes
    /// local state (+ the registry over HTTP); needs no API key.
    Mcp {
        #[command(subcommand)]
        cmd: McpCmd,
    },

    /// Connect an issue tracker (GitHub/Linear) via OAuth or a pasted key —
    /// enables the issue tools (search_issues, create_issue, list_labels, …)
    /// and the deck's Issues tab. Credentials land owner-only in
    /// ~/.stella/integrations.json; needs no model API key.
    Connect {
        #[command(subcommand)]
        cmd: ConnectCmd,
    },

    /// Show current configuration
    Config,

    /// Check the local state stella owns and report each named check's
    /// verdict — today the integrity of this workspace's session store
    /// (.stella/private/store.db), verified with SQLite's own
    /// quick_check/integrity_check. Exits non-zero if any check fails, so it
    /// can gate a script. Reads local state only; needs no API key.
    Doctor {
        /// Repair a store.db that failed the check: move it aside to a
        /// timestamped name (RENAMED, never deleted — its WAL/SHM siblings
        /// travel with it), then copy out whatever is still readable into a
        /// separate salvaged database. Only ever acts on a database SQLite
        /// itself judged corrupt; a healthy store and an inconclusive check
        /// are both left untouched. The next session starts a fresh store.
        #[arg(long)]
        repair: bool,
    },

    /// Manage BYOK provider keys stored in
    /// ~/.stella/credentials.toml (set/remove/list) — keys resolved
    /// via an env var or settings.json still take precedence per the normal
    /// chain; `stella models`/`stella config` show which source actually
    /// wins. Never prints a secret value; needs no model API key itself.
    Auth {
        #[command(subcommand)]
        cmd: AuthCmd,
    },

    /// Print the version and exit
    Version,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
enum TelemetryCmd {
    /// Show enrollment state, pending bytes/rows, and local drop count.
    Status,
    /// Attempt one bounded delivery batch now.
    Flush,
    /// Explicitly discard rows bound to a superseded enrolled sink.
    RolloverDiscard,
}

/// `stella auth` subcommands — the whole `~/.stella/credentials.toml`
/// management surface. Deliberately small: a handful of BYOK keys, not a
/// config language (mirrors `CredentialsFile`'s own doc intent).
#[derive(Subcommand)]
pub enum AuthCmd {
    /// Store (or replace) a provider's API key in credentials.toml.
    Set {
        /// Provider id (a built-in like `zai`/`anthropic`/`openai`, or a
        /// settings.json-defined custom provider id)
        provider: String,

        /// Pass the key directly on the command line. WARNING: this value
        /// becomes visible in shell history and `ps` output — prefer
        /// --stdin, or omit both flags for an interactive masked prompt.
        #[arg(long, conflicts_with = "stdin")]
        key: Option<String>,

        /// Read the key from stdin (one line, trimmed) instead of a flag or
        /// an interactive prompt — for scripts, e.g. `printf '%s' "$KEY" | \
        /// stella auth set zai --stdin`.
        #[arg(long)]
        stdin: bool,
    },

    /// Remove a provider's stored key from credentials.toml.
    Remove {
        /// Provider id
        provider: String,
    },

    /// List providers with a key stored in credentials.toml (redacted
    /// preview + resolution source).
    List,
}

/// `stella models` subcommands — the model-catalog surface. A bare
/// `stella models` keeps its provider/key listing; these manage the
/// on-disk master list every slug validates against.
#[derive(Subcommand)]
enum ModelsCmd {
    /// Sync the model catalog: the models.dev master list (public, no API
    /// key), then every configured provider's own live /models listing.
    /// Incremental: an unchanged master list is one conditional request
    /// (ETag) and zero writes; pricing changes append a new model-card
    /// version (the latest version is what displays everywhere). Also runs
    /// automatically once a day while a provider credential is configured.
    Refresh {
        /// Re-download even when the server says the list is unchanged
        /// (recovery hatch for a corrupted local catalog)
        #[arg(long)]
        force: bool,
    },
    /// List the model catalog: provider/model slugs with latest pricing
    /// (USD per Mtok), context window, capability, and model maker. Scoped
    /// to providers whose credential resolves; --all lifts the scope.
    List {
        /// Only this provider id (e.g. anthropic, openrouter)
        #[arg(long)]
        provider: Option<String>,

        /// Include providers with no configured credential
        #[arg(long)]
        all: bool,
    },
}

/// `stella connect` subcommands — tracker connections consumed by the issue
/// tools. GitHub uses the OAuth device flow (public client, no secret in the
/// binary); Linear uses browser OAuth when an app is configured, else a
/// personal API key. All traffic is user-initiated — connecting is what opts
/// a workspace into tracker calls.
#[derive(Subcommand)]
pub enum ConnectCmd {
    /// Connect GitHub via the OAuth device flow (or --token to paste a PAT)
    Github {
        /// Paste a personal access token instead of running the device flow
        #[arg(long)]
        token: bool,
    },
    /// Connect Linear via browser OAuth (needs STELLA_LINEAR_CLIENT_ID) or a
    /// personal API key
    Linear {
        /// Paste a personal API key even when an OAuth app is configured.
        /// (Named to stay clear of the session-wide `--api-key`, which is
        /// the model-provider credential — a different secret entirely.)
        #[arg(long)]
        paste_key: bool,
    },
    /// Show stored connections, their accounts, and credential precedence
    Status,
    /// Forget a stored connection
    Remove {
        /// github | linear
        provider: String,
    },
}

/// `stella mcp` subcommands — the scriptable half of the MCP management surface
/// (the deck's MCP tab is the interactive half; per-session enable/disable and
/// the masked auth prompt live only there).
#[derive(Subcommand)]
pub enum McpCmd {
    /// List configured MCP servers (.stella/mcp.toml)
    List,
    /// Search the MCP server registry (settings.json `mcp.registry_url`, else
    /// the official registry)
    Search {
        /// Substring to match server names (omit to list)
        query: Vec<String>,
        /// Max results in the page
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Install a registry server into .stella/mcp.toml (overwrites — MCP
    /// servers are not versioned)
    Install {
        /// The registry server name (as shown by `stella mcp search`)
        name: String,
        /// Local alias / tool-namespace segment (default: sanitized name)
        #[arg(long)]
        alias: Option<String>,
    },
    /// Remove a configured server from .stella/mcp.toml
    Remove {
        /// The configured server's local name
        name: String,
    },
    /// OAuth login to a configured http server (opens your browser; tokens
    /// land owner-only in .stella/private/mcp_oauth.json and auto-refresh)
    Login {
        /// The configured server's local name
        name: String,
    },
    /// Forget a server's OAuth tokens
    Logout {
        /// The configured server's local name
        name: String,
    },
    /// Show MCP tool-usage telemetry (.stella/private/store.db): calls per server/tool
    Usage,
}

/// NUL boundaries prevent LLVM's string pooling from adjoining identifier
/// characters to the version bytes. The claim launcher can therefore attest
/// the full compile-time identity without executing the binary.
const BUILD_VERSION_IDENTITY: &str = concat!("\0", env!("STELLA_BUILD_VERSION"), "\0");

/// The version string shown by `--version` and `stella version`. `build.rs`
/// turns an optional compile-time `STELLA_BUILD_GIT_SHA` into one contiguous
/// literal; this returns the interior of the deliberately delimited identity.
/// Ordinary release builds still carry the bare package version.
fn version_string() -> &'static str {
    &BUILD_VERSION_IDENTITY[1..BUILD_VERSION_IDENTITY.len() - 1]
}

/// clap's `version` attribute needs a `'static` string.
fn version_static() -> &'static str {
    version_string()
}

/// Honour `TERM=dumb` by switching ANSI output off process-wide.
///
/// The `colored` crate already respects `NO_COLOR`, `CLICOLOR*`, and a
/// non-tty stream, but not `TERM` — so a dumb terminal (Emacs `M-x shell`,
/// a bare serial console, an editor's build pane, `TERM=dumb` in CI) got the
/// full escape-sequence treatment and rendered it literally. Every other Unix
/// tool that colours output treats `dumb` as "this terminal cannot", so
/// stella does too. An explicit `CLICOLOR_FORCE` still wins: it is the
/// documented way to say "I know what my terminal is", and `colored`'s own
/// override resolution keeps honouring it because this only sets the default
/// when the user has not forced anything.
fn apply_dumb_terminal_policy() {
    let forced = std::env::var_os("CLICOLOR_FORCE").is_some_and(|v| !v.is_empty() && v != "0");
    if forced {
        return;
    }
    if std::env::var_os("TERM").is_some_and(|term| term == "dumb") {
        colored::control::set_override(false);
    }
}

/// Whether `chat` should launch the Command Deck: an explicit `--plain` or
/// STELLA_PLAIN=1 opts out, and both stdin and stdout must be real terminals
/// (raw mode + the alternate screen are meaningless on a pipe).
fn use_deck(plain_flag: bool) -> bool {
    let plain_env = std::env::var_os("STELLA_PLAIN").is_some_and(|v| !v.is_empty() && v != "0");
    !plain_flag && !plain_env && std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
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

/// `--budget` must be a positive, finite dollar amount — a NaN or negative
/// limit would make every comparison silently false and turn the "hard
/// cap" into a no-op, the worst failure mode for a money control.
fn parse_budget(raw: &str) -> Result<f64, String> {
    let value: f64 = raw
        .parse()
        .map_err(|_| format!("`{raw}` is not a number"))?;
    if !value.is_finite() || value <= 0.0 {
        return Err(format!(
            "budget must be a positive dollar amount, got `{raw}`"
        ));
    }
    Ok(value)
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
    apply_dumb_terminal_policy();

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

    let cli = Cli::parse();

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
        Some(Command::Tools { validate }) => {
            return match validate {
                // `--validate` (dir optional) is the strict pre-flight path;
                // a plain `stella tools` stays the lenient listing.
                Some(dir) => agent::run_tools_validation(dir.as_deref()),
                None => agent::run_tools_listing(),
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
        // Phase 3 (#714). Reads and appends to the local lifecycle ledger
        // only — no provider, no API key.
        Some(Command::Proposals { cmd }) => {
            return proposals_cmd::run_proposals(cmd);
        }
        // #831 first slice. Reads loop-bench result files + the local ledger;
        // writes settings only on `--promote`. No provider, no API key.
        Some(Command::Tune { cmd }) => {
            return tune_cmd::run_tune(cmd);
        }
        Some(Command::Inspect {
            execution_id,
            turn,
            step,
            call_seq,
            format,
            full,
        }) => {
            // Reads the local receipt tables only — no provider, no API key.
            return inspect::run_inspect(*execution_id, *turn, *step, *call_seq, *format, *full);
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
        Some(Command::Doctor { repair }) => {
            // Reads local state only — and with --repair renames files inside
            // .stella/private/. No provider, no API key, and deliberately
            // before `Config::load`: a workspace whose store is corrupt must be
            // diagnosable without a working model configuration.
            return doctor::run_doctor(*repair);
        }
        Some(Command::Version) => {
            println!("stella v{}", version_string());
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
                cli.globals.no_anim,
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
            if use_deck(cli.globals.plain) {
                signals::block_on_interruptible(
                    rt()?,
                    command_deck::run_deck_session(
                        &cfg,
                        cli.globals.budget,
                        cli.globals.no_anim,
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
            if !use_deck(cli.globals.plain) {
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
                    cli.globals.no_anim,
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
        // Phase 3 (#714)
        | Command::Proposals { .. }
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
