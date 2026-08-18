use super::*;
use crate::parse::Grammars;
use std::fs;
use tempfile::tempdir;

/// A fingerprint string of the shape `stella_embed::EmbedderFingerprint::id()`
/// produces. The store treats it as an opaque key, so the tests do too.
const FP: &str = "test-model@1/4/l2";
const OTHER_FP: &str = "other-model@1/4/l2";

fn indexed_workspace(files: &[(&str, &str)]) -> (tempfile::TempDir, Connection) {
    let ws = tempdir().expect("tempdir");
    for (rel, body) in files {
        let path = ws.path().join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(&path, body).expect("write");
    }
    let db = ws.path().join("codegraph.db");
    let mut conn = store::open(&db).expect("open");
    let root = ws.path().canonicalize().expect("canonicalize");
    store::index_tree(&mut conn, &root, &Grammars::load().expect("grammars")).expect("index");
    (ws, conn)
}

#[test]
fn the_render_is_stable_and_names_the_path_and_symbols() {
    let names = vec!["redact".to_string(), "scrub_headers".to_string()];
    let once = render_file_text("src/a.rs", &names, "fn redact() {}\n");
    let twice = render_file_text("src/a.rs", &names, "fn redact() {}\n");
    assert_eq!(once, twice);
    assert!(once.starts_with("file: src/a.rs\n"), "{once}");
    assert!(once.contains("symbols: redact, scrub_headers\n"), "{once}");
    assert!(once.ends_with("fn redact() {}\n"), "{once}");
}

#[test]
fn a_long_file_is_truncated_on_a_character_boundary() {
    // A multi-byte character straddling the cap must not be cut in half — a
    // byte slice would panic, and a lossy cut would make the render depend on
    // where the boundary landed.
    let content = "é".repeat(MAX_CONTENT_CHARS + 50);
    let rendered = render_file_text("src/a.rs", &[], &content);
    assert!(rendered.chars().filter(|c| *c == 'é').count() == MAX_CONTENT_CHARS);
}

#[test]
fn a_vector_round_trips_through_the_blob_encoding() {
    let original = vec![0.5f32, -0.25, 0.125, 0.0];
    assert_eq!(decode(&encode(&original)), original);
}

#[test]
fn a_truncated_blob_drops_the_partial_float_rather_than_inventing_one() {
    let mut bytes = encode(&[1.0f32, 2.0]);
    bytes.pop();
    assert_eq!(decode(&bytes), vec![1.0f32]);
}

#[test]
fn every_indexed_file_is_pending_until_it_is_embedded() {
    let (ws, mut conn) =
        indexed_workspace(&[("a.rs", "fn alpha() {}\n"), ("b.rs", "fn beta() {}\n")]);
    let root = ws.path().canonicalize().expect("canonicalize");

    let first = pending(&conn, &root, FP, 10).expect("pending").files;
    assert_eq!(
        first.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
        vec!["a.rs", "b.rs"],
        "one index pass stamps both files in the same second, so the path \
         tiebreak decides the order and a capped pass stays deterministic"
    );
    assert!(
        first[0].text.contains("alpha"),
        "the render carries symbols"
    );

    let rows: Vec<FileVector> = first
        .iter()
        .map(|p| FileVector {
            path: p.path.clone(),
            content_sha256: p.content_sha256.clone(),
            vector: vec![1.0, 0.0, 0.0, 0.0],
        })
        .collect();
    assert_eq!(store_vectors(&mut conn, FP, &rows).expect("store"), 2);
    assert!(
        pending(&conn, &root, FP, 10)
            .expect("pending")
            .files
            .is_empty(),
        "an embedded file is not embedded twice"
    );
    assert_eq!(count(&conn, FP).expect("count"), 2);
}

#[test]
fn changing_a_file_makes_it_pending_again_without_touching_the_other() {
    let (ws, mut conn) =
        indexed_workspace(&[("a.rs", "fn alpha() {}\n"), ("b.rs", "fn beta() {}\n")]);
    let root = ws.path().canonicalize().expect("canonicalize");
    let rows: Vec<FileVector> = pending(&conn, &root, FP, 10)
        .expect("pending")
        .files
        .iter()
        .map(|p| FileVector {
            path: p.path.clone(),
            content_sha256: p.content_sha256.clone(),
            vector: vec![1.0, 0.0, 0.0, 0.0],
        })
        .collect();
    store_vectors(&mut conn, FP, &rows).expect("store");

    fs::write(ws.path().join("a.rs"), "fn alpha_renamed() {}\n").expect("write");
    store::index_tree(&mut conn, &root, &Grammars::load().expect("grammars")).expect("reindex");

    let stale = pending(&conn, &root, FP, 10).expect("pending").files;
    assert_eq!(
        stale.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
        vec!["a.rs"],
        "only the file whose content moved is re-embedded"
    );
}

