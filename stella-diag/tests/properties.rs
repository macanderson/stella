// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The properties `docs/design/diagnostics.md` §12 asks for, as properties
//! rather than as examples.
//!
//! | # | Property | Here |
//! |---|---|---|
//! | 3 | Filter never gates emission | [`the_filter_governs_sinks_and_nothing_else`] |
//! | 4 | A failing sink never fails a caller | [`a_sink_that_always_fails_changes_nothing_but_its_own_counter`] |
//! | 8 | Envelope round-trips | [`every_record_round_trips_byte_for_byte`] |
//!
//! Properties 1 (content cannot enter a record) and 5 (the ring survives a
//! panic) live where they can be observed: `tests/compile_fail.rs` and
//! `src/crash.rs` respectively.
//!
//! The generators below build records only from things a real emit site could
//! build them from — `&'static str` codes and targets out of a fixed pool,
//! [`Loggable`] field values — because a strategy that could produce a record
//! containing a `String` would be testing a type this crate does not admit.

use std::sync::Arc;

use proptest::prelude::*;
use stella_diag::{
    Bound, Capture, Cx, Dx, FailingSink, FieldValue, Fields, Filter, Level, LevelFilter, Record,
    Ring, ShortId, Sink, StaticStr,
};

const CODES: [&str; 6] = [
    "store.migration.retry",
    "model.request.retried",
    "tools.write.denied",
    "agent.tool.call",
    "cli.boot",
    "diag.filter.bad_clause",
];

const TARGETS: [&str; 5] = [
    "stella_store::migrate",
    "stella_model::anthropic",
    "stella_tools::write",
    "stella_cli::agent",
    "stella_serve",
];

const KEYS: [&str; 5] = ["attempt", "backoff_ms", "tool", "ok", "elapsed"];

fn level() -> impl Strategy<Value = Level> {
    prop::sample::select(Level::ALL.to_vec())
}

fn field_value() -> impl Strategy<Value = FieldValue> {
    prop_oneof![
        // Through `FieldValue::int`, not the variant, because that is the only
        // way an emit site can build a signed field — and it is where the
        // Int/Uint normalisation that makes the round trip exact happens.
        any::<i64>().prop_map(FieldValue::int),
        any::<u64>().prop_map(FieldValue::Uint),
        // Floats a diagnostic actually carries: a duration, a ratio, a rate —
        // three decimal places over a wide range, not an arbitrary bit
        // pattern. That restriction is deliberate and is NOT the property
        // getting weaker to fit: `serde_json`'s float *parser* is lossy by one
        // ULP for a large fraction of full-precision doubles (its formatter is
        // exact), so a strategy over `any::<f64>()` would be measuring a
        // dependency's rounding rather than this crate's envelope.
        // `a_full_precision_float_is_the_documented_exception` below pins that
        // limitation down so it is recorded rather than quietly avoided.
        (-1_000_000_000_i64..1_000_000_000)
            .prop_map(|scaled| FieldValue::Float(scaled as f64 / 1000.0)),
        any::<bool>().prop_map(FieldValue::Bool),
        prop::sample::select(vec!["locked", "busy", "bash", "read_file"])
            .prop_map(|text| FieldValue::Str(StaticStr::new(text))),
        Just(FieldValue::Null),
    ]
}

fn fields() -> impl Strategy<Value = Fields> {
    prop::collection::vec((prop::sample::select(KEYS.to_vec()), field_value()), 0..5).prop_map(
        |entries| {
            let mut fields = Fields::new();
            for (key, value) in entries {
                fields.push(key, value);
            }
            fields
        },
    )
}

fn cx() -> impl Strategy<Value = Cx> {
    (
        prop::option::of("[0-9a-f]{1,40}"),
        prop::option::of(any::<u64>()),
        prop::option::of(any::<u64>()),
    )
        .prop_map(|(session, turn, step)| {
            let mut cx = Cx::new();
            if let Some(session) = session {
                cx.session = Some(ShortId::new(&session));
            }
            cx.turn = turn;
            cx.step = step;
            cx
        })
}

