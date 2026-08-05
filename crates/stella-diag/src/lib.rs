// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The **diagnostic plane**: why the program behaved the way it did.
//!
//! Stella has four observability planes and, until this crate, had built
//! three. `docs/spec/diagnostics.md` §2 is the table:
//!
//! | Plane | Question it answers | Where it lives |
//! |---|---|---|
//! | **Domain** | *What did the agent do?* | `AgentEvent`, `stella-protocol` |
//! | **Ledger** | *What did it cost, and can I prove it?* | receipts, `stella-cli::trace` |
//! | **Presentation** | *What should the human see?* | `println!`, the TUI |
//! | **Diagnostic** | *Why did the program behave this way?* | **here** |
//!
//! The missing fourth is why 682 `println!`s and 625 `let _ =`s coexisted: a
//! statement with no home gets split between the plane that is easiest to
//! reach (stdout) and the one that costs nothing (the floor).
//!
//! ## The routing rule
//!
//! Contributors need a rule they can apply without reading the design document
//! (§2.1):
//!
//! > **A human reads it now → presentation. It should replay → domain. It
//! > explains a decision the program made → diagnostic. It is none of those →
//! > it is not a log line, it is a metric.**
//!
//! ## What is distinctive here
//!
//! Two things, and neither is available off the shelf.
//!
//! **Content in a log does not compile** ([`Loggable`], §5.2). A field value is
//! not `serde_json::Value` and not `impl Display`; it is a closed type
//! constructible only from things that cannot carry runtime content. `tracing`
//! would log `%user_path` happily. Here it is a type error, so invariant 3's
//! most dangerous failure mode stops being a thing review has to catch.
//!
//! **A crash dump you can safely ask for** ([`Ring`], §7.4). Every record at
//! every level lands in a bounded in-memory ring regardless of filter, and it
//! reaches disk only on a panic or a non-zero exit. Because content cannot
//! enter a record, that file is content-free *by construction* — a maintainer
//! can ask a stranger to attach it without a privacy review, and the stranger
//! can send it without reading it first.
//!
//! ## Using it
//!
//! ```
//! use stella_diag::{Cx, Dx, diag};
//!
//! let (dx, records) = Dx::capturing();
//! let cx = Cx::new().session("session-a1b2c3d4").turn(3);
//!
//! diag!(&dx, warn, "store.migration.retry", cx: cx, attempt = 2_u32, backoff_ms = 250_u32);
//!
//! let record = records.find("store.migration.retry").expect("recorded");
//! assert_eq!(record.cx.turn, Some(3));
//! ```
//!
//! [`Dx`] is a **handle, not a global**: invariant 2 already banned ambient
//! state, so correlation rides as an explicit parameter and this crate needs no
//! thread-locals, no task-locals, and no spans (§7.1). That is also the
//! strongest argument in §10 for not adopting a facade — `tracing`'s spans
//! exist to reconstruct context that ordinary code loses, and this codebase
//! does not lose it.
//!
//! ## What this deliberately does not do
//!
//! Verbatim from §14, because each of these will be proposed again:
//!
//! - **No logging in `stella-core`'s pure functions** (§7.3). They return
//!   rationale as typed values; the caller — already at an I/O boundary —
//!   records it. This is the invariant most likely to erode.
//! - **No egress, at any level, in any build.** No sink opens a socket.
//! - **No per-token records.** [`Dx`] emits nothing per `TextDelta`; the
//!   sanctioned alternative is a bounded tally.
//! - **No `Display`/`Debug` blanket impl for fields**, however convenient —
//!   that single blanket impl is the entire hole §5.2 exists to close.
//! - **No replacement of `AgentEvent`.** The domain plane stays the authority
//!   for replay; the diagnostic plane references it by `seq` and never
//!   restates it (§8).

mod crash;
mod cx;
mod dx;
mod error;
mod field;
mod filter;
mod level;
mod path;
mod private;
mod record;
mod ring;
mod sink;

pub use crash::{dump, dump_after_panic, install_panic_hook};
pub use cx::{Cx, ShortId};
pub use dx::{CounterSnapshot, Counters, Dx, Facet};
pub use error::error_field;
pub use field::{FieldValue, Fields, Loggable, ReviewedValue, StaticStr};
pub use filter::{BadClause, ClauseFault, Filter, Parsed};
pub use level::{Level, LevelFilter};
pub use path::{Extension, PathClass, PathContext, PathKind};
pub use record::{Record, now_millis};
pub use ring::{
    CRASH_KEEP, CRASH_PREFIX, MAX_BYTES, MAX_RECORDS, Ring, RingStats, latest_crash_file,
    prune_crash_files,
};
pub use sink::{
    Bound, Capture, FailingSink, JsonlSink, NullSink, Sink, SinkError, TerminalGated, TerminalHold,
    TextSink,
};

mod redact;
pub use redact::{Note, Redacted};

/// The environment variable that carries the filter spec (§6).
pub const LOG_ENV: &str = "STELLA_LOG";

/// The shipped alias, kept so `stella-serve`'s surface does not break (§6).
///
/// It named a *level* and now names a full filter spec, which is a strict
/// widening: every value that worked before still means what it meant.
pub const SERVE_LOG_ENV: &str = "STELLA_SERVE_LOG";

/// Read a filter from the environment: `STELLA_LOG`, else `STELLA_SERVE_LOG`,
/// else the default.
///
/// Returns the parse result whole, so the caller can emit the `warn` record §6
/// asks for once it has a sink to emit it to — which is the ordering problem
/// this returns rather than solves: the complaint about a bad filter has to
/// outlive the filter that would have suppressed it.
#[must_use]
pub fn filter_from_env() -> Parsed {
    match std::env::var(LOG_ENV).or_else(|_| std::env::var(SERVE_LOG_ENV)) {
        Ok(spec) => Filter::parse(&spec),
        Err(_) => Parsed {
            filter: Filter::default(),
            bad: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped `STELLA_SERVE_LOG` values were bare levels. Every one of
    /// them must still parse to the same thing under the widened grammar, or
    /// §6's "the shipped surface does not break" is a claim and not a fact.
    #[test]
    fn every_shipped_serve_log_value_still_means_what_it_meant() {
        for (spec, expected) in [
            ("off", LevelFilter::Off),
            ("error", LevelFilter::Error),
            ("warn", LevelFilter::Warn),
            ("info", LevelFilter::Info),
            ("debug", LevelFilter::Debug),
        ] {
            let parsed = Filter::parse(spec);
            assert!(parsed.bad.is_empty(), "{spec}");
            assert_eq!(parsed.filter.level_for("stella_serve"), expected, "{spec}");
        }
    }
}
