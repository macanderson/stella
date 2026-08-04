//! Adoption tooling — the human-initiated commands that move a staged tool
//! through the #830 protocol: prove it, record it, and (separately) allow it.
//!
//! Three commands, one per decision, on purpose:
//!
//! - `stella tools --adopt <name>` runs the capability witness against the
//!   candidate and, only on a flip, writes an adoption record. The tool is
//!   **not usable** afterwards — [`stella_tools::foundry_gate`] withholds it,
//!   because adoption is evidence, not permission.
//! - `stella tools --enable <name>` is the permission, and the only step a
//!   machine never takes on its own.
//! - `stella tools --foundry` reports what the workspace has adopted and what
//!   each adoption has actually been worth — including the ones that were
//!   proven and never used, which #830 asks be tracked as the cost.
//!
//! # Why adoption writes the files before it proves them
//!
//! The candidate is moved into `.stella/tools/` *first*, and the witness runs
//! against the tool as it will really exist — same path, same manifest bytes,
//! same script. Proving a staged copy and then adopting a rewritten one would
//! mean the digests in the ledger pin bytes no witness ever ran against.
//!
//! That ordering is only safe because the gate exists. A foundry manifest in
//! `.stella/tools/` with no adoption record does not register, so the window
//! between "written" and "proven" grants nothing. If the witness fails, both
//! files are removed again and the staged pair is left untouched for review.

use std::path::{Path, PathBuf};

use colored::Colorize;
use serde_json::Value;
use stella_core::ports::ToolExecutor;
use stella_protocol::tool::{ToolOutput, ToolSchema};
use stella_store::{AdoptedTool, Store};
use stella_tools::custom::{self, CustomTool, CustomToolSet};
use stella_tools::foundry_author::PROPOSED_DIR;
use stella_tools::foundry_witness::{WitnessVerdict, prove};

/// Where an adopted manifest and script live — the directory discovery scans.
const ADOPTED_DIR: &str = ".stella/tools";

/// `stella tools --adopt <name>`.
pub(crate) fn run_tools_adopt(name: &str) -> Result<(), String> {
    let root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let store = Store::open(&root).map_err(|e| format!("cannot open the workspace store: {e}"))?;
    let adopted = adopt_in(&root, &store, name)?;
    let (manifest, script) = adopted_paths(&root, name);

    crate::tui::section_header("Tool adopted — still disabled");
    println!(
        "  {} {}\n  {} {}",
        "·".green(),
        manifest.display(),
        "·".green(),
        script.display(),
    );
    println!("\n  {} {}", "witness:".dimmed(), adopted.witness);
    println!(
        "\n  {}\n  {}",
        "a self-authored tool lands disabled — nothing can call it yet:".dimmed(),
        format!("enable: stella tools --enable {name}").dimmed()
    );
    Ok(())
}

/// `stella tools --enable <name>` / `--disable <name>`.
pub(crate) fn run_tools_enable(name: &str, enabled: bool) -> Result<(), String> {
    let root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let store = Store::open(&root).map_err(|e| format!("cannot open the workspace store: {e}"))?;
    set_enabled_in(&root, &store, name, enabled)?;

    if enabled {
        crate::tui::section_header("Tool enabled");
        println!(
            "  {} {} {}",
            "·".green(),
            name.bright_magenta(),
            "is now offered to the model".dimmed()
        );
    } else {
        crate::tui::section_header("Tool disabled");
        println!(
            "  {} {} {}",
            "·".yellow(),
            name.bright_magenta(),
            "is adopted but no longer offered".dimmed()
        );
    }
    Ok(())
}

/// `stella tools --foundry` — the adoption report and the #830 metric.
pub(crate) fn run_tools_foundry_report() -> Result<(), String> {
    let root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let store = Store::open(&root).map_err(|e| format!("cannot open the workspace store: {e}"))?;
    print!("{}", render_report(&store)?);
    Ok(())
}

