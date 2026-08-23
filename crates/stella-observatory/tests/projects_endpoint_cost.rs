// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What `/api/projects` costs, with the per-row `stat` separated from the SQL
//! (#3974).
//!
//! `global::projects` decides liveness by testing
//! `<root_path>/.stella/private/store.db` per hub row, and the hub's `projects`
//! table only grows — 2338 rows on the machine that filed #3953, most of them
//! bench temp directories that are long gone. #3974 asked whether that stat
//! loop is worth bounding, and named the first step: nobody had separated it
//! from the `LEFT JOIN` it shares an endpoint with, and a fix aimed at the
//! wrong half is worse than none.
//!
//! `#[ignore]` — it is a measurement, not an assertion. `cargo test` still
//! compiles it, so it cannot rot into something that no longer builds, and
//! anyone re-asking the question runs one command instead of rebuilding a
//! harness:
//!
//! ```text
//! DEAD=3000 cargo test -p stella-observatory --release \
//!   --test projects_endpoint_cost -- --ignored --nocapture
//! ```
//!
//! **Release matters.** The 5.4 ms in #3974 is a debug build, and it is the
//! whole endpoint rather than either half.
//!
//! # What it measured (2026-08-23, M-series laptop, idle, medians of 5)
//!
//! | hub rows | whole endpoint | stat loop | SQL alone |
//! |---|---|---|---|
//! | 800 | 3.15 ms | 0.72 ms | 0.69 ms |
//! | 2338 | 9.11 ms | 4.83 ms | 3.90 ms |
//! | 3000 | 7.59 ms | 2.93 ms | 3.79 ms |
//! | 10000 | 38.55 ms | 21.93 ms | 15.64 ms |
//!
//! Both terms are roughly linear at a couple of microseconds a row, and the
//! stat loop is about half the endpoint rather than the dominant term the
//! issue expected. The 2338 and 3000 rows are out of order by a few
//! milliseconds, which is the run-to-run spread on a laptop and is why no
//! conclusion here rests on a difference that small.
//!
//! At the size that prompted the question — 2338 rows, refetched every 5 s —
//! the stat loop is 4.8 ms of a 5000 ms cycle, a 0.1% duty cycle. Bounding it
//! would cost either a liveness-aware filter (the issue rules out a recency
//! `LIMIT`: the dead bench roots are *more recent* than the live workspaces,
//! so it drops live projects out of the switcher first) or a TTL cache, which
//! puts mutable state in a module that is read-only by posture and delays a
//! new workspace appearing in the switcher. Neither is worth 0.1%, and it
//! remains true at 10000 rows.

use std::path::Path;
use std::time::Instant;

use rusqlite::Connection;
use stella_observatory::respond;

/// One hub row per dead project, plus one rollup row so the `LEFT JOIN` has
/// something to join to — the shape `projects_ships_only_selectable_rows_and_
/// counts_the_rest` seeds, at whatever size `DEAD` asks for.
fn seed_hub(usage: &Connection, dead: usize) -> Vec<String> {
    usage
        .execute_batch(
            "CREATE TABLE projects (
               project_id TEXT PRIMARY KEY, name TEXT NOT NULL,
               root_path TEXT NOT NULL, first_seen_at TEXT NOT NULL,
               last_seen_at TEXT NOT NULL);
             CREATE TABLE execution_rollup (
               project_id TEXT NOT NULL, execution_id INTEGER NOT NULL,
               kind TEXT NOT NULL, prompt_digest TEXT NOT NULL,
               prompt_preview TEXT NOT NULL DEFAULT '',
               model TEXT NOT NULL, provider TEXT NOT NULL,
               outcome TEXT NOT NULL, cost_usd REAL NOT NULL,
               input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
               duration_ms INTEGER NOT NULL, tool_calls INTEGER NOT NULL,
               files_written INTEGER NOT NULL, produced_output INTEGER NOT NULL,
               self_rating INTEGER, started_at TEXT NOT NULL,
               usage_complete INTEGER NOT NULL DEFAULT 1,
               PRIMARY KEY (project_id, execution_id));",
        )
        .unwrap();
    let tx = usage.unchecked_transaction().unwrap();
    let mut roots = Vec::with_capacity(dead);
    for i in 0..dead {
        let id = format!("{i:016x}");
        let root = format!("/var/folders/zz/gone-{i}/T/bench-{i}");
        tx.execute(
            "INSERT INTO projects VALUES (?1, ?2, ?3, '2026-01-01', ?4)",
            rusqlite::params![
                id,
                format!("bench-{i}"),
                root,
                format!("2026-02-{:02}", i % 28 + 1)
            ],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO execution_rollup \
             VALUES (?1, 1, 'run', 'd', '', 'm', 'p', 'completed', 0.1, 1, 1, 1, 1, 1, 1, \
                     NULL, '2026-01-01', 1)",
            rusqlite::params![id],
        )
        .unwrap();
        roots.push(root);
    }
    tx.commit().unwrap();
    roots
}

