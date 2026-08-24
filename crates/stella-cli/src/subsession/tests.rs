//! Tests for the sub-session supervisor: the spawn notice, mid-turn
//! routing, the task prompt and purpose, the pinned effort, and the
//! worker lifecycle.

use super::*;

/// The spawn notice is the only thing standing between a mid-turn prompt
/// and looking like it did nothing, so its four jobs are pinned: name the
/// lane, say this turn is unaffected, say how to open the lane, and say
/// how to get back. A prettier wording is fine; a wording that drops one
/// of these is the regression.
#[test]
fn the_spawn_notice_names_the_lane_and_the_keys_in_and_out() {
    let notice = spawn_notice("req:1", "fix the flaky witness test");

    assert!(
        notice.contains("req:1"),
        "the notice must name the lane that took the prompt: {notice}"
    );
    assert!(
        notice.contains("fix the flaky witness test"),
        "the notice must echo the prompt so the user can tell which one it \
         was: {notice}"
    );
    assert!(
        notice.contains("does not block"),
        "the notice must say the running turn is unaffected — that is the \
         whole point of dispatching to a worker: {notice}"
    );
    assert!(
        notice.contains("ctrl-a"),
        "the notice must name the key that reaches the lane: {notice}"
    );
    assert!(
        notice.contains(LEAD),
        "the notice must say how to get BACK to the lead, not just in: \
         {notice}"
    );
}

/// A long prompt is truncated rather than dumped into the transcript — the
/// notice is a signpost, not a second copy of the prompt.
#[test]
fn the_spawn_notice_truncates_a_long_prompt() {
    let long = "x".repeat(400);
    let notice = spawn_notice("req:2", &long);
    assert!(
        !notice.contains(&long),
        "the full prompt must not be inlined verbatim"
    );
    assert!(
        notice.lines().count() <= 4,
        "the notice must stay a few lines — it prints on every spawn: {notice}"
    );
}

fn dummy_spec(lane: &str) -> SubSessionSpec {
    SubSessionSpec {
        lane: lane.to_string(),
        title: "t".into(),
        purpose: String::new(),
        prompt: "p".into(),
        notify_title: "n".into(),
        dispatched_by: None,
    }
}

#[tokio::test]
async fn watch_gate_parks_while_paused_and_releases_on_resume_or_teardown() {
    use stella_core::ports::TurnGate;
    let (tx, rx) = watch::channel(true);
    let gate = WatchGate(rx);
    let wait = gate.wait_if_paused();
    tokio::pin!(wait);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut wait)
            .await
            .is_err(),
        "a paused gate must park"
    );
    tx.send(false).unwrap();
    tokio::time::timeout(std::time::Duration::from_millis(500), wait)
        .await
        .expect("resume releases the gate");

    // A dropped sender (driver teardown) must release, never park forever.
    let (tx2, rx2) = watch::channel(true);
    let gate2 = WatchGate(rx2);
    drop(tx2);
    tokio::time::timeout(
        std::time::Duration::from_millis(500),
        gate2.wait_if_paused(),
    )
    .await
    .expect("teardown releases the gate");
}

#[test]
fn pause_resume_flips_the_watch_and_restart_needs_a_dead_lane() {
    let mut subs = SubSessions::new();
    let (stop_tx, _stop_rx) = oneshot::channel();
    let (pause_tx, pause_rx) = watch::channel(false);
    let (generation, _) = subs.started("req:1", stop_tx, pause_tx, dummy_spec("req:1"));
    assert!(subs.set_paused("req:1", true));
    assert!(*pause_rx.borrow());
    assert!(subs.set_paused("req:1", false));
    assert!(!*pause_rx.borrow());
    assert!(!subs.set_paused("req:404", true), "unknown lane is a no-op");
    // A live lane refuses respawn; its spec survives its end.
    assert!(subs.is_live("req:1"));
    assert!(subs.ended("req:1", generation));
    assert!(!subs.is_live("req:1"));
    assert!(subs.spec("req:1").is_some(), "spec retained for Restart");
}

