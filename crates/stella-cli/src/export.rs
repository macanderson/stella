//! `/export` — export **one session's** telemetry as a ZIP archive of raw
//! JSON dumps plus a self-contained HTML dashboard.
//!
//! The archive lives at `.stella/exports/` and is named with the microsecond
//! timestamp of the **last log entry** included (the data's own clock, not the
//! user's submission time). The HTML is fully static — no external CSS/JS,
//! everything inlined — so it can be opened offline, emailed, or committed
//! alongside a PR as evidence.
//!
//! The dashboard surfaces the metrics that actually change software quality:
//! resolve rate, cost-per-resolved-task, token efficiency, tool-call
//! frequency, retry patterns, and file-edit heat — the same data `stella
//! stats` summarizes in a table, but visually and interactively.
//!
//! **Scope is a safety property here, not a convenience** (#2558). Until the
//! session argument existed, this module dumped the entire workspace store:
//! attaching an export to a public PR to show one run disclosed every other
//! run in that project — their prompts, their tool arguments, their touched
//! files' contents. The credential masking below and the `0600` archive mode
//! both assume the blast radius is one session, so the scoping is what makes
//! the rest of the hardening mean what it says.

use std::path::{Path, PathBuf};

use stella_store::{ExportExclusions, Store};

/// One `(table_name, json_array)` pair from the export dump.
type TableDump = (&'static str, String);

/// Build the export archive for one session. Returns the path to the written
/// file, or an error message. `workspace_root` is where `.stella/exports/` is
/// created; `session_id` is the session registry id
/// ([`stella_store::SessionRecord::id`]) whose telemetry the archive covers.
///
/// There is deliberately no whole-workspace variant on this path. The archive
/// is built to be shared, and "export everything by default" is the defect
/// #2558 records — a caller who wants workspace-wide analytics wants `stella
/// stats`, which is not an artifact that leaves the machine.
pub fn export_session(workspace_root: &Path, session_id: &str) -> Result<PathBuf, String> {
    let store = Store::open(workspace_root).map_err(|e| format!("cannot open store: {e}"))?;

    // Collect this session's raw data — never the workspace's.
    let dumps = store
        .export_session_json(session_id)
        .map_err(|e| format!("cannot read telemetry: {e}"))?;
    // #817: the archive leaves the machine (emailed, committed to a PR as
    // evidence), so mask any credential that reached the telemetry — a key
    // pasted into a prompt, printed by a tool into `args_json`, or living in a
    // touched file's events — before it is written to the raw dumps OR embedded
    // in the dashboard. Applied once, here, so both sinks see redacted data.
    let dumps: Vec<TableDump> = dumps
        .into_iter()
        .map(|(table, json)| (table, redact_dump(&json)))
        .collect();

    if dumps.iter().all(|(_, json)| json == "[]") {
        return Err("no telemetry recorded for this session yet — run a few turns first.".into());
    }

    // What the scope left out, so the manifest can state it. A census failure
    // must not sink an otherwise-good export: an unstated exclusion count is a
    // gap in the archive's provenance, not a reason to withhold the evidence.
    let excluded = store.export_exclusions(session_id).unwrap_or_default();

    // The watermark: the timestamp of the last log entry in this set — read
    // over the same session, or the filename would assert a moment no row in
    // the archive reaches. Falls back to "now" only if the store somehow has
    // no timestamps at all.
    let watermark = store
        .last_log_timestamp_for_session(session_id)
        .ok()
        .flatten()
        .unwrap_or_else(|| {
            // SQLite's CURRENT_TIMESTAMP is second-resolution; we need
            // microsecond precision for a unique, sortable filename. Use
            // SystemTime as the final fallback.
            use std::time::{SystemTime, UNIX_EPOCH};
            let micros = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_micros())
                .unwrap_or(0);
            format!("{micros}")
        });

    // Sanitize the watermark into a filename-safe folder name.
    let folder = sanitize_timestamp(&watermark);

    // Scoped too: the KPI tiles and every chart are computed from these rows,
    // so workspace totals rendered beside one session's dumps would describe
    // runs the archive does not contain.
    let usage_stats = store
        .usage_stats_for_session(session_id)
        .map_err(|e| format!("cannot read usage stats: {e}"))?;

    // The transcript. The nine dumped tables say what the session cost and
    // which tools it called; none of them holds what it actually did, because
    // the ordered event stream is not one of them. `session_events` is already
    // scoped to this session by the same predicate the dumps use.
    //
    // A journal that will not read must not sink the export: the tables and
    // the dashboard are still worth having, and an empty transcript reports
    // itself on the page rather than pretending the session was silent.
    let journal = store.session_events(session_id).unwrap_or_default();
    let transcript = transcript::render(&journal, &execution_prompts(&dumps));

    // Build the self-contained HTML dashboard.
    let html = render_dashboard(
        &usage_stats,
        &dumps,
        &watermark,
        session_id,
        &excluded,
        &transcript,
    );

    // Assemble the ZIP.
    //
    // Owner-only, directory and archive both. #817 masks *credentials* from
    // the dumps, but what remains is still the whole session: every prompt,
    // every tool argument, every touched file's content. That was landing at
    // the process umask (0644 on a stock system) inside the project tree, so on
    // a shared machine or a multi-user build box any other account could read
    // the complete transcript. The rest of the tree treats data of this
    // sensitivity as `.stella/private/`; the export is no less sensitive for
    // being a file the user later chooses to share deliberately.
    let exports_dir = workspace_root.join(".stella").join("exports");
    create_private_dir(&exports_dir)?;
    let zip_path = exports_dir.join(format!("session-{folder}.zip"));

    let mut zip = ZipWriter::new();
    // Raw JSON dumps — one per table, inside the timestamped folder.
    for (table, json) in &dumps {
        let pretty = pretty_json(json);
        zip.add_file(&format!("{folder}/raw/{table}.json"), pretty.as_bytes())?;
    }
    // The dashboard.
    zip.add_file(&format!("{folder}/dashboard.html"), html.as_bytes())?;
    // A manifest with the watermark, the scope, and the table list.
    //
    // `session` and `excluded` are the archive's provenance: without them a
    // reader cannot tell "this session did nothing else" from "the exporter
    // dropped the rest", and the scope becomes an assumption rather than a
    // claim they can check.
    let manifest = serde_json::json!({
        "exported_at": watermark,
        "session": session_id,
        "excluded": excluded,
        "tables": dumps.iter().map(|(t, j)| {
            let count = serde_json::from_str::<Vec<serde_json::Value>>(j)
                .map(|v| v.len())
                .unwrap_or(0);
            serde_json::json!({"table": t, "rows": count})
        }).collect::<Vec<_>>(),
    });
    zip.add_file(
        &format!("{folder}/manifest.json"),
        serde_json::to_string_pretty(&manifest)
            .unwrap_or_default()
            .as_bytes(),
    )?;

    let bytes = zip.finish()?;
    stella_store::durable::write_atomic(&zip_path, &bytes, stella_store::durable::MODE_PRIVATE)
        .map_err(|e| format!("write archive: {e}"))?;

    Ok(zip_path)
}

