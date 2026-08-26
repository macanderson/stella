// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Whether a run shipped anything, apart from how it ended (#2808).
//!
//! `executions.outcome` says how a run ended and nothing about whether it
//! delivered. The two come apart in both directions: a completed run can ship
//! nothing, and a cancelled one can have merged a pull request before the
//! cancel landed.
//!
//! **The column reads three ways, and the absent one is an answer.** `None` is
//! "nothing observed this run's delivery"; [`Delivery::Nothing`] is "something
//! looked, and it shipped nothing". Do not collapse them — reading an absence
//! of evidence as a record of failure is the defect this column exists to fix.
//! Most rows are `None`, because only a door that can see its own commits
//! writes here.
//!
//! Not `execution_reflection.delivered`, which is the model's self-report
//! about its turn. This is an observation.

use rusqlite::OptionalExtension as _;
use rusqlite::params;

use crate::{Result, Store};

/// What a run shipped, as observed by the door that ran it.
///
/// The *absence* of a value is a third answer and is spelled `Option::None` by
/// every reader — see the module doc.
///
/// A token is declared when a door writes it, not when one can be imagined.
/// Both of these are written by the fleet attempt close; a pull-request or
/// merge token waits for a door that can join a pull request to the execution
/// that opened it, which `pull_requests` cannot do today — it is keyed by URL
/// and linked to a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// The run's delivery was observed and it shipped nothing.
    Nothing,
    /// The run landed at least one commit.
    Commits,
}

impl Delivery {
    /// The stored token. Closed set; `parse` is its inverse.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nothing => "none",
            Self::Commits => "commits",
        }
    }

    /// Read a stored token back.
    ///
    /// `None` for anything this build does not know. That is not a silent
    /// widening: the only writer is [`Store::record_delivery`], and a file
    /// written by a newer build is refused at open by
    /// [`crate::StoreError::SchemaTooNew`] before any read gets here — so an
    /// unrecognised token means a hand-edited database, and reading it as
    /// "unobserved" is the safe direction.
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "none" => Some(Self::Nothing),
            "commits" => Some(Self::Commits),
            _ => None,
        }
    }

    /// Whether this reading says the run shipped something.
    pub fn shipped(self) -> bool {
        matches!(self, Self::Commits)
    }
}

impl Store {
    /// Record what a run shipped.
    ///
    /// Separate from [`Store::finish_execution_accounted`] rather than another
    /// parameter on it, because the two facts are observed by different code at
    /// different moments: every door can name an outcome, and only some can see
    /// a delivery. A door that cannot leaves the column `NULL`, which is a
    /// different answer from [`Delivery::Nothing`] and must stay one.
    ///
    /// Writing is idempotent and last-writer-wins; a run's delivery is observed
    /// once, at its close.
    pub fn record_delivery(&self, execution_id: i64, delivery: Delivery) -> Result<()> {
        self.lock().execute(
            "UPDATE executions SET delivery = ?2 WHERE id = ?1",
            params![execution_id, delivery.as_str()],
        )?;
        Ok(())
    }

    /// What a run shipped, or `None` when nothing observed it.
    ///
    /// `None` also covers an execution id that names no row — both are "this
    /// store cannot say", and a caller that needs to tell a missing run from an
    /// unobserved one is asking a question about the `executions` spine rather
    /// than about delivery.
    pub fn delivery(&self, execution_id: i64) -> Result<Option<Delivery>> {
        let token: Option<Option<String>> = self
            .lock()
            .query_row(
                "SELECT delivery FROM executions WHERE id = ?1",
                params![execution_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(token.flatten().as_deref().and_then(Delivery::parse))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_run(outcome: &str) -> (Store, i64) {
        let store = Store::in_memory().expect("store");
        let id = store
            .begin_execution("fleet", "fix the router", "zai", "glm-5.2")
            .expect("open a run");
        store
            .finish_execution(id, outcome, 5.29)
            .expect("close the run");
        (store, id)
    }

    /// **The witness.** A cancelled run that shipped a merged pull request is
    /// not the same record as a cancelled run that shipped nothing, and neither
    /// is the same as one nobody looked at. Before this column the three were
    /// one row: `outcome = 'cancelled'`.
    #[test]
    fn how_a_run_ended_and_whether_it_shipped_are_two_separate_readings() {
        let (shipped, shipped_id) = store_with_run("cancelled");
        shipped
            .record_delivery(shipped_id, Delivery::Commits)
            .expect("record");

        let (empty, empty_id) = store_with_run("cancelled");
        empty
            .record_delivery(empty_id, Delivery::Nothing)
            .expect("record");

        let (unobserved, unobserved_id) = store_with_run("cancelled");

        assert_eq!(
            shipped.delivery(shipped_id).expect("read"),
            Some(Delivery::Commits),
            "a cancelled run that landed commits must read as having shipped"
        );
        assert_eq!(
            empty.delivery(empty_id).expect("read"),
            Some(Delivery::Nothing),
            "a cancelled run that shipped nothing must say so, not stay silent"
        );
        assert_eq!(
            unobserved.delivery(unobserved_id).expect("read"),
            None,
            "a run nobody observed must not read as having shipped nothing"
        );
        assert!(Delivery::Commits.shipped());
        assert!(!Delivery::Nothing.shipped());
    }

    /// Recording delivery does not disturb the outcome, and closing a run does
    /// not disturb the delivery. Two columns, two writers, no ordering rule
    /// between them.
    #[test]
    fn the_two_facts_do_not_overwrite_each_other() {
        let (store, id) = store_with_run("cancelled");
        store
            .record_delivery(id, Delivery::Commits)
            .expect("record");
        store
            .finish_execution(id, "cancelled", 5.29)
            .expect("close again");
        assert_eq!(store.delivery(id).expect("read"), Some(Delivery::Commits));
    }

    /// Every token round-trips, and nothing else parses.
    #[test]
    fn the_token_set_is_closed_and_round_trips() {
        for delivery in [Delivery::Nothing, Delivery::Commits] {
            assert_eq!(Delivery::parse(delivery.as_str()), Some(delivery));
        }
        assert_eq!(Delivery::parse("merged"), None);
        assert_eq!(Delivery::parse(""), None);
    }

    /// An execution id that names no row reads as unobserved rather than
    /// erroring — see the reader's own doc for why that is the right shape.
    #[test]
    fn an_unknown_execution_reads_as_unobserved() {
        let store = Store::in_memory().expect("store");
        assert_eq!(store.delivery(9_999).expect("read"), None);
    }
}
