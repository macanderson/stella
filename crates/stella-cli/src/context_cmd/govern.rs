//! `stella context promote` / `stella context govern` — the regulated
//! governance tier (#994, `docs/spec/adaptive-context/context-pr.md` §5.3/§9, ADR 0007).
//!
//! Promotion is the grant that lets a record deny tool calls, and this module
//! is where that grant becomes **accountable**: every enforcement transition
//! is an immutable, hash-chained event in the repository-visible ledger
//! (`.stella/rules/promotions.jsonl`), naming its approver, its reason, and
//! the policy version it created. In regulated mode with proposer/approver
//! separation on, the identity that authored a record cannot approve its own
//! enforcement.

use std::path::Path;

use colored::Colorize;
use stella_core::records::promotion::{
    Governance, GovernanceMode, PromotionEvent, blocking_grants, policy_version,
};
use stella_core::records::trust::Trust;

use crate::context_records::{
    GOVERNANCE_FILE, PROMOTION_LEDGER, append_promotion, load_registry, now_rfc3339,
    read_decisions, read_governance, read_promotions, write_governance,
};

use super::review::actor;

/// `stella context govern [MODE]` — show or change the governance mode.
pub(crate) fn run_govern(
    root: &Path,
    mode: Option<&str>,
    separation: Option<bool>,
    yes: bool,
) -> Result<(), String> {
    let current = read_governance(root)?;
    // Nothing to change: show. `--separation` alone IS something to change, and
    // used to be discarded by an early return here (#5328).
    if mode.is_none() && separation.is_none() {
        return show_governance(root, &current);
    }
    // Merged with the document rather than rebuilt from the flags. Every field
    // the caller did not speak about keeps the value the repository recorded —
    // `govern regulated` with no `--separation` used to silently turn an
    // existing separation off, which is a governance change nobody asked for
    // and nobody was told about (#5328).
    let next = Governance {
        mode: match mode {
            Some(mode) => parse_mode(mode)?,
            None => current.mode,
        },
        separation: separation.unwrap_or(current.separation),
    };
    let target = next.mode;
    if next == current {
        println!(
            "governance already {} (separation {})",
            current.mode.as_str(),
            if current.separation { "on" } else { "off" }
        );
        return Ok(());
    }
    // §5.4: never change the governance mode silently. Interactively this is
    // a question; non-interactively it requires the explicit flag.
    if !yes {
        if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            return Err(format!(
                "changing governance to {} alters who can grant enforcement — re-run with \
                 --yes to confirm (stdin is not a terminal, so I cannot ask)",
                target.as_str()
            ));
        }
        println!(
            "Change governance from {} to {}{}? Personal records stay private; published \
             records and their enforcement are unchanged until promoted under the new mode. [y/N]",
            current.mode.as_str(),
            target.as_str(),
            // What the change actually does to separation, read off the merged
            // document — a prompt that echoed the flag would say nothing about
            // separation whenever the flag was absent, which is exactly when
            // the caller most needs to be told it is unchanged.
            match (current.separation, next.separation) {
                (false, true) => ", turning proposer/approver separation on",
                (true, false) => ", turning proposer/approver separation OFF",
                (true, true) => ", with proposer/approver separation still on",
                (false, false) => "",
            }
        );
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .map_err(|e| format!("cannot read confirmation: {e}"))?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("unchanged.");
            return Ok(());
        }
    }
    write_governance(root, &next)?;
    println!(
        "  {}  governance {} (separation {}) — recorded in {}",
        "set".green(),
        next.mode.as_str(),
        if next.separation { "on" } else { "off" },
        GOVERNANCE_FILE
    );
    Ok(())
}

