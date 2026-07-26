//! Who reads this workspace's suppression state, and when.
//!
//! Its own module because the read now happens inside the context provider
//! rather than after it (#712 deliverable 4), and the *when* is the part
//! that has to stay obvious: cached suppression would keep serving a memory
//! the previous turn just called untruthful.

/// The two suppression sets, read together and applied as one, fresh on every
/// recall.
///
/// Quarantine is *derived* — the model kept calling a memory untruthful; a
/// tombstone is *stored* — a person read it and said remove it. Both are
/// fail-closed for the same reason: if the state cannot be read, surfacing
/// everything is the one outcome that is definitely wrong.
///
/// The tombstone half is now also projected onto `node.superseded_at` when a
/// forget is recorded, so recall would exclude it even with this reader absent.
/// It stays in the set anyway: a workspace whose tombstones predate that
/// projection has rows the plane does not know about, and a suppression that
/// only works for memories forgotten by a new enough binary is not one the
/// product can claim.
pub(super) fn suppression_reader(
    workspace_root: &std::path::Path,
) -> crate::contextgraph::SuppressionReader {
    let root = workspace_root.to_path_buf();
    std::sync::Arc::new(move || {
        stella_store::Store::open(&root)
            .and_then(|store| {
                let mut ids = store.quarantined_memory_ids()?;
                ids.extend(store.forgotten_ids(stella_store::ContextSurface::Memory)?);
                Ok(ids)
            })
            .map_err(|e| format!("suppression state unavailable: {e}"))
    })
}
