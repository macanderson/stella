//! `stella fleet claims` — the human-visible half of the dispatch lease
//! (#1675, residual of #1136).
//!
//! The lease in [`stella_fleet::ledger::lease`] stopped two sessions taking
//! one unit of work, but it stopped them *invisibly*: the row lived in
//! `.stella/private/fleet.db` and the only way to read it was `sqlite3`.
//! #1136's own recommendation was a claim store humans can see, for a reason
//! its evidence demonstrates — the collision it was filed from was two
//! *people-facing* sessions picking the same issue, and a claim nobody can
//! read is a claim somebody collides with by hand. Preventing the race
//! without surfacing it fixes the machine's half and leaves the human's.
//!
//! Read-only and offline: it opens the ledger, never writes, and needs no API
//! key. There is deliberately **no** verb here to break or release someone
//! else's claim. A lease expires on its own (see the lease module's docs on
//! why there is no reaper and no `--force`), so "unwedge a stuck claim" is
//! not an operation that needs to exist — and one that did would be a
//! supported way to hand the same task to two workers.
//!
//! A sibling of `fleet_cmd.rs` rather than a section of it, because that file
//! is a grandfathered god file closed to growth (AGENTS.md § *God files*).

use std::path::Path;

use colored::Colorize;
use serde::Serialize;
use stella_fleet::{DispatchClaim, Ledger};

use crate::query_format::{QueryFormat, Rows};
use crate::tui;

/// One claim, as `--format json` reports it.
///
/// A CLI-owned shape rather than `Serialize` bolted onto
/// [`DispatchClaim`]: the ledger type is an internal read model free to grow
/// a field, whereas this rides the versioned `query_format` envelope and is
/// a contract with whoever scripts against it. The derived fields (`live`,
/// `expires_in_ms`, `held_for_ms`) are the ones a reader would otherwise
/// have to recompute against a clock they cannot see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ClaimRow {
    /// The unit of dispatch — `task:<id>` from a fan-out, `issue:<n>` from a
    /// tracker-driven dispatcher. Opaque to the lease.
    claim_key: String,
    /// The run holding it. A fleet run id embeds the minting process's pid,
    /// so this names a session a human can go look at.
    owner: String,
    /// Which grant of this key this is; bumped on every (re)claim.
    fence: i64,
    /// When this grant was taken (wall clock, ms).
    acquired_at_ms: u64,
    /// When the holder last heartbeated.
    renewed_at_ms: u64,
    /// When the claim stops being live.
    expires_at_ms: u64,
    /// Whether it was still live at the instant this listing was read. Only
    /// ever `false` under `--all`.
    live: bool,
    /// Milliseconds until expiry — **negative** once lapsed, which reads as
    /// how long ago that was.
    expires_in_ms: i64,
    /// How long the holder had held it when this was read.
    held_for_ms: i64,
}

impl ClaimRow {
    /// Project a ledger row against the reading clock.
    fn read(claim: &DispatchClaim, now_ms: u64) -> Self {
        Self {
            claim_key: claim.claim_key.clone(),
            owner: claim.owner.clone(),
            fence: claim.fence,
            acquired_at_ms: claim.acquired_at_ms,
            renewed_at_ms: claim.renewed_at_ms,
            expires_at_ms: claim.expires_at_ms,
            live: claim.is_live_at(now_ms),
            expires_in_ms: delta_ms(now_ms, claim.expires_at_ms),
            held_for_ms: delta_ms(claim.acquired_at_ms, now_ms),
        }
    }
}

/// Print the workspace's dispatch claims.
///
/// A workspace with no ledger at all is not an error: it means no fan-out has
/// ever run here, which is a perfectly good answer to "is anything claimed?"
/// and the same stance `stella fleet clean` takes.
pub(crate) fn claims(root: &Path, all: bool, format: QueryFormat) -> Result<(), String> {
    let ledger_path = stella_store::workspace_private_sqlite_path(root, "fleet.db")
        .map_err(|e| format!("could not locate the fleet ledger: {e}"))?;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("system clock is before the Unix epoch: {e}"))?
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;

    let claims = if ledger_path.exists() {
        let ledger = Ledger::open(&ledger_path)
            .map_err(|e| format!("could not open the fleet ledger: {e}"))?;
        if all {
            ledger.all_dispatch_claims()
        } else {
            ledger.live_dispatch_claims(now_ms)
        }
        .map_err(|e| format!("could not read the fleet ledger: {e}"))?
    } else {
        Vec::new()
    };

    let rows: Vec<ClaimRow> = claims.iter().map(|c| ClaimRow::read(c, now_ms)).collect();

    match format {
        QueryFormat::Json => {
            let json = serde_json::to_string_pretty(&Rows::new(&rows))
                .map_err(|e| format!("could not render the claims as JSON: {e}"))?;
            println!("{json}");
        }
        QueryFormat::Text => {
            tui::section_header("Stella — fleet dispatch claims");
            print!("{}", render(&rows, all));
        }
    }
    Ok(())
}

