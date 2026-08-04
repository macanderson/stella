//! One precedence order over both substrates.
//!
//! `docs/design/adaptive-context/context-pr.md` §13 asks for a single merge: *"store-published and
//! Git-published rules merge under one precedence order"* — and ADR 0011 adds a
//! second format to that same order. So the registry takes the [`RuleFile`]s a
//! [`RuleSource`][src] produced, in directory-precedence order, and resolves both
//! `.md` rules and `.toml` records into one answer about what steers this session.
//!
//! [src]: super::super::rules::RuleSource
//!
//! # Why the two formats do not get two pipelines
//!
//! Two loaders would mean two precedence orders, and a rule whose `.md` version
//! lost to a `.toml` record in the prompt but won at the tool boundary is a bug
//! nobody would find by reading either file. The registry is a merge, keyed by the
//! same identifier the prompt cites (`^handle` for a record, the rule id for
//! markdown), so what overrides what is one rule with one answer.
//!
//! # Where the two formats deliberately differ
//!
//! **Markdown guards are grandfathered.** A `.md` rule with `guard-deny-path:`
//! blocks today with no approval ceremony, and it must keep doing so — silently
//! disarming shipped guards to enforce a new gate would be a security regression
//! delivered as a feature. The [`super::bridge`] blocking gate (human approval,
//! trusted origin, real enforcer) applies to **TOML records only**, which is the
//! new surface and can carry the stricter contract from the start.
//!
//! **Markdown rules have no `force`.** They have always been rendered as "binding —
//! follow exactly", so they project to `must` and ride the cached channel. That
//! also gives them a `^handle`, which means legacy rules become citable — the
//! attribution loop closes for them too, at no cost to the author.
//!
//! # Why the merge takes two lists
//!
//! `lineage_id` is the identity precedence acts on, and it is *author-supplied*.
//! Over one flat list in directory order that is a hole: the project directories
//! come last, so a repository could name its record `ctx.<something the user
//! publishes>` and, under last-wins, replace the user's record wholesale — taking
//! its armed guard with it. A cloned checkout could switch off a protection the
//! person at the keyboard had set up for themselves.
//!
//! The markdown path never had that hole: `merge_rule_trust_tiers` in `stella-cli`
//! keeps a user-global hard guard **monotonic** — a project rule with the same id
//! can never remove it, and a project rule carrying a *different* guard is kept
//! alongside so both constraints stay armed. [`load`] takes the two tiers as two
//! arguments so the compiler asks which is which, and the tier fold applies that
//! same invariant to records. What a project record may still do is exactly what it
//! could always do: override a user record that established no enforcement, and add
//! enforcement of its own.

use std::collections::{BTreeMap, BTreeSet};

use super::super::context_record::kind::{Origin, RecordStatus};
use super::super::ingest::record::{
    Enforcement, EnforcementMode, Force, Record, RecordKind, SharingScope, Steering, Verdict,
};
use super::super::rules::{Rule, RuleFile, rule_from_file_checked};
use super::{
    Channel, Conflict, Disposition, GuardDecision, LoadedRecord, RecordFinding, RenderInput,
    RenderedChannel, SweepInput, Trust, assign_handles, declared_hard_guard, load_context_file,
    render_channel,
};

/// One parsed entry: the record, and the legacy `.md` rule it was projected from
/// when it was projected from one.
type Parsed = (LoadedRecord, Option<Rule>);

/// What the registry could not load, and why. Reported rather than swallowed: a
/// policy file that silently stopped loading is indistinguishable from a policy
/// file that was deleted, and only one of those is something somebody chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// The file.
    pub source: String,
    /// What went wrong, in words a person can act on.
    pub detail: String,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.source, self.detail)
    }
}

/// One resolved record: what it says, whether it may steer, and whether it blocks.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The record, with handle assigned and findings attached.
    pub record: LoadedRecord,
    /// The truth sweep's decision.
    pub disposition: Disposition,
    /// The blocking decision, and the refusal when there was one.
    pub guard: GuardDecision,
    /// The legacy `.md` rule this entry was projected from, when it was. Kept so
    /// the tool boundary sees the rule's own text and description verbatim rather
    /// than a reconstruction of them.
    pub markdown: Option<Rule>,
}

