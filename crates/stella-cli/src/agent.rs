//! The agent loop — ties providers, tools, the step-driver, and TUI
//! together.
//!
//! `run_turn` drives `stella_core::Engine::run_turn` (the step-driver: one
//! model call per step, retry+backoff, compaction, loop detection, budget
//! checks — see `crates/stella-core/src/driver.rs`) and renders its
//! `AgentEvent` stream live via a spawned draining task.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use colored::Colorize;
use stella_core::ports::{Principal, ToolExecutor};
use stella_core::router::{CircuitBreaker, ProviderProfile};
use stella_core::{
    BudgetGuard, CalibrationMap, Engine, EngineConfig, GoalConfig, GoalOutcome, RoleTable, Router,
    TurnOutcome,
};
use stella_mcp::{McpConfig, McpServerConfig, McpToolSet};
use stella_model::credential::ApiKey;
use stella_model::provider::Provider;
use stella_protocol::{AgentEvent, CompletionMessage, ModelRef, Role, ToolOutput, UNKNOWN_MODEL};
use stella_store::{ContextBlockRow, ManifestBlockRow, StepManifestRow, Store, TelemetryRow};
use stella_tools::ToolRegistry;
use stella_tools::custom::{self, CustomTool};
use stella_tools::hook_runner::ShellHookRunner;
use stella_tools::validate;
use tokio::sync::mpsc;

use crate::domains::{Domains, heuristic_domains, infer_domains};
use crate::failure::CliFailure;
use crate::interactive::human_is_present;
use crate::memory::{
    ReflectionReport, SessionMemory, TurnEvidence, inject_recall_block, reflect_routed,
    should_reflect_on, turn_warrants_reflection,
};
use crate::plain::{self, accent};
use crate::runtime::{SystemClock, TokioSleeper};
use crate::{OutputFormat, config::Config};
use stella_context::EpisodeOutcome;

mod budget;
mod engine;
mod goal;
mod graph;
mod init;
pub(crate) mod outcome;
mod output;
pub(crate) mod persistence;
mod presence;
mod prompt;
mod reflect;
pub(crate) mod resume;
mod skill_usage;
mod summary;
pub(crate) mod tool_stack;
pub(crate) mod tools;
mod turn_close;
pub(crate) use budget::{build_budget_guard, remaining_budget, settle_reflection_budget};

pub(crate) use engine::*;
pub(crate) use goal::*;
pub(crate) use graph::spawn_session_graph;
#[cfg(test)]
use graph::{GraphSummary, format_graph_stats, index_workspace_graph_blocking};
pub use init::run_init;
pub(crate) use init::{InitIo, InitLine, deck_narrator, deck_notice_narrator, init_workspace};
pub(crate) use outcome::settled_cost_since;
use output::*;
pub(crate) use persistence::{
    PersistOutcome, begin_execution, close_event_stream, persist_event, persist_event_detailed,
    record_execution_end, seed_calibration, spawn_renderer, warn_store_write_failed,
};
pub(crate) use presence::SessionPresence;
pub(crate) use prompt::*;
use reflect::reflect_on_interactive_turn;
pub(crate) use reflect::surface_reflection;
pub(crate) use skill_usage::stamp_and_record_skill_usage;
pub(crate) use tools::*;

/// Whether this process may touch durable workspace state, as the
/// [`stella_runtime::Persistence`] switch the runtime crate takes explicitly.
///
/// This is the one place the ambient answer is read. Every runtime call site
/// below goes through it, so the process global has exactly one reader rather
/// than the seven it had before the extraction — and `stella-serve`, which has
/// no such global, simply passes its own per-session decision instead.
pub(crate) fn session_persistence() -> stella_runtime::Persistence {
    if crate::settings::filesystem_settings_disabled() {
        stella_runtime::Persistence::Disabled
    } else {
        stella_runtime::Persistence::Enabled
    }
}

/// Run a one-shot prompt. [`PipelineChoice`](crate::wrapper_plugin::PipelineChoice) selects which
/// wrapper runs over the turn (`Raw` by default, #3381). `test_command`, when given, arms a
/// bound wrapper plugin's own oracle.
///
/// `--keep-witness`/`--require-verified` used to reach this function too, back when a
/// `Classic` arm selected the built-in staged pipeline. That pipeline is gone
/// (#3865) and its variant with it (#3867), and `wrapper_plugin::reject_verification_flags_without_pipeline` now refuses both
/// flags unconditionally before a caller ever resolves a prompt, so nothing downstream of that
/// refusal has a use for them any more.
pub async fn run_one_shot(
    cfg: &Config,
    prompt: &str,
    budget_limit: Option<f64>,
    format: OutputFormat,
    pipeline: crate::wrapper_plugin::PipelineChoice<'_>,
    test_command: Option<&str>,
) -> Result<(), CliFailure> {
    // A benchmark's durable sink is part of the accounting boundary. Prove the exact mounted file
    // is writable before provider construction or any code path that can make a paid call.
    preflight_durable_stream(format)?;
    // #3381 made Raw the default, so this now admits the common no-flag `stella run` case too.
    crate::enterprise_telemetry::authorize_one_shot(!pipeline.is_raw())?;
    run_raw_one_shot(cfg, prompt, budget_limit, format, pipeline, test_command).await
}

/// The `--output-format json` summary of a raw (`--no-pipeline`) step-loop run.
/// A struct rather than `serde_json::json!` so the version leads the object
/// (`json!` builds a sorted map and would bury it mid-envelope); key order is
/// not contractual either way — this is for whoever reads the output by eye.
///
/// [`schema_version`](Self::schema_version) is governed by the bump rule on
/// [`crate::SUMMARY_SCHEMA_VERSION`].
#[derive(serde::Serialize)]
pub(crate) struct RawRunSummary {
    pub(crate) schema_version: u32,
    pub(crate) status: &'static str,
    pub(crate) text: Option<String>,
    pub(crate) cost_usd: Option<f64>,
    pub(crate) reason: Option<String>,
    pub(crate) model: String,
    pub(crate) events: Vec<AgentEvent>,
    /// The file-touch telemetry envelope. Filled from the turn's own measured
    /// `FileChange` events (`agent::summary::files_touched`) — the key stays
    /// even on a quiet turn because the envelope's key set is the versioned
    /// contract.
    pub(crate) files_touched: serde_json::Value,
}

