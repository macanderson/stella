//! The backlog half of the self-driving verbs, read through the issue port.
//!
//! `doc:backlog-self-driving` B1. Split out of `self_driving_cmd.rs` rather
//! than added to it: that file is close enough to the 1500-line ceiling
//! `make file-size` enforces that new logic lands in siblings here, and it
//! carries no baseline entry, so a crossing fails the gate outright rather
//! than being grandfathered (AGENTS.md § *God files* — plan around them,
//! never into them).
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

use stella_autonomy::escalation::{self, EscalationPolicy, EscalationRecord};
use stella_autonomy::{BacklogConvention, Conformance, Demand, Violation, conform, finding_digest};
use stella_protocol::issue::{IssueDraft, IssueError, IssueKey, IssueLabel, IssueProvider};

use crate::query_format::{QueryFormat, Rows};

use super::state::LoopState;

/// How many open issues cross the port to produce one cycle's batch.
///
/// This is a page size, and a page the backlog outgrows silently changes the
/// answer. The tracker orders what it returns; the ladder is applied here,
/// afterwards, to whatever arrived. `gh issue list` hands back the newest
/// first, so once a backlog is longer than this number the oldest issues stop
/// crossing the port at all — and the loop takes work in ladder order from a
/// set that was chosen by filing date. A P0 filed last year is then invisible
/// to a ranker whose whole job is to find it, with nothing anywhere reporting
/// a truncated read.
///
/// So this is sized to be larger than a backlog rather than smaller than one.
/// Reading a page nobody outgrows costs a few paginated calls per claim pass;
/// the alternative costs the ordering guarantee the ladder exists to provide.
/// A repository that outgrows *this* number has the same defect again, which
/// is what `a_backlog_larger_than_one_page_still_surfaces_its_oldest_p0`
/// pins.
pub(super) const QUEUE_READ_LIMIT: usize = 1_000;

/// What an operator is told when the queue read filled its page.
///
/// A full page means the tracker held at least this many open issues, so the
/// ranking covers only the ones that crossed. The loop reports and carries on
/// rather than refusing, because it can still work the issues it can see and a
/// queue that stops dead on a large backlog helps nobody. Silence is the one
/// option ruled out: a truncated read that looks like a complete one is the
/// whole defect.
///
/// A function rather than an inline print, so what an operator is told can be
/// asserted without running a loop.
fn truncation_notice(total: usize, provider: &str) -> Option<String> {
    (total >= QUEUE_READ_LIMIT).then(|| {
        format!(
            "warning: `{provider}` returned {total} open issues, which fills this read. \
             The ladder ranks that page alone, and the tracker chose it by its own order. \
             Issues behind the page did not reach the ranker."
        )
    })
}

/// Read the tracker once and rank it, or explain why it could not be read.
///
/// The port is async because a tracker is a network service. These callers are
/// short-lived CLI verbs with no runtime of their own, so each blocks on one
/// current-thread runtime rather than making every self-driving verb async for
/// a single awaited call.
pub(super) fn ranked(
    provider: &dyn IssueProvider,
    policy: &stella_autonomy::priority::TriagePolicy,
) -> Result<(stella_autonomy::priority::Queue, usize), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start a runtime for the issue provider: {error}"))?;
    let issues = runtime
        .block_on(provider.list_open(QUEUE_READ_LIMIT))
        .map_err(|error| error.to_string())?;
    let total = issues.len();
    if let Some(notice) = truncation_notice(total, provider.id()) {
        eprintln!("{notice}");
    }
    let queue = stella_autonomy::priority::triage(
        issues
            .iter()
            .map(crate::issue_provider::to_queue_issue)
            .collect(),
        policy,
    );

    // Escalated issues, excluded kinds and the unassessed split all happen
    // inside `triage`, in the one read. The renderer and the driver must not
    // be able to disagree about what the queue holds — that is exactly the
    // two-definitions defect B1 removed from `demand`.
    Ok((queue, total))
}

