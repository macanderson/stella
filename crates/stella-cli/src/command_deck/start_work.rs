// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The ISSUES tab's start-work driver — SPEC 8.2, rendering `10-start-work`.
//!
//! Two requests, and the whole design is in the gap between them.
//! `draft_plan` **reads**: the issue's text through the tracker port, the
//! files the code graph couples to the paths that text names, the workspace
//! RULEs whose guards those files trip, and the rates this workspace's own
//! recorded calls give for the session's model. `approve` is the only thing
//! that writes, and it runs when — and only when — a human has pressed `a`
//! over the draft the read produced.
//!
//! # The draft reaches no model
//!
//! Deriving the plan from the issue's own checklist is not a weaker version of
//! asking a model to write one: it is what lets the overlay's footer say
//! `nothing runs before approval` and mean it. A drafting call would spend the
//! user's money on a plan they have not agreed to look at, and would make the
//! card's contents unreproducible from the issue a reviewer can read. The
//! model gets the plan once it is approved, as the turn.
//!
//! # The claim: why the deck takes the self-driving loop's lease
//!
//! The stub this replaces refused with "start work is the self-driving loop's
//! claim, not a tracker status", and it was right about the hazard: two
//! dispatchers working one issue is exactly what `dispatch_claims` exists to
//! prevent (#4300). So the deck becomes the third dispatcher on that table
//! rather than inventing a second mechanism —
//! [`crate::self_driving_cmd::claim::acquire_as`], the same fenced lease on
//! the same `issue:<n>` key, taken under a `deck:<pid>` owner so a peer's
//! audit line can say who holds it.
//!
//! The lease is taken **before** the branch and dropped after it, which is the
//! layering the loop already uses: the lease closes the race (two claimants in
//! the same instant, decided by one conditional write), and the branch carries
//! the duration. `contention::for_issue` weighs a remote branch, an open pull
//! request, a worktree and the ledger together, so the branch is what says the
//! issue is taken and the lease never has to be held for the length of a
//! human's afternoon.
//!
//! # Why the branch is pushed before any work exists
//!
//! A branch only carries the duration for a peer that can see it, and a local
//! branch is seen by one clone. Of the four signals `contention::gather`
//! reads, two cross a machine boundary: a remote branch and an open pull
//! request. So `approve` pushes the branch it opens, while the lease is still
//! held — `--set-upstream`, the same `git push` the loop's own
//! `deliver::open` runs.
//!
//! The two alternatives cannot reach a second clone at all. Holding
//! the lease for the whole deck session does not, because `dispatch_claims`
//! lives in this workspace's own `.stella/private/fleet.db`. Teaching the
//! probe to read local branches does not either. Both close the same-clone
//! case, which already defers on the worktree and the ledger.
//!
//! The price is an empty `stella/issue-<n>-<slug>` branch on the remote for
//! every start-work a human abandons, removed with `git push -d origin`. What
//! it buys is that two people on two machines cannot both start the same
//! issue.
//!
//! A push that fails — no `origin`, no network, no write access — does not
//! fail the approval: the branch and the plan exist, and refusing to start
//! work because a network write failed is the worse failure, on the same
//! fail-open rule every probe over this table follows. The summary line then
//! says the branch is local only and names git's reason, because a card that
//! stayed quiet would claim a protection this workspace does not have.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use stella_core::plan_graph::PlanGraph;
use stella_learn::rules::{ProposedAction, Rule, evaluate_guards};
use stella_protocol::issue::{Issue, IssueKey, IssueProvider};
use stella_protocol::plan_graph::TaskNode;
use stella_protocol::tokens::estimate_tokens_for_bytes;
use stella_store::ModelRates;
use stella_tui::{DraftContract, DraftRule, DraftSources, DraftTask, StartWorkDraft};

/// Most tasks a draft proposes.
///
/// A cap rather than a target: the plan comes out of the issue's own
/// checklist, and an issue with forty boxes is a plan nobody can approve by
/// reading. The overflow is not hidden — the card names what it kept and the
/// human edits from there.
const MAX_TASKS: usize = 8;

/// Most coupled files the sources line lists.
const MAX_COUPLED: usize = 6;