/// The REPL's productized command names — reserved: a custom definition can
/// never run under one of these, argument-carrying forms included (the
/// exact-match handlers in the loop only claim the bare forms). Must cover
/// every `/`-command the loop below handles.
const REPL_RESERVED: &[&str] = &[
    "/exit", "/quit", "/models", "/config", "/help", "/clear", "/agents", "/init", "/rename",
    "/color", "/goal",
];

/// The usage line for an argument-requiring local command invoked bare (or
/// with only whitespace). These are reserved names, so `expand` never claims
/// them — without a local answer the bare form would fall through to a paid
/// model turn. `/goal`'s handler owns its own bare form the same way.
fn bare_local_command_usage(input: &str) -> Option<&'static str> {
    let (head, rest) = match input.split_once(char::is_whitespace) {
        Some((head, rest)) => (head, rest),
        None => (input, ""),
    };
    if !rest.trim().is_empty() {
        return None;
    }
    match head {
        "/rename" => Some("usage: /rename <name>"),
        "/color" => Some("usage: /color <name>"),
        _ => None,
    }
}

/// Run an interactive REPL session. `budget_limit` is per-session: the
/// `BudgetGuard`'s session-scoped total accumulates across every turn in
/// the conversation, while `BudgetGuard::begin_turn` resets only the
/// turn-scoped counter at the start of each one.
pub async fn run_interactive(cfg: &Config, budget_limit: Option<f64>) -> Result<(), String> {
    crate::enterprise_telemetry::authorize_execution_surface(
        crate::enterprise_telemetry::ExecutionSurface::Interactive,
    )?;
    let provider = build_provider(cfg)?;
    let registry: std::sync::Arc<ToolRegistry> =
        std::sync::Arc::new(crate::write_dirs::registry_for(cfg));
    let mcp = connect_mcp(
        cfg,
        registry.clone(),
        Some(registry.mcp_usage_ledger()),
        true,
    )
    .await?;

    crate::subagent::install_for_session(cfg, &registry)?;
    let ask = human_is_present(true);
    let active_rules =
        crate::rules::enforce_workspace_rules(&registry, &cfg.workspace_root, &cfg.authority, ask);
    // Auto-build the code-graph index in the background (a cheap incremental
    // refresh if it already exists) and keep it fresh via the live watcher, so
    // `stella search` and the deck's Graph tab have an index this session
    // without a manual `stella init`. Non-blocking; status goes to stderr so
    // it never disturbs the prompt. Kept alive for the whole REPL; the
    // watcher stops when it drops.
    let (_session_graph, _graph_build) = spawn_session_graph(
        &cfg.workspace_root,
        Box::new(init::stderr_narrator()),
        Box::new(|| {}),
    );
    let base_tools: &dyn ToolExecutor = match &mcp {
        Some(set) => set.as_ref(),
        None => &*registry,
    };
    let custom_tools = discover_custom_tools(cfg, true).await;
    let mut budget = build_budget_guard(budget_limit);
    let store = open_store(&cfg.workspace_root);
    // Session-scoped like `budget`: seeded once from prior sessions'
    // telemetry, then sharpened by every turn in this REPL.
    let calibration = seed_calibration(&store, cfg);
    // Session-scoped like `calibration`: every turn's engine reports its
    // call outcomes into this router's breaker (#2673) — the state a
    // resolution (and #2679's mid-turn fallback) reads to route around a
    // provider this session has watched fail.
    let router = session_router(cfg, &ModelRef::new(cfg.provider.id, cfg.model_id.clone()));

    plain::welcome_banner(
        cfg.provider.id,
        &cfg.model_id,
        &cfg.workspace_root.display().to_string(),
    );

    // Built once per session and reused verbatim on /clear — the byte-stable
    // prefix (instructions + baked memories + SessionStart hook context) is
    // the prompt-cache contract (see build_system_prompt).
    let system_prompt = with_session_hook_context(
        build_system_prompt(cfg, &cfg.workspace_root, &active_rules),
        cfg,
    )
    .await;
    let mut messages = vec![CompletionMessage::system(system_prompt.clone())];
    let mut memory =
        SessionMemory::open_for_session(&cfg.workspace_root, true, &cfg.authority, &active_rules);
    if let Some(m) = &mut memory {
        // Conformance-gated external CGP providers join before the first
        // recall, or are refused with a reason (#453).
        m.register_external_providers(|message| println!("  {} {message}", "!".yellow()))
            .await;
    }
    // Custom extensions: ⚡ commands/skills invocable as `/name args`, custom
    // agents behind `/agents`. Reloaded after `/init`, which may adopt new
    // ones from `.claude/`/`.agents/`. Load problems print up front so a
    // definition that failed to parse is visible, not silently absent.
    let mut custom = crate::extensions::CustomExtensions::load_with_authority(
        &cfg.workspace_root,
        &cfg.authority,
    );
    if let Some(report) = custom.problems_report() {
        for line in report.lines() {
            println!("  {line}");
        }
        println!();
    }

    // Machine-wide presence: the plain REPL registers like the deck does,
    // so its sessions are findable in every SESSIONS overlay and replayable
    // from their journals. No inbox notifications — the user is right here.
    let mut presence = SessionPresence::announce(cfg, "interactive session");

    loop {
        print!("{} ", ">".bright_cyan().bold());
        std::io::stdout().flush().map_err(|e| e.to_string())?;

        // Blocking stdin read off the async runtime's worker threads — the
        // user thinking at the prompt must not hold a worker hostage while
        // the session's graph watcher, MCP readers, and journal tasks share
        // that pool (matches `interactive::TtyAskUserIo::prompt`).
        let read = tokio::task::spawn_blocking(|| {
            let mut line = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut line)
                .map(|n| (n, line))
        })
        .await;
        let input = match read {
            Ok(Ok((0, _))) => break, // EOF (Ctrl+D)
            Ok(Ok((_, line))) => line,
            Ok(Err(e)) => return Err(format!("read error: {e}")),
            Err(e) => return Err(format!("read error: {e}")),
        };

        let input = input.trim();
        if input.is_empty() {
            continue;
        }
        if input == "/exit" || input == "/quit" || input == "exit" {
            break;
        }
        if input == "/models" || input == "/models list" {
            cfg.print_models();
            continue;
        }
        // `/models refresh` is handled model-free: when the configured model
        // itself is broken, the catalog re-sync is part of digging out —
        // routing it into a model turn would fail on the very error being
        // fixed. (Changing a model happens in the deck's SETTINGS tab, via
        // `--model`, or by editing settings.json — not through a command.)
        if input == "/models refresh" || input == "/models refresh --force" {
            println!();
            if let Err(e) = crate::model_catalog::run_refresh(input.ends_with("--force")).await {
                println!("  {} refresh failed: {e}", "✗".red());
            }
            println!();
            continue;
        }
        if input == "/config" {
            // The REPL fallback has no startup dotenv-load record handy —
            // the source label just degrades to the generic `env:VAR` form
            // (see `Config::print_config`'s doc).
            cfg.print_config(None);
            continue;
        }
        if input == "/help" {
            print_help();
            continue;
        }
        if input == "/clear" {
            messages = vec![CompletionMessage::system(system_prompt.clone())];
            println!("  {}\n", "conversation cleared".dimmed());
            continue;
        }
        if input == "/agents" {
            println!("  {}\n", custom.render_agent_list().replace('\n', "\n  "));
            continue;
        }
        if input == "/init" {
            println!();
            let mut io = init::InitIo::stdout_tty();
            match init_workspace(
                Some(&*provider),
                &cfg.workspace_root,
                Some(&cfg.model_id),
                remaining_budget(&budget),
                &mut io,
            )
            .await
            {
                Ok((_domains, _cost_usd)) => {
                    // Re-open memory so recall/reflection use the taxonomy
                    // `/init` just wrote — otherwise the cached domains stay
                    // stale until the next launch. The re-open carries the
                    // record channel with it, because the constructor is where
                    // the channel is attached.
                    memory = SessionMemory::open_for_session(
                        &cfg.workspace_root,
                        true,
                        &cfg.authority,
                        &active_rules,
                    );
                    // `/init` may also have adopted new custom
                    // commands/skills/agents — make them invocable now, and
                    // report anything that failed to load.
                    custom = crate::extensions::CustomExtensions::load_with_authority(
                        &cfg.workspace_root,
                        &cfg.authority,
                    );
                    if let Some(report) = custom.problems_report() {
                        for line in report.lines() {
                            println!("  {line}");
                        }
                    }
                }
                Err(e) => println!("  {} init failed: {e}", "✗".red()),
            }
            println!();
            continue;
        }
        if let Some(usage) = bare_local_command_usage(input) {
            println!("  {}\n", usage.dimmed());
            continue;
        }
        if let Some(title) = input.strip_prefix("/rename ") {
            plain::rename_tab(title.trim());
            println!(
                "  {}\n",
                format!("tab renamed to `{}`", title.trim()).dimmed()
            );
            continue;
        }
        if let Some(color) = input.strip_prefix("/color ") {
            let name = color.trim();
            if plain::set_accent(name) {
                // Acknowledge in the newly-set accent itself — the welcome
                // banner uses a fixed palette and can't reflect the accent,
                // so re-printing it would silently ignore the change.
                println!(
                    "  {} {}\n",
                    "◆".color(accent()),
                    format!("accent set to {name}").color(accent()).bold()
                );
            }
            continue;
        }
        if input == "/goal" {
            println!(
                "  {}\n",
                "usage: /goal <what must be true when done>".dimmed()
            );
            continue;
        }
        if let Some(goal) = input.strip_prefix("/goal ") {
            let goal = goal.trim();
            if goal.is_empty() {
                println!(
                    "  {}\n",
                    "usage: /goal <what must be true when done>".dimmed()
                );
                continue;
            }
            println!();
            // Phase 2 (#713): carried to `run_goal_turn`, which owns the
            // event channel this turn's telemetry rides.
            let mut recall_event = None;
            if let Some(m) = &mut memory {
                // Same schedule as the plain prompts above (#1221): one
                // interleaved sequence per workspace, not one per command.
                m.arm_recall_control();
                let recalled = m.recall_block_reported(goal).await;
                recall_event = recalled.telemetry_event();
                inject_recall_block(&mut messages, recalled.text);
            }
            // Everything the goal loop appends past here is this turn's work,
            // gating reflection on it (see `turn_warrants_reflection`).
            let turn_start = messages.len();
            let started_unix = crate::memory::unix_now_secs();
            presence.update_prompt(goal);
            let result = run_goal_turn(
                &*provider,
                base_tools,
                &custom_tools,
                &registry,
                &mut messages,
                &mut budget,
                &calibration,
                cfg,
                &store,
                goal,
                Some(presence.id()),
                recall_event,
                memory.as_mut(),
            )
            .await;
            presence.needs_input();
            record_turn_episode(
                &memory,
                goal,
                &result,
                started_unix,
                &messages[turn_start..],
            )
            .await;
            if let Err(e) = &result {
                eprintln!("  {} {}\n", "Error:".red().bold(), e);
            }
            reflect_on_interactive_turn(
                &*provider,
                cfg,
                &mut memory,
                &messages,
                turn_start,
                &result,
                &mut budget,
            )
            .await;
            continue;
        }

        // A custom command/skill (⚡): expand the template — arguments and
        // all — into the prompt the model turn runs. Reserved names never
        // reach a custom definition, so the REPL vocabulary above cannot be
        // shadowed even in argument-carrying forms the exact-match handlers
        // let through (e.g. `/help topic`).
        let expanded = if input.starts_with('/') {
            custom.expand(input, REPL_RESERVED)
        } else {
            None
        };
        let input = expanded.as_deref().unwrap_or(input);

        messages.push(crate::attachments::user_message_in(
            input,
            &cfg.workspace_root,
        ));
        println!();

        let mut recall_event = None;
        if let Some(m) = &mut memory {
            // Proposal 4: A/B recall measurement — on every `rate`-th turn in
            // this workspace, suppress recall so the outcome is comparable to
            // recalled turns. The suppressed flag rides with the turn for
            // attribution.
            m.arm_recall_control();
            let recalled = m.recall_block_reported(input).await;
            recall_event = recalled.telemetry_event();
            inject_recall_block(&mut messages, recalled.text);
        }

        // Everything `run_turn` appends past here is this turn's work; the
        // reflection gate reads only that slice (see `turn_warrants_reflection`).
        let turn_start = messages.len();
        let started_unix = crate::memory::unix_now_secs();
        presence.update_prompt(input);
        let result = run_turn(
            &*provider,
            base_tools,
            &custom_tools,
            &registry,
            &mut messages,
            &mut budget,
            &calibration,
            &router,
            cfg,
            OutputFormat::Text,
            &store,
            persistence::TurnDoor::new("chat"),
            input,
            Some(presence.id()),
            recall_event,
            memory.as_mut(),
        )
        .await;
        presence.needs_input();
        record_turn_episode(
            &memory,
            input,
            &result,
            started_unix,
            &messages[turn_start..],
        )
        .await;
        if let Err(e) = &result {
            eprintln!("  {} {}\n", "Error:".red().bold(), e);
        }
        reflect_on_interactive_turn(
            &*provider,
            cfg,
            &mut memory,
            &messages,
            turn_start,
            &result,
            &mut budget,
        )
        .await;
    }

    if let Some(set) = &mcp {
        set.close_all().await;
    }
    presence.finish(stella_store::SessionStatus::Complete, None);
    println!("\n  {}", "Goodbye! ✦".magenta());
    Ok(())
}

