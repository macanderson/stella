use super::*;
use crate::parse::Grammars;
use std::fs;
use tempfile::tempdir;

fn canon(ws: &tempfile::TempDir) -> PathBuf {
    ws.path().canonicalize().expect("canonicalize")
}

/// The read path must not run DDL. `load_storage_snapshot` — the
/// pre-write gate — opens the store on every write an agent proposes, and
/// `open` used to replay the whole `MIGRATION` batch there.
#[test]
fn open_read_runs_no_ddl() {
    let dbdir = tempdir().unwrap();
    let db = dbdir.path().join("codegraph.db");

    let reader = open_read(&db).unwrap();
    let tables: i64 = reader
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' \
                 AND name LIKE 'code_graph_%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0, "open_read must not create the schema");
    // Reads against the unmigrated file fail, and the caller's
    // `unwrap_or_default` turns that into the same empty snapshot a missing
    // store yields.
    assert!(storage_rows(&reader).is_err());
    drop(reader);

    // The writer's open still migrates.
    let writer = open(&db).unwrap();
    let tables: i64 = writer
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' \
                 AND name LIKE 'code_graph_%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(tables > 0, "open must migrate");
    assert!(storage_rows(&writer).unwrap().is_empty());
}

fn user_version(conn: &Connection) -> i64 {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("read user_version")
}

/// #617: the writer stamps, a reopen accepts the stamp it finds, and the
/// read path never writes one.
#[test]
fn the_writer_stamps_the_schema_version_and_the_reader_does_not() {
    let dbdir = tempdir().unwrap();
    let db = dbdir.path().join("codegraph.db");

    let reader = open_read(&db).unwrap();
    assert_eq!(user_version(&reader), 0, "the read path must not stamp");
    drop(reader);

    let writer = open(&db).unwrap();
    assert_eq!(user_version(&writer), SCHEMA_VERSION);
    drop(writer);

    let reopened = open(&db).unwrap();
    assert_eq!(user_version(&reopened), SCHEMA_VERSION, "stamp persists");
}

/// The in-the-field case the policy is written for: a `codegraph.db`
/// created before the stamp existed. Its rows must survive the upgrade —
/// stamped, never rebuilt.
#[test]
fn an_unstamped_store_in_the_field_is_stamped_not_rebuilt() {
    let ws = tempdir().unwrap();
    let dbdir = tempdir().unwrap();
    let root = canon(&ws);
    let db = dbdir.path().join("codegraph.db");
    fs::write(root.join("x.py"), "def foo():\n    pass\n").unwrap();
    let grammars = Grammars::load().unwrap();
    let mut conn = open(&db).unwrap();
    index_tree(&mut conn, &root, &grammars).unwrap();
    let indexed: i64 = conn
        .query_row("SELECT count(*) FROM code_graph_symbols", [], |r| r.get(0))
        .unwrap();
    assert!(indexed > 0, "the fixture must have indexed something");
    // Rewind the header to what a pre-#617 store looks like on disk.
    conn.pragma_update(None, "user_version", 0i64).unwrap();
    drop(conn);

    let reopened = open(&db).unwrap();

    assert_eq!(user_version(&reopened), SCHEMA_VERSION);
    let after: i64 = reopened
        .query_row("SELECT count(*) FROM code_graph_symbols", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, indexed, "field rows must survive the stamp");
}

#[test]
fn a_store_from_a_newer_stella_is_refused() {
    let dbdir = tempdir().unwrap();
    let db = dbdir.path().join("codegraph.db");
    let conn = open(&db).unwrap();
    conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    drop(conn);

    let error = open(&db).expect_err("a newer store must fail closed");
    assert!(
        matches!(&error, GraphError::Schema(message) if message.contains("codegraph.db")),
        "unexpected error: {error}"
    );
}

/// The schema declares `REFERENCES … ON DELETE CASCADE` on three tables;
/// SQLite ignores every one of them unless the pragma is set per
/// connection. Pinned on both opens (#617).
#[test]
fn foreign_keys_are_enforced_on_both_opens() {
    let dbdir = tempdir().unwrap();
    let db = dbdir.path().join("codegraph.db");
    for conn in [open(&db).unwrap(), open_read(&db).unwrap()] {
        let on: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(on, 1, "foreign key enforcement must be on");
    }
    let conn = open(&db).unwrap();
    let orphan = conn.execute(
        "INSERT INTO code_graph_symbols (file_id, name, kind, start_line, end_line) \
             VALUES (9999, 'x', 'function', 1, 1)",
        [],
    );
    assert!(orphan.is_err(), "an orphan row must not be representable");
}

