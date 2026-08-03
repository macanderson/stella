use super::*;

fn human(chars: u64) -> Event {
    Event::HumanInput {
        chars,
        interrupted: false,
    }
}

fn interrupt(chars: u64) -> Event {
    Event::HumanInput {
        chars,
        interrupted: true,
    }
}

fn good(board: Scoreboard) -> Scoreboard {
    board.rated(Verdict::Good, VerdictSource::Human)
}

// ── folding events ──────────────────────────────────────────────────────────

#[test]
fn the_opening_request_is_not_a_follow_up() {
    let board = Scoreboard::from_events(&[human(50), Event::ModelCall]);
    assert_eq!(
        board.follow_ups, 0,
        "opening request counted as a follow-up"
    );
    assert_eq!(board.supplied_chars, 50);
}

#[test]
fn every_human_input_after_the_first_is_a_follow_up() {
    let board = Scoreboard::from_events(&[human(50), Event::ModelCall, human(10), human(5)]);
    assert_eq!(board.follow_ups, 2);
    assert_eq!(board.supplied_chars, 65, "all human chars counted");
}

#[test]
fn interrupts_are_a_subset_of_follow_ups() {
    let board = Scoreboard::from_events(&[human(50), interrupt(10), human(10)]);
    assert_eq!(board.follow_ups, 2);
    assert_eq!(board.interrupts, 1);
    assert!(board.interrupts <= board.follow_ups);
}

/// An opening request that is itself an interrupt still counts as an interrupt
/// — it means the previous work was stopped to start this — but must not count
/// as a follow-up of a task it began.
#[test]
fn an_opening_interrupt_is_not_a_follow_up() {
    let board = Scoreboard::from_events(&[interrupt(50), Event::ModelCall]);
    assert_eq!(board.follow_ups, 0);
    assert_eq!(board.interrupts, 1);
}

#[test]
fn calls_and_tool_calls_are_counted_separately() {
    let board = Scoreboard::from_events(&[
        human(1),
        Event::ModelCall,
        Event::ToolCall,
        Event::ToolCall,
        Event::ModelCall,
    ]);
    assert_eq!(board.model_calls, 2);
    assert_eq!(board.tool_calls, 2);
}

#[test]
fn an_unprompted_run_scores_zero_rather_than_failing() {
    let board = Scoreboard::from_events(&[Event::ModelCall, Event::ToolCall]);
    assert_eq!(board.supplied_chars, 0);
    assert_eq!(board.follow_ups, 0);
    assert_eq!(board.model_calls, 1);
}

#[test]
fn an_empty_sequence_is_all_zeroes() {
    assert_eq!(Scoreboard::from_events(&[]), Scoreboard::default());
}

// ── verdicts ────────────────────────────────────────────────────────────────

#[test]
fn a_fresh_board_is_unrated() {
    let board = Scoreboard::from_events(&[human(10)]);
    assert!(!board.is_rated());
    assert!(
        !board.was_one_shot(),
        "unrated work must not read as one-shot"
    );
}

#[test]
fn one_shot_requires_no_follow_ups_and_a_good_verdict() {
    let clean = good(Scoreboard::from_events(&[human(10), Event::ModelCall]));
    assert!(clean.was_one_shot());

    let corrected = good(Scoreboard::from_events(&[human(10), human(5)]));
    assert!(
        !corrected.was_one_shot(),
        "follow-up still counted as one-shot"
    );

    let bad = Scoreboard::from_events(&[human(10)]).rated(Verdict::Bad, VerdictSource::Human);
    assert!(!bad.was_one_shot(), "bad verdict counted as one-shot");

    let mixed = Scoreboard::from_events(&[human(10)]).rated(Verdict::Mixed, VerdictSource::Human);
    assert!(!mixed.was_one_shot(), "mixed verdict counted as one-shot");
}

#[test]
fn the_verdict_source_is_retained() {
    let merged =
        Scoreboard::from_events(&[human(10)]).rated(Verdict::Good, VerdictSource::PrMerged);
    assert_eq!(
        merged.verdict,
        Some((Verdict::Good, VerdictSource::PrMerged))
    );
}

// ── trends, and the coverage rule ───────────────────────────────────────────

