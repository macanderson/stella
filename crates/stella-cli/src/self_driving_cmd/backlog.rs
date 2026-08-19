//! The backlog half of the self-driving verbs, read through the issue port.
//!
//! `doc:backlog-self-driving` B1. Split out of `self_driving_cmd.rs` rather
//! than added to it: that file is 1005 lines and #3599 records it as closed to
//! growth, with new logic landing in siblings here (AGENTS.md § *God files* —
//! plan around them, never into them).
//!
//! The parent keeps the two verb entry points, which name the concrete
//! provider. Everything below takes a `&dyn IssueProvider` and has never heard
//! of GitHub — which is what makes "GitHub is an adapter, not the answer" a
//! property a test can falsify rather than a claim about the code's shape.
//!
//! # Why these two verbs are one module
//!
//! `queue` and the governor's `demand` are the same read. Before B1 they were
//! three separate `gh` invocations carrying **two** definitions of the word
//! "defect" — `rank_defects`'s label filter, and a `--label bug` flag written
//! into `demand`'s argv — which could disagree with each other about the very
//! backlog one cycle drew its batch from. One read, one definition, folded two
//! ways.

use stella_autonomy::{BacklogConvention, Conformance, Demand, Violation, conform, finding_digest};
use stella_protocol::issue::{IssueDraft, IssueError, IssueKey, IssueProvider};

use crate::query_format::{QueryFormat, Rows};

use super::state::LoopState;

/// How many open issues cross the port to produce one cycle's batch.
///
/// A ceiling rather than "all of them": ranking is deterministic and a batch is
/// single digits, so reading a ten-thousand-issue backlog to pick five is spend
/// with no effect on the answer. 200 is what the shell driver asked `gh` for,
/// kept unchanged so the picked batch cannot move in the change that relocates
/// the read.
pub(super) const QUEUE_READ_LIMIT: usize = 200;

/// Read the tracker once and rank it, or explain why it could not be read.
///
/// The port is async because a tracker is a network service. These callers are
/// short-lived CLI verbs with no runtime of their own, so each blocks on one
/// current-thread runtime rather than making every self-driving verb async for
/// a single awaited call.
fn ranked(
    provider: &dyn IssueProvider,
) -> Result<(Vec<stella_autonomy::QueueIssue>, usize), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start a runtime for the issue provider: {error}"))?;
    let issues = runtime
        .block_on(provider.list_open(QUEUE_READ_LIMIT))
        .map_err(|error| error.to_string())?;
    let total = issues.len();
    let defects = stella_autonomy::rank_defects(
        issues
            .iter()
            .map(crate::issue_provider::to_queue_issue)
            .collect(),
    );
    Ok((defects, total))
}