/// The record's author, or a refusal when separation bars this actor from
/// writing the ledger event they are about to write.
///
/// Shared by `promote` and by `keep`'s supersession path. The check lived only
/// inside `promote`, so a supersession — which the fold reads as revoking any
/// blocking grant the lineage held — went to the ledger with `proposer: None`
/// and no separation check at all: one door checked, the other open (#5328).
///
/// The author comes from the local decision ledger's publish events. Absent
/// means "could not be established", and under separation that fails **closed**
/// — an unattributable record is exactly the case separation exists to refuse.
///
/// `bars` and `instead` name the act in the caller's own words, so the refusal
/// tells a human what to do about *this* command rather than a generic one.
pub(crate) fn separation_cleared(
    root: &Path,
    governance: &Governance,
    lineage: &str,
    handle: &str,
    approver: &str,
    bars: &str,
    instead: &str,
) -> Result<Option<String>, String> {
    let proposer = read_decisions(root)
        .iter()
        .filter(|event| event.lineage_id == lineage && event.decision.publishes())
        .map(|event| event.actor.clone())
        .next_back();
    if governance.mode != GovernanceMode::Regulated || !governance.separation {
        return Ok(proposer);
    }
    match &proposer {
        None => Err(format!(
            "proposer/approver separation is on, and ^{handle}'s author could not be \
             established from the decision ledger — record the authorship first, or have \
             the author's machine publish the record through `stella context keep`"
        )),
        Some(author) if *author == approver => Err(format!(
            "proposer/approver separation is on: {author} authored ^{handle} and cannot \
             {bars} — a different identity must {instead}"
        )),
        Some(_) => Ok(proposer),
    }
}

/// [`separation_cleared`], but only where the lineage actually holds a blocking
/// grant.
///
/// The gate every **lifecycle** event goes through: `keep`'s supersession,
/// `proposals retract`, and `ingest --refresh`'s retirement all append events
/// whose latest-event fold drops any blocking grant the lineage held. That is
/// enforcement being revoked, which is the act separation governs — and all
/// three used to write it with `proposer: None` and no check at all (#5328).
///
/// Scoped rather than universal, because these three are also how an ordinary
/// advisory record is edited or retired. `promote` guards the transition INTO
/// blocking and needs no scoping; these guard the ones that silently undo it,
/// and must not make routine editing in a regulated repository need a second
/// identity.
pub(crate) fn separation_cleared_if_enforced(
    root: &Path,
    governance: &Governance,
    lineage: &str,
    approver: &str,
    bars: &str,
    instead: &str,
) -> Result<Option<String>, String> {
    if !blocking_grants(&read_promotions(root)?).contains_key(lineage) {
        return Ok(None);
    }
    separation_cleared(root, governance, lineage, lineage, approver, bars, instead)
}

fn show_governance(root: &Path, governance: &Governance) -> Result<(), String> {
    let events = read_promotions(root)?;
    println!(
        "governance: {} · separation {} · policy version {}",
        governance.mode.as_str(),
        if governance.separation { "on" } else { "off" },
        policy_version(&events)
    );
    let grants = blocking_grants(&events);
    if grants.is_empty() {
        println!("no ledger enforcement grants ({PROMOTION_LEDGER})");
    } else {
        println!("ledger enforcement grants:");
        for (lineage, event) in &grants {
            println!(
                "  {lineage} — blocking since policy v{} (approved by {}: {})",
                event.seq, event.approver, event.reason
            );
        }
    }
    Ok(())
}

fn parse_mode(text: &str) -> Result<GovernanceMode, String> {
    match text {
        "solo" => Ok(GovernanceMode::Solo),
        "team" => Ok(GovernanceMode::Team),
        "regulated" => Ok(GovernanceMode::Regulated),
        other => Err(format!(
            "unknown governance mode `{other}` — one of solo, team, regulated"
        )),
    }
}

