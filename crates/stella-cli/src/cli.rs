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
    OutputFormat, build_info, commands_cmd, context_cmd, dataset_cmd, fleet_verbs, ingest_cmd,
    inspect, memory_cmd, plugin_cmd, proposals_cmd, query_format, self_driving_cmd, stats,
    storage_cmd, tune_cmd, usage_cmd,
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

// The session-wide flag surface, split out beside `help` when #4543 pushed
// this file against the 1500-line ratchet; the maintainer notes about
// `global = true` and name reservation moved with it (`cli/globals.rs`).
mod globals;
pub(crate) use globals::GlobalArgs;

#[derive(Subcommand)]
pub(crate) enum DaemonCmd {
    /// List supervised runs on this machine
    List,

    /// Stream a supervised run's output into this terminal
    ///
    /// Picks the stream up live and stays until the run ends; a run that has
    /// already finished prints in full and exits. Detaching again (Ctrl-C)
    /// leaves the run alone — `stella daemon stop` is what stops it.
    Attach {
        /// Run to attach to. A unique prefix of the id, or the run's pid,
        /// is enough. Omitted: the most recently started supervised run.
        id: Option<String>,
    },

    /// Print the tail of a supervised run's output and exit
    Logs {
        /// Run to read. A unique prefix of the id, or the run's pid, is
        /// enough. Omitted: the most recently started supervised run.
        id: Option<String>,

        /// How many lines back to start from.
        #[arg(short = 'n', long, default_value_t = 40)]
        lines: usize,
    },

    /// Stop a supervised run
    ///
    /// Asks it to stop the way Ctrl-C would — the engine finishes the tool it
    /// is running and aborts at the next safe boundary, never mid-tool. A run
    /// that has not stopped after the grace period is killed, and either way
    /// the stop is recorded as deliberate rather than left to read as a crash.
    Stop {
        /// Run to stop. A unique prefix of the id, or the run's pid, is
        /// enough.
        id: String,
    },

    /// Resume a killed supervised run from its last step boundary
    ///
    /// A supervised run killed mid-turn — OOM, `kill -9`, a reboot — leaves a
    /// resume point at its last completed step. Resume relaunches the same
    /// session and continues that turn from the boundary: completed steps are
    /// already in its transcript and are not re-run, so no tool effect is
    /// applied twice. A run that ended cleanly discarded its resume point on
    /// the way out, and resume says so instead of restarting it.
    Resume {
        /// Run to resume. A unique prefix of the id, or the run's pid, is
        /// enough. Omitted: the most recently started supervised run.
        id: Option<String>,
    },

    /// Resume every run this machine interrupted, once
    ///
    /// The verb `stella daemon install --resume-all` registers, and a useful
    /// one by hand after an unplanned reboot. It continues each supervised run
    /// whose process was killed mid-turn and which left a resume point, from
    /// that run's last completed step — never restarting one, and never
    /// touching a run that finished, was stopped, or was set aside. Each run
    /// gets at most three boot-time resumes before it is retired from the
    /// sweep and left for `stella daemon resume <id>` by hand.
    ///
    /// The sweep is sequential, so each resumed run is also bounded by a
    /// wall-clock ceiling: a turn that never ends — a wedged tool, a provider
    /// that never returns — is stopped gracefully at the ceiling (asked
    /// first, killed only past the grace period, never mid-tool) so the runs
    /// behind it still get their resume.
    ResumeAll {
        /// Print the decision for every run and exit without resuming
        /// anything — no process spawned, no budget spent.
        #[arg(long)]
        dry_run: bool,

        /// Minutes one resumed run may take before the sweep stops it
        /// gracefully and moves on. `stella daemon resume <id>` by hand has
        /// no ceiling.
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
        ceiling: u64,
    },

