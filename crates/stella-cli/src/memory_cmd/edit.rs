//! `stella memory edit <id> <text>` — change the words of a memory.
//!
//! Two doors reach this code. One is the CLI verb. The other is `e` on a
//! memory row in the deck. Both call [`edit_memory_text`], so both do the
//! same thing: one live row on the same lineage, the old words kept as
//! history, and the kind kept too.
//!
//! The write is async, since the store is. That decides who owns the runtime.
//! The CLI verb is sync, so it blocks on the future here. The deck is already
//! in a runtime, so it awaits. A `block_on` there would panic and end the
//! session.

use colored::Colorize;

use super::{clip, open_context};

/// Entry point for `stella memory edit <id> <text>`.
///
/// Writes a new version on the memory's lineage. The mirror row is keyed by
/// lineage, so it moves in place: one live row, the same `nod_…` id, and the
/// old text kept as history. It does not compete for a recall slot.
///
/// Before lineage this verb could not be built. A memory was named by the
/// hash of its text. "The same memory with new words" made no sense: you got
/// a second memory, and the first was still recalled with its old text.
pub fn run_memory_edit(id: &str, text: &str) -> Result<(), String> {
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    // The one `block_on` on this path, and it belongs here. `main::run` is
    // sync, and the write is async because the store is. The deck awaits
    // instead: a `block_on` on its thread would panic, since that thread
    // already drives a runtime.
    let previous = tokio::runtime::Runtime::new()
        .map_err(|e| format!("cannot start the async runtime: {e}"))?
        .block_on(edit_memory_text(&workspace_root, id, text))?;

    println!("  {} revised {id}", "✓".green());
    println!(
        "    {} {}",
        "was:".dimmed(),
        clip(previous.trim(), 68).dimmed()
    );
    println!("    {} {}", "now:".dimmed(), clip(text.trim(), 68).dimmed());

    // The tally is this verb's own reporting, not part of the edit, so it
    // reads the store again after the write lands. The deck answers with one
    // notice line and has no use for it.
    let Some(context) = open_context(&workspace_root)? else {
        return Ok(());
    };
    let stats = context
        .memory_lineage_stats()
        .map_err(|e| format!("cannot read lineage stats: {e}"))?;
    println!(
        "    {}",
        format!(
            "{} live {}, {} superseded revision{} kept as history",
            stats.live,
            if stats.live == 1 {
                "memory"
            } else {
                "memories"
            },
            stats.superseded,
            if stats.superseded == 1 { "" } else { "s" }
        )
        .dimmed()
    );
    Ok(())
}

/// [`run_memory_edit`]'s work, over a root it is handed, printing nothing.
///
/// Split out for the deck's `e edit`, which makes the same change from a
/// transcript row. Both readers must get the same thing. Two copies of "write
/// a new version on the lineage" would be free to drift on the part that
/// matters: keeping the kind.
///
/// Returns the text it replaced, which the CLI verb prints as `was:`.
///
/// Async because the deck calls it from inside its driver loop.
/// `tokio::runtime::Runtime::block_on` panics on a thread that already drives
/// a runtime, so one keypress would end the session. The sync CLI verb owns
/// the one `block_on` instead.
pub async fn edit_memory_text(
    workspace_root: &std::path::Path,
    id: &str,
    text: &str,
) -> Result<String, String> {
    if text.trim().is_empty() {
        return Err(
            "the replacement text must not be empty — use `stella memory forget` to \
                    remove a memory"
                .to_string(),
        );
    }
    let Some(context) = open_context(workspace_root)? else {
        return Err("this workspace has no context store yet — nothing to edit".to_string());
    };
    let Some(node) = context
        .node_by_public_id(id)
        .map_err(|e| format!("cannot read memory `{id}`: {e}"))?
    else {
        return Err(format!(
            "`{id}` is not a live memory — check the id against `stella memory list`"
        ));
    };
    let Some(lineage) = context
        .memory_lineage(id)
        .map_err(|e| format!("cannot resolve the lineage of `{id}`: {e}"))?
    else {
        return Err(format!(
            "`{id}` is not a memory — only memories have revisions (episodes are a record \
             of what happened and are not rewritten)"
        ));
    };
    let previous = node.content.clone();
    // Keep the kind the lineage already has. A default here would turn a
    // mined lesson into a hand-written note. That changes whether the
    // restatement filter treats it as one the loop may write again.
    let kind = context
        .memory_kind(&lineage)
        .map_err(|e| format!("cannot read the kind of `{id}`: {e}"))?
        .and_then(|k| stella_context::MemoryKind::parse(&k))
        .unwrap_or(stella_context::MemoryKind::Note);

    context
        .upsert(
            stella_context::ContextDelta::new()
                .with_memory(stella_context::MemoryInput::new(kind, text.trim()).revises(&lineage)),
        )
        .await
        .map_err(|e| format!("cannot write the revision: {e}"))?;

    Ok(previous)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The witness: an edit must land from a thread that already drives a
    /// runtime.
    ///
    /// A `#[tokio::test]` is one such thread. So is the deck's driver loop,
    /// where `e` on a memory row is served. Code that builds a runtime of its
    /// own and calls `block_on` panics here. So this test fails against that
    /// shape by construction, not by an assertion someone can weaken.
    #[tokio::test]
    async fn a_memory_edits_from_inside_a_running_runtime() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let context_db = stella_store::workspace_private_sqlite_path(root, "context.db").unwrap();
        let context = stella_context::ContextStore::open(&context_db).unwrap();
        context
            .upsert(stella_context::ContextDelta::new().with_memory(
                stella_context::MemoryInput::new(
                    stella_context::MemoryKind::Note,
                    "prefer rg over grep",
                ),
            ))
            .await
            .unwrap();
        let id = context.memory_nodes().unwrap()[0].public_id.clone();
        drop(context);

        let previous = edit_memory_text(root, &id, "prefer rg over grep, and fd over find")
            .await
            .expect("the deck edits from inside the driver loop's runtime");
        assert_eq!(previous, "prefer rg over grep");

        let context = stella_context::ContextStore::open(&context_db).unwrap();
        let nodes = context.memory_nodes().unwrap();
        assert_eq!(
            nodes.len(),
            1,
            "a revision replaces; it mints no second row"
        );
        assert_eq!(
            nodes[0].public_id, id,
            "the id a reader holds still resolves"
        );
        assert_eq!(nodes[0].content, "prefer rg over grep, and fd over find");
    }
}
