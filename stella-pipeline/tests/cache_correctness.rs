//! Cache-correctness drift gate over the committed golden trajectories.
//!
//! Prompt caching has no stale-read failure mode: a hit and a miss return the
//! same tokens, so nothing here can be caught by asserting on model output. A
//! cache defect is invisible until it shows up on an invoice. What *is*
//! checkable is that the prompt we assemble still has the shape caching needs,
//! and that is a property of the manifests the golden recordings already
//! carry — `make record-golden` writes a `cache_zone` per block, so the
//! fixtures are a standing record of where the breakpoints fell on the day
//! they were recorded.
//!
//! This gate reads that record and fails when the shape drifts. It is the
//! structural half of the correctness story in
//! `docs/design/session-telemetry-receipts-spec.md` §7 — the half that needs
//! no new events. The other half (reconciling predicted zone tokens against
//! the provider's reported `cached_input_tokens`, and naming the block that
//! broke the prefix) is `AgentEvent::CacheAttribution`, specced at §7 and
//! classified Tier A3, and is not built. When it lands, [`reported_cache_reads_never_exceed_reported_input`]
//! is where the reconciliation belongs.
//!
//! # What each invariant would catch
//!
//! - **Zone order** — a recalled frame inserted before the tail breakpoint
//!   shifts the boundary, and every block after it silently stops caching.
//!   The hit rate drops; nothing errors.
//! - **Prefix position/uniqueness** — `AgentEvent::StepManifest`'s own doc
//!   comment says "index 0 is the system prefix" (`stella-protocol/src/event.rs`).
//!   Nothing enforced it until now.
//! - **Prefix content address** — the loudest signal available. Block ids are
//!   content-addressed, so a timestamp, a session id, or a non-deterministic
//!   map iteration spliced into the system prefix fans one id out into many.
//!   That is the classic silent invalidator, and here it is a hard failure.
//!
//! # Honest limits
//!
//! The golden recordings are driven by scripted ports, which report a zeroed
//! usage envelope. [`reported_cache_reads_never_exceed_reported_input`] is
//! therefore **arithmetically vacuous today** — it passes because there are no
//! non-zero counts to violate it, not because caching was observed working. It
//! is committed anyway so the conservation law is asserted from the moment a
//! fixture gains a real envelope, rather than being remembered later. The
//! corpus guards below exist so the *structural* invariants cannot go vacuous
//! the same way without failing loudly.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use stella_pipeline::replay::parse_jsonl;
use stella_protocol::{AgentEvent, CacheZone, ManifestEntry};

/// One manifest lifted out of a fixture, tagged with where it came from so a
/// failure names the file and step rather than an anonymous index.
struct Manifest {
    fixture: String,
    turn_instance: u32,
    step: usize,
    call_seq: u64,
    blocks: Vec<ManifestEntry>,
}

impl Manifest {
    /// `fixture.jsonl turn 0 step 0 call_seq 2` — the coordinates a failure
    /// message needs to be actionable without opening the fixture.
    fn at(&self) -> String {
        format!(
            "{} turn {} step {} call_seq {}",
            self.fixture, self.turn_instance, self.step, self.call_seq
        )
    }
}

/// Rank a zone by its distance from the front of the prompt. Cache breakpoints
/// partition the prompt into a prefix that should hit every call, a middle that
/// can hit across calls, and a tail recomputed by construction — so a block's
/// zone is a monotonic function of its position, and this rank is what makes
/// "monotonic" checkable.
///
/// [`CacheZone::Other`] is deliberately unranked. It is the `serde(other)`
/// catch-all for a token this build does not recognize; in a fixture recorded
/// by *this* repo it can only mean the enum grew and this gate was not updated
/// with it, which must fail rather than sort to some arbitrary position.
fn zone_rank(zone: CacheZone) -> Option<u8> {
    match zone {
        CacheZone::StablePrefix => Some(0),
        CacheZone::Cacheable => Some(1),
        CacheZone::Volatile => Some(2),
        CacheZone::Other => None,
    }
}

fn zone_name(zone: CacheZone) -> &'static str {
    match zone {
        CacheZone::StablePrefix => "stable_prefix",
        CacheZone::Cacheable => "cacheable",
        CacheZone::Volatile => "volatile",
        CacheZone::Other => "other",
    }
}

fn golden_dir() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", "golden"]
        .iter()
        .collect()
}

