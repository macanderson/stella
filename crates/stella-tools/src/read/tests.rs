
use super::*;

/// A bare execution context rooted at `root` — every file-tool test
/// drives the tool through one, since `Tool::execute` takes the
/// context rather than the bare root path it used to (#3284).
fn cx(root: impl AsRef<std::path::Path>) -> crate::ctx::ToolCtx {
    crate::ctx::ToolCtx::bare(root.as_ref().to_path_buf())
}

/// #3145: an input the tool could not read is classified
/// [`stella_protocol::ErrorClass::InvalidInput`] — the model's mistake,
/// excluded from the tool's own error rate — with the message bytes
/// unchanged from the pre-class wording.
#[tokio::test]
async fn missing_path_is_classified_invalid_input() {
    use stella_protocol::ErrorClass;
    let result = ReadFile::default()
        .execute(&serde_json::json!({}), &cx(std::env::temp_dir()))
        .await;
    let ToolOutput::Error { message, class } = result else {
        panic!("expected an error for a missing required field");
    };
    assert_eq!(class, Some(ErrorClass::InvalidInput));
    assert_eq!(message, "missing required field `path`");
}

#[tokio::test]
async fn reads_file_with_line_numbers() {
    let dir = std::env::temp_dir();
    let path = format!("stella_test_read_{}.txt", std::process::id());
    let full = dir.join(&path);
    tokio::fs::write(&full, "line one\nline two\nline three\n")
        .await
        .unwrap();

    let result = ReadFile::default()
        .execute(&serde_json::json!({"path": path}), &cx(&dir))
        .await;
    match result {
        ToolOutput::Ok { content, .. } => {
            assert!(content.contains("1\tline one"));
            assert!(content.contains("2\tline two"));
            assert!(content.contains("3/3 lines shown"));
        }
        ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
    }
    let _ = tokio::fs::remove_file(&full).await;
}

#[tokio::test]
async fn respects_offset_and_limit() {
    let dir = std::env::temp_dir();
    let path = format!("stella_test_range_{}.txt", std::process::id());
    let full = dir.join(&path);
    tokio::fs::write(&full, "a\nb\nc\nd\ne\n").await.unwrap();

    let result = ReadFile::default()
        .execute(
            &serde_json::json!({"path": path, "offset": 2, "limit": 2}),
            &cx(&dir),
        )
        .await;
    match result {
        ToolOutput::Ok { content, .. } => {
            assert!(content.contains("2\tb"));
            assert!(content.contains("3\tc"));
            assert!(!content.contains("4\td"));
            assert!(content.contains("2/5 lines shown"));
        }
        ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
    }
    let _ = tokio::fs::remove_file(&full).await;
}

/// The #3144 witness: a wrong-typed `offset`/`limit` is refused, never
/// silently defaulted. On main, `{"limit": "200"}` vanished into the
/// default window — no refusal, no note, wrong-sized read.
#[tokio::test]
async fn a_mistyped_offset_or_limit_is_refused_not_defaulted() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "a\nb\nc\n").unwrap();
    let tool = ReadFile::default();

    let out = tool
        .execute(
            &serde_json::json!({"path": "f.txt", "limit": "200"}),
            &cx(dir.path()),
        )
        .await;
    let ToolOutput::Error { message, .. } = out else {
        panic!("a mistyped limit must be an error, got: {out:?}");
    };
    assert_eq!(
        message,
        "field `limit` must be a non-negative integer, got string"
    );

    let out = tool
        .execute(
            &serde_json::json!({"path": "f.txt", "offset": true}),
            &cx(dir.path()),
        )
        .await;
    let ToolOutput::Error { message, .. } = out else {
        panic!("a mistyped offset must be an error, got: {out:?}");
    };
    assert_eq!(
        message,
        "field `offset` must be a non-negative integer, got boolean"
    );

    // Absent still defaults — the fix refuses wrong types, not absence.
    let out = tool
        .execute(&serde_json::json!({"path": "f.txt"}), &cx(dir.path()))
        .await;
    assert!(!out.is_error(), "{out:?}");
}

