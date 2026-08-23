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
use stella_tools::registry::Tool;

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
/// `TempDir` — at the same rung as `delegate`, which spends money. The grades are
/// reviewed judgements against a stated rubric, which is exactly why they are
/// declared.
const RISK_NOTE: &str = "\
# risk_level: how bad one honest call is — a reviewed judgement, declared in
# crates/stella-tools/src/catalog.rs beside the flags above and graded against
# the rubric on `ToolEntry::risk` (#2716, #3060). A DIFFERENT axis from
# `read_only`: `delegate` mutates no file and spends real money, while
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
    /// Whether the census found this name in the schema list the measured runs
    /// advertised, as opposed to only in their tool traffic.
    ///
    /// Deserialized without a `serde(default)` on purpose. Every claim below
    /// that a tool *was advertised* used to be inferred from the row merely
    /// existing, while the census's own direct answer sat one field away
    /// unread (#4420) — the same defect class as #3846, where a page said no
    /// measurement existed and the fixture it was generated from held one. A
    /// default would restore exactly that: a fixture missing the field would
    /// silently read `false` and the pages would go on stating advertisement
    /// as fact. A fixture that cannot answer must fail to parse instead.
    in_schema: bool,
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
///
/// With one pin, [`pin_capability_conditional_schemas`], because one tool
/// chooses its description from a capability the *generator's own host* has or
/// has not got.
fn native_schemas(scratch: &Path) -> BTreeMap<String, ToolSchema> {
    let registry = ToolRegistry::new(scratch.to_path_buf());
    let mut schemas: BTreeMap<String, ToolSchema> = registry
        .schemas()
        .into_iter()
        .map(|schema| (schema.name.clone(), schema))
        .collect();
    pin_capability_conditional_schemas(&mut schemas);
    schemas
}

// ── capability-conditional descriptions ─────────────────────────────────────
//
// `search` picks its advertised description at construction from whether an
// embedder resolved (#3139), which is the right thing for the model and the
// wrong thing for a generated page: derived from the live registry, this
// reference would say one thing on a machine with `OPENAI_API_KEY` exported
// and another on CI, and `make gate` would go red on a developer's shell
// rather than on their diff.
//
// So the pages document the BASELINE — the surface a host with no optional
// capability configured advertises — and name the other variant in a comment
// generated from the same source, so neither is hidden. Scrubbing the
// environment instead was rejected: `cargo test --workspace` runs this same
// drift test with nobody's wrapper script in front of it, so a fix that lives
// in `scripts/check-tool-docs.sh` protects only one of the two ways it runs.

/// Replace any schema whose description is chosen from an ambient capability
/// with its baseline, so `generate` is a function of the source tree.
fn pin_capability_conditional_schemas(schemas: &mut BTreeMap<String, ToolSchema>) {
    // `SearchConfig::default()`, not `from_env`: depth and budget reach no part
    // of the schema, but a generator that reads the environment at all is one
    // whose output a reader has to reason about.
    let baseline = stella_tools::search::Search::with_semantic_rung(
        stella_tools::search::SearchConfig::default(),
        stella_tools::search::SemanticRung::Unavailable,
    )
    .schema();
    // Asserted, not assumed: if `search` ever stops being registered under
    // that name the pin would silently become a no-op and the page would go
    // back to reporting whatever the host resolved.
    assert!(
        schemas.contains_key(&baseline.name),
        "`{}` is not in the registry's schemas — the description pin has nothing to pin",
        baseline.name
    );
    schemas.insert(baseline.name.clone(), baseline);
}

