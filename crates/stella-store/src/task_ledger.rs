// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The other two thirds of SPEC 7.1's task: its **evidence ledger** and its
//! **cost**, both read out of `events.task_id` (#5039).
//!
//! [`crate::task_board`] mirrors what a task *is* — subject, status, owner,
//! and since #4238 what it promised. It cannot say what the task did or what
//! it cost, because a board row is state and both of those are history. The
//! history was always in `events`, one row per stream position; what was
//! missing was the column saying which task a row belongs to.
//!
//! So there is no new store here. [`Store::task_events`] is a selection and
//! [`Store::session_task_costs`] is a fold, both over that one column.
//!
//! Do not read a task's rows as bounded by its board row. `/clear` deletes a
//! session's `tasks` mirror and leaves the journal alone, because the journal
//! is the audit trail and the mirror is not ([`crate::task_board`] makes that
//! call), so the cost fold is keyed on the *tags it finds* and the board only
//! supplies status where it still has a row.

use rusqlite::params;
use stella_protocol::{AgentEvent, TaskId, TaskStatus};

use crate::{Result, SessionEventRecord, SessionJournal, Store};

/// One task's cost, in the shape SPEC 7.1 names: `$ · tok · cache rd% · model
/// calls · est remain`.
///
/// Four of the five clauses are measured and one is projected, and they are
/// kept apart rather than presented as one number of mixed provenance.
///
/// - `$` is [`Self::spent_usd`], `model calls` is [`Self::model_calls`], `tok`
///   is [`Self::total_tokens`].
/// - `cache rd%` is **not a field**. This crate does not depend on
///   `stella-model`, where the one definition of that ratio lives
///   (`cache_economics::hit_rate`, cached input over total input), and a
///   second spelling of it here would be one rule in two places — what
///   `stella_protocol::event::MODEL_CALL_FAILED_PREFIX`'s doc argues against
///   at length. The two counts it is computed from ([`Self::input_tokens`],
///   [`Self::cached_input_tokens`]) are carried instead, so a renderer that
///   already links `stella-model` reads the percentage from the shared rule
///   and nothing has to agree with anything.
/// - `est remain` is [`Self::estimated_remaining_usd`], and it is `Option`
///   because this is where SPEC 6.1's `det %` went wrong: it was specified, it
///   had no source, and it was dropped rather than fabricated (see
///   `stella_protocol::task_contract`). An estimate here does have a source —
///   this session's own finished tasks — and where that source is empty the
///   answer is `None`, not a guess. [`Store::session_task_costs`] carries the
///   derivation.
///
/// There is no `det %` clause and nothing here reintroduces one.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskCost {
    /// The board task this is the cost of.
    pub task_id: TaskId,
    /// Dollars this task's model calls are known to have cost.
    ///
    /// A **lower bound** whenever [`Self::unaccounted_calls`] is non-zero;
    /// [`Self::is_lower_bound`] is the question rather than a comparison a
    /// caller has to remember to make.
    pub spent_usd: f64,
    /// Prompt tokens billed to this task, cached and uncached together — the
    /// denominator of `cache rd%`.
    pub input_tokens: u64,
    /// The cached share of [`Self::input_tokens`] — its numerator.
    pub cached_input_tokens: u64,
    /// Completion tokens billed to this task.
    pub output_tokens: u64,
    /// Committed model calls attributed to this task: SPEC 7.1's `model
    /// calls`.
    pub model_calls: u64,
    /// Dispatched calls that died without a usage envelope, so their spend is
    /// partly or wholly unknowable (`usage_incomplete`).
    ///
    /// Counted apart from [`Self::model_calls`] rather than added to it: they
    /// are not calls that landed, and folding them in would make the ratio
    /// `$ / model calls` describe a different population from the one it
    /// names. What they do change is the confidence of the total — see
    /// [`Self::is_lower_bound`].
    pub unaccounted_calls: u64,
    /// SPEC 7.1's `est remain`, or `None` when nothing measured can support
    /// one. See [`Store::session_task_costs`] for the derivation and for what
    /// each answer means.
    pub estimated_remaining_usd: Option<f64>,
}

impl TaskCost {
    /// SPEC 7.1's `tok`: everything billed, prompt and completion together.
    ///
    /// Cache *writes* are excluded, matching the events this is folded from:
    /// they are reported outside `input_tokens` by the providers that report
    /// them at all, so including them would make one task's `tok` mean a
    /// different thing depending on which vendor served it.
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    /// Whether [`Self::spent_usd`] is a floor rather than a total.
    ///
    /// `true` when at least one dispatched call died without accounting for
    /// itself. A surface that renders a bare number here is claiming a
    /// precision the store does not have, which is the failure #4147 named one
    /// scope up: "incomplete" is not "unknown", but it is not "complete"
    /// either.
    #[must_use]
    pub fn is_lower_bound(&self) -> bool {
        self.unaccounted_calls > 0
    }

