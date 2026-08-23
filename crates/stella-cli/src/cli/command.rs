//! The top-level `stella <subcommand>` enum.
//!
//! Split out of `cli.rs` when that file crowded the 1500-line ceiling
//! (#3776) — a pure move, the enum's own doc comments (the `stella --help`
//! summaries and `long_about` text) unchanged. `use super::*` carries over
//! everything `cli.rs` already had in scope, including the sibling
//! `MigrateCmd`/`AuthCmd`/`McpCmd` enums this one's variants name.

use super::*;

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
