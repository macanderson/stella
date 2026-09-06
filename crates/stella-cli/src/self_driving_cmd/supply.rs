// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Drawing work from the supplies that do not drain.
//!
//! The ranked queue runs out. Three other supplies do not.
//! `stella_autonomy::supply` holds the rules for them. This module is the half
//! that touches the world. It asks git how far the base has moved, runs the
//! open lens, reads the loop's own files, and files what the rules hand back.
//!
//! Each supply is shut until an operator opens it in `stella.toml`. A build
//! that gains this code changes no running loop.
//!
//! # What one pass does
//!
//! [`pass`] draws from each open supply in turn. What the seen set already
//! holds is dropped first, so a re-pass yields the new code and nothing else.
//! The rest is filed through `backlog::file_finding`, the one door: it checks
//! the digest and the workspace rules before the tracker is reached.
//!
//! A pass that files nothing returns `false`. The driver then shuts the supply
//! for the rest of the run, so the loop cannot ask for the same step again and
//! again.
//!
//! `doc:backlog-self-driving` §4 is the design.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use stella_autonomy::regress::ClosureReceipt;
use stella_autonomy::supply::{Baseline, Finding, Rearm};
use stella_autonomy::{Citation, Closure};
use stella_protocol::issue::{IssueDraft, IssueLabel, IssueProvider};

use super::audit::{self, Action as Audit};
use super::config::LoopConfig;
use super::state::LoopState as Durable;
use super::{backlog, convention, state};

/// The lens the loop has open, or `None` when no supply is.
///
/// Read once per run, before the first step. Anything but `None` makes the
/// machine ask for a sweep when the queue is dry.
///
/// When the ladder is spent the re-arm rule says whether it opens again. That
/// write lands on disk, so the rung the loop picks up survives the run.
pub(super) fn open_lens(durable: &Durable, cfg: &LoopConfig, root: &Path) -> Option<String> {
    if !cfg.supply.any_open() {
        return None;
    }
    let aperture = durable.aperture();
    if aperture != stella_autonomy::WATCH {
        return Some(aperture);
    }

    let base = baseline(durable, root);
    // Decided before the match, so the borrow of `aperture` ends here and the
    // hold arm below can hand the name back.
    let decision = stella_autonomy::supply::rearm(&aperture, &base, &cfg.supply);
    match decision {
        Rearm::Reopen { lens } => match durable.set_aperture(lens) {
            Ok(()) => {
                audit::record(
                    durable,
                    Audit::Swept,
                    None,
                    &format!(
                        "the base has moved since the last clean sweep, so the ladder \
                         re-opens at `{lens}`"
                    ),
                );
                Some(lens.to_owned())
            }
            Err(error) => {
                audit::record(
                    durable,
                    Audit::Transient,
                    None,
                    &format!("could not re-open the ladder at `{lens}`: {error}"),
                );
                None
            }
        },
        // Still a supply to draw from: `regress` and `meta` do not need a
        // lens. The name is what the machine reports, not what it draws from.
        Rearm::Hold { .. } => Some(aperture),
    }
}

/// How one open lens is run and read.
///
/// A seam, so a test can hand [`pass_with`] a lens whose findings it wrote
/// itself. [`Declared`] is the shipped one, and it runs what
/// `stella_autonomy::LENSES` declares.
pub(super) trait LensAudit {
    /// What this lens offers, or why running it failed.
    fn sweep(&self, lens: &str, root: &Path, work_dir: &Path) -> Result<Swept, String>;
}

/// What one run of a lens produced.
pub(super) enum Swept {
    /// The lens ran, and offers these.
    Findings(Vec<Finding>),
    /// Nothing to run here: the name is not a lens, or the lens declares no
    /// mechanical reading. The model-driven audit phase still reads it.
    NotMechanical,
}

/// Seconds one lens command may take before it is stopped.
///
/// Fifteen minutes is a bound and not a measurement. `make supply-chain`
/// resolves a whole dependency graph, so a minute is too short; a lens still
/// running after this has hung, and the loop has other work.
const LENS_TIMEOUT_SECS: u64 = 900;

/// How long to wait between asking whether the lens command has finished.
const LENS_POLL: Duration = Duration::from_millis(200);

/// Where a lens command's output is kept, under the loop's state directory.
const LENS_OUTPUT_FILE: &str = "lens-output.txt";

/// The shipped audit: run what the lens declares, and read what it printed.
pub(super) struct Declared;

