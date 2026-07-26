//! `stella memory` — the human window into the memory-citation loop.
//!
//! `list` ranks the project's memories most-cited first, joining the
//! citation feedback the agent recorded (`cite_memory` → `.stella/private/store.db`)
//! with the memories themselves (`.stella/private/context.db`), and flags the ones
//! that have earned rule promotion. `promote <id>` turns an eligible memory
//! into a project rule at `.stella/rules/<slug>.md` — the rules engine's own
//! authoring format (`stella_core::rules`), so the promoted file is loaded
//! and parsed exactly like a hand-written rule.
//!
//! Like `stella stats`, both subcommands read local state only and need no
//! API key; `list` never creates a database as a side effect. Promotion is
//! deliberately explicit and human-invoked: eligibility is computed
//! automatically, the write only happens here.

use clap::{Subcommand, ValueEnum};
use colored::Colorize;
use serde::Serialize;
use stella_context::{ContextStore, NodeKind, NodeRow};
use stella_core::rules::{self, PromoteStatus, RuleCandidate};
use stella_store::{ContextSurface, MemoryCitationStats, PROMOTION_CITATIONS_REQUIRED, Store};

/// `stella memory` subcommands — the inspection and promotion surface of the
/// memory-citation loop (agents cite the memories that informed a turn via
/// the `cite_memory` tool; the citations aggregate into the eligibility gate
/// `promote` enforces).
#[derive(Subcommand)]
pub enum MemoryCmd {
    /// List memories ranked by citation count, with average usefulness,
    /// truthfulness rate, and rule-promotion eligibility
    List {
        /// Output format: table (aligned) or json
        #[arg(long, value_enum, default_value = "table")]
        format: MemoryFormat,
    },
    /// Promote an eligible memory to a project rule at
    /// `.stella/rules/<slug>.md`. Eligibility is strict: cited successfully
    /// MORE THAN 10 consecutive times since its last negative remark — one
    /// negative citation resets the count until it is re-earned.
    Promote {
        /// The memory's stable id (nod_…) as shown by `stella memory list`
        id: String,
    },
    /// Re-validate old memories against the current codebase (Proposal 5).
    /// Scans each memory for file-path anchors (e.g. `stella-cli/src/agent.rs`),
    /// checks whether those paths still exist, and flags stale ones. A stale
    /// memory is one whose referenced path no longer exists — a strong signal
    /// the memory is about refactored-away code and may mislead.
    Validate,
    /// Forget a memory: stop it steering the agent, and stop the reflection
    /// loop re-learning it. Reversible with `stella memory restore <id>`.
    ///
    /// This is a tombstone, not a delete. The reflection loop re-mines
    /// paraphrases of lessons it has already learned, so removing the row
    /// alone does not hold — the tombstone also suppresses restatements at
    /// the point new lessons are recorded and at the point the log is mined
    /// into skills.
    Forget {
        /// The memory's stable id (nod_…) as shown by `stella memory list`
        id: String,
        /// Optional note on why, shown by `stella memory forgotten`
        #[arg(long, default_value = "")]
        reason: String,
    },
    /// Rewrite a memory in place, as a new revision of the same memory.
    ///
    /// The old text becomes history rather than a competitor: one live record,
    /// the same id, and recall stops serving the words you replaced. Before
    /// memories had a lineage, changing one meant writing a second memory and
    /// leaving the first live — both citable, with nothing to say which was
    /// current.
    Edit {
        /// The memory's stable id (nod_…) as shown by `stella memory list`
        id: String,
        /// The replacement text
        text: String,
    },
    /// Lift a tombstone written by `stella memory forget`, letting the memory
    /// be recalled again and be re-learnable.
    Restore {
        /// The memory's stable id (nod_…) as shown by `stella memory forgotten`
        id: String,
    },
    /// List the tombstones in this workspace — what was forgotten, when, and
    /// why.
    Forgotten,
}

/// Output format for `stella memory list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MemoryFormat {
    /// Aligned human-readable columns (default).
    Table,
    /// Pretty-printed JSON array of per-memory rows.
    Json,
}

/// One memory in the inspection view: the stable id citations tie to, the
/// citation aggregates (zeroed when never cited), and the memory text.
/// Field order is the `--format json` serialization contract — append new
/// fields at the end, never reorder.
#[derive(Debug, Clone, Serialize)]
struct MemoryListRow {
    id: String,
    /// `memory` (a mined reflection) or `episode` (a verbatim past prompt).
    /// Episodes are listed because they are recalled and injected exactly like
    /// memories — omitting them hid a whole class of steering input from the
    /// only command that can name it for `stella memory forget`.
    kind: String,
    citations: i64,
    avg_score: f64,
    truthful_rate: f64,
    positive_streak: i64,
    eligible: bool,
    #[serde(skip_serializing_if = "is_false")]
    quarantined: bool,
    memory: String,
}

fn is_false(b: &bool) -> bool {
    !b
}

/// Entry point for `stella memory list`.
pub fn run_memory_list(format: MemoryFormat) -> Result<(), String> {
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let rows = list_rows(&workspace_root)?;

    if rows.is_empty() {
        let message = "No memories recorded yet — run `stella chat`/`stella run` and let the \
                       post-turn reflection populate .stella/private/context.db.";
        match format {
            MemoryFormat::Table => println!("{message}"),
            MemoryFormat::Json => {
                eprintln!("{message}");
                println!("[]");
            }
        }
        return Ok(());
    }

    match format {
        MemoryFormat::Table => {
            print!("{}", render_table(&rows));
            let eligible = rows.iter().filter(|r| r.eligible).count();
            if eligible > 0 {
                println!(
                    "\n{} {eligible} memory(ies) eligible for rule promotion — \
                     `stella memory promote <id>` writes .stella/rules/<slug>.md",
                    "✦".magenta()
                );
            }
            let quarantined = rows.iter().filter(|r| r.quarantined).count();
            if quarantined > 0 {
                println!(
                    "\n{} {quarantined} quarantined memor{them} excluded from recall — \
                     cited untruthful {threshold}+ times. Review with `stella memory list --json`.",
                    "⚠".yellow(),
                    them = if quarantined == 1 { "y" } else { "ies" },
                    threshold = stella_store::QUARANTINE_NEGATIVES_THRESHOLD,
                );
            }
        }
        MemoryFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&rows).map_err(|e| format!("serialize: {e}"))?
        ),
    }
    Ok(())
}

