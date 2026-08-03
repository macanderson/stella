// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The emitter every call site talks to, and the macro that builds a record.
//!
//! [`Dx`] is a **handle, not a global**. `docs/design/diagnostics/diagnostics.md` §7.1 is
//! the reason: invariant 2 already banned ambient state, so a diagnostic
//! handle rides as an explicit parameter exactly like every other input a
//! decision function receives. Nothing here is a thread-local or a task-local,
//! both of which are wrong under `!Send` turn futures and wrong under `tokio`
//! task migration.
//!
//! Emission is unconditional and filtering is not. §6:
//!
//! > Everything at every level, always, goes to the ring regardless of filter —
//! > the filter governs *sinks*, never *emission*, which is what keeps counters
//! > correct at any verbosity.
//!
//! Property 3 of §12 is that sentence as a proptest.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::cx::Cx;
use crate::field::Fields;
use crate::filter::Filter;
use crate::level::Level;
use crate::record::Record;
use crate::ring::Ring;
use crate::sink::{Bound, Capture, Sink};

/// Running totals, incremented on every record before any filter is consulted.
///
/// This is the "cannot disagree with the log" property `observe/metrics.rs`
/// established, generalised: a counter derived from the same emission the
/// records come from cannot drift from them, and raising verbosity cannot
/// change a number.
#[derive(Debug, Default)]
pub struct Counters {
    by_level: [AtomicU64; 5],
    sink_failures: AtomicU64,
}

/// A consistent-enough read of [`Counters`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CounterSnapshot {
    pub error: u64,
    pub warn: u64,
    pub info: u64,
    pub debug: u64,
    pub trace: u64,
    /// Writes a sink refused. Non-zero means records are being lost *to disk*,
    /// never that a caller failed.
    pub sink_failures: u64,
}

impl CounterSnapshot {
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.error + self.warn + self.info + self.debug + self.trace
    }

    #[must_use]
    pub const fn at(&self, level: Level) -> u64 {
        match level {
            Level::Error => self.error,
            Level::Warn => self.warn,
            Level::Info => self.info,
            Level::Debug => self.debug,
            Level::Trace => self.trace,
        }
    }
}

impl Counters {
    fn record(&self, level: Level) {
        self.by_level[level as usize].fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> CounterSnapshot {
        let read = |level: Level| self.by_level[level as usize].load(Ordering::Relaxed);
        CounterSnapshot {
            error: read(Level::Error),
            warn: read(Level::Warn),
            info: read(Level::Info),
            debug: read(Level::Debug),
            trace: read(Level::Trace),
            sink_failures: self.sink_failures.load(Ordering::Relaxed),
        }
    }
}

/// The diagnostic handle: sinks, the crash ring, and the counters.
///
/// Cheap to share (`Arc<Dx>`), safe to hold across threads, and impossible to
/// make fail — [`Dx::emit`] returns `()` no matter what a sink does.
pub struct Dx {
    sinks: Vec<Bound>,
    ring: Ring,
    counters: Counters,
}

impl Dx {
    /// Build a handle over the given bound sinks.
    #[must_use]
    pub fn new(sinks: Vec<Bound>) -> Self {
        Self {
            sinks,
            ring: Ring::default(),
            counters: Counters::default(),
        }
    }

    /// A handle with no sinks.
    ///
    /// The default for a library type that has not been wired to anything —
    /// and note that the ring still fills, so an embedder who never configured
    /// a sink can still dump one on a crash. "No sinks" is not "no
    /// diagnostics".
    #[must_use]
    pub fn disabled() -> Self {
        Self::new(Vec::new())
    }

    /// Replace the ring, for a caller that wants a different bound than
    /// §7.4's.
    #[must_use]
    pub fn with_ring(mut self, ring: Ring) -> Self {
        self.ring = ring;
        self
    }

    /// A handle whose one sink records everything in memory, plus that sink.
    ///
    /// The shape most tests want: assert on typed [`Record`] values rather
    /// than on scraped stderr.
    #[must_use]
    pub fn capturing() -> (Self, Arc<Capture>) {
        let capture = Arc::new(Capture::new());
        let dx = Self::new(vec![Bound::new(
            Filter::parse("trace").filter,
            capture.clone() as Arc<dyn Sink>,
        )]);
        (dx, capture)
    }

    /// Record one thing.
    ///
    /// Infallible by signature, which is the contract §3.4 asks for: losing a
    /// log line must never lose a turn. A sink that refuses the write is
    /// counted in [`CounterSnapshot::sink_failures`] and otherwise ignored.
    pub fn emit(&self, record: Record) {
        self.counters.record(record.level);
        for bound in &self.sinks {
            if let Some(Err(_)) = bound.offer(&record) {
                self.counters.sink_failures.fetch_add(1, Ordering::Relaxed);
            }
        }
        // Last, and unconditionally. The ring takes ownership, so pushing
        // first would force a clone on every record for the sinks' benefit.
        self.ring.push(record);
    }

