//! `stella storage` and `stella observe` — the read-only doors onto this
//! workspace's own storage: the storage map the pre-write gate enforces
//! (`docs/design/storage-map.md`) and the local Observatory dashboard over
//! the private telemetry stores. Both are offline and need no API key;
//! `storage prune` is the alias onto `stella stats prune`, so retention has
//! exactly one engine.
//!
//! Split verbatim out of `main.rs` — no behavior change — to keep the CLI
//! entry point under the file-size ratchet while later phases grow the
//! subcommand surface. `main.rs` keeps the `Command::Storage` /
//! `Command::Observe` variants (clap needs them on the root enum) and
//! dispatches into this module, the same shape `stats.rs` and
//! `usage_cmd.rs` already use.

use clap::Subcommand;
use colored::Colorize;

#[derive(Subcommand)]
pub enum StorageCmd {
    /// The full map: layer → namespace → relation tree with intents
    Tree,
    /// One relation's card: fields, constraints, meaning, provenance.
    /// Accepts a store:// address, a bare address, or a relation name.
    Show { address: String },
    /// Find relations and fields whose (normalized) name contains the query
    Grep { name: String },
    /// Drift report: near-duplicate relations, orphaned manifest meanings,
    /// and intent coverage. Report-only — nothing is changed.
    Drift,
    /// Bound `store.db`'s growth: drop whole executions and every row keyed to
    /// them, by age and/or a ceiling. Alias for `stella stats prune` — same
    /// flags, same engine. Start with `--dry-run`.
    Prune(crate::stats::PruneArgs),
}

/// `stella observe` — serve the Observatory dashboard for this workspace on
/// `127.0.0.1` until interrupted. Telemetry stores are opened read-only; the
/// page and its assets are embedded, so nothing is fetched from anywhere.
pub(crate) fn preflight_observatory_stores(root: &std::path::Path) -> Result<(), String> {
    for name in ["store.db", "fleet.db", "context.db", "codegraph.db"] {
        stella_store::existing_workspace_private_sqlite_path(root, name)
            .map_err(|e| format!("cannot resolve private Observatory state `{name}`: {e}"))?;
    }
    Ok(())
}

pub fn run_observe(port: u16, open: bool) -> Result<(), String> {
    let root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    preflight_observatory_stores(&root)?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to start runtime: {e}"))?;
    rt.block_on(stella_observatory::serve(root, port, move |addr| {
        let url = format!("http://{addr}/");
        println!();
        // Comet kit v1.0: gold accent (ANSI yellow is the terminal's phosphor
        // gold), lowercase always.
        println!("  {} {}", "◆".yellow(), "stella observatory".bold());
        println!("  {} {}", "→".yellow(), url);
        println!(
            "  {}",
            "reads .stella (read-only) · binds 127.0.0.1 only · Ctrl+C to stop".dimmed()
        );
        if open {
            // Best-effort convenience; the printed URL is the contract.
            let opener = if cfg!(target_os = "macos") {
                "open"
            } else {
                "xdg-open"
            };
            let mut command = std::process::Command::new(opener);
            command.arg(&url);
            stella_tools::subprocess_env::scrub_sensitive_std_env(&mut command);
            let _ = command.spawn();
        }
    }))
    .map_err(|e| e.to_string())
}

/// `stella storage <cmd>` — the human door to the storage map the pre-write
/// gate enforces (docs/design/storage-map.md). Reads the persisted index +
/// stella.storage.toml; empty output means `stella init` hasn't indexed any
/// storage yet.
pub(crate) fn load_storage_snapshot_checked(
    root: &std::path::Path,
) -> Result<stella_graph::StorageSnapshot, String> {
    stella_tools::graph::load_storage_snapshot(root)
        .map_err(|e| format!("cannot resolve private storage-map index: {e}"))
}

