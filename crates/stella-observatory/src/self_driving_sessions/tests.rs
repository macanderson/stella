//! The fold's tests. Each one seeds a journal on disk. Each one reads it back
//! through [`sessions_from`], the way the route does.

use super::*;

fn line(at: &str, action: &str, subject: Option<&str>, outcome: &str) -> String {
    let subj = subject.map_or("null".to_string(), |s| format!("\"{s}\""));
    format!(
        r#"{{"at":"{at}","run_id":"unassigned","action":"{action}","subject":{subj},"outcome":"{outcome}"}}"#
    )
}

fn seed(root: &Path, slug: &str, lines: &[String], queue: Option<&str>) -> PathBuf {
    let workspace = PathBuf::from(format!("/nowhere/{slug}"));
    seed_for(root, slug, lines, queue, &workspace)
}

/// [`seed`], with the loop pointed at a real directory. The fold then reads
/// that workspace's `store.db`, `context.db` and `fleet.db` too.
fn seed_for(
    root: &Path,
    slug: &str,
    lines: &[String],
    queue: Option<&str>,
    workspace: &Path,
) -> PathBuf {
    let dir = root.join("self-driving").join(slug);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("audit.jsonl"), lines.join("\n") + "\n").unwrap();
    std::fs::write(
        dir.join("workspace.json"),
        json!({ "roots": [workspace.to_string_lossy()], "slug": slug }).to_string(),
    )
    .unwrap();
    if let Some(q) = queue {
        std::fs::write(dir.join("queue.json"), q).unwrap();
    }
    dir
}

/// One of the workspace's private databases. It carries just the columns the
/// fold reads. The DDL is written by hand for the same reason the queries are:
/// this crate links none of the crates that own these files. See the crate
/// README. `tests/schema_conformance.rs` is what checks the queries against
/// the real migration path.
fn private_db(workspace: &Path, name: &str, ddl: &str) -> Connection {
    let dir = workspace.join(".stella").join("private");
    std::fs::create_dir_all(&dir).unwrap();
    let conn = Connection::open(dir.join(name)).unwrap();
    conn.execute_batch(ddl).unwrap();
    conn
}

/// A `pr_opened` outcome. The fold reads the issue number out of it. Built
/// rather than spelled, so the file itself carries no issue number.
fn pr_opened_for(issue: &str) -> String {
    format!("opened for #{issue} — ci in progress")
}

/// An episode summary that names its issue. This is the shape the fold reads
/// a tag out of.
fn episode_naming(issue: &str) -> String {
    format!("Issue #{issue}: name the tagged one")
}

#[test]
fn timestamps_in_both_dialects_parse_to_the_same_instant() {
    assert_eq!(
        parse_unix("2026-08-23T04:32:50Z"),
        parse_unix("2026-08-23 04:32:50")
    );
    assert_eq!(parse_unix("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(parse_unix("1970-01-02T00:00:01Z"), Some(86_401));
    assert_eq!(parse_unix("not a date"), None);
}

/// The journal that exists today: `run_id: "unassigned"` on every line and
/// no session id. Two `session_started` lines in the same second are one
/// launch; the next `session_started` is a new session.
#[test]
fn legacy_records_fold_into_sessions_by_session_started() {
    let lines = [
        line(
            "2026-08-23T04:20:02Z",
            "session_started",
            None,
            "session began — up to 10 issue(s)",
        ),
        line(
            "2026-08-23T04:20:02Z",
            "session_started",
            None,
            "changes will be proved here",
        ),
        line(
            "2026-08-23T04:22:38Z",
            "claimed",
            Some("1180"),
            "taken off the ranked queue",
        ),
        line(
            "2026-08-23T04:22:40Z",
            "work_started",
            Some("1180"),
            "began work — P1: drizzle mock",
        ),
        line(
            "2026-08-23T04:26:28Z",
            "session_started",
            None,
            "session began — up to 10 issue(s)",
        ),
        line(
            "2026-08-23T04:32:43Z",
            "claimed",
            Some("1180"),
            "taken off the ranked queue",
        ),
        line(
            "2026-08-23T04:46:41Z",
            "work_changed",
            Some("1180"),
            "the turn left changes",
        ),
        line(
            "2026-08-23T04:48:04Z",
            "pr_opened",
            Some("1186"),
            &pr_opened_for("1180"),
        ),
        line(
            "2026-08-23T04:48:55Z",
            "pr_observed",
            Some("1186"),
            "ci=Green -> Wait",
        ),
        line(
            "2026-08-23T04:49:40Z",
            "pr_observed",
            Some("1186"),
            "ci=Green -> Wait",
        ),
        line("2026-08-23T05:00:00Z", "pr_merged", Some("1186"), "merged"),
        line(
            "2026-08-23T05:00:01Z",
            "session_stopped",
            None,
            "reached the bound",
        ),
    ];
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), "demo", &lines, None);

    let out = sessions_from(
        &[tmp.path().join("self-driving")],
        Path::new("/nowhere/demo"),
        parse_unix("2026-08-23T05:01:00Z").unwrap(),
    );
    let s = out["sessions"].as_array().unwrap();
    assert_eq!(s.len(), 2, "two launches, two sessions: {s:?}");
    // Newest first.
    assert_eq!(s[0]["status"], "stopped");
    assert_eq!(s[0]["prs_merged"], 1);
    assert_eq!(s[0]["claimed"], 1);
    let issue = &s[0]["issues"][0];
    assert_eq!(issue["number"], "1180");
    assert_eq!(
        issue["pr"], "1186",
        "pr_opened's outcome links the PR to its issue"
    );
    assert_eq!(issue["outcome"], "merged");
    assert_eq!(issue["polls"], 2, "pr_observed polls collapse to a count");
    assert_eq!(issue["title"], "P1: drizzle mock");
    // The first launch never stopped and its journal is stale.
    assert_eq!(s[1]["status"], "lost");
    assert_eq!(s[1]["issues"][0]["outcome"], "working");
    assert_eq!(out["totals"]["merged"], 1);
    assert_eq!(out["loops"][0]["is_current_workspace"], true);
}

