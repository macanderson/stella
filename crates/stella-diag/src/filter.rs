// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A real filter, not a level — `docs/spec/diagnostics.md` §6.
//!
//! `STELLA_SERVE_LOG` is a single level, which `serve-observability.md` §9
//! knowingly gave up. At seventeen crates that stops being acceptable: `debug`
//! across the workspace is noise nobody can read, so the one thing an operator
//! actually needs is "quiet everywhere, loud in `stella_store`".
//!
//! ```text
//! STELLA_LOG=warn                                  # global
//! STELLA_LOG=warn,stella_store=debug,stella_model=trace
//! STELLA_LOG=off
//! ```
//!
//! Longest-prefix wins, so `stella_store=warn,stella_store::migrate=trace`
//! means what it looks like. Matching is on module-path boundaries rather than
//! raw string prefixes: `stella_store` selects `stella_store::migrate` and not
//! a hypothetical `stella_storage`.
//!
//! **A typo must not take a process down.** An unparseable clause is skipped,
//! not fatal, and is reported in [`Parsed::bad`] so the caller can emit one
//! `warn` record naming it — the posture `STELLA_SERVE_LOG` already takes, and
//! it is right.

use crate::level::{Level, LevelFilter};
use crate::log_enum;

log_enum! {
    /// Why one clause of a filter spec was skipped.
    ///
    /// A closed vocabulary rather than a message, because this is reported
    /// *through* the diagnostic plane and §5.2 does not make an exception for
    /// the plane's own errors.
    pub enum ClauseFault {
        /// `warn,,debug` — an empty clause between two commas.
        Empty => "empty",
        /// `stella_store=shout` — the level is not one of the six.
        UnknownLevel => "unknown_level",
        /// `=debug` — a directive with no target to attach to.
        EmptyTarget => "empty_target",
    }
}

/// One clause the parser refused, with enough to name it without guessing.
///
/// `clause` is the operator's own text. It reaches a record only through
/// [`Redacted::reviewed`](crate::Redacted::reviewed) — see
/// [`Parsed::report`] — because §5.2 has no carve-out for convenience.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadClause {
    /// Position in the comma-separated spec, 0-based. Loggable on its own,
    /// which is what a `warn` record carries when the text is not worth the
    /// escape hatch.
    pub index: usize,
    pub clause: String,
    pub reason: ClauseFault,
}

/// One `target=level` rule.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Directive {
    target: String,
    level: LevelFilter,
}

impl Directive {
    /// Whether this directive governs `target`, matching on module-path
    /// boundaries. `stella_store` governs `stella_store` and
    /// `stella_store::migrate`, but not `stella_storage`.
    fn governs(&self, target: &str) -> bool {
        target == self.target
            || (target.len() > self.target.len()
                && target.starts_with(&self.target)
                && target[self.target.len()..].starts_with("::"))
    }
}

/// Which records reach a sink.
///
/// Governs **sinks only**. Emission, the counters and the crash ring are
/// untouched by it (§6, and property 3 of §12) — that is what keeps a crash
/// dump complete even when the operator asked for silence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    default: LevelFilter,
    /// Sorted longest-target-first at construction, so [`Filter::level_for`]
    /// is a linear scan that stops at the first match rather than a search for
    /// the maximum.
    directives: Vec<Directive>,
}

impl Default for Filter {
    fn default() -> Self {
        Self::level(LevelFilter::default())
    }
}

/// The result of parsing a spec: the filter to use, and every clause that was
/// skipped getting there.
///
/// Both halves, always. A parse that returned only the filter would make a
/// typo silent, and a parse that returned only an error would make one typo
/// discard an otherwise-good spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub filter: Filter,
    pub bad: Vec<BadClause>,
}

impl Filter {
    /// Nothing reaches a sink.
    #[must_use]
    pub fn off() -> Self {
        Self::level(LevelFilter::Off)
    }

    /// One level for every target.
    #[must_use]
    pub fn level(default: LevelFilter) -> Self {
        Self {
            default,
            directives: Vec::new(),
        }
    }

