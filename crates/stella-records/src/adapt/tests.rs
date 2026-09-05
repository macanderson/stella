// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Moved here with the record adapters.

use super::{record_candidates, record_drops};
use crate::records::{Facts, Registry, TurnFacts, registry};
use stella_core::rules::RuleFile;
use stella_core::steering::SteeringSource;

/// One record is drawn. The other is cut. That covers both halves.
fn two_record_registry() -> Registry {
    let file = RuleFile {
        path: ".stella/rules/ctx.acme.pair.toml".to_string(),
        contents: r#"
schema = "context-record/v0.1"
set_id = "acme"

[[record]]
lineage_id = "ctx.acme.staging-url"
kind = "preference"
statement = "The staging URL is https://stage.example."
status = "active"
origin = "user"

[record.steering]
force = "may"
precedence = 40

[[record]]
lineage_id = "ctx.acme.long-tail"
kind = "preference"
statement = "A long low-precedence fact that will not fit a tight budget at all."
status = "active"
origin = "user"

[record.steering]
force = "info"
"#
        .to_string(),
        contributed_by: None,
    };
    registry::load(&[], &[file], &Facts::default())
}

/// Drawn handles map to candidates. Cut handles map to the drop ledger. The
/// adapter re-decides nothing, so the two lists are what the channel picked.
#[test]
fn record_candidates_and_drops_partition_the_channels_own_decision() {
    let registry = two_record_registry();
    let facts = TurnFacts {
        text: "anything",
        paths: &[],
    };
    // A budget the first record fits and the second does not.
    let rendered = registry.render_volatile_for_turn(&facts, Some(80));
    assert_eq!(rendered.rendered, vec!["staging-url"]);
    assert_eq!(rendered.dropped, vec!["long-tail"]);

    let candidates = record_candidates(&registry, &rendered);
    let drops = record_drops(&registry, &rendered);

    assert_eq!(candidates.len(), 1);
    let c = &candidates[0];
    assert_eq!(
        (c.source, c.handle.as_str()),
        (SteeringSource::Record, "staging-url")
    );
    assert!(c.est_tokens > 0, "a rendered record costs its bullet line");
    assert!(
        c.why.contains("^staging-url") && c.why.contains("precedence 40"),
        "the why names the handle and the declared rank: {}",
        c.why
    );
    assert_eq!(
        drops
            .iter()
            .map(|d| (d.source, d.handle.as_str()))
            .collect::<Vec<_>>(),
        vec![(SteeringSource::Record, "long-tail")],
        "the channel's named drops become the generalized ledger"
    );
}

/// The flat `score` sorts as the channel's own cut rank does. `force` comes
/// first. Then `precedence`.
#[test]
fn record_scores_preserve_the_channels_force_then_precedence_rank() {
    let registry = two_record_registry();
    let facts = TurnFacts {
        text: "anything",
        paths: &[],
    };
    let rendered = registry.render_volatile_for_turn(&facts, None);
    assert_eq!(rendered.rendered.len(), 2, "no budget: both render");

    let candidates = record_candidates(&registry, &rendered);
    let may = candidates
        .iter()
        .find(|c| c.handle == "staging-url")
        .unwrap();
    let info = candidates.iter().find(|c| c.handle == "long-tail").unwrap();
    assert!(
        may.score > info.score,
        "`may` precedence 40 outranks `info` precedence 0: {} vs {}",
        may.score,
        info.score
    );
}
