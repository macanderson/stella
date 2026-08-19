// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What KIND of thing a tool call was, and the colour that says so.
//!
//! # Why this module exists
//!
//! A transcript is a log of actions, and the first question a reader asks of
//! any row is not *which* tool ran but what kind of thing it did: was that a
//! look, a change, an opaque external call, a hand-off to another agent? Every
//! tool name used to render in one colour — the brand gold — so answering that
//! question meant reading each name and recalling what it does. Twenty rows in,
//! nobody does. The rest of the transcript was one warm neutral end to end,
//! which is the state the deck's own owner described as "impossible to discern
//! what is reasoning prose and what is a real tool call".
//!
//! So the tool NAME carries the class as a hue, and the neutral tiers are
//! spent on exactly three things: tool *results*, the agent's own thinking, and
//! the arguments beside a call. Everything a reader scans *for* is coloured;
//! everything they read only after finding it is not.
//!
//! # Where the classification comes from
//!
//! [`stella_tools::catalog`] — the same gate-enforced table the policy layer
//! addresses, so a tool cannot be filed under one family for `tools.<group>:
//! "off"` and a different one on screen. This module is a projection of that
//! table onto four visual classes, plus the two dynamic groups (`mcp`,
//! `custom`) the catalog already answers for names it has never heard of.
//!
//! The one axis the catalog's group column cannot supply is mutation: its
//! `scratch` group holds `get_state` and `save_state` alike, and telling a
//! read from a write is the single distinction the reader asked for first.
//! That comes from the catalog's own `read_only` column, which is the same
//! bit the engine's concurrency contract and the ladder's no-op rung already
//! trust.
//!
//! # Colour law
//!
//! Every class hue is categorical (`crate::palette`'s `data-*` series) and
//! therefore *not* the brand accent: gold means brand/active/focus and nothing
//! else, and a tool name is none of those. `crate::theme`'s
//! `every_tool_class_is_categorical_and_distinct` is the enforcement.

use ratatui::style::Color;

use crate::theme;

/// The visual classes a tool call falls into.
///
/// Four, deliberately — one per question a reader actually asks of a
/// transcript row on the built-in surface (a look, a change, an external
/// call, a hand-off). Every finer distinction the catalog draws is already
/// spelled out by the tool's own name beside the colour, and the categorical
/// series has hues to spare, so a class earns its slot only when a family of
/// tools can actually land in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolClass {
    /// Reads: scratch-state reads, the task listing, the environment report.
    /// Nothing in the workspace changed.
    Inspect,
    /// Writes to session state: scratch-state writes. The class a reviewer
    /// scans for.
    Mutate,
    /// Calls whose effects this table cannot vouch for: MCP tools, custom
    /// script tools, anything unrecognised. The class that promises the
    /// least.
    Execute,
    /// Orchestration: sub-agent delegation and the plan board.
    Delegate,
}

impl ToolClass {
    /// The hue this class paints its tool name in.
    ///
    /// Named roles, never literals: a recolour is a `theme` edit, and the
    /// palette law test walks these values.
    pub fn color(self) -> Color {
        match self {
            ToolClass::Inspect => theme::TEAL,
            ToolClass::Mutate => theme::MAGENTA,
            ToolClass::Execute => theme::VIOLET,
            ToolClass::Delegate => theme::ORCHID,
        }
    }

    /// A one-word label, for the help overlay's legend.
    pub fn label(self) -> &'static str {
        match self {
            ToolClass::Inspect => "read",
            ToolClass::Mutate => "write",
            ToolClass::Execute => "run",
            ToolClass::Delegate => "delegate",
        }
    }

    /// Every class, in legend order.
    pub const ALL: [ToolClass; 4] = [
        ToolClass::Inspect,
        ToolClass::Mutate,
        ToolClass::Execute,
        ToolClass::Delegate,
    ];
}