impl Entry {
    /// True for a record projected from a legacy `.md` rule.
    pub fn is_legacy_markdown(&self) -> bool {
        self.markdown.is_some()
    }

    /// Whether a Tier-2 guard is actually armed for this entry.
    pub fn is_enforced(&self) -> bool {
        self.guard.guard.is_some()
    }

    /// This entry's `RenderInput`, so the renderer never re-derives enforcement.
    pub fn render_input(&self) -> RenderInput<'_> {
        RenderInput {
            record: &self.record,
            disposition: &self.disposition,
            enforced: self.is_enforced(),
        }
    }
}

/// Everything this session's rule directories resolved to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Registry {
    /// Every record, in merged precedence order.
    pub entries: Vec<Entry>,
    /// The engine's `Rule` view, for [`evaluate_guards`][eg] at the tool boundary.
    ///
    /// [eg]: super::super::rules::evaluate_guards
    pub rules: Vec<Rule>,
    /// Equal-precedence conflicts. Never resolved silently.
    pub conflicts: Vec<Conflict>,
    /// Files, or records, that could not be loaded.
    pub diagnostics: Vec<Diagnostic>,
}

impl Registry {
    /// Render one prompt channel.
    pub fn render(&self, channel: Channel, budget_chars: Option<usize>) -> RenderedChannel {
        let inputs: Vec<RenderInput<'_>> = self.entries.iter().map(Entry::render_input).collect();
        render_channel(&inputs, channel, budget_chars)
    }

    /// Render the volatile channel for **one turn**, keeping only records whose
    /// `applies_to` matches the turn's facts.
    ///
    /// This is where `applies_to` finally earns its keep as a selector (see
    /// [`super::select`]): an unscoped record still renders on every turn, a
    /// scoped one only when the turn names a matching path, task, or keyword.
    /// The cached channel never goes through here — `must`/`should` records are
    /// unconditional by contract and their bytes must not vary per turn.
    ///
    /// A record deselected here appears in neither `rendered` nor `dropped`:
    /// `dropped` is the budget's silent-gap ledger (`NotRendered`), and a record
    /// that does not apply this turn was never a candidate the budget could lose.
    pub fn render_volatile_for_turn(
        &self,
        facts: &super::select::TurnFacts<'_>,
        budget_chars: Option<usize>,
    ) -> RenderedChannel {
        let inputs: Vec<RenderInput<'_>> = self
            .entries
            .iter()
            .filter(|entry| {
                let applies_to = entry
                    .record
                    .record
                    .steering
                    .as_ref()
                    .and_then(|steering| steering.applies_to.as_ref());
                super::select::applies_this_turn(applies_to, facts)
            })
            .map(Entry::render_input)
            .collect();
        render_channel(&inputs, Channel::Volatile, budget_chars)
    }

    /// The entry a `^handle` names, for `stella context explain`.
    pub fn by_handle(&self, handle: &str) -> Option<&Entry> {
        let handle = handle.trim_start_matches('^');
        self.entries
            .iter()
            .find(|entry| entry.record.handle == handle)
    }

    /// Every record that had something wrong with it, worst first.
    pub fn findings(&self) -> Vec<(&Entry, &RecordFinding)> {
        let mut out: Vec<(&Entry, &RecordFinding)> = self
            .entries
            .iter()
            .flat_map(|entry| entry.record.findings.iter().map(move |f| (entry, f)))
            .collect();
        out.sort_by_key(|(_, finding)| std::cmp::Reverse(finding.severity()));
        out
    }
}