/// A session-stamped journal (the writer after this change) groups by id,
/// and a recorded pid decides liveness rather than the journal's age.
#[test]
fn stamped_records_group_by_session_id_and_a_dead_pid_reads_crashed() {
    let stamped = |at: &str, action: &str, sid: &str, pid: u32| {
        format!(
            r#"{{"at":"{at}","run_id":"{sid}","session_id":"{sid}","pid":{pid},"action":"{action}","subject":null,"outcome":""}}"#
        )
    };
    // Interleaved: two drives against one repo at once.
    let lines = [
        stamped(
            "2026-08-23T04:20:02Z",
            "session_started",
            "sd-a",
            4_000_000_000,
        ),
        stamped(
            "2026-08-23T04:20:03Z",
            "session_started",
            "sd-b",
            4_000_000_001,
        ),
        stamped("2026-08-23T04:21:00Z", "claimed", "sd-b", 4_000_000_001),
        stamped("2026-08-23T04:22:00Z", "claimed", "sd-a", 4_000_000_000),
    ];
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), "demo", &lines, None);
    let out = sessions_from(
        &[tmp.path().join("self-driving")],
        Path::new("/x"),
        parse_unix("2026-08-23T04:23:00Z").unwrap(),
    );
    let s = out["sessions"].as_array().unwrap();
    assert_eq!(s.len(), 2);
    for row in s {
        // A pid past pid_t cannot be alive, so both read crashed even though
        // their journals are seconds old — the pid is the witness now.
        assert_eq!(row["status"], "crashed", "{row}");
        assert_eq!(row["liveness"], "pid gone, no stop record");
    }
    assert_eq!(out["totals"]["running"], 0);
}

/// The queue is the loop's own snapshot, ranked, with the issues a running
/// session holds marked in progress.
#[test]
fn queue_snapshot_is_ranked_and_overlaid_with_live_claims() {
    let lines = [
        line(
            "2026-08-23T04:20:02Z",
            "session_started",
            None,
            "session began",
        ),
        line(
            "2026-08-23T04:22:38Z",
            "claimed",
            Some("1180"),
            "taken off the ranked queue",
        ),
        line(
            "2026-08-23T04:22:40Z",
            "work_started",
            Some("1180"),
            "began work — P1: x",
        ),
    ];
    let queue = r#"{"at":"2026-08-23T04:22:00Z","open_total":7,"untriaged":2,
        "items":[{"number":1180,"title":"x","rank":"P1"},{"number":1182,"title":"y","rank":"P1"},
                 {"number":1190,"title":"z","rank":"untriaged"}]}"#;
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), "demo", &lines, Some(queue));
    let out = sessions_from(
        &[tmp.path().join("self-driving")],
        Path::new("/x"),
        parse_unix("2026-08-23T04:23:00Z").unwrap(),
    );
    assert_eq!(
        out["totals"]["running"], 1,
        "a recent legacy journal reads running"
    );
    assert_eq!(out["totals"]["busy"], 1);
    let items = out["loops"][0]["queue"]["items"].as_array().unwrap();
    assert_eq!(items[0]["in_progress"], true);
    assert_eq!(items[1]["in_progress"], false);
    assert_eq!(out["queue_by_rank"]["P1"], 2);
    assert_eq!(out["queue_by_rank"]["untriaged"], 1);
}