/// The inspection rows for a workspace — [`run_memory_list`] minus the cwd
/// resolution and rendering, so tests can drive it against a temp root.
/// Read-only politeness (the `stella stats` discipline): opening either
/// store would create `.stella/` and empty databases as a side effect, so
/// a missing file reads as "nothing yet", never a write.
fn list_rows(workspace_root: &std::path::Path) -> Result<Vec<MemoryListRow>, String> {
    let Some(context_db) =
        stella_store::existing_workspace_private_sqlite_path(workspace_root, "context.db")
            .map_err(|e| format!("cannot resolve context store: {e}"))?
    else {
        return Ok(Vec::new());
    };
    let context =
        ContextStore::open(&context_db).map_err(|e| format!("cannot open context store: {e}"))?;
    // Episodes as well as memories: both are recalled and injected, so both
    // must be nameable for `stella memory forget`.
    let memories = context
        .recallable_nodes()
        .map_err(|e| format!("cannot read memories: {e}"))?;
    let stats = citation_stats(workspace_root)?;
    Ok(join_rows(memories, &stats))
}

/// Entry point for `stella memory promote <id>`.
pub fn run_memory_promote(id: &str) -> Result<(), String> {
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    promote_in(&workspace_root, id)
}

/// The promotion flow for a workspace — [`run_memory_promote`] minus the cwd
/// resolution, so tests can drive it end-to-end against a temp root.
fn promote_in(workspace_root: &std::path::Path, id: &str) -> Result<(), String> {
    let stats = citation_stats(workspace_root)?;
    let Some(stat) = stats.iter().find(|s| s.memory_id == id) else {
        return Err(format!(
            "memory `{id}` has never been cited — only memories the agent cited successfully \
             more than {PROMOTION_CITATIONS_REQUIRED} times can be promoted \
             (`stella memory list` shows citation counts)"
        ));
    };
    if !stat.eligible {
        return Err(format!(
            "memory `{id}` is not promotion-eligible: {} consecutive positive citation(s) since \
             its last negative remark, and strictly more than {PROMOTION_CITATIONS_REQUIRED} are \
             required ({} citation(s) total, {} negative). One negative remark resets the streak \
             until it is re-earned.",
            stat.positive_streak, stat.citations, stat.negatives
        ));
    }

    let context_db =
        stella_store::existing_workspace_private_sqlite_path(workspace_root, "context.db")
            .map_err(|e| format!("cannot resolve context store: {e}"))?
            .ok_or_else(|| {
                "no private context.db in this workspace — nothing to promote".to_string()
            })?;
    let context =
        ContextStore::open(&context_db).map_err(|e| format!("cannot open context store: {e}"))?;
    let node = context
        .node_by_public_id(id)
        .map_err(|e| format!("cannot read memory `{id}`: {e}"))?
        .ok_or_else(|| format!("memory `{id}` is not in the context store (cited id is stale?)"))?;
    if node.kind != NodeKind::Memory {
        return Err(format!(
            "`{id}` is a {} node, not a memory — only memories can become rules",
            node.kind.as_str()
        ));
    }

    let candidate = promotion_candidate(&node, stat);
    let rules_dir = workspace_root.join(".stella").join("rules");
    let path = rules_dir.join(format!("{}.md", candidate.id));
    // The pure promotion decision is the rules engine's (`decide_promotion`);
    // this command owns only the I/O halves: the exists-check and the write.
    match rules::decide_promotion(true, path.exists()) {
        PromoteStatus::AlreadyExists => Err(format!(
            "{} already exists — not clobbering a possibly hand-edited rule; delete it first \
             to re-promote",
            path.display()
        )),
        PromoteStatus::Declined => unreachable!("promote is always approved here"),
        PromoteStatus::Written => {
            std::fs::create_dir_all(&rules_dir)
                .map_err(|e| format!("could not create {}: {e}", rules_dir.display()))?;
            std::fs::write(&path, rules::render_rule_markdown(&candidate))
                .map_err(|e| format!("could not write {}: {e}", path.display()))?;
            println!(
                "  {} promoted memory {} → {}",
                "✦".magenta().bold(),
                id.bright_magenta(),
                path.display()
            );
            println!(
                "  {}",
                "the rule now loads with the workspace rules (.stella/rules/)".dimmed()
            );
            Ok(())
        }
    }
}

/// Citation stats from `.stella/private/store.db`, or empty when the store does not
/// exist yet (never create it from a read-only command).
fn citation_stats(workspace_root: &std::path::Path) -> Result<Vec<MemoryCitationStats>, String> {
    if stella_store::existing_workspace_private_sqlite_path(workspace_root, "store.db")
        .map_err(|e| format!("cannot resolve local store: {e}"))?
        .is_none()
    {
        return Ok(Vec::new());
    }
    let store = Store::open(workspace_root).map_err(|e| format!("cannot open local store: {e}"))?;
    store
        .memory_citation_stats()
        .map_err(|e| format!("cannot read memory citations: {e}"))
}

/// Join memories with their citation stats: cited memories first (already
/// most-cited-first from the store), then never-cited ones newest first (the
/// order `memory_nodes` returns). Stats for ids that no longer resolve to a
/// live memory node (superseded, or a fabricated citation) are dropped — the
/// view shows the project's memories, not raw citation rows.
fn join_rows(memories: Vec<NodeRow>, stats: &[MemoryCitationStats]) -> Vec<MemoryListRow> {
    let mut cited: Vec<MemoryListRow> = Vec::new();
    let mut uncited: Vec<MemoryListRow> = Vec::new();
    for node in &memories {
        match stats.iter().find(|s| s.memory_id == node.public_id) {
            Some(s) => cited.push(MemoryListRow {
                id: node.public_id.clone(),
                kind: node.kind.as_str().to_string(),
                citations: s.citations,
                avg_score: s.avg_score,
                truthful_rate: s.truthful_rate,
                positive_streak: s.positive_streak,
                eligible: s.eligible,
                quarantined: s.quarantined,
                memory: node.content.trim().to_string(),
            }),
            None => uncited.push(MemoryListRow {
                id: node.public_id.clone(),
                kind: node.kind.as_str().to_string(),
                citations: 0,
                avg_score: 0.0,
                truthful_rate: 0.0,
                positive_streak: 0,
                eligible: false,
                quarantined: false,
                memory: node.content.trim().to_string(),
            }),
        }
    }
    cited.sort_by(|a, b| b.citations.cmp(&a.citations).then_with(|| a.id.cmp(&b.id)));
    cited.extend(uncited);
    cited
}

