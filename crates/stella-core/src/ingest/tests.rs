use super::*;

/// Every rule here is asserted in both directions. A classifier tested only on
/// the documents it is supposed to catch passes just as happily when it catches
/// everything, and a tier that swallows the whole tree is indistinguishable
/// from one that works until you look at what it did NOT flag.

// ── depth and extension ─────────────────────────────────────────────────────

#[test]
fn depth_counts_directories_not_path_segments() {
    assert!(is_within_depth("README.md"));
    assert!(is_within_depth("a/b/c/d/README.md"));
    assert!(!is_within_depth("a/b/c/d/e/README.md"));
}

#[test]
fn markdown_extensions_only() {
    assert!(is_markdown("docs/x.md"));
    assert!(is_markdown("docs/x.MD"));
    assert!(is_markdown("site/page.mdx"));
    assert!(!is_markdown("src/main.rs"));
    assert!(!is_markdown("notes.txt"));
    // `.md` has to be the extension, not merely present in the name.
    assert!(!is_markdown("docs/x.md.bak"));
}

#[test]
fn excluded_segments_match_whole_segments_only() {
    assert!(is_excluded_segment("node_modules/pkg/README.md"));
    assert!(is_excluded_segment("a/target/doc/x.md"));
    // A directory that merely starts with an excluded name is not excluded —
    // `targeting/` and `buildkite/` are ordinary project directories.
    assert!(!is_excluded_segment("targeting/README.md"));
    assert!(!is_excluded_segment("buildkite/README.md"));
    assert!(!is_excluded_segment("docs/spec/x.md"));
}

// ── supersession, and its direction ─────────────────────────────────────────

#[test]
fn supersession_marker_found_in_head() {
    let doc = "# Old Spec\n\n> **Superseded — see docs/new.md**\n\nBody.";
    assert_eq!(supersession_marker(doc).as_deref(), Some("superseded"));
}

/// The subtle one. A live document routinely announces that it *supersedes* an
/// older draft. Matching the shorter stem would demote the canonical spec and
/// promote the draft it replaced — exactly backwards.
#[test]
fn a_document_that_supersedes_others_is_not_itself_superseded() {
    let canonical = "# Context PRs\n\nThis document consolidates and supersedes \
                     earlier Context-PR drafts.\n";
    assert_eq!(supersession_marker(canonical), None);

    let retired =
        "# Context PRs — draft\n\n> Superseded by docs/spec/adaptive-context/context-pr.md\n";
    assert!(supersession_marker(retired).is_some());
}

#[test]
fn supersession_is_not_read_from_the_body() {
    // A migration guide discusses deprecation at length without being deprecated.
    let mut doc = String::from("# How we deprecate APIs\n\n");
    for _ in 0..HEAD_LINES {
        doc.push_str("Filler line about process.\n");
    }
    doc.push_str("Old endpoints are marked deprecated before removal.\n");
    // The title itself contains no supersession phrase, and the body mention is
    // past the head window.
    assert_eq!(supersession_marker(&doc), None);
}

// ── index detection ─────────────────────────────────────────────────────────

#[test]
fn a_bullet_list_of_markdown_links_is_an_index() {
    let idx = "# Memory index\n\n\
               - [First](first.md) — hook\n\
               - [Second](second.md) — hook\n\
               - [Third](third.md) — hook\n";
    assert!(is_index_of_siblings(idx));
}

#[test]
fn prose_with_a_few_links_is_not_an_index() {
    let doc = "# Design\n\n\
               - it must be fast\n\
               - it must be correct\n\
               - see [the spec](spec.md) for details\n\
               - it must be reviewable\n";
    assert!(!is_index_of_siblings(doc));
}

#[test]
fn two_links_are_not_enough_to_be_an_index() {
    let doc = "# Notes\n\n- [a](a.md)\n- [b](b.md)\n";
    assert!(!is_index_of_siblings(doc));
}

#[test]
fn a_document_with_no_bullets_is_not_an_index() {
    assert!(!is_index_of_siblings(
        "# Title\n\nJust prose about [x](x.md).\n"
    ));
}

// ── tiers ───────────────────────────────────────────────────────────────────

#[test]
fn the_two_primary_files_are_offered_unprompted() {
    for name in ["CLAUDE.md", "AGENTS.md", "claude.md", "agents.md"] {
        let c = classify(name, "# Title\n\nDo the thing.\n");
        assert_eq!(c.tier, Tier::Primary, "{name}");
        assert!(c.is_offered_by_default(), "{name}");
    }
}

