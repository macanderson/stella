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
//! # What is wired, and what is not
//!
//! [`latest_verdicts`] and [`queue_candidate`] are live: the creation gate
//! reads the first and the learning loop calls the second on every held
//! candidate.
//!
//! [`record_turn`], [`sweep`], [`record_appraisal`] and [`queued_candidates`]
//! are the retirement half, and they carry `#[allow(dead_code)]` because they
//! have no production caller **yet** — the two seams they need are a turn-end
//! hook that knows both the offered and the selected skill sets, and the
//! retirement sweep that writes the tombstone. The lint is right and the
//! allow says so; what it is not is dead code, because every function here is
//! exercised by `tests.rs` and the shapes are the ones the wiring will call.
//! An allow without that follow-up is how a module rots, so it names it.

use std::collections::HashMap;
use std::path::Path;

use colored::Colorize;
use serde::{Deserialize, Serialize};
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
#[allow(dead_code, reason = "the retirement half; see the module docs")]
pub const TRIALS_FILE: &str = "skill_trials.jsonl";

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
#[allow(dead_code, reason = "the retirement half; see the module docs")]
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
#[allow(dead_code, reason = "the retirement half; see the module docs")]
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
#[allow(dead_code, reason = "the retirement half; see the module docs")]
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
#[allow(dead_code, reason = "the retirement half; see the module docs")]
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
#[allow(dead_code, reason = "the retirement half; see the module docs")]
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
#[path = "appraisals/tests.rs"]
mod tests;