/// Prove and record one staged tool. The testable core of
/// [`run_tools_adopt`]: no cwd, no printing.
pub(crate) fn adopt_in(root: &Path, store: &Store, name: &str) -> Result<AdoptedTool, String> {
    let staged_manifest = root.join(PROPOSED_DIR).join(format!("{name}.toml"));
    let staged_script = root.join(PROPOSED_DIR).join(format!("{name}.sh"));
    let manifest_text = std::fs::read_to_string(&staged_manifest).map_err(|e| {
        format!(
            "cannot read {} ({e}) — stage it first with `stella tools --author {name}`",
            staged_manifest.display()
        )
    })?;
    let script = std::fs::read(&staged_script)
        .map_err(|e| format!("cannot read {}: {e}", staged_script.display()))?;

    let staged = custom::parse_manifest(&manifest_text, &staged_manifest)?;
    let provenance = staged
        .foundry
        .as_ref()
        .filter(|p| p.is_foundry_authored())
        .ok_or_else(|| {
            format!(
                "`{name}` carries no [foundry] provenance — adoption is for tools the foundry \
                 authored. A hand-written manifest belongs directly in {ADOPTED_DIR}/."
            )
        })?
        .clone();

    let (adopted_manifest_path, adopted_script_path) = adopted_paths(root, name);
    // A manifest already sitting in the live directory with no ledger row is
    // someone's hand-move. Overwriting it would destroy whatever they edited;
    // re-adoption of a tool this workspace already adopted is fine, and
    // deliberately revokes its approval (the store's writer enforces that).
    let already_adopted = store
        .adopted_foundry_tool(name)
        .map_err(|e| format!("cannot read the adoption ledger: {e}"))?;
    if already_adopted.is_none() && adopted_manifest_path.exists() {
        return Err(format!(
            "`{}` already exists but was never adopted — remove it (or keep it as your own \
             hand-written tool) and re-run",
            adopted_manifest_path.display()
        ));
    }

    let adopted_text = relocate_command(&manifest_text, name)?;
    std::fs::create_dir_all(root.join(ADOPTED_DIR))
        .map_err(|e| format!("cannot create {ADOPTED_DIR}: {e}"))?;
    write_pair(
        &adopted_manifest_path,
        &adopted_text,
        &adopted_script_path,
        &script,
    )?;

    // Self-check before the proof, mirroring the authoring pass: the bytes
    // just written must parse to a tool pointing at the script beside them.
    let candidate = match custom::parse_manifest(&adopted_text, &adopted_manifest_path) {
        Ok(candidate) if candidate.command == vec![format!("./{ADOPTED_DIR}/{name}.sh")] => {
            candidate
        }
        other => {
            remove_pair(&adopted_manifest_path, &adopted_script_path);
            return Err(match other {
                Ok(bad) => format!(
                    "the relocated manifest points at `{}`, not the adopted script",
                    bad.command.first().map(String::as_str).unwrap_or("")
                ),
                Err(e) => format!("the relocated manifest failed its own parse check: {e}"),
            });
        }
    };

    let verdict = run_witness(root, &candidate)?;
    let WitnessVerdict::Proven(case) = &verdict else {
        remove_pair(&adopted_manifest_path, &adopted_script_path);
        return Err(format!(
            "witness failed: {} — nothing was adopted, and the staged pair in {PROPOSED_DIR}/ is \
             untouched",
            verdict.summary()
        ));
    };

    let record = AdoptedTool {
        name: name.to_string(),
        signature: provenance.signature.clone(),
        manifest_digest: stella_tools::foundry_gate::digest(adopted_text.as_bytes()),
        script_digest: stella_tools::foundry_gate::digest(&script),
        witness: verdict.summary(),
        witness_input: case.input.to_string(),
        witness_expect: case.expect.clone(),
        // Set by the store, which refuses to adopt-and-enable in one step.
        enabled: false,
        adopted_at: String::new(),
    };
    store
        .adopt_foundry_tool(&record)
        .map_err(|e| format!("cannot record the adoption: {e}"))?;
    Ok(record)
}

