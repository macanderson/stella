//! The merge, over both formats, from real file contents.

use super::*;

const NOW: &str = "2026-07-20T00:00:00Z";

fn md(path: &str, contents: &str) -> RuleFile {
    RuleFile {
        path: path.to_string(),
        contents: contents.to_string(),
    }
}

fn facts<'a>() -> Facts<'a> {
    Facts {
        now: NOW,
        ..Facts::default()
    }
}

fn toml_record(lineage: &str, statement: &str, force: &str) -> String {
    format!(
        r#"
schema = "context-record/v0.1"
set_id = "acme.web"

[defaults]
origin = "user"
status = "active"

[[record]]
lineage_id = "{lineage}"
kind = "rule"
statement = "{statement}"

[record.steering]
force = "{force}"
precedence = 50
"#
    )
}

// One precedence order over both formats.

#[test]
fn markdown_rules_and_toml_records_render_in_one_block() {
    let files = vec![
        md(
            ".stella/rules/style.md",
            "---\ndescription: Style\n---\nMatch the surrounding code style.",
        ),
        md(
            ".stella/rules/acme.web.toml",
            &toml_record(
                "ctx.acme.web.pkg-manager",
                "This repository uses pnpm.",
                "must",
            ),
        ),
    ];
    let registry = load(&[], &files, &facts());
    assert!(
        registry.diagnostics.is_empty(),
        "{:?}",
        registry.diagnostics
    );
    let cached = registry.render(Channel::Cached, None);
    assert_eq!(
        cached.rendered,
        vec!["style", "pkg-manager"],
        "both formats reach one block, in directory order"
    );
    assert!(
        cached.text.contains("^style") && cached.text.contains("^pkg-manager"),
        "a legacy markdown rule becomes citable too: {}",
        cached.text
    );
    assert_eq!(
        cached.text.matches("## Workspace rules").count(),
        1,
        "one heading, not one per format: {}",
        cached.text
    );
}

#[test]
fn a_later_directory_overrides_an_earlier_one_across_formats() {
    let files = vec![
        md(
            "/home/u/.stella/rules/pkg-manager.md",
            "---\nname: pkg-manager\n---\nThe user's version.",
        ),
        md(
            ".stella/rules/acme.web.toml",
            &toml_record("ctx.pkg-manager", "The project's version.", "must"),
        ),
    ];
    let registry = load(&[], &files, &facts());
    assert_eq!(
        registry.entries.len(),
        1,
        "one handle merges across formats"
    );
    assert_eq!(
        registry.entries[0].record.record.statement, "The project's version.",
        "the later directory wins, as the markdown loader has always done"
    );
}

// #891 — the nesting refusal, surfaced.

#[test]
fn a_nested_frontmatter_key_refuses_the_file_with_an_actionable_error() {
    let files = vec![md(
        ".stella/rules/api-integration-coverage.md",
        "---\nname: api-integration-coverage\nscope:\n  repository_id: repo_stella\n---\nAPI changes need integration coverage.",
    )];
    let registry = load(&[], &files, &facts());
    assert!(
        registry.entries.is_empty(),
        "a record wearing a scope nobody wrote must not load"
    );
    let diagnostic = registry
        .diagnostics
        .first()
        .expect("the refusal must be reported, not silent");
    assert_eq!(
        diagnostic.source,
        ".stella/rules/api-integration-coverage.md"
    );
    assert!(diagnostic.detail.contains("repository_id"), "{diagnostic}");
    assert!(
        diagnostic.detail.contains("TOML"),
        "the error must say what to do instead: {diagnostic}"
    );
}

#[test]
fn a_legacy_multi_paragraph_rule_body_still_loads() {
    // The record surface requires a single-sentence statement. A markdown rule's
    // body has never had that constraint, and applying it here would refuse rules
    // that have shipped for months.
    let files = vec![md(
        ".stella/rules/long.md",
        "---\nname: long\n---\nFirst paragraph of guidance.\n\nSecond paragraph, with more detail.",
    )];
    let registry = load(&[], &files, &facts());
    assert_eq!(registry.entries.len(), 1, "{:?}", registry.diagnostics);
    assert!(
        registry.entries[0].record.findings.is_empty(),
        "legacy bodies are not judged against the record schema: {:?}",
        registry.entries[0].record.findings
    );
}

#[test]
fn a_malformed_toml_file_is_reported_and_the_rest_still_load() {
    let files = vec![
        md(
            ".stella/rules/broken.toml",
            "schema = \"context-record/v0.1\"\n[[record]\n",
        ),
        md(
            ".stella/rules/good.toml",
            &toml_record("ctx.acme.web.ok", "A valid record.", "must"),
        ),
    ];
    let registry = load(&[], &files, &facts());
    assert_eq!(registry.entries.len(), 1);
    assert_eq!(registry.diagnostics.len(), 1);
    assert_eq!(registry.diagnostics[0].source, ".stella/rules/broken.toml");
}

