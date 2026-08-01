//! The review surface, end to end — every acceptance criterion the epic states,
//! exercised through the commands a person actually runs.

use super::*;
use stella_core::records::{Channel, Decision, decision};

use crate::context_records::{PROPOSALS_DIR, RULES_DIR, load_registry, read_decisions};

/// Two proposals as `stella ingest CLAUDE.md` would write them: one the tree
/// supports, one it refutes.
fn write_proposals(root: &Path) {
    let dir = root.join(PROPOSALS_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("claude-md.toml"),
        r#"
schema = "context-record/v0.1"
set_id = "acme.web"
ingest_run_id = "ing_01"

[defaults]
sharing_scope = "repository"
origin = "imported"
status = "active"

[defaults.provenance]
source_kind = "document"
source_uri = "CLAUDE.md"
commit = "6ee3d4a"

[[proposal]]
candidate_id = "pkg-manager-1111aaaa"
proposal_kind = "directive"
status = "eligible"
confidence = 95
observed_at = "2026-07-20T18:30:00Z"
eligibility = "explicit_instruction"

[proposal.record]
lineage_id = "ctx.acme.web.pkg-manager"
kind = "rule"
statement = "This repository uses pnpm exclusively."

[proposal.record.steering]
force = "must"
precedence = 60

[proposal.record.provenance]
source_lines = [12, 12]

[proposal.record.truth]
basis = "measured"

[proposal.record.truth.probe]
kind = "path_exists"
path = "pnpm-lock.yaml"

[proposal.refutation]
verdict = "supported"
checked_at = "2026-07-20T18:30:01Z"
probe_kind = "path_exists"
detail = "path `pnpm-lock.yaml` exists"

[[proposal]]
candidate_id = "node-version-2222bbbb"
proposal_kind = "knowledge"
status = "eligible"
confidence = 88
observed_at = "2026-07-20T18:30:00Z"

[proposal.record]
lineage_id = "ctx.acme.web.node-version"
kind = "fact"
statement = "Development runs on Node 20."

[proposal.record.steering]
force = "must"
precedence = 60

[proposal.record.truth]
basis = "measured"

[proposal.record.truth.probe]
kind = "file_contains"
path = ".nvmrc"
pattern = "20"

[proposal.refutation]
verdict = "refuted"
checked_at = "2026-07-20T18:30:01Z"
probe_kind = "file_contains"
detail = "`.nvmrc` does not contain `20` (it says 22)"
recommend = "ignore"

[[proposal]]
candidate_id = "compound-3333cccc"
proposal_kind = "directive"
status = "dismissed"
confidence = 60
observed_at = "2026-07-20T18:30:00Z"
dismissed_reason = "compound_claim"

[proposal.record]
lineage_id = "ctx.acme.web.deploys"
kind = "rule"
statement = "Deploys run from main, happen on Tuesdays, and are triggered with make ship."

[proposal.record.steering]
force = "must"
"#,
    )
    .unwrap();
}

fn workspace() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    write_proposals(root.path());
    std::fs::write(root.path().join("pnpm-lock.yaml"), "lockfileVersion: 9\n").unwrap();
    std::fs::write(root.path().join(".nvmrc"), "22\n").unwrap();
    root
}

// Resolution

#[test]
fn a_candidate_resolves_by_unique_prefix() {
    let root = workspace();
    let proposals = read_proposals(root.path());
    assert_eq!(proposals.len(), 3);
    assert_eq!(
        resolve_candidate(&proposals, "pkg-manager")
            .unwrap()
            .proposal
            .candidate_id,
        "pkg-manager-1111aaaa"
    );
    assert_eq!(
        resolve_candidate(&proposals, "pkg-manager-1111aaaa")
            .unwrap()
            .proposal
            .candidate_id,
        "pkg-manager-1111aaaa"
    );
}

#[test]
fn an_unknown_candidate_says_what_to_run_instead() {
    let root = workspace();
    let proposals = read_proposals(root.path());
    let err = resolve_candidate(&proposals, "nope").unwrap_err();
    assert!(err.contains("stella context review"), "{err}");
}

