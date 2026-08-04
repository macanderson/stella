// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! How loud a record is, and how loud a sink is willing to be.
//!
//! Two types rather than one, and the distinction is load-bearing:
//! [`Level`] is a property of a record and always exists; [`LevelFilter`] is a
//! property of a *sink* and can be [`LevelFilter::Off`], which a record's level
//! never can. `docs/design/diagnostics/diagnostics.md` §6 is the rule that keeps them
//! apart — the filter governs sinks, never emission, so counters and the crash
//! ring stay correct at any verbosity.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// How loud one record is.
///
/// Ordered so filtering is a comparison: a record passes when
/// `record.level <= filter`. `Error` is therefore the *smallest*, which reads
/// backwards for a second and then never again — it is the same ordering
/// `stella-serve::observe::event::Level` already uses, and agreeing with the
/// crate this design generalises is worth more than matching intuition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    /// Every level, quietest first. The ordering `-v`/`-vv`/`-vvv` walks.
    pub const ALL: [Self; 5] = [
        Self::Error,
        Self::Warn,
        Self::Info,
        Self::Debug,
        Self::Trace,
    ];

    #[must_use]
    pub const fn as_static(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_static())
    }
}

/// How loud a sink is willing to be.
///
/// [`LevelFilter::Off`] silences a sink entirely; it does **not** stop records
/// being emitted (§6), which is what property 3 of §12 pins down.
/// Variant order is the loudness order, so `Ord` means "at least as verbose
/// as". The default is [`LevelFilter::Warn`] rather than the first variant: an
/// operator who set nothing wants to hear about problems and nothing else (§6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum LevelFilter {
    Off,
    Error,
    #[default]
    Warn,
    Info,
    Debug,
    Trace,
}

impl LevelFilter {
    /// Whether a record at `level` reaches a sink set to this filter.
    #[must_use]
    pub const fn admits(self, level: Level) -> bool {
        match self {
            Self::Off => false,
            Self::Error => matches!(level, Level::Error),
            Self::Warn => matches!(level, Level::Error | Level::Warn),
            Self::Info => matches!(level, Level::Error | Level::Warn | Level::Info),
            Self::Debug => !matches!(level, Level::Trace),
            Self::Trace => true,
        }
    }

    #[must_use]
    pub const fn as_static(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }

    /// The filter `-v` repeated `count` times asks for: `info`, `debug`, then
    /// `trace`, saturating. Zero leaves the caller's default alone, which is
    /// why this returns `None` rather than `Warn` — "the user said nothing"
    /// and "the user asked for warn" must stay distinguishable so `STELLA_LOG`
    /// can win over an unstated default and lose to an explicit `-v`.
    #[must_use]
    pub const fn from_verbosity(count: u8) -> Option<Self> {
        match count {
            0 => None,
            1 => Some(Self::Info),
            2 => Some(Self::Debug),
            _ => Some(Self::Trace),
        }
    }
}

impl fmt::Display for LevelFilter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_static())
    }
}

impl FromStr for LevelFilter {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        // Case-insensitive because `STELLA_LOG=WARN` is a thing people type and
        // rejecting it would teach nothing.
        match text.trim().to_ascii_lowercase().as_str() {
            "off" | "none" => Ok(Self::Off),
            "error" => Ok(Self::Error),
            "warn" | "warning" => Ok(Self::Warn),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            "trace" => Ok(Self::Trace),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filter_admits_everything_at_or_below_it() {
        assert!(LevelFilter::Warn.admits(Level::Error));
        assert!(LevelFilter::Warn.admits(Level::Warn));
        assert!(!LevelFilter::Warn.admits(Level::Info));
        assert!(!LevelFilter::Off.admits(Level::Error));
        for level in Level::ALL {
            assert!(LevelFilter::Trace.admits(level));
            assert!(!LevelFilter::Off.admits(level));
        }
    }

    /// `-v` with no `v`s must not be mistaken for an explicit `warn`, or an
    /// unset flag would silently outrank `STELLA_LOG`.
    #[test]
    fn no_verbosity_flag_states_no_opinion() {
        assert_eq!(LevelFilter::from_verbosity(0), None);
        assert_eq!(LevelFilter::from_verbosity(1), Some(LevelFilter::Info));
        assert_eq!(LevelFilter::from_verbosity(2), Some(LevelFilter::Debug));
        assert_eq!(LevelFilter::from_verbosity(9), Some(LevelFilter::Trace));
    }

    #[test]
    fn level_names_round_trip_through_serde() {
        for level in Level::ALL {
            let json = serde_json::to_string(&level).expect("serialize");
            assert_eq!(json, format!("\"{}\"", level.as_static()));
            let back: Level = serde_json::from_str(&json).expect("parse");
            assert_eq!(back, level);
        }
    }

    #[test]
    fn filter_names_parse_case_insensitively() {
        assert_eq!("WARN".parse(), Ok(LevelFilter::Warn));
        assert_eq!(" trace ".parse(), Ok(LevelFilter::Trace));
        assert_eq!("off".parse(), Ok(LevelFilter::Off));
        assert_eq!("shout".parse::<LevelFilter>(), Err(()));
    }
}