/// The ranked defect batch this cycle draws from.
pub(super) fn render_queue(
    _st: &LoopState,
    provider: &dyn IssueProvider,
    policy: &stella_autonomy::priority::TriagePolicy,
    limit: usize,
    format: QueryFormat,
) -> Result<(), String> {
    let (queue, total_issues) = ranked(provider, policy)?;
    let defects = queue.ranked;
    let picked = &defects[..limit.min(defects.len())];

    if format == QueryFormat::Json {
        println!(
            "{}",
            serde_json::to_string_pretty(&Rows::new(picked)).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    for i in picked {
        // The operator's rungs, not a built-in list — a tracker spelling
        // urgency `Sev1` must render its own word.
        let prio = policy
            .ladder
            .rungs
            .iter()
            .find(|rung| i.labels.iter().any(|l| &l.name == *rung))
            .map_or("--", String::as_str);
        let area = i
            .labels
            .iter()
            .find(|l| l.name.starts_with("area:"))
            .map(|l| l.name.as_str())
            .unwrap_or("");
        println!("{prio:>2}  #{:<6} {area:<18} {}", i.number, i.title);
    }
    eprintln!(
        "\n{} of {} ranked defects ({} open issues total, {} awaiting triage)",
        picked.len(),
        defects.len(),
        total_issues,
        queue.unassessed.len()
    );
    Ok(())
}

/// Size the demand half of the governor from the same read.
///
/// `Err` when the tracker could not be read, which is **not** a demand of
/// zero. This used to return [`Demand::default`] for both, on the argument
/// that a cycle sized as though the backlog were empty is survivable where a
/// refusal to plan is not. That argument is right about `plan` and wrong about
/// every other reader: `watch` printed `✓ defect queue empty` for an
/// unreachable tracker and stood the loop down on it.
///
/// So the degradation moves to the callers, where it can differ, and neither
/// of them takes it silently — `plan` still sizes against an empty backlog and
/// says that is what it is doing, and `watch` treats the unread queue as a
/// reason to wake.
pub(super) fn demand_from(
    provider: &dyn IssueProvider,
    policy: &stella_autonomy::priority::TriagePolicy,
) -> Result<Demand, String> {
    // The read's failure is reported, not flattened. `Demand::default()` is
    // zero open defects, which is the *answer to a different question* — an
    // unreachable tracker and an empty backlog are opposite facts and used to
    // arrive here as the same number. `watch` then printed
    // `✓ defect queue empty` and stood the loop down, and the governor planned
    // a cycle against a demand nobody had measured.
    let (queue, _) = ranked(provider, policy)?;
    // The most urgent rung the operator declared, whatever they call it.
    let urgent = policy.ladder.most_urgent().unwrap_or_default();
    let p0 = queue
        .ranked
        .iter()
        .filter(|issue| issue.labels.iter().any(|label| label.name == urgent))
        .count();
    Ok(Demand {
        open_defects: u32::try_from(queue.ranked.len()).unwrap_or(u32::MAX),
        p0: u32::try_from(p0).unwrap_or(u32::MAX),
    })
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

impl Filed {
    /// This outcome's canonical name, for
    /// [`stella_autonomy::SessionStats::record_filing`].
    ///
    /// A `&str` rather than the enum, because `stella-autonomy` is a leaf crate
    /// with no workspace dependencies and must not learn what a `Filed` is —
    /// the same boundary `record_closure` takes a resolution across.
    pub(super) fn canonical(&self) -> &'static str {
        match self {
            Filed::New(_) => "new",
            Filed::Duplicate { .. } => "duplicate",
            Filed::Refused { .. } => "refused",
        }
    }
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
pub(super) fn ranked_keys(
    st: &super::state::LoopState,
    provider: &dyn IssueProvider,
    policy: &stella_autonomy::priority::TriagePolicy,
) -> Result<Vec<String>, String> {
    let (queue, total) = ranked(provider, policy)?;
    // The driver's claim pass is the one place the queue is read on a
    // cadence, so it is the one place a snapshot stays current for the
    // observatory — still the single `ranked` read: the keys the loop claims
    // from and the items the dashboard shows are one list, folded twice.
    st.write_queue_snapshot(&queue, &policy.ladder, total);
    Ok(queue
        .ranked
        .into_iter()
        .map(|issue| issue.number.to_string())
        .collect())
}

/// Issues nobody has placed, oldest first.
///
/// Read through the **same** `ranked` call the driver claims from, so the two
/// cannot disagree about which issues are still questions. A driver that ran
/// its own read would be the second definition this module exists to prevent.
pub(super) fn unassessed(
    provider: &dyn IssueProvider,
    policy: &stella_autonomy::priority::TriagePolicy,
) -> Result<Vec<stella_autonomy::priority::Unassessed>, String> {
    let (queue, _) = ranked(provider, policy)?;
    Ok(queue.unassessed)
}

/// The label that marks an issue as *the base branch is broken*.
///
/// A label rather than a title convention, because the title is prose a human
/// may rewrite and the label is the thing both this loop and the next one
/// match on. It is how a restarted process — or a second process entirely —
/// discovers that the emergency is already filed instead of filing it again.
pub(super) const BASE_BREAKAGE_LABEL: &str = "main-red";

/// The open base-breakage issue, if one exists.
///
/// Read through the port like everything else, and matched on
/// [`BASE_BREAKAGE_LABEL`] rather than on words in the title. An unreachable
/// tracker answers `None`, which reads as *nobody has filed it* — the loop
/// then tries to file, the filing fails too, and it waits. That is the right
/// shape: a forge outage should make it wait, not make it act on a guess.
#[must_use]
pub(super) fn open_base_breakage(provider: &dyn IssueProvider) -> Option<String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    let issues = runtime
        .block_on(provider.list_open(QUEUE_READ_LIMIT))
        .ok()?;

    issues
        .into_iter()
        .find(|issue| {
            issue
                .labels
                .iter()
                .any(|label| label.name == BASE_BREAKAGE_LABEL)
        })
        .map(|issue| issue.key.as_str().to_owned())
}

/// File the report that the base branch is broken.
///
/// Deliberately **not** routed through [`file_finding`]. That path dedups on a
/// content digest and conforms the draft to the workspace's convention, both
/// of which are right for a defect the loop discovered by looking at code. A
/// base outage is neither: there is exactly one of them at a time, the dedup
/// key is "is one already open" (which [`open_base_breakage`] answers), and a
/// convention refusal here would leave `main` broken because a label was
/// spelled wrong.
///
/// The body names what a fresh reader needs and nothing this loop cannot
/// actually know: which branch, which checks, and how to reproduce. It does
/// not guess at a cause — the turn that adopts this issue will read the run.
pub(super) fn file_base_breakage(
    provider: &dyn IssueProvider,
    base: &str,
    attribution: &stella_autonomy::Attribution,
) -> Result<String, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start a runtime for the issue provider: {error}"))?;

    let body = stella_autonomy::sign(
        &format!(
            "`{base}` is red on at least one **required** check, so every pull request \
             opened against it inherits the failure.\n\n\
             This blocks the whole repository, not one branch: a contributor whose own \
             diff is clean still sees a red build and cannot tell their failure from \
             this one.\n\n\
             ## Reproduce\n\n\
             ```\n\
             gh run list --branch {base} --workflow ci.yml --limit 3\n\
             ```\n\n\
             Read the newest failing run, then run the failing step locally. \
             `make gate` covers the required checks.\n\n\
             ## Done when\n\n\
             The required checks are green on `{base}`, and the fix names which run \
             it was diagnosed from.\n\n\
             Filed automatically on noticing the base was red and no issue was open. \
             The label `{BASE_BREAKAGE_LABEL}` is what marks it as this — removing the \
             label makes the loop file a second one."
        ),
        &attribution.issue,
    );

    let draft = IssueDraft {
        title: format!("{base} is red — required checks failing on the base branch"),
        body,
        labels: vec![
            IssueLabel::from(BASE_BREAKAGE_LABEL),
            IssueLabel::from("bug"),
            IssueLabel::from("P0"),
        ],
        parent: None,
        assignee: None,
    };

    runtime
        .block_on(provider.file(&draft))
        .map(|key| key.as_str().to_owned())
        .map_err(|error| error.to_string())
}

/// The label that marks an issue as *the release workflow is red*.
///
/// A sibling of [`BASE_BREAKAGE_LABEL`] with the same contract. The label
/// is the dedup key. A restarted process — or a second one — finds the
/// emergency already filed instead of filing it again.
pub(super) const DEPLOY_BREAKAGE_LABEL: &str = "release-red";

/// The open deploy-breakage issue, if one exists.
///
/// Matched on [`DEPLOY_BREAKAGE_LABEL`] and read through the port. It
/// degrades as [`open_base_breakage`] does. An unreachable tracker answers
/// `None`, the filing that follows fails too, and the loop waits rather
/// than acting on a guess.
#[must_use]
pub(super) fn open_deploy_breakage(provider: &dyn IssueProvider) -> Option<String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    let issues = runtime
        .block_on(provider.list_open(QUEUE_READ_LIMIT))
        .ok()?;

    issues
        .into_iter()
        .find(|issue| {
            issue
                .labels
                .iter()
                .any(|label| label.name == DEPLOY_BREAKAGE_LABEL)
        })
        .map(|issue| issue.key.as_str().to_owned())
}

