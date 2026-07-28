//! `stella ingest` — turning markdown you already wrote into steering.
//!
//! Two ways in, and they are deliberately different conversations.
//!
//! **Unprompted.** A workspace usually already contains `AGENTS.md` or
//! `CLAUDE.md` — instructions written for an agent, on purpose, by someone who
//! knew what they meant. Those two files are named directly. Everything else
//! the scan finds is held back as a suggestion, because a first-run dialog that
//! opens with nine files and a ranking is a chore, and the honest answer to a
//! chore is "not now".
//!
//! **By hand.** Any path, anywhere, at any time. A file the scan skipped, a
//! design doc outside the workspace, a scratch file of notes — if you name it,
//! it is a valid argument. The tiering is a default, not a gate.
//!
//! The walking and reading live here; every decision about *what a document is*
//! lives in [`stella_core::ingest`], which is pure and tested without a fixture
//! tree.

use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;
use colored::Colorize;

use stella_core::ingest::{self, Candidate, Plan, Tier};

/// Largest slice of any one file read for classification.
///
/// Classification looks at the head and the bullet structure; a document that
/// needs more than this to classify is not a document anyone hand-wrote as
/// steering.
const MAX_READ_BYTES: u64 = 64 * 1024;

/// `stella ingest` — see what steering a workspace already contains.
#[derive(Debug, Args)]
pub struct IngestArgs {
    /// Markdown files to ingest. Any path is valid, inside the workspace or
    /// not; naming a file overrides every tiering rule the scan would apply.
    ///
    /// With no paths, the workspace is scanned and the candidates are grouped
    /// the way the first-run dialog groups them.
    pub paths: Vec<PathBuf>,

    /// Show every candidate found, including the ones normally held back.
    #[arg(long)]
    pub all: bool,
}

/// One discovered file, with the cheap facts worth showing a person.
struct Found {
    candidate: Candidate,
    lines: usize,
}

/// Walk `root` for markdown, honouring the depth limit and the excluded
/// directories, and classify everything found.
fn scan(root: &Path) -> Vec<Found> {
    let mut found = Vec::new();
    walk(root, root, 0, &mut found);
    found.sort_by(|a, b| {
        a.candidate
            .tier
            .cmp(&b.candidate.tier)
            .then_with(|| a.candidate.path.cmp(&b.candidate.path))
    });
    found
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<Found>) {
    if depth > ingest::MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(rel) = relative(root, &path) else {
            continue;
        };
        if ingest::is_excluded_segment(&rel) {
            continue;
        }
        if path.is_dir() {
            walk(root, &path, depth + 1, out);
        } else if ingest::is_markdown(&rel)
            && let Some(found) = read_and_classify(&path, &rel)
        {
            out.push(found);
        }
    }
}

