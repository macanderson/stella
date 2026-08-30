//! The skill-appraisal ledger — the durable half of the measured promote/retire
//! gate (#1067, #1068).
//!
//! `stella-core`'s [`stella_core::skills::appraisal`] decides; this module is
//! where the evidence lives between sessions, because neither direction of that
//! gate can be answered from one turn:
//!
//! - **Promotion** needs a with-skill/without-skill comparison over a task set,
//!   and a candidate that has never been injected has no with-skill arm at all.
//!   So a candidate the miner raises is *queued* here and a later appraisal
//!   promotes it. Queued is not discarded: the mining log keeps every
//!   observation, so a candidate that is never appraised simply waits.
//! - **Retirement** needs a window of turns, joined from *selection* to
//!   *outcome*. That join is recorded explicitly, one trial per known skill per
//!   turn — never inferred from the work a turn produced, which would credit a
//!   skill for any turn that happened to touch its subject.
//!
//! Both files are append-only JSONL under `.stella/private/` (0700, files
//! 0600), for the same reason `trace.rs` is: they carry prompts' worth of
//! context about what a workspace does, and nothing here reaches a store table
//! an egress path reads (AGENTS.md invariant 3).
//!
//! # How the loop closes
//!
//! Every piece here has a production caller:
//!
//! - [`record_turn`] is called at turn end from the episode seam
//!   (`SessionMemory::record_episode`), with the join the turn-start seam
//!   noted — every skill the loader offered, and the subset selection
//!   actually injected.
//! - [`sweep`] runs on the post-turn reflection path
//!   (`SessionMemory::auto_create_skills`), appraises the accumulated trials,
//!   and [`record_appraisal`] writes each **measured** verdict — never an
//!   `Insufficient` one, which would collapse "nobody looked" into "measured
//!   and found wanting" the moment [`latest_verdicts`] read it back.
//! - A run of demotable verdicts
//!   (`context.promotion.skill.demote_after_consecutive_negatives`, default
//!   3) demotes: [`record_demotion`] appends a
//!   `promotion_event` to the append-only `context_records` ledger (its DDL
//!   aborts `UPDATE`/`DELETE` by trigger — a demotion is a new row, never an
//!   edit), and [`demoted_skills`] is the last-write-wins fold the skill
//!   loader excludes by. The file stays on disk; appending a later event is
//!   what un-demotes.
//! - [`queued_candidates`] is read back by the same reflection path: a held
//!   candidate whose ledger verdict has turned to
//!   [`EvalEvidence::MeasuredLift`] is promoted without waiting to be
//!   re-mined.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use colored::Colorize;
use serde::{Deserialize, Serialize};
use stella_context::ContextStore;
use stella_core::context_record::{PromotionAction, PromotionActor, PromotionEventRecord};
use stella_core::skills::SkillCandidate;
use stella_core::skills::appraisal::{
    AppraisalConfig, DemotionDecision, EvalEvidence, SkillAppraisal, SkillTrial, appraise,
    decide_demotion,
};

/// Verdicts, newest last. One line per appraisal run.
pub const APPRAISALS_FILE: &str = "skill_appraisals.jsonl";

/// Candidates the eval gate held, newest last.
pub const QUEUE_FILE: &str = "skill_candidates.jsonl";

/// The selection→outcome join, one line per skill per turn.
pub const TRIALS_FILE: &str = "skill_trials.jsonl";

/// The shared pairing key live trials are recorded under. One key for the
/// whole window because every turn is unique: per-turn pairing would leave
/// nothing paired at all, and under one key the comparison degrades to the
/// unpaired two-sample test it actually is (see
/// `stella_core::skills::appraisal`'s module docs).
pub const LIVE_WINDOW_TASK: &str = "live-window";

/// The `proposal_lineage_id` prefix a skill's demotion events are filed
/// under, so the fold in [`demoted_skills`] can tell them from directive
/// events in the same ledger.
const SKILL_LINEAGE_PREFIX: &str = "skill:";

/// One queued candidate: enough to appraise and then render it, without
/// re-mining.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedCandidate {
    /// The candidate's stable `<slug>-<hash8>` identity — the same string the
    /// skill file would be named for, so a later promotion targets the path
    /// the miner already chose.
    pub name: String,
    pub description: String,
    pub domains: Vec<String>,
    pub occurrences: usize,
    /// What the gate said when this was queued.
    pub evidence: EvalEvidence,
    /// The candidate's rendered body, carried so a later appraisal can
    /// promote it without re-mining. `None` on lines from builds that
    /// predate the field; those wait for the miner's next re-raise instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// The newest verdict per skill, as the creation gate reads it.
