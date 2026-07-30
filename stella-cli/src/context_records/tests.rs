//! The I/O half, against real files.

use super::*;
use stella_core::records::{Channel, Decision};

/// A proposal file as `stella ingest` writes one.
fn write_proposal_file(root: &Path, lineage: &str, statement: &str, probe_path: &str) {
    let dir = root.join(PROPOSALS_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("claude-md.toml"),
        format!(
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

[[proposal]]
candidate_id = "node-version-9f3c1a2b"
proposal_kind = "knowledge"
status = "eligible"
confidence = 90
observed_at = "2026-07-20T18:30:00Z"

[proposal.record]
lineage_id = "{lineage}"
kind = "fact"
statement = "{statement}"

[proposal.record.steering]
force = "must"
precedence = 50

[proposal.record.truth]
basis = "measured"
review_every = "P30D"

[proposal.record.truth.probe]
kind = "file_contains"
path = "{probe_path}"
pattern = "20"
"#
        ),
    )
    .unwrap();
}

/// A published record with a `file_contains` probe against `.nvmrc`.
fn write_published_record(root: &Path, lineage: &str, pattern: &str) {
    let dir = root.join(RULES_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{lineage}.toml")),
        format!(
            r#"
schema = "context-record/v0.1"
set_id = "acme.web"

[defaults]
origin = "user"
status = "active"

[[record]]
lineage_id = "{lineage}"
kind = "fact"
statement = "Development runs on Node 20."

[record.steering]
force = "must"
precedence = 50

[record.truth]
basis = "measured"
review_every = "P30D"

[record.truth.probe]
kind = "file_contains"
path = ".nvmrc"
pattern = "{pattern}"
"#
        ),
    )
    .unwrap();
}

// The sweep

#[test]
fn a_never_checked_record_is_probed_and_its_verdict_cached() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(".nvmrc"), "22\n").unwrap();
    write_published_record(root.path(), "ctx.acme.web.node-version", "20");

    let registry = load_registry(root.path());
    let entry = registry.by_handle("node-version").expect("record loaded");
    assert!(
        !entry.disposition.is_selected(),
        "the tree says 22 and the record says 20 — it must stop steering: {:?}",
        entry.disposition
    );
    assert!(
        registry.render(Channel::Cached, None).text.is_empty(),
        "and it must not reach the prompt at all"
    );

    let cache = SweepCache::read(root.path());
    let cached = cache
        .checked
        .get("ctx.acme.web.node-version")
        .expect("the verdict is cached so the next session need not re-probe");
    assert_eq!(cached.verdict, "refuted");
    assert!(!cached.detail.is_empty(), "the reason must be recorded too");
}

#[test]
fn a_supported_record_reaches_the_prompt_with_its_handle() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(".nvmrc"), "20\n").unwrap();
    write_published_record(root.path(), "ctx.acme.web.node-version", "20");

    let registry = load_registry(root.path());
    let cached = registry.render(Channel::Cached, None);
    assert_eq!(cached.rendered, vec!["node-version"]);
    assert!(cached.text.contains("^node-version"), "{}", cached.text);
    assert_eq!(
        SweepCache::read(root.path())
            .checked
            .get("ctx.acme.web.node-version")
            .map(|entry| entry.verdict.as_str()),
        Some("supported")
    );
}

#[test]
fn a_cached_verdict_inside_the_cadence_is_reused_rather_than_re_probed() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(".nvmrc"), "20\n").unwrap();
    write_published_record(root.path(), "ctx.acme.web.node-version", "20");
    load_registry(root.path());
    let first = SweepCache::read(root.path());
    let first_checked = first.checked["ctx.acme.web.node-version"]
        .checked_at
        .clone();

    // Change the tree so a fresh probe would say something different. Inside the
    // P30D cadence the sweep must NOT re-run — that is what keeps a filesystem scan
    // out of every session start.
    std::fs::write(root.path().join(".nvmrc"), "22\n").unwrap();
    let registry = load_registry(root.path());
    let second = SweepCache::read(root.path());
    assert_eq!(
        second.checked["ctx.acme.web.node-version"].verdict, "supported",
        "the cached verdict stands until the record's own cadence says otherwise"
    );
    assert_eq!(
        second.checked["ctx.acme.web.node-version"].checked_at, first_checked,
        "and the clock does not move"
    );
    assert_eq!(
        registry.render(Channel::Cached, None).rendered,
        vec!["node-version"]
    );
}

#[test]
fn probe_everything_ignores_the_cadence_and_does_not_move_the_scheduled_clock() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(".nvmrc"), "20\n").unwrap();
    write_published_record(root.path(), "ctx.acme.web.node-version", "20");
    load_registry(root.path());

    std::fs::write(root.path().join(".nvmrc"), "22\n").unwrap();
    let files = rule_files(root.path(), false, true);
    let fresh = probe_everything(root.path(), &files.all(), "2026-07-20T00:00:00Z");
    assert_eq!(
        fresh.checked["ctx.acme.web.node-version"].verdict, "refuted",
        "validate must tell the truth about now, not about the last scheduled sweep"
    );
    assert_eq!(
        SweepCache::read(root.path()).checked["ctx.acme.web.node-version"].verdict,
        "supported",
        "and reporting on demand must not silence a record that is genuinely due next time"
    );
}

