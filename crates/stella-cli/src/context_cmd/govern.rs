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
    separation: bool,
    yes: bool,
) -> Result<(), String> {
    let current = read_governance(root);
    let Some(mode) = mode else {
        return show_governance(root, &current);
    };
    let target = parse_mode(mode)?;
    let next = Governance {
        mode: target,
        separation,
    };
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
            if separation {
                ", with proposer/approver separation"
            } else {
                ""
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

    let governance = read_governance(root);
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
    let proposer = read_decisions(root)
        .iter()
        .filter(|event| event.lineage_id == lineage && event.decision.publishes())
        .map(|event| event.actor.clone())
        .next_back();
    if governance.mode == GovernanceMode::Regulated && governance.separation {
        match &proposer {
            None => {
                return Err(format!(
                    "proposer/approver separation is on, and ^{}'s author could not be \
                     established from the decision ledger — record the authorship first, or \
                     have the author's machine publish the record through `stella context keep`",
                    entry.record.handle
                ));
            }
            Some(author) if *author == approver => {
                return Err(format!(
                    "proposer/approver separation is on: {author} authored ^{} and cannot \
                     approve its own enforcement — a different identity must run this \
                     promotion (or pass --approver)",
                    entry.record.handle
                ));
            }
            Some(_) => {}
        }
    }

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