/// Record one interactive turn as an episode — outcome and time window,
/// gated the same way as reflection so trivial conversational turns write
/// nothing. `pub(crate)`: the Command Deck's turn driver records through the
/// same helper.
pub(crate) async fn record_turn_episode<T, E>(
    memory: &Option<SessionMemory>,
    prompt: &str,
    result: &Result<T, E>,
    started_unix: i64,
    turn_messages: &[CompletionMessage],
) {
    let Some(m) = memory else {
        return;
    };
    if !turn_warrants_reflection(turn_messages) {
        return;
    }
    let episode_outcome = if result.is_ok() {
        EpisodeOutcome::Success
    } else {
        EpisodeOutcome::Failure
    };
    // Proposal 4's `[ab-control]` tag is appended by `record_episode` itself,
    // from the suppression flag this turn was armed with — it used to be
    // composed into the prompt here, where a 240-character prompt truncated it
    // away, and where the three other episode-writing surfaces never had it at
    // all.
    m.record_episode(prompt, episode_outcome, &[], started_unix, None)
        .await;
}

/// Query the code graph (if `stella init` has built it) for the
/// best-connected file's neighborhood, converted to the deck's Graph-tab
/// snapshot. `None` when there is no index, it is empty, or any read fails —
/// the tab then shows its "run stella init" hint instead of an empty graph.
///
/// This is [`graph_snapshot_focus`] with no explicit focus: the neighborhood
/// centers on [`busiest_file`](stella_graph::CodeGraph::busiest_file), which
/// the deck opens on and can re-root away from via the picker.
pub(crate) fn graph_snapshot(
    workspace_root: &std::path::Path,
) -> Option<stella_tui::GraphSnapshot> {
    graph_snapshot_focus(workspace_root, None)
}