    /// Register a stella invocation with the OS's per-user service manager
    ///
    /// Supervision survives the terminal; only the service manager survives
    /// logout and reboot. This writes a per-user definition — a launchd agent
    /// on macOS, a `systemd --user` unit on Linux — that starts the given
    /// invocation in the current directory at every login (macOS) or boot
    /// (Linux, once lingering is on), and loads it now.
    ///
    /// A service manager re-starts what it registers, so a registered command
    /// begins a fresh turn and a fresh spend each boot: register standing
    /// verbs (`monitor`, a fleet watch) — a one-shot `run` would repeat its
    /// work, and its spend, on every boot. By default a command that exits
    /// stays down; `--keep-alive` opts into restarts. To come back
    /// *continuing* the work a reboot interrupted rather than repeating it,
    /// register the resume sweep with `--resume-all`.
    Install {
        /// Name for the service (default: the command's first word, or
        /// `resume-boot` with --resume-all). Lowercase letters, digits and
        /// dashes.
        #[arg(long)]
        label: Option<String>,

        /// Restart the command whenever it exits, at most once a minute.
        /// Off by default: an agent that fails the moment it starts would
        /// otherwise be restarted forever, spending budget each time.
        #[arg(long)]
        keep_alive: bool,

        /// Register `stella daemon resume-all` instead of a command of your
        /// own: at every boot, continue the turns this machine interrupted
        /// from their last completed step rather than starting them over.
        /// Cannot be combined with a command or with --keep-alive.
        #[arg(long)]
        resume_all: bool,

        /// The stella invocation to register, after `--`:
        /// `stella daemon install -- monitor --interval 300`
        #[arg(last = true, value_name = "STELLA ARGS")]
        command: Vec<String>,
    },

    /// Remove a service registered by `install`
    ///
    /// Unloads it from the service manager and deletes the definition.
    /// Already-gone is reported and succeeds — uninstalling twice is the
    /// requested state, not an error.
    Uninstall {
        /// The label `install` reported.
        label: String,
    },
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

        /// Deprecated, does nothing (#3381). The raw step-loop is the default
        /// now; pass `--pipeline <variant>` to opt into a wrapper instead.
        /// Kept parseable so no script breaks the day this changed — passing
        /// it prints a one-line notice and has no other effect, including
        /// alongside `--pipeline`, which always wins.
        #[arg(long, hide = true)]
        no_pipeline: bool,

        /// Run this turn under an installed wrapper plugin, by the `[wrapper]
        /// id` its manifest declares (`stella plugin list`). The plugin
        /// contributes context before the turn, gathers evidence after it, and
        /// its declared rule — evaluated by Stella, never by the plugin —
        /// decides whether another turn runs. The id is recorded on the
        /// execution row, so two variants can be compared. Omitted, the raw
        /// step-loop runs with nothing over it (the default since #3381).
        /// `classic` named the built-in staged pipeline and is refused
        /// outright: that pipeline was deleted from the workspace (#3865), and
        /// the refusal names `stella plugin install` as the remedy.
        ///
        /// Several ids separated by commas — `--pipeline research-v1,plan-v1`
        /// — run as one composed selection, in the order given: the selection
        /// states the order because no manifest vocabulary does (#3801).
        #[arg(long, value_name = "VARIANT")]
        pipeline: Option<String>,

        /// Test command an installed wrapper plugin's own `[oracle]` runs
        /// deterministically (e.g. "cargo test -p my-crate"). Arms that
        /// plugin's fail→pass flip check: a change that flips a failing test
        /// to passing is proven done, and the plugin reports the evidence.
        /// Refused on the raw loop — nothing there consumes it (#3696), and
        /// since #3865 there is no built-in stage left to hand it to; pass
        /// `--pipeline <variant>` naming an installed plugin instead.
        #[arg(long, value_name = "CMD")]
        test_command: Option<String>,

        /// Keep the authored witness test as a file in your working tree.
        /// The witness was scaffolding: it proved a run's goal inside the
        /// candidate workspace and was discarded with it, so an
        /// already-satisfied test was never left behind in your test tree,
        /// and this promoted it to a real test you could commit.
        ///
        /// Refused unconditionally since #3865 deleted the staged pipeline
        /// that authored the witness — there is nothing on any resolution for
        /// it to keep. A verification plugin owns its own witness files;
        /// install one (`stella plugin install`) and ask it.
        #[arg(long)]
        keep_witness: bool,

        /// Exit non-zero unless the run was actually verified.
        ///
        /// Refused unconditionally since #3865. It turned "completed but
        /// unproven" into a failure exactly like a refuted verification, and
        /// both of those were verdicts the built-in staged pipeline reached;
        /// the raw loop reaches neither, so there is no standing for this flag
        /// to read. A run's JSON `status` is `completed` or `aborted` (see
        /// `agent::summary`) — a delivery gate that must not ship unproven
        /// work wants a verification plugin (`stella plugin install`) and
        /// `--pipeline <variant>` naming it.
        #[arg(long)]
        require_verified: bool,

