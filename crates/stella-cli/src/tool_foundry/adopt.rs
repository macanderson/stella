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
use stella_store::{AdoptedTool, EnableAuthority, Store};
use stella_tools::custom::{self, CustomTool, CustomToolSet};
use stella_tools::foundry_gate::PROPOSED_DIR;
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

    crate::plain::section_header("Tool adopted — still disabled");
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
///
/// # Why enabling asks and disabling does not
///
/// The evolution ledger requires deterministic proof **plus a local human** for
/// an executable tool, and the proof half was real: `adopt_in` runs the
/// capability witness, adoption lands disabled, re-adoption revokes approval,
/// and enabling re-checks the bytes. The human half was enforced by nothing but
/// the convention that a CLI invocation comes from a person — which an agent
/// holding the `bash` tool falsifies. It could stage, adopt and enable its own
/// authored tool inside one turn, and the next session's model could call it
/// (#5332).
///
/// So `--enable` renders what is being granted and asks, exactly the way
/// `stella plugin install` does, and refuses with no terminal attached unless
/// `--yes` says a human already read it. Disabling withdraws authority and asks
/// nobody: the direction that needs a person is the one that lets a model call
/// new code.
pub(crate) fn run_tools_enable(name: &str, enabled: bool, yes: bool) -> Result<(), String> {
    let root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let store = Store::open(&root).map_err(|e| format!("cannot open the workspace store: {e}"))?;
    if !run_tools_enable_in(&root, &store, name, enabled, yes)? {
        println!("not enabled.");
        return Ok(());
    }

    if enabled {
        crate::plain::section_header("Tool enabled");
        println!(
            "  {} {} {}",
            "·".green(),
            name.bright_magenta(),
            "is now offered to the model".dimmed()
        );
    } else {
        crate::plain::section_header("Tool disabled");
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
/// [`run_tools_adopt`]: no cwd, no printing. Builds its own runtime for the
/// witness — callers already inside one use [`adopt_in_async`].
pub(crate) fn adopt_in(root: &Path, store: &Store, name: &str) -> Result<AdoptedTool, String> {
    let prepared = prepare_adoption(root, store, name)?;
    let verdict = run_witness(root, &prepared.candidate)?;
    finish_adoption(root, store, prepared, verdict, "adopt")
}

/// [`adopt_in`] for a caller already on a tokio runtime — the autonomy
/// pipeline, which runs on the session's own end-of-turn path where
/// `Runtime::block_on` would panic. Same steps, same gates; `reason` labels
/// the version-history row so an autonomous adoption is distinguishable from
/// a human one forever.
pub(crate) async fn adopt_in_async(
    root: &Path,
    store: &Store,
    name: &str,
    reason: &str,
) -> Result<AdoptedTool, String> {
    let prepared = prepare_adoption(root, store, name)?;
    let verdict = prove_candidate(root, &prepared.candidate).await;
    finish_adoption(root, store, prepared, verdict, reason)
}

/// Everything [`adopt_in`] does before the witness runs: read the staged
/// pair, check provenance, relocate, write the adopted pair, and re-parse it.
struct PreparedAdoption {
    candidate: CustomTool,
    provenance: stella_tools::foundry_gate::FoundryProvenance,
    adopted_text: String,
    script: Vec<u8>,
    manifest_path: PathBuf,
    script_path: PathBuf,
}

fn prepare_adoption(root: &Path, store: &Store, name: &str) -> Result<PreparedAdoption, String> {
    let staged_manifest = root.join(PROPOSED_DIR).join(format!("{name}.toml"));
    let staged_script = root.join(PROPOSED_DIR).join(format!("{name}.sh"));
    let manifest_text = std::fs::read_to_string(&staged_manifest).map_err(|e| {
        format!(
            "cannot read {} ({e}) — stage a `{name}.toml` + `{name}.sh` pair under \
             {PROPOSED_DIR}/ first",
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

    Ok(PreparedAdoption {
        candidate,
        provenance,
        adopted_text,
        script,
        manifest_path: adopted_manifest_path,
        script_path: adopted_script_path,
    })
}

/// Judge the verdict and record the adoption — plus one append-only row in
/// the version history, so `stella tools --rollback` always has the
/// exact adopted bytes to restore, whatever later happens to the files.
fn finish_adoption(
    _root: &Path,
    store: &Store,
    prepared: PreparedAdoption,
    verdict: WitnessVerdict,
    reason: &str,
) -> Result<AdoptedTool, String> {
    let PreparedAdoption {
        candidate,
        provenance,
        adopted_text,
        script,
        manifest_path,
        script_path,
    } = prepared;
    let name = candidate.name.as_str();
    let WitnessVerdict::Proven(case) = &verdict else {
        remove_pair(&manifest_path, &script_path);
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
        disabled_reason: String::new(),
        enabled_authority: None,
    };
    store
        .adopt_foundry_tool(&record)
        .map_err(|e| format!("cannot record the adoption: {e}"))?;
    // The version row carries the BYTES, so rollback does not depend on the
    // files surviving. Best-effort by design would hide a broken rollback
    // path until the day it is needed — so a failed write is an error.
    store
        .record_foundry_version(
            name,
            adopted_text.as_bytes(),
            &script,
            &record.manifest_digest,
            &record.script_digest,
            reason,
        )
        .map_err(|e| format!("adopted, but cannot record the version row: {e}"))?;
    Ok(record)
}

/// What `stella tools --enable` says when there is no terminal to ask at.
///
/// Not "the one approval a machine never grants for itself": ADR 0023 makes
/// that false, because with `foundry.autonomy = "auto"` the foundry adopts
/// and enables a tool by itself, inside the sandbox that path requires.
/// `--enable` is the draft-only path — the one taken when autonomy is off, or
/// when the sandbox is not available — and the message says so.
fn no_terminal_to_ask(name: &str) -> String {
    format!(
        "nothing here can ask you to approve `{name}` — no terminal is attached. \
         This is the draft-only path, which `stella tools --enable` takes when \
         `foundry.autonomy` is off or the sandbox the automatic path needs is \
         missing, so the approval has to come from you; re-run with --yes if you \
         have read the declaration above and accept it."
    )
}

/// The consent gate and the ledger write. The testable core of
/// [`run_tools_enable`]: no cwd, no reporting.
///
/// `Ok(false)` means a human was asked and said no — the one outcome that is
/// neither a grant nor an error.
///
/// Which consent path ran is stamped onto the row: a typed yes records
/// [`EnableAuthority::InteractiveHuman`], `--yes` records
/// [`EnableAuthority::FlagAssertion`]. They are different claims — a person
/// the CLI saw at a terminal versus a claim by whatever process passed the
/// flag — and recording them as one would throw away the difference the
/// consent gate exists for.
pub(crate) fn run_tools_enable_in(
    root: &Path,
    store: &Store,
    name: &str,
    enabled: bool,
    yes: bool,
) -> Result<bool, String> {
    // Before the ledger write, and rendered from the manifest on disk rather
    // than from the name the caller typed: the thing being approved is the
    // script the model will run.
    if enabled && !yes {
        print!("{}", enable_consent_text(root, store, name)?);
        // `true` for the first input: this is a plain text command, so the only
        // questions are whether stdio is a terminal.
        if !crate::interactive::human_is_present(true) {
            return Err(no_terminal_to_ask(name));
        }
        if !confirm_enable(name)? {
            return Ok(false);
        }
    }

    let authority = match (enabled, yes) {
        (false, _) => None,
        (true, true) => Some(EnableAuthority::FlagAssertion),
        // The prompt above ran and was answered yes, at a verified terminal.
        (true, false) => Some(EnableAuthority::InteractiveHuman),
    };
    set_enabled_in(root, store, name, authority)?;
    Ok(true)
}

/// What enabling `name` would grant, in the words of the manifest on disk.
///
/// Rendered from the files rather than the ledger, and from the files rather
/// than the caller's argument: the thing a human is approving is the script the
/// model will run, so that is what is shown. Pure over the workspace so the
/// text is testable without a terminal — the presence check around it is the
/// half a test cannot drive.
pub(crate) fn enable_consent_text(
    root: &Path,
    store: &Store,
    name: &str,
) -> Result<String, String> {
    use std::fmt::Write as _;

    let record = store
        .adopted_foundry_tool(name)
        .map_err(|e| format!("cannot read the adoption ledger: {e}"))?
        .ok_or_else(|| {
            format!(
                "`{name}` has not been adopted — prove it first with \
                 `stella tools --adopt {name}`"
            )
        })?;
    let manifest_path = root.join(ADOPTED_DIR).join(format!("{name}.toml"));
    let script_path = root.join(ADOPTED_DIR).join(format!("{name}.sh"));
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
    let tool = custom::parse_manifest(&text, &manifest_path)?;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "\nEnabling `{name}` offers it to the model every turn."
    );
    let _ = writeln!(out, "\n  description  {}", tool.description);
    let _ = writeln!(out, "  runs         {}", script_path.display());
    let _ = writeln!(out, "  witness      {}", record.witness);
    let _ = writeln!(
        out,
        "\nIt was authored by the foundry, not by you. The witness above is proof that the \
         call fails without it and succeeds with it — it is not proof that the script is \
         safe to run, which is the question only you can answer."
    );
    Ok(out)
}

/// The yes/no on an `--enable`.
///
/// A bare `y`/`yes` and nothing else, matching `plugin install` — anything a
/// caller pipes in that is not an explicit yes is a no.
fn confirm_enable(name: &str) -> Result<bool, String> {
    use std::io::{BufRead as _, Write as _};

    print!("\nEnable `{name}`? [y/N] ");
    std::io::stdout()
        .flush()
        .map_err(|e| format!("cannot write the prompt: {e}"))?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("cannot read your answer: {e}"))?;
    let answer = line.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}

/// Flip one adoption's enablement, refusing to enable a tool whose bytes no
/// longer match the ones the witness ran against. `Some(authority)` enables,
/// saying how the grant was authorised; `None` disables.
///
/// Checking here as well as at discovery is not redundant: the gate withholds
/// a tampered tool silently-but-explainably at session start, whereas someone
/// typing `--enable` is asking a direct question and deserves a direct no.
pub(crate) fn set_enabled_in(
    root: &Path,
    store: &Store,
    name: &str,
    authority: Option<EnableAuthority>,
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

    if authority.is_some() {
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
        .set_foundry_tool_enabled(name, authority)
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
            "  no self-authored tools adopted yet — stage a manifest+script pair under \
             .stella/tools/proposed/, then `--adopt` proves it\n",
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
            "  {} [{state}] {}\n    witness: {}\n",
            row.tool.name, row.tool.signature, row.tool.witness,
        ));
        // Only for enabled rows: the answer to "who approved this, and how?"
        // is a fact about a live grant. `None` on an enabled row is a grant
        // made before the ledger recorded the fact, and says so instead of
        // guessing. The parenthesised tag is the provenance vocabulary
        // (`stella_protocol::provenance::PublicationAuthority`): the
        // strongest authority the recorded path proves.
        if row.tool.enabled {
            out.push_str(&format!(
                "    enabled: {}\n",
                match row.tool.enabled_authority {
                    Some(authority) => format!(
                        "{} (proves {})",
                        authority.describe(),
                        authority.established_authority().as_str()
                    ),
                    None => "unrecorded — turned on before the ledger kept this".to_string(),
                }
            ));
        }
        out.push_str(&format!(
            "    reuse:   {} call(s), {} failed{}\n",
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
pub(crate) fn write_pair(
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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to start runtime: {e}"))?;
    Ok(runtime.block_on(prove_candidate(root, candidate)))
}

/// The witness itself, over the real gated tool surface — the async core
/// both [`run_witness`] and [`adopt_in_async`] share.
async fn prove_candidate(root: &Path, candidate: &CustomTool) -> WitnessVerdict {
    let user_root = crate::paths::user_extension_root();
    // World A is the surface as it really is, so these go through the gate
    // like any other registration — a withheld tool is not part of it.
    let others: Vec<CustomTool> =
        gate_discovery(custom::discover_in(root, user_root.as_deref()), root)
            .tools
            .into_iter()
            .filter(|tool| tool.name != candidate.name)
            .collect();
    let builtins = Builtins;
    let surface = CustomToolSet::new(&builtins, others, root.to_path_buf());
    prove(&surface, candidate, root).await
}

/// Every built-in tool name, advertised and never executed — the "does this
/// capability already exist?" half of world A. Advertising is all the witness
/// reads; a built-in that ran here would be a side effect nobody asked for.
pub(crate) struct Builtins;

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
        ToolOutput::classified_error(
            stella_protocol::ErrorClass::Internal,
            format!("no tool named `{name}` is available"),
        )
    }
}

/// Rule on a discovery scan with this workspace's adoption ledger — the host
/// half of the gate, and the **only** thing in the CLI that turns an
/// [`custom::UngatedDiscovery`] into tools a session can register.
///
/// The type is what makes that true. Discovery hands back a value whose tools
/// cannot be read, so a new discovery path cannot quietly skip this the way the
/// listing and the candidate surface once did — it will not compile.
///
/// Two properties worth stating plainly:
///
/// - **Free for everyone else.** A workspace whose manifests are all
///   hand-written never opens the ledger: `has_foundry_authored` answers `false`
///   and the gate returns without a filesystem touch.
/// - **Fails closed.** If the store cannot be opened, every foundry tool is
///   ruled on against an *empty* ledger and therefore withheld. An unreadable
///   authority record is not permission to skip the authority check — the same
///   posture the gate takes toward a script it cannot digest.
pub(crate) fn gate_discovery(
    found: custom::UngatedDiscovery,
    root: &Path,
) -> custom::DiscoveryReport {
    let adopted = if found.has_foundry_authored() {
        Store::open(root)
            .and_then(|store| store.adopted_foundry_tools())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    stella_tools::foundry_gate::gate_report(found, &adopted, root)
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
