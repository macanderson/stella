//! The command-line surface: the argument tree `clap` parses, and nothing
//! else. Extracted from `main.rs` (#629's file-size ratchet) so the shape of
//! the CLI is one readable file, and `main.rs` is dispatch.
//!
//! Two audiences read this file's doc comments. The FIRST line of each
//! variant's `///` is the summary users see in `stella --help` and in shell
//! completion menus, so it must stand alone and stay short. Everything after
//! the first blank `///` line is `long_about`: the full explanation, shown
//! only by `stella help <command>`. Writing a paragraph with no blank line
//! makes the whole paragraph the summary and buries the command list — the
//! bug `help.rs` and `about_lines_are_short` exist to prevent.

use clap::{Parser, Subcommand};

pub(crate) mod help;

use crate::{
    OutputFormat, build_info, commands_cmd, context_cmd, contextgraph, dataset_cmd, ingest_cmd,
    inspect, memory_cmd, proposals_cmd, scripts_cmd, stats, storage_cmd, tune_cmd, usage_cmd,
};

#[derive(Parser)]
#[command(
    name = "stella",
    version = build_info::version_static(),
    long_version = build_info::long_version_static(),
    about = "A fast, BYOK, model-agnostic terminal coding agent",
    // Set explicitly, and not only for the extra paragraph. With `long_about`
    // absent, clap derive falls back to the doc comment of the *flattened*
    // `GlobalArgs` — so `stella --help` opened with fifteen lines of clap
    // implementation notes addressed to this repo's maintainers, before the
    // usage line. `the_long_help_opens_with_the_product_not_the_source`
    // fails if that regresses.
    long_about = "\
A fast, BYOK, model-agnostic terminal coding agent.

Bring your own key: point --model at any provider (Anthropic, OpenAI, z.ai, \
OpenRouter, …) or at a local OpenAI-compatible server, and stella runs \
against it. Sessions, telemetry, and the code graph stay on this machine, \
under .stella/ in the workspace.

New here: `stella init` indexes the workspace, `stella run \"<prompt>\"` does \
one task, `stella chat` opens a session."
)]
pub(crate) struct Cli {
    #[command(flatten)]
    pub(crate) globals: GlobalArgs,

    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

impl Cli {
    /// The output format this invocation promised its caller.
    ///
    /// `--output-format` stopped being global in #1493: 34 of 36 subcommands
    /// never consulted it, which made the flag a promise the CLI mostly broke.
    /// It now exists only on the commands that keep it — but three
    /// process-wide decisions in `main` still need one answer for ANY
    /// command: whether interactive credential prompts are forbidden, whether
    /// the env-file announcement may print, and whether a failure gets a
    /// machine-readable error envelope on stdout.
    ///
    /// `Run` and `Fleet` answer with their declared flag. `Arena` has no flag
    /// to consult because its contract IS stream-json (the runner is the only
    /// reader), so it answers that — which also closes the hole where an
    /// arena run could block on a masked credential prompt no runner would
    /// ever see. Every other command renders human text; their machine
    /// surfaces are their own `--format` flags.
    pub(crate) fn output_format(&self) -> OutputFormat {
        match &self.command {
            Some(Command::Run { output_format, .. } | Command::Fleet { output_format, .. }) => {
                *output_format
            }
            Some(Command::Arena { .. }) => OutputFormat::StreamJson,
            _ => OutputFormat::Text,
        }
    }
}

// The two paragraphs below are `//` and not `///` deliberately. clap derive
// lifts a flattened `Args` struct's doc comment into the *parent's*
// `long_about` when the parent sets none, which is exactly how these notes —
// addressed to maintainers of this file — became the opening screen of
// `stella --help`. `Cli` now sets `long_about` explicitly, and this keeps the
// leak closed even if it is ever dropped.
//
// Every field here MUST carry `global = true`. clap accepts a plain
// root-level flag only *before* the subcommand token, so a non-global
// field in this struct is silently unreachable in the position users
// naturally type it (`stella fleet … --budget 5` dies with "unexpected
// argument"). `global = true` registers the flag with every subcommand,
// making both positions valid. The invariant is machine-enforced by
// `every_root_flag_is_global` in src/tests.rs — a new field without the
// attribute fails the suite, not a user's shell.
//
// The names here are reserved CLI-wide: a subcommand flag reusing one
// does not shadow cleanly — clap propagates the global's value slot into
// every subcommand, and the id collision panics at match time in debug
// builds and misbinds in release. `no_subcommand_flag_reuses_a_global_name`
// in src/tests.rs enforces uniqueness (it is why `connect linear` pastes
// a key via `--paste-key`, not `--api-key`).
/// Session-wide flags shared by every subcommand — model routing,
/// credentials, spend limit, UI toggles.
#[derive(clap::Args)]
// Heading rather than clap's default "Options": these are the flags that hold
// for the whole session and are accepted on either side of the subcommand
// token, which is the one thing about them worth saying in a help page.
#[command(next_help_heading = "Session flags (before or after the command)")]
pub(crate) struct GlobalArgs {
    /// Worker model for this run: provider/model_id
    ///
    /// Examples: zai/glm-5.2, anthropic/claude-fable-5, openai/gpt-5.5.
    /// Scoped to this invocation — the configured default is untouched.
    #[arg(long, global = true, env = "STELLA_MODEL")]
    pub(crate) model: Option<String>,