/// Headings whose list is the plan, lowercased, in the order they are tried.
const PLAN_HEADINGS: &[&str] = &[
    "definition of done",
    "done means",
    "acceptance",
    "tasks",
    "the work",
];

/// Verbs that make a task read-only — it changes no file, so no diff can
/// settle it and it declares `read only · no contract` rather than a check
/// nothing can run.
const READING_VERBS: &[&str] = &[
    "read",
    "review",
    "audit",
    "inspect",
    "investigate",
    "survey",
    "locate",
    "identify",
    "understand",
    "measure",
    "find",
    "trace",
];

/// The check mechanisms a draft may propose, and whether each reaches a model.
///
/// The `det` tag is read out of this table rather than decided per task, so a
/// mechanism cannot be deterministic on one row and not on another. Every
/// mechanism here is a command, so every one of them is deterministic today —
/// the column exists because a mechanism that *did* reach a model would have
/// to say so on the card, and SPEC §1 is explicit that the tag is that
/// boolean and never a ratio.
const MECHANISMS: &[(&str, bool)] = &[
    ("unit", true),
    ("gate", true),
    ("build", true),
    ("graph", true),
];

/// Draft a plan for one issue. Reads only — see the module docs.
///
/// `display_key` is the browse row's spelling (`#5044`); the tracker takes the
/// bare one, exactly as [`super::issues::issues_act`] does.
pub(super) fn draft_plan<P: IssueProvider + ?Sized>(
    provider: &P,
    root: &Path,
    model_id: &str,
    display_key: &str,
) -> Result<StartWorkDraft, String> {
    let key = IssueKey::from(display_key.trim_start_matches('#'));
    let issue = super::issues::block_on(provider.get(&key))?.map_err(|error| error.to_string())?;
    let coupled = coupled_files(root, &issue);
    let rules = applied_rules(root, &coupled);
    let tasks = plan_tasks(&issue);
    let rates = stella_store::Store::open(root)
        .ok()
        .and_then(|store| store.model_rates(model_id).ok().flatten());
    let estimate = estimate(rates, issue_bytes(&issue, root, &coupled), tasks.len());
    Ok(StartWorkDraft {
        issue_key: format!("#{key}"),
        issue_title: issue.title,
        sources: DraftSources {
            coupled_files: coupled,
            rules,
        },
        tasks,
        gates: gate_count(root),
        estimate,
    })
}

/// Approve a drafted plan: take the issue's claim, open the branch under it,
/// publish that branch, and author the plan's first revision.
///
/// Ordered so that nothing is left behind by a failure. The claim comes first
/// because it is the only step a peer can lose a race on; the branch second,
/// so a lost race leaves no branch; the push third, so nothing reaches the
/// remote that a lost race would have to clean up; the plan last, because it
/// is derived from the tasks and cannot fail for a reason the first two would
/// not have caught.
pub(super) fn approve(root: &Path, display_key: &str, tasks: &[String]) -> Result<String, String> {
    if tasks.is_empty() {
        return Err("nothing to approve — the plan has no tasks".to_string());
    }
    let key = display_key.trim_start_matches('#').to_string();
    let owner = format!("deck:{}", std::process::id());
    // Held across the branch write and its publication. The published branch
    // is what a peer's contention probe reads afterwards, on this clone and on
    // any other — see the module docs.
    let _lease = match crate::self_driving_cmd::claim::acquire_as(root, &key, &owner) {
        crate::self_driving_cmd::claim::Claim::Granted(lease) => Some(lease),
        crate::self_driving_cmd::claim::Claim::HeldBy(who) => {
            return Err(format!("#{key} is already claimed by {who}"));
        }
        // Fails open, like every other probe over this table: a ledger that
        // will not open is not a peer, and refusing to start work because a
        // local file would not open would be the worse failure.
        crate::self_driving_cmd::claim::Claim::Unavailable => None,
    };
    let branch = branch_name(&key, tasks);
    create_branch(root, &branch)?;
    let reach = match publish_branch(root, &branch) {
        Ok(()) => "pushed to origin".to_string(),
        Err(reason) => format!("local only — a peer on another clone cannot see it: {reason}"),
    };
    let graph = PlanGraph::approve(
        tasks
            .iter()
            .enumerate()
            .map(|(i, subject)| TaskNode::new(format!("{}", i + 1), subject.clone()))
            .collect(),
    )
    .map_err(|error| format!("the plan could not be authored: {error}"))?;
    Ok(format!(
        "#{key}: branch {branch} {reach} · plan r{} · {} tasks · {} [:NEXT] edges",
        graph.revision().get(),
        tasks.len(),
        graph.edges().len()
    ))
}