#[test]
fn an_unknown_session_is_an_empty_object() {
    assert_eq!(session_detail("no-such-session"), json!({}));
}

/// Two drives against one repo at once is a case the loop supports. Spend per
/// session is a window sum over a cost vector nothing splits. So both sessions
/// count the whole overlap. Adding those two figures up billed the machine
/// twice for one dollar. The tile read $20; the chart under it read $10.
#[test]
fn overlapping_sessions_do_not_bill_one_dollar_twice() {
    let workspace = tempfile::tempdir().unwrap();
    let store = private_db(
        workspace.path(),
        "store.db",
        "CREATE TABLE executions (id INTEGER PRIMARY KEY, started_at TEXT, cost_usd REAL);",
    );
    store
        .execute(
            "INSERT INTO executions VALUES (1, '2026-08-23 04:22:00', 10.0)",
            [],
        )
        .unwrap();

    let lines = [
        line("2026-08-23T04:20:02Z", "session_started", None, "first"),
        line("2026-08-23T04:21:00Z", "session_started", None, "second"),
    ];
    let tmp = tempfile::tempdir().unwrap();
    seed_for(tmp.path(), "demo", &lines, None, workspace.path());
    let out = sessions_from(
        &[tmp.path().join("self-driving")],
        Path::new("/x"),
        parse_unix("2026-08-23T04:23:00Z").unwrap(),
    );

    let sessions = out["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2, "{out}");
    for s in sessions {
        assert_eq!(
            s["spend_usd"], 10.0,
            "each session's window covers the whole execution: {s}"
        );
    }
    assert_eq!(out["daily"][0]["spend_usd"], 10.0, "{out}");
    assert_eq!(
        out["totals"]["spend_usd"], 10.0,
        "the machine spent ten dollars, once: {out}"
    );
}

/// An episode that named the second issue was counted against the first as
/// well. The tag and the time window were OR'd, and the first issue's window
/// was still open. One session claiming two issues in a row is the plain
/// case.
#[test]
fn an_episode_tagged_to_one_issue_is_not_counted_against_another() {
    let workspace = tempfile::tempdir().unwrap();
    let context = private_db(
        workspace.path(),
        "context.db",
        "CREATE TABLE episode (public_id TEXT, summary TEXT, outcome TEXT,
                               started_at TEXT, ended_at TEXT);",
    );
    let tagged = episode_naming("101");
    for (id, summary, at) in [
        ("ep-1", tagged.as_str(), "2026-08-23 04:10:30"),
        (
            "ep-2",
            "a turn that named no issue at all",
            "2026-08-23 04:05:00",
        ),
    ] {
        context
            .execute(
                "INSERT INTO episode VALUES (?1, ?2, 'ok', ?3, ?3)",
                rusqlite::params![id, summary, at],
            )
            .unwrap();
    }

    let lines = [
        line("2026-08-23T04:00:00Z", "session_started", None, "began"),
        line("2026-08-23T04:00:02Z", "claimed", Some("100"), "taken"),
        line("2026-08-23T04:10:00Z", "claimed", Some("101"), "taken"),
        line(
            "2026-08-23T04:10:40Z",
            "work_changed",
            Some("101"),
            "changed",
        ),
        // Keeps the first issue's window open past the second's episode.
        line(
            "2026-08-23T04:20:00Z",
            "work_changed",
            Some("100"),
            "changed",
        ),
    ];
    let tmp = tempfile::tempdir().unwrap();
    seed_for(tmp.path(), "demo", &lines, None, workspace.path());
    let out = sessions_from(
        &[tmp.path().join("self-driving")],
        Path::new("/x"),
        parse_unix("2026-08-23T04:21:00Z").unwrap(),
    );

    let issues = out["sessions"][0]["issues"].as_array().unwrap();
    let episodes_of = |number: &str| {
        issues
            .iter()
            .find(|i| i["number"] == number)
            .unwrap_or_else(|| panic!("issue {number} missing from {out}"))["episodes"]
            .clone()
    };
    assert_eq!(episodes_of("101"), 1, "the tag names its own issue: {out}");
    assert_eq!(
        episodes_of("100"),
        1,
        "only the untagged episode falls back to the window: {out}"
    );
}