#[test]
fn a_malformed_proposal_file_does_not_hide_the_readable_ones() {
    let root = workspace();
    std::fs::write(
        root.path().join(PROPOSALS_DIR).join("broken.toml"),
        "schema = \"context-record/v0.1\"\n[[proposal]\n",
    )
    .unwrap();
    assert_eq!(
        read_proposals(root.path()).len(),
        3,
        "the good file still loads"
    );
}

// #885 — the review surface

#[test]
fn review_runs_over_a_real_proposal_set() {
    let root = workspace();
    review::run_review(root.path(), false).expect("review succeeds");
    review::run_review(root.path(), true).expect("--all succeeds");
}

#[test]
fn review_on_an_empty_workspace_points_at_ingest() {
    let root = tempfile::tempdir().unwrap();
    review::run_review(root.path(), false).expect("no proposals is not an error");
}

/// A refuted proposal shows its verdict and can be ignored in one action.
#[test]
fn a_refuted_proposal_can_be_declined_in_one_action() {
    let root = workspace();
    let proposals = read_proposals(root.path());
    let refuted = resolve_candidate(&proposals, "node-version").unwrap();
    assert_eq!(
        refuted
            .proposal
            .refutation
            .as_ref()
            .map(|r| r.verdict.as_str()),
        Some("refuted"),
        "the verdict is on the proposal, so review can lead with it"
    );

    review::run_ignore(
        root.path(),
        "node-version",
        Some("the repo moved to 22".to_string()),
        None,
    )
    .expect("ignore succeeds");

    let states = decision::fold(&read_decisions(root.path()));
    let state = &states["node-version-2222bbbb"];
    assert_eq!(state.decision, Decision::Ignore);
    assert!(
        state.cooldown_until.is_some(),
        "a decline without a cooldown is a decline that does nothing"
    );
}

/// A declined proposal is not re-proposed on the next ingest of the same source.
#[test]
fn a_declined_claim_is_withheld_from_the_next_ingest() {
    let root = workspace();
    review::run_ignore(root.path(), "node-version", None, None).expect("ignore succeeds");
    let states = decision::fold(&read_decisions(root.path()));
    assert!(
        !stella_core::records::should_repropose(
            &states,
            "node-version-2222bbbb",
            "2026-08-01T00:00:00Z"
        ),
        "this is the fact `stella ingest` consults before offering a claim"
    );
    assert!(
        stella_core::records::should_repropose(
            &states,
            "node-version-2222bbbb",
            "2027-01-01T00:00:00Z"
        ),
        "and the cooldown does lapse"
    );
}

#[test]
fn an_unparseable_cooldown_is_refused_before_anything_is_recorded() {
    let root = workspace();
    let err = review::run_ignore(root.path(), "node-version", None, Some("3 months")).unwrap_err();
    assert!(err.contains("ISO-8601"), "{err}");
    assert!(
        read_decisions(root.path()).is_empty(),
        "a rejected command must not half-record its decision"
    );
}

// #886 — publication, and the record reaching a frame

/// A kept record is selected into a compiled frame, cited by handle.
#[test]
fn keeping_a_proposal_publishes_a_record_the_engine_loads_and_cites() {
    let root = workspace();
    review::run_keep(root.path(), "pkg-manager", None, false).expect("keep succeeds");

    let published = root
        .path()
        .join(RULES_DIR)
        .join("ctx.acme.web.pkg-manager.toml");
    assert!(published.exists(), "keep writes a Git-tracked record file");

    let registry = load_registry(root.path());
    let entry = registry.by_handle("pkg-manager").expect("the record loads");
    assert_eq!(
        entry.record.record.statement,
        "This repository uses pnpm exclusively."
    );
    let cached = registry.render(Channel::Cached, None);
    assert!(
        cached.text.contains("^pkg-manager"),
        "it must reach the model with a handle it can cite: {}",
        cached.text
    );
    assert!(
        entry.record.findings.is_empty(),
        "a freshly published record must verify on load: {:?}",
        entry.record.findings
    );
}

