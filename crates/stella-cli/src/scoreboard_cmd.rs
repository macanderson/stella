//! `stella scoreboard` — what the work cost, and whether anyone said it was good.
//!
//! Four numbers per unit of work: how many times intelligence was invoked, how
//! much a person had to type, how many times they came back, and what they
//! thought of the result. Three are counted from rows the store already writes.
//! The fourth is read from a pull request somebody merged or closed.
//!
//! No model verifiers anything here, which is the point. The store does carry a
//! rating field — `execution_reflection.self_rating` — and it is deliberately
//! not used: it is the model grading its own homework.
//!
//! The arithmetic lives in [`stella_core::scoreboard`] and is unit-tested there.
//! This module reads rows, maps a PR status to a verdict, and renders.

use colored::Colorize;

use stella_core::scoreboard::{Event, Scoreboard, Trend, Verdict, VerdictSource, summarize};
use stella_store::Store;
use stella_store::scoreboard::SessionRollup;

/// Map a pull-request status to a person's verdict.
///
/// `draft` and `open` are not verdicts. Work still in flight has not been
/// judged, and counting it as anything would be inventing an opinion nobody
/// expressed — which is the failure this whole surface exists to avoid.
fn verdict_of(status: &str) -> Option<(Verdict, VerdictSource)> {
    match status {
        "merged" => Some((Verdict::Good, VerdictSource::PrMerged)),
        "closed" => Some((Verdict::Bad, VerdictSource::PrAbandoned)),
        _ => None,
    }
}

/// Rebuild the event sequence a rollup summarises, so the shared, tested
/// arithmetic in `stella-core` produces the numbers rather than this module
/// re-deriving them.
///
/// Every execution begins with something a person typed, so N executions are N
/// human inputs and N model calls — which makes follow-ups `N - 1`. The prompt
/// characters are already summed by the query, so they ride on the first input;
/// the scoreboard totals them either way.
///
/// `interrupted` is never set: nothing in the store distinguishes a human
/// cancellation from a provider error today. See the note in
/// `stella_store::scoreboard`.
fn events_for(rollup: &SessionRollup) -> Vec<Event> {
    let mut events = Vec::new();
    for index in 0..rollup.executions {
        events.push(Event::HumanInput {
            chars: if index == 0 { rollup.prompt_chars } else { 0 },
            interrupted: false,
        });
        events.push(Event::ModelCall);
    }
    for _ in 0..rollup.tool_calls {
        events.push(Event::ToolCall);
    }
    events
}

fn board_for(rollup: &SessionRollup) -> Scoreboard {
    let board = Scoreboard::from_events(&events_for(rollup));
    match rollup.pr_status.as_deref().and_then(verdict_of) {
        Some((verdict, source)) => board.rated(verdict, source),
        None => board,
    }
}

fn verdict_cell(board: &Scoreboard) -> String {
    match board.verdict {
        Some((Verdict::Good, _)) => "good".green().to_string(),
        Some((Verdict::Mixed, _)) => "mixed".yellow().to_string(),
        Some((Verdict::Bad, _)) => "bad".red().to_string(),
        None => "unrated".dimmed().to_string(),
    }
}

fn render(rollups: &[SessionRollup], boards: &[Scoreboard], trend: &Trend) {
    println!("\n{}", "Delivery scoreboard".bold());
    println!(
        "{}",
        format!(
            "{} task{}, {} rated ({:.0}% coverage)",
            trend.tasks,
            if trend.tasks == 1 { "" } else { "s" },
            trend.tasks - trend.unrated,
            trend.coverage() * 100.0
        )
        .dimmed()
    );

    println!(
        "\n  {:<26} {:>6} {:>7} {:>10} {:>10}  {}",
        "SESSION".dimmed(),
        "CALLS".dimmed(),
        "TOOLS".dimmed(),
        "TYPED".dimmed(),
        "FOLLOW-UPS".dimmed(),
        "VERDICT".dimmed()
    );
    for (rollup, board) in rollups.iter().zip(boards) {
        let session: String = rollup.session_id.chars().take(26).collect();
        println!(
            "  {session:<26} {:>6} {:>7} {:>10} {:>10}  {}",
            board.model_calls,
            board.tool_calls,
            board.supplied_chars,
            board.follow_ups,
            verdict_cell(board)
        );
    }

    if trend.unrated == trend.tasks {
        println!(
            "\n{}",
            "Nothing is rated yet, so no averages are shown. Open a pull request from a \
             session and its merge decision becomes that session's verdict."
                .dimmed()
        );
    } else {
        println!("\n{}", "Averages, over rated tasks only".bold());
        println!(
            "  {:<22}{:.1}\n  {:<22}{:.0}\n  {:<22}{:.1}\n  {:<22}{} of {}",
            "calls",
            trend.mean_model_calls,
            "characters typed",
            trend.mean_supplied_chars,
            "follow-ups",
            trend.mean_follow_ups,
            "delivered first try",
            trend.one_shot,
            trend.tasks - trend.unrated
        );
    }

    // Stated, not implied. A number missing from a scoreboard is indistinguishable
    // from a number that is zero unless the scoreboard says which it is.
    println!(
        "\n{}",
        "Interrupts are not counted: the store records a human cancellation and a \
         provider error as the same outcome."
            .dimmed()
    );
}