#[test]
fn a_directory_of_prose_is_not_reported_as_broken_rules() {
    let files = vec![md(".stella/rules/README.md", "---\nname: readme\n---\n")];
    let registry = load(&[], &files, &facts());
    assert!(registry.entries.is_empty());
    assert!(
        registry.diagnostics.is_empty(),
        "a file with no statement was never a rule; saying so per README is noise"
    );
}

// Grandfathering — the regression this design exists to avoid.

#[test]
fn a_shipped_markdown_guard_is_still_armed_with_no_approval_ceremony() {
    let files = vec![md(
        ".stella/rules/no-applied-migration.md",
        "---\nguard-tool: Edit\nguard-deny-path: migrations/*-applied/**\n---\nAdd a forward migration.",
    )];
    let registry = load(&[], &files, &facts());
    let entry = &registry.entries[0];
    assert!(
        entry.is_enforced(),
        "silently disarming a shipped guard would be a security regression"
    );
    assert!(entry.is_legacy_markdown());
    assert_eq!(
        registry.rules[0]
            .guard
            .as_ref()
            .and_then(|g| g.tool.as_deref()),
        Some("Edit"),
        "the guard must reach the tool boundary"
    );
    assert_eq!(
        registry.rules[0].id, "no-applied-migration",
        "the boundary cites the handle, so a blocked call names something the model can reuse"
    );
    assert!(
        registry
            .render(Channel::Cached, None)
            .text
            .contains("[enforced]"),
        "the prompt must not understate what actually happens"
    );
}

#[test]
fn a_toml_record_needs_the_full_blocking_gate() {
    let hard = r#"
schema = "context-record/v0.1"
set_id = "acme.web"

[defaults]
origin = "imported"
status = "active"

[[record]]
lineage_id = "ctx.acme.web.no-force-push"
kind = "constraint"
statement = "Never force-push to a shared branch."

[record.steering]
force = "must"

[record.enforcement]
mode = "hard"
guard_tool = "Bash"
guard_deny_command = "git push --force*"
"#;
    let registry = load(&[], &[md(".stella/rules/acme.web.toml", hard)], &facts());
    let entry = &registry.entries[0];
    assert!(
        !entry.is_enforced(),
        "an imported record must never start blocking, however it is committed"
    );
    assert!(
        registry
            .diagnostics
            .iter()
            .any(|d| d.detail.contains("no-force-push") && d.detail.contains("origin is imported")),
        "the refusal must be reported so the author knows the guard is inert: {:?}",
        registry.diagnostics
    );
    assert!(
        !registry
            .render(Channel::Cached, None)
            .text
            .contains("[enforced]"),
        "and the prompt must not claim enforcement that does not exist"
    );
}

// The truth sweep, through the registry.

#[test]
fn a_refuted_record_leaves_the_registry_before_anything_renders() {
    let mut facts = facts();
    facts
        .verdicts
        .insert("ctx.acme.web.node-version".to_string(), Verdict::Refuted);
    let files = vec![md(
        ".stella/rules/acme.web.toml",
        &toml_record(
            "ctx.acme.web.node-version",
            "Development runs on Node 20.",
            "must",
        ),
    )];
    let registry = load(&[], &files, &facts);
    assert!(
        registry.render(Channel::Cached, None).text.is_empty(),
        "the whole point: a claim the world refuted stops steering"
    );
    assert!(
        matches!(registry.entries[0].disposition, Disposition::Drop { .. }),
        "{:?}",
        registry.entries[0].disposition
    );
}

// Conflicts suspend enforcement rather than picking a winner.

