//! What the context plane is told not to surface, and how it finds out.
//!
//! Its own module because the read moved *inside* the provider, ahead of the
//! budget pass (#712 deliverable 4), and the timing is the part that has to
//! stay obvious.

use std::collections::HashSet;
use std::sync::Arc;

/// Reads the ids this workspace currently suppresses, fresh.
///
/// A closure rather than a captured set because suppression must apply to the
/// *next* recall, not to the state at session start: quarantine is derived from
/// citations the previous turn wrote, and a session that cached it would keep
/// serving a memory the model just called untruthful.
pub type SuppressionReader = Arc<dyn Fn() -> Result<HashSet<String>, String> + Send + Sync>;

/// A reader that suppresses nothing — for tests, and for a plane assembled
/// without a workspace store behind it.
#[cfg(test)]
#[must_use]
pub fn no_suppression() -> SuppressionReader {
    Arc::new(|| Ok(HashSet::new()))
}