#[tokio::test]
async fn counts_reads_per_file_and_reports_them() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "one\ntwo\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "one\n").unwrap();
    let tool = ReadFile::default();

    // Two reads of the same file under different spellings aggregate.
    for spelling in ["src/a.rs", "src/./a.rs"] {
        let out = tool
            .execute(&serde_json::json!({"path": spelling}), &cx(dir.path()))
            .await;
        assert!(!out.is_error(), "{out:?}");
    }
    let third = tool
        .execute(&serde_json::json!({"path": "src/a.rs"}), &cx(dir.path()))
        .await;
    match third {
        ToolOutput::Ok { content, .. } => {
            assert!(
                content.contains("read 3× this session"),
                "third read reports its count: {content}"
            );
        }
        ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
    }
    assert_eq!(tool.read_count(dir.path(), "src/a.rs"), 3);
    assert_eq!(tool.read_count(dir.path(), "src/./a.rs"), 3);

    // Other files and failed reads don't inflate the tally.
    let other = tool
        .execute(&serde_json::json!({"path": "b.rs"}), &cx(dir.path()))
        .await;
    assert!(!other.is_error());
    assert_eq!(tool.read_count(dir.path(), "b.rs"), 1);
    let missing = tool
        .execute(&serde_json::json!({"path": "ghost.rs"}), &cx(dir.path()))
        .await;
    assert!(missing.is_error());
    assert_eq!(tool.read_count(dir.path(), "ghost.rs"), 0);
}

#[tokio::test]
async fn read_records_last_seen_hash_even_for_ranged_reads() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "one\ntwo\nthree\n").unwrap();
    let ledger = Arc::new(ReadLedger::default());
    let tool = ReadFile::with_ledger(ledger.clone());

    assert_eq!(ledger.last_seen_sha(dir.path(), "a.rs"), None);
    let out = tool
        .execute(
            &serde_json::json!({"path": "a.rs", "offset": 2, "limit": 1}),
            &cx(dir.path()),
        )
        .await;
    assert!(!out.is_error(), "{out:?}");
    // The hash covers the FULL file content current at read time, not
    // just the displayed range — drift asks about the file, not the view.
    assert_eq!(
        ledger.last_seen_sha(dir.path(), "a.rs"),
        Some(crate::staleness::hex_sha256(b"one\ntwo\nthree\n")),
    );

    // An out-of-band change is visible as a hash mismatch…
    std::fs::write(dir.path().join("a.rs"), "rewritten\n").unwrap();
    assert_ne!(
        ledger.last_seen_sha(dir.path(), "a.rs"),
        Some(crate::staleness::hex_sha256(b"rewritten\n")),
    );
    // …until the next read refreshes the baseline.
    let again = tool
        .execute(&serde_json::json!({"path": "a.rs"}), &cx(dir.path()))
        .await;
    assert!(!again.is_error());
    assert_eq!(
        ledger.last_seen_sha(dir.path(), "a.rs"),
        Some(crate::staleness::hex_sha256(b"rewritten\n")),
    );
}

/// The line cap alone never saw this file: 5 MB on ONE line is a single
/// line, so it sailed under `MAX_LINES` and landed in the transcript
/// whole (~1.4M estimated tokens — enough to hard-fail the next provider
/// call). The width cap must clip it, loudly, and say so.
#[tokio::test]
async fn a_single_pathologically_long_line_is_clipped_and_named() {
    let dir = tempfile::tempdir().unwrap();
    let huge = "x".repeat(5 * 1024 * 1024);
    std::fs::write(dir.path().join("bundle.min.js"), &huge).unwrap();

    let out = ReadFile::default()
        .execute(
            &serde_json::json!({"path": "bundle.min.js"}),
            &cx(dir.path()),
        )
        .await;
    let ToolOutput::Ok { content, .. } = out else {
        panic!("expected ok, got: {out:?}");
    };
    assert!(
        content.len() < 64 * 1024,
        "a 5 MB one-liner must not reach the model whole (got {} bytes)",
        content.len()
    );
    assert!(
        content.contains("bytes elided"),
        "elision is loud: {content}"
    );
    assert!(
        content.contains("clipped at the 1000-byte per-line cap"),
        "the footer names the cap: {content}"
    );
    assert!(content.contains("1/1 lines shown"), "{content}");
}