    /// Parse the `STELLA_LOG` / `--log-level` grammar.
    ///
    /// Never fails. A clause that does not parse is skipped and reported in
    /// [`Parsed::bad`]; the clauses around it still apply, because one typo in
    /// a four-clause spec should cost the operator that clause and not the
    /// other three.
    #[must_use]
    pub fn parse(spec: &str) -> Parsed {
        let mut filter = Self::default();
        let mut bad = Vec::new();
        // An all-whitespace spec is "the operator set the variable to nothing",
        // which is not an opinion — the default stands, silently.
        if spec.trim().is_empty() {
            return Parsed { filter, bad };
        }

        for (index, clause) in spec.split(',').enumerate() {
            let clause = clause.trim();
            if clause.is_empty() {
                bad.push(BadClause {
                    index,
                    clause: clause.to_owned(),
                    reason: ClauseFault::Empty,
                });
                continue;
            }
            match clause.split_once('=') {
                None => match clause.parse::<LevelFilter>() {
                    Ok(level) => filter.default = level,
                    Err(()) => bad.push(BadClause {
                        index,
                        clause: clause.to_owned(),
                        reason: ClauseFault::UnknownLevel,
                    }),
                },
                Some((target, level)) => {
                    let target = target.trim();
                    if target.is_empty() {
                        bad.push(BadClause {
                            index,
                            clause: clause.to_owned(),
                            reason: ClauseFault::EmptyTarget,
                        });
                        continue;
                    }
                    match level.parse::<LevelFilter>() {
                        Ok(level) => {
                            // Targets are `module_path!()` values, which spell
                            // crates with underscores. Accepting the hyphenated
                            // *crate* name too means `stella-store=debug` works,
                            // because that is what the operator sees in
                            // `Cargo.toml` and on the command line.
                            let target = target.replace('-', "_");
                            // A repeated target is last-one-wins, the shell
                            // convention for a repeated setting. Replacing here
                            // rather than leaning on the stability of the sort
                            // below keeps that a stated rule instead of an
                            // emergent one.
                            filter
                                .directives
                                .retain(|existing| existing.target != target);
                            filter.directives.push(Directive { target, level });
                        }
                        Err(()) => bad.push(BadClause {
                            index,
                            clause: clause.to_owned(),
                            reason: ClauseFault::UnknownLevel,
                        }),
                    }
                }
            }
        }

        // Longest target first, so the first match in `level_for` is the most
        // specific one. Duplicate targets were already collapsed above, so no
        // tie here is between two rules that could both match.
        filter
            .directives
            .sort_by_key(|directive| std::cmp::Reverse(directive.target.len()));
        Parsed { filter, bad }
    }

    /// The level in force for `target`: the longest matching directive, or the
    /// spec's default.
    #[must_use]
    pub fn level_for(&self, target: &str) -> LevelFilter {
        self.directives
            .iter()
            .find(|directive| directive.governs(target))
            .map_or(self.default, |directive| directive.level)
    }

    /// Whether a record from `target` at `level` reaches a sink.
    #[must_use]
    pub fn admits(&self, target: &str, level: Level) -> bool {
        self.level_for(target).admits(level)
    }

    /// The level in force where no directive matches.
    #[must_use]
    pub fn default_level(&self) -> LevelFilter {
        self.default
    }

    /// Whether this filter admits nothing at all, for any target — the cheap
    /// check a caller uses to skip building a sink it would never write to.
    #[must_use]
    pub fn is_off(&self) -> bool {
        self.default == LevelFilter::Off
            && self
                .directives
                .iter()
                .all(|directive| directive.level == LevelFilter::Off)
    }

    /// Raise this filter's floor to at least `level`, leaving anything already
    /// louder alone.
    ///
    /// This is how `--log-file` gets a usable file without silently overriding
    /// an operator who asked for `trace`: a file sink defaults to `info`, but
    /// `STELLA_LOG=trace --log-file x` still writes trace.
    #[must_use]
    pub fn at_least(mut self, level: LevelFilter) -> Self {
        self.default = self.default.max(level);
        for directive in &mut self.directives {
            directive.level = directive.level.max(level);
        }
        self
    }
}

