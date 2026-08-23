//! The eager embedding pass, chunk and whole-file: batching, progress, caps, dedup, unreadable files, and a dead backend (#4494 split of `../tests.rs`).
use super::*;

/// The witness for the eager `stella init` chunk pass (#3098): a fixture
/// covered in one call, so a search can rank by meaning on its very first
/// invocation.
#[tokio::test]
async fn one_eager_pass_embeds_every_pending_chunk_no_matter_how_many_files() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = write_fixture_at_the_real_workspace_path(workspace.path());

    let outcome = warm_chunk_vectors(&root, &ConceptEmbedder, 1_000).await;
    let ChunkWarmOutcome::Warmed {
        files_embedded,
        files_remaining,
        unreadable,
    } = outcome
    else {
        panic!("expected Warmed, got {outcome:?}");
    };
    assert_eq!(
        files_embedded,
        FIXTURE.len(),
        "every fixture file has symbols to chunk"
    );
    assert_eq!(files_remaining, 0, "nothing left pending after one pass");
    assert_eq!(unreadable, 0);

    // Idempotent: a second pass over an already-warm index embeds nothing new.
    let again = warm_chunk_vectors(&root, &ConceptEmbedder, 1_000).await;
    assert_eq!(
        again,
        ChunkWarmOutcome::Warmed {
            files_embedded: 0,
            files_remaining: 0,
            unreadable: 0,
        },
        "re-running against a fully warm index must be a no-op, not a re-embed"
    );
}

/// The witness for #3102 at the chunk rung: the eager pass reports its
/// cumulative file count as it commits, one report per file, so a long pass
/// can be narrated while it happens.
#[tokio::test]
async fn an_eager_chunk_pass_reports_progress_as_files_commit() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = write_fixture_at_the_real_workspace_path(workspace.path());

    let mut reported: Vec<usize> = Vec::new();
    let outcome =
        warm_chunk_vectors_with_progress(&root, &ConceptEmbedder, 1_000, &mut |files_embedded| {
            reported.push(files_embedded)
        })
        .await;
    let ChunkWarmOutcome::Warmed { files_embedded, .. } = outcome else {
        panic!("expected Warmed, got {outcome:?}");
    };
    assert_eq!(
        reported,
        (1..=files_embedded).collect::<Vec<_>>(),
        "one cumulative report per committed file"
    );
}

/// A capped pass says honestly what it left behind, rather than reporting
/// success over a partial index.
#[tokio::test]
async fn a_capped_pass_reports_what_it_left_pending() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = write_fixture_at_the_real_workspace_path(workspace.path());

    // FIXTURE has 4 files; cap the pass at fewer files than that.
    let outcome = warm_chunk_vectors(&root, &ConceptEmbedder, 2).await;
    let ChunkWarmOutcome::Warmed {
        files_embedded,
        files_remaining,
        ..
    } = outcome
    else {
        panic!("expected Warmed, got {outcome:?}");
    };
    assert_eq!(files_embedded, 2, "the pass must stop exactly at its cap");
    assert!(
        files_remaining > 0,
        "a capped pass over a wider fixture must say something is still pending"
    );
}