/// The session facts the registry needs that `stella-core` cannot discover.
#[derive(Debug, Clone, Default)]
pub struct Facts<'a> {
    /// Probe verdicts from this session's sweep, keyed by `lineage_id`.
    pub verdicts: BTreeMap<String, Verdict>,
    /// When each lineage's probe last ran, keyed by `lineage_id`.
    pub last_checked: BTreeMap<String, String>,
    /// Lineages a human explicitly approved for blocking, **qualified by the tier
    /// the approval was granted at**.
    ///
    /// Keyed by tier and not by lineage alone because a lineage is author-supplied:
    /// an approval the user granted their own `~/.stella/rules` record would
    /// otherwise transfer, silently and automatically, to any repository record that
    /// happened to name the same lineage.
    pub approved_blocking: BTreeSet<(Trust, String)>,
    /// Now, RFC-3339.
    pub now: &'a str,
}

impl Facts<'_> {
    /// Whether a human approved blocking for this record — at this record's own tier.
    fn approves(&self, trust: Trust, lineage: &str) -> bool {
        self.approved_blocking
            .contains(&(trust, lineage.to_string()))
    }
}

/// Resolve this session's rule files — `.md` rules and `.toml` records — into one
/// registry.
///
/// The two tiers are separate arguments, not one list in directory order, because
/// the merge treats them differently: see the module docs on why a repository record
/// must not be able to displace a user record's enforcement. Within each argument,
/// files are in directory-precedence order and later still wins.
pub fn load(user_files: &[RuleFile], project_files: &[RuleFile], facts: &Facts<'_>) -> Registry {
    let mut diagnostics = Vec::new();

    // Pass 1: parse each tier into records, in directory-precedence order. Each
    // carries the legacy markdown rule it came from, when it came from one, and the
    // tier it was read out of — which is the one fact the file cannot claim about
    // itself.
    let user = parse_tier(user_files, Trust::User, &mut diagnostics);
    let project = parse_tier(project_files, Trust::Project, &mut diagnostics);

    // Pass 2: merge by `lineage_id`, BEFORE handles are assigned.
    //
    // The order matters and is easy to get backwards. Handles are deliberately made
    // *unique* — two records that would collide get lengthened — so merging by handle
    // could never merge anything: the two revisions of one rule would be handed
    // distinct handles and both would steer, with the older one silently outliving
    // the override that was supposed to replace it. `lineage_id` is the identity that
    // survives revisions, so it is the identity precedence acts on.
    let merged = merge_tiers(merge_by_lineage(user), merge_by_lineage(project));

    // Handles are only unambiguous across the surviving set, and conflicts compare
    // records against each other, so both happen once, globally. The per-record
    // schema checks run on TOML records only — see `validate::check_record` on why a
    // legacy markdown body must not be judged against the record schema.
    let (mut records, markdown): (Vec<LoadedRecord>, Vec<Option<Rule>>) =
        merged.into_iter().unzip();
    assign_handles(&mut records);
    for (record, markdown) in records.iter_mut().zip(&markdown) {
        if markdown.is_none() {
            super::validate::check_record(record);
        }
    }
    let conflicts = super::validate::detect_conflicts(&mut records);

    // Pass 3: sweep and arm.
    let mut entries: Vec<Entry> = Vec::new();
    for (record, markdown) in records.into_iter().zip(markdown) {
        let mut disposition = super::disposition(&SweepInput {
            record: &record.record,
            verdict: facts.verdicts.get(&record.record.lineage_id).copied(),
            last_checked: facts
                .last_checked
                .get(&record.record.lineage_id)
                .map(String::as_str),
            now: facts.now,
        });
        // A blocking finding — a leaked credential, a guard that cannot fire, a
        // retracted status — means this record must not steer. That is expressed as
        // a disposition rather than by dropping the record, because the two are not
        // the same thing: a dropped record is invisible to `stella context validate`
        // and `explain`, which is precisely where somebody needs to be told that the
        // policy they committed is inert. It stays visible; it stops steering.
        if let Some(reason) = blocking_reason(&record) {
            disposition = Disposition::Block { reason };
        }
        let guard = resolve_guard(
            &record,
            markdown.as_ref(),
            facts,
            &conflicts,
            &mut diagnostics,
        );
        entries.push(Entry {
            record,
            disposition,
            guard,
            markdown,
        });
    }

    let rules = entries.iter().map(projected_rule).collect();

    Registry {
        entries,
        rules,
        conflicts,
        diagnostics,
    }
}

