// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The leaf subcommand enums `Command`'s variants delegate to
//! (`stella daemon <verb>`, `stella auth <verb>`, …) and the `value_parser`
//! functions `cli.rs`'s flags reference.
//!
//! Split out of `cli.rs` rather than added to it because that file sits close
//! to the 1500-line ratchet: a pure move, re-exported from `cli` so every
//! existing `crate::DaemonCmd`-style path keeps resolving unchanged.

use clap::Subcommand;

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
pub(crate) fn parse_env_flag(raw: &str) -> Result<bool, String> {
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
pub(crate) fn parse_spend_limit(raw: &str) -> Result<f64, String> {
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
pub(crate) fn parse_turn_timeout(raw: &str) -> Result<std::time::Duration, String> {
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
pub(crate) fn parse_max_output_tokens(raw: &str) -> Result<u32, String> {
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