/// #3128: two symbols that render byte-identical text collapse to one stored
/// row, so a raw symbol-count-vs-stored-count pre-filter can never reach
/// equality for that file — this is the witness for the pass's no-progress
/// early exit.
#[tokio::test]
async fn a_file_whose_symbols_collide_on_rendered_text_does_not_spin_the_pass() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = {
        let file = workspace.path().join("src/dupes.rs");
        fs::create_dir_all(file.parent().expect("a parent")).expect("mkdir");
        // Two distinct symbols, same name, same kind, byte-identical body —
        // ordinary trait-stub-across-fixtures shape, not a contrived one.
        fs::write(
            &file,
            "mod a {\n    pub fn execute() { todo!() }\n}\n\
             mod b {\n    pub fn execute() { todo!() }\n}\n",
        )
        .expect("write");
        let root = workspace.path().canonicalize().expect("canonicalize");
        let opened = codegraph::open_or_build(&root).expect("open_or_build");
        opened.graph.shutdown();
        root
    };

    // First pass: real embedding work happens (however few distinct chunks
    // there are), and the pass must still terminate rather than hang.
    let outcome = warm_chunk_vectors(&root, &ConceptEmbedder, 1_000).await;
    assert!(
        matches!(outcome, ChunkWarmOutcome::Warmed { .. }),
        "expected Warmed, got {outcome:?}"
    );

    // Second pass over the now-embedded fixture: without the early exit this
    // would spend its entire `limit` re-visiting the file. It must instead
    // return quickly, having made no further progress.
    let again = warm_chunk_vectors(&root, &ConceptEmbedder, 1_000).await;
    let ChunkWarmOutcome::Warmed { files_embedded, .. } = again else {
        panic!("expected Warmed, got {again:?}");
    };
    assert!(
        files_embedded <= stella_graph::MAX_FILES_PER_CHUNK_PASS,
        "a fully-covered fixture must stop after at most one window, not spin \
         toward the cap: files_embedded={files_embedded}"
    );
}

/// **The witness for #3124.** A workspace with nothing left to embed must
/// stop describing itself as partial.
///
/// The premise is asserted first, so this cannot pass vacuously: if the
/// fixture ever stops colliding, a raw chunk-vs-symbol count comparison
/// would succeed too and the test would prove nothing.
#[tokio::test]
async fn a_fully_embedded_workspace_stops_calling_itself_partial() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let file = workspace.path().join("src/accessors.rs");
    fs::create_dir_all(file.parent().expect("a parent")).expect("mkdir");
    // Two distinct symbols, byte-identical rendered text — the ordinary
    // trait-accessor shape, not a contrived one.
    fs::write(
        &file,
        "mod a {\n    pub fn id() -> u8 { 0 }\n}\n\
         mod b {\n    pub fn id() -> u8 { 0 }\n}\n",
    )
    .expect("write");
    let root = workspace.path().canonicalize().expect("canonicalize");
    codegraph::open_or_build(&root)
        .expect("open_or_build")
        .graph
        .shutdown();

    let fingerprint = ConceptEmbedder.fingerprint().id();
    let warmed = warm_chunk_vectors(&root, &ConceptEmbedder, 1_000).await;
    assert!(
        matches!(warmed, ChunkWarmOutcome::Warmed { .. }),
        "expected Warmed, got {warmed:?}"
    );

    let opened = codegraph::open_or_build(&root).expect("open_or_build");
    crate::search::backfill::backfill_opened(&opened.graph, &ConceptEmbedder, &mut |_| {}).await;

    // The premise: the collision really happened, so a count comparison over
    // chunks-vs-symbols is unsatisfiable for this workspace.
    let symbols = opened.graph.symbol_count().expect("symbol_count");
    let chunks = opened
        .graph
        .embedded_chunk_count(&fingerprint)
        .expect("embedded_chunk_count");
    assert!(
        chunks < symbols,
        "fixture no longer collides ({chunks} chunk rows for {symbols} symbols) — a \
         `chunks >= symbols` comparison would have passed and this witness proves nothing"
    );

    let note = coverage_note(&opened.graph, &fingerprint);
    opened.graph.shutdown();
    assert!(
        note.is_none(),
        "a workspace with nothing left to embed still calls itself partial: {note:?}"
    );
}

/// The other half of the same contract: a workspace that genuinely has work
/// left must still say so. Without this, #3124 could be "fixed" by never
/// warning at all, which loses the disclosure #3117 added.
#[tokio::test]
async fn a_workspace_with_work_left_still_says_it_is_partial() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = write_fixture_at_the_real_workspace_path(workspace.path());

    // Indexed, nothing embedded at all — the state right after `stella init`
    // on a build with no embedder configured.
    let opened = codegraph::open_or_build(&root).expect("open_or_build");
    let note = coverage_note(&opened.graph, &ConceptEmbedder.fingerprint().id());
    opened.graph.shutdown();

    let note = note.expect("an unembedded workspace must disclose that it is partial");
    assert!(note.contains("PARTIAL INDEX"), "{note}");
}

