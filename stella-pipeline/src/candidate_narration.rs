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
//!
//! [`messages_rooted_at`] narrates the same fan-out to its *other* audience.
//! The user is told a candidate is isolated; until it was added, the candidate
//! itself never was.

use stella_protocol::CompletionMessage;

/// Announce a fan-out whose candidates run *together* (#1215), before any of
/// them starts.
///
/// Two facts the user cannot infer from the per-candidate lines that follow.
/// The first is that the wall clock is now the slowest candidate rather than
/// the sum, which is the whole reason to pay for N. The second is why their
/// terminal stops streaming mid-thought: while N models share one event
/// stream the live text preview is muted, and its absence would otherwise read
/// as a stalled run. A sequential fan-out (`width <= 1`) says neither, because
/// neither is true of it.
pub(crate) fn candidate_fanout_notice(n: u32, width: u32) -> Option<String> {
    if n <= 1 || width <= 1 {
        return None;
    }
    Some(format!(
        "\n▸ best-of-{n}: running {width} candidates at once — this turn costs the slowest \
         candidate, not the sum. Live text previews are paused while they share the stream; \
         each candidate's full answer still arrives.\n"
    ))
}

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

/// A candidate's own starting messages: the shared prefix, plus — when this
/// candidate runs in a tree of its own — one line naming that tree.
///
/// The model is otherwise never told. Its tools are re-rooted silently, so
/// `pwd` answers with a path the task statement never mentions, and a task that
/// spells absolute paths (a benchmark's `/app`, a monorepo checkout) reads as a
/// flat contradiction. Leaving it unsaid is not merely confusing: a candidate
/// that resolves the contradiction the wrong way `cd`s to the original tree and
/// works there, where its edits are outside the snapshot adoption diffs — so
/// they are dropped — while the edits it did make inside can leave the snapshot
/// no longer byte-identical to its seal, which fails adoption for the whole
/// candidate. Both halves of a split turn lose.
///
/// Appended AFTER the shared prefix, never inside it: the candidates of a
/// fan-out share one cached prefix, so the single message that differs between
/// them has to be last (the prompt-cache stability invariant).
pub(crate) fn messages_rooted_at(
    base: &[CompletionMessage],
    root: Option<&str>,
) -> Vec<CompletionMessage> {
    let mut messages = base.to_vec();
    if let Some(root) = root {
        messages.push(CompletionMessage::user(format!(
            "Workspace: your tools and shell are rooted at `{root}`, an isolated \
             snapshot of the project. Only work inside this root is collected when \
             the turn finishes. Absolute paths in the task above name the original \
             tree; resolve them against this root instead (`/x/y` -> `{root}/x/y`). \
             Another copy of the project may be readable elsewhere on this machine — \
             editing it does not count, so do not `cd` out of this root."
        )));
    }
    messages
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

    /// The fact the candidate could not otherwise get: which tree is real.
    /// It must also say that the *other* copy is not collected — a candidate
    /// that knows only its own path still has no reason to distrust the
    /// absolute paths the task statement gave it.
    #[test]
    fn an_isolated_candidate_is_told_its_root_and_that_only_it_counts() {
        let base = vec![
            CompletionMessage::system("prefix"),
            CompletionMessage::user("write vm.js to /app"),
        ];
        let rooted = messages_rooted_at(&base, Some("/tmp/stella_candidate_64_0"));

        assert_eq!(
            rooted.len(),
            base.len() + 1,
            "exactly one message is added: {rooted:?}"
        );
        // Byte-identical prefix, appended last: N candidates differ only in the
        // tail, so the cached prefix is still shared across the fan-out.
        assert_eq!(&rooted[..base.len()], &base[..], "prefix must not move");

        let notice = &rooted[base.len()].content;
        assert!(notice.contains("/tmp/stella_candidate_64_0"), "{notice}");
        assert!(
            notice.contains("do not `cd` out of this root"),
            "the escape hatch that splits a turn across two trees: {notice}"
        );
        assert!(
            notice.contains("Only work inside this root is collected"),
            "naming the root is not enough — say the other copy does not count: {notice}"
        );
    }

    /// A shared-tree candidate has no second tree to confuse, and the session
    /// root is already the one the task text names. Saying anything here would
    /// be a per-candidate message with nothing to report, paid for on a turn
    /// that has no ambiguity to resolve.
    #[test]
    fn a_shared_tree_candidate_is_told_nothing() {
        let base = vec![CompletionMessage::user("goal")];
        assert_eq!(messages_rooted_at(&base, None), base);
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
        assert!(candidate_fanout_notice(1, 1).is_none());
    }

    #[test]
    fn a_concurrent_fan_out_names_its_width_and_the_muted_preview() {
        let notice = candidate_fanout_notice(3, 3).expect("a wide fan-out narrates");
        assert!(notice.contains("best-of-3"), "{notice}");
        assert!(notice.contains("3 candidates at once"), "{notice}");
        assert!(
            notice.contains("not the sum"),
            "the wall-clock claim is the reason to pay for N: {notice}"
        );
        assert!(
            notice.contains("previews are paused"),
            "a silent stream must be explained, not discovered: {notice}"
        );
    }

    /// A fan-out that runs one candidate at a time streams exactly as it
    /// always did, so it must not claim otherwise.
    #[test]
    fn a_sequential_fan_out_promises_no_concurrency() {
        assert!(candidate_fanout_notice(3, 1).is_none());
    }
}
