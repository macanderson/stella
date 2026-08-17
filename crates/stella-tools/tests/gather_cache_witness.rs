// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The #3467 witness, as an integration test so it drives the tool exactly as
//! a session does — through the public API, with nothing crate-private in
//! reach.
//!
//! The in-module test of the same name guards its equality assertion with
//! `if gathered > 0`, because a workspace whose graph never opens gathers
//! nothing and the claim would hold vacuously. This test removes that escape:
//! it asserts the first search **did** gather, so the second search gathering
//! nothing is a real saving rather than an absence of work.

use stella_tools::ctx::ToolCtx;
use stella_tools::registry::Tool;
use stella_tools::search::Search;

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
