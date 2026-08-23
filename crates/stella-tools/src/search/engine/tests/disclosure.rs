//! What a search discloses about itself: capped scans, a cut name-match list, merged notes, doc-comment walking, and truncation (#4494 split of `../tests.rs`).
use super::*;

/// A file-scan that hit its walk cap must say so: on an unindexed tree
/// bigger than the cap, a silent miss reads as absence — the exact failure
/// the module docs argue against.
#[test]
fn a_capped_scan_discloses_what_it_never_saw() {
    let workspace = tempfile::tempdir().expect("tempdir");
    for index in 0..6 {
        fs::write(
            workspace.path().join(format!("file{index}.txt")),
            "nothing relevant\n",
        )
        .expect("write");
    }
    fs::write(workspace.path().join("aaa.txt"), "credential rotation\n").expect("write");
    let root = workspace.path().canonicalize().expect("canonicalize");

    let capped = scan::scan_hits_bounded(&root, "credential rotation", 10, 3);
    assert!(
        capped.exhausted,
        "a walk over 7 files with a 3-file cap must report exhaustion"
    );

    let full = scan::scan_hits_bounded(&root, "credential rotation", 10, 100);
    assert!(
        !full.exhausted,
        "a walk that saw the whole tree must not claim exhaustion"
    );
    assert_eq!(full.matched, 1);
    assert_eq!(
        full.hits.first().map(|hit| hit.path.as_str()),
        Some("aaa.txt")
    );
}

/// The capped walk is shallow-first: the files most likely to matter — the
/// root's own — are inside the cap, not past it.
#[test]
fn a_capped_scan_sees_the_root_before_the_leaves() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let deep = workspace.path().join("zzz").join("deeper");
    fs::create_dir_all(&deep).expect("mkdir");
    for index in 0..4 {
        fs::write(deep.join(format!("noise{index}.txt")), "rotation\n").expect("write");
    }
    fs::write(workspace.path().join("rotation.txt"), "rotation\n").expect("write");
    let root = workspace.path().canonicalize().expect("canonicalize");

    let capped = scan::scan_hits_bounded(&root, "rotation", 10, 1);
    assert_eq!(
        capped.hits.first().map(|hit| hit.path.as_str()),
        Some("rotation.txt"),
        "a 1-file cap must spend its budget at the root, not in the deepest subtree: {:?}",
        capped.hits
    );
}

/// A name-match answer wider than the shown list must disclose the cut:
/// "10 results" over 14 matching files reads as "only 10 matched".
#[tokio::test]
async fn a_cut_name_match_list_discloses_how_many_matched() {
    let workspace = tempfile::tempdir().expect("tempdir");
    for index in 0..14 {
        let file = workspace.path().join(format!("src/widget_{index:02}.rs"));
        fs::create_dir_all(file.parent().expect("a parent")).expect("mkdir");
        fs::write(&file, format!("pub fn widget_{index:02}() {{}}\n")).expect("write");
    }
    let root = workspace.path().canonicalize().expect("canonicalize");
    let graph = CodeGraph::open(&root, &root.join("codegraph.db")).expect("open");
    graph.index_all().expect("index");

    let answer = dispatch(Some(&graph), &root, "widget", None).await;
    assert_eq!(answer.strategies, vec![Strategy::GraphNames]);
    assert_eq!(answer.hits.len(), 10, "the shown list stays capped");
    let note = answer.note.expect("a cut list must carry a note");
    assert!(
        note.contains("14 files matched"),
        "the note must say how many matched: {note}"
    );

    graph.shutdown();
}

/// A misconfigured embedder must not eat the dispatch's own disclosure: over
/// a 14-match corpus whose name rung cuts its list, the one note line carries
/// **both** the cut ("14 files matched") and the misconfiguration.
#[tokio::test]
async fn a_misconfigured_embedder_note_joins_the_cut_list_note_instead_of_replacing_it() {
    let workspace = tempfile::tempdir().expect("tempdir");
    for index in 0..14 {
        let file = workspace.path().join(format!("src/widget_{index:02}.rs"));
        fs::create_dir_all(file.parent().expect("a parent")).expect("mkdir");
        fs::write(&file, format!("pub fn widget_{index:02}() {{}}\n")).expect("write");
    }
    let root = workspace.path().canonicalize().expect("canonicalize");

    let report = uncached::report_with(
        &root,
        "widget",
        SearchConfig::default(),
        stella_embed::Resolution::Incomplete("STELLA_EMBED_MODEL is unset".into()),
    )
    .await;

    let note = report.note.expect("both caveats need a note to live in");
    assert!(
        note.contains("14 files matched"),
        "the cut-list disclosure must survive the misconfig note: {note}"
    );
    assert!(
        note.contains("misconfigured"),
        "the misconfiguration must still be said: {note}"
    );
}