/// Classify one dispatch name.
///
/// Group first, then the read/write bit *within* the group that holds both.
/// The `task` group is orchestration whole — the reader's question there is
/// "what moved on the board", and `task_list` beside `task_create` is part of
/// that answer, not a file read. The `scratch` group genuinely mixes, so it
/// splits on mutation, because there the read/write distinction *is* the
/// question.
///
/// An unknown name — an MCP server's tool, a customer's own manifest tool —
/// falls back to the same read-only bit, so a third-party read never renders
/// as a write. That direction is deliberate: the fallback for "we do not
/// recognise this" is [`ToolClass::Execute`], the class that promises the
/// least about what happened.
pub fn classify(name: &str) -> ToolClass {
    let read_only = stella_tools::catalog::get(name).is_some_and(|entry| entry.read_only);
    match stella_tools::catalog::group_for(name) {
        // Split on mutation: this group holds both halves.
        "scratch" => {
            if read_only {
                ToolClass::Inspect
            } else {
                ToolClass::Mutate
            }
        }
        // The file family mixes exactly as `scratch` does, and for the same
        // reason the split matters more here: `read_file` beside `write_file`
        // in one colour would make a turn that only looked indistinguishable
        // from one that rewrote the tree.
        "file" => {
            if read_only {
                ToolClass::Inspect
            } else {
                ToolClass::Mutate
            }
        }
        // A shell command is the one built-in whose effect the renderer
        // genuinely cannot predict — `Execute` is the class that promises the
        // least, which is the honest promise for `bash -c`.
        "shell" => ToolClass::Execute,
        "search" => ToolClass::Inspect,
        "environment" => ToolClass::Inspect,
        "task" => ToolClass::Delegate,
        // `mcp`, `custom`, and any group added to the catalog without a class
        // here. A read stays a read; everything else promises the least.
        _ if read_only => ToolClass::Inspect,
        _ => ToolClass::Execute,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The distinction the reader asked for first, and the one the catalog's
    /// `group` column cannot draw on its own: within one group, a read and a
    /// write must not share a colour.
    #[test]
    fn a_read_and_a_write_of_the_same_family_are_different_classes() {
        for (read, write) in [("get_state", "save_state"), ("list_state", "delete_state")] {
            assert_eq!(classify(read), ToolClass::Inspect, "{read}");
            assert_eq!(classify(write), ToolClass::Mutate, "{write}");
            assert_ne!(
                classify(read).color(),
                classify(write).color(),
                "{read} and {write} must not share a hue"
            );
        }
    }

    /// Every registered tool lands where its name says it should. Pinned by
    /// example rather than by exhaustion so the test stays readable;
    /// `every_catalog_group_has_an_explicit_class` below covers the rest.
    #[test]
    fn each_family_lands_in_its_own_class() {
        for (name, class) in [
            ("get_state", ToolClass::Inspect),
            ("list_state", ToolClass::Inspect),
            ("get_environment", ToolClass::Inspect),
            ("save_state", ToolClass::Mutate),
            ("delete_state", ToolClass::Mutate),
            ("task", ToolClass::Delegate),
            ("task_create", ToolClass::Delegate),
            ("task_list", ToolClass::Delegate),
            ("task_complete", ToolClass::Delegate),
            ("task_assign", ToolClass::Delegate),
        ] {
            assert_eq!(classify(name), class, "{name}");
        }
    }

    /// Every name the catalog knows classifies — a group added there without
    /// a class here would silently fall into the unknown arm, and the whole
    /// point of reading the catalog is that the two cannot drift.
    #[test]
    fn every_catalog_group_has_an_explicit_class() {
        for group in stella_tools::catalog::groups() {
            if matches!(group, "mcp" | "custom") {
                continue;
            }
            let name = stella_tools::catalog::names_in_group(group)
                .first()
                .copied()
                .unwrap_or_else(|| panic!("group `{group}` is empty"));
            let explicit = matches!(
                group,
                "scratch" | "environment" | "task" | "file" | "shell" | "search"
            );
            assert!(
                explicit,
                "catalog group `{group}` (e.g. `{name}`) has no tool class — add an \
                 arm to `classify` in the same change that added the group"
            );
        }
    }

    /// A tool nobody here has heard of promises the least. The alternative —
    /// defaulting to `Inspect` — would paint an MCP server's mutating call in
    /// the read colour, which is the one direction that misleads.
    #[test]
    fn an_unknown_tool_promises_the_least() {
        assert_eq!(
            classify("mcp__github__create_pull_request"),
            ToolClass::Execute
        );
        assert_eq!(classify("some_customer_manifest_tool"), ToolClass::Execute);
    }

    /// The four classes are four colours. A class that shared a hue with
    /// another would be a class the reader cannot see.
    #[test]
    fn the_four_classes_are_four_distinct_colors() {
        let colors: Vec<Color> = ToolClass::ALL.iter().map(|c| c.color()).collect();
        for (i, color) in colors.iter().enumerate() {
            assert!(
                !colors[..i].contains(color),
                "{:?} duplicates an earlier class colour",
                ToolClass::ALL[i]
            );
        }
    }
}