/// Render the claim table.
///
/// Pure — rows in, block out — so the test reads exactly what a human reads
/// rather than a mock of it. Colour is confined to the fixed-width status
/// marker: every other column is padded to a computed width, and an ANSI
/// escape inside a padded cell counts toward that width while occupying no
/// columns on screen, which is how coloured tables come out ragged.
fn render(rows: &[ClaimRow], all: bool) -> String {
    if rows.is_empty() {
        return if all {
            format!(
                "    {} no dispatch claims recorded in this workspace\n\n",
                "·".dimmed()
            )
        } else {
            format!(
                "    {} no live dispatch claims — nothing here is claimed\n      \
                 (pass --all to include claims that have already lapsed)\n\n",
                "·".dimmed()
            )
        };
    }

    let key_width = rows.iter().map(|r| r.claim_key.len()).max().unwrap_or(0);
    let owner_width = rows.iter().map(|r| r.owner.len()).max().unwrap_or(0);
    let held_width = rows
        .iter()
        .map(|r| human_ms(r.held_for_ms).len())
        .max()
        .unwrap_or(0);

    let mut out = String::new();
    for row in rows {
        let marker = if row.live {
            "●".green()
        } else {
            "○".dimmed()
        };
        let expiry = if row.live {
            format!("expires in {}", human_ms(row.expires_in_ms))
        } else {
            format!("lapsed {} ago", human_ms(row.expires_in_ms))
        };
        out.push_str(&format!(
            "    {marker} {:<key_width$}  {:<owner_width$}  held {:>held_width$}  {expiry}\n",
            row.claim_key,
            row.owner,
            human_ms(row.held_for_ms),
        ));
    }

    let live = rows.iter().filter(|r| r.live).count();
    out.push_str(&format!("\n  {} claim(s), {live} live\n\n", rows.len()));
    out
}

