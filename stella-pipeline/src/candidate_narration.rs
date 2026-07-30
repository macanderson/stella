//! What a best-of-N fan-out says about itself while it runs.
//!
//! A fan-out used to be indistinguishable from one slow turn: the same
//! `Execute` stage repeated N times, with nothing naming the fan-out, saying
//! which candidate was running, whether each had its own workspace, or which
//! one was finally kept. So a steer or a verdict that landed in a losing
//! candidate read as the run ignoring the user.
//!
//! Both functions return `None` for `n <= 1`. `run_best_of_n` also serves a
//! single candidate when a witness is being authored, and "candidate 1/1" there
//! is noise rather than signal.

/// Announce a candidate as it starts, naming its position and whether it is
/// isolated — isolation is what decides whether a loser's edits survive, so it
/// belongs in the line the user actually reads.
pub(crate) fn candidate_start_notice(i: u32, n: u32, isolated: bool) -> Option<String> {
    if n <= 1 {
        return None;
    }
    let where_ = if isolated {
        "its own isolated workspace"
    } else {
        "the shared working tree"
    };
    Some(format!(
        "\n▸ best-of-{n}: candidate {}/{n} starting in {where_}.\n",
        i + 1
    ))
}

/// Announce which candidate was kept. `ran < n` is called out because a
/// fan-out that lost candidates to isolation failures spent less than the
/// configured N implies.
pub(crate) fn candidate_winner_notice(best_idx: usize, n: u32, ran: u32) -> Option<String> {
    if n <= 1 {
        return None;
    }
    let ran_note = if ran == n {
        String::new()
    } else {
        format!(" ({ran} of {n} actually ran)")
    };
    Some(format!(
        "\n▸ best-of-{n}: candidate {}/{n} won{ran_note}.\n",
        best_idx + 1
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fan_out_names_its_position_total_and_isolation() {
        let isolated = candidate_start_notice(0, 3, true).expect("n > 1 narrates");
        assert!(isolated.contains("best-of-3"), "{isolated}");
        assert!(isolated.contains("candidate 1/3"), "one-based: {isolated}");
        assert!(isolated.contains("isolated workspace"), "{isolated}");

        let shared = candidate_start_notice(2, 3, false).expect("n > 1 narrates");
        assert!(shared.contains("candidate 3/3"), "{shared}");
        assert!(
            shared.contains("shared working tree"),
            "a shared-tree fan-out must say so — a loser's edits stay on disk: {shared}"
        );
    }

    #[test]
    fn the_winner_is_named_and_a_short_fan_out_says_how_many_ran() {
        let all_ran = candidate_winner_notice(1, 4, 4).expect("n > 1 narrates");
        assert!(all_ran.contains("candidate 2/4 won"), "{all_ran}");
        assert!(
            !all_ran.contains("actually ran"),
            "no parenthetical when every candidate ran: {all_ran}"
        );

        let some_skipped = candidate_winner_notice(0, 4, 2).expect("n > 1 narrates");
        assert!(
            some_skipped.contains("2 of 4 actually ran"),
            "a fan-out that lost candidates must say so: {some_skipped}"
        );
    }

    /// The witness-authoring path runs `run_best_of_n` with one candidate.
    #[test]
    fn a_single_candidate_never_narrates() {
        assert!(candidate_start_notice(0, 1, true).is_none());
        assert!(candidate_winner_notice(0, 1, 1).is_none());
    }
}