    /// An empty cost — a task that exists and has spent nothing yet.
    fn zero(task_id: TaskId) -> Self {
        Self {
            task_id,
            spent_usd: 0.0,
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            model_calls: 0,
            unaccounted_calls: 0,
            estimated_remaining_usd: None,
        }
    }

    /// Fold one metering event into this task's totals.
    ///
    /// Only the two accounting cases contribute. A `tool_result` is evidence
    /// (it is in [`Store::task_events`]) and costs nothing on its own, so
    /// counting it here would double-report the model call that requested it.
    fn absorb(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::StepUsage {
                cost_usd,
                input_tokens,
                output_tokens,
                cached_input_tokens,
                ..
            } => {
                self.spent_usd += *cost_usd;
                self.input_tokens = self.input_tokens.saturating_add(*input_tokens);
                self.cached_input_tokens = self
                    .cached_input_tokens
                    .saturating_add(*cached_input_tokens);
                self.output_tokens = self.output_tokens.saturating_add(*output_tokens);
                self.model_calls += 1;
            }
            AgentEvent::UsageIncomplete { partial, .. } => {
                self.unaccounted_calls += 1;
                // Whatever the adapter had already been told still counts.
                // Dropping it would make the floor lower than the store's own
                // evidence supports, which is a different kind of wrong from
                // the one the flag is warning about.
                if let Some(partial) = partial {
                    self.spent_usd += partial.cost_usd;
                    self.input_tokens =
                        self.input_tokens.saturating_add(partial.usage.input_tokens);
                    self.cached_input_tokens = self
                        .cached_input_tokens
                        .saturating_add(partial.usage.cached_input_tokens);
                    self.output_tokens = self
                        .output_tokens
                        .saturating_add(partial.usage.output_tokens);
                }
            }
            _ => {}
        }
    }
}