    /// The crash ring (§7.4).
    #[must_use]
    pub fn ring(&self) -> &Ring {
        &self.ring
    }

    #[must_use]
    pub fn counters(&self) -> CounterSnapshot {
        self.counters.snapshot()
    }

    /// Whether any sink would record something from `target` at `level`.
    ///
    /// For the rare caller whose *fields* are expensive to compute. Do not
    /// reach for it by default: the ring wants the record either way, and a
    /// guard that skips emission is a counter that lies.
    #[must_use]
    pub fn would_record(&self, target: &str, level: Level) -> bool {
        self.sinks
            .iter()
            .any(|bound| bound.filter().admits(target, level))
    }

    /// Flush every sink. Best-effort; failures are counted, never returned.
    pub fn flush(&self) {
        for bound in &self.sinks {
            if bound.flush().is_err() {
                self.counters.sink_failures.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

impl Default for Dx {
    fn default() -> Self {
        Self::disabled()
    }
}

impl std::fmt::Debug for Dx {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Dx")
            .field("sinks", &self.sinks.len())
            .field("ring", &self.ring.stats())
            .field("counters", &self.counters.snapshot())
            .finish()
    }
}

/// Map the lowercase level written at a call site onto [`Level`].
///
/// Exported because [`diag!`] expands in the caller's crate; not part of the
/// public API.
#[doc(hidden)]
#[macro_export]
macro_rules! __diag_level {
    (trace) => {
        $crate::Level::Trace
    };
    (debug) => {
        $crate::Level::Debug
    };
    (info) => {
        $crate::Level::Info
    };
    (warn) => {
        $crate::Level::Warn
    };
    (error) => {
        $crate::Level::Error
    };
}

/// Emit one diagnostic record.
///
/// ```
/// # use stella_diag::{diag, Cx, Dx};
/// let (dx, records) = Dx::capturing();
/// let cx = Cx::new().turn(3);
///
/// diag!(&dx, warn, "store.migration.retry", cx: cx, attempt = 2_u32, backoff_ms = 250_u32);
/// diag!(&dx, info, "cli.boot");
///
/// assert_eq!(records.codes(), ["store.migration.retry", "cli.boot"]);
/// ```
///
/// `target` is `module_path!()` at the call site, which is what
/// [`Filter`](crate::Filter) matches on — so `STELLA_LOG=stella_store=debug`
/// selects records by where they were written, with no bookkeeping at the emit
/// site.
///
/// Field values must implement [`Loggable`](crate::Loggable), and that is the
/// point: a `String`, a `Path`, or a non-`'static` `&str` **does not compile**
/// (§5.2). Reach for [`Redacted::reviewed`](crate::Redacted::reviewed) when a
/// runtime string genuinely belongs in a record, and write down why.
#[macro_export]
macro_rules! diag {
    ($dx:expr, $level:ident, $code:literal, cx: $cx:expr $(, $key:ident = $value:expr)* $(,)?) => {{
        // Bound to a typed local so a caller who passes something else gets an
        // error naming `Dx`, rather than a method-not-found further in.
        let dx: &$crate::Dx = $dx;
        #[allow(unused_mut)]
        let mut fields = $crate::Fields::new();
        $(fields.push(
            ::core::stringify!($key),
            $crate::Loggable::to_field(&$value),
        );)*
        dx.emit($crate::Record::new(
            $crate::__diag_level!($level),
            $code,
            ::core::module_path!(),
            $cx,
            fields,
        ));
    }};
    ($dx:expr, $level:ident, $code:literal $(, $key:ident = $value:expr)* $(,)?) => {
        $crate::diag!($dx, $level, $code, cx: $crate::Cx::EMPTY $(, $key = $value)*)
    };
}

/// Fields for a record built outside [`diag!`](crate::diag) — the
/// facet-rendering path.
///
/// A per-crate facet enum (§5.1) implements this to turn itself into a record's
/// worth of fields, so the crate that owns the vocabulary owns the rendering
/// too and nobody edits a shared enum.
pub trait Facet {
    /// This event's stable diagnostic code (§11).
    fn code(&self) -> &'static str;

    /// How loud it is. Derived from the variant rather than stored on it, so a
    /// record's severity can never disagree with what happened.
    fn level(&self) -> Level;

    fn fields(&self) -> Fields;

    /// Render into a record. `target` is the emitting module — pass
    /// `module_path!()`.
    fn to_record(&self, target: &'static str, cx: Cx) -> Record {
        Record::new(self.level(), self.code(), target, cx, self.fields())
    }
}

impl Dx {
    /// Emit a facet's record.
    pub fn emit_facet(&self, facet: &dyn Facet, target: &'static str, cx: Cx) {
        self.emit(facet.to_record(target, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::FailingSink;
    use crate::{Redacted, note};

    #[test]
    fn a_record_carries_the_call_sites_module_as_its_target() {
        let (dx, records) = Dx::capturing();
        diag!(&dx, warn, "test.target");
        assert_eq!(records.records()[0].target.as_str(), module_path!());
    }

    #[test]
    fn fields_reach_the_record_in_the_order_written() {
        let (dx, records) = Dx::capturing();
        let cx = Cx::new().session("session-abcd1234").turn(7);
        diag!(
            &dx,
            debug,
            "agent.tool.call",
            cx: cx,
            tool = "bash",
            args_bytes = 412_u64,
        );
        let record = records.find("agent.tool.call").expect("a record");
        assert_eq!(record.level, Level::Debug);
        assert_eq!(record.cx.turn, Some(7));
        assert_eq!(
            record.fields.iter().map(|(key, _)| key).collect::<Vec<_>>(),
            vec!["tool", "args_bytes"]
        );
    }

    /// §3.4 and property 4 of §12: `emit` is infallible, and a sink that
    /// refuses every write leaves the caller intact and says so in a counter.
    #[test]
    fn a_sink_that_fails_every_write_never_fails_the_caller() {
        let dx = Dx::new(vec![Bound::new(
            Filter::parse("trace").filter,
            Arc::new(FailingSink) as Arc<dyn Sink>,
        )]);
        for n in 0..5_u64 {
            diag!(&dx, error, "test.doomed", n = n);
        }
        let counters = dx.counters();
        assert_eq!(counters.error, 5, "every record was still emitted");
        assert_eq!(counters.sink_failures, 5, "and every loss was counted");
        assert_eq!(dx.ring().stats().held, 5, "the ring is unaffected");
    }

    /// §6: the filter governs sinks, never emission.
    #[test]
    fn a_silenced_sink_changes_neither_the_ring_nor_the_counters() {
        let (loud, loud_records) = Dx::capturing();
        let (quiet, quiet_records) = {
            let capture = Arc::new(Capture::new());
            let dx = Dx::new(vec![Bound::new(
                Filter::parse("off").filter,
                capture.clone() as Arc<dyn Sink>,
            )]);
            (dx, capture)
        };

        for dx in [&loud, &quiet] {
            diag!(dx, error, "test.a");
            diag!(dx, debug, "test.b");
            diag!(dx, trace, "test.c");
        }

        assert_eq!(loud.counters(), quiet.counters());
        assert_eq!(
            loud.ring().records().len(),
            quiet.ring().records().len(),
            "the ring holds everything at every level, filter notwithstanding"
        );
        assert_eq!(loud_records.records().len(), 3);
        assert!(
            quiet_records.records().is_empty(),
            "only sink output differs"
        );
    }

    #[test]
    fn a_record_with_no_fields_and_no_cx_is_legal() {
        let (dx, records) = Dx::capturing();
        diag!(&dx, info, "cli.boot");
        let record = records.find("cli.boot").expect("a record");
        assert!(record.fields.is_empty());
        assert!(record.cx.is_empty());
    }

    #[test]
    fn the_reviewed_hatch_is_reachable_from_the_macro() {
        let (dx, records) = Dx::capturing();
        let branch = String::from("feature/logging");
        diag!(
            &dx,
            info,
            "git.branch.selected",
            branch = Redacted::reviewed(
                branch,
                note!("git ref names are user-chosen but are not model content"),
            ),
        );
        let json = serde_json::to_string(&records.records()[0]).expect("serialize");
        assert!(json.contains("feature/logging"), "{json}");
    }

    #[test]
    fn would_record_reflects_the_loudest_sink() {
        let dx = Dx::new(vec![
            Bound::new(Filter::parse("error").filter, Arc::new(Capture::new())),
            Bound::new(
                Filter::parse("off,stella_diag=trace").filter,
                Arc::new(Capture::new()),
            ),
        ]);
        assert!(dx.would_record("stella_diag::dx", Level::Trace));
        assert!(dx.would_record("stella_cli", Level::Error));
        assert!(!dx.would_record("stella_cli", Level::Debug));
    }

    #[test]
    fn a_disabled_handle_still_fills_the_ring() {
        let dx = Dx::disabled();
        diag!(&dx, warn, "test.unwired");
        assert_eq!(dx.ring().stats().held, 1);
        assert_eq!(dx.counters().warn, 1);
    }

    #[test]
    fn a_facet_renders_itself_into_the_envelope() {
        struct Migrated {
            attempt: u32,
        }
        impl Facet for Migrated {
            fn code(&self) -> &'static str {
                "store.migration.retry"
            }
            fn level(&self) -> Level {
                Level::Warn
            }
            fn fields(&self) -> Fields {
                Fields::new().with("attempt", self.attempt)
            }
        }

        let (dx, records) = Dx::capturing();
        dx.emit_facet(&Migrated { attempt: 2 }, "stella_store::migrate", Cx::EMPTY);
        let record = records.find("store.migration.retry").expect("a record");
        assert_eq!(record.target.as_str(), "stella_store::migrate");
        assert_eq!(
            record.fields.get("attempt"),
            Some(&crate::FieldValue::Uint(2))
        );
    }
}