impl LensAudit for Declared {
    fn sweep(&self, lens: &str, root: &Path, work_dir: &Path) -> Result<Swept, String> {
        let Some(open) = stella_autonomy::lens(lens) else {
            return Ok(Swept::NotMechanical);
        };
        let stella_autonomy::Tooling::Command {
            run,
            reading: Some(_),
            ..
        } = open.tooling
        else {
            return Ok(Swept::NotMechanical);
        };

        let printed = run_lens(run, root, work_dir)?;
        Ok(Swept::Findings(stella_autonomy::supply::read(
            open, &printed,
        )))
    }
}

/// Run one lens command and hand back everything it printed.
///
/// Both streams go to a file rather than a pipe. A pipe that fills stops the
/// child, and the loop would then be waiting on a command that is waiting on
/// it. The file stays behind for a person to read.
///
/// The exit status is not read. A lens tool exits non-zero exactly when it
/// found something, so a failing run is the finding rather than an error.
fn run_lens(run: &str, root: &Path, work_dir: &Path) -> Result<String, String> {
    let path = work_dir.join(LENS_OUTPUT_FILE);
    let out = std::fs::File::create(&path)
        .map_err(|error| format!("could not open {}: {error}", path.display()))?;
    let errs = out
        .try_clone()
        .map_err(|error| format!("could not open {} twice: {error}", path.display()))?;

    let mut child = Command::new("bash")
        .arg("-lc")
        .arg(run)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(errs))
        .spawn()
        .map_err(|error| format!("could not start `{run}`: {error}"))?;

    let deadline = Instant::now() + Duration::from_secs(LENS_TIMEOUT_SECS);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                return Err(format!(
                    "`{run}` outran {LENS_TIMEOUT_SECS}s and was stopped"
                ));
            }
            Ok(None) => std::thread::sleep(LENS_POLL),
            Err(error) => return Err(format!("could not wait on `{run}`: {error}")),
        }
    }

    std::fs::read_to_string(&path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))
}

/// Run the open lens and add what it found.
///
/// Held to the `rearm` switch. The lens belongs to that supply, and a loop
/// that opened only `meta` must not start running tool commands.
fn draw_lens(
    durable: &Durable,
    root: &Path,
    lens: &str,
    audit_lens: &dyn LensAudit,
    into: &mut Vec<Finding>,
) {
    match audit_lens.sweep(lens, root, &durable.dir) {
        Ok(Swept::Findings(found)) => {
            audit::record(
                durable,
                Audit::Swept,
                None,
                &format!(
                    "{lens}: {} finding(s) from the lens's own tooling",
                    found.len()
                ),
            );
            into.extend(found);
        }
        Ok(Swept::NotMechanical) => audit::record(
            durable,
            Audit::Swept,
            None,
            &format!("{lens}: nothing to run — the audit phase reads this lens"),
        ),
        Err(error) => audit::record(
            durable,
            Audit::Transient,
            None,
            &format!("could not run the `{lens}` lens: {error}"),
        ),
    }
}

/// Draw from every open supply once. `true` when something reached the
/// tracker.
pub(super) fn pass(
    durable: &Durable,
    provider: &dyn IssueProvider,
    cfg: &LoopConfig,
    root: &Path,
    lens: &str,
) -> bool {
    pass_with(durable, provider, cfg, root, lens, &Declared)
}

