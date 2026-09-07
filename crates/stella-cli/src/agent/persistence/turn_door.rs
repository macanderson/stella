// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! [`TurnDoor`] — who asked for a turn, on whose behalf, and what they want
//! told about it.
//!
//! Split out of [`super`] when that file crossed the 1500-line guard (#3552's
//! observer is what pushed it over). The cut follows the concern rather than
//! the line count: everything else in `persistence` *writes* — execution rows,
//! events, the run terminator — while this is a plain value threaded **into**
//! a turn, carrying no store and touching no telemetry.

use std::sync::Arc;

use stella_store::Store;

use crate::turn_facts::TurnFacts;
use crate::turn_row::TurnRow;

/// Which door a turn came in by, and which wrapper — if any — ran over it.
///
/// The two facts #3388 split into two columns, travelling together so a caller
/// cannot supply one and forget the other. `kind` is the **command the user
/// ran** and nothing else; `variant` is the wrapper, and it is written *only
/// when that manifest was the thing that ran*. A raw turn with no wrapper over
/// it leaves the column NULL, which is a real answer ("no wrapper"), not a gap.
#[derive(Debug, Clone)]
pub(crate) struct TurnDoor<'a> {
    /// The door — `run`, `chat`, `deck`, …
    pub(crate) kind: &'a str,
    /// The wrapper variant that ran over this turn, if one did.
    pub(crate) variant: Option<&'a str>,
    /// What the caller wants told about the turn it is asking for, when it
    /// wants anything. `None` — every door but the wrapper driver's — installs
    /// no tap and pays nothing.
    ///
    /// It rides on the door rather than as a parameter of its own because the
    /// door is already the value that says *who is asking for this turn and on
    /// whose behalf*, and because `crate::agent::run_turn` lives in a file
    /// close to the 1500-line ratchet: threading a fact through a value that
    /// is already threaded costs that file no lines (AGENTS.md
    /// § "God files").
    pub(crate) facts: Option<TurnFacts>,
    /// Where the turn reports the execution row it opens, for a caller with
    /// something to add to that row once the turn is over. `None` — every
    /// door but the wrapper driver's — records nothing.
    ///
    /// It rides on the door for `facts`' reason above, and it is the other
    /// half of the same seam: `facts` is what the turn's events say, and this
    /// is where they were written.
    pub(crate) row: Option<TurnRow>,
}

impl<'a> TurnDoor<'a> {
    /// A turn with no wrapper over it.
    pub(crate) fn new(kind: &'a str) -> Self {
        Self {
            kind,
            variant: None,
            facts: None,
            row: None,
        }
    }

    /// The same door, recording the wrapper plugin that ran over the turn.
    ///
    /// Named for what it asserts: the variant is written because this manifest
    /// *ran*, never because it was installed or selected and then declined.
    pub(crate) fn wrapped_by(self, variant: &'a str) -> Self {
        Self {
            variant: Some(variant),
            ..self
        }
    }

    /// The same door, folding this turn's tools and file changes into `facts`
    /// as its events go past (#3552).
    pub(crate) fn reporting_to(self, facts: TurnFacts) -> Self {
        Self {
            facts: Some(facts),
            ..self
        }
    }

    /// The same door, reporting the execution row the turn opens into `row`.
    ///
    /// `stella run --pipeline <variant>` is the caller: its wrapper decides a
    /// verdict after the last round's turn has closed its own channel, so the
    /// row is the only way left to reach the journal that verdict belongs in
    /// (`crate::turn_row`).
    pub(crate) fn recording_to(self, row: TurnRow) -> Self {
        Self {
            row: Some(row),
            ..self
        }
    }

    /// Tell whoever asked for this turn which execution row it opened.
    ///
    /// Nothing to do for a door that asked for nothing, which is every door
    /// but the one above.
    pub(crate) fn opened(&self, execution: Option<&(Arc<Store>, i64)>) {
        if let Some(row) = &self.row {
            row.opened(execution);
        }
    }

    /// Wrap the turn's event sender with whatever observer this door asked for.
    ///
    /// Identity when it asked for none, so a door that wants no report pays no
    /// closure — this is on the send path of every event of every turn.
    pub(crate) fn observing(&self, events: stella_core::EventSender) -> stella_core::EventSender {
        match &self.facts {
            Some(facts) => facts.observing(events),
            None => events,
        }
    }
}
