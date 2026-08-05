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
/// It also names what the snapshot does NOT carry. A git-shaped snapshot
/// elides every ignored path, so a candidate can be handed a tree whose
/// `node_modules/` or `.venv/` is simply gone — and the only symptom is the
/// project's own test command failing for a reason the tree does not explain.
/// A candidate that knows can reinstall; a candidate that doesn't reports the
/// project as broken.
///
/// # Why it must promise adoption, not just forbid the escape
///
/// Naming the root and forbidding `cd` was the whole of this notice, and it was
/// not enough — because it described the isolation without ever describing the
/// hand-off. A candidate reading only "work outside this root does not count"
/// learns that its answer is trapped somewhere the user will never see it, and
/// the *helpful* response to that belief is to deliver the answer by hand: `cp`
/// the finished file to the absolute path the task named. That is not a model
/// ignoring the instruction, it is a model obeying the goal under a
/// true-sounding premise nobody corrected. It is also the one move that
/// guarantees the loss it was trying to prevent — the out-of-band write lands
/// in the real tree, so the winner's patch can no longer create a file that is
/// already there and adoption fails for every path at once.
///
/// So the notice states the mechanism: the winner's diff is applied to the real
/// project automatically. With that said, staying inside the root is no longer
/// a rule the model must trust against its own reading of the task — it is the
/// path that visibly delivers the work, and copying out is visibly redundant.
///
/// Appended AFTER the shared prefix, never inside it: the candidates of a
/// fan-out share one cached prefix, so the single message that differs between
/// them has to be last (the prompt-cache stability invariant).
pub(crate) fn messages_rooted_at(
    base: &[CompletionMessage],
    workspace: Option<&dyn crate::ports::CandidateWorkspace>,
) -> Vec<CompletionMessage> {
    let mut messages = base.to_vec();
    if let Some(ws) = workspace {
        let root = ws.root();
        let mut notice = format!(
            "Workspace: your tools and shell are rooted at `{root}`, an isolated \
             snapshot of the project. Absolute paths in the task above name the \
             original tree; resolve them against this root instead \
             (`/x/y` -> `{root}/x/y`).\n\
             Your work is collected for you: if this turn is chosen, everything you \
             changed inside this root is applied to the real project automatically, \
             at the paths the task names. You never need to copy, move, or install \
             your result anywhere else to make it count.\n\
             So write only inside this root. Another copy of the project may be \
             readable elsewhere on this machine — do not `cd` out of this root and \
             do not write to it. Work placed there is not collected, and it also \
             breaks the automatic hand-off above, which discards this turn entirely."
        );
        let omitted = ws.omitted_paths();
        if !omitted.is_empty() {
            notice.push_str(&format!(
                "\n\nThis snapshot carries tracked and untracked files but NOT ignored \
                 ones, so these exist in the original tree and are absent here: {}. \
                 If a build or test fails on something they would have provided, \
                 regenerate it inside this root (reinstall, rebuild) rather than \
                 reaching outside for it.",
                omitted.join(", ")
            ));
        }
        messages.push(CompletionMessage::user(notice));
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{
        AdoptedChange, CandidateWorkspace, DiagnosticRunner, RepoStatusPort, TestRunner,
        WorkspaceError,
    };

    /// A workspace that answers only what the notice actually reads. The other
    /// nine port methods are unreachable from this code path, and stubbing
    /// them with plausible values would invite a future caller to depend on
    /// something this fake never modelled.
    struct NoticeOnlyWorkspace {
        root: String,
        omitted: Vec<String>,
    }

    impl NoticeOnlyWorkspace {
        fn at(root: &str) -> Self {
            Self {
                root: root.into(),
                omitted: Vec::new(),
            }
        }

        fn missing(mut self, paths: &[&str]) -> Self {
            self.omitted = paths.iter().map(|p| (*p).to_string()).collect();
            self
        }
    }

    #[async_trait::async_trait]
    impl CandidateWorkspace for NoticeOnlyWorkspace {
        fn root(&self) -> &str {
            &self.root
        }
        fn omitted_paths(&self) -> &[String] {
            &self.omitted
        }
        fn tools(&self) -> &dyn stella_core::ToolExecutor {
            unimplemented!("the workspace notice dispatches no tool")
        }
        fn witness_tools(&self) -> &dyn stella_core::ToolExecutor {
            unimplemented!("the workspace notice dispatches no tool")
        }
        fn diagnostics(&self) -> &dyn DiagnosticRunner {
            unimplemented!("the workspace notice runs no diagnostics")
        }
        fn tests(&self) -> &dyn TestRunner {
            unimplemented!("the workspace notice runs no tests")
        }
        fn repo_status(&self) -> &dyn RepoStatusPort {
            unimplemented!("the workspace notice reads no repo status")
        }
        async fn seal(&self) -> Result<(), WorkspaceError> {
            unimplemented!("the workspace notice seals nothing")
        }
        async fn sealed_is_unchanged(&self) -> Result<bool, WorkspaceError> {
            unimplemented!("the workspace notice seals nothing")
        }
        async fn adopt(&self, _withhold: &[String]) -> Result<Vec<AdoptedChange>, WorkspaceError> {
            unimplemented!("the workspace notice adopts nothing")
        }
        async fn graft_witness(&self, _source: &str, _path: &str) -> Result<(), WorkspaceError> {
            unimplemented!("the workspace notice grafts nothing")
        }
        async fn remove(&self) {
            unimplemented!("the workspace notice removes nothing")
        }
    }

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
        let ws = NoticeOnlyWorkspace::at("/tmp/stella_candidate_64_0");
        let rooted = messages_rooted_at(&base, Some(&ws));

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
            notice.contains("not collected"),
            "naming the root is not enough — say the other copy does not count: {notice}"
        );
    }

    /// The half that removes the *motive* for the escape rather than
    /// forbidding it again.
    ///
    /// A candidate told only that outside work "does not count" concludes its
    /// answer is stranded, and the cooperative move is then to deliver it by
    /// hand to the absolute path the task named — which is exactly the
    /// out-of-band write that makes the winner's patch unappliable. The notice
    /// has to state that adoption happens automatically, or the prohibition is
    /// asking the model to leave the task undelivered.
    #[test]
    fn an_isolated_candidate_is_told_its_work_is_adopted_for_it() {
        let ws = NoticeOnlyWorkspace::at("/tmp/stella_candidate_7_0");
        let rooted = messages_rooted_at(&[CompletionMessage::user("write vm.js to /app")], Some(&ws));
        let notice = &rooted[1].content;

        assert!(
            notice.contains("applied to the real project automatically"),
            "the candidate must learn its work is adopted, not stranded: {notice}"
        );
        assert!(
            notice.contains("never need to copy"),
            "name the specific move the old wording invited — the `cp` out: {notice}"
        );
        assert!(
            notice.contains("breaks the automatic hand-off"),
            "writing outside is not merely uncounted, it fails adoption for the \
             whole candidate — say the real cost: {notice}"
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

    /// The failure this prevents: `npm test` dies on a missing module, in a
    /// tree that looks complete, for a reason the candidate cannot observe.
    /// Naming the elision converts an inexplicable failure into a fixable one.
    #[test]
    fn a_candidate_is_told_what_the_snapshot_left_behind() {
        let base = vec![CompletionMessage::user("run the tests")];
        let ws = NoticeOnlyWorkspace::at("/tmp/stella_candidate_7_0")
            .missing(&["node_modules/", ".venv/"]);

        let notice = messages_rooted_at(&base, Some(&ws))
            .pop()
            .expect("a notice is appended")
            .content;

        assert!(notice.contains("node_modules/"), "{notice}");
        assert!(notice.contains(".venv/"), "{notice}");
        assert!(
            notice.contains("regenerate it inside this root"),
            "naming the gap without naming the remedy leaves the candidate stuck: {notice}"
        );
    }

    /// A tree that ignores nothing must not be handed a paragraph about
    /// nothing — the notice is paid for on every candidate of every fan-out.
    #[test]
    fn a_snapshot_that_elides_nothing_says_nothing_about_omissions() {
        let base = vec![CompletionMessage::user("goal")];
        let ws = NoticeOnlyWorkspace::at("/tmp/stella_candidate_7_1");

        let notice = messages_rooted_at(&base, Some(&ws))
            .pop()
            .expect("a notice is appended")
            .content;

        assert!(notice.contains("/tmp/stella_candidate_7_1"), "{notice}");
        assert!(!notice.contains("absent here"), "{notice}");
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