    /// API key for the selected provider (prefer `stella auth`)
    ///
    /// The highest-precedence step of the credential chain (CLI flag -> env
    /// var -> credentials file -> interactive prompt). Prefer an env var or
    /// ~/.stella/credentials.toml for anything long-lived — a flag value is
    /// visible in shell history and `ps`.
    // Long help only: `-h` is the screen people hit while working, and it
    // earns its space by showing what changes a run, not every knob. The
    // credential flags have `stella auth`, the rest have `--help`.
    #[arg(long, global = true, hide_short_help = true)]
    pub(crate) api_key: Option<String>,

    /// Base URL override; required by `--model local/<model>`
    ///
    /// Points at a local OpenAI-compatible server (Ollama, vLLM, LM Studio,
    /// llama.cpp server — e.g. http://localhost:11434/v1). Optional for every
    /// other provider, where it routes through a proxy.
    #[arg(long, global = true, env = "STELLA_BASE_URL", hide_short_help = true)]
    pub(crate) base_url: Option<String>,

    /// Hard USD spend limit for the whole session
    ///
    /// Scoped to the session, not the turn: enforced mode aborts cleanly
    /// (never mid-tool) once cumulative spend across every turn and goal round
    /// exceeds this. Omit to meter spend for the cost summary without ever
    /// blocking (observed mode).
    #[arg(long, global = true, env = "STELLA_BUDGET", value_parser = parse_budget)]
    pub(crate) budget: Option<f64>,

    /// Wall-clock seconds one turn may spend before it wraps up
    ///
    /// The time twin of `--budget`, for callers running under an EXTERNAL
    /// deadline — a benchmark harness that kills the process on elapsed
    /// time. Two mechanisms arm from it: the engine declines to start an
    /// output-limit continuation it cannot finish (ending with a truthful
    /// partial instead of being destroyed mid-flight), and a one-shot
    /// `run` also stops at the next safe boundary once the allowance is
    /// spent, so the work on disk is scored rather than discarded with the
    /// kill (#1503). Interactive sessions (chat, the deck) treat it as
    /// advisory only. Set it slightly below the real deadline. Omit for no
    /// time-based limit. Env: STELLA_TURN_BUDGET.
    #[arg(long, global = true, env = "STELLA_TURN_BUDGET", value_parser = parse_turn_budget)]
    pub(crate) turn_budget: Option<std::time::Duration>,

