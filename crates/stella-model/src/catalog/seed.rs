//! The in-binary seed rows, one module per provider.
//!
//! [`Catalog::seed`](super::Catalog::seed) is the offline floor: one row per
//! provider `crates/stella-cli/src/config.rs`'s `PROVIDERS` table can select,
//! keyed to that table's `default_model`. `stella models refresh` grows the
//! *runtime* catalog with live master-list data and never touches these.
//!
//! A row is not one line — it carries a slug, four prices, two ceilings, and
//! the argument for why the number it holds is the number a user gets. That is
//! what put `catalog.rs` 77 lines from the 1500-line ceiling with the seed
//! still inside it (#3862), and why the rows live here: a new model row goes in
//! its provider's file, and every provider has room to grow independently.
//!
//! The order of [`rows`] is the order [`Catalog::seed`](super::Catalog::seed)
//! declared before the split, and it is observable —
//! `install_runtime_extends_current_without_disturbing_seed_rows` reads the
//! first entry, and `with_entries` documents that seed rows come first so seed
//! lookups keep their exact pre-refresh results.

use super::CatalogEntry;

mod anthropic;
mod bedrock;
mod deepseek;
mod google;
mod openai;
mod openrouter;
mod xai;
mod zai;

/// Every seeded row, in declaration order.
pub(super) fn rows() -> Vec<CatalogEntry> {
    let mut entries = zai::rows();
    entries.extend(anthropic::rows());
    entries.extend(openai::rows());
    entries.extend(xai::rows());
    entries.extend(deepseek::rows());
    entries.extend(google::rows());
    entries.extend(bedrock::rows());
    entries.extend(openrouter::rows());
    entries
}
