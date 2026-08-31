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
use stella_protocol::issue::{IssueKey, IssueProvider, IssueState};

/// The size vocabulary a triage turn answers in, smallest first.
///
/// Fixed rather than declared in [`TriagePolicy`]. A size is an effort
/// estimate for a human reader, not a rung the ranker consumes, so no
/// operator needs to respell it. If a workspace ever wants its own scale,
/// this graduates into the policy the way the ladder did.
pub(super) const SIZES: [&str; 5] = ["XS", "S", "M", "L", "XL"];

/// Prefix a size answer wears as a tracker label — `M` becomes `size/M`.
///
/// The prefix is also the durable mark that this loop placed the issue.
/// No other actor writes `size/` labels here. That is what lets
/// [`assessment_stripped`] tell "placed, then stripped by the triage
/// guard" from "never placed at all".
pub(super) const SIZE_LABEL_PREFIX: &str = "size/";

/// The label meaning an assessed issue has no open blockers left.
///
/// One definition, owned by `stella_autonomy::ready` — the backlog
/// generator reads the same label this flip writes.
pub(super) const READY_LABEL: &str = stella_autonomy::ready::READY_LABEL;

/// The label meaning an issue waits on another issue named in its body.
pub(super) const BLOCKED_LABEL: &str = "status:blocked";

/// The tracker spelling of a size answer.
#[must_use]
pub(super) fn size_label(size: &str) -> String {
    format!("{SIZE_LABEL_PREFIX}{size}")
}

/// Where a turn decided an issue belongs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Assessment {
    /// Work for this loop: a defect kind, a rung, and an effort size.
    Place {
        /// The kind label to add, from `defect_kinds`.
        kind: String,
        /// The rung label to add, from the ladder.
        priority: String,
        /// The size answer, from [`SIZES`] — written as [`size_label`].
        size: String,
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
            Self::Place {
                kind,
                priority,
                size,
            } => vec![kind.clone(), priority.clone(), size_label(size)],
            Self::Exclude { kind } => vec![kind.clone()],
        }
    }

    /// A one-line account for the audit log and the issue comment.
    pub(super) fn reason(&self) -> String {
        match self {
            Self::Place {
                kind,
                priority,
                size,
            } => {
                format!("placed as `{kind}` at `{priority}`, sized `{size}`")
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
    let sizes = SIZES.join(", ");

    format!(
        "Triage one issue for this repository. Do not change any code.\n\n\
         Issue #{key}: {title}\n\n\
         --- body ---\n{body}\n--- end body ---\n\n\
         Decide three things, judged against this project's context records and \
         steering documents — they are the standard, not your own preferences:\n\n\
         1. Is this a defect this repository's self-driving loop should work, \
         or is it something else?\n\
         2. If it is work, how urgent is it?\n\
         3. If it is work, how large is it — judged on the largest of risk, \
         blast radius and effort?\n\n\
         Answer on a single final line, in exactly one of these two forms, using \
         only the words listed:\n\n\
         {MARKER} kind=<one of: {defects}>; priority=<one of: {rungs}>; \
         size=<one of: {sizes}>\n\
         {MARKER} exclude=<one of: {excluded}>\n\n\
         Any other vocabulary is not an answer. If the issue is too poorly \
         described to place, say so in prose and emit no {MARKER} line.",
        key = issue.key,
        title = issue.title,
    )
}

/// Everything a turn said, whatever envelope it arrived in.
///
/// `work::run_turn` spawns `stella run --output-format json`, so the answer
/// arrives **inside a JSON document** with its newlines escaped — no line of
/// that text ever begins with `ASSESSMENT:`. Scanning it line by line found
/// nothing, every assessment read as a refusal, and the first four issues the
/// loop triaged were escalated to a human with the turn's actual answer sitting
/// in the string it had just been handed.
///
/// Every string value is collected rather than one named field, because which
/// field carries the answer is `stella run`'s business and a name copied here
/// is a second definition waiting to drift. Non-JSON output is returned
/// unchanged, so a plain-text turn still works.
#[must_use]
fn answer_text(output: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return output.to_owned();
    };
    let mut collected = String::new();
    collect_strings(&value, &mut collected);
    collected
}

/// Append every string in a JSON document, one per line.
fn collect_strings(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::String(text) => {
            out.push_str(text);
            out.push('\n');
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_strings(item, out);
            }
        }
        serde_json::Value::Object(fields) => {
            for item in fields.values() {
                collect_strings(item, out);
            }
        }
        _ => {}
    }
}

