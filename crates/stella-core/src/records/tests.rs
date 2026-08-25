//! Loader tests, plus the record builders the rest of the module's tests share.
//!
//! The builders are `pub(super)` on purpose: every submodule needs a plausible
//! record to test against, and a per-module copy of "how to build one" is how a test
//! suite ends up asserting against six subtly different notions of a valid record.

use super::super::context_record::kind::{Origin, RecordStatus};
use super::super::ingest::record::{
    AppliesTo, Enforcement, EnforcementMode, Force, Record, RecordKind, SharingScope, Steering,
    Tier, Truth,
};
use super::*;

/// A minimal valid record under `lineage`, unstamped.
pub(super) fn record_named(lineage: &str) -> Record {
    Record {
        lineage_id: lineage.to_string(),
        record_id: None,
        record_hash: None,
        kind: RecordKind::Rule,
        statement: "This repository uses pnpm exclusively.".to_string(),
        tags: Vec::new(),
        origin: Some(Origin::Imported),
        sharing_scope: Some(SharingScope::Repository),
        status: Some(RecordStatus::Active),
        supersedes_record_id: None,
        provenance: None,
        steering: Some(Steering {
            tier: None,
            force: Force::Must,
            precedence: Some(50),
            applies_to: None,
        }),
        enforcement: None,
        truth: None,
        links: Vec::new(),
    }
}

/// `record` with `truth` attached.
pub(super) fn with_truth(mut record: Record, truth: Truth) -> Record {
    record.truth = Some(truth);
    record
}

/// `record` at a different steering force.
/// `record` with an explicit promotion tier on top of `force` (#2709).
pub(super) fn with_tier(record: Record, force: Force, tier: Tier) -> Record {
    let mut record = with_force(record, force);
    record
        .steering
        .as_mut()
        .expect("with_force set steering")
        .tier = Some(tier);
    record
}

pub(super) fn with_force(mut record: Record, force: Force) -> Record {
    record.steering = Some(Steering {
        tier: None,
        force,
        precedence: Some(50),
        applies_to: None,
    });
    record
}

/// `record` scoped to `paths` at `precedence`.
pub(super) fn with_scope(mut record: Record, paths: &[&str], precedence: u32) -> Record {
    record.steering = Some(Steering {
        tier: None,
        force: Force::Must,
        precedence: Some(precedence),
        applies_to: Some(AppliesTo {
            paths: paths.iter().map(|path| path.to_string()).collect(),
            tasks: Vec::new(),
            keywords: Vec::new(),
        }),
    });
    record
}

/// A `hard`-mode enforcement block with the given guard fields.
pub(super) fn hard_guard(
    tool: Option<&str>,
    deny_path: Option<&str>,
    deny_command: Option<&str>,
) -> Enforcement {
    Enforcement {
        mode: EnforcementMode::Hard,
        guard_tool: tool.map(str::to_string),
        guard_deny_command: deny_command.map(str::to_string),
        guard_deny_path: deny_path.map(str::to_string),
        severity: Some("error".to_string()),
        on_violation: None,
        check: None,
        rubric: None,
    }
}

/// A [`LoadedRecord`] around `record`, stamped, with no handle yet.
pub(super) fn loaded_from(mut record: Record) -> LoadedRecord {
    record.stamp(&Defaults::default()).expect("stamps");
    LoadedRecord {
        record,
        set_id: "acme.web".to_string(),
        source: ".stella/rules/acme.web.toml".to_string(),
        trust: super::Trust::Project,
        contributed_by: None,
        handle: String::new(),
        findings: Vec::new(),
    }
}

/// A published-record file with one record, in the shape ADR 0011 ratified.
const ONE_RECORD: &str = r#"
schema = "context-record/v0.1"
set_id = "acme.web"

[defaults]
sharing_scope = "repository"
origin = "user"
status = "active"

[defaults.provenance]
source_kind = "document"
source_uri = "CLAUDE.md"
commit = "6ee3d4a"

[[record]]
lineage_id = "ctx.acme.web.pkg-manager"
kind = "rule"
statement = "This repository uses pnpm exclusively; npm and yarn must not be used."

[record.steering]
force = "must"
precedence = 60

[record.truth]
basis = "measured"
confidence = 95
"#;