/// The lease table is what says an issue is being worked right now, and
/// `in_progress` never read it. A driver that dies without a stop record
/// leaves its lease to run out. Its journal still reads open. So the page told
/// an operator someone was on an issue that was free.
#[test]
fn a_working_issue_with_no_live_lease_is_not_in_progress() {
    let workspace = tempfile::tempdir().unwrap();
    let now = parse_unix("2026-08-23T04:23:00Z").unwrap();
    let fleet = private_db(
        workspace.path(),
        "fleet.db",
        "CREATE TABLE dispatch_claims (claim_key TEXT PRIMARY KEY, owner TEXT NOT NULL,
             fence INTEGER NOT NULL, acquired_at_ms INTEGER NOT NULL,
             renewed_at_ms INTEGER NOT NULL, expires_at_ms INTEGER NOT NULL);",
    );
    // The first is held. The second's holder died, and its lease ran out a
    // minute ago.
    for (key, expires) in [("issue:1180", now + 300), ("issue:1182", now - 60)] {
        fleet
            .execute(
                "INSERT INTO dispatch_claims VALUES (?1, 'self-driving:4242', 1, ?2, ?2, ?3)",
                rusqlite::params![key, (now - 600) * 1000, expires * 1000],
            )
            .unwrap();
    }

    let mut lines = vec![line(
        "2026-08-23T04:20:02Z",
        "session_started",
        None,
        "began",
    )];
    for issue in ["1180", "1182"] {
        lines.push(line(
            "2026-08-23T04:22:38Z",
            "claimed",
            Some(issue),
            "taken",
        ));
        lines.push(line(
            "2026-08-23T04:22:40Z",
            "work_started",
            Some(issue),
            "began work — P1: x",
        ));
    }
    let queue = r#"{"at":"2026-08-23T04:22:00Z","items":[
        {"number":1180,"title":"x","rank":"P1"},{"number":1182,"title":"y","rank":"P1"}]}"#;
    let tmp = tempfile::tempdir().unwrap();
    seed_for(tmp.path(), "demo", &lines, Some(queue), workspace.path());
    let out = sessions_from(&[tmp.path().join("self-driving")], Path::new("/x"), now);

    let items = out["loops"][0]["queue"]["items"].as_array().unwrap();
    assert_eq!(items[0]["in_progress"], true, "the lease holds it: {out}");
    assert_eq!(
        items[1]["in_progress"], false,
        "the lease expired, so nobody is running a turn on it: {out}"
    );
    assert_eq!(out["totals"]["holders"], 1, "{out}");
}

/// The other half of the rule. The loop drops the lease once the pull request
/// exists, then polls for as long as CI takes. A missing lease says nothing
/// about a parked issue, so the journal stands.
#[test]
fn an_issue_parked_on_a_pull_request_stays_in_progress_without_a_lease() {
    let workspace = tempfile::tempdir().unwrap();
    private_db(
        workspace.path(),
        "fleet.db",
        "CREATE TABLE dispatch_claims (claim_key TEXT PRIMARY KEY, owner TEXT NOT NULL,
             fence INTEGER NOT NULL, acquired_at_ms INTEGER NOT NULL,
             renewed_at_ms INTEGER NOT NULL, expires_at_ms INTEGER NOT NULL);",
    );
    let lines = [
        line("2026-08-23T04:20:02Z", "session_started", None, "began"),
        line("2026-08-23T04:22:38Z", "claimed", Some("1180"), "taken"),
        line(
            "2026-08-23T04:22:40Z",
            "work_started",
            Some("1180"),
            "began work — P1: x",
        ),
        line(
            "2026-08-23T04:22:50Z",
            "pr_opened",
            Some("1186"),
            &pr_opened_for("1180"),
        ),
    ];
    let queue =
        r#"{"at":"2026-08-23T04:22:00Z","items":[{"number":1180,"title":"x","rank":"P1"}]}"#;
    let tmp = tempfile::tempdir().unwrap();
    seed_for(tmp.path(), "demo", &lines, Some(queue), workspace.path());
    let out = sessions_from(
        &[tmp.path().join("self-driving")],
        Path::new("/x"),
        parse_unix("2026-08-23T04:23:00Z").unwrap(),
    );

    assert_eq!(
        out["sessions"][0]["issues"][0]["outcome"], "pr open",
        "{out}"
    );
    assert_eq!(
        out["loops"][0]["queue"]["items"][0]["in_progress"], true,
        "an empty lease table cannot contradict a parked issue: {out}"
    );
}