/// Parse one trust tier's files, stamping every record with the tier it was read
/// out of.
fn parse_tier(files: &[RuleFile], trust: Trust, diagnostics: &mut Vec<Diagnostic>) -> Vec<Parsed> {
    let mut parsed: Vec<Parsed> = Vec::new();
    for file in files {
        if file.path.ends_with(".toml") {
            match load_context_file(&file.path, &file.contents) {
                Ok(records) => parsed.extend(records.into_iter().map(|mut record| {
                    record.trust = trust;
                    (record, None)
                })),
                Err(err) => diagnostics.push(Diagnostic {
                    source: err.source,
                    detail: err.detail,
                }),
            }
            continue;
        }
        match rule_from_file_checked(&file.path, &file.contents) {
            Ok(rule) => parsed.push((record_from_markdown(&rule, trust), Some(rule))),
            // A file with no statement is not a rule and never was; saying so for
            // every README in a rules directory would be noise. The nesting and
            // missing-id refusals are real defects and get reported.
            Err(super::super::rules::RuleFileError::EmptyStatement) => {}
            Err(err) => diagnostics.push(Diagnostic {
                source: file.path.clone(),
                detail: err.to_string(),
            }),
        }
    }
    parsed
}

/// Merge by `lineage_id` **within one tier**: a later entry replaces an earlier one
/// with the same lineage, keeping the earlier one's position. The same semantics the
/// markdown loader has always had (JS `Map.set` on an existing key), now over both
/// formats.
fn merge_by_lineage(parsed: Vec<Parsed>) -> Vec<Parsed> {
    let mut order: Vec<String> = Vec::new();
    let mut by_lineage: BTreeMap<String, Parsed> = BTreeMap::new();
    for entry in parsed {
        let lineage = entry.0.record.lineage_id.clone();
        if !by_lineage.contains_key(&lineage) {
            order.push(lineage.clone());
        }
        by_lineage.insert(lineage, entry);
    }
    order
        .into_iter()
        .filter_map(|lineage| by_lineage.remove(&lineage))
        .collect()
}

/// Fold the project tier into the user tier, monotonically.
///
/// The invariant, matching `merge_rule_trust_tiers` on the markdown side: **a
/// project record may add enforcement and may never remove it.** Concretely, for a
/// project record whose lineage a user record already holds:
///
/// | The user record | The project record | Result |
/// | --- | --- | --- |
/// | establishes no guard | anything | overrides, as it always has |
/// | establishes a guard | establishes a *different* one | both kept — two constraints, neither weakened |
/// | establishes a guard | the same one, or none | dropped — it could only weaken or duplicate |
///
/// The last row is the hole this closes. A `mode = "none"` record naming a user
/// lineage used to win the merge outright and take the user's armed guard with it.
///
/// "Establishes a guard" is [`declared_hard_guard`] — deliberately independent of
/// whether the guard is *approved*, so adding a ledger entry can never change which
/// record survives the merge.
fn merge_tiers(mut user: Vec<Parsed>, project: Vec<Parsed>) -> Vec<Parsed> {
    for entry in project {
        let Some(index) = user
            .iter()
            .position(|(loaded, _)| loaded.record.lineage_id == entry.0.record.lineage_id)
        else {
            user.push(entry);
            continue;
        };
        let established = declared_hard_guard(&user[index].0.record);
        let Some(established) = established else {
            user[index] = entry;
            continue;
        };
        // Keeping both is what makes this *monotonic* rather than merely
        // protective: the project's own constraint still applies, under its own
        // handle, subject to its own approval. Dropping it would let a user record
        // silently switch off a repository's guard, which is the same bug pointing
        // the other way.
        if declared_hard_guard(&entry.0.record).is_some_and(|guard| guard != established) {
            user.push(entry);
        }
    }
    user
}

