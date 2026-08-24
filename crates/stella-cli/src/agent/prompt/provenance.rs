//! Which setting put each span in the system prompt.
//!
//! A reconstructed call carries the exact `system_prefix` bytes it was sent
//! ([`stella_store::Reconstruction`]), and a reader who opens them wants one
//! thing the bytes do not say: *which knob produced this paragraph?* This
//! module answers it by splitting those bytes on the assembler's own section
//! openers and labelling each span with the configuration surface behind it.
//!
//! # The markers are the emitting constants, not copies
//!
//! Every prefix in `MARKERS` is the same `&'static str` the emitting site
//! pushes — [`super::SESSION_ENVIRONMENT_HEADER`], [`super::MEMORIES_HEADER`],
//! [`super::MEMORIES_OMITTED_PREFIX`],
//! [`crate::agent::engine::SESSION_HOOK_CONTEXT_HEADER`] — so a reworded
//! heading moves both sides at once and cannot silently relabel a span. The
//! rules heading is the one that cannot be shared outright:
//! [`stella_core::records::CACHED_HEADING`] opens with a single newline and
//! [`super::assemble_system_prompt`] pushes the second, so the marker is that
//! constant with a newline in front of it. `RULES_HEADER` spells the joined
//! bytes out, and `the_rules_marker_is_the_heading_the_record_channel_renders`
//! below pins it against the real constant.
//!
//! `the_provenance_markers_are_what_this_assembler_emits` in `super::tests`
//! holds the other direction: a really-assembled prompt carries these bytes,
//! so a reworded heading fails even if this table were changed to match it.
//! That test is also what a second splitter would be pinned against — the
//! Observatory renders the same view on its own page and links no crate that
//! defines these constants, so its table can only ever be an acknowledged
//! copy (#4602).
//!
//! # A view over exact bytes, not a parse
//!
//! Splitting is by earliest next marker, so a memory file that quotes a
//! heading can shift a boundary. What can never be wrong is the whole:
//! concatenating the section bodies in order reproduces the input
//! byte-for-byte (`sections_concatenate_to_the_input` below is the property),
//! so a reader who distrusts a boundary still has every byte the call was sent
//! in front of them, in order.

use crate::agent::engine::SESSION_HOOK_CONTEXT_HEADER;

use super::{MEMORIES_HEADER, MEMORIES_OMITTED_PREFIX, SESSION_ENVIRONMENT_HEADER};

/// The workspace-rules heading as it lands in the prompt: the record
/// channel's own [`stella_core::records::CACHED_HEADING`] with the newline
/// [`super::assemble_system_prompt`] pushes before it. Written out because
/// `const` concatenation of a non-literal is not a thing Rust does; pinned by
/// a test rather than by a comment asking the next reader to check.
const RULES_HEADER: &str = "\n\n## Workspace rules (cite the ^handle of any you apply)";

/// One provenance marker: the bytes that open a section, the label a reader
/// sees, and the setting surface that produces it.
struct Marker {
    prefix: &'static str,
    label: &'static str,
    source: &'static str,
}

/// The section openers an assembled prompt can carry, in the order
/// [`super::assemble_system_prompt`] appends them. Everything before the first
/// hit is the base instruction set.
const MARKERS: &[Marker] = &[
    Marker {
        prefix: SESSION_ENVIRONMENT_HEADER,
        label: "Session environment",
        source: "computed from the live process and workspace at session open — not configurable",
    },
    Marker {
        prefix: MEMORIES_HEADER,
        label: "Workspace memories",
        source: ".stella/memories/*.md (loaded when authority permits project prompts)",
    },
    Marker {
        prefix: MEMORIES_OMITTED_PREFIX,
        label: "Workspace memories (omitted)",
        source: ".stella/memories/*.md — withheld because the suppression state was unreadable",
    },
    Marker {
        prefix: RULES_HEADER,
        label: "Workspace rules",
        source: ".stella/rules/*.toml context records (edit via `stella context`)",
    },
    Marker {
        prefix: SESSION_HOOK_CONTEXT_HEADER,
        label: "SessionStart hook context",
        source: "hooks.SessionStart (settings.json / stella.toml [hooks])",
    },
];

/// The label on the span before the first marker.
const BASE_LABEL: &str = "Base instructions";

/// Where that span comes from — named so a reader who wants it different
/// knows which field to set.
const BASE_SOURCE: &str = "the built-in persona (default, pipeline, or minimal via --minimal / \
                           [agents] minimal_prompt), replaced — or in minimal mode extended — by \
                           agents.default.prompt (model settings)";

/// One labelled span of a system prompt: where it came from, and its exact
/// bytes.
pub(crate) struct Section<'a> {
    /// What this span is, in a reader's words.
    pub(crate) label: &'static str,
    /// The configuration surface that produced it.
    pub(crate) source: &'static str,
    /// The span itself, including the marker bytes that opened it.
    pub(crate) body: &'a str,
}