/// Every `*.jsonl` under `tests/fixtures/golden/`, parsed. Sorted by filename so
/// a failure reports the same fixture first on every machine.
fn golden_events() -> Vec<(String, Vec<AgentEvent>)> {
    let dir = golden_dir();
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {dir:?}: {e}"))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    paths.sort();

    paths
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .expect("fixture has a filename")
                .to_string_lossy()
                .into_owned();
            let raw = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("reading fixture {path:?}: {e}"));
            let events = parse_jsonl(&raw)
                .unwrap_or_else(|e| panic!("fixture {name} must parse as a trajectory: {e:?}"));
            (name, events)
        })
        .collect()
}

fn manifests() -> Vec<Manifest> {
    golden_events()
        .into_iter()
        .flat_map(|(fixture, events)| {
            events.into_iter().filter_map({
                let fixture = fixture.clone();
                move |event| match event {
                    AgentEvent::StepManifest {
                        turn_instance,
                        step,
                        call_seq,
                        blocks,
                        ..
                    } => Some(Manifest {
                        fixture: fixture.clone(),
                        turn_instance,
                        step,
                        call_seq,
                        blocks,
                    }),
                    _ => None,
                }
            })
        })
        .collect()
}

/// Every fixture must carry at least one manifest with at least one block.
///
/// This is the anti-vacuity guard, and it is the first test on purpose. Every
/// other assertion in this file is a `for` loop over manifests: if the fixtures
/// were regenerated into a shape that emits no manifests — or manifests with no
/// blocks — those loops would iterate zero times and report green while proving
/// nothing. That is the exact failure mode this gate exists to prevent, so it
/// is checked before anything else rather than assumed.
///
/// The check is deliberately **per fixture, not corpus-wide**. A corpus-wide
/// sum was the first thing written here and it did not hold: mutation-testing
/// this gate (empty one fixture's `blocks`, keep the other's) passed green,
/// because one healthy fixture carried the total over zero while the other had
/// silently stopped contributing anything. Coverage that can be lost one
/// fixture at a time has to be asserted one fixture at a time.
#[test]
fn every_golden_fixture_carries_zoned_manifests_to_check() {
    let corpus = golden_events();
    assert!(
        !corpus.is_empty(),
        "no *.jsonl fixtures under {:?} — this gate has nothing to read",
        golden_dir(),
    );

    for (fixture, events) in corpus {
        let blocks: usize = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::StepManifest { blocks, .. } => Some(blocks.len()),
                _ => None,
            })
            .sum();
        assert!(
            blocks > 0,
            "{fixture} carries no zoned manifest blocks, so it contributes nothing \
             to this gate while still looking like coverage. Re-record with \
             `make record-golden`, or drop the fixture."
        );
    }
}

/// Blocks in wire order must never move *backwards* through the cache zones.
///
/// The zones are a partition of the prompt by breakpoint, so in wire order they
/// can only advance: stable prefix, then cacheable body, then the volatile tail.
/// A volatile block appearing before a cacheable one means something was
/// injected ahead of a breakpoint and pushed it — from that block on, the
/// prompt is recomputed every call. Nothing errors when this happens; the bill
/// just goes up, which is why it needs a gate rather than a runtime check.
///
/// Worth being precise about what the middle zone is, because it changes what a
/// pass here proves. `stella-core`'s assignment (`receipts.rs`) sets exactly two
/// zones explicitly — the tail to `Volatile`, then the head to `StablePrefix` if
/// it is the system prefix — and everything between them keeps `CacheZone`'s
/// derived default of `Cacheable`. So `Cacheable` is a **residual, not a
/// measurement**: it means "neither head nor tail", not "observed to have hit".
/// Monotonicity is therefore a property of that head/tail algorithm, and this
/// test is a regression gate on it — it catches a block appended after the tail
/// was stamped, or a reordering that moves the prefix off the head. It does not
/// and cannot establish that a middle block actually cached; that needs the
/// reported-vs-predicted reconciliation in §7.
#[test]
fn cache_zones_never_move_backwards_in_wire_order() {
    for manifest in manifests() {
        let mut high_water = 0u8;
        for (position, block) in manifest.blocks.iter().enumerate() {
            let rank = zone_rank(block.cache_zone).unwrap_or_else(|| {
                panic!(
                    "{}: block {} at wire position {position} has an unrecognized cache zone. \
                     CacheZone grew a variant and this gate's zone_rank was not updated.",
                    manifest.at(),
                    block.block_id,
                )
            });
            assert!(
                rank >= high_water,
                "{}: block {} at wire position {position} is zoned {} after a block \
                 already in a later zone. The breakpoint moved — every block from \
                 here on is recomputed each call.",
                manifest.at(),
                block.block_id,
                zone_name(block.cache_zone),
            );
            high_water = rank;
        }
    }
}