    /// Cap output tokens per step, below what the model can write
    ///
    /// The default is the model's OWN maximum, which the catalog learns from
    /// the provider — so you never need this to get a full-length answer. Use
    /// it to deliberately spend LESS: bound cost on a long unattended run, cut
    /// latency, or match a comparator that stops lower.
    ///
    /// Clamped to the model's real ceiling rather than sent as written.
    /// Over-asking is not a longer answer, it is a provider-side rejection on
    /// every step — one that costs the round trip and then needs another call
    /// to retry.
    ///
    /// Wins over `[models.output_caps]` and per-agent `params.max_tokens`, and
    /// applies to every role. Omit for the model's own ceiling.
    /// Env: STELLA_MAX_OUTPUT_TOKENS.
    #[arg(long, global = true, env = "STELLA_MAX_OUTPUT_TOKENS", value_parser = parse_max_output_tokens)]
    pub(crate) max_output_tokens: Option<u32>,

    /// Use the plain line REPL instead of the Command Deck
    ///
    /// The deck (the tabbed TUI) also steps aside automatically when stdin or
    /// stdout is not a terminal. Env: STELLA_PLAIN=1.
    #[arg(long, global = true, hide_short_help = true)]
    pub(crate) plain: bool,

    /// Run the Command Deck so a screen reader can read it
    ///
    /// The same deck — every tab, every key, nothing removed — drawn as
    /// ordinary terminal output instead of a full-window app. It never takes
    /// over the screen: the deck draws inline under your prompt, and each
    /// completed message is written into normal scrollback once, so a reader
    /// announces it as it arrives, your review cursor can go back through it,
    /// and it is still there after you quit. Animation is frozen, panels stack
    /// in one column, grid views read as labelled rows, and moving between
    /// tabs or overlays is announced. Needs a terminal on both stdin and
    /// stdout; use --plain otherwise. Env: STELLA_ACCESSIBLE=1.
    #[arg(long, global = true, alias = "simple", hide_short_help = true)]
    pub(crate) accessible: bool,

    /// Freeze all deck animation to a static frame
    ///
    /// Stills the run progress bar's shimmer/pulse and the caret blink, for CI
    /// and asciinema-style recordings. Also forced on by STELLA_NO_ANIM or
    /// NO_COLOR, and by --accessible.
    #[arg(long, global = true, hide_short_help = true)]
    pub(crate) no_anim: bool,

    /// Diagnostics: -v info, -vv debug, -vvv trace
    ///
    /// Turns on the diagnostic plane — the stream that explains why stella
    /// behaved the way it did, as distinct from what the agent did (the
    /// event stream) or what it cost (the receipts). Records carry a stable
    /// code and typed fields, never prompts, paths, or model output.
    /// Default is `warn`. See --log-level for per-crate control.
    #[arg(short = 'v', long = "verbose", global = true, action = clap::ArgAction::Count)]
    pub(crate) verbose: u8,

    /// Diagnostic filter, per crate: "warn,stella_store=debug"
    ///
    /// A comma-separated spec. A bare level sets the default; `target=level`
    /// overrides it for that module subtree, longest match winning — so
    /// `warn,stella_store=debug,stella_model=trace` is quiet everywhere
    /// except where you are looking. Levels: off, error, warn, info, debug,
    /// trace. A target is a Rust module path, so library crates are
    /// `stella_store`, `stella_model`, … and this binary's own records are
    /// under `stella` (hyphens are accepted too: `stella-store=debug`).
    /// Also settable as STELLA_LOG (this flag wins over it, and over -v).
    /// An unrecognised clause is skipped and reported, never fatal.
    #[arg(long, global = true, value_name = "SPEC", hide_short_help = true)]
    pub(crate) log_level: Option<String>,

    /// Show the plan and wait for approval before anything runs.
    ///
    /// Scope review already interrupts for a *large* plan (more than 5 steps,
    /// 8 files, or $1.00 estimated). This asks for the plan whatever its size
    /// — the size thresholds answer "is this big?", and only you can answer
    /// "do I want to see it first?". The gate sits ahead of the stage that
    /// touches your working tree, so declining leaves it untouched.
    ///
    /// Needs an interactive terminal: there is nobody to ask in a headless
    /// run, and silently proceeding would deliver the opposite of the ask.
    //
    // `--plan-mode`, not `--plan`: `stella fleet --plan <file>` already owns
    // that name, and a global sharing it does not shadow cleanly — clap
    // propagates the global's value slot into every subcommand, so the two
    // collide at match time ("could not downcast to PathBuf, need bool").
    // `no_subcommand_flag_reuses_a_global_name` is the test that says so.
    #[arg(long, global = true)]
    pub(crate) plan_mode: bool,

