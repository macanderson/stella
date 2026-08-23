// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One model's catalog card, for the turn page's agent profile section.
//!
//! The profile card prints, per seat a turn used, the settings the model was
//! bound with *and* the defaults the model itself offers — context window,
//! output ceiling, reasoning support, pricing. The bindings come from the
//! run's own receipts; the offers live in the user-tier model catalog
//! (`~/.stella/catalog.db`, written by `stella-model`'s catalog sync). This
//! module reads that catalog the same way the rest of the crate reads
//! `store.db`: read-only over rusqlite, never by linking the crate that
//! writes it — the observer boundary `db.rs` documents.
//!
//! The catalog distinguishes `api_provider` (whom we call — `openrouter`)
//! from `model_provider` (whose silicon — `anthropic`), which is exactly the
//! split the profile card has to print for a gateway-routed seat.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::db::{DbError, open_read_only};

/// The user-tier catalog database, or wherever `STELLA_HOME` points.
fn catalog_db() -> PathBuf {
    stella_home::data_dir().join("catalog.db")
}

/// The newest catalog card for `(api_provider, slug)`.
///
/// `found: false` when the catalog is absent or holds no such card — a state,
/// not a failure: a workspace that never synced the catalog still gets a
/// profile card, just one that says the offered defaults are unknown.
pub(crate) fn model_card(api_provider: &str, slug: &str) -> Result<Value, DbError> {
    let Some(conn) = open_read_only(&catalog_db()) else {
        return Ok(json!({ "found": false, "note": "no model catalog on this machine" }));
    };
    let mut stmt = match conn.prepare(
        "SELECT c.api_provider, c.model_provider, c.slug, c.display_name, c.family,
                v.context_window, v.max_output_tokens, v.supports_reasoning,
                v.supports_tools, v.knowledge, v.release_date, v.last_updated,
                v.input_usd_per_mtok, v.output_usd_per_mtok,
                v.cached_input_usd_per_mtok, v.cache_write_usd_per_mtok
         FROM model_cards c
         JOIN model_card_versions v ON v.model_card_id = c.id
         WHERE c.api_provider = ?1 AND c.slug = ?2
         ORDER BY v.version DESC
         LIMIT 1",
    ) {
        Ok(stmt) => stmt,
        // A catalog older than these columns is the same state as no catalog.
        Err(_) => return Ok(json!({ "found": false, "note": "catalog schema too old" })),
    };
    let row = stmt
        .query_row([api_provider, slug], |r| {
            Ok(json!({
                "found": true,
                "api_provider": r.get::<_, String>(0)?,
                "model_provider": r.get::<_, String>(1)?,
                "slug": r.get::<_, String>(2)?,
                "display_name": r.get::<_, Option<String>>(3)?,
                "family": r.get::<_, Option<String>>(4)?,
                "context_window": r.get::<_, Option<i64>>(5)?,
                "max_output_tokens": r.get::<_, Option<i64>>(6)?,
                "supports_reasoning": r.get::<_, Option<i64>>(7)?.map(|v| v != 0),
                "supports_tools": r.get::<_, Option<i64>>(8)?.map(|v| v != 0),
                "knowledge": r.get::<_, Option<String>>(9)?,
                "release_date": r.get::<_, Option<String>>(10)?,
                "last_updated": r.get::<_, Option<String>>(11)?,
                "input_usd_per_mtok": r.get::<_, Option<f64>>(12)?,
                "output_usd_per_mtok": r.get::<_, Option<f64>>(13)?,
                "cached_input_usd_per_mtok": r.get::<_, Option<f64>>(14)?,
                "cache_write_usd_per_mtok": r.get::<_, Option<f64>>(15)?,
            }))
        })
        .unwrap_or(json!({ "found": false }));
    Ok(row)
}
