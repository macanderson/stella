// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The name rung's eval: what `search` answers a prose question with when
//! **no embedder is configured** (#3138).
//!
//! # Why this exists as an eval and not as an example
//!
//! Ranking quality is not a property any single assertion can hold. A change
//! that lifts one query and drops three others looks identical, from inside
//! the diff, to a change that lifts all four — which is how the defect this
//! file pins got shipped in the first place. So the unit of measurement here
//! is a *probe set*: a fixed corpus, a fixed list of questions, and for each
//! one the file that is genuinely its answer, verified by reading that file
//! before it was pinned.
//!
//! # The corpus
//!
//! `fixtures/name_rung_corpus.jsonl` is this repository's own code-graph
//! index — every indexed file's path and symbol names, 1,761 files — frozen
//! as a dataset, as the tree stood on the day it was harvested. It therefore
//! still carries `crates/stella-pipeline/**` rows, from before that crate was
//! deleted from the workspace (#3865); that is the snapshot working as
//! intended, not drift to repair. It is deliberately **not** re-harvested on
//! every run:
//!
//! - a probe measures the *ranker*, so its corpus must not move underneath
//!   it, or a rank that changed says nothing about the change that caused it;
//! - the before/after numbers below are measured against this exact snapshot,
//!   so they compare like for like (a corpus containing the fix's own new
//!   files would have confounded them);
//! - and it makes the whole eval hermetic: no index build, no tree-sitter, no
//!   network, no key, and no dependence on what the working tree happens to
//!   contain today.
//!
//! So the corpus is a **snapshot of a past tree, not a description of this
//! one**, and it is expected to name files the workspace no longer has — it
//! carries `crates/stella-pipeline/…` rows, for instance, from before #3865
//! deleted that crate. Those are data, not stale citations: re-harvesting to
//! remove them would invalidate every recorded number in the table below,
//! which is the one thing this fixture exists to prevent.
//!
//! The harvest excludes the work-in-flight design scratchpad, because a
//! corpus carries every indexed path as data and `check-design-refs` reads
//! any mention of one as a citation. Dropping those six rows moved exactly
//! one recorded rank, and it moved the right way (63 -> 53); the table
//! below is re-recorded against the 1,761-row corpus that ships here.
//!
//! One further row was removed for a different reason: the first harvest
//! ran in a working tree that still held the authoring session's own
//! scratch probe, so the "frozen snapshot of this repository" contained a
//! file that is in no commit. A corpus that includes an artifact of the
//! session that produced it is not the thing it claims to be, however
//! little it moves a number — and here it moved none of them.
//!
//! Re-harvest deliberately, never incidentally, with
//! `HARVEST=1 cargo test -p stella-tools --test search_recall -- --ignored`,
//! and re-record every number in the table below in the same commit — a
//! corpus swap and a ranker change in one diff are two experiments wearing
//! one result.
//!
//! # Measured, on the corpus above
//!
//! `before` is the substring ranker this replaced (`main` at the time this
//! file was written); `after` is [`stella_tools::search::names::rank`]. Both
//! were measured against this snapshot. It is data, not a claim the test can
//! re-derive — the old ranker no longer exists to run.
//!
//! | probe | before | after |
//! |---|---|---|
//! | how a provider recovers when its streaming path is broken | 277 | 5 |
//! | which telemetry columns may be exported off the machine | 16 | **22** |
//! | which columns an encoder is allowed to put in a telemetry drain | 1 | 1 |
//! | detecting that the agent is repeating the same tool call | 133 | 3 |
//! | compacting the conversation when the context window fills up | absent | 9 |
//! | retry backoff jitter between workers | 1 | 1 |
//! | resolving where the stella home directory lives | 46 | 1 |
//! | deciding whether a human is present to answer a prompt | 26 | **53** |
//! | which files the workspace ignores when walking the tree | 44 | 1 |
//! | the ledger of fleet runs attempts and spend | 1 | 1 |
//! | producing a unified diff with hunk headers | 1 | 2 |
//!
//! Top-5 recall 4/11 → 8/11; mean reciprocal rank 0.378 → 0.564. **Two
//! probes got worse and are bolded**, both of them files the rung was never
//! reaching anyway — see [`PROBES`] for what each one costs and why it is
//! recorded rather than tuned away.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use stella_tools::search::names::{self, IndexedNames};

