//! Tests for [`crate::export`] — the `/export` archive.
//!
//! Split out of `export.rs` because that file reached 1482 of the
//! 1500-line god-file ceiling: the next test added to it would have failed
//! `make file-size` for a reason its author did not cause. Same shape as
//! `agent.rs` + `agent/tests.rs`.

use super::*;

/// Seed one session's execution, telemetry, and touched file into the
/// store at `root`.
///
/// Every end-to-end export test needs a REAL `executions` row stamped with
/// a session id. Telemetry hanging off a dangling `execution_id` was
/// visible to the old workspace-wide dump and is invisible to a scoped
/// one — which is the fix working, not a fixture problem.
fn seed_session(root: &Path, session: &str, provider: &str, touched: &str) {
    use stella_store::{FileTouchRow, TelemetryRow};
    let store = Store::open(root).unwrap();
    let id = store
        .begin_execution(
            "deck",
            &format!("prompt of {session}"),
            provider,
            "claude-test",
        )
        .unwrap();
    store.set_execution_session(id, session).unwrap();
    store
        .record_telemetry(
            id,
            &TelemetryRow {
                step: 0,
                call_role: "worker".into(),
                provider: provider.into(),
                model: "claude-test".into(),
                input_tokens: 1000,
                estimated_input_tokens: 900,
                output_tokens: 200,
                cache_read_tokens: 500,
                cache_miss_tokens: 500,
                cache_write_tokens: 10,
                cost_usd: 0.012,
                duration_ms: 1500,
                retries: 1,
                tool_calls: 3,
                usage_complete: true,
            },
        )
        .unwrap();
    store
        .record_files_touched(
            id,
            &[FileTouchRow {
                path: touched.into(),
                ops: "M".into(),
                lines_added: 10,
                lines_removed: 2,
                events_json: "[]".into(),
            }],
        )
        .unwrap();
}

#[test]
fn zip_writer_produces_a_valid_archive() {
    let mut zip = ZipWriter::new();
    zip.add_file("a.txt", b"hello").unwrap();
    zip.add_file("b.txt", b"world!!!").unwrap();
    let bytes = zip.finish().unwrap();

    // Minimum valid ZIP with 2 entries: 2 local headers + 2 central + EOCD.
    // Each local header is 30 + name_len; central is 46 + name_len; EOCD is 22.
    assert!(bytes.len() > 100);
    // Starts with PK\x03\x04.
    assert_eq!(&bytes[..4], &[0x50, 0x4b, 0x03, 0x04]);
    // Ends with EOCD PK\x05\x06.
    assert_eq!(
        &bytes[bytes.len() - 22..bytes.len() - 18],
        &[0x50, 0x4b, 0x05, 0x06]
    );

    // Verify the content is stored verbatim (store-only).
    let hello_pos = find_subsequence(&bytes, b"hello");
    assert!(hello_pos.is_some(), "file content stored in the zip");
}

#[test]
fn zip_writer_handles_empty_files() {
    let mut zip = ZipWriter::new();
    zip.add_file("empty.txt", b"").unwrap();
    let bytes = zip.finish().unwrap();
    assert_eq!(&bytes[..4], &[0x50, 0x4b, 0x03, 0x04]);
}

/// The four-byte size/offset guard, exercised with plain lengths — the
/// point is refusing loudly at the ZIP32 boundary, not allocating 4 GiB.
#[test]
fn zip32_field_refuses_lengths_the_headers_cannot_store() {
    assert_eq!(zip32_field(0, "entry `x`"), Ok(0));
    assert_eq!(
        zip32_field(u32::MAX as usize, "entry `x`"),
        Ok(u32::MAX),
        "the last representable length still fits"
    );
    let err = zip32_field(u32::MAX as usize + 1, "entry `big.json`").unwrap_err();
    assert!(err.contains("entry `big.json`"), "{err}");
    assert!(err.contains("4 GiB"), "{err}");
    assert!(err.contains("ZIP64"), "{err}");
}

/// Past 65,535 entries the EOCD's two-byte counts wrap — `finish` must
/// refuse rather than emit an archive most tools would misread.
#[test]
fn zip_writer_refuses_more_entries_than_the_eocd_can_count() {
    let mut zip = ZipWriter::new();
    for i in 0..=u16::MAX as u32 {
        zip.add_file(&format!("{i}"), b"").unwrap();
    }
    let err = zip.finish().unwrap_err();
    assert!(err.contains("65,535"), "{err}");
    assert!(err.contains("ZIP64"), "{err}");
}