        /// Exit non-zero unless the `--pipeline` wrapper declared its
        /// requirements met (#3554). Refused without `--pipeline`, where
        /// nothing declares a verdict; see `wrapper_plugin::verdict_gate` for
        /// why it is opt-in.
        #[arg(long)]
        require_verdict: bool,

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

        /// Deprecated, does nothing (#3381). The raw step-loop is the default
        /// now — this flag named that fallback before the flip and is kept
        /// parseable only so no script breaks; passing it prints a one-line
        /// notice and has no other effect.
        #[arg(long, hide = true)]
        no_pipeline: bool,

        /// Run each episode under a wrapper, by the `[wrapper] id` its
        /// manifest declares (`stella plugin list`). `classic` names the
        /// built-in staged pipeline; omitted, the raw step-loop runs with
        /// nothing over it (the default since #3381). The same flag
        /// [`stella run`](crate::cli::Command::Run) takes, so a panel can
        /// measure either driver rather than only the one the default
        /// happens to name — comma-separated ids included.
        #[arg(long, value_name = "VARIANT")]
        pipeline: Option<String>,

        /// Test command for the pipeline's deterministic verify ladder.
        ///
        /// Belongs to the staged pipeline's verify machinery, so it is
        /// refused rather than silently ignored unless `--pipeline` selects
        /// a driver that can honor it.
        #[arg(long, value_name = "CMD")]
        test_command: Option<String>,
    },

    /// Work in judged rounds until a verifier says the goal is met
    ///
    /// Work in judged rounds until a verifier model confirms the goal is met.
    /// Each working round runs the raw step-loop by default (#3381); pass
    /// `--pipeline classic` to route rounds through the staged pipeline
    /// (triage, plan, witness, execute, verify) instead.
    Goal {
        /// What must be true when done — assessed by the verifier each round.
        /// Omit it to read the goal from stdin when it is piped, or pass `-`
        /// to read stdin explicitly.
        goal: Option<String>,

        /// Deprecated, does nothing (#3381). The raw step-loop is the default
        /// now; pass `--pipeline <variant>` to opt into a wrapper instead.
        /// Kept parseable so no script breaks the day this changed — passing
        /// it prints a one-line notice and has no other effect, including
        /// alongside `--pipeline`, which always wins.
        #[arg(long, hide = true)]
        no_pipeline: bool,

        /// Run each round's working turn through an installed wrapper
        /// plugin, by the `[wrapper] id` its manifest declares (`stella
        /// plugin list`). `classic` names the built-in staged pipeline
        /// (triage → recall → plan → witness → execute → verify); omitted,
        /// the raw step-loop runs with nothing over it (the default since
        /// #3381). A named plugin variant dispatches every round's worker
        /// turn through the wrapper; the goal verifier that decides met/unmet
        /// is unchanged either way. Comma-separated ids run as one composed
        /// selection, in the order given.
        #[arg(long, value_name = "VARIANT")]
        pipeline: Option<String>,

        /// The oracle a wrapper plugin's `[oracle]` observes the flip with.
        ///
        /// One command for the whole goal run, not per round: it names the
        /// witness the run is judged against, and a witness that changed
        /// between rounds is the tampering the host is watching for. It
        /// crosses the host's closed runner vocabulary before reaching a
        /// plugin, and the artifacts it names are pinned once before the
        /// first round. Refused rather than silently ignored unless
        /// `--pipeline` names a wrapper that can honor it (#3835).
        #[arg(long, value_name = "CMD")]
        test_command: Option<String>,

        /// Exit non-zero unless the `--pipeline` wrapper declared its
        /// requirements met (#3554, on this door #4543). The LAST round's
        /// verdict decides — the round whose work ships — because every
        /// earlier round's refusal was superseded by another round being
        /// driven. A goal the verifier left unmet already fails on its own,
        /// ahead of this gate. Refused without `--pipeline`, where nothing
        /// declares a verdict; see `wrapper_plugin::verdict_gate` for why it
        /// is opt-in.
        #[arg(long)]
        require_verdict: bool,
    },

    /// Watch CI for a branch or PR and fix it until green
    Monitor {
        /// Branch name or PR number (default: main)
        target: Option<String>,
    },

    /// Drive the perpetual delivery loop: plan, cycle, audit, watch
    ///
    /// The deterministic half of self-driving — the loop that fixes a batch of
    /// defects, audits what is left, files what it cannot fix, benchmarks,
    /// ships, and repeats. These verbs are the machine-decidable controls:
    /// the governor that sizes a cycle to this machine (plan), the ledger and
    /// its folds (state, metrics, run), the audit lens ladder and the dedup
    /// oracle that advance it (aperture, seen, cycle), and the low-duty
    /// sentinel (watch). The judgement half stays with the model driving the
    /// loop; `scripts/self-driving.sh` delegates these verbs here. Offline except
    /// for `gh` reads of the defect queue; needs no API key.
    SelfDriving {
        #[command(subcommand)]
        cmd: self_driving_cmd::SelfDrivingCmd,
    },

    /// Start an interactive session (the Command Deck)
    Chat,

    /// Reopen a previous session exactly where it stood
    ///
    /// Reopen a previous session exactly where it stood — transcript,
    /// conversation, pending prompts. Sessions are durable (quit, crash, and
    /// power loss included); the deck's SESSIONS overlay (`ctrl-e`, `⏎` on a
    /// row) is the same navigation from inside a session.
    Resume {
        /// Registry id (`ses-…`) of the session to reopen. Omitted: the most
        /// recently active resumable session of this workspace.
        id: Option<String>,

        /// List this machine's sessions (resumable ones marked) and exit.
        #[arg(long)]
        list: bool,
    },

    /// Find, watch, and stop runs that outlived their terminal
    ///
    /// A long-running verb started from a terminal is handed to a supervisor:
    /// the work runs as a detached process that survives the window closing,
    /// an `ssh` disconnect, and a logout. These are the commands for finding
    /// one again afterwards. `--foreground` on the original invocation opts
    /// out; an invocation with no terminal (a pipe, CI, a container) is never
    /// supervised and never appears here.
    ///
    /// Supervision survives the terminal, not the process: a supervised run
    /// that is killed loses its turn, and only the fact of it is recorded.
    Daemon {
        #[command(subcommand)]
        cmd: DaemonCmd,
    },

    /// Analyze this workspace: domain taxonomy and code graph
    ///
    /// Analyze this workspace: infer its domain taxonomy
    /// (.stella/domains.toml) — the tagging vocabulary for memories,
    /// reflections, and every code-graph node/edge — and build the
    /// code-graph index (.stella/private/codegraph.db).
    ///
    /// Incremental re-runs happen automatically: unchanged files are
    /// skipped by content hash, every session start catches the index up in
    /// the background, a live watcher follows edits for the rest of the
    /// session, and graph-backed tools run the same catch-up when they open
    /// the index — so you rarely need to re-run this command for the index's
    /// sake. The domain taxonomy is the part only `stella init` refreshes,
    /// and it re-infers (a model call) only when the repository's shape has
    /// changed; otherwise the existing .stella/domains.toml is reused at no
    /// cost. Delete that file to force re-inference.
    Init {
        /// Delete semantic vectors left behind by an embedding model this
        /// workspace no longer uses, reclaiming their disk space.
        ///
        /// Vectors are keyed by embedder fingerprint so two models never
        /// share a vector space — which means switching model hides the old
        /// vectors rather than removing them, and switching back reuses them
        /// at no cost. That is why this is a flag and not automatic: sweeping
        /// on every init would bill a full re-embed to anyone comparing two
        /// models. Without it, `stella init` only *reports* what is stranded.
        #[arg(long)]
        prune_vectors: bool,
    },

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

        /// Adopt a staged tool: move it into .stella/tools/ and run its
        /// capability witness — the call must FAIL on the existing tool
        /// surface and PASS with the new tool, producing a real value.
        /// Records the proof. Does NOT make the tool usable.
        #[arg(long, value_name = "NAME", conflicts_with = "validate")]
        adopt: Option<String>,

        /// Enable an adopted tool — the one approval in the protocol a
        /// machine never grants itself. Refused if the tool's bytes changed
        /// since its witness ran.
        #[arg(long, value_name = "NAME", conflicts_with_all = ["validate", "adopt"])]
        enable: Option<String>,

        /// Stop offering an adopted tool, keeping its proof on file
        #[arg(
            long,
            value_name = "NAME",
            conflicts_with_all = ["validate", "adopt", "enable"]
        )]
        disable: Option<String>,

        /// Report every self-authored tool: its witness, whether it is
        /// enabled, and how often it has actually been reused since adoption
        /// (with the never-used ones named as the cost)
        #[arg(
            long,
            conflicts_with_all = ["validate", "adopt", "enable", "disable"]
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
    /// branch is left in place for review — until `stella fleet clean`
    /// reclaims the finished ones.
    ///
    /// A prompt whose first word is exactly a subcommand name (`clean`,
    /// `claims`) is read as that subcommand; pass the prompts after `--`
    /// when that bites.
    #[command(subcommand_negates_reqs = true)]
    Fleet {
        /// Verbs (`clean`, `claims`) — omit to run a fan-out
        #[command(subcommand)]
        cmd: Option<fleet_verbs::FleetCmd>,

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

        /// Deprecated, does nothing (#3381). The raw step-loop is the default
        /// now; pass `--pipeline <variant>` to opt into a wrapper instead.
        /// Kept parseable so no script breaks the day this changed — passing
        /// it prints a one-line notice and has no other effect, including
        /// alongside `--pipeline`, which always wins.
        #[arg(long, hide = true)]
        no_pipeline: bool,

        /// Run each worker through an installed wrapper plugin, by the
        /// `[wrapper] id` its manifest declares (`stella plugin list`). The
        /// wrapper is bound once per worker attempt, in that attempt's own
        /// tree, and every attempt's execution row records its variant id
        /// (#3695). Omitted, the raw step-loop runs with nothing over it (the
        /// default since #3381); `classic` named the built-in staged pipeline
        /// and is refused now that it is deleted (#3865).
        #[arg(long, value_name = "VARIANT")]
        pipeline: Option<String>,

        /// Wall-clock ceiling per worker attempt, in seconds. On expiry the
        /// task's stop line fires (the same clean cancel the dashboard's
        /// `[x]` sends) and the attempt reports as failed instead of
        /// occupying its concurrency slot forever — the only way to unstick
        /// a hung worker on a piped or CI run. Unset = unbounded.
        #[arg(long, value_name = "SECS")]
        task_timeout: Option<u64>,

        /// Fail any attempt whose `--pipeline` wrapper did not declare its
        /// requirements met (#3554, on this door #4543). PER ATTEMPT: an
        /// unmet or undecided verdict fails that attempt by name, and a
        /// failed attempt fails the run — the rule every failed task already
        /// follows. The attempt's own abort wins when both fire. Refused
        /// without `--pipeline`, where nothing declares a verdict; see
        /// `wrapper_plugin::verdict_gate` for why it is opt-in.
        #[arg(long)]
        require_verdict: bool,

        /// Output shape: text, json, or stream-json
        ///
        /// Anything but `text` keeps the live grid off and the run headless,
        /// with machine-readable error envelopes and no interactive credential
        /// prompts. Declared per-command, not globally, so it exists exactly
        /// where it is honoured (#1493).
        #[arg(long, env = "STELLA_OUTPUT_FORMAT", value_enum, default_value = "text")]
        output_format: OutputFormat,
    },

    /// Find code — semantic and structural search over the workspace
    ///
    /// Describe what you are looking for — a question, a behaviour, or a
    /// symbol/file name — and get back the files that answer it, ranked by
    /// meaning when an embedder is configured and falling back to graph
    /// symbol-name matching and then a file scan otherwise. Reads/builds
    /// .stella/private/codegraph.db; ranking is offline, embedding (when
    /// configured) is a network write-through into that same index.
    Search {
        /// What you are looking for — a question, a description of the
        /// behaviour, or a symbol/file name
        query: String,

        /// Output format: text (the answer as the agent reads it), or json
        /// under the versioned query envelope — for scripted testing
        #[arg(long, value_enum, default_value = "text")]
        format: query_format::QueryFormat,
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

    /// Install, list, and remove plugins
    ///
    /// A plugin declares its say in the turn loop — a participation grade,
    /// the hook points it may act at, the process it runs as, and the exact
    /// environment slice that process inherits. `install` shows the whole
    /// declaration and installs nothing until you accept it; `remove` deletes
    /// it, and its hooks stop being routed immediately. Offline: reads and
    /// writes local files, needs no API key.
    Plugin {
        #[command(subcommand)]
        cmd: plugin_cmd::PluginCmd,
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

    /// Curate a redacted training dataset from workspace receipts
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

        /// Output format: text, or json under the versioned query envelope
        #[arg(long, value_enum, default_value = "text")]
        format: query_format::QueryFormat,

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

    /// Pass calibration: false-positive rate vs CI and reverts
    ///
    /// Fold every recorded session's pass verdicts against the evidence
    /// observed after them (#871, #1293): how often did an unproven PASS —
    /// and, as the comparison cohort, a deterministic ladder pass — later
    /// fail CI, or get reverted by a human? Rates are reported as unmeasured
    /// until ground truth exists, and a workspace that recorded no verdict at
    /// all says so rather than reporting zeroes. Reads
    /// .stella/private/store.db and the git log only; needs no API key and
    /// never writes.
    Calibration {
        /// Output format: text, or json under the versioned query envelope
        #[arg(long, value_enum, default_value = "text")]
        format: query_format::QueryFormat,
    },

    /// Cost, tokens, and resolve rate per provider and model
    ///
    /// Summarize cost, tokens, and resolve rate per provider/model from
    /// local telemetry (.stella/private/store.db) — $/resolved-task receipts.
    /// `stella stats prune` bounds that store's growth
    Stats {
        /// Output format: text (aligned table with TOTAL row), json under
        /// the versioned query envelope, or csv
        #[arg(long, value_enum, default_value = "text")]
        format: query_format::StatsFormat,

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

/// `--foreground` / `--detach` through their environment variables
/// (`STELLA_FOREGROUND`, `STELLA_DETACH`).
///
/// clap's inferred `bool` parser accepts only the literals `true`/`false`,
/// and the environment is exactly where other spellings arrive: the
/// supervisor's own child telegram is `STELLA_FOREGROUND=1`
/// (`daemon::launch`), and `export STELLA_DETACH=1` is the convention a
/// shell user reaches for. Under the inferred parser each of those died in
/// argument parsing — for the supervised child, before it ran a turn, which
/// made every supervised run dead on arrival (#2142). Two asymmetries are
/// deliberate: empty reads as unset rather than erroring, because `export
/// STELLA_FOREGROUND=` is a shell's way of saying "not set"; anything
/// unrecognized is still refused by name, never silently read as true.
fn parse_env_flag(raw: &str) -> Result<bool, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "t" | "yes" | "y" | "on" => Ok(true),
        "" | "0" | "false" | "f" | "no" | "n" | "off" => Ok(false),
        _ => Err(format!(
            "`{raw}` is not a boolean — use 1/0, true/false, yes/no, or on/off"
        )),
    }
}

/// `--spend-limit` must be a positive, finite dollar amount — a NaN or
/// negative limit would make every comparison silently false and turn the
/// "hard cap" into a no-op, the worst failure mode for a money control.
fn parse_spend_limit(raw: &str) -> Result<f64, String> {
    let value: f64 = raw
        .parse()
        .map_err(|_| format!("`{raw}` is not a number"))?;
    if !value.is_finite() || value <= 0.0 {
        return Err(format!(
            "spend limit must be a positive dollar amount, got `{raw}`"
        ));
    }
    Ok(value)
}

/// `--turn-timeout` must be a positive, finite number of seconds.
///
/// Same reasoning as `parse_spend_limit`, one step stronger: a zero or
/// negative timeout would make every continuation unaffordable and silently
/// disable the recovery path entirely, which looks exactly like the
/// truncation bug it exists to mitigate. Refusing at parse is the only place
/// that is cheap to notice.
fn parse_turn_timeout(raw: &str) -> Result<std::time::Duration, String> {
    let value: f64 = raw
        .parse()
        .map_err(|_| format!("`{raw}` is not a number"))?;
    if !value.is_finite() || value <= 0.0 {
        return Err(format!(
            "turn timeout must be a positive number of seconds, got `{raw}`"
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
