//! What one unit of delivered work cost — and whether it was any good.
//!
//! Correctness eventually bottoms out in model capability, which no amount of
//! engineering controls. What *is* controllable, and therefore what is worth
//! measuring, is the shape of the interaction around it:
//!
//! 1. how many times intelligence was invoked,
//! 2. how much information a person had to supply,
//! 3. how many times a person had to correct the trajectory,
//! 4. what that person thought of the finished result.
//!
//! Those four are the scoreboard. Every other mechanism in the system —
//! context records, memories, skills, guards — earns its place by moving them,
//! and is dead weight otherwise.
//!
//! # Nothing here is judged by a model
//!
//! Three of the four are counted from events that already happen. The fourth
//! belongs to a person and is one keystroke, or free when the unit of work is a
//! pull request that either merged or did not.
//!
//! This matters because the alternative is already in the tree and does not
//! work: `execution_reflection.self_rating` is the model grading its own
//! homework, which measures how plausible the work looked to its author.
//!
//! # Unrated is not good
//!
//! [`Verdict`] is deliberately optional and [`Trend`] reports how many tasks
//! were never rated. A scoreboard that silently treats unrated work as
//! successful is a scoreboard that improves fastest when nobody is looking.
//!
//! # Purity
//!
//! Like the rest of `stella-core`, this module performs no I/O. Callers fold
//! their own event stream into [`Event`]s; the arithmetic is a deterministic
//! function of that sequence and is unit-testable without a store.

#[cfg(test)]
mod tests;

/// One thing that happened during a unit of work.
///
/// Deliberately coarse. Anything requiring interpretation to emit — "was that
/// message a correction or a clarification?" — is a classification problem, and
/// a classifier here would put a model back in the measurement path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A person typed something. `chars` counts only what the person wrote,
    /// never what was quoted or generated back to them.
    ///
    /// `interrupted` is `true` when the input arrived while the agent was
    /// mid-flight. That is the one correction signal needing no interpretation:
    /// a person does not stop a working agent to agree with it.
    HumanInput { chars: u64, interrupted: bool },
    /// One invocation of the model.
    ModelCall,
    /// One tool call, of any surface.
    ToolCall,
}

/// A person's opinion of the finished work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Good,
    Mixed,
    Bad,
}

/// How a [`Verdict`] was arrived at.
///
/// Recorded for the same reason `ContextUseFeedback::evaluation_method` is
/// mandatory on a negative judgement: a verdict whose provenance is unknown
/// cannot be argued with, and an unrated task must be distinguishable from a
/// task somebody actually looked at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerdictSource {
    /// The pull request this work produced was merged. Free, deterministic,
    /// and an imperfect but genuine proxy for "this was acceptable".
    PrMerged,
    /// The pull request was closed without merging.
    PrAbandoned,
    /// A person said so directly.
    Human,
}

/// The four numbers, for one unit of work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Scoreboard {
    /// How many times intelligence was invoked.
    pub model_calls: u32,
    /// Tool calls made across those invocations.
    pub tool_calls: u32,
    /// Characters a person had to type, including the opening request.
    pub supplied_chars: u64,
    /// Human inputs after the opening request.
    ///
    /// Not called `corrections`: some follow-ups supply information that was
    /// never asked for and some redirect the work, and telling those apart
    /// needs a judgement this module refuses to make. It is still the right
    /// number to watch — a task that took one message is better than one that
    /// took nine, whatever the nine were for.
    pub follow_ups: u32,
    /// Follow-ups that stopped the agent mid-flight. The unambiguous subset.
    pub interrupts: u32,
    /// What a person thought, and how that was established.
    pub verdict: Option<(Verdict, VerdictSource)>,
}

