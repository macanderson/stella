//! What `prune` keeps and what it drops.
//!
//! Its own file because `work_journal.rs` sits at the size ceiling and is
//! closed to growth.

use super::tests::scratch;
use super::*;

/// Whether the store still holds `oid` at all. `cat-file -e` is the
/// cheapest way to ask. It is also the only check that tells "the ref is
/// gone" from "the bytes are gone".
fn object_exists(journal: &WorkJournal, oid: &str) -> bool {
    journal.git(&["cat-file", "-e", oid]).is_ok()
}

#[test]
fn pruning_a_session_unreaches_its_objects_so_gc_can_reclaim_them() {
    // Deleting the ref is what frees the objects. `gc` must KEEP
    // whatever a ref reaches. A store that deletes no refs grows
    // forever, however often it packs.
    let (_guard, ws, store) = scratch();
    std::fs::write(ws.join("a.txt"), "theirs\n").unwrap();
    let theirs = WorkJournal::open_in(&store, &ws, "ses-old").unwrap();
    let dropped = theirs.record(&["a.txt".into()], &[], "their work").unwrap();
    theirs.mark_turn(1, &dropped).unwrap();

    std::fs::write(ws.join("b.txt"), "mine\n").unwrap();
    let mine = WorkJournal::open_in(&store, &ws, "ses-live").unwrap();
    let kept = mine.record(&["b.txt".into()], &[], "my work").unwrap();

    let report = mine
        .prune(&JournalPrunePolicy {
            older_than: Some(Duration::ZERO),
            gc: true,
            ..Default::default()
        })
        .unwrap();

    assert_eq!(report.aged_out, 1, "the running session is never pruned");
    assert_eq!(report.total_sessions(), 1);
    assert_eq!(report.refs_deleted, 2, "the head plus its one turn mark");
    assert_eq!(report.indexes_removed, 1, "and the sidecar index with it");
    assert!(report.gc_ran);

    assert!(
        theirs.session_tip().is_none(),
        "the pruned session's head is gone"
    );
    assert!(
        theirs.read_at_turn(1, "a.txt").is_err(),
        "and so is its turn mark"
    );
    assert!(
        !index_file_path(&mine.store_root, &mine.workspace_id, "ses-old").exists(),
        "the pruned session's index file is gone"
    );
    assert_eq!(
        mine.session_tip().as_deref(),
        Some(kept.as_str()),
        "the live session is untouched"
    );

    assert!(object_exists(&mine, &kept), "a retained commit stays");
    assert!(
        !object_exists(&mine, &dropped),
        "the pruned commit's objects were actually reclaimed"
    );
}