#[test]
fn a_published_records_hash_verifies_on_load() {
    let root = workspace();
    review::run_keep(root.path(), "pkg-manager", None, false).expect("keep succeeds");
    let registry = load_registry(root.path());
    let entry = registry.by_handle("pkg-manager").unwrap();
    assert!(
        !entry
            .record
            .findings
            .iter()
            .any(|f| matches!(f, stella_core::records::RecordFinding::HashMismatch { .. })),
        "publication must write a hash that recomputes: {:?}",
        entry.record.findings
    );
    assert!(entry.record.record.record_hash.is_some());
}

#[test]
fn keep_refuses_to_overwrite_an_existing_record() {
    let root = workspace();
    review::run_keep(root.path(), "pkg-manager", None, false).expect("first keep succeeds");
    let err = review::run_keep(root.path(), "pkg-manager", None, false).unwrap_err();
    assert!(err.contains("already exists"), "{err}");
    assert!(
        err.contains("reviewed work"),
        "the message must say why refusing is the right answer: {err}"
    );
}

#[test]
fn edit_publishes_the_reviewers_wording_under_the_same_lineage() {
    let root = workspace();
    review::run_keep(
        root.path(),
        "pkg-manager",
        Some("This repository uses pnpm; npm and yarn are not used."),
        false,
    )
    .expect("edit succeeds");

    let registry = load_registry(root.path());
    let entry = registry.by_handle("pkg-manager").expect("loads");
    assert_eq!(
        entry.record.record.statement,
        "This repository uses pnpm; npm and yarn are not used."
    );
    assert_eq!(
        entry.record.record.lineage_id, "ctx.acme.web.pkg-manager",
        "an edit is a revision of the same claim, not a new claim"
    );
    let states = decision::fold(&read_decisions(root.path()));
    assert_eq!(states["pkg-manager-1111aaaa"].decision, Decision::Edit);
}

#[test]
fn an_empty_edit_statement_is_refused() {
    let root = workspace();
    assert!(review::run_keep(root.path(), "pkg-manager", Some("   "), false).is_err());
}

#[test]
fn a_gate_dismissed_proposal_cannot_be_published_and_says_what_to_do() {
    let root = workspace();
    let err = review::run_keep(root.path(), "compound", None, false).unwrap_err();
    assert!(err.contains("compound_claim"), "{err}");
    assert!(
        err.contains("split"),
        "the refusal must name the remedy: {err}"
    );
    assert!(
        !root
            .path()
            .join(RULES_DIR)
            .join("ctx.acme.web.deploys.toml")
            .exists()
    );
}

// #888 — the refuted claim never reaches the prompt

#[test]
fn keeping_a_refuted_claim_still_leaves_it_out_of_the_prompt() {
    let root = workspace();
    // A reviewer can keep it — the record is a claim somebody stands behind — but the
    // truth sweep is what decides whether it steers, and `.nvmrc` says 22.
    review::run_keep(root.path(), "node-version", None, false).expect("keep succeeds");
    let registry = load_registry(root.path());
    let entry = registry.by_handle("node-version").expect("it loads");
    assert!(
        !entry.disposition.is_selected(),
        "the whole reason the truth axis exists: {:?}",
        entry.disposition
    );
    assert!(
        !registry
            .render(Channel::Cached, None)
            .text
            .contains("Node 20"),
        "it must not be taught to the agent on every turn"
    );
}

/// The epic's own acceptance criterion: a revert removes the rule from all
/// subsequent selection (§17).
///
/// Worth pinning even though it looks like it needs no code: the whole design rests
/// on Git being the authority and the loader deriving from it, so "reverting the
/// publishing commit un-publishes the record" must be true *by construction* rather
/// than by a cleanup path somebody could forget to call. If a cache or a database
/// row ever outlived the file, this is the test that would notice.
#[test]
fn reverting_the_publication_removes_the_record_from_selection() {
    let root = workspace();
    review::run_keep(root.path(), "pkg-manager", None, false).expect("keep succeeds");
    let published = root
        .path()
        .join(RULES_DIR)
        .join("ctx.acme.web.pkg-manager.toml");
    assert!(
        load_registry(root.path())
            .by_handle("pkg-manager")
            .is_some()
    );

    // What `git revert` of the publishing commit does to the tree.
    std::fs::remove_file(&published).unwrap();

    let registry = load_registry(root.path());
    assert!(
        registry.by_handle("pkg-manager").is_none(),
        "the record must leave selection with the file"
    );
    assert!(
        registry.render(Channel::Cached, None).text.is_empty(),
        "and must not linger in the prompt from a cache"
    );
    // The decision log deliberately still remembers the keep — history is not
    // rewritten by a revert; the record simply stops being loaded.
    assert!(
        !read_decisions(root.path()).is_empty(),
        "a revert un-publishes; it does not erase that the decision was made"
    );
}