/// File the report that the release workflow is red — once.
///
/// The dedup lives in this function, not in the caller. The property that
/// matters is one open issue per outage, and a caller that filed first and
/// checked second would already have the duplicate in the tracker.
/// `Ok(None)` is the ordinary answer on every poll after the first: the
/// emergency is known, nothing was sent.
///
/// Like [`file_base_breakage`] it bypasses [`file_finding`]. There is one
/// deploy outage at a time, its dedup key is "is one already open", and a
/// convention refusal here would leave releases broken over a label
/// spelling.
pub(super) fn file_deploy_breakage(
    provider: &dyn IssueProvider,
    workflow: &str,
    run_url: &str,
    attribution: &stella_autonomy::Attribution,
) -> Result<Option<String>, String> {
    if open_deploy_breakage(provider).is_some() {
        return Ok(None);
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start a runtime for the issue provider: {error}"))?;

    let body = stella_autonomy::sign(
        &format!(
            "The `{workflow}` workflow's most recent run finished red, so the \
             release path is broken: nothing can ship until it is green again.\n\n\
             Failing run: {run_url}\n\n\
             ## Reproduce\n\n\
             ```\n\
             gh run list --workflow {workflow} --limit 3\n\
             ```\n\n\
             Read the newest failing run, then reproduce the failing step \
             locally where the job permits it.\n\n\
             ## Done when\n\n\
             A run of `{workflow}` completes green, and the fix names which run \
             it was diagnosed from.\n\n\
             Filed automatically on noticing the release workflow was red and no \
             issue was open. The label `{DEPLOY_BREAKAGE_LABEL}` is what marks it \
             as this — removing the label makes the loop file a second one."
        ),
        &attribution.issue,
    );

    let draft = IssueDraft {
        title: format!("{workflow} is red — the release path is broken"),
        body,
        labels: vec![
            IssueLabel::from(DEPLOY_BREAKAGE_LABEL),
            IssueLabel::from("bug"),
            IssueLabel::from("P0"),
        ],
        parent: None,
        assignee: None,
    };

    runtime
        .block_on(provider.file(&draft))
        .map(|key| Some(key.as_str().to_owned()))
        .map_err(|error| error.to_string())
}

/// [`escalate`] for a synchronous caller.
///
/// The driver's arms are synchronous and each awaited port call would
/// otherwise make one runtime inline at the call site — which is how two of
/// them end up spelled differently. One wrapper, one spelling.
pub(super) fn escalate_blocking(
    provider: &dyn IssueProvider,
    key: &str,
    why: &str,
    body: &str,
    policy: &EscalationPolicy,
    signature: &str,
) -> Result<EscalationRecord, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start a runtime for the issue provider: {error}"))?;
    runtime
        .block_on(escalate(
            provider,
            &IssueKey::from(key),
            why,
            body,
            policy,
            signature,
        ))
        .map_err(|error| error.to_string())
}