#[test]
fn the_session_ceiling_evicts_the_oldest_and_reports_what_the_guard_blocks() {
    let (_guard, ws, store) = scratch();
    std::fs::write(ws.join("a.txt"), "shared\n").unwrap();
    // All three land in the same second. The tiebreak is then the
    // session name, so the order is fixed: ses-1 first (this handle's
    // own, and so protected), then ses-2, then ses-3.
    for session in ["ses-1", "ses-2", "ses-3"] {
        let journal = WorkJournal::open_in(&store, &ws, session).unwrap();
        journal.record(&["a.txt".into()], &[], session).unwrap();
    }
    let mine = WorkJournal::open_in(&store, &ws, "ses-1").unwrap();

    let report = mine
        .prune(&JournalPrunePolicy {
            max_sessions: Some(1),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(report.aged_out, 0, "no age window means no age phase");
    assert_eq!(report.ceiling_evicted, 2);
    assert_eq!(report.still_over_ceiling, 0);
    assert_eq!(mine.recorded_sessions().unwrap().len(), 1);

    // A ceiling the guard cannot meet is reported, never forced. The
    // running session outranks any policy.
    let report = mine
        .prune(&JournalPrunePolicy {
            max_sessions: Some(0),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(report.ceiling_evicted, 0);
    assert_eq!(report.still_over_ceiling, 1);
    assert!(mine.session_tip().is_some());
}

#[test]
fn an_empty_policy_is_a_no_op_and_a_dry_run_deletes_nothing() {
    let (_guard, ws, store) = scratch();
    std::fs::write(ws.join("a.txt"), "theirs\n").unwrap();
    let theirs = WorkJournal::open_in(&store, &ws, "ses-old").unwrap();
    let tip = theirs.record(&["a.txt".into()], &[], "their work").unwrap();
    theirs.mark_turn(1, &tip).unwrap();
    let mine = WorkJournal::open_in(&store, &ws, "ses-live").unwrap();
    mine.record(&["a.txt".into()], &[], "my work").unwrap();

    assert_eq!(
        mine.prune(&JournalPrunePolicy::default()).unwrap(),
        JournalPruneReport::default(),
        "a policy with neither knob set deletes nothing"
    );

    let report = mine
        .prune(&JournalPrunePolicy {
            older_than: Some(Duration::ZERO),
            gc: true,
            dry_run: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(report.aged_out, 1, "a dry run reports what it would drop");
    assert_eq!(report.refs_deleted, 2);
    assert_eq!(report.indexes_removed, 0);
    assert!(!report.gc_ran, "and never reclaims");
    assert_eq!(
        theirs.session_tip().as_deref(),
        Some(tip.as_str()),
        "the session it named is still there"
    );
}

/// The tip of `key`'s own record, read through a fresh handle.
fn tip_of(store: &Path, ws: &Path, key: &str) -> Option<String> {
    WorkJournal::open_in(store, ws, key).unwrap().session_tip()
}

#[test]
fn a_sessions_lanes_are_reaped_with_it_and_are_never_counted_as_sessions() {
    let (_guard, ws, store) = scratch();
    std::fs::write(ws.join("a.txt"), "shared\n").unwrap();

    // One lead with two worker lanes, plus two sessions with none. All
    // five land in the same second. The tiebreak is then the key, so the
    // order is fixed: `ses-1` and its lanes sort ahead of `ses-2` and
    // `ses-3`.
    let lane_a = lane::lane_key("ses-1", "req:1");
    let lane_b = lane::lane_key("ses-1", "req:2");
    for key in ["ses-1", lane_a.as_str(), lane_b.as_str(), "ses-2", "ses-3"] {
        let journal = WorkJournal::open_in(&store, &ws, key).unwrap();
        journal.record(&["a.txt".into()], &[], key).unwrap();
    }
    // A handle on none of them. The guard protects nothing here, so the
    // ceiling alone decides.
    let mine = WorkJournal::open_in(&store, &ws, "ses-9").unwrap();

    assert_eq!(
        mine.recorded_sessions().unwrap().len(),
        3,
        "five records, three sessions: a lane is part of the session that \
         dispatched it"
    );
    assert_eq!(
        mine.prune(&JournalPrunePolicy {
            max_sessions: Some(3),
            ..Default::default()
        })
        .unwrap(),
        JournalPruneReport::default(),
        "three sessions already fit under a ceiling of three — counting the \
         lanes would evict real sessions to make room for one session's lanes"
    );
    assert!(
        tip_of(&store, &ws, &lane_a).is_some(),
        "and deletes nothing"
    );

    let report = mine
        .prune(&JournalPrunePolicy {
            max_sessions: Some(2),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(report.ceiling_evicted, 1, "one session over, one evicted");
    assert_eq!(report.lanes_removed, 2, "and its two lanes go with it");
    assert_eq!(
        report.refs_deleted, 3,
        "the lead's head plus one per lane — a lane left standing would keep \
         its own objects reachable forever"
    );
    assert_eq!(report.indexes_removed, 3, "each record's sidecar index too");
    assert_eq!(report.still_over_ceiling, 0);

    assert!(
        tip_of(&store, &ws, "ses-1").is_none(),
        "the lead's record is gone"
    );
    assert!(
        tip_of(&store, &ws, &lane_a).is_none(),
        "and its first lane's"
    );
    assert!(
        tip_of(&store, &ws, &lane_b).is_none(),
        "and its second lane's"
    );
    assert!(
        !index_file_path(&mine.store_root, &mine.workspace_id, &lane_a).exists(),
        "a reaped lane leaves no sidecar index behind"
    );
    assert!(
        tip_of(&store, &ws, "ses-2").is_some() && tip_of(&store, &ws, "ses-3").is_some(),
        "the sessions under the ceiling are untouched"
    );
}

#[test]
fn a_lane_handle_never_prunes_the_session_it_hangs_off() {
    // The guard protects the session, not just the key. A prune run from
    // a lane's own handle must not delete the lead it hangs off.
    let (_guard, ws, store) = scratch();
    std::fs::write(ws.join("a.txt"), "shared\n").unwrap();
    let lead = WorkJournal::open_in(&store, &ws, "ses-1").unwrap();
    let tip = lead.record(&["a.txt".into()], &[], "lead").unwrap();
    let worker = WorkJournal::open_in(&store, &ws, &lane::lane_key("ses-1", "req:1")).unwrap();
    worker.record(&["a.txt".into()], &[], "lane").unwrap();

    let report = worker
        .prune(&JournalPrunePolicy {
            older_than: Some(Duration::ZERO),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(report.total_sessions(), 0, "nothing here is prunable");
    assert_eq!(
        lead.session_tip().as_deref(),
        Some(tip.as_str()),
        "the lead this lane hangs off is still there"
    );
}