/// One row in the validation report (Proposal 5).
#[derive(Debug, Clone, Serialize)]
struct MemoryValidationRow {
    id: String,
    status: &'static str,
    anchors_found: usize,
    anchors_missing: usize,
    memory: String,
}

/// Entry point for `stella memory validate` (Proposal 5).
///
/// Scans each memory for file-path anchors (workspace-relative paths like
/// `stella-cli/src/agent.rs`), checks whether those paths still exist, and
/// reports stale memories — ones whose referenced files have been moved or
/// deleted, a strong signal the memory is about refactored-away code.
pub fn run_memory_validate() -> Result<(), String> {
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let rows = validate_memories(&workspace_root)?;
    if rows.is_empty() {
        println!("no memories to validate — .stella/private/context.db is empty or missing");
        return Ok(());
    }

    let stale = rows.iter().filter(|r| r.status == "stale").count();
    let ok = rows.iter().filter(|r| r.status == "ok").count();
    let no_anchors = rows.iter().filter(|r| r.status == "no-anchors").count();

    for row in &rows {
        let symbol = match row.status {
            "stale" => "⚠".yellow(),
            "ok" => "✓".green(),
            _ => "·".dimmed(),
        };
        let detail = match row.status {
            "stale" => format!(
                " — {} of {} anchor path(s) missing",
                row.anchors_missing, row.anchors_found
            ),
            "ok" => format!(" — {} anchor(s) verified", row.anchors_found),
            _ => String::new(),
        };
        println!(
            "  {symbol} {} [{status}{detail}] {memory}",
            row.id,
            status = row.status,
            memory = row.memory.chars().take(60).collect::<String>()
        );
    }

    println!("\n  {ok} ok, {stale} stale, {no_anchors} no path anchors");
    if stale > 0 {
        println!(
            "\n  {} {stale} stale memor{them} reference paths that no longer exist — \
             cite them untruthful (or delete) to quarantine them from recall.",
            "⚠".yellow(),
            them = if stale == 1 { "y" } else { "ies" },
        );
    }
    report_duplicate_lineages(&workspace_root)?;
    Ok(())
}

/// Report memories that look like revisions of one another — never merge them.
///
/// Before #712 a memory's identity was the hash of its content, so rewording
/// one produced a *second* memory and left the first live. Those pairs are
/// still in every workspace that ran the old code, and the lineage migration
/// deliberately did not repair them: two similar memories and one memory
/// edited twice are indistinguishable without a judgment about meaning, and a
/// migration is the wrong place to make one — it runs unattended, on the only
/// copy a user has. So this reports, and a person decides (#711, decision 4).
///
/// The similarity test is the one the tombstone path already uses, so
/// "restatement" means the same thing here as it does when the reflection loop
/// declines to re-learn something forgotten.
fn report_duplicate_lineages(workspace_root: &std::path::Path) -> Result<(), String> {
    let Some(context) = open_context(workspace_root)? else {
        return Ok(());
    };
    let memories = context
        .memory_nodes()
        .map_err(|e| format!("cannot read memories: {e}"))?;

    // O(n²) over live memories, which is a report a person runs, not a path a
    // turn takes — and `memory_nodes` is already the whole live set.
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut assigned = vec![false; memories.len()];
    for i in 0..memories.len() {
        if assigned[i] {
            continue;
        }
        let mut group = vec![i];
        for j in (i + 1)..memories.len() {
            if assigned[j] {
                continue;
            }
            if stella_store::is_restatement(&memories[j].content, &memories[i].content) {
                assigned[j] = true;
                group.push(j);
            }
        }
        if group.len() > 1 {
            assigned[i] = true;
            groups.push(group);
        }
    }
    if groups.is_empty() {
        return Ok(());
    }

    println!(
        "\n  {} {} group{} of memories say close to the same thing. Editing a memory used to \
         mint a second one and leave the first live; these are what that left behind.",
        "⚠".yellow(),
        groups.len(),
        if groups.len() == 1 { "" } else { "s" }
    );
    for group in &groups {
        println!();
        for (rank, &idx) in group.iter().enumerate() {
            let node = &memories[idx];
            let marker = if rank == 0 { "keep?" } else { "dupe?" };
            println!(
                "    {} {} {}",
                marker.dimmed(),
                node.public_id,
                clip(node.content.trim(), 58).dimmed()
            );
        }
    }
    println!(
        "\n  {}",
        "Nothing was merged. Consolidate with `stella memory edit <keep-id> \"<text>\"` and \
         `stella memory forget <dupe-id>` — both reversible."
            .dimmed()
    );
    Ok(())
}

