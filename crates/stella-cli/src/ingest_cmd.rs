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

mod extract;
pub(crate) mod probe;
mod progress;

pub(crate) use extract::derive_set_id;

/// Largest slice of any one file read for classification.
///
/// Must stay above `extract`'s prompt cap, or the scan hides documents the
/// extractor would happily take: a file past this is dropped from the listing
/// entirely, so at 64 KiB against a 120,000-character prompt cap a large
/// `AGENTS.md` was ingestable by name and invisible to `stella ingest` with no
/// arguments. `the_scan_never_hides_a_document_extraction_would_accept` pins the
/// ordering so raising one cap cannot silently strand the other.
///
/// Classification looks at the head and the bullet structure; a document that
/// needs more than this to classify is not a document anyone hand-wrote as
/// steering.
const MAX_READ_BYTES: u64 = 512 * 1024;

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

/// Say how much of a document extraction will not read.
///
/// Only the head of a long document reaches the model. Nothing used to tell the
/// person who named the file, so at the old 24,000-character cap a 44,545-character
/// `AGENTS.md` reported "688 lines", extracted 54% of itself, and read as a
/// complete success — the god files, the glossary and the testing section could
/// not have produced a record and no output said why. The cap is five times
/// larger now, which moves the edge rather than removing it, so the notice still
/// has to exist. Printed beside the line count so the two numbers a reader would
/// compare sit together.
fn report_truncation(content: &str) {
    if let Some(notice) = truncation_notice(content) {
        println!("    {}", notice.yellow());
    }
}

/// The notice itself, or `None` when the whole document is extracted.
///
/// Separated from the printing so the sentence a person reads is a value a test
/// can assert on, rather than something only a human running the command sees.
fn truncation_notice(content: &str) -> Option<String> {
    let skipped = extract::skipped_chars(content);
    if skipped == 0 {
        return None;
    }
    let total = content.chars().count();
    Some(format!(
        "heads up: only the first {} of {total} characters are extracted — {skipped} ({}%) are \
         skipped, so nothing below that point can become a record",
        total - skipped,
        percent(skipped, total),
    ))
}

/// `part` as a whole percentage of `whole`, rounded to nearest.
///
/// Widened to `u64` for the multiply: these are character counts of a
/// user-named file, which no cap bounds, and a 32-bit `usize` would overflow
/// on one of ~43 million characters.
fn percent(part: usize, whole: usize) -> u64 {
    if whole == 0 {
        return 0;
    }
    let (part, whole) = (part as u64, whole as u64);
    (part * 100 + whole / 2) / whole
}

/// `stella ingest <paths>` — the user named files, so tiering does not apply.
///
/// Reads and displays each named file, then hands the readable ones to
/// [`extract::extract_all`], which makes the model call and writes the proposals.
/// An unreadable file is reported and skipped; only a run with nothing readable
/// at all is an error.
fn run_named(
    root: &Path,
    paths: &[PathBuf],
    model: Option<&str>,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> Result<(), String> {
    let mut unreadable = false;
    let mut docs = Vec::new();
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
                report_truncation(&content);
                docs.push(extract::NamedDoc {
                    rel,
                    content,
                    tier: candidate.tier,
                });
            }
            Err(err) => {
                unreadable = true;
                eprintln!("  {}  {}", rel.red(), err.to_string().dimmed());
            }
        }
    }
    if docs.is_empty() {
        return Err("no readable files to ingest".to_string());
    }
    let result = extract::extract_all(root, &docs, model, api_key, base_url);
    if unreadable && result.is_ok() {
        eprintln!(
            "{}",
            "  (some named files could not be read — see above)".dimmed()
        );
    }
    result
}

/// Entry point for `stella ingest`.
pub fn run(
    args: &IngestArgs,
    model: Option<&str>,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> Result<(), String> {
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if args.paths.is_empty() {
        run_scan(&root, args.all);
        Ok(())
    } else {
        // Extraction resolves a provider and makes a model call, so it needs
        // the same live catalog every other model-calling command gets.
        // `stella ingest` is dispatched BEFORE `main`'s bootstrap (it has a
        // scan mode that touches no provider at all), which is exactly how a
        // catalog-provable bad slug used to slip past validation here and die
        // at the provider instead (#895). Called on this branch only: scanning
        // stays catalog-free, and this call site is synchronous, so the full
        // refresh — not just the network-free half — is safe.
        crate::model_catalog::bootstrap();
        run_named(&root, &args.paths, model, api_key, base_url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A document the cap does not reach is extracted whole, and saying
    /// anything about truncation would be a lie.
    #[test]
    fn a_short_document_gets_no_notice() {
        assert_eq!(truncation_notice("# short\n\nnothing to drop.\n"), None);
    }

    /// The witness for the 5x cap: the document that motivated all of this now
    /// fits whole. At the old 24,000 it was extracted at 54% and reported
    /// success, so a real `AGENTS.md` losing nothing is the thing to pin.
    #[test]
    fn a_real_agents_md_now_fits_the_cap_whole() {
        // This repository's own AGENTS.md, 44,545 characters.
        assert_eq!(truncation_notice(&"x".repeat(44_545)), None);
    }

    /// Still reported when a document genuinely exceeds the raised cap — a
    /// bigger limit that lies about its own edge is the original defect again.
    #[test]
    fn an_oversized_document_says_how_much_is_skipped() {
        let content = "x".repeat(160_000);
        let notice = truncation_notice(&content).expect("a document over the cap is truncated");
        assert!(notice.contains("120000"), "kept count missing: {notice}");
        assert!(notice.contains("160000"), "total missing: {notice}");
        assert!(notice.contains("40000"), "skipped count missing: {notice}");
        assert!(notice.contains("25%"), "skipped share missing: {notice}");
    }

    /// One character past the cap is still reported. A notice that appeared
    /// only once the loss was large would be silent for exactly the documents
    /// whose missing tail is hardest to notice.
    #[test]
    fn one_character_over_the_cap_is_still_reported() {
        let notice = truncation_notice(&"x".repeat(120_001)).expect("one over the cap truncates");
        assert!(notice.contains("1 (0%)"), "{notice}");
    }

    /// The scan must never drop a document extraction would accept.
    ///
    /// `read_and_classify` returns `None` past `MAX_READ_BYTES`, which removes
    /// the file from the listing entirely rather than showing it as too large.
    /// So the read cap has to clear the prompt cap at its widest — UTF-8 is up
    /// to four bytes per character, and a cap counted in characters against one
    /// counted in bytes is exactly the mismatch that hides a document.
    #[test]
    fn the_scan_never_hides_a_document_extraction_would_accept() {
        let widest_possible_bytes = extract::MAX_PROMPT_CHARS as u64 * 4;
        assert!(
            MAX_READ_BYTES >= widest_possible_bytes,
            "the scan reads at most {MAX_READ_BYTES} bytes but extraction accepts up to \
             {} characters ({widest_possible_bytes} bytes of 4-byte codepoints) — a document \
             between the two is ingestable by name and invisible to the scan",
            extract::MAX_PROMPT_CHARS,
        );
    }

    #[test]
    fn percent_rounds_to_nearest_and_never_divides_by_zero() {
        assert_eq!(percent(0, 0), 0);
        assert_eq!(percent(40_000, 160_000), 25);
        assert_eq!(percent(20_545, 44_545), 46);
        assert_eq!(percent(1, 3), 33);
        assert_eq!(percent(2, 3), 67);
        assert_eq!(percent(7, 7), 100);
    }
}