/// Build the Graph-tab snapshot centered on `focus` (a root-relative file
/// path), or on the busiest file when `focus` is `None`. The snapshot always
/// carries the full [`files`](stella_tui::GraphSnapshot::files) list so the
/// deck's picker can re-root onto any of them — the deck answers a
/// `FocusGraphFile` request by calling this with `Some(file)` and shipping the
/// result back as a fresh `Inbound::GraphSnapshot`. `None` when there is no
/// index, it is empty, or any read fails.
pub(crate) fn graph_snapshot_focus(
    workspace_root: &std::path::Path,
    focus: Option<&str>,
) -> Option<stella_tui::GraphSnapshot> {
    use stella_tui::{GraphEdge, GraphNode, GraphSnapshot};

    let db_path =
        stella_store::existing_workspace_private_sqlite_path(workspace_root, "codegraph.db")
            .ok()??;
    if !db_path.exists() {
        return None;
    }
    let graph = stella_graph::CodeGraph::open(workspace_root, &db_path).ok()?;
    // An explicit pick roots there; otherwise fall back to the busiest file.
    let focus = match focus {
        Some(f) => f.to_string(),
        None => graph.busiest_file().ok()??,
    };
    let hood = graph.file_neighborhood(std::path::Path::new(&focus)).ok()?;
    // The full file list backs the picker (a superset of this neighborhood).
    let files = graph.all_files().unwrap_or_default();
    graph.shutdown();

    let mut nodes = vec![GraphNode {
        label: hood.file.clone(),
        kind: "file".to_string(),
        location: Some(hood.file.clone()),
    }];
    let mut edges = Vec::new();
    for symbol in &hood.symbols {
        edges.push(GraphEdge {
            from: 0,
            to: nodes.len(),
            kind: "defines".to_string(),
        });
        nodes.push(GraphNode {
            label: symbol.name.clone(),
            kind: symbol.kind.clone(),
            location: Some(format!("{}:{}", hood.file, symbol.start_line)),
        });
    }
    for import in &hood.imports {
        edges.push(GraphEdge {
            from: 0,
            to: nodes.len(),
            kind: "imports".to_string(),
        });
        nodes.push(GraphNode {
            label: import.clone(),
            kind: "module".to_string(),
            location: None,
        });
    }
    for importer in &hood.importers {
        edges.push(GraphEdge {
            from: nodes.len(),
            to: 0,
            kind: "imports".to_string(),
        });
        nodes.push(GraphNode {
            label: importer.clone(),
            kind: "file".to_string(),
            location: Some(importer.clone()),
        });
    }
    Some(GraphSnapshot {
        focus: hood.file,
        nodes,
        edges,
        files,
    })
}

