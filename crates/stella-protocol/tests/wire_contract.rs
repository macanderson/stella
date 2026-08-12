//! The committed wire contract, checked against the types it describes.
//!
//! `docs/wire/agentevent.schema.json` is generated from [`AgentEvent`] and
//! committed so that a change to the wire format lands as a reviewable diff
//! (issue #971). `scripts/check-wire-schema.sh` proves the committed file
//! still matches the types. This file proves the other direction, which is the
//! one a consumer actually depends on: **every event this build can emit
//! validates against the file consumers were handed.**
//!
//! Those are different claims. A schema can match its types perfectly and
//! still be useless — if the derive mis-describes an `Option`, or a
//! `serde(default)` field is written as required, both halves stay
//! self-consistent while every real stream fails the published contract.
//!
//! # Coverage, stated honestly
//!
//! Deliberately no totals. Every count here is derived from a list the tests
//! already walk, so a number in this comment is a second copy that only ever
//! goes stale — and one had: "nineteen enums" was written when the arm-count
//! table held nineteen rows and was never touched as the table grew past it.
//! What the coverage IS, checked rather than claimed:
//!
//! - Every `AgentEvent` variant: exhaustive, and *proved* exhaustive — the
//!   sample table is checked against [`KNOWN_TYPE_TAGS`], which the
//!   `agent_event_tags!` macro generates from a match the compiler requires to
//!   be total. A new variant that reaches the wire without a sample here fails
//!   [`every_known_tag_has_a_sample`].
//! - Every arm of every enum in the payload graph — `ProofStep`,
//!   `SubAgentPhase`, `MediaJobState`, `ToolOutput`, `DeliveryOutcome` and the
//!   unit-only vocabularies. Two independent checks, because they catch different
//!   mistakes: [`every_nested_vocabulary_is_fully_sampled`] compares each
//!   sample list's length against the arm count in the schema's own `$defs`
//!   (so growing a vocabulary without extending the samples fails), and
//!   [`every_enum_arm_in_the_payload_graph_actually_reaches_the_wire`] proves
//!   each arm was actually *serialized* into a validated sample. Listing an
//!   arm and wiring it into an event are not the same thing — the second check
//!   found `ProofTree::Baseline` sampled in a list but never emitted.
//! - Both the "all optional fields present" and "all optional fields absent"
//!   shapes of the events that carry them.
//!
//! What is **not** covered: [`AgentEvent::Unknown`], deliberately and
//! necessarily. It carries no wire tag of its own, so it is absent from the
//! schema, and it re-serializes as the foreign object it wrapped — whose
//! `"type"` is by construction one this build does not know.
//! [`an_event_from_the_future_is_outside_the_schema_by_construction`] pins
//! that as the expected result rather than leaving it as a silent gap.
//!
//! The validator below implements the subset of JSON Schema 2020-12 that
//! `schemars` emits for these types. It is deliberately strict: an unmodelled
//! keyword panics rather than being skipped, so the proof can never quietly
//! become weaker than it looks.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde_json::{Value, json};
use stella_protocol::{AgentEvent, KNOWN_TYPE_TAGS};

// ---------------------------------------------------------------------------
// The committed artifact
// ---------------------------------------------------------------------------

/// The schema as *committed*, not as regenerated. Reading the file is the
/// point: this test speaks for the artifact consumers were given.
fn committed_schema() -> Value {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "..",
        "docs",
        "wire",
        "agentevent.schema.json",
    ]
    .iter()
    .collect();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "reading the committed wire schema {path:?}: {e}\n\
             Generate it with: bash scripts/export-agentevent-schema.sh"
        )
    });
    serde_json::from_str(&text).expect("the committed wire schema is valid JSON")
}

/// The sample fixtures — one value per arm of every enum in the payload
/// graph, and the `AgentEvent` table the proofs below validate. Its own
/// module because it grows with every new variant while the validator does
/// not (`scripts/check-file-size.sh`).
///
/// `#[path]` because this file is a test-binary *crate root*, so a bare
/// `mod samples;` would resolve to a sibling `tests/samples.rs` — which Cargo
/// would then compile as a second test target of its own.
#[path = "wire_contract/samples.rs"]
mod samples;