#[test]
fn byte_identical_content_is_never_reparsed() {
    let ws = tempdir().unwrap();
    let dbdir = tempdir().unwrap();
    let root = canon(&ws);
    let db = dbdir.path().join("context.db");
    fs::write(root.join("x.py"), "def foo():\n    pass\n").unwrap();
    let grammars = Grammars::load().unwrap();
    let mut conn = open(&db).unwrap();

    let first = index_tree(&mut conn, &root, &grammars).unwrap();
    assert_eq!(first.files_parsed, 1);

    // Second pass over unchanged content: zero parses (L-C2).
    let second = index_tree(&mut conn, &root, &grammars).unwrap();
    assert_eq!(
        second.files_parsed, 0,
        "unchanged content must not re-parse"
    );
    assert_eq!(second.files_skipped_unchanged, 1);

    // Change the bytes → it parses again.
    fs::write(root.join("x.py"), "def foo():\n    return 1\n").unwrap();
    let third = index_tree(&mut conn, &root, &grammars).unwrap();
    assert_eq!(third.files_parsed, 1);
}

#[test]
fn busiest_file_picks_the_most_connected_file() {
    let ws = tempdir().unwrap();
    let dbdir = tempdir().unwrap();
    let root = canon(&ws);
    let db = dbdir.path().join("context.db");
    // `hub.rs` carries three symbols; `leaf.rs` carries one. The busiest
    // file is the one with the highest symbol+import degree.
    fs::write(
        root.join("hub.rs"),
        "pub fn a() {}\npub fn b() {}\npub struct C;\n",
    )
    .unwrap();
    fs::write(root.join("leaf.rs"), "pub fn d() {}\n").unwrap();
    let grammars = Grammars::load().unwrap();
    let mut conn = open(&db).unwrap();
    index_tree(&mut conn, &root, &grammars).unwrap();

    assert_eq!(busiest_file(&conn).unwrap().as_deref(), Some("hub.rs"));
}

#[test]
fn busiest_file_is_none_on_an_empty_index() {
    let dbdir = tempdir().unwrap();
    let conn = open(&dbdir.path().join("context.db")).unwrap();
    assert_eq!(busiest_file(&conn).unwrap(), None);
}

#[test]
fn kill_during_indexing_leaves_a_consistent_store() {
    let ws = tempdir().unwrap();
    let dbdir = tempdir().unwrap();
    let root = canon(&ws);
    let db = dbdir.path().join("context.db");
    fs::write(root.join("a.rs"), "pub fn a() {}\n").unwrap();
    fs::write(root.join("b.rs"), "pub struct B;\n").unwrap();
    let grammars = Grammars::load().unwrap();

    // Consistent state A: two files indexed.
    let mut conn = open(&db).unwrap();
    let stats = index_tree(&mut conn, &root, &grammars).unwrap();
    assert_eq!(stats.files_parsed, 2);
    assert_eq!(file_count(&conn).unwrap(), 2);

    // Simulate a kill mid-batch: open a transaction, write a partial row,
    // then drop it WITHOUT committing (rusqlite rolls back on drop).
    {
        let tx = conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO code_graph_files(path, language, content_sha256, mtime_ns, indexed_at) \
                 VALUES('c.rs', 'rust', 'deadbeef', 0, 0)",
            [],
        )
        .unwrap();
        // no commit → rollback on drop here
    }
    drop(conn);

    // Reopen: the rolled-back batch left nothing behind.
    let conn2 = open(&db).unwrap();
    assert_eq!(
        file_count(&conn2).unwrap(),
        2,
        "an uncommitted batch must not persist"
    );
    drop(conn2);

    // And a re-index still completes cleanly against the consistent store.
    let mut conn3 = open(&db).unwrap();
    let reindex = index_tree(&mut conn3, &root, &grammars).unwrap();
    assert_eq!(reindex.files_parsed, 0);
    assert_eq!(reindex.files_skipped_unchanged, 2);
    assert_eq!(file_count(&conn3).unwrap(), 2);
}