/// Entry point for `stella memory forget <id>`.
///
/// Resolves the id to its text first and stores that alongside the
/// tombstone. The copy is the whole point: the reflection loop re-mines
/// paraphrases under fresh ids, so suppressing only `id` would hold until
/// the next reflection turn. Forgetting an id that is not live is still
/// allowed — a memory may have gone by other means and the user may want the
/// tombstone anyway — but it is reported, because the usual cause is a
/// typo'd id that would otherwise silently do nothing.
pub fn run_memory_forget(id: &str, reason: &str) -> Result<(), String> {
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;

    let content = memory_text(&workspace_root, id)?;
    if content.is_none() {
        eprintln!(
            "  {} `{id}` is not a live memory — recording the tombstone anyway, but check the id \
             against `stella memory list` if you expected a match",
            "!".yellow()
        );
    }
    let content = content.unwrap_or_default();

    let store = Store::open(&workspace_root).map_err(|e| format!("cannot open store: {e}"))?;
    store
        .forget(ContextSurface::Memory, id, &content, reason)
        .map_err(|e| format!("cannot record tombstone: {e}"))?;

    // Project the tombstone into the plane that owns the memory, so recall
    // stops offering it at the SQL boundary instead of at the CLI after a
    // budget has already been spent on it (#712 deliverable 4). The tombstone
    // in `store.db` stays canonical — it is surface-aware, carries the reason,
    // and outlives the row — so this is a projection, not a second source of
    // truth, and it is written second: a failure here leaves the tombstone
    // recorded and the memory merely still visible, which is recoverable, where
    // the opposite order could hide a memory with no record of why.
    if let Some(context) = open_context(&workspace_root)? {
        context
            .supersede_node(id)
            .map_err(|e| format!("cannot suppress `{id}` in the context plane: {e}"))?;
    }

    println!("  {} forgot {id}", "✓".green());
    if !content.is_empty() {
        println!("    {}", clip(content.trim(), 72).dimmed());
        println!(
            "    {}",
            "restatements of this will also be suppressed when the loop reflects".dimmed()
        );
    }
    println!(
        "    {}",
        format!("undo: stella memory restore {id}").dimmed()
    );
    Ok(())
}

/// Entry point for `stella memory edit <id> <text>` — #712 deliverable 5.
///
/// Writes a new revision of the memory's lineage. The mirror node is keyed by
/// lineage, so it is updated in place: one live record, the same `nod_…` id,
/// and the old text kept as history rather than left competing for recall
/// slots.
///
/// Before lineage, this operation did not exist and could not be built. A
/// memory's identity was the hash of its content, so "the same memory with
/// different words" was a contradiction — you got a second memory, and the
/// first went on being recalled with its old text and its own vector.
pub fn run_memory_edit(id: &str, text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err(
            "the replacement text must not be empty — use `stella memory forget` to \
                    remove a memory"
                .to_string(),
        );
    }
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let Some(context) = open_context(&workspace_root)? else {
        return Err("this workspace has no context store yet — nothing to edit".to_string());
    };
    let Some(node) = context
        .node_by_public_id(id)
        .map_err(|e| format!("cannot read memory `{id}`: {e}"))?
    else {
        return Err(format!(
            "`{id}` is not a live memory — check the id against `stella memory list`"
        ));
    };
    let Some(lineage) = context
        .memory_lineage(id)
        .map_err(|e| format!("cannot resolve the lineage of `{id}`: {e}"))?
    else {
        return Err(format!(
            "`{id}` is not a memory — only memories have revisions (episodes are a record \
             of what happened and are not rewritten)"
        ));
    };
    let previous = node.content.clone();
    // Carry the lineage's classification forward. Defaulting here would
    // silently reclassify a mined reflection as an authored note, which changes
    // whether the restatement filter treats it as regenerable.
    let kind = context
        .memory_kind(&lineage)
        .map_err(|e| format!("cannot read the kind of `{id}`: {e}"))?
        .and_then(|k| stella_context::MemoryKind::parse(&k))
        .unwrap_or(stella_context::MemoryKind::Note);

    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| format!("cannot start the async runtime: {e}"))?;
    runtime
        .block_on(async {
            context
                .upsert(stella_context::ContextDelta::new().with_memory(
                    stella_context::MemoryInput::new(kind, text.trim()).revises(&lineage),
                ))
                .await
        })
        .map_err(|e| format!("cannot write the revision: {e}"))?;

    println!("  {} revised {id}", "✓".green());
    println!(
        "    {} {}",
        "was:".dimmed(),
        clip(previous.trim(), 68).dimmed()
    );
    println!("    {} {}", "now:".dimmed(), clip(text.trim(), 68).dimmed());
    let stats = context
        .memory_lineage_stats()
        .map_err(|e| format!("cannot read lineage stats: {e}"))?;
    println!(
        "    {}",
        format!(
            "{} live {}, {} superseded revision{} kept as history",
            stats.live,
            if stats.live == 1 {
                "memory"
            } else {
                "memories"
            },
            stats.superseded,
            if stats.superseded == 1 { "" } else { "s" }
        )
        .dimmed()
    );
    Ok(())
}

/// Entry point for `stella memory restore <id>`.
pub fn run_memory_restore(id: &str) -> Result<(), String> {
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let store = Store::open(&workspace_root).map_err(|e| format!("cannot open store: {e}"))?;
    let lifted = store
        .restore(ContextSurface::Memory, id)
        .map_err(|e| format!("cannot lift tombstone: {e}"))?;
    // The exact inverse of what `forget` projected. Run unconditionally rather
    // than only when `lifted`: the two writes can disagree if a forget failed
    // halfway, and a restore that refuses to fix that is a memory no command
    // can bring back.
    if let Some(context) = open_context(&workspace_root)? {
        context
            .restore_node(id)
            .map_err(|e| format!("cannot lift `{id}` in the context plane: {e}"))?;
    }
    if lifted {
        println!("  {} restored {id}", "✓".green());
    } else {
        println!(
            "  {} `{id}` was not forgotten — nothing to restore",
            "·".dimmed()
        );
    }
    Ok(())
}

/// Entry point for `stella memory forgotten` — the tombstones in this
/// workspace. Read-only politeness like `list`: a workspace with no store
/// reads as "nothing forgotten", never a created database.
pub fn run_memory_forgotten() -> Result<(), String> {
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    if stella_store::existing_workspace_private_sqlite_path(&workspace_root, "store.db")
        .map_err(|e| format!("cannot resolve store: {e}"))?
        .is_none()
    {
        println!("nothing forgotten in this workspace");
        return Ok(());
    }
    let store = Store::open(&workspace_root).map_err(|e| format!("cannot open store: {e}"))?;
    let rows = store
        .forgotten_rows()
        .map_err(|e| format!("cannot read tombstones: {e}"))?;
    if rows.is_empty() {
        println!("nothing forgotten in this workspace");
        return Ok(());
    }
    for row in &rows {
        println!(
            "  {} {:<16} {:<30} {}",
            "·".dimmed(),
            row.surface,
            row.item_id,
            clip(row.content.trim(), 56).dimmed()
        );
        if !row.reason.is_empty() {
            println!("      {}", row.reason.dimmed());
        }
    }
    println!(
        "\n  {} forgotten — `stella memory restore <id>` lifts one",
        rows.len()
    );
    Ok(())
}

