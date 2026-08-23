// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What the session gather cache must and must not survive, driven through
//! the tool's public API exactly as a session drives it — the #3467 witnesses
//! that it holds across a repeat search and a cancelled one, and the #3196
//! witness that it does not hold across an index change.
//!
//! The in-module #3467 test guards its equality assertion with
//! `if gathered > 0`, because a workspace whose graph never opens gathers
//! nothing and the claim would hold vacuously. These remove that escape: each
//! asserts the first search **did** gather, so the second search's behaviour
//! is a real saving (or a real re-gather) rather than an absence of work.

use stella_tools::ctx::ToolCtx;
use stella_tools::registry::Tool;
use stella_tools::search::Search;

/// **Witness (#3196).** A cached neighborhood's `importers` list is a function
/// of the *rest* of the tree, so the file's own content identity cannot
/// invalidate it: a second file importing this one changes nothing about this
/// one's bytes. Before the index-generation stamp, the second search in a
/// session served the first search's `imported by:` line for a file nobody had
/// touched, and only a new session or an edit to the imported file recovered
/// the truth.
///
/// Depth 6 because `Facet::Importers` first appears at 5
/// (`stella_core::search::facets_at`); at the default depth the assertion
/// would pass by rendering neither list.
#[tokio::test]
async fn an_importer_added_mid_session_reaches_the_next_search() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    // A `use crate::…` path resolves through the package name, so the fixture
    // needs a manifest and a crate root (`stella_graph::rust_resolve`).
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"importer-fixture\"\nversion = \"0.0.0\"\n",
    )
    .expect("write manifest");
    std::fs::write(root.join("src/lib.rs"), "pub mod imported;\n").expect("write crate root");
    std::fs::write(root.join("src/imported.rs"), "pub fn stamped_target() {}\n")
        .expect("write imported");

    let tool = Search::with_config(stella_tools::search::SearchConfig {
        depth: stella_core::search::Depth::new(6),
        budget: 200_000,
    });
    let ctx = ToolCtx::bare(root.to_path_buf());
    let input = serde_json::json!({ "query": "stamped_target" });

    let first = tool.execute(&input, &ctx).await;
    assert!(!first.is_error(), "{first:?}");
    let first_text = rendered(&first);
    assert!(
        first_text.contains("src/imported.rs"),
        "the fixture must rank the imported file, or nothing below proves \
         anything: {first_text}"
    );
    assert!(
        !first_text.contains("src/importing.rs"),
        "nothing imports it yet: {first_text}"
    );
    assert!(
        tool.gathered() > 0,
        "the first search must gather from the graph, or there is no cached \
         entry for the second one to be served stale from"
    );

    // A new file importing it. `src/imported.rs` is untouched, so its content
    // identity — the cache's only key before #3196 — is unchanged.
    std::fs::write(
        root.join("src/importing.rs"),
        "use crate::imported::stamped_target;\n\npub fn call_it() {\n    stamped_target();\n}\n",
    )
    .expect("write importer");

    let second = tool.execute(&input, &ctx).await;
    assert!(!second.is_error(), "{second:?}");
    let second_text = rendered(&second);
    assert!(
        second_text.contains("src/importing.rs"),
        "the second search must name the new importer; a cache keyed only by \
         the imported file's own bytes serves the stale list: {second_text}"
    );
}

fn rendered(output: &stella_protocol::tool::ToolOutput) -> String {
    match output {
        stella_protocol::tool::ToolOutput::Ok { content, .. } => content.clone(),
        other => panic!("expected a rendered answer, got {other:?}"),
    }
}

/// A search whose future is **dropped mid-flight** must not destroy the
/// session cache.
///
/// Found by adversarial audit. The first implementation moved the cache out of
/// its mutex with `mem::take`, awaited the engine, and put it back — but
/// `stella_core::driver` runs every tool call inside `tokio::time::timeout`,
/// which *drops* the future when the limit elapses. The taken cache went with
/// it, the mutex kept a default forever, and every later search in the session
/// silently paid full price again. A cancelled call is ordinary (a timeout, a
/// budget stop, Esc), so this is not an edge case.
///
/// Fails against that implementation: `gathered()` returns 0 after the drop
/// and the warm entry is gone.
#[tokio::test]
async fn a_cancelled_search_leaves_the_session_cache_intact() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("src")).expect("mkdir src");
    std::fs::write(
        dir.path().join("src/callee.rs"),
        "pub fn cached_target() {}\n",
    )
    .expect("write");

    let tool = Search::from_env();
    let ctx = ToolCtx::bare(dir.path().to_path_buf());
    let input = serde_json::json!({ "query": "cached_target" });

    // Warm it.
    let first = tool.execute(&input, &ctx).await;
    assert!(!first.is_error(), "{first:?}");
    let warmed = tool.gathered();
    assert!(warmed > 0, "the first search must gather something to lose");

    // Now cancel one: poll it once and drop it, exactly as an elapsed
    // `tokio::time::timeout` does.
    {
        let pending = tool.execute(&input, &ctx);
        tokio::pin!(pending);
        let _ = futures_util::poll!(pending.as_mut());
        // `pending` is dropped here, mid-await.
    }

    assert_eq!(
        tool.gathered(),
        warmed,
        "a cancelled search must neither lose the cache nor re-gather"
    );

    // And the cache still serves.
    let after = tool.execute(&input, &ctx).await;
    assert!(!after.is_error(), "{after:?}");
    assert_eq!(
        tool.gathered(),
        warmed,
        "the cache must still be warm after a cancelled call"
    );
}

/// One `Search` instance, two identical searches: the first populates the
/// session cache, the second must be served from it.
#[tokio::test]
async fn a_repeat_search_gathers_nothing_new_and_the_first_one_gathered() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("src")).expect("mkdir src");
    std::fs::write(
        dir.path().join("src/callee.rs"),
        "pub fn cached_target() {}\n",
    )
    .expect("write callee");
    std::fs::write(
        dir.path().join("src/caller.rs"),
        "pub fn calling_site() {\n    cached_target();\n}\n",
    )
    .expect("write caller");

    let tool = Search::from_env();
    let ctx = ToolCtx::bare(dir.path().to_path_buf());
    let input = serde_json::json!({ "query": "cached_target" });

    let first = tool.execute(&input, &ctx).await;
    let after_first = tool.gathered();
    assert!(
        !first.is_error(),
        "the first search must succeed: {first:?}"
    );
    assert!(
        after_first > 0,
        "the first search must actually gather from the code graph, or the \
         repeat assertion below proves nothing"
    );

    let second = tool.execute(&input, &ctx).await;
    assert!(
        !second.is_error(),
        "the second search must succeed: {second:?}"
    );
    assert_eq!(
        tool.gathered(),
        after_first,
        "the repeat search must be served entirely from the session cache"
    );
}
