//! `docs/tools/` — one TOML page per dispatchable tool, generated from the
//! declarations, and the guard that fails when the committed pages drift.
//!
//! A hand-written tool reference does not survive contact with a merge queue.
//! This repository has watched that happen twice: #1435 stranded three prose
//! copies of the god-file list behind a generated baseline, and #3029 found
//! `docs/prompts/worker.md` five contracts behind its own code with nothing
//! checking it. So these pages are not written, they are *derived*, and the
//! derivation is re-run by the gate:
//!
//! - `make tool-docs-update` regenerates `docs/tools/`.
//! - `make tool-docs` — a `make gate` step — regenerates into memory and fails
//!   on any difference, naming the file.
//!
//! The property that makes this worth building: adding a row to
//! [`stella_tools::catalog`] without regenerating turns the gate red. There is
//! no path where a new tool ships undocumented.
//!
//! # Where each field comes from
//!
//! | Field | Source of truth |
//! |---|---|
//! | `name`, `description`, `input_schema` | the tool's own [`ToolSchema`] |
//! | `read_only`, `available_for_speculation`, `category`, `availability`, `risk_level` | [`stella_tools::catalog`] |
//! | `output_schema` | [`ToolOutput`]'s serde encoding, read off the type |
//! | the example payloads | an observed bench capture, via a committed fixture |
//!
//! # Why this lives in a `#[cfg(test)]` module of a binary crate
//!
//! The generator needs the registry's live schemas, the committed example
//! fixture, and the repository root, and it needs to run inside `make gate`
//! on every push — a unit test compiled into the shipping binary's crate is
//! the one place all three meet without a dedicated exporter binary.
//! `make record-golden` establishes the fixture's shape — an env-var-blessed
//! test that rewrites it, with the plain test run as the drift guard.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};
use stella_protocol::{ToolOutput, ToolSchema};
use stella_tools::ToolRegistry;
use stella_tools::catalog::{self, Availability, ToolEntry};

/// The env var that turns the drift guard into a writer, spelled like
/// `STELLA_REFRESH_GOLDEN` because it is the same idea.
const REFRESH_ENV: &str = "STELLA_REFRESH_TOOL_DOCS";

/// Where the generated pages live, relative to the repository root.
const DOCS_DIR: &str = "docs/tools";

/// What `risk_level` means on these pages, and why it is declared rather than
/// computed.
///
/// This field used to read `"undeclared"`: nothing in the repository carried a
/// per-tool risk, because #2716's `ToolContract` — the machinery for it — had
/// been closed `wontfix`. #3060 asked for the fix to land as a `risk` column
/// on `ToolEntry`, declared once beside the flags it sits with and reviewed
/// like them, and that is where it now lives
/// ([`stella_tools::catalog::ToolEntry::risk`]).
///
/// The note survives the fix because the reasoning it records is still what
/// keeps the column honest: a rung derived from `read_only` +
/// `speculation_safe` would be arithmetic on two booleans this page already
/// prints, and it would put `save_state` — which writes into a self-deleting
/// `TempDir` — at the same rung as `task`, which spends money. The grades are
/// reviewed judgements against a stated rubric, which is exactly why they are
/// declared.
const RISK_NOTE: &str = "\
# risk_level: how bad one honest call is — a reviewed judgement, declared in
# crates/stella-tools/src/catalog.rs beside the flags above and graded against
# the rubric on `ToolEntry::risk` (#2716, #3060). A DIFFERENT axis from
# `read_only`: `task` mutates no file and spends real money, while
# `task_create` mutates a board that dies with the session. Deliberately not
# derived from the two booleans above — that would be a relabelling, not
# information. A policy grant is expressed as a ceiling over this grade, and
# every non-built-in tool (MCP, custom manifest) is graded `high` for being
# unreviewed.";

// ── the committed example fixture ───────────────────────────────────────────

/// The observed call/result pairs, distilled from a bench capture by
/// `scripts/build-tool-doc-examples.py` and committed so generation stays
/// hermetic.
#[derive(Debug, Deserialize)]
struct Fixture {
    provenance: Provenance,
    examples: BTreeMap<String, Example>,
    usage: BTreeMap<String, Usage>,
}

