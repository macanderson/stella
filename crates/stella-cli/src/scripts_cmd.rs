//! `stella scripts` — the human door to the project scripts index, mirroring
//! the `list_scripts`/`run_script` agent tools one-for-one
//! (`docs/spec/scripts-index.md`). Detection is static manifest parsing and
//! `run` executes one indexed entry, so the surface is offline and needs no
//! API key.
//!
//! Split verbatim out of `main.rs` — no behavior change — to keep the CLI
//! entry point under the file-size ratchet while later phases grow the
//! subcommand surface. `main.rs` keeps the `Command::Scripts` variant (clap
//! needs it on the root enum) and dispatches into this module, the same
//! shape `stats.rs` and `usage_cmd.rs` already use.

use clap::Subcommand;

/// The `stella scripts` surface, mirroring the `list_scripts`/`run_script`
/// tools one-for-one so a human at the CLI and the model inside a turn see
/// the same frames (docs/spec/scripts-index.md).
#[derive(Subcommand)]
pub enum ScriptsCmd {
    /// List detected scripts and their canonical verb bindings
    List {
        /// Emit the index as JSON (schema_version 1)
        #[arg(long)]
        json: bool,

        /// Narrow to one workspace package dir
        #[arg(long)]
        dir: Option<String>,
    },
    /// Run a script by canonical verb or qualified id (e.g. test, pnpm:build)
    Run {
        /// install|build|check|start|test|lint|format, or a qualified id
        script: String,

        /// Package dir when the id exists in several packages
        #[arg(long)]
        dir: Option<String>,

        /// Timeout in seconds
        #[arg(long, default_value_t = 600)]
        timeout_secs: u64,

        /// Extra args appended runner-natively (after `--` for npm-family)
        #[arg(last = true)]
        args: Vec<String>,
    },
}

/// `stella scripts …` — the human door to the project scripts index.
/// Detection is static manifest parsing; `run` executes one indexed entry.
pub fn run_scripts(cmd: &ScriptsCmd) -> Result<(), String> {
    let root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    match cmd {
        ScriptsCmd::List { json, dir } => {
            let index = stella_tools::scripts::ScriptIndex::detect_blocking(&root);
            if *json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&index.to_json())
                        .map_err(|e| format!("serialize index: {e}"))?
                );
            } else {
                println!("{}", index.render_list(dir.as_deref()));
            }
            Ok(())
        }
        ScriptsCmd::Run {
            script,
            dir,
            timeout_secs,
            args,
        } => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("failed to start runtime: {e}"))?;
            let output = rt.block_on(stella_tools::scripts::run_by_name(
                &root,
                script,
                dir.as_deref(),
                args,
                *timeout_secs,
            ));
            match output {
                stella_protocol::tool::ToolOutput::Ok { content } => {
                    println!("{content}");
                    Ok(())
                }
                stella_protocol::tool::ToolOutput::Error { message } => Err(message),
            }
        }
    }
}