/// First `max` characters, ellipsised. Char-safe: a memory containing
/// multi-byte text must not panic a listing on a byte-boundary slice.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut clipped: String = text.chars().take(max.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}

/// The context plane for this workspace, or `None` when the workspace has none
/// yet. Read-only politeness: an absent `context.db` is "nothing indexed here",
/// never a database this command creates as a side effect.
fn open_context(workspace_root: &std::path::Path) -> Result<Option<ContextStore>, String> {
    let Some(context_db) =
        stella_store::existing_workspace_private_sqlite_path(workspace_root, "context.db")
            .map_err(|e| format!("cannot resolve context store: {e}"))?
    else {
        return Ok(None);
    };
    ContextStore::open(&context_db)
        .map(Some)
        .map_err(|e| format!("cannot open context store: {e}"))
}

/// The text of a memory by public id, or `None` when no such memory is live.
fn memory_text(workspace_root: &std::path::Path, id: &str) -> Result<Option<String>, String> {
    let Some(context) = open_context(workspace_root)? else {
        return Ok(None);
    };
    let node = context
        .node_by_public_id(id)
        .map_err(|e| format!("cannot read memory `{id}`: {e}"))?;
    Ok(node.map(|n| n.content))
}

/// Source extensions a token must end in to count as a file reference.
const ANCHOR_EXTS: &[&str] = &[
    "rs", "py", "ts", "tsx", "js", "jsx", "go", "md", "sql", "toml",
];

/// Extract file anchors from memory text. Two shapes count:
///
/// * **Rooted paths** — `word/word[/word…]` with a recognized source
///   extension, e.g. `stella-cli/src/agent.rs`, `docs/context-reuse.md`.
///   These resolve against the workspace root directly.
/// * **Bare filenames** — a name with a recognized extension and no
///   directory at all, e.g. `slash_models_witness.rs`, `Cargo.toml`.
///
/// Bare filenames matter because that is how reflections actually name
/// files: a lesson mined from a turn says "in `slash_models_witness.rs`",
/// not "in `stella-cli/tests/slash_models_witness.rs`". Requiring a slash
/// meant the memories most likely to rot — the ones naming a single test
/// file that later got deleted — were reported as `no-anchors` and never
/// checked, which is the failure this function exists to catch.
fn extract_path_anchors(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '/' || c == '.' || c == '-' || c == '_'))
        .filter_map(|tok| {
            // Trim trailing dots — a sentence like "see src/lib.rs." would
            // otherwise produce token "src/lib.rs." with extension "rs.".
            let tok = tok.trim_end_matches('.');
            if tok.len() > 256 || tok.starts_with('/') || tok.split('/').any(|seg| seg == "..") {
                return None;
            }
            let (stem, ext) = tok.rsplit_once('.')?;
            // A bare `.rs` has no stem and names nothing; `graph_snapshot`
            // has no extension. Both must stay out.
            if stem.is_empty() || !ANCHOR_EXTS.contains(&ext) {
                return None;
            }
            Some(tok.to_string())
        })
        .collect()
}

/// Directories that never hold source a memory would meaningfully anchor to,
/// and that dominate the walk cost when present.
const ANCHOR_WALK_SKIP: &[&str] = &[
    ".git",
    ".stella",
    "target",
    "node_modules",
    ".next",
    ".venv",
    "venv",
    "dist",
    "build",
    "__pycache__",
];

/// Every file name present anywhere under the workspace, for resolving
/// bare-filename anchors — they carry no directory to join onto the root, so
/// existence is a "does the tree contain a file by this name" question.
///
/// Built once per `validate` run and bounded: a pathological or symlinked
/// tree must not stall an inspection command.
fn workspace_file_names(root: &std::path::Path) -> std::collections::HashSet<String> {
    const MAX_FILES: usize = 200_000;
    let mut names = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::from([root.to_path_buf()]);
    while let Some(dir) = queue.pop_front() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            // `is_dir`/`is_file` on the entry type does not follow symlinks,
            // so a self-referential link cannot re-enter the walk.
            if kind.is_dir() {
                if !ANCHOR_WALK_SKIP.contains(&name.as_str()) {
                    queue.push_back(entry.path());
                }
            } else if kind.is_file() {
                names.insert(name);
                if names.len() >= MAX_FILES {
                    return names;
                }
            }
        }
    }
    names
}

/// Validate all memories in a workspace against the current file tree.
fn validate_memories(workspace_root: &std::path::Path) -> Result<Vec<MemoryValidationRow>, String> {
    let Some(context_db) =
        stella_store::existing_workspace_private_sqlite_path(workspace_root, "context.db")
            .map_err(|e| format!("cannot resolve context store: {e}"))?
    else {
        return Ok(Vec::new());
    };
    let context =
        ContextStore::open(&context_db).map_err(|e| format!("cannot open context store: {e}"))?;
    // Episodes as well as memories: both are recalled and injected, so both
    // must be nameable for `stella memory forget`.
    let memories = context
        .recallable_nodes()
        .map_err(|e| format!("cannot read memories: {e}"))?;

    // Built lazily: a workspace whose memories name no bare filenames never
    // pays for the walk.
    let mut file_names: Option<std::collections::HashSet<String>> = None;

    let mut rows = Vec::new();
    for node in &memories {
        let anchors = extract_path_anchors(&node.content);
        if anchors.is_empty() {
            rows.push(MemoryValidationRow {
                id: node.public_id.clone(),
                status: "no-anchors",
                anchors_found: 0,
                anchors_missing: 0,
                memory: node.content.trim().to_string(),
            });
            continue;
        }
        let missing = anchors
            .iter()
            .filter(|anchor| {
                if anchor.contains('/') {
                    !workspace_root.join(anchor).exists()
                } else {
                    !file_names
                        .get_or_insert_with(|| workspace_file_names(workspace_root))
                        .contains(anchor.as_str())
                }
            })
            .count();
        let status = if missing > 0 { "stale" } else { "ok" };
        rows.push(MemoryValidationRow {
            id: node.public_id.clone(),
            status,
            anchors_found: anchors.len(),
            anchors_missing: missing,
            memory: node.content.trim().to_string(),
        });
    }
    Ok(rows)
}