/// One pass, against a named lens audit.
///
/// Split from [`pass`] so a test can supply the lens rather than the tree.
fn pass_with(
    durable: &Durable,
    provider: &dyn IssueProvider,
    cfg: &LoopConfig,
    root: &Path,
    lens: &str,
    audit_lens: &dyn LensAudit,
) -> bool {
    // What the seen set drops depends on which of this loop's issues have
    // closed, and most of them close with no receipt. Cached on the cycle
    // counter, so a second pass in one cycle asks the tracker nothing.
    super::closures::reconcile(durable, provider);

    let mut findings: Vec<Finding> = Vec::new();

    if cfg.supply.rearm {
        draw_lens(durable, root, lens, audit_lens, &mut findings);
    }

    if cfg.supply.regress {
        let receipts = durable.receipts();
        let report = stella_autonomy::regress::sweep(&receipts, |cite| present_on_base(root, cite));
        audit::record(
            durable,
            Audit::Swept,
            None,
            &format!(
                "regress: {} of {} receipt(s) re-checked, {} could not be, {} fix(es) gone",
                report.checked,
                receipts.len(),
                report.skipped.len(),
                report.findings.len()
            ),
        );
        findings.extend(report.findings);
    }

    if cfg.supply.meta {
        let rows = durable.cycles().rows;
        let found = stella_autonomy::meta::sweep(&rows, &durable.calibration());
        audit::record(
            durable,
            Audit::Swept,
            None,
            &format!(
                "meta: {} finding(s) over {} cycle(s)",
                found.len(),
                rows.len()
            ),
        );
        findings.extend(found);
    }

    let seen = durable.live_seen();
    let fresh: Vec<Finding> = stella_autonomy::supply::novel(&findings, &seen)
        .into_iter()
        .cloned()
        .collect();
    if fresh.is_empty() {
        audit::record(
            durable,
            Audit::Swept,
            None,
            &format!(
                "the `{lens}` sweep found nothing the seen set does not already hold \
                 ({} finding(s) offered)",
                findings.len()
            ),
        );
        return false;
    }

    let mut filed = 0_u32;
    for finding in &fresh {
        match file(durable, provider, cfg, root, finding) {
            Ok(true) => filed += 1,
            Ok(false) => {}
            Err(error) => audit::record(
                durable,
                Audit::Transient,
                None,
                &format!("could not file `{}`: {error}", finding.title),
            ),
        }
    }
    filed > 0
}

/// Write down what a closure cited, and whether that change was on the base.
///
/// Called as an issue closes. Both closing paths funnel through one place, so
/// both write a receipt. The check runs now rather than later so a sweep can
/// tell *gone* from *never seen*.
pub(super) fn record_receipt(durable: &Durable, key: &str, closure: &Closure) {
    let root = durable.repo_root.as_path();
    let by = match closure {
        Closure::Fixed { by } | Closure::Partial { by, .. } => Some(by.clone()),
        Closure::NotPlanned { .. } | Closure::Duplicate { .. } => None,
    };
    let present_at_close = by.as_ref().map(|cite| {
        // The merge that carried the fix may be newer than this clone's copy
        // of the base. Fetch first, or every fix reads as absent and the
        // sweep has nothing it can ever check.
        let _ = state::git(root, &["fetch", "--quiet", "origin"]);
        present_on_base(root, cite)
    });

    let receipt = ClosureReceipt {
        key: key.trim().trim_start_matches('#').to_owned(),
        closed_at: crate::timefmt::rfc3339_utc_now(),
        by,
        present_at_close,
    };
    if let Err(error) = durable.append_receipt(&receipt) {
        audit::record(
            durable,
            Audit::Transient,
            None,
            &format!("could not write the receipt for #{key}: {error}"),
        );
    }
}

/// Whether the change a citation names is on the base branch now.
///
/// A commit is asked about by sha. A pull request is looked for by the `(#N)`
/// a squash merge leaves in the subject line. Anything else answers `false`.
/// A document names a choice, and a choice cannot leave a branch.
fn present_on_base(root: &Path, cite: &Citation) -> bool {
    match cite {
        Citation::Commit { sha } => {
            let sha = sha.trim();
            if sha.is_empty() {
                return false;
            }
            let base = super::work::base_ref(root);
            state::git_ok(root, &["merge-base", "--is-ancestor", sha, base.as_str()])
        }
        Citation::PullRequest { key } => {
            let key = key.trim().trim_start_matches('#');
            if key.is_empty() {
                return false;
            }
            let base = super::work::base_ref(root);
            let needle = format!("--grep=(#{key})");
            state::git(
                root,
                &[
                    "log",
                    "-n",
                    "1",
                    "--format=%H",
                    "--fixed-strings",
                    needle.as_str(),
                    base.as_str(),
                ],
            )
            .is_some()
        }
        Citation::Document { .. } | Citation::ContextRecord { .. } => false,
    }
}

/// How far the base has moved since the last clean sweep.
fn baseline(durable: &Durable, root: &Path) -> Baseline {
    let cal = durable.calibration();
    let head = cal
        .extra
        .get("last_clean_head")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .filter(|head| !head.is_empty() && head.as_str() != "none");
    let at = cal
        .extra
        .get("last_clean_at")
        .and_then(serde_json::Value::as_i64);

    let base = super::work::base_ref(root);
    let commits = head
        .and_then(|head| {
            let range = format!("{head}..{base}");
            state::git(root, &["rev-list", "--count", range.as_str()])
        })
        .and_then(|count| count.trim().parse::<u64>().ok());
    let days = at.map(|then| {
        let elapsed = crate::timefmt::now_unix().saturating_sub(then).max(0);
        elapsed as u64 / 86_400
    });

    Baseline {
        commits,
        days,
        noisy: noisy(durable),
    }
}

