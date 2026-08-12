//! `docs/tools/` — one TOML page per dispatchable tool, generated from the
//! declarations, and the guard that fails when the committed pages drift.
//!
//! Seventy-eight tools is past the size where a hand-written reference survives
//! contact with a merge queue. This repository has watched that happen twice:
//! #1435 stranded three prose copies of the god-file list behind a generated
//! baseline, and #3029 found `docs/prompts/worker.md` five contracts behind
//! its own code with nothing checking it. So these pages are not written, they
//! are *derived*, and the derivation is re-run by the gate:
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
//! | `read_only`, `available_for_speculation`, `category`, `availability` | [`stella_tools::catalog`] |
//! | `output_schema` | [`ToolOutput`]'s serde encoding, read off the type |
//! | `risk_level` | **nothing declares it** — see [`RISK_NOTE`] |
//! | the example payloads | an observed bench capture, via a committed fixture |
//!
//! # Why this lives in a `#[cfg(test)]` module of a binary crate
//!
//! Because that is the only place all seventy-eight schemas exist at once.
//! Sixty-four are declared by `stella-tools`' registry; the other eight —
//! `ask_user`, the two skills-registry tools, the discovery trio,
//! `invoke_skill` and `recall_context` — are declared by layers inside
//! `stella-cli`, which has no `src/lib.rs` (deliberately: adding one would
//! reclassify its 400-odd `Result<_, String>` signatures as library
//! violations). An exporter binary cannot link them; a unit test compiled into
//! the crate can. `make record-golden` establishes the shape — an
//! env-var-blessed test that rewrites a committed fixture, with the plain test
//! run as the drift guard.
//!
//! The residue is filed: #3061 asks for the session layer's schema
//! declarations to sit behind a linkable seam, which would let this become an
//! ordinary exporter binary.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use stella_media::{
    CostDecision, ImageRequest, MediaArtifact, MediaCapabilities, MediaError, MediaJob,
    MediaJobStatus, MediaOperationJournal, MediaOperationRetention, MediaProvider, MediaSpendGate,
    MediaSpendRequest, SqliteMediaOperationJournal, VideoRequest,
};
use stella_protocol::{ToolOutput, ToolSchema};
use stella_tools::catalog::{self, Availability, ToolEntry};
use stella_tools::media::{
    HostDataIsolation, HostMediaOperation, MediaBackend, MediaOperationIdSource,
};
use stella_tools::registry::Tool;
use stella_tools::{RegistryOptions, ToolRegistry};

/// The env var that turns the drift guard into a writer, spelled like
/// `STELLA_REFRESH_GOLDEN` because it is the same idea.
const REFRESH_ENV: &str = "STELLA_REFRESH_TOOL_DOCS";

/// Where the generated pages live, relative to the repository root.
const DOCS_DIR: &str = "docs/tools";

/// The one thing in these pages that is a stated absence rather than a value.
///
/// `risk_level` was requested as a field and does not exist as data. Nothing
/// in [`stella_tools::catalog`], in [`ToolSchema`], or in the policy layer
/// carries a per-tool risk, and the one PR that would have introduced the
/// machinery for it (#2716's `ToolContract`) was closed `wontfix`.
///
/// Two options were available and only one is honest. Deriving a three-rung
/// ordering from `read_only` + `speculation_safe` is arithmetic on two
/// booleans the page already prints: it would put `save_state` — which writes
/// into a self-deleting `TempDir` — at the same rung as `bash`, and it would
/// read to every future maintainer as a reviewed judgement rather than as a
/// relabelling. Writing seventy-eight judgement calls by hand is worse: it
/// manufactures a source of truth nobody reviewed, in the one artifact whose
/// entire value proposition is that it is derived.
///
/// So the field is emitted, and its value is `"undeclared"`, with the note
/// below and an issue. That keeps the requested shape, keeps the page honest,
/// and puts the fix where it belongs: a `risk` column on `ToolEntry`, declared
/// once beside the flags it sits with and reviewed like them.
const RISK_NOTE: &str = "\
# risk_level: NOT DECLARED. Nothing in this repository carries a per-tool risk
# level — not the catalog, not ToolSchema, not the policy layer (#2716, which
# would have introduced the machinery, was closed wontfix). The two booleans
# above are the only machine-checked safety claims a tool makes. Relabelling
# them \"low/medium/high\" would add no information while reading as a reviewed
# judgement, and hand-writing 72 judgements would manufacture a source of truth
# nobody reviewed. Tracked in #3060: put a `risk` column on `ToolEntry`, where
# it is declared once and reviewed like every other column.";

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

/// An inert media provider. It exists so the registry will *register* the
/// three media tools; nothing here is ever called, because only `schema()` is
/// read. Media registration requires an approving host context, which in turn
/// requires process-free isolation — the same posture that withholds the web
/// and issue families — which is why the native side below builds two
/// registries and unions them.
struct InertMediaProvider;

#[async_trait]
impl MediaProvider for InertMediaProvider {
    fn id(&self) -> &str {
        "tool-docs-inert"
    }

    fn capabilities(&self) -> MediaCapabilities {
        MediaCapabilities {
            provider_id: self.id().into(),
            image: true,
            video: true,
            ..Default::default()
        }
    }

    async fn generate_image(&self, _request: ImageRequest) -> Result<MediaArtifact, MediaError> {
        Err(MediaError::Transport("tool-docs: never executed".into()))
    }

    async fn generate_video(&self, _request: VideoRequest) -> Result<MediaJob, MediaError> {
        Err(MediaError::Transport("tool-docs: never executed".into()))
    }