/// Many long lines blow the payload ceiling even though each one is
/// individually clipped — the render stops and the footer says so, with
/// an honest shown/total count.
#[tokio::test]
async fn the_total_payload_cap_stops_the_render_and_reports_it() {
    let dir = tempfile::tempdir().unwrap();
    let line = "y".repeat(4096);
    let body: String = std::iter::repeat_n(line.as_str(), 800)
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dir.path().join("dump.sql"), &body).unwrap();

    let out = ReadFile::default()
        .execute(&serde_json::json!({"path": "dump.sql"}), &cx(dir.path()))
        .await;
    let ToolOutput::Ok { content, .. } = out else {
        panic!("expected ok, got: {out:?}");
    };
    assert!(
        content.len() < MAX_RENDER_BYTES + 4096,
        "payload stays under the ceiling (got {} bytes)",
        content.len()
    );
    // Derived from the constant, not written out: this assertion said
    // "400 KB" and would have gone on passing for a cap that moved, which
    // is the shape a size guard can least afford.
    assert!(
        content.contains(&format!(
            "stopped at the {} KB payload cap",
            MAX_RENDER_BYTES / 1024
        )),
        "the footer names the cap: {content}"
    );
    assert!(
        !content.contains("800/800 lines shown"),
        "the shown count must be the lines actually emitted: {content}"
    );
    assert!(content.contains("/800 lines shown"), "{content}");

    // The paging half (#1842). A cap that says "there is more" without
    // saying WHERE costs the model a guess, and the answer is not the
    // shown count — `start` may be non-zero and clipped lines still count
    // as shown. Asserted by re-reading at the named offset and requiring
    // the continuation to begin exactly one line after the last shown.
    let resume: usize = content
        .split("continue with offset=")
        .nth(1)
        .and_then(|tail| {
            let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .unwrap_or_else(|| panic!("the footer must name the line to resume from: {content}"));
    assert!(resume > 1, "a resume offset of {resume} names no progress");

    // The offset has to be usable, not merely present: re-reading at it
    // must begin exactly where this render stopped. An off-by-one here
    // silently skips a line or repeats one, and the model cannot tell.
    assert!(
        !content.contains(&format!("\n{resume:>6}\t")),
        "line {resume} must NOT already be in this render — it is where the \
             next one starts"
    );
    let next = ReadFile::default()
        .execute(
            &serde_json::json!({"path": "dump.sql", "offset": resume}),
            &cx(dir.path()),
        )
        .await;
    let ToolOutput::Ok { content: next, .. } = next else {
        panic!("expected ok, got: {next:?}");
    };
    assert!(
        next.starts_with(&format!("{resume:>6}\t")),
        "reading at the named offset must continue at line {resume}: {next}"
    );
}

/// The caps must be invisible for ordinary source: no marker, no footer
/// noise, byte-identical numbering.
#[tokio::test]
async fn ordinary_files_are_unaffected_by_the_byte_caps() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn main() {}\nlet x = 1;\n").unwrap();
    let out = ReadFile::default()
        .execute(&serde_json::json!({"path": "a.rs"}), &cx(dir.path()))
        .await;
    let ToolOutput::Ok { content, .. } = out else {
        panic!("expected ok, got: {out:?}");
    };
    assert!(!content.contains("elided"), "{content}");
    assert!(!content.contains("cap"), "{content}");
    assert!(
        content.ends_with("(2/2 lines shown · read 1× this session)"),
        "{content}"
    );
}