/// The `/export` deck command: build `session_id`'s archive and return the
/// message the deck prints.
///
/// Runs off the runtime worker. The export opens SQLite, dumps and
/// pretty-prints every telemetry table, renders the dashboard, and builds the
/// whole ZIP without yielding — awaiting it inline stalls the deck's event
/// pump, so keystrokes go unprocessed and the TUI looks hung on the crate's
/// most I/O-heavy command.
pub async fn export_command(workspace_root: &Path, session_id: &str) -> String {
    let root = workspace_root.to_path_buf();
    let session = session_id.to_string();
    let exported = tokio::task::spawn_blocking(move || export_session(&root, &session)).await;
    match exported.map_err(|e| format!("export task failed: {e}")) {
        Ok(Ok(path)) => format!(
            "Export Session Telemetry — archive written to {}\n\
             Scope: this session ({session_id}) only — the archive is safe to attach to a \
             PR or email without disclosing your other runs in this workspace. The ZIP \
             holds a `dashboard.html` (open in any browser), raw JSON dumps of this \
             session's telemetry tables, and a `manifest.json` naming the session and \
             what was excluded. The timestamped folder name matches the last log entry's \
             timestamp.",
            path.display()
        ),
        Ok(Err(e)) | Err(e) => format!("export failed: {e}"),
    }
}

/// Create `dir` (and parents) owner-only. An existing directory is tightened
/// too: an archive written 0600 into a 0755 directory is still listed by
/// everyone, and a directory created by an older build is exactly the case
/// that needs fixing.
///
/// Shared with `dataset_cmd` (#872), which writes redacted prompts and full
/// tool outputs and needs exactly this posture. The messages name the
/// directory rather than the caller's noun, so one helper can serve both
/// without either error reading as the other's.
pub(crate) fn create_private_dir(dir: &Path) -> Result<(), String> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|e| format!("create {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("restrict {}: {e}", dir.display()))?;
    }
    Ok(())
}

/// Redact credentials from one table's JSON dump (#817). Parses the array and
/// runs [`stella_core::redact::redact_secrets`] over **every string value**,
/// then re-serializes — so the output is always valid JSON and a secret
/// embedded anywhere (a prompt, a tool's `args_json`, a touched file's events)
/// is masked. If the dump is not the JSON shape we expect, falls back to a
/// whole-string redaction, which is still safe (it only ever removes content).
fn redact_dump(json: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(json) {
        Ok(mut value) => {
            redact_json_strings(&mut value);
            serde_json::to_string(&value)
                .unwrap_or_else(|_| stella_core::redact::redact_secrets(json).text)
        }
        Err(_) => stella_core::redact::redact_secrets(json).text,
    }
}

/// Recursively replace every string value in `value` with its redacted form.
fn redact_json_strings(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            let redaction = stella_core::redact::redact_secrets(text);
            if redaction.redacted {
                *text = redaction.text;
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(redact_json_strings),
        serde_json::Value::Object(map) => map.values_mut().for_each(redact_json_strings),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

/// Format an integer with comma thousands separators (e.g. `1234567` →
/// `1,234,567`). Rust's format strings don't support `:,`, so we do it here.
fn comma(n: i64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let neg = n < 0;
    let digits = if neg { &bytes[1..] } else { bytes };
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, &b) in digits.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(b as char);
    }
    if neg { format!("-{out}") } else { out }
}

/// Sanitize a timestamp string for use as a directory name: strip anything
/// that isn't alphanumeric, dash, or underscore, and collapse runs.
fn sanitize_timestamp(ts: &str) -> String {
    let clean: String = ts
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    // Collapse runs of dashes (e.g. "2024-01-15 10:30:00" → "2024-01-15-10-30-00").
    let mut result = String::with_capacity(clean.len());
    let mut prev_dash = false;
    for c in clean.chars() {
        if c == '-' {
            if !prev_dash {
                result.push(c);
            }
            prev_dash = true;
        } else {
            result.push(c);
            prev_dash = false;
        }
    }
    result.trim_matches('-').to_string()
}

/// Pretty-print a compact JSON string (best-effort — falls back to raw).
fn pretty_json(compact: &str) -> String {
    serde_json::from_str::<serde_json::Value>(compact)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| compact.to_string())
}

// ── Self-contained HTML dashboard ───────────────────────────────────────────

/// `execution_id` → that execution's prompt, read off the `executions` dump.
///
/// The transcript opens each turn with what was asked, and the prompt is a
/// column rather than an event — it never appears in the journal. Taking it
/// from the dump rather than re-querying means it has already been through
/// [`redact_dump`], so the same masking covers it.
fn execution_prompts(dumps: &[TableDump]) -> std::collections::HashMap<i64, String> {
    serde_json::from_str::<Vec<serde_json::Value>>(table_json(dumps, "executions"))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| {
            let id = row.get("id")?.as_i64()?;
            let prompt = row.get("prompt")?.as_str()?.to_string();
            Some((id, prompt))
        })
        .collect()
}

/// One table's JSON array from the dump set, or an empty array when absent.
fn table_json<'a>(dumps: &'a [TableDump], table: &str) -> &'a str {
    dumps
        .iter()
        .find(|(name, _)| *name == table)
        .map(|(_, json)| json.as_str())
        .unwrap_or("[]")
}

