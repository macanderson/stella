use super::*;
use crate::parse::Grammars;
use std::fs;
use tempfile::tempdir;

const FP: &str = "test-model@1/4/l2";

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

/// Store every chunk a scan handed back for one file, as
/// [`store_chunk_vectors`]'s contract requires — one file per call, the whole
/// set — and give each a distinguishable vector.
fn store_whole_file(conn: &mut Connection, file: &PendingChunkFile) {
    let rows: Vec<ChunkVector> = file
        .chunks
        .iter()
        .enumerate()
        .map(|(index, chunk)| ChunkVector {
            chunk_sha256: chunk.chunk_sha256.clone(),
            name: chunk.name.clone(),
            kind: chunk.kind.clone(),
            start_line: chunk.start_line,
            end_line: chunk.end_line,
            vector: Some(vec![index as f32, 1.0, 0.0, 0.0]),
        })
        .collect();
    store_chunk_vectors(conn, FP, &file.path, &file.file_sha256, &rows).expect("store");
}

/// The witness: a file with symbols is pending until its chunks are stored,
/// stops being pending once they are, and **becomes pending again when its
/// content changes**.
///
/// The last leg is the one that keeps [`NEEDS_CHUNK_PASS`] honest. The
/// predicate is a coverage marker — "does this file carry any chunk vector
/// stamped with its *current* hash" — so the content-change leg is what proves
/// the marker is keyed to content and not merely to existence, which is the
/// only way it could go stale silently.
///
/// It used to store one of two chunks and assert the file was still pending.
/// That state is unreachable: [`store_chunk_vectors`] documents "one file per
/// call, one transaction", the single production caller
/// (`stella_tools::search::embed_and_store_chunk_file`) always passes the
/// file's complete row set, and splitting a file across two calls is precisely
/// what that doc says the sweep would corrupt. Pinning the count's behaviour in
/// a state the writer forbids pinned the old predicate's arithmetic rather than
/// the contract (#3128).
#[test]
fn the_pending_file_count_tracks_whole_file_coverage() {
    let (ws, mut conn) = indexed_workspace(&[("a.rs", "fn alpha() {}\nfn beta() {}\n")]);
    let root = ws.path().canonicalize().expect("canonicalize");

    assert_eq!(
        pending_chunk_file_count(&conn, FP).expect("count"),
        1,
        "one file, two un-embedded symbols — still pending"
    );

    let scan = pending_chunks(&conn, &root, FP, 10).expect("scan");
    assert_eq!(scan.files.len(), 1);
    assert_eq!(
        scan.files[0].chunks.len(),
        2,
        "alpha and beta are both chunks"
    );
    store_whole_file(&mut conn, &scan.files[0]);

    assert_eq!(
        pending_chunk_file_count(&conn, FP).expect("count"),
        0,
        "the file's chunks are stored — no longer pending"
    );
    assert_eq!(chunk_count(&conn, FP).expect("chunk_count"), 2);
    assert!(
        pending_chunks(&conn, &root, FP, 10)
            .expect("scan")
            .files
            .is_empty(),
        "the page of work must agree with the count"
    );

    // Edit the file: its content hash moves, so the stored vectors no longer
    // describe it and it must re-enter the pending set.
    fs::write(
        ws.path().join("a.rs"),
        "fn alpha() {}\nfn beta() {}\nfn gamma() {}\n",
    )
    .expect("rewrite");
    store::index_tree(&mut conn, &root, &Grammars::load().expect("grammars")).expect("reindex");
    assert_eq!(
        pending_chunk_file_count(&conn, FP).expect("count"),
        1,
        "an edited file is not covered by vectors stamped with its old content hash"
    );
}

/// The graph-level witness for #3128: two symbols whose rendered text is
/// byte-identical store one row for two symbols, and the file must still leave
/// the pending set once it has been embedded.
///
/// Before the coverage marker this was unreachable — the raw symbol count
/// exceeded the stored row count forever, so the file was re-selected on every
/// pass no matter how many times it had been correctly processed, and every
/// consumer of "how much is left" reported a number that could never reach
/// zero (#3124).
#[test]
fn a_file_whose_symbols_render_identical_text_still_finishes() {
    let (ws, mut conn) = indexed_workspace(&[(
        "dupes.rs",
        "mod a {\n    pub fn id() -> u8 { 0 }\n}\nmod b {\n    pub fn id() -> u8 { 0 }\n}\n",
    )]);
    let root = ws.path().canonicalize().expect("canonicalize");

    let scan = pending_chunks(&conn, &root, FP, 10).expect("scan");
    assert_eq!(scan.files.len(), 1);
    let symbols = scan.files[0].chunks.len();
    store_whole_file(&mut conn, &scan.files[0]);

    // The premise: the collision really happened, so this is not a file that
    // would have finished under the old count comparison anyway.
    let stored = chunk_count(&conn, FP).expect("chunk_count");
    assert!(
        stored < symbols,
        "fixture no longer collides ({stored} rows for {symbols} symbols) — the old predicate \
         would have been satisfied and this witness proves nothing"
    );
    assert_eq!(
        pending_chunk_file_count(&conn, FP).expect("count"),
        0,
        "a fully embedded file must leave the pending set even when its symbols collide"
    );
}

/// A different fingerprint's vectors do not satisfy this one's pending count
/// — the same discipline `vectors::tests` pins for whole-file vectors, so a
/// model swap does not silently under-report unembedded work.
#[test]
fn a_different_fingerprint_still_counts_as_pending() {
    let (ws, mut conn) = indexed_workspace(&[("a.rs", "fn alpha() {}\n")]);
    let root = ws.path().canonicalize().expect("canonicalize");
    let scan = pending_chunks(&conn, &root, FP, 10).expect("scan");
    let file = &scan.files[0];
    let rows = vec![ChunkVector {
        chunk_sha256: file.chunks[0].chunk_sha256.clone(),
        name: file.chunks[0].name.clone(),
        kind: file.chunks[0].kind.clone(),
        start_line: file.chunks[0].start_line,
        end_line: file.chunks[0].end_line,
        vector: Some(vec![1.0, 0.0, 0.0, 0.0]),
    }];
    store_chunk_vectors(&mut conn, FP, &file.path, &file.file_sha256, &rows).expect("store");

    assert_eq!(pending_chunk_file_count(&conn, FP).expect("count"), 0);
    assert_eq!(
        pending_chunk_file_count(&conn, "other-model@1/4/l2").expect("count"),
        1,
        "a fingerprint that has embedded nothing must still see the file as pending"
    );
}