/// Flip one adoption's enablement, refusing to enable a tool whose bytes no
/// longer match the ones the witness ran against.
///
/// Checking here as well as at discovery is not redundant: the gate withholds
/// a tampered tool silently-but-explainably at session start, whereas someone
/// typing `--enable` is asking a direct question and deserves a direct no.
pub(crate) fn set_enabled_in(
    root: &Path,
    store: &Store,
    name: &str,
    enabled: bool,
) -> Result<(), String> {
    let record = store
        .adopted_foundry_tool(name)
        .map_err(|e| format!("cannot read the adoption ledger: {e}"))?
        .ok_or_else(|| {
            format!(
                "`{name}` has not been adopted — prove it first with \
                 `stella tools --adopt {name}`"
            )
        })?;

    if enabled {
        let manifest_path = root.join(ADOPTED_DIR).join(format!("{name}.toml"));
        let text = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
        let tool = custom::parse_manifest(&text, &manifest_path)?;
        let observed = stella_tools::foundry_gate::observe(&tool, root)?;
        let decision = stella_tools::foundry_gate::decide(
            tool.foundry.as_ref(),
            Ok(&observed),
            // Ask the gate what it would say if this were already enabled, so
            // the one reason left to hear is a byte mismatch.
            Some(&AdoptedTool {
                enabled: true,
                ..record.clone()
            }),
        );
        if let stella_tools::foundry_gate::GateDecision::Withhold(reason) = decision {
            return Err(format!(
                "cannot enable `{name}`: {} — re-adopt it with `stella tools --adopt {name}`",
                reason.sentence()
            ));
        }
    }

    if !store
        .set_foundry_tool_enabled(name, enabled)
        .map_err(|e| format!("cannot update the adoption ledger: {e}"))?
    {
        return Err(format!("`{name}` has not been adopted"));
    }
    Ok(())
}

/// Render the adoption report — pure over the store so the shape is testable
/// without a terminal.
pub(crate) fn render_report(store: &Store) -> Result<String, String> {
    let rows = store
        .foundry_reuse()
        .map_err(|e| format!("cannot read the adoption ledger: {e}"))?;
    let mut out = String::new();
    if rows.is_empty() {
        out.push_str(
            "  no self-authored tools adopted yet — `stella tools --author` stages one, \
             `--adopt` proves it\n",
        );
        return Ok(out);
    }
    for row in &rows {
        let state = if row.tool.enabled {
            "enabled"
        } else {
            "adopted, disabled"
        };
        out.push_str(&format!(
            "  {} [{state}] {}\n    witness: {}\n    reuse:   {} call(s), {} failed{}\n",
            row.tool.name,
            row.tool.signature,
            row.tool.witness,
            row.calls,
            row.errors,
            match &row.last_used {
                Some(ts) => format!(", last {ts}"),
                None => String::new(),
            },
        ));
    }
    let adopted = rows.len();
    let used = rows.iter().filter(|r| !r.is_false_start()).count();
    // Both numbers, always. "3 adopted, 3 reused" and "3 adopted, 0 reused"
    // are the difference between the foundry working and the foundry
    // producing shelfware, and reporting only the first hides it.
    out.push_str(&format!(
        "\n  {adopted} adopted · {used} reused · {} never used\n",
        adopted - used
    ));
    Ok(out)
}

/// Rewrite the manifest's staged `command` path to the adopted one, failing
/// closed unless it appears exactly once. A manifest whose command is not the
/// staged script is not something this command should be relocating.
fn relocate_command(manifest_text: &str, name: &str) -> Result<String, String> {
    let staged = format!("./{PROPOSED_DIR}/{name}.sh");
    let adopted = format!("./{ADOPTED_DIR}/{name}.sh");
    match manifest_text.matches(staged.as_str()).count() {
        1 => Ok(manifest_text.replace(&staged, &adopted)),
        0 => Err(format!(
            "the staged manifest does not run `{staged}` — adopt only tools this workspace's \
             foundry authored"
        )),
        n => Err(format!(
            "the staged manifest names `{staged}` {n} times; refusing to guess which is the \
             command"
        )),
    }
}

