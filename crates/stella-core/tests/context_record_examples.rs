//! The schema-reference examples parse, round-trip, and carry no drift.
//!
//! ADR 0012's first consequence: *"The examples in `docs/context-record-examples/`
//! are the schema reference."* A reference example the loader silently
//! disagrees with is worse than none — serde ignores unknown fields by
//! default, so a field the schema dropped (`truth.owner`, `probe.verdict`)
//! reads as documentation while steering nothing, and nothing said so until
//! this test. Three properties, per `context-record/v0.1` example:
//!
//! 1. **It parses** as a [`ContextFile`] under the shipped schema.
//! 2. **It round-trips**: serialize the parsed file back to TOML, re-parse,
//!    and get the same value — the schema can express what it accepted.
//! 3. **It carries nothing the schema drops**: the original file re-read as a
//!    raw TOML value equals the re-serialized one. This is the drift catcher —
//!    an unknown field survives parsing (serde ignores it) but cannot survive
//!    this comparison, and a TOML datetime aimed at a string field fails at
//!    step 1.
//!
//! Records additionally load through [`load_context_file`] without findings —
//! the reference files carry their real content-derived `record_id`/
//! `record_hash` (or none, for the deliberately hand-authored 03), so a hash
//! mismatch here means someone edited a record's semantics without re-stamping.
//!
//! Two examples are deliberately NOT this schema: `08-witness.toml`
//! (`context-witness/v0.1`) and `09-effect-witness.toml`
//! (`context-effect/v0.1`) are sketch-stage designs with no shipped Rust type.
//! For those the pinned property is the refusal: the record loader must reject
//! them loudly rather than half-load a witness file that strays into a rules
//! directory.

use std::path::PathBuf;

use stella_core::ingest::record::ContextFile;
use stella_core::records::{RecordFinding, load_context_file};

/// The shipped record schema tag ([`stella_core::ingest::record::SCHEMA_TAG`],
/// spelled out so a tag change shows up here as a decision, not an accident).
const RECORD_SCHEMA: &str = "context-record/v0.1";

fn examples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/spec/adaptive-context/context-record-examples")
}

/// Every example `.toml`, as `(name, contents, schema tag)`. Each must at
/// least be valid TOML with a top-level `schema` string — whatever its schema.
fn example_files() -> Vec<(String, String, String)> {
    let mut files: Vec<(String, String, String)> = std::fs::read_dir(examples_dir())
        .expect("the examples directory exists — ADR 0012 names it the schema reference")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension()? == "toml").then(|| {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                let contents = std::fs::read_to_string(&path).expect("readable example");
                let schema = toml::from_str::<toml::Value>(&contents)
                    .unwrap_or_else(|e| panic!("{name} is not valid TOML: {e}"))
                    .get("schema")
                    .and_then(|v| v.as_str())
                    .unwrap_or_else(|| panic!("{name} carries no top-level schema tag"))
                    .to_string();
                (name, contents, schema)
            })
        })
        .collect();
    files.sort();
    assert!(
        files.len() >= 8,
        "the schema reference has at least eight TOML examples, found {}",
        files.len()
    );
    files
}

/// The examples written against the shipped record schema.
fn record_files() -> Vec<(String, String)> {
    let files: Vec<(String, String)> = example_files()
        .into_iter()
        .filter(|(_, _, schema)| schema == RECORD_SCHEMA)
        .map(|(name, contents, _)| (name, contents))
        .collect();
    assert!(
        files.len() >= 6,
        "at least the six numbered record examples carry the record schema, found {}",
        files.len()
    );
    files
}

#[test]
fn every_record_example_parses_and_round_trips_under_the_shipped_schema() {
    for (name, contents) in record_files() {
        let parsed: ContextFile = toml::from_str(&contents)
            .unwrap_or_else(|e| panic!("{name} does not parse under the shipped schema: {e}"));

        let reserialized = toml::to_string(&parsed)
            .unwrap_or_else(|e| panic!("{name} does not serialize back to TOML: {e}"));
        let reparsed: ContextFile = toml::from_str(&reserialized)
            .unwrap_or_else(|e| panic!("{name} does not re-parse after round-trip: {e}"));
        assert_eq!(
            parsed, reparsed,
            "{name} loses information across a serialize/parse round-trip"
        );
    }
}

#[test]
fn no_record_example_carries_a_field_the_schema_silently_drops() {
    for (name, contents) in record_files() {
        let parsed: ContextFile = toml::from_str(&contents).expect("parses (previous test)");
        let reserialized = toml::to_string(&parsed).expect("serializes (previous test)");

        let original: toml::Value = toml::from_str(&contents).expect("raw TOML value");
        let survived: toml::Value = toml::from_str(&reserialized).expect("raw TOML value");
        assert_eq!(
            original, survived,
            "{name} carries content the shipped schema drops or respells — serde \
             ignores unknown fields, so without this check the example would keep \
             documenting a field that steers nothing"
        );
    }
}

#[test]
fn every_record_example_loads_with_its_real_identity_and_no_findings() {
    for (name, contents) in record_files() {
        let records = load_context_file(&name, &contents)
            .unwrap_or_else(|e| panic!("{name} fails the record loader: {}", e.detail));
        for record in &records {
            for finding in &record.findings {
                assert!(
                    matches!(finding, RecordFinding::IdentityStamped),
                    "{name}: {} carries a finding the schema reference must not \
                     ship: {finding}",
                    record.record.lineage_id
                );
            }
        }
    }
}

#[test]
fn the_sketch_stage_witness_schemas_are_refused_by_the_record_loader() {
    let sketches: Vec<(String, String, String)> = example_files()
        .into_iter()
        .filter(|(_, _, schema)| schema != RECORD_SCHEMA)
        .collect();
    assert!(
        !sketches.is_empty(),
        "the witness/effect sketches exist beside the record examples"
    );
    for (name, contents, _schema) in sketches {
        // Refusal can happen either way — as an unknown-schema error or as a
        // parse error on witness-only tables — but a sketch file must never
        // come back as loadable records.
        let err = load_context_file(&name, &contents)
            .expect_err(&format!("{name} must not half-load as records"));
        assert!(!err.detail.is_empty(), "{name}: the refusal says why");
    }
}