/// Cap on each MCP server's connect — the per-server bound
/// `McpToolSet::connect` enforces. Deliberately short: a server that cannot
/// even complete its handshake in 10s should not stall session start. Each
/// later `tools/call` gets the much longer `stella_mcp::DEFAULT_CALL_TIMEOUT`
/// instead (applied in [`connect_mcp_servers`]) — without that override the
/// connect bound would double as the call bound and kill every long-running
/// tool call at 10s.
const MCP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The parse of `.stella/mcp.toml`, split from the connect so a caller that
/// owns a UI (the deck) can announce the slow part before awaiting it (#98).
pub(crate) enum McpPlan {
    /// No config file, or one naming zero servers — nothing to connect.
    None,
    /// The config exists but is unreadable/invalid: MCP is disabled this
    /// session, and the reason must be surfaced exactly once.
    Invalid(String),
    /// Servers to connect via [`connect_mcp_servers`].
    Servers(Vec<McpServerConfig>),
}

pub(crate) fn load_mcp_plan(cfg: &Config) -> McpPlan {
    if crate::settings::filesystem_settings_disabled()
        || crate::enterprise_telemetry::process_free_authority_active()
    {
        return McpPlan::None;
    }
    let path = cfg.workspace_root.join(".stella").join("mcp.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return McpPlan::None;
    };
    // Trust gate. A cloned repo's `.stella/mcp.toml` can name an arbitrary
    // stdio `command` (executed at session start — RCE on `git clone && stella`)
    // or an attacker-controlled http endpoint (egress + a would-be-whitelisted
    // phone-home). This is the same code-execution risk as project hooks, so it
    // is gated by the same flag: untrusted, we do not connect and say why once.
    // (Project settings.json hooks/credential-routing are already gated in
    // settings.rs; this closes the parallel .stella/mcp.toml hole.)
    if !crate::settings::project_code_execution_trusted() {
        return McpPlan::Invalid(format!(
            "{} was NOT loaded — set STELLA_TRUST_PROJECT=1 to let this repo start its \
             MCP servers (they run commands / open connections on your machine)",
            path.display()
        ));
    }
    let parsed = match McpConfig::from_toml_str(&text) {
        Ok(parsed) => parsed,
        Err(e) => {
            return McpPlan::Invalid(format!(
                "{} is invalid: {e} — MCP servers disabled this session",
                path.display()
            ));
        }
    };
    let servers = parsed.into_servers();
    if servers.is_empty() {
        McpPlan::None
    } else {
        McpPlan::Servers(servers)
    }
}

/// Stage 2 of MCP assembly: the slow part — up to [`MCP_CONNECT_TIMEOUT`]
/// per server. Best-effort and isolated per server (stella-mcp records
/// failures in the set instead of propagating them); the returned set wraps
/// `native` so non-`mcp__` tool names fall through to it.
pub(crate) async fn connect_mcp_servers(
    servers: &[McpServerConfig],
    native: std::sync::Arc<dyn ToolExecutor>,
    usage: Option<stella_core::mcp_usage::McpUsageLedger>,
    disabled: Option<stella_mcp::DisabledServers>,
    auth: Option<std::sync::Arc<stella_mcp::OAuthManager>>,
) -> McpToolSet {
    let mut set = McpToolSet::connect_with_auth(servers, MCP_CONNECT_TIMEOUT, auth)
        .await
        // The connect bound would otherwise carry over as the per-call bound,
        // killing any tool call slower than [`MCP_CONNECT_TIMEOUT`] — connects
        // stay short, calls get the long default.
        .with_call_timeout(stella_mcp::DEFAULT_CALL_TIMEOUT)
        .wrapping(native);
    // Record each successful MCP call into the session's usage ledger, and
    // honor the session's disabled-servers set (both may be absent for a
    // one-shot run that never toggles servers).
    if let Some(usage) = usage {
        set = set.with_usage_ledger(usage);
    }
    if let Some(disabled) = disabled {
        set = set.with_disabled_servers(disabled);
    }
    set
}

/// Connect the workspace's MCP servers (.stella/mcp.toml), wrapping the
/// native registry so their tools merge into the agent's set under
/// `mcp__<server>__<tool>` names. Absent config -> None (zero overhead).
/// Connection is best-effort per server (stella-mcp isolates failures);
/// failed servers are reported once in text mode, never fatal. Deck mode
/// stages [`load_mcp_plan`] / [`connect_mcp_servers`] itself instead: the
/// connect must run behind the live TUI, with diagnostics as transcript
/// events rather than prints (#98).
pub(crate) async fn connect_mcp(
    cfg: &Config,
    native: std::sync::Arc<dyn ToolExecutor>,
    usage: Option<stella_core::mcp_usage::McpUsageLedger>,
    print_diagnostics: bool,
) -> Result<Option<Arc<McpToolSet>>, String> {
    let servers = match load_mcp_plan(cfg) {
        McpPlan::None => return Ok(None),
        McpPlan::Invalid(reason) => {
            if print_diagnostics {
                eprintln!("  {} {reason}", "!".yellow());
            }
            return Ok(None);
        }
        McpPlan::Servers(servers) => servers,
    };
    // A one-shot run has no interactive enable/disable, so no disabled set.
    let auth = crate::mcp_cmd::oauth_manager(&cfg.workspace_root)?;
    let set = connect_mcp_servers(&servers, native, usage, None, Some(auth)).await;
    if print_diagnostics {
        crate::mcp_cmd::print_connect_diagnostics(&set);
    }
    // Arc'd so a pipeline driver can share the same connected set into the
    // Best-of-N candidate tool surface and orchestrator pre-fetch (issue
    // #248 Phase 1) alongside its own `&dyn ToolExecutor` borrow.
    Ok(Some(Arc::new(set)))
}

