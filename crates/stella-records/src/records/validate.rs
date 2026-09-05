//! The checks every publication runs (`docs/spec/adaptive-context/context-pr.md` §12).
//!
//! Ingest has a safety gate. Publication did not — so a record that passed
//! extraction could be hand-edited into an unparseable guard, a secret, or a
//! silent contradiction of the record next to it, and load cleanly.
//!
//! Every check here answers a question with a concrete, actionable answer, because
//! §12 requires it: *"Overlap: api-integration-coverage conflicts with
//! api-test-exemption — both match routes/internal/\*\* with different enforcement.
//! Resolution: add an explicit exclusion or a supersession relationship."* A check
//! that says "invalid record" has told the reader nothing they can act on.
//!
//! # Conflicts are reported, never silently resolved
//!
//! Two records at equal precedence matching the same paths with different
//! enforcement is a governance failure, not an input to a tiebreak. §12 is explicit:
//! the frame carries a conflict record and **enforcement falls back to the less
//! restrictive behavior** until an owner resolves it. So [`detect_conflicts`] does
//! both — it attaches the conflict to both records *and* suspends the more
//! restrictive one's enforcement. Picking a winner silently would mean a repository
//! could start hard-blocking tool calls because of an ordering accident.
//!
//! # Why overlap detection errs toward reporting
//!
//! `stella_protocol::glob` implements one wildcard: `*`. Deciding whether two such
//! globs can match a common path is decidable but fiddly, and the two failure
//! directions are not symmetric. A missed overlap means a real conflict resolves
//! silently — the thing §12 forbids. A spurious overlap means a warning about two
//! records that never actually collide, which a human reads and dismisses in
//! seconds. So [`globs_overlap`] compares literal prefixes and reports on
//! containment, accepting the cheap false positive to make the expensive false
//! negative impossible.
//!
//! Paths are half a scope. A record can also be scoped by `tasks` and
//! `keywords`, and two records scoped that way collide on every turn that names
//! a trigger they share — a `hard` record and an advisory one over the same
//! task, with no path anywhere between them. So overlap falls back to
//! [`super::select::shared_triggers`] rather than growing a second matcher, and
//! the conflict is judged by the code that decides whether both records fire.

use super::super::ingest::gate::atomicity_validation;
use super::super::ingest::record::{AppliesTo, EnforcementMode, Record};
use super::select::shared_triggers;
use super::{KNOWN_TASKS, LoadedRecord, RecordFinding, Trust};
use stella_learn::redact::redact_secrets;

/// Glob metacharacters that are **literals** in this engine's matcher. A guard
/// written with them expresses an intent the matcher will not honor, so it silently
/// never fires — the worst kind of guard, because it advertises enforcement.
const NON_WILDCARDS: &[char] = &['?', '[', ']', '{', '}', '!'];

/// The longest a single-sentence statement plausibly is. Past this, the field is
/// carrying something that was pasted rather than written.
const MAX_STATEMENT_CHARS: usize = 600;

/// True when `statement` violates the one-sentence shape (ADR 0012, decision 8):
/// embedded newlines or novel-length text read as pasted prompt or tool output
/// rather than a written claim. Exposed so every writer of a record — the CLI's
/// promotion and mining paths included — can refuse the shape this validator
/// would flag, instead of publishing a file that fails `stella context validate`.
pub fn statement_reads_as_pasted(statement: &str) -> bool {
    statement.contains('\n') || statement.chars().count() > MAX_STATEMENT_CHARS
}

/// An equal-precedence overlap between two records with different enforcement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// One record's handle.
    pub left: String,
    /// The other's.
    pub right: String,
    /// What overlaps and what differs, in the shape §12 asks for.
    pub detail: String,
    /// The handle whose enforcement is suspended — the more restrictive of the two.
    /// Its guard does not arm until an owner resolves the conflict.
    pub suspended: String,
}