/// The comment a capability-conditional page carries above its `description`,
/// naming the variant the baseline is not.
fn description_variant_note(name: &str) -> Option<String> {
    (name == "search").then(|| {
        wrap(
            &format!(
                "The description below is the BASELINE: what a host with no embedder \
                 configured advertises. `search` chooses it at construction from whether \
                 `stella_embed::from_env()` resolved (#3139) — a session that has one \
                 advertises meaning-matching instead, because that is what its ladder \
                 runs. Both strings are declared in crates/stella-tools/src/search.rs; \
                 this page pins the baseline so it stays a function of the source tree \
                 rather than of the environment the generator ran in.\n\
                 \n\
                 With an embedder resolved, the description reads:\n\
                 \n\
                 {alternate}",
                alternate = stella_tools::search::SEMANTIC_DESCRIPTION,
            ),
            76,
        )
    })
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
        data: None,
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

/// Which census rows describe one tool, once the rename ledger is consulted.
///
/// The fixture is keyed by the dispatch name a measured run advertised, so a
/// tool renamed since the capture has its measurement filed under a key the
/// catalog no longer holds. [`catalog::FORMER_TOOL_NAMES`] is the reviewed
/// join between the two, and this type keeps the provenance attached to the
/// numbers all the way to the page: a reader is always told which name a row
/// was recorded under.
enum UsageEvidence<'a> {
    /// A row filed under the tool's own dispatch name.
    Current(&'a Usage),
    /// One row per former name that carries one, in ledger order.
    ///
    /// **The rows are reported side by side and never combined into one.**
    /// `trials_used` counts trials, and two former names' trial sets overlap
    /// by an amount the census does not record — `grep` (23/408) and `glob`
    /// (62/408) both fold into `search`, whose true trial count is somewhere
    /// between 62 and 85 and is not recoverable from these rows.
    /// `avg_first_step` and `fail_rate` are means over per-call data the
    /// fixture distilled away, so they cannot be re-weighted either. A total
    /// would therefore be a figure this generator invented, which is the one
    /// thing a measurement page may not print.
    Former(Vec<(&'static str, &'a Usage)>),
    /// Neither the dispatch name nor any name in the ledger carries a row.
    Unmeasured,
}

impl UsageEvidence<'_> {
    /// Calls recorded across every row this evidence holds, or `None` when
    /// there is no row to count. `Some(0)` is the measured claim "advertised
    /// and never chosen"; `None` is "never measured", which is a different
    /// fact and reads differently on the page.
    fn calls(&self) -> Option<u64> {
        match self {
            UsageEvidence::Current(usage) => Some(usage.calls),
            UsageEvidence::Former(rows) => Some(rows.iter().map(|(_, u)| u.calls).sum()),
            UsageEvidence::Unmeasured => None,
        }
    }

    /// Whether the census recorded this tool in the schema list the measured
    /// runs advertised, or `None` when it holds no row to answer from.
    ///
    /// Read off [`Usage::in_schema`] rather than inferred from the row
    /// existing (#4420). The two answers differ: a row with `in_schema:
    /// false` is a name the census saw *on the wire* without finding it among
    /// the schemas those runs offered, which is a fact about the run, not
    /// about the catalog. `None` is the third state and is genuinely an
    /// inference from absence — no row means the census has nothing to say
    /// either way, and the prose that consumes it says so.
    ///
    /// A former-name set answers `true` if any of its rows does: the tool was
    /// offered under one of the names it has answered to, which is what the
    /// sentence on the page claims.
    fn advertised(&self) -> Option<bool> {
        match self {
            UsageEvidence::Current(usage) => Some(usage.in_schema),
            UsageEvidence::Former(rows) => Some(rows.iter().any(|(_, u)| u.in_schema)),
            UsageEvidence::Unmeasured => None,
        }
    }
}

/// The census rows for `name`, falling back to the names it dispatched under
/// before.
///
/// Deliberately does not resolve *examples* through the ledger, only usage
/// counts: a payload recorded under a former name was shaped by that tool's
/// input schema, and a rename can move the schema with the name — `search`
/// accepts arguments `grep` never had. A call count survives that move with
/// its provenance stated; a payload rendered under this page's schema block
/// would be a worked example of a schema the run never saw.
fn usage_evidence<'a>(name: &str, fixture: &'a Fixture) -> UsageEvidence<'a> {
    if let Some(usage) = fixture.usage.get(name) {
        return UsageEvidence::Current(usage);
    }
    let former: Vec<(&'static str, &Usage)> = catalog::FORMER_TOOL_NAMES
        .iter()
        .filter(|(current, _)| *current == name)
        .filter_map(|(_, former)| fixture.usage.get(*former).map(|usage| (*former, usage)))
        .collect();
    if former.is_empty() {
        UsageEvidence::Unmeasured
    } else {
        UsageEvidence::Former(former)
    }
}