/// One question, the file that answers it, and the worst rank that answer is
/// allowed to hold.
struct Probe {
    /// The question, phrased the way someone would actually ask it.
    query: &'static str,
    /// The file that answers it. Every one of these was read before it was
    /// pinned; `why` records what makes it the answer.
    target: &'static str,
    /// Why `target` is the answer — the evidence, so a later reader can
    /// re-check the pin instead of trusting it.
    why: &'static str,
    /// The rank measured when this probe was recorded. A **down-only
    /// ratchet**: the test fails if the target ranks worse, and reports (but
    /// passes) if it ranks better, so an improvement is visible and a
    /// regression is not survivable.
    ceiling: usize,
}

/// The probe set.
///
/// Two of these are recorded misses rather than targets the ranking meets,
/// and both are recorded on purpose — a probe set that only contains
/// questions the mechanism answers well is a demo, not an eval:
///
/// - *"which telemetry columns may be exported off the machine"* (22). The
///   phrasing is genuinely ambiguous: `enterprise_telemetry/export_ledger.rs`
///   ranks first and is a defensible answer to it. The unambiguous phrasing
///   of the same question is the probe below it, which ranks the intended
///   file first. Its `before` rank of 16 was tie-luck — it scored 2 of 6
///   terms alongside dozens of other files and won the alphabetical
///   tiebreak — so 16 and 22 are the same "not found" wearing two numbers.
/// - *"deciding whether a human is present to answer a prompt"* (53). The
///   answer is `crates/stella-tty/src/lib.rs`, whose file name says nothing
///   and which holds exactly two symbols. A rung that ranks names has no
///   signal to work with here, and no weighting of names can invent one;
///   this is the query shape that needs the semantic rung (#2994), and the
///   probe is kept so that stays measured rather than assumed.
const PROBES: &[Probe] = &[
    Probe {
        query: "how a provider recovers when its streaming path is broken",
        target: "crates/stella-model/src/provider_parity.rs",
        why: "declares `StreamFallbackPosture` — the axis AGENTS.md invariant 8 \
              names for exactly this question",
        ceiling: 5,
    },
    Probe {
        query: "which telemetry columns may be exported off the machine",
        target: "crates/stella-store/src/content_free.rs",
        why: "holds the reviewed allowlist of hub telemetry columns \
              (`live_hub_telemetry_columns`, AGENTS.md invariant 3)",
        ceiling: 22,
    },
    Probe {
        query: "which columns an encoder is allowed to put in a telemetry drain",
        target: "crates/stella-store/src/content_free.rs",
        why: "same file, asked unambiguously: `ContentFreeEncoder::allowed_keys` \
              and `drain_format` are the answer",
        ceiling: 1,
    },
    Probe {
        query: "detecting that the agent is repeating the same tool call",
        target: "crates/stella-core/src/loop_detect.rs",
        why: "the engine's loop detector — the only non-test file that decides \
              a repeat is a loop",
        ceiling: 3,
    },
    Probe {
        query: "compacting the conversation when the context window fills up",
        target: "crates/stella-core/src/compaction.rs",
        why: "the engine's compaction decision (AGENTS.md routes compaction to \
              stella-core)",
        ceiling: 9,
    },
    Probe {
        query: "retry backoff jitter between workers",
        target: "crates/stella-core/src/retry.rs",
        why: "the retry policy, including the decorrelation that stops workers \
              waking in lockstep",
        ceiling: 1,
    },
    Probe {
        query: "resolving where the stella home directory lives",
        target: "crates/stella-home/src/lib.rs",
        why: "the single implementation of `~/.stella` resolution — the whole \
              crate is one file",
        ceiling: 1,
    },
    Probe {
        query: "deciding whether a human is present to answer a prompt",
        target: "crates/stella-tty/src/lib.rs",
        why: "`human_can_answer` — the one derivation both the CLI's approval \
              prompt and the credential prompt share",
        ceiling: 53,
    },
    Probe {
        query: "which files the workspace ignores when walking the tree",
        target: "crates/stella-graph/src/workspace_ignore.rs",
        why: "`WorkspaceIgnore::excludes` — the indexer's ignore resolution",
        ceiling: 1,
    },
    Probe {
        query: "the ledger of fleet runs attempts and spend",
        target: "crates/stella-fleet/src/ledger.rs",
        why: "the fleet run/attempt/commit/spend ledger named in AGENTS.md's \
              glossary",
        ceiling: 1,
    },
    Probe {
        query: "producing a unified diff with hunk headers",
        target: "crates/stella-diff/src/lib.rs",
        why: "defines `unified_diff` and `Hunk::header`",
        ceiling: 2,
    },
];