/// The per-record half of the validation pass — the checks that need no
/// neighbours.
///
/// Separate from the conflict pass because the two apply to different populations:
/// a legacy `.md` rule projected onto a record must NOT be judged against the
/// record surface's schema (its body is prose, often several paragraphs, and the
/// "a statement is one sentence" check would refuse rules that have shipped for
/// months) — but it must still take part in conflict detection, because a markdown
/// guard and a TOML record really can contradict each other.
pub fn check_record(loaded: &mut LoadedRecord) {
    let mut findings = check_one(&loaded.record, loaded.trust);
    loaded.findings.append(&mut findings);
}

/// The cross-record half: every equal-precedence conflict, attached to both sides.
pub fn detect_conflicts(records: &mut [LoadedRecord]) -> Vec<Conflict> {
    let conflicts = find_conflicts(records);
    for conflict in &conflicts {
        for loaded in records.iter_mut() {
            if loaded.handle == conflict.left {
                loaded.findings.push(RecordFinding::Conflict {
                    with: conflict.right.clone(),
                    detail: conflict.detail.clone(),
                });
            } else if loaded.handle == conflict.right {
                loaded.findings.push(RecordFinding::Conflict {
                    with: conflict.left.clone(),
                    detail: conflict.detail.clone(),
                });
            }
        }
    }
    conflicts
}

/// Whether this record's enforcement is suspended by a conflict — the caller must
/// not arm its guard. Derived from the conflicts [`detect_conflicts`] returned.
pub fn is_suspended(handle: &str, conflicts: &[Conflict]) -> bool {
    conflicts
        .iter()
        .any(|conflict| conflict.suspended == handle)
}

/// Everything wrong with one record, independent of its neighbours.
fn check_one(record: &Record, trust: Trust) -> Vec<RecordFinding> {
    let mut findings = Vec::new();
    findings.extend(forbidden_data(record));
    findings.extend(guard_lint(record));
    findings.extend(unknown_tasks(record));
    findings.extend(atomicity(record));
    findings.extend(scoped_without_trigger(record));
    findings.extend(gated_probe_refused(record, trust));
    findings
}

/// A declared gated probe (`command_succeeds`/`http_ok`) the staleness sweep
/// will never run: [`super::sweep::honored_probe`] refuses it for this record's
/// trust tier, origin or truth basis. Without this finding the record loads
/// clean and its claim silently never gets re-checked — the author believes a
/// probe is standing guard, and nothing is.
///
/// The tier is why this is the surface the refusal is said out loud on. Almost
/// every gated probe in a repository is refused for its tier alone, and a record
/// whose fields all look impeccable gives its author no other way to find out.
fn gated_probe_refused(record: &Record, trust: Trust) -> Vec<RecordFinding> {
    let declared = record
        .truth
        .as_ref()
        .and_then(|truth| truth.probe.as_ref())
        .filter(|probe| probe.kind.is_gated());
    match declared {
        Some(probe) if super::sweep::honored_probe(record, trust).is_none() => {
            vec![RecordFinding::GatedProbeRefused(probe.kind)]
        }
        _ => Vec::new(),
    }
}

/// An explicit `tier = "scoped"` with no `applies_to` trigger (#2709). Only
/// the explicit declaration can produce this: a derived scoped tier requires a
/// non-empty `applies_to` by construction ([`Record::tier`]).
fn scoped_without_trigger(record: &Record) -> Vec<RecordFinding> {
    let Some(steering) = record.steering.as_ref() else {
        return Vec::new();
    };
    let scoped = steering.tier == Some(super::super::ingest::record::Tier::Scoped);
    let untriggered = steering
        .applies_to
        .as_ref()
        .is_none_or(super::super::ingest::record::AppliesTo::is_empty);
    if scoped && untriggered {
        vec![RecordFinding::ScopedWithoutTrigger]
    } else {
        Vec::new()
    }
}