pub fn run_storage(cmd: &StorageCmd) -> Result<(), String> {
    use stella_graph::storage::{dedup_key, display_address, embed_card, normalize_name};
    // Retention operates on store.db itself, not on the storage *map*, so it
    // runs ahead of the snapshot load below — a workspace with no
    // stella.storage.toml still has a store.db worth bounding.
    if let StorageCmd::Prune(args) = cmd {
        return crate::stats::run_stats_prune(args);
    }
    let root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let snapshot = load_storage_snapshot_checked(&root)?;
    // A manifest that will not parse contributes nothing — no layers, no
    // boundaries, no intent — so without this the map simply looks emptier
    // than the repo declared it to be, and the pre-write gate is quietly
    // enforcing less than the user believes.
    if let Some(error) = &snapshot.manifest_error {
        eprintln!(
            "{} stella.storage.toml did not parse, so its layers, boundaries and \
             intent are NOT in this map (and the pre-write gate is not enforcing \
             them): {error}",
            "warning:".yellow().bold()
        );
    }
    if snapshot.relations.is_empty() && snapshot.layers.is_empty() {
        println!(
            "{}",
            "storage map is empty — run `stella init` to index the workspace, and/or \
             declare layers in stella.storage.toml"
                .dimmed()
        );
        return Ok(());
    }
    match cmd {
        // Returned above, before the storage map is even loaded — retention
        // operates on `store.db`, not on the map. Exhaustiveness is not
        // flow-sensitive, so the arm has to exist; it is genuinely unreachable.
        StorageCmd::Prune(_) => {
            unreachable!("`stella storage prune` returns before the map loads")
        }
        StorageCmd::Tree => {
            for layer in &snapshot.layers {
                println!(
                    "{} {} {}",
                    "◆".bright_magenta(),
                    layer.key.bold(),
                    format!("({}, {}, {})", layer.engine, layer.class, layer.durability).dimmed()
                );
                if let Some(boundary) = &layer.boundary {
                    println!("  {}", boundary.dimmed());
                }
                let mut namespaces: Vec<&str> = snapshot
                    .relations
                    .iter()
                    .filter(|r| r.layer == layer.key)
                    .map(|r| r.namespace.as_str())
                    .collect();
                namespaces.sort_unstable();
                namespaces.dedup();
                for ns in namespaces {
                    println!("  {}", ns.bright_cyan());
                    for rel in snapshot
                        .relations
                        .iter()
                        .filter(|r| r.layer == layer.key && r.namespace == ns)
                    {
                        let meta = match &rel.intent {
                            Some(intent) => format!(" — {intent}"),
                            None => String::new(),
                        };
                        println!(
                            "    {} {} ({} {}){}",
                            "·".dimmed(),
                            rel.name,
                            rel.fields.len(),
                            if rel.fields.len() == 1 {
                                "field"
                            } else {
                                "fields"
                            },
                            meta.dimmed()
                        );
                    }
                }
            }
        }
        StorageCmd::Show { address } => {
            let needle = address.trim_start_matches("store://");
            let normalized = needle
                .split('/')
                .map(normalize_name)
                .collect::<Vec<_>>()
                .join("/");
            let found = snapshot
                .relations
                .iter()
                .find(|r| r.address == normalized)
                .or_else(|| {
                    // Fall back to a bare relation-name lookup.
                    snapshot
                        .relations
                        .iter()
                        .find(|r| dedup_key(&r.name) == dedup_key(needle))
                })
                .ok_or_else(|| format!("no relation at `{address}` in the storage map"))?;
            let layer = snapshot.layers.iter().find(|l| l.key == found.layer);
            print!("{}", embed_card(layer, found));
        }
        StorageCmd::Grep { name } => {
            let needle = normalize_name(name);
            let mut hits = 0usize;
            for rel in &snapshot.relations {
                if normalize_name(&rel.name).contains(&needle) {
                    hits += 1;
                    println!(
                        "{} ({}){}",
                        display_address(&rel.address).bold(),
                        rel.kind,
                        rel.source
                            .as_deref()
                            .map(|s| format!("  {s}"))
                            .unwrap_or_default()
                            .dimmed()
                    );
                }
                for field in &rel.fields {
                    if normalize_name(&field.name).contains(&needle) {
                        hits += 1;
                        println!(
                            "{}/{} {}",
                            display_address(&rel.address),
                            field.name.bold(),
                            field.data_type.as_deref().unwrap_or("").dimmed()
                        );
                    }
                }
            }
            if hits == 0 {
                println!("{}", format!("no storage object matches `{name}`").dimmed());
            }
        }
        StorageCmd::Drift => {
            let mut findings = 0usize;
            // Near-duplicate relations: same layer, similar token sets.
            for (i, a) in snapshot.relations.iter().enumerate() {
                for b in snapshot.relations.iter().skip(i + 1) {
                    if a.layer != b.layer || dedup_key(&a.name) == dedup_key(&b.name) {
                        continue;
                    }
                    let key_a = dedup_key(&a.name);
                    let key_b = dedup_key(&b.name);
                    let set_a: std::collections::HashSet<&str> =
                        key_a.split('_').filter(|t| !t.is_empty()).collect();
                    let set_b: std::collections::HashSet<&str> =
                        key_b.split('_').filter(|t| !t.is_empty()).collect();
                    if set_a.is_empty() || set_b.is_empty() {
                        continue;
                    }
                    let overlap = set_a.intersection(&set_b).count() as f64
                        / set_a.union(&set_b).count() as f64;
                    if overlap >= 0.5 {
                        findings += 1;
                        println!(
                            "{} {} ≈ {}",
                            "near-duplicate:".yellow().bold(),
                            display_address(&a.address),
                            display_address(&b.address)
                        );
                    }
                }
            }
            for orphan in &snapshot.orphaned_meanings {
                findings += 1;
                println!(
                    "{} {} {}",
                    "orphaned meaning:".yellow().bold(),
                    display_address(orphan),
                    "(manifest entry with no parsed entity — entity deleted, or address typo)"
                        .dimmed()
                );
            }
            let with_intent = snapshot
                .relations
                .iter()
                .filter(|r| r.intent.is_some())
                .count();
            println!(
                "{} {}/{} relations carry an intent sentence",
                "coverage:".bold(),
                with_intent,
                snapshot.relations.len()
            );
            if findings == 0 {
                println!("{}", "no drift signals".dimmed());
            }
        }
    }
    Ok(())
}
