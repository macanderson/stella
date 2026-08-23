//! The review surface, end to end — every acceptance criterion the epic states,
//! exercised through the commands a person actually runs.

use super::*;
use stella_core::records::{Channel, Decision, decision};

use crate::context_records::{PROPOSALS_DIR, RULES_DIR, load_registry, read_decisions};
use crate::query_format::QueryFormat;

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

/// Archive a published record in place, the shape `stella ingest --refresh` writes:
/// a new revision with `status = "archived"` that supersedes the one it replaces,
/// re-stamped so the file still verifies on load
/// (`crate::ingest_cmd::refresh::retire`). Written out here rather than driven
/// through `refresh` because this test is about the *reading* half; the handshake
/// between the two commands has its own witness in that module's tests.
fn archive_in_place(root: &Path, lineage_id: &str) {
    let path = root.join(RULES_DIR).join(format!("{lineage_id}.toml"));
    let contents = std::fs::read_to_string(&path).expect("published");
    let mut file: ContextFile = toml::from_str(&contents).expect("parses");
    let defaults = file.defaults.clone().unwrap_or_default();
    for record in &mut file.records {
        record.status = Some(stella_core::context_record::RecordStatus::Archived);
        record.supersedes_record_id = record.record_id.clone();
        record.stamp(&defaults).expect("re-stamps");
    }
    std::fs::write(&path, toml::to_string_pretty(&file).expect("serializes")).expect("writes");
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

/// The other fork of an existing publication (#2708): a *different* claim at
/// the same lineage supersedes rather than refuses. The new revision replaces
/// the file, cites the revision it retires, and the ledger records why.
#[test]
fn keeping_a_changed_claim_supersedes_the_published_revision() {
    let root = workspace();
    review::run_keep(root.path(), "pkg-manager", None, false).expect("first keep succeeds");
    let published = root
        .path()
        .join(".stella/rules/ctx.acme.web.pkg-manager.toml");
    let old: stella_core::ingest::ContextFile =
        toml::from_str(&std::fs::read_to_string(&published).expect("readable")).expect("parses");
    let old_id = old.records[0].record_id.clone().expect("stamped");

    review::run_keep(
        root.path(),
        "pkg-manager",
        Some("This repository uses pnpm; npm and yarn are not used."),
        false,
    )
    .expect("a changed claim supersedes");

    let new: stella_core::ingest::ContextFile =
        toml::from_str(&std::fs::read_to_string(&published).expect("readable")).expect("parses");
    let record = &new.records[0];
    assert_eq!(
        record.statement,
        "This repository uses pnpm; npm and yarn are not used."
    );
    assert_eq!(
        record.supersedes_record_id.as_deref(),
        Some(old_id.as_str())
    );
    assert_ne!(
        record.record_id.as_deref(),
        Some(old_id.as_str()),
        "a superseding revision has its own identity"
    );
    assert_eq!(
        new.records.len(),
        1,
        "the file carries exactly the live revision — selection can never see both"
    );

    let last = read_decisions(root.path())
        .into_iter()
        .next_back()
        .expect("a decision was appended");
    assert_eq!(
        last.reason.as_deref(),
        Some(format!("supersedes {old_id}").as_str()),
        "the ledger must record the supersession, not just the file diff"
    );

    // And the accountable promotion-ledger event (spec §4, #2728), chained
    // and naming both revisions.
    let events = crate::context_records::read_promotions(root.path()).expect("the chain verifies");
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].action,
        stella_core::records::promotion::LedgerAction::Superseded
    );
    assert!(events[0].reason.contains(&old_id), "{}", events[0].reason);
    assert!(
        events[0]
            .reason
            .contains(record.record_id.as_deref().expect("stamped")),
        "{}",
        events[0].reason
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

/// The title is the statement's first sentence, and records name files —
/// so the sentence terminator cannot be "any period". This repository's own
/// first Context PR was titled `… the license allow-list in \`deny` before the
/// fix, which reads as a different claim rather than a shortened one.
#[test]
fn a_title_is_not_cut_at_a_dot_inside_a_file_name() {
    assert_eq!(
        propose::first_clause(
            "A dependency that `cargo deny` rejects must be dropped rather than admitted by \
             widening the license allow-list in `deny.toml`."
        ),
        "A dependency that `cargo deny` rejects must be dropped rather than admitted by widening \
         the license allow-list in `deny.toml`",
        "a period inside `deny.toml` is not a sentence break; the trailing one is"
    );
    assert_eq!(
        propose::first_clause("This repository uses pnpm exclusively; npm must not be used."),
        "This repository uses pnpm exclusively",
        "a real clause break still ends the title"
    );
    assert_eq!(
        propose::first_clause("Pin the toolchain in rust-toolchain.toml"),
        "Pin the toolchain in rust-toolchain.toml",
        "a statement with no terminator at all is returned whole"
    );
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
    validate::run_validate(root.path(), QueryFormat::Text).expect("a supported record validates");
    validate::run_validate(root.path(), QueryFormat::Json).expect("and in json");

    // Publishing the claim the tree refutes must make validation fail, so it can be
    // a CI check rather than a report nobody acts on.
    review::run_keep(root.path(), "node-version", None, false).expect("keep succeeds");
    let err = validate::run_validate(root.path(), QueryFormat::Text).unwrap_err();
    assert!(err.contains("must not steer"), "{err}");
}

/// #3254: the retirement exemption is for the lifecycle status alone.
///
/// `validate` stops counting a record that is out of selection **because somebody
/// retired it**, which is what lets a completed retirement end green. The half that
/// keeps this a check is here: a refuted claim beside it must still fail, and the
/// count must name only it — an exemption that also swallowed the refutation would
/// turn the whole command into a report nobody acts on.
#[test]
fn an_archived_record_does_not_block_but_a_refuted_one_still_does() {
    let root = workspace();
    review::run_keep(root.path(), "pkg-manager", None, false).expect("keep succeeds");
    archive_in_place(root.path(), "ctx.acme.web.pkg-manager");
    validate::run_validate(root.path(), QueryFormat::Text)
        .expect("a retired revision does not fail the check");
    validate::run_validate(root.path(), QueryFormat::Json).expect("and in json");

    review::run_keep(root.path(), "node-version", None, false).expect("keep succeeds");
    let err = validate::run_validate(root.path(), QueryFormat::Text).unwrap_err();
    assert_eq!(
        err, "1 record(s) must not steer anything until this is resolved",
        "exactly one — the refuted claim, not the retired one"
    );
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
    validate::run_list(root.path(), QueryFormat::Text).expect("list succeeds");
}

#[test]
fn list_reports_what_steers_and_survives_an_empty_workspace() {
    let root = workspace();
    validate::run_list(root.path(), QueryFormat::Text).expect("empty list is fine");
    review::run_keep(root.path(), "pkg-manager", None, false).expect("keep succeeds");
    validate::run_list(root.path(), QueryFormat::Text).expect("list succeeds");
    validate::run_list(root.path(), QueryFormat::Json).expect("list --format json succeeds");
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

    let err = validate::run_validate(root.path(), QueryFormat::Text).unwrap_err();
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

// #4264 — amending a published record's scope

/// The published record's steering, read straight off the file.
fn published_steering(root: &Path, lineage: &str) -> stella_core::ingest::record::Steering {
    let path = root.join(RULES_DIR).join(format!("{lineage}.toml"));
    let file: stella_core::ingest::ContextFile =
        toml::from_str(&std::fs::read_to_string(&path).expect("readable")).expect("parses");
    file.records[0]
        .steering
        .clone()
        .expect("a steering section")
}

/// **Witness (#4264).** A published record's precedence can be changed through
/// the CLI, and the file that comes back verifies its own identity.
///
/// Fails on base twice over: there was no `amend` verb at all, so the only
/// route was a hand-edit — and a hand-edit strands `record_hash`, because
/// `Record::stamp` is a two-pass hash over an RFC 8785 preimage and every
/// other `stamp()` call site is a publish path. Scope is exactly the field
/// that needs amending after publication: `read-crate-readme-first` was
/// published with `paths = ["crates"]` and suspended eight invariant records
/// across the whole crate tree.
#[test]
fn amending_precedence_republishes_a_record_that_verifies() {
    let root = workspace();
    review::run_keep(root.path(), "pkg-manager", None, false).expect("publish");
    let lineage = "ctx.acme.web.pkg-manager";
    assert_eq!(
        published_steering(root.path(), lineage).precedence,
        Some(60),
        "the control: the proposal published at 60"
    );

    amend::run_amend(
        root.path(),
        "pkg-manager",
        &amend::Amendment {
            precedence: Some(20),
            ..amend::Amendment::default()
        },
    )
    .expect("amend succeeds");

    assert_eq!(
        published_steering(root.path(), lineage).precedence,
        Some(20),
        "the change reaches the published file"
    );

    let registry = load_registry(root.path());
    let entry = registry.by_handle("pkg-manager").expect("still loaded");
    assert!(
        entry.record.findings.iter().all(|finding| !matches!(
            finding,
            stella_core::records::RecordFinding::HashMismatch { .. }
                | stella_core::records::RecordFinding::IdentityStamped
        )),
        "the amended file must carry an identity that recomputes: {:?}",
        entry.record.findings
    );
    validate::run_validate(root.path(), QueryFormat::Text).expect("and it validates");
}

/// Scope is replaced, not appended: the common repair is narrowing a record
/// that matched too much, which an append could not express.
#[test]
fn amending_paths_replaces_the_scope_rather_than_widening_it() {
    let root = workspace();
    review::run_keep(root.path(), "pkg-manager", None, false).expect("publish");
    let lineage = "ctx.acme.web.pkg-manager";

    amend::run_amend(
        root.path(),
        "^pkg-manager",
        &amend::Amendment {
            paths: Some(vec!["apps/web/**".to_string()]),
            ..amend::Amendment::default()
        },
    )
    .expect("the caret is optional");
    amend::run_amend(
        root.path(),
        lineage,
        &amend::Amendment {
            paths: Some(vec!["apps/web/package.json".to_string()]),
            ..amend::Amendment::default()
        },
    )
    .expect("and the lineage id resolves too");

    let applies = published_steering(root.path(), lineage)
        .applies_to
        .expect("scoped now");
    assert_eq!(
        applies.paths,
        vec!["apps/web/package.json".to_string()],
        "the second amendment narrows rather than accumulating"
    );

    // Clap cannot express "this flag was given no values", so the empty string
    // is how a caller unscopes. A record scoped to `""` would match nothing
    // while reading as scoped, which is the worse of the two answers.
    amend::run_amend(
        root.path(),
        lineage,
        &amend::Amendment {
            paths: Some(vec![String::new()]),
            ..amend::Amendment::default()
        },
    )
    .expect("clearing is an amendment too");
    assert!(
        published_steering(root.path(), lineage)
            .applies_to
            .expect("the section survives")
            .paths
            .is_empty(),
        "`--paths ''` clears rather than scoping to the empty string"
    );
}

/// A bare amend re-stamps, which is the whole repair for a file somebody
/// already hand-edited — the case that has no other remedy at all.
#[test]
fn a_bare_amend_restamps_a_hand_edited_record() {
    let root = workspace();
    review::run_keep(root.path(), "pkg-manager", None, false).expect("publish");
    let lineage = "ctx.acme.web.pkg-manager";
    let path = root.path().join(RULES_DIR).join(format!("{lineage}.toml"));

    // The hand-edit the issue describes: a real change, and a stored hash that
    // now describes bytes which no longer exist.
    let edited = std::fs::read_to_string(&path)
        .expect("readable")
        .replace("precedence = 60", "precedence = 10");
    std::fs::write(&path, edited).expect("hand-edit");
    let stranded = load_registry(root.path());
    assert!(
        stranded
            .by_handle("pkg-manager")
            .expect("still loaded")
            .record
            .findings
            .iter()
            .any(|f| matches!(f, stella_core::records::RecordFinding::HashMismatch { .. })),
        "the control: a hand-edit strands the stored hash"
    );

    amend::run_amend(root.path(), "pkg-manager", &amend::Amendment::default())
        .expect("a bare amend re-stamps");

    let repaired = load_registry(root.path());
    assert!(
        repaired
            .by_handle("pkg-manager")
            .expect("still loaded")
            .record
            .findings
            .is_empty(),
        "and nothing is left to report: {:?}",
        repaired.by_handle("pkg-manager").unwrap().record.findings
    );
    assert_eq!(
        published_steering(root.path(), lineage).precedence,
        Some(10),
        "the hand-editor's change is kept — this repairs the identity, not the edit"
    );
}

/// A name nothing publishes is refused with the names that are, rather than
/// silently doing nothing.
#[test]
fn amending_an_unknown_record_names_what_is_published() {
    let root = workspace();
    review::run_keep(root.path(), "pkg-manager", None, false).expect("publish");
    let err =
        amend::run_amend(root.path(), "no-such-rule", &amend::Amendment::default()).unwrap_err();
    assert!(err.contains("no record named no-such-rule"), "{err}");
    assert!(err.contains("^pkg-manager"), "and names one that is: {err}");
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