    async fn poll_video(&self, _job: &MediaJob) -> Result<MediaJobStatus, MediaError> {
        Err(MediaError::Transport("tool-docs: never executed".into()))
    }
}

struct InertSpendGate;

#[async_trait]
impl MediaSpendGate for InertSpendGate {
    async fn authorize(&self, _request: &MediaSpendRequest) -> CostDecision {
        CostDecision::Deny
    }
}

struct InertOperationId;

impl MediaOperationIdSource for InertOperationId {
    fn operation_id(&self) -> HostMediaOperation {
        HostMediaOperation {
            opaque_id: "tool-docs".into(),
            expires_at: 0,
        }
    }
}

/// Every schema the native [`ToolRegistry`] can advertise, with each optional
/// backend supplied.
///
/// Two registries, because no single one advertises all of them: the media
/// tools register only under an approving host context (process-free
/// isolation), and that same posture is what withholds the web and issue
/// families. Their union is the native surface — which is exactly the "up to
/// M native" ceiling [`catalog::native`] names.
fn native_schemas(scratch: &Path) -> BTreeMap<String, ToolSchema> {
    let mut schemas = BTreeMap::new();

    // Hosted posture: the web family and the issue family.
    let hosted = ToolRegistry::with_backends_and_options(
        scratch.join("hosted"),
        Some(stella_tools::issues::IssueBackend::GitHub),
        None,
        RegistryOptions::default(),
    );
    for schema in hosted.schemas() {
        schemas.insert(schema.name.clone(), schema);
    }

    // `web_search` is the one native tool no registry configuration reaches
    // from here: it registers on `detect_search_backend()`, which reads
    // BRAVE_API_KEY/TAVILY_API_KEY out of the process environment, and a
    // reference page that appears or vanishes with the generating machine's
    // env is not a reference page. So its declaration is read from the tool
    // type directly, through the injectable half of the same detection the
    // registry uses — the key is a placeholder and nothing is ever executed.
    let search = stella_tools::web::detect_search_backend_with(|_| Some("tool-docs-inert".into()))
        .expect("the injected env always yields a backend");
    let web_search = stella_tools::web::WebSearch(search).schema();
    schemas.insert(web_search.name.clone(), web_search);

    let journal: Arc<dyn MediaOperationJournal> = Arc::new(
        SqliteMediaOperationJournal::open(
            scratch.join("media/operations.db"),
            MediaOperationRetention::default(),
        )
        .expect("open an empty operations journal under a scratch dir"),
    );
    let media = ToolRegistry::with_backends_and_options(
        scratch.join("media"),
        None,
        Some(MediaBackend {
            image: Arc::new(InertMediaProvider),
            video: Some(Arc::new(InertMediaProvider)),
        }),
        RegistryOptions {
            media_requires_host_approval: true,
            media_spend_gate: Some(Arc::new(InertSpendGate)),
            media_operation_ids: Some(Arc::new(InertOperationId)),
            media_operation_journal: Some(journal),
            media_host_data_isolation: Some(HostDataIsolation::ProcessFree),
            ..Default::default()
        },
    );
    for schema in media.schemas() {
        schemas.entry(schema.name.clone()).or_insert(schema);
    }
    schemas
}

/// The eight schemas the CLI layers on top of the native registry.
///
/// Collected from the declaring functions rather than from a constructed tool
/// set, deliberately: a constructed set renders session-conditional text (the
/// lean-mode note `tool_search` grows, the tools a live skill grant withholds),
/// and a reference page documents the tool as *declared*, not as one session
/// happened to advertise it.
fn session_schemas() -> BTreeMap<String, ToolSchema> {
    let mut schemas = BTreeMap::new();
    for schema in crate::interactive::declared_session_schemas()
        .into_iter()
        .chain(crate::discovery::declared_discovery_schemas())
    {
        schemas.insert(schema.name.clone(), schema);
    }
    schemas
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
    let error = serde_json::to_value(ToolOutput::Error {
        message: String::new(),
    })
    .expect("ToolOutput is Serialize");
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
        Availability::WebSearch => "requires-search-key",
        Availability::Media => "requires-media-key",
        Availability::Video => "requires-video-capable-media-key",
        Availability::Issue => "requires-issue-backend",
        Availability::Session => "cli-session-layer",
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
         #                                          crates/stella-tools/src/catalog.rs\n\
         #   output_schema                          stella_protocol::ToolOutput\n\
         #   risk_level                             nothing declares it (see below)",
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
         risk_level = \"undeclared\"\n\
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
         `read_only`, `available_for_speculation`, category, and a commented example \
         input and output payload.\n\n\
         Two fields are stated absences rather than values, because inventing them \
         would manufacture a source of truth nobody reviewed:\n\n\
         - **`risk_level` is `\"undeclared\"`.** Nothing in the repository carries a \
           per-tool risk level. Tracked in #3060.\n\
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
    let scratch = tempfile::tempdir().expect("a scratch dir for the schema-collecting registries");
    let mut schemas = native_schemas(scratch.path());
    schemas.extend(session_schemas());

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
/// This is the whole point of the directory. Sixty-four registry rows, eight
/// CLI-layer declarations and seventy-eight catalog rows have to agree, and the
/// only way to keep them agreeing through a merge queue is to derive one from
/// the others and fail when the derivation drifts.
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
        // The two declared gaps are gaps on purpose. If either ever becomes
        // real data, this assertion is the reminder to change the prose that
        // explains why it is not.
        assert_eq!(table["risk_level"].as_str(), Some("undeclared"));
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