/// `stella/issue-<n>-<slug>` — the workspace's own branch prefix
/// (`self_driving_cmd::config`'s default) so the deck's branches sort beside
/// the loop's rather than under a name only this tab uses.
fn branch_name(key: &str, tasks: &[String]) -> String {
    let slug: String = tasks
        .first()
        .map(String::as_str)
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .to_lowercase()
        .split('-')
        .filter(|part| !part.is_empty())
        .take(5)
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        format!("stella/issue-{key}")
    } else {
        format!("stella/issue-{key}-{slug}")
    }
}

/// `git checkout -b` in `root`, reporting git's own refusal.
///
/// Not `git branch`: SPEC 8.2 says the human is now working the issue, and a
/// branch they are not standing on would leave the next edit on whatever they
/// were on before. A branch that already exists is git's error to report — it
/// almost always means a previous attempt, and silently reusing it would put
/// new work on top of old without saying so.
fn create_branch(root: &Path, branch: &str) -> Result<(), String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["checkout", "-b", branch])
        .output()
        .map_err(|error| format!("could not run git: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let reason = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if reason.is_empty() {
        format!("git refused to create {branch}")
    } else {
        reason
    })
}

/// Push `branch` to `origin` and track it, so a peer on any clone can see
/// that this issue is taken.
///
/// `--set-upstream` rather than a bare push: the human who approved this is
/// about to work on the branch, and an upstream is what makes their next
/// `git push` need no arguments.
///
/// The error is git's own first refusal, for the summary line to quote. It is
/// never fatal to the approval — see the module docs on why a failed network
/// write must not take the branch and the plan with it.
fn publish_branch(root: &Path, branch: &str) -> Result<(), String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["push", "--set-upstream", "origin", branch])
        .output()
        .map_err(|error| format!("could not run git: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(refusal(&String::from_utf8_lossy(&output.stderr)))
}

/// The one line of a git failure worth showing a human.
///
/// git writes its whole conversation to stderr, so the first line is often
/// `To <url>` and the last is a hint. The refusal is the line git marks as
/// one — `fatal:`, `error:`, or the `!` of a rejected ref — and the first
/// non-empty line is the fallback for a refusal git marked in some other way.
fn refusal(stderr: &str) -> String {
    let mut first: Option<&str> = None;
    for line in stderr.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if line.starts_with("fatal:") || line.starts_with("error:") || line.starts_with('!') {
            return line.to_string();
        }
        first.get_or_insert(line);
    }
    first
        .unwrap_or("git refused the push and said nothing")
        .to_string()
}

/// How many gate steps block a merge in this workspace, from the Makefile's
/// own `print-gate-steps` target — the same variable
/// `scripts/check-gate-parity.sh` consumes, so the card cannot name a
/// different number from the gate.
///
/// Zero when this workspace has no such target, and the card then says
/// `verify` without a count rather than claiming there are no gates.
fn gate_count(root: &Path) -> usize {
    Command::new("make")
        .arg("-C")
        .arg(root)
        .arg("-s")
        .arg("print-gate-steps")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .count()
        })
        .unwrap_or(0)
}