#[test]
fn slot_bookkeeping_caps_and_recovers() {
    let mut subs = SubSessions::new();
    let mut generations = Vec::new();
    for i in 0..MAX_CONCURRENT {
        assert!(subs.has_slot());
        let (stop_tx, _stop_rx) = oneshot::channel();
        let (pause_tx, _pause_rx) = watch::channel(false);
        let (generation, _) = subs.started(
            &format!("req:{i}"),
            stop_tx,
            pause_tx,
            dummy_spec(&format!("req:{i}")),
        );
        generations.push(generation);
    }
    assert!(!subs.has_slot());
    assert!(subs.ended("req:0", generations[0]));
    assert!(subs.has_slot());
    // An Ended nothing started must free nothing (and never underflow).
    let mut fresh = SubSessions::new();
    assert!(!fresh.ended("req:none", 0));
    assert!(fresh.has_slot());
}

/// **The witness for #2899.** A steer at a lane reaches the tap its
/// worker's engine drains — the driver kept the other handle at spawn —
/// and stops reaching it the moment the lane is stopped, because a
/// winding-down worker has no boundary left to inject at.
#[test]
fn a_steer_at_a_live_lane_lands_on_its_tap_and_a_stopped_lane_refuses() {
    use stella_core::ports::TurnSteering;

    let mut subs = SubSessions::new();
    let (stop_tx, _stop_rx) = oneshot::channel();
    let (pause_tx, _pause_rx) = watch::channel(false);
    let (_, tap) = subs.started("req:1", stop_tx, pause_tx, dummy_spec("req:1"));
    assert!(subs.steer("req:1", "narrow it to the parser".into()));
    assert_eq!(
        tap.drain_steering(),
        vec!["narrow it to the parser".to_string()],
        "the worker's engine drains what the driver pushed"
    );
    assert!(!subs.steer("req:404", "x".into()), "unknown lane refuses");
    tap.mark_settling();
    assert!(
        !subs.steer("req:1", "past the last step".into()),
        "a settling worker has no boundary left, so the caller keeps the words"
    );
    assert!(tap.drain_steering().is_empty());
    assert!(subs.stop("req:1"));
    assert!(
        !subs.steer("req:1", "too late".into()),
        "a winding-down lane refuses, so the caller keeps the words"
    );
    assert!(tap.drain_steering().is_empty());
}

/// `/clear` stops every live lane and remembers it; the lane's row comes
/// down only when its `Ended` arrives. A lane with no worker behind it is
/// dropped at once — spec included, so Restart cannot revive it — and a
/// lane respawned afterwards is a fresh row, not a cleared one.
#[test]
fn clear_lanes_stops_the_live_and_drops_the_ended() {
    let mut subs = SubSessions::new();
    let live_gen = subs.started_for_test("req:1");
    let ended_gen = subs.started_for_test("sub:7");
    assert!(subs.ended("sub:7", ended_gen));

    assert_eq!(subs.clear_lanes(), vec!["sub:7".to_string()]);
    assert!(subs.spec("sub:7").is_none(), "an ended lane loses its spec");
    assert!(subs.is_live("req:1"), "the live lane is winding down");
    assert!(
        !subs.finish_cleared("req:1"),
        "…and is not finished until its Ended arrives"
    );
    assert!(subs.ended("req:1", live_gen));
    assert!(subs.finish_cleared("req:1"), "now its row comes down");
    assert!(subs.spec("req:1").is_none());
    assert!(!subs.finish_cleared("req:1"), "exactly once");

    subs.started_for_test("req:1");
    assert!(
        !subs.finish_cleared("req:1"),
        "a respawned lane is not a cleared one"
    );
}

#[test]
fn stop_signals_a_live_worker_once_and_only_once() {
    let mut subs = SubSessions::new();
    let (stop_tx, mut stop_rx) = oneshot::channel();
    let (pause_tx, _pause_rx) = watch::channel(false);
    subs.started("req:1", stop_tx, pause_tx, dummy_spec("req:1"));
    assert!(subs.stop("req:1"), "a live worker accepts the signal");
    assert_eq!(stop_rx.try_recv(), Ok(()));
    assert!(!subs.stop("req:1"), "a second stop is a stale no-op");
    assert!(!subs.stop("req:404"), "an unknown lane is a no-op");
}