/// Everything else instructional is a suggestion, not an opener. A first-run
/// dialog listing nine files is a chore, and the honest answer to a chore is
/// "not now".
#[test]
fn other_instructional_files_are_suggestions_not_openers() {
    for name in ["CONTRIBUTING.md", "GEMINI.md", "copilot-instructions.md"] {
        let c = classify(name, "# Title\n\nDo the thing.\n");
        assert_eq!(c.tier, Tier::Instructional, "{name}");
        assert!(!c.is_offered_by_default(), "{name} opened the dialog");
        assert!(c.is_suggestion(), "{name}");
    }
}

#[test]
fn instructional_by_directory_regardless_of_filename() {
    for path in [
        ".claude/rules/anything.md",
        ".agents/whatever.md",
        ".cursor/rules/x.md",
        "packages/api/memories/lesson.md",
        "docs/memory/lesson.md",
    ] {
        let c = classify(path, "# L\n\nSomething learned.\n");
        assert_eq!(c.tier, Tier::Instructional, "{path}");
    }
}

#[test]
fn ordinary_docs_are_descriptive_not_instructional() {
    for path in ["README.md", "docs/spec/storage-map.md", "docs/adr/0001-x.md"] {
        let c = classify(path, "# Title\n\nThis describes the system.\n");
        assert_eq!(c.tier, Tier::Descriptive, "{path}");
        assert!(!c.is_offered_by_default(), "{path}");
    }
}

/// An active ADR is current policy and must not be swept into `Historical`
/// merely for living in `docs/adr/`. Only a marker in its own head demotes it.
#[test]
fn an_active_adr_is_not_historical() {
    let active = classify(
        "docs/adr/0008-markdown-canonical.md",
        "# ADR 0008\n\n- Status: Accepted\n\nMarkdown stays canonical.\n",
    );
    assert_eq!(active.tier, Tier::Descriptive);

    let retired = classify(
        "docs/adr/0002-old.md",
        "# ADR 0002\n\n- Status: Superseded by ADR 0008\n",
    );
    assert_eq!(retired.tier, Tier::Historical);
}

#[test]
fn changelogs_are_historical() {
    let c = classify("CHANGELOG.md", "# Changelog\n\n## 1.0\n");
    assert_eq!(c.tier, Tier::Historical);
    assert!(c.signals.contains(&Signal::HistoricalName));
}

#[test]
fn boilerplate_and_excluded_and_indexes_are_skipped() {
    assert_eq!(classify("LICENSE.md", "MIT").tier, Tier::Skip);
    assert_eq!(
        classify("node_modules/pkg/README.md", "# Pkg").tier,
        Tier::Skip
    );
    let idx = "- [a](a.md)\n- [b](b.md)\n- [c](c.md)\n";
    assert_eq!(classify("memories/index.md", idx).tier, Tier::Skip);
}

/// The index trap is the reason this check exists: a memories directory is
/// `Instructional`, so without index detection its table of contents would be
/// offered by default and mint one duplicate claim per file it lists.
#[test]
fn an_index_inside_a_memory_directory_is_skipped_not_offered() {
    let idx = "# Memory index\n\n- [a](a.md) — x\n- [b](b.md) — y\n- [c](c.md) — z\n";
    let c = classify("memories/index.md", idx);
    assert_eq!(c.tier, Tier::Skip);
    assert!(c.signals.contains(&Signal::MemoryDirectory));
    assert!(c.signals.contains(&Signal::IndexOfSiblings));
}

/// A retired `AGENTS.md` is more misleading than a retired design note, not
/// less — its whole voice is imperative. Supersession outranks instruction.
#[test]
fn supersession_demotes_even_a_primary_file() {
    let c = classify("AGENTS.md", "# Agents\n\n> Deprecated — see docs/new.md\n");
    assert_eq!(c.tier, Tier::Historical);
    assert!(c.signals.contains(&Signal::PrimaryInstructionFile));
    assert!(
        !c.is_offered_by_default(),
        "retired AGENTS.md still offered"
    );
}

// ── the first-run plan ──────────────────────────────────────────────────────

#[test]
fn plan_opens_with_the_primary_files_and_keeps_the_rest_for_step_two() {
    let candidates = classify_all(vec![
        ("AGENTS.md", "# A\n\nSteering.\n"),
        ("CLAUDE.md", "# C\n\nSteering.\n"),
        ("CONTRIBUTING.md", "# Contributing\n\nHow to help.\n"),
        ("README.md", "# R\n\nDescribes the project.\n"),
    ]);
    let plan = plan(candidates);

    let primary: Vec<&str> = plan.primary.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(primary, vec!["AGENTS.md", "CLAUDE.md"]);

    let rest: Vec<&str> = plan.suggestions.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(rest, vec!["CONTRIBUTING.md", "README.md"]);
}