/// `to - from` in milliseconds as a signed delta that saturates rather than
/// wrapping.
///
/// Both timestamps come off some other process's wall clock, so a pair that
/// makes no sense — a claim from the future, a clock stepped backwards — must
/// render oddly and never panic or wrap a lapsed claim into a live one.
fn delta_ms(from_ms: u64, to_ms: u64) -> i64 {
    let delta = i128::from(to_ms) - i128::from(from_ms);
    delta.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

/// A compact, fixed-shape rendering of a millisecond span: `45s`, `12m 30s`,
/// `3h 04m`.
///
/// Deliberately not prose ("about 2 minutes ago"): this is a column, the
/// reader is comparing two rows against a 15-minute TTL, and a stable shape
/// is what makes that comparison possible at a glance. The sign is dropped —
/// the caller's wording already says which direction the span runs.
fn human_ms(ms: i64) -> String {
    let secs = ms.unsigned_abs() / 1000;
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(key: &str, owner: &str, acquired_at_ms: u64, expires_at_ms: u64) -> DispatchClaim {
        DispatchClaim {
            claim_key: key.to_string(),
            owner: owner.to_string(),
            fence: 1,
            acquired_at_ms,
            renewed_at_ms: acquired_at_ms,
            expires_at_ms,
        }
    }

    /// The witness for #1675's first half: a claim is legible to a human
    /// without `sqlite3` — the key, who holds it, and when it lapses.
    #[test]
    fn a_live_claim_names_its_holder_and_when_it_lapses() {
        let now = 600_000;
        let rows = vec![ClaimRow::read(
            &claim("task:fix-1136", "run-8f21a3-41207", 420_000, 1_320_000),
            now,
        )];

        let out = render(&rows, false);
        assert!(out.contains("task:fix-1136"), "the unit of work: {out}");
        assert!(out.contains("run-8f21a3-41207"), "the holder: {out}");
        assert!(
            out.contains("held 3m 00s"),
            "how long they have had it: {out}"
        );
        assert!(out.contains("expires in 12m 00s"), "when it lapses: {out}");
        assert!(out.contains("1 claim(s), 1 live"), "the tally: {out}");
    }

    /// `--all` answers the question a dead session leaves behind: a lapsed
    /// row still names its last holder, and says how long ago it let go.
    #[test]
    fn a_lapsed_claim_says_who_held_it_and_how_long_ago_it_went() {
        let now = 600_000;
        let rows = vec![
            ClaimRow::read(
                &claim("issue:1136", "run-2b90cc-41999", 60_000, 480_000),
                now,
            ),
            ClaimRow::read(
                &claim("task:live", "run-8f21a3-41207", 590_000, 900_000),
                now,
            ),
        ];

        let out = render(&rows, true);
        assert!(out.contains("run-2b90cc-41999"), "the dead holder: {out}");
        assert!(out.contains("lapsed 2m 00s ago"), "when it went: {out}");
        assert!(
            out.contains("expires in 5m 00s"),
            "the live one stays: {out}"
        );
        assert!(out.contains("2 claim(s), 1 live"), "the tally: {out}");
        assert!(!rows[0].live && rows[1].live);
    }

    /// The common case — nobody is on anything — must read as an answer, not
    /// as an empty table, and must point at the listing that would show a
    /// session that died holding something.
    #[test]
    fn no_claims_reads_as_an_answer() {
        let out = render(&[], false);
        assert!(out.contains("no live dispatch claims"), "{out}");
        assert!(out.contains("--all"), "points at the lapsed listing: {out}");
        assert!(render(&[], true).contains("no dispatch claims recorded"));
    }

    /// The JSON arm rides `query_format`'s versioned envelope, and carries
    /// the derived fields a consumer cannot recompute without the clock the
    /// listing was read against.
    #[test]
    fn json_rows_ride_the_versioned_query_envelope() {
        let rows = vec![ClaimRow::read(
            &claim("task:fix-1136", "run-8f21a3-41207", 420_000, 1_320_000),
            600_000,
        )];
        let json = serde_json::to_string(&Rows::new(&rows)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["schema_version"], 1);
        let row = &parsed["rows"][0];
        assert_eq!(row["claim_key"], "task:fix-1136");
        assert_eq!(row["owner"], "run-8f21a3-41207");
        assert_eq!(row["expires_at_ms"], 1_320_000_u64);
        assert_eq!(row["live"], true);
        assert_eq!(row["expires_in_ms"], 720_000);
        assert_eq!(row["held_for_ms"], 180_000);
    }

    /// A lapsed claim's `expires_in_ms` goes negative rather than wrapping —
    /// the arithmetic runs on two different processes' wall clocks, so the
    /// sign is the only thing distinguishing "in 5 minutes" from "5 minutes
    /// ago" for a consumer reading the JSON.
    #[test]
    fn a_lapsed_claim_reports_a_negative_time_to_expiry() {
        let row = ClaimRow::read(&claim("issue:1136", "run-a", 60_000, 480_000), 600_000);
        assert_eq!(row.expires_in_ms, -120_000);
        assert!(!row.live);

        // A claim minted by a clock far ahead of ours must saturate, never
        // wrap: an expiry in the far future stays in the future, and an
        // acquisition in the far future reads as a negative age rather than
        // as an absurdly old claim.
        let skewed = ClaimRow::read(&claim("issue:1", "run-a", u64::MAX, u64::MAX), 0);
        assert_eq!(skewed.expires_in_ms, i64::MAX, "saturated, not wrapped");
        assert!(skewed.held_for_ms < 0, "{}", skewed.held_for_ms);
    }

    #[test]
    fn spans_render_in_one_fixed_shape() {
        assert_eq!(human_ms(0), "0s");
        assert_eq!(human_ms(45_000), "45s");
        assert_eq!(
            human_ms(-45_000),
            "45s",
            "the caller's wording carries the sign"
        );
        assert_eq!(human_ms(750_000), "12m 30s");
        assert_eq!(human_ms(11_040_000), "3h 04m");
    }

    /// `stella fleet claims` parses as a subcommand rather than as a task
    /// prompt, and defaults to the live listing in text.
    #[test]
    fn fleet_claims_parses_as_a_verb_and_defaults_to_live_text() {
        let cli = Cli::try_parse_from(["stella", "fleet", "claims"]).unwrap();
        match cli.command {
            Some(Command::Fleet {
                cmd: Some(crate::fleet_verbs::FleetCmd::Claims { all, format }),
                ..
            }) => {
                assert!(!all, "the live listing is the default");
                assert_eq!(format, QueryFormat::Text);
            }
            // `Command` carries no `Debug`, so the arm names what was
            // expected rather than printing what arrived.
            _ => panic!("expected `fleet claims` to parse as the claims verb"),
        }

        let cli = Cli::try_parse_from(["stella", "fleet", "claims", "--all", "--format", "json"])
            .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Fleet {
                cmd: Some(crate::fleet_verbs::FleetCmd::Claims {
                    all: true,
                    format: QueryFormat::Json
                }),
                ..
            })
        ));

        // And the documented escape hatch still works: `--` makes it a prompt.
        let cli = Cli::try_parse_from(["stella", "fleet", "--", "claims are stale"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Fleet { cmd: None, .. })
        ));
    }
}