/// The promoted rule as a mining candidate, so `render_rule_markdown`
/// produces the exact frontmatter shape `rule_from_file` parses back —
/// promotion never invents its own rule format. Prompt-only (no guard): the
/// citation loop scores usefulness, it carries no file evidence to infer a
/// safe deny-glob from.
fn promotion_candidate(node: &NodeRow, stat: &MemoryCitationStats) -> RuleCandidate {
    RuleCandidate {
        id: slugify(&node.display_name),
        description: format!(
            "Promoted from memory {} after {} citation(s) ({} consecutive positive).",
            node.public_id, stat.citations, stat.positive_streak
        ),
        text: node.content.trim().to_string(),
        occurrences: stat.citations as usize,
        salient: false,
        evidence: Vec::new(),
        guard: None,
        score: 0,
    }
}

/// Filename-safe slug from a memory's label: lowercase alphanumeric runs
/// joined by `-`, capped at 48 chars, `memory-rule` when nothing survives.
fn slugify(label: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for c in label.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(c.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
        if slug.len() >= 48 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() {
        "memory-rule".to_string()
    } else {
        slug
    }
}

/// Column headers, in `MemoryListRow` field order (`memory` last, so the
/// one unbounded column never breaks alignment).
const TABLE_HEADERS: [&str; 8] = [
    "ID", "KIND", "CITES", "AVG", "TRUTHFUL", "STREAK", "STATUS", "MEMORY",
];

fn table_cells(row: &MemoryListRow) -> [String; 8] {
    let mut memory: String = row.memory.chars().take(60).collect();
    if row.memory.chars().count() > 60 {
        memory.push('…');
    }
    // Keep the free-text column single-line so alignment holds.
    let memory = memory.replace(['\n', '\r'], " ");
    let status = if row.quarantined {
        "QUARANTINED"
    } else if row.eligible {
        "eligible"
    } else {
        "-"
    };
    [
        row.id.clone(),
        row.kind.clone(),
        row.citations.to_string(),
        if row.citations > 0 {
            format!("{:.1}", row.avg_score)
        } else {
            "-".into()
        },
        if row.citations > 0 {
            format!("{:.0}%", row.truthful_rate * 100.0)
        } else {
            "-".into()
        },
        row.positive_streak.to_string(),
        status.to_string(),
        memory,
    ]
}

/// Aligned table, `stella stats` style: left-aligned strings, right-aligned
/// numbers, two-space gutter, no trailing whitespace.
fn render_table(rows: &[MemoryListRow]) -> String {
    let body: Vec<[String; 8]> = rows.iter().map(table_cells).collect();
    let mut widths: Vec<usize> = TABLE_HEADERS.iter().map(|h| h.len()).collect();
    for cells in &body {
        for (w, cell) in widths.iter_mut().zip(cells.iter()) {
            *w = (*w).max(cell.len());
        }
    }
    let render_line = |cells: &[String]| -> String {
        let cols: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                // Numeric middle columns right-align; ID, KIND and MEMORY
                // stay left.
                if (2..=5).contains(&i) {
                    format!("{cell:>width$}", width = widths[i])
                } else {
                    format!("{cell:<width$}", width = widths[i])
                }
            })
            .collect();
        cols.join("  ").trim_end().to_string()
    };
    let mut out = String::new();
    let headers: Vec<String> = TABLE_HEADERS.iter().map(|h| h.to_string()).collect();
    out.push_str(&render_line(&headers));
    out.push('\n');
    for cells in &body {
        out.push_str(&render_line(cells));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn memory_node(public_id: &str, label: &str, content: &str) -> NodeRow {
        NodeRow {
            id: 1,
            public_id: public_id.into(),
            kind: NodeKind::Memory,
            display_name: label.into(),
            content: content.into(),
            content_hash: "h".into(),
            uri: None,
            valid_from: None,
            recorded_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn stat(memory_id: &str, citations: i64, streak: i64, eligible: bool) -> MemoryCitationStats {
        MemoryCitationStats {
            memory_id: memory_id.into(),
            citations,
            avg_score: 4.5,
            truthful_rate: 1.0,
            negatives: 0,
            positive_streak: streak,
            eligible,
            quarantined: false,
        }
    }

    #[test]
    fn promoted_markdown_is_parsed_back_by_the_rules_engine() {
        let node = memory_node(
            "nod_0123456789abcdef01234567",
            "Never edit generated files",
            "Never edit generated files under gen/ — regenerate them instead.",
        );
        let candidate = promotion_candidate(&node, &stat(node.public_id.as_str(), 12, 12, true));
        let markdown = rules::render_rule_markdown(&candidate);

        // The promotion contract: the file must parse through the rules
        // engine's own frontmatter parser and loader, exactly like a
        // hand-authored .stella/rules/*.md file.
        let fm = rules::parse_frontmatter(&markdown);
        assert_eq!(
            fm.data.get("description").unwrap(),
            "Promoted from memory nod_0123456789abcdef01234567 after 12 citation(s) \
             (12 consecutive positive)."
        );
        assert_eq!(fm.body, candidate.text);

        let path = format!(".stella/rules/{}.md", candidate.id);
        let rule = rules::rule_from_file(&path, &markdown).expect("a loadable rule");
        assert_eq!(rule.id, "never-edit-generated-files");
        assert_eq!(rule.text, candidate.text);
        assert!(rule.guard.is_none(), "promoted rules are prompt-only");
    }

    #[test]
    fn slugify_produces_filename_safe_stems() {
        assert_eq!(
            slugify("Never edit generated files"),
            "never-edit-generated-files"
        );
        assert_eq!(slugify("  ~~weird?? punctuation!!  "), "weird-punctuation");
        assert_eq!(slugify("///"), "memory-rule");
        assert!(slugify(&"long word ".repeat(20)).len() <= 48);
    }

    #[test]
    fn join_ranks_cited_memories_first_and_keeps_uncited_ones_visible() {
        let memories = vec![
            memory_node("nod_new", "newest, never cited", "newest, never cited"),
            memory_node("nod_two", "cited twice", "cited twice"),
            memory_node("nod_ten", "cited ten times", "cited ten times"),
        ];
        let stats = vec![stat("nod_two", 2, 2, false), stat("nod_ten", 10, 10, false)];
        let rows = join_rows(memories, &stats);
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["nod_ten", "nod_two", "nod_new"],
            "most-cited first, uncited (but inspectable) last"
        );
        assert_eq!(rows[0].citations, 10);
        assert!(!rows[0].eligible, "exactly 10 is not more than 10");
        assert_eq!(rows[2].citations, 0);
    }

    #[test]
    fn join_drops_stats_for_ids_that_are_not_live_memories() {
        let rows = join_rows(
            vec![memory_node("nod_live", "l", "l")],
            &[stat("nod_ghost", 99, 99, true)],
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "nod_live");
    }

    /// The whole loop against real stores in a temp workspace: seed a
    /// memory (context.db), cite it (store.db), watch the strict >10 gate
    /// hold at exactly 10, tip it at 11, promote, and parse the written
    /// rule back through the rules engine. No cwd mutation — the `_in`/
    /// `list_rows` seams take the root directly.
    #[tokio::test]
    async fn promotion_gate_and_rule_write_work_end_to_end() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let lesson = "Never edit generated files under gen/ — regenerate them instead.";

        let context_db = stella_store::workspace_private_sqlite_path(root, "context.db").unwrap();
        let context = ContextStore::open(context_db).unwrap();
        context
            .upsert(stella_context::ContextDelta {
                memories: vec![stella_context::MemoryInput::new(
                    stella_context::MemoryKind::Note,
                    lesson,
                )],
                ..Default::default()
            })
            .await
            .unwrap();
        let id = context.memory_nodes().unwrap()[0].public_id.clone();
        drop(context);

        let cite = |store: &stella_store::Store| {
            let exec = store.begin_execution("run", "p", "zai", "glm-5.2").unwrap();
            store
                .record_memory_citations(
                    exec,
                    &[stella_store::MemoryCitationRow {
                        memory_id: id.clone(),
                        useful_score: 5,
                        truthful: true,
                        remark: "held".into(),
                    }],
                )
                .unwrap();
        };
        let store = stella_store::Store::open(root).unwrap();
        for _ in 0..10 {
            cite(&store);
        }
        // Exactly 10 successful citations: the strict gate holds.
        let err = promote_in(root, &id).unwrap_err();
        assert!(err.contains("not promotion-eligible"), "{err}");
        // The 11th tips it.
        cite(&store);
        drop(store);
        assert!(list_rows(root).unwrap()[0].eligible);
        promote_in(root, &id).unwrap();

        let rules_dir = root.join(".stella").join("rules");
        let written: Vec<std::path::PathBuf> = std::fs::read_dir(&rules_dir)
            .expect("rules dir created")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        assert_eq!(written.len(), 1, "exactly one rule written: {written:?}");
        let rule_path = &written[0];
        let raw = std::fs::read_to_string(rule_path).expect("promoted rule written");
        let rule = rules::rule_from_file(&rule_path.display().to_string(), &raw)
            .expect("the rules engine loads the promoted file");
        assert_eq!(rule.text, lesson);

        // Idempotence: re-promotion never clobbers the written rule.
        let err = promote_in(root, &id).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
    }

    #[test]
    fn table_flags_eligibility_and_blanks_scores_for_uncited_rows() {
        let rows = join_rows(
            vec![
                memory_node("nod_eligible", "e", "an eligible memory"),
                memory_node("nod_quiet", "q", "a never-cited memory"),
            ],
            &[stat("nod_eligible", 12, 12, true)],
        );
        let out = render_table(&rows);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("ID"));
        assert!(lines[1].contains("eligible"), "eligible row flagged: {out}");
        assert!(
            lines[2].contains("  -  "),
            "uncited rows show `-`, not fake zeros: {out}"
        );
    }

    #[test]
    fn extract_path_anchors_finds_source_paths() {
        let text = "graph_snapshot() in stella-cli/src/agent.rs computes the \
                    neighborhood. See also docs/hooks.md and src/lib.rs.";
        let anchors = extract_path_anchors(text);
        assert!(anchors.contains(&"stella-cli/src/agent.rs".to_string()));
        assert!(anchors.contains(&"docs/hooks.md".to_string()));
        assert!(anchors.contains(&"src/lib.rs".to_string()));
        // No false positives from bare words.
        assert!(!anchors.iter().any(|a| a == "graph_snapshot"));
    }

    #[test]
    fn extract_path_anchors_finds_bare_filenames() {
        // The shape reflections actually use: a file named with no directory.
        let text = "In stella-cli witness tests (slash_models_witness.rs), prefer \
                    updating assertions rather than changing the renderer.";
        let anchors = extract_path_anchors(text);
        assert!(
            anchors.contains(&"slash_models_witness.rs".to_string()),
            "bare filenames must anchor: {anchors:?}"
        );
    }

    #[test]
    fn extract_path_anchors_rejects_non_files() {
        // A bare extension has no stem, version strings have no source
        // extension, and prose words have no extension at all.
        let anchors = extract_path_anchors("the .rs files in v0.4.47 use cargo test -- --exact");
        assert!(anchors.is_empty(), "no false anchors: {anchors:?}");
    }

    #[test]
    fn validate_flags_stale_bare_filename_anchors() {
        // Regression: a memory naming a deleted test file by bare name was
        // reported `no-anchors` and never checked, so recall kept surfacing
        // it long after the file was gone.
        let root = tempdir().unwrap();
        let context_db =
            stella_store::workspace_private_sqlite_path(root.path(), "context.db").unwrap();
        std::fs::create_dir_all(root.path().join("stella-cli/tests")).unwrap();
        std::fs::write(
            root.path().join("stella-cli/tests/live_witness.rs"),
            "#[test] fn t() {}",
        )
        .unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let context = stella_context::ContextStore::open(&context_db).unwrap();
            use stella_context::{ContextDelta, MemoryInput};
            let delta = ContextDelta {
                memories: vec![
                    MemoryInput::reflection(
                        "In witness tests (deleted_witness.rs), prefer updating assertions.",
                        Vec::<String>::new(),
                    ),
                    MemoryInput::reflection(
                        "In witness tests (live_witness.rs), prefer updating assertions.",
                        Vec::<String>::new(),
                    ),
                ],
                ..Default::default()
            };
            context.upsert(delta).await.unwrap();
        });

        let rows = validate_memories(root.path()).unwrap();
        let stale = rows.iter().find(|r| r.status == "stale").expect(
            "a memory naming a deleted file by bare name must be flagged stale, not no-anchors",
        );
        assert!(stale.memory.contains("deleted_witness.rs"));
        let ok = rows
            .iter()
            .find(|r| r.status == "ok")
            .expect("live file verifies");
        assert!(ok.memory.contains("live_witness.rs"));
    }

    #[test]
    fn validate_flags_stale_path_anchors() {
        let root = tempdir().unwrap();
        // Create the stores + a memory node referencing a path that exists
        // and one that does not.
        let context_db =
            stella_store::workspace_private_sqlite_path(root.path(), "context.db").unwrap();
        std::fs::create_dir_all(root.path().join("stella-cli/src")).unwrap();
        std::fs::write(root.path().join("stella-cli/src/agent.rs"), "pub fn x() {}").unwrap();
        // Write memories via the context store's async upsert.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let context = stella_context::ContextStore::open(&context_db).unwrap();
            use stella_context::{ContextDelta, MemoryInput};
            let delta = ContextDelta {
                memories: vec![
                    MemoryInput::reflection(
                        "agent.rs at stella-cli/src/agent.rs does X",
                        Vec::<String>::new(),
                    ),
                    MemoryInput::reflection(
                        "old.rs at deleted/old.rs was removed",
                        Vec::<String>::new(),
                    ),
                    MemoryInput::reflection("a memory with no paths at all", Vec::<String>::new()),
                ],
                ..Default::default()
            };
            context.upsert(delta).await.unwrap();
        });

        let rows = validate_memories(root.path()).unwrap();
        assert_eq!(rows.len(), 3);
        let stale = rows.iter().find(|r| r.status == "stale");
        assert!(stale.is_some(), "must flag the deleted path");
        let ok = rows.iter().find(|r| r.status == "ok");
        assert!(ok.is_some(), "must verify the existing path");
        let no_anchors = rows.iter().find(|r| r.status == "no-anchors");
        assert!(no_anchors.is_some(), "must flag no-anchor memories");
    }

    /// The duplicate-lineage report finds what past edits left behind, and
    /// changes nothing (#711, decision 4).
    ///
    /// Two live memories saying close to the same thing is exactly the shape a
    /// pre-lineage edit produced. Reporting is the whole contract here: a
    /// merge would have to guess which wording the user meant to keep, and
    /// guessing wrong is unrecoverable in a way that leaving both is not.
    #[tokio::test]
    async fn duplicate_lineages_are_reported_and_never_merged() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let context_db = stella_store::workspace_private_sqlite_path(root, "context.db").unwrap();
        let context = ContextStore::open(context_db).unwrap();
        context
            .upsert(stella_context::ContextDelta {
                memories: vec![
                    stella_context::MemoryInput::new(
                        stella_context::MemoryKind::Note,
                        "always run the database migration before the deploy step",
                    ),
                    stella_context::MemoryInput::new(
                        stella_context::MemoryKind::Note,
                        "always run the database migration before the deploy step, every time",
                    ),
                    stella_context::MemoryInput::new(
                        stella_context::MemoryKind::Note,
                        "the retry budget for the billing webhook is three attempts",
                    ),
                ],
                ..Default::default()
            })
            .await
            .unwrap();
        drop(context);

        let before = {
            let context = ContextStore::open(
                stella_store::workspace_private_sqlite_path(root, "context.db").unwrap(),
            )
            .unwrap();
            context.memory_lineage_stats().unwrap()
        };
        assert_eq!(before.lineages, 3, "three memories, three lineages");

        report_duplicate_lineages(root).expect("report");

        let after = {
            let context = ContextStore::open(
                stella_store::workspace_private_sqlite_path(root, "context.db").unwrap(),
            )
            .unwrap();
            context.memory_lineage_stats().unwrap()
        };
        assert_eq!(
            before, after,
            "reporting duplicates must not merge, supersede, or delete anything"
        );
    }

    /// `stella memory edit` revises in place: one live record, the same id, and
    /// the previous wording kept as history rather than as a competitor.
    #[tokio::test]
    async fn editing_a_memory_revises_it_instead_of_minting_a_second() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let context_db = stella_store::workspace_private_sqlite_path(root, "context.db").unwrap();
        let context = ContextStore::open(context_db).unwrap();
        context
            .upsert(stella_context::ContextDelta {
                memories: vec![stella_context::MemoryInput::reflection(
                    "prefer rg over grep",
                    Vec::<String>::new(),
                )],
                ..Default::default()
            })
            .await
            .unwrap();
        let id = context.memory_nodes().unwrap()[0].public_id.clone();
        let lineage = context.memory_lineage(&id).unwrap().expect("lineage");

        context
            .upsert(stella_context::ContextDelta {
                memories: vec![
                    stella_context::MemoryInput::new(
                        stella_context::MemoryKind::Reflection,
                        "prefer rg over grep, and fd over find",
                    )
                    .revises(&lineage),
                ],
                ..Default::default()
            })
            .await
            .unwrap();

        let nodes = context.memory_nodes().unwrap();
        assert_eq!(nodes.len(), 1, "one memory, not two");
        assert_eq!(nodes[0].public_id, id, "the id a user holds still resolves");
        assert_eq!(nodes[0].content, "prefer rg over grep, and fd over find");
        let stats = context.memory_lineage_stats().unwrap();
        assert_eq!((stats.lineages, stats.live, stats.superseded), (1, 1, 1));
        // The kind is carried forward, not reset — a mined reflection stays
        // mined, which is what decides whether the restatement filter treats it
        // as regenerable.
        assert_eq!(
            context.memory_kind(&lineage).unwrap().as_deref(),
            Some("reflection")
        );
    }
}