/// **The #4034 witness.** A monotonic paging sweep of one file is stopped
/// before it can spend a turn's whole budget.
///
/// The observed turn made 164 forty-line reads of one 3,943-line file for
/// $7.83, ran off the end, wrapped to offset 1 and started over. Every
/// loop verdict stayed silent because each read returned a genuinely
/// different window — all four are defined on byte-identical *output*, and
/// this sweep never repeats one. On `main` this test's forty reads all
/// succeed and the turn is left to grind to `max_steps`.
#[tokio::test]
async fn a_monotonic_paging_sweep_is_refused_before_it_burns_a_turn() {
    let dir = tempfile::tempdir().unwrap();
    let body = (1..=4000)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dir.path().join("deck_ui.rs"), &body).unwrap();
    let tool = ReadFile::default();
    let ctx = cx(dir.path());

    let mut refused_at = None;
    let mut refusals = Vec::new();
    for step in 0..40u64 {
        let out = tool
            .execute(
                &serde_json::json!({
                    "path": "deck_ui.rs",
                    "offset": step * 40 + 1,
                    "limit": 40,
                }),
                &ctx,
            )
            .await;
        if let ToolOutput::Error { message, .. } = out {
            refused_at.get_or_insert(step);
            refusals.push(message);
        }
    }
    let refused_at = refused_at.expect("the sweep must be refused, not run to the cap");
    assert!(
        refused_at < 40,
        "the sweep has to be stopped before step 40, got {refused_at}"
    );
    assert_eq!(
        refused_at, MAX_UNCHANGED_READS,
        "the 25th read of unchanged bytes is the first refused"
    );
    // Constant by construction — a tally inside it would make every
    // refusal a different string and leave a model that ignores the
    // ceiling as undetectable as the sweep that earned it.
    assert!(
        refusals.windows(2).all(|w| w[0] == w[1]),
        "every refusal must be byte-identical: {refusals:?}"
    );
    // The remedy has to be reachable from inside this same tool, or the
    // ceiling is a wall rather than a redirection.
    assert!(refusals[0].contains("omitting `limit`"), "{}", refusals[0]);
    assert!(refusals[0].contains("`search`"), "{}", refusals[0]);
}

/// The remedy the refusal advertises has to actually work: once the
/// paging sweep has tripped the ceiling, the model is told that "omitting
/// `limit` shows you all of it at once". A whole-file read at that point
/// must be let through — otherwise `record_read` counts it as one more
/// unchanged read and the identical refusal comes back, steering the model
/// toward an action that can never succeed. Byte-identical whole-file
/// rereads stay caught by the loop detector, so nothing is lost.
#[tokio::test]
async fn a_whole_file_read_escapes_the_ceiling_the_refusal_advertises() {
    let dir = tempfile::tempdir().unwrap();
    let body = (1..=200)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dir.path().join("small.rs"), &body).unwrap();
    let tool = ReadFile::default();
    let ctx = cx(dir.path());

    // Sweep it in small windows until the ceiling trips, exactly as the
    // pathological turn did.
    let mut tripped = false;
    for step in 0..40u64 {
        let out = tool
            .execute(
                &serde_json::json!({
                    "path": "small.rs",
                    "offset": (step % 5) * 40 + 1,
                    "limit": 40,
                }),
                &ctx,
            )
            .await;
        if matches!(out, ToolOutput::Error { .. }) {
            tripped = true;
            break;
        }
    }
    assert!(tripped, "the windowed sweep must trip the ceiling");

    // Following the refusal's advice — a whole-file read — must succeed.
    let out = tool
        .execute(&serde_json::json!({"path": "small.rs"}), &ctx)
        .await;
    assert!(
        matches!(out, ToolOutput::Ok { .. }),
        "the whole-file read the refusal advertises must be reachable: {out:?}"
    );

    // A windowed read of the same unchanged bytes is still refused — the
    // exemption is for whole-file reads only, not a hole in the ceiling.
    let out = tool
        .execute(
            &serde_json::json!({"path": "small.rs", "offset": 1, "limit": 40}),
            &ctx,
        )
        .await;
    assert!(
        matches!(out, ToolOutput::Error { .. }),
        "a windowed sweep must still be refused after the ceiling: {out:?}"
    );
}

/// The ceiling counts reads of bytes that have not moved, so the
/// read → edit → read cycle that is most of an agent's working life never
/// approaches it. Without the reset this would refuse the 25th pass of an
/// ordinary edit loop.
#[tokio::test]
async fn editing_the_file_resets_the_read_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.rs");
    let tool = ReadFile::default();
    let ctx = cx(dir.path());
    for pass in 0..40 {
        // A file long enough that the read below is a *window*, and a
        // window on purpose: a whole-file read is exempt from the ceiling
        // (it returns identical bytes, so the loop verdicts already catch a
        // spiral of them), which would make this test pass even with the
        // reset deleted. The reset is the whole safety argument for the
        // ceiling, so it has to be exercised by a read the ceiling can
        // actually refuse.
        let body: String = (0..200)
            .map(|line| format!("fn f{line}() {{}} // pass {pass}\n"))
            .collect();
        std::fs::write(&path, body).unwrap();
        let out = tool
            .execute(
                &serde_json::json!({"path": "a.rs", "offset": 1, "limit": 40}),
                &ctx,
            )
            .await;
        assert!(
            matches!(out, ToolOutput::Ok { .. }),
            "pass {pass} of a read→edit→read cycle must never be refused: {out:?}"
        );
    }
}