pub(crate) async fn discover_custom_tools(
    cfg: &Config,
    print_diagnostics: bool,
) -> Vec<CustomTool> {
    if crate::enterprise_telemetry::process_free_authority_active() {
        return Vec::new();
    }
    // The manifest walk is synchronous directory I/O — off the runtime
    // worker thread it goes (#64).
    let root = cfg.workspace_root.clone();
    let include_workspace = cfg.authority.project_custom_tools_allowed;
    let report = tokio::task::spawn_blocking(move || {
        custom_tool_report_for_scopes(&root, include_workspace)
    })
    .await
    .unwrap_or_else(|_| custom_tool_report_for_scopes(&cfg.workspace_root, include_workspace));
    // The tool-foundry gate (#830) — applied at this chokepoint because it
    // needs the adoption ledger and discovery has no store handle.
    let report = crate::tool_foundry::adopt::gate_discovery(report, &cfg.workspace_root);
    if print_diagnostics {
        for diagnostic in &report.diagnostics {
            eprintln!(
                "  {} custom tool skipped: {} — {}",
                "!".yellow(),
                diagnostic.path.display(),
                diagnostic.reason
            );
        }
        if !report.diagnostics.is_empty() {
            eprintln!(
                "  {}",
                "run `stella tools --validate` to check every custom tool manifest".dimmed()
            );
        }
    }
    report.tools
}

/// Why a tool is off, phrased as the settings entry that did it — nothing is
/// "disabled (default)" any more, so the only honest answer names a key.
fn policy_reason(policy: &stella_tools::policy::ToolPolicy, name: &str) -> String {
    match crate::tool_policy::disabled_by(policy, name) {
        Some(key) => format!("\"tools\": {{\"{key}\": \"off\"}} in settings"),
        // Unreachable for a name the caller already found denied; a plain
        // sentence beats an unwrap if the two ever disagree.
        None => "a settings entry".to_string(),
    }
}

/// `stella tools` — list the tools the agent would have this session:
/// native built-ins, developer custom tools (with their source manifests),
/// and any discovery diagnostics for broken manifests. MCP-server tools
/// (.stella/mcp.toml) are merged in at session build time and are not
/// enumerated here — connecting to the servers is out of scope for a listing.
pub fn run_tools_listing() -> Result<(), String> {
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    plain::section_header("Stella tools");

    // The listing mirrors a real session: the registry builds the full
    // surface, and the operator's `"tools"` switches decide what survives.
    let settings = crate::settings::Settings::load(&workspace_root)?;
    let policy = settings.tool_policy();
    let registry = ToolRegistry::new(workspace_root.clone());
    println!("  {}", "built-in:".dimmed());
    let mut native: Vec<String> = stella_core::ports::ToolExecutor::schemas(&registry)
        .into_iter()
        .map(|s| s.name)
        .collect();
    native.sort();
    for name in &native {
        if policy.allows(name) {
            println!("    {} {}", "·".dimmed(), name);
        } else {
            println!(
                "    {} {}",
                "·".dimmed(),
                format!("{name} — off ({})", policy_reason(&policy, name)).dimmed()
            );
        }
    }
    // A denied name that never registered would otherwise vanish silently;
    // list it as off rather than pretend it does not exist.
    let mut withheld: Vec<&str> = policy
        .denied_builtins()
        .into_iter()
        .filter(|name| !native.iter().any(|live| live == name))
        .collect();
    withheld.sort_unstable();
    for name in withheld {
        println!(
            "    {} {}",
            "·".dimmed(),
            format!("{name} — off ({})", policy_reason(&policy, name)).dimmed()
        );
    }
    if policy.is_default() {
        println!(
            "    {}",
            "every tool is on — switch one off with \"tools\": {\"<name|group|*>\": \"off\"} \
             in settings"
                .dimmed()
        );
    }

    let user_root = crate::paths::user_extension_root();
    // Gated: an ungated listing would advertise a withheld tool as available.
    let found = custom::discover_in_scopes(
        &workspace_root,
        user_root.as_deref(),
        settings.authority_policy.project_custom_tools_allowed,
    );
    let report = crate::tool_foundry::adopt::gate_discovery(found, &workspace_root);
    println!(
        "\n  {}",
        "custom (.stella/tools/, ~/.stella/tools/):".dimmed()
    );
    if report.tools.is_empty() {
        println!(
            "    {}",
            "none — drop a <name>.toml manifest in .stella/tools/ to add one".dimmed()
        );
    }
    for tool in &report.tools {
        println!(
            "    {} {} — {}",
            "·".green(),
            tool.name.bright_magenta(),
            format!("{}{}", tool.claims_label(), tool.description).dimmed()
        );
    }
    for diagnostic in &report.diagnostics {
        println!(
            "    {} {} — {}",
            "✗".red(),
            diagnostic.path.display(),
            diagnostic.reason.red()
        );
    }

    println!(
        "\n  {}",
        "MCP servers (.stella/mcp.toml) merge more tools at session start — \
         not enumerated here."
            .dimmed()
    );
    Ok(())
}

