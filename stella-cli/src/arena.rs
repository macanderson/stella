//! `stella arena` — the CLI surface of the README's standing challenge
//! (issue #16): view the leaderboard, package a receipted submission.
//!
//! The registry starts file/PR-based, exactly as the Arena rules demand
//! ("receipts or it didn't happen"): standings live in
//! `bench/leaderboard.json` in the Stella repo, and every entry lands there
//! by PR after a community-submitted, officially-scored run is verified.
//! `leaderboard` reads the local file when run inside a Stella checkout and
//! falls back to fetching it from GitHub via `gh` (the same explicit,
//! user-invoked network path as the `ci_status` tool — never phone-home).
//!
//! `submit <run-dir>` validates the run artifacts the harness produced
//! (`summary.json`, `predictions.jsonl`), stamps a SHA-256 over the
//! predictions (the receipt's integrity anchor), writes
//! `<run-dir>/submission.json`, and drafts the submission issue in the
//! README's required title format — via `gh` when available, printed for
//! copy-paste when not.

use std::path::Path;

use colored::Colorize;
use sha2::{Digest, Sha256};

const ARENA_REPO: &str = "oxageninc/stella";
const LEADERBOARD_PATH: &str = "bench/leaderboard.json";
const DIVISIONS: [&str; 4] = ["heavyweight", "featherweight", "off-grid", "cross-harness"];

/// One verified leaderboard entry, as stored in `bench/leaderboard.json`.
#[derive(Debug, serde::Deserialize)]
struct Entry {
    pilot: String,
    matchup: String,
    model: String,
    division: String,
    resolved: String,
    #[serde(rename = "usd_per_resolved")]
    usd_per_resolved: String,
    receipts: String,
}

#[derive(Debug, Default, serde::Deserialize)]
struct Leaderboard {
    #[serde(default)]
    entries: Vec<Entry>,
}

/// `stella arena leaderboard [--division …]`.
pub fn run_leaderboard(division: Option<&str>) -> Result<(), String> {
    if let Some(d) = division
        && !DIVISIONS.contains(&d)
    {
        return Err(format!(
            "unknown division `{d}` — one of: {}",
            DIVISIONS.join(", ")
        ));
    }

    let raw = load_leaderboard_json()?;
    let board: Leaderboard = serde_json::from_str(&raw)
        .map_err(|e| format!("leaderboard registry is malformed: {e}"))?;

    crate::tui::section_header("⚔ Arena leaderboard");
    let entries: Vec<&Entry> = board
        .entries
        .iter()
        .filter(|e| division.is_none_or(|d| e.division == d))
        .collect();
    if entries.is_empty() {
        println!(
            "  {}\n\n  {}\n  {}",
            "The board is empty — on purpose.".bold(),
            "Every row that ever lands here comes from a community-submitted,".dimmed(),
            "officially-scored, fully-receipted run. Be the first:".dimmed(),
        );
        println!(
            "\n    stella arena submit bench/results/<run-id> --matchup \"stella vs <agent>\"\n"
        );
        return Ok(());
    }
    println!(
        "  {:<3} {:<16} {:<26} {:<24} {:<14} {:<9} {:<12} receipts",
        "#", "pilot", "match-up", "model", "division", "resolved", "$/resolved"
    );
    for (i, e) in entries.iter().enumerate() {
        println!(
            "  {:<3} {:<16} {:<26} {:<24} {:<14} {:<9} {:<12} {}",
            i + 1,
            e.pilot,
            e.matchup,
            e.model,
            e.division,
            e.resolved,
            e.usd_per_resolved,
            e.receipts.dimmed(),
        );
    }
    println!();
    Ok(())
}