#[test]
fn a_conflict_suspends_the_more_restrictive_guard_and_reports_both_sides() {
    let conflicting = r#"
schema = "context-record/v0.1"
set_id = "acme.web"

[defaults]
origin = "user"
status = "active"

[[record]]
lineage_id = "ctx.acme.web.internal-guard"
kind = "constraint"
statement = "Internal routes are not edited directly."

[record.steering]
force = "must"
precedence = 50

[record.steering.applies_to]
paths = ["routes/internal/**"]

[record.enforcement]
mode = "hard"
guard_tool = "Edit"
guard_deny_path = "routes/internal/**"

[record.truth]
basis = "decree"
verified_by = "mac"

[[record]]
lineage_id = "ctx.acme.web.routes-exemption"
kind = "rule"
statement = "Route changes are reviewed but not blocked."

[record.steering]
force = "should"
precedence = 50

[record.steering.applies_to]
paths = ["routes/**"]

[record.enforcement]
mode = "none"
"#;
    let mut facts = facts();
    facts
        .approved_blocking
        .insert((Trust::Project, "ctx.acme.web.internal-guard".to_string()));
    let registry = load(
        &[],
        &[md(".stella/rules/acme.web.toml", conflicting)],
        &facts,
    );

    assert_eq!(registry.conflicts.len(), 1, "{:?}", registry.conflicts);
    assert_eq!(registry.conflicts[0].suspended, "internal-guard");
    let guarded = registry.by_handle("internal-guard").expect("loaded");
    assert!(
        !guarded.is_enforced(),
        "enforcement falls back to the less restrictive behavior until an owner resolves it"
    );
    assert!(
        registry.entries.iter().all(|entry| entry
            .record
            .findings
            .iter()
            .any(|f| matches!(f, RecordFinding::Conflict { .. }))),
        "both sides must carry the conflict so either record explains it"
    );
    // Both still steer — the conflict downgrades enforcement, it does not silence
    // the policy. `must` and `should` both ride the cached channel, grouped.
    let cached = registry.render(Channel::Cached, None);
    assert_eq!(cached.rendered, vec!["internal-guard", "routes-exemption"]);
    assert!(
        !cached.text.contains("[enforced]"),
        "the suspended guard must not be advertised as armed: {}",
        cached.text
    );
    assert!(
        registry.render(Channel::Volatile, None).text.is_empty(),
        "nothing was demoted"
    );
}

// The trust tier: a project record may add enforcement and may never remove it.

/// A record that establishes a hard guard, decreed by a named human — the shape
/// that self-approves in `~/.stella/rules` and needs a ledger entry anywhere else.
fn guarded_record(lineage: &str, deny_command: &str) -> String {
    format!(
        r#"
schema = "context-record/v0.1"
set_id = "acme.web"

[[record]]
lineage_id = "{lineage}"
kind = "constraint"
statement = "Never force-push to a shared branch."
origin = "user"
status = "active"

[record.steering]
force = "must"
precedence = 50

[record.truth]
basis = "decree"
verified_by = "mac"

[record.enforcement]
mode = "hard"
guard_tool = "Bash"
guard_deny_command = "{deny_command}"
"#
    )
}

/// A record naming `lineage` that enforces nothing — the shape that used to win the
/// merge outright and take the guard it displaced with it.
fn unguarded_record(lineage: &str) -> String {
    format!(
        r#"
schema = "context-record/v0.1"
set_id = "acme.web"

[[record]]
lineage_id = "{lineage}"
kind = "rule"
statement = "Force-pushing is fine on this project."
origin = "user"
status = "active"

[record.steering]
force = "must"
precedence = 50

[record.enforcement]
mode = "none"
"#
    )
}

const GUARDED: &str = "ctx.acme.web.no-force-push";

#[test]
fn a_project_record_cannot_disarm_a_user_records_guard() {
    // The hole: `lineage_id` is author-supplied, and the project directories used to
    // come last in one flat list. A checkout naming the user's lineage replaced the
    // user's record wholesale — armed guard and all.
    let user = vec![md(
        "/home/mac/.stella/rules/no-force-push.toml",
        &guarded_record(GUARDED, "git push --force*"),
    )];
    let project = vec![md(
        ".stella/rules/permissive.toml",
        &unguarded_record(GUARDED),
    )];
    let registry = load(&user, &project, &facts());

    assert_eq!(
        registry.entries.len(),
        1,
        "the project record could only ever weaken this lineage, so it is dropped"
    );
    let entry = &registry.entries[0];
    assert_eq!(entry.record.trust, Trust::User);
    assert!(
        entry.is_enforced(),
        "a cloned repository must not be able to switch off the user's own guard"
    );
    assert_eq!(
        entry.record.record.statement, "Never force-push to a shared branch.",
        "and the user's statement is the one that steers"
    );
}

#[test]
fn a_project_record_carrying_a_different_guard_is_kept_alongside() {
    // Monotonic, not merely protective: the repository's own constraint still
    // applies. Dropping it would let a user record switch off a repository guard,
    // which is the same bug pointing the other way.
    let user = vec![md(
        "/home/mac/.stella/rules/no-force-push.toml",
        &guarded_record(GUARDED, "git push --force*"),
    )];
    let project = vec![md(
        ".stella/rules/stricter.toml",
        &guarded_record(GUARDED, "git push*"),
    )];
    let mut facts = facts();
    facts
        .approved_blocking
        .insert((Trust::Project, GUARDED.to_string()));
    let registry = load(&user, &project, &facts);

    assert_eq!(registry.entries.len(), 2, "both constraints survive");
    assert!(
        registry.entries.iter().all(Entry::is_enforced),
        "neither tier weakens the other"
    );
    let handles: Vec<&str> = registry
        .entries
        .iter()
        .map(|entry| entry.record.handle.as_str())
        .collect();
    assert_ne!(
        handles[0], handles[1],
        "two records sharing a lineage must still be separately citable"
    );
}