/// A sweep that runs off the end of the file must stagnate like anything
/// else. The past-end reply embedded the caller's own offset in its
/// payload, so every call produced a different string, nothing ever
/// compared equal, and the stagnation rung could not fire on it (#4034).
/// The offset now rides the footer, which loop comparison strips.
#[tokio::test]
async fn past_end_reads_compare_equal_once_the_footer_is_stripped() {
    use stella_core::driver::loop_evidence::comparable_output;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "one\ntwo\n").unwrap();
    let tool = ReadFile::default();
    let ctx = cx(dir.path());
    let mut compared = Vec::new();
    for offset in [10, 50, 900] {
        let out = tool
            .execute(&serde_json::json!({"path": "a.rs", "offset": offset}), &ctx)
            .await;
        assert!(matches!(out, ToolOutput::Ok { .. }), "{out:?}");
        let ToolOutput::Ok { content, .. } = &*comparable_output(&out) else {
            panic!("expected ok, got: {out:?}");
        };
        compared.push(content.clone());
    }
    assert!(
        compared.windows(2).all(|w| w[0] == w[1]),
        "three past-end reads at different offsets must normalize to one \
             string, or stagnation can never fire on a sweep past EOF: {compared:?}"
    );
    assert!(
        !compared[0].contains("900"),
        "the caller's offset must not survive normalization: {}",
        compared[0]
    );
    assert!(
        compared[0].contains("past the end"),
        "the reply still has to say what happened: {}",
        compared[0]
    );
}

/// The footer's producer is here and its consumer is in `stella-core`, so
/// neither crate's own tests can catch the two drifting apart. This is the
/// seam that can: real tool output, run through the real normalization the
/// loop detector compares. Reword the footer on either side without the
/// other and this fails.
///
/// Every shape has to normalize away, not just the bare tally. A read that
/// clipped a long line or stopped at the payload cap appends further
/// clauses after it, and a match that ended at the tally would leave the
/// per-session count in the compared bytes for exactly the large-file
/// rereads a stuck agent produces most.
#[tokio::test]
async fn the_footer_a_read_writes_is_the_one_loop_comparison_strips() {
    use stella_core::driver::loop_evidence::comparable_output;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("plain.rs"), "fn main() {}\nlet x = 1;\n").unwrap();
    std::fs::write(
        dir.path().join("wide.min.js"),
        "x".repeat(4 * MAX_LINE_BYTES),
    )
    .unwrap();
    let dump = std::iter::repeat_n("y".repeat(4096), 800)
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dir.path().join("dump.sql"), &dump).unwrap();

    // One tool, so the ledger's tally really does move between the two
    // reads of a file — which is the whole reason the footer is volatile.
    let tool = ReadFile::default();
    for name in ["plain.rs", "wide.min.js", "dump.sql"] {
        let input = serde_json::json!({ "path": name });
        let first = tool.execute(&input, &cx(dir.path())).await;
        let second = tool.execute(&input, &cx(dir.path())).await;
        let (
            ToolOutput::Ok { content: raw, .. },
            ToolOutput::Ok {
                content: raw_again, ..
            },
        ) = (&first, &second)
        else {
            panic!("expected ok for {name}, got: {first:?} / {second:?}");
        };
        assert!(
            raw.ends_with(READ_FOOTER_CLOSE) && raw.contains(READ_FOOTER_TALLY_END),
            "{name} emitted no recognizable footer: {raw}"
        );
        assert_ne!(
            raw, raw_again,
            "{name}: the tally must move, or this proves nothing"
        );

        let normalized = comparable_output(&first);
        assert_eq!(
            normalized,
            comparable_output(&second),
            "{name}: two identical reads must compare equal once the footer is off"
        );
        let ToolOutput::Ok { content, .. } = normalized.as_ref() else {
            unreachable!("normalizing an Ok output cannot change its variant");
        };
        assert!(
            !content.contains(READ_FOOTER_TALLY_END),
            "{name} kept part of the footer: {content}"
        );
    }
}