/// `stella context promote <rule> --to <level> --reason <why>`.
pub(crate) fn run_promote(
    root: &Path,
    rule: &str,
    to: &str,
    reason: &str,
    approver: Option<&str>,
) -> Result<(), String> {
    if !matches!(to, "advisory" | "blocking") {
        return Err(format!(
            "`--to {to}` is not an enforcement level — ADR 0007 fixes exactly two: advisory, \
             blocking (the four review-ladder labels are UI over them)"
        ));
    }
    if reason.trim().is_empty() {
        return Err(
            "a promotion needs a --reason — a grant with no reason is evidence \
                    nobody can audit"
                .to_string(),
        );
    }
    let registry = load_registry(root);
    let needle = rule.trim_start_matches('^');
    let entry = registry
        .by_handle(needle)
        .or_else(|| {
            registry.entries.iter().find(|entry| {
                entry.record.record.lineage_id == needle
                    || entry.record.record.record_id.as_deref() == Some(needle)
            })
        })
        .ok_or_else(|| format!("no loaded record matches \"{rule}\""))?;
    let lineage = entry.record.record.lineage_id.clone();
    if entry.record.trust != Trust::Project {
        return Err(format!(
            "^{} is a personal record — the promotion ledger governs repository policy only. \
             For your own records, `stella context keep --enforce` is the grant (writing the \
             file on your own disk is the approval).",
            entry.record.handle
        ));
    }
    // §11: blocking only when a real enforcer exists. A natural-language
    // statement never silently becomes blocking behavior.
    if to == "blocking" && entry.record.record.enforcement.is_none() {
        return Err(format!(
            "^{} declares no guard keys, so nothing could enforce a blocking grant — add \
             `[record.enforcement]` with evaluable guards first (§11: a rule becomes blocking \
             only when a real enforcer exists)",
            entry.record.handle
        ));
    }

    let governance = read_governance(root)?;
    let events = read_promotions(root)?;
    let current = blocking_grants(&events)
        .get(&lineage)
        .map_or("advisory", |_| "blocking");
    if current == to {
        return Err(format!(
            "^{} is already {to} in the ledger",
            entry.record.handle
        ));
    }

    let approver = approver
        .map(str::to_string)
        .or_else(git_identity)
        .unwrap_or_else(actor);
    // The record's author, for separation: the publish actor from the local
    // decision ledger when this machine published it. Absent means "could
    // not be established" — and under separation that fails CLOSED.
    let proposer = separation_cleared(
        root,
        &governance,
        &lineage,
        &entry.record.handle,
        &approver,
        "approve its own enforcement",
        "run this promotion (or pass --approver)",
    )?;

    let version = append_promotion(
        root,
        PromotionEvent {
            seq: 0,
            prev: String::new(),
            at: now_rfc3339(),
            lineage_id: lineage,
            from: current.to_string(),
            to: to.to_string(),
            approver: approver.clone(),
            proposer,
            reason: reason.to_string(),
            mode: governance.mode.as_str().to_string(),
            action: stella_core::records::promotion::LedgerAction::Grant,
        },
    )?;
    println!(
        "  {}  ^{} {} → {} · policy v{version} · approved by {approver}",
        "promoted".green(),
        entry.record.handle,
        current,
        to
    );
    println!(
        "    {}",
        format!(
            "immutable event appended to {PROMOTION_LEDGER} — commit it so the grant \
             travels with the repository"
        )
        .dimmed()
    );
    if to == "blocking" && !entry.is_enforced() {
        println!(
            "    {}",
            "note: the guard is not yet armed in this session — it arms on the next load \
             (and still requires a trusted origin and an evaluable guard)"
                .yellow()
        );
    }
    Ok(())
}

/// `git config user.email`, the identity promotions default to — a real,
/// review-visible identity where one exists, unlike a local username.
fn git_identity() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["config", "user.email"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let email = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!email.is_empty()).then_some(email)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_records::{GOVERNANCE_FILE, read_governance};

    /// Seed a repository whose recorded policy is regulated with separation on.
    fn regulated_with_separation() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(root.path().join(".stella/rules")).expect("rules dir");
        std::fs::write(
            root.path().join(GOVERNANCE_FILE),
            "mode = \"regulated\"\nseparation = true\n",
        )
        .expect("seed policy");
        root
    }

    /// **Witness (#5328, door 3).** Changing the mode does not silently turn an
    /// existing separation off.
    ///
    /// Fails on the base, where `run_govern` rebuilt `Governance` from the
    /// flags — `separation` was a bare `bool`, so a caller who said nothing
    /// about separation was indistinguishable from one who asked for it off,
    /// and `stella context govern regulated` revoked it without a word.
    #[test]
    fn changing_the_mode_leaves_separation_as_the_repository_recorded_it() {
        let root = regulated_with_separation();

        run_govern(root.path(), Some("team"), None, true).expect("mode change");

        let after = read_governance(root.path()).expect("policy still parses");
        assert_eq!(after.mode, GovernanceMode::Team, "the mode moved");
        assert!(
            after.separation,
            "and separation stayed on — nobody asked for it to change"
        );
    }

    /// **Witness (#5328, door 3).** `--separation` with no mode is an update,
    /// not a no-op.
    ///
    /// Fails on the base, where a `None` mode returned early through
    /// `show_governance` and the flag was discarded — the command printed the
    /// current policy and exited 0, so it looked like it had worked.
    #[test]
    fn separation_alone_changes_separation_and_leaves_the_mode() {
        let root = regulated_with_separation();

        run_govern(root.path(), None, Some(false), true).expect("separation change");

        let after = read_governance(root.path()).expect("policy still parses");
        assert!(!after.separation, "separation was turned off as asked");
        assert_eq!(
            after.mode,
            GovernanceMode::Regulated,
            "and the mode is untouched"
        );
    }

    /// Neither argument still shows rather than writes, so the two above cannot
    /// be satisfied by making every invocation a write.
    #[test]
    fn neither_argument_leaves_the_policy_exactly_as_it_was() {
        let root = regulated_with_separation();
        let before = std::fs::read_to_string(root.path().join(GOVERNANCE_FILE)).expect("read");

        run_govern(root.path(), None, None, true).expect("show");

        assert_eq!(
            std::fs::read_to_string(root.path().join(GOVERNANCE_FILE)).expect("read"),
            before,
            "showing the policy must not rewrite it"
        );
    }
}

