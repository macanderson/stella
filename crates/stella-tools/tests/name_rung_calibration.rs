// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What the name rung's four weights are worth, measured.
//!
//! Four numbers set the score a file gets for matching a query by name. None
//! of them came from a measurement. This file is the measurement.
//!
//! # How it reads
//!
//! `fixtures/name_rung_queries.jsonl` holds 32 prose questions. Each one
//! names the single file that answers it. Each answer was picked by reading
//! that file, never by looking at where it ranked.
//!
//! The corpus is `fixtures/name_rung_corpus.jsonl`, a frozen sample of this
//! repository's own index. `search_recall.rs` explains why it stays frozen.
//!
//! Runs are scored two ways. `p@1` counts the questions whose answer ranks
//! first. `p@3` counts the ones whose answer lands in the top three.
//!
//! # What the sweep found
//!
//! The shipped weights score 10 and 16 out of 32. The best of all 480
//! settings scores 13 and 17. That gap is four wins against one loss, which
//! a paired sign test reads as p = 0.375, so the corpus cannot tell the two
//! apart.
//!
//! The hold-out says the same thing more plainly. Pick the best setting on
//! half the questions, then score it on the other half. In three of the four
//! arms it does worse than the shipped one. The grid's peaks are noise.
//!
//! So the four values stay, and this eval is the record of why. Each one's
//! own `MEASURED:` marker in `crates/stella-tools/src/search/names.rs`
//! carries the row of the sweep that read it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use stella_tools::search::names::{self, IndexedNames, Weights};

/// One question, the file that answers it, and why that file is the answer.
struct Labelled {
    /// The question, phrased the way someone would ask it.
    query: String,
    /// The file that answers it.
    answer: String,
    /// What makes it the answer, so a later reader can re-check the label.
    why: String,
}

/// Questions whose answer ranks first, at the shipped weights.
const RECORDED_TOP_HIT: usize = 10;

/// Questions whose answer lands in the top three, at the shipped weights.
const RECORDED_TOP_THREE: usize = 16;

/// The floor a hit would have to clear to be shown, in matched query terms.
///
/// One is the shipped rule, and it admits every file that matched anything.
const FLOOR_UNDER_TEST: usize = 2;

/// Labelled answers a floor of [`FLOOR_UNDER_TEST`] terms drops from the
/// result entirely.
const ANSWERS_CUT_BY_THE_FLOOR: usize = 6;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// The frozen corpus. The fixture's first line is a header with no `path`
/// key, so a row without one is skipped.
fn corpus() -> Vec<IndexedNames> {
    let raw =
        std::fs::read_to_string(fixtures().join("name_rung_corpus.jsonl")).expect("the corpus");
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let row: serde_json::Value = serde_json::from_str(line).expect("a corpus row");
            let path = row.get("path")?.as_str().expect("a path").to_string();
            Some(IndexedNames {
                path,
                symbols: row["symbols"]
                    .as_array()
                    .expect("a symbol list")
                    .iter()
                    .map(|symbol| symbol.as_str().expect("a symbol name").to_string())
                    .collect(),
            })
        })
        .collect()
}

/// The labelled questions, in the fixture's own order.
fn labelled() -> Vec<Labelled> {
    let raw =
        std::fs::read_to_string(fixtures().join("name_rung_queries.jsonl")).expect("the queries");
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let row: serde_json::Value = serde_json::from_str(line).expect("a query row");
            Labelled {
                query: row["query"].as_str().expect("a query").to_string(),
                answer: row["answer"].as_str().expect("an answer").to_string(),
                why: row["why"].as_str().expect("a reason").to_string(),
            }
        })
        .collect()
}

/// Where `answer` ranks for `query` under `weights`, counting from one.
fn rank_of(corpus: &[IndexedNames], query: &str, answer: &str, weights: Weights) -> Option<usize> {
    names::rank_with(corpus, query, weights)
        .iter()
        .position(|hit| hit.path == answer)
        .map(|index| index + 1)
}