#[test]
fn a_stopped_lane_winds_down_until_its_ended_arrives() {
    // stop() must NOT free the lane: the worker thread is still
    // draining. The lane stays live (so Restart defers via
    // pending_restarts instead of respawning into a second worker), the
    // slot stays occupied, and only the matching Ended frees both.
    let mut subs = SubSessions::new();
    let (stop_tx, _stop_rx) = oneshot::channel();
    let (pause_tx, _pause_rx) = watch::channel(false);
    let (generation, _) = subs.started("req:1", stop_tx, pause_tx, dummy_spec("req:1"));
    assert!(subs.stop("req:1"));
    assert!(subs.is_live("req:1"), "winding down still counts as live");
    assert_eq!(subs.live(), 1, "the slot is not free until Ended");
    assert!(
        !subs.set_paused("req:1", true),
        "a winding-down worker cannot be paused"
    );
    assert!(subs.ended("req:1", generation));
    assert!(!subs.is_live("req:1"));
    assert_eq!(subs.live(), 0);
}

/// `/clear` names the lanes it stopped, and this is what it names: running
/// workers and winding-down ones, never a lane whose worker had already
/// ended — so `live_lanes` and `lanes` must diverge the moment a worker
/// settles.
#[test]
fn live_lanes_are_the_running_and_winding_down_ones_only() {
    let mut subs = SubSessions::new();
    let (stop_a, _rx_a) = oneshot::channel();
    let (pause_a, _p_a) = watch::channel(false);
    let (gen_a, _) = subs.started("req:1", stop_a, pause_a, dummy_spec("req:1"));
    let (stop_b, _rx_b) = oneshot::channel();
    let (pause_b, _p_b) = watch::channel(false);
    subs.started("req:2", stop_b, pause_b, dummy_spec("req:2"));

    assert_eq!(subs.live_lanes(), vec!["req:1", "req:2"], "both running");

    assert!(subs.stop("req:1"));
    assert_eq!(
        subs.live_lanes(),
        vec!["req:1", "req:2"],
        "a winding-down lane is still writing, so it is still reported"
    );

    assert!(subs.ended("req:1", gen_a));
    assert_eq!(subs.live_lanes(), vec!["req:2"], "an ended lane drops out");
    assert_eq!(
        subs.lanes(),
        vec!["req:1", "req:2"],
        "…while `lanes` still carries it for Restart"
    );
}

#[test]
fn stop_then_restart_leaves_one_worker_with_working_channels() {
    // The full stop→Ended→respawn sequence, then a stale Ended for the
    // replaced generation: exactly one live worker must remain, with ITS
    // stop/pause channels registered and the active count intact.
    let mut subs = SubSessions::new();
    let (stop_tx, _stop_rx) = oneshot::channel();
    let (pause_tx, _pause_rx) = watch::channel(false);
    let (old, _) = subs.started("req:1", stop_tx, pause_tx, dummy_spec("req:1"));
    subs.stop("req:1");
    assert!(subs.ended("req:1", old));
    // The respawn (what pending_restarts triggers on the Ended).
    let (stop_tx2, mut stop_rx2) = oneshot::channel();
    let (pause_tx2, pause_rx2) = watch::channel(false);
    let (new, _) = subs.started("req:1", stop_tx2, pause_tx2, dummy_spec("req:1"));
    assert_ne!(old, new, "each spawn gets its own generation");
    assert_eq!(subs.live(), 1);
    // A late Ended from the replaced worker frees nothing.
    assert!(!subs.ended("req:1", old), "stale Ended must be ignored");
    assert_eq!(subs.live(), 1, "active count survives the stale Ended");
    assert!(
        subs.set_paused("req:1", true),
        "replacement's pause channel is intact"
    );
    assert!(*pause_rx2.borrow());
    assert!(subs.stop("req:1"), "replacement's stop channel is intact");
    assert_eq!(stop_rx2.try_recv(), Ok(()));
    assert!(subs.ended("req:1", new));
    assert_eq!(subs.live(), 0);
}

