//! The I/O half of ingest provenance lineages (#2683, first slice).
//!
//! [`stella_core::ingest::lineage`] decides everything — what a lineage
//! remembers, when drift warrants an alert, and what dismissal means. This
//! module supplies the three facts it cannot know:
//!
//! 1. **The ledger file.** `.stella/private/ingest-lineages.json`, private for
//!    the same reason as the sweep cache: a lineage is a local measurement of a
//!    local tree, and committing it would let one developer's filesystem decide
//!    what another developer's agent is nagged about.
//! 2. **Content hashes.** Git's own blob hash when git can compute one (so
//!    clean filters apply exactly as they would on commit), a plain sha256
//!    otherwise. The scheme is part of the stored string, so the comparison in
//!    core stays an opaque equality.
//! 3. **The inbox.** Drift becomes a [`stella_store::Notification`], deduped by
//!    a deterministic id the way tool-foundry proposals are, so one drift is
//!    surfaced once rather than once per session. The permanent silencer is
//!    `stella ingest alerts dismiss` — dedup only stops the short-term nag.
//!
//! Everything here is best-effort in the same sense as the sweep cache: an
//! unreadable ledger is an empty one, a failed hash reads as "missing", and a
//! failed notification push is dropped. None of it can fail an ingest or a
//! session start.

use std::path::Path;

use colored::Colorize;
use stella_core::ingest::lineage::{AlertState, Drift, Lineage, LineageLedger};
use stella_store::{Notification, NotificationStore};

/// Where the private lineage ledger lives, relative to the workspace root.
const LINEAGE_LEDGER: &str = ".stella/private/ingest-lineages.json";

/// The `source` stamped on every staleness alert (shown dimmed in the inbox).
const SOURCE: &str = "ingest-staleness";

/// Read the ledger. Missing or unreadable is empty — the one thing that must
/// never happen silently is *forgetting a dismissal*, and an empty ledger
/// cannot re-arm anything: it has no lineages to alert on at all.
pub(crate) fn read_ledger(root: &Path) -> LineageLedger {
    std::fs::read_to_string(root.join(LINEAGE_LEDGER))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Write the ledger back, best-effort.
pub(crate) fn write_ledger(root: &Path, ledger: &LineageLedger) {
    let path = root.join(LINEAGE_LEDGER);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(body) = serde_json::to_string_pretty(ledger) {
        let _ = std::fs::write(&path, body);
    }
}

/// The hash of the content an ingest actually read, in the same scheme
/// [`current_source_hash`] will use later — `git hash-object --path <rel>
/// --stdin` so clean filters apply to the text exactly as they apply to the
/// file, falling back to sha256 of the text outside git's reach.
///
/// Hashing the in-memory text rather than re-reading the file closes the race
/// between "what extraction read" and "what is on disk now": a file edited
/// mid-ingest records the hash of what the records were actually derived from.
pub(crate) fn ingested_content_hash(root: &Path, rel: &str, content: &str) -> String {
    git_blob_hash_of_content(root, rel, content)
        .map(|oid| format!("git:{oid}"))
        .unwrap_or_else(|| sha256_of(content))
}

/// The hash the source has *now*, or `None` when it no longer exists (or can
/// no longer be read). Same scheme preference as [`ingested_content_hash`], so
/// an unchanged file compares equal.
pub(crate) fn current_source_hash(root: &Path, rel: &str) -> Option<String> {
    let path = root.join(rel);
    if let Some(oid) = git_blob_hash_of_path(root, rel) {
        return Some(format!("git:{oid}"));
    }
    std::fs::read_to_string(&path).ok().map(|c| sha256_of(&c))
}

/// `git hash-object --path <rel> --stdin` — the blob id of `content` as git
/// would store it at that path, clean filters applied. `None` when git is
/// absent or errors.
fn git_blob_hash_of_content(root: &Path, rel: &str, content: &str) -> Option<String> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["hash-object", "--path", rel, "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(content.as_bytes()).ok()?;
    let output = child.wait_with_output().ok()?;
    parse_oid(output)
}

/// `git hash-object --path <rel> -- <rel>` — the blob id of the file as it
/// stands. `--path` so the clean filter applies exactly as it would on commit;
/// without it a repo with `.gitattributes` filters would hash raw bytes and an
/// untouched file would read as drifted (the same reasoning as
/// `candidate_ws::adopt`).
fn git_blob_hash_of_path(root: &Path, rel: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["hash-object", "--path", rel, "--", rel])
        .output()
        .ok()?;
    parse_oid(output)
}