#[test]
fn a_published_record_file_loads_with_defaults_merged() {
    let records = load_context_file(".stella/rules/acme.web.toml", ONE_RECORD).expect("loads");
    assert_eq!(records.len(), 1);
    let loaded = &records[0];
    assert_eq!(loaded.set_id, "acme.web");
    assert_eq!(loaded.record.origin, Some(Origin::User));
    assert_eq!(
        loaded.record.sharing_scope,
        Some(SharingScope::Repository),
        "file defaults must reach the record"
    );
    assert_eq!(
        loaded
            .record
            .provenance
            .as_ref()
            .and_then(|p| p.commit.as_deref()),
        Some("6ee3d4a"),
        "provenance is what makes a record citable with commit-level authority"
    );
}

#[test]
fn a_hand_authored_record_has_its_identity_stamped_from_content() {
    let records = load_context_file("f.toml", ONE_RECORD).expect("loads");
    let loaded = &records[0];
    assert!(
        loaded
            .record
            .record_id
            .as_deref()
            .unwrap()
            .starts_with("rec_acme_web_pkg_manager_"),
        "{:?}",
        loaded.record.record_id
    );
    assert!(
        loaded
            .record
            .record_hash
            .as_deref()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(loaded.findings, vec![RecordFinding::IdentityStamped]);
    assert_eq!(
        loaded.severity(),
        Some(Severity::Note),
        "stamping is not a problem"
    );
    assert!(loaded.is_selectable());
}

// The #886 acceptance criterion.

#[test]
fn record_hash_recomputes_to_the_stored_value_for_an_unedited_record() {
    let stamped = &load_context_file("f.toml", ONE_RECORD).expect("loads")[0];
    let hash = stamped.record.record_hash.clone().expect("stamped");
    let id = stamped.record.record_id.clone().expect("stamped");

    // Write the identity back into the file, exactly as publication does, and load
    // again. An unedited record must verify.
    let with_identity = ONE_RECORD.replace(
        "lineage_id = \"ctx.acme.web.pkg-manager\"",
        &format!(
            "lineage_id = \"ctx.acme.web.pkg-manager\"\nrecord_id = \"{id}\"\nrecord_hash = \"{hash}\""
        ),
    );
    let reloaded = &load_context_file("f.toml", &with_identity).expect("loads")[0];
    assert_eq!(reloaded.record.record_hash.as_deref(), Some(hash.as_str()));
    assert!(
        reloaded.findings.is_empty(),
        "an unedited record must load clean: {:?}",
        reloaded.findings
    );
}

#[test]
fn a_hand_edited_statement_mints_a_new_revision_rather_than_being_accepted() {
    let stamped = &load_context_file("f.toml", ONE_RECORD).expect("loads")[0];
    let stale_hash = stamped.record.record_hash.clone().expect("stamped");
    // Same stored hash, different statement — the shape of somebody editing the
    // semantics of a published record by hand.
    let edited = ONE_RECORD
        .replace(
            "lineage_id = \"ctx.acme.web.pkg-manager\"",
            &format!("lineage_id = \"ctx.acme.web.pkg-manager\"\nrecord_hash = \"{stale_hash}\""),
        )
        .replace("uses pnpm exclusively", "uses npm exclusively");

    let reloaded = &load_context_file("f.toml", &edited).expect("loads")[0];
    let mismatch = reloaded
        .findings
        .iter()
        .find_map(|finding| match finding {
            RecordFinding::HashMismatch { stored, recomputed } => Some((stored, recomputed)),
            _ => None,
        })
        .expect("the edit must be detected");
    assert_eq!(mismatch.0, &stale_hash);
    assert_ne!(
        mismatch.1, &stale_hash,
        "the new content must hash to a new identity"
    );
    assert_eq!(
        reloaded.record.record_hash.as_ref(),
        Some(mismatch.1),
        "the record adopts the recomputed hash — the stored one is never written back \
         over new content"
    );
    assert!(
        reloaded.is_selectable(),
        "a hand-edit is reported, not silently disarmed: the statement is still policy"
    );
}

#[test]
fn a_record_missing_its_source_uri_inherits_the_file_it_was_read_from() {
    let no_provenance = r#"
schema = "context-record/v0.1"
set_id = "acme.web"

[[record]]
lineage_id = "ctx.acme.web.orphan"
kind = "fact"
statement = "A statement with no declared provenance."
"#;
    let loaded =
        &load_context_file(".stella/rules/found-here.toml", no_provenance).expect("loads")[0];
    assert_eq!(
        loaded
            .record
            .provenance
            .as_ref()
            .and_then(|p| p.source_uri.as_deref()),
        Some(".stella/rules/found-here.toml"),
        "a record with no provenance must still be traceable to an authority"
    );
}

#[test]
fn a_retracted_record_is_not_selectable() {
    let retracted = ONE_RECORD.replace("status = \"active\"", "status = \"retracted\"");
    let loaded = &load_context_file("f.toml", &retracted).expect("loads")[0];
    assert!(
        !loaded.is_selectable(),
        "a retracted record must leave selection — that is what revert relies on"
    );
}

#[test]
fn a_proposal_file_yields_no_published_records() {
    let proposals = r#"
schema = "context-record/v0.1"
set_id = "acme.web"
ingest_run_id = "ing_01"

[[proposal]]
candidate_id = "pkg-manager-9f3c1a2b"
proposal_kind = "directive"
status = "eligible"
confidence = 92
observed_at = "2026-07-20T18:30:00Z"

[proposal.record]
lineage_id = "ctx.acme.web.pkg-manager"
kind = "rule"
statement = "This repository uses pnpm exclusively."
"#;
    assert!(
        load_context_file("p.toml", proposals)
            .expect("loads")
            .is_empty(),
        "a proposal is not a record until somebody keeps it"
    );
}

// Malformed input

#[test]
fn malformed_toml_is_an_error_the_caller_must_surface() {
    let err = load_context_file(
        "broken.toml",
        "schema = \"context-record/v0.1\"\n[[record]\n",
    )
    .expect_err("must not load");
    assert_eq!(err.source, "broken.toml");
    assert!(!err.detail.is_empty());
    assert!(err.to_string().starts_with("broken.toml: "));
}

#[test]
fn an_unknown_schema_tag_is_refused_rather_than_guessed_at() {
    let future = r#"
schema = "context-record/v9.9"
set_id = "acme.web"
"#;
    let err = load_context_file("f.toml", future).expect_err("must not load");
    assert!(err.detail.contains("context-record/v0.1"), "{}", err.detail);
}

// The substrate rule (#893): sharing_scope selects the location.

#[test]
fn sharing_scope_selects_which_location_a_record_is_published_to() {
    assert_eq!(
        publication_dir(SharingScope::Personal),
        PublicationDir::User
    );
    assert_eq!(
        publication_dir(SharingScope::Repository),
        PublicationDir::Repository
    );
    assert_eq!(
        publication_dir(SharingScope::Organization),
        PublicationDir::Repository,
        "an organization record is published through a repository the org owns"
    );
}

// The whole chain, in one test: load → handle → validate → sweep → render.

#[test]
fn a_kept_record_reaches_the_prompt_citably_and_a_refuted_one_does_not() {
    let file = r#"
schema = "context-record/v0.1"
set_id = "acme.web"

[defaults]
origin = "user"
status = "active"

[[record]]
lineage_id = "ctx.acme.web.pkg-manager"
kind = "rule"
statement = "This repository uses pnpm exclusively."

[record.steering]
force = "must"
precedence = 60

[[record]]
lineage_id = "ctx.acme.web.node-version"
kind = "fact"
statement = "Development runs on Node 20."

[record.steering]
force = "must"
precedence = 60

[record.truth]
basis = "measured"

[record.truth.probe]
kind = "file_contains"
path = ".nvmrc"
pattern = "20"
"#;
    let mut records = load_context_file(".stella/rules/acme.web.toml", file).expect("loads");
    assign_handles(&mut records);
    let conflicts = validate_records(&mut records);
    assert!(conflicts.is_empty(), "{conflicts:?}");
    assert_eq!(
        records
            .iter()
            .map(|r| r.handle.as_str())
            .collect::<Vec<_>>(),
        vec!["pkg-manager", "node-version"]
    );

    // The `.nvmrc` now says 22, so the node-version claim is refuted.
    let dispositions: Vec<Disposition> = records
        .iter()
        .map(|loaded| {
            let verdict = (loaded.handle == "node-version")
                .then_some(super::super::ingest::record::Verdict::Refuted);
            sweep::disposition(&SweepInput {
                record: &loaded.record,
                verdict,
                last_checked: None,
                now: "2026-07-20T00:00:00Z",
            })
        })
        .collect();
    let inputs: Vec<render::RenderInput<'_>> = records
        .iter()
        .zip(&dispositions)
        .map(|(record, disposition)| render::RenderInput {
            record,
            disposition,
            enforced: false,
        })
        .collect();

    let cached = render_channel(&inputs, Channel::Cached, None);
    assert_eq!(cached.rendered, vec!["pkg-manager"]);
    assert!(
        !cached.text.contains("Node 20"),
        "the refuted claim must not reach the model at all: {}",
        cached.text
    );
    assert!(cached.text.contains("^pkg-manager"), "{}", cached.text);
    assert!(
        render_channel(&inputs, Channel::Volatile, None)
            .text
            .is_empty(),
        "a dropped record does not reappear in the other channel"
    );
}

/// Four-class hit-discipline certification for per-turn selection.
///
/// The corpus below is written the way an adversarial reviewer would write it,
/// against one realistic terminal-bench-shaped task (repairing a corrupted git
/// repository), with one record per relevance class:
///
/// 1. **Similar but irrelevant** — the statement is drenched in the task's own
///    vocabulary (git, fsck, reflog, dangling) but its declared scope names a
///    different situation. It must NOT be selected: selection keys on declared
///    scope, never on how much the statement *sounds* like the task.
/// 2. **Dissimilar and irrelevant** — plain noise. Must not be selected.
/// 3. **Similar-situation, non-obvious relevance** — the statement shares no
///    vocabulary with the task at all; only its declared scope knows why it
///    matters here. It MUST be selected: declared scope is the signal.
/// 4. **Total match** — obviously relevant, declared for exactly this
///    situation. Must be selected, first.
///
/// The tuning direction the assertions encode: **compact** (rendered bytes are
/// invariant under injected noise), **sufficient-only** (exactly the relevant
/// handles render, nothing else), **honest under budget** (a drop is ledgered
/// as `dropped`, never silent), and **cheap** (the whole certification is pure
/// and model-free, so it runs on every `cargo test`).
mod four_class_certification {
    use super::super::registry;
    use super::super::select::TurnFacts;
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// The published corpus, in the exact artifact shape `stella context keep`
    /// publishes. All four ride the volatile channel (`force = "may"`): the
    /// cached channel is unconditional by contract and never selected.
    const CORPUS: &str = r#"
schema = "context-record/v0.1"
set_id = "tb.rescue"

[defaults]
sharing_scope = "repository"
origin = "user"
status = "active"

[[record]]
lineage_id = "ctx.tb.rescue.git-tutorial-style"
kind = "rule"
statement = "The git tutorial chapter must demonstrate fsck, dangling commits, and reflog recovery on a scripted corrupted repository."
[record.steering]
force = "may"
precedence = 50
applies_to = { paths = ["docs/tutorials/*"], keywords = ["tutorial"] }

[[record]]
lineage_id = "ctx.tb.rescue.css-design-tokens"
kind = "rule"
statement = "Component styles reference the design tokens, never raw hex colors."
[record.steering]
force = "may"
precedence = 50
applies_to = { paths = ["web/styles/*"], keywords = ["stylesheet"] }

[[record]]
lineage_id = "ctx.tb.rescue.mtime-not-recency"
kind = "rule"
statement = "Object-file mtimes survive packing unchanged, so file age is not evidence of when work was created."
[record.steering]
force = "may"
precedence = 40
applies_to = { paths = [".git/*"] }

[[record]]
lineage_id = "ctx.tb.rescue.reflog-first"
kind = "rule"
statement = "Recover lost commits from the reflog before any history rewrite; the reflog survives a corrupted HEAD."
[record.steering]
force = "may"
precedence = 80
applies_to = { paths = [".git/*"], keywords = ["fsck", "reflog"] }
"#;

    /// The corpus with the two irrelevant classes removed, for the
    /// noise-invariance assertion.
    const CORPUS_WITHOUT_NOISE: &str = r#"
schema = "context-record/v0.1"
set_id = "tb.rescue"

[defaults]
sharing_scope = "repository"
origin = "user"
status = "active"

[[record]]
lineage_id = "ctx.tb.rescue.mtime-not-recency"
kind = "rule"
statement = "Object-file mtimes survive packing unchanged, so file age is not evidence of when work was created."
[record.steering]
force = "may"
precedence = 40
applies_to = { paths = [".git/*"] }

[[record]]
lineage_id = "ctx.tb.rescue.reflog-first"
kind = "rule"
statement = "Recover lost commits from the reflog before any history rewrite; the reflog survives a corrupted HEAD."
[record.steering]
force = "may"
precedence = 80
applies_to = { paths = [".git/*"], keywords = ["fsck", "reflog"] }
"#;

    fn registry_of(corpus: &str) -> registry::Registry {
        let files = [crate::rules::RuleFile {
            path: ".stella/rules/tb.rescue.toml".to_string(),
            contents: corpus.to_string(),
            contributed_by: None,
        }];
        let facts = Facts {
            verdicts: BTreeMap::new(),
            last_checked: BTreeMap::new(),
            approved_blocking: BTreeSet::new(),
            now: "2026-08-08T00:00:00Z",
        };
        registry::load(&[], &files, &facts)
    }

    /// The task turn: a terminal-bench-shaped git-repository rescue.
    fn git_rescue_turn() -> (String, Vec<String>) {
        (
            "Repair the corrupted repository: recover the dangling commits, \
             restore main to the latest good commit, and make git fsck exit clean."
                .to_string(),
            vec![".git/HEAD".to_string(), ".git/objects".to_string()],
        )
    }

    #[test]
    fn the_task_turn_selects_exactly_the_two_relevant_classes() {
        let registry = registry_of(CORPUS);
        let (text, paths) = git_rescue_turn();
        let facts = TurnFacts {
            text: &text,
            paths: &paths,
        };
        let rendered = registry.render_volatile_for_turn(&facts, None);

        // Render order is load order by contract ("records are emitted in the
        // order given" — render.rs), so this asserts the exact set in that
        // order. Precedence ranks nothing here: it decides which records
        // survive a budget, and this render is unbudgeted, so the
        // precedence-40 record still precedes the precedence-80 one.
        assert_eq!(
            rendered.rendered,
            vec!["mtime-not-recency", "reflog-first"],
            "sufficient-only: exactly the relevant records, in load order"
        );
        assert!(
            !rendered.text.contains("tutorial"),
            "the similar-but-irrelevant record leaked in: {}",
            rendered.text
        );
        assert!(
            !rendered.text.contains("hex colors"),
            "noise leaked in: {}",
            rendered.text
        );
        // The non-obvious record earned its place through declared scope, not
        // vocabulary: its statement shares no task words, and it still rendered.
        assert!(rendered.text.contains("mtimes survive packing"));
    }

    #[test]
    fn statement_similarity_alone_never_selects() {
        // A turn whose words heavily overlap the tutorial record's *statement*
        // but whose situation (paths, keywords) matches nothing it declared.
        let registry = registry_of(CORPUS);
        let text = "git fsck reports dangling commits; recover them via the reflog.";
        let facts = TurnFacts { text, paths: &[] };
        let rendered = registry.render_volatile_for_turn(&facts, None);
        assert!(
            !rendered
                .rendered
                .contains(&"git-tutorial-style".to_string()),
            "a record was selected because its statement sounded similar: {:?}",
            rendered.rendered
        );
    }

    #[test]
    fn the_same_corpus_reaims_for_a_different_situation() {
        let registry = registry_of(CORPUS);
        let paths = vec!["web/styles/buttons.css".to_string()];
        let facts = TurnFacts {
            text: "Audit the stylesheet and replace raw hex colors with tokens.",
            paths: &paths,
        };
        let rendered = registry.render_volatile_for_turn(&facts, None);
        assert_eq!(
            rendered.rendered,
            vec!["css-design-tokens"],
            "context re-aims per turn: the css record alone applies here"
        );
    }

    #[test]
    fn rendered_bytes_are_invariant_under_injected_noise() {
        // Compactness as a property: adding irrelevant records to the corpus
        // must not change one byte of what the task turn receives — cost does
        // not grow with corpus size, only with relevance.
        let (text, paths) = git_rescue_turn();
        let facts = TurnFacts {
            text: &text,
            paths: &paths,
        };
        let with_noise = registry_of(CORPUS).render_volatile_for_turn(&facts, None);
        let without_noise =
            registry_of(CORPUS_WITHOUT_NOISE).render_volatile_for_turn(&facts, None);
        assert_eq!(
            with_noise.text, without_noise.text,
            "irrelevant records changed the bytes the model receives"
        );
    }

    #[test]
    fn a_budget_drop_is_ledgered_never_silent() {
        let registry = registry_of(CORPUS);
        let (text, paths) = git_rescue_turn();
        let facts = TurnFacts {
            text: &text,
            paths: &paths,
        };
        let full = registry.render_volatile_for_turn(&facts, None);
        assert_eq!(full.dropped, Vec::<String>::new());

        let squeezed = registry.render_volatile_for_turn(&facts, Some(full.text.len() - 1));
        let mut accounted: Vec<String> = squeezed
            .rendered
            .iter()
            .chain(squeezed.dropped.iter())
            .cloned()
            .collect();
        accounted.sort();
        assert_eq!(
            accounted,
            vec!["mtime-not-recency".to_string(), "reflog-first".to_string()],
            "every selected record is accounted for as rendered or dropped"
        );
        assert!(
            !squeezed.dropped.is_empty(),
            "the budget dropped a record and the ledger must say so"
        );

        // Honesty is not enough: the ledger can be truthful about a victim the
        // budget had no business choosing. The two records are within a byte of
        // each other in length, so a budget one byte short of the block fits
        // exactly one — and precedence, not load order, must say which. The
        // precedence-40 record loads first and used to win on that alone (#2299).
        assert_eq!(
            squeezed.rendered,
            vec!["reflog-first".to_string()],
            "the surviving record must be the higher-precedence one"
        );
        assert_eq!(
            squeezed.dropped,
            vec!["mtime-not-recency".to_string()],
            "a precedence-80 record lost its place to a precedence-40 one that \
             merely loaded earlier"
        );
    }

    #[test]
    fn the_volatile_corpus_never_touches_the_cached_prefix() {
        let registry = registry_of(CORPUS);
        let cached = registry.render(Channel::Cached, None);
        assert_eq!(
            cached.rendered,
            Vec::<String>::new(),
            "a may-force record leaked into the byte-stable prefix"
        );
    }
}

/// Time-lapse certification: one record walked through simulated months.
///
/// Adaptive context means context has a **lifecycle**, and this pins the
/// in-engine half of it end to end: a believed `should` record rides the
/// cached prefix; when its review cadence lapses unverified it is demoted to
/// the volatile channel with its staleness said out loud; when its truth probe
/// is refuted it leaves the prompt entirely (or survives demoted, when the
/// record declared `on_expiry = "stale"`). The whole walk is pure `Facts`
/// arithmetic — no clock, no model, no filesystem — so CI replays months in
/// microseconds on every `cargo test`.
mod time_lapse_certification {
    use super::super::registry;
    use super::super::select::TurnFacts;
    use super::*;
    use crate::ingest::Verdict;
    use std::collections::{BTreeMap, BTreeSet};

    const AGED_RECORD: &str = r#"
schema = "context-record/v0.1"
set_id = "tb.rescue"

[defaults]
sharing_scope = "repository"
origin = "user"
status = "active"

[[record]]
lineage_id = "ctx.tb.rescue.head-symref"
kind = "rule"
statement = "HEAD on this image is a symref; repair it by rewriting the ref text, never by deleting HEAD."
[record.steering]
force = "should"
precedence = 70
applies_to = { paths = [".git/*"] }
[record.truth]
basis = "measured"
confidence = 90
verified_at = "2026-01-01T00:00:00Z"
review_every = "P30D"
[record.truth.probe]
kind = "file_contains"
path = ".git/HEAD"
pattern = "ref:"
expect = "present"
"#;

    fn registry_at(
        now: &'static str,
        verdict: Option<Verdict>,
        last_checked: &str,
    ) -> registry::Registry {
        let files = [crate::rules::RuleFile {
            path: ".stella/rules/tb.rescue.toml".to_string(),
            contents: AGED_RECORD.to_string(),
            contributed_by: None,
        }];
        let lineage = "ctx.tb.rescue.head-symref".to_string();
        let mut verdicts = BTreeMap::new();
        if let Some(verdict) = verdict {
            verdicts.insert(lineage.clone(), verdict);
        }
        let mut checked = BTreeMap::new();
        checked.insert(lineage, last_checked.to_string());
        let facts = Facts {
            verdicts,
            last_checked: checked,
            approved_blocking: BTreeSet::new(),
            now,
        };
        registry::load(&[], &files, &facts)
    }

    fn git_turn_facts() -> (String, Vec<String>) {
        (
            "Repair the corrupted repository so git fsck exits clean.".to_string(),
            vec![".git/HEAD".to_string()],
        )
    }

    #[test]
    fn month_zero_a_confirmed_record_rides_the_cached_prefix() {
        let registry = registry_at(
            "2026-01-02T00:00:00Z",
            Some(Verdict::Supported),
            "2026-01-01T00:00:00Z",
        );
        let cached = registry.render(Channel::Cached, None);
        assert_eq!(cached.rendered, vec!["head-symref"]);
    }

    #[test]
    fn month_two_an_unreverified_record_demotes_to_volatile_with_its_reason() {
        // The P30D cadence lapsed two months ago and nothing re-ran the probe
        // (a verdict in `Facts` means "the probe ran this sweep", so a lapsed
        // cadence arrives as no verdict at all): the record leaves the
        // byte-stable prefix and rides the volatile channel, where its
        // staleness can be said without a clock entering the cache.
        let registry = registry_at("2026-03-05T00:00:00Z", None, "2026-01-01T00:00:00Z");
        let cached = registry.render(Channel::Cached, None);
        assert_eq!(
            cached.rendered,
            Vec::<String>::new(),
            "a stale record must not sit in the cached prefix"
        );

        let (text, paths) = git_turn_facts();
        let facts = TurnFacts {
            text: &text,
            paths: &paths,
        };
        let volatile = registry.render_volatile_for_turn(&facts, None);
        assert_eq!(volatile.rendered, vec!["head-symref"]);

        // And demotion does not bypass selection: a turn its scope does not
        // match still does not receive it.
        let unrelated = TurnFacts {
            text: "Write a haiku about the ocean.",
            paths: &[],
        };
        assert_eq!(
            registry.render_volatile_for_turn(&unrelated, None).rendered,
            Vec::<String>::new(),
        );
    }

    #[test]
    fn a_refuted_record_leaves_the_prompt_entirely() {
        let registry = registry_at(
            "2026-03-05T00:00:00Z",
            Some(Verdict::Refuted),
            "2026-03-05T00:00:00Z",
        );
        let cached = registry.render(Channel::Cached, None);
        let (text, paths) = git_turn_facts();
        let facts = TurnFacts {
            text: &text,
            paths: &paths,
        };
        let volatile = registry.render_volatile_for_turn(&facts, None);
        assert!(
            cached.rendered.is_empty() && volatile.rendered.is_empty(),
            "a refuted claim reached the model: cached {:?}, volatile {:?}",
            cached.rendered,
            volatile.rendered
        );
    }
}

/// Witness for #2709: an explicit `tier = "scoped"` with no `applies_to`
/// trigger is named — no turn can ever select it, so the declaration is a
/// silent gap — while the derived tiers, which cannot produce the mismatch,
/// stay finding-free.
#[test]
fn an_explicitly_scoped_record_without_a_trigger_is_named() {
    let mut untriggered = loaded_from(with_tier(
        record_named("ctx.acme.web.untriggered"),
        Force::May,
        Tier::Scoped,
    ));
    validate::check_record(&mut untriggered);
    assert!(
        untriggered
            .findings
            .contains(&RecordFinding::ScopedWithoutTrigger),
        "scoped with nothing to fire on must be named: {:?}",
        untriggered.findings
    );

    let mut derived = loaded_from(with_force(record_named("ctx.acme.web.plain"), Force::May));
    validate::check_record(&mut derived);
    assert!(
        !derived
            .findings
            .contains(&RecordFinding::ScopedWithoutTrigger),
        "a derived retrieved record carries no scoped-without-trigger finding"
    );
}