/// Two caveats must both survive into the one note line.
#[test]
fn merging_notes_loses_neither() {
    assert_eq!(
        merge_notes(Some("first".into()), Some("second".into())),
        Some("first; second".into())
    );
    assert_eq!(merge_notes(None, Some("only".into())), Some("only".into()));
    assert_eq!(merge_notes(Some("only".into()), None), Some("only".into()));
    assert_eq!(merge_notes(None, None), None);
}

/// An attribute between a declaration and its doc comment is not prose: the
/// quoted doc must skip `#[must_use]` and still reach the real comment above
/// it.
#[test]
fn a_doc_comment_walk_skips_attributes_instead_of_quoting_them() {
    let source = "/// Keeps confidential material out of diagnostics.\n\
                  #[must_use]\n\
                  pub fn scrub() {}\n";
    let symbol = stella_graph::NeighborhoodSymbol {
        name: "scrub".into(),
        kind: "function".into(),
        start_line: 3,
    };
    let doc = enrich::doc_comment(Some(source), &symbol).expect("a doc line");
    assert!(
        !doc.contains("must_use"),
        "the attribute was quoted as prose: {doc}"
    );
    assert!(
        doc.contains("confidential material"),
        "the real doc comment above the attribute was lost: {doc}"
    );
}

/// A budget too small for every hit must say so. A reader who cannot tell a
/// complete answer from a truncated one stops looking, which is the exact
/// failure this line prevents.
#[test]
fn a_truncated_answer_says_it_was_truncated() {
    let hits: Vec<Hit> = (0..6)
        .map(|index| Hit {
            path: format!("src/file{index}.rs"),
            why: "ranked by MEANING against your query (cosine 0.900)".into(),
            focus: None,
        })
        .collect();
    let answer = Answer {
        hits,
        strategies: vec![Strategy::Semantic],
        note: None,
    };

    let content = content_of(&render(
        None,
        Path::new("/nonexistent"),
        "anything",
        &answer,
        SearchConfig {
            depth: Depth::MIN,
            budget: 130,
        },
    ));
    assert!(
        content.contains("TRUNCATED"),
        "a truncated answer did not say so: {content}"
    );
    assert!(
        content.contains("further result(s) were dropped"),
        "the truncation line did not say what was dropped: {content}"
    );

    // The complement: given room, the same answer must NOT claim truncation.
    let roomy = content_of(&render(
        None,
        Path::new("/nonexistent"),
        "anything",
        &answer,
        SearchConfig {
            depth: Depth::MIN,
            budget: 100_000,
        },
    ));
    assert!(
        !roomy.contains("TRUNCATED"),
        "an untruncated answer claimed truncation: {roomy}"
    );
}

/// The allocator and the renderer must agree about how many hits were shown;
/// they are separately written and a drift between them would silently drop
/// results with no truncation line.
#[test]
fn the_rendered_hit_count_matches_the_allocation() {
    let hits: Vec<Hit> = (0..7)
        .map(|index| Hit {
            path: format!("src/file{index}.rs"),
            why: "matched 1 query term(s)".into(),
            focus: None,
        })
        .collect();
    let answer = Answer {
        hits: hits.clone(),
        strategies: vec![Strategy::FileScan],
        note: None,
    };
    let config = SearchConfig {
        depth: Depth::new(3),
        budget: 900,
    };
    let allocation = allocate(hits.len(), config.depth, config.budget);
    let content = content_of(&render(
        None,
        Path::new("/nonexistent"),
        "anything",
        &answer,
        config,
    ));

    let shown = hits
        .iter()
        .filter(|hit| content.contains(&hit.path))
        .count();
    assert_eq!(shown, allocation.granted.len());
    assert_eq!(allocation.omitted, hits.len() - shown);
    assert!(facets_at(config.depth).len() >= 3);
}
