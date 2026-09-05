//! How a worker lane's record is named, and how long it lives.
//!
//! A deck session can start worker lanes. Each lane keeps a record of its
//! own. It does not write into the lead's. So a lane that is killed still has
//! a transcript, and a later spawn on the same lane id can pick it up. A
//! lane's key is the session id, then [`LANE_SEPARATOR`], then the lane id.
//!
//! # How long a lane record lives
//!
//! Just as long as the session that started it. A lane is not a session, so
//! it is not a unit of retention. It has no clock of its own. It takes no
//! line against the ceiling. When
//! [`crate::work_journal::WorkJournal::prune`] drops a session it drops that
//! session's lanes too, in the same batch. The age cutoff reads the whole
//! group by its newest tip.
//!
//! Left to age out alone, a lane outlives the session it belongs to. Its
//! objects stay reachable, and `gc` must keep whatever a ref reaches.
//!
//! Lane ids are not capped. A deck session can hand out `req:1` … `req:n`
//! all day. Each lane that wrote a record leaves a name under
//! `refs/stella/`. Counted as sessions, one busy session fills a ceiling
//! meant to hold many, and pushes real sessions out.
//!
//! Nothing drops a lane record when the lane ends. `prune` runs when a
//! command asks for it, never on exit. A lane that dies leaves a terminal
//! frame ([`crate::lane_frame`]). The frame stands until that lane finishes
//! a later try. Dropping it on exit would take it away before the lead read
//! it.
//!
//! # Where the key shape lives
//!
//! Here, in the store. `prune` has to know which keys a session owns before
//! it can drop them. The crate that starts lanes still picks which lanes
//! exist and what they are called. It builds their keys with [`lane_key`].

/// What sits between a session id and a lane id in a lane's journal key.
///
/// Not `/`. A key is part of a file name too
/// (`<workspace-id>.<key>.index`), and a `/` there reads as a path, into a
/// folder nothing makes. Git refs take `/` happily, so only one of the two
/// axes would break, and it would break in silence. `__` is plain to git and
/// to a file system alike.
pub const LANE_SEPARATOR: &str = "__";

/// The journal key one lane of `session` writes its record under.
///
/// The lane id is made ref-safe on the way in. Every lane id the deck mints
/// holds a `:` (`req:<n>`, `sub:<task-id>`), and `:` is not legal in a ref
/// name. So this is the normal case, not an edge one.
pub fn lane_key(session: &str, lane: &str) -> String {
    format!("{session}{LANE_SEPARATOR}{}", lane.replace(':', "-"))
}

/// The session a journal key belongs to. For a session's own record that is
/// the key itself. For a lane's it is the part before [`LANE_SEPARATOR`].
///
/// A session id (`ses-<ms>-<pid>`) holds no `__`, so the split is exact for
/// every key this workspace mints. A key that does hold one, and names no
/// session that ever wrote a record, still groups under its prefix. That is
/// the right answer: such keys are lanes of a lead that wrote nothing, and
/// they belong together.
pub fn owning_session(key: &str) -> &str {
    match key.split_once(LANE_SEPARATOR) {
        Some((session, _)) => session,
        None => key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lane_key_names_its_session_and_survives_the_round_trip() {
        let key = lane_key("ses-17-42", "req:3");
        assert_eq!(key, "ses-17-42__req-3");
        assert_eq!(owning_session(&key), "ses-17-42");
        assert!(
            !key.contains(':') && !key.contains('/'),
            "a key is part of a ref name and part of a file name: {key}"
        );
    }

    #[test]
    fn a_session_key_owns_itself() {
        assert_eq!(owning_session("ses-17-42"), "ses-17-42");
    }
}