/// A damaged store is not a fatal error for a rebuildable cache: `open`
/// quarantines the corrupt file (sidecars included) and hands back a fresh,
/// migrated store the next index can fill.
#[test]
fn a_corrupt_store_is_quarantined_and_rebuilt_on_open() {
    let ws = tempdir().unwrap();
    let dbdir = tempdir().unwrap();
    let root = canon(&ws);
    let db = dbdir.path().join("codegraph.db");
    fs::write(root.join("a.rs"), "pub fn a() {}\n").unwrap();
    let grammars = Grammars::load().unwrap();

    // A real, indexed store first — the quarantine must preserve it.
    let mut conn = open(&db).unwrap();
    index_tree(&mut conn, &root, &grammars).unwrap();
    assert_eq!(file_count(&conn).unwrap(), 1);
    drop(conn);

    // Overwrite the header so SQLite cannot read the image. The store is a
    // WAL database, so checkpoint first: otherwise the live pages sit in the
    // `-wal` the quarantine moves aside with it, and the "corrupt" main file
    // would be an empty one.
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
        drop(conn);
        use std::io::{Seek, SeekFrom, Write};
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(&db)
            .expect("open db for corruption");
        file.seek(SeekFrom::Start(0)).expect("seek");
        file.write_all(&[0x7f; 128]).expect("write garbage");
        file.sync_all().expect("sync");
    }

    // The open heals itself: the damaged file moves aside and a fresh store
    // answers in its place.
    let mut conn = open(&db).unwrap();
    assert_eq!(file_count(&conn).unwrap(), 0, "the rebuild starts empty");
    let quarantined: Vec<_> = fs::read_dir(dbdir.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("codegraph.db.corrupt-"))
        .collect();
    assert_eq!(
        quarantined.len(),
        1,
        "the corrupt store was moved aside, found: {quarantined:?}"
    );

    // And the fresh store indexes the tree end to end.
    let stats = index_tree(&mut conn, &root, &grammars).unwrap();
    assert_eq!(stats.files_parsed, 1);
    assert_eq!(file_count(&conn).unwrap(), 1);
}

#[test]
fn sql_files_produce_storage_rows_and_prune_with_their_file() {
    let ws = tempdir().unwrap();
    let dbdir = tempdir().unwrap();
    let root = canon(&ws);
    let db = dbdir.path().join("context.db");
    fs::write(
        root.join("001_init.sql"),
        "CREATE TABLE payments (\n\
                 id SERIAL PRIMARY KEY,\n\
                 amount NUMERIC(10,2) NOT NULL,\n\
                 user_id INTEGER REFERENCES users(id)\n\
             );\n\
             COMMENT ON COLUMN payments.amount IS 'Gross amount charged.';\n",
    )
    .unwrap();
    fs::write(
        root.join("002_alter.sql"),
        "ALTER TABLE payments ADD COLUMN refunded_at TIMESTAMP;\n",
    )
    .unwrap();
    let grammars = Grammars::load().unwrap();
    let mut conn = open(&db).unwrap();
    index_tree(&mut conn, &root, &grammars).unwrap();

    let rows = storage_rows(&conn).unwrap();
    assert_eq!(rows.len(), 1, "one relation expected: {rows:?}");
    let rel = &rows[0];
    assert_eq!(rel.address, "sql/default/payments");
    assert_eq!(rel.fields.len(), 4, "3 columns + 1 ALTER addition");
    let amount = rel.fields.iter().find(|f| f.name == "amount").unwrap();
    assert_eq!(amount.data_type.as_deref(), Some("NUMERIC(10,2)"));
    assert!(!amount.nullable);
    assert_eq!(amount.intent.as_deref(), Some("Gross amount charged."));
    let user_id = rel.fields.iter().find(|f| f.name == "user_id").unwrap();
    assert_eq!(user_id.references.as_deref(), Some("users"));
    assert!(rel.fields.iter().any(|f| f.name == "refunded_at"));

    // The ALTER's column prunes with its file (CASCADE); the CREATE stays.
    fs::remove_file(root.join("002_alter.sql")).unwrap();
    index_tree(&mut conn, &root, &grammars).unwrap();
    let rows = storage_rows(&conn).unwrap();
    assert_eq!(rows[0].fields.len(), 3);
    assert!(!rows[0].fields.iter().any(|f| f.name == "refunded_at"));
}

