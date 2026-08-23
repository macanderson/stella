// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The session-wide flag surface — [`GlobalArgs`], the struct `Cli` flattens.
//!
//! A sibling of `cli.rs` rather than more lines in it, for the reason
//! AGENTS.md § "God files" splits everything: the parent sat three lines
//! under the 1500-line ratchet when #4543 added a flag to two subcommands.
//! The parse helpers the fields name stay in `cli.rs` beside the subcommand
//! flags that also use them; this child module reaches them through `super::`.

use super::{parse_env_flag, parse_max_output_tokens, parse_spend_limit, parse_turn_timeout};
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
// naturally type it (`stella fleet … --spend-limit 5` dies with "unexpected
// argument"). `global = true` registers the flag with every subcommand,
// making both positions valid. The invariant is machine-enforced by
// `every_root_flag_is_global` in src/tests.rs — a new field without the
// attribute fails the suite, not a user's shell.
//
// The names here are reserved CLI-wide: a subcommand flag reusing one
// does not shadow cleanly — clap propagates the global's value slot into
// every subcommand, and the id collision panics at match time in debug
// builds and misbinds in release. `no_subcommand_flag_reuses_a_global_name`
// in src/tests.rs enforces uniqueness.
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

    /// Pin a gateway to these upstreams, in order; refuse any fallback
    ///
    /// Only a gateway routes to somebody else's silicon, so this applies to
    /// OpenRouter (`provider.order` + `allow_fallbacks: false`) and is inert
    /// on a direct endpoint. Repeat the flag, or comma-separate, to express a
    /// preference order: `--upstream-pin z-ai --upstream-pin anthropic`.
    ///
    /// Reach for it when two runs must be comparable. The gateway otherwise
    /// picks per app identity and may serve the same slug from a different
    /// vendor between runs — which silently varies the model provider a
    /// head-to-head claims to hold fixed. A flag rather than settings-only
    /// because the benchmark harness runs with settings isolation
    /// (`STELLA_NO_SETTINGS`), so an argument is the only authority that
    /// reaches a measured trial — the same reason `--base-url` is one.
    #[arg(
        long,
        global = true,
        env = "STELLA_UPSTREAM_PIN",
        value_delimiter = ',',
        hide_short_help = true
    )]
    pub(crate) upstream_pin: Vec<String>,

    /// Extra directory a write tool may touch, outside the workspace root
    ///
    /// Repeat the flag, or comma-separate, to name several:
    /// `--allow-dir /srv/shared --allow-dir ../vendor`. A relative path
    /// resolves against the workspace root, so it means the same directory
    /// however the run was launched.
    ///
    /// ADDITIVE to `stella.toml`'s `[workspace] allowed_dirs` — this widens
    /// the project's list for one invocation and never replaces it, so an
    /// operator reaching for one directory cannot silently revoke the rest.
    /// A flag as well as a settings key for the same reason as
    /// `--upstream-pin`: a run under settings isolation
    /// (`STELLA_NO_SETTINGS`) has no other way to be granted one.
    #[arg(
        long,
        global = true,
        env = "STELLA_ALLOW_DIR",
        value_delimiter = ',',
        hide_short_help = true
    )]
    pub(crate) allow_dir: Vec<String>,

    /// Hard USD spend limit for the whole invocation
    ///
    /// Scoped to the invocation, not the turn: enforced mode aborts cleanly
    /// (never mid-tool) once cumulative spend across every turn and goal round
    /// exceeds this. `stella self-driving drive` honours it the same way,
    /// across every turn it spawns — triage, work and retry alike — and stops
    /// before starting one it cannot pay for, reporting *budget reached*
    /// rather than *finished*. Omit to meter spend for the cost summary
    /// without ever blocking (observed mode).
    #[arg(long, global = true, env = "STELLA_SPEND_LIMIT", value_parser = parse_spend_limit)]
    pub(crate) spend_limit: Option<f64>,

    /// Wall-clock seconds one turn may spend before it wraps up
    ///
    /// The time twin of `--spend-limit`, for callers running under an
    /// EXTERNAL deadline — a benchmark harness that kills the process on
    /// elapsed time. Two mechanisms arm from it: the engine declines to
    /// start an output-limit continuation it cannot finish (ending with a
    /// truthful partial instead of being destroyed mid-flight), and a
    /// one-shot `run` also stops at the next safe boundary once the
    /// allowance is spent, so the work on disk is scored rather than
    /// discarded with the kill (#1503). Interactive sessions (chat, the
    /// deck) treat it as advisory only. Set it slightly below the real
    /// deadline. Omit for no time-based limit. Env: STELLA_TURN_TIMEOUT.
    #[arg(long, global = true, env = "STELLA_TURN_TIMEOUT", value_parser = parse_turn_timeout)]
    pub(crate) turn_timeout: Option<std::time::Duration>,

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
    /// `--tools '*:off,task_list:on,get_environment:on'` is a read-only run;
    /// `--tools 'task_assign:off'` removes one capability.
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

    /// Own this terminal instead of surviving it
    ///
    /// A long-running verb started from a terminal (`run`, `goal`, `monitor`,
    /// `fleet`) is normally handed to a supervisor: the work becomes a
    /// detached process that keeps going when the window closes, and this
    /// terminal only streams it. `stella daemon list` finds it afterwards;
    /// `stella daemon attach <id>` picks the stream back up.
    ///
    /// This flag runs the work in this process instead, exactly as older
    /// releases did — closing the terminal kills it, and in exchange the run
    /// can ask you to approve an expanded scope, which a supervised run has
    /// nobody to ask. Invocations with no terminal to lose (a pipe, CI, a
    /// container) are never supervised and are unaffected either way.
    /// Env: STELLA_FOREGROUND.
    // `action` and `value_parser` are spelled out because the inferred `bool`
    // parser accepts only the literals `true`/`false` — and the supervisor
    // tells its child to do the work through `STELLA_FOREGROUND=1`
    // (`daemon::launch`), so under the inferred parser every supervised child
    // died in argument parsing before it ran a turn (#2142).
    #[arg(
        long,
        global = true,
        env = "STELLA_FOREGROUND",
        hide_short_help = true,
        action = clap::ArgAction::SetTrue,
        value_parser = parse_env_flag,
    )]
    pub(crate) foreground: bool,

    /// Start the run in the background and return to the shell immediately.
    ///
    /// The work is supervised exactly as it would be otherwise — same session
    /// id, same console, `stella daemon list | attach | logs | stop` all
    /// address it — but this process does not stay to stream it. Use it to
    /// launch a long run from a script, or to keep using the terminal you
    /// started one in. Unlike plain supervision this does not need a terminal,
    /// so it works from a pipe, a container, or CI.
    ///
    /// The exit code is the launch's, not the run's: the run's own answer is
    /// in `stella daemon list` afterwards. `--foreground` wins if both are
    /// given. Env: STELLA_DETACH.
    // Same parser as `foreground`, same reason (#2142): `export
    // STELLA_DETACH=1` is the conventional shell spelling, and the inferred
    // parser refuses it.
    #[arg(
        long,
        global = true,
        env = "STELLA_DETACH",
        hide_short_help = true,
        action = clap::ArgAction::SetTrue,
        value_parser = parse_env_flag,
    )]
    pub(crate) detach: bool,
}
