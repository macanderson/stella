//! The I/O half of the published-record engine: the sweep, the ledgers, and the
//! session's registry.
//!
//! [`stella_core::records`] decides everything — which records load, whether a
//! claim still holds, what reaches the prompt, what blocks a tool call. None of it
//! touches a filesystem. This module supplies the three facts it cannot know:
//!
//! 1. **The files.** Walked by [`crate::rules::FsRuleSource`].
//! 2. **Probe verdicts.** Run here, against the real tree, and cached.
//! 3. **Human approvals.** Read from the decision ledger.
//!
//! # Why the sweep is due-driven and cached
//!
//! Re-probing every record on every session start would put a filesystem scan in
//! front of every prompt, and the answer almost never changes between two sessions
//! a minute apart. So a probe runs when the record's own `review_every`/`ttl` says
//! it is due, and the verdict is cached in `.stella/private/context-sweep.json`
//! with the time it was taken. A record whose cadence has not elapsed reuses its
//! last verdict; one that has never been checked is checked now.
//!
//! The cache is **private, not Git-tracked**. A verdict is a local measurement of
//! a local tree — committing it would mean one developer's filesystem deciding
//! what another developer's agent is told.
//!
//! # Why a missing ledger is never an error
//!
//! Both ledgers are best-effort, like every other persistence path in the CLI: an
//! unreadable sweep cache means probes run again, and an unreadable decision log
//! means no blocking approvals this session. Neither can stop a session from
//! starting. The one asymmetry is deliberate — an unreadable *decision* log fails
//! **closed** (no approvals, so no TOML record blocks), because the failure mode of
//! guessing wrong in the other direction is a tool call denied for a reason nobody
//! authorized.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use stella_core::ingest::record::{ProbeKind, Verdict};
use stella_core::records::{
    self, DecisionEvent, Facts, LoadedRecord, Registry, Trust, decision, load_context_file,
};
use stella_core::rules::{LoadRulesOptions, RuleFile, RuleSource, rule_search_dirs};

/// Where the private sweep cache lives, relative to the workspace root.
const SWEEP_CACHE: &str = ".stella/private/context-sweep.json";

/// Where review decisions are appended, relative to the workspace root.
const DECISION_LEDGER: &str = ".stella/private/context-decisions.jsonl";

/// Where ingest writes proposals, relative to the workspace root.
pub(crate) const PROPOSALS_DIR: &str = ".stella/proposals";

/// Where kept repository-scoped records are published, relative to the root.
pub(crate) const RULES_DIR: &str = ".stella/rules";

/// The repository-visible, hash-chained promotion ledger (#994, ADR 0007).
/// Under `RULES_DIR` — NOT `.stella/private/` — because a team's enforcement
/// grants must travel with the repository and be reviewed through the same
/// pull requests as the rules they govern.
pub(crate) const PROMOTION_LEDGER: &str = ".stella/rules/promotions.jsonl";

/// The repository's governance settings, beside the ledger for the same
/// reason: which tier applies is itself reviewed policy.
pub(crate) const GOVERNANCE_FILE: &str = ".stella/rules/governance.toml";