/// Entry point for `stella scoreboard`.
pub fn run() -> Result<(), String> {
    let root =
        std::env::current_dir().map_err(|e| format!("cannot read working directory: {e}"))?;
    let store = Store::open(&root).map_err(|e| format!("cannot open the workspace store: {e}"))?;
    let rollups = store
        .session_rollups()
        .map_err(|e| format!("cannot read session rollups: {e}"))?;

    if rollups.is_empty() {
        println!("No finished sessions in this workspace yet.");
        return Ok(());
    }

    let boards: Vec<Scoreboard> = rollups.iter().map(board_for).collect();
    let trend = summarize(&boards);
    render(&rollups, &boards, &trend);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rollup(session: &str, executions: u32, chars: u64, pr: Option<&str>) -> SessionRollup {
        SessionRollup {
            session_id: session.to_string(),
            executions,
            tool_calls: 3,
            prompt_chars: chars,
            pr_status: pr.map(str::to_string),
        }
    }

    #[test]
    fn a_merged_pull_request_is_a_good_verdict_and_a_closed_one_is_bad() {
        assert_eq!(
            verdict_of("merged"),
            Some((Verdict::Good, VerdictSource::PrMerged))
        );
        assert_eq!(
            verdict_of("closed"),
            Some((Verdict::Bad, VerdictSource::PrAbandoned))
        );
    }

    /// Work still in flight has not been judged. Counting it would invent an
    /// opinion nobody expressed.
    #[test]
    fn an_open_or_draft_pull_request_is_not_a_verdict() {
        assert_eq!(verdict_of("open"), None);
        assert_eq!(verdict_of("draft"), None);
        assert_eq!(verdict_of("anything-else"), None);
    }

    #[test]
    fn a_session_with_no_pull_request_stays_unrated() {
        let board = board_for(&rollup("s", 2, 40, None));
        assert!(!board.is_rated());
        assert!(!board.was_one_shot(), "unrated work read as one-shot");
    }

    #[test]
    fn the_first_execution_is_not_a_follow_up() {
        assert_eq!(board_for(&rollup("s", 1, 10, None)).follow_ups, 0);
        assert_eq!(board_for(&rollup("s", 4, 10, None)).follow_ups, 3);
    }

    #[test]
    fn calls_tools_and_typed_characters_survive_the_round_trip() {
        let board = board_for(&rollup("s", 3, 250, Some("merged")));
        assert_eq!(board.model_calls, 3);
        assert_eq!(board.tool_calls, 3);
        assert_eq!(board.supplied_chars, 250);
        assert!(board.is_rated());
    }

    #[test]
    fn interrupts_are_never_synthesised() {
        let board = board_for(&rollup("s", 5, 99, Some("merged")));
        assert_eq!(
            board.interrupts, 0,
            "an interrupt was invented from data that cannot express one"
        );
    }

    #[test]
    fn coverage_reflects_only_sessions_with_a_decided_pull_request() {
        let boards: Vec<Scoreboard> = [
            rollup("a", 1, 10, Some("merged")),
            rollup("b", 1, 10, Some("open")),
            rollup("c", 1, 10, None),
            rollup("d", 1, 10, Some("closed")),
        ]
        .iter()
        .map(board_for)
        .collect();
        let trend = summarize(&boards);
        assert_eq!(trend.tasks, 4);
        assert_eq!(trend.unrated, 2, "open/absent PRs counted as rated");
        assert_eq!(trend.good, 1);
        assert_eq!(trend.bad, 1);
        assert_eq!(trend.coverage(), 0.5);
    }
}