/// The ranked defect batch this cycle draws from.
pub(super) fn render_queue(
    _st: &LoopState,
    provider: &dyn IssueProvider,
    limit: usize,
    format: QueryFormat,
) -> Result<(), String> {
    let (defects, total_issues) = ranked(provider)?;
    let picked = &defects[..limit.min(defects.len())];

    if format == QueryFormat::Json {
        println!(
            "{}",
            serde_json::to_string_pretty(&Rows::new(picked)).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    for i in picked {
        let prio = ["P0", "P1", "P2"]
            .into_iter()
            .find(|p| i.labels.iter().any(|l| l.name == *p))
            .unwrap_or("--");
        let area = i
            .labels
            .iter()
            .find(|l| l.name.starts_with("area:"))
            .map(|l| l.name.as_str())
            .unwrap_or("");
        println!("{prio:>2}  #{:<6} {area:<18} {}", i.number, i.title);
    }
    eprintln!(
        "\n{} of {} open defects ({} open issues total)",
        picked.len(),
        defects.len(),
        total_issues
    );
    Ok(())
}

/// Size the demand half of the governor from the same read.
///
/// A tracker that cannot be reached yields [`Demand::default`] rather than an
/// error: the governor's job is to size a cycle, and a cycle sized as though
/// the backlog were empty is a survivable answer where a refusal to plan is
/// not. That degradation is inherited from the `gh_available()` check this
/// replaces, not introduced here.
pub(super) fn demand_from(provider: &dyn IssueProvider) -> Demand {
    let Ok((defects, _)) = ranked(provider) else {
        return Demand::default();
    };
    let p0 = defects
        .iter()
        .filter(|issue| issue.labels.iter().any(|label| label.name == "P0"))
        .count();
    Demand {
        open_defects: u32::try_from(defects.len()).unwrap_or(u32::MAX),
        p0: u32::try_from(p0).unwrap_or(u32::MAX),
    }
}

/// Why a filing did not happen.
///
/// Three outcomes rather than a `Result<IssueKey, IssueError>`, because
/// **"already filed" is a success from the loop's point of view** and an error
/// from the tracker's. Collapsing it into the error arm is how a loop starts
/// reporting failures for working correctly, and how a human learns to ignore
/// them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Filed {
    /// The tracker assigned it a key.
    New(IssueKey),
    /// A finding with this digest was filed before. Nothing was sent.
    Duplicate {
        /// The dedup key that matched.
        digest: String,
    },
    /// The draft does not conform to this workspace's convention. Nothing was
    /// sent.
    Refused {
        /// Every violation, so a human fixes them in one pass.
        violations: Vec<Violation>,
    },
}

/// File a finding, if it is both novel and conformant.
///
/// The order is the design, and both checks happen **before the provider is
/// reached**:
///
/// 1. **Dedup by digest**, against the same `seen.txt` contract the aperture
///    ladder rests on. Re-filing what the loop already filed is the fastest way
///    to make a backlog worthless, and `finding_digest` normalization is what
///    makes "the same defect with a shifted line number" one finding.
/// 2. **Conform to the workspace's convention.** A filing the tracker's own
///    automation will re-label is a filing the loop reads back next cycle as
///    somebody else's untriaged defect (`stella_autonomy::convention`).
///
/// A caller cannot get this order wrong because there is no other way in: the
/// provider's `file` is never called directly by a verb.
pub(super) async fn file_finding(
    provider: &dyn IssueProvider,
    convention: &BacklogConvention,
    seen: &[String],
    draft: &IssueDraft,
    signature: &str,
) -> Result<Filed, IssueError> {
    let digest = finding_digest(&draft.title);
    if seen.iter().any(|s| s == &digest) {
        return Ok(Filed::Duplicate { digest });
    }

    let labels: Vec<&str> = draft.labels.iter().map(|l| l.name.as_str()).collect();
    if let Conformance::Refused { violations } = conform(convention, &labels) {
        return Ok(Filed::Refused { violations });
    }

    // Signed at the last moment, so the dedup digest and the conformance check
    // both see the text a human wrote rather than the loop's own footer — a
    // signature that varied by distribution would otherwise change the digest
    // and re-file every finding.
    let signed = IssueDraft {
        body: stella_autonomy::sign(&draft.body, signature),
        ..draft.clone()
    };
    provider.file(&signed).await.map(Filed::New)
}

/// The ranked defect queue as bare keys, in the order the loop should take
/// them.
///
/// The same read and the same ranking `queue` renders — one definition of
/// "defect", folded a third way. A driver that filtered the queue itself would
/// be the second definition B1 removed.
pub(super) fn ranked_keys(provider: &dyn IssueProvider) -> Result<Vec<String>, String> {
    let (defects, _) = ranked(provider)?;
    Ok(defects
        .into_iter()
        .map(|issue| issue.number.to_string())
        .collect())
}

/// Resolve one issue by key, through the port.
///
/// Reads the open queue and finds the key rather than asking the tracker for
/// one issue, and that is a deliberate limit rather than an oversight: the port
/// has no single-issue read, and adding one to serve a caller that only ever
/// works **open** issues would widen a trait for a case that does not exist
/// yet. The loop works what it drew from the ranked queue, and a claim lives in
/// the fleet ledger rather than in the tracker's assignee field — so an issue
/// being worked is still `Open` and still in this read.
///
/// A key that is not in the open queue is a typed refusal naming the two
/// reasons a caller can act on: it is closed, or it does not exist.
pub(super) fn resolve(
    provider: &dyn IssueProvider,
    key: &str,
) -> Result<stella_protocol::issue::Issue, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start a runtime for the issue provider: {error}"))?;
    let issues = runtime
        .block_on(provider.list_open(QUEUE_READ_LIMIT))
        .map_err(|error| error.to_string())?;

    issues
        .into_iter()
        .find(|issue| issue.key.as_str() == key)
        .ok_or_else(|| {
            format!(
                "#{key} is not in the open queue — it is closed, or it does not exist \
                 (read {QUEUE_READ_LIMIT} open issues from `{}`)",
                provider.id()
            )
        })
}

