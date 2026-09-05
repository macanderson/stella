//! The appraisal ledgers — the durable half of the measured promote/retire
//! gate (#1067, #1068).
//!
//! [`stella_learn::skills::appraisal`] decides; this module is
//! where the evidence lives between sessions, because neither direction of that
//! gate can be answered from one turn:
//!
//! - **Promotion** needs a with-skill/without-skill comparison over a task set,
//!   and a candidate that has never been injected has no with-skill arm at all.
//!   So a candidate the miner raises is *queued* here and a later appraisal
//!   promotes it. Queued is not discarded: the mining log keeps every
//!   observation, so a candidate that is never appraised simply waits.
//! - **Retirement** needs a window of turns, joined from *selection* to
//!   *outcome*. Both directions read one population — the turns this skill's
//!   trigger matched — and the join is recorded explicitly, never inferred
//!   from the work a turn produced.
//!
//! Every file here is append-only JSONL under `.stella/private/` (0700, files
//! 0600), for the same reason `trace.rs` is: they carry prompts' worth of
//! context about what a workspace does, and nothing here reaches a store table
//! an egress path reads (AGENTS.md invariant 3).
//!
//! # One trial ledger, three kinds
//!
//! [`TRIALS_FILE`] holds a row per artifact per turn, keyed by
//! [`stella_learn::ledger::ArtifactKind`] and an id. All three kinds have a
//! producer: `SessionMemory::record_skill_trials` writes the skill rows, and
//! `SessionMemory::record_context_trials` (`memory::trials`) writes the
//! memory and rule rows from what each render offered and showed. One key,
//! so [`sweep`] reads one ledger rather than three.
//!
//! [`LEGACY_TRIALS_FILE`] holds the rows a build that only knew skills wrote:
//! a `skill` field and no kind. [`sweep`] reads both files and
//! [`stella_learn::ledger::ArtifactTrial`] reads both field names, so a
//! workspace keeps the window it already paid for. Nothing rewrites that
//! file. An append-only ledger is not edited; it simply stops growing.
//!
//! # How the loop closes
//!
//! Every piece here has a production caller:
//!
//! - [`record_turn`] is called at turn end from the episode seam
//!   (`SessionMemory::record_episode`), with the join the turn-start seam
//!   noted — every skill whose trigger matched the prompt, and the subset
//!   selection actually injected.
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
use stella_learn::ledger::{ArtifactKind, ArtifactTrial};
use stella_learn::skills::SkillCandidate;
use stella_learn::skills::appraisal::{
    AppraisalConfig, DemotionDecision, EvalEvidence, SkillAppraisal, SkillTrial, appraise,
    decide_demotion,
};
use stella_records::context_record::{PromotionAction, PromotionActor, PromotionEventRecord};

/// Verdicts, newest last. One line per appraisal run.
pub const APPRAISALS_FILE: &str = "skill_appraisals.jsonl";

/// Candidates the eval gate held, newest last.
pub const QUEUE_FILE: &str = "skill_candidates.jsonl";

/// The selection→outcome join, one line per artifact per turn.
pub const TRIALS_FILE: &str = "artifact_trials.jsonl";

/// The same join as [`TRIALS_FILE`], written by a build that only knew
/// skills. Read, never written, and never rewritten in place.
pub const LEGACY_TRIALS_FILE: &str = "skill_trials.jsonl";

/// The shared pairing key live trials are recorded under.
///
/// One key for the whole window, and it is not the same decision as *which
/// turns enter the window*. Live turns are unique by construction — nobody
/// sends the same prompt twice — so a per-turn key would put one trial in each
/// stratum, and [`stella_learn::comparison`] counts a task only when every arm
/// ran it. Every stratum would then be unpaired and the window would produce no
/// evidence at all. Under one key nothing is dropped and the comparison
/// degrades to the unpaired two-sample test it actually is.
///
/// What makes the comparison about *this skill* is therefore the population,
/// not the key: [`record_turn`] admits only the turns whose prompt matched the
/// skill's trigger, so both arms under this key are trigger-matched turns and
/// the delta between them is the skill's own.
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

/// Append this turn's offered→outcome join: one trial per artifact of `kind`
/// this turn could have used, `selected` set from what the turn actually ran
/// with.
///
/// `offered` is the population, and it has to be exactly that — every artifact
/// the turn could have injected, not only the ones it did and not everything on
/// disk. Narrower and there is no control arm, so an artifact can only be
/// measured against itself. Wider and the baseline fills with turns the
/// artifact could not have affected, which measures those turns rather than the
/// artifact. For a skill that population is the skills whose trigger matched
/// the prompt; for a memory it is what the recall query answered; for a rule
/// it is the records that applied to the turn's facts.
pub fn record_turn(
    workspace_root: &Path,
    kind: ArtifactKind,
    offered: &[String],
    selected: &[String],
    trial: &SkillTrial,
) {
    for id in offered {
        append(
            workspace_root,
            TRIALS_FILE,
            &ArtifactTrial {
                kind,
                id: id.clone(),
                trial: SkillTrial {
                    selected: selected.iter().any(|s| s == id),
                    ..trial.clone()
                },
            },
        );
    }
}

/// Every trial row the workspace holds, oldest first — the current file after
/// the legacy one, so a fold that keeps the last write keeps the newest.
fn stored_trials(workspace_root: &Path) -> Vec<ArtifactTrial> {
    let mut rows = read_jsonl::<ArtifactTrial>(&path(workspace_root, LEGACY_TRIALS_FILE));
    rows.extend(read_jsonl::<ArtifactTrial>(&path(
        workspace_root,
        TRIALS_FILE,
    )));
    rows
}

/// Appraise every artifact of `kind` the trial ledger has evidence for, and
/// return the demotion decisions for the ones whose origin allows it.
///
/// The kind is a filter rather than a fan-out because each surface retires on
/// its own terms: a skill leaves selection, a memory record is retired, a rule
/// is retracted. One sweep returning all three would hand its caller a list it
/// has to sort back out.
///
/// `origins` maps an id to its origin; an id absent from it is treated as
/// hand-authored, which is the fail-safe direction — an unknown provenance must
/// never be enough to retire something.
pub fn sweep(
    workspace_root: &Path,
    kind: ArtifactKind,
    origins: &HashMap<String, stella_learn::skills::SkillOrigin>,
    config: &AppraisalConfig,
) -> Vec<(SkillAppraisal, DemotionDecision)> {
    let mut by_id: HashMap<String, Vec<SkillTrial>> = HashMap::new();
    for stored in stored_trials(workspace_root) {
        if stored.kind != kind {
            continue;
        }
        by_id.entry(stored.id).or_default().push(stored.trial);
    }
    // Sorted so a sweep's output — and the ledger lines it writes — do not
    // depend on a hash seed.
    let mut ids: Vec<String> = by_id.keys().cloned().collect();
    ids.sort();

    let mut out = Vec::new();
    for id in ids {
        let trials = by_id.remove(&id).unwrap_or_default();
        let appraisal = appraise(&id, &trials, config);
        let origin = origins
            .get(&id)
            .copied()
            .unwrap_or(stella_learn::skills::SkillOrigin::Workspace);
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