/// Split one call's exact `system_prefix` bytes into provenance-labelled
/// sections.
///
/// A marker's own bytes belong to the section it opens, and the leading span
/// is the base instruction set. The sections concatenate back to `prompt`
/// byte-for-byte — including for the empty prompt, which is one empty base
/// section rather than none, so a caller never has to distinguish "no
/// sections" from "nothing to show".
pub(crate) fn sections(prompt: &str) -> Vec<Section<'_>> {
    let mut sections = Vec::new();
    // The open section's provenance, where its body starts, and where the
    // search for the next marker resumes — past the open marker's own bytes,
    // so a marker can never re-find itself.
    let mut open = (BASE_LABEL, BASE_SOURCE);
    let mut start = 0usize;
    let mut search_from = 0usize;
    loop {
        let next = MARKERS
            .iter()
            .filter_map(|marker| {
                prompt[search_from..]
                    .find(marker.prefix)
                    .map(|at| (search_from + at, marker))
            })
            .min_by_key(|(at, _)| *at);
        match next {
            Some((at, marker)) => {
                if at > start {
                    sections.push(section(open, &prompt[start..at]));
                }
                open = (marker.label, marker.source);
                start = at;
                search_from = at + marker.prefix.len();
            }
            None => {
                if prompt.len() > start || sections.is_empty() {
                    sections.push(section(open, &prompt[start..]));
                }
                return sections;
            }
        }
    }
}

/// One section from its provenance pair and its bytes.
///
/// `'static` and the body's borrow are two different lifetimes in parameter
/// position, so there is no single one for an elided `'_` to mean.
fn section<'a>((label, source): (&'static str, &'static str), body: &'a str) -> Section<'a> {
    Section {
        label,
        source,
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A prompt shaped the way `assemble_system_prompt` plus the SessionStart
    /// hook append shape one: base, environment, memories, rules, hook
    /// context. Built from the marker constants themselves, so it cannot
    /// describe a prompt the assembler would not emit.
    fn shaped_prompt() -> String {
        format!(
            "You are Stella, a terminal coding agent.\
             {SESSION_ENVIRONMENT_HEADER}Workspace root: /w — a git repository\
             {MEMORIES_HEADER}\n### one\nlesson\n\
             {RULES_HEADER}\n\n### Must\n- rule ^r1\
             {SESSION_HOOK_CONTEXT_HEADER}hook output"
        )
    }

    fn labels(sections: &[Section<'_>]) -> Vec<&'static str> {
        sections.iter().map(|s| s.label).collect()
    }

    #[test]
    fn sections_split_a_real_shaped_prompt() {
        let prompt = shaped_prompt();
        let sections = sections(&prompt);
        assert_eq!(
            labels(&sections),
            [
                BASE_LABEL,
                "Session environment",
                "Workspace memories",
                "Workspace rules",
                "SessionStart hook context",
            ]
        );
        assert_eq!(
            sections[0].body, "You are Stella, a terminal coding agent.",
            "the base span is everything before the first marker"
        );
        assert!(sections[1].body.contains("Workspace root: /w"));
        assert!(sections[4].body.ends_with("hook output"));
        // A section that cannot name the setting behind it teaches nothing,
        // which is the whole reason this view exists over the raw bytes.
        for section in &sections {
            assert!(
                !section.source.is_empty(),
                "{} names no setting surface",
                section.label
            );
        }
    }

    /// The property the module docs promise: whatever a boundary heuristic
    /// does, the whole is exact.
    #[test]
    fn sections_concatenate_to_the_input() {
        for prompt in [
            shaped_prompt(),
            "a bare custom prompt with no markers at all".to_string(),
            // A memory whose body quotes a later heading: the boundary is
            // allowed to shift, the concatenation is not.
            format!(
                "base{SESSION_ENVIRONMENT_HEADER}x{MEMORIES_HEADER}\n### sneaky\nquotes {RULES_HEADER} in prose"
            ),
            // The fail-closed branch, which replaces the memories section
            // rather than joining it.
            format!("base{SESSION_ENVIRONMENT_HEADER}x{MEMORIES_OMITTED_PREFIX}permission denied."),
            String::new(),
        ] {
            let rebuilt: String = sections(&prompt).iter().map(|s| s.body).collect();
            assert_eq!(rebuilt, prompt, "sections must concatenate to the input");
        }
    }

    /// A markerless prompt — a custom base with nothing appended — is one base
    /// section, never zero and never an error.
    #[test]
    fn a_markerless_prompt_is_one_base_section() {
        assert_eq!(labels(&sections("just a custom persona")), [BASE_LABEL]);
        assert_eq!(labels(&sections("")), [BASE_LABEL]);
    }

    /// `RULES_HEADER` is the only marker that spells its bytes out instead
    /// of naming the constant that emits them, because the assembler pushes a
    /// newline before the record channel's heading. This is what keeps the
    /// spelled-out copy equal to the real thing.
    #[test]
    fn the_rules_marker_is_the_heading_the_record_channel_renders() {
        assert_eq!(
            RULES_HEADER,
            format!("\n{}", stella_core::records::CACHED_HEADING),
            "the assembler pushes '\\n' then the rendered channel, which opens with CACHED_HEADING"
        );
    }
}
