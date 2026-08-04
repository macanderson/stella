//! `stella context propose <handle>` — turning a record into a reviewable change.
//!
//! The headline of `docs/design/adaptive-context/context-pr.md` is that a Context PR reuses the workflows
//! engineers already trust: diff, ownership, approval, revert. This command is where
//! a record becomes one of those.
//!
//! # Preconditions, and why failing them produces nothing
//!
//! §8.1 lists six things that must be true before a branch is created, and §8.1's
//! last line is the important one: *"If any precondition fails, Stella records a
//! candidate only. It must not manufacture a PR around vague policy."* A PR that
//! cannot say which repository it targets, who should review it, or what will
//! actually match at runtime is a review request with no reviewable content — and
//! the cost of one lands on somebody else. So the preconditions are checked, the
//! failures are named, and nothing is created.
//!
//! # Owner routing activates only when owners resolve
//!
//! §5.2. Until `CODEOWNERS` resolves somebody for the path the record would land in,
//! the proposal stays **advisory and unowned** — it prints the plan, and says so.
//! Inventing a reviewer would route a policy change to whoever happened to be in
//! `git log`, which is worse than admitting nobody is assigned.
//!
//! # Personal records never enter a Context PR
//!
//! §10 makes this a privacy boundary rather than a preference: a `personal` record
//! lives in `~/.stella/rules/` and is nobody else's business. `propose` refuses one
//! outright rather than filtering it out of a diff later, so the refusal is visible
//! at the point somebody asks for it.
//!
//! # The body and the diff are generated independently
//!
//! §8.2's last line. The body is written from the record; the diff is the record
//! file. Neither is derived from the other, so a body stays legible even when an
//! evidence link has gone stale — which is the state most evidence links reach.

use std::path::Path;
use std::process::Command;

use colored::Colorize;

use stella_core::ingest::record::{LinkRelation, SharingScope};
use stella_core::records::{Entry, Registry, Severity};

/// `stella context propose <rule>`.
pub fn run_propose(root: &Path, needle: &str, commit: bool) -> Result<(), String> {
    let registry = crate::context_records::load_registry(root);
    let entry = registry
        .by_handle(needle)
        .or_else(|| {
            registry
                .entries
                .iter()
                .find(|entry| entry.record.record.lineage_id == needle)
        })
        .ok_or_else(|| {
            format!("no record matches \"{needle}\" — `stella context list` shows what is loaded")
        })?;

    let scope = entry
        .record
        .record
        .sharing_scope
        .unwrap_or(SharingScope::Repository);
    if scope == SharingScope::Personal {
        return Err(format!(
            "^{} is a personal record. Personal records live in ~/.stella/rules/ and are never \
             promoted into a shared change (docs/design/adaptive-context/context-pr.md §10). Change its sharing_scope to \
             `repository` first if it really is repository policy.",
            entry.record.handle
        ));
    }

    let checks = Preconditions::check(root, &registry, entry);
    println!();
    checks.print();
    if !checks.all_met() {
        return Err(
            "preconditions not met — recorded nothing. A PR that cannot state its target, its \
             owners, or its runtime effect is a review request with no reviewable content."
                .to_string(),
        );
    }

    let branch = format!("stella/context/{}", entry.record.handle);
    let title = format!("context: {}", summary(entry));
    // Repo-relative, because everything below is read by somebody else: an absolute
    // path names a directory layout on one machine, and in a PR body it is both
    // noise and a diff reference that does not resolve.
    let record_path = relative_to(root, &entry.record.source);
    let body = pr_body(entry, &checks, &record_path);

    println!("{}", "Branch".bold());
    println!("  {branch}");
    println!("{}", "Title".bold());
    println!("  {title}");
    println!("{}", "Labels".bold());
    println!("  {}", "context, proposed-policy".dimmed());
    println!("{}", "Diff".bold());
    println!("  {}  {}", "1 file".dimmed(), record_path);
    println!();
    println!("{body}");

    match checks.owners.as_deref() {
        Some(owners) => println!("{}  {owners}", "Reviewers".bold()),
        None => println!(
            "{}",
            "No CODEOWNERS entry resolves for this path, so this proposal is advisory and \
             unowned. It can still be committed locally."
                .yellow()
        ),
    }

    if !commit {
        println!(
            "\n{}",
            "Nothing was created. Re-run with --commit to make the branch and the local commit \
             (solo mode); push it to open the pull request."
                .dimmed()
        );
        return Ok(());
    }
    commit_locally(root, &branch, &title, &body, &record_path)
}