    /// Turn tools off for this run only, without editing settings.json.
    ///
    /// Same spelling as the settings `"tools"` table — a comma-separated list
    /// of `name:on` / `name:off`, where a name is a tool, a group, or `*`.
    /// `--tools '*:off,read_file:on,grep:on'` is a read-only run;
    /// `--tools 'repo_push:off'` removes one capability.
    ///
    /// This scope can only narrow. It composes with settings by intersection,
    /// so it can never switch on something an org or project policy turned
    /// off — turning things off is always permitted, turning them on never
    /// overrides a deny from higher up.
    #[arg(long, global = true, value_name = "SPEC")]
    pub(crate) tools: Option<String>,

    /// Write diagnostics as JSONL to a file (created 0600)
    ///
    /// Defaults to `info` where stderr defaults to `warn`, since a file
    /// nobody reads until something breaks may as well be able to explain it;
    /// --log-level or -v raise it further. An unwritable path is reported and
    /// the run continues. For a crash you did not anticipate you need no flag
    /// at all: stella writes .stella/private/crash-*.jsonl on a panic or a
    /// failed run, and `stella doctor --last-failure` prints the newest.
    #[arg(long, global = true, value_name = "PATH", hide_short_help = true)]
    pub(crate) log_file: Option<std::path::PathBuf>,
}

#[derive(Subcommand)]
pub(crate) enum MigrateCmd {
    /// Rewrite settings.json as stella.toml
    Config {
        /// Render and report without writing anything
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Send a one-shot prompt (non-interactive)
    Run {
        /// The prompt to send. Omit it to read the prompt from stdin when it
        /// is piped (`cat spec.md | stella run`), or pass `-` to read stdin
        /// explicitly even on a terminal. A prompt that begins with `-` needs
        /// the usual `--` separator: `stella run -- --my-prompt`.
        prompt: Option<String>,

        /// Use the raw step-loop instead of the staged pipeline (triage, plan,
        /// execute, verify, verifier). The pipeline is the default; this flag
        /// falls back to the direct Engine::run_turn path.
        #[arg(long)]
        no_pipeline: bool,

        /// Test command the pipeline's verify stage runs deterministically
        /// (e.g. "cargo test -p my-crate"). Arms the fail→pass flip oracle:
        /// a change that flips a failing test to passing can submit without
        /// a model-verifier call. Omitted, verification always escalates to the
        /// verifier.
        #[arg(long, value_name = "CMD")]
        test_command: Option<String>,

        /// Keep the authored witness test as a file in your working tree.
        /// By default the witness is scaffolding: it proves this run's goal
        /// inside the candidate workspace and is discarded with it, so an
        /// already-satisfied test is never left behind in your test tree.
        /// Pass this to promote it to a real test you can commit.
        #[arg(long)]
        keep_witness: bool,

        /// Output shape: text, json, or stream-json
        ///
        /// Declared here rather than globally because this is a promise about
        /// what reaches stdout, and only the commands that keep it may make it
        /// (#1493). `json` prints one summary object; `stream-json` prints one
        /// JSON line per AgentEvent. Both also turn every failure into a
        /// machine-readable error envelope and refuse interactive credential
        /// prompts.
        #[arg(long, env = "STELLA_OUTPUT_FORMAT", value_enum, default_value = "text")]
        output_format: OutputFormat,
    },

    /// arena-bench adapter, invoked by the benchmark runner
    ///
    /// arena-bench adapter: run the task in --task-dir (prompt in TASK.md)
    /// while recording a contextgraph-trace journal the arena runner verifiers
    /// with the protocol's replay oracles. Speaks the adapter contract
    /// (--task-dir/--journal/--state-dir/--resume); see
    /// <https://github.com/macanderson/arena-bench>.
    ///
    /// Output is fixed stream-json — the runner is the only intended reader,
    /// so there is no --output-format here to promise otherwise (#1493).
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

    /// Work in judged rounds until a verifier says the goal is met
    ///
    /// Work in judged rounds until a verifier model confirms the goal is met.
    /// Each working round runs through the staged pipeline (triage, plan,
    /// witness, execute, verify) by default; --no-pipeline falls back to the
    /// raw step-loop.
    Goal {
        /// What must be true when done — assessed by the verifier each round.
        /// Omit it to read the goal from stdin when it is piped, or pass `-`
        /// to read stdin explicitly.
        goal: Option<String>,

        /// Use the raw step-loop instead of the staged pipeline for each
        /// working round. The pipeline is the default.
        #[arg(long)]
        no_pipeline: bool,
    },

    /// Watch CI for a branch or PR and fix it until green
    Monitor {
        /// Branch name or PR number (default: main)
        target: Option<String>,
    },

    /// Start an interactive session (the Command Deck)
    Chat,

    /// Reopen a previous session exactly where it stood
    ///
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

    /// Analyze this workspace: domain taxonomy and code graph
    ///
    /// Analyze this workspace and infer its domain taxonomy
    /// (.stella/domains.toml) — the tagging vocabulary for memories,
    /// reflections, and every code-graph node/edge
    Init,

    /// List every tool available to the agent this session
    ///
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

        /// Author a tool-foundry proposal into a staged custom tool:
        /// writes `<name>.toml` + `<name>.sh` under .stella/tools/proposed/
        /// (inert until it is adopted AND enabled). Omit the value to list
        /// the current proposals mined from recent bash receipts.
        #[arg(long, value_name = "NAME", conflicts_with = "validate")]
        author: Option<Option<String>>,

        /// Adopt a staged tool: move it into .stella/tools/ and run its
        /// capability witness — the call must FAIL on the existing tool
        /// surface and PASS with the new tool, producing a real value.
        /// Records the proof. Does NOT make the tool usable.
        #[arg(long, value_name = "NAME", conflicts_with_all = ["validate", "author"])]
        adopt: Option<String>,

        /// Enable an adopted tool — the one approval in the protocol a
        /// machine never grants itself. Refused if the tool's bytes changed
        /// since its witness ran.
        #[arg(long, value_name = "NAME", conflicts_with_all = ["validate", "author", "adopt"])]
        enable: Option<String>,

        /// Stop offering an adopted tool, keeping its proof on file
        #[arg(
            long,
            value_name = "NAME",
            conflicts_with_all = ["validate", "author", "adopt", "enable"]
        )]
        disable: Option<String>,