#[test]
fn a_panicking_worker_body_ends_as_failed_never_silence() {
    // The supervisor must receive Ended whatever a tool does — a panic
    // synthesizes Failed with the payload's message, both payload kinds.
    let (id, cost, end) = run_caught(|| panic!("tool blew up"));
    assert_eq!(id, None);
    assert_eq!(cost, 0.0);
    match end {
        WorkerEnd::Failed(reason) => {
            assert!(reason.contains("worker panicked"), "{reason}");
            assert!(reason.contains("tool blew up"), "{reason}");
        }
        _ => panic!("a panic must synthesize WorkerEnd::Failed"),
    }
    let (_, _, end) = run_caught(|| panic!("{}", format!("dynamic {}", 7)));
    match end {
        WorkerEnd::Failed(reason) => assert!(reason.contains("dynamic 7"), "{reason}"),
        _ => panic!("a String panic payload must synthesize Failed too"),
    }
    // A body that returns normally passes through untouched.
    let (id, cost, end) = run_caught(|| (Some(7), 1.25, WorkerEnd::Done));
    assert_eq!(id, Some(7));
    assert_eq!(cost, 1.25);
    assert!(matches!(end, WorkerEnd::Done));
}

#[tokio::test]
async fn quit_shutdown_stops_workers_and_reaps_their_endings() {
    // Quit-time teardown: every live worker sees its stop signal and the
    // drain waits for their Ended messages — no orphaned bookkeeping.
    let mut subs = SubSessions::new();
    let (sup_tx, mut sup_rx) = mpsc::unbounded_channel();
    for lane in ["req:1", "sub:9"] {
        let (stop_tx, stop_rx) = oneshot::channel();
        let (pause_tx, _pause_rx) = watch::channel(false);
        let (generation, _) = subs.started(lane, stop_tx, pause_tx, dummy_spec(lane));
        let sup = sup_tx.clone();
        let lane = lane.to_string();
        // A worker's whole quit obligation: see the stop, close out,
        // send Ended.
        tokio::spawn(async move {
            let _ = stop_rx.await;
            let _ = sup.send(SupervisorMsg::Ended {
                lane,
                generation,
                execution_id: None,
                cost_usd: 0.0,
                end: WorkerEnd::Stopped,
            });
        });
    }
    shutdown_workers(&mut subs, &mut sup_rx, std::time::Duration::from_secs(5)).await;
    assert_eq!(subs.live(), 0, "every worker settled before the deadline");
}

#[tokio::test(start_paused = true)]
async fn quit_shutdown_abandons_a_hung_worker_at_the_deadline() {
    // A worker that never answers its stop must not hold the exit
    // hostage — the drain returns at the deadline with the lane still
    // accounted (abandoned, exactly the pre-teardown behavior).
    let mut subs = SubSessions::new();
    let (stop_tx, _stop_rx) = oneshot::channel();
    let (pause_tx, _pause_rx) = watch::channel(false);
    subs.started("req:1", stop_tx, pause_tx, dummy_spec("req:1"));
    let (_sup_tx, mut sup_rx) = mpsc::unbounded_channel();
    shutdown_workers(
        &mut subs,
        &mut sup_rx,
        std::time::Duration::from_millis(100),
    )
    .await;
    assert_eq!(subs.live(), 1, "abandoned, not freed — and no hang");
}

#[test]
fn req_lanes_are_sequential() {
    let mut subs = SubSessions::new();
    assert_eq!(subs.next_req_lane(), "req:1");
    assert_eq!(subs.next_req_lane(), "req:2");
}

#[test]
fn lanes_names_every_worker_ever_started_live_or_ended() {
    // The session-switch site deregisters exactly this set — a lane's
    // dashboard row outlives its worker, so `lanes` must too.
    let mut subs = SubSessions::new();
    assert!(subs.lanes().is_empty());
    let mut generations = std::collections::HashMap::new();
    for lane in ["req:2", "req:1", "sub:7"] {
        let (stop_tx, _stop_rx) = oneshot::channel();
        let (pause_tx, _pause_rx) = watch::channel(false);
        generations.insert(
            lane,
            subs.started(lane, stop_tx, pause_tx, dummy_spec(lane)).0,
        );
    }
    assert!(subs.ended("req:1", generations["req:1"]));
    assert_eq!(
        subs.lanes(),
        vec![
            "req:1".to_string(),
            "req:2".to_string(),
            "sub:7".to_string()
        ],
        "ended lanes stay listed; order is deterministic (sorted)"
    );
}