#[test]
fn crc32_matches_known_values() {
    // CRC-32 of "hello" is 0x3610a686.
    assert_eq!(crc32(b"hello"), 0x3610a686);
    // CRC-32 of "" is 0.
    assert_eq!(crc32(b""), 0);
    // CRC-32 of "123456789" is 0xcbf43926 (the standard check value).
    assert_eq!(crc32(b"123456789"), 0xcbf43926);
}

#[test]
fn sanitize_timestamp_strips_unsafe_chars() {
    assert_eq!(
        sanitize_timestamp("2024-01-15 10:30:00"),
        "2024-01-15-10-30-00"
    );
    assert_eq!(sanitize_timestamp("1705312200000000"), "1705312200000000");
    assert_eq!(sanitize_timestamp("../etc/passwd"), "etc-passwd");
}

/// A workspace file path, a tool name, and a prompt all reach the
/// dashboard's `<script>` block verbatim, and all three are text an agent
/// or a cloned repo chooses. A literal `</script` in any of them would end
/// the element and turn the rest of the page into attacker markup — in an
/// artifact the module doc tells you to email or attach to a PR.
#[test]
fn script_json_cannot_close_the_script_element() {
    let hostile = r#"[{"path":"</script><img src=x onerror=alert(1)>"}]"#;
    let safe = script_json(hostile);
    assert!(!safe.contains('<'), "{safe}");
    assert!(!safe.contains('>'), "{safe}");
    // …and it is still the same document to a JSON parser.
    let parsed: serde_json::Value = serde_json::from_str(&safe).expect("valid JSON");
    assert_eq!(parsed[0]["path"], "</script><img src=x onerror=alert(1)>");
}

/// A file path an agent created lands in `FILES`, flows into `barChart`
/// as `d.label`, and is assigned to `innerHTML`. `script_json` only stops
/// the payload from closing the `<script>` element — the JS parser decodes
/// `<` straight back and the live string reaches the sink — so every
/// `innerHTML` interpolation of agent-, MCP-, or repo-chosen text has to
/// escape at the sink.
///
/// The assertion is structural (over the emitted JS) rather than over the
/// rendered DOM on purpose: the escaping happens in the browser, so no
/// Rust-side output ever contains the escaped `&lt;img` form.
#[test]
fn dashboard_escapes_untrusted_text_at_every_innerhtml_sink() {
    let hostile = r#"[{"path":"<img src=x onerror=alert(1)>","lines_added":1,"lines_removed":0}]"#;
    let html = render_dashboard(
        &[],
        &[("files_touched", hostile.to_string())],
        "now",
        "ses-x",
        &ExportExclusions::default(),
    );

    // The payload does reach the page — escaping at the sink, not
    // dropping the datum, is what makes it inert.
    assert!(
        html.contains("onerror=alert(1)"),
        "the hostile path is embedded in the dashboard data"
    );
    assert!(
        html.contains("const esc ="),
        "the dashboard defines an HTML escape helper"
    );

    // Every interpolation that reaches `innerHTML` goes through it…
    for wrapped in [
        "${esc(t.label)}",
        "${esc(t.text)}",
        "${esc(r.provider)}",
        "${esc(r.model)}",
        "${esc(d.label)}",
        "${esc(d.display)}",
    ] {
        assert!(html.contains(wrapped), "escaped at the sink: {wrapped}");
    }
    // …and none of them is left raw.
    for raw in [
        "${t.label}",
        "${t.text}",
        "${r.provider}",
        "${r.model}",
        "${d.label}",
        "${d.display}",
    ] {
        assert!(!html.contains(raw), "no unescaped sink remains: {raw}");
    }
}

#[test]
fn script_json_leaves_ordinary_payloads_alone() {
    let plain = r#"[{"model":"glm-5.2","runs":3}]"#;
    assert_eq!(script_json(plain), plain);
}

#[test]
fn table_json_falls_back_to_an_empty_array() {
    let dumps: Vec<TableDump> = vec![("telemetry", "[1]".to_string())];
    assert_eq!(table_json(&dumps, "telemetry"), "[1]");
    assert_eq!(table_json(&dumps, "missing"), "[]");
}