/// The engine's working-set restoration (#2685) replays this tool by the
/// name and path parameter `stella_core::restore` spells, and refuses the
/// replay unless the schema declares `read_only` — the same one-definition
/// tie as the footer test above. A drift on either side is not cosmetic:
/// it is restoration silently ceasing to restore files.
///
/// This replaces the narrowed `the_read_tool_keeps_the_schema_shape_a_replay_requires`
/// that stood in while the replay was disarmed by the tool purge (#3244):
/// the constants exist again, so the pin goes back to asserting against
/// them rather than against literals (#3470).
#[test]
fn the_read_tool_is_the_one_the_engines_restoration_replays() {
    let schema = ReadFile::default().schema();
    assert_eq!(schema.name, stella_core::restore::READ_TOOL);
    assert!(
        schema.read_only,
        "restoration (and the parked-wait probe) replay only schema-declared \
             read-only tools; dropping the claim silently disables both"
    );
    assert!(
        schema.input_schema["properties"]
            .get(stella_core::restore::READ_PATH_PARAM)
            .is_some(),
        "the path parameter must keep the spelling the engine replays"
    );
}

/// A binary file used to come back as "stream did not contain valid
/// UTF-8", which reads to a model as a transient IO fault worth retrying.
/// It must be named as binary and point somewhere useful.
#[tokio::test]
async fn a_binary_file_is_named_as_binary_not_as_an_io_fault() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("blob.bin"), [0x00u8, 0xff, 0xfe, 0x41]).unwrap();
    let out = ReadFile::default()
        .execute(&serde_json::json!({"path": "blob.bin"}), &cx(dir.path()))
        .await;
    match out {
        ToolOutput::Error { message, .. } => {
            assert!(message.contains("binary"), "{message}");
            assert!(message.contains("not UTF-8"), "{message}");
        }
        ToolOutput::Ok { content, .. } => panic!("expected error, got: {content}"),
    }
}

/// A directory used to surface the raw `Is a directory (os error 21)`,
/// which says what the syscall thought and not what to do instead.
#[tokio::test]
async fn a_directory_is_refused_with_the_tool_that_does_answer() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    let out = ReadFile::default()
        .execute(&serde_json::json!({"path": "src"}), &cx(dir.path()))
        .await;
    match out {
        ToolOutput::Error { message, .. } => {
            assert!(message.contains("is a directory"), "{message}");
            assert!(message.contains("glob("), "names the tool: {message}");
        }
        ToolOutput::Ok { content, .. } => panic!("expected error, got: {content}"),
    }
}

/// The heap half of the caps: `offset`/`limit` bound what the MODEL sees,
/// never what Stella loads, so a one-line read of a multi-gigabyte dump
/// paid for the whole file. Above the ceiling the refusal is named and
/// points at the tools that stream.
#[tokio::test]
async fn an_oversized_file_is_refused_before_it_is_loaded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dump.sql");
    let file = std::fs::File::create(&path).unwrap();
    // Sparse: the ceiling is decided from metadata, so no bytes are spent.
    file.set_len(MAX_FILE_BYTES + 1).unwrap();
    drop(file);

    let out = ReadFile::default()
        .execute(
            &serde_json::json!({"path": "dump.sql", "offset": 1, "limit": 1}),
            &cx(dir.path()),
        )
        .await;
    match out {
        ToolOutput::Error { message, .. } => {
            assert!(message.contains("ceiling"), "{message}");
            assert!(message.contains("grep"), "points somewhere: {message}");
        }
        ToolOutput::Ok { content, .. } => {
            panic!("an oversized file must be refused, got: {content}")
        }
    }
}