/// The files the code graph couples to the paths this issue names.
///
/// Two steps, both of them the graph's answer rather than the issue's: the
/// text is scanned for path-shaped tokens, each one that the index actually
/// holds is kept, and the neighbourhood of each — what it imports and what
/// imports it — is what the sources line calls *coupled*. An issue naming no
/// indexed path couples to nothing, and a workspace with no index answers
/// nothing at all; both render as an absent clause rather than a zero.
fn coupled_files(root: &Path, issue: &Issue) -> Vec<String> {
    let Ok(db_path) = stella_store::workspace_private_sqlite_path(root, "codegraph.db") else {
        return Vec::new();
    };
    let Ok(graph) = stella_graph::CodeGraph::open(root, &db_path) else {
        return Vec::new();
    };
    let mut found: BTreeSet<String> = BTreeSet::new();
    for candidate in path_tokens(&format!("{}\n{}", issue.title, issue.body)) {
        let Ok(hood) = graph.file_neighborhood(Path::new(&candidate)) else {
            continue;
        };
        // A path the index does not hold has no symbols, no imports and no
        // importers — nothing to couple, and keeping it would put a filename
        // the issue merely mentioned onto a line that claims the graph said it.
        if hood.symbols.is_empty() && hood.imports.is_empty() && hood.importers.is_empty() {
            continue;
        }
        found.insert(hood.file);
        for neighbour in hood.imports.into_iter().chain(hood.importers) {
            found.insert(neighbour);
        }
    }
    graph.shutdown();
    found.into_iter().take(MAX_COUPLED).collect()
}

/// The workspace RULEs that apply to this plan: those whose Tier-2 guard
/// refuses a write to one of the coupled files.
///
/// A guard evaluation, not a text search — `stella_learn::rules::evaluate_guards`
/// is the same function the tool boundary runs, so a rule listed here is one
/// that will actually fire on the work, and a rule that would not fire is not
/// listed as though it steered anything.
fn applied_rules(root: &Path, coupled: &[String]) -> Vec<DraftRule> {
    if coupled.is_empty() {
        return Vec::new();
    }
    let rules = crate::rules::load_workspace_rules_unfiltered(root);
    let mut applied: Vec<DraftRule> = Vec::new();
    for path in coupled {
        for violated in guards_hit(&rules, path) {
            if !applied.iter().any(|seen| seen.id == violated.id) {
                applied.push(violated);
            }
        }
    }
    applied
}

/// Every guarded rule that refuses a write to `path`.
fn guards_hit(rules: &[Rule], path: &str) -> Vec<DraftRule> {
    let action = ProposedAction {
        tool: "Write",
        path: Some(path),
        command: None,
    };
    evaluate_guards(rules, &action)
        .violations
        .into_iter()
        .filter_map(|violation| {
            let rule = rules.iter().find(|rule| rule.id == violation.rule_id)?;
            Some(DraftRule {
                id: rule.id.clone(),
                text: rule.text.clone(),
            })
        })
        .collect()
}

/// The drafted tasks: the issue's own plan list, or its title when it has
/// none.
fn plan_tasks(issue: &Issue) -> Vec<DraftTask> {
    let subjects = plan_list(&issue.body);
    let subjects = if subjects.is_empty() {
        vec![issue.title.clone()]
    } else {
        subjects
    };
    subjects
        .into_iter()
        .take(MAX_TASKS)
        .map(|subject| DraftTask {
            contract: contract_for(&subject),
            subject,
        })
        .collect()
}

/// The list under the issue's plan heading, else the body's first list.
///
/// Both are the issue author's own words. The heading is preferred because an
/// issue that has one has said which list is the plan; the first list is the
/// fallback for an issue that only ever wrote one.
fn plan_list(body: &str) -> Vec<String> {
    let mut under_heading: Vec<String> = Vec::new();
    let mut first_list: Vec<String> = Vec::new();
    let mut in_plan_heading = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix('#') {
            let heading = heading.trim_start_matches('#').trim().to_lowercase();
            in_plan_heading = PLAN_HEADINGS
                .iter()
                .any(|wanted| heading.contains(wanted) && under_heading.is_empty());
            continue;
        }
        let Some(item) = list_item(trimmed) else {
            continue;
        };
        if in_plan_heading {
            under_heading.push(item);
        } else if under_heading.is_empty() {
            first_list.push(item);
        }
    }
    if under_heading.is_empty() {
        first_list
    } else {
        under_heading
    }
}

/// One markdown list item's text, with its bullet, ordinal and checkbox
/// stripped. `None` for anything that is not a list item.
fn list_item(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| {
            let digits: String = line.chars().take_while(char::is_ascii_digit).collect();
            (!digits.is_empty())
                .then(|| line[digits.len()..].strip_prefix(". "))
                .flatten()
        })?;
    let rest = rest
        .strip_prefix("[ ] ")
        .or_else(|| rest.strip_prefix("[x] "))
        .or_else(|| rest.strip_prefix("[X] "))
        .unwrap_or(rest);
    let rest = rest.trim().trim_end_matches('.').trim();
    (!rest.is_empty()).then(|| rest.to_string())
}