#[test]
fn sanitize_timestamp_collapses_dash_runs() {
    assert_eq!(sanitize_timestamp("a--b---c"), "a-b-c");
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[test]
fn export_all_json_from_an_in_memory_store() {
    let store = Store::in_memory().unwrap();
    // An empty store should produce empty arrays, not errors.
    let dumps = store.export_all_json().unwrap();
    assert!(
        dumps.iter().all(|(_, j)| j == "[]"),
        "an empty store exports empty arrays"
    );
}

#[test]
fn redact_dump_masks_secrets_and_keeps_valid_json() {
    // #817: a GitHub PAT in a prompt and an AWS key inside a nested
    // `args_json` string. Both must be gone, the dump must stay valid JSON,
    // and the non-secret content must survive.
    let raw = r#"[{"prompt":"deploy with ghp_016C7e4a9b2d3f5081726354ABCDabcd1234","args_json":"{\"key\":\"AKIAIOSFODNN7EXAMPLE\"}"},{"n":42}]"#;
    let out = redact_dump(raw);
    let parsed: serde_json::Value =
        serde_json::from_str(&out).expect("a redacted dump is still valid JSON");
    assert!(parsed.is_array(), "shape preserved");
    assert!(
        !out.contains("ghp_016C7e4a9b2d3f5081726354"),
        "github token leaked: {out}"
    );
    assert!(
        !out.contains("AKIAIOSFODNN7EXAMPLE"),
        "aws key leaked: {out}"
    );
    assert!(out.contains("[redacted]"), "placeholder present: {out}");
    assert!(out.contains("deploy with"), "non-secret prose survives");
    assert!(out.contains("42"), "non-string values survive");
}

#[test]
fn export_dumps_redact_a_secret_that_reached_the_telemetry() {
    // The end-to-end guarantee: a credential recorded in real telemetry is
    // masked by the same redaction `export_session` applies before writing.
    let store = Store::in_memory().unwrap();
    use stella_store::FileTouchRow;
    store
        .record_files_touched(
            1,
            &[FileTouchRow {
                path: "deploy.env".into(),
                ops: "A".into(),
                lines_added: 1,
                lines_removed: 0,
                events_json: r#"[{"note":"leaked ghp_016C7e4a9b2d3f5081726354ABCDabcd1234"}]"#
                    .into(),
            }],
        )
        .unwrap();
    let dumps: Vec<TableDump> = store
        .export_all_json()
        .unwrap()
        .into_iter()
        .map(|(t, j)| (t, redact_dump(&j)))
        .collect();
    let files = dumps
        .iter()
        .find(|(t, _)| *t == "files_touched")
        .map(|(_, j)| j.as_str())
        .expect("files_touched present");
    assert!(
        !files.contains("ghp_016C7e4a9b2d3f5081726354"),
        "secret leaked into the export: {files}"
    );
    assert!(files.contains("[redacted]"), "secret was masked: {files}");
}

#[test]
fn export_round_trips_through_a_real_store() {
    // Record a minimal execution + telemetry, then export and verify the
    // JSON round-trips.
    let store = Store::in_memory().unwrap();
    use stella_store::{FileTouchRow, TelemetryRow};

    // We need an execution id — use the internal record path via a direct
    // SQL insert since start_execution is on the CLI side.
    store
        .record_telemetry(
            1,
            &TelemetryRow {
                step: 0,
                call_role: "worker".into(),
                provider: "test".into(),
                model: "test-model".into(),
                input_tokens: 100,
                estimated_input_tokens: 90,
                output_tokens: 50,
                cache_read_tokens: 0,
                cache_miss_tokens: 100,
                cache_write_tokens: 10,
                cost_usd: 0.001,
                duration_ms: 500,
                retries: 0,
                tool_calls: 1,
                usage_complete: true,
            },
        )
        .unwrap();
    store
        .record_files_touched(
            1,
            &[FileTouchRow {
                path: "src/main.rs".into(),
                ops: "M".into(),
                lines_added: 10,
                lines_removed: 2,
                events_json: "[]".into(),
            }],
        )
        .unwrap();

    let dumps = store.export_all_json().unwrap();
    let telemetry = dumps
        .iter()
        .find(|(t, _)| *t == "telemetry")
        .map(|(_, j)| j.as_str())
        .unwrap();
    let parsed: Vec<serde_json::Value> = serde_json::from_str(telemetry).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0]["provider"], "test");
    assert_eq!(parsed[0]["input_tokens"], 100);

    let files = dumps
        .iter()
        .find(|(t, _)| *t == "files_touched")
        .map(|(_, j)| j.as_str())
        .unwrap();
    let parsed: Vec<serde_json::Value> = serde_json::from_str(files).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0]["path"], "src/main.rs");
}