#[test]
fn schema_as_code_files_index_through_their_adapters() {
    let ws = tempdir().unwrap();
    let dbdir = tempdir().unwrap();
    let root = canon(&ws);
    let db = dbdir.path().join("context.db");
    // A grammar-less Prisma DSL file: reached by the walker, indexed by
    // the prisma adapter, no symbols expected.
    fs::write(
        root.join("schema.prisma"),
        "model Payment {\n  id Int @id\n  amount Decimal\n  @@map(\"payments\")\n}\n",
    )
    .unwrap();
    // A Drizzle table in TS: relational, so it shares the implicit sql
    // layer with DDL.
    fs::write(
        root.join("schema.ts"),
        "export const users = pgTable(\"users\", {\n\
                 id: serial(\"id\").primaryKey(),\n\
             });\n",
    )
    .unwrap();
    // A Mongoose collection in JS: its own storage technology, so it
    // lands in the implicit mongo layer.
    fs::write(
        root.join("session.js"),
        "const mongoose = require('mongoose');\n\
             const s = new mongoose.Schema({ token: String });\n\
             mongoose.model('Session', s);\n",
    )
    .unwrap();
    let grammars = Grammars::load().unwrap();
    let mut conn = open(&db).unwrap();
    index_tree(&mut conn, &root, &grammars).unwrap();

    let rows = storage_rows(&conn).unwrap();
    let payments = rows.iter().find(|r| r.name == "payments").unwrap();
    assert_eq!(payments.address, "sql/default/payments");
    assert_eq!(payments.fields.len(), 2);
    assert!(
        payments
            .source
            .as_deref()
            .unwrap()
            .starts_with("schema.prisma:"),
        "{:?}",
        payments.source
    );

    let users = rows.iter().find(|r| r.name == "users").unwrap();
    assert_eq!(users.layer, "sql", "drizzle shares the relational layer");

    let sessions = rows.iter().find(|r| r.name == "sessions").unwrap();
    assert_eq!(sessions.layer, "mongo");
    assert_eq!(sessions.kind, "collection");

    // Removing the prisma file prunes its relation like any source file.
    fs::remove_file(root.join("schema.prisma")).unwrap();
    index_tree(&mut conn, &root, &grammars).unwrap();
    let rows = storage_rows(&conn).unwrap();
    assert!(!rows.iter().any(|r| r.name == "payments"), "{rows:?}");
}

/// #335 B1: call sites persist per file, answer both reads (span and
/// reverse name), and prune with their file like every other row.
#[test]
fn call_sites_index_answer_both_reads_and_prune_with_their_file() {
    let ws = tempdir().unwrap();
    let dbdir = tempdir().unwrap();
    let root = canon(&ws);
    let db = dbdir.path().join("codegraph.db");
    fs::write(
        root.join("caller.rs"),
        "pub fn outer() {\n    helper();\n    helper();\n    other();\n}\n\
             pub fn separate() {\n    helper();\n}\n",
    )
    .unwrap();
    let grammars = Grammars::load().unwrap();
    let mut conn = open(&db).unwrap();
    let stats = index_tree(&mut conn, &root, &grammars).unwrap();
    assert_eq!(stats.calls, 4, "{stats:?}");
    assert_eq!(call_count(&conn).unwrap(), 4);

    // Span read: only the calls inside `outer`'s lines (1-5).
    let in_outer = calls_in_span(&conn, "caller.rs", 1, 5).unwrap();
    assert_eq!(in_outer.len(), 3, "{in_outer:?}");
    assert!(in_outer.iter().any(|c| c.callee == "other"));

    // Reverse name read, capped: `helper` is called three times.
    let helper_calls = calls_of_name(&conn, "helper", 50).unwrap();
    assert_eq!(helper_calls.len(), 3, "{helper_calls:?}");
    assert!(helper_calls.iter().all(|c| c.path == "caller.rs"));
    let capped = calls_of_name(&conn, "helper", 2).unwrap();
    assert_eq!(capped.len(), 2, "the limit bounds the reverse lookup");

    // Rows prune with the file (CASCADE), same as symbols and imports.
    fs::remove_file(root.join("caller.rs")).unwrap();
    index_tree(&mut conn, &root, &grammars).unwrap();
    assert_eq!(call_count(&conn).unwrap(), 0);
    assert!(calls_of_name(&conn, "helper", 50).unwrap().is_empty());
}

