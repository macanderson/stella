//! The effective system prompt, sectioned by provenance.
//!
//! One model call's `system_prefix` bytes are already reconstructed by
//! [`crate::sent_context`]; this module answers the question a reader brings
//! to them — *which setting put each part here?* — by splitting the exact
//! bytes on the assembler's own section markers and labelling each span with
//! the configuration surface that produces it.
//!
//! # An acknowledged copy, pinned from both sides
//!
//! This crate links no workspace crate but `stella-home` (see `crate::db`'s
//! module docs), so the markers below are a copy of the strings
//! `crates/stella-cli/src/agent/prompt.rs`'s `assemble_system_prompt` (and
//! `agent/engine.rs`'s SessionStart hook append) actually emit — the same
//! bargain `sent_context` takes for the receipt fold. Two tests pin the
//! copy: `sections_split_a_real_shaped_prompt` here pins the exact bytes,
//! and `the_observatory_provenance_markers_are_what_this_assembler_emits` in
//! `stella-cli`'s `agent/prompt/tests.rs` asserts a really-assembled prompt
//! contains them, in this order, so a reworded heading fails on whichever
//! side moved.
//!
//! # A view over exact bytes, not a parse
//!
//! Splitting is by earliest next marker, so a memory file that happens to
//! contain a marker's bytes can shift a boundary. What can never be wrong is
//! the whole: concatenating the section bodies in order reproduces the input
//! byte-for-byte (`sections_concatenate_to_the_input` is the property), so a
//! reader who distrusts a boundary still has the full prompt in front of
//! them.

use serde_json::{Value, json};

/// One provenance marker: the byte prefix that opens a section, the label the
/// dashboard shows, and the configuration surface that produces the section.
struct Marker {
    prefix: &'static str,
    label: &'static str,
    source: &'static str,
}

/// The section openers `assemble_system_prompt` can emit, in the order it
/// appends them. Everything before the first hit is the base instruction set.
const MARKERS: &[Marker] = &[
    Marker {
        prefix: "\n\n## Session environment\n",
        label: "Session environment",
        source: "computed from the live process and workspace at session open — not configurable",
    },
    Marker {
        prefix: "\n\nWorkspace memories (lessons from previous sessions — apply them):\n",
        label: "Workspace memories",
        source: ".stella/memories/*.md (loaded when authority permits project prompts)",
    },
    Marker {
        prefix: "\n\nWorkspace memories were omitted from this prompt: ",
        label: "Workspace memories (omitted)",
        source: ".stella/memories/*.md — withheld because the suppression state was unreadable",
    },
    Marker {
        prefix: "\n\n## Workspace rules (cite the ^handle of any you apply)",
        label: "Workspace rules",
        source: ".stella/rules/*.toml context records (edit via `stella context`)",
    },
    Marker {
        prefix: "\n\nSession context (from SessionStart hooks):\n",
        label: "SessionStart hook context",
        source: "hooks.SessionStart (settings.json / stella.toml [hooks])",
    },
];

/// What the base span is, and where it comes from — the prompt-shaping
/// settings that can replace or extend it, named so the reader knows which
/// knob to reach for.
const BASE_SOURCE: &str = "the built-in persona (default, pipeline, or minimal via --minimal / \
                           [agents] minimal_prompt), replaced — or in minimal mode extended — by \
                           agents.default.prompt (model settings)";

/// Split one call's exact `system_prefix` bytes into provenance-labelled
/// sections. `full` lifts the same per-body clip the journal route applies,
/// through the same helper, so "clipped" means one thing on this page.
///
/// The sections concatenate back to `text` byte-for-byte; each carries the
/// `label`/`source` of the marker that opened it (the leading span is the
/// base instruction set), and a marker's own bytes belong to the section it
/// opens.
pub(crate) fn sectioned(text: &str, full: bool) -> Value {
    let mut sections = Vec::new();
    // The open section's provenance, where its body starts, and where the
    // search for the next marker resumes (past the open marker's own bytes,
    // so a marker can never re-find itself).
    let mut open: (&str, &str) = ("Base instructions", BASE_SOURCE);
    let mut start = 0usize;
    let mut search_from = 0usize;
    loop {
        let next = MARKERS
            .iter()
            .filter_map(|marker| {
                text[search_from..]
                    .find(marker.prefix)
                    .map(|at| (search_from + at, marker))
            })
            .min_by_key(|(at, _)| *at);
        match next {
            Some((at, marker)) => {
                if at > start {
                    sections.push(section(open, &text[start..at], full));
                }
                open = (marker.label, marker.source);
                start = at;
                search_from = at + marker.prefix.len();
            }
            None => {
                if text.len() > start || sections.is_empty() {
                    sections.push(section(open, &text[start..], full));
                }
                return json!({ "found": true, "sections": Value::Array(sections) });
            }
        }
    }
}

