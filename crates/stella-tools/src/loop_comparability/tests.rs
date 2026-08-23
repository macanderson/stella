// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The two halves of the guard: totality against the catalog, and a sentinel
//! that drives every non-exempt tool twice through the real registry.
//!
//! The sentinel is the half that can be a false green, so it is built to be
//! falsifiable rather than merely passing:
//!
//! - a `Deterministic` row with no [`Probe`] fails, so a tool cannot be
//!   declared stable and then quietly left undriven;
//! - the two postures are checked by *different* comparisons —
//!   `Deterministic` on the raw bytes, `VolatileWithNormalizer` on
//!   raw-differ-then-normalized-match — so neither declaration can stand in
//!   for the other. Comparing normalized output on both arms was the first
//!   version, and it let `read_file` pass as `Deterministic`;
//! - the raw-*difference* requirement on the volatile arm is what proves its
//!   fixture reaches the volatile bytes at all;
//! - `the_sentinel_can_fail` runs both comparisons against an output that
//!   carries a counter and requires both to reject it.

use std::path::Path;

use serde_json::{Value, json};
use stella_core::driver::loop_evidence::comparable_output;
use stella_protocol::tool::ToolOutput;

use super::{LoopComparability, REGISTRY, for_tool};
use crate::registry::ToolRegistry;

/// One call a probe makes before the measured one: the tool's name and a
/// builder for its arguments.
///
/// A builder rather than a `Value`, because a `Value` is not constructible in
/// a `const`, and the probe table wants to stay a table.
type PreludeCall = (&'static str, fn() -> Value);

/// What one tool is driven with, and the state it needs in front of it.
///
/// The files and the prelude are re-applied before **every** measured call,
/// which is what makes the two calls comparable: the second one must see
/// exactly what the first did, or a difference in the output says nothing
/// about the tool.
struct Probe {
    /// Workspace files, rewritten from scratch each round after the previous
    /// round's tree is removed.
    files: &'static [(&'static str, &'static str)],
    /// Calls made through the same registry before the measured one — for a
    /// tool whose subject is session state no file can hold (`get_state`
    /// needs something saved, `delete_state` needs something to delete).
    prelude: &'static [PreludeCall],
    /// The measured call's arguments.
    input: fn() -> Value,
}

/// A small source tree every file-shaped probe shares, so the fixtures differ
/// only where the tool under test differs.
const TREE: &[(&str, &str)] = &[
    (
        "src/lifecycle.rs",
        "pub fn retry_budget() -> usize {\n    3\n}\n",
    ),
    ("src/wire.rs", "pub fn send_header() {}\n"),
    ("notes.md", "one\ntwo\nthree\n"),
];

/// The probe for `name`, or `None` when its row is an exemption.
///
/// Exhaustive over the non-exempt rows by assertion, not by construction:
/// `every_non_exempt_tool_is_driven_by_the_sentinel` fails on a missing arm,
/// which is what stops a new `Deterministic` row from being declared and
/// never checked.
fn probe(name: &str) -> Option<Probe> {
    let probe = match name {
        // Read the same file twice: the volatile half is the session tally in
        // the footer, which is exactly what the normalizer strips.
        "read_file" => Probe {
            files: TREE,
            prelude: &[],
            input: || json!({"path": "notes.md"}),
        },
        // A path the fixture does not contain, so the no-clobber guard is not
        // the thing under test and both rounds write a brand-new file.
        "write_file" => Probe {
            files: TREE,
            prelude: &[],
            input: || json!({"path": "src/fresh.rs", "content": "pub fn fresh() {}\n"}),
        },
        "edit_file" => Probe {
            files: TREE,
            prelude: &[],
            input: || json!({"path": "notes.md", "old_string": "two", "new_string": "TWO"}),
        },
        "delete_file" => Probe {
            files: TREE,
            prelude: &[],
            input: || json!({"path": "src/wire.rs"}),
        },
        // No embedder is configured under test, so this exercises the name
        // and scan rungs — the ones invariant 7 requires to rank identically
        // on two runs of one query.
        "search" => Probe {
            files: TREE,
            prelude: &[],
            input: || json!({"query": "retry budget"}),
        },
        "save_state" => Probe {
            files: &[],
            prelude: &[],
            input: || json!({"key": "plan", "content": "step one\nstep two\n"}),
        },
        "get_state" => Probe {
            files: &[],
            prelude: &[(
                "save_state",
                || json!({"key": "plan", "content": "step one\nstep two\n"}),
            )],
            input: || json!({"key": "plan"}),
        },
        "list_state" => Probe {
            files: &[],
            prelude: &[(
                "save_state",
                || json!({"key": "plan", "content": "step one\nstep two\n"}),
            )],
            input: || json!({}),
        },
        "delete_state" => Probe {
            files: &[],
            prelude: &[(
                "save_state",
                || json!({"key": "plan", "content": "step one\nstep two\n"}),
            )],
            input: || json!({"key": "plan"}),
        },
        "get_environment" => Probe {
            files: &[],
            prelude: &[],
            input: || json!({}),
        },
        _ => return None,
    };
    Some(probe)
}

/// Rebuild `root` to hold exactly `files`.
///
/// Removes the previous round's tree rather than writing over it, so an
/// artefact a tool left behind — a `.stella/private/codegraph.db`, a file the
/// call itself created — cannot make the second round's workspace differ from
/// the first's.
fn seed(root: &Path, files: &[(&str, &str)]) {
    for entry in std::fs::read_dir(root)
        .expect("read the fixture root")
        .flatten()
    {
        let path = entry.path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path).expect("clear a fixture directory");
        } else {
            std::fs::remove_file(&path).expect("clear a fixture file");
        }
    }
    for (relative, body) in files {
        let file = root.join(relative);
        std::fs::create_dir_all(file.parent().expect("a parent")).expect("mkdir");
        std::fs::write(&file, body).expect("write a fixture file");
    }
}