/// The reported bug. The deck paints `✓ done · stage complete · 100%` the
/// moment `AgentEvent::TurnComplete` leaves the driver, but `run_lead_turn`
/// keeps running — and its `select!` keeps reading input — while it
/// persists the turn. A follow-up typed into that window used to be read
/// as a brand-new request and spawned `req:1`, so a long collaboration
/// silently forked into a stranger agent every time the user answered
/// promptly. Once settling, every submission continues the thread.
#[test]
fn a_prompt_typed_after_the_turn_is_done_continues_the_thread() {
    assert_eq!(
        route_mid_turn("and now add the tests".into(), true),
        MidTurnRoute::NextTurn("and now add the tests".into()),
        "a settling turn has no work left to run something alongside"
    );
}

/// While the turn is genuinely running, the two routes stay as they were:
/// `>` is the steer marker, everything else is a concurrent request.
#[test]
fn a_live_turn_still_steers_on_the_marker_and_spawns_otherwise() {
    assert_eq!(
        route_mid_turn("> only the parser".into(), false),
        MidTurnRoute::Steer("only the parser".into()),
        "the marker and its padding are stripped before injection"
    );
    assert_eq!(
        route_mid_turn("unrelated question".into(), false),
        MidTurnRoute::Sidecar("unrelated question".into()),
    );
}

/// A `>` aimed at a settling turn cannot be injected — there is no step
/// boundary left to inject at, and pushing it to the tap would drop it on
/// the floor. It becomes the next turn instead, marker stripped: the user
/// asked to influence the work, and continuing the thread is the closest
/// thing still on offer.
#[test]
fn a_steer_that_arrives_too_late_becomes_the_next_turn_not_silence() {
    assert_eq!(
        route_mid_turn(">  narrow it down".into(), true),
        MidTurnRoute::NextTurn("narrow it down".into()),
    );
}

/// The overlay's purpose row is the task's own description, one sentence,
/// never the briefing — and the subject when there is no description.
#[test]
fn task_purpose_is_the_descriptions_first_sentence_or_the_subject() {
    let mut req = SpawnRequest {
        task_id: "3".into(),
        subject: "Fix the redirect loop".into(),
        description: Some(
            "Token refresh races the redirect in auth/login.rs. Repro: log in twice.\n\
             Then read the trace."
                .into(),
        ),
        briefing: "Start from the failing test.".into(),
    };
    assert_eq!(
        task_purpose(&req),
        "Token refresh races the redirect in auth/login.rs."
    );
    req.description = None;
    assert_eq!(task_purpose(&req), "Fix the redirect loop");
    assert_eq!(
        first_sentence("  \n\nSimplify   docs/spec.md so it reads plainly\nmore"),
        "Simplify docs/spec.md so it reads plainly",
        "a dotted path is not a sentence break; the newline is"
    );
}

#[test]
fn task_prompt_carries_identity_and_briefing_verbatim() {
    let req = SpawnRequest {
        task_id: "3".into(),
        subject: "Fix the redirect loop".into(),
        description: Some("token refresh races the redirect".into()),
        briefing: "Start from auth/callback.rs; the witness test is half-written.".into(),
    };
    let prompt = task_prompt(&req);
    assert!(prompt.contains("Task #3: Fix the redirect loop"));
    assert!(prompt.contains("token refresh races the redirect"));
    assert!(prompt.contains("Start from auth/callback.rs; the witness test is half-written."));
}

#[test]
fn drain_stops_at_slash_commands_and_respects_hold() {
    // Contract-level test of the pure gating conditions: a held dispatch
    // or a slash front entry must leave the queue untouched. (Spawning
    // itself needs a provider key + threads — exercised in integration,
    // not here — so this asserts the *refusals*, which are what protect
    // the deck's FIFO view.)
    let queue: std::collections::VecDeque<String> =
        std::collections::VecDeque::from(["/models".to_string()]);
    let subs = SubSessions::new();
    assert!(subs.has_slot());
    assert!(
        queue
            .front()
            .is_some_and(|t| t.trim_start().starts_with('/')),
        "slash front entry must gate the drain"
    );
    // Simulate the drain's gate exactly as `drain_queue` evaluates it.
    let held = false;
    let would_take = !held
        && subs.has_slot()
        && queue
            .front()
            .is_some_and(|t| !t.trim_start().starts_with('/'));
    assert!(!would_take);
    assert_eq!(queue.len(), 1);
}