/// The median of five, in milliseconds — a median rather than a mean because
/// one scheduler hiccup on a laptop moves a mean and does not move this.
fn median_ms(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).expect("no NaN timings"));
    samples[samples.len() / 2]
}

#[test]
#[ignore = "a measurement, not an assertion — see the module docs"]
fn projects_endpoint_cost_by_term() {
    let dead: usize = std::env::var("DEAD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);

    let home = tempfile::TempDir::new().unwrap();
    let data = tempfile::TempDir::new().unwrap();
    let usage = Connection::open(data.path().join("usage.db")).unwrap();
    let roots = seed_hub(&usage, dead);
    drop(usage);

    // SAFETY: this binary holds exactly one test, so nothing else in the
    // process is reading the environment while it is set.
    unsafe { std::env::set_var("STELLA_DATA_DIR", data.path()) };

    // Warm the page cache and the negative dentry cache: the first pass over a
    // set of absent paths pays for discovering they are absent, and the
    // question here is the steady-state 5-second refresh.
    let _ = respond(home.path(), "/api/projects");

    let mut whole = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        let response = respond(home.path(), "/api/projects");
        whole.push(start.elapsed().as_secs_f64() * 1000.0);
        assert!(!response.body.is_empty());
    }

    // The stat loop alone, over the same roots the endpoint just tested.
    let mut stats = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        let mut live = 0u32;
        for root in &roots {
            if Path::new(root).join(".stella/private/store.db").exists() {
                live += 1;
            }
        }
        stats.push(start.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(live, 0, "every seeded root is deliberately gone");
    }

    // The SQL alone: `global::projects`'s query, every row consumed, no stat.
    let conn = Connection::open(data.path().join("usage.db")).unwrap();
    let mut sql = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        let mut stmt = conn
            .prepare(
                "SELECT p.project_id, p.name, p.root_path, p.last_seen_at,
                        coalesce(r.runs, 0), coalesce(r.cost, 0),
                        coalesce(r.input_tokens, 0), coalesce(r.output_tokens, 0)
                 FROM projects p
                 LEFT JOIN (SELECT project_id, count(*) AS runs,
                                   sum(cost_usd) AS cost,
                                   sum(input_tokens) AS input_tokens,
                                   sum(output_tokens) AS output_tokens
                            FROM execution_rollup GROUP BY project_id) r
                   ON r.project_id = p.project_id
                 ORDER BY p.last_seen_at DESC",
            )
            .unwrap();
        let mut seen = 0usize;
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            let _: String = row.get(2).unwrap();
            seen += 1;
        }
        sql.push(start.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(seen, dead);
    }

    println!("rows={dead}");
    println!("whole_endpoint_ms={:.3}", median_ms(whole));
    println!("stat_loop_ms={:.3}", median_ms(stats));
    println!("sql_only_ms={:.3}", median_ms(sql));
}