/// `stella tools --validate [DIR]` — the strict pre-flight for custom tool
/// manifests. Where discovery (and the plain listing above) stays lenient,
/// this checks every `*.toml` in `dir` (or, by default, the same directories
/// discovery scans) and reports errors, warnings, and infos per file — see
/// `stella_tools::validate`. Returns `Err` when any manifest has errors, so
/// the process exits non-zero and a broken manifest is caught *before* a run
/// consumes model budget.
pub fn run_tools_validation(dir: Option<&std::path::Path>) -> Result<(), String> {
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    plain::section_header("Custom tool manifests — validation");

    let report = match dir {
        Some(dir) => {
            if !dir.is_dir() {
                return Err(format!(
                    "`{}` is not a directory — pass a directory of *.toml manifests, or omit \
                     the value to check .stella/tools/ and ~/.stella/tools/",
                    dir.display()
                ));
            }
            println!("  {} {}", "checking:".dimmed(), dir.display());
            validate::validate_dir(dir, &workspace_root)
        }
        None => {
            println!(
                "  {} {}",
                "checking:".dimmed(),
                ".stella/tools/, ~/.stella/tools/".dimmed()
            );
            validate::validate_default(&workspace_root)
        }
    };

    if report.manifests.is_empty() {
        println!(
            "  {}",
            "no manifests found — drop a <name>.toml in .stella/tools/ to add a custom tool"
                .dimmed()
        );
        return Ok(());
    }

    println!();
    for manifest in &report.manifests {
        let mark = if manifest.has_errors() {
            "✗".red()
        } else {
            "✓".green()
        };
        let name = manifest
            .name
            .as_deref()
            .map(|n| format!(" ({n})"))
            .unwrap_or_default();
        println!(
            "  {mark} {}{}",
            manifest.path.display(),
            name.bright_magenta()
        );
        for issue in &manifest.issues {
            let (label, message) = match issue.severity {
                validate::Severity::Error => ("error:".red().bold(), issue.message.red()),
                validate::Severity::Warning => ("warning:".yellow().bold(), issue.message.normal()),
                validate::Severity::Info => ("info:".dimmed(), issue.message.dimmed()),
            };
            println!("      {label} {message}");
        }
    }

    let failed = report.manifests.iter().filter(|m| m.has_errors()).count();
    let ok = report.manifests.len() - failed;
    println!(
        "\n  {} manifest(s) checked: {} ok, {} with errors, {} warning(s)",
        report.manifests.len(),
        ok,
        failed,
        report.warning_count()
    );

    if failed > 0 {
        Err(format!(
            "{failed} of {} custom tool manifest(s) failed validation",
            report.manifests.len()
        ))
    } else {
        Ok(())
    }
}

/// Open the workspace SQLite store (`.stella/private/store.db`). Persistence is
/// observability, not a work dependency: a store that won't open warns once
/// and the session runs on without it — never a startup failure.
pub(crate) fn open_store(workspace_root: &std::path::Path) -> Option<Arc<Store>> {
    // Persisted telemetry can feed calibration and extension-authored rules
    // back into later sessions. Claim-mode trials are isolated and ephemeral:
    // do not read that state or create `.stella/private/store.db` in the task — which
    // is what `session_persistence()` answers.
    //
    // The open itself is `stella_runtime::open_store`, which *returns* the
    // degradation notice instead of printing it. Rendering it is this layer's
    // job precisely because it is the layer that knows stdout may be
    // machine-readable JSON and the glyph belongs on stderr (#971).
    let (store, notice) = stella_runtime::open_store(workspace_root, session_persistence());
    if let Some(notice) = notice {
        eprintln!("  {} {}", "⚠".yellow(), notice.message);
    }
    store
}

