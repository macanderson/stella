// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The project-trust boundary: whether the enumeration
//! `project_code_execution_trusted`'s doc comment claims is the one the code
//! has.
//!
//! A sibling file rather than more lines in `settings/tests.rs`, which sits at
//! the 1500-line ratchet.

/// **Witness for #4426.** Every surface the project-trust gate withholds
/// reaches the verdict through one named accessor, so a single grep
/// enumerates the boundary and `project_code_execution_trusted`'s doc comment
/// — the normative list — can be checked against the code instead of
/// believed.
///
/// Before this, two of the five rows read `trust.hooks` directly: a field
/// whose name says *hooks* and whose meaning is *code execution*. `rg
/// 'project_code_execution_trusted\('` found three sites for a five-row
/// table, so the list was unfalsifiable by any search a reviewer could run.
///
/// The set is written out rather than counted, because a count tells you the
/// answer changed and a set tells you which surface moved. Each entry names
/// the row it satisfies.
#[test]
fn every_code_execution_gate_is_reachable_by_one_grep() {
    let sites = crate::source_scan::production_files_mentioning("code_execution_trusted(");
    assert_eq!(
        sites,
        [
            // "The MCP servers declared in `.stella/mcp.toml`".
            "agent.rs",
            // "`<workspace>/.stella/plugins/`" — the roster's chokepoint …
            "plugin_cmd/roster.rs",
            // … plus `install --scope project`, which warns rather than
            // gates: the copy is the operator's own act, but the loader
            // refuses to read the tier back.
            "plugin_cmd.rs",
            // "`stella self-driving`'s issue work".
            "self_driving_cmd/work.rs",
            // The definition, and the doc comment that is the normative list.
            "settings.rs",
            // "Project-scope lifecycle hooks" and "project-scope
            // `context_providers`" — two rows, one function.
            "settings/merge.rs",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<std::collections::BTreeSet<_>>(),
        "a new gated surface adds its row to `project_code_execution_trusted`'s \
         doc comment and its file here, in the same change"
    );
}
