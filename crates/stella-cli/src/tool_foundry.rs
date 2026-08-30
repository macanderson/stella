//! Tool Foundry wiring — the CLI half of the self-authored-tool protocol.
//!
//! A proposal is a manifest+script pair staged under `.stella/tools/proposed/`,
//! a directory discovery's non-recursive scan can never register from. The
//! manual verbs live in [`adopt`] (`--adopt` proves a staged tool with its
//! capability witness, `--enable` grants, `--foundry` reports) and [`ops`]
//! (`--draft` authors from the gap ledger, `--rollback` restores a recorded
//! version, `--status` shows per-tool health).
//!
//! The live loop runs on the end-of-turn seam beside skill
//! mining: [`end_of_turn`] mines the store's recent shell history for gaps
//! ([`gaps`]), ledgers the novel ones, and — under `foundry.autonomy =
//! "auto"` — carries them through author → validate → witness-adopt → enable
//! ([`autonomy`]), with network denial, telemetry, the circuit breaker, and
//! versioned rollback standing in place of the retired human ceremony.

pub(crate) mod adopt;
pub(crate) mod author;
pub(crate) mod autonomy;
pub(crate) mod gaps;
pub(crate) mod ops;

use std::path::Path;

/// The end-of-turn hook: detect gaps from the store's recent shell
/// history, ledger the novel ones, and run whatever autonomy the workspace's
/// `[foundry]` settings allow. Returns user-visible notices; never fails the
/// turn it rides on.
pub(crate) async fn end_of_turn(root: &Path, store: Option<&stella_store::Store>) -> Vec<String> {
    let config = match crate::settings::Settings::load(root) {
        Ok(settings) => match settings.foundry_config() {
            Ok(config) => config,
            // A threshold the module refuses is a mistake someone has to
            // see, and a config that cannot be trusted runs nothing
            // autonomous — fail closed, loudly.
            Err(diagnostic) => {
                return vec![format!(
                    "{diagnostic} — tool-gap detection skipped this turn"
                )];
            }
        },
        Err(_) => crate::settings::FoundryConfig::default(),
    };
    end_of_turn_with(root, store, &config).await
}

/// [`end_of_turn`] with the config already resolved — the seam the witness
/// tests drive, so they exercise the live hook path without depending on
/// the test machine's own settings scopes.
pub(crate) async fn end_of_turn_with(
    root: &Path,
    store: Option<&stella_store::Store>,
    config: &crate::settings::FoundryConfig,
) -> Vec<String> {
    let (new_gaps, notice) = gaps::scan_and_ledger(root, store, config.detection);
    let mut notices: Vec<String> = notice.into_iter().collect();
    if !new_gaps.is_empty() {
        notices.extend(autonomy::run_autonomy(root, store, &new_gaps, config).await);
    }
    notices
}

/// Stamp the operator's `[foundry]` runtime policy onto every discovered
/// foundry-authored tool: whether it is on the network allowlist, and the
/// breaker thresholds its launches are held to. Hand-written tools are left
/// untouched. Fail-closed: an unreadable settings chain means the defaults —
/// empty allowlist, shipped breaker.
pub(crate) fn apply_foundry_runtime(tools: &mut [stella_tools::custom::CustomTool], root: &Path) {
    let config = crate::settings::Settings::load(root)
        .ok()
        .and_then(|settings| settings.foundry_config().ok())
        .unwrap_or_default();
    for tool in tools.iter_mut() {
        if tool
            .foundry
            .as_ref()
            .is_some_and(|p| p.is_foundry_authored())
        {
            tool.foundry_runtime = stella_tools::custom::FoundryRuntimePolicy {
                network_allowed: config.network_allowlist.iter().any(|n| n == &tool.name),
                breaker: Some(config.breaker),
            };
        }
    }
}

/// A tiny FNV-1a hash — enough to key dedup deterministically, and it keeps
/// this module free of a hashing dependency. `pub(crate)` because the ingest
/// staleness alerts (`ingest_cmd::lineage`) derive their notification ids
/// this way, and two copies of a hash function is how they drift apart.
pub(crate) fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hash keys durable notification ids (`ingest_cmd::lineage`), so its
    /// output for a given input is a compatibility surface: a changed value
    /// re-surfaces every already-delivered notification.
    #[test]
    fn fnv1a_is_stable_for_a_signature() {
        assert_eq!(fnv1a(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a("jq {p1} {p2}"), fnv1a("jq {p1} {p2}"));
        assert_ne!(fnv1a("jq {p1} {p2}"), fnv1a("jq {p1}"));
    }
}
