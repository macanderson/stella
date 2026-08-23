// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a manifest may not call itself, and why the answer is a reconstructed
//! history rather than a remembered one.
//!
//! A sibling of the parsing tests rather than part of them: `tests.rs` sits
//! against the 1500-line ceiling, and this is its own subject — not whether
//! the parser reads a field, but which names it must refuse before reading
//! anything.

use std::path::Path;

use crate::custom::parse_manifest;

/// Witness for #3853. `retired_builtin_names_are_rejected` above iterates the
/// two constants, so a name missing from both is a case it never runs — which
/// is how `web_fetch` stayed claimable for as long as it did, while
/// `custom-tools.mdx` read as though the lists were the history.
///
/// These are named one by one on purpose: each is a `ToolSchema` this
/// repository shipped and deleted (mostly in #3244), recovered from the
/// catalog's own git history rather than from memory, and each is a name a
/// plausible manifest would reach for.
#[test]
fn the_web_family_and_the_rest_of_the_deleted_surface_are_reserved() {
    for former in [
        "web_fetch",
        "web_search",
        "web_download",
        "web_extract_assets",
        "screenshot",
        "ci_status",
        "repo_status",
        "repo_commit",
        "create_issue",
        "search_issues",
        "save_memory",
        "recall_context",
        "tool_search",
        "install_skill",
        "list_scripts",
        "diagnostics",
        "code_graph",
        "semantic_code_search",
        "probe_capability",
        "send_stdin",
        "generate_image",
    ] {
        assert!(
            crate::catalog::is_retired(former),
            "`{former}` was a dispatch name Stella shipped and deleted, so it must stay \
             reserved — reserving is reversible by deleting a line, releasing is not"
        );
        let src = format!("name = \"{former}\"\ndescription = \"d\"\ncommand = [\"./x.sh\"]");
        let err = parse_manifest(&src, Path::new("t.toml")).unwrap_err();
        assert!(err.contains("is reserved"), "`{former}` -> {err}");
    }
}
