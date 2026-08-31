// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One question, answered here: how many workers should run right now?
//!
//! `plan` has always computed this number from the probes and the learned
//! calibration. Until this module, nothing consumed it. The fleet fanned
//! out at a written-down width, and `drive` ran one worker, so the
//! governor probed, learned, and spoke into a void.
//!
//! The math itself is [`stella_autonomy::recommended_parallelism`]. This
//! module adds the live reading a caller needs. The supply comes through
//! the probes, so every `SELF_DRIVING_PROBE_*` pin is honoured. The
//! learned `calibration` comes from this repo's own state dir. The limits
//! come through the same env knobs the loop reads, and
//! `SELF_DRIVING_PARALLEL_MAX` is first among them.

use super::state::{self, LoopState};

/// How many workers this box should run right now, for this repository.
///
/// Never fails. A box with no state directory still has a width worth
/// knowing. So that arm reads the live probes and takes the seed value —
/// the same hopeful seed a first run writes.
pub(crate) fn recommended_parallelism() -> u32 {
    match LoopState::open() {
        Ok(st) => for_state(&st),
        Err(_) => {
            let limits = state::aimd_limits();
            stella_autonomy::recommended_parallelism(
                super::probes::supply(&state::repo_root()),
                &stella_autonomy::Calibration::seeded(&limits),
                state::floors(),
                &limits,
            )
        }
    }
}

/// The same answer against a loop state the caller already holds.
pub(crate) fn for_state(st: &LoopState) -> u32 {
    stella_autonomy::recommended_parallelism(
        super::probes::supply(&st.repo_root),
        &st.calibration(),
        state::floors(),
        &state::aimd_limits(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The witness.** The governor's number is derived, end to end, from
    /// the senses an operator can pin. Every `SELF_DRIVING_PROBE_*` pin
    /// reaches the supply. The saved calibration sets the cap. And
    /// `SELF_DRIVING_PARALLEL_MAX` still outranks the machine.
    ///
    /// One test rather than three, on purpose. These knobs are process
    /// env, shared by every thread. One body that runs in order is what
    /// keeps the pins from racing across the suite.
    #[test]
    fn the_governor_answers_to_the_probe_pins_and_the_calibration() {
        let dir = tempfile::tempdir().expect("a state dir for the test");
        // Pin every sense: a healthy 8-core box on mains power, quiet.
        let pins = [
            ("SELF_DRIVING_STATE_DIR", dir.path().display().to_string()),
            ("SELF_DRIVING_PROBE_CPU", "8".into()),
            ("SELF_DRIVING_PROBE_LOAD1", "1".into()),
            ("SELF_DRIVING_PROBE_MEM_TOTAL_GB", "16".into()),
            ("SELF_DRIVING_PROBE_MEM_FREE_GB", "8".into()),
            ("SELF_DRIVING_PROBE_DISK_FREE_GB", "50".into()),
            ("SELF_DRIVING_PROBE_ON_BATTERY", "0".into()),
            ("SELF_DRIVING_PROBE_CONTENTION", "0".into()),
        ];
        for (name, value) in &pins {
            // SAFETY: test-only process env, removed below.
            unsafe { std::env::set_var(name, value) };
        }

        let st = LoopState::open().expect("the pinned state dir opens");

        // A fresh state carries the hopeful seed: a cap of 2.
        assert_eq!(for_state(&st), 2, "a healthy box starts at the seed");

        // A raised calibration raises the answer. The number is learned.
        let mut cal = st.calibration();
        cal.parallel_ceiling = 5;
        st.write_calibration(&cal).expect("calibration writes");
        assert_eq!(for_state(&st), 5, "the saved cap is the answer");

        // The env cap outranks the calibration.
        // SAFETY: test-only process env, removed below.
        unsafe { std::env::set_var("SELF_DRIVING_PARALLEL_MAX", "3") };
        assert_eq!(for_state(&st), 3, "SELF_DRIVING_PARALLEL_MAX still wins");
        // SAFETY: removing what this test set.
        unsafe { std::env::remove_var("SELF_DRIVING_PARALLEL_MAX") };

        // A pinned busy box gets one worker, whatever was learned.
        // SAFETY: test-only process env, removed below.
        unsafe { std::env::set_var("SELF_DRIVING_PROBE_CONTENTION", "1") };
        assert_eq!(for_state(&st), 1, "a busy box gets one worker");

        for (name, _) in &pins {
            // SAFETY: removing what this test set.
            unsafe { std::env::remove_var(name) };
        }
    }
}