/// Close an issue with a receipt naming the evidence, signed.
///
/// The receipt is an issue comment, so it takes the `issue_comment` signature
/// rather than the `issue` one: the two are read in different places and a
/// deployment will want different words.
pub(super) async fn close_with_receipt(
    provider: &dyn IssueProvider,
    key: &IssueKey,
    receipt: &str,
    signature: &str,
    state: &str,
) -> Result<(), IssueError> {
    provider
        .close(key, &stella_autonomy::sign(receipt, signature), state)
        .await
}

/// Close an issue whose receipt is already on the trail.
///
/// The partial-closure path posts the receipt as a standalone comment *before*
/// closing (`lifecycle::close_issue` — the remainder link has to survive a
/// failed close), so attaching the same receipt to the close would leave the
/// identical signed comment on the issue twice. This closes in the terminal
/// state without a second comment.
pub(super) async fn close_bare(
    provider: &dyn IssueProvider,
    key: &IssueKey,
    state: &str,
) -> Result<(), IssueError> {
    provider.close(key, "", state).await
}

/// Bring an issue's labels up to this workspace's convention.
///
/// Stella owns the backlog, so an issue that arrived unclassified is hers to
/// classify rather than hers to refuse. This is the repair half of
/// `stella_autonomy::conform`: the same rule that decides whether a filing may
/// go out decides what an existing issue is missing.
///
/// It only ever **adds** what the convention requires and **removes** what it
/// reserves. Nothing else is touched — a label the loop does not understand is
/// somebody's deliberate choice, and stripping it would be the loop asserting
/// authority over a vocabulary it did not learn.
pub(super) async fn relabel(
    provider: &dyn IssueProvider,
    key: &IssueKey,
    add: &[String],
    remove: &[String],
) -> Result<(), IssueError> {
    provider.relabel(key, add, remove).await
}

/// Post a comment on an issue, signed.
pub(super) async fn comment(
    provider: &dyn IssueProvider,
    key: &IssueKey,
    body: &str,
    signature: &str,
) -> Result<(), IssueError> {
    provider
        .comment(key, &stella_autonomy::sign(body, signature))
        .await
}

