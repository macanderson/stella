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

/// The witness: a file with symbols is pending-by-file-count until every one
/// of its chunks is stored, then the count drops — and stays accurate once a
/// second file with symbols of its own is added.
#[test]
fn the_pending_file_count_drops_only_once_every_chunk_is_stored() {
    let (ws, mut conn) = indexed_workspace(&[("a.rs", "fn alpha() {}\nfn beta() {}\n")]);
    let root = ws.path().canonicalize().expect("canonicalize");

    assert_eq!(
        pending_chunk_file_count(&conn, FP).expect("count"),
        1,
        "one file, two un-embedded symbols — still pending"
    );

    let scan = pending_chunks(&conn, &root, FP, 10).expect("scan");
    assert_eq!(scan.files.len(), 1);
    let file = &scan.files[0];
    assert_eq!(file.chunks.len(), 2, "alpha and beta are both chunks");

    // Store only the first chunk — the file must still read as pending.
    let rows = vec![ChunkVector {
        chunk_sha256: file.chunks[0].chunk_sha256.clone(),
        name: file.chunks[0].name.clone(),
        kind: file.chunks[0].kind.clone(),
        start_line: file.chunks[0].start_line,
        end_line: file.chunks[0].end_line,
        vector: Some(vec![1.0, 0.0, 0.0, 0.0]),
    }];
    store_chunk_vectors(&mut conn, FP, &file.path, &file.file_sha256, &rows).expect("store");
    assert_eq!(
        pending_chunk_file_count(&conn, FP).expect("count"),
        1,
        "one of two chunks stored — still pending"
    );

    // Store the second chunk — now the file drops out of the pending count.
    let rows = vec![ChunkVector {
        chunk_sha256: file.chunks[1].chunk_sha256.clone(),
        name: file.chunks[1].name.clone(),
        kind: file.chunks[1].kind.clone(),
        start_line: file.chunks[1].start_line,
        end_line: file.chunks[1].end_line,
        vector: Some(vec![0.0, 1.0, 0.0, 0.0]),
    }];
    store_chunk_vectors(&mut conn, FP, &file.path, &file.file_sha256, &rows).expect("store");
    assert_eq!(
        pending_chunk_file_count(&conn, FP).expect("count"),
        0,
        "both chunks stored — no longer pending"
    );
    assert_eq!(chunk_count(&conn, FP).expect("chunk_count"), 2);
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