use samples::*;

// ---------------------------------------------------------------------------
// A minimal JSON Schema 2020-12 validator
// ---------------------------------------------------------------------------

/// Keywords that carry documentation or numeric annotation only. Ignored on
/// purpose; every *other* unrecognized keyword panics, so the validator cannot
/// silently stop checking something.
const IGNORED: &[&str] = &[
    "$schema",
    "$defs",
    "$comment",
    "title",
    "description",
    "default",
    "examples",
    "deprecated",
    "format",
    "minimum",
    "maximum",
];

/// Validate `instance` against `schema`, resolving `$ref`s against `root`.
/// Returns one message per failure; empty means valid.
fn errors(root: &Value, schema: &Value, instance: &Value, path: &str) -> Vec<String> {
    match schema {
        Value::Bool(true) => return Vec::new(),
        Value::Bool(false) => return vec![format!("{path}: schema is `false`, nothing validates")],
        Value::Object(_) => {}
        other => panic!("{path}: subschema is neither object nor boolean: {other}"),
    }
    let map = schema.as_object().expect("checked above");
    let mut out = Vec::new();

    for key in map.keys() {
        assert!(
            IGNORED.contains(&key.as_str())
                || matches!(
                    key.as_str(),
                    "$ref"
                        | "oneOf"
                        | "anyOf"
                        | "allOf"
                        | "const"
                        | "enum"
                        | "type"
                        | "properties"
                        | "required"
                        | "additionalProperties"
                        | "items"
                ),
            "{path}: unmodelled JSON Schema keyword `{key}` — extend this validator \
             rather than letting the proof silently weaken"
        );
    }

    if let Some(Value::String(reference)) = map.get("$ref") {
        let name = reference
            .strip_prefix("#/$defs/")
            .unwrap_or_else(|| panic!("{path}: only local `#/$defs/…` refs are modelled"));
        let target = root
            .pointer(&format!("/$defs/{name}"))
            .unwrap_or_else(|| panic!("{path}: dangling $ref {reference}"));
        out.extend(errors(root, target, instance, path));
    }

    if let Some(Value::Array(branches)) = map.get("oneOf") {
        let matched = branches
            .iter()
            .filter(|b| errors(root, b, instance, path).is_empty())
            .count();
        if matched != 1 {
            out.push(format!(
                "{path}: matched {matched} of {} oneOf branches (want exactly 1)",
                branches.len()
            ));
        }
    }
    if let Some(Value::Array(branches)) = map.get("anyOf") {
        let matched = branches
            .iter()
            .filter(|b| errors(root, b, instance, path).is_empty())
            .count();
        if matched == 0 {
            out.push(format!(
                "{path}: matched none of {} anyOf branches",
                branches.len()
            ));
        }
    }
    if let Some(Value::Array(branches)) = map.get("allOf") {
        for (i, branch) in branches.iter().enumerate() {
            out.extend(errors(root, branch, instance, &format!("{path}/allOf/{i}")));
        }
    }

    if let Some(constant) = map.get("const")
        && instance != constant
    {
        out.push(format!("{path}: expected const {constant}, got {instance}"));
    }
    if let Some(Value::Array(values)) = map.get("enum")
        && !values.contains(instance)
    {
        out.push(format!("{path}: {instance} is not one of {values:?}"));
    }

    if let Some(ty) = map.get("type") {
        let names: Vec<&str> = match ty {
            Value::String(one) => vec![one.as_str()],
            Value::Array(many) => many.iter().filter_map(Value::as_str).collect(),
            other => panic!("{path}: `type` is neither string nor array: {other}"),
        };
        if !names.iter().any(|n| json_is(n, instance)) {
            out.push(format!("{path}: {instance} is not of type {names:?}"));
        }
    }

    if let Some(Value::Array(required)) = map.get("required")
        && let Some(object) = instance.as_object()
    {
        for name in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(name) {
                out.push(format!("{path}: missing required property `{name}`"));
            }
        }
    }

    if let Some(object) = instance.as_object() {
        let properties = map.get("properties").and_then(Value::as_object);
        if let Some(properties) = properties {
            for (name, value) in object {
                if let Some(subschema) = properties.get(name) {
                    out.extend(errors(root, subschema, value, &format!("{path}/{name}")));
                }
            }
        }
        if map.get("additionalProperties") == Some(&Value::Bool(false)) {
            let known: BTreeSet<&str> = properties
                .map(|p| p.keys().map(String::as_str).collect())
                .unwrap_or_default();
            for name in object.keys() {
                if !known.contains(name.as_str()) {
                    out.push(format!("{path}: unexpected property `{name}`"));
                }
            }
        }
    }

    if let Some(items) = map.get("items")
        && let Some(array) = instance.as_array()
    {
        for (i, element) in array.iter().enumerate() {
            out.extend(errors(root, items, element, &format!("{path}/{i}")));
        }
    }

    out
}