#[test]
fn a_project_record_still_overrides_a_user_record_that_enforces_nothing() {
    // The historical override is not collateral damage of the fix. Repository policy
    // beating personal preference is the *designed* behavior everywhere it is not
    // removing a protection.
    let user = vec![md(
        "/home/mac/.stella/rules/prefs.toml",
        &toml_record("ctx.acme.web.pkg-manager", "Personal preference.", "should"),
    )];
    let project = vec![md(
        ".stella/rules/acme.web.toml",
        &toml_record(
            "ctx.acme.web.pkg-manager",
            "This repository uses pnpm.",
            "must",
        ),
    )];
    let registry = load(&user, &project, &facts());

    assert_eq!(registry.entries.len(), 1);
    assert_eq!(
        registry.entries[0].record.record.statement, "This repository uses pnpm.",
        "the later directory still wins when there is no enforcement to protect"
    );
    assert_eq!(registry.entries[0].record.trust, Trust::Project);
}

#[test]
fn a_repository_record_cannot_approve_its_own_blocking() {
    // `origin`, `basis`, and `verified_by` are fields inside the file being judged.
    // In `~/.stella/rules` they are the author speaking about themselves; in a
    // checkout they are a stranger asserting the author's consent.
    let project = vec![md(
        ".stella/rules/no-force-push.toml",
        &guarded_record(GUARDED, "git push --force*"),
    )];
    let registry = load(&[], &project, &facts());

    assert!(
        !registry.entries[0].is_enforced(),
        "a repository record reaches the tool boundary only through the ledger"
    );
    assert!(
        registry
            .diagnostics
            .iter()
            .any(|d| d.detail.contains("approve it locally")),
        "and the refusal must say what would actually help: {:?}",
        registry.diagnostics
    );
}

#[test]
fn the_same_record_in_the_users_own_directory_self_approves() {
    // The other half of the same decision — the local-first solo path must not
    // acquire a ledger requirement.
    let user = vec![md(
        "/home/mac/.stella/rules/no-force-push.toml",
        &guarded_record(GUARDED, "git push --force*"),
    )];
    let registry = load(&user, &[], &facts());
    assert!(
        registry.entries[0].is_enforced(),
        "writing the file, naming yourself, and keeping it on your own disk IS the approval"
    );
}

#[test]
fn a_user_approval_does_not_transfer_to_a_project_record_of_the_same_lineage() {
    // Approvals used to be keyed by lineage alone. A lineage does not identify a
    // record — it is author-supplied — so an approval the user granted their own
    // record was handed to any repository record that guessed the name.
    let project = vec![md(
        ".stella/rules/no-force-push.toml",
        &guarded_record(GUARDED, "git push --force*"),
    )];
    let mut facts = facts();
    facts
        .approved_blocking
        .insert((Trust::User, GUARDED.to_string()));
    let registry = load(&[], &project, &facts);

    assert!(
        !registry.entries[0].is_enforced(),
        "the approval was granted to the user's record, not to this lineage as a name"
    );
}

// Lookup + reporting

#[test]
fn by_handle_accepts_the_caret_form_the_agent_writes() {
    let files = vec![md(
        ".stella/rules/acme.web.toml",
        &toml_record(
            "ctx.acme.web.pkg-manager",
            "This repository uses pnpm.",
            "must",
        ),
    )];
    let registry = load(&[], &files, &facts());
    assert!(registry.by_handle("^pkg-manager").is_some());
    assert!(registry.by_handle("pkg-manager").is_some());
    assert!(registry.by_handle("nope").is_none());
}

#[test]
fn findings_are_reported_worst_first() {
    let files = vec![md(
        ".stella/rules/acme.web.toml",
        &toml_record(
            "ctx.acme.web.pkg-manager",
            "This repository uses pnpm.",
            "must",
        ),
    )];
    let registry = load(&[], &files, &facts());
    let findings = registry.findings();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].1, &RecordFinding::IdentityStamped);
}

#[test]
fn an_empty_workspace_loads_an_empty_registry() {
    let registry = load(&[], &[], &facts());
    assert_eq!(registry, Registry::default());
    assert!(registry.render(Channel::Cached, None).text.is_empty());
}
