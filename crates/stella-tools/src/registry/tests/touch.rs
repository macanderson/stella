//! File-touch telemetry: what `record_touch` writes to the ledger for each
//! CRUD shape, and that concurrent tools lose no events.

use super::*;

#[tokio::test]
async fn touch_read_only_file_records_one_r_with_zero_deltas() {
    let (dir, reg) = telemetry_fixture();
    std::fs::write(dir.path().join("notes.txt"), "one\ntwo\n").unwrap();

    exec_ok(&reg, "read_file", serde_json::json!({"path": "notes.txt"})).await;

    let payload = reg.file_touch_telemetry();
    payload.validate().unwrap();
    assert_eq!(payload.files_touched.len(), 1);
    let record = &payload.files_touched[0];
    assert_eq!(record.path, "notes.txt");
    assert_eq!(record.crud_events, vec![FileOp::Read]);
    assert_eq!((record.lines_added, record.lines_removed), (0, 0));
    assert_eq!(record.events.len(), 1);
    assert_eq!(
        (record.events[0].lines_added, record.events[0].lines_removed),
        (0, 0)
    );
}

#[tokio::test]
async fn touch_multiple_reads_are_one_record_with_multiple_events() {
    let (dir, reg) = telemetry_fixture();
    std::fs::write(dir.path().join("notes.txt"), "one\ntwo\n").unwrap();

    for _ in 0..3 {
        exec_ok(&reg, "read_file", serde_json::json!({"path": "notes.txt"})).await;
    }

    let payload = reg.file_touch_telemetry();
    payload.validate().unwrap();
    assert_eq!(payload.files_touched.len(), 1, "one record per file");
    let record = &payload.files_touched[0];
    assert_eq!(record.crud_events, vec![FileOp::Read], "crud deduplicated");
    assert_eq!(record.events.len(), 3, "audit log never deduplicated");
}

#[tokio::test]
async fn touch_create_counts_the_new_files_full_line_count() {
    let (_dir, reg) = telemetry_fixture();

    exec_ok(
        &reg,
        "write_file",
        serde_json::json!({"path": "src/new.rs", "content": "a\nb\nc\n"}),
    )
    .await;

    let payload = reg.file_touch_telemetry();
    payload.validate().unwrap();
    let record = &payload.files_touched[0];
    assert_eq!(record.path, "src/new.rs");
    assert_eq!(record.crud_events, vec![FileOp::Create]);
    assert_eq!((record.lines_added, record.lines_removed), (3, 0));
}

#[tokio::test]
async fn touch_update_counts_the_line_diff() {
    let (dir, reg) = telemetry_fixture();
    std::fs::write(dir.path().join("code.rs"), "a\nb\nc\n").unwrap();

    // edit_file: "b" becomes two lines — +2 −1.
    exec_ok(
        &reg,
        "edit_file",
        serde_json::json!({"path": "code.rs", "old_string": "b", "new_string": "x\ny"}),
    )
    .await;
    // write_file over an existing file is also an update — the diff of
    // pre vs post, not the full content: "a\nx\ny\nc\n" → "a\nc\n" is −2.
    exec_ok(
        &reg,
        "write_file",
        serde_json::json!({"path": "code.rs", "content": "a\nc\n"}),
    )
    .await;

    let payload = reg.file_touch_telemetry();
    payload.validate().unwrap();
    let record = &payload.files_touched[0];
    assert_eq!(record.crud_events, vec![FileOp::Update]);
    assert_eq!(record.events.len(), 2);
    assert_eq!(
        (record.events[0].lines_added, record.events[0].lines_removed),
        (2, 1)
    );
    assert_eq!(
        (record.events[1].lines_added, record.events[1].lines_removed),
        (0, 2)
    );
    assert_eq!((record.lines_added, record.lines_removed), (2, 3));
}

#[tokio::test]
async fn touch_delete_counts_the_full_pre_deletion_line_count() {
    let (dir, reg) = telemetry_fixture();
    std::fs::write(dir.path().join("doomed.txt"), "1\n2\n3\n4\n5\n").unwrap();

    exec_ok(
        &reg,
        "delete_file",
        serde_json::json!({"path": "doomed.txt"}),
    )
    .await;

    let payload = reg.file_touch_telemetry();
    payload.validate().unwrap();
    let record = &payload.files_touched[0];
    assert_eq!(record.crud_events, vec![FileOp::Delete]);
    assert_eq!((record.lines_added, record.lines_removed), (0, 5));
}

#[tokio::test]
async fn touch_read_update_delete_keeps_order_and_dedupes_crud() {
    let (dir, reg) = telemetry_fixture();
    std::fs::write(dir.path().join("f.rs"), "a\nb\nc\n").unwrap();

    exec_ok(
        &reg,
        "read_file",
        serde_json::json!({"path": "f.rs", "reason": "inspect before edit"}),
    )
    .await;
    exec_ok(
        &reg,
        "edit_file",
        serde_json::json!({
            "path": "f.rs", "old_string": "b", "new_string": "x\ny",
            "reason": "split b into x and y"
        }),
    )
    .await;
    exec_ok(
        &reg,
        "delete_file",
        serde_json::json!({"path": "f.rs", "reason": "no longer needed"}),
    )
    .await;

    let payload = reg.file_touch_telemetry();
    payload.validate().unwrap();
    assert_eq!(payload.files_touched.len(), 1);
    let record = &payload.files_touched[0];
    assert_eq!(
        record.crud_events,
        vec![FileOp::Read, FileOp::Update, FileOp::Delete],
        "deduplicated, first-occurrence order"
    );
    let ops: Vec<FileOp> = record.events.iter().map(|e| e.event).collect();
    assert_eq!(ops, vec![FileOp::Read, FileOp::Update, FileOp::Delete]);
    let reasons: Vec<&str> = record.events.iter().map(|e| e.reason.as_str()).collect();
    assert_eq!(
        reasons,
        vec![
            "inspect before edit",
            "split b into x and y",
            "no longer needed"
        ]
    );
    // R: 0/0. U: +2 −1. D: −4 (the post-edit file "a\nx\ny\nc\n").
    assert_eq!((record.lines_added, record.lines_removed), (2, 5));
}