/// What settles this task, or `None` when nothing can — see [`READING_VERBS`].
fn contract_for(subject: &str) -> Option<DraftContract> {
    let words = subject.to_lowercase();
    let first = words.split_whitespace().next().unwrap_or_default();
    if READING_VERBS.contains(&first) {
        return None;
    }
    let mechanism = mechanism_for(&words);
    let deterministic = MECHANISMS
        .iter()
        .find(|(name, _)| *name == mechanism)
        .is_some_and(|(_, deterministic)| *deterministic);
    Some(DraftContract {
        done_means: done_means(mechanism, subject),
        mechanism: mechanism.to_string(),
        deterministic,
    })
}

/// Which check the task's own words point at.
///
/// The order is most-specific-first: a task that says "test" is settled by
/// running that test whether or not it also names a file, and a task that
/// names neither falls back to `unit`, which is this repository's own
/// definition of done (AGENTS.md § witness tests) rather than a guess.
fn mechanism_for(words: &str) -> &'static str {
    let has = |needle: &str| words.contains(needle);
    if has("test") || has("witness") || has("property") {
        "unit"
    } else if has("gate") || has(" ci") || has("clippy") || has("lint") || has("fmt") {
        "gate"
    } else if has("build") || has("compile") {
        "build"
    } else if path_tokens(words).next().is_some() {
        "graph"
    } else {
        "unit"
    }
}

/// The one `done means:` line a diff-producing task shows.
fn done_means(mechanism: &str, subject: &str) -> String {
    match mechanism {
        "gate" => "`make gate` passes on the branch".to_string(),
        "build" => "the workspace compiles on the branch".to_string(),
        "graph" => match path_tokens(subject).next() {
            Some(path) => format!("{path} is changed on the branch"),
            None => "the named file is changed on the branch".to_string(),
        },
        _ => "a test over this fails before the change and passes after".to_string(),
    }
}

/// The bytes the plan's known inputs come to: the issue's own text plus every
/// coupled file on disk. A file the graph names and the filesystem no longer
/// has contributes nothing rather than a guess.
fn issue_bytes(issue: &Issue, root: &Path, coupled: &[String]) -> u64 {
    let files: u64 = coupled
        .iter()
        .filter_map(|path| std::fs::metadata(root.join(path)).ok())
        .map(|meta| meta.len())
        .sum();
    issue.title.len() as u64 + issue.body.len() as u64 + files
}

/// Path-shaped tokens in free text: something with a `/` and a dotted
/// extension, which is what a file path looks like in an issue body and a
/// prose word does not.
fn path_tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| c.is_whitespace() || "`\"'(),;<>[]{}".contains(c))
        .map(|token| token.trim_end_matches(['.', ':']))
        .filter(|token| {
            token.contains('/')
                && token.rsplit_once('.').is_some_and(|(_, ext)| {
                    !ext.is_empty() && ext.chars().all(char::is_alphanumeric)
                })
        })
        .map(str::to_string)
}

/// Price and time the drafted plan against what this workspace has measured.
///
/// The token figure is the plan's **input floor**: the issue's text plus every
/// coupled file's bytes, through the engine's own byte heuristic, once per
/// task — one pass over the same context per task, which is the cheapest way
/// the plan can possibly run. It is a floor and the card says `~`; a number
/// that guessed at the output a model has not produced would not be a
/// measurement of anything.
fn estimate(
    rates: Option<ModelRates>,
    bytes: u64,
    tasks: usize,
) -> Option<stella_tui::DraftEstimate> {
    let rates = rates?;
    let per_task = estimate_tokens_for_bytes(bytes);
    let tokens = per_task.saturating_mul(tasks.max(1) as u64);
    Some(stella_tui::DraftEstimate {
        usd: rates.usd_per_token * tokens as f64,
        tokens,
        minutes: (rates.ms_per_token * tokens as f64 / 60_000.0).round() as u64,
    })
}

#[cfg(test)]
mod tests;