fn json_is(name: &str, value: &Value) -> bool {
    match name {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        other => panic!("unmodelled JSON type `{other}`"),
    }
}

/// Assert one event validates, with a message that names the offending field.
fn assert_validates(root: &Value, event: &AgentEvent) {
    let instance = serde_json::to_value(event).expect("AgentEvent always serializes");
    let failures = errors(root, root, &instance, event.type_tag());
    assert!(
        failures.is_empty(),
        "`{}` does not validate against the committed schema:\n  {}\n  instance: {instance}",
        event.type_tag(),
        failures.join("\n  ")
    );
}

// ---------------------------------------------------------------------------
// The proofs
// ---------------------------------------------------------------------------

#[test]
fn every_sample_event_validates_against_the_committed_schema() {
    let schema = committed_schema();
    let samples = sample_events();
    assert!(samples.len() > 100, "the sample set should be substantial");
    for event in &samples {
        assert_validates(&schema, event);
    }
}

/// How one arm of an enum shows up in serialized JSON.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Discriminant {
    /// A unit enum: the arm is a bare string value somewhere.
    Value(String),
    /// Internally tagged: `{"<tag>": "<arm>", …}`.
    Tagged(String, String),
    /// Externally tagged: `{"<arm>": {…}}`.
    Key(String),
}

/// Read the discriminants of one `$defs` enum straight out of the schema.
fn discriminants(def: &Value, name: &str) -> Vec<Discriminant> {
    def["oneOf"]
        .as_array()
        .unwrap_or_else(|| panic!("$defs/{name} is not a oneOf"))
        .iter()
        .map(|arm| {
            if let Some(Value::String(token)) = arm.get("const") {
                return Discriminant::Value(token.clone());
            }
            if let Some(properties) = arm.get("properties").and_then(Value::as_object) {
                for (property, subschema) in properties {
                    if let Some(Value::String(token)) = subschema.get("const") {
                        return Discriminant::Tagged(property.clone(), token.clone());
                    }
                }
            }
            if let Some(Value::Array(required)) = arm.get("required")
                && let [Value::String(only)] = required.as_slice()
            {
                return Discriminant::Key(only.clone());
            }
            panic!("$defs/{name}: cannot read a discriminant from {arm}")
        })
        .collect()
}

/// Every object key, string value, and (key, string value) pair anywhere in a
/// serialized sample — enough to answer "did this arm actually reach the wire".
#[derive(Default)]
struct Wire {
    keys: BTreeSet<String>,
    values: BTreeSet<String>,
    pairs: BTreeSet<(String, String)>,
}

impl Wire {
    fn absorb(&mut self, value: &Value) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    self.keys.insert(key.clone());
                    if let Value::String(text) = child {
                        self.pairs.insert((key.clone(), text.clone()));
                    }
                    self.absorb(child);
                }
            }
            Value::Array(items) => items.iter().for_each(|i| self.absorb(i)),
            Value::String(text) => {
                self.values.insert(text.clone());
            }
            _ => {}
        }
    }

    fn has(&self, discriminant: &Discriminant) -> bool {
        match discriminant {
            Discriminant::Value(token) => self.values.contains(token),
            Discriminant::Tagged(tag, token) => self.pairs.contains(&(tag.clone(), token.clone())),
            Discriminant::Key(key) => self.keys.contains(key),
        }
    }
}