/// How many questions the given weights place at rank one, and in the top
/// three.
fn score(corpus: &[IndexedNames], set: &[Labelled], weights: Weights) -> (usize, usize) {
    let mut top_hit = 0;
    let mut top_three = 0;
    for item in set {
        if let Some(rank) = rank_of(corpus, &item.query, &item.answer, weights) {
            top_hit += usize::from(rank == 1);
            top_three += usize::from(rank <= 3);
        }
    }
    (top_hit, top_three)
}

/// Every labelled answer has to be in the corpus, or its rank means nothing.
#[test]
fn every_labelled_answer_is_a_file_the_corpus_holds() {
    let corpus = corpus();
    let set = labelled();
    assert!(
        set.len() >= 20,
        "the labelled set shrank to {}; a grid needs a set it cannot fit by luck",
        set.len()
    );
    let paths: BTreeSet<&str> = corpus.iter().map(|file| file.path.as_str()).collect();
    for item in &set {
        assert!(
            paths.contains(item.answer.as_str()),
            "labelled answer `{}` is not in the corpus ({})",
            item.answer,
            item.why
        );
        assert!(
            !item.why.trim().is_empty(),
            "`{}` carries no reason for its answer",
            item.query
        );
    }
}

/// **The pin.** The shipped weights are the four constants, and they score
/// what the sweep recorded.
///
/// A merge that moves one of the four fails here, which is the whole point:
/// a value set by a measurement can otherwise be reverted in silence.
#[test]
fn the_shipped_weights_score_what_the_sweep_recorded() {
    assert_eq!(
        Weights::SHIPPED,
        Weights {
            basename: 3,
            directory: 1,
            min_prefix_stem: 4,
            max_prefix_tail: 3,
        },
        "a shipped weight moved; re-run the sweep and re-record every number \
         in this file and in the markers in search/names.rs"
    );

    let corpus = corpus();
    let set = labelled();
    for item in &set {
        let rank = rank_of(&corpus, &item.query, &item.answer, Weights::SHIPPED);
        println!(
            "{:>5}  {}\n       -> {} ({})",
            rank.map_or_else(|| "-".to_string(), |rank| rank.to_string()),
            item.query,
            item.answer,
            item.why
        );
    }

    let (top_hit, top_three) = score(&corpus, &set, Weights::SHIPPED);
    println!("p@1 {top_hit}/{}  p@3 {top_three}/{}", set.len(), set.len());
    assert_eq!(
        (top_hit, top_three),
        (RECORDED_TOP_HIT, RECORDED_TOP_THREE),
        "the shipped ranking moved; re-record it here in the same commit"
    );
}

/// **The floor.** The rung shows every file that matched one query term, and
/// a floor over that count would cost more than it buys.
///
/// This is the question the issue asked: the safety floor it named lives in
/// `stella-embed` and reads cosine scores, which this rung has none of. The
/// count of matched terms is the only floor this rung could have. At two
/// terms it drops six of the 32 answers out of the result, and the ranking
/// gains one top hit and no top-three answers. Six answers hidden to move
/// one is the trade, so the rung keeps no floor.
#[test]
fn a_floor_on_matched_terms_would_hide_answers_and_buy_almost_nothing() {
    let corpus = corpus();
    let set = labelled();
    let mut cut = 0;
    let mut top_hit = 0;
    let mut top_three = 0;

    for item in &set {
        let ranked = names::rank_with(&corpus, &item.query, Weights::SHIPPED);
        let answer = ranked.iter().find(|hit| hit.path == item.answer);
        if answer.is_some_and(|hit| hit.matched_terms < FLOOR_UNDER_TEST) {
            cut += 1;
        }
        let kept: Vec<&str> = ranked
            .iter()
            .filter(|hit| hit.matched_terms >= FLOOR_UNDER_TEST)
            .map(|hit| hit.path.as_str())
            .collect();
        if let Some(rank) = kept.iter().position(|path| *path == item.answer) {
            top_hit += usize::from(rank == 0);
            top_three += usize::from(rank < 3);
        }
    }

    println!(
        "floor of {FLOOR_UNDER_TEST} terms: p@1 {top_hit}, p@3 {top_three}, \
         answers cut {cut} of {}",
        set.len()
    );
    assert_eq!(
        cut, ANSWERS_CUT_BY_THE_FLOOR,
        "the cost of the floor moved; re-record it"
    );
    assert_eq!(
        top_hit,
        RECORDED_TOP_HIT + 1,
        "the floor's gain moved; re-record it"
    );
    assert_eq!(
        top_three, RECORDED_TOP_THREE,
        "a floor of {FLOOR_UNDER_TEST} terms bought a top-three answer; \
         re-open the question of whether the rung should have one"
    );
}