/// Write the adopted pair, script first and executable.
fn write_pair(
    manifest_path: &Path,
    manifest_text: &str,
    script_path: &Path,
    script: &[u8],
) -> Result<(), String> {
    std::fs::write(script_path, script)
        .map_err(|e| format!("cannot write {}: {e}", script_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(script_path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("cannot mark {} executable: {e}", script_path.display()))?;
    }
    std::fs::write(manifest_path, manifest_text)
        .map_err(|e| format!("cannot write {}: {e}", manifest_path.display()))
}

/// Undo [`write_pair`] after a failed proof. Best-effort: the caller is
/// already returning an error, and a leftover file is caught by the gate.
fn remove_pair(manifest_path: &Path, script_path: &Path) {
    let _ = std::fs::remove_file(manifest_path);
    let _ = std::fs::remove_file(script_path);
}

/// Run the capability witness against the workspace's tool surface.
///
/// World A is the surface *without* the candidate: every built-in name, plus
/// the other custom tools this workspace already has. MCP servers are not
/// connected for an adopt command — a name only an MCP server provides is
/// therefore not detected here, and would in any case be shadowed by that
/// server at session time (the chain is native ← custom ← mcp), so the
/// consequence is a tool that never gets reached rather than one that
/// silently replaces something.
fn run_witness(root: &Path, candidate: &CustomTool) -> Result<WitnessVerdict, String> {
    let home = crate::paths::home();
    let others: Vec<CustomTool> = custom::discover_in(root, home.as_deref())
        .tools
        .into_iter()
        .filter(|tool| tool.name != candidate.name)
        .collect();
    let builtins = Builtins;
    let surface = CustomToolSet::new(&builtins, others, root.to_path_buf());

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to start runtime: {e}"))?;
    Ok(runtime.block_on(prove(&surface, candidate, root)))
}

/// Every built-in tool name, advertised and never executed — the "does this
/// capability already exist?" half of world A. Advertising is all the witness
/// reads; a built-in that ran here would be a side effect nobody asked for.
struct Builtins;

#[async_trait::async_trait]
impl ToolExecutor for Builtins {
    fn schemas(&self) -> Vec<ToolSchema> {
        stella_tools::catalog::ALL_NAMES
            .iter()
            .map(|name| ToolSchema {
                name: (*name).to_string(),
                description: String::new(),
                input_schema: serde_json::json!({ "type": "object" }),
                read_only: false,
                speculation_safe: false,
            })
            .collect()
    }

    async fn execute(&self, name: &str, _: &Value) -> ToolOutput {
        ToolOutput::Error {
            message: format!("no tool named `{name}` is available"),
        }
    }
}

/// Apply the adoption gate to a discovery report — the session's registration
/// path for self-authored tools.
///
/// Two properties worth stating plainly:
///
/// - **Free for everyone else.** A workspace whose manifests are all
///   hand-written never opens the ledger; the gate has nothing to decide and
///   returns the report untouched.
/// - **Fails closed.** If the store cannot be opened, every foundry tool is
///   gated against an *empty* ledger and therefore withheld. An unreadable
///   authority record is not permission to skip the authority check — the same
///   posture the gate takes toward a script it cannot digest.
pub(crate) fn gate_discovery(
    report: custom::DiscoveryReport,
    root: &Path,
) -> custom::DiscoveryReport {
    if !report.tools.iter().any(|tool| {
        tool.foundry
            .as_ref()
            .is_some_and(|p| p.is_foundry_authored())
    }) {
        return report;
    }
    let adopted = Store::open(root)
        .and_then(|store| store.adopted_foundry_tools())
        .unwrap_or_default();
    stella_tools::foundry_gate::gate_report(report, &adopted, root)
}

/// The two paths an adopted pair occupies — one spelling, so the writer, the
/// report, and the tests cannot disagree about where an adopted tool lives.
pub(crate) fn adopted_paths(root: &Path, name: &str) -> (PathBuf, PathBuf) {
    (
        root.join(ADOPTED_DIR).join(format!("{name}.toml")),
        root.join(ADOPTED_DIR).join(format!("{name}.sh")),
    )
}

#[cfg(test)]
mod tests;