/// Drive `name` twice through one registry — one session, as the loop
/// detector sees it — restoring the workspace and the prelude before each
/// call.
async fn drive_twice(name: &str, probe: &Probe) -> (ToolOutput, ToolOutput) {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = workspace.path().canonicalize().expect("canonicalize");
    let registry = ToolRegistry::new(root.clone());

    let mut outputs = Vec::with_capacity(2);
    for _ in 0..2 {
        seed(&root, probe.files);
        for (tool, arguments) in probe.prelude {
            let out = registry.execute(tool, &arguments()).await;
            assert!(
                !out.is_error(),
                "the `{name}` probe's prelude failed: {out:?}"
            );
        }
        outputs.push(registry.execute(name, &(probe.input)()).await);
    }
    let second = outputs.pop().expect("two calls");
    let first = outputs.pop().expect("two calls");
    (first, second)
}

/// Totality, catalog → registry. A tool added without a row here is a tool
/// whose author never answered "can the loop detector compare this?".
#[test]
fn every_catalog_tool_declares_its_loop_comparability() {
    let missing: Vec<&str> = crate::catalog::ALL_NAMES
        .iter()
        .copied()
        .filter(|name| for_tool(name).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "these built-ins declare no loop comparability: {missing:?} — add a row to \
         `loop_comparability::REGISTRY` in the change that adds the tool"
    );
}

/// Totality, registry → catalog, plus no duplicates. A row for a name nothing
/// dispatches is a row the sentinel silently stops exercising.
#[test]
fn every_declared_row_names_a_live_tool_exactly_once() {
    for (name, _) in REGISTRY {
        assert!(
            crate::catalog::ALL_NAMES.contains(name),
            "`{name}` has a loop-comparability row but is not a built-in — delete the row, or \
             restore the tool"
        );
    }
    let mut names: Vec<&str> = REGISTRY.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    let count = names.len();
    names.dedup();
    assert_eq!(count, names.len(), "a tool is declared twice");
    assert_eq!(
        count,
        crate::catalog::ALL_NAMES.len(),
        "the registry and the catalog must be the same set"
    );
}

/// A posture must carry the prose its variant promises. An empty rationale is
/// an exemption nobody argued for, which is the shape this guard exists to
/// stop.
#[test]
fn every_posture_carries_the_prose_its_variant_promises() {
    for (name, posture) in REGISTRY {
        match posture {
            LoopComparability::Deterministic => {}
            LoopComparability::VolatileWithNormalizer {
                normalizer,
                pinned_by,
            } => {
                assert!(!normalizer.is_empty(), "`{name}` names no normalizer");
                assert!(!pinned_by.is_empty(), "`{name}` names no pinning test");
            }
            LoopComparability::ExemptWorldState { rationale } => assert!(
                rationale.len() > 40,
                "`{name}`'s exemption must say which state it means, in a sentence"
            ),
        }
    }
}

