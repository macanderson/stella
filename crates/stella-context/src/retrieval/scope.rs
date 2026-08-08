//! Domain scoping: the two scopes a recall runs under, and the two
//! projections the corpus tag map feeds.
//!
//! Split out of [`super`] because the distinction between them is the subtlest
//! thing in retrieval and had already been got wrong once. Judged against the
//! session's scope, "shares a domain with the query" is the same predicate as
//! "is tagged at all", so a gate admitting on it admitted the whole in-scope
//! corpus (#2289, and the rung's removal and return in #2333). Keeping the
//! ranking projection and the evidence projection side by side here makes the
//! asymmetry visible: they read the same map through different scopes, and
//! only one of them may decide whether a frame exists.
//!
//! Like [`super::evidence`] and [`super::ranking`], everything here is a pure
//! function over already-loaded rows — the parent owns the one query that
//! loads the map.

use std::collections::{HashMap, HashSet};

use crate::candidates::NodeMeta;

/// The two domain scopes one recall runs under.
///
/// They are separate because they answer different questions, and no single
/// value answers both correctly:
///
/// - [`session`](Self::session) is the workspace vocabulary the session opened
///   with. It **filters** (a node tagged exclusively outside it is not a
///   candidate) and it **ranks** (`overlap_ranking`). Both are correct uses of
///   a scope that does not vary per turn.
/// - [`query`](Self::query) is what *this* goal selected — in the CLI, the
///   domains owning the workspace files the goal named. It is the only one
///   that may serve as admission **evidence** (`evidence_ids`), and then only
///   when it is a non-empty **proper subset** of `session`. A scope equal to
///   the vocabulary has narrowed nothing, so it admits nothing.
///
/// Collapsing them into one value was the #2289 regression. Narrowing
/// `session` to repair it would have been worse rather than better: `session`
/// also drives the exclusion filter, so a narrower value silently starts
/// *suppressing* memories instead of merely declining to admit them. Two
/// fields is the only shape that gets both halves right.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecallScope {
    /// The session's domain vocabulary. Empty means the whole workspace.
    pub session: Vec<String>,
    /// The domains this query selected. Empty means the query narrowed
    /// nothing, which disarms the domain evidence channel.
    pub query: Vec<String>,
}

impl RecallScope {
    /// A scope with no per-query narrowing: filters and ranks by `session`,
    /// and can never admit on domain overlap.
    ///
    /// The honest default for a caller with no way to derive a per-query
    /// scope, which is every caller except the CLI's `ScopedStore::query`.
    pub fn session_only(session: &[String]) -> Self {
        Self {
            session: session.to_vec(),
            query: Vec::new(),
        }
    }
}

/// The domain-overlap **ranking** list: nodes sharing more of the session's
/// domains rank higher, folded into RRF like any other signal.
///
/// Descending overlap, ties on node id. The tiebreak is not cosmetic: `metas`
/// arrives in SQLite's unordered scan order and overlap counts collide
/// constantly (most tagged nodes carry exactly one domain), so ordering by
/// overlap alone would hand equally-tagged nodes a different rank each run,
/// and RRF converts rank directly into score.
pub(crate) fn overlap_ranking(
    metas: &[NodeMeta],
    tags: &HashMap<i64, Vec<String>>,
    session: &HashSet<&str>,
) -> Vec<i64> {
    if session.is_empty() {
        return Vec::new();
    }
    let none: Vec<String> = Vec::new();
    let mut scored: Vec<(i64, usize)> = metas
        .iter()
        .filter_map(|m| {
            let overlap = tags
                .get(&m.id)
                .unwrap_or(&none)
                .iter()
                .filter(|d| session.contains(d.as_str()))
                .count();
            (overlap > 0).then_some((m.id, overlap))
        })
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.into_iter().map(|(id, _)| id).collect()
}

/// The domain **evidence** set: nodes carrying at least one domain the query
/// itself selected.
///
/// Unranked on purpose — admission is a set membership question, and the
/// fusion above has already decided the order. The caller must have cleared
/// [`super::evidence::scope_is_query_conditional`] first; this function does
/// not re-check it, because the check needs both scopes and this needs only
/// one.
pub(crate) fn evidence_ids(
    metas: &[NodeMeta],
    tags: &HashMap<i64, Vec<String>>,
    query: &[String],
) -> Vec<i64> {
    let selected: HashSet<&str> = query.iter().map(String::as_str).collect();
    metas
        .iter()
        .filter(|m| {
            tags.get(&m.id)
                .is_some_and(|t| t.iter().any(|d| selected.contains(d.as_str())))
        })
        .map(|m| m.id)
        .collect()
}