/// Why this record must not steer, when something says it must not.
///
/// A retracted or archived record is not a defect — somebody retired it, and revert
/// relies on that taking effect — so it reads as an ordinary status rather than a
/// finding to fix.
fn blocking_reason(record: &LoadedRecord) -> Option<String> {
    if !matches!(record.record.status, None | Some(RecordStatus::Active)) {
        return Some(format!(
            "its status is {}",
            record
                .record
                .status
                .map(|status| status.as_str())
                .unwrap_or("unset")
        ));
    }
    let blockers: Vec<String> = record
        .findings
        .iter()
        .filter(|finding| finding.severity() == super::Severity::Blocking)
        .map(ToString::to_string)
        .collect();
    (!blockers.is_empty()).then(|| blockers.join("; "))
}

/// The blocking decision for one record.
fn resolve_guard(
    record: &LoadedRecord,
    markdown: Option<&Rule>,
    facts: &Facts<'_>,
    conflicts: &[Conflict],
    diagnostics: &mut Vec<Diagnostic>,
) -> GuardDecision {
    if let Some(rule) = markdown {
        // Grandfathered: a shipped markdown guard keeps working exactly as it did.
        return GuardDecision {
            guard: rule.guard.clone(),
            refusal: None,
        };
    }
    if blocking_reason(record).is_some() {
        // A record that must not steer must certainly not deny a tool call.
        return GuardDecision {
            guard: None,
            refusal: None,
        };
    }
    if super::is_suspended(&record.handle, conflicts) {
        // §12: enforcement falls back to the less restrictive behavior until an
        // owner resolves the conflict.
        return GuardDecision {
            guard: None,
            refusal: None,
        };
    }
    let approved = facts.approves(record.trust, &record.record.lineage_id);
    let decision = super::guard_for(&record.record, record.trust, approved);
    if let Some(refusal) = decision.refusal.as_ref() {
        diagnostics.push(Diagnostic {
            source: record.source.clone(),
            detail: format!("^{} {refusal}", record.handle),
        });
    }
    decision
}

/// The `Rule` an entry contributes to the tool boundary.
fn projected_rule(entry: &Entry) -> Rule {
    // A legacy rule keeps its own text, description, and metadata verbatim; only the
    // id becomes the handle, so a blocked call cites something the model can name.
    if let Some(rule) = entry.markdown.as_ref() {
        return Rule {
            id: entry.record.handle.clone(),
            guard: entry.guard.guard.clone(),
            ..rule.clone()
        };
    }
    let (mut rule, _) = super::rule_from_record(&entry.record, true);
    rule.guard = entry.guard.guard.clone();
    rule
}

/// Project a legacy markdown rule onto a record, so one renderer and one merge
/// serve both formats.
///
/// The force is `must`: markdown rules have always been rendered under "binding —
/// follow exactly", and quietly weakening them on the way to the new renderer would
/// change what the model is told without anyone editing a rule.
fn record_from_markdown(rule: &Rule, trust: Trust) -> LoadedRecord {
    let record = Record {
        lineage_id: format!("ctx.{}", rule.id),
        record_id: None,
        record_hash: None,
        kind: RecordKind::Rule,
        statement: rule.text.clone(),
        tags: Vec::new(),
        // The markdown surface has no origin field. `user` is the honest reading —
        // somebody wrote or promoted the file — and it is inert here anyway: the
        // blocking gate does not run on legacy rules.
        origin: Some(Origin::User),
        sharing_scope: Some(SharingScope::Repository),
        status: Some(RecordStatus::Active),
        provenance: None,
        steering: Some(Steering {
            force: Force::Must,
            precedence: Some(50),
            applies_to: None,
        }),
        enforcement: rule.guard.as_ref().map(|guard| Enforcement {
            mode: EnforcementMode::Hard,
            guard_tool: guard.tool.clone(),
            guard_deny_command: guard.deny_command_glob.clone(),
            guard_deny_path: guard.deny_path_glob.clone(),
            severity: None,
            on_violation: None,
            check: None,
            rubric: None,
        }),
        truth: None,
        links: Vec::new(),
    };
    LoadedRecord {
        record,
        set_id: "markdown".to_string(),
        source: rule.source.clone(),
        trust,
        handle: String::new(),
        findings: Vec::new(),
    }
}

#[cfg(test)]
mod tests;