/// Secrets, credentials, and raw pasted text must never enter a Git-tracked policy
/// file (§10). The redaction pass that already protects persisted records is reused
/// here as the detector: anything it *would* redact is something that must not be
/// committed.
fn forbidden_data(record: &Record) -> Vec<RecordFinding> {
    let mut findings = Vec::new();
    let mut scan = |field: &'static str, text: Option<&str>| {
        if let Some(text) = text.filter(|text| redact_secrets(text).redacted) {
            let _ = text;
            findings.push(RecordFinding::ForbiddenData { field });
        }
    };
    scan("statement", Some(record.statement.as_str()));
    if let Some(enforcement) = record.enforcement.as_ref() {
        scan(
            "enforcement.on_violation",
            enforcement.on_violation.as_deref(),
        );
        scan("enforcement.rubric", enforcement.rubric.as_deref());
    }
    if let Some(probe) = record.truth.as_ref().and_then(|truth| truth.probe.as_ref()) {
        scan("truth.probe.pattern", probe.pattern.as_deref());
        scan("truth.probe.note", probe.note.as_deref());
    }

    // A statement is one sentence. Embedded newlines or novel-length text mean raw
    // prompt or tool output was pasted into a reviewable field — which is both a
    // privacy boundary (§10) and an atomicity problem.
    if statement_reads_as_pasted(&record.statement) {
        findings.push(RecordFinding::GuardLint(format!(
            "statement is {} characters over {} lines — a record's statement is a single \
             sentence; this reads as pasted prompt or tool text",
            record.statement.chars().count(),
            record.statement.lines().count()
        )));
    }
    findings
}

/// Guard-key lint: names that cannot resolve, globs that cannot match what they
/// say, and scopes with no bound.
fn guard_lint(record: &Record) -> Vec<RecordFinding> {
    let Some(enforcement) = record.enforcement.as_ref() else {
        return Vec::new();
    };
    let mut findings = Vec::new();

    if let Some(tool) = enforcement.guard_tool.as_deref().map(str::trim)
        && !tool.is_empty()
        && (tool.contains(char::is_whitespace) || tool.contains('/') || tool.contains('('))
    {
        findings.push(RecordFinding::GuardLint(format!(
            "guard_tool \"{tool}\" is not a tool name — expected a canonical tool \
             (Bash, Write, Edit, Read, Delete), an exact registry tool name, or *"
        )));
    }

    for (field, glob) in [
        ("guard_deny_path", enforcement.guard_deny_path.as_deref()),
        (
            "guard_deny_command",
            enforcement.guard_deny_command.as_deref(),
        ),
    ] {
        let Some(glob) = glob.map(str::trim).filter(|glob| !glob.is_empty()) else {
            continue;
        };
        if let Some(bad) = glob.chars().find(|ch| NON_WILDCARDS.contains(ch)) {
            findings.push(RecordFinding::GuardLint(format!(
                "{field} \"{glob}\" uses '{bad}', which this matcher treats as a literal \
                 character — * is the only wildcard, so this guard will never fire"
            )));
        }
        if glob.chars().all(|ch| ch == '*') {
            let tool = enforcement.guard_tool.as_deref().unwrap_or("*");
            let detail =
                format!("{field} \"{glob}\" has no literal part, so it matches every value");
            findings.push(if tool == "*" {
                RecordFinding::GuardLint(format!(
                    "{detail} — combined with guard_tool \"*\" this denies every tool call \
                     in the session; bound one of the two"
                ))
            } else {
                RecordFinding::GuardLint(format!("{detail} for tool {tool} — is that intended?"))
            });
        }
    }
    findings
}

