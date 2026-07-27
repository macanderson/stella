//! Who reads this workspace's suppression state, and when.
//!
//! Its own module because the read now happens inside the context provider
//! rather than after it (#712 deliverable 4), and the *when* is the part
//! that has to stay obvious: cached suppression would keep serving a memory
//! the previous turn just called untruthful.

/// The three suppression sets, read together and applied as one, fresh on every
/// recall.
///
/// Quarantine is *derived* — the model kept calling a memory untruthful; a
/// tombstone is *stored* — a person read it and said remove it; retirement is
/// *derived* — the efficacy loop replayed the promotion log and the record's
/// standing came back `Retired` (#715 deliverable 5). All three are fail-closed
/// for the same reason: if the state cannot be read, surfacing everything is
/// the one outcome that is definitely wrong.
///
/// **Retirement is not a second suppression mechanism** (spec §5.7). It adds a
/// set to the union this reader already computes rather than a parallel path,
/// and it deliberately does *not* write `node.superseded_at` — that column is
/// filtered by `node_by_public_id`, so projecting retirement onto it would make
/// a retired record impossible to fetch by id, when staying explicitly
/// retrievable is exactly what distinguishes retirement from forgetting.
///
/// The tombstone half is now also projected onto `node.superseded_at` when a
/// forget is recorded, so recall would exclude it even with this reader absent.
/// It stays in the set anyway: a workspace whose tombstones predate that
/// projection has rows the plane does not know about, and a suppression that
/// only works for memories forgotten by a new enough binary is not one the
/// product can claim.
pub(super) fn suppression_reader(
    workspace_root: &std::path::Path,
    context: std::sync::Arc<stella_context::ContextStore>,
) -> crate::contextgraph::SuppressionReader {
    let root = workspace_root.to_path_buf();
    std::sync::Arc::new(move || {
        let mut ids = stella_store::Store::open(&root)
            .and_then(|store| {
                let mut ids = store.quarantined_memory_ids()?;
                ids.extend(store.forgotten_ids(stella_store::ContextSurface::Memory)?);
                Ok(ids)
            })
            .map_err(|e| format!("suppression state unavailable: {e}"))?;
        ids.extend(super::retirement::retired_ids(&context));
        Ok(ids)
    })
}