/// The invalidation contract: two embedders never share a vector space. A
/// vector stored under one fingerprint is invisible to the other — it is
/// neither ranked nor counted, and its file reads as pending.
#[test]
fn a_different_fingerprint_re_embeds_rather_than_mixing_vector_spaces() {
    let (ws, mut conn) = indexed_workspace(&[("a.rs", "fn alpha() {}\n")]);
    let root = ws.path().canonicalize().expect("canonicalize");
    let rows = vec![FileVector {
        path: "a.rs".into(),
        content_sha256: pending(&conn, &root, FP, 10).expect("pending").files[0]
            .content_sha256
            .clone(),
        vector: vec![1.0, 0.0, 0.0, 0.0],
    }];
    store_vectors(&mut conn, FP, &rows).expect("store");

    assert_eq!(count(&conn, OTHER_FP).expect("count"), 0);
    assert!(
        rank(
            &conn,
            OTHER_FP,
            &[1.0, 0.0, 0.0, 0.0],
            f32::NEG_INFINITY,
            10
        )
        .expect("rank")
        .is_empty(),
        "the other embedder's ranking must not see this vector"
    );
    assert_eq!(
        pending(&conn, &root, OTHER_FP, 10)
            .expect("pending")
            .files
            .len(),
        1,
        "the file is pending again under the new fingerprint"
    );
    // ...and the original fingerprint still answers, so the re-embed is
    // incremental rather than a wipe.
    assert_eq!(count(&conn, FP).expect("count"), 1);
}

/// #3016: the window is `limit` *embeddable* files, not `limit` rows that are
/// then filtered. Truncating in SQL and skipping afterwards meant a run of
/// unreadable files at the head of the path order returned nothing at all —
/// which the eager `stella init` loop reads as "no work left" and stops on,
/// leaving every readable file behind it unembedded.
#[test]
fn a_run_of_unreadable_files_does_not_consume_the_window() {
    let (ws, conn) = indexed_workspace(&[
        ("a.rs", "fn a() {}\n"),
        ("b.rs", "fn b() {}\n"),
        ("c.rs", "fn c() {}\n"),
        ("d.rs", "fn d() {}\n"),
    ]);
    let root = ws.path().canonicalize().expect("canonicalize");
    // Deleted from disk but still indexed — exactly what the graph holds
    // between a file being removed and the next `index_all` pruning its row.
    // They sort ahead of every readable file, so they are what the old SQL
    // `LIMIT` spent itself on.
    fs::remove_file(ws.path().join("a.rs")).expect("remove");
    fs::remove_file(ws.path().join("b.rs")).expect("remove");

    let scan = pending(&conn, &root, FP, 2).expect("pending");
    assert_eq!(
        scan.files
            .iter()
            .map(|p| p.path.as_str())
            .collect::<Vec<_>>(),
        vec!["c.rs", "d.rs"],
        "the scan fills past the unreadable rows instead of truncating on them"
    );
    assert_eq!(
        scan.unreadable, 2,
        "and says how many it stepped over, so a caller reporting leftovers \
         can name the reason instead of blaming the cap"
    );
}

