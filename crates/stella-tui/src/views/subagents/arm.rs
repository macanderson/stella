// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The SUB-AGENTS overlay's two-press verbs. A verb with no undo — kill,
//! delete, restart — takes two presses of its key: the first arms, the
//! second fires, any other key disarms. The arm is scoped to the lane it was
//! pressed on, so moving the cursor between presses disarms too — the same
//! per-row shape the AGENTS tab's uninstall uses, chosen over a bare flag
//! because a fire aimed at whatever the cursor lands on is exactly the
//! misfire the second press exists to prevent.

/// Which verb the first press armed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmedVerb {
    /// `ctrl-x` — the next `ctrl-x` stops the lane's worker.
    Kill,
    /// `x` — the next `x` removes the lane's row from the deck for good.
    Delete,
    /// `r` — the next `r` respawns the lane from its retained spec.
    Restart,
}

/// One armed verb on one lane. Held by the overlay for exactly one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Armed {
    pub verb: ArmedVerb,
    pub lane: String,
}

impl Armed {
    pub fn new(verb: ArmedVerb, lane: &str) -> Self {
        Self {
            verb,
            lane: lane.to_string(),
        }
    }

    /// The footer line shown while this arm is up: what the second press
    /// does, and that any other key stands down.
    pub fn footer(&self) -> String {
        match self.verb {
            ArmedVerb::Kill => format!(
                " ctrl-x again kills {} · any other key keeps it ",
                self.lane
            ),
            ArmedVerb::Delete => format!(
                " x again removes {}'s row from the deck · any other key keeps it ",
                self.lane
            ),
            ArmedVerb::Restart => format!(
                " r again restarts {} from step 1 · any other key leaves it ",
                self.lane
            ),
        }
    }
}

/// Whether a press of `verb` on `lane` is the second press — i.e. the arm
/// taken from the overlay matches both. The caller has already `take`n the
/// arm, so a mismatch leaves the overlay disarmed by construction.
pub fn fires(taken: &Option<Armed>, verb: ArmedVerb, lane: &str) -> bool {
    taken
        .as_ref()
        .is_some_and(|a| a.verb == verb && a.lane == lane)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The arm fires only for the same verb on the same lane.
    #[test]
    fn an_arm_fires_only_for_its_own_verb_and_lane() {
        let armed = Some(Armed::new(ArmedVerb::Delete, "sub:2"));
        assert!(fires(&armed, ArmedVerb::Delete, "sub:2"));
        assert!(!fires(&armed, ArmedVerb::Restart, "sub:2"), "other verb");
        assert!(!fires(&armed, ArmedVerb::Delete, "sub:3"), "other lane");
        assert!(!fires(&None, ArmedVerb::Delete, "sub:2"), "nothing armed");
    }

    /// Each armed footer names the lane and the key that fires.
    #[test]
    fn the_armed_footer_names_the_lane_and_the_key() {
        let kill = Armed::new(ArmedVerb::Kill, "sub:1").footer();
        assert!(kill.contains("ctrl-x again kills sub:1"), "{kill}");
        let delete = Armed::new(ArmedVerb::Delete, "sub:1").footer();
        assert!(delete.contains("x again removes sub:1"), "{delete}");
        let restart = Armed::new(ArmedVerb::Restart, "sub:1").footer();
        assert!(restart.contains("r again restarts sub:1"), "{restart}");
    }
}