/// Workspace-relative, forward-slashed. `None` when the path escapes `root`,
/// which `walk` never produces but a caller-supplied path can.
fn relative(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

fn read_and_classify(path: &Path, rel: &str) -> Option<Found> {
    let meta = fs::metadata(path).ok()?;
    if meta.len() > MAX_READ_BYTES {
        return None;
    }
    let content = fs::read_to_string(path).ok()?;
    Some(Found {
        lines: content.lines().count(),
        candidate: ingest::classify(rel, &content),
    })
}

/// `stella ingest` with no paths — scan, group, and describe.
fn run_scan(root: &Path, show_all: bool) {
    let found = scan(root);
    if found.is_empty() {
        println!("No markdown found in this workspace.");
        return;
    }

    let by_path: Vec<Candidate> = found.iter().map(|f| f.candidate.clone()).collect();
    let plan: Plan = ingest::plan(by_path);
    let lines_for = |path: &str| {
        found
            .iter()
            .find(|f| f.candidate.path == path)
            .map_or(0, |f| f.lines)
    };

    if plan.is_empty() && !show_all {
        println!("Nothing here reads like steering. Name a file directly to ingest it anyway:");
        println!("  {}", "stella ingest path/to/notes.md".dimmed());
        return;
    }

    if !plan.primary.is_empty() {
        println!(
            "\n{}",
            "You already wrote instructions for an agent.".bold()
        );
        for candidate in &plan.primary {
            println!(
                "  {}  {}",
                candidate.path.green(),
                format!("{} lines", lines_for(&candidate.path)).dimmed()
            );
        }
        println!(
            "\n{}",
            "Stella can turn these into records it can check, cite, and retire."
                .to_string()
                .dimmed()
        );
        println!(
            "{}",
            "Every claim is reviewed before it steers anything.".dimmed()
        );
    }

    if !plan.suggestions.is_empty() {
        println!("\n{}", "Anything else worth steering with?".bold());
        for candidate in &plan.suggestions {
            println!(
                "  {}  {}",
                candidate.path,
                format!("{} lines", lines_for(&candidate.path)).dimmed()
            );
        }
        if plan.hidden_suggestions > 0 {
            println!(
                "  {}",
                format!(
                    "… and {} more, closest-to-the-root first",
                    plan.hidden_suggestions
                )
                .dimmed()
            );
        }
    }

    if show_all {
        print_held_back(&found);
    } else {
        let held = found
            .iter()
            .filter(|f| matches!(f.candidate.tier, Tier::Historical | Tier::Skip))
            .count();
        if held > 0 {
            println!(
                "\n{}",
                format!("{held} more held back (history, licences, indexes) — `--all` to see why.")
                    .dimmed()
            );
        }
    }

    println!(
        "\n{}",
        "Any path is a valid argument, listed or not:".dimmed()
    );
    println!(
        "  {}",
        "stella ingest AGENTS.md docs/conventions.md".dimmed()
    );
}

fn print_held_back(found: &[Found]) {
    let held: Vec<&Found> = found
        .iter()
        .filter(|f| matches!(f.candidate.tier, Tier::Historical | Tier::Skip))
        .collect();
    if held.is_empty() {
        return;
    }
    println!("\n{}", "Held back:".bold());
    for f in held {
        println!(
            "  {}  {}",
            f.candidate.path.dimmed(),
            format!("{:?}", f.candidate.signals).dimmed()
        );
    }
}

/// `stella ingest <paths>` — the user named files, so tiering does not apply.
fn run_named(root: &Path, paths: &[PathBuf]) -> Result<(), String> {
    let mut failed = false;
    println!();
    for path in paths {
        let rel = relative(root, path).unwrap_or_else(|| path.to_string_lossy().to_string());
        match fs::read_to_string(path) {
            Ok(content) => {
                let candidate = ingest::classify(&rel, &content);
                let note = match candidate.tier {
                    // Naming a file overrides the tier, but saying so is the
                    // difference between respecting the choice and hiding a
                    // known risk behind it.
                    Tier::Historical => " — heads up: this reads like a retired document".yellow(),
                    Tier::Skip => " — heads up: this looks like an index or boilerplate".yellow(),
                    _ => "".normal(),
                };
                println!(
                    "  {}  {}{}",
                    rel.green(),
                    format!("{} lines", content.lines().count()).dimmed(),
                    note
                );
            }
            Err(err) => {
                failed = true;
                eprintln!("  {}  {}", rel.red(), err.to_string().dimmed());
            }
        }
    }
    if failed {
        return Err("one or more files could not be read".to_string());
    }
    println!(
        "\n{}",
        "Extraction into context records is not wired yet — this reports what would be read."
            .dimmed()
    );
    Ok(())
}

/// Entry point for `stella ingest`.
pub fn run(args: &IngestArgs) -> Result<(), String> {
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if args.paths.is_empty() {
        run_scan(&root, args.all);
        Ok(())
    } else {
        run_named(&root, &args.paths)
    }
}
