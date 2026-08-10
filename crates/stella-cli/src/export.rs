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

/// A theme token as a CSS `#rrggbb` literal.
///
/// The dashboard is a standalone HTML file, so its palette has to be inlined —
/// but inlining was being done by hand, and the hand-written block drifted two
/// whole recolours behind the identity while sitting in an artifact users mail
/// around. Generating the values means the export cannot disagree with the
/// terminal it came from.
fn css_hex(color: ratatui::style::Color) -> String {
    let (r, g, b) = crate::plain::token_rgb(color);
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// The dark-mode custom properties, resolved from `stella_tui::theme`.
///
/// Every value here is *derived*, never typed: the hand-written block this
/// replaced had gone two recolours stale while sitting in an artifact users
/// mail around, so only the slot NAMES live in the template. That constraint
/// is what decides the mapping below — each reference slot takes the theme
/// token that already means what the slot is for, rather than the nearest hex:
///
/// - `--faint` is the reference's timestamp/label tone. The theme's
///   `TEXT_TERTIARY` is documented as exactly "labels, captions", and it is
///   also the accessible choice: the reference's own `#55534F` measures
///   **2.56:1** on its ground, where `TEXT_TERTIARY` is 5.71:1. That token
///   paints `.t` and `.lbl` on every row in the transcript, so sub-AA there is
///   not a detail.
/// - `--fail` takes `DANGER` ("error / failed"), not `ORACLE_RED` — which
///   happens to be the reference's exact `#F87171` but means "the test is red
///   before the patch", a healthy state. Matching the hex would have meant
///   borrowing a token whose whole purpose is to *not* say "something broke".
///
/// The light palette has no counterpart here and is written literally in the
/// template: a TUI has no light theme, so there is no source to derive it from
/// and nothing for it to drift against.
fn dark_tokens() -> String {
    use stella_tui::theme;
    format!(
        "--ground:{ground}; --surface:{surface}; --sunk:{sunk}; --line:{line};\n    \
         --ink:{ink}; --dim:{dim}; --faint:{faint};\n    \
         --stella:{stella}; --pass:{pass}; --fail:{fail}; --warn:{warn};",
        ground = css_hex(theme::GROUND),
        surface = css_hex(theme::SURFACE),
        sunk = css_hex(theme::RAISED),
        line = css_hex(theme::HAIRLINE_STRONG),
        ink = css_hex(theme::TEXT_PRIMARY),
        dim = css_hex(theme::TEXT_SECONDARY),
        faint = css_hex(theme::TEXT_TERTIARY),
        stella = css_hex(theme::ACCENT),
        pass = css_hex(theme::SUCCESS),
        fail = css_hex(theme::DANGER),
        warn = css_hex(theme::WARNING),
    )
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

    // The dark palette, resolved from the live theme rather than typed into
    // the template — see `dark_tokens`. Emitted twice (media query and
    // explicit opt-in), so it is built once here.
    let dark_tokens = dark_tokens();

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
  /* The page is an instrument's printout, not an app: mono everywhere, corners
     nearly square, and colour spent only where it carries meaning (the brand
     rule on a stage, pass/fail on a verdict). One grammar covers the metrics
     and the transcript, because they are two views of one run.

     LIGHT is the default and is written literally: a terminal has no light
     theme, so there is no token to derive these from and nothing for them to
     drift against. DARK is interpolated from `stella_tui::theme` — see
     `dark_tokens()` for why that half may never be typed by hand. */
  :root {{
    --ground:#FAFAF9; --surface:#FFFFFF; --sunk:#F2F1EE; --line:#E2E0DB;
    --ink:#1A1917; --dim:#6B6862; --faint:#77736B;
    --stella:#B57A00; --pass:#187A45; --fail:#C0392B; --warn:#7A5C00;
  }}
  @media (prefers-color-scheme: dark) {{
    :root:not([data-theme="light"]) {{ {dark_tokens} }}
  }}
  :root[data-theme="dark"] {{ {dark_tokens} }}

  * {{ box-sizing: border-box; }}
  body {{
    margin: 0; background: var(--ground); color: var(--ink);
    font: 13px/1.55 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    padding: 28px 20px 80px;
  }}
  .wrap {{ max-width: 1100px; margin: 0 auto; }}
  h1 {{ font-size: 16px; margin: 0 0 2px; letter-spacing: -.01em; }}
  h2 {{ font-size: 13px; margin: 26px 0 12px; font-weight: 600; color: var(--stella);
       border-top: 1px solid var(--line); padding-top: 10px; }}
  .sub {{ color: var(--dim); margin: 0 0 14px; font-size: 12px; }}
  .scope {{ color: var(--dim); font-size: 12px; margin: 0 0 22px; padding: 7px 10px;
           background: var(--surface); border: 1px solid var(--line);
           border-left: 3px solid var(--stella); border-radius: 3px; }}
  code {{ font: inherit; color: var(--ink); }}

  /* KPI cards — the reference's dl/dt/dd, one card per measure. */
  .cards {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(190px, 1fr));
           gap: 12px; margin-bottom: 22px; }}
  .card {{ background: var(--surface); border: 1px solid var(--line); border-radius: 3px;
          padding: 12px 14px; border-left-width: 3px; border-left-color: var(--stella); }}
  dt {{ color: var(--faint); font-size: 10px; text-transform: uppercase; letter-spacing: .06em; }}
  dd {{ margin: 2px 0 0; font-size: 19px; font-variant-numeric: tabular-nums; }}
  .card .sub {{ margin: 2px 0 0; font-size: 11px; color: var(--dim); }}
  .card.good dd {{ color: var(--pass); }}
  .card.cost dd {{ color: var(--warn); }}

  /* Tabs — the reference's sticky bar; here it switches the metrics view for
     the transcript rather than one arm for another. */
  .tabs {{ display: flex; gap: 6px; margin-bottom: 14px; flex-wrap: wrap; position: sticky;
          top: 0; background: var(--ground); padding: 8px 0; z-index: 5;
          border-bottom: 1px solid var(--line); }}
  .tab {{ font: inherit; cursor: pointer; background: var(--surface); color: var(--dim);
         border: 1px solid var(--line); border-radius: 3px; padding: 6px 12px; }}
  .tab[aria-selected="true"] {{ color: var(--stella); border-color: var(--stella); }}
  .tab .n {{ color: var(--faint); font-size: 11px; }}
  .tab:focus-visible {{ outline: 2px solid var(--stella); outline-offset: 2px; }}
  .panel {{ display: none; }} .panel.on {{ display: block; }}

  /* Row grammar — every transcript entry is `.ev` with a timestamp, a kind
     label, and content. Kept identical across kinds so the eye can scan the
     left two columns and never re-learn the row. */
  .ev {{ margin: 0 0 3px; padding: 5px 8px; border-left: 2px solid transparent;
        background: var(--surface); border-radius: 2px; }}
  .t {{ color: var(--faint); font-variant-numeric: tabular-nums; margin-right: 10px;
       font-size: 11px; white-space: pre; }}
  .lbl {{ display: inline-block; min-width: 74px; color: var(--faint); font-size: 10px;
         letter-spacing: .07em; margin-right: 8px; }}
  .meta {{ color: var(--dim); font-size: 11px; margin-left: 92px; }}
  .ev.stage {{ background: transparent; border-left-color: var(--stella); margin: 18px 0 6px;
              padding-top: 8px; border-top: 1px solid var(--line); border-radius: 0; }}
  .ev.stage b {{ letter-spacing: .08em; text-transform: uppercase; font-size: 12px; }}
  .ev.step {{ background: var(--sunk); }}
  .ev.say .prose, .ev.user .prose, .ev.think .prose {{
    margin-left: 92px; white-space: pre-wrap; word-break: break-word; max-width: 78ch; }}
  .ev.say {{ border-left-color: var(--pass); }}
  .ev.user {{ border-left-color: var(--dim); }}
  .ev.think .prose {{ color: var(--dim); font-style: italic; }}
  .ev.verdict, .ev.proof {{ border-left-color: var(--stella); }}
  .ev.err, .ev.tool.err, .ev.verdict.err {{ border-left-color: var(--fail); }}
  details.ev {{ padding: 0; border-left-color: var(--dim); }}
  details.ev summary {{ cursor: pointer; padding: 5px 8px; list-style: none; }}
  details.ev summary::-webkit-details-marker {{ display: none; }}
  details.ev summary:hover {{ background: var(--sunk); }}
  details.ev[open] summary {{ border-bottom: 1px solid var(--line); }}
  .ev pre {{ margin: 0; padding: 8px 10px 8px 100px; white-space: pre-wrap;
            word-break: break-word; font: inherit; overflow-x: auto; }}
  pre.in {{ color: var(--ink); background: var(--sunk); }}
  pre.out {{ color: var(--dim); border-top: 1px dashed var(--line); max-height: 340px; overflow: auto; }}
  pre.out.err {{ color: var(--fail); }}
  pre.out.pending {{ color: var(--faint); font-style: italic; }}
  pre.diff {{ color: var(--dim); max-height: 300px; overflow: auto; }}
  .empty {{ color: var(--dim); padding: 10px 0; }}

  /* Metrics view. */
  table {{ width: 100%; border-collapse: collapse; background: var(--surface);
          border: 1px solid var(--line); border-radius: 3px; }}
  th, td {{ padding: 6px 10px; text-align: left; font-size: 12px; border-bottom: 1px solid var(--line); }}
  th {{ background: var(--sunk); color: var(--faint); font-weight: 600; font-size: 10px;
       text-transform: uppercase; letter-spacing: .06em; }}
  tr:last-child td {{ border-bottom: none; }}
  td.num, th.num {{ text-align: right; font-variant-numeric: tabular-nums; }}
  .badge {{ display: inline-block; padding: 0 5px; border-radius: 2px; font-size: 10px;
           letter-spacing: .06em; text-transform: uppercase; border: 1px solid var(--line); }}
  .badge.completed {{ color: var(--pass); border-color: var(--pass); }}
  .badge.failed {{ color: var(--fail); border-color: var(--fail); }}
  .badge.other {{ color: var(--dim); }}
  .chart-container {{ background: var(--surface); border: 1px solid var(--line);
                     border-radius: 3px; padding: 12px; margin-bottom: 12px; overflow-x: auto; }}
  .bar-chart {{ display: flex; flex-direction: column; gap: 4px; }}
  .bar-row {{ display: flex; align-items: center; gap: 8px; font-size: 12px; }}
  .bar-row .bar-label {{ width: 200px; text-align: right; color: var(--dim); white-space: nowrap;
                        overflow: hidden; text-overflow: ellipsis; }}
  .bar-row .bar-track {{ flex: 1; background: var(--sunk); border-radius: 2px; height: 18px;
                        min-width: 100px; }}
  .bar-row .bar-fill {{ height: 100%; border-radius: 2px; background: var(--stella); }}
  .bar-row .bar-value {{ width: 66px; color: var(--faint); font-size: 11px;
                        font-variant-numeric: tabular-nums; }}
  .insight {{ background: var(--surface); border: 1px solid var(--line);
             border-left: 3px solid var(--stella); padding: 8px 12px; border-radius: 3px;
             margin-bottom: 6px; font-size: 12px; }}
  .insight .insight-label {{ color: var(--stella); font-size: 10px; text-transform: uppercase;
                            letter-spacing: .06em; margin-right: 8px; }}
  .footer {{ margin-top: 34px; padding-top: 12px; border-top: 1px solid var(--line);
            color: var(--faint); font-size: 11px; }}

  /* Under 720px the 92px indent costs more than it buys — the label becomes a
     row of its own and every indent collapses to the gutter. */
  @media (max-width: 720px) {{
    .lbl {{ min-width: 0; display: block; margin: 0 0 2px; }}
    .meta, .ev .prose {{ margin-left: 0; }}
    .ev pre {{ padding-left: 10px; }}
    .bar-row .bar-label {{ width: 110px; }}
  }}