/// The solo path end to end, in a real repository: `--commit` makes the branch and
/// the ordinary local commit §5.1 describes ("add rule to this repository", not
/// "open a pull request").
#[test]
fn propose_commit_makes_a_branch_and_a_local_commit() {
    let root = workspace();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(root.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t.t"]);
    git(&["config", "user.name", "t"]);
    git(&["remote", "add", "origin", "git@github.com:acme/web.git"]);
    std::fs::write(
        root.path().join("README.md"),
        "base
",
    )
    .unwrap();
    git(&["add", "README.md"]);
    git(&["commit", "-q", "-m", "base"]);

    review::run_keep(root.path(), "pkg-manager", None, false).expect("keep succeeds");
    propose::run_propose(root.path(), "pkg-manager", true).expect("propose --commit succeeds");

    assert_eq!(
        git(&["rev-parse", "--abbrev-ref", "HEAD"]),
        "stella/context/pkg-manager",
        "the branch is named after the handle, so it reads as what it changes"
    );
    let message = git(&["log", "-1", "--pretty=%B"]);
    assert!(
        message.starts_with("context: this repository uses pnpm"),
        "{message}"
    );
    assert!(
        message.contains("## Proposed steering") && message.contains("## Expected runtime effect"),
        "the §8.2 body travels with the commit: {message}"
    );
    let changed = git(&["show", "--name-only", "--pretty=format:", "HEAD"]);
    assert_eq!(
        changed.lines().filter(|l| !l.trim().is_empty()).count(),
        1,
        "the diff touches exactly one rule file (§17): {changed}"
    );

    // A second run must not clobber the branch it already made.
    let err = propose::run_propose(root.path(), "pkg-manager", true).unwrap_err();
    assert!(err.contains("already exists"), "{err}");
}

// #892 — explain

#[test]
fn explain_reproduces_a_kept_records_rule_and_provenance() {
    let root = workspace();
    review::run_keep(root.path(), "pkg-manager", None, false).expect("keep succeeds");
    explain::run_explain(root.path(), "^pkg-manager").expect("explain by caret handle");
    explain::run_explain(root.path(), "pkg-manager").expect("explain by bare handle");
    explain::run_explain(root.path(), "ctx.acme.web.pkg-manager").expect("explain by lineage");
}

#[test]
fn explain_on_an_unknown_rule_lists_what_is_loaded() {
    let root = workspace();
    review::run_keep(root.path(), "pkg-manager", None, false).expect("keep succeeds");
    let err = explain::run_explain(root.path(), "nope").unwrap_err();
    assert!(err.contains("^pkg-manager"), "{err}");
}

// #889 — validate

#[test]
fn validate_passes_a_clean_workspace_and_fails_a_refuted_one() {
    let root = workspace();
    review::run_keep(root.path(), "pkg-manager", None, false).expect("keep succeeds");
    validate::run_validate(root.path(), false).expect("a supported record validates");
    validate::run_validate(root.path(), true).expect("and in json");

    // Publishing the claim the tree refutes must make validation fail, so it can be
    // a CI check rather than a report nobody acts on.
    review::run_keep(root.path(), "node-version", None, false).expect("keep succeeds");
    let err = validate::run_validate(root.path(), false).unwrap_err();
    assert!(err.contains("must not steer"), "{err}");
}

/// `list`'s channel column reports where a record actually lands.
///
/// It used to read the declared `force` and print "cached" for a `must` record the
/// truth sweep had dropped — the line above the reason saying it is in the prompt,
/// the line below saying why it is not.
#[test]
fn list_does_not_label_a_dropped_record_as_being_in_a_channel() {
    let root = workspace();
    review::run_keep(root.path(), "node-version", None, false).expect("keep succeeds");
    let registry = load_registry(root.path());
    let entry = registry.by_handle("node-version").expect("loaded");
    assert!(!entry.disposition.is_selected(), "the probe refutes it");
    assert_eq!(
        entry
            .record
            .record
            .steering
            .as_ref()
            .map(|steering| steering.force.as_str()),
        Some("must"),
        "and it still declares `must` — which is exactly the trap"
    );
    validate::run_list(root.path(), false).expect("list succeeds");
}

#[test]
fn list_reports_what_steers_and_survives_an_empty_workspace() {
    let root = workspace();
    validate::run_list(root.path(), false).expect("empty list is fine");
    review::run_keep(root.path(), "pkg-manager", None, false).expect("keep succeeds");
    validate::run_list(root.path(), false).expect("list succeeds");
    validate::run_list(root.path(), true).expect("list --json succeeds");
}

// #894 — propose

#[test]
fn propose_prints_a_plan_without_creating_anything() {
    let root = workspace();
    review::run_keep(root.path(), "pkg-manager", None, false).expect("keep succeeds");
    // No git remote in a tempdir, so the repository precondition fails — and the
    // §8.1 contract is that a failed precondition creates nothing.
    let err = propose::run_propose(root.path(), "pkg-manager", false).unwrap_err();
    assert!(err.contains("preconditions not met"), "{err}");
    assert!(
        !root.path().join(".git").exists(),
        "a failed precondition must not leave a branch behind"
    );
}

#[test]
fn propose_refuses_a_personal_record() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(RULES_DIR)).unwrap();
    std::fs::write(
        root.path().join(RULES_DIR).join("ctx.me.style.toml"),
        r#"
schema = "context-record/v0.1"
set_id = "me"

[defaults]
sharing_scope = "personal"
origin = "user"
status = "active"

[[record]]
lineage_id = "ctx.me.style"
kind = "preference"
statement = "I prefer terse commit messages."

[record.steering]
force = "may"
"#,
    )
    .unwrap();
    let err = propose::run_propose(root.path(), "style", false).unwrap_err();
    assert!(err.contains("personal"), "{err}");
    assert!(
        err.contains("§10"),
        "the refusal must cite the privacy boundary it is enforcing: {err}"
    );
}