///
/// Later lines win: an appraisal is a measurement at a point in time, and a
/// skill that stopped helping must not be held up by the run that said it did.
/// A missing or unreadable ledger yields an empty map, which the gate reads as
/// [`EvalEvidence::Unevaluated`] — the honest answer when nobody has looked.
pub fn latest_verdicts(workspace_root: &Path) -> HashMap<String, EvalEvidence> {
    let mut verdicts = HashMap::new();
    for appraisal in read_jsonl::<SkillAppraisal>(&path(workspace_root, APPRAISALS_FILE)) {
        verdicts.insert(
            appraisal.skill.clone(),
            EvalEvidence::from_verdict(&appraisal.verdict),
        );
    }
    verdicts
}

/// Record an appraisal. The whole report rides along, so a promoted skill's
/// *why* — the task set, the arms, the thresholds, the lift — survives it.
pub fn record_appraisal(workspace_root: &Path, appraisal: &SkillAppraisal) {
    append(workspace_root, APPRAISALS_FILE, appraisal);
}

/// Queue a candidate the gate held, and say so once.
///
/// Deduplicated by identity: the miner re-raises the same candidate on every
/// session until something promotes it, and an unbounded queue of one repeated
/// line is a log, not a queue.
pub fn queue_candidate(
    workspace_root: &Path,
    candidate: &SkillCandidate,
    evidence: EvalEvidence,
    quiet: bool,
) {
    let already = read_jsonl::<QueuedCandidate>(&path(workspace_root, QUEUE_FILE))
        .iter()
        .any(|q| q.name == candidate.name);
    if already {
        return;
    }
    append(
        workspace_root,
        QUEUE_FILE,
        &QueuedCandidate {
            name: candidate.name.clone(),
            description: candidate.description.clone(),
            domains: candidate.domains.clone(),
            occurrences: candidate.occurrences,
            evidence,
            body: Some(candidate.body.clone()),
        },
    );
    if !quiet {
        println!(
            "  {} skill candidate held pending evaluation: {} ({} observations, {})",
            "◇".dimmed(),
            candidate.name.bright_magenta(),
            candidate.occurrences,
            match evidence {
                EvalEvidence::Unevaluated => "never measured",
                EvalEvidence::NoLift => "measured, no lift",
                EvalEvidence::MeasuredLift => "measured",
            }
        );
    }
}

/// Every queued candidate, oldest first, deduplicated by identity.
pub fn queued_candidates(workspace_root: &Path) -> Vec<QueuedCandidate> {
    let mut seen = Vec::new();
    let mut out: Vec<QueuedCandidate> = Vec::new();
    for candidate in read_jsonl::<QueuedCandidate>(&path(workspace_root, QUEUE_FILE)) {
        if !seen.contains(&candidate.name) {
            seen.push(candidate.name.clone());
            out.push(candidate);
        }
    }
    out
}

/// Append this turn's selection→outcome join: one trial per known skill,
/// `selected` set from the selection the turn actually ran with.
///
/// `known` must be every skill the loader offered, not only the selected ones
/// — the unselected turns *are* the control arm, and a ledger of selections
/// alone can only ever measure a skill against itself.
pub fn record_turn(
    workspace_root: &Path,
    known: &[String],
    selected: &[String],
    trial: &SkillTrial,
) {
    for name in known {
        append(
            workspace_root,
            TRIALS_FILE,
            &StoredTrial {
                skill: name.clone(),
                trial: SkillTrial {
                    selected: selected.iter().any(|s| s == name),
                    ..trial.clone()
                },
            },
        );
    }
}

/// One stored trial, keyed by the skill it is evidence about.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredTrial {
    skill: String,
    #[serde(flatten)]
    trial: SkillTrial,
}

/// Appraise every skill the trial ledger has evidence for, and return the
/// demotion decisions for the ones whose origin allows it.
///
/// `origins` maps a skill name to its origin; a skill absent from it is
/// treated as hand-authored, which is the fail-safe direction — an unknown
/// provenance must never be enough to retire something.
pub fn sweep(
    workspace_root: &Path,
    origins: &HashMap<String, stella_core::skills::SkillOrigin>,
    config: &AppraisalConfig,
) -> Vec<(SkillAppraisal, DemotionDecision)> {
    let mut by_skill: HashMap<String, Vec<SkillTrial>> = HashMap::new();
    for stored in read_jsonl::<StoredTrial>(&path(workspace_root, TRIALS_FILE)) {
        by_skill.entry(stored.skill).or_default().push(stored.trial);
    }
    // Sorted so a sweep's output — and the ledger lines it writes — do not
    // depend on a hash seed.
    let mut names: Vec<String> = by_skill.keys().cloned().collect();
    names.sort();

    let mut out = Vec::new();
    for name in names {
        let trials = by_skill.remove(&name).unwrap_or_default();
        let appraisal = appraise(&name, &trials, config);
        let origin = origins
            .get(&name)
            .copied()
            .unwrap_or(stella_core::skills::SkillOrigin::Workspace);
        let decision = decide_demotion(origin, &appraisal);
        out.push((appraisal, decision));
    }
    out
}