impl Store {
    /// A task's **evidence ledger**: every event tagged with `task_id` in this
    /// session, in stream order across every execution the session ran.
    ///
    /// Ordered by `(execution_id, seq)` for the reason
    /// [`Store::session_events`] gives — execution ids are AUTOINCREMENT, so
    /// that is turn order then stream order within a turn — and a row whose
    /// payload no longer parses is skipped and counted rather than failing the
    /// read, on the same terms. A task with no tagged events reads as an empty
    /// journal, which is what a task that has not started is.
    pub fn task_events(&self, session_id: &str, task_id: &TaskId) -> Result<SessionJournal> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT e.execution_id, e.seq, e.ts, e.payload FROM events e \
             JOIN executions x ON x.id = e.execution_id \
             WHERE x.session_id = ? AND e.task_id = ? \
             ORDER BY e.execution_id ASC, e.seq ASC",
        )?;
        let rows = stmt.query_map(params![session_id, task_id.as_str()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut journal = SessionJournal::default();
        for row in rows {
            let (execution_id, seq, ts, payload) = row?;
            match serde_json::from_str::<AgentEvent>(&payload) {
                Ok(event) => journal.events.push(SessionEventRecord {
                    execution_id,
                    seq,
                    ts,
                    event,
                }),
                Err(_) => journal.skipped += 1,
            }
        }
        Ok(journal)
    }

    /// Every task in this session that has a board row or a tagged event, with
    /// its cost, ordered by the board's ordinal (`"10"` after `"2"`).
    ///
    /// # Why the whole session at once
    ///
    /// `est remain` is the reason. A single task cannot estimate its own
    /// remainder from its own history — spend so far says nothing about how
    /// much is left — so the estimate is drawn from **this session's finished
    /// tasks**: the mean of what tasks that reached
    /// [`TaskStatus::Completed`] actually cost, minus what this task has spent
    /// already, floored at zero. That population only exists at session scope,
    /// so computing one task's estimate means reading them all, and doing it
    /// once for the whole board is both cheaper and the only way two rows of
    /// the same panel cannot disagree about the mean.
    ///
    /// The three answers, and what each means:
    ///
    /// - `Some(0.0)` on a task the board reports as terminal — completed or
    ///   cancelled. Nothing more will be spent on it. That is a fact, not an
    ///   estimate, and it is why a finished task's line reads as finished.
    /// - `Some(n)` on an open task, once at least one sibling has completed.
    ///   It is an extrapolation and it says so by being an estimate; what
    ///   makes it legitimate rather than the `det %` this spec dropped for
    ///   having no source is that it is derived from measurements this session
    ///   took, not from a constant somebody chose.
    /// - `None` when no sibling has completed yet, or when the board no longer
    ///   has a row for this task (a `/clear` took the mirror and left the
    ///   journal). There is nothing to extrapolate from, and an invented
    ///   number on a receipt is worse than an absent one.
    pub fn session_task_costs(&self, session_id: &str) -> Result<Vec<TaskCost>> {
        let mut costs: Vec<TaskCost> = Vec::new();
        // Board order first, so the common case comes back in the order a
        // panel renders it and the ordinal sort below has nothing to do.
        let board = self.list_session_tasks(session_id)?;
        let mut status: Vec<(String, TaskStatus)> = Vec::with_capacity(board.len());
        for item in &board {
            status.push((item.id.clone(), item.status));
            costs.push(TaskCost::zero(TaskId::new(item.id.clone())));
        }

        for (tag, event) in self.tagged_metering_events(session_id)? {
            let cost = match costs.iter_mut().find(|c| c.task_id == tag) {
                Some(existing) => existing,
                None => {
                    // A tag whose board row is gone. Its evidence is still the
                    // audit trail, so it gets a row; with no status it gets no
                    // estimate (see the doc above).
                    costs.push(TaskCost::zero(tag));
                    costs.last_mut().expect("just pushed")
                }
            };
            cost.absorb(&event);
        }

        let completed: Vec<f64> = costs
            .iter()
            .filter(|c| {
                status
                    .iter()
                    .any(|(id, s)| c.task_id == **id && *s == TaskStatus::Completed)
            })
            .map(|c| c.spent_usd)
            .collect();
        let mean_completed =
            (!completed.is_empty()).then(|| completed.iter().sum::<f64>() / completed.len() as f64);
        for cost in &mut costs {
            let state = status
                .iter()
                .find(|(id, _)| cost.task_id == **id)
                .map(|(_, s)| *s);
            cost.estimated_remaining_usd = match state {
                Some(status) if !status.is_open() => Some(0.0),
                Some(_) => mean_completed.map(|mean| (mean - cost.spent_usd).max(0.0)),
                None => None,
            };
        }

        // Ordinal, then lexical — the same order `list_session_tasks` returns
        // its rows in, extended to the tags it had no row for.
        costs.sort_by(|a, b| {
            let key = |c: &TaskCost| {
                (
                    c.task_id.as_str().parse::<u64>().ok(),
                    c.task_id.as_str().to_string(),
                )
            };
            key(a).cmp(&key(b))
        });
        Ok(costs)
    }

    /// One task's cost, or `None` when the session has neither a board row nor
    /// a tagged event for it.
    ///
    /// Through [`Store::session_task_costs`] rather than a narrower query,
    /// because `est remain` is a session-scoped derivation and a per-task
    /// shortcut would have to either recompute it or leave it out — and a
    /// field that is present here and absent there is the kind of difference
    /// nobody notices until a panel and a report disagree.
    pub fn task_cost(&self, session_id: &str, task_id: &TaskId) -> Result<Option<TaskCost>> {
        Ok(self
            .session_task_costs(session_id)?
            .into_iter()
            .find(|c| c.task_id == *task_id))
    }

    /// The tagged accounting events of one session, as `(tag, event)` pairs.
    ///
    /// Narrowed in SQL to the two metering cases: a session's `events` table
    /// holds every tool result and every block registration too, and folding a
    /// cost would otherwise mean deserializing all of them to ignore most.
    fn tagged_metering_events(&self, session_id: &str) -> Result<Vec<(TaskId, AgentEvent)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT e.task_id, e.payload FROM events e \
             JOIN executions x ON x.id = e.execution_id \
             WHERE x.session_id = ? AND e.task_id IS NOT NULL \
               AND e.event_type IN ('step_usage', 'usage_incomplete') \
             ORDER BY e.execution_id ASC, e.seq ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (tag, payload) = row?;
            // A payload that no longer parses is skipped rather than fatal,
            // exactly as `session_events` skips one: a single corrupt row must
            // not hide a whole task's cost. It does make the total a floor,
            // which is the same statement `unaccounted_calls` makes.
            if let Ok(event) = serde_json::from_str::<AgentEvent>(&payload) {
                out.push((TaskId::new(tag), event));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests;
