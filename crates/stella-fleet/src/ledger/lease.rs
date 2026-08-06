//! Dispatch claims — the check-and-set lease that stops two agent sessions
//! picking up the same unit of work (#1136).
//!
//! Two sessions running against one workspace could both decide to work on
//! the same issue, read the same files, and open near-identical PRs; it has
//! happened, and the second run's whole cost was wasted. Nothing in the fleet
//! asked "is somebody already on this?" before dispatching, because there was
//! nowhere to ask.
//!
//! This is that place: one row per **unit of dispatch**, in the ledger that
//! already survives a process. The unit is named by an opaque `claim_key` —
//! `crate::fleet` claims `task:<task id>`, and a dispatcher working from a
//! tracker would claim `issue:1136` — so the mechanism does not have to know
//! what a unit of work is.
//!
//! # The semantics, exactly
//!
//! - **A lease, not a lock.** Every claim carries an expiry. A session that
//!   is SIGKILLed, panics, or loses power leaves a row behind, and that row
//!   stops mattering the moment `now_ms >= expires_at_ms`. Nothing has to run
//!   to make that true — there is no reaper, no daemon, and no manual
//!   `--force`: expiry is evaluated inside the claim statement itself, so the
//!   next claimant reclaims a dead holder's key as a side effect of asking
//!   for it. A wedged unit of work is not reachable from here.
//! - **Check-and-set, in one statement.** [`Ledger::claim_dispatch`] is a
//!   single conditional `INSERT … ON CONFLICT DO UPDATE … WHERE
//!   expires_at_ms <= now` and the affected-row count decides the winner. A
//!   read-then-write in Rust would reintroduce exactly the race this exists
//!   to close — two claimants can both observe "free" before either writes.
//!   SQLite serializes the write, so of N simultaneous claimants exactly one
//!   sees a row and the rest see none.
//! - **Renewal is the liveness proof.** A holder that is still working calls
//!   [`Ledger::renew_dispatch`] on a cadence well inside the TTL
//!   ([`DispatchLease::heartbeat_interval_ms`] recommends TTL/3, so two
//!   consecutive missed beats still do not expire it). Renewal is itself
//!   conditional — owner, fence, and *still live* — so it can only extend a
//!   grant that is still this holder's.
//! - **The fence is the grant's identity.** Each grant of a key bumps a
//!   monotonic `fence`. That is what makes the expired-but-alive case
//!   decidable rather than merely unfortunate: if a holder stalls past its
//!   TTL (a long GC pause, a suspended laptop, a wall clock jumping) and a
//!   rival reclaims the key, the fence moves — and the stalled holder's very
//!   next renewal fails with [`RenewOutcome::Lost`] instead of quietly
//!   extending a lease it no longer holds. Its release is fenced too, so it
//!   cannot delete the rival's row on the way out. It learns it lost within
//!   one heartbeat; what it does then is the caller's policy (`crate::fleet`
//!   deliberately does not kill a worker mid-flight — see
//!   `Fleet::dispatch`).
//!
//! The one thing a lease cannot promise: between the moment a lease expires
//! and the moment its holder next heartbeats, two workers can be *running* on
//! the same unit. That window is bounded by the heartbeat interval and is the
//! unavoidable price of not wedging on a dead session — the alternative,
//! a lock with no expiry, trades a bounded overlap for an unbounded stall.
//! Pick a TTL comfortably longer than the longest pause a healthy worker can
//! take between beats.

use rusqlite::{OptionalExtension, params};

use super::{Ledger, LedgerError};