#[tokio::test]
async fn missing_file_returns_error() {
    let dir = std::env::temp_dir();
    let result = ReadFile::default()
        .execute(
            &serde_json::json!({"path": "nonexistent_xyz_123.txt"}),
            &cx(&dir),
        )
        .await;
    assert!(result.is_error());
}

/// **Reads are not confined to the workspace**, and this test now says so.
///
/// It used to assert that `../../etc/passwd` was refused. That was the
/// behaviour when `read_file` opened only the session root, and it is the
/// behaviour that was deliberately changed: an agent fixing a build needs
/// system headers, the toolchain and a dependency's source, and a read
/// cannot damage the user's tree (`stella_core::workspace_scope`).
///
/// Worth noting how it was passing on macOS while the change was already
/// in: `std::env::temp_dir()` there is `/var/folders/…/T/`, so
/// `../../etc/passwd` resolves to a path that does not exist, and the read
/// failed for the wrong reason. On Linux CI the same expression resolves
/// to the real `/etc/passwd` and the read succeeded — which is how the
/// stale assertion surfaced at all. A test that passes on one platform by
/// accident of path arithmetic is worse than no test, so this one now
/// pins the rule directly, on a file it creates itself.
#[tokio::test]
async fn a_read_outside_the_workspace_is_allowed() {
    let workspace = tempfile::tempdir().expect("workspace");
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let outside = elsewhere.path().join("readable.txt");
    std::fs::write(&outside, "readable\n").expect("write");

    let result = ReadFile::default()
        .execute(
            &serde_json::json!({ "path": outside.to_string_lossy() }),
            &cx(workspace.path()),
        )
        .await;
    let ToolOutput::Ok { content, .. } = result else {
        panic!("a read outside the workspace must succeed: {result:?}");
    };
    assert!(content.contains("readable"), "{content}");
}

/// The one read that IS refused: another session's worktree — a second
/// checkout of the same repository at another revision, so reading it
/// answers about the wrong copy of the file being edited.
#[tokio::test]
async fn a_read_into_a_sibling_worktree_is_refused() {
    let workspace = tempfile::tempdir().expect("workspace");
    let worktree = workspace.path().join(".stella/worktrees/sibling");
    std::fs::create_dir_all(&worktree).expect("mkdir");
    std::fs::write(worktree.join("other.rs"), "pub fn other() {}\n").expect("write");

    let result = ReadFile::default()
        .execute(
            &serde_json::json!({ "path": ".stella/worktrees/sibling/other.rs" }),
            &cx(workspace.path()),
        )
        .await;
    assert!(result.is_error(), "{result:?}");
}

#[tokio::test]
async fn missing_path_field_returns_error() {
    let dir = std::env::temp_dir();
    let result = ReadFile::default()
        .execute(&serde_json::json!({}), &cx(&dir))
        .await;
    assert!(result.is_error());
}

// ── batching (#4151) ──────────────────────────────────────────────────

/// **The #4151 witness.** One call, several files.
///
/// The measured defect: in one execution the model issued 24 `sed -n
/// 'A,Bp'` range reads against 2 `read_file` calls, with `read_file`
/// recording zero errors — it was reaching for `bash` because that was the
/// only surface through which it could ask about several files at once. On
/// `main` there is no `files` key, so this call reads nothing.
#[tokio::test]
async fn one_call_reads_several_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "fn b() {}\nfn b2() {}\n").unwrap();

    let out = ReadFile::default()
        .execute(
            &serde_json::json!({"files": [
                {"path": "a.rs"},
                {"path": "b.rs", "offset": 2, "limit": 1}
            ]}),
            &cx(dir.path()),
        )
        .await;
    let ToolOutput::Ok { content, .. } = out else {
        panic!("expected ok, got: {out:?}");
    };
    // Each section names its file — with two of them in one payload, an
    // unlabelled render is unreadable.
    assert!(content.contains("===== a.rs ====="), "{content}");
    assert!(content.contains("===== b.rs ====="), "{content}");
    assert!(content.contains("fn a() {}"), "{content}");
    // The per-target range is honoured, not just the path.
    assert!(content.contains("fn b2() {}"), "{content}");
    assert!(
        !content.contains("1\tfn b() {}"),
        "b.rs was asked for from line 2: {content}"
    );
}