fn record() -> impl Strategy<Value = Record> {
    (
        any::<u64>(),
        level(),
        prop::sample::select(CODES.to_vec()),
        prop::sample::select(TARGETS.to_vec()),
        cx(),
        fields(),
    )
        .prop_map(|(ts, level, code, target, cx, fields)| {
            Record::at(ts, level, code, target, cx, fields)
        })
}

/// Filter specs an operator could plausibly type, including nonsense.
fn spec() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            prop::sample::select(vec![
                "off", "error", "warn", "info", "debug", "trace", "shout", "",
            ])
            .prop_map(str::to_owned),
            (
                prop::sample::select(TARGETS.to_vec()),
                prop::sample::select(vec!["off", "debug", "trace", "nope"]),
            )
                .prop_map(|(target, level)| format!("{target}={level}")),
        ],
        0..4,
    )
    .prop_map(|clauses| clauses.join(","))
}

/// The one documented exception to property 8, stated plainly because
/// overclaiming is worse than the gap (§5.2 takes the same posture about
/// `Box::leak`).
///
/// `serde_json` writes every `f64` exactly and reads a large fraction of
/// full-precision doubles back one ULP off. Nothing in this crate can fix
/// that, and the alternative — quantising floats on the way in — would trade a
/// visible limitation for a silent one.
///
/// It costs nothing in practice: a diagnostic float is a duration, a ratio, or
/// a rate, and every such value round-trips exactly. This test exists so the
/// next person to widen the float strategy learns that from a failing name
/// rather than from a flake.
#[test]
fn a_full_precision_float_is_the_documented_exception() {
    let exact = 0.25_f64;
    let round_trip = |value: f64| -> f64 {
        let fields = Fields::new().with("value", value);
        let json = serde_json::to_string(&fields).expect("serialize");
        match serde_json::from_str::<Fields>(&json)
            .expect("parse")
            .get("value")
        {
            Some(&FieldValue::Float(parsed)) => parsed,
            other => panic!("expected a float, got {other:?}"),
        }
    };
    assert_eq!(round_trip(exact), exact, "a realistic value is exact");

    let awkward = 2.343_412_932_371_421_7e-125_f64;
    assert_eq!(
        serde_json::to_string(&awkward).expect("serialize"),
        "2.3434129323714217e-125",
        "the formatter is exact; it is the parser that rounds"
    );
    assert_ne!(
        round_trip(awkward),
        awkward,
        "if serde_json's parser has been fixed, widen the float strategy above \
         and delete this test"
    );
}