#[test]
fn unrated_tasks_are_counted_but_never_averaged() {
    let boards = vec![
        good(Scoreboard::from_events(&[human(10), Event::ModelCall])),
        Scoreboard::from_events(&[
            human(999),
            Event::ModelCall,
            Event::ModelCall,
            Event::ModelCall,
        ]),
    ];
    let trend = summarize(&boards);

    assert_eq!(trend.tasks, 2);
    assert_eq!(trend.unrated, 1);
    // The unrated task had 3 calls and 999 chars. If it leaked into the means,
    // these would be 2.0 and 504.5.
    assert_eq!(trend.mean_model_calls, 1.0, "unrated task leaked into mean");
    assert_eq!(trend.mean_supplied_chars, 10.0, "unrated chars leaked in");
}

/// The whole point of the field. An unrated run must never look like a
/// successful one.
#[test]
fn unrated_work_is_not_counted_as_good() {
    let boards = vec![
        Scoreboard::from_events(&[human(10)]),
        Scoreboard::from_events(&[human(10)]),
    ];
    let trend = summarize(&boards);
    assert_eq!(trend.good, 0);
    assert_eq!(trend.bad, 0);
    assert_eq!(trend.one_shot, 0);
    assert_eq!(trend.unrated, 2);
    assert_eq!(trend.coverage(), 0.0);
}

#[test]
fn coverage_reports_the_rated_share() {
    let boards = vec![
        good(Scoreboard::from_events(&[human(1)])),
        good(Scoreboard::from_events(&[human(1)])),
        Scoreboard::from_events(&[human(1)]),
        Scoreboard::from_events(&[human(1)]),
    ];
    assert_eq!(summarize(&boards).coverage(), 0.5);
}

#[test]
fn coverage_of_nothing_is_zero_not_a_division_by_zero() {
    let trend = summarize(&[]);
    assert_eq!(trend.tasks, 0);
    assert_eq!(trend.coverage(), 0.0);
    assert!(
        trend.mean_model_calls.is_finite(),
        "mean is NaN on empty input"
    );
}

#[test]
fn good_and_bad_are_tallied_separately() {
    let boards = vec![
        good(Scoreboard::from_events(&[human(1)])),
        Scoreboard::from_events(&[human(1)]).rated(Verdict::Bad, VerdictSource::PrAbandoned),
        Scoreboard::from_events(&[human(1)]).rated(Verdict::Mixed, VerdictSource::Human),
    ];
    let trend = summarize(&boards);
    assert_eq!(trend.good, 1);
    assert_eq!(trend.bad, 1);
    // Mixed is neither. It must not be quietly folded into good.
    assert_eq!(trend.good + trend.bad, 2);
    assert_eq!(trend.unrated, 0);
}

#[test]
fn follow_ups_and_interrupts_average_over_rated_tasks() {
    let boards = vec![
        good(Scoreboard::from_events(&[human(1), human(1), interrupt(1)])),
        good(Scoreboard::from_events(&[human(1)])),
    ];
    let trend = summarize(&boards);
    assert_eq!(trend.mean_follow_ups, 1.0);
    assert_eq!(trend.mean_interrupts, 0.5);
}

#[test]
fn summarize_is_deterministic() {
    let boards = vec![
        good(Scoreboard::from_events(&[human(3), Event::ModelCall])),
        Scoreboard::from_events(&[human(7)]),
    ];
    assert_eq!(summarize(&boards), summarize(&boards));
}

#[test]
fn an_interrupted_opening_request_is_not_an_interrupt() {
    // Nothing is in flight when the opening request arrives, so an
    // `interrupted: true` opening must not count — before the guard,
    // `interrupts` could exceed `follow_ups` (its documented superset) and a
    // task scored one-shot AND interrupted at once.
    let board = Scoreboard::from_events(&[interrupt(20), Event::ModelCall]);
    assert_eq!(board.follow_ups, 0);
    assert_eq!(
        board.interrupts, 0,
        "the opening request counted as an interrupt"
    );
    assert!(good(board).was_one_shot());

    // A genuine mid-flight interruption still counts.
    let board = Scoreboard::from_events(&[human(20), Event::ModelCall, interrupt(5)]);
    assert_eq!(board.follow_ups, 1);
    assert_eq!(board.interrupts, 1);
}