fn parse_oid(output: std::process::Output) -> Option<String> {
    if !output.status.success() {
        return None;
    }
    let oid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!oid.is_empty()).then_some(oid)
}

/// `sha256:<hex>` of `content` — the scheme for sources git cannot hash.
fn sha256_of(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(content.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    format!("sha256:{hex}")
}

/// Record one successful ingest into the ledger: replace the path's lineage
/// with a fresh era, re-arming alerts unless told to keep a dismissal.
pub(crate) fn record_ingest(root: &Path, rel: &str, lineage: Lineage, keep_dismissed: bool) {
    let mut ledger = read_ledger(root);
    ledger.record_ingest(rel, lineage, keep_dismissed);
    write_ledger(root, &ledger);
}

/// Check every alert-active lineage against its source and push a deduped
/// inbox notification per drift. Best-effort and side-effect-only: returns how
/// many *new* alerts were surfaced, and never propagates an error.
///
/// Runs off the turn path (a session-start background thread), never blocks a
/// turn, never enters the model's prompt, and never mutates anything but the
/// inbox — re-ingest and dismissal stay deliberate acts.
pub(crate) fn surface_stale_sources(root: &Path) -> usize {
    surface_stale_sources_into(root, &NotificationStore::open_default())
}

/// The testable core of [`surface_stale_sources`], with the inbox injected so
/// a test can point at a temp directory.
pub(crate) fn surface_stale_sources_into(root: &Path, inbox: &NotificationStore) -> usize {
    let ledger = read_ledger(root);
    if ledger.sources.is_empty() {
        return 0;
    }
    let existing: std::collections::HashSet<String> =
        inbox.list().into_iter().map(|n| n.id).collect();
    let mut pushed = 0;
    for (rel, lineage) in &ledger.sources {
        let current = current_source_hash(root, rel);
        let Some(drift) = lineage.drift(current.as_deref()) else {
            continue;
        };
        let id = notification_id(rel, &drift);
        if existing.contains(&id) {
            continue;
        }
        let mut note = Notification::new(alert_title(rel, &drift), alert_body(rel, &drift), SOURCE);
        // A deterministic id from the drift itself is what makes dedup survive
        // across sessions — `Notification::new` would otherwise mint a fresh id
        // each pass and re-surface the same drift every session start.
        note.id = id;
        if inbox.push(&note).is_ok() {
            pushed += 1;
        }
    }
    pushed
}

/// A stable, filesystem-safe notification id derived from the drift, so one
/// drift maps to exactly one inbox item. A *further* change to the same file
/// is a different drift and earns a fresh alert.
fn notification_id(rel: &str, drift: &Drift) -> String {
    let signature = match drift {
        Drift::Changed { from, to } => format!("{rel}|{from}|{to}"),
        Drift::Missing { from } => format!("{rel}|{from}|missing"),
    };
    format!(
        "ntf-ingestdrift-{:016x}",
        crate::tool_foundry::fnv1a(&signature)
    )
}

/// The short spelling of a stored hash for alert prose: scheme dropped, first
/// eight characters kept — enough to tell two hashes apart by eye.
fn short(hash: &str) -> &str {
    let hex = hash.split_once(':').map_or(hash, |(_, rest)| rest);
    &hex[..hex.len().min(8)]
}

fn alert_title(rel: &str, drift: &Drift) -> String {
    match drift {
        Drift::Changed { .. } => format!("Ingested source changed: `{rel}`"),
        Drift::Missing { .. } => format!("Ingested source deleted: `{rel}`"),
    }
}

fn alert_body(rel: &str, drift: &Drift) -> String {
    match drift {
        Drift::Changed { from, to } => format!(
            "`{rel}` changed since it was ingested ({} → {}). Its context records still \
             reflect the old text.\n\nRun `stella ingest --refresh {rel}` to reconcile — \
             unchanged records are kept, changed claims re-proposed, dropped claims retired — \
             or silence alerts for this file permanently:\n  stella ingest alerts dismiss {rel}",
            short(from),
            short(to),
        ),
        Drift::Missing { from } => format!(
            "`{rel}` no longer exists (it hashed {} at ingest). Its context records remain \
             live.\n\nRe-ingest a replacement file, or — if the file was scaffolding and the \
             records are the product — silence alerts for it permanently:\n  stella ingest \
             alerts dismiss {rel}",
            short(from),
        ),
    }
}

// ── The `stella ingest alerts` surface ───────────────────────────────────────

/// `stella ingest alerts list` — every lineage, its state, and its live drift.
pub(crate) fn run_list(root: &Path) -> Result<(), String> {
    let ledger = read_ledger(root);
    if ledger.sources.is_empty() {
        println!("No ingested source files are being tracked yet.");
        println!(
            "  {}",
            "stella ingest <file> records a lineage per ingested file.".dimmed()
        );
        return Ok(());
    }
    println!();
    for (rel, lineage) in &ledger.sources {
        let state = match lineage.alerts {
            AlertState::Active => "alerts active".green(),
            AlertState::Dismissed => "alerts dismissed".dimmed(),
        };
        let drift = match lineage.drift(current_source_hash(root, rel).as_deref()) {
            None if lineage.alerts == AlertState::Dismissed => String::new(),
            None => "  · in sync".dimmed().to_string(),
            Some(Drift::Changed { ref from, ref to }) => format!(
                "  · {}",
                format!("changed since ingest ({} → {})", short(from), short(to)).yellow()
            ),
            Some(Drift::Missing { ref from }) => format!(
                "  · {}",
                format!("deleted since ingest (was {})", short(from)).yellow()
            ),
        };
        println!(
            "  {}  {}  {}{}",
            rel.bold(),
            format!(
                "ingested {} ({} record(s))",
                lineage.ingested_at,
                lineage.candidate_ids.len()
            )
            .dimmed(),
            state,
            drift,
        );
    }
    println!(
        "\n  {}",
        "stella ingest alerts dismiss <file> silences one file forever; restore re-arms it."
            .dimmed()
    );
    Ok(())
}

/// `stella ingest alerts dismiss|restore <path>` — flip one lineage's alert
/// state and say what changed.
pub(crate) fn run_set_alerts(root: &Path, path: &Path, state: AlertState) -> Result<(), String> {
    let rel = display_path(root, path);
    let mut ledger = read_ledger(root);
    ledger.set_alerts(&rel, state).map_err(|err| {
        if err.known.is_empty() {
            format!("`{rel}` was never ingested — nothing here is tracked yet")
        } else {
            format!(
                "`{rel}` was never ingested. Tracked source files:\n  {}",
                err.known.join("\n  ")
            )
        }
    })?;
    write_ledger(root, &ledger);
    match state {
        AlertState::Dismissed => {
            println!("Alerts for `{rel}` are dismissed — permanently, for this file only.");
            println!(
                "  {}",
                "Its records stay live. `stella ingest alerts restore` re-arms it; re-ingesting \
                 the file re-arms it too."
                    .dimmed()
            );
        }
        AlertState::Active => {
            println!("Alerts for `{rel}` are active again.");
        }
    }
    Ok(())
}

/// The ledger key for a user-supplied path: workspace-relative and
/// forward-slashed when the path is inside the workspace, the path as typed
/// otherwise — the same normalization ingest applies when recording, so the
/// string a user saw in an alert is exactly the string that dismisses it.
pub(crate) fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests;