/// If a manifest has a stable prefix at all, it is the leading block and there
/// is exactly one of it.
///
/// `AgentEvent::StepManifest` documents `blocks` as "in wire order; index 0 is
/// the system prefix" — an invariant that was stated and never enforced. A
/// manifest with *no* stable prefix is legal and common: the pipeline's triage
/// and judge roles send a single message, and one message has nothing in front
/// of a breakpoint to cache. What is not legal is a stable prefix that is not
/// first (the bytes before it are uncacheable, so it is not a prefix) or a
/// second one (two disjoint spans cannot both be the prompt's head).
#[test]
fn the_stable_prefix_leads_its_manifest_and_is_unique() {
    for manifest in manifests() {
        let prefix_positions: Vec<usize> = manifest
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b.cache_zone == CacheZone::StablePrefix)
            .map(|(i, _)| i)
            .collect();

        match prefix_positions.as_slice() {
            // A single-message call has no prefix to cache. Legal.
            [] => {}
            [0] => {}
            [other] => panic!(
                "{}: the stable prefix sits at wire position {other}, not 0. \
                 Whatever precedes it is uncacheable, so it is not a prefix.",
                manifest.at(),
            ),
            many => panic!(
                "{}: {} blocks are zoned stable_prefix (wire positions {many:?}). \
                 The prompt has one head.",
                manifest.at(),
                many.len(),
            ),
        }
    }
}

/// Every stable-prefix block in the whole corpus shares one content address.
///
/// This is the sharpest instrument available here. Block ids are
/// content-addressed, so identical bytes produce an identical id — and the
/// system prefix is, by construction, the one span that must be byte-identical
/// on every call for caching to work at all. Across independent recordings of
/// different tasks it must therefore resolve to a single id.
///
/// A second id appearing is the signature of a silent invalidator: a timestamp,
/// a session or run id, a per-task detail, or a non-deterministic iteration
/// order that reached the prefix. That change costs a full-price prompt on
/// every call and produces no error anywhere — it is precisely the defect the
/// hit-rate threshold in `cache_economics.rs` can only notice after the fact,
/// from spend.
#[test]
fn every_stable_prefix_block_shares_one_content_address() {
    let mut ids: BTreeSet<String> = BTreeSet::new();
    let mut witnesses: Vec<String> = Vec::new();

    for manifest in manifests() {
        for block in &manifest.blocks {
            if block.cache_zone == CacheZone::StablePrefix && ids.insert(block.block_id.clone()) {
                witnesses.push(format!("{} -> {}", manifest.at(), block.block_id));
            }
        }
    }

    assert!(
        !ids.is_empty(),
        "no stable_prefix block anywhere in the golden corpus. Either the system \
         prefix stopped being zoned, or every recording is now single-message — \
         both make this gate blind."
    );
    assert_eq!(
        ids.len(),
        1,
        "the system prefix resolved to {} distinct content addresses, so it is not \
         byte-stable across calls. Something volatile reached the cached prefix. \
         First sighting of each:\n  {}",
        ids.len(),
        witnesses.join("\n  "),
    );
}

/// Reported cache reads never exceed the reported input they are drawn from.
///
/// The conservation law the pricing path already leans on: `cached_input_tokens`
/// is a *subset* of `input_tokens` (unlike `cache_write_tokens`, which is billed
/// on its own line and is deliberately not a subset — see the field's doc
/// comment in `stella-protocol/src/event.rs`). `Pricing::cache_savings_usd`
/// clamps rather than trusting this, so a violation would not crash; it would
/// quietly understate savings. Asserting it here means a provider adapter that
/// starts double-counting reads into the input total is caught at the fixture,
/// not in a receipt.
///
/// **Vacuous on today's corpus.** The golden recordings run against scripted
/// ports that report an all-zero envelope, so there is nothing here to violate
/// the inequality. It is committed now so the law is already asserted when a
/// fixture first carries real counts, and it is the natural home for the §7
/// predicted-vs-reported reconciliation once `CacheAttribution` exists.
#[test]
fn reported_cache_reads_never_exceed_reported_input() {
    for (fixture, events) in golden_events() {
        for event in events {
            let AgentEvent::StepUsage {
                step,
                input_tokens,
                cached_input_tokens,
                complete,
                ..
            } = event
            else {
                continue;
            };
            // An incomplete envelope is the provider declining to report, not a
            // claim about zero — see `CompletionUsage::reported`. Nothing to
            // conserve.
            if !complete {
                continue;
            }
            assert!(
                cached_input_tokens <= input_tokens,
                "{fixture} step {step}: {cached_input_tokens} cached tokens reported \
                 against {input_tokens} input tokens. Cache reads are a subset of \
                 input; this inflates the hit rate and understates cost."
            );
        }
    }
}