/// A live claim on one unit of dispatch, as read from the ledger.
///
/// A snapshot: the row can be reclaimed by anyone the instant it expires, so
/// treat this as "what was true when it was read", never as a permission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchClaim {
    /// The unit of dispatch — `task:<id>`, `issue:<n>`, whatever the
    /// dispatcher names its work. Opaque here.
    pub claim_key: String,
    /// Who holds it. `crate::fleet` uses the run id, which embeds the
    /// minting process's pid (see `FleetConfig::run_id`), so a human reading
    /// the row can tell which session it belongs to.
    pub owner: String,
    /// Which grant of this key this is — bumped on every (re)claim, never
    /// reused. See the module docs on the expired-but-alive case.
    pub fence: i64,
    /// When this grant was taken (wall-clock ms, from the claimant's clock).
    pub acquired_at_ms: u64,
    /// When it was last renewed — equal to `acquired_at_ms` until the first
    /// heartbeat lands.
    pub renewed_at_ms: u64,
    /// When it stops being live. At or after this instant any claimant may
    /// take the key.
    pub expires_at_ms: u64,
}

impl DispatchClaim {
    /// Whether this claim is still live at `now_ms`. Expiry is inclusive:
    /// a claim is reclaimable *at* its expiry instant.
    #[must_use]
    pub fn is_live_at(&self, now_ms: u64) -> bool {
        now_ms < self.expires_at_ms
    }
}

/// Proof that this session holds a unit of dispatch — returned by
/// [`Ledger::claim_dispatch`] and required by renew/release, which are
/// conditional on the `owner`/`fence` it carries.
///
/// Holding the value is not itself the lease: the lease is the row, and the
/// row expires. A holder that stops renewing stops holding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchLease {
    /// The claimed unit of dispatch.
    pub claim_key: String,
    /// This holder's identity, as written into the row.
    pub owner: String,
    /// This grant's fence — what renew and release are conditional on.
    pub fence: i64,
    /// When this grant was taken.
    pub acquired_at_ms: u64,
    /// When it expires unless renewed.
    pub expires_at_ms: u64,
    /// The TTL this grant was taken with, carried so a heartbeat can renew
    /// with the same one without the caller threading it through.
    pub ttl_ms: u64,
}

impl DispatchLease {
    /// The recommended heartbeat cadence: a third of the TTL, so a holder
    /// survives two consecutive missed beats. At least 1ms, so a caller that
    /// passes a tiny TTL still gets a usable interval rather than a busy
    /// loop on zero.
    #[must_use]
    pub fn heartbeat_interval_ms(&self) -> u64 {
        (self.ttl_ms / 3).max(1)
    }
}

/// The answer to a claim attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// This session won the key.
    Granted(DispatchLease),
    /// Somebody else holds it. The claim carried is a best-effort snapshot
    /// read after losing (it may already have changed again) — it exists so a
    /// caller can *tell the user who* rather than only that it lost. `None`
    /// only if the row vanished between the two statements.
    Held(Option<DispatchClaim>),
}

/// The answer to a heartbeat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenewOutcome {
    /// Still ours; the lease value carries the extended expiry.
    Renewed(DispatchLease),
    /// No longer ours — expired and reclaimed, or released. The claim
    /// carried is whoever holds the key now, if anyone. A holder that sees
    /// this must stop treating itself as the owner of the work.
    Lost(Option<DispatchClaim>),
}