#[tokio::test]
async fn touch_equivalent_path_spellings_aggregate_into_one_record() {
    let (dir, reg) = telemetry_fixture();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();

    for spelling in [
        "src/main.rs",
        "src/./main.rs",
        "src/../src/main.rs",
        "./src/main.rs",
    ] {
        exec_ok(&reg, "read_file", serde_json::json!({"path": spelling})).await;
    }

    let payload = reg.file_touch_telemetry();
    payload.validate().unwrap();
    assert_eq!(
        payload.files_touched.len(),
        1,
        "equivalent spellings must aggregate: {payload:?}"
    );
    let record = &payload.files_touched[0];
    assert_eq!(record.path, "src/main.rs");
    assert_eq!(record.events.len(), 4);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn touch_concurrent_tools_lose_no_events_and_keep_totals_exact() {
    let (dir, reg) = telemetry_fixture();
    for i in 0..4 {
        std::fs::write(dir.path().join(format!("read{i}.txt")), "x\n").unwrap();
    }
    let reg = std::sync::Arc::new(reg);

    let mut handles = Vec::new();
    // 32 concurrent reads across 4 files (read-only tools are the ones
    // the engine actually parallelizes)...
    for i in 0..32 {
        let reg = reg.clone();
        handles.push(tokio::spawn(async move {
            let input = serde_json::json!({"path": format!("read{}.txt", i % 4)});
            let out = reg.execute("read_file", &input).await;
            assert!(!out.is_error(), "{out:?}");
        }));
    }
    // ...plus 8 concurrent creates of distinct files.
    for i in 0..8 {
        let reg = reg.clone();
        handles.push(tokio::spawn(async move {
            let input = serde_json::json!({
                "path": format!("new{i}.txt"),
                "content": "l1\nl2\n"
            });
            let out = reg.execute("write_file", &input).await;
            assert!(!out.is_error(), "{out:?}");
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }

    let payload = reg.file_touch_telemetry();
    payload.validate().unwrap();
    assert_eq!(payload.files_touched.len(), 12, "4 read + 8 created files");
    let mut read_events = 0;
    let mut created_lines = 0;
    for record in &payload.files_touched {
        if record.path.starts_with("read") {
            assert_eq!(record.crud_events, vec![FileOp::Read]);
            read_events += record.events.len();
        } else {
            assert_eq!(record.crud_events, vec![FileOp::Create]);
            assert_eq!(record.events.len(), 1);
            created_lines += record.lines_added;
        }
    }
    assert_eq!(read_events, 32, "no read event may be lost");
    assert_eq!(created_lines, 16, "8 files × 2 lines");
}

#[tokio::test]
async fn touch_failed_operations_record_nothing() {
    let (dir, reg) = telemetry_fixture();
    std::fs::write(dir.path().join("f.rs"), "a\n").unwrap();

    // Missing file, escaping path, and a failed edit precondition.
    for (tool, input) in [
        ("read_file", serde_json::json!({"path": "ghost.txt"})),
        ("read_file", serde_json::json!({"path": "../../etc/passwd"})),
        ("delete_file", serde_json::json!({"path": "ghost.txt"})),
        (
            "edit_file",
            serde_json::json!({"path": "f.rs", "old_string": "nope", "new_string": "x"}),
        ),
    ] {
        let out = reg.execute(tool, &input).await;
        assert!(out.is_error(), "{tool} {input} should fail");
    }

    assert!(
        reg.file_touch_telemetry().files_touched.is_empty(),
        "failed operations must not create CRUD events"
    );
}

#[tokio::test]
async fn touch_reason_defaults_when_omitted() {
    let (dir, reg) = telemetry_fixture();
    std::fs::write(dir.path().join("f.rs"), "a\n").unwrap();

    exec_ok(&reg, "read_file", serde_json::json!({"path": "f.rs"})).await;

    let payload = reg.file_touch_telemetry();
    payload.validate().unwrap();
    let event = &payload.files_touched[0].events[0];
    assert!(
        !event.reason.is_empty(),
        "schema requires a non-empty reason"
    );
}

#[tokio::test]
async fn files_touched_compact_view_still_uses_crud_display_order() {
    // The legacy (path, "RU") view feeds the TUI panel and episodic
    // memory: letters stay in C,R,U,D display order even though the
    // telemetry payload orders by first occurrence.
    let (dir, reg) = telemetry_fixture();
    std::fs::write(dir.path().join("f.rs"), "a\nb\n").unwrap();

    exec_ok(
        &reg,
        "edit_file",
        serde_json::json!({"path": "f.rs", "old_string": "a", "new_string": "z"}),
    )
    .await;
    exec_ok(&reg, "read_file", serde_json::json!({"path": "f.rs"})).await;

    assert_eq!(
        reg.files_touched(),
        vec![("f.rs".to_string(), "RU".to_string())]
    );
    assert_eq!(
        reg.file_touch_telemetry().files_touched[0].crud_events,
        vec![FileOp::Update, FileOp::Read],
        "payload keeps first-occurrence order"
    );
}