/// The registry JSON: the local checkout's copy when present (a Stella
/// checkout, or any repo that vendored a board), else fetched from the
/// canonical repo via `gh`.
fn load_leaderboard_json() -> Result<String, String> {
    let local = std::path::PathBuf::from(LEADERBOARD_PATH);
    if local.exists() {
        return std::fs::read_to_string(&local)
            .map_err(|e| format!("cannot read {}: {e}", local.display()));
    }
    let output = std::process::Command::new("gh")
        .args([
            "api",
            &format!("repos/{ARENA_REPO}/contents/{LEADERBOARD_PATH}"),
            "-H",
            "Accept: application/vnd.github.raw+json",
        ])
        .output()
        .map_err(|e| {
            format!(
                "no local {LEADERBOARD_PATH} and `gh` is unavailable ({e}) — run inside a \
                 Stella checkout or install/authenticate gh"
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "could not fetch the leaderboard from {ARENA_REPO}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// `stella arena submit <run-dir>`: validate artifacts, write
/// `submission.json`, draft the submission issue.
pub fn run_submit(run_dir: &Path, matchup: Option<&str>, dry_run: bool) -> Result<(), String> {
    let summary_path = run_dir.join("summary.json");
    let predictions_path = run_dir.join("predictions.jsonl");
    if !summary_path.exists() || !predictions_path.exists() {
        return Err(format!(
            "{} is not a completed run directory — expected summary.json and \
             predictions.jsonl (produced by bench/run_swebench.py)",
            run_dir.display()
        ));
    }

    let summary_raw = std::fs::read_to_string(&summary_path)
        .map_err(|e| format!("cannot read {}: {e}", summary_path.display()))?;
    let summary: serde_json::Value = serde_json::from_str(&summary_raw)
        .map_err(|e| format!("{} is not valid JSON: {e}", summary_path.display()))?;
    let field = |k: &str| {
        summary
            .get(k)
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string())
            })
            .unwrap_or_else(|| "?".to_string())
    };
    let run_id = field("run_id");
    let model = field("model_name_or_path");
    let division = field("division");

    let predictions = std::fs::read(&predictions_path)
        .map_err(|e| format!("cannot read {}: {e}", predictions_path.display()))?;
    let prediction_count = predictions.iter().filter(|b| **b == b'\n').count();
    let sha = {
        let mut h = Sha256::new();
        h.update(&predictions);
        h.finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };

    // The official evaluator's report, if it has been run — encouraged, not
    // required at packaging time (scoring may happen on another machine).
    let evaluated = std::fs::read_dir(run_dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .any(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            name.contains("evaluation") || name.contains("report")
        });

    let matchup = matchup.unwrap_or("stella vs ?").to_string();
    let title = format!("arena: {matchup} — {model} @ ${}", field("budget_usd"));
    let submission = serde_json::json!({
        "title": title,
        "matchup": matchup,
        "run_id": run_id,
        "model": model,
        "division": division,
        "predictions": {
            "path": predictions_path.display().to_string(),
            "count": prediction_count,
            "sha256": sha,
        },
        "officially_scored_report_present": evaluated,
        "summary": summary,
    });
    let submission_path = run_dir.join("submission.json");
    std::fs::write(
        &submission_path,
        serde_json::to_string_pretty(&submission).unwrap_or_default(),
    )
    .map_err(|e| format!("cannot write {}: {e}", submission_path.display()))?;

    let body = format!(
        "## Arena submission\n\n\
         | field | value |\n|---|---|\n\
         | match-up | {matchup} |\n| model | {model} |\n| division | {division} |\n\
         | run id | {run_id} |\n| predictions | {prediction_count} (sha256 `{sha}`) |\n\n\
         ### Receipts checklist (all required before a leaderboard PR)\n\n\
         - [{}] official evaluator report (`python -m swebench.harness.run_evaluation …`)\n\
         - [ ] `predictions.jsonl` attached\n\
         - [ ] `summary.json` attached\n\
         - [ ] per-instance logs attached\n\
         - [ ] token/cost receipts from local telemetry (`stella stats --format json`)\n\n\
         `submission.json` (attach it too):\n```json\n{}\n```\n",
        if evaluated { "x" } else { " " },
        serde_json::to_string_pretty(&submission).unwrap_or_default(),
    );

    crate::tui::section_header("⚔ Arena submission");
    println!("  {} {}", "✓".green(), submission_path.display());
    if !evaluated {
        println!(
            "  {} no evaluator report found in the run dir — score it with the official \
             Docker harness before the leaderboard PR",
            "!".yellow()
        );
    }

    if dry_run {
        println!("\n  — dry run: paste this issue yourself —\n\n{title}\n\n{body}");
        return Ok(());
    }
    let created = std::process::Command::new("gh")
        .args([
            "issue", "create", "--repo", ARENA_REPO, "--title", &title, "--body", &body,
        ])
        .status();
    match created {
        Ok(status) if status.success() => {
            println!(
                "  {} submission issue opened — attach the artifacts listed in its checklist",
                "✓".green()
            );
            Ok(())
        }
        _ => {
            println!(
                "\n  {} `gh` could not open the issue — paste it manually:\n\n{title}\n\n{body}",
                "!".yellow()
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_dir_with(summary: &str, predictions: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("summary.json"), summary).expect("summary");
        std::fs::write(dir.path().join("predictions.jsonl"), predictions).expect("preds");
        dir
    }

    #[test]
    fn submit_requires_the_harness_artifacts() {
        let empty = tempfile::tempdir().expect("tempdir");
        let err = run_submit(empty.path(), None, true).unwrap_err();
        assert!(err.contains("summary.json"), "{err}");
    }

    #[test]
    fn submit_writes_a_receipted_submission_json() {
        let dir = run_dir_with(
            r#"{"run_id": "r1", "model_name_or_path": "anthropic/claude-fable-5",
                "division": "heavyweight", "attempted": 2}"#,
            "{\"instance_id\": \"a\"}\n{\"instance_id\": \"b\"}\n",
        );
        run_submit(dir.path(), Some("stella vs claude-code"), true).expect("submits");
        let written = std::fs::read_to_string(dir.path().join("submission.json")).expect("file");
        let parsed: serde_json::Value = serde_json::from_str(&written).expect("json");
        assert_eq!(parsed["division"], "heavyweight");
        assert_eq!(parsed["matchup"], "stella vs claude-code");
        assert_eq!(parsed["predictions"]["count"], 2);
        let sha = parsed["predictions"]["sha256"].as_str().expect("sha");
        assert_eq!(sha.len(), 64, "a full sha256 receipt");
        assert_eq!(parsed["officially_scored_report_present"], false);
    }

    #[test]
    fn a_malformed_summary_is_a_named_error() {
        let dir = run_dir_with("{ not json", "");
        let err = run_submit(dir.path(), None, true).unwrap_err();
        assert!(err.contains("not valid JSON"), "{err}");
    }

    #[test]
    fn unknown_division_filter_is_rejected_with_the_valid_set() {
        let err = run_leaderboard(Some("cruiserweight")).unwrap_err();
        assert!(err.contains("cross-harness"), "{err}");
    }
}