/// Every non-exempt row is actually driven. Without this the sentinel below
/// would pass by covering nothing: a new `Deterministic` tool with no probe
/// arm would simply not be tested.
#[test]
fn every_non_exempt_tool_is_driven_by_the_sentinel() {
    for (name, posture) in REGISTRY {
        let exempt = matches!(posture, LoopComparability::ExemptWorldState { .. });
        assert_eq!(
            probe(name).is_some(),
            !exempt,
            "`{name}` is declared {posture:?} but the sentinel {} drive it — a tool is either \
             exempt with a rationale or checked with a probe",
            if exempt { "does" } else { "does not" }
        );
    }
}

/// **The sentinel.** A `Deterministic` tool called twice against the same
/// state must produce byte-identical **raw** output; a
/// `VolatileWithNormalizer` tool must produce output that differs raw and
/// matches once normalized.
///
/// The two arms are chosen so the postures are not interchangeable, which is
/// the first thing this harness was caught getting wrong. Comparing
/// `comparable_output` on the `Deterministic` arm let `read_file` — whose
/// footer is volatile and whose normalizer works — pass as `Deterministic`,
/// so the distinction the registry exists to record was unenforced in exactly
/// the direction that matters. Raw on one arm, raw-then-normalized on the
/// other, and each row's declaration is now falsifiable by the other's.
///
/// The raw-*difference* requirement on the volatile arm is the same argument
/// pointed the other way: without it a normalizer row passes on a fixture
/// that never reaches the volatile bytes, and the test proves only that two
/// identical strings are identical.
#[tokio::test]
async fn declared_comparability_holds_when_the_tool_is_actually_run() {
    for (name, posture) in REGISTRY {
        let Some(probe) = probe(name) else { continue };
        let (first, second) = drive_twice(name, &probe).await;
        assert!(
            !first.is_error() && !second.is_error(),
            "the `{name}` probe must exercise the tool's success path: {first:?} / {second:?}"
        );

        match posture {
            LoopComparability::Deterministic => assert_eq!(
                first, second,
                "`{name}` is declared Deterministic but two identical calls against identical \
                 state produced different bytes — the loop detector is blind for this tool. \
                 Route the volatile part to the diagnostic plane, or declare a normalizer."
            ),
            LoopComparability::VolatileWithNormalizer { normalizer, .. } => {
                assert_ne!(
                    first, second,
                    "`{name}` is declared volatile but its probe produced identical raw \
                     output — the fixture does not reach the volatile bytes, so the \
                     normalized comparison below would prove nothing"
                );
                assert_eq!(
                    comparable_output(&first).into_owned(),
                    comparable_output(&second).into_owned(),
                    "`{name}` declares {normalizer} strips its volatile bytes, and it did not"
                );
            }
            LoopComparability::ExemptWorldState { .. } => {
                unreachable!("an exempt row has no probe, which the test above pins")
            }
        }
    }
}

/// **The negative control.** A harness that cannot fail is worse than none,
/// because it converts an unasked question into a green check.
///
/// Two outputs differing only in a running counter — the shape a tool
/// acquires the moment someone adds an elapsed time or a call tally to a
/// verdict — must be rejected by both comparisons the sentinel uses, and two
/// identical outputs must be accepted by both.
///
/// The counter is deliberately *not* inside a `read_file` footer: the
/// normalizer strips that one span and nothing else, so a volatile byte
/// anywhere in an ordinary verdict survives normalization and is caught on
/// either arm. That is the property that makes the volatile posture a
/// narrow, earned exception rather than a way out.
#[test]
fn the_sentinel_can_fail() {
    let stable = |body: &str| ToolOutput::Ok {
        content: body.to_string(),
        data: None,
    };
    let counted = |n: usize| stable(&format!("wrote 12 bytes to a.rs (call {n})"));

    assert_ne!(
        counted(1),
        counted(2),
        "the Deterministic arm must reject an output carrying a counter"
    );
    assert_ne!(
        comparable_output(&counted(1)).into_owned(),
        comparable_output(&counted(2)).into_owned(),
        "and so must the normalized arm, or a volatile row would pass by declaring a \
         normalizer that does not reach its bytes"
    );
    assert_eq!(
        stable("wrote 12 bytes to a.rs"),
        stable("wrote 12 bytes to a.rs"),
        "and both must accept a stable one, or the sentinel above can never pass"
    );
}
