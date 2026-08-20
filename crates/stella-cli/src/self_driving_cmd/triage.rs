//! `triage` — place an issue nobody has judged, using the project's own
//! steering documents as the standard.
//!
//! `doc:backlog-self-driving` §3.2 (#3599). [`stella_autonomy::priority`] can
//! say *that* an issue is unjudged; only a reader can say *what* it should be.
//! This is that reader.
//!
//! # Why this is a turn and not a rule
//!
//! Every other decision in this loop is arithmetic over observed facts, and
//! deliberately so — deciding buys no model call. Placing an unlabelled issue
//! is the exception, and it is worth being precise about why rather than
//! treating it as a convenient escape.
//!
//! The inputs are a title and a body written by a human in prose, and the
//! standard they are measured against is this repository's context records —
//! also prose. There is no arithmetic over those two things. A regular
//! expression on the title would be a rule, but it would be a rule nobody
//! declared and nobody could change by publishing a record, which is the whole
//! point of having records.
//!
//! So the turn runs **with the project's steering already loaded** — that is
//! what `refuse_if_unsteered` guarantees — and the prompt does not restate the
//! policy. It names the vocabulary it must answer in and tells the turn to
//! consult the records. Changing how issues get prioritized is then a matter of
//! retiring a record and publishing another, with no code change at all.
//!
//! # It answers in the operator's vocabulary or it does not answer
//!
//! [`parse`] accepts only labels the [`TriagePolicy`] declared. A turn that
//! invents `urgent` when the ladder says `P0` has not answered the question,
//! and writing that label would put a word into the tracker that no ranker can
//! read — the issue would come straight back as unassessed on the next cycle
//! and the loop would pay for the same turn forever.
//!
//! An unparseable answer is therefore a refusal, and the caller escalates. That
//! costs one issue a human glance. Guessing costs an unbounded number of turns.

use stella_autonomy::priority::{TriagePolicy, Unassessed};
use stella_protocol::issue::{IssueKey, IssueProvider};

/// Where a turn decided an issue belongs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Assessment {
    /// Work for this loop: a defect kind and a rung.
    Place {
        /// The kind label to add, from `defect_kinds`.
        kind: String,
        /// The rung label to add, from the ladder.
        priority: String,
    },
    /// Not this loop's work: a kind from `excluded_kinds`.
    Exclude {
        /// The kind label to add.
        kind: String,
    },
}

impl Assessment {
    /// The labels this assessment adds to the issue.
    pub(super) fn labels(&self) -> Vec<String> {
        match self {
            Self::Place { kind, priority } => vec![kind.clone(), priority.clone()],
            Self::Exclude { kind } => vec![kind.clone()],
        }
    }

    /// A one-line account for the audit log and the issue comment.
    pub(super) fn reason(&self) -> String {
        match self {
            Self::Place { kind, priority } => {
                format!("placed as `{kind}` at `{priority}`")
            }
            Self::Exclude { kind } => format!("placed as `{kind}` — not this loop's work"),
        }
    }
}

/// The line a triage turn must end with.
const MARKER: &str = "ASSESSMENT:";

/// Compose the prompt that asks a turn to place one issue.
///
/// Deliberately short. The standard lives in the context records the turn has
/// already loaded, and restating policy here would create a second copy that
/// drifts from the records the moment anybody edits one — the exact failure
/// this loop is supposed to make impossible.
#[must_use]
pub(super) fn prompt(issue: &Unassessed, body: &str, policy: &TriagePolicy) -> String {
    let rungs = policy.ladder.rungs.join(", ");
    let defects = policy.defect_kinds.join(", ");
    let excluded = policy.excluded_kinds.join(", ");

    format!(
        "Triage one issue for this repository. Do not change any code.\n\n\
         Issue #{key}: {title}\n\n\
         --- body ---\n{body}\n--- end body ---\n\n\
         Decide two things, judged against this project's context records and \
         steering documents — they are the standard, not your own preferences:\n\n\
         1. Is this a defect this repository's self-driving loop should work, \
         or is it something else?\n\
         2. If it is work, how urgent is it?\n\n\
         Answer on a single final line, in exactly one of these two forms, using \
         only the words listed:\n\n\
         {MARKER} kind=<one of: {defects}>; priority=<one of: {rungs}>\n\
         {MARKER} exclude=<one of: {excluded}>\n\n\
         Any other vocabulary is not an answer. If the issue is too poorly \
         described to place, say so in prose and emit no {MARKER} line.",
        key = issue.key,
        title = issue.title,
    )
}