#[cfg(test)]
mod lifecycle_gate_tests {
    use super::*;
    use crate::context_records::{RULES_DIR, append_promotion};

    fn regulated() -> Governance {
        Governance {
            mode: GovernanceMode::Regulated,
            separation: true,
        }
    }

    fn grant(root: &Path, lineage: &str) {
        std::fs::create_dir_all(root.join(RULES_DIR)).expect("rules dir");
        append_promotion(
            root,
            PromotionEvent {
                seq: 0,
                prev: String::new(),
                at: "2026-08-04T09:00:00Z".into(),
                lineage_id: lineage.into(),
                from: "advisory".into(),
                to: "blocking".into(),
                approver: "lead".into(),
                proposer: None,
                reason: "measured over 30 days".into(),
                mode: "regulated".into(),
                action: stella_core::records::promotion::LedgerAction::Grant,
            },
        )
        .expect("grant");
    }

    /// **Witness (#5328).** An armed record's author cannot clear its own
    /// grant through a lifecycle event.
    ///
    /// Fails on the base, where the separation check lived only in
    /// `run_promote`. The three lifecycle writers — `keep`'s supersession,
    /// `proposals retract`, `ingest --refresh`'s retirement — each appended an
    /// event whose latest-event fold drops the grant, with `proposer: None`
    /// and no check. One door was guarded and three were open.
    #[test]
    fn clearing_a_grant_is_refused_when_the_author_could_not_be_established() {
        let root = tempfile::tempdir().expect("tempdir");
        grant(root.path(), "ctx.acme.web.a");

        let refusal = separation_cleared_if_enforced(
            root.path(),
            &regulated(),
            "ctx.acme.web.a",
            "someone",
            "retire its own enforced record",
            "make this change",
        )
        .expect_err("separation fails closed on an unattributable record");

        assert!(refusal.contains("could not be established"), "{refusal}");
    }

    /// A lineage holding no grant is untouched, so routine editing and
    /// retirement of an advisory record still work in a regulated repository.
    ///
    /// Without this the fix would make every `keep`, `retract` and
    /// `ingest --refresh` in a regulated repo need a second identity — a much
    /// larger behaviour change than the issue asks for.
    #[test]
    fn an_advisory_lineage_passes_the_lifecycle_gate_untouched() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(root.path().join(RULES_DIR)).expect("rules dir");

        assert_eq!(
            separation_cleared_if_enforced(
                root.path(),
                &regulated(),
                "ctx.acme.web.never-promoted",
                "someone",
                "retire its own enforced record",
                "make this change",
            ),
            Ok(None)
        );
    }

    /// Separation off — the shipped default — clears everything, so the gate
    /// cannot break a solo or team repository.
    #[test]
    fn the_gate_is_inert_where_separation_is_not_in_force() {
        let root = tempfile::tempdir().expect("tempdir");
        grant(root.path(), "ctx.acme.web.a");

        assert!(
            separation_cleared_if_enforced(
                root.path(),
                &Governance::default(),
                "ctx.acme.web.a",
                "someone",
                "retire its own enforced record",
                "make this change",
            )
            .is_ok()
        );
    }
}