#[test]
fn full_export_pipeline_creates_valid_zip_with_dashboard() {
    // End-to-end: seed a temp store with data, export it, and verify the
    // archive is a valid ZIP containing the HTML dashboard and raw JSON.
    let tmp = tempfile::tempdir().unwrap();
    seed_session(tmp.path(), "ses-export", "anthropic", "src/main.rs");

    // Export.
    let zip_path = export_session(tmp.path(), "ses-export").expect("export should succeed");
    assert!(zip_path.exists(), "the zip file was written");
    assert!(
        zip_path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("session-"),
        "filename starts with session-"
    );
    assert!(
        zip_path.extension().is_some_and(|e| e == "zip"),
        "extension is .zip"
    );

    // Verify the archive content.
    let bytes = std::fs::read(&zip_path).unwrap();
    assert_eq!(&bytes[..2], b"PK", "valid ZIP magic bytes");
    let content = String::from_utf8_lossy(&bytes);
    assert!(
        content.contains("Stella Session Telemetry"),
        "HTML dashboard is embedded"
    );
    assert!(
        content.contains("anthropic"),
        "provider name appears in the data"
    );
    assert!(
        content.contains("claude-test"),
        "model name appears in the data"
    );
    assert!(content.contains("manifest"), "manifest is present");
}

/// #817 masks credentials from the dump, but what remains is the whole
/// session — every prompt, tool argument, and touched file's content. It
/// was written at the process umask (0644 on a stock system) inside the
/// project tree, so on a shared machine any other account could read the
/// complete transcript just by looking. Archive and directory are both
/// owner-only now; the directory matters as much as the file, because an
/// archive nobody can open is still an archive everybody can list.
#[cfg(unix)]
#[test]
fn the_export_archive_and_its_directory_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    seed_session(tmp.path(), "ses-perms", "anthropic", "src/main.rs");

    // A pre-existing world-readable exports dir (an older build's) must be
    // tightened, not accepted.
    let exports = tmp.path().join(".stella/exports");
    std::fs::create_dir_all(&exports).unwrap();
    std::fs::set_permissions(&exports, std::fs::Permissions::from_mode(0o755)).unwrap();

    let zip_path = export_session(tmp.path(), "ses-perms").unwrap();

    assert_eq!(
        std::fs::metadata(&zip_path).unwrap().permissions().mode() & 0o777,
        0o600,
        "the session archive must not be readable by other accounts"
    );
    assert_eq!(
        std::fs::metadata(&exports).unwrap().permissions().mode() & 0o777,
        0o700,
        "the exports directory must not be listable by other accounts"
    );
}

/// The witness for #2558, asserted where it actually matters: the bytes of
/// the archive that leaves the machine.
///
/// This module's own documentation invites you to email the archive or
/// attach it to a PR as evidence, and #817 hardened it — masking every
/// credential, tightening the file to `0600` — on the premise that its
/// blast radius is one session. It was the whole workspace store, so
/// attaching an export to show one run disclosed every other run in that
/// project: their prompts, their tool arguments, their touched files.
///
/// Scanned over the raw archive rather than the dump vector on purpose.
/// The dumps are one of three places a row can reach the recipient — the
/// dashboard embeds the same JSON in a `<script>` block, and the manifest
/// summarizes it — so an assertion over the dumps alone would pass while
/// the HTML someone opens still carried the other session. The ZIP is
/// store-only (uncompressed), so its bytes are exactly what a reader can
/// recover.
#[test]
fn the_archive_holds_one_session_and_states_what_it_left_out() {
    let tmp = tempfile::tempdir().unwrap();
    seed_session(tmp.path(), "ses-mine", "anthropic", "src/mine.rs");
    seed_session(tmp.path(), "ses-theirs", "openai", "src/their-secret.rs");

    let zip_path = export_session(tmp.path(), "ses-mine").expect("export");
    let bytes = std::fs::read(&zip_path).unwrap();
    let archive = String::from_utf8_lossy(&bytes);

    for leaked in [
        "ses-theirs",
        "prompt of ses-theirs",
        "src/their-secret.rs",
        "openai",
    ] {
        assert!(!archive.contains(leaked), "the archive leaked `{leaked}`");
    }

    // …while this session's own evidence all survived the filter. An
    // archive that leaks nothing because it contains nothing is not a fix.
    for kept in ["ses-mine", "prompt of ses-mine", "src/mine.rs", "anthropic"] {
        assert!(archive.contains(kept), "the archive dropped `{kept}`");
    }

    // The manifest states the scope and the exclusion count, so a reader
    // can check the claim instead of trusting it.
    assert!(
        archive.contains("\"session\": \"ses-mine\""),
        "the manifest names the exported session"
    );
    assert!(
        archive.contains("\"other_session_executions\": 1"),
        "the manifest counts what it left out"
    );
    // And the dashboard says so on the page someone actually opens.
    assert!(
        archive.contains("This archive covers <strong>one session</strong>"),
        "the dashboard states its own scope"
    );
}

