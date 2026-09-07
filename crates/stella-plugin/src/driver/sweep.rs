//! The `sweep` family's wire table: what the host answers when a driver asks
//! for one supply to be drawn.
//!
//! `doc:backlog-self-driving` §4. [`super`] is the channel this rides on, and
//! its header states the rule it lands under. A table arrives with the verb
//! that reads it.
//!
//! # Why the words are spelled again here
//!
//! `stella_autonomy` already names all of this. It has `regress::Report` and
//! `regress::Skip`. This crate may not take that dependency. AGENTS.md's
//! workspace table makes `stella-protocol` its only workspace edge.
//!
//! So these are the wire's own names. The host maps between the two sets in
//! `crates/stella-cli/src/driver_plugin/capabilities.rs`. That map is total,
//! so the two vocabularies cannot drift apart. [`super::deliver`] carries the
//! same argument for the same reason.
//!
//! # The same facts the hand-run verb prints
//!
//! `stella self-driving sweep regress|meta` reports over the same draw, in
//! `crates/stella-cli/src/self_driving_cmd/sweep.rs`. The counts here are its
//! counts, so an operator reading the terminal and a driver reading the socket
//! learn the same thing about one sweep.
//!
//! # Why a sweep ask reads nothing
//!
//! `sweep_regress` re-checks every receipt the loop has written, and
//! `sweep_meta` folds the whole ledger. Neither is about one unit, so there is
//! nothing for an argument to name. `work_status` carries no table for the
//! same reason.

use serde::{Deserialize, Serialize};

/// Which supply a sweep drew from.
///
/// Echoed on the answer, so a driver that pipelines two asks can read one
/// report without joining it back to the ask that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SweptSupply {
    /// The closed issues this loop has claimed to fix.
    Regress,
    /// The loop's own cycle ledger.
    Meta,
}

impl SweptSupply {
    /// The name this supply is written as on the wire and in
    /// `[self_driving.supply]`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Regress => "regress",
            Self::Meta => "meta",
        }
    }
}

impl std::fmt::Display for SweptSupply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What one draw from one supply produced.
///
/// Three counts and the keys, because a driver decides what to do next from
/// them. A sweep that offered nothing means the supply is dry and the loop
/// should watch. A sweep that offered plenty and filed none means the seen set
/// is holding it all back. Those are different states and call for different
/// answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweepReport {
    /// Which supply this is a report about.
    pub supply: SweptSupply,
    /// How many findings the supply offered, before the seen set was read.
    pub offered: u64,
    /// How many of those the seen set did not already hold.
    pub fresh: u64,
    /// The tracker keys the sweep filed, as the tracker spells them.
    ///
    /// Shorter than [`Self::fresh`] when a filing was refused or read as a
    /// repeat. Both are ordinary answers, and neither is an error.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filed: Vec<String>,
    /// What the closure ledger could answer, for the supply that reads one.
    ///
    /// Absent for `sweep_meta`, which folds the cycle ledger and reads no
    /// receipts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipts: Option<SweepReceipts>,
}

/// How much of the closure ledger one `sweep_regress` draw could re-ask.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweepReceipts {
    /// Receipts on file.
    pub total: u64,
    /// Those whose cited change could be looked for on the base.
    pub checked: u64,
    /// Those that could not be, one entry per reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<SweepSkip>,
}

/// One reason receipts could not be re-checked, and how many carried it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweepSkip {
    /// Why.
    pub reason: SweepSkipReason,
    /// How many receipts this reason accounts for.
    pub count: u64,
}

/// Why one closed issue's receipt could not be re-checked.
///
/// A receipt says what a closure cited and whether that change was on the base
/// branch at the time. Missing either fact makes *gone* and *never seen* look
/// alike, and filing the wrong one of those costs a person an hour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SweepSkipReason {
    /// The closure cited no change at all.
    NoChangeCited,
    /// The host could not tell whether the change was on the base at the time.
    UnknownAtClose,
    /// The change was already absent when the issue closed.
    AbsentAtClose,
}

impl SweepSkipReason {
    /// The name this reason is written as on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoChangeCited => "no_change_cited",
            Self::UnknownAtClose => "unknown_at_close",
            Self::AbsentAtClose => "absent_at_close",
        }
    }
}

impl std::fmt::Display for SweepSkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