#[test]
fn deleted_files_are_pruned_on_reindex() {
    let ws = tempdir().unwrap();
    let dbdir = tempdir().unwrap();
    let root = canon(&ws);
    let db = dbdir.path().join("context.db");
    fs::write(root.join("keep.rs"), "pub fn keep() {}\n").unwrap();
    fs::write(root.join("drop.rs"), "pub fn gone() {}\n").unwrap();
    let grammars = Grammars::load().unwrap();
    let mut conn = open(&db).unwrap();
    index_tree(&mut conn, &root, &grammars).unwrap();
    assert_eq!(file_count(&conn).unwrap(), 2);

    fs::remove_file(root.join("drop.rs")).unwrap();
    let stats = index_tree(&mut conn, &root, &grammars).unwrap();
    assert_eq!(stats.files_pruned, 1);
    assert_eq!(file_count(&conn).unwrap(), 1);
    // The pruned file's symbols are gone (CASCADE).
    assert!(definitions(&conn, "gone").unwrap().is_empty());
    assert!(!definitions(&conn, "keep").unwrap().is_empty());
}

/// Issue #272: a minified bundle (one giant line, well past the byte
/// floor) never persists a row, while an ordinary file alongside it
/// indexes normally.
#[test]
fn minified_content_is_skipped_and_not_persisted() {
    let ws = tempdir().unwrap();
    let dbdir = tempdir().unwrap();
    let root = canon(&ws);
    let db = dbdir.path().join("context.db");
    let long_line = "function bigOne(){return 1;}".repeat(200);
    fs::write(root.join("bundle.js"), format!("{long_line}\n")).unwrap();
    fs::write(root.join("real.js"), "function real() { return 2; }\n").unwrap();
    let grammars = Grammars::load().unwrap();
    let mut conn = open(&db).unwrap();

    let stats = index_tree(&mut conn, &root, &grammars).unwrap();
    assert_eq!(stats.files_skipped_generated, 1, "{stats:?}");
    assert_eq!(
        file_count(&conn).unwrap(),
        1,
        "only the real file is indexed"
    );
    assert!(definitions(&conn, "bigOne").unwrap().is_empty());
    assert!(!definitions(&conn, "real").unwrap().is_empty());
}

/// The `*.min.*` filename convention is enough on its own — no minified
/// content shape required.
#[test]
fn min_dot_filename_convention_is_skipped_regardless_of_content_shape() {
    let ws = tempdir().unwrap();
    let dbdir = tempdir().unwrap();
    let root = canon(&ws);
    let db = dbdir.path().join("context.db");
    fs::write(root.join("app.min.js"), "function tiny() {}\n").unwrap();
    let grammars = Grammars::load().unwrap();
    let mut conn = open(&db).unwrap();

    let stats = index_tree(&mut conn, &root, &grammars).unwrap();
    assert_eq!(stats.files_skipped_generated, 1);
    assert!(definitions(&conn, "tiny").unwrap().is_empty());
}

/// Issue #272's retroactive-cleanup requirement: a file indexed normally
/// under an older version, then later declared `linguist-generated=true`
/// with its bytes completely unchanged, must still be pruned on the very
/// next pass — proving the exclusion check runs ahead of (and overrides)
/// the byte-compat skip rather than being hidden behind it forever.
#[test]
fn a_file_marked_generated_after_the_fact_is_pruned_even_with_unchanged_bytes() {
    let ws = tempdir().unwrap();
    let dbdir = tempdir().unwrap();
    let root = canon(&ws);
    let db = dbdir.path().join("context.db");
    fs::write(root.join("legacy.js"), "function legacyThing() {}\n").unwrap();
    let grammars = Grammars::load().unwrap();
    let mut conn = open(&db).unwrap();

    index_tree(&mut conn, &root, &grammars).unwrap();
    assert!(
        !definitions(&conn, "legacyThing").unwrap().is_empty(),
        "indexed normally before any .gitattributes rule exists"
    );

    // Mark it generated without touching its bytes at all.
    fs::write(
        root.join(".gitattributes"),
        "legacy.js linguist-generated=true\n",
    )
    .unwrap();
    let stats = index_tree(&mut conn, &root, &grammars).unwrap();

    assert_eq!(stats.files_skipped_generated, 1);
    assert!(
        definitions(&conn, "legacyThing").unwrap().is_empty(),
        "a stale pre-fix row must be retroactively pruned, not hidden behind the byte-compat skip"
    );
    assert_eq!(file_count(&conn).unwrap(), 0);
}