#[test]
fn propose_on_an_unknown_rule_points_at_list() {
    let root = tempfile::tempdir().unwrap();
    let err = propose::run_propose(root.path(), "nope", false).unwrap_err();
    assert!(err.contains("stella context list"), "{err}");
}

// The verdict label is the one piece of shared presentation, and getting it wrong
// would color a refutation as good news.

#[test]
fn verdict_labels_do_not_dress_up_a_refutation() {
    assert!(verdict_label("refuted").to_string().contains("refuted"));
    assert!(verdict_label("supported").to_string().contains("supported"));
    assert!(
        verdict_label("anything-else")
            .to_string()
            .contains("unfalsifiable"),
        "an unrecognized verdict must read as unverified, never as supported"
    );
}

// ── The regulated governance tier (#994) ─────────────────────────────────

/// A project record that declares a hard guard — the shape that needs a
/// ledger grant to arm anywhere outside `~/.stella/rules`.
fn write_guarded_project_record(root: &Path, lineage: &str) {
    let dir = root.join(RULES_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{lineage}.toml")),
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
guard_deny_command = "git push --force*"
"#
        ),
    )
    .unwrap();
}

/// The #994 acceptance, end to end: a promotion to blocking in regulated
/// mode carries approver + reason, is replayable from the hash-chained
/// ledger, and ARMS the guard on the next registry load.
#[test]
fn a_ledger_promotion_is_replayable_and_arms_the_guard() {
    let root = tempfile::tempdir().unwrap();
    let lineage = "ctx.acme.web.no-force-push";
    write_guarded_project_record(root.path(), lineage);
    crate::context_records::write_governance(
        root.path(),
        &stella_core::records::promotion::Governance {
            mode: stella_core::records::promotion::GovernanceMode::Regulated,
            separation: false,
        },
    )
    .unwrap();

    // Before the grant: the guard must not be armed.
    let before = load_registry(root.path());
    assert!(
        !before.entries[0].is_enforced(),
        "a project guard must not arm without a grant"
    );

    govern::run_promote(
        root.path(),
        lineage,
        "blocking",
        "measured advisory precision over 30 days",
        Some("lead@example.test"),
    )
    .unwrap();

    let events = crate::context_records::read_promotions(root.path()).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].approver, "lead@example.test");
    assert_eq!(events[0].reason, "measured advisory precision over 30 days");
    assert_eq!(events[0].mode, "regulated");
    assert_eq!(
        stella_core::records::promotion::policy_version(&events),
        1,
        "the promotion created policy version 1"
    );

    let after = load_registry(root.path());
    assert!(
        after.entries[0].is_enforced(),
        "the ledger grant arms the guard on the next load"
    );
}