</style>
</head>
<body>
<div class="wrap">

<h1>stella session — {session}</h1>
<p class="sub">as of {watermark} · every model step, tool call and result, in order</p>
<div class="scope">This archive covers <strong>one session</strong> — {scope_note}.</div>

<div class="cards">
  <div class="card"><dl><dt>runs</dt><dd>{total_runs}</dd></dl><p class="sub">{total_resolved} resolved</p></div>
  <div class="card good"><dl><dt>resolve rate</dt><dd>{resolve_rate:.1}%</dd></dl><p class="sub">{total_resolved}/{total_runs}</p></div>
  <div class="card cost"><dl><dt>cost</dt><dd>${total_cost:.4}</dd></dl><p class="sub">${cost_per_resolved:.4}/resolved</p></div>
  <div class="card"><dl><dt>tokens in</dt><dd>{total_input_fmt}</dd></dl><p class="sub">{total_cache_read_fmt} cache reads</p></div>
  <div class="card"><dl><dt>tokens out</dt><dd>{total_output_fmt}</dd></dl><p class="sub">generated</p></div>
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
  <p class="sub">{transcript_provenance}</p>
  {transcript_body}
</section>

<section id="metrics" class="panel" role="tabpanel">
  <div id="insights"></div>

  <h2>Cost &amp; efficiency by model</h2>
  <div id="stats-table"></div>

  <h2>Token economy</h2>
  <div class="chart-container">
    <div id="token-chart" class="bar-chart"></div>
  </div>

  <h2>Tool usage</h2>
  <div class="chart-container">
    <div id="tool-chart" class="bar-chart"></div>
  </div>

  <h2>Files touched</h2>
  <div class="chart-container">
    <div id="file-chart" class="bar-chart"></div>
  </div>

  <h2>Execution outcomes</h2>
  <div class="chart-container">
    <div id="outcome-chart" class="bar-chart"></div>
  </div>
</section>

<div class="footer">
  Exported by <strong>stella /export</strong> · {total_runs} executions ·
  all data is local (no server, no account) · this page is fully self-contained
</div>
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
// you are not looking at, and printing is unaffected.
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
  if (!USAGE.length) {{ el.innerHTML = '<p style="color:var(--faint)">No usage data.</p>'; return; }}
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
  html += `<tr style="border-top:2px solid var(--line)"><td colspan="2"><strong>TOTAL</strong></td><td class="num"><strong>${{runs}}</strong></td><td class="num"><strong>${{resolved}}</strong></td><td class="num"><strong>${{rate}}</strong></td><td class="num"><strong>$${{cost.toFixed(4)}}</strong></td><td class="num"><strong>${{per}}</strong></td><td class="num"><strong>${{inTok.toLocaleString()}}</strong></td><td class="num"><strong>${{outTok.toLocaleString()}}</strong></td><td class="num">—</td></tr>`;
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
  if (!data.length) {{ el.innerHTML = '<p style="color:var(--faint)">No data.</p>'; return; }}
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