/// The §8.1 preconditions, each with the answer it produced.
struct Preconditions {
    /// The repository this would target.
    repository: Option<String>,
    /// Whether the record is schema-valid enough to publish.
    schema_valid: bool,
    /// The scope, in words.
    scope: String,
    /// The resolved code owners, when `CODEOWNERS` names any.
    owners: Option<String>,
    /// Evidence ids from the record's `supports` links.
    evidence: Vec<String>,
    /// The confidence the record carries.
    confidence: Option<u8>,
    /// What will match at runtime.
    runtime_effect: String,
    /// Whether the forbidden-data scan is clean.
    no_restricted_data: bool,
    /// Blocking findings, which are also what makes `schema_valid` false.
    blockers: Vec<String>,
}

impl Preconditions {
    fn check(root: &Path, registry: &Registry, entry: &Entry) -> Self {
        let record = &entry.record.record;
        let blockers: Vec<String> = entry
            .record
            .findings
            .iter()
            .filter(|finding| finding.severity() == Severity::Blocking)
            .map(ToString::to_string)
            .collect();
        // A record that reached the registry has already survived the blocking filter,
        // so a clean list here is a real answer rather than an unchecked one — but the
        // conflict findings still matter, and they are warnings.
        let conflicted = registry
            .conflicts
            .iter()
            .any(|conflict| conflict.suspended == entry.record.handle);

        let applies_to = record
            .steering
            .as_ref()
            .and_then(|steering| steering.applies_to.as_ref());
        let paths: Vec<String> = applies_to
            .map(|applies| applies.paths.clone())
            .unwrap_or_default();
        let scope = if paths.is_empty() {
            "repository: this repo — every path".to_string()
        } else {
            format!("repository: this repo — {}", paths.join(", "))
        };

        Self {
            repository: git(root, &["remote", "get-url", "origin"]),
            schema_valid: blockers.is_empty(),
            scope,
            owners: resolve_owners(root, &entry.record.source),
            evidence: record
                .links
                .iter()
                .filter(|link| link.relation == LinkRelation::Supports)
                .map(|link| link.target.clone())
                .collect(),
            confidence: record.truth.as_ref().and_then(|truth| truth.confidence),
            runtime_effect: runtime_effect(entry, &paths, conflicted),
            no_restricted_data: !blockers
                .iter()
                .any(|blocker| blocker.contains("must not enter a Git-tracked record")),
            blockers,
        }
    }

    fn all_met(&self) -> bool {
        self.repository.is_some() && self.schema_valid && self.no_restricted_data
    }

    fn print(&self) {
        println!("{}", "Preconditions".bold());
        // `✗` is reserved for what actually stops the proposal. Unresolved owners,
        // missing evidence links, and an unstated confidence are all things §5.2 and
        // §8.1 permit — marking them with a failure glyph would tell the reader the
        // run was refused when it was not.
        let mark = |ok: bool| if ok { "✓".green() } else { "✗".red() };
        let soft = |ok: bool| if ok { "✓".green() } else { "·".dimmed() };
        println!(
            "  {} repository: {}",
            mark(self.repository.is_some()),
            self.repository
                .as_deref()
                .unwrap_or("no git remote named origin — nothing to open a change against")
        );
        println!(
            "  {} schema-valid, single-concern diff",
            mark(self.schema_valid)
        );
        println!("  {} scope: {}", mark(true), self.scope);
        println!(
            "  {} owners: {}",
            soft(self.owners.is_some()),
            self.owners
                .as_deref()
                .unwrap_or("unresolved — the proposal stays advisory and unowned (§5.2)")
        );
        println!(
            "  {} evidence: {}",
            soft(!self.evidence.is_empty()),
            if self.evidence.is_empty() {
                "no supporting evidence links on the record".to_string()
            } else {
                self.evidence.join(", ")
            }
        );
        println!(
            "  {} confidence: {}",
            soft(self.confidence.is_some()),
            self.confidence
                .map(|value| format!("{value}"))
                .unwrap_or_else(|| "not stated".to_string())
        );
        println!("  {} runtime effect stated", mark(true));
        println!(
            "  {} no restricted data in the record",
            mark(self.no_restricted_data)
        );
        for blocker in &self.blockers {
            println!("      {}", blocker.red());
        }
        println!();
    }
}