/// A file over [`MAX_INDEXABLE_BYTES`] is skipped without ever being read.
///
/// Nothing upstream bounded size: the walk denies by directory NAME and
/// `looks_minified` only catches long-LINE shapes, so a newline-per-line
/// giant with a recognized extension went straight into `fs::read` and
/// then tree-sitter, whose tree is a multiple of the source. The content
/// here is deliberately ordinary, well-formed, short-lined source — every
/// other exclusion signal says "index me", so only the size gate can be
/// what skips it.
#[test]
fn a_file_over_the_size_cap_is_skipped_and_never_parsed() {
    let ws = tempdir().unwrap();
    let dbdir = tempdir().unwrap();
    let root = canon(&ws);
    let db = dbdir.path().join("context.db");

    // Short lines, real syntax — indistinguishable from hand-written
    // source to every filter except the size cap.
    let mut huge = String::with_capacity(MAX_INDEXABLE_BYTES as usize + 4096);
    let mut n = 0u32;
    while (huge.len() as u64) <= MAX_INDEXABLE_BYTES {
        huge.push_str(&format!("function generated_{n}() {{ return {n}; }}\n"));
        n += 1;
    }
    fs::write(root.join("huge.js"), &huge).unwrap();
    fs::write(root.join("small.js"), "function keeper() {}\n").unwrap();

    let grammars = Grammars::load().unwrap();
    let mut conn = open(&db).unwrap();
    let stats = index_tree(&mut conn, &root, &grammars).unwrap();

    assert_eq!(stats.files_skipped_too_large, 1, "{stats:?}");
    assert_eq!(stats.files_parsed, 1, "only the small file is parsed");
    assert!(
        definitions(&conn, "generated_0").unwrap().is_empty(),
        "nothing from an oversize file may enter the index"
    );
    assert!(
        !definitions(&conn, "keeper").unwrap().is_empty(),
        "an oversize neighbour must not stop the rest of the pass"
    );
}

/// The retroactive half, mirroring the generated-exclusion case: a file
/// indexed while small and since grown past the cap must have its row
/// dropped, not left answering queries with symbols that no longer
/// describe the file.
#[test]
fn a_file_that_grows_past_the_cap_has_its_stale_row_pruned() {
    let ws = tempdir().unwrap();
    let dbdir = tempdir().unwrap();
    let root = canon(&ws);
    let db = dbdir.path().join("context.db");
    let path = root.join("grower.js");
    fs::write(&path, "function wasSmall() {}\n").unwrap();

    let grammars = Grammars::load().unwrap();
    let mut conn = open(&db).unwrap();
    index_tree(&mut conn, &root, &grammars).unwrap();
    assert!(
        !definitions(&conn, "wasSmall").unwrap().is_empty(),
        "indexed normally while under the cap"
    );

    let mut huge = String::from("function wasSmall() {}\n");
    let mut n = 0u32;
    while (huge.len() as u64) <= MAX_INDEXABLE_BYTES {
        huge.push_str(&format!("function grew_{n}() {{ return {n}; }}\n"));
        n += 1;
    }
    fs::write(&path, &huge).unwrap();
    let stats = index_tree(&mut conn, &root, &grammars).unwrap();

    assert_eq!(stats.files_skipped_too_large, 1);
    assert!(
        definitions(&conn, "wasSmall").unwrap().is_empty(),
        "a stale row from before the file grew must be pruned"
    );
    assert_eq!(file_count(&conn).unwrap(), 0);
}