/// Explain a filing that did not happen.
///
/// A refusal is rendered with **every** violation and with where the convention
/// came from, because the two questions a human has are "what did I get wrong"
/// and "wrong according to what" — and the second one is unanswerable if the
/// convention was discovered rather than written down.
pub(super) fn render_not_filed(
    outcome: &Filed,
    bound: &super::convention::Bound,
    format: QueryFormat,
) -> Result<(), String> {
    match outcome {
        Filed::New(_) => Ok(()),
        Filed::Duplicate { digest } => {
            if format == QueryFormat::Json {
                println!(r#"{{"duplicate":"{digest}"}}"#);
            } else {
                println!("already filed (digest {digest}) — nothing sent");
            }
            Ok(())
        }
        Filed::Refused { violations } => {
            let source = match bound.provenance {
                super::convention::Provenance::Manifest => ".stella/issues/convention.json",
                super::convention::Provenance::Discovered => {
                    ".github/workflows/issue-triage.yml (discovered)"
                }
                super::convention::Provenance::Proposed => {
                    "a PROPOSED convention — nothing may be filed until a human accepts it"
                }
            };
            if format == QueryFormat::Json {
                println!(
                    "{}",
                    serde_json::json!({ "refused": violations, "convention": source })
                );
                // Still a failure exit: a caller that scripted this must not
                // read "nothing was filed" as "the finding is recorded".
                return Err(
                    "not filed: the draft does not match this workspace's convention".into(),
                );
            }

            // The whole explanation rides in the error rather than being
            // printed here, so the binary renders it once, in its own voice.
            // Printing first and returning an empty error produced a bare
            // `stella:` line and looked like a crash.
            let mut message =
                String::from("not filed — the draft does not match this workspace's convention\n");
            message.push_str(&format!("  convention: {source}\n"));
            for violation in violations {
                message.push_str(&format!("  {}\n", describe(violation)));
            }
            Err(message.trim_end().to_owned())
        }
    }
}

fn describe(violation: &Violation) -> String {
    match violation {
        Violation::AxisMissing { axis, candidates } => {
            format!(
                "missing a `{axis}` label — one of: {}",
                candidates.join(", ")
            )
        }
        Violation::AxisAmbiguous { axis, present } => format!(
            "several `{axis}` labels ({}) — exactly one decides the rank",
            present.join(", ")
        ),
        Violation::ReservedApplied { label } => {
            format!("`{label}` is applied by automation, never by the loop")
        }
        Violation::ConventionUnaccepted => {
            "this workspace's convention has not been accepted by a human".to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use stella_protocol::issue::{Issue, IssueClass, IssueError, IssueKey, IssueLabel, IssueState};

    use super::*;

    /// A tracker that is not GitHub and is not a process.
    ///
    /// Records what it was asked to write, so a test can assert not only what
    /// was filed but that nothing **was** — which is the half that matters for
    /// a refusal, since a conformance check that ran after the write would pass
    /// the same assertions on the returned value.
    #[derive(Default)]
    struct FixtureProvider {
        open: Vec<Issue>,
        filed: std::sync::Mutex<Vec<IssueDraft>>,
    }

    impl FixtureProvider {
        fn with(open: Vec<Issue>) -> Self {
            Self {
                open,
                filed: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn filed(&self) -> Vec<IssueDraft> {
            self.filed.lock().expect("fixture lock").clone()
        }
    }

    #[async_trait]
    impl IssueProvider for FixtureProvider {
        fn id(&self) -> &str {
            "fixture"
        }

        async fn list_open(&self, limit: usize) -> Result<Vec<Issue>, IssueError> {
            Ok(self.open.iter().take(limit).cloned().collect())
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

    /// A tracker that is not reachable — the degradation path.
    struct DeadProvider;

    impl DeadProvider {
        fn gone() -> IssueError {
            IssueError::Unavailable {
                provider: "dead".into(),
                reason: "no tracker here".into(),
            }
        }
    }

    #[async_trait]
    impl IssueProvider for DeadProvider {
        fn id(&self) -> &str {
            "dead"
        }

        async fn list_open(&self, _limit: usize) -> Result<Vec<Issue>, IssueError> {
            Err(Self::gone())
        }

        async fn file(&self, _draft: &IssueDraft) -> Result<IssueKey, IssueError> {
            Err(Self::gone())
        }

        async fn close(
            &self,
            _key: &IssueKey,
            _receipt: &str,
            _state: &str,
        ) -> Result<(), IssueError> {
            Err(Self::gone())
        }

        async fn comment(&self, _key: &IssueKey, _body: &str) -> Result<(), IssueError> {
            Err(Self::gone())
        }

        async fn relabel(
            &self,
            _key: &IssueKey,
            _add: &[String],
            _remove: &[String],
        ) -> Result<(), IssueError> {
            Err(Self::gone())
        }

        async fn edit(
            &self,
            _key: &IssueKey,
            _title: Option<&str>,
            _body: Option<&str>,
        ) -> Result<(), IssueError> {
            Err(Self::gone())
        }
    }

    fn issue(key: &str, labels: &[&str], created: &str) -> Issue {
        Issue {
            key: IssueKey::from(key),
            title: format!("issue {key}"),
            body: String::new(),
            state: IssueState::Open,
            class: IssueClass::Bug,
            labels: labels.iter().copied().map(IssueLabel::from).collect(),
            created_at: created.into(),
            url: String::new(),
            parent: None,
        }
    }

    /// This repository's convention, as `issue-triage.yml` enforces it.
    fn convention() -> BacklogConvention {
        use stella_autonomy::{Acceptance, AxisRequirement, ConventionSource, LabelAxis};
        BacklogConvention {
            axes: vec![LabelAxis {
                name: "type".into(),
                members: ["bug", "feature", "epic", "documentation", "question"]
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect(),
                requirement: AxisRequirement::ExactlyOne,
                source: ConventionSource::Enforced,
            }],
            reserved: vec!["triage".into()],
            acceptance: Acceptance::Bound,
        }
    }

    fn draft(title: &str, labels: &[&str]) -> IssueDraft {
        IssueDraft {
            title: title.into(),
            body: "a handoff".into(),
            labels: labels.iter().copied().map(IssueLabel::from).collect(),
            parent: None,
        }
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(f)
    }

    /// **The write-half witness.** A non-conformant draft is refused *before the
    /// provider is reached*, and the fixture records that nothing was sent.
    ///
    /// Asserting only the returned value would pass on an implementation that
    /// filed first and checked afterwards — which is the bug, since the issue
    /// would already be in the tracker.
    #[test]
    fn a_non_conformant_filing_never_reaches_the_tracker() {
        let provider = FixtureProvider::default();

        let outcome = block_on(file_finding(
            &provider,
            &convention(),
            &[],
            &draft("the retry counter survives a goal round", &["P1"]),
            "Created by stella.",
        ))
        .expect("the port was reachable");

        assert!(
            matches!(outcome, Filed::Refused { .. }),
            "expected a refusal, got {outcome:?}"
        );
        assert!(
            provider.filed().is_empty(),
            "a refused filing must not reach the tracker, but it recorded {:?}",
            provider.filed()
        );
    }

    /// The other half — the identical draft, classified, does reach it.
    #[test]
    fn a_conformant_filing_reaches_the_tracker_and_returns_its_key() {
        let provider = FixtureProvider::default();

        let outcome = block_on(file_finding(
            &provider,
            &convention(),
            &[],
            &draft("the retry counter survives a goal round", &["bug", "P1"]),
            "Created by stella.",
        ))
        .expect("the port was reachable");

        assert_eq!(outcome, Filed::New(IssueKey::from("1001")));
        assert_eq!(provider.filed().len(), 1);
    }

    /// Re-filing what the loop already filed is the fastest way to make a
    /// backlog worthless, and the dedup runs before the conformance check
    /// because a duplicate should cost nothing at all.
    #[test]
    fn a_finding_already_seen_is_not_filed_again() {
        let provider = FixtureProvider::default();
        let title = "the retry counter survives a goal round";
        let seen = vec![finding_digest(title)];

        let outcome = block_on(file_finding(
            &provider,
            &convention(),
            &seen,
            &draft(title, &["bug"]),
            "Created by stella.",
        ))
        .expect("the port was reachable");

        assert!(matches!(outcome, Filed::Duplicate { .. }), "{outcome:?}");
        assert!(provider.filed().is_empty());
    }

    /// `finding_digest` normalizes line numbers, so the same defect reported
    /// against a shifted line is one finding — the property the aperture
    /// ladder's dry streak rests on, now also governing what gets filed.
    #[test]
    fn the_same_defect_at_a_shifted_line_is_one_finding() {
        let provider = FixtureProvider::default();
        let seen = vec![finding_digest("driver.rs:412 retry counter leaks")];

        let outcome = block_on(file_finding(
            &provider,
            &convention(),
            &seen,
            &draft("driver.rs:987 retry counter leaks", &["bug"]),
            "Created by stella.",
        ))
        .expect("the port was reachable");

        assert!(matches!(outcome, Filed::Duplicate { .. }), "{outcome:?}");
        assert!(provider.filed().is_empty());
    }

    /// A tracker that cannot be written to is an error, never a silent
    /// success — the loop must not believe it filed what it found.
    #[test]
    fn an_unreachable_tracker_fails_the_filing_rather_than_swallowing_it() {
        let outcome = block_on(file_finding(
            &DeadProvider,
            &convention(),
            &[],
            &draft("something", &["bug"]),
            "Created by stella.",
        ));

        assert!(
            matches!(outcome, Err(IssueError::Unavailable { .. })),
            "{outcome:?}"
        );
    }

    fn backlog() -> FixtureProvider {
        FixtureProvider::with(vec![
            issue("7", &["bug", "P1"], "2026-08-19T00:00:00Z"),
            issue("9", &["triage"], "2026-08-01T00:00:00Z"),
            issue("3", &["bug", "P0"], "2026-08-18T00:00:00Z"),
            issue("5", &["bug", "P1"], "2026-08-02T00:00:00Z"),
            // Feature work is deliberately not a defect: this loop closes
            // defects, and a mixed batch is unreviewable.
            issue("11", &["feature", "P0"], "2026-07-01T00:00:00Z"),
        ])
    }

    /// **The B1 witness.** A ranked defect queue is produced end to end — read,
    /// mapped, ranked — from a provider that is not GitHub, holds no
    /// credential, and runs no subprocess.
    ///
    /// It cannot compile on `main`: there is no `IssueProvider` to implement
    /// there, because the queue read *is* the `gh` call. That is the property
    /// under test — not that ranking works, which it already did, but that
    /// ranking no longer requires GitHub to exist.
    #[test]
    fn a_ranked_queue_needs_no_github_at_all() {
        let (defects, total) = ranked(&backlog()).expect("fixture read");
        let order: Vec<u64> = defects.iter().map(|issue| issue.number).collect();
        assert_eq!(
            order,
            vec![3, 5, 7, 9],
            "P0 first, then P1 aged-before-fresh, then triage — and no feature"
        );
        assert_eq!(total, 5, "the total counts every open issue, defect or not");
    }

    /// The governor's two numbers are folds of that one ranking, so they cannot
    /// disagree with the batch the same cycle draws.
    #[test]
    fn demand_is_a_fold_of_the_same_ranking() {
        let demand = demand_from(&backlog());
        assert_eq!(demand.open_defects, 4, "the feature is not a defect");
        assert_eq!(demand.p0, 1, "and its P0 label does not make it one");
    }

    /// An unreachable tracker sizes a cycle as though the backlog were empty,
    /// rather than refusing to plan.
    #[test]
    fn an_unreachable_tracker_degrades_rather_than_failing() {
        assert_eq!(demand_from(&DeadProvider), Demand::default());
    }

    /// The limit bounds what crosses the port, not what the ranker discards
    /// afterwards.
    #[test]
    fn the_read_limit_bounds_the_crossing() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let got = runtime.block_on(backlog().list_open(2)).expect("read");
        assert_eq!(got.len(), 2);
    }
}