/// Probes whose target must reach the top of the answer, not merely beat its
/// recorded rank. This is #3138's definition of done, stated where it is
/// enforced rather than in a commit message.
const TOP_FIVE_REQUIRED: &[&str] = &["how a provider recovers when its streaming path is broken"];

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/name_rung_corpus.jsonl")
}

/// The frozen corpus, in the fixture's own order.
fn corpus() -> Vec<IndexedNames> {
    let raw = std::fs::read_to_string(fixture_path()).expect("the probe corpus fixture");
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let row: serde_json::Value = serde_json::from_str(line).expect("a corpus row");
            IndexedNames {
                path: row["path"].as_str().expect("a path").to_string(),
                symbols: row["symbols"]
                    .as_array()
                    .expect("a symbol list")
                    .iter()
                    .map(|symbol| symbol.as_str().expect("a symbol name").to_string())
                    .collect(),
            }
        })
        .collect()
}

/// Where `target` ranks for `query`, 1-based, or `None` if it does not appear
/// in the answer at all.
fn rank_of(corpus: &[IndexedNames], query: &str, target: &str) -> Option<usize> {
    names::rank(corpus, query)
        .iter()
        .position(|hit| hit.path == target)
        .map(|index| index + 1)
}

/// The fixture must be the thing the probes think it is. A corpus that
/// silently lost half its files would make every ceiling below pass for the
/// wrong reason.
#[test]
fn the_probe_corpus_is_the_snapshot_the_numbers_were_measured_against() {
    let corpus = corpus();
    assert_eq!(
        corpus.len(),
        1761,
        "the frozen corpus changed size — re-record the probe table in the same commit"
    );
    let paths: BTreeSet<&str> = corpus.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths.len(), corpus.len(), "the corpus has duplicate paths");
    for probe in PROBES {
        assert!(
            paths.contains(probe.target),
            "probe target `{}` is not in the corpus, so its rank means nothing",
            probe.target
        );
    }
}

/// **The eval.** Every probe's answer must rank at least as well as it did
/// when the probe was recorded.
#[test]
fn every_probe_answer_holds_its_recorded_rank() {
    let corpus = corpus();
    let mut failures = Vec::new();
    let mut improvements = Vec::new();
    let mut in_top_five = 0;

    for probe in PROBES {
        let rank = rank_of(&corpus, probe.query, probe.target);
        println!(
            "{:>4}  {}\n      → {} ({})",
            rank.map_or_else(|| "—".to_string(), |rank| rank.to_string()),
            probe.query,
            probe.target,
            probe.why
        );
        match rank {
            Some(rank) if rank <= probe.ceiling => {
                if rank < probe.ceiling {
                    improvements.push(format!(
                        "`{}`: {} → {} (tighten the ceiling)",
                        probe.query, probe.ceiling, rank
                    ));
                }
                if rank <= 5 {
                    in_top_five += 1;
                }
            }
            Some(rank) => failures.push(format!(
                "`{}`: {} ranked {rank}, worse than the recorded {}",
                probe.query, probe.target, probe.ceiling
            )),
            None => failures.push(format!(
                "`{}`: {} fell out of the answer entirely",
                probe.query, probe.target
            )),
        }
    }

    for improvement in &improvements {
        println!("IMPROVED {improvement}");
    }
    assert!(
        failures.is_empty(),
        "the name rung got worse on {} of {} probes:\n  {}",
        failures.len(),
        PROBES.len(),
        failures.join("\n  ")
    );
    assert!(
        in_top_five >= 8,
        "top-5 recall fell to {in_top_five} of {}; it was 8 when this was recorded",
        PROBES.len()
    );
}

/// **The witness for #3138.** The measured repro query ranks the file that
/// answers it inside the top five, with no embedder anywhere in sight.
///
/// On the ranker this replaced it ranked 277th, behind
/// ten bench and arenabench Python modules that matched `provider`, `recovers`
/// and `path` as incidental substrings of unrelated test-function names.
#[test]
fn the_repro_query_ranks_its_real_answer_in_the_top_five() {
    let corpus = corpus();
    for probe in PROBES
        .iter()
        .filter(|probe| TOP_FIVE_REQUIRED.contains(&probe.query))
    {
        let ranked = names::rank(&corpus, probe.query);
        let top: Vec<&str> = ranked.iter().take(5).map(|hit| hit.path.as_str()).collect();
        assert!(
            top.contains(&probe.target),
            "`{}` must rank {} in the top five; got {top:?}",
            probe.query,
            probe.target
        );
    }
}

