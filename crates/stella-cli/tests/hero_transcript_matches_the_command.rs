// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The landing page's proof block shows a transcript the command above it can
//! actually produce.
//!
//! `website/src/components/command-deck.tsx` renders the first thing a visitor
//! reads, and its own doc comment promises the content is "faithful, not
//! fabricated". It had been showing a plain `stella run` printing
//! `triage → plan → witness → execute → verify → verdict` and ending on
//! `✓ verified`. A plain `stella run` is the raw step loop: it emits no
//! `Stage` event, so `plain::stage_rule` never fires, and it reaches no
//! verdict. The staged pipeline that produced that flow was deleted (#3865),
//! and `stella run --pipeline classic` is refused outright by
//! `wrapper_plugin::PipelineChoice::resolve` (#4761).
//!
//! So the rule this test enforces is the issue's own: staged vocabulary may
//! appear in a transcript **only** beside a command that produces it — today
//! `stella run --pipeline <plugin-id>` against an installed wrapper plugin.
//! Prose about the pipeline is unaffected; this reads the transcript rows,
//! which are the string literals inside the JSX.
//!
//! Why a test rather than a gate step: `make gate` already runs `make test`,
//! and a gate step is five coupled edits plus another shared cell for two
//! concurrent PRs to collide on — the argument `design_token_parity.rs` makes
//! at length, and this file follows it, including living in `stella-cli`
//! because that crate has no `lib.rs` and this reads the component as text.
//! The path is declared `read` in `scripts/website-rust-inputs.txt`, so a
//! website-only diff that edits it still runs the Rust gate (#4632).

use std::path::{Path, PathBuf};

/// The component holding every landing-page terminal transcript.
fn component() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../website/src/components/command-deck.tsx")
        .canonicalize()
        .expect("the landing page's terminal component resolves from CARGO_MANIFEST_DIR")
}

/// Stage names the raw step loop never prints, in the arrow form the deleted
/// staged pipeline used. Matched case-sensitively and with the arrow, so the
/// word "triage" in ordinary prose is not a hit.
const STAGED_FLOW_MARKERS: &[&str] = &[
    "triage \u{2192} plan",
    "\u{2192} witness \u{2192}",
    "\u{2192} verify \u{2192} verdict",
];

/// The verdict wording only a verification path reaches.
const VERDICT_MARKERS: &[&str] = &["\u{2713} verified", "\u{2717} unverified"];

/// The command that legitimises either marker: a run handed to an installed
/// wrapper plugin.
const WRAPPED_COMMAND: &str = "--pipeline";

#[test]
fn the_hero_transcript_claims_no_stage_the_raw_loop_cannot_print() {
    let path = component();
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let wrapped = source.contains(WRAPPED_COMMAND);

    for marker in STAGED_FLOW_MARKERS.iter().chain(VERDICT_MARKERS) {
        assert!(
            !source.contains(marker) || wrapped,
            "{} shows `{marker}`, which only a wrapper plugin's run produces, \
             with no `{WRAPPED_COMMAND}` command anywhere in the file. A plain \
             `stella run` is the raw step loop: no stage line, no verdict. \
             Either show what the raw loop prints, or show the command that \
             produces this (#4761).",
            path.display()
        );
    }
}
