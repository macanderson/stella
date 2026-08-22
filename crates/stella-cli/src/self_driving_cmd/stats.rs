//! `stats` — what a session has done so far, rendered for a person.
//!
//! The counters themselves are [`stella_autonomy::SessionStats`], shared with
//! the Observatory so the dashboard and the terminal cannot disagree. This
//! module is only the rendering.
//!
//! Split out of `self_driving_cmd.rs` rather than added to it: that file is
//! within a couple of hundred lines of the 1500-line ceiling `make file-size`
//! enforces, and it carries no entry in `scripts/file-size-baseline.txt` — so
//! a crossing fails the gate outright rather than being grandfathered
//! (AGENTS.md § *God files* — plan around them, never into them).

use crate::query_format::QueryFormat;

use super::state::LoopState;

/// `stella self-driving stats` — the session's counters.
///
/// Renders the ratios beside the counts, deliberately. A raw count answers
/// "how busy was it", which is the question that flatters; the ratios answer
/// "was it worth it", and each can report badly. A dashboard showing only
/// counts would make a loop that created twenty issues and closed three look
/// like a productive week.
pub(super) fn session_stats(st: &LoopState, format: QueryFormat) -> Result<(), String> {
    let stats = st.stats();

    if format == QueryFormat::Json {
        println!(
            "{}",
            serde_json::to_string(&stats).map_err(|e| e.to_string())?
        );
        return Ok(());
    }

    // `--` rather than `0.00` for a ratio with no denominator: a ratio against
    // zero is undefined, not small, and printing a number would tell a reader
    // something the session has not yet established.
    let ratio = |value: Option<f64>| value.map_or_else(|| "  --".to_owned(), |v| format!("{v:.2}"));

    println!("backlog");
    println!("  claimed         {:>5}", stats.issues_claimed);
    println!("  attempted       {:>5}", stats.issues_attempted);
    println!("  changed         {:>5}", stats.issues_changed);
    println!("  no change       {:>5}", stats.issues_no_change);
    println!("  failed          {:>5}", stats.issues_failed);
    println!("  escalated       {:>5}", stats.issues_escalated);
    println!("  deferred        {:>5}", stats.issues_deferred);

    println!("\nfiled");
    println!("  attempted       {:>5}", stats.filings_attempted);
    println!("  created         {:>5}", stats.issues_created);
    println!("  refused         {:>5}", stats.filings_refused);
    println!("  duplicate       {:>5}", stats.filings_duplicate);
    if !stats.filings_balance() {
        eprintln!(
            "  warning: the three outcomes do not sum to the attempts — a filing \
             outcome this build does not recognise was recorded"
        );
    }

    println!("\nclosed");
    println!("  total           {:>5}", stats.closed_total);
    println!("    completed     {:>5}", stats.closed_completed);
    println!("    not planned   {:>5}", stats.closed_not_planned);
    println!("    duplicate     {:>5}", stats.closed_duplicate);
    if !stats.closures_balance() {
        eprintln!(
            "  warning: the three kinds do not sum to the total — a resolution \
             this build does not recognise was recorded"
        );
    }

    println!("\ndelivery");
    println!("  prs opened      {:>5}", stats.prs_opened);
    println!("  prs merged      {:>5}", stats.prs_merged);
    println!("  prs escalated   {:>5}", stats.prs_escalated);
    println!("  fixes pushed    {:>5}", stats.fixes_pushed);
    println!("  rebases         {:>5}", stats.rebases);
    println!("  base-broken     {:>5}", stats.base_broken_waits);

    println!("\nlearning");
    println!("  reflections     {:>5}", stats.reflections_logged);
    println!("  memories        {:>5}", stats.memories_created);
    println!("  proposals       {:>5}", stats.proposals_made);

    println!("\ncost");
    println!("  turns run       {:>5}", stats.turns_run);
    println!("  over budget     {:>5}", stats.turns_over_budget);

    println!("\nyield");
    println!(
        "  inflation       {:>5}   created per closed; above 1.00 is losing ground",
        ratio(stats.inflation_ratio())
    );
    println!(
        "  attempt yield   {:>5}   attempts that changed something",
        ratio(stats.attempt_yield())
    );
    println!(
        "  merge rate      {:>5}   opened prs that landed",
        ratio(stats.merge_rate())
    );

    Ok(())
}