/// The safety half, and the reason batching could not just be a loop.
///
/// `MAX_RENDER_BYTES` bounds one render. Applied per file, a ten-file batch
/// would be a 640 KB tool result — the exact context flood #1842 closed,
/// reopened by the new key. The budget is spent across the whole call, and
/// what did not fit is named rather than dropped silently.
#[tokio::test]
async fn the_payload_cap_is_a_batch_budget_not_a_per_file_one() {
    let dir = tempfile::tempdir().unwrap();
    // Each file alone would fill the whole single-read budget.
    let body = std::iter::repeat_n("y".repeat(4096), 800)
        .collect::<Vec<_>>()
        .join("\n");
    for name in ["one.sql", "two.sql", "three.sql"] {
        std::fs::write(dir.path().join(name), &body).unwrap();
    }

    let out = ReadFile::default()
        .execute(
            &serde_json::json!({"files": [
                {"path": "one.sql"}, {"path": "two.sql"}, {"path": "three.sql"}
            ]}),
            &cx(dir.path()),
        )
        .await;
    let ToolOutput::Ok { content, .. } = out else {
        panic!("expected ok, got: {out:?}");
    };
    assert!(
        content.len() < MAX_RENDER_BYTES + 8192,
        "three files must share one budget, not get one each (got {} bytes, \
             per-file would be ~{})",
        content.len(),
        3 * MAX_RENDER_BYTES
    );
    // Silent truncation would read as "you have seen all three".
    assert!(
        content.contains("payload cap"),
        "the cap is named: {content}"
    );
}

/// Every file in a batch earns its own ledger entry.
///
/// This is the obligation that makes batching safe to prefer over `sed`:
/// the read→edit drift oracle (#331) and `write_file`'s no-clobber guard
/// both key on `ReadLedger`, and a shell read records nothing. A batch that
/// recorded only its first file would reintroduce the same blindness
/// through the front door.
#[tokio::test]
async fn every_file_in_a_batch_is_recorded_in_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "one\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "two\n").unwrap();
    let ledger = Arc::new(ReadLedger::default());
    let tool = ReadFile::with_ledger(ledger.clone());

    let out = tool
        .execute(
            &serde_json::json!({"files": [{"path": "a.rs"}, {"path": "b.rs"}]}),
            &cx(dir.path()),
        )
        .await;
    assert!(!out.is_error(), "{out:?}");

    for (name, bytes) in [("a.rs", b"one\n".as_slice()), ("b.rs", b"two\n".as_slice())] {
        assert_eq!(
            ledger.last_seen_sha(dir.path(), name),
            Some(crate::staleness::hex_sha256(bytes)),
            "{name} must be recorded, or edit_file's drift oracle goes blind on it"
        );
        assert!(
            ledger.saw_whole_file(dir.path(), name),
            "{name} was shown in full, so the no-clobber guard must know it"
        );
    }
}

/// A miss inside a batch reports in place and the rest still render.
///
/// A read leaves nothing half-done, so all-or-nothing would throw away work
/// that succeeded. This is also what the `sed` chains being replaced did —
/// minus having to guess from the output which command failed.
#[tokio::test]
async fn a_failed_target_does_not_discard_the_rest_of_the_batch() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("real.rs"), "fn real() {}\n").unwrap();

    let out = ReadFile::default()
        .execute(
            &serde_json::json!({"files": [
                {"path": "ghost.rs"}, {"path": "real.rs"}
            ]}),
            &cx(dir.path()),
        )
        .await;
    let ToolOutput::Ok { content, .. } = out else {
        panic!("a batch with one bad path must still return the good one: {out:?}");
    };
    assert!(content.contains("not read:"), "{content}");
    assert!(content.contains("fn real() {}"), "{content}");
}

/// Both spellings at once is refused rather than resolved by precedence:
/// either choice silently drops work, and the model cannot tell which half
/// ran.
#[tokio::test]
async fn the_single_and_plural_spellings_cannot_be_mixed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "one\n").unwrap();
    let out = ReadFile::default()
        .execute(
            &serde_json::json!({"path": "a.rs", "files": [{"path": "a.rs"}]}),
            &cx(dir.path()),
        )
        .await;
    assert!(out.is_error(), "{out:?}");
}