proptest! {
    /// **Property 8 — the envelope round-trips (invariant 4).**
    ///
    /// Both directions: the parsed record equals the original, *and*
    /// re-serializing it reproduces the same bytes. The second half is the one
    /// that matters for a crash file, which is read by tools nobody in this
    /// repository wrote.
    #[test]
    fn every_record_round_trips_byte_for_byte(record in record()) {
        let json = serde_json::to_string(&record).expect("serialize");
        let parsed: Record = serde_json::from_str(&json).expect("parse");
        prop_assert_eq!(&parsed, &record);
        prop_assert_eq!(serde_json::to_string(&parsed).expect("re-serialize"), json);
    }

    /// A record must never span lines: JSONL is the format, and one embedded
    /// newline makes every downstream `while read line` wrong.
    #[test]
    fn a_rendered_record_is_exactly_one_line(record in record()) {
        let json = serde_json::to_string(&record).expect("serialize");
        prop_assert!(!json.contains('\n'));
    }

    /// **Property 3 — the filter governs sinks, never emission.**
    ///
    /// For any filter and any record sequence, the counters and the ring are
    /// identical to a run with everything turned on; only sink output differs.
    /// This is what keeps a crash dump complete for a user who ran at the
    /// default `warn`, and what stops raising verbosity from changing a number.
    #[test]
    fn the_filter_governs_sinks_and_nothing_else(
        records in prop::collection::vec(record(), 0..40),
        spec in spec(),
    ) {
        let filtered_sink = Arc::new(Capture::new());
        let filtered = Dx::new(vec![Bound::new(
            Filter::parse(&spec).filter,
            filtered_sink.clone() as Arc<dyn Sink>,
        )]);

        let open_sink = Arc::new(Capture::new());
        let open = Dx::new(vec![Bound::new(
            Filter::level(LevelFilter::Trace),
            open_sink.clone() as Arc<dyn Sink>,
        )]);

        for record in &records {
            filtered.emit(record.clone());
            open.emit(record.clone());
        }

        prop_assert_eq!(filtered.counters(), open.counters());
        prop_assert_eq!(filtered.counters().total(), records.len() as u64);
        prop_assert_eq!(filtered.ring().records(), open.ring().records());
        prop_assert_eq!(open_sink.records().len(), records.len());
        prop_assert!(filtered_sink.records().len() <= records.len());

        // And the sink output, whatever it turned out to be, is exactly what
        // the filter admits — no more, and no less.
        let filter = Filter::parse(&spec).filter;
        let admitted = records
            .iter()
            .filter(|record| filter.admits(record.target.as_str(), record.level))
            .count();
        prop_assert_eq!(filtered_sink.records().len(), admitted);
    }

    /// **Property 4 — a failing sink never fails a caller.**
    ///
    /// A sink returning `Err` on every write leaves `emit` infallible and the
    /// turn intact. `emit` returns `()`, so "the caller cannot fail" is a
    /// property of the signature; what this pins down is that the *record* is
    /// not lost either — the ring and the counters are byte-identical to a run
    /// with a sink that works.
    #[test]
    fn a_sink_that_always_fails_changes_nothing_but_its_own_counter(
        records in prop::collection::vec(record(), 1..30),
    ) {
        let doomed = Dx::new(vec![Bound::new(
            Filter::level(LevelFilter::Trace),
            Arc::new(FailingSink) as Arc<dyn Sink>,
        )]);
        let working = Dx::new(vec![Bound::new(
            Filter::level(LevelFilter::Trace),
            Arc::new(Capture::new()) as Arc<dyn Sink>,
        )]);

        for record in &records {
            doomed.emit(record.clone());
            working.emit(record.clone());
        }

        prop_assert_eq!(doomed.ring().records(), working.ring().records());
        prop_assert_eq!(doomed.counters().total(), working.counters().total());
        prop_assert_eq!(doomed.counters().sink_failures, records.len() as u64);
        prop_assert_eq!(working.counters().sink_failures, 0);
    }

    /// The ring is bounded from above and holds the *tail*, whatever the
    /// traffic looked like. A ring that kept the head would dump the least
    /// relevant records at exactly the moment they matter least.
    #[test]
    fn the_ring_stays_bounded_and_keeps_the_tail(
        records in prop::collection::vec(record(), 1..80),
        capacity in 1_usize..12,
    ) {
        let ring = Ring::new(capacity, stella_diag::MAX_BYTES);
        for record in &records {
            ring.push(record.clone());
        }

        let held = ring.records();
        prop_assert!(held.len() <= capacity);
        prop_assert_eq!(held.len(), records.len().min(capacity));
        prop_assert_eq!(
            ring.stats().dropped,
            (records.len() - held.len()) as u64
        );
        prop_assert_eq!(held.as_slice(), &records[records.len() - held.len()..]);
    }

    /// Parsing a filter spec never panics and never rejects wholesale: a spec
    /// with three good clauses and one typo still yields the three. The gate
    /// this protects is a startup path — a typo in a log knob must not take a
    /// process down (§6).
    #[test]
    fn parsing_a_spec_is_total(spec in spec()) {
        let parsed = Filter::parse(&spec);
        let clauses = if spec.trim().is_empty() {
            0
        } else {
            spec.split(',').count()
        };
        prop_assert!(parsed.bad.len() <= clauses);
        for level in Level::ALL {
            // Every target answers, including ones no directive mentions.
            let _ = parsed.filter.admits("stella_unmentioned::module", level);
        }
    }
}