#[test]
fn a_workspace_with_no_records_loads_and_writes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let registry = load_registry(root.path());
    assert!(registry.entries.is_empty());
    assert!(
        !root.path().join(SWEEP_CACHE).exists(),
        "loading rules must not create private state as a side effect"
    );
}

// The decision ledger

#[test]
fn decisions_are_append_only_and_replay_to_the_same_state() {
    let root = tempfile::tempdir().unwrap();
    let first = DecisionEvent::ignore(
        "cand-1",
        "ctx.a.b.c",
        "mac",
        "2026-07-20T00:00:00Z",
        Some("not our convention".to_string()),
        None,
    );
    let second = DecisionEvent::keep(
        "cand-2",
        "ctx.a.b.d",
        "mac",
        "2026-07-21T00:00:00Z",
        ".stella/rules/ctx.a.b.d.toml",
    );
    append_decision(root.path(), &first).unwrap();
    append_decision(root.path(), &second).unwrap();

    let read_back = read_decisions(root.path());
    assert_eq!(read_back, vec![first.clone(), second.clone()]);
    let states = stella_core::records::decision::fold(&read_back);
    assert_eq!(states["cand-1"].decision, Decision::Ignore);
    assert_eq!(states["cand-2"].decision, Decision::Keep);

    // Appending never rewrites: the first line is still there byte for byte.
    append_decision(root.path(), &first).unwrap();
    assert_eq!(read_decisions(root.path()).len(), 3);
}

#[test]
fn a_corrupt_line_does_not_erase_the_history_around_it() {
    let root = tempfile::tempdir().unwrap();
    let good = DecisionEvent::keep("c", "ctx.a.b.c", "mac", "2026-07-20T00:00:00Z", "f.toml");
    append_decision(root.path(), &good).unwrap();
    let path = root.path().join(DECISION_LEDGER);
    let mut raw = std::fs::read_to_string(&path).unwrap();
    raw.push_str("{ not json\n");
    std::fs::write(&path, raw).unwrap();
    append_decision(root.path(), &good).unwrap();
    assert_eq!(
        read_decisions(root.path()).len(),
        2,
        "one bad append must not take the readable decisions with it"
    );
}

#[test]
fn a_blocking_approval_is_read_back_from_the_published_path() {
    let root = tempfile::tempdir().unwrap();
    let mut event = DecisionEvent::keep(
        "cand-1",
        "ctx.acme.web.no-force-push",
        "mac",
        "2026-07-20T00:00:00Z",
        ".stella/rules/ctx.acme.web.no-force-push.toml",
    );
    event.approved_blocking = true;
    append_decision(root.path(), &event).unwrap();

    // The approval must reach the registry, so a record that asked to block and was
    // approved actually arms.
    std::fs::create_dir_all(root.path().join(RULES_DIR)).unwrap();
    std::fs::write(
        root.path()
            .join(RULES_DIR)
            .join("ctx.acme.web.no-force-push.toml"),
        r#"
schema = "context-record/v0.1"
set_id = "acme.web"

[defaults]
origin = "user"
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
"#,
    )
    .unwrap();

    let registry = load_registry(root.path());
    let entry = registry.by_handle("no-force-push").expect("loaded");
    assert!(
        entry.is_enforced(),
        "an approved, user-origin, concretely-guarded record must arm: {:?}",
        entry.guard.refusal
    );
}

#[test]
fn without_the_approval_the_same_record_does_not_arm() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(RULES_DIR)).unwrap();
    std::fs::write(
        root.path()
            .join(RULES_DIR)
            .join("ctx.a.b.no-force-push.toml"),
        r#"
schema = "context-record/v0.1"
set_id = "acme.web"

[defaults]
origin = "user"
status = "active"

[[record]]
lineage_id = "ctx.a.b.no-force-push"
kind = "constraint"
statement = "Never force-push to a shared branch."

[record.steering]
force = "must"

[record.enforcement]
mode = "hard"
guard_tool = "Bash"
guard_deny_command = "git push --force*"
"#,
    )
    .unwrap();
    let registry = load_registry(root.path());
    let entry = registry.by_handle("no-force-push").expect("loaded");
    assert!(
        !entry.is_enforced(),
        "keeping a record is not authorizing it to deny a tool call"
    );
    assert!(entry.guard.refusal.is_some());
}

#[test]
fn a_proposal_file_contributes_no_published_records() {
    let root = tempfile::tempdir().unwrap();
    write_proposal_file(
        root.path(),
        "ctx.acme.web.node-version",
        "Development runs on Node 20.",
        ".nvmrc",
    );
    // Proposals live outside the rule directories, so nothing about them can steer.
    assert!(load_registry(root.path()).entries.is_empty());
}

#[test]
fn the_published_path_maps_back_to_its_lineage() {
    assert_eq!(
        lineage_from_published_path(".stella/rules/ctx.acme.web.pkg-manager.toml").as_deref(),
        Some("ctx.acme.web.pkg-manager")
    );
    assert_eq!(lineage_from_published_path("not-a-record").as_deref(), None);
}