/// **The sweep.** Every setting of the four weights, scored on the labelled
/// set.
///
/// Ignored because it ranks the whole corpus 15,360 times. Run it with
/// `cargo test --release -p stella-tools --test name_rung_calibration --
/// --ignored --nocapture` and read the table it prints. Its assertions are
/// the numbers the markers in `search/names.rs` record.
#[test]
#[ignore = "walks 480 settings over the whole corpus; run it on purpose"]
fn the_grid_cannot_separate_the_shipped_weights_from_their_neighbours() {
    let corpus = corpus();
    let set = labelled();

    println!("one axis at a time, the other three at the shipped values");
    for basename in 1..=6 {
        let weights = Weights {
            basename,
            ..Weights::SHIPPED
        };
        let (top_hit, top_three) = score(&corpus, &set, weights);
        println!("  basename={basename}: p@1 {top_hit}  p@3 {top_three}");
    }
    for directory in 0..=3 {
        let weights = Weights {
            directory,
            ..Weights::SHIPPED
        };
        let (top_hit, top_three) = score(&corpus, &set, weights);
        println!("  directory={directory}: p@1 {top_hit}  p@3 {top_three}");
    }
    for min_prefix_stem in 3..=6 {
        let weights = Weights {
            min_prefix_stem,
            ..Weights::SHIPPED
        };
        let (top_hit, top_three) = score(&corpus, &set, weights);
        println!("  min_prefix_stem={min_prefix_stem}: p@1 {top_hit}  p@3 {top_three}");
    }
    for max_prefix_tail in 1..=5 {
        let weights = Weights {
            max_prefix_tail,
            ..Weights::SHIPPED
        };
        let (top_hit, top_three) = score(&corpus, &set, weights);
        println!("  max_prefix_tail={max_prefix_tail}: p@1 {top_hit}  p@3 {top_three}");
    }

    let mut best_top_hit = 0;
    let mut best_top_three = 0;
    for basename in 1..=6 {
        for directory in 0..=3 {
            for min_prefix_stem in 3..=6 {
                for max_prefix_tail in 1..=5 {
                    let weights = Weights {
                        basename,
                        directory,
                        min_prefix_stem,
                        max_prefix_tail,
                    };
                    let (top_hit, top_three) = score(&corpus, &set, weights);
                    best_top_hit = best_top_hit.max(top_hit);
                    best_top_three = best_top_three.max(top_three);
                }
            }
        }
    }

    println!("best over the grid: p@1 {best_top_hit}  p@3 {best_top_three}");
    assert_eq!(
        (best_top_hit, best_top_three),
        (13, 17),
        "the grid's best moved; re-record it here and in the markers"
    );
    let (top_hit, top_three) = score(&corpus, &set, Weights::SHIPPED);
    assert!(
        best_top_hit - top_hit <= 3 && best_top_three - top_three <= 1,
        "a setting now beats the shipped one by more than the corpus can \
         resolve; measure the hold-out again before moving a constant"
    );
}