#[test]
fn full_export_pipeline_errors_on_empty_store() {
    let tmp = tempfile::tempdir().unwrap();
    // Create the store (so .stella/ exists) but record nothing.
    let _ = Store::open(tmp.path()).unwrap();
    let result = export_session(tmp.path(), "ses-nothing");
    assert!(result.is_err(), "exporting an empty store is an error");
    assert!(
        result
            .unwrap_err()
            .contains("no telemetry recorded for this session"),
        "error message is helpful"
    );
}

/// A session with no rows of its own must refuse, not silently widen to
/// the workspace. This is the failure mode a "scope it when you can"
/// implementation would ship: the one store where scoping is hardest to
/// satisfy is exactly where falling back leaks everything.
#[test]
fn exporting_a_session_with_no_rows_refuses_rather_than_widening() {
    let tmp = tempfile::tempdir().unwrap();
    seed_session(tmp.path(), "ses-real", "anthropic", "src/main.rs");

    let result = export_session(tmp.path(), "ses-never-ran");
    assert!(
        result.is_err(),
        "a session with no telemetry must not fall back to the workspace"
    );
}

/// The dashboard's `:root` palette is generated from the live theme, not
/// typed into the template. The block it replaced was two recolours stale
/// — a true-black ground and the retired sky/violet pair — in an artifact
/// users mail around and attach to PRs, so it was the most widely seen
/// remnant of a retired identity in the product.
#[test]
fn the_dashboard_palette_is_generated_from_the_live_theme() {
    let html = render_dashboard(
        &[],
        &[],
        "2026-01-01 00:00:00",
        "ses-x",
        &ExportExclusions::default(),
    );

    for (var, token) in [
        ("--bg", stella_tui::theme::GROUND),
        ("--brand", stella_tui::theme::ACCENT),
        ("--brand-fill", stella_tui::theme::ACCENT_FILL),
        ("--violet", stella_tui::theme::VIOLET),
        ("--text3", stella_tui::theme::TEXT_TERTIARY),
    ] {
        let declaration = format!("{var}: {};", css_hex(token));
        assert!(
            html.contains(&declaration),
            "expected `{declaration}` in the dashboard's :root block"
        );
    }

    // The retired values must not survive anywhere in the document —
    // including the chart JS, which referenced a `--azure` the `:root`
    // block no longer defines.
    for retired in [
        "#7dd3fc", "#38bdf8", "#a78bfa", "#6c7b90", "#4d9fff", "--sky", "--azure",
    ] {
        assert!(
            !html.contains(retired),
            "retired brand value `{retired}` still ships in the dashboard"
        );
    }

    // Every custom property the chart code asks for must exist, or the bar
    // renders with no colour at all.
    for used in ["--brand-fill", "--violet", "--success"] {
        assert!(
            html.contains(&format!("{used}: #")),
            "chart JS references `{used}` but :root never defines it"
        );
    }
}

/// The watermark is the one store-supplied value that reaches markup
/// rather than the `<script>` block, so it gets the markup escape.
#[test]
fn escape_html_neutralizes_markup() {
    assert_eq!(escape_html("2024-01-15 10:30:00"), "2024-01-15 10:30:00");
    assert_eq!(
        escape_html("<img src=x onerror=alert(1)>"),
        "&lt;img src=x onerror=alert(1)&gt;"
    );
    assert_eq!(
        escape_html("a & \"b\" 'c'"),
        "a &amp; &quot;b&quot; &#39;c&#39;"
    );
}