/// Read a turn's answer, accepting only the declared vocabulary.
///
/// The last `ASSESSMENT:` line wins, so a turn that thinks out loud and then
/// commits does not trip over its own reasoning.
#[must_use]
pub(super) fn parse(output: &str, policy: &TriagePolicy) -> Option<Assessment> {
    let line = output
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix(MARKER))?
        .trim();

    let mut kind = None;
    let mut priority = None;
    let mut exclude = None;

    for field in line.split(';') {
        let Some((name, value)) = field.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('`').to_owned();
        match name.trim() {
            "kind" => kind = Some(value),
            "priority" => priority = Some(value),
            "exclude" => exclude = Some(value),
            _ => {}
        }
    }

    // Checked against the policy, never merely non-empty. A label the ranker
    // cannot read would send the issue straight back round as unassessed.
    if let Some(kind) = exclude {
        return policy
            .excluded_kinds
            .contains(&kind)
            .then_some(Assessment::Exclude { kind });
    }

    let kind = kind?;
    let priority = priority?;
    (policy.defect_kinds.contains(&kind) && policy.ladder.rungs.contains(&priority))
        .then_some(Assessment::Place { kind, priority })
}

/// Write an assessment onto the issue.
///
/// The label and the comment are one action: a label with no comment leaves a
/// human wondering who decided this and on what basis, which is exactly the
/// complaint people have about bots touching their backlog.
pub(super) fn apply(
    provider: &dyn IssueProvider,
    key: &str,
    assessment: &Assessment,
    signature: &str,
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start a runtime for the issue provider: {error}"))?;

    let issue_key = IssueKey::from(key);
    let add = assessment.labels();

    runtime
        .block_on(provider.relabel(&issue_key, &add, &[]))
        .map_err(|error| error.to_string())?;

    let body = stella_autonomy::sign(
        &format!(
            "Triaged automatically: {}.\n\nJudged against this repository's \
             context records. If that is the wrong call, re-label the issue — \
             the next cycle reads the labels, not this comment.",
            assessment.reason()
        ),
        signature,
    );

    runtime
        .block_on(provider.comment(&issue_key, &body))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_autonomy::priority::PriorityLadder;

    fn policy() -> TriagePolicy {
        TriagePolicy::default()
    }

    fn unassessed() -> Unassessed {
        Unassessed {
            key: "42".into(),
            title: "the thing is broken".into(),
            created_at: "2026-08-19T00:00:00Z".into(),
        }
    }

    /// A well-formed answer in the declared vocabulary is accepted.
    #[test]
    fn a_declared_answer_places_the_issue() {
        assert_eq!(
            parse("ASSESSMENT: kind=bug; priority=P0", &policy()),
            Some(Assessment::Place {
                kind: "bug".into(),
                priority: "P0".into()
            })
        );
    }

    /// An invented label is not an answer.
    ///
    /// The witness for this module's strictness. Writing `urgent` when the
    /// ladder says `P0` would put a word in the tracker that no ranker reads,
    /// so the issue returns as unassessed on the next cycle and the loop pays
    /// for the same turn forever. Refusing costs one human glance.
    #[test]
    fn a_label_outside_the_vocabulary_is_not_an_answer() {
        assert_eq!(
            parse("ASSESSMENT: kind=bug; priority=urgent", &policy()),
            None
        );
        assert_eq!(
            parse("ASSESSMENT: kind=defect; priority=P0", &policy()),
            None
        );
        assert_eq!(parse("ASSESSMENT: exclude=whatever", &policy()), None);
    }

    /// Thinking out loud before committing is fine — the last line wins.
    #[test]
    fn the_last_assessment_line_wins() {
        let output = "ASSESSMENT: kind=bug; priority=P2\n\
                      on reflection this blocks a release\n\
                      ASSESSMENT: kind=bug; priority=P0";
        assert_eq!(
            parse(output, &policy()),
            Some(Assessment::Place {
                kind: "bug".into(),
                priority: "P0".into()
            })
        );
    }

    /// No answer at all is a refusal, not a default.
    ///
    /// The prompt explicitly permits this for an issue too poorly described to
    /// place. Defaulting to the lowest rung would bury it exactly the way the
    /// old `priority_rank` did.
    #[test]
    fn silence_is_a_refusal_not_a_default() {
        assert_eq!(
            parse("I cannot tell what this issue is asking for.", &policy()),
            None
        );
        assert_eq!(parse("", &policy()), None);
    }

    /// The prompt names the operator's vocabulary, not a built-in one.
    #[test]
    fn the_prompt_offers_only_the_declared_words() {
        let policy = TriagePolicy {
            ladder: PriorityLadder::new(vec!["Sev1".into()]),
            defect_kinds: vec!["defect".into()],
            excluded_kinds: vec!["wontfix".into()],
        };
        let text = prompt(&unassessed(), "it broke", &policy);
        assert!(text.contains("Sev1"));
        assert!(text.contains("defect"));
        assert!(text.contains("wontfix"));
        assert!(!text.contains("P0"), "a built-in rung must not leak in");
    }

    /// An excluded kind is a complete answer on its own.
    #[test]
    fn an_exclusion_needs_no_rung() {
        assert_eq!(
            parse("ASSESSMENT: exclude=enhancement", &policy()),
            Some(Assessment::Exclude {
                kind: "enhancement".into()
            })
        );
    }
}