#[derive(Debug, Deserialize)]
struct Provenance {
    captured: String,
    source: String,
    corpus_rows: u64,
    corpus_tasks: u64,
    census_trials_scanned: u64,
    census_model_calls: u64,
    selection: String,
    scrubbing: String,
}

#[derive(Debug, Deserialize)]
struct Example {
    task: String,
    trial: String,
    outcome: String,
    input: String,
    input_truncated: bool,
    output: String,
    output_truncated: bool,
}

#[derive(Debug, Deserialize)]
struct Usage {
    calls: u64,
    trials_used: u64,
    calls_per_turn: f64,
    avg_first_step: Option<f64>,
    failures: u64,
    fail_rate: f64,
}

fn load_fixture() -> Fixture {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tool_doc_examples.json");
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/stella-cli sits two levels under the repository root")
        .to_path_buf()
}

// ── collecting every declared schema ────────────────────────────────────────

/// Every schema the native [`ToolRegistry`] advertises. One registry: every
/// catalog row registers unconditionally, so the constructor's surface IS the
/// documented surface.
fn native_schemas(scratch: &Path) -> BTreeMap<String, ToolSchema> {
    let registry = ToolRegistry::new(scratch.to_path_buf());
    registry
        .schemas()
        .into_iter()
        .map(|schema| (schema.name.clone(), schema))
        .collect()
}

// ── the output envelope, read off the type ──────────────────────────────────

/// The JSON Schema for [`ToolOutput`], derived from the type's own serde
/// encoding rather than transcribed beside it.
///
/// The tag (`ok`/`error`) and the payload field names (`content`/`message`)
/// are read out of what the two variants actually serialize to, so a
/// `#[serde(rename)]` on either arm changes these pages and the gate says so.
/// This is the whole of what is declared about a tool result: the envelope.
/// What rides *inside* `ok.content` is a per-tool text convention with no
/// schema anywhere behind it — which is why the observed example is the only
/// evidence of its shape, and why it is labelled observed rather than
/// specified.
fn output_schema() -> Value {
    fn arm(value: &Value) -> (String, String) {
        let object = value
            .as_object()
            .expect("ToolOutput serializes to an object");
        let (tag, payload) = object
            .iter()
            .next()
            .expect("an externally tagged enum has exactly one key");
        let field = payload
            .as_object()
            .expect("both arms carry a struct payload")
            .keys()
            .next()
            .expect("both arms carry exactly one field")
            .clone();
        (tag.clone(), field)
    }

    let ok = serde_json::to_value(ToolOutput::Ok {
        content: String::new(),
    })
    .expect("ToolOutput is Serialize");
    let error =
        serde_json::to_value(ToolOutput::error(String::new())).expect("ToolOutput is Serialize");
    let (ok_tag, ok_field) = arm(&ok);
    let (error_tag, error_field) = arm(&error);

    let variant = |tag: &str, field: &str| {
        json!({
            "type": "object",
            "required": [tag],
            "additionalProperties": false,
            "properties": {
                tag: {
                    "type": "object",
                    "required": [field],
                    "additionalProperties": false,
                    "properties": { field: { "type": "string" } }
                }
            }
        })
    };

    json!({
        "$comment": "stella_protocol::ToolOutput — the envelope every tool answers in. \
                     The envelope is declared; the payload inside is not.",
        "oneOf": [variant(&ok_tag, &ok_field), variant(&error_tag, &error_field)]
    })
}

// ── rendering ───────────────────────────────────────────────────────────────

fn availability_word(availability: Availability) -> &'static str {
    // Matched exhaustively on purpose: a new Availability variant should fail
    // to compile here rather than render as something plausible.
    match availability {
        Availability::Always => "always",
    }
}

/// A TOML multi-line literal string. Literal (not basic) because every value
/// that reaches here is JSON or prose full of quotes and backslashes, and a
/// literal string needs no escaping at all — at the price of being unable to
/// contain `'''`, which is asserted rather than silently mangled.
fn literal_block(label: &str, body: &str) -> String {
    assert!(
        !body.contains("'''"),
        "{label} contains ''' and cannot ride in a TOML literal string; \
         the generator must learn another quoting before this value can ship"
    );
    let body = body.trim_end();
    format!("'''\n{body}\n'''")
}