/// Run one full turn through `stella_core::Engine`, rendering its
/// `AgentEvent` stream live via a spawned draining task. Ordinary runs
/// enqueue to an unbounded channel; benchmark stream-json runs synchronously
/// append+flush each event before enqueueing it, so paid-call evidence
/// survives a paused/cancelled renderer. The drain task ([`spawn_renderer`])
/// persists every event and each `StepUsage` to the workspace store when one
/// is open. `registry` is the concrete tool registry (its ledgers close the
/// execution's audit record); `base_tools` is the same registry as the
/// engine's executor, possibly MCP-wrapped.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_turn(
    provider: &dyn Provider,
    base_tools: &dyn ToolExecutor,
    custom_tools: &[CustomTool],
    registry: &ToolRegistry,
    messages: &mut Vec<CompletionMessage>,
    budget: &mut BudgetGuard,
    calibration: &CalibrationMap,
    // Session-scoped breaker feedback (#2673): the engine reports outcomes.
    router: &Router,
    cfg: &Config,
    format: OutputFormat,
    store: &Option<Arc<Store>>,
    door: persistence::TurnDoor<'_>,
    prompt: &str,
    session: Option<&str>,
    // Phase 2 (#713): this turn's `ContextRecall`, if recall ran. Recall
    // happens before the turn's event channel exists — it has to, because its
    // frames go into the messages the turn is built from — so the caller hands
    // the event forward rather than emitting it into a stream that is not
    // there yet. Passed rather than re-derived: re-running recall to report it
    // would double the retrieval cost of every interactive turn.
    recall_event: Option<AgentEvent>,
    // The caller's session memory, borrowed for the duration of the turn so
    // the execution seam can stamp this execution's id and record its
    // skill-version usage before the turn runs — the caller reflects with the
    // same memory afterwards, and a reflection that cannot name its execution
    // files an id-less row (NULL `self_rating`).
    mut session_memory: Option<&mut SessionMemory>,
) -> Result<TurnOutcome, CliFailure> {
    budget.begin_turn();
    let turn_start = Instant::now();
    let execution = begin_execution(store, door.kind, prompt, cfg, session, door.variant);
    stamp_and_record_skill_usage(
        &execution,
        session_memory.as_deref_mut(),
        prompt,
        &cfg.workspace_root,
    );
    let (raw_tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
    let (tx, durable_pre_persisted) = output::raw_event_sender_for_run(raw_tx, format, &door);
    // The proactive re-query (#3243 Phase 3): the engine consults this at
    // every step boundary; the adapter's hysteresis makes an undrifted turn
    // free. Seeded from `messages` so the turn-opening block is never
    // re-injected, and given `tx` so its own recall is metered (#3366).
    let requery = crate::memory::requery_for_turn(session_memory.as_deref(), messages, tx.clone());
    persistence::attach_run_streams(registry, &tx);
    let renderer = spawn_renderer(
        rx,
        format,
        execution.clone(),
        cfg.provider.id.to_string(),
        durable_pre_persisted,
        Some(prompt.to_string()),
    );
    // Recall's frames, then this run's own opening stage boundary — see
    // `output::open_raw_turn` for the ordering and for why it lives there.
    output::open_raw_turn(&tx, recall_event);

    // Mid-turn fallback (#2679): on an exhausted retry ladder the engine
    // re-resolves the worker role through this session router.
    let fallback = engine::SessionFallback::new(router);
    // The scoped tool set must drop its tx clone before awaiting the renderer.
    let outcome = if crate::enterprise_telemetry::process_free_authority_active() {
        // Even when process-free authority strips the MCP/custom/interactive
        // layers, the `"tools"` policy (operator/managed-org tool switches)
        // and the authorization gate must still hold above the session tool
        // stack — mirroring every other driver path, so disabled tools cannot
        // be invoked here either.
        let bus = registry.hook_bus();
        let permitted = tool_stack::policy_stack(registry, cfg, Principal::User, bus);
        let config = engine::engine_config_for_kind(cfg, door.kind);
        let mut engine = Engine::with_sleeper(provider, &permitted, config, &TokioSleeper)
            .with_calibration(calibration)
            .with_provider_outcomes(router)
            .with_fallback_resolver(&fallback);
        if let Some(requery) = &requery {
            engine = engine.with_requery(requery);
        }
        engine.run_turn_with_sender(messages, budget, &tx).await
    } else {
        // Customs, the operator's switches, and the authorization gate,
        // outermost-last (#3283) — one assembly for every driver.
        let bus = registry.hook_bus();
        let tools =
            tool_stack::session_stack(base_tools, custom_tools.to_vec(), cfg, Principal::User, bus);
        let hook_runner = ShellHookRunner;
        // A PreToolUse hook's `require_approval` parks on the #2676 broker
        // flow (#2684). Snapshotted here, after assembly attached any
        // responder and bus, so the route asks the surface this run has.
        let hook_approvals = stella_tools::hook_bridge::BrokerApprovalRoute::for_registry(registry);
        let config = engine::engine_config_for_kind(cfg, door.kind);
        let mut engine = Engine::with_sleeper(provider, &tools, config, &TokioSleeper)
            .with_calibration(calibration)
            .with_provider_outcomes(router)
            .with_fallback_resolver(&fallback);
        if let Some(hooks) = &cfg.hooks {
            engine = engine
                .with_hooks(hooks, &hook_runner)
                .with_hook_approval_route(&hook_approvals);
        }
        if let Some(requery) = &requery {
            engine = engine.with_requery(requery);
        }
        engine.run_turn_with_sender(messages, budget, &tx).await
    };
    // What this turn changed in the shared tree (#3413), measured at the
    // boundary and emitted *before* the close below: these are the turn's own
    // events and a consumer folding the stream must see them inside the turn
    // they describe. See `crate::turn_files` for why a measurement, not a hook.
    crate::turn_files::emit_shared_tree_changes(cfg, &tx, execution.as_ref());
    // This path owns its run — one raw engine turn, no pipeline above it — so
    // it owes the run's terminator (#3379). The engine ends the turn with
    // `TurnComplete` and deliberately says nothing about the run, and every
    // other owner (the deck, fleet, goal, resume, the pipeline one-shot) goes
    // through this same seam; without it a raw `stella run` ended on
    // `turn_complete` and simply stopped, leaving every consumer waiting for a
    // terminal event that never came. Last, after the turn's own events above.
    persistence::emit_run_complete_for_turn(&tx, &cfg.model_id, &outcome);
    // The re-query adapter holds an `EventSender` clone of this run's channel
    // (#3366 telemetry), so it must be released here too — otherwise it keeps
    // the channel open and the renderer's `recv()` loop never ends (#2290).
    drop(requery);
    // Releasing every sender — the registry's clones included — closes the
    // channel, ending the renderer's `recv()` loop; awaiting it ensures every
    // already-queued event has actually printed before this function returns.
    let rendered = close_event_stream(registry, tx, renderer).await;
    let persistence_complete = rendered.persistence_complete;
    let collected = rendered.events;

    let (outcome_label, cost) = match &outcome {
        TurnOutcome::Completed { cost_usd, .. } => ("completed", *cost_usd),
        TurnOutcome::Aborted { cost_usd, .. } => ("aborted", *cost_usd),
    };
    turn_close::close_turn(
        cfg,
        store,
        &execution,
        registry,
        session,
        turn_close::TurnOutcomeRecord {
            label: outcome_label,
            cost_usd: cost,
            persistence_complete,
        },
    );

    if format == OutputFormat::Json {
        // One final JSON object: the outcome summary plus the full event log
        // (the same objects stream-json would have emitted line by line).
        summary::print_json_summary(cfg, &outcome, collected);
    }

    if let TurnOutcome::Completed { cost_usd, .. } = &outcome
        && format == OutputFormat::Text
    {
        plain::cost_summary(
            *cost_usd,
            &format!("{}/{}", cfg.provider.id, cfg.model_id),
            turn_start.elapsed(),
        );
        println!();
    }
    match outcome {
        TurnOutcome::Aborted { reason, kind, .. } => Err(CliFailure::from_abort(reason, kind)),
        completed => Ok(completed),
    }
}

fn print_help() {
    println!("  {}\n", "Stella Commands".bright_cyan().bold());
    println!("  {}  Send a prompt to the agent", "type message".dimmed());
    println!(
        "  {}       List configured providers and models (`/models refresh` re-syncs the catalog)",
        "/models".bright_magenta()
    );
    println!(
        "  {}        Show current configuration",
        "/config".bright_magenta()
    );
    println!(
        "  {}         Clear conversation history",
        "/clear".bright_magenta()
    );
    println!(
        "  {}  Work in judged rounds until a verifier confirms the goal is met",
        "/goal <text>".bright_magenta()
    );
    println!(
        "  {}      List custom agents (⚡ from .stella/agents or ~/.stella/agents)",
        "/agents".bright_magenta()
    );
    println!(
        "  {} Rename this terminal tab",
        "/rename <name>".bright_magenta()
    );
    println!(
        "  {}  Change the accent color (multi-window)",
        "/color <name>".bright_magenta()
    );
    println!(
        "  {}          Index the workspace: domain taxonomy + code graph",
        "/init".bright_magenta()
    );
    println!("  {}          Show this help", "/help".bright_magenta());
    println!("  {}          Exit Stella", "/exit".bright_magenta());
    println!("  {}          Exit Stella", "/quit".bright_magenta());
    println!("  {}         Exit Stella", "Ctrl+D".dimmed());
    println!();
}

#[cfg(test)]
mod tests;