impl Parsed {
    /// Fields naming one skipped clause, for the `warn` record §6 asks for.
    ///
    /// The clause text is the operator's own keystrokes — never model output,
    /// never file content, never a path — so it goes through the reviewed
    /// escape hatch rather than around the type system. This is the escape
    /// hatch working exactly as §5.5 intends: rare, justified in source, and
    /// visible in the diff that introduced it.
    #[must_use]
    pub fn report(bad: &BadClause) -> crate::Fields {
        crate::Fields::new()
            .with("clause_index", bad.index)
            .with("reason", bad.reason)
            .with(
                "clause",
                crate::Redacted::reviewed(
                    bad.clause.clone(),
                    crate::note!(
                        "a log-filter clause is typed by the operator on their own command line \
                         or in their own environment; it is never model output, file content, or \
                         a path, and naming it back is the entire point of the message"
                    ),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_level_sets_the_default() {
        let parsed = Filter::parse("debug");
        assert!(parsed.bad.is_empty());
        assert_eq!(parsed.filter.level_for("anything"), LevelFilter::Debug);
    }

    #[test]
    fn a_directive_overrides_the_default_for_its_subtree() {
        let parsed = Filter::parse("warn,stella_store=debug,stella_model=trace");
        assert!(parsed.bad.is_empty());
        let filter = parsed.filter;
        assert_eq!(filter.level_for("stella_cli::agent"), LevelFilter::Warn);
        assert_eq!(filter.level_for("stella_store"), LevelFilter::Debug);
        assert_eq!(
            filter.level_for("stella_store::migrate"),
            LevelFilter::Debug
        );
        assert_eq!(
            filter.level_for("stella_model::anthropic"),
            LevelFilter::Trace
        );
    }

    /// The whole reason directives are sorted: a more specific target must
    /// beat a less specific one regardless of the order they were written in.
    #[test]
    fn the_longest_matching_target_wins_whichever_order_it_was_written() {
        for spec in [
            "warn,stella_store=info,stella_store::migrate=trace",
            "warn,stella_store::migrate=trace,stella_store=info",
        ] {
            let filter = Filter::parse(spec).filter;
            assert_eq!(
                filter.level_for("stella_store::migrate"),
                LevelFilter::Trace,
                "{spec}"
            );
            assert_eq!(
                filter.level_for("stella_store::open"),
                LevelFilter::Info,
                "{spec}"
            );
        }
    }

    /// A prefix that is not a module boundary must not match, or
    /// `stella_store` would quietly turn on `stella_storage` too.
    #[test]
    fn a_target_matches_on_module_boundaries_not_raw_prefixes() {
        let filter = Filter::parse("off,stella_store=trace").filter;
        assert!(filter.admits("stella_store::migrate", Level::Trace));
        assert!(!filter.admits("stella_storage::migrate", Level::Error));
    }

    /// Operators read crate names off `Cargo.toml`, where they are hyphenated;
    /// `module_path!()` spells them with underscores. Accepting both is the
    /// difference between a knob that works and a knob that silently does
    /// nothing.
    #[test]
    fn a_hyphenated_crate_name_selects_the_same_target() {
        let filter = Filter::parse("off,stella-store=debug").filter;
        assert!(filter.admits("stella_store::migrate", Level::Debug));
    }

    /// One typo costs the operator that clause, not the other three, and it is
    /// reported rather than swallowed.
    #[test]
    fn a_bad_clause_is_skipped_and_named_without_discarding_the_rest() {
        let parsed = Filter::parse("warn,stella_store=shout,stella_model=trace");
        assert_eq!(parsed.bad.len(), 1);
        assert_eq!(parsed.bad[0].index, 1);
        assert_eq!(parsed.bad[0].reason, ClauseFault::UnknownLevel);
        assert_eq!(parsed.bad[0].clause, "stella_store=shout");
        assert_eq!(
            parsed.filter.level_for("stella_model"),
            LevelFilter::Trace,
            "a later clause must survive an earlier typo"
        );
        assert_eq!(parsed.filter.level_for("stella_store"), LevelFilter::Warn);
    }

    /// A repeated setting is last-one-wins, the way a repeated shell variable
    /// is. Anything else makes an operator's own override lose to their
    /// earlier attempt.
    #[test]
    fn a_repeated_target_takes_the_last_value_written() {
        let filter = Filter::parse("off,stella_store=trace,stella_store=error").filter;
        assert_eq!(
            filter.level_for("stella_store::migrate"),
            LevelFilter::Error
        );
    }

    #[test]
    fn an_empty_or_targetless_clause_is_reported() {
        let parsed = Filter::parse("warn,,=debug");
        let reasons: Vec<_> = parsed.bad.iter().map(|bad| bad.reason).collect();
        assert_eq!(reasons, vec![ClauseFault::Empty, ClauseFault::EmptyTarget]);
    }

    /// An unset variable and a variable set to nothing are the same statement:
    /// no opinion. Neither should produce a complaint.
    #[test]
    fn an_empty_spec_is_the_default_and_not_a_complaint() {
        let parsed = Filter::parse("   ");
        assert!(parsed.bad.is_empty());
        assert_eq!(parsed.filter, Filter::default());
    }

    #[test]
    fn off_silences_every_target() {
        let filter = Filter::parse("off").filter;
        assert!(filter.is_off());
        for level in Level::ALL {
            assert!(!filter.admits("stella_store", level));
        }
    }

    /// `--log-file` raises the floor without overruling a louder operator.
    #[test]
    fn at_least_raises_the_floor_and_never_lowers_it() {
        let quiet = Filter::parse("warn").filter.at_least(LevelFilter::Info);
        assert_eq!(quiet.level_for("stella_cli"), LevelFilter::Info);

        let loud = Filter::parse("trace").filter.at_least(LevelFilter::Info);
        assert_eq!(loud.level_for("stella_cli"), LevelFilter::Trace);
    }

    /// The report is a diagnostic record like any other, so the clause text
    /// must travel through the reviewed hatch — carrying the note with it.
    #[test]
    fn a_bad_clause_report_names_the_clause_through_the_reviewed_hatch() {
        let parsed = Filter::parse("warn,stella_store=shout");
        let fields = Parsed::report(&parsed.bad[0]);
        let json = serde_json::to_string(&fields).expect("serialize");
        assert!(json.contains("stella_store=shout"), "{json}");
        assert!(json.contains("\"reason\":\"unknown_level\""), "{json}");
        assert!(
            json.contains("typed by the operator"),
            "the justification must travel with the value: {json}"
        );
    }
}