/// Escape a value that is interpolated into HTML *markup* rather than into the
/// `<script>` block that [`script_json`] covers. Only the watermark takes this
/// path today — it is a store-supplied string, and a store column is not a
/// place this module gets to assume markup-safety about.
fn escape_html(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Make a JSON document safe to embed inside an HTML `<script>` element.
///
/// The dumps are interpolated straight into `<script>…</script>`, and HTML —
/// not JavaScript — tokenizes that content first: a literal `</script`
/// anywhere inside, INCLUDING inside a JSON string, ends the element and
/// everything after it is parsed as markup. The dumps carry prompts, tool
/// names, and workspace file paths — all of them text an agent, an MCP
/// server, or a repo can choose — so `</script><img src=x onerror=…>` in any
/// one of them would execute in a dashboard this module's own doc invites you
/// to email or attach to a PR. `serde_json` has no reason to escape `<`, so
/// escape it here.
///
/// `\uXXXX` is the identical character to every JSON parser and is inert to
/// the HTML tokenizer, so the document round-trips unchanged. U+2028/U+2029
/// go too: they are JSON string content but were raw line terminators to
/// pre-ES2019 JavaScript, and this JSON is an inline literal, not a
/// `JSON.parse` argument.
fn script_json(raw: &str) -> String {
    raw.replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/// Render the full HTML dashboard. All CSS and JS are inlined — no external
/// dependencies. The data is embedded as a JSON blob so the JS can build
/// interactive charts client-side.
fn render_dashboard(
    usage_stats: &[stella_store::UsageStatsRow],
    dumps: &[TableDump],
    watermark: &str,
    session_id: &str,
    excluded: &ExportExclusions,
    transcript: &transcript::Transcript,
) -> String {
    let total_cost: f64 = usage_stats.iter().map(|r| r.total_cost_usd).sum();
    let total_runs: i64 = usage_stats.iter().map(|r| r.runs).sum();
    let total_resolved: i64 = usage_stats.iter().map(|r| r.resolved).sum();
    let total_input: i64 = usage_stats.iter().map(|r| r.input_tokens).sum();
    let total_output: i64 = usage_stats.iter().map(|r| r.output_tokens).sum();
    let total_cache_read: i64 = usage_stats.iter().map(|r| r.cache_read_tokens).sum();
    let resolve_rate = if total_runs > 0 {
        total_resolved as f64 / total_runs as f64 * 100.0
    } else {
        0.0
    };
    let cost_per_resolved = if total_resolved > 0 {
        total_cost / total_resolved as f64
    } else {
        0.0
    };

    // Pre-format integers with comma separators (Rust's format! doesn't
    // support `:,` like Python's).
    let total_input_fmt = comma(total_input);
    let total_output_fmt = comma(total_output);
    let total_cache_read_fmt = comma(total_cache_read);

    // The watermark and the session id are the only values interpolated into
    // markup rather than into the `<script>` block; every dump below goes
    // through `script_json` instead — see its doc for why raw JSON is not safe
    // there. The session id is a store column like any other, and a store
    // column is not a place this module gets to assume markup-safety about.
    let watermark = escape_html(watermark);
    let session = escape_html(session_id);

    // The scope line. The dashboard is the artifact someone opens before
    // deciding to forward it, so what it does and does not contain belongs on
    // the page — not only in the manifest beside it.
    let scope_note = if excluded.is_empty() {
        "the only session recorded in this workspace".to_string()
    } else {
        format!(
            "{} execution(s) from other sessions and {} unattributed execution(s) in this \
             workspace were <strong>not</strong> included",
            excluded.other_session_executions, excluded.unattributed_executions,
        )
    };

    // Telemetry rows for the timeline chart.
    let telemetry_json = script_json(table_json(dumps, "telemetry"));

    // Tool-call frequency.
    let tool_calls_json = script_json(table_json(dumps, "tool_calls"));

    // Executions (for the outcome breakdown).
    let executions_json = script_json(table_json(dumps, "executions"));

    // Files touched.
    let files_json = script_json(table_json(dumps, "files_touched"));

    // Usage stats as JSON for the per-model table.
    let stats_json =
        script_json(&serde_json::to_string(usage_stats).unwrap_or_else(|_| "[]".into()));

    // What the transcript panel says about itself, above its first row.
    let transcript_provenance = transcript.provenance();
    let transcript_rows = comma(transcript.rendered as i64);
    // An empty transcript is a real state, not a bug: a session whose events
    // were never persisted, or one recorded by a build old enough that none of
    // them replay. Say which, rather than rendering a blank panel that reads
    // as a broken page.
    let transcript_body = if transcript.rendered > 0 {
        transcript.body.clone()
    } else {
        "<p class=\"empty\">No replayable events were recorded for this session. \
         The metrics tab and the archive's <code>raw/</code> dumps are unaffected.</p>"
            .to_string()
    };

    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<!-- Page-level backstop (#615): the export is a standalone file built from
     workspace-influenced text, so even with every interpolation escaped, the
     page itself declares it loads nothing and talks to no one — inline
     script/style only (its own), no frames, no forms, no external fetches. -->
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'; connect-src 'none'">
<title>stella session telemetry — {watermark}</title>
<style>
  /* ── Instrument tokens ─────────────────────────────────────────────────
     This file is the third surface on stella's web instrument system; the
     other two are crates/stella-observatory/src/assets/index.html and
     arenabench/ui/app/globals.css. That file's delimited palette block is
     the single definition, and this block is a derivation of it —
     crates/stella-cli/tests/design_token_parity.rs fails if any token they
     both name disagrees, so the copy cannot drift the way the two before it
     did.

     The palette used to be interpolated from `stella_tui::theme`, which was
     right when the export's only sibling was the terminal. It is wrong now:
     the TUI palette is accent-chromed by design (ACCENT == BRAND == Ion
     #00D1F9, and that hue marks a Running state), while a web instrument's
     chrome must not carry a hue at all. Generating from the terminal guaranteed the
     export matched the one surface it should no longer match. The parity
     test replaces that guarantee with the correct one.

     Colour is meaning, and only meaning:
       --ok / --warn / --bad   a verdict — it passed, it needs attention, it
                               failed. Nothing else may take these.
       --c1..--c4              categorical series, separated by LIGHTNESS not
                               hue, so a series survives greyscale printing,
                               colour-vision deficiency and a projector.
       --identity              the wordmark and at most one primary action.
                               Never a state: --identity #00D1F9 against
                               --warn #D9A62E is 1.23:1, so a reader cannot
                               tell them apart by hue.
       --accent                what is selected. It IS the text colour, so
                               "active" is an ink/paper inversion rather than
                               a colour — the one dimension a reader cannot
                               mistake for a measurement.
     Cost and token counts get none of these. They are measurements, not
     verdicts; the old block painted cost in --warn, which told every reader
     that spending money was a fault condition. */
  :root {{
    --void: #010306; --ground: #070B10; --surface: #0D1319; --raised: #11171D;
    --hairline: #1F262D; --hairline-strong: #333B43;
    --identity: #00D1F9; --identity-ink: #070B10;
    --text: #E9EDF2; --text-2: #A4ABB3; --text-3: #737C88;
    --ok: #3FD99B; --warn: #D9A62E; --bad: #F2687A;
    --c1: #E9EDF2; --c2: #A4ABB3; --c3: #737C88; --c4: #4A535D;
    --neutral-mark: #4A535D;
    --ink: #070B10;
    --accent: #E9EDF2;
    --accent-wash: rgba(233,237,242,.08);
    --accent-edge: rgba(233,237,242,.38);
    --sunken: #0B1116;
    --control-edge: #737C88;

    /* One face. The product lives in a terminal, so the brand speaks in
       monospace — and this artifact is a measurement, where a proportional
       digit is a defect. Named, never fetched: the CSP below is
       `default-src 'none'`, so an @font-face with a URL would be a page that
       silently renders in the fallback. */
    --mono: "JetBrains Mono", ui-monospace, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace;
    --fs-micro: 11px; --fs-sm: 12px; --fs-base: 13px; --fs-md: 15px;
    --fs-lg: 22px; --fs-xl: 28px; --fs-metric: 40px;
    --fw-prose: 300; --fw-ui: 500; --fw-metric: 600;
    --tr-label: .16em; --tr-heading: -.01em; --tr-display: -.02em;
    --measure: 68ch;
    /* One 8px unit; every gutter, gap and pad is a multiple of it. The nine
       Instrument steps are named by value so a rule spelling `var(--sp-12)`
       documents itself; --sp1..--sp4 stay as aliases because the rules below
       spell them, and renaming a token that the chart JS also names inside a
       string literal is how this page once shipped a colourless chart. */
    --sp-4: 4px;   --sp-8: 8px;   --sp-12: 12px; --sp-16: 16px; --sp-24: 24px;
    --sp-32: 32px; --sp-48: 48px; --sp-64: 64px; --sp-96: 96px;
    --sp1: var(--sp-8); --sp2: var(--sp-16); --sp3: var(--sp-24); --sp4: var(--sp-32);
    /* Motion reports progress or it does not exist: 120ms linear for a state
       change, because the eye should not be able to watch a hover happen, and
       240ms on the Instrument easing for a reveal. There is no third. */
    --dur-state: 120ms; --dur-reveal: 240ms;
    --ease-reveal: cubic-bezier(.2,0,0,1);
    /* Square. A rounded corner says "surface" where a hairline says
       "boundary", and a telemetry report is all boundaries. Kept as a token
       rather than deleted so the rules below stay honest about where a corner
       is being set, and so the decision lives in one line. */
    --radius: 0;
  }}

  /* ── Light mode ────────────────────────────────────────────────────────
     Byte-identical to the Observatory's light scheme and to arenabench's.
     This report is mailed around and attached to PRs, so it lands in readers'
     browsers, not ours — it was dark-only, which meant half of them opened a
     black page on a white desktop.

     Contrast, computed against WCAG 2.1 relative luminance, worst case on
     --surface (#F7F9FC):

       --text 18.97:1  --text-2 7.49:1  --text-3 3.10:1
       --ok 5.91:1     --warn 5.99:1    --bad 6.74:1    --identity 8.33:1

     and dark against --raised (#11171D):

       --text 16.13:1  --text-2 7.31:1  --text-3 3.70:1
       --ok 8.52:1     --warn 7.81:1    --bad 6.17:1    --identity 6.34:1

     --text and --text-2 clear AAA on both; the semantic three and --identity
     clear AA on both. --text-3 clears neither and is not meant to: it carries
     labelling only — a units suffix, a legend key — never a value a reader
     must act on.

     Two gates, the same pattern as docs/brand/css/tokens.css: the OS
     preference unless the page was told "dark" explicitly, and an explicit
     `data-theme` the reader can stamp. No colour is defined only inside the
     media query, so the attribute wins in both directions. */
  @media (prefers-color-scheme: light) {{
    :root:not([data-theme="dark"]) {{
      color-scheme: light;
      --void: #E9EDF2; --ground: #FFFFFF; --surface: #F7F9FC; --raised: #FFFFFF;
      --hairline: #E7EBF0; --hairline-strong: #CFD6DD;
      --identity: #00778F; --identity-ink: #FFFFFF;
      --text: #070B10; --text-2: #4D535A; --text-3: #828C97;
      --ok: #0B6B3D; --warn: #7A5200; --bad: #A82036;
      --c1: #070B10; --c2: #4D535A; --c3: #828C97; --c4: #C2C9D1;
      --neutral-mark: #C2C9D1;
      --ink: #FFFFFF;
      --accent: #070B10;
      --accent-wash: rgba(7,11,16,.06);
      --accent-edge: rgba(7,11,16,.28);
      --sunken: #F3F6F9;
      --control-edge: #828C97;
    }}
  }}
  :root[data-theme="light"] {{
    color-scheme: light;
    --void: #E9EDF2; --ground: #FFFFFF; --surface: #F7F9FC; --raised: #FFFFFF;
    --hairline: #E7EBF0; --hairline-strong: #CFD6DD;
    --identity: #00778F; --identity-ink: #FFFFFF;
    --text: #070B10; --text-2: #4D535A; --text-3: #828C97;
    --ok: #0B6B3D; --warn: #7A5200; --bad: #A82036;
    --c1: #070B10; --c2: #4D535A; --c3: #828C97; --c4: #C2C9D1;
    --neutral-mark: #C2C9D1;
    --ink: #FFFFFF;
    --accent: #070B10;
    --accent-wash: rgba(7,11,16,.06);
    --accent-edge: rgba(7,11,16,.28);
    --sunken: #F3F6F9;
    --control-edge: #828C97;
  }}
  * {{ box-sizing: border-box; margin: 0; padding: 0; }}
  html {{ color-scheme: dark; }}
  body {{
    font-family: var(--mono); font-size: var(--fs-base);
    background: var(--ground); color: var(--text);
    line-height: 1.55; padding: var(--sp3); max-width: 1280px; margin: 0 auto;
  }}
  /* The masthead. `stella` is the wordmark in text form, which is the one
     place --identity is permitted; the rest of the title is --text. */
  h1 {{ font-size: var(--fs-xl); font-weight: 600; letter-spacing: -.02em; margin-bottom: 4px; color: var(--text); }}
  h1 .wordmark {{ color: var(--identity); }}
  /* Section rules are boundaries, so they are hairlines and the heading is
     ordinary text. This heading used to be painted in the brand hue, which
     made every section title compete with the data underneath it. */
  h2 {{ font-size: var(--fs-md); font-weight: 600; margin: var(--sp4) 0 var(--sp2); color: var(--text);
       border-bottom: 1px solid var(--hairline-strong); padding-bottom: var(--sp1); letter-spacing: .02em; }}
  .watermark {{ color: var(--text-3); font-size: var(--fs-micro); margin-bottom: var(--sp1); }}
  .scope {{ color: var(--text-2); font-size: var(--fs-sm); margin-bottom: var(--sp3); padding: var(--sp1) var(--sp2);
            background: var(--surface); border-left: 2px solid var(--accent-edge); border-radius: var(--radius); }}
  .kpi-grid {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: var(--sp1); margin-bottom: var(--sp1); }}
  .kpi {{
    background: var(--surface); border: 1px solid var(--hairline); border-radius: var(--radius); padding: var(--sp2);
  }}
  .kpi .label {{ font-size: var(--fs-micro); text-transform: uppercase; letter-spacing: .14em; color: var(--text-3); margin-bottom: 4px; }}
  .kpi .value {{ font-size: var(--fs-xl); font-weight: 600; font-variant-numeric: tabular-nums; }}
  .kpi .sub {{ font-size: var(--fs-micro); color: var(--text-2); margin-top: 2px; font-variant-numeric: tabular-nums; }}
  .kpi.good .value {{ color: var(--ok); }}
  .kpi.warn .value {{ color: var(--warn); }}
  /* Cost deliberately takes no hue. It is a measurement, not a verdict — it
     was painted --warn, which told the reader that spending was a fault. */
  .kpi.cost .value {{ color: var(--text); }}
  table {{ width: 100%; border-collapse: collapse; background: var(--surface); border-radius: var(--radius); }}
  th, td {{ padding: var(--sp1) var(--sp2); text-align: left; font-size: var(--fs-sm); border-bottom: 1px solid var(--hairline); }}
  th {{ background: var(--raised); color: var(--text-2); font-weight: 600; font-size: var(--fs-micro);
       text-transform: uppercase; letter-spacing: .14em; }}
  tr:last-child td {{ border-bottom: none; }}
  td.num {{ text-align: right; font-variant-numeric: tabular-nums; }}
  /* Badges are hairline-edged rather than wash-filled: a filled chip reads as
     a control the reader can press, and nothing on this page is operable. */
  .badge {{ display: inline-block; padding: 1px 6px; border: 1px solid currentColor; border-radius: var(--radius);
           font-size: var(--fs-micro); font-weight: 600; letter-spacing: .06em; text-transform: uppercase; }}
  .badge.completed {{ color: var(--ok); }}
  .badge.failed {{ color: var(--bad); }}
  .badge.other {{ color: var(--text-2); }}
  .chart-container {{ background: var(--surface); border: 1px solid var(--hairline); border-radius: var(--radius);
                     padding: var(--sp2); margin-bottom: var(--sp2); overflow-x: auto; }}
  .bar-chart {{ display: flex; flex-direction: column; gap: 4px; }}
  .bar-row {{ display: flex; align-items: center; gap: var(--sp1); font-size: var(--fs-sm); }}
  .bar-row .bar-label {{ width: 200px; text-align: right; color: var(--text-2); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }}
  .bar-row .bar-track {{ flex: 1; background: var(--sunken); border-radius: var(--radius); height: 22px; position: relative; min-width: 100px; }}
  .bar-row .bar-fill {{ height: 100%; border-radius: var(--radius); background: var(--c1); transition: width 0.3s; }}
  .bar-row .bar-value {{ width: 60px; color: var(--text-3); font-size: var(--fs-micro); font-variant-numeric: tabular-nums; }}
  .pie-legend {{ display: flex; gap: var(--sp2); flex-wrap: wrap; margin-top: var(--sp1); font-size: var(--fs-sm); }}
  .pie-legend span {{ display: flex; align-items: center; gap: 4px; }}
  .dot {{ width: 10px; height: 10px; border-radius: var(--radius); display: inline-block; }}
  .insight {{ background: var(--surface); border-left: 2px solid var(--accent-edge); padding: var(--sp2);
             border-radius: var(--radius); margin-bottom: var(--sp1); font-size: var(--fs-base); }}
  .insight .insight-label {{ color: var(--text); font-weight: 600; font-size: var(--fs-micro);
                            text-transform: uppercase; letter-spacing: .14em; }}
  .footer {{ margin-top: var(--sp4); padding-top: var(--sp2); border-top: 1px solid var(--hairline);
            color: var(--text-3); font-size: var(--fs-micro); }}
  .footer b {{ color: var(--identity); font-weight: 600; }}

  /* ── The transcript ─────────────────────────────────────────────────────
     Every entry is `.ev` with three parts: a timestamp, a kind label, and
     content. Kept identical across kinds so the eye scans the left two
     columns once and never re-learns the row.

     It takes the tokens above rather than a palette of its own — the rule
     this page runs on is that colour is meaning, so `--ok`/`--bad` mark a
     verdict and nothing else takes a hue. A transcript is the densest thing
     here and would have been the easiest place to break that. */
  .tabs {{ display: flex; gap: var(--sp1); margin-bottom: var(--sp2); flex-wrap: wrap;
          position: sticky; top: 0; background: var(--ground); padding: var(--sp1) 0; z-index: 5;
          border-bottom: 1px solid var(--hairline); }}
  .tab {{ font: inherit; cursor: pointer; background: var(--surface); color: var(--text-2);
         border: 1px solid var(--hairline-strong); border-radius: var(--radius);
         padding: 6px var(--sp2); }}
  .tab[aria-selected="true"] {{ color: var(--text); border-color: var(--control-edge); }}
  .tab .n {{ color: var(--text-3); font-size: var(--fs-micro); }}
  .tab:focus-visible {{ outline: 2px solid var(--accent-edge); outline-offset: 2px; }}
  .panel {{ display: none; }} .panel.on {{ display: block; }}

  .ev {{ margin: 0 0 3px; padding: 5px var(--sp1); border-left: 2px solid transparent;
        background: var(--surface); border-radius: var(--radius); }}
  .t {{ color: var(--text-3); font-variant-numeric: tabular-nums; margin-right: 10px;
       font-size: var(--fs-micro); white-space: pre; }}
  .lbl {{ display: inline-block; min-width: 74px; color: var(--text-3);
         font-size: var(--fs-micro); letter-spacing: .14em; margin-right: var(--sp1); }}
  .meta {{ color: var(--text-2); font-size: var(--fs-micro); margin-left: 92px; }}
  .ev.stage {{ background: transparent; border-left-color: var(--control-edge);
              margin: var(--sp2) 0 6px; padding-top: var(--sp1);
              border-top: 1px solid var(--hairline); }}
  .ev.stage b {{ letter-spacing: .14em; text-transform: uppercase; font-size: var(--fs-sm); }}
  .ev.step {{ background: var(--sunken); }}
  .ev.say .prose, .ev.user .prose, .ev.think .prose {{
    margin-left: 92px; white-space: pre-wrap; word-break: break-word; max-width: 78ch; }}
  .ev.say {{ border-left-color: var(--accent-edge); }}
  .ev.user {{ border-left-color: var(--control-edge); }}
  .ev.think .prose {{ color: var(--text-2); font-style: italic; }}
  /* The two places a hue is earned: a verdict passed, or something failed. */
  .ev.verdict {{ border-left-color: var(--ok); }}
  .ev.proof {{ border-left-color: var(--accent-edge); }}
  .ev.err, .ev.tool.err, .ev.verdict.err {{ border-left-color: var(--bad); }}
  details.ev {{ padding: 0; border-left-color: var(--hairline-strong); }}
  details.ev summary {{ cursor: pointer; padding: 5px var(--sp1); list-style: none; }}
  details.ev summary::-webkit-details-marker {{ display: none; }}
  details.ev summary:hover {{ background: var(--sunken); }}
  details.ev[open] summary {{ border-bottom: 1px solid var(--hairline); }}
  .ev pre {{ margin: 0; padding: var(--sp1) 10px var(--sp1) 100px; white-space: pre-wrap;
            word-break: break-word; font: inherit; overflow-x: auto; }}
  pre.in {{ color: var(--text); background: var(--sunken); }}
  pre.out {{ color: var(--text-2); border-top: 1px dashed var(--hairline);
            max-height: 340px; overflow: auto; }}
  pre.out.err {{ color: var(--bad); }}
  pre.out.pending {{ color: var(--text-3); font-style: italic; }}
  /* Only the fallback for diff text that parses to no hunks at all. */
  pre.diff {{ color: var(--text-2); max-height: 300px; overflow: auto; }}

  /* Unified diff — git's shape, drawn the way a reviewer reads it and
     byte-for-byte the component the Observatory uses
     (`crates/stella-observatory/src/assets/index.html`). This archive is
     attached to a pull request as evidence and read beside that dashboard, so
     the two must not render one edit two ways. Old and new line numbers get
     their own gutters, the sigil gets a third, and the change is carried by a
     tinted ROW rather than by a coloured glyph — hue is never the only signal
     (BRAND.md), and it is the only signal that survives a monochrome print. */
  /* Square, like everything else on this page: a rounded corner says
     "surface" where a hairline says "boundary", and this page is boundaries
     (`the_dashboard_is_monospace_and_square` enforces it). */
  .dx {{ overflow-x: auto; border: 1px solid var(--hairline);
        background: var(--sunken); margin: var(--sp1) 10px var(--sp1) 100px; }}
  .dx-hunk {{ min-width: 100%; }}
  .dx-hunk + .dx-hunk {{ border-top: 1px solid var(--hairline); }}
  .dx-at {{ color: var(--text-3); background: var(--raised); padding: 3px 12px;
           white-space: pre; border-bottom: 1px solid var(--hairline); }}
  .dx-line {{ display: grid; grid-template-columns: 52px 52px 22px minmax(max-content, 1fr);
             color: var(--text-2); }}
  .dx-line > span {{ padding: 0 6px; white-space: pre; }}
  .dx-line .no, .dx-line .nn {{ color: var(--text-3); text-align: right;
    font-variant-numeric: tabular-nums; -webkit-user-select: none; user-select: none; }}
  .dx-line .sg {{ text-align: center; -webkit-user-select: none; user-select: none; }}
  .dx-line.add {{ background: rgba(74, 222, 128, .10); color: var(--text); }}
  .dx-line.add .sg {{ color: var(--ok); }}
  .dx-line.rem {{ background: rgba(255, 92, 122, .10); color: var(--text); }}
  .dx-line.rem .sg {{ color: var(--bad); }}
  /* Word-level highlight: the exact changed tokens inside a paired
     removal/addition, a stronger wash of the same hue the row already
     carries — never a third colour, so the rule stays "the row's tint says
     which side, the token's tint says which part". */
  .dx-line.add .ww {{ background: rgba(74, 222, 128, .34); }}
  .dx-line.rem .ww {{ background: rgba(255, 92, 122, .34); }}
  /* The elision sits between the hunks it replaces, so it has to read as
     unmistakably not a line of the file: raised ground, centred, no gutter. */
  .dx-fold {{ color: var(--text-3); background: var(--raised); text-align: center;
             padding: 2px 12px; -webkit-user-select: none; user-select: none; }}
  .dx-hunk + .dx-fold, .dx-fold + .dx-hunk {{ border-top: 1px solid var(--hairline); }}
  .empty {{ color: var(--text-2); padding: 10px 0; }}

  /* Under 720px the 92px indent costs more than it buys — the label becomes a
     row of its own and every indent collapses to the gutter. */
  @media (max-width: 720px) {{
    .lbl {{ min-width: 0; display: block; margin: 0 0 2px; }}
    .meta, .ev .prose {{ margin-left: 0; }}
    .ev pre {{ padding-left: 10px; }}
  }}

  /* A stated preference for less motion wins. Every transition on this page
     reports a state change rather than carrying information, so removing them
     costs the reader nothing — and this is a report people stare at. */
  @media (prefers-reduced-motion: reduce) {{
    * {{ animation: none !important; transition: none !important; }}
  }}

  /* Print. The module doc tells you to attach this to a PR or email it, and a
     dark instrument printed on paper is a black rectangle that empties a
     cartridge. The panels are forced open because `display:none` cannot be
     paged: without this, printing the report gives you whichever single tab
     happened to be selected. */
  @media print {{
    :root {{
      --ground: #FFFFFF; --surface: #FFFFFF; --raised: #FFFFFF; --sunken: #FFFFFF;
      --hairline: #CFD6DD; --hairline-strong: #828C97;
      --text: #070B10; --text-2: #333333; --text-3: #555555;
      --accent: #070B10; --ink: #FFFFFF; --identity: #00778F;
      --accent-wash: transparent;
    }}
    body {{ padding: 0; }}
    .tabs {{ display: none; }}
    .panel {{ display: block !important; }}
    .ev, table, .chart-container {{ break-inside: avoid; }}
  }}
</style>
</head>
<body>

<h1><span class="wordmark">stella</span> session telemetry</h1>
<div class="watermark">session {session} · as of {watermark}</div>
<div class="scope">This archive covers <strong>one session</strong> — {scope_note}.</div>

<div class="kpi-grid">
  <div class="kpi"><div class="label">Total Runs</div><div class="value">{total_runs}</div><div class="sub">{total_resolved} resolved</div></div>
  <div class="kpi good"><div class="label">Resolve Rate</div><div class="value">{resolve_rate:.1}%</div><div class="sub">{total_resolved}/{total_runs}</div></div>
  <div class="kpi cost"><div class="label">Total Cost</div><div class="value">${total_cost:.4}</div><div class="sub">${cost_per_resolved:.4}/resolved</div></div>
  <div class="kpi"><div class="label">Tokens In</div><div class="value">{total_input_fmt}</div><div class="sub">{total_cache_read_fmt} cache reads</div></div>
  <div class="kpi"><div class="label">Tokens Out</div><div class="value">{total_output_fmt}</div><div class="sub">generated</div></div>
</div>

<div class="tabs" role="tablist">
  <button class="tab" data-target="transcript" role="tab" aria-selected="true">Transcript <span class="n">{transcript_rows}</span></button>
  <button class="tab" data-target="metrics" role="tab" aria-selected="false">Metrics</button>
</div>

<!-- `on` is set HERE, not by the script. The tabs are a convenience; the
     archive is evidence, and a reader with scripts disabled must not open it
     to a blank page — which is exactly what a JS-assigned initial class gives
     them, since the CSP already forbids the page every other resource. The
     script re-asserts this class on load and then owns it. -->
<section id="transcript" class="panel on" role="tabpanel">
  <div class="watermark">{transcript_provenance}</div>
  {transcript_body}
</section>

<section id="metrics" class="panel" role="tabpanel">
<div id="insights"></div>

<h2>Cost &amp; Efficiency by Model</h2>
<div id="stats-table"></div>

<h2>Token Economy</h2>
<div class="chart-container">
  <div id="token-chart" class="bar-chart"></div>
</div>

<h2>Tool Usage</h2>
<div class="chart-container">
  <div id="tool-chart" class="bar-chart"></div>
</div>

<h2>Files Touched</h2>
<div class="chart-container">
  <div id="file-chart" class="bar-chart"></div>
</div>

<h2>Execution Outcomes</h2>
<div class="chart-container">
  <div id="outcome-chart" class="bar-chart"></div>
</div>
</section>

<div class="footer">
  Exported by <b>stella</b> <code>/export</code> · {total_runs} executions ·
  All data is local (no server, no account) · Dashboard is fully self-contained
</div>

<script>
const USAGE = {stats_json};
const TELEMETRY = {telemetry_json};
const TOOL_CALLS = {tool_calls_json};
const EXECUTIONS = {executions_json};
const FILES = {files_json};

// ── HTML escape for the innerHTML sinks below ───────────────────────────
// Every renderer in this script assigns to `innerHTML`, and the strings it
// interpolates — workspace file paths, MCP/tool names, provider and model
// ids, execution outcomes — are all text an agent, an MCP server, or a
// cloned repo chooses. `script_json` escapes `<`/`>`/`&` only for the HTML
// tokenizer, so the element cannot be closed early; the JS parser decodes
// them straight back, and the live string reaches `innerHTML`. Escape at
// the sink, which is the only place that knows the value is about to
// become markup.
const esc = s => String(s).replace(/[&<>"']/g, c => ({{'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}}[c]));

// ── Tabs ────────────────────────────────────────────────────────────────
// The transcript opens first: it is what the archive is for, and the metrics
// are the summary of it. Everything is in the document either way — the tabs
// only choose what is displayed, so Ctrl-F still finds a tool call on the tab
// you are not looking at.
(function tabs() {{
  const buttons = [...document.querySelectorAll('.tab')];
  const panels = [...document.querySelectorAll('.panel')];
  const show = id => {{
    panels.forEach(p => p.classList.toggle('on', p.id === id));
    buttons.forEach(b => b.setAttribute('aria-selected', String(b.dataset.target === id)));
  }};
  buttons.forEach(b => b.addEventListener('click', () => show(b.dataset.target)));
  if (buttons.length) show(buttons[0].dataset.target);
}})();

// ── KPI insights — surface the patterns that change quality ─────────────
(function insights() {{
  const el = document.getElementById('insights');
  const tips = [];

  // Cache hit rate.
  const totalIn = USAGE.reduce((s,r)=>s+r.input_tokens,0);
  const cacheRead = USAGE.reduce((s,r)=>s+r.cache_read_tokens,0);
  if (totalIn > 0) {{
    const rate = (cacheRead/totalIn*100).toFixed(1);
    if (rate > 50) tips.push({{label:'Cache Efficiency',text:`Prompt caching is saving ${{rate}}% of input tokens — the session is reusing context well.`}});
    else if (totalIn > 10000) tips.push({{label:'Cache Opportunity',text:`Only ${{rate}}% of input tokens were cache reads. Longer, stable system prompts with cache breakpoints would cut cost.`}});
  }}

  // Resolve rate.
  const runs = USAGE.reduce((s,r)=>s+r.runs,0);
  const resolved = USAGE.reduce((s,r)=>s+r.resolved,0);
  const rate = runs > 0 ? resolved/runs*100 : 0;
  if (runs >= 3) {{
    if (rate >= 80) tips.push({{label:'High Resolve Rate',text:`${{rate.toFixed(0)}}% of turns resolved successfully — the prompts and model are well-matched.`}});
    else if (rate < 50) tips.push({{label:'Low Resolve Rate',text:`Only ${{rate.toFixed(0)}}% of turns resolved. Consider clearer prompts, a stronger model, or the staged pipeline (/pipeline).`}});
  }}

  // Cost efficiency.
  const cost = USAGE.reduce((s,r)=>s+r.total_cost_usd,0);
  if (resolved > 0 && cost > 0) {{
    const per = (cost/resolved).toFixed(4);
    tips.push({{label:'Cost per Resolution',text:`Average $${{per}} per resolved task across all models.`}});
  }}

  // Most expensive model vs cheapest.
  if (USAGE.length > 1) {{
    const sorted = [...USAGE].sort((a,b)=>b.total_cost_usd-a.total_cost_usd);
    const top = sorted[0];
    if (top.total_cost_usd > 0) tips.push({{label:'Cost Concentration',text:`${{top.provider}}/${{top.model}} accounts for $${{top.total_cost_usd.toFixed(4)}} (${{(top.total_cost_usd/cost*100).toFixed(0)}}% of total spend).`}});
  }}

  // Retries — signal from telemetry.
  const retries = TELEMETRY.reduce((s,t)=>s+(t.retries||0),0);
  if (retries > 5) tips.push({{label:'Retry Pressure',text:`${{retries}} API retries this session — may indicate rate limiting or transient errors.`}});

  el.innerHTML = tips.map(t=>`<div class="insight"><div class="insight-label">${{esc(t.label)}}</div>${{esc(t.text)}}</div>`).join('');
}})();

// ── Stats table ─────────────────────────────────────────────────────────
(function statsTable() {{
  const el = document.getElementById('stats-table');
  if (!USAGE.length) {{ el.innerHTML = '<p style="color:var(--text-3)">No usage data.</p>'; return; }}
  let html = '<table><thead><tr><th>Provider</th><th>Model</th><th class="num">Runs</th><th class="num">Resolved</th><th class="num">Rate</th><th class="num">Cost</th><th class="num">$/Resolved</th><th class="num">In Tok</th><th class="num">Out Tok</th><th class="num">Avg ms</th></tr></thead><tbody>';
  for (const r of USAGE) {{
    const rate = r.runs > 0 ? (r.resolved/r.runs*100).toFixed(1)+'%' : '-';
    const perResolved = r.cost_per_resolved_usd != null ? '$'+r.cost_per_resolved_usd.toFixed(4) : '-';
    html += `<tr><td>${{esc(r.provider)}}</td><td>${{esc(r.model)}}</td><td class="num">${{r.runs}}</td><td class="num">${{r.resolved}}</td><td class="num">${{rate}}</td><td class="num">$${{r.total_cost_usd.toFixed(4)}}</td><td class="num">${{perResolved}}</td><td class="num">${{r.input_tokens.toLocaleString()}}</td><td class="num">${{r.output_tokens.toLocaleString()}}</td><td class="num">${{Math.round(r.avg_duration_ms)}}</td></tr>`;
  }}
  // Totals.
  const runs = USAGE.reduce((s,r)=>s+r.runs,0);
  const resolved = USAGE.reduce((s,r)=>s+r.resolved,0);
  const cost = USAGE.reduce((s,r)=>s+r.total_cost_usd,0);
  const inTok = USAGE.reduce((s,r)=>s+r.input_tokens,0);
  const outTok = USAGE.reduce((s,r)=>s+r.output_tokens,0);
  const rate = runs>0?(resolved/runs*100).toFixed(1)+'%':'-';
  const per = resolved>0?'$'+(cost/resolved).toFixed(4):'-';
  html += `<tr style="border-top:2px solid var(--hairline-strong)"><td colspan="2"><strong>TOTAL</strong></td><td class="num"><strong>${{runs}}</strong></td><td class="num"><strong>${{resolved}}</strong></td><td class="num"><strong>${{rate}}</strong></td><td class="num"><strong>$${{cost.toFixed(4)}}</strong></td><td class="num"><strong>${{per}}</strong></td><td class="num"><strong>${{inTok.toLocaleString()}}</strong></td><td class="num"><strong>${{outTok.toLocaleString()}}</strong></td><td class="num">—</td></tr>`;
  html += '</tbody></table>';
  el.innerHTML = html;
}})();

// ── Bar chart helper ────────────────────────────────────────────────────
// `colorVar` is the series token for the whole chart; a datum may override it
// with its own `colorVar` when the bar carries a verdict rather than a
// position in a series (see the outcome chart). The token name is chart code's
// own literal — never reader-supplied — so it is not run through `esc`, which
// is for the label and display text either side of it.
function barChart(containerId, data, colorVar) {{
  const el = document.getElementById(containerId);
  if (!data.length) {{ el.innerHTML = '<p style="color:var(--text-3)">No data.</p>'; return; }}
  const max = Math.max(...data.map(d=>d.value), 1);
  el.innerHTML = data.map(d => {{
    const pct = (d.value/max*100).toFixed(1);
    const fill = d.colorVar || colorVar;
    return `<div class="bar-row"><div class="bar-label" title="${{esc(d.label)}}">${{esc(d.label)}}</div><div class="bar-track"><div class="bar-fill" style="width:${{pct}}%;background:var(${{fill}})"></div></div><div class="bar-value">${{esc(d.display)}}</div></div>`;
  }}).join('');
}}

// ── Token economy chart ─────────────────────────────────────────────────
barChart('token-chart', USAGE.map(r=>({{label:r.provider+'/'+r.model, value:r.input_tokens, display:r.input_tokens.toLocaleString()}})), '--c1');

// ── Tool frequency chart ────────────────────────────────────────────────
(function toolChart() {{
  const counts = {{}};
  for (const c of TOOL_CALLS) {{ counts[c.name] = (counts[c.name]||0)+1; }}
  const data = Object.entries(counts)
    .map(([name,n])=>({{label:name, value:n, display:String(n)}}))
    .sort((a,b)=>b.value-a.value)
    .slice(0,15);
  barChart('tool-chart', data, '--c2');
}})();

// ── Files touched chart ─────────────────────────────────────────────────
(function fileChart() {{
  const data = FILES
    .map(f=>({{label:f.path, value:(f.lines_added||0)+(f.lines_removed||0), display:'+'+(f.lines_added||0)+'/-'+(f.lines_removed||0)}}))
    .sort((a,b)=>b.value-a.value)
    .slice(0,15);
  barChart('file-chart', data, '--c3');
}})();

// ── Execution outcomes ──────────────────────────────────────────────────
(function outcomeChart() {{
  const counts = {{}};
  for (const e of EXECUTIONS) {{
    const o = e.outcome || 'open';
    counts[o] = (counts[o]||0)+1;
  }}
  // An outcome IS a verdict, so these bars are the one chart on the page
  // entitled to semantic colour. Every bar used to be painted --success,
  // including the failures — the chart said "pass" in the one channel the
  // palette reserves for saying it, about rows that did not.
  const verdict = o =>
    o === 'completed' || o === 'resolved' || o === 'success' ? '--ok'
    : o === 'failed' || o === 'error' || o === 'aborted' ? '--bad'
    : '--neutral-mark';
  const data = Object.entries(counts)
    .map(([name,n])=>({{label:name, value:n, display:String(n), colorVar:verdict(name)}}))
    .sort((a,b)=>b.value-a.value);
  barChart('outcome-chart', data, '--neutral-mark');
}})();
</script>

</body>
</html>"##
    )
}

// ── Minimal ZIP writer (store-only, no compression) ─────────────────────────
//
// We avoid a `zip` crate dependency by writing the simplest valid ZIP: stored
// (uncompressed) entries with correct CRC-32, local file headers, central
// directory, and end-of-central-directory record. This is fully compatible
// with every unzip tool and OS file explorer.
//
// Store-only is a deliberate trade, not a claim about the data: pretty-printed
// JSON compresses very well, so the archive is several times larger than a
// deflated one would be. What it buys is zero dependencies and no compression
// state machine to get wrong. A telemetry dump large enough for that size to
// matter is the signal to reach for a real zip crate — and to stream entries
// instead of assembling the whole archive in memory, which this writer also
// does not do.
//
// Sizes and offsets are classic-ZIP four-byte fields (entry counts two-byte).
// An export that would overflow them — a 4 GiB entry or archive, 65,536
// entries — is refused with an error rather than truncated into a silently
// corrupt archive; ZIP64 is deliberately out of scope.

/// CRC-32 lookup table (polynomial 0xEDB88320), built once and shared by
/// every `crc32` call — an export writes one entry per table dump plus the
/// dashboard and manifest, and there is no reason to redo the 256-entry
/// table for each.
fn crc32_table() -> &'static [u32; 256] {
    static TABLE: std::sync::LazyLock<[u32; 256]> = std::sync::LazyLock::new(|| {
        let mut table = [0u32; 256];
        for i in 0..256u32 {
            let mut c = i;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB88320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            table[i as usize] = c;
        }
        table
    });
    &TABLE
}

/// Compute CRC-32 for a byte slice.
fn crc32(data: &[u8]) -> u32 {
    let table = crc32_table();
    let mut crc = 0xFFFFFFFFu32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFFFFFF
}

/// Guard one classic-ZIP four-byte size/offset field. `what` names what
/// overflowed for the error message; the mapped value is what the header
/// stores. Testable with a plain length — no 4 GiB buffer required.
fn zip32_field(len: usize, what: &str) -> Result<u32, String> {
    u32::try_from(len).map_err(|_| {
        format!(
            "{what} is {len} bytes — past the 4 GiB classic-ZIP limit; refusing to write a \
             corrupt archive (ZIP64 is unsupported)"
        )
    })
}

/// A minimal stored-entry ZIP writer.
struct ZipWriter {
    entries: Vec<ZipEntry>,
    data: Vec<u8>,
}

struct ZipEntry {
    name: String,
    offset: u32,
    crc32: u32,
    size: u32,
}

impl ZipWriter {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            data: Vec::new(),
        }
    }

    fn add_file(&mut self, name: &str, content: &[u8]) -> Result<(), String> {
        let crc = crc32(content);
        let offset = zip32_field(self.data.len(), &format!("archive (at entry `{name}`)"))?;
        let size = zip32_field(content.len(), &format!("entry `{name}`"))?;

        // Local file header (PK\x03\x04)
        self.data.extend_from_slice(&[
            0x50, 0x4b, 0x03, 0x04, // signature
            0x14, 0x00, // version needed (2.0)
            0x00, 0x00, // flags
            0x00, 0x00, // compression: stored
            0x00, 0x00, // mod time
            0x00, 0x00, // mod date
        ]);
        self.data.extend_from_slice(&crc.to_le_bytes());
        self.data.extend_from_slice(&size.to_le_bytes()); // compressed size
        self.data.extend_from_slice(&size.to_le_bytes()); // uncompressed size
        let name_bytes = name.as_bytes();
        self.data
            .extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        self.data.extend_from_slice(&0u16.to_le_bytes()); // extra field length
        self.data.extend_from_slice(name_bytes);
        self.data.extend_from_slice(content);

        self.entries.push(ZipEntry {
            name: name.to_string(),
            offset,
            crc32: crc,
            size,
        });
        Ok(())
    }

    fn finish(mut self) -> Result<Vec<u8>, String> {
        // The EOCD stores the entry count in two bytes; past it lies ZIP64.
        let count = u16::try_from(self.entries.len()).map_err(|_| {
            format!(
                "{} zip entries — past the 65,535 classic-ZIP limit; refusing to write a \
                 corrupt archive (ZIP64 is unsupported)",
                self.entries.len()
            )
        })?;
        let cd_start = self.data.len();
        let cd_offset = zip32_field(cd_start, "archive (at central directory)")?;

        // Central directory file headers (PK\x01\x02)
        for entry in &self.entries {
            self.data.extend_from_slice(&[
                0x50, 0x4b, 0x01, 0x02, // signature
                0x14, 0x00, // version made by
                0x14, 0x00, // version needed
                0x00, 0x00, // flags
                0x00, 0x00, // compression: stored
                0x00, 0x00, // mod time
                0x00, 0x00, // mod date
            ]);
            self.data.extend_from_slice(&entry.crc32.to_le_bytes());
            self.data.extend_from_slice(&entry.size.to_le_bytes());
            self.data.extend_from_slice(&entry.size.to_le_bytes());
            let name_bytes = entry.name.as_bytes();
            self.data
                .extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            self.data.extend_from_slice(&0u16.to_le_bytes()); // extra
            self.data.extend_from_slice(&0u16.to_le_bytes()); // comment
            self.data.extend_from_slice(&0u16.to_le_bytes()); // disk number
            self.data.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            self.data.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            self.data.extend_from_slice(&entry.offset.to_le_bytes());
            self.data.extend_from_slice(name_bytes);
        }

        let cd_size = zip32_field(self.data.len() - cd_start, "central directory")?;

        // End of central directory (PK\x05\x06)
        self.data.extend_from_slice(&[
            0x50, 0x4b, 0x05, 0x06, // signature
            0x00, 0x00, // disk number
            0x00, 0x00, // disk with CD
        ]);
        self.data.extend_from_slice(&count.to_le_bytes()); // entries on this disk
        self.data.extend_from_slice(&count.to_le_bytes()); // total entries
        self.data.extend_from_slice(&cd_size.to_le_bytes());
        self.data.extend_from_slice(&cd_offset.to_le_bytes());
        self.data.extend_from_slice(&0u16.to_le_bytes()); // comment length

        Ok(self.data)
    }
}

mod transcript;

#[cfg(test)]
mod tests;