/// Read the governance settings; absent file = solo defaults (§5.1).
pub(crate) fn read_governance(root: &Path) -> stella_core::records::promotion::Governance {
    let path = root.join(GOVERNANCE_FILE);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

/// Write the governance settings. Callers ask the user first (§5.4) — this
/// only persists an already-confirmed change.
pub(crate) fn write_governance(
    root: &Path,
    governance: &stella_core::records::promotion::Governance,
) -> Result<(), String> {
    let path = root.join(GOVERNANCE_FILE);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let text = toml::to_string_pretty(governance)
        .map_err(|e| format!("cannot serialize governance settings: {e}"))?;
    std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// The promotion ledger's raw text (empty when absent) and its path.
pub(crate) fn promotion_ledger_text(root: &Path) -> (std::path::PathBuf, String) {
    let path = root.join(PROMOTION_LEDGER);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    (path, text)
}

/// Verify and parse the promotion ledger. A chain violation is an error the
/// caller must surface — never silently treated as an empty ledger, which
/// would read tampering as "no grants" and drop enforcement without a trace.
pub(crate) fn read_promotions(
    root: &Path,
) -> Result<Vec<stella_core::records::promotion::PromotionEvent>, String> {
    let (path, text) = promotion_ledger_text(root);
    stella_core::records::promotion::parse_and_verify(&text).map_err(|violation| {
        format!(
            "{} line {}: {}",
            path.display(),
            violation.line,
            violation.reason
        )
    })
}

/// Append one promotion event, stamping seq + prev from the verified tail.
pub(crate) fn append_promotion(
    root: &Path,
    event: stella_core::records::promotion::PromotionEvent,
) -> Result<u64, String> {
    let (path, text) = promotion_ledger_text(root);
    let line = stella_core::records::promotion::next_line(&text, event).map_err(|violation| {
        format!(
            "{} line {}: {}",
            path.display(),
            violation.line,
            violation.reason
        )
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let mut appended = text;
    appended.push_str(&line);
    appended.push('\n');
    std::fs::write(&path, &appended)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let events = stella_core::records::promotion::parse_and_verify(&appended)
        .map_err(|violation| format!("ledger invalid after append: {}", violation.reason))?;
    Ok(stella_core::records::promotion::policy_version(&events))
}

/// Now, as every ledger here stores it.
pub(crate) fn now_rfc3339() -> String {
    stella_context::format_rfc3339(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0) as i64,
    )
}

/// One cached probe result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SweepEntry {
    /// The verdict, in the surface spelling.
    pub verdict: String,
    /// When the probe ran, RFC-3339.
    pub checked_at: String,
    /// The probe's own explanation, so `stella context validate` can say *why*
    /// rather than only *what*.
    pub detail: String,
    /// Which probe produced it.
    pub probe_kind: String,
}

/// The private cache of probe results, keyed by `lineage_id`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct SweepCache {
    /// Verdicts by lineage.
    #[serde(default)]
    pub checked: BTreeMap<String, SweepEntry>,
}

impl SweepCache {
    /// Read the cache. A missing or unreadable cache is an empty one — probes will
    /// simply run again.
    pub fn read(root: &Path) -> Self {
        std::fs::read_to_string(root.join(SWEEP_CACHE))
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    /// Write the cache back, best-effort.
    pub fn write(&self, root: &Path) {
        let path = root.join(SWEEP_CACHE);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(body) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&path, body);
        }
    }

    /// The verdict for a lineage, when one was cached and parses.
    fn verdict_of(&self, lineage: &str) -> Option<Verdict> {
        match self.checked.get(lineage)?.verdict.as_str() {
            "supported" => Some(Verdict::Supported),
            "refuted" => Some(Verdict::Refuted),
            "unfalsifiable" => Some(Verdict::Unfalsifiable),
            _ => None,
        }
    }
}