/// The bench module that used to **win** the repro query outright is now
/// rank 12, and must not climb back.
///
/// Named explicitly rather than left implicit in the rank above: the top five
/// could be right while the 100-symbol test module still sat at six, and that
/// would mean the symbol-count bias had been crowded out rather than fixed.
/// It is still in the answer, and honestly so — it does name `provider`,
/// `path` and `recovers` — but at a rank that reflects what those mentions
/// are worth rather than how many of them there are.
#[test]
fn the_symbol_count_decoy_no_longer_places_in_the_repro_query() {
    let corpus = corpus();
    const DECOY: &str = "bench/terminal_bench_analysis/tests/test_tb21_analysis.py";
    let ranked = names::rank(&corpus, PROBES[0].query);
    let decoy = ranked
        .iter()
        .position(|hit| hit.path == DECOY)
        .map(|index| index + 1);
    /// Where the decoy sits today. A ratchet in the opposite direction to
    /// [`Probe::ceiling`]: the decoy may sink further, never rise.
    const DECOY_FLOOR: usize = 12;
    assert!(
        decoy.is_none_or(|rank| rank >= DECOY_FLOOR),
        "the 100-symbol decoy climbed to rank {decoy:?}; it was rank 1 before #3138 and \
         {DECOY_FLOOR} when this was recorded"
    );
}

/// Invariant 7, on the real corpus rather than a three-file fixture: the same
/// query ranks byte-identically twice, and reversing the corpus's order does
/// not move a single hit.
#[test]
fn the_ranking_is_byte_stable_across_runs_and_corpus_order() {
    let corpus = corpus();
    let query = PROBES[0].query;
    let once = names::rank(&corpus, query);
    assert_eq!(once, names::rank(&corpus, query));

    let mut reversed = corpus;
    reversed.reverse();
    assert_eq!(once, names::rank(&reversed, query));
}

/// Re-harvest `fixtures/name_rung_corpus.jsonl` from this repository's live
/// code-graph index. Ignored, and additionally gated on `HARVEST=1`, because
/// it indexes the whole workspace (tens of seconds) and rewrites a committed
/// dataset.
///
/// Swapping the corpus invalidates every number in this file's table — record
/// the new ones in the same commit, and do not fold the swap into a diff that
/// also changes the ranker.
#[test]
#[ignore = "rewrites the committed corpus; run deliberately with HARVEST=1"]
fn harvest_the_probe_corpus_from_this_repository() {
    if std::env::var("HARVEST").as_deref() != Ok("1") {
        println!("set HARVEST=1 to rewrite {}", fixture_path().display());
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root");
    let database = tempfile::tempdir().expect("tempdir");
    let graph = stella_graph::CodeGraph::open(&root, &database.path().join("codegraph.db"))
        .expect("open the index");
    graph.index_all().expect("index the workspace");

    let mut rows: Vec<String> = graph
        .all_files()
        .expect("the indexed file list")
        .into_iter()
        // The work-in-flight design scratchpad is excluded. It must not be
        // named by anything outside itself (`check-design-refs`), and a
        // harvested corpus carries every indexed path as data -- which that
        // guard reads as a citation, because it greps source text and cannot
        // tell a dataset row from a reference. Widening the guard to carve out
        // a fixture would trade a real invariant for one file's convenience,
        // so the rows go instead: no probe targets a document there, and the
        // eval measures the same thing without them.
        //
        // Matched structurally rather than by a path literal, so this line
        // does not become the citation it is excluding.
        .filter(|path| {
            let mut parts = path.split('/');
            !(parts.next() == Some("docs") && parts.next() == Some("design"))
        })
        .map(|path| {
            let symbols: Vec<String> = graph
                .file_neighborhood(Path::new(&path))
                .map(|neighborhood| {
                    neighborhood
                        .symbols
                        .iter()
                        .map(|symbol| symbol.name.clone())
                        .collect()
                })
                .unwrap_or_default();
            serde_json::json!({ "path": path, "symbols": symbols }).to_string()
        })
        .collect();
    graph.shutdown();
    rows.sort();
    std::fs::write(fixture_path(), rows.join("\n") + "\n").expect("write the corpus");
    println!("harvested {} files", rows.len());
}