/// The §8.2 body. Generated from the record, independently of the diff.
fn pr_body(entry: &Entry, checks: &Preconditions, record_path: &str) -> String {
    let record = &entry.record.record;
    let enforcement = record
        .enforcement
        .as_ref()
        .map(|enforcement| enforcement.mode.as_str())
        .unwrap_or("none");
    let tier = if entry.is_enforced() {
        "blocking (Tier 2 — matching tool calls are denied)"
    } else {
        "advisory (Tier 1 — prompt only)"
    };

    let mut evidence = String::new();
    if let Some(provenance) = record.provenance.as_ref()
        && let Some(uri) = provenance.source_uri.as_deref()
    {
        let lines = provenance
            .source_lines
            .as_ref()
            .filter(|range| range.len() == 2)
            .map(|range| format!(" lines {}–{}", range[0], range[1]))
            .unwrap_or_default();
        evidence.push_str(&format!("- {uri}{lines}\n"));
    }
    for id in &checks.evidence {
        evidence.push_str(&format!("- {id}\n"));
    }
    if let Some(confidence) = checks.confidence {
        evidence.push_str(&format!("- extraction confidence {confidence}\n"));
    }
    if evidence.is_empty() {
        evidence.push_str("- none recorded on this record\n");
    }

    format!(
        "## Proposed steering\n{statement}\n\n\
         ## Scope\n{scope}\n\n\
         ## Suggested enforcement\n{tier} — `enforcement.mode = \"{enforcement}\"`\n\n\
         ## Evidence\n{evidence}\n\
         ## Expected runtime effect\n{effect}\n\n\
         ## Alternatives considered\n\
         - No record: rejected — the claim is already written down somewhere that drifts.\n\
         - {alternative}\n\n\
         ## Provenance\n\
         `{source}` · `{lineage}` · cited as `^{handle}`\n",
        statement = record.statement,
        scope = checks.scope,
        evidence = evidence,
        effect = checks.runtime_effect,
        alternative = if entry.is_enforced() {
            "Advisory only: rejected — an owner approved blocking and a real enforcer exists."
        } else {
            "Tier-2 guard: deferred until advisory-mode accuracy is measured."
        },
        source = record_path,
        lineage = record.lineage_id,
        handle = entry.record.handle,
    )
}

/// What will and will not match, in the concrete terms §8.1(5) and §12 both ask for.
fn runtime_effect(entry: &Entry, paths: &[String], conflicted: bool) -> String {
    let mut effect = String::new();
    if entry.is_enforced() {
        let guard = entry.guard.guard.as_ref();
        // A guard with no `guard_tool` is not tool-scoped, which is a real and
        // deliberate shape — a path guard that names no tool covers every tool
        // that names a path. Rendering it as the tool literally called
        // "any tool" both misread as a name and produced "A `any tool` call".
        let subject = guard.and_then(|guard| guard.tool.clone()).map_or_else(
            || "A call from any tool".to_string(),
            |tool| format!("A `{tool}` call"),
        );
        let pattern = guard
            .and_then(|guard| {
                guard
                    .deny_path_glob
                    .clone()
                    .or_else(|| guard.deny_command_glob.clone())
            })
            .unwrap_or_else(|| "*".to_string());
        effect.push_str(&format!(
            "{subject} matching `{pattern}` is denied at the tool boundary and the record's \
             text is returned to the model. Nothing else is affected."
        ));
    } else if paths.is_empty() {
        effect.push_str(
            "The statement is rendered into the system prompt for every task in this workspace. \
             No tool call is blocked.",
        );
    } else {
        effect.push_str(&format!(
            "Agents working under {} are prompted with this statement. Paths outside that scope \
             are unaffected, and no tool call is blocked.",
            paths.join(", ")
        ));
    }
    if conflicted {
        effect.push_str(
            "\n\n**Note:** this record currently conflicts with another at equal precedence, so \
             its enforcement is suspended until the conflict is resolved.",
        );
    }
    effect
}