/// Read a turn's answer, accepting only the declared vocabulary.
///
/// The last `ASSESSMENT:` line wins, so a turn that thinks out loud and then
/// commits does not trip over its own reasoning.
#[must_use]
pub(super) fn parse(output: &str, policy: &TriagePolicy) -> Option<Assessment> {
    let text = answer_text(output);
    let line = text
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix(MARKER))?
        .trim();

    let mut kind = None;
    let mut priority = None;
    let mut size = None;
    let mut exclude = None;

    for field in line.split(';') {
        let Some((name, value)) = field.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('`').to_owned();
        match name.trim() {
            "kind" => kind = Some(value),
            "priority" => priority = Some(value),
            "size" => size = Some(value),
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
    // A placement without a size is half an answer, and a half answer is
    // a refusal. Accepting it would ship an issue with no size label, and
    // the caller promises exactly one size per assessed issue.
    let size = size?;
    (policy.defect_kinds.contains(&kind)
        && policy.ladder.rungs.contains(&priority)
        && SIZES.contains(&size.as_str()))
    .then_some(Assessment::Place {
        kind,
        priority,
        size,
    })
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

/// Did the loop place this issue, and did the guard then strip it?
///
/// `triage-guard.yml` strips a priority applied by any login outside its
/// `TRIAGE_LOGINS` list, and re-queues the issue as unassessed. The queue
/// read cannot tell that issue from one nobody judged. A runner on a
/// disallowed login would re-triage it, get stripped again, and pay for
/// the same turn forever.
///
/// The size label is the tell. Only this loop writes `size/` labels, the
/// guard strips only priorities, and every placement writes both. So
/// "sized with no rung" can only mean the placement landed and its
/// priority was removed. The caller escalates once instead of re-asking,
/// and the escalation label keeps the issue out of the queue.
#[must_use]
pub(super) fn assessment_stripped(
    issue: &stella_protocol::issue::Issue,
    policy: &TriagePolicy,
) -> bool {
    let sized = issue
        .labels
        .iter()
        .any(|label| label.name.starts_with(SIZE_LABEL_PREFIX));
    let runged = policy
        .ladder
        .rungs
        .iter()
        .any(|rung| issue.labels.iter().any(|label| &label.name == rung));
    sized && !runged
}

/// Flip a placed issue from blocked to ready, if nothing blocks it.
///
/// The `Blocked by:` lines are parsed by `stella_autonomy::ready` — the
/// one definition the backlog generator reads too. Each declared blocker
/// is then resolved through the port. The body is a claim and the tracker
/// holds the fact: a blocker that has since closed is not a blocker, and
/// one the tracker cannot find never was. The flip happens only when no
/// blocker is still open — one relabel adding [`READY_LABEL`] and removing
/// [`BLOCKED_LABEL`]. For an issue never labelled blocked, the removal is
/// a no-op.
///
/// A blocker the forge cannot answer for fails the whole decision. Reading
/// an outage as "closed" would mark waiting work ready.
pub(super) fn flip_ready(
    provider: &dyn IssueProvider,
    key: &str,
    body: &str,
) -> Result<bool, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start a runtime for the issue provider: {error}"))?;

    for blocker in stella_autonomy::ready::blocker_refs(body) {
        let blocker = blocker.to_string();
        // An issue cannot hold itself out of the queue.
        if blocker == key {
            continue;
        }
        match runtime.block_on(provider.get(&IssueKey::from(blocker.as_str()))) {
            Ok(issue) if issue.state == IssueState::Open => return Ok(false),
            Ok(_) => {}
            // A key the tracker cannot find is not an open blocker — the
            // declaration outlived the issue, or never named one.
            Err(stella_protocol::issue::IssueError::NotFound { .. }) => {}
            Err(error) => return Err(error.to_string()),
        }
    }

    runtime
        .block_on(provider.relabel(
            &IssueKey::from(key),
            &[READY_LABEL.to_owned()],
            &[BLOCKED_LABEL.to_owned()],
        ))
        .map_err(|error| error.to_string())?;
    Ok(true)
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
            parse("ASSESSMENT: kind=bug; priority=P0; size=M", &policy()),
            Some(Assessment::Place {
                kind: "bug".into(),
                priority: "P0".into(),
                size: "M".into()
            })
        );
    }

    /// **The sizing witness.** A sized placement parses, and its labels carry
    /// exactly one `size/` label alongside the kind and the rung — the shape
    /// the tracker convention asks for.
    #[test]
    fn a_sized_assessment_parses_and_writes_exactly_one_size_label() {
        let assessment =
            parse("ASSESSMENT: kind=bug; priority=P1; size=XL", &policy()).expect("a placement");
        let labels = assessment.labels();
        assert_eq!(labels, vec!["bug", "P1", "size/XL"]);
        assert_eq!(
            labels
                .iter()
                .filter(|label| label.starts_with(SIZE_LABEL_PREFIX))
                .count(),
            1,
            "exactly one size label per assessed issue"
        );
    }

    /// A placement that names no size is half an answer, and half an answer
    /// is a refusal — otherwise the issue ships without the size label the
    /// convention promises.
    #[test]
    fn a_placement_without_a_size_is_not_an_answer() {
        assert_eq!(parse("ASSESSMENT: kind=bug; priority=P0", &policy()), None);
        assert_eq!(
            parse("ASSESSMENT: kind=bug; priority=P0; size=huge", &policy()),
            None,
            "an invented size is no better than a missing one"
        );
    }

    /// The answer survives the JSON envelope it arrives in.
    ///
    /// **The witness for a feature that was decorative without it.**
    /// `run_turn` spawns `stella run --output-format json`, so the turn's text
    /// arrives inside a JSON string with its newlines escaped. Scanning that
    /// document line by line finds no `ASSESSMENT:` line, every answer reads
    /// as a refusal, and the loop escalates issues it had in fact placed — it
    /// did exactly that to the first four it triaged.
    #[test]
    fn an_answer_inside_the_json_envelope_is_still_an_answer() {
        let envelope = serde_json::json!({
            "status": "ok",
            "result": "Looking at the context records, this blocks a release.\nASSESSMENT: kind=bug; priority=P0; size=S",
        })
        .to_string();

        assert!(
            !envelope
                .lines()
                .any(|l| l.trim().starts_with("ASSESSMENT:")),
            "the envelope must genuinely hide the marker, or this proves nothing"
        );
        assert_eq!(
            parse(&envelope, &policy()),
            Some(Assessment::Place {
                kind: "bug".into(),
                priority: "P0".into(),
                size: "S".into()
            })
        );
    }

    /// Plain text still works, so the envelope handling is additive.
    #[test]
    fn a_plain_text_answer_still_parses() {
        assert_eq!(
            parse("ASSESSMENT: exclude=documentation", &policy()),
            Some(Assessment::Exclude {
                kind: "documentation".into()
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
            parse("ASSESSMENT: kind=bug; priority=urgent; size=M", &policy()),
            None
        );
        assert_eq!(
            parse("ASSESSMENT: kind=defect; priority=P0; size=M", &policy()),
            None
        );
        assert_eq!(parse("ASSESSMENT: exclude=whatever", &policy()), None);
    }

    /// Thinking out loud before committing is fine — the last line wins.
    #[test]
    fn the_last_assessment_line_wins() {
        let output = "ASSESSMENT: kind=bug; priority=P2; size=S\n\
                      on reflection this blocks a release\n\
                      ASSESSMENT: kind=bug; priority=P0; size=S";
        assert_eq!(
            parse(output, &policy()),
            Some(Assessment::Place {
                kind: "bug".into(),
                priority: "P0".into(),
                size: "S".into()
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

    use async_trait::async_trait;
    use stella_protocol::issue::{Issue, IssueClass, IssueDraft, IssueError, IssueLabel};

    fn issue(key: &str, state: IssueState, labels: &[&str], body: &str) -> Issue {
        Issue {
            key: IssueKey::from(key),
            title: format!("issue {key}"),
            body: body.into(),
            state,
            class: IssueClass::Bug,
            labels: labels.iter().copied().map(IssueLabel::from).collect(),
            created_at: "2026-08-19T00:00:00Z".into(),
            updated_at: "2026-08-19T00:00:00Z".into(),
            url: String::new(),
            parent: None,
        }
    }

    /// One recorded relabel: the key, the labels added, the labels removed.
    type Relabel = (String, Vec<String>, Vec<String>);

    /// A tracker that answers `get` from a fixed set and records relabels —
    /// no GitHub, no credential, no subprocess.
    #[derive(Default)]
    struct FlipFixture {
        known: Vec<Issue>,
        relabels: std::sync::Mutex<Vec<Relabel>>,
    }

    impl FlipFixture {
        fn relabels(&self) -> Vec<Relabel> {
            self.relabels.lock().expect("fixture lock").clone()
        }
    }

    #[async_trait]
    impl IssueProvider for FlipFixture {
        fn id(&self) -> &str {
            "fixture"
        }

        async fn list_open(&self, limit: usize) -> Result<Vec<Issue>, IssueError> {
            Ok(self
                .known
                .iter()
                .filter(|issue| issue.state == IssueState::Open)
                .take(limit)
                .cloned()
                .collect())
        }

        async fn get(&self, key: &IssueKey) -> Result<Issue, IssueError> {
            self.known
                .iter()
                .find(|issue| issue.key == *key)
                .cloned()
                .ok_or_else(|| IssueError::NotFound { key: key.clone() })
        }

        async fn file(&self, _draft: &IssueDraft) -> Result<IssueKey, IssueError> {
            Ok(IssueKey::from("1000"))
        }

        async fn close(
            &self,
            _key: &IssueKey,
            _receipt: &str,
            _state: &str,
        ) -> Result<(), IssueError> {
            Ok(())
        }

        async fn comment(&self, _key: &IssueKey, _body: &str) -> Result<(), IssueError> {
            Ok(())
        }

        async fn relabel(
            &self,
            key: &IssueKey,
            add: &[String],
            remove: &[String],
        ) -> Result<(), IssueError> {
            self.relabels.lock().expect("fixture lock").push((
                key.as_str().to_owned(),
                add.to_vec(),
                remove.to_vec(),
            ));
            Ok(())
        }

        async fn edit(
            &self,
            _key: &IssueKey,
            _title: Option<&str>,
            _body: Option<&str>,
        ) -> Result<(), IssueError> {
            Ok(())
        }
    }

    /// **The ready-flip witness.** The flip happens only when no declared
    /// blocker is still open, and it is one relabel — ready on, blocked off.
    #[test]
    fn the_ready_flip_happens_only_when_no_blocker_is_still_open() {
        let body = "Fix the thing.\n\nBlocked by: #7\n";

        // #7 is open: no flip, and the tracker is not written to at all.
        let blocked = FlipFixture {
            known: vec![issue("7", IssueState::Open, &["bug"], "")],
            ..FlipFixture::default()
        };
        assert_eq!(flip_ready(&blocked, "42", body), Ok(false));
        assert!(
            blocked.relabels().is_empty(),
            "a blocked issue must not be relabelled"
        );

        // #7 has closed: the flip happens, as exactly one relabel.
        let cleared = FlipFixture {
            known: vec![issue("7", IssueState::Closed, &["bug"], "")],
            ..FlipFixture::default()
        };
        assert_eq!(flip_ready(&cleared, "42", body), Ok(true));
        assert_eq!(
            cleared.relabels(),
            vec![(
                "42".to_owned(),
                vec![READY_LABEL.to_owned()],
                vec![BLOCKED_LABEL.to_owned()]
            )]
        );
    }

    /// A blocker the tracker never heard of is not an open blocker — the
    /// declaration outlived the issue, and the flip proceeds.
    #[test]
    fn a_blocker_the_tracker_cannot_find_does_not_block() {
        let fixture = FlipFixture::default();
        assert_eq!(flip_ready(&fixture, "42", "Blocked by: #9\n"), Ok(true));
        assert_eq!(fixture.relabels().len(), 1);
    }

    /// Several blockers on one line all count, and any one still open holds
    /// the flip back.
    #[test]
    fn any_one_open_blocker_holds_the_flip() {
        let fixture = FlipFixture {
            known: vec![
                issue("7", IssueState::Closed, &["bug"], ""),
                issue("8", IssueState::Open, &["bug"], ""),
            ],
            ..FlipFixture::default()
        };
        assert_eq!(
            flip_ready(&fixture, "42", "Blocked by: #7, #8\n"),
            Ok(false)
        );
        assert!(fixture.relabels().is_empty());
    }

    /// **The strip witness.** An issue this loop sized whose rung has been
    /// removed reads as stripped — and once the caller's one escalation
    /// lands, the queue drops it on the escalation label, so a guard that
    /// disagrees with the runner's login costs one human glance, never an
    /// infinite re-triage loop.
    #[test]
    fn a_stripped_assessment_escalates_once_rather_than_looping() {
        let policy = policy();

        let stripped = issue("42", IssueState::Open, &["bug", "size/M", "triage"], "");
        assert!(
            assessment_stripped(&stripped, &policy),
            "sized with no rung — only the guard produces this shape"
        );
        assert!(
            !assessment_stripped(
                &issue("43", IssueState::Open, &["bug", "size/M", "P1"], ""),
                &policy
            ),
            "an intact assessment is not a strip"
        );
        assert!(
            !assessment_stripped(
                &issue("44", IssueState::Open, &["bug", "triage"], ""),
                &policy
            ),
            "an issue never assessed has no size label and is a question, not a strip"
        );

        // The once-ness: after the caller escalates, the issue leaves the
        // queue on the escalation label — neither ranked nor re-asked.
        let escalated = stella_autonomy::priority::triage(
            vec![stella_autonomy::QueueIssue {
                number: 42,
                title: "stripped".into(),
                created_at: "2026-08-19T00:00:00Z".into(),
                labels: ["bug", "size/M", stella_autonomy::ESCALATION_LABEL]
                    .iter()
                    .map(|name| stella_autonomy::IssueLabel {
                        name: (*name).to_owned(),
                    })
                    .collect(),
                url: String::new(),
            }],
            &policy,
        );
        assert!(
            escalated.is_empty(),
            "an escalated strip must not come back as work or as a question"
        );
    }
}
