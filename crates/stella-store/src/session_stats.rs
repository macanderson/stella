// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a session did, as numbers: the per-session rollup the SESSIONS
//! overlay shows beside each row.
//!
//! Read off `executions` alone — one row per turn, stamped with its
//! `session_id` since schema v8 — so the answer is a count and a sum rather
//! than a journal replay, and a registry of forty sessions is priced at forty
//! indexed queries.

use rusqlite::{OptionalExtension, params};

use crate::{Result, Store};

/// One session's rollup.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionStats {
    /// Turns recorded — executions stamped with the session id.
    pub turns: u32,
    /// Spend across those turns, USD.
    pub cost_usd: f64,
    /// The model the latest turn ran on.
    pub model: Option<String>,
    /// The prompts, oldest first, capped at [`PROMPT_SAMPLE`] — what a
    /// one-sentence description of the session is written from.
    pub prompts: Vec<String>,
}

/// Most prompts [`SessionStats::prompts`] carries.
pub const PROMPT_SAMPLE: usize = 8;

impl Store {
    /// The rollup for `session_id`. A session the store never saw reads as
    /// zero turns, which is the truth and not an error.
    pub fn session_stats(&self, session_id: &str) -> Result<SessionStats> {
        let conn = self.lock();
        let (turns, cost_usd): (i64, f64) = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(cost_usd), 0) FROM executions WHERE session_id = ?1",
            params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let model: Option<String> = conn
            .query_row(
                "SELECT model FROM executions WHERE session_id = ?1 ORDER BY id DESC LIMIT 1",
                params![session_id],
                |r| r.get(0),
            )
            .optional()?;
        let mut stmt = conn
            .prepare("SELECT prompt FROM executions WHERE session_id = ?1 ORDER BY id LIMIT ?2")?;
        let prompts = stmt
            .query_map(params![session_id, PROMPT_SAMPLE as i64], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(SessionStats {
            turns: u32::try_from(turns).unwrap_or(u32::MAX),
            cost_usd,
            model: model.filter(|m| !m.is_empty()),
            prompts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sessions_turns_spend_and_model_roll_up_from_its_executions() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let first = store
            .begin_execution("run", "fix the parser", "zai", "glm-5.2")
            .unwrap();
        store.set_execution_session(first, "ses-a").unwrap();
        store
            .finish_execution_accounted(first, "success", 0.25, true)
            .unwrap();
        let second = store
            .begin_execution("run", "add a test", "zai", "glm-5.2-air")
            .unwrap();
        store.set_execution_session(second, "ses-a").unwrap();
        store
            .finish_execution_accounted(second, "success", 0.05, true)
            .unwrap();
        let other = store
            .begin_execution("run", "unrelated", "zai", "glm-5.2")
            .unwrap();
        store.set_execution_session(other, "ses-b").unwrap();

        let stats = store.session_stats("ses-a").unwrap();
        assert_eq!(stats.turns, 2);
        assert!((stats.cost_usd - 0.30).abs() < 1e-9, "{}", stats.cost_usd);
        assert_eq!(stats.model.as_deref(), Some("glm-5.2-air"));
        assert_eq!(stats.prompts, vec!["fix the parser", "add a test"]);

        let none = store.session_stats("ses-never").unwrap();
        assert_eq!(none, SessionStats::default());
    }
}
