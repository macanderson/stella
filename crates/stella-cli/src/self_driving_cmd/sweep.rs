//! `stella self-driving sweep` — draw from one supply by hand.
//!
//! `doc:backlog-self-driving` §5 lists these as verbs a person or a host can
//! run. The code behind them shipped inside the loop, where only the `drive`
//! verb could reach it. So a supply that ships shut had no way to be tried
//! once (`#6184`).
//!
//! Both verbs draw through [`super::supply`]'s `draw_regress` and
//! `draw_meta`. Those are the same calls the loop's `pass` makes. A hand-run
//! report cannot show a different sweep from the one the loop does.
//!
//! Neither verb reads the `[supply]` switches. Those say what the loop may
//! draw from on its own. A person who types the verb has already chosen.
//!
//! `--dry-run` says what would be filed and reaches no tracker. A supply you
//! can only try by letting it write to the tracker is one nobody tries.

use clap::Subcommand;
use serde::Serialize;
use stella_autonomy::supply::Finding;

use super::state::LoopState;
use super::{config, supply};
use crate::query_format::{QueryFormat, Versioned};

/// Which supply to draw from.
///
/// `sweep audit` is left out on purpose. Running a lens's own tooling by
/// hand waits on the driver learning to run it (`#6182`).
#[derive(Subcommand)]
pub(crate) enum SweepCmd {
    /// Re-check every fix this loop has closed, and file the ones whose
    /// change has left the base branch.
    Regress {
        /// Report what would be filed and write nothing to the tracker.
        #[arg(long)]
        dry_run: bool,

        /// Output format: the human report, or the versioned query envelope.
        #[arg(long, value_enum, default_value = "text")]
        format: QueryFormat,
    },

    /// Fold the loop's own cycle ledger and file what its pathology signals
    /// say about the loop.
    Meta {
        /// Report what would be filed and write nothing to the tracker.
        #[arg(long)]
        dry_run: bool,

        /// Output format: the human report, or the versioned query envelope.
        #[arg(long, value_enum, default_value = "text")]
        format: QueryFormat,
    },
}

/// One hand-run sweep, as a host parses it and as a person reads it.
#[derive(Serialize)]
struct Report {
    /// Which supply was drawn from.
    supply: &'static str,
    /// Whether the tracker was left alone.
    dry_run: bool,
    /// What the supply offered, before the seen set was consulted.
    offered: usize,
    /// Those the seen set does not already hold.
    novel: usize,
    /// Those the tracker took. Zero under `--dry-run`.
    filed: usize,
    /// What the closure ledger could answer, for the supply that reads one.
    #[serde(skip_serializing_if = "Option::is_none")]
    receipts: Option<Receipts>,
    /// The novel findings, in the order the supply offered them.
    findings: Vec<Row>,
}

/// The receipt counts a `regress` sweep read, in the wire shape.
#[derive(Serialize)]
struct Receipts {
    total: usize,
    checked: u64,
    skipped: usize,
}

/// One finding the sweep would file, or did.
#[derive(Serialize)]
struct Row {
    title: String,
    labels: Vec<String>,
    /// The issue key the tracker gave it, when it took it.
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,
}

pub(super) fn run(st: &LoopState, cmd: &SweepCmd) -> Result<(), String> {
    let (name, drawn, dry_run, format) = match cmd {
        SweepCmd::Regress { dry_run, format } => (
            "regress",
            supply::draw_regress(st, &st.repo_root),
            *dry_run,
            *format,
        ),
        SweepCmd::Meta { dry_run, format } => ("meta", supply::draw_meta(st), *dry_run, *format),
    };
    let report = collect(st, name, drawn, dry_run)?;
    render(&report, format)
}

/// Drop what the seen set holds, file what is left, and count both.
///
/// A dry run builds no issue provider. The tracker is not reached at all,
/// not even to be asked a question.
fn collect(
    st: &LoopState,
    supply_name: &'static str,
    drawn: supply::Drawn,
    dry_run: bool,
) -> Result<Report, String> {
    let seen = st.live_seen();
    let novel: Vec<Finding> = stella_autonomy::supply::novel(&drawn.findings, &seen)
        .into_iter()
        .cloned()
        .collect();

    let keys = if dry_run {
        vec![None; novel.len()]
    } else {
        file_each(st, &novel)?
    };

    Ok(Report {
        supply: supply_name,
        dry_run,
        offered: drawn.findings.len(),
        novel: novel.len(),
        filed: keys.iter().filter(|key| key.is_some()).count(),
        receipts: drawn.receipts.map(|counts| Receipts {
            total: counts.total,
            checked: counts.checked,
            skipped: counts.skipped,
        }),
        findings: novel
            .into_iter()
            .zip(keys)
            .map(|(finding, key)| Row {
                title: finding.title,
                labels: finding.labels,
                key,
            })
            .collect(),
    })
}

/// File each finding through the one door, and say what the tracker did.
///
/// A refusal is printed, never raised. The rest of the sweep still has
/// findings worth filing.
fn file_each(st: &LoopState, novel: &[Finding]) -> Result<Vec<Option<String>>, String> {
    let cfg = config::load(&st.repo_root);
    let provider = crate::issue_provider::GhIssueProvider::from_manifest(&cfg.manifest);
    let mut keys = Vec::with_capacity(novel.len());
    for finding in novel {
        match supply::file(st, &provider, &cfg, &st.repo_root, finding) {
            Ok(key) => keys.push(key),
            Err(error) => {
                eprintln!("could not file `{}`: {error}", finding.title);
                keys.push(None);
            }
        }
    }
    Ok(keys)
}