        /// Report every self-authored tool: its witness, whether it is
        /// enabled, and how often it has actually been reused since adoption
        /// (with the never-used ones named as the cost)
        #[arg(
            long,
            conflicts_with_all = ["validate", "author", "adopt", "enable", "disable"]
        )]
        foundry: bool,
    },

    /// Fan tasks out to a fleet of worker agents in one tree
    ///
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

        /// Wall-clock ceiling per worker attempt, in seconds. On expiry the
        /// task's stop line fires (the same clean cancel the dashboard's
        /// `[x]` sends) and the attempt reports as failed instead of
        /// occupying its concurrency slot forever — the only way to unstick
        /// a hung worker on a piped or CI run. Unset = unbounded.
        #[arg(long, value_name = "SECS")]
        task_timeout: Option<u64>,

        /// Output shape: text, json, or stream-json
        ///
        /// Anything but `text` keeps the live grid off and the run headless,
        /// with machine-readable error envelopes and no interactive credential
        /// prompts. Declared per-command, not globally, so it exists exactly
        /// where it is honoured (#1493).
        #[arg(long, env = "STELLA_OUTPUT_FORMAT", value_enum, default_value = "text")]
        output_format: OutputFormat,
    },

    /// Query the code graph — definitions, references, callers, imports
    ///
    /// Query the code graph built by `stella init` — symbol definitions and
    /// references, recorded call sites (callees/callers, name-based), a
    /// file's imports/importers, or its graph neighborhood.
    /// Offline: reads .stella/private/codegraph.db, needs no API key.
    Graph {
        /// What to ask the graph
        #[arg(value_enum)]
        op: contextgraph::GraphOp,

        /// Symbol name (definitions/references/callees/callers) or
        /// workspace-relative file path (imports/importers/neighbors)
        target: String,
    },

    /// List or run the project's package-manager scripts
    ///
    /// List or run the project's package-manager scripts — deterministic
    /// static detection (cargo/npm/uv/go/make/just/…) mapped onto canonical
    /// verbs (install/build/check/start/test/lint/format). Offline: manifest
    /// parsing plus a local subprocess, needs no API key.
    Scripts {
        #[command(subcommand)]
        cmd: scripts_cmd::ScriptsCmd,
    },

    /// List or convert this workspace's custom slash commands
    ///
    /// Custom slash commands: list what this workspace offers, or convert
    /// markdown definitions to TOML. Conversion is deliberate and never part
    /// of `init` — `init` SYMLINKS `.claude/commands/`, so a converted copy
    /// trades the live link for a typed `allowed-tools` and a delimiter-free
    /// `prompt`. Offline: reads and writes local files, needs no API key.
    Commands {
        #[command(subcommand)]
        cmd: commands_cmd::CommandsCmd,
    },

    /// Inspect the storage map — layers, namespaces, relations
    ///
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

    /// Review what the adaptive-context loop wants to keep
    ///
    /// Review what the adaptive-context loop wants to make durable, before it
    /// lands: the proposals it induced, the distinct tasks behind each, and
    /// Keep/Edit/Ignore. Every decision is recorded as an immutable event, so
    /// the review history replays exactly. Offline: reads and appends to
    /// .stella/private/context.db only, needs no API key. Phase 3 (#714).
    Proposals {
        #[command(subcommand)]
        cmd: proposals_cmd::ProposalsCmd,
    },

    /// Review, publish, and explain context records
    ///
    /// Review, publish, and explain context records — the reviewable steering
    /// `stella ingest` extracts from documents you already wrote. `review` shows
    /// what was proposed and what its truth probe found; `keep`/`edit`/`ignore`
    /// decide; `list` shows what currently steers; `validate` re-probes every claim
    /// and reports every finding; `explain` says why a rule applied; `propose` turns
    /// a record into a reviewable change. Offline: local files only, no API key.
    /// Epic #897.
    Context {
        #[command(subcommand)]
        cmd: context_cmd::ContextCmd,
    },

    /// A/B one policy knob over two loop-bench result files
    ///
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

    /// Curate a redacted training dataset from this workspace's receipts
    ///
    /// Curate a redacted training dataset from this workspace's receipts
    /// (#872): one JSONL record per accepted turn — prompt, tool calls with
    /// arguments and outputs, the change that landed, the verifier's verdict —
    /// with a manifest stating the exact filter that selected them. Every
    /// string passes through the secret redactor, and the output is written
    /// owner-only. Offline: reads .stella/private/store.db only, needs no API
    /// key. Human sign-off is required before a dataset is used for training.
    Dataset {
        #[command(subcommand)]
        cmd: dataset_cmd::DatasetCmd,
    },

    /// Show the exact context a past model call was sent
    ///
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

        /// Show a unified diff of what changed instead of the whole context.
        /// Bare `--diff` compares against whatever ran immediately before in
        /// the same role — the previous step, or the previous turn when this
        /// is a turn's first call. The session's very first call has no
        /// predecessor, so it is compared against the prompt as the user
        /// submitted it: the mutation no other view shows
        #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "prev")]
        diff: Option<inspect::DiffBase>,

        /// Unchanged lines to print around each change in a `--diff`
        #[arg(long, default_value_t = 3)]
        context: usize,

        /// Restrict `--diff` to one message role — `system` is the usual
        /// reason to reach for this, since every step appends tool traffic
        /// that would otherwise bury the prompt's own delta
        #[arg(long, value_enum, default_value = "all")]
        only: inspect::RoleFilter,
    },

    /// Verifier calibration: false-positive rate vs CI ground truth
    ///
    /// Fold every recorded session's pass verdicts against the CI verdicts
    /// observed after them (#871): how often did a model-verifier PASS — and,
    /// as the comparison cohort, a deterministic ladder pass — later fail
    /// CI? Rates are reported as unmeasured until CI ground truth exists.
    /// Reads .stella/private/store.db only; needs no API key and never
    /// writes.
    Calibration {
        /// Output format
        #[arg(long, value_enum, default_value = "text")]
        format: inspect::InspectFormat,
    },

    /// Cost, tokens, and resolve rate per provider and model
    ///
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

    /// Cross-project telemetry hub (~/.stella/usage.db)
    ///
    /// Cross-project telemetry hub (~/.stella/usage.db): global report,
    /// cursor-based sync, backfill across every known project
    Usage {
        #[command(subcommand)]
        cmd: Option<usage_cmd::UsageCmd>,
    },

    /// Cloud account registration (stub)
    ///
    /// Cloud account registration (stub): org / workspace identity that
    /// scopes replicated telemetry; OAuth login attaches here later
    Cloud {
        #[command(subcommand)]
        cmd: usage_cmd::CloudCmd,
    },

    /// Inspect or flush the managed enterprise telemetry spool
    ///
    /// Inspect or explicitly flush the managed enterprise operational spool.
    /// Disabled by default; requires a signed org-managed enrollment.
    Telemetry {
        #[command(subcommand)]
        cmd: TelemetryCmd,
    },

    /// Open the Observatory, a local telemetry dashboard
    ///
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

    /// Turn markdown you already wrote into checkable steering
    ///
    /// Turn markdown you already wrote — `AGENTS.md`, `CLAUDE.md`, or any file
    /// you name — into steering Stella can check, cite, and retire. With no
    /// arguments, scans the workspace and shows what it found. Reads local
    /// files only; needs no API key.
    Ingest(ingest_cmd::IngestArgs),

    /// What the work cost, and whether anyone called it good
    ///
    /// What the work cost, and whether anyone said it was good: calls, characters
    /// typed, follow-ups, and the verdict a merged or closed pull request implies.
    /// Reads local state only; needs no API key, and no model verifiers anything.
    Scoreboard,

    /// Inspect and promote the project's memories
    ///
    /// Inspect the project's memories through the citation feedback loop —
    /// most-cited first, usefulness scores, truthfulness — and promote an
    /// eligible memory to a project rule (.stella/rules/). Reads local state
    /// only; needs no API key.
    Memory {
        #[command(subcommand)]
        cmd: memory_cmd::MemoryCmd,
    },

    /// Manage MCP servers: search, install, list, telemetry
    ///
    /// Manage MCP servers: search a registry, install into .stella/mcp.toml,
    /// list configured servers, and show tool-usage telemetry. Enable/disable
    /// is per-session and lives in the deck's MCP tab (`/mcp`). Reads/writes
    /// local state (+ the registry over HTTP); needs no API key.
    Mcp {
        #[command(subcommand)]
        cmd: McpCmd,
    },

    /// Connect an issue tracker (GitHub or Linear)
    ///
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

    /// Move settings.json to stella.toml
    ///
    /// Writes `<repo>/stella.toml` for the project scope and
    /// `~/.stella/stella.toml` for the user scope, with every value serialized
    /// from what stella actually read rather than transcribed. The JSON is
    /// KEPT, never deleted — review the TOML, then remove it yourself; until
    /// you do, stella reads the TOML and says the JSON is shadowed.
    ///
    /// Runs before provider resolution, so a config too broken to start with
    /// is still migratable. The org-managed scope is not migrated: an
    /// administrator deploys that file directly.
    Migrate {
        #[command(subcommand)]
        cmd: MigrateCmd,
    },

    /// Check the local state stella owns, and report each verdict
    ///
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

        /// Print the newest crash dump instead of running the checks:
        /// .stella/private/crash-*.jsonl, written automatically when a run
        /// panics or exits non-zero. It is content-free by construction — the
        /// diagnostic record type cannot hold a prompt, a path, or model
        /// output — so it is safe to attach to a bug report without reading
        /// it first. Exits non-zero if there is no dump to print.
        #[arg(long)]
        last_failure: bool,
    },

    /// Manage BYOK provider keys in ~/.stella/credentials.toml
    ///
    /// Manage BYOK provider keys stored in
    /// ~/.stella/credentials.toml (set/remove/list) — keys resolved
    /// via an env var or settings.json still take precedence per the normal
    /// chain; `stella models`/`stella config` show which source actually
    /// wins. Never prints a secret value; needs no model API key itself.
    Auth {
        #[command(subcommand)]
        cmd: AuthCmd,
    },

    /// Print a shell completion script for stella to stdout.
    ///
    /// Install it the way your shell expects, e.g.
    ///
    ///     bash: stella completions bash > /etc/bash_completion.d/stella
    ///
    ///     zsh:  stella completions zsh  > "${fpath[1]}/_stella"
    ///
    ///     fish: stella completions fish > ~/.config/fish/completions/stella.fish
    ///
    /// Offline, writes nothing, and needs no API key.
    Completions {
        /// bash | zsh | fish | powershell | elvish
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Print the version and exit
    Version,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub(crate) enum TelemetryCmd {
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

        /// Store one companion value a provider needs beyond its key, as
        /// NAME=VALUE, repeatable. Only Bedrock has any today:
        /// AWS_SECRET_ACCESS_KEY, AWS_SESSION_TOKEN, AWS_REGION. Omit these
        /// for an interactive masked prompt covering each one. WARNING: like
        /// --key, a value passed here is visible in shell history and `ps`.
        #[arg(long = "field", value_name = "NAME=VALUE")]
        fields: Vec<String>,
    },

    /// Remove a provider's stored key (and companion values) from
    /// credentials.toml.
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
pub(crate) enum ModelsCmd {
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

/// `--turn-budget` must be a positive, finite number of seconds.
///
/// Same reasoning as `parse_budget`, one step stronger: a zero or negative
/// budget would make every continuation unaffordable and silently disable the
/// recovery path entirely, which looks exactly like the truncation bug it
/// exists to mitigate. Refusing at parse is the only place that is cheap to
/// notice.
fn parse_turn_budget(raw: &str) -> Result<std::time::Duration, String> {
    let value: f64 = raw
        .parse()
        .map_err(|_| format!("`{raw}` is not a number"))?;
    if !value.is_finite() || value <= 0.0 {
        return Err(format!(
            "turn budget must be a positive number of seconds, got `{raw}`"
        ));
    }
    Ok(std::time::Duration::from_secs_f64(value))
}

/// `--max-output-tokens`: a positive step ceiling, refused rather than
/// reinterpreted when it is anything else.
///
/// Zero is rejected by name instead of being read as "no limit". Elsewhere in
/// this engine `0` does spell "no ceiling" (`model_timeout_secs`), but here
/// the flag's ABSENCE already means "the model's own maximum", so zero has no
/// second meaning left to carry — and taking it literally would ask the
/// provider for an empty completion on every step, which fails a run in a way
/// that looks like the model refusing to work.
fn parse_max_output_tokens(raw: &str) -> Result<u32, String> {
    let value: u32 = raw
        .trim()
        .parse()
        .map_err(|_| format!("`{raw}` is not a whole number of output tokens"))?;
    if value == 0 {
        return Err(
            "max output tokens must be greater than 0 — omit the flag to use \
             the model's own maximum"
                .to_string(),
        );
    }
    Ok(value)
}