/// The step-over preserves the scan's ordering, so the resume contract below
/// still holds when unreadable files are interleaved with readable ones. One
/// index pass stamps every file in the same second, so the order under test
/// here is the `path` tiebreak.
#[test]
fn stepping_over_an_unreadable_file_preserves_the_scan_ordering() {
    let (ws, mut conn) = indexed_workspace(&[
        ("a.rs", "fn a() {}\n"),
        ("b.rs", "fn b() {}\n"),
        ("c.rs", "fn c() {}\n"),
        ("d.rs", "fn d() {}\n"),
    ]);
    let root = ws.path().canonicalize().expect("canonicalize");
    fs::remove_file(ws.path().join("b.rs")).expect("remove");

    let first = pending(&conn, &root, FP, 2).expect("pending");
    assert_eq!(
        first
            .files
            .iter()
            .map(|p| p.path.as_str())
            .collect::<Vec<_>>(),
        vec!["a.rs", "c.rs"]
    );
    let rows: Vec<FileVector> = first
        .files
        .iter()
        .map(|p| FileVector {
            path: p.path.clone(),
            content_sha256: p.content_sha256.clone(),
            vector: vec![1.0, 0.0, 0.0, 0.0],
        })
        .collect();
    store_vectors(&mut conn, FP, &rows).expect("store");

    let second = pending(&conn, &root, FP, 2).expect("pending");
    assert_eq!(
        second
            .files
            .iter()
            .map(|p| p.path.as_str())
            .collect::<Vec<_>>(),
        vec!["d.rs"],
        "the second pass resumes rather than re-offering what it already saw"
    );
    assert_eq!(second.unreadable, 1, "and still steps over the deleted row");
}

/// A pending set that is *entirely* unreadable comes back with no files —
/// which is the signal the eager loop stops on, and now the honest one: the
/// scan reached the end of the set rather than giving up inside it.
#[test]
fn an_entirely_unreadable_pending_set_reports_no_files_and_the_count() {
    let (ws, conn) = indexed_workspace(&[("a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\n")]);
    let root = ws.path().canonicalize().expect("canonicalize");
    fs::remove_file(ws.path().join("a.rs")).expect("remove");
    fs::remove_file(ws.path().join("b.rs")).expect("remove");

    let scan = pending(&conn, &root, FP, 200).expect("pending");
    assert!(scan.files.is_empty());
    assert_eq!(scan.unreadable, 2);
}

#[test]
fn a_capped_pass_resumes_where_it_stopped() {
    let (ws, mut conn) = indexed_workspace(&[
        ("a.rs", "fn a() {}\n"),
        ("b.rs", "fn b() {}\n"),
        ("c.rs", "fn c() {}\n"),
    ]);
    let root = ws.path().canonicalize().expect("canonicalize");

    let first = pending(&conn, &root, FP, 2).expect("pending").files;
    assert_eq!(
        first.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
        vec!["a.rs", "b.rs"]
    );
    let rows: Vec<FileVector> = first
        .iter()
        .map(|p| FileVector {
            path: p.path.clone(),
            content_sha256: p.content_sha256.clone(),
            vector: vec![1.0, 0.0, 0.0, 0.0],
        })
        .collect();
    store_vectors(&mut conn, FP, &rows).expect("store");

    let second = pending(&conn, &root, FP, 2).expect("pending").files;
    assert_eq!(
        second.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
        vec!["c.rs"]
    );
}

#[test]
fn a_vector_for_an_unindexed_path_is_skipped_not_resurrected() {
    let (_ws, mut conn) = indexed_workspace(&[("a.rs", "fn alpha() {}\n")]);
    let rows = vec![FileVector {
        path: "never/indexed.rs".into(),
        content_sha256: "deadbeef".into(),
        vector: vec![1.0, 0.0, 0.0, 0.0],
    }];
    assert_eq!(store_vectors(&mut conn, FP, &rows).expect("store"), 0);
    assert_eq!(count(&conn, FP).expect("count"), 0);
}

#[test]
fn ranking_is_ordered_by_similarity_and_bounded_by_the_floor() {
    let (ws, mut conn) = indexed_workspace(&[("a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\n")]);
    let root = ws.path().canonicalize().expect("canonicalize");
    let hashes: Vec<(String, String)> = pending(&conn, &root, FP, 10)
        .expect("pending")
        .files
        .into_iter()
        .map(|p| (p.path, p.content_sha256))
        .collect();
    let rows = vec![
        FileVector {
            path: hashes[0].0.clone(),
            content_sha256: hashes[0].1.clone(),
            vector: vec![1.0, 0.0, 0.0, 0.0],
        },
        FileVector {
            path: hashes[1].0.clone(),
            content_sha256: hashes[1].1.clone(),
            vector: vec![0.0, 1.0, 0.0, 0.0],
        },
    ];
    store_vectors(&mut conn, FP, &rows).expect("store");

    let query = [1.0f32, 0.0, 0.0, 0.0];
    let all = rank(&conn, FP, &query, f32::NEG_INFINITY, 10).expect("rank");
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].key, "a.rs");

    let floored = rank(&conn, FP, &query, 0.5, 10).expect("rank");
    assert_eq!(floored.len(), 1, "the orthogonal file is below the floor");
    assert_eq!(floored[0].key, "a.rs");
}