/// `applies_to.tasks` outside the ratified vocabulary. See [`KNOWN_TASKS`] on why
/// the vocabulary is closed.
fn unknown_tasks(record: &Record) -> Vec<RecordFinding> {
    record
        .steering
        .as_ref()
        .and_then(|steering| steering.applies_to.as_ref())
        .map(|applies_to| {
            applies_to
                .tasks
                .iter()
                .filter(|task| !KNOWN_TASKS.contains(&task.trim().to_lowercase().as_str()))
                .map(|task| RecordFinding::UnknownTask(task.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// The gate's compound test, re-applied on publish. A record can be hand-edited
/// after extraction, and a compound statement cannot receive the single refutation
/// verdict the whole truth axis is built on.
fn atomicity(record: &Record) -> Vec<RecordFinding> {
    atomicity_validation(&record.statement, false)
        .map(|validation| {
            vec![RecordFinding::GuardLint(format!(
                "{} — {}",
                validation.detail, validation.action
            ))]
        })
        .unwrap_or_default()
}

/// Every equal-precedence overlap with differing enforcement.
fn find_conflicts(records: &[LoadedRecord]) -> Vec<Conflict> {
    let mut conflicts = Vec::new();
    for (i, left) in records.iter().enumerate() {
        for right in records.iter().skip(i + 1) {
            if let Some(conflict) = conflict_between(left, right) {
                conflicts.push(conflict);
            }
        }
    }
    conflicts
}

fn conflict_between(left: &LoadedRecord, right: &LoadedRecord) -> Option<Conflict> {
    if left.record.precedence() != right.record.precedence() {
        // Different precedence is the *designed* way to express which record wins;
        // there is nothing to resolve.
        return None;
    }
    let left_mode = mode_of(&left.record);
    let right_mode = mode_of(&right.record);
    if left_mode == right_mode {
        return None;
    }
    let shared = overlapping_scope(applies_to(&left.record), applies_to(&right.record))?;
    // The less restrictive behavior wins until an owner resolves it, so the record
    // asking for more restriction is the one suspended.
    let (suspended, kept) = if restrictiveness(left_mode) > restrictiveness(right_mode) {
        (&left.handle, &right.handle)
    } else {
        (&right.handle, &left.handle)
    };
    Some(Conflict {
        left: left.handle.clone(),
        right: right.handle.clone(),
        detail: format!(
            "both match {shared} with different enforcement ({} vs {}) at precedence {} \
             — ^{kept} applies and ^{suspended} is suspended; resolve with an explicit \
             exclusion, a different precedence, or a supersession link",
            left_mode.as_str(),
            right_mode.as_str(),
            left.record.precedence(),
        ),
        suspended: suspended.clone(),
    })
}

fn applies_to(record: &Record) -> Option<&AppliesTo> {
    record
        .steering
        .as_ref()
        .and_then(|steering| steering.applies_to.as_ref())
}

fn mode_of(record: &Record) -> EnforcementMode {
    record
        .enforcement
        .as_ref()
        .map_or(EnforcementMode::None, |enforcement| enforcement.mode)
}

/// How much a mode restricts. `hard` blocks a call, `soft` warns, `none` advises.
fn restrictiveness(mode: EnforcementMode) -> u8 {
    match mode {
        EnforcementMode::None => 0,
        EnforcementMode::Soft => 1,
        EnforcementMode::Hard => 2,
    }
}

/// A description of what two records both match, or `None` when they cannot
/// collide.
///
/// An empty `applies_to` matches everything, so a record with no scope overlaps
/// every other record — which is correct, and is why an unscoped `hard` record
/// beside an unscoped advisory one is a conflict worth reporting.
///
/// Two scoped records overlap when their path globs can reach a common path, or
/// when they share a task or keyword trigger. The trigger half only runs when
/// the path half found nothing, so a pair that already collides on a path keeps
/// the more specific report.
fn overlapping_scope(left: Option<&AppliesTo>, right: Option<&AppliesTo>) -> Option<String> {
    let unscoped = |applies: Option<&AppliesTo>| applies.is_none_or(AppliesTo::is_empty);
    if unscoped(left) && unscoped(right) {
        return Some("every path (neither record is scoped)".to_string());
    }
    if unscoped(left) || unscoped(right) {
        let scoped = left.filter(|a| !a.is_empty()).or(right)?;
        let paths = scoped.paths.join(", ");
        return Some(if paths.is_empty() {
            "every path (one record is unscoped)".to_string()
        } else {
            format!("{paths} (the other record is unscoped, so it matches everything)")
        });
    }
    let (left, right) = (left?, right?);
    overlapping_paths(left, right).or_else(|| overlapping_triggers(left, right))
}

/// The path globs two scoped records can both reach.
fn overlapping_paths(left: &AppliesTo, right: &AppliesTo) -> Option<String> {
    let shared: Vec<String> = left
        .paths
        .iter()
        .flat_map(|l| {
            right
                .paths
                .iter()
                .filter(move |r| globs_overlap(l, r))
                .map(move |r| {
                    if l == r {
                        l.clone()
                    } else {
                        format!("{l} ∩ {r}")
                    }
                })
        })
        .collect();
    (!shared.is_empty()).then(|| shared.join(", "))
}

/// The task and keyword triggers two scoped records share — the turns on which
/// both of them fire, when no path connects them.
///
/// Asked in both directions and unioned, because
/// [`super::select::shared_triggers`] is asymmetric: a single-word keyword
/// matches inside a multi-word one and the reverse does not, so one direction
/// alone would miss the pair.
fn overlapping_triggers(left: &AppliesTo, right: &AppliesTo) -> Option<String> {
    let mut shared = shared_triggers(left, right);
    for term in shared_triggers(right, left) {
        if !shared.iter().any(|seen| seen.eq_ignore_ascii_case(&term)) {
            shared.push(term);
        }
    }
    (!shared.is_empty()).then(|| format!("the triggers {}", shared.join(", ")))
}

/// Whether two globs can match a common value.
///
/// Compares the literal prefix before each glob's first `*`: if either prefix is a
/// prefix of the other, the two can reach the same values. Deliberately
/// over-reporting — see the module docs on why a false positive is the cheap
/// direction.
pub fn globs_overlap(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    let prefix = |glob: &str| glob.split('*').next().unwrap_or("").to_string();
    let (left_prefix, right_prefix) = (prefix(left), prefix(right));
    left_prefix.starts_with(&right_prefix) || right_prefix.starts_with(&left_prefix)
}

#[cfg(test)]
mod tests {
    use super::super::super::ingest::record::{Enforcement, Probe, ProbeKind, Truth, TruthBasis};
    use super::super::tests::{hard_guard, loaded_from, record_named, with_scope};
    use super::*;

    fn at(handle: &str, mode: EnforcementMode, paths: &[&str], precedence: u32) -> LoadedRecord {
        scoped_by(
            handle,
            mode,
            AppliesTo {
                paths: paths.iter().map(|path| path.to_string()).collect(),
                ..AppliesTo::default()
            },
            precedence,
        )
    }

    /// A record scoped by tasks and keywords and by no path at all — the shape
    /// a path-only comparison cannot see.
    fn triggered(
        handle: &str,
        mode: EnforcementMode,
        tasks: &[&str],
        keywords: &[&str],
        precedence: u32,
    ) -> LoadedRecord {
        scoped_by(
            handle,
            mode,
            AppliesTo {
                paths: Vec::new(),
                tasks: tasks.iter().map(|task| task.to_string()).collect(),
                keywords: keywords.iter().map(|word| word.to_string()).collect(),
            },
            precedence,
        )
    }

    fn scoped_by(
        handle: &str,
        mode: EnforcementMode,
        applies_to: AppliesTo,
        precedence: u32,
    ) -> LoadedRecord {
        let deny_path = applies_to
            .paths
            .first()
            .cloned()
            .unwrap_or_else(|| "**".to_string());
        let mut record = with_scope(record_named(&format!("ctx.a.b.{handle}")), &[], precedence);
        if let Some(steering) = record.steering.as_mut() {
            steering.applies_to = Some(applies_to);
        }
        record.enforcement = Some(Enforcement {
            mode,
            guard_tool: Some("Edit".to_string()),
            guard_deny_command: None,
            guard_allow_command: None,
            guard_deny_path: Some(deny_path),
            severity: None,
            on_violation: None,
            check: None,
            rubric: None,
        });
        let mut loaded = loaded_from(record);
        loaded.handle = handle.to_string();
        loaded
    }

    // Conflict detection — the §12 acceptance test.

    #[test]
    fn equal_precedence_overlap_with_different_enforcement_is_reported_not_resolved() {
        let mut records = vec![
            at(
                "api-integration-coverage",
                EnforcementMode::Hard,
                &["routes/internal/**"],
                50,
            ),
            at(
                "api-test-exemption",
                EnforcementMode::None,
                &["routes/**"],
                50,
            ),
        ];
        records.iter_mut().for_each(check_record);
        let conflicts = detect_conflicts(&mut records);
        assert_eq!(conflicts.len(), 1, "{conflicts:?}");
        let conflict = &conflicts[0];
        assert!(
            conflict.detail.contains("routes/internal/**"),
            "{}",
            conflict.detail
        );
        assert!(
            conflict.detail.contains("hard vs none"),
            "{}",
            conflict.detail
        );
        assert_eq!(
            conflict.suspended, "api-integration-coverage",
            "enforcement falls back to the LESS restrictive behavior"
        );
        assert!(
            is_suspended("api-integration-coverage", &conflicts)
                && !is_suspended("api-test-exemption", &conflicts)
        );
        // Both sides carry the finding, so either record explains the situation.
        for loaded in &records {
            assert!(
                loaded
                    .findings
                    .iter()
                    .any(|f| matches!(f, RecordFinding::Conflict { .. })),
                "^{} carries no conflict finding",
                loaded.handle
            );
        }
    }

    #[test]
    fn different_precedence_is_a_designed_tiebreak_not_a_conflict() {
        let mut records = vec![
            at(
                "specific",
                EnforcementMode::Hard,
                &["routes/internal/**"],
                80,
            ),
            at("general", EnforcementMode::None, &["routes/**"], 50),
        ];
        records.iter_mut().for_each(check_record);
        assert!(detect_conflicts(&mut records).is_empty());
    }

    #[test]
    fn same_enforcement_over_the_same_paths_is_not_a_conflict() {
        let mut records = vec![
            at("one", EnforcementMode::None, &["src/api/**"], 50),
            at("two", EnforcementMode::None, &["src/api/**"], 50),
        ];
        records.iter_mut().for_each(check_record);
        assert!(detect_conflicts(&mut records).is_empty());
    }

    #[test]
    fn disjoint_paths_do_not_conflict_however_the_enforcement_differs() {
        let mut records = vec![
            at("web", EnforcementMode::Hard, &["apps/web/**"], 50),
            at("api", EnforcementMode::None, &["services/api/**"], 50),
        ];
        records.iter_mut().for_each(check_record);
        assert!(detect_conflicts(&mut records).is_empty());
    }

    #[test]
    fn an_unscoped_record_overlaps_everything() {
        let mut records = vec![
            at("scoped", EnforcementMode::Hard, &["src/api/**"], 50),
            at("unscoped", EnforcementMode::None, &[], 50),
        ];
        records.iter_mut().for_each(check_record);
        let conflicts = detect_conflicts(&mut records);
        assert_eq!(conflicts.len(), 1);
        assert!(
            conflicts[0].detail.contains("unscoped"),
            "{}",
            conflicts[0].detail
        );
    }

    #[test]
    fn two_records_scoped_to_one_task_conflict_with_no_path_between_them() {
        // Neither record names a path, so the glob comparison has nothing to
        // compare and the trigger fallback is the only thing that can see the
        // collision. Without it a hard record and an advisory one over the same
        // task resolve silently.
        let mut records = vec![
            triggered(
                "release-checklist",
                EnforcementMode::Hard,
                &["release"],
                &[],
                50,
            ),
            triggered(
                "release-exemption",
                EnforcementMode::None,
                &["release"],
                &[],
                50,
            ),
        ];
        records.iter_mut().for_each(check_record);
        let conflicts = detect_conflicts(&mut records);
        assert_eq!(conflicts.len(), 1, "{conflicts:?}");
        let conflict = &conflicts[0];
        assert!(conflict.detail.contains("release"), "{}", conflict.detail);
        assert_eq!(
            conflict.suspended, "release-checklist",
            "enforcement falls back to the LESS restrictive behavior"
        );
        for loaded in &records {
            assert!(
                loaded
                    .findings
                    .iter()
                    .any(|f| matches!(f, RecordFinding::Conflict { .. })),
                "^{} carries no conflict finding",
                loaded.handle
            );
        }
    }

    #[test]
    fn disjoint_task_scopes_do_not_conflict_however_the_enforcement_differs() {
        let mut records = vec![
            triggered("deploys", EnforcementMode::Hard, &["deploy"], &[], 50),
            triggered("documentation", EnforcementMode::None, &["docs"], &[], 50),
        ];
        records.iter_mut().for_each(check_record);
        assert!(detect_conflicts(&mut records).is_empty());
    }

    #[test]
    fn a_task_on_one_record_and_the_same_word_as_a_keyword_on_the_other_conflict() {
        // Selection matches tasks and single-word keywords the same way, so a
        // turn that says "deploy" fires both records whichever field carries
        // the word.
        let mut records = vec![
            triggered("deploy-guard", EnforcementMode::Hard, &["deploy"], &[], 50),
            triggered("deploy-notes", EnforcementMode::None, &[], &["deploy"], 50),
        ];
        records.iter_mut().for_each(check_record);
        let conflicts = detect_conflicts(&mut records);
        assert_eq!(conflicts.len(), 1, "{conflicts:?}");
        assert!(
            conflicts[0].detail.contains("deploy"),
            "{}",
            conflicts[0].detail
        );
    }

    #[test]
    fn globs_overlap_reports_containment_in_both_directions() {
        assert!(globs_overlap("routes/**", "routes/internal/**"));
        assert!(globs_overlap("routes/internal/**", "routes/**"));
        assert!(globs_overlap("src/api/**", "src/api/**"));
        assert!(globs_overlap("**", "anything/at/all"));
        assert!(!globs_overlap("apps/web/**", "services/api/**"));
    }

    // Guard lint

    #[test]
    fn a_glob_using_a_non_wildcard_metacharacter_is_blocking() {
        let mut record = record_named("ctx.a.b.c");
        record.enforcement = Some(hard_guard(Some("Edit"), Some("migrations/[0-9]*/**"), None));
        let findings = check_one(&record, Trust::User);
        let lint = findings
            .iter()
            .find_map(|f| match f {
                RecordFinding::GuardLint(detail) => Some(detail),
                _ => None,
            })
            .expect("lint present");
        assert!(lint.contains("'['"), "{lint}");
        assert!(lint.contains("never fire"), "{lint}");
        assert_eq!(
            findings.iter().map(RecordFinding::severity).max(),
            Some(super::super::Severity::Blocking),
            "a guard that cannot fire must not advertise enforcement"
        );
    }

    #[test]
    fn a_deny_everything_guard_on_every_tool_is_refused() {
        let mut record = record_named("ctx.a.b.c");
        record.enforcement = Some(hard_guard(Some("*"), Some("**"), None));
        let findings = check_one(&record, Trust::User);
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, RecordFinding::GuardLint(d) if d.contains("every tool call"))),
            "{findings:?}"
        );
    }

    #[test]
    fn an_unbounded_glob_on_one_tool_is_questioned_not_refused() {
        let mut record = record_named("ctx.a.b.c");
        record.enforcement = Some(hard_guard(Some("Write"), Some("*"), None));
        let findings = check_one(&record, Trust::User);
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, RecordFinding::GuardLint(d) if d.contains("for tool Write"))),
            "{findings:?}"
        );
    }

    #[test]
    fn a_tool_name_that_cannot_resolve_is_linted() {
        let mut record = record_named("ctx.a.b.c");
        record.enforcement = Some(hard_guard(Some("Edit(migrations)"), None, None));
        assert!(
            check_one(&record, Trust::User).iter().any(
                |f| matches!(f, RecordFinding::GuardLint(d) if d.contains("is not a tool name"))
            ),
        );
    }

    #[test]
    fn a_well_formed_guard_lints_clean() {
        let mut record = record_named("ctx.a.b.c");
        record.enforcement = Some(hard_guard(Some("Bash"), None, Some("git push --force*")));
        assert!(
            check_one(&record, Trust::User).is_empty(),
            "{:?}",
            check_one(&record, Trust::User)
        );
    }

    // Forbidden data

    #[test]
    fn a_credential_in_a_statement_is_blocking() {
        let mut record = record_named("ctx.a.b.c");
        record.statement =
            "Authenticate with sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA."
                .to_string();
        let findings = check_one(&record, Trust::User);
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, RecordFinding::ForbiddenData { field: "statement" })),
            "{findings:?}"
        );
        assert_eq!(
            findings.iter().map(RecordFinding::severity).max(),
            Some(super::super::Severity::Blocking)
        );
    }

    #[test]
    fn a_multiline_statement_reads_as_pasted_text() {
        let mut record = record_named("ctx.a.b.c");
        record.statement = "Run the build.\n$ pnpm build\nDone in 4.2s".to_string();
        assert!(
            check_one(&record, Trust::User)
                .iter()
                .any(|f| matches!(f, RecordFinding::GuardLint(d) if d.contains("single \nsentence") || d.contains("single sentence"))),
            "{:?}",
            check_one(&record, Trust::User)
        );
    }

    // Task vocabulary

    #[test]
    fn an_unrecognized_task_warns_without_dropping_the_record() {
        let mut record = with_scope(record_named("ctx.a.b.c"), &["src/**"], 50);
        if let Some(steering) = record.steering.as_mut()
            && let Some(applies_to) = steering.applies_to.as_mut()
        {
            applies_to.tasks = vec!["install".to_string(), "intall".to_string()];
        }
        let findings = check_one(&record, Trust::User);
        assert_eq!(
            findings,
            vec![RecordFinding::UnknownTask("intall".to_string())],
            "the typo must be visible; `install` must not be"
        );
        assert_eq!(
            findings.iter().map(RecordFinding::severity).max(),
            Some(super::super::Severity::Warning)
        );
    }

    // Atomicity on publish

    // Gated probes — a declared probe the sweep will never run must be reported.

    fn probing(kind: ProbeKind, basis: TruthBasis, verified_by: Option<&str>) -> Record {
        let mut record = record_named("ctx.a.b.c");
        record.truth = Some(Truth {
            basis,
            confidence: None,
            verified_by: verified_by.map(str::to_string),
            verified_at: None,
            valid_from: None,
            ttl: None,
            on_expiry: None,
            review_every: None,
            probe: Some(Probe {
                kind,
                path: None,
                pattern: None,
                expect: None,
                note: None,
            }),
        });
        record
    }

    #[test]
    fn a_gated_probe_the_sweep_refuses_is_reported_not_silently_inert() {
        let findings = check_one(
            &probing(ProbeKind::HttpOk, TruthBasis::Measured, None),
            Trust::User,
        );
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, RecordFinding::GatedProbeRefused(ProbeKind::HttpOk))),
            "{findings:?}"
        );
        assert_eq!(
            findings.iter().map(RecordFinding::severity).max(),
            Some(super::super::Severity::Warning),
            "reported, not blocking — the record still steers, it just never re-checks"
        );
    }

    #[test]
    fn a_gated_probe_on_a_decreed_human_verified_record_is_honored_silently() {
        let mut record = probing(ProbeKind::CommandSucceeds, TruthBasis::Decree, Some("mac"));
        // `record_named` stamps an imported origin, which the sweep refuses on
        // its own; the honored case needs the trusted one.
        record.origin = Some(super::super::super::context_record::kind::Origin::User);
        assert!(
            check_one(&record, Trust::User).is_empty(),
            "{:?}",
            check_one(&record, Trust::User)
        );
    }

    #[test]
    fn the_same_record_at_project_trust_is_reported_instead() {
        let mut record = probing(ProbeKind::CommandSucceeds, TruthBasis::Decree, Some("mac"));
        record.origin = Some(super::super::super::context_record::kind::Origin::User);
        let findings = check_one(&record, Trust::Project);
        assert!(
            findings.iter().any(|f| matches!(
                f,
                RecordFinding::GatedProbeRefused(ProbeKind::CommandSucceeds)
            )),
            "a repository record's gated probe is refused, and the author has to be \
             told, or the claim silently stops being re-checked: {findings:?}"
        );
    }

    #[test]
    fn an_ungated_probe_is_never_reported() {
        let record = probing(ProbeKind::FileContains, TruthBasis::Measured, None);
        assert!(
            check_one(&record, Trust::User).is_empty(),
            "{:?}",
            check_one(&record, Trust::User)
        );
    }

    #[test]
    fn a_compound_statement_edited_in_after_ingest_is_caught_on_load() {
        let mut record = record_named("ctx.a.b.c");
        record.statement =
            "Deploys run from main, happen on Tuesdays, and are triggered with make ship."
                .to_string();
        assert!(
            check_one(&record, Trust::User)
                .iter()
                .any(|f| matches!(f, RecordFinding::GuardLint(d) if d.contains("re-extract"))),
        );
    }
}