/// Whether the loop files far more than it finds.
fn noisy(durable: &Durable) -> bool {
    stella_autonomy::metrics(&durable.cycles().rows)
        .signals
        .iter()
        .any(|signal| signal.code == "NOISY")
}

/// File one finding through the door every filing goes through.
///
/// `Ok(true)` means the tracker took it. A repeat and a refusal are both
/// `Ok(false)`. Neither is an error, and both are counted already.
fn file(
    durable: &Durable,
    provider: &dyn IssueProvider,
    cfg: &LoopConfig,
    root: &Path,
    finding: &Finding,
) -> Result<bool, String> {
    let bound = convention::load(root);
    let draft = IssueDraft {
        title: finding.title.clone(),
        body: finding.body.clone(),
        labels: finding
            .labels
            .iter()
            .map(|name| IssueLabel { name: name.clone() })
            .collect(),
        parent: None,
        assignee: None,
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start a runtime for the issue provider: {error}"))?;
    let outcome = runtime
        .block_on(backlog::file_finding(
            provider,
            &bound.convention,
            &durable.live_seen(),
            &draft,
            &cfg.attribution.issue,
        ))
        .map_err(|error| error.to_string())?;

    durable.update_stats(|stats| stats.record_filing(outcome.canonical()));

    if let backlog::Filed::New(key) = &outcome {
        durable.record_filing(
            &stella_autonomy::finding_digest(&finding.title),
            &key.to_string(),
        )?;
        audit::record(
            durable,
            Audit::IssueFiled,
            Some(&key.to_string()),
            &finding.title,
        );
        return Ok(true);
    }

    audit::record(
        durable,
        Audit::Swept,
        None,
        &format!("not filed ({}): {}", outcome.canonical(), finding.title),
    );
    Ok(false)
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use stella_protocol::issue::{Issue, IssueError, IssueKey};

    use super::*;

    /// A lens that offers one finding, whatever the tree holds.
    ///
    /// What is under test is the driver's own draw, so the fixture stands in
    /// for the tool rather than running one.
    struct OneFinding;

    impl LensAudit for OneFinding {
        fn sweep(&self, lens: &str, _root: &Path, _work_dir: &Path) -> Result<Swept, String> {
            Ok(Swept::Findings(vec![Finding {
                title: format!("the `{lens}` lens found a defect in the fixture"),
                body: "The fixture lens offers one finding.".to_owned(),
                labels: vec!["defect".to_owned()],
            }]))
        }
    }

    /// A tracker that keeps every draft, so a test can count what was filed.
    #[derive(Default)]
    struct FixtureProvider {
        filed: std::sync::Mutex<Vec<IssueDraft>>,
    }

    impl FixtureProvider {
        fn filed(&self) -> usize {
            self.filed.lock().expect("fixture lock").len()
        }
    }

    #[async_trait]
    impl IssueProvider for FixtureProvider {
        fn id(&self) -> &str {
            "fixture"
        }

        async fn list_open(&self, _limit: usize) -> Result<Vec<Issue>, IssueError> {
            Ok(Vec::new())
        }

        async fn file(&self, draft: &IssueDraft) -> Result<IssueKey, IssueError> {
            let mut filed = self.filed.lock().expect("fixture lock");
            filed.push(draft.clone());
            Ok(IssueKey::from(format!("{}", 1000 + filed.len()).as_str()))
        }

        async fn close(
            &self,
            _key: &IssueKey,
            _receipt: &str,
            _state: &str,
        ) -> Result<(), IssueError> {
            Ok(())
        }

        async fn comment(&self, _key: &IssueKey, _body: &str) -> Result<(), IssueError> {
            Ok(())
        }

        async fn relabel(
            &self,
            _key: &IssueKey,
            _add: &[String],
            _remove: &[String],
        ) -> Result<(), IssueError> {
            Ok(())
        }

        async fn edit(
            &self,
            _key: &IssueKey,
            _title: Option<&str>,
            _body: Option<&str>,
        ) -> Result<(), IssueError> {
            Ok(())
        }
    }

    /// A workspace whose convention is bound, so a draft can be filed at all.
    /// A proposed convention refuses every filing, which would hide what this
    /// test is asking about.
    fn bind_convention(root: &Path) {
        let dir = root.join(".stella").join("issues");
        std::fs::create_dir_all(&dir).expect("issues dir");
        std::fs::write(
            dir.join("convention.json"),
            r#"{"axes":[{"name":"kind","members":["defect"],
                 "requirement":"exactly_one","source":"declared"}],
                 "reserved":[],"acceptance":"bound"}"#,
        )
        .expect("convention");
    }

    /// **The witness.** A re-armed lens produces a finding inside one pass,
    /// and a second pass over the same tree files nothing because the digest
    /// is in the seen set.
    ///
    /// Before this change `pass` drew from `regress` and `meta` alone, so a
    /// pass with both shut filed nothing and returned `false`.
    #[test]
    fn a_rearmed_lens_is_drawn_from_once_and_then_deduped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        bind_convention(root);
        let dir = root.join("state");
        std::fs::create_dir_all(&dir).expect("state dir");

        let durable = Durable {
            dir,
            repo_root: root.to_path_buf(),
        };
        let cfg = LoopConfig {
            supply: stella_autonomy::supply::SupplyPolicy {
                rearm: true,
                ..stella_autonomy::supply::SupplyPolicy::default()
            },
            ..LoopConfig::default()
        };
        let provider = FixtureProvider::default();

        let first = pass_with(&durable, &provider, &cfg, root, "rubric", &OneFinding);
        assert!(first, "the lens's finding reached the tracker");
        assert_eq!(provider.filed(), 1);

        let again = pass_with(&durable, &provider, &cfg, root, "rubric", &OneFinding);
        assert!(!again, "the same finding is not filed twice");
        assert_eq!(provider.filed(), 1, "the seen set holds that digest now");
    }

    /// A loop that opened only `meta` never runs a lens command, so a build
    /// with this code cannot start spawning tools somebody did not ask for.
    #[test]
    fn a_shut_rearm_switch_draws_no_lens() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        bind_convention(root);
        let dir = root.join("state");
        std::fs::create_dir_all(&dir).expect("state dir");

        let durable = Durable {
            dir,
            repo_root: root.to_path_buf(),
        };
        let provider = FixtureProvider::default();

        let filed = pass_with(
            &durable,
            &provider,
            &LoopConfig::default(),
            root,
            "rubric",
            &OneFinding,
        );

        assert!(!filed);
        assert_eq!(provider.filed(), 0);
    }

    /// A lens whose tooling has no reading offers nothing, and the driver says
    /// so rather than reporting an empty sweep.
    #[test]
    fn a_model_only_lens_is_left_to_the_audit_phase() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let swept = Declared
            .sweep("concurrency", tmp.path(), tmp.path())
            .expect("a lens with no command runs nothing");

        assert!(matches!(swept, Swept::NotMechanical));
    }

    /// **The witness.** A workspace that says nothing about supplies opens
    /// none, so a build that gains this code changes no running loop.
    #[test]
    fn a_default_workspace_opens_no_supply() {
        let cfg = LoopConfig::default();

        assert!(!cfg.supply.any_open());
        assert!(!cfg.supply.rearm);
        assert!(!cfg.supply.regress);
        assert!(!cfg.supply.meta);
    }

    /// The document's own switches reach the policy the loop reads.
    #[test]
    fn the_document_switches_reach_the_policy() {
        use crate::settings::toml_config::{SupplySection, SupplySwitch};

        let section = SupplySection {
            rearm: SupplySwitch::On,
            regress: SupplySwitch::Off,
            meta: SupplySwitch::On,
            rearm_commits: Some(7),
            rearm_days: None,
        };
        let policy = section.policy();

        assert!(policy.rearm);
        assert!(!policy.regress);
        assert!(policy.meta);
        assert_eq!(policy.rearm_commits, 7);
        assert_eq!(
            policy.rearm_days,
            stella_autonomy::supply::DEFAULT_REARM_DAYS
        );
    }

    /// A decision cannot leave a branch, so a document citation is never
    /// reported as a fix that went missing.
    #[test]
    fn an_authority_citation_is_never_present_on_a_branch() {
        let cite = Citation::Document {
            doc_id: "doc:backlog-self-driving".to_owned(),
        };

        assert!(!present_on_base(Path::new("."), &cite));
    }
}