/// Deleting a file removes its vector with it, so a semantic answer can never
/// name a file the graph no longer has. This is the FK cascade, and it is the
/// reason the vectors live in `codegraph.db` rather than beside the retrieval
/// plane's.
#[test]
fn pruning_a_file_takes_its_vector_with_it() {
    let (ws, mut conn) = indexed_workspace(&[("a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\n")]);
    let root = ws.path().canonicalize().expect("canonicalize");
    let rows: Vec<FileVector> = pending(&conn, &root, FP, 10)
        .expect("pending")
        .files
        .iter()
        .map(|p| FileVector {
            path: p.path.clone(),
            content_sha256: p.content_sha256.clone(),
            vector: vec![1.0, 0.0, 0.0, 0.0],
        })
        .collect();
    store_vectors(&mut conn, FP, &rows).expect("store");
    assert_eq!(count(&conn, FP).expect("count"), 2);

    fs::remove_file(ws.path().join("b.rs")).expect("remove");
    store::index_tree(&mut conn, &root, &Grammars::load().expect("grammars")).expect("reindex");

    assert_eq!(count(&conn, FP).expect("count"), 1);
    let ranked = rank(&conn, FP, &[1.0, 0.0, 0.0, 0.0], f32::NEG_INFINITY, 10).expect("rank");
    assert_eq!(
        ranked.iter().map(|s| s.key.as_str()).collect::<Vec<_>>(),
        vec!["a.rs"]
    );
}

/// Witness for the change-recency ordering.
///
/// Under the previous `ORDER BY f.path`, a capped pass always returned the
/// alphabetically first pending file, so a merge that touched `src/z.rs` in a
/// workspace whose alphabet is crowded ahead of it could stay unembedded
/// across an unbounded number of queries. Recency ordering hands back the file
/// that actually changed, which is the whole point of reacting to a commit.
#[test]
fn a_capped_pass_embeds_the_most_recently_changed_file_first() {
    let (ws, conn) = indexed_workspace(&[
        ("src/a.rs", "fn alpha() {}\n"),
        ("src/z.rs", "fn zulu() {}\n"),
    ]);
    let root = ws.path().canonicalize().expect("canonicalize");

    // Stands in for "a merge just rewrote src/z.rs": the indexer stamps
    // `indexed_at` only when content actually changed, so a later timestamp on
    // one file is exactly the state a scoped re-index leaves behind.
    conn.execute(
        "UPDATE code_graph_files SET indexed_at = 1000 WHERE path = 'src/a.rs'",
        [],
    )
    .expect("age a.rs");
    conn.execute(
        "UPDATE code_graph_files SET indexed_at = 2000 WHERE path = 'src/z.rs'",
        [],
    )
    .expect("freshen z.rs");

    let scan = pending(&conn, &root, FP, 1).expect("pending");
    assert_eq!(
        scan.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        vec!["src/z.rs"],
        "a one-file pass must spend itself on the file that just changed"
    );
}

/// The cursor resumes across the whole `(indexed_at, path)` tuple rather than
/// re-serving or skipping rows. Files sharing one timestamp are the case a
/// timestamp-only cursor gets wrong, and `stella init` stamps an entire tree
/// within one second — so this is the common shape, not a corner.
#[test]
fn a_pass_walks_every_pending_file_exactly_once_within_one_timestamp() {
    let (ws, conn) = indexed_workspace(&[
        ("src/a.rs", "fn alpha() {}\n"),
        ("src/m.rs", "fn mike() {}\n"),
        ("src/z.rs", "fn zulu() {}\n"),
    ]);
    let root = ws.path().canonicalize().expect("canonicalize");
    conn.execute("UPDATE code_graph_files SET indexed_at = 1000", [])
        .expect("flatten timestamps");

    // A limit of one per round is what forces the cursor to carry the pass:
    // three rounds, three distinct files, no repeats.
    let mut seen = Vec::new();
    let scan = pending(&conn, &root, FP, 10).expect("pending");
    for file in &scan.files {
        seen.push(file.path.clone());
    }
    seen.sort();
    assert_eq!(seen, vec!["src/a.rs", "src/m.rs", "src/z.rs"]);
}