/// A changelog or a retired doc is dropped from the plan entirely. Showing it
/// only to explain why it is a bad idea makes the first run longer for no gain.
#[test]
fn plan_drops_historical_and_skipped_documents() {
    let plan = plan(classify_all(vec![
        ("AGENTS.md", "# A\n\nSteering.\n"),
        ("CHANGELOG.md", "# Changelog\n\n## 1.0\n"),
        ("LICENSE.md", "MIT"),
        ("docs/old.md", "# Old\n\n> Superseded by new.md\n"),
    ]));
    assert_eq!(plan.primary.len(), 1);
    assert!(plan.suggestions.is_empty(), "{:?}", plan.suggestions);
}

#[test]
fn a_workspace_with_nothing_to_import_yields_an_empty_plan() {
    let plan = plan(classify_all(vec![
        ("LICENSE.md", "MIT"),
        ("CHANGELOG.md", "# Changelog\n"),
    ]));
    assert!(plan.is_empty());
}

#[test]
fn a_workspace_with_only_suggestions_is_not_empty() {
    let plan = plan(classify_all(vec![("README.md", "# R\n\nAbout.\n")]));
    assert!(plan.primary.is_empty());
    assert!(!plan.is_empty(), "step two alone still worth asking");
}

/// Running the scan on the stella repo produced 148 lines of suggestions. That
/// is not a question anybody answers, it is a wall people dismiss.
#[test]
fn suggestions_are_capped_and_the_remainder_is_reported() {
    let docs: Vec<(String, String)> = (0..25)
        .map(|i| {
            (
                format!("docs/note-{i:02}.md"),
                "# N\n\nProse.\n".to_string(),
            )
        })
        .collect();
    let plan = plan(classify_all(
        docs.iter().map(|(p, c)| (p.as_str(), c.as_str())),
    ));

    assert_eq!(plan.suggestions.len(), MAX_SUGGESTIONS);
    assert_eq!(plan.hidden_suggestions, 25 - MAX_SUGGESTIONS);
}

#[test]
fn nothing_is_hidden_when_everything_fits() {
    let plan = plan(classify_all(vec![("README.md", "# R\n\nAbout.\n")]));
    assert_eq!(plan.hidden_suggestions, 0);
}

/// Depth is the cheap proxy for "is this about the project or about a corner of
/// it", and instructional still outranks depth.
#[test]
fn suggestions_rank_instructional_first_then_shallowest() {
    let plan = plan(classify_all(vec![
        ("bench/adapter/docs/README.md", "# Deep\n\nCorner.\n"),
        ("README.md", "# Root\n\nProject.\n"),
        ("docs/guide.md", "# Guide\n\nMiddling.\n"),
        ("CONTRIBUTING.md", "# Contributing\n\nHow to help.\n"),
    ]));
    let order: Vec<&str> = plan.suggestions.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(
        order,
        vec![
            "CONTRIBUTING.md",
            "README.md",
            "docs/guide.md",
            "bench/adapter/docs/README.md",
        ]
    );
}

#[test]
fn the_matched_phrase_is_reported_for_review() {
    let c = classify("docs/x.md", "# X\n\n> Superseded by y.md\n");
    let phrase = c.signals.iter().find_map(|s| match s {
        Signal::SupersededMarker(p) => Some(p.clone()),
        _ => None,
    });
    assert_eq!(phrase.as_deref(), Some("superseded"));
}

// ── the whole scan ──────────────────────────────────────────────────────────

#[test]
fn classify_all_drops_non_markdown_and_deep_paths_and_sorts_by_tier() {
    let files = vec![
        ("README.md", "# R\n\nDescribes things.\n"),
        ("src/main.rs", "fn main() {}"),
        ("a/b/c/d/e/deep.md", "# Too deep\n"),
        ("CLAUDE.md", "# C\n\nInstructions.\n"),
        ("CHANGELOG.md", "# Changelog\n"),
        ("LICENSE.md", "MIT"),
    ];
    let out = classify_all(files);
    let paths: Vec<&str> = out.iter().map(|c| c.path.as_str()).collect();

    assert!(!paths.contains(&"src/main.rs"), "non-markdown kept");
    assert!(!paths.contains(&"a/b/c/d/e/deep.md"), "over-deep kept");
    assert_eq!(
        paths,
        vec!["CLAUDE.md", "README.md", "CHANGELOG.md", "LICENSE.md"],
        "not ordered instructional → descriptive → historical → skip"
    );
}

#[test]
fn classify_all_is_deterministic() {
    let files = vec![("b.md", "# B\n"), ("a.md", "# A\n"), ("CLAUDE.md", "# C\n")];
    assert_eq!(classify_all(files.clone()), classify_all(files));
}