/// Every review decision, oldest first. Unparseable lines are skipped rather than
/// failing the whole log: one corrupt append must not erase the review history
/// around it.
pub(crate) fn read_decisions(root: &Path) -> Vec<DecisionEvent> {
    std::fs::read_to_string(root.join(DECISION_LEDGER))
        .map(|raw| {
            raw.lines()
                .filter(|line| !line.trim().is_empty())
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Append one decision. Append-only by construction — the file is opened for
/// append, never truncated, so a decision cannot be edited away by the tool that
/// recorded it.
pub(crate) fn append_decision(root: &Path, event: &DecisionEvent) -> Result<(), String> {
    use std::io::Write;
    let path = root.join(DECISION_LEDGER);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let line = serde_json::to_string(event).map_err(|e| format!("cannot encode decision: {e}"))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    writeln!(file, "{line}").map_err(|e| format!("cannot append to {}: {e}", path.display()))
}

/// The rule files this workspace's directories contain, in precedence order.
///
/// **Two independent trust axes, and collapsing them is a security bug.**
/// `include_user` controls whether `~/.stella/rules/` is read; `include_project`
/// controls whether the *repository's* `.claude/rules/` and `.stella/rules/` are.
/// An untrusted checkout must contribute nothing to the privileged system prompt
/// (`AuthorityPolicy::project_prompts_allowed`), while the user's own rules are
/// trusted by definition — they are the user's.
///
/// [`rule_search_dirs`] returns all three in precedence order and
/// [`RuleSource`] reads every directory it is handed, so the filtering has to
/// happen here, on the slice. Passing one flag for both axes reads plausibly and
/// silently loads an untrusted repository's rules into the prompt.
pub(crate) fn rule_files(root: &Path, include_user: bool, include_project: bool) -> TieredFiles {
    let user_rules_dir = if include_user {
        crate::settings::user_home_dir()
            .map(|home| home.join(".stella").join("rules").display().to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };
    let dirs = rule_search_dirs(&LoadRulesOptions {
        cwd: root.display().to_string(),
        user_rules_dir,
    });
    // `rule_search_dirs` puts the user directory first, then the two project ones.
    let (user, project) = dirs.split_at(1);
    TieredFiles {
        user: crate::rules::FsRuleSource.read_rule_files(user),
        project: if include_project {
            crate::rules::FsRuleSource.read_rule_files(project)
        } else {
            Vec::new()
        },
    }
}

/// This workspace's rule files, kept in their trust tiers.
///
/// The split is preserved all the way to [`records::registry::load`] rather than
/// flattened here, because the merge needs it: a project record may add enforcement
/// and may never remove a user record's. Flattening at the boundary and re-deriving
/// the tier from path prefixes downstream would put a security decision on string
/// matching.
#[derive(Debug, Clone, Default)]
pub(crate) struct TieredFiles {
    /// `~/.stella/rules` — the user's own.
    pub user: Vec<RuleFile>,
    /// `<repo>/.claude/rules` and `<repo>/.stella/rules`, in precedence order.
    pub project: Vec<RuleFile>,
}

impl TieredFiles {
    /// Every file, tier forgotten — for the passes that only ask what is on disk,
    /// like running due probes.
    pub(crate) fn all(&self) -> Vec<RuleFile> {
        let mut all = self.user.clone();
        all.extend(self.project.iter().cloned());
        all
    }
}

/// The registry a **session** loads: the authority policy decides whether an
/// untrusted checkout's records may steer.
///
/// Named separately from [`load_registry`] rather than taking a bool, because the
/// bool version of this distinction is what let an untrusted project record reach
/// the privileged system prompt — `load_registry(root, flag)` reads plausibly with
/// either value, and the compiler has nothing to say about which one is right.
pub(crate) fn load_session_registry(
    root: &Path,
    authority: &crate::settings::AuthorityPolicy,
) -> Registry {
    load_registry_with(root, authority.project_prompts_allowed)
}

/// The registry a **CLI command** loads: everything on disk.
///
/// `stella context list`/`validate`/`explain` answer "what is in this workspace",
/// which is a question about the filesystem rather than about what this session is
/// permitted to be steered by — the same distinction
/// [`crate::rules::load_workspace_rules_unfiltered`] draws for the miner. Trust
/// governs whether a record steers the model; it has nothing to say about whether a
/// file exists.
pub(crate) fn load_registry(root: &Path) -> Registry {
    load_registry_with(root, true)
}

/// Load this workspace's record registry, running any probe that is due.
///
/// The parse happens twice — once here to find due probes, once inside
/// [`records::registry::load`] with the verdicts in hand. That is deliberate:
/// making the pure loader call back into a probe runner would put I/O behind
/// `stella-core`'s only I/O-free boundary, and parsing a handful of small TOML
/// files twice costs less than the seam it would cost to avoid.
fn load_registry_with(root: &Path, include_project: bool) -> Registry {
    let files = rule_files(root, true, include_project);
    let now = now_rfc3339();
    let mut cache = SweepCache::read(root);
    let ran = run_due_probes(root, &files.all(), &mut cache, &now);
    if ran > 0 {
        cache.write(root);
    }
    registry_from(root, &files, &cache, &now)
}

/// Build the registry from already-collected files and an already-swept cache.
fn registry_from(root: &Path, files: &TieredFiles, cache: &SweepCache, now: &str) -> Registry {
    let states = decision::fold(&read_decisions(root));
    let mut verdicts = BTreeMap::new();
    let mut last_checked = BTreeMap::new();
    for (lineage, entry) in &cache.checked {
        if let Some(verdict) = cache.verdict_of(lineage) {
            verdicts.insert(lineage.clone(), verdict);
        }
        last_checked.insert(lineage.clone(), entry.checked_at.clone());
    }
    let mut approved_blocking: BTreeSet<(Trust, String)> = states
        .values()
        .filter(|state| state.approved_blocking && state.decision.publishes())
        .filter_map(|state| state.published_to.clone())
        .filter_map(|path| {
            Some((
                trust_of_published_path(&path),
                lineage_from_published_path(&path)?,
            ))
        })
        .collect();
    // The repository-visible promotion ledger (#994) arms project-tier
    // records exactly like a private approval — but accountably: every grant
    // in it names its approver and reason and is hash-chain verified. A
    // ledger that fails verification grants NOTHING (fail closed for
    // enforcement) — the validator, not this load path, is where the
    // violation is surfaced loudly.
    if let Ok(events) = read_promotions(root) {
        for lineage in stella_core::records::promotion::blocking_grants(&events).into_keys() {
            approved_blocking.insert((Trust::Project, lineage));
        }
    }

    records::registry::load(
        &files.user,
        &files.project,
        &Facts {
            verdicts,
            last_checked,
            approved_blocking,
            now,
        },
    )
}

/// The lineage a published record file holds — `.stella/rules/<lineage>.toml`.
///
/// Publication names the file after the lineage precisely so this mapping exists:
/// an approval recorded against a path can be read back as an approval against a
/// lineage without consulting the file.
fn lineage_from_published_path(path: &str) -> Option<String> {
    let base = path.rsplit('/').next()?;
    Some(base.strip_suffix(".toml")?.to_string())
}

/// Which tier an approval was granted at, read from where the record was published.
///
/// An approval is a statement about one record, and a lineage does not identify one
/// record — it is author-supplied, so a repository can name the same lineage the
/// user's own record uses. Without this, approving your own `~/.stella/rules` guard
/// would hand the same approval to any repository record that guessed the name.
///
/// Compared against the real home directory rather than by looking for
/// `.stella/rules` in the path, which **both** tiers end with.
fn trust_of_published_path(path: &str) -> Trust {
    let Some(user_rules) = crate::settings::user_home_dir() else {
        // No home directory to compare against: nothing can be established as the
        // user's, so nothing is.
        return Trust::Project;
    };
    if Path::new(path).starts_with(user_rules.join(".stella").join("rules")) {
        Trust::User
    } else {
        Trust::Project
    }
}

/// Run every probe that is due, writing results into `cache`. Returns how many ran.
fn run_due_probes(root: &Path, files: &[RuleFile], cache: &mut SweepCache, now: &str) -> usize {
    let mut ran = 0;
    for record in parse_records(files) {
        let lineage = record.record.lineage_id.clone();
        let last = cache
            .checked
            .get(&lineage)
            .map(|entry| entry.checked_at.clone());
        let input = records::SweepInput {
            record: &record.record,
            verdict: None,
            last_checked: last.as_deref(),
            now,
        };
        // Never checked, with a probe worth running, also counts as due — otherwise a
        // record with no cadence would never be measured at all and would present as
        // supported forever on the strength of never having been asked.
        let never_checked = last.is_none() && probe_is_runnable(&record);
        if !(never_checked || records::probe_is_due(&input)) {
            continue;
        }
        let Some(probe) = records::honored_probe(&record.record) else {
            continue;
        };
        let refutation = crate::ingest_cmd::probe::evaluate(root, probe, now);
        cache.checked.insert(
            lineage,
            SweepEntry {
                verdict: refutation.verdict.as_str().to_string(),
                checked_at: refutation.checked_at.clone(),
                detail: refutation.detail.clone(),
                probe_kind: refutation.probe_kind.as_str().to_string(),
            },
        );
        ran += 1;
    }
    ran
}

/// Whether this record has a probe a machine can actually run. `manual` is a
/// human's cadence and `none` is an admission that nothing can check the claim;
/// firing either would produce a verdict nobody took.
fn probe_is_runnable(record: &LoadedRecord) -> bool {
    records::honored_probe(&record.record)
        .is_some_and(|probe| !matches!(probe.kind, ProbeKind::Manual | ProbeKind::None))
}

/// Every `.toml` record in `files`, for the probe pass. Parse failures are ignored
/// here — [`records::registry::load`] reports them properly, and reporting the
/// same broken file twice reads like two problems.
pub(crate) fn parse_records(files: &[RuleFile]) -> Vec<LoadedRecord> {
    files
        .iter()
        .filter(|file| file.path.ends_with(".toml"))
        .filter_map(|file| load_context_file(&file.path, &file.contents).ok())
        .flatten()
        .collect()
}

/// Force every probe to run, ignoring cadence — what `stella context validate`
/// does. Returns the fresh cache without writing it, so a validation run can
/// report on demand without moving the scheduled sweep's clock forward.
pub(crate) fn probe_everything(root: &Path, files: &[RuleFile], now: &str) -> SweepCache {
    let mut cache = SweepCache::default();
    for record in parse_records(files) {
        let Some(probe) = records::honored_probe(&record.record) else {
            continue;
        };
        let refutation = crate::ingest_cmd::probe::evaluate(root, probe, now);
        cache.checked.insert(
            record.record.lineage_id.clone(),
            SweepEntry {
                verdict: refutation.verdict.as_str().to_string(),
                checked_at: refutation.checked_at.clone(),
                detail: refutation.detail.clone(),
                probe_kind: refutation.probe_kind.as_str().to_string(),
            },
        );
    }
    cache
}

/// Build a registry from an explicit cache — the seam `stella context validate`
/// uses so the report reflects the probes it just ran rather than the scheduled
/// sweep's older verdicts.
pub(crate) fn registry_with_cache(
    root: &Path,
    files: &TieredFiles,
    cache: &SweepCache,
    now: &str,
) -> Registry {
    registry_from(root, files, cache, now)
}

/// Where a record of this sharing scope is published, as a real path.
pub(crate) fn publication_path(
    root: &Path,
    scope: stella_core::ingest::record::SharingScope,
    lineage: &str,
) -> Option<PathBuf> {
    let dir = match records::publication_dir(scope) {
        records::PublicationDir::Repository => root.join(RULES_DIR),
        records::PublicationDir::User => crate::settings::user_home_dir()?
            .join(".stella")
            .join("rules"),
    };
    Some(dir.join(format!("{lineage}.toml")))
}

#[cfg(test)]
mod tests;
