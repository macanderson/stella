// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! When the ranking's memory guard is worth telling the caller about.
//!
//! A sibling of the ladder tests rather than part of them: `tests.rs` sits
//! against the 1500-line ceiling, and this is its own subject — not what a
//! search answers, but what it says about the shape of the answer.
//!
//! The guard itself cannot cost a better result: `stella_embed::rank::top_k`
//! scores the whole corpus and truncates last, so the rows past the ceiling
//! are the ones that scored lowest. What it *can* cost is a row the relevance
//! boundary would have kept, and only when that boundary kept everything it
//! was given — which is the distinction these tests pin.

use super::super::RANK_CEILING;
use super::super::{ceiling_note, merge_rungs};

/// The ranking's memory guard is disclosed on the one occasion it can have
/// cost an answer: the list came back full **and** the relevance boundary
/// kept all of it, so a 201st row would have been rendered had it existed.
#[test]
fn a_full_candidate_list_the_boundary_kept_earns_a_ceiling_note() {
    assert!(ceiling_note(RANK_CEILING, 0, true).is_some());
    assert!(ceiling_note(0, RANK_CEILING, true).is_some());
    assert_eq!(ceiling_note(5, 7, true), None);
}

/// Witness for #4385: the note fired on any full list, which on this
/// repository is effectively every query — 60 of 61 `search` calls in one
/// session, after which the model mined a lesson telling itself to use `rg`
/// instead. A caveat that fires on every answer is not a caveat.
///
/// When the relevance boundary cuts before the end, the guard changed
/// nothing: the answer would be identical with a ceiling of a hundred
/// thousand, so there is nothing to disclose.
#[test]
fn a_full_list_the_boundary_cut_early_earns_no_ceiling_note() {
    assert_eq!(ceiling_note(RANK_CEILING, 0, false), None);
    assert_eq!(ceiling_note(RANK_CEILING, RANK_CEILING, false), None);
}

/// The other half of the same claim, at the seam that produces it: a ranking
/// with an obvious relevance edge reports the boundary as having cut early,
/// and one with no edge at all reports that it kept everything.
#[test]
fn the_merge_reports_whether_the_boundary_ran_to_the_end() {
    let files: Vec<stella_embed::rank::Scored> = [0.90f32, 0.89, 0.40, 0.39]
        .iter()
        .enumerate()
        .map(|(index, score)| stella_embed::rank::Scored {
            key: format!("src/file{index}.rs"),
            score: *score,
        })
        .collect();
    let (hits, ran_to_end) = merge_rungs(&[], &files);
    assert!(
        !ran_to_end,
        "a 0.49 gap against a 0.17 mean is an edge, so the boundary must cut before the end"
    );
    assert_eq!(hits.len(), 2, "and the answer is the prefix: {hits:?}");

    let flat: Vec<stella_embed::rank::Scored> = (0..4)
        .map(|index| stella_embed::rank::Scored {
            key: format!("src/flat{index}.rs"),
            score: 0.7,
        })
        .collect();
    let (hits, ran_to_end) = merge_rungs(&[], &flat);
    assert!(
        ran_to_end,
        "no gap anywhere means no edge, so the boundary keeps the whole list"
    );
    assert_eq!(hits.len(), 4);
}