impl Scoreboard {
    /// Fold an event sequence into a scoreboard.
    ///
    /// The opening request is whichever [`Event::HumanInput`] comes first;
    /// everything after it counts as a follow-up. An empty sequence, or one
    /// with no human input at all, yields zeroes rather than an error — an
    /// autonomous run nobody prompted is a legitimate thing to score.
    pub fn from_events(events: &[Event]) -> Self {
        let mut board = Self::default();
        let mut seen_opening = false;
        for event in events {
            match *event {
                Event::HumanInput { chars, interrupted } => {
                    board.supplied_chars += chars;
                    if seen_opening {
                        board.follow_ups += 1;
                    }
                    seen_opening = true;
                    if interrupted {
                        board.interrupts += 1;
                    }
                }
                Event::ModelCall => board.model_calls += 1,
                Event::ToolCall => board.tool_calls += 1,
            }
        }
        board
    }

    /// Attach a person's opinion.
    pub fn rated(mut self, verdict: Verdict, source: VerdictSource) -> Self {
        self.verdict = Some((verdict, source));
        self
    }

    /// `true` when somebody actually formed an opinion about this work.
    pub fn is_rated(&self) -> bool {
        self.verdict.is_some()
    }

    /// `true` when the work landed with no follow-up of any kind — one request
    /// in, an acceptable result out.
    ///
    /// This is the number the whole system exists to increase.
    pub fn was_one_shot(&self) -> bool {
        self.follow_ups == 0 && matches!(self.verdict, Some((Verdict::Good, _)))
    }
}

/// How a run of tasks is trending.
///
/// Averages are over rated tasks only, except `tasks` and `unrated`, so that
/// coverage is always visible next to the numbers it qualifies.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Trend {
    /// Every task considered.
    pub tasks: u32,
    /// How many nobody rated. Reported, never silently dropped.
    pub unrated: u32,
    /// Rated tasks whose verdict was [`Verdict::Good`].
    pub good: u32,
    /// Rated tasks whose verdict was [`Verdict::Bad`].
    pub bad: u32,
    /// Rated tasks delivered on the first request with a good verdict.
    pub one_shot: u32,
    /// Mean model calls per rated task.
    pub mean_model_calls: f64,
    /// Mean characters a person supplied per rated task.
    pub mean_supplied_chars: f64,
    /// Mean follow-ups per rated task.
    pub mean_follow_ups: f64,
    /// Mean interrupts per rated task.
    pub mean_interrupts: f64,
}

impl Trend {
    /// What share of tasks were rated at all, `0.0..=1.0`.
    ///
    /// Read this before reading anything else. At low coverage the averages
    /// describe whichever tasks somebody happened to look at, which is rarely
    /// a fair sample of the ones that went badly.
    pub fn coverage(&self) -> f64 {
        if self.tasks == 0 {
            return 0.0;
        }
        f64::from(self.tasks - self.unrated) / f64::from(self.tasks)
    }
}

/// Summarise a run of tasks.
///
/// Unrated tasks are counted in `tasks` and `unrated` and excluded from every
/// average. Including them would require inventing a verdict for work nobody
/// judged, and the convenient invention is always "fine".
pub fn summarize(boards: &[Scoreboard]) -> Trend {
    let mut trend = Trend {
        tasks: u32::try_from(boards.len()).unwrap_or(u32::MAX),
        ..Trend::default()
    };
    let rated: Vec<&Scoreboard> = boards.iter().filter(|b| b.is_rated()).collect();
    trend.unrated = trend.tasks - u32::try_from(rated.len()).unwrap_or(u32::MAX);
    if rated.is_empty() {
        return trend;
    }

    let n = rated.len() as f64;
    for board in &rated {
        match board.verdict {
            Some((Verdict::Good, _)) => trend.good += 1,
            Some((Verdict::Bad, _)) => trend.bad += 1,
            _ => {}
        }
        if board.was_one_shot() {
            trend.one_shot += 1;
        }
        trend.mean_model_calls += f64::from(board.model_calls);
        trend.mean_supplied_chars += board.supplied_chars as f64;
        trend.mean_follow_ups += f64::from(board.follow_ups);
        trend.mean_interrupts += f64::from(board.interrupts);
    }
    trend.mean_model_calls /= n;
    trend.mean_supplied_chars /= n;
    trend.mean_follow_ups /= n;
    trend.mean_interrupts /= n;
    trend
}