/// How many of the newest appraisals of `skill` are demotable verdicts
/// (`Harms` or `Inert`), counted back from the ledger's end until the first
/// verdict that is not.
///
/// The hysteresis input: one confident negative is recorded and visible, but
/// only a run of them retires a skill — the length is
/// `context.promotion.skill.demote_after_consecutive_negatives` (default 3),
/// because a single unlucky window must not undo a promotion that took a
/// task set to earn.
pub fn consecutive_negative_appraisals(workspace_root: &Path, skill: &str) -> usize {
    read_jsonl::<SkillAppraisal>(&path(workspace_root, APPRAISALS_FILE))
        .iter()
        .rev()
        .filter(|a| a.skill == skill)
        .take_while(|a| a.verdict.demotes())
        .count()
}

/// Append the demotion state row for `skill` to the `context_records` ledger.
///
/// An INSERT and only an INSERT: the ledger's own triggers abort `UPDATE` and
/// `DELETE` (`stella-context::store::schema`, `migrate_v8`), so a demotion is
/// a new `promotion_event` row with [`PromotionAction::Retired`] against the
/// `skill:<name>` lineage, and un-demoting is a later row against the same
/// lineage — never an edit. The skill's file is untouched: demotion is
/// removal from *selection*, and restore must survive it.
///
/// [`PromotionActor::System`] because no person was asked, which also caps
/// what this call can ever grant: `PromotionEventRecord::new` refuses a
/// system actor carrying blocking enforcement outright.
pub fn record_demotion(
    store: &ContextStore,
    skill: &str,
    reason: &str,
    occurred_at: &str,
) -> Result<String, String> {
    let event = PromotionEventRecord::new(
        format!("{SKILL_LINEAGE_PREFIX}{skill}"),
        PromotionAction::Retired,
        PromotionActor::System,
        None,
        None,
        reason,
        occurred_at,
    )
    .map_err(|e| e.to_string())?;
    crate::proposals_cmd::record_event(store, &event)?;
    Ok(event.record_id)
}

/// The skills currently demoted out of selection — the last-write-wins fold
/// over the `skill:` lineages in the promotion-event ledger.
///
/// Last write wins for the same reason `proposals_cmd::decisions` folds that
/// way: a skill demoted and later reinstated is reinstated, and both acts
/// remain readable. An unreadable ledger folds to the empty set, which fails
/// toward offering a skill that should be excluded — recoverable on the next
/// sweep — rather than silently withholding every skill a user has.
pub fn demoted_skills(store: &ContextStore) -> HashSet<String> {
    let mut standing: HashMap<String, PromotionAction> = HashMap::new();
    for event in crate::proposals_cmd::promotion_events(store) {
        if let Some(skill) = event.proposal_lineage_id.strip_prefix(SKILL_LINEAGE_PREFIX) {
            standing.insert(skill.to_string(), event.action);
        }
    }
    standing
        .into_iter()
        .filter(|(_, action)| *action == PromotionAction::Retired)
        .map(|(skill, _)| skill)
        .collect()
}

fn path(workspace_root: &Path, file: &str) -> std::path::PathBuf {
    workspace_root.join(".stella").join("private").join(file)
}

/// Read a JSONL file, skipping any line that does not parse.
///
/// Skipping rather than failing: these ledgers outlive the shapes that wrote
/// them, and one unreadable line from an older build must not cost the whole
/// window. A missing file is an empty window, not an error.
fn read_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Vec<T> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Append one JSON line, owner-only, creating the private directory if needed.
/// Best-effort by construction: a ledger write must never fail the turn it
/// describes.
fn append<T: Serialize>(workspace_root: &Path, file: &str, value: &T) {
    let Ok(line) = serde_json::to_string(value) else {
        return;
    };
    let target = path(workspace_root, file);
    let Some(dir) = target.parent() else { return };
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    if builder.create(dir).is_err() {
        return;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let Ok(mut handle) = options.open(&target) else {
        return;
    };
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600));
        let _ = handle
            .write_all(line.as_bytes())
            .and_then(|()| handle.write_all(b"\n"));
    }
    #[cfg(not(unix))]
    {
        use std::io::Write as _;
        let _ = handle
            .write_all(line.as_bytes())
            .and_then(|()| handle.write_all(b"\n"));
    }
}

#[cfg(test)]
mod tests;