/// The eager whole-file pass embeds the corpus and is idempotent — a second
/// pass over a warm index has nothing left to do.
#[tokio::test]
async fn an_eager_pass_embeds_the_corpus_and_is_idempotent() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = write_fixture_at_the_real_workspace_path(workspace.path());

    let outcome = warm_file_vectors(&root, &ConceptEmbedder, NO_FILE_CEILING).await;
    assert_eq!(
        outcome,
        WarmOutcome::Warmed {
            embedded: FIXTURE.len(),
            remaining: 0,
            unreadable: 0
        }
    );

    let again = warm_file_vectors(&root, &ConceptEmbedder, NO_FILE_CEILING).await;
    assert_eq!(
        again,
        WarmOutcome::Warmed {
            embedded: 0,
            remaining: 0,
            unreadable: 0
        }
    );
}

/// The witness for #3102, whole-file half: an eager pass reports its
/// cumulative count after every batch it commits.
#[tokio::test]
async fn an_eager_pass_reports_progress_per_batch() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = write_fixture_at_the_real_workspace_path(workspace.path());

    let mut reported: Vec<usize> = Vec::new();
    let outcome = warm_file_vectors_with_progress(
        &root,
        &ConceptEmbedder,
        NO_FILE_CEILING,
        &mut |embedded| reported.push(embedded),
    )
    .await;

    let WarmOutcome::Warmed { embedded, .. } = outcome else {
        panic!("the pass must warm: {outcome:?}");
    };
    assert_eq!(embedded, FIXTURE.len());
    assert!(
        !reported.is_empty(),
        "progress must be reported while the pass runs"
    );
    assert!(
        reported.windows(2).all(|pair| pair[0] < pair[1]),
        "cumulative counts must increase: {reported:?}"
    );
    assert_eq!(
        reported.last(),
        Some(&embedded),
        "the last report and the outcome agree"
    );
}

/// A repository past the cap gets a *stated* partial index — the number left
/// over is what the caller renders, so it can never be silent.
#[tokio::test]
async fn a_pass_that_hits_its_cap_reports_what_it_left() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = write_fixture_at_the_real_workspace_path(workspace.path());

    let outcome = warm_file_vectors(&root, &ConceptEmbedder, 1).await;
    assert_eq!(
        outcome,
        WarmOutcome::Warmed {
            embedded: 1,
            remaining: FIXTURE.len() - 1,
            unreadable: 0
        }
    );
}

/// #3016: an unreadable file at the head of the path order must not end the
/// pass. The window is sized in embeddable files, so the readable ones behind
/// it are still offered — and the leftover is reported as what it is rather
/// than as the cap's doing.
#[tokio::test]
async fn an_unreadable_file_does_not_stop_the_eager_pass_at_the_readable_ones() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = write_fixture_at_the_real_workspace_path(workspace.path());
    // Indexed, then gone — `src/hkey.rs` sorts ahead of `src/wire.rs`, so it
    // is what a window of one lands on first.
    fs::remove_file(root.join("src/hkey.rs")).expect("remove");

    let outcome = warm_file_vectors(&root, &ConceptEmbedder, 1).await;
    let WarmOutcome::Warmed {
        embedded,
        unreadable,
        ..
    } = outcome
    else {
        panic!("expected Warmed, got {outcome:?}");
    };
    assert_eq!(
        embedded, 1,
        "the readable file behind the deleted one must still be embedded"
    );
    assert_eq!(unreadable, 1, "the deleted file is reported as unreadable");
}

/// A dead backend is a report, never a panic and never a lost graph.
#[tokio::test]
async fn a_broken_backend_makes_the_eager_pass_a_named_failure() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = write_fixture_at_the_real_workspace_path(workspace.path());

    let WarmOutcome::Failed { embedded, reason } =
        warm_file_vectors(&root, &BrokenEmbedder, NO_FILE_CEILING).await
    else {
        panic!("a failing backend must report a failure");
    };
    assert_eq!(embedded, 0);
    assert!(reason.contains("upstream is down"), "{reason}");
}