/// Mark an issue as one the loop tried and could not resolve, and say why.
///
/// Three writes, each for a different reader. The label is the marker a
/// **person** scans for. The comment is what that person reads to learn what
/// went wrong — without it the label is an accusation with no evidence. The
/// record stamped into the body is what the **next run** reads: how many
/// times this has happened, why the last one did, and when.
///
/// The record goes in the body rather than in a comment because the queue
/// read already carries every body (`ready::fold_ready`), so a cooldown
/// costs no extra call to the tracker. It is an HTML comment, so nobody
/// reading the rendered issue sees it.
///
/// Returns the record it wrote, so the caller can count an issue that has
/// just used up its last attempt.
pub(super) async fn escalate(
    provider: &dyn IssueProvider,
    key: &IssueKey,
    why: &str,
    body: &str,
    policy: &EscalationPolicy,
    signature: &str,
) -> Result<EscalationRecord, IssueError> {
    let record = escalation::next(
        escalation::parse(body).as_ref(),
        escalation::classify(why),
        &crate::timefmt::rfc3339_utc_now(),
        crate::timefmt::now_unix(),
    );

    provider
        .relabel(key, &[stella_autonomy::ESCALATION_LABEL.to_owned()], &[])
        .await?;
    provider
        .edit(key, None, Some(&escalation::stamp(body, &record)))
        .await?;

    let next_step = match escalation::retry_after(&record, policy) {
        Some(wait) => format!(
            "The loop will take it again by itself in about {} minutes, once \
             the cooldown is over. Nobody has to remove the label.",
            wait.as_secs() / 60
        ),
        None => format!(
            "That was attempt {} of {}, so the loop will not take it again. \
             It is waiting on a person now.",
            record.attempts, policy.park_after
        ),
    };
    let note = format!(
        "This loop attempted this issue and could not resolve it, so it is \
         labelled `{}`.\n\n\
         What happened: {why}\n\n\
         {next_step}\n\n\
         The work is still wanted — this is not a closure.",
        stella_autonomy::ESCALATION_LABEL
    );
    comment(provider, key, &note, signature).await?;
    Ok(record)
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
    canonical_resolution: &str,
) -> Result<(), IssueError> {
    provider
        .close(
            key,
            &stella_autonomy::sign(receipt, signature),
            canonical_resolution,
        )
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
        edited: std::sync::Mutex<Vec<String>>,
        labelled: std::sync::Mutex<Vec<String>>,
    }

    impl FixtureProvider {
        fn with(open: Vec<Issue>) -> Self {
            Self {
                open,
                ..Self::default()
            }
        }

        fn filed(&self) -> Vec<IssueDraft> {
            self.filed.lock().expect("fixture lock").clone()
        }

        fn edited(&self) -> Vec<String> {
            self.edited.lock().expect("fixture lock").clone()
        }

        fn labelled(&self) -> Vec<String> {
            self.labelled.lock().expect("fixture lock").clone()
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
            add: &[String],
            _remove: &[String],
        ) -> Result<(), IssueError> {
            self.labelled
                .lock()
                .expect("fixture lock")
                .extend(add.iter().cloned());
            Ok(())
        }

        async fn edit(
            &self,
            _key: &IssueKey,
            _title: Option<&str>,
            body: Option<&str>,
        ) -> Result<(), IssueError> {
            if let Some(body) = body {
                self.edited
                    .lock()
                    .expect("fixture lock")
                    .push(body.to_owned());
            }
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
            updated_at: created.into(),
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
            assignee: None,
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

    /// **The witness.** Every filing outcome reaches the session's counters,
    /// not just the one that got a key.
    ///
    /// `filings_refused` and `filings_duplicate` had a `println!` and no write
    /// site anywhere in the workspace, so a loop whose every finding was being
    /// refused rendered identically to a loop that found nothing — beside a
    /// live `created` in the same block (#4118).
    #[test]
    fn every_filing_outcome_reaches_the_session_counters() {
        let dir = tempfile::tempdir().expect("tempdir");
        let st = super::super::state::LoopState {
            dir: dir.path().to_path_buf(),
            repo_root: dir.path().to_path_buf(),
        };
        let provider = FixtureProvider::default();
        let title = "the retry counter survives a goal round";

        for (seen, labels) in [
            // conformant and novel — filed
            (Vec::new(), &["bug", "P1"][..]),
            // conformant but already filed — duplicate
            (vec![finding_digest(title)], &["bug", "P1"][..]),
            // novel but off-convention — refused
            (Vec::new(), &["P1"][..]),
        ] {
            let outcome = block_on(file_finding(
                &provider,
                &convention(),
                &seen,
                &draft(title, labels),
                "Created by stella.",
            ))
            .expect("the port was reachable");
            st.update_stats(|s| s.record_filing(outcome.canonical()));
        }

        let stats = st.stats();
        assert_eq!(stats.filings_attempted, 3);
        assert_eq!(stats.issues_created, 1);
        assert_eq!(stats.filings_duplicate, 1);
        assert_eq!(stats.filings_refused, 1);
        assert!(stats.filings_balance());
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
        let policy = stella_autonomy::priority::TriagePolicy::default();
        let (queue, total) = ranked(&backlog(), &policy).expect("fixture read");
        let order: Vec<u64> = queue.ranked.iter().map(|issue| issue.number).collect();
        assert_eq!(
            order,
            vec![3, 5, 7],
            "P0 first, then P1 aged-before-fresh — and no feature"
        );
        assert_eq!(total, 5, "the total counts every open issue, defect or not");
    }

    /// **Witness.** The ladder must not be applied to a set the tracker chose
    /// by filing date.
    ///
    /// The tracker returns the newest issues first and the ladder is applied
    /// here, to whatever arrived. So a page smaller than the backlog does not
    /// merely cost a few stale issues at the bottom: it decides membership by
    /// date and then ranks by urgency inside that, which is how the oldest P0
    /// in a repository becomes the one issue the ranker cannot see.
    ///
    /// The fixture is the shape that breaks it — a page of fresh P2s, then one
    /// old P0 behind them — sized to 200, so the test fails against a port
    /// asking for that many and passes against a page the backlog has not
    /// outgrown.
    #[test]
    fn a_backlog_larger_than_one_page_still_surfaces_its_oldest_p0() {
        const OLD_PAGE: usize = 200;

        let mut open: Vec<Issue> = (0..OLD_PAGE)
            .map(|n| {
                issue(
                    &format!("{}", n + 100),
                    &["bug", "P2"],
                    "2026-08-01T00:00:00Z",
                )
            })
            .collect();
        // Last, because the tracker hands back the newest first and this is the
        // oldest thing in the repository.
        open.push(issue("7", &["bug", "P0"], "2020-01-01T00:00:00Z"));

        let provider = FixtureProvider::with(open);
        let policy = stella_autonomy::priority::TriagePolicy::default();
        let (queue, total) = ranked(&provider, &policy).expect("fixture read");

        assert_eq!(
            queue.ranked.first().map(|issue| issue.number),
            Some(7),
            "the oldest P0 must outrank a whole page of fresher P2s, which it \
             cannot do while the read stops before reaching it"
        );
        assert_eq!(
            total,
            OLD_PAGE + 1,
            "the reported backlog size counts every open issue, not one page \
             of them"
        );
    }

    /// **Witness.** A read that fills its page says so.
    ///
    /// The page is sized to exceed a backlog, and a repository that outgrows it
    /// has the ordering defect back. What must not come back with it is the
    /// silence. The queue is then ranked over a page the tracker chose by date,
    /// and the ranking alone gives an operator no way to see that.
    #[test]
    fn a_read_that_fills_its_page_reports_the_truncation() {
        assert_eq!(
            truncation_notice(QUEUE_READ_LIMIT - 1, "github"),
            None,
            "a read the backlog has not filled is a complete one and says nothing"
        );

        let notice = truncation_notice(QUEUE_READ_LIMIT, "github")
            .expect("a filled page is a truncated read and has to be reported");
        assert!(
            notice.contains("github"),
            "the notice names the tracker it read: {notice}"
        );
        assert!(
            notice.contains(&QUEUE_READ_LIMIT.to_string()),
            "the notice names how many issues crossed: {notice}"
        );
    }

    /// A defect nobody gave a rung is a question, not the bottom of the queue.
    ///
    /// #9 carries `triage` and no priority label. It used to rank *last*,
    /// below every `P2`, because the old `priority_rank` mapped "no rung" to
    /// the number one past the ladder — so an unjudged issue was
    /// indistinguishable from one somebody had deliberately ranked least
    /// urgent. It now surfaces as unassessed, which is what lets the loop go
    /// and place it instead of burying it.
    #[test]
    fn a_defect_with_no_rung_surfaces_as_a_question() {
        let policy = stella_autonomy::priority::TriagePolicy::default();
        let (queue, _) = ranked(&backlog(), &policy).expect("fixture read");
        assert_eq!(
            queue
                .unassessed
                .iter()
                .map(|u| u.key.as_str())
                .collect::<Vec<_>>(),
            vec!["9"],
            "the untriaged defect is a question the loop must answer"
        );
        assert!(
            !queue.ranked.iter().any(|i| i.number == 9),
            "and it must not also be sitting at the bottom of the ranked work"
        );
    }

    /// The governor's two numbers are folds of that one ranking, so they cannot
    /// disagree with the batch the same cycle draws.
    #[test]
    fn demand_is_a_fold_of_the_same_ranking() {
        let policy = stella_autonomy::priority::TriagePolicy::default();
        let demand = demand_from(&backlog(), &policy).expect("the fake backlog reads");
        assert_eq!(
            demand.open_defects, 3,
            "the feature is not a defect, and the unranked one is not yet work"
        );
        assert_eq!(
            demand.p0, 1,
            "and the feature's P0 label does not make it one"
        );
    }

    /// An unreachable tracker is not an empty backlog.
    ///
    /// This used to assert the opposite — that the read's failure yields
    /// `Demand::default()` — on the argument that a cycle sized as though the
    /// backlog were empty is survivable where a refusal to plan is not. That
    /// argument holds for `plan`, and it still degrades there, out loud. It
    /// does not hold for `watch`, which read the same zero and printed
    /// `✓ defect queue empty` about a queue it had never seen, then stood the
    /// loop down.
    ///
    /// So the degradation belongs to the caller, and this reports what it
    /// read.
    #[test]
    fn an_unreachable_tracker_is_not_a_measured_zero() {
        let answer = demand_from(
            &DeadProvider,
            &stella_autonomy::priority::TriagePolicy::default(),
        );
        assert!(
            answer.is_err(),
            "a tracker that could not be read reports so: {answer:?}"
        );
        assert_ne!(
            answer.ok(),
            Some(Demand::default()),
            "and is distinguishable from a backlog that really is empty"
        );
    }

    /// **The deploy-watch witness.** A red release run is filed exactly
    /// once. The first pass files, with the dedup label riding the draft.
    /// A pass that finds the label already open sends nothing.
    #[test]
    fn a_red_release_run_is_filed_exactly_once() {
        let attribution = stella_autonomy::Attribution::default();
        let run_url = "https://example.invalid/actions/runs/1";

        // Nobody has filed it: one draft goes out, carrying the dedup label.
        let provider = FixtureProvider::default();
        let filed = file_deploy_breakage(&provider, "release.yml", run_url, &attribution)
            .expect("the port was reachable");
        assert_eq!(filed, Some("1001".to_owned()));
        let drafts = provider.filed();
        assert_eq!(drafts.len(), 1);
        assert!(
            drafts[0]
                .labels
                .iter()
                .any(|label| label.name == DEPLOY_BREAKAGE_LABEL),
            "the dedup label must ride the draft, or the next pass re-files"
        );
        assert!(
            drafts[0].body.contains(run_url),
            "the report must name the failing run"
        );

        // The issue is open: the same red run files nothing further.
        let already = FixtureProvider::with(vec![issue(
            "77",
            &[DEPLOY_BREAKAGE_LABEL, "bug", "P0"],
            "2026-08-19T00:00:00Z",
        )]);
        let second = file_deploy_breakage(&already, "release.yml", run_url, &attribution)
            .expect("the port was reachable");
        assert_eq!(second, None, "already filed — nothing to send");
        assert!(already.filed().is_empty());
    }

    /// An unreachable tracker fails the deploy filing rather than
    /// swallowing it — the contract every write in this module keeps.
    #[test]
    fn an_unreachable_tracker_fails_the_deploy_filing() {
        let outcome = file_deploy_breakage(
            &DeadProvider,
            "release.yml",
            "https://example.invalid/actions/runs/1",
            &stella_autonomy::Attribution::default(),
        );
        assert!(outcome.is_err(), "{outcome:?}");
    }

    /// **The escalation-record witness.** Escalating writes the label a
    /// person reads *and* a record the next run reads: the count,
    /// the reason, and the moment. A second escalation on the stamped body
    /// carries the count forward rather than starting over, which is what
    /// makes parking reachable.
    #[test]
    fn escalating_stamps_a_record_the_next_run_can_read() {
        let provider = FixtureProvider::default();
        let policy = stella_autonomy::escalation::EscalationPolicy::default();

        let first = escalate_blocking(
            &provider,
            "17",
            "the turn exited 1 — the same `bash` call with identical arguments \
             produced byte-identical output every time",
            "## What happens\nThe environment was stale.\n",
            &policy,
            "created by stella*",
        )
        .expect("the fixture accepts writes");

        assert_eq!(first.attempts, 1);
        assert_eq!(
            first.last_reason,
            stella_autonomy::escalation::EscalationReason::Environmental(
                stella_autonomy::escalation::EnvCause::StuckLoop
            ),
            "a stuck-loop abort is a broken machine, and it is retried eagerly"
        );
        assert_eq!(
            provider.labelled(),
            vec![stella_autonomy::ESCALATION_LABEL.to_owned()],
            "the label stays as the marker a person scans for"
        );

        let stamped = provider.edited().pop().expect("the body was stamped");
        assert!(
            stamped.starts_with("## What happens"),
            "the issue's own text is kept: {stamped}"
        );
        assert_eq!(
            stella_autonomy::escalation::parse(&stamped).as_ref(),
            Some(&first),
            "the record must survive a read of the body it was written into"
        );

        let second = escalate_blocking(
            &provider,
            "17",
            "the turn ran and could not work out what the issue asks for",
            &stamped,
            &policy,
            "created by stella*",
        )
        .expect("the fixture accepts writes");
        assert_eq!(
            second.attempts, 2,
            "the count carries forward, or parking is never reached"
        );
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