fn render(report: &Report, format: QueryFormat) -> Result<(), String> {
    if format == QueryFormat::Json {
        println!(
            "{}",
            serde_json::to_string_pretty(&Versioned::new(report)).map_err(|e| e.to_string())?
        );
        return Ok(());
    }

    let mode = if report.dry_run {
        " (dry run — nothing was filed)"
    } else {
        ""
    };
    println!("sweep {}{mode}", report.supply);
    if let Some(counts) = &report.receipts {
        println!(
            "  {} receipt(s) on file, {} re-checked, {} could not be",
            counts.total, counts.checked, counts.skipped
        );
    }
    println!(
        "  {} offered, {} the seen set does not hold, {} filed",
        report.offered, report.novel, report.filed
    );
    for row in &report.findings {
        let key = row.key.as_deref().unwrap_or("-");
        println!("  {key}  {}  [{}]", row.title, row.labels.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use stella_autonomy::Citation;
    use stella_autonomy::regress::ClosureReceipt;

    use super::*;

    /// A loop state in an empty directory. A sweep here reads only what the
    /// test wrote.
    fn workspace(tmp: &std::path::Path) -> LoopState {
        let dir = tmp.join("state");
        std::fs::create_dir_all(&dir).expect("state dir");
        LoopState {
            dir,
            repo_root: tmp.to_path_buf(),
        }
    }

    fn write_receipt(st: &LoopState, receipt: &ClosureReceipt) {
        let line = serde_json::to_string(receipt).expect("receipt serializes");
        std::fs::write(st.dir.join("receipts.jsonl"), format!("{line}\n")).expect("receipts");
    }

    /// **The witness.** `sweep regress --dry-run` draws the supply by hand.
    /// A receipt whose cited fix is gone from the base comes back as a
    /// finding. The counts are reported and the tracker is not reached.
    /// Before this change no verb could draw it. The `drive` loop was the
    /// only caller.
    #[test]
    fn a_hand_run_regress_sweep_reports_a_lost_fix_and_files_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let st = workspace(tmp.path());
        write_receipt(
            &st,
            &ClosureReceipt {
                key: "4100".to_owned(),
                closed_at: "2026-09-01T00:00:00Z".to_owned(),
                by: Some(Citation::PullRequest {
                    key: "4101".to_owned(),
                }),
                present_at_close: Some(true),
            },
        );

        let drawn = supply::draw_regress(&st, &st.repo_root);
        let report = collect(&st, "regress", drawn, true).expect("a dry run reaches no tracker");

        assert_eq!(report.supply, "regress");
        assert!(report.dry_run);
        assert_eq!(report.offered, 1);
        assert_eq!(report.novel, 1);
        assert_eq!(report.filed, 0, "a dry run writes nothing to the tracker");
        let counts = report.receipts.expect("the regress sweep reads receipts");
        assert_eq!(counts.total, 1);
        assert_eq!(counts.checked, 1);
        assert_eq!(counts.skipped, 0);
        assert!(
            report.findings[0]
                .title
                .contains("has left the base branch"),
            "{}",
            report.findings[0].title
        );
        assert!(report.findings[0].key.is_none());
    }

    /// A workspace with no receipts reports zero rather than failing. That
    /// is the state every workspace starts in.
    #[test]
    fn a_regress_sweep_over_no_receipts_reports_zero_checked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let st = workspace(tmp.path());

        let drawn = supply::draw_regress(&st, &st.repo_root);
        let report = collect(&st, "regress", drawn, true).expect("a dry run reaches no tracker");

        assert_eq!(report.offered, 0);
        assert_eq!(report.filed, 0);
        let counts = report.receipts.expect("the regress sweep reads receipts");
        assert_eq!(counts.total, 0);
        assert_eq!(counts.checked, 0);
        assert!(report.findings.is_empty());
    }

    /// **The witness.** `sweep meta --dry-run` folds the loop's own
    /// calibration by hand. A ceiling that stayed low through five clean runs
    /// raises `STARVED`. The finding is reported and not filed.
    #[test]
    fn a_hand_run_meta_sweep_reports_a_starved_controller_and_files_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let st = workspace(tmp.path());
        std::fs::write(
            st.dir.join("calibration.json"),
            r#"{"batch_ceiling":3,"parallel_ceiling":2,"clean_run":6,"note":"test"}"#,
        )
        .expect("calibration");

        let drawn = supply::draw_meta(&st);
        let report = collect(&st, "meta", drawn, true).expect("a dry run reaches no tracker");

        assert_eq!(report.supply, "meta");
        assert!(
            report.receipts.is_none(),
            "the meta supply reads the cycle ledger, not receipts"
        );
        assert_eq!(report.novel, 1);
        assert_eq!(report.filed, 0, "a dry run writes nothing to the tracker");
        assert!(
            report.findings[0].title.contains("STARVED"),
            "{}",
            report.findings[0].title
        );
    }

    /// The JSON leads with the envelope version. A host can branch on it
    /// before it reads anything else.
    #[test]
    fn the_json_payload_leads_with_the_envelope_version() {
        let report = Report {
            supply: "meta",
            dry_run: true,
            offered: 0,
            novel: 0,
            filed: 0,
            receipts: None,
            findings: Vec::new(),
        };

        let rendered = serde_json::to_string(&Versioned::new(&report)).expect("serializes");
        assert!(
            rendered.starts_with(r#"{"schema_version":1,"supply":"meta","dry_run":true"#),
            "{rendered}"
        );
    }
}
