//! Domains and scope — the normalized tag table, its indexable junctions, and
//! the anti-join that decides which nodes a scoped recall must exclude.
//!
//! Split out of the store module unchanged (#712 deliverable 1).

use rusqlite::{Connection, params};

use crate::error::ContextError;

/// Insert a domain by name (idempotent), optionally setting/refreshing its
/// description. Returns its rowid.
pub(crate) fn upsert_domain(
    conn: &Connection,
    name: &str,
    description: Option<&str>,
    now: &str,
) -> Result<i64, ContextError> {
    if name.trim().is_empty() {
        return Err(ContextError::InvalidInput(
            "domain name must be non-empty".into(),
        ));
    }
    // COALESCE keeps an existing description when a later tag-only write passes
    // None, but lets an explicit definition set or update it.
    let id: i64 = conn.query_row(
        "INSERT INTO domain (name, description, recorded_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(name) DO UPDATE SET
             description = COALESCE(excluded.description, domain.description)
         RETURNING id",
        params![name, description, now],
        |r| r.get(0),
    )?;
    Ok(id)
}

/// Tag a node with domains (auto-creating unknown ones). Returns the number of
/// new tag associations written.
pub(crate) fn tag_node_domains(
    conn: &Connection,
    node_id: i64,
    domains: &[String],
    now: &str,
) -> Result<usize, ContextError> {
    let mut added = 0;
    for name in domains {
        let domain_id = upsert_domain(conn, name, None, now)?;
        added += conn.execute(
            "INSERT INTO node_domains (node_id, domain_id) VALUES (?1, ?2)
             ON CONFLICT(node_id, domain_id) DO NOTHING",
            params![node_id, domain_id],
        )?;
    }
    Ok(added)
}

/// Tag an edge/fact with domains (auto-creating unknown ones). Returns the
/// number of new tag associations written.
pub(crate) fn tag_edge_domains(
    conn: &Connection,
    edge_id: i64,
    domains: &[String],
    now: &str,
) -> Result<usize, ContextError> {
    let mut added = 0;
    for name in domains {
        let domain_id = upsert_domain(conn, name, None, now)?;
        added += conn.execute(
            "INSERT INTO edge_domains (edge_id, domain_id) VALUES (?1, ?2)
             ON CONFLICT(edge_id, domain_id) DO NOTHING",
            params![edge_id, domain_id],
        )?;
    }
    Ok(added)
}

/// Every LIVE node's domain names in one scan, sorted per node for stable
/// citation display — the batched form of the old per-node query. Recall
/// runs this once per prompt; one statement per live node was an N+1 whose
/// cost grew with lifetime memory size. Superseded nodes are filtered in
/// SQL (same liveness predicate as [`live_node_metas`]): recall only looks up
/// live candidates, so loading dead nodes' tags made the scan grow with
/// historical store size for no reader.
pub(crate) fn domains_by_node(
    conn: &Connection,
) -> Result<std::collections::HashMap<i64, Vec<String>>, ContextError> {
    let mut stmt = conn.prepare(
        "SELECT nd.node_id, d.name FROM node_domains nd
         JOIN domain d ON d.id = nd.domain_id
         JOIN node n ON n.id = nd.node_id
         WHERE n.superseded_at IS NULL
         ORDER BY nd.node_id, d.name",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    let mut out: std::collections::HashMap<i64, Vec<String>> = std::collections::HashMap::new();
    for r in rows {
        let (id, name) = r?;
        out.entry(id).or_default().push(name);
    }
    Ok(out)
}

/// Live node ids that carry a domain tag but NONE in `scope` — exactly the
/// set a scoped recall must exclude. Untagged nodes are never returned (they
/// stay candidates): most memories — reflections whose lessons name no domain,
/// episodes from turns that touched no taxonomy-covered file — are untagged,
/// and a scope filter that dropped them would silence recall entirely the
/// moment `stella init` writes a taxonomy. An empty `scope` returns an empty
/// set (nothing is out of scope).
///
/// The out-of-scope test runs in one SQL statement (an anti-join against the
/// in-scope tag set) rather than materializing every tagged id in memory and
/// differencing in Rust — a large initialized workspace can carry many tags
/// unrelated to the active scope.
pub(crate) fn node_ids_excluded_by_scope(
    conn: &Connection,
    scope: &[String],
) -> Result<std::collections::HashSet<i64>, ContextError> {
    if scope.is_empty() {
        return Ok(std::collections::HashSet::new());
    }
    let placeholders = std::iter::repeat_n("?", scope.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT DISTINCT nd.node_id FROM node_domains nd
         JOIN node n ON n.id = nd.node_id
         WHERE n.superseded_at IS NULL
           AND nd.node_id NOT IN (
             SELECT nd2.node_id FROM node_domains nd2
             JOIN domain d ON d.id = nd2.domain_id
             WHERE d.name IN ({placeholders})
           )"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut set = std::collections::HashSet::new();
    for r in stmt.query_map(rusqlite::params_from_iter(scope.iter()), |r| {
        r.get::<_, i64>(0)
    })? {
        set.insert(r?);
    }
    Ok(set)
}

/// All defined domains as `(name, description)`, for status/inspection.
pub(crate) fn list_domains(
    conn: &Connection,
) -> Result<Vec<(String, Option<String>)>, ContextError> {
    let mut stmt = conn.prepare("SELECT name, description FROM domain ORDER BY name")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}