/// Whether the rename ledger records an earlier name for `name`.
///
/// The absence prose is scoped by this rather than asserting over the ledger
/// unconditionally: telling a reader that no *former* name of `ask_question`
/// was advertised either is true, and implies the tool had one.
fn has_former_names(name: &str) -> bool {
    catalog::FORMER_TOOL_NAMES
        .iter()
        .any(|(current, _)| *current == name)
}

/// One row of census numbers, verbatim from the fixture.
fn usage_row(usage: &Usage, trials: u64) -> String {
    let first = usage
        .avg_first_step
        .map(|step| format!("{step:.1}"))
        .unwrap_or_else(|| "n/a".into());
    format!(
        "calls {calls} · trials that used it {trials_used}/{trials} · \
         calls per model call {per_turn:.4} · mean position of first use {first} · \
         failures {failures} ({fail_rate:.1}%).",
        calls = usage.calls,
        trials_used = usage.trials_used,
        per_turn = usage.calls_per_turn,
        failures = usage.failures,
        fail_rate = usage.fail_rate * 100.0,
    )
}

/// The usage paragraph for one tool, or the honest absence.
///
/// Measurements are included, and they are the reason several of these pages
/// say anything at all: thirty-three of the seventy-two tools declared at
/// capture time were never called once across 408 trials, and that is the
/// most useful sentence on those pages. (Both figures are properties of the
/// 2026-08-11 capture, not of today's catalog — the surface has been cut
/// since, and what the current pages count is derived in [`render_index`].)
/// They are comments rather than fields, and every one of them is
/// date-stamped, because a measurement ages and a schema does not — a reader
/// who finds `calls = 0` as a datum a year from now would reasonably read it
/// as a property of the tool instead of a property of one week in August.
fn usage_comment(name: &str, fixture: &Fixture) -> String {
    let p = &fixture.provenance;
    let usage = match usage_evidence(name, fixture) {
        UsageEvidence::Unmeasured => {
            let (also, subject) = if has_former_names(name) {
                (
                    ", and neither does any name it dispatched under before (the rename \
                     ledger in crates/stella-tools/src/catalog.rs)",
                    "no name this tool has answered to was",
                )
            } else {
                ("", "this tool was not")
            };
            return format!(
                "No usage measurement. `{name}` carries no row in the {captured} census, \
                 which scanned {trials} trials{also} — the census enumerates the schemas \
                 those runs advertised, and {subject} among them.",
                captured = p.captured,
                trials = p.census_trials_scanned,
            );
        }
        UsageEvidence::Former(rows) => {
            let provenance = match rows.as_slice() {
                [(former, _)] => format!(
                    ", under this tool's then-name `{former}` — the census records the \
                     name that was on the wire, and the tool has been renamed since"
                ),
                _ => {
                    let names = rows
                        .iter()
                        .map(|(former, _)| format!("`{former}`"))
                        .collect::<Vec<_>>()
                        .join(" and ");
                    format!(
                        ", under the names {names} that this tool replaced. One row \
                         each, and no combined row: `trials_used` counts trials, those \
                         sets overlap by an amount the census does not record, and the \
                         two rates are means over per-call data the capture distilled \
                         away"
                    )
                }
            };
            let body = rows
                .iter()
                .map(|(former, usage)| {
                    format!(
                        "as `{former}`: {row}",
                        row = usage_row(usage, p.census_trials_scanned)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            return format!(
                "Measured {captured} across {trials} trials / {model_calls} model calls\
                 {provenance}:\n\
                 {body}\n\
                 A measurement, not a contract: it ages, the schema above does not.",
                captured = p.captured,
                trials = p.census_trials_scanned,
                model_calls = p.census_model_calls,
            );
        }
        UsageEvidence::Current(usage) => usage,
    };
    if usage.calls == 0 {
        // The advertisement half is the census's `in_schema`, not the row's
        // existence (#4420). A row that answers `false` is one the census saw
        // in tool traffic without finding among the advertised schemas, so
        // "advertised and chosen zero times" would be the page asserting the
        // opposite of its own evidence.
        let offering = if usage.in_schema {
            format!("`{name}` was advertised and chosen zero times")
        } else {
            format!(
                "`{name}` was chosen zero times, and the census did not find this name \
                 among the schemas those runs advertised either — so it may never have \
                 been on the menu to choose from"
            )
        };
        return format!(
            "Never called. Across {trials} trials and {calls} model calls \
             (census of {captured}), {offering}. \
             That is a fact about the tool menu, not a defect in the tool — see #3032, \
             which this page is input to.",
            trials = p.census_trials_scanned,
            calls = p.census_model_calls,
            captured = p.captured,
        );
    }
    // Calls without advertisement is a real observation and reads as a
    // contradiction unless it is stated: the name was on the wire in runs
    // whose advertised schema list the census could not find it in.
    let unadvertised = if usage.in_schema {
        String::new()
    } else {
        format!(
            "\nThe census did not find `{name}` among the schemas those runs advertised, \
             only in their tool traffic — so these calls were made under a name the run \
             itself did not declare."
        )
    };
    format!(
        "Measured {captured} across {trials} trials / {model_calls} model calls:\n\
         {row}{unadvertised}\n\
         A measurement, not a contract: it ages, the schema above does not.",
        captured = p.captured,
        trials = p.census_trials_scanned,
        model_calls = p.census_model_calls,
        row = usage_row(usage, p.census_trials_scanned),
    )
}

/// The observed example, or a stated absence with the reason for it.
fn example_comment(name: &str, fixture: &Fixture) -> String {
    let p = &fixture.provenance;
    let Some(example) = fixture.examples.get(name) else {
        let why = match usage_evidence(name, fixture) {
            // Absent from the census entirely: these are the tools that no
            // measured run advertised — a later arrival, or one whose
            // registration needs a backend those runs had none of — so it
            // never had the chance to be called. That is a different fact
            // from "offered and refused", and it is a claim about every name
            // the tool has answered to, not only today's.
            UsageEvidence::Unmeasured => {
                let names = if has_former_names(name) {
                    "neither this name nor any name it dispatched under before was"
                } else {
                    "it was not"
                };
                format!(
                    "no call to record: {names} advertised in the runs the capture comes \
                     from, so it never had the chance to be called"
                )
            }
            // Advertisement is asserted from the census's own `in_schema`
            // note, never from the row existing (#4420).
            UsageEvidence::Current(usage) if usage.calls == 0 && usage.in_schema => {
                "no call to record: it was advertised and never called".to_string()
            }
            UsageEvidence::Current(usage) if usage.calls == 0 => {
                "no call to record: it was never called, and the census did not find \
                 this name among the schemas those runs advertised either"
                    .to_string()
            }
            UsageEvidence::Current(usage) => format!(
                "{calls} calls in the wider census, but none in the \
                 {rows}-row capture the examples are drawn from",
                calls = usage.calls,
                rows = p.corpus_rows,
            ),
            // The census filed these calls under a name this page does not
            // otherwise mention, and it is reported per name for the same
            // reason the usage block is: a call count is additive, but
            // handing the reader one number invites them to read it as a
            // measurement of a tool that did not exist yet.
            UsageEvidence::Former(rows) => {
                let tally = rows
                    .iter()
                    .map(|(former, usage)| {
                        format!("{calls} calls as `{former}`", calls = usage.calls)
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "no call in the {rows_scanned}-row capture the examples are drawn from; \
                     the wider census records {tally}, under the {names} this tool replaced",
                    rows_scanned = p.corpus_rows,
                    names = if rows.len() == 1 { "name" } else { "names" },
                )
            }
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
         {description_note}description = {description}\n\
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
        description_note = match description_variant_note(entry.name) {
            Some(note) => format!("{}\n", comment_block(&note)),
            None => String::new(),
        },
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
    // Through the rename ledger, so the count keeps meaning "advertised and
    // chosen zero times". A tool measured only under a former name is counted
    // on that measurement — and a tool the census never advertised under any
    // of its names is not counted at all, because "no row" is not a zero
    // (#3846).
    //
    // Both halves of that sentence are now asserted rather than inferred: the
    // zero from `calls()`, the advertisement from the census's own `in_schema`
    // note (#4420). Before, a row whose `in_schema` said `false` counted
    // toward a figure the prose below calls "advertised" — the row's mere
    // existence standing in for evidence that was sitting unread.
    let never_called = entries
        .iter()
        .filter(|entry| {
            let evidence = usage_evidence(entry.name, fixture);
            evidence.calls() == Some(0) && evidence.advertised() == Some(true)
        })
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
         {never_called} tools were advertised and never called once across {trials} \
         trials and {model_calls} model calls — the most interesting fact on those \
         pages, and input to #3032. Advertisement is the census's own `in_schema` \
         column, not the presence of a row (#4420).\n\n",
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

/// A renamed tool's page reports the census row recorded under its then-name.
///
/// The failure this pins is a false statement of fact, not a missing feature:
/// before the rename ledger, `delegate` took the no-row branch and its page
/// read *"the census enumerates the schemas those runs advertised, and this
/// tool was not among them"* — while `usage["task"]` in the very fixture that
/// sentence is generated from records two calls (#3846). The page now names
/// the measurement and the name it was filed under, and the assertions below
/// are on the generated bytes rather than on `usage_comment`, because the
/// committed page is what a reader is misled by.
#[test]
fn a_renamed_tools_page_reports_the_measurement_under_its_former_name() {
    /// The `── usage ──` block, uncommented and unwrapped onto one line, so
    /// an assertion tests the sentence rather than the greedy wrap.
    fn usage_block(page: &str) -> String {
        let (_, tail) = page
            .split_once("── usage")
            .expect("every page carries a usage block");
        tail.lines()
            .map(|line| line.trim_start_matches('#').trim())
            .collect::<Vec<_>>()
            .join(" ")
    }

    let files = generate();
    let fixture = load_fixture();

    // `task` → `delegate` (#3192): one former name, one row.
    let delegate = usage_block(&files["delegate.toml"]);
    let task = &fixture.usage["task"];
    assert!(
        delegate.contains("under this tool's then-name `task`"),
        "delegate.toml does not name the measurement's provenance:\n{delegate}"
    );
    assert!(
        delegate.contains(&format!("as `task`: calls {}", task.calls)),
        "delegate.toml does not carry the `task` row's call count:\n{delegate}"
    );
    assert!(
        !delegate.contains("No usage measurement"),
        "delegate.toml still claims no measurement exists, and the fixture \
         disagrees:\n{delegate}"
    );

    // `grep` + `glob` → `search` (#3120): two rows, reported separately
    // because `trials_used` counts trials and the two sets overlap by an
    // amount the census does not record.
    let search = usage_block(&files["search.toml"]);
    for former in ["grep", "glob"] {
        let row = &fixture.usage[former];
        assert!(
            search.contains(&format!("as `{former}`: calls {}", row.calls)),
            "search.toml does not carry the `{former}` row:\n{search}"
        );
    }
    let summed = fixture.usage["grep"].calls + fixture.usage["glob"].calls;
    assert!(
        !search.contains(&format!("calls {summed}")),
        "search.toml prints a total across two former names; the rows are \
         reported side by side because no honest total exists:\n{search}"
    );

    // The fallback survives: a tool the census never advertised under any of
    // its names still says so. `ask_question` landed after the capture and
    // holds no ledger row (#4212).
    let unmeasured = usage_block(&files["ask_question.toml"]);
    assert!(
        unmeasured.contains("No usage measurement"),
        "ask_question.toml carries no row under any name and must say so:\n{unmeasured}"
    );
}

/// Every advertisement claim on a page comes from the census's `in_schema`
/// column, not from the row existing.
///
/// The failure this pins is the one #3846 pinned one field over: a sentence of
/// fact the fixture it was generated from disagrees with. The generator wrote
/// `in_schema` per tool and `Usage` did not deserialize it, so *every* page
/// saying a tool "was advertised" was inferring that from row presence while
/// the direct evidence sat unread (#4420). No live catalog row carries
/// `in_schema: false` today — the two that do, `create_witness_test` and `sh`,
/// are not tools this catalog declares — so the flip is applied to a live
/// row's copy here rather than waiting for one to arrive.
#[test]
fn an_advertisement_claim_reads_the_censuss_in_schema_column() {
    /// A live tool with calls, and one with none, in the committed fixture —
    /// the two shapes whose prose asserts advertisement.
    fn pick(fixture: &Fixture, called: bool) -> String {
        catalog::CATALOG
            .iter()
            .map(|entry| entry.name)
            .find(|name| {
                fixture
                    .usage
                    .get(*name)
                    .is_some_and(|usage| (usage.calls > 0) == called && usage.in_schema)
            })
            .unwrap_or_else(|| {
                panic!("the fixture holds no advertised live row with calls={called}")
            })
            .to_string()
    }

    let mut fixture = load_fixture();
    let with_calls = pick(&fixture, true);
    let never_called = pick(&fixture, false);

    // As committed: both are advertised, and neither page hedges.
    let advertised = usage_comment(&with_calls, &fixture);
    assert!(
        !advertised.contains("did not find"),
        "an advertised row must not carry the unadvertised clause:\n{advertised}"
    );
    let zero = usage_comment(&never_called, &fixture);
    assert!(
        zero.contains(&format!(
            "`{never_called}` was advertised and chosen zero times"
        )),
        "an advertised zero-call row states it plainly:\n{zero}"
    );
    assert!(
        example_comment(&never_called, &fixture).contains("it was advertised and never called"),
        "the example block states the same fact from the same column"
    );

    // Flip the column and nothing else. Every clause that asserted
    // advertisement has to change; on the code before #4420 none of them did,
    // because none of them could see this field.
    for name in [&with_calls, &never_called] {
        fixture
            .usage
            .get_mut(name.as_str())
            .expect("the row just read still exists")
            .in_schema = false;
    }

    let unadvertised = usage_comment(&with_calls, &fixture);
    assert!(
        unadvertised.contains(&format!(
            "The census did not find `{with_calls}` among the schemas those runs advertised"
        )),
        "a row with calls but no schema entry must say so rather than reporting the \
         calls as if the tool had been on the menu:\n{unadvertised}"
    );

    let zero = usage_comment(&never_called, &fixture);
    assert!(
        !zero.contains("was advertised and chosen zero times"),
        "a zero-call row the census never found in a schema list may not claim it was \
         advertised:\n{zero}"
    );
    assert!(
        zero.contains("may never have been on the menu to choose from"),
        "the honest clause replaces it:\n{zero}"
    );
    let example = example_comment(&never_called, &fixture);
    assert!(
        !example.contains("it was advertised and never called"),
        "the example block asserts from the same column:\n{example}"
    );

    // And the index's headline count, which the prose calls "advertised and
    // never called once", drops both flipped rows.
    let entries: Vec<&ToolEntry> = catalog::CATALOG.iter().collect();
    let committed = render_index(&entries, &load_fixture());
    let flipped = render_index(&entries, &fixture);
    assert_ne!(
        committed, flipped,
        "flipping `in_schema` on a live row must change what the index counts"
    );
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