impl Ledger {
    /// Claim a unit of dispatch for `ttl_ms`, atomically against every
    /// concurrent claimant.
    ///
    /// One statement decides it: the row is inserted if the key is unclaimed,
    /// or taken over if the existing claim has expired, and otherwise nothing
    /// is written and the caller loses. There is no read-then-write window
    /// for a rival to slip through — which is the entire point (#1136).
    ///
    /// `now_ms` is wall-clock milliseconds; the expiry is `now_ms + ttl_ms`,
    /// saturating. Claimants in one workspace are processes on one machine,
    /// so they share a clock; a rewound clock only postpones an expiry, never
    /// grants two live leases at once (the winner is decided by the write,
    /// not by the timestamp).
    ///
    /// Re-claiming a key this session already holds and has not let expire
    /// loses like anyone else — a claim is a grant, not a mode. Release first
    /// (or renew), which keeps "exactly one grant is live" unconditional.
    pub fn claim_dispatch(
        &self,
        claim_key: &str,
        owner: &str,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<ClaimOutcome, LedgerError> {
        let expires_at_ms = now_ms.saturating_add(ttl_ms);
        let granted: Option<(i64, i64)> = self
            .conn
            .query_row(
                "INSERT INTO dispatch_claims \
                 (claim_key, owner, fence, acquired_at_ms, renewed_at_ms, expires_at_ms) \
                 VALUES (?1, ?2, 1, ?3, ?3, ?4) \
                 ON CONFLICT(claim_key) DO UPDATE SET \
                     owner = excluded.owner, \
                     fence = dispatch_claims.fence + 1, \
                     acquired_at_ms = excluded.acquired_at_ms, \
                     renewed_at_ms = excluded.renewed_at_ms, \
                     expires_at_ms = excluded.expires_at_ms \
                 WHERE dispatch_claims.expires_at_ms <= ?3 \
                 RETURNING fence, acquired_at_ms",
                params![claim_key, owner, now_ms as i64, expires_at_ms as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        match granted {
            Some((fence, acquired_at_ms)) => Ok(ClaimOutcome::Granted(DispatchLease {
                claim_key: claim_key.to_string(),
                owner: owner.to_string(),
                fence,
                acquired_at_ms: acquired_at_ms as u64,
                expires_at_ms,
                ttl_ms,
            })),
            None => Ok(ClaimOutcome::Held(self.dispatch_claim(claim_key)?)),
        }
    }

    /// Extend a lease this session still holds — the heartbeat that says "the
    /// worker is alive".
    ///
    /// Conditional on all three of key, owner and fence, *and* on the lease
    /// still being live: a holder that let its TTL lapse must go back through
    /// [`claim_dispatch`](Self::claim_dispatch) rather than resurrect a grant
    /// a rival may already have taken. That is what keeps "one live grant per
    /// key" true even when a worker stalls past its own expiry.
    pub fn renew_dispatch(
        &self,
        lease: &DispatchLease,
        now_ms: u64,
    ) -> Result<RenewOutcome, LedgerError> {
        let expires_at_ms = now_ms.saturating_add(lease.ttl_ms);
        let renewed: Option<i64> = self
            .conn
            .query_row(
                "UPDATE dispatch_claims \
                 SET renewed_at_ms = ?3, expires_at_ms = ?4 \
                 WHERE claim_key = ?1 AND owner = ?2 AND fence = ?5 \
                   AND dispatch_claims.expires_at_ms > ?3 \
                 RETURNING fence",
                params![
                    lease.claim_key,
                    lease.owner,
                    now_ms as i64,
                    expires_at_ms as i64,
                    lease.fence,
                ],
                |row| row.get(0),
            )
            .optional()?;

        match renewed {
            Some(_) => Ok(RenewOutcome::Renewed(DispatchLease {
                expires_at_ms,
                ..lease.clone()
            })),
            None => Ok(RenewOutcome::Lost(self.dispatch_claim(&lease.claim_key)?)),
        }
    }

    /// Give a unit of dispatch back the moment the work settles, rather than
    /// making the next session wait out the TTL.
    ///
    /// Fenced: a holder whose lease was already reclaimed deletes nothing, so
    /// a late release can never free the rival that superseded it. Returns
    /// whether a row was removed.
    pub fn release_dispatch(&self, lease: &DispatchLease) -> Result<bool, LedgerError> {
        let removed = self.conn.execute(
            "DELETE FROM dispatch_claims \
             WHERE claim_key = ?1 AND owner = ?2 AND fence = ?3",
            params![lease.claim_key, lease.owner, lease.fence],
        )?;
        Ok(removed > 0)
    }

    /// The claim row for one key, live or expired — `None` when the key has
    /// never been claimed or was released.
    ///
    /// Expired rows are returned deliberately: "who last held this, and when
    /// did it lapse" is exactly what a human deciding what to hand out next
    /// wants to see. Use [`DispatchClaim::is_live_at`] to judge it.
    pub fn dispatch_claim(&self, claim_key: &str) -> Result<Option<DispatchClaim>, LedgerError> {
        let claim = self
            .conn
            .query_row(
                &format!("{CLAIM_SELECT} WHERE claim_key = ?1"),
                params![claim_key],
                row_to_claim,
            )
            .optional()?;
        Ok(claim)
    }

    /// Every claim still live at `now_ms`, soonest expiry first — what a
    /// dispatcher (or a human, via `stella fleet claims`) reads to see what
    /// the fleet is already on.
    pub fn live_dispatch_claims(&self, now_ms: u64) -> Result<Vec<DispatchClaim>, LedgerError> {
        self.claims_where(
            &format!("{CLAIM_SELECT} WHERE expires_at_ms > ?1 ORDER BY expires_at_ms, claim_key"),
            params![now_ms as i64],
        )
    }

    /// Every claim row, live **or lapsed**, soonest expiry first — the read
    /// behind `stella fleet claims --all`.
    ///
    /// The lapsed rows are the reason this exists next to
    /// [`live_dispatch_claims`](Self::live_dispatch_claims): "who last held
    /// this, and when did it lapse" is precisely what a human deciding what
    /// to hand out next asks after a session dies, and a lapsed row survives
    /// until something re-claims or releases the key. Nothing here is a
    /// permission — judge each row with [`DispatchClaim::is_live_at`].
    pub fn all_dispatch_claims(&self) -> Result<Vec<DispatchClaim>, LedgerError> {
        self.claims_where(
            &format!("{CLAIM_SELECT} ORDER BY expires_at_ms, claim_key"),
            params![],
        )
    }

    /// Run one `CLAIM_SELECT`-shaped query and collect its rows, so the two
    /// list readers share their error handling as well as their columns.
    fn claims_where(
        &self,
        sql: &str,
        args: impl rusqlite::Params,
    ) -> Result<Vec<DispatchClaim>, LedgerError> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(args, row_to_claim)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

/// The column list every claim reader selects, in the order [`row_to_claim`]
/// reads them back. Hoisted for the same reason `row_to_claim` exists: three
/// readers spelling the same six columns is three chances for one of them to
/// drift, and a swapped column reads as a valid row rather than an error.
const CLAIM_SELECT: &str = "SELECT claim_key, owner, fence, acquired_at_ms, renewed_at_ms, \
                            expires_at_ms FROM dispatch_claims";

/// The one place a `dispatch_claims` row becomes a [`DispatchClaim`], so the
/// two readers cannot drift in column order.
fn row_to_claim(row: &rusqlite::Row<'_>) -> rusqlite::Result<DispatchClaim> {
    Ok(DispatchClaim {
        claim_key: row.get(0)?,
        owner: row.get(1)?,
        fence: row.get(2)?,
        acquired_at_ms: row.get::<_, i64>(3)? as u64,
        renewed_at_ms: row.get::<_, i64>(4)? as u64,
        expires_at_ms: row.get::<_, i64>(5)? as u64,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use super::*;

    const TTL: u64 = 10_000;

    fn granted(outcome: ClaimOutcome) -> DispatchLease {
        match outcome {
            ClaimOutcome::Granted(lease) => lease,
            ClaimOutcome::Held(held) => panic!("expected a grant, was held by {held:?}"),
        }
    }

    /// **Witness (#1136), the race half.** Eight threads, each with its own
    /// connection to one `fleet.db`, released simultaneously against a single
    /// key: exactly one is granted.
    ///
    /// This is the property that a read-then-write cannot have. It is run
    /// against a file rather than `:memory:` on purpose — an in-memory
    /// connection is private per handle, so the threads would not even be
    /// contending for the same rows.
    #[test]
    fn concurrent_claimers_race_for_one_key_and_exactly_one_wins() {
        const CLAIMANTS: usize = 8;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fleet.db");
        // Create the schema once up front so the race is over the claim and
        // not over migration.
        drop(Ledger::open(&path).unwrap());

        let barrier = Arc::new(Barrier::new(CLAIMANTS));
        let mut threads = Vec::new();
        for i in 0..CLAIMANTS {
            let barrier = Arc::clone(&barrier);
            let path = path.clone();
            threads.push(std::thread::spawn(move || {
                let ledger = Ledger::open(&path).unwrap();
                let owner = format!("session-{i}");
                barrier.wait();
                ledger.claim_dispatch("issue:1136", &owner, 1_000, TTL)
            }));
        }

        let outcomes: Vec<ClaimOutcome> = threads
            .into_iter()
            .map(|t| t.join().unwrap().unwrap())
            .collect();
        let winners: Vec<&DispatchLease> = outcomes
            .iter()
            .filter_map(|o| match o {
                ClaimOutcome::Granted(lease) => Some(lease),
                ClaimOutcome::Held(_) => None,
            })
            .collect();
        assert_eq!(
            winners.len(),
            1,
            "exactly one claimant may win a key; outcomes: {outcomes:#?}"
        );

        // And every loser was told who won, not merely that it lost.
        let winner = &winners[0];
        for outcome in &outcomes {
            if let ClaimOutcome::Held(held) = outcome {
                let held = held.as_ref().expect("the winning row is readable");
                assert_eq!(held.owner, winner.owner);
                assert_eq!(held.fence, winner.fence);
            }
        }

        // The durable row agrees with the value the winner was handed.
        let ledger = Ledger::open(&path).unwrap();
        let row = ledger.dispatch_claim("issue:1136").unwrap().unwrap();
        assert_eq!(row.owner, winner.owner);
        assert_eq!(row.fence, 1, "the first grant of a key is fence 1");
        assert_eq!(row.expires_at_ms, 11_000);
    }

    /// **Witness (#1136), the expiry half.** A session that died holding a
    /// claim cannot wedge the work: once the TTL lapses the key is
    /// reclaimable with no reaper, no operator, and no force flag.
    #[test]
    fn an_expired_lease_is_reclaimable_and_a_live_one_is_not() {
        let ledger = Ledger::open_in_memory().unwrap();
        let dead = granted(
            ledger
                .claim_dispatch("issue:1136", "session-a", 1_000, TTL)
                .unwrap(),
        );
        assert_eq!(dead.expires_at_ms, 11_000);

        // While it is live, a rival is refused and told who holds it.
        let held = ledger
            .claim_dispatch("issue:1136", "session-b", 10_999, TTL)
            .unwrap();
        match held {
            ClaimOutcome::Held(Some(claim)) => {
                assert_eq!(claim.owner, "session-a");
                assert!(claim.is_live_at(10_999));
            }
            other => panic!("a live lease must refuse a rival, got {other:?}"),
        }

        // At the expiry instant — session A having died without releasing —
        // the rival takes it, and the fence moves.
        let reclaimed = granted(
            ledger
                .claim_dispatch("issue:1136", "session-b", 11_000, TTL)
                .unwrap(),
        );
        assert_eq!(reclaimed.owner, "session-b");
        assert_eq!(reclaimed.fence, 2, "a reclaim is a new grant");
        assert_eq!(reclaimed.expires_at_ms, 21_000);
    }

    /// A heartbeat keeps the key: the same rival that would have won after
    /// the original TTL is still refused.
    #[test]
    fn renewal_extends_the_lease_and_keeps_a_rival_out() {
        let ledger = Ledger::open_in_memory().unwrap();
        let lease = granted(
            ledger
                .claim_dispatch("task:t1", "run-a", 1_000, TTL)
                .unwrap(),
        );
        let renewed = match ledger.renew_dispatch(&lease, 5_000).unwrap() {
            RenewOutcome::Renewed(lease) => lease,
            other => panic!("a live lease must renew, got {other:?}"),
        };
        assert_eq!(renewed.expires_at_ms, 15_000);
        assert_eq!(renewed.fence, lease.fence, "a renewal is the same grant");

        assert!(matches!(
            ledger
                .claim_dispatch("task:t1", "run-b", 11_000, TTL)
                .unwrap(),
            ClaimOutcome::Held(_)
        ));
        assert_eq!(ledger.live_dispatch_claims(11_000).unwrap().len(), 1);
    }

    /// The expired-but-alive case: a holder that stalls past its TTL and is
    /// superseded learns it lost at its next heartbeat, and its late release
    /// cannot free the rival's claim.
    #[test]
    fn a_superseded_holder_loses_its_renewal_and_cannot_release_the_rival() {
        let ledger = Ledger::open_in_memory().unwrap();
        let stalled = granted(
            ledger
                .claim_dispatch("task:t1", "run-a", 1_000, TTL)
                .unwrap(),
        );
        let rival = granted(
            ledger
                .claim_dispatch("task:t1", "run-b", 11_000, TTL)
                .unwrap(),
        );

        match ledger.renew_dispatch(&stalled, 12_000).unwrap() {
            RenewOutcome::Lost(Some(now_held_by)) => {
                assert_eq!(now_held_by.owner, "run-b");
                assert_eq!(now_held_by.fence, rival.fence);
            }
            other => panic!("a superseded holder must not renew, got {other:?}"),
        }

        assert!(
            !ledger.release_dispatch(&stalled).unwrap(),
            "a fenced-out holder releases nothing"
        );
        let still = ledger.dispatch_claim("task:t1").unwrap().unwrap();
        assert_eq!(still.owner, "run-b", "the rival's claim survives");

        // A holder that lapsed but was NOT superseded is in the same boat: it
        // must re-claim, not resurrect. Its own lapsed row is still there and
        // is what it is told about — nobody else has taken the key yet, but it
        // no longer holds it either.
        assert!(ledger.release_dispatch(&rival).unwrap());
        let lapsing = granted(
            ledger
                .claim_dispatch("task:t2", "run-c", 1_000, TTL)
                .unwrap(),
        );
        match ledger.renew_dispatch(&lapsing, 11_000).unwrap() {
            RenewOutcome::Lost(Some(own)) => {
                assert_eq!(own.owner, "run-c");
                assert!(!own.is_live_at(11_000), "its own lease has lapsed");
            }
            other => panic!("a lapsed lease must not renew, got {other:?}"),
        }
        // And re-claiming it works, as a NEW grant.
        assert_eq!(
            granted(
                ledger
                    .claim_dispatch("task:t2", "run-c", 11_000, TTL)
                    .unwrap()
            )
            .fence,
            2
        );
    }

    /// Releasing hands the key back immediately rather than making the next
    /// session wait out the TTL — and the next grant is a new fence.
    #[test]
    fn releasing_frees_the_key_before_its_ttl() {
        let ledger = Ledger::open_in_memory().unwrap();
        let lease = granted(
            ledger
                .claim_dispatch("task:t1", "run-a", 1_000, TTL)
                .unwrap(),
        );
        assert!(ledger.release_dispatch(&lease).unwrap());
        assert_eq!(ledger.dispatch_claim("task:t1").unwrap(), None);
        assert!(ledger.live_dispatch_claims(1_000).unwrap().is_empty());

        let next = granted(
            ledger
                .claim_dispatch("task:t1", "run-b", 2_000, TTL)
                .unwrap(),
        );
        assert_eq!(next.fence, 1, "a released key starts a fresh row");
        assert_eq!(next.owner, "run-b");
    }

    /// Re-claiming a key this session already holds is refused like any other
    /// claim — a claim is a grant, not a mode.
    #[test]
    fn re_claiming_a_key_this_session_holds_is_refused() {
        let ledger = Ledger::open_in_memory().unwrap();
        let lease = granted(
            ledger
                .claim_dispatch("task:t1", "run-a", 1_000, TTL)
                .unwrap(),
        );
        match ledger
            .claim_dispatch("task:t1", "run-a", 2_000, TTL)
            .unwrap()
        {
            ClaimOutcome::Held(Some(claim)) => assert_eq!(claim.fence, lease.fence),
            other => panic!("a second claim by the same owner must lose, got {other:?}"),
        }
    }

    /// Different keys never contend, and the live listing is ordered by
    /// soonest expiry.
    #[test]
    fn distinct_keys_are_independent_and_listed_by_soonest_expiry() {
        let ledger = Ledger::open_in_memory().unwrap();
        for (key, ttl) in [("issue:2", 30_000), ("issue:1", 10_000)] {
            granted(ledger.claim_dispatch(key, "run-a", 1_000, ttl).unwrap());
        }
        let live = ledger.live_dispatch_claims(1_000).unwrap();
        assert_eq!(
            live.iter()
                .map(|c| c.claim_key.as_str())
                .collect::<Vec<_>>(),
            vec!["issue:1", "issue:2"]
        );
        // Once the first lapses only the second is live — and the lapsed row
        // is still readable, because "who last held this" is the question a
        // human dispatching work actually asks.
        let live = ledger.live_dispatch_claims(11_000).unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].claim_key, "issue:2");
        assert!(ledger.dispatch_claim("issue:1").unwrap().is_some());
    }

    /// `all_dispatch_claims` is the lapsed-inclusive listing behind
    /// `stella fleet claims --all`: the live view answers "what is being
    /// worked on", and only this one answers "who was last on this, and did
    /// they die holding it" — the question that survives the session.
    #[test]
    fn the_full_listing_keeps_lapsed_claims_the_live_one_drops() {
        let ledger = Ledger::open_in_memory().unwrap();
        for (key, ttl) in [("issue:2", 30_000), ("issue:1", 10_000)] {
            granted(ledger.claim_dispatch(key, "run-a", 1_000, ttl).unwrap());
        }

        // At 11s `issue:1` has lapsed: the live listing has forgotten it, the
        // full listing still names its holder.
        assert_eq!(
            ledger
                .live_dispatch_claims(11_000)
                .unwrap()
                .iter()
                .map(|c| c.claim_key.as_str())
                .collect::<Vec<_>>(),
            vec!["issue:2"]
        );
        let all = ledger.all_dispatch_claims().unwrap();
        assert_eq!(
            all.iter().map(|c| c.claim_key.as_str()).collect::<Vec<_>>(),
            vec!["issue:1", "issue:2"],
            "the full listing keeps lapsed rows, soonest expiry first"
        );
        assert!(!all[0].is_live_at(11_000), "issue:1 lapsed at 11s");
        assert!(all[1].is_live_at(11_000), "issue:2 runs to 31s");
        assert_eq!(all[0].owner, "run-a");

        // A released key leaves nothing behind — released is not lapsed.
        let lease = granted(
            ledger
                .claim_dispatch("issue:1", "run-b", 11_000, 10_000)
                .unwrap(),
        );
        assert!(ledger.release_dispatch(&lease).unwrap());
        assert_eq!(
            ledger
                .all_dispatch_claims()
                .unwrap()
                .iter()
                .map(|c| c.claim_key.as_str())
                .collect::<Vec<_>>(),
            vec!["issue:2"]
        );
    }

    /// The claims table reaches a ledger that already exists on disk — the
    /// migration half, not just `FRESH_SCHEMA`.
    #[test]
    fn a_legacy_ledger_gains_the_claims_table_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy-fleet.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(super::super::MIGRATION_V1).unwrap();
        }
        let ledger = Ledger::open(&path).unwrap();
        let lease = granted(
            ledger
                .claim_dispatch("issue:1136", "run-a", 1_000, TTL)
                .unwrap(),
        );
        assert_eq!(lease.fence, 1);
    }

    #[test]
    fn heartbeat_interval_is_a_third_of_the_ttl_and_never_zero() {
        let lease = DispatchLease {
            claim_key: "k".into(),
            owner: "o".into(),
            fence: 1,
            acquired_at_ms: 0,
            expires_at_ms: 900,
            ttl_ms: 900,
        };
        assert_eq!(lease.heartbeat_interval_ms(), 300);
        let tiny = DispatchLease { ttl_ms: 2, ..lease };
        assert_eq!(tiny.heartbeat_interval_ms(), 1);
    }
}