/// Greedy word wrap. Prose in these pages is generated from format strings
/// and would otherwise land as 300-character comment lines; the payloads it
/// sits beside are never wrapped, because a clipped example must stay
/// byte-faithful to what the run produced.
fn wrap(text: &str, width: usize) -> String {
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        out.push(line);
    }
    out.join("\n")
}

/// Every line of `body`, commented. The example payloads ride as comments
/// because that is what the maintainer asked for and because a comment cannot
/// be mistaken for the contract — the observed shape of `ok.content` is
/// evidence, not a declaration.
fn comment_block(body: &str) -> String {
    body.lines()
        .map(|line| {
            if line.is_empty() {
                "#".to_string()
            } else {
                format!("# {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn rule(title: &str) -> String {
    let width = 74usize.saturating_sub(title.chars().count() + 4);
    format!("# ── {title} {}", "─".repeat(width.max(1)))
}

/// The usage paragraph for one tool, or the honest absence.
///
/// Measurements are included, and they are the reason several of these pages
/// say anything at all: thirty-three declared tools were never called once
/// across 408 trials, and that is the most useful sentence on those pages.
/// They are comments rather than fields, and every one of them is
/// date-stamped, because a measurement ages and a schema does not — a reader
/// who finds `calls = 0` as a datum a year from now would reasonably read it
/// as a property of the tool instead of a property of one week in August.
fn usage_comment(name: &str, fixture: &Fixture) -> String {
    let p = &fixture.provenance;
    let Some(usage) = fixture.usage.get(name) else {
        return format!(
            "No usage measurement. `{name}` carries no row in the {captured} census, \
             which scanned {trials} trials — the census enumerates the schemas those \
             runs advertised, and this tool was not among them.",
            captured = p.captured,
            trials = p.census_trials_scanned,
        );
    };
    if usage.calls == 0 {
        return format!(
            "Never called. Across {trials} trials and {calls} model calls \
             (census of {captured}), `{name}` was advertised and chosen zero times. \
             That is a fact about the tool menu, not a defect in the tool — see #3032, \
             which this page is input to.",
            trials = p.census_trials_scanned,
            calls = p.census_model_calls,
            captured = p.captured,
        );
    }
    let first = usage
        .avg_first_step
        .map(|step| format!("{step:.1}"))
        .unwrap_or_else(|| "n/a".into());
    format!(
        "Measured {captured} across {trials} trials / {model_calls} model calls:\n\
         calls {calls} · trials that used it {trials_used}/{trials} · \
         calls per model call {per_turn:.4} · mean position of first use {first} · \
         failures {failures} ({fail_rate:.1}%).\n\
         A measurement, not a contract: it ages, the schema above does not.",
        captured = p.captured,
        trials = p.census_trials_scanned,
        model_calls = p.census_model_calls,
        calls = usage.calls,
        trials_used = usage.trials_used,
        per_turn = usage.calls_per_turn,
        failures = usage.failures,
        fail_rate = usage.fail_rate * 100.0,
    )
}

/// The observed example, or a stated absence with the reason for it.
fn example_comment(name: &str, fixture: &Fixture) -> String {
    let p = &fixture.provenance;
    let Some(example) = fixture.examples.get(name) else {
        let why = match fixture.usage.get(name).map(|usage| usage.calls) {
            // Absent from the census entirely: these are the tools whose
            // registration needs a backend the measured runs had none of, so
            // they were never advertised and never had the chance to be
            // called. That is a different fact from "offered and refused".
            None => "no call to record: it was not advertised in the runs the capture \
                     comes from, so it never had the chance to be called"
                .to_string(),
            Some(0) => "no call to record: it was advertised and never called".to_string(),
            Some(called) => format!(
                "{called} calls in the wider census, but none in the \
                 {rows}-row capture the examples are drawn from",
                rows = p.corpus_rows,
            ),
        };
        return wrap(
            &format!(
                "OBSERVED EXAMPLE: none.\n\
                 `{name}` has {why}. No payload is shown, because an invented one \
                 would teach the reader what the author imagined this tool is for."
            ),
            76,
        );
    };

    let clipped = |flag: bool| if flag { " (clipped)" } else { "" };
    let preamble = wrap(
        &format!(
            "OBSERVED EXAMPLE — a real call from a real run, not an illustration.\n\
             Run: {source}\n\
             Captured {captured} · task `{task}` · trial `{trial}` · result: \
             {outcome} arm.\n\
             Selection: {selection}.\n\
             Scrubbing: {scrubbing}.",
            source = p.source,
            captured = p.captured,
            task = example.task,
            trial = example.trial,
            outcome = example.outcome,
            selection = p.selection,
            scrubbing = p.scrubbing,
        ),
        76,
    );
    format!(
        "{preamble}\n\
         \n\
         input{input_clip}:\n\
         {input}\n\
         \n\
         output{output_clip}:\n\
         {output}",
        input_clip = clipped(example.input_truncated),
        output_clip = clipped(example.output_truncated),
        input = example.input,
        output = example.output,
    )
}

/// One tool's page.
fn render_tool(entry: &ToolEntry, schema: &ToolSchema, fixture: &Fixture) -> String {
    let header = format!(
        "# {name} — GENERATED FILE, DO NOT EDIT.\n\
         #\n\
         # Regenerate:   make tool-docs-update\n\
         # Guarded by:   make tool-docs (a `make gate` step)\n\
         #\n\
         # Where each field comes from:\n\
         #   name / description / input_schema      the tool's own ToolSchema\n\
         #   read_only / available_for_speculation / category / availability\n\
         #   risk_level                             crates/stella-tools/src/catalog.rs\n\
         #   output_schema                          stella_protocol::ToolOutput",
        name = entry.name,
    );

    let input_schema = serde_json::to_string_pretty(&schema.input_schema)
        .expect("a ToolSchema's input_schema is JSON by construction");
    let output_schema = serde_json::to_string_pretty(&output_schema())
        .expect("the derived output envelope is JSON by construction");

    format!(
        "{header}\n\
         \n\
         name = {name:?}\n\
         category = {category:?}\n\
         availability = {availability:?}\n\
         read_only = {read_only}\n\
         available_for_speculation = {speculation}\n\
         \n\
         {risk_note}\n\
         risk_level = {risk:?}\n\
         \n\
         description = {description}\n\
         \n\
         # The JSON Schema the model is handed for this tool's arguments, verbatim.\n\
         input_schema = {input_schema}\n\
         \n\
         # The envelope every tool answers in. Declared; what rides inside\n\
         # `ok.content` is a per-tool text convention with no schema behind it.\n\
         output_schema = {output_schema}\n\
         \n\
         {example_rule}\n\
         {example}\n\
         \n\
         {usage_rule}\n\
         {usage}\n",
        header = header,
        name = entry.name,
        category = entry.group,
        availability = availability_word(entry.availability),
        read_only = entry.read_only,
        speculation = entry.speculation_safe,
        risk_note = RISK_NOTE,
        risk = entry.risk.as_str(),
        description = literal_block("description", &schema.description),
        input_schema = literal_block("input_schema", &input_schema),
        output_schema = literal_block("output_schema", &output_schema),
        example_rule = rule("example"),
        example = comment_block(&example_comment(entry.name, fixture)),
        usage_rule = rule("usage"),
        usage = comment_block(&wrap(&usage_comment(entry.name, fixture), 76)),
    )
}

/// The directory's own index.
fn render_index(entries: &[&ToolEntry], fixture: &Fixture) -> String {
    let p = &fixture.provenance;
    let observed = entries
        .iter()
        .filter(|entry| fixture.examples.contains_key(entry.name))
        .count();
    let never_called = entries
        .iter()
        .filter(|entry| fixture.usage.get(entry.name).is_some_and(|u| u.calls == 0))
        .count();

    let mut out = String::new();
    out.push_str(
        "---\n\
         id: tool-reference\n\
         title: \"docs/tools/ — the generated per-tool reference\"\n\
         status: living\n\
         ---\n\
         \n\
         <!-- GENERATED FILE, DO NOT EDIT. Regenerate: make tool-docs-update -->\n\
         \n\
         # `docs/tools/`\n\
         \n",
    );
    out.push_str(&format!(
        "One TOML page per dispatchable tool — {count} of them — generated from the \
         declarations by `crates/stella-cli/src/tool_docs.rs` and re-derived by the \
         `tool-docs` gate step. A tool added to `crates/stella-tools/src/catalog.rs` \
         without regenerating turns the gate red; there is no path where a new tool \
         ships undocumented.\n\n",
        count = entries.len()
    ));
    out.push_str(
        "Each page carries the tool's name, description, input schema, output schema, \
         `read_only`, `available_for_speculation`, `risk_level`, category, and a \
         commented example input and output payload.\n\n\
         `risk_level` is a reviewed judgement declared beside the flags it sits with \
         in `crates/stella-tools/src/catalog.rs`, graded against the rubric on \
         `ToolEntry::risk` (#2716, #3060). It answers a different question from \
         `read_only` — what one honest call costs the world, rather than whether the \
         workspace changes — and is deliberately not derived from the booleans above \
         it, which would be a relabelling rather than information. A policy grant is \
         expressed as a ceiling over this grade; every tool that is not a built-in \
         (MCP, custom manifest) is graded `high` for being unreviewed.\n\n\
         One field remains a stated absence rather than a value, because inventing it \
         would manufacture a source of truth nobody reviewed:\n\n\
         - **`output_schema` is the envelope only.** Every tool answers in \
           `ToolOutput { ok | error }`, which is declared and is what the field \
           holds; the shape of the text inside `ok.content` is a per-tool \
           convention with nothing behind it, so the observed example is its only \
           evidence.\n\n",
    );
    out.push_str(&format!(
        "**Examples are observed, not written.** They come from {source}, \
         captured {captured} over {rows} call/result pairs across {tasks} tasks. \
         {observed} of {count} tools carry a real example; the rest say so. \
         {never_called} tools were never called once across {trials} trials and \
         {model_calls} model calls — the most interesting fact on those pages, and \
         input to #3032.\n\n",
        source = p.source,
        captured = p.captured,
        rows = p.corpus_rows,
        tasks = p.corpus_tasks,
        observed = observed,
        count = entries.len(),
        never_called = never_called,
        trials = p.census_trials_scanned,
        model_calls = p.census_model_calls,
    ));
    out.push_str(
        "| Tool | Category | Availability | Read-only | Speculation-safe | Observed example |\n",
    );
    out.push_str("|---|---|---|---|---|---|\n");
    for entry in entries {
        out.push_str(&format!(
            "| [`{name}`]({name}.toml) | {group} | {availability} | {read_only} | {speculation} | {example} |\n",
            name = entry.name,
            group = entry.group,
            availability = availability_word(entry.availability),
            read_only = if entry.read_only { "yes" } else { "no" },
            speculation = if entry.speculation_safe { "yes" } else { "no" },
            example = if fixture.examples.contains_key(entry.name) {
                "yes"
            } else {
                "none observed"
            },
        ));
    }
    out
}

/// Every file the generator would write, keyed by path relative to `docs/tools`.
fn generate() -> BTreeMap<String, String> {
    let fixture = load_fixture();
    let scratch = tempfile::tempdir().expect("a scratch dir for the schema-collecting registry");
    let schemas = native_schemas(scratch.path());

    let mut entries: Vec<&ToolEntry> = catalog::CATALOG.iter().collect();
    entries.sort_by_key(|entry| entry.name);

    let mut files = BTreeMap::new();
    for entry in &entries {
        let schema = schemas.get(entry.name).unwrap_or_else(|| {
            panic!(
                "`{}` is declared in the catalog but no layer produced a ToolSchema for it — \
                 either it is registered under a condition tool_docs does not construct, or \
                 the catalog row is for a tool that no longer exists",
                entry.name
            )
        });
        files.insert(
            format!("{}.toml", entry.name),
            render_tool(entry, schema, &fixture),
        );
    }
    files.insert("README.md".into(), render_index(&entries, &fixture));
    files
}

// ── the guard ───────────────────────────────────────────────────────────────

/// `docs/tools/` still matches the declarations — or `STELLA_REFRESH_TOOL_DOCS`
/// is set and it does now.
///
/// This is the whole point of the directory. The registry's rows and the
/// catalog's have to agree, and the only way to keep them agreeing through a
/// merge queue is to derive one from the other and fail when the derivation
/// drifts.
#[test]
fn tool_docs_match_the_declarations() {
    let root = repo_root().join(DOCS_DIR);
    let expected = generate();

    if std::env::var_os(REFRESH_ENV).is_some() {
        std::fs::create_dir_all(&root).expect("create docs/tools");
        let keep: BTreeSet<&str> = expected.keys().map(String::as_str).collect();
        for entry in std::fs::read_dir(&root).expect("read docs/tools") {
            let path = entry.expect("read a docs/tools entry").path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            // A retired tool must take its page with it, or the directory
            // becomes a graveyard that reads as current.
            if path.is_file() && !keep.contains(name.as_str()) {
                std::fs::remove_file(&path).expect("remove a stale tool page");
            }
        }
        for (name, body) in &expected {
            std::fs::write(root.join(name), body).expect("write a tool page");
        }
        return;
    }

    let mut stale = Vec::new();
    for (name, body) in &expected {
        match std::fs::read_to_string(root.join(name)) {
            Ok(found) if &found == body => {}
            Ok(_) => stale.push(format!(
                "  {DOCS_DIR}/{name} — differs from the declarations"
            )),
            Err(_) => stale.push(format!("  {DOCS_DIR}/{name} — missing")),
        }
    }
    if let Ok(dir) = std::fs::read_dir(&root) {
        for entry in dir.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if entry.path().is_file() && !expected.contains_key(&name) {
                stale.push(format!(
                    "  {DOCS_DIR}/{name} — not produced by the generator (a retired tool?)"
                ));
            }
        }
    }

    assert!(
        stale.is_empty(),
        "docs/tools/ no longer matches the tool declarations:\n{}\n\n\
         Regenerate and commit the diff, so the change is reviewable:\n\n    \
         make tool-docs-update\n\n\
         Then read the diff as a contract change, not as noise: a renamed field, a \
         narrowed input schema or a flipped read_only flag all change what the model \
         is told it may do.",
        stale.join("\n")
    );
}

/// Every generated page is valid TOML with the fields the reference promises.
///
/// The pages are assembled by string formatting — fast to read, and one
/// unescaped quote away from a file that looks fine and parses as nothing.
/// Parsing them back is the cheap proof that never happened.
#[test]
fn generated_pages_parse_and_carry_every_promised_field() {
    for (name, body) in generate() {
        if name.ends_with(".md") {
            continue;
        }
        let parsed: toml::Value =
            toml::from_str(&body).unwrap_or_else(|e| panic!("{name} is not valid TOML: {e}"));
        let table = parsed.as_table().expect("a tool page is a TOML table");
        for field in [
            "name",
            "category",
            "availability",
            "read_only",
            "available_for_speculation",
            "risk_level",
            "description",
            "input_schema",
            "output_schema",
        ] {
            assert!(table.contains_key(field), "{name} is missing `{field}`");
        }
        assert_eq!(
            table["name"].as_str(),
            Some(name.trim_end_matches(".toml")),
            "{name} documents a different tool than its filename claims"
        );
        // `risk_level` stopped being a declared gap in #2716/#3060, so the
        // assertion got stronger rather than disappearing: the page must print
        // the grade its catalog row actually declares. Presence alone would
        // let the generator drift from the declaration it derives from, which
        // is the one failure this whole directory exists to prevent.
        let declared = catalog::get(name.trim_end_matches(".toml"))
            .expect("every page is generated from a catalog row")
            .risk;
        assert_eq!(
            table["risk_level"].as_str(),
            Some(declared.as_str()),
            "{name} prints a risk grade its catalog row does not declare"
        );
        let schema = table["input_schema"]
            .as_str()
            .expect("input_schema is a string");
        serde_json::from_str::<Value>(schema)
            .unwrap_or_else(|e| panic!("{name}'s input_schema is not JSON: {e}"));
    }
}

/// The catalog and the generated directory describe the same set of tools.
///
/// Stated separately from the drift guard because it is a different claim: the
/// guard says the committed bytes match the generator, and this says the
/// generator covers every declared tool. A generator that silently skipped a
/// row would satisfy the first and fail this.
#[test]
fn every_catalog_row_gets_a_page() {
    let files = generate();
    for entry in catalog::CATALOG {
        assert!(
            files.contains_key(&format!("{}.toml", entry.name)),
            "`{}` is declared in the catalog but got no page",
            entry.name
        );
    }
    assert_eq!(
        files.len(),
        catalog::CATALOG.len() + 1,
        "the generator wrote a page for something the catalog does not declare"
    );
}