#[test]
fn every_enum_arm_in_the_payload_graph_actually_reaches_the_wire() {
    // The length check below ratchets the sample *lists*; this checks the
    // thing that actually matters — that each arm was serialized into a
    // validated sample. The two are not the same: a vocabulary can be listed
    // in full and still only be half-wired into events (`BudgetDenied` zips
    // two of the three budget modes; the third rides `BudgetTick`).
    let schema = committed_schema();
    let mut wire = Wire::default();
    for event in sample_events() {
        wire.absorb(&serde_json::to_value(&event).expect("AgentEvent always serializes"));
    }

    let mut missing = Vec::new();
    for (name, def) in schema["$defs"].as_object().expect("$defs is an object") {
        if def.get("oneOf").is_none() {
            continue; // a struct, not an enum
        }
        for discriminant in discriminants(def, name) {
            if !wire.has(&discriminant) {
                missing.push(format!("{name}: {discriminant:?}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "these enum arms are never serialized by any sample, so nothing proves \
         they validate:\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn every_known_tag_has_a_sample() {
    // `KNOWN_TYPE_TAGS` is generated from a match the compiler requires to be
    // total over `AgentEvent`, so covering it IS covering every variant.
    let samples = sample_events();
    let covered: BTreeSet<&str> = samples.iter().map(AgentEvent::type_tag).collect();
    let expected: BTreeSet<&str> = KNOWN_TYPE_TAGS.iter().copied().collect();
    let missing: Vec<&&str> = expected.difference(&covered).collect();
    assert!(
        missing.is_empty(),
        "no sample event for {missing:?} — a new variant reached the wire without \
         being proved against docs/wire/agentevent.schema.json"
    );
    assert_eq!(
        covered, expected,
        "a sample carries a tag outside the vocabulary"
    );
}

#[test]
fn every_nested_vocabulary_is_fully_sampled() {
    // The schema's `$defs` are derived from the enums, so the arm count there
    // is the enum's arm count. Comparing each sample list against it makes
    // growing a vocabulary without extending the samples a test failure rather
    // than a silent coverage hole.
    let schema = committed_schema();
    let arms = |name: &str| -> usize {
        let def = schema
            .pointer(&format!("/$defs/{name}"))
            .unwrap_or_else(|| panic!("the schema has no $defs/{name}"));
        def["oneOf"]
            .as_array()
            .unwrap_or_else(|| panic!("$defs/{name} is not a oneOf"))
            .len()
    };

    let counts: BTreeMap<&str, usize> = BTreeMap::from([
        ("StageKind", all_stage_kinds().len()),
        ("BudgetMode", all_budget_modes().len()),
        ("BudgetScope", all_budget_scopes().len()),
        ("PolicyKind", all_policy_kinds().len()),
        ("ModelCallRole", all_model_call_roles().len()),
        (
            "UsageIncompleteReason",
            all_usage_incomplete_reasons().len(),
        ),
        ("FinishReason", all_finish_reasons().len()),
        ("FileChangeKind", all_file_change_kinds().len()),
        ("ProofTree", all_proof_trees().len()),
        ("LadderRung", all_ladder_rungs().len()),
        ("FlipOutcome", all_flip_outcomes().len()),
        ("MediaKind", all_media_kinds().len()),
        ("PrStatus", all_pr_statuses().len()),
        ("CiStatus", all_ci_statuses().len()),
        ("TaskStatus", all_task_statuses().len()),
        ("BlockKind", all_block_kinds().len()),
        ("CacheZone", all_cache_zones().len()),
        ("SubAgentStatus", all_subagent_statuses().len()),
        ("ProofStep", all_proof_steps().len()),
        ("MediaJobState", all_media_job_states().len()),
        ("ToolOutput", all_tool_outputs().len()),
        ("SubAgentPhase", all_subagent_phases().len()),
        ("DeliveryDecline", all_delivery_declines().len()),
        // Two arms, and both are sampled directly rather than through an
        // `all_*` helper: `Delivered` carries six fields with no vocabulary of
        // its own, so a helper enumerating it would be a list of one.
        ("DeliveryOutcome", 2),
    ]);
    for (name, sampled) in counts {
        assert_eq!(
            sampled,
            arms(name),
            "{name} has {} arms in the schema but {sampled} sample(s) here",
            arms(name)
        );
    }

    // And the list above must not itself go stale: every `$defs` entry that is
    // an enum (a `oneOf`) needs a row.
    let enum_defs: BTreeSet<&str> = schema["$defs"]
        .as_object()
        .expect("$defs is an object")
        .iter()
        .filter(|(_, def)| def.get("oneOf").is_some())
        .map(|(name, _)| name.as_str())
        .collect();
    let rows: BTreeSet<&str> = BTreeSet::from([
        "StageKind",
        "BudgetMode",
        "BudgetScope",
        "PolicyKind",
        "ModelCallRole",
        "UsageIncompleteReason",
        "FinishReason",
        "FileChangeKind",
        "ProofTree",
        "LadderRung",
        "FlipOutcome",
        "MediaKind",
        "PrStatus",
        "CiStatus",
        "TaskStatus",
        "BlockKind",
        "CacheZone",
        "SubAgentStatus",
        "ProofStep",
        "MediaJobState",
        "ToolOutput",
        "SubAgentPhase",
        "DeliveryDecline",
        "DeliveryOutcome",
    ]);
    assert_eq!(
        enum_defs, rows,
        "an enum in the payload graph has no sampled-arm-count row above"
    );
}

#[test]
fn a_wrong_field_name_fails_validation() {
    // The validator must actually reject things, or every assertion above is
    // vacuous. A `complete` event with its `model` field renamed is exactly
    // the drift the whole exercise exists to catch.
    let schema = committed_schema();
    let broken = json!({ "type": "complete", "modle": "opus", "cost_usd": 0.42 });
    let failures = errors(&schema, &schema, &broken, "complete");
    assert!(
        !failures.is_empty(),
        "a renamed required field must fail validation"
    );
}

#[test]
fn a_wrong_field_type_fails_validation() {
    let schema = committed_schema();
    let broken = json!({ "type": "complete", "model": "opus", "cost_usd": "free" });
    assert!(!errors(&schema, &schema, &broken, "complete").is_empty());
}

#[test]
fn an_event_from_the_future_is_outside_the_schema_by_construction() {
    // `AgentEvent::Unknown` re-serializes as the foreign object it wrapped, so
    // it matches no variant here. That is the correct and intended result: the
    // schema describes THIS build's vocabulary, and an unmatched `"type"` is
    // version skew, not corruption. Consumers must branch on that difference,
    // which is why it is pinned rather than left implicit.
    let schema = committed_schema();
    let future = AgentEvent::Unknown {
        event_type: "quantum_reticulation".into(),
        payload: json!({ "type": "quantum_reticulation", "splines": ["alpha", "beta"] }),
    };
    let instance = serde_json::to_value(&future).unwrap();
    assert!(
        !errors(&schema, &schema, &instance, "unknown").is_empty(),
        "an unrecognized tag must not validate — it is news, not a known event"
    );
    assert!(!KNOWN_TYPE_TAGS.contains(&future.type_tag()));
}

#[test]
fn the_committed_schema_declares_the_whole_known_vocabulary() {
    let schema = committed_schema();
    let tags: Vec<&str> = schema["oneOf"]
        .as_array()
        .expect("the root is a oneOf over tagged variants")
        .iter()
        .map(|v| {
            v.pointer("/properties/type/const")
                .and_then(Value::as_str)
                .expect("every variant carries a type const")
        })
        .collect();
    assert_eq!(tags, KNOWN_TYPE_TAGS, "in order, and complete");
}

/// The drift check the shell gate runs, available to `cargo test` when the
/// `schema` feature is on. The gate script is the required one — this exists
/// so `cargo test -p stella-protocol --features schema` catches it too.
#[cfg(feature = "schema")]
#[test]
fn the_committed_artifacts_match_the_types() {
    for (name, generated) in stella_protocol::schema_export::artifacts().unwrap() {
        let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "..", "..", "docs", "wire", name]
            .iter()
            .collect();
        let committed = std::fs::read_to_string(&path).expect("the artifact is committed");
        assert_eq!(
            committed, generated,
            "docs/wire/{name} is stale — run: bash scripts/export-agentevent-schema.sh"
        );
    }
}