/// A one-line summary for the title — the statement's first clause, lowercased.
fn summary(entry: &Entry) -> String {
    let statement = entry.record.record.statement.trim();
    let mut chars = first_clause(statement).chars();
    match chars.next() {
        Some(first_char) => format!("{}{}", first_char.to_lowercase(), chars.as_str()),
        None => entry.record.handle.clone(),
    }
}

/// The statement up to its first sentence break.
///
/// A period only ends a sentence when what follows it is whitespace or nothing.
/// Splitting on every `.` instead truncated the very first Context PR this repo
/// opened — the statement ends `… widening the license allow-list in
/// \`deny.toml\`.`, and the title came out at `… the license allow-list in
/// \`deny`. Records name files, and file names have dots in them; a title cut
/// mid-identifier is worse than a long one, because it reads as a different
/// claim rather than an abbreviated one.
pub(super) fn first_clause(statement: &str) -> &str {
    for (i, ch) in statement.char_indices() {
        if ch != '.' && ch != ';' {
            continue;
        }
        let rest = &statement[i + ch.len_utf8()..];
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return statement[..i].trim();
        }
    }
    statement
}

/// The `CODEOWNERS` owners for `path`, when the file exists and a pattern matches.
///
/// Deliberately simple matching: the literal directory prefix, plus a bare `*`
/// catch-all. `CODEOWNERS` supports more than that, and a partial implementation
/// that silently mismatched would route a policy change to the wrong team — so an
/// unmatched path resolves to `None`, which §5.2 already has a defined meaning for.
fn resolve_owners(root: &Path, path: &str) -> Option<String> {
    let candidates = [
        root.join("CODEOWNERS"),
        root.join(".github/CODEOWNERS"),
        root.join("docs/CODEOWNERS"),
    ];
    let raw = candidates
        .iter()
        .find_map(|candidate| std::fs::read_to_string(candidate).ok())?;
    let relative = path.trim_start_matches("./");
    let mut fallback = None;
    for line in raw.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let pattern = parts.next()?;
        let owners: Vec<&str> = parts.collect();
        if owners.is_empty() {
            continue;
        }
        if pattern == "*" {
            fallback = Some(owners.join(" "));
            continue;
        }
        let prefix = pattern.trim_start_matches('/').trim_end_matches('*');
        if !prefix.is_empty() && relative.contains(prefix) {
            return Some(owners.join(" "));
        }
    }
    fallback
}

/// Solo mode: a branch and an ordinary local commit. Not a pull request — in the UI
/// this reads "add this rule to the repository" (§5.1), and pushing it is the user's
/// call, not ours.
fn commit_locally(
    root: &Path,
    branch: &str,
    title: &str,
    body: &str,
    record_path: &str,
) -> Result<(), String> {
    if git(root, &["rev-parse", "--git-dir"]).is_none() {
        return Err("not a git repository — nothing to commit into".to_string());
    }
    if git(root, &["rev-parse", "--verify", branch]).is_some() {
        return Err(format!(
            "branch {branch} already exists — check it out and amend, or delete it first"
        ));
    }
    run_git(root, &["checkout", "-b", branch])?;
    run_git(root, &["add", "--", record_path])?;
    let message = format!("{title}\n\n{body}");
    run_git(root, &["commit", "-m", &message])?;
    println!("\n  {}  {branch}", "committed".green());
    println!(
        "    {}",
        "push it to open the pull request, or keep it local — the record is already loaded either \
         way"
        .dimmed()
    );
    Ok(())
}

/// `path` relative to `root`, or unchanged when it is not under it.
fn relative_to(root: &Path, path: &str) -> String {
    let candidate = Path::new(path);
    candidate
        .strip_prefix(root)
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string())
}

/// A git command whose output we want, or `None` when it failed.
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// A git command that must succeed.
fn run_git(root: &Path, args: &[&str]) -> Result<(), String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git {}: {e}", args.join(" ")))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}