/// One rendered section: its provenance labels plus the clipped body.
fn section((label, source): (&str, &str), body: &str, full: bool) -> Value {
    let mut out = json!({ "label": label, "source": source });
    crate::journal::set_journal_body(&mut out, body, full);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A prompt shaped the way `assemble_system_prompt` + the SessionStart
    /// hook append shape one: base, environment, memories, rules, hook
    /// context. The marker bytes here are the acknowledged copy's own test
    /// fixture — `stella-cli`'s prompt tests pin the same bytes from the
    /// producing side.
    fn shaped_prompt() -> String {
        concat!(
            "You are Stella, a terminal coding agent.",
            "\n\n## Session environment\nWorkspace root: /w — a git repository",
            "\n\nWorkspace memories (lessons from previous sessions — apply them):\n\n### one\nlesson\n",
            "\n\n## Workspace rules (cite the ^handle of any you apply)\n\n### Must\n- rule ^r1",
            "\n\nSession context (from SessionStart hooks):\nhook output",
        )
        .to_string()
    }

    fn labels(out: &Value) -> Vec<String> {
        out["sections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["label"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn sections_split_a_real_shaped_prompt() {
        let out = sectioned(&shaped_prompt(), true);
        assert_eq!(
            labels(&out),
            [
                "Base instructions",
                "Session environment",
                "Workspace memories",
                "Workspace rules",
                "SessionStart hook context",
            ]
        );
        let sections = out["sections"].as_array().unwrap();
        assert_eq!(
            sections[0]["body"], "You are Stella, a terminal coding agent.",
            "the base span is everything before the first marker"
        );
        assert!(
            sections[1]["body"]
                .as_str()
                .unwrap()
                .contains("Workspace root: /w"),
        );
        // Every section names the setting surface that produced it — the
        // whole point of this view over the raw bytes.
        for section in sections {
            assert!(
                !section["source"].as_str().unwrap_or_default().is_empty(),
                "a section with no source teaches nothing: {section}"
            );
        }
    }

    /// The property the module docs promise: whatever the boundary heuristics
    /// do, the whole is exact.
    #[test]
    fn sections_concatenate_to_the_input() {
        for text in [
            shaped_prompt(),
            "a bare custom prompt with no markers at all".to_string(),
            // A memory whose body embeds a marker's bytes: the boundary is
            // allowed to shift, the concatenation is not.
            format!(
                "base\n\n## Session environment\nx{}",
                "\n\nWorkspace memories (lessons from previous sessions — apply them):\n\
                 \n### sneaky\nquotes the heading \"\n\n## Workspace rules (cite the ^handle of any you apply)\""
            ),
            String::new(),
        ] {
            let out = sectioned(&text, true);
            let rebuilt: String = out["sections"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s["body"].as_str().unwrap())
                .collect();
            assert_eq!(rebuilt, text, "sections must concatenate to the input");
        }
    }

    /// A markerless prompt — a custom base with nothing appended — is one
    /// base section, never an error and never zero sections.
    #[test]
    fn a_markerless_prompt_is_one_base_section() {
        let out = sectioned("just a custom persona", true);
        assert_eq!(labels(&out), ["Base instructions"]);
    }

    /// The clip is the journal route's clip, lifted by the same `full` flag.
    #[test]
    fn the_clip_is_shared_with_the_journal_route() {
        let long = format!("base{}", "x".repeat(20_000));
        let clipped = sectioned(&long, false);
        assert_eq!(clipped["sections"][0]["truncated"], true);
        let full = sectioned(&long, true);
        assert_eq!(full["sections"][0]["truncated"], false);
        assert_eq!(full["sections"][0]["body"], Value::String(long));
    }
}