/// Separation (#994 acceptance b): the record's author cannot approve its
/// own enforcement grant; a different identity can.
#[test]
fn separation_refuses_the_authors_own_grant() {
    let root = tempfile::tempdir().unwrap();
    let lineage = "ctx.acme.web.no-force-push";
    write_guarded_project_record(root.path(), lineage);
    // Record authorship in the decision ledger, as `stella context keep`
    // would have on the author's machine.
    let event = stella_core::records::decision::DecisionEvent::keep(
        "no-force-push-1234abcd",
        lineage,
        "author@example.test",
        "2026-08-01T00:00:00Z",
        format!("{RULES_DIR}/{lineage}.toml"),
    );
    crate::context_records::append_decision(root.path(), &event).unwrap();
    crate::context_records::write_governance(
        root.path(),
        &stella_core::records::promotion::Governance {
            mode: stella_core::records::promotion::GovernanceMode::Regulated,
            separation: true,
        },
    )
    .unwrap();

    let refused = govern::run_promote(
        root.path(),
        lineage,
        "blocking",
        "trying to self-approve",
        Some("author@example.test"),
    )
    .unwrap_err();
    assert!(
        refused.contains("cannot approve its own enforcement"),
        "got: {refused}"
    );

    govern::run_promote(
        root.path(),
        lineage,
        "blocking",
        "reviewed by a second pair of eyes",
        Some("lead@example.test"),
    )
    .unwrap();
    let events = crate::context_records::read_promotions(root.path()).unwrap();
    assert_eq!(
        events[0].proposer.as_deref(),
        Some("author@example.test"),
        "the ledger records who authored what was granted"
    );
}

/// #994 acceptance (c): validate fails on a hash mismatch — an edited grant
/// is a governance failure regardless of what the records say.
#[test]
fn a_tampered_promotion_ledger_fails_validate() {
    let root = tempfile::tempdir().unwrap();
    let lineage = "ctx.acme.web.no-force-push";
    write_guarded_project_record(root.path(), lineage);
    govern::run_promote(
        root.path(),
        lineage,
        "blocking",
        "measured advisory precision",
        Some("lead@example.test"),
    )
    .unwrap();

    // A second event, so the first has a successor whose `prev` pins it — a
    // single-line ledger's head is only anchored by git history, which is
    // exactly why the ledger is committed rather than private.
    govern::run_promote(
        root.path(),
        lineage,
        "advisory",
        "demoting while we investigate",
        Some("lead@example.test"),
    )
    .unwrap();
    let path = root.path().join(crate::context_records::PROMOTION_LEDGER);
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("measured advisory precision", "edited")).unwrap();

    let err = validate::run_validate(root.path(), false).unwrap_err();
    assert!(
        err.contains("promotion ledger failed verification"),
        "got: {err}"
    );
    // And the tampered ledger grants NOTHING: enforcement fails closed.
    let registry = load_registry(root.path());
    assert!(
        !registry.entries[0].is_enforced(),
        "a tampered ledger must not arm anything"
    );
}

/// §11: blocking needs a real enforcer — a record with no guard keys cannot
/// be promoted to blocking, whatever the approvals say.
#[test]
fn blocking_without_guard_keys_is_refused() {
    let root = workspace();
    review::run_keep(root.path(), "pkg-manager", None, false).unwrap();
    let refused = govern::run_promote(
        root.path(),
        "ctx.acme.web.pkg-manager",
        "blocking",
        "no guard exists though",
        Some("lead@example.test"),
    )
    .unwrap_err();
    assert!(refused.contains("no guard keys"), "got: {refused}");
}
