//! **The witness (#4463).** The withheld-steering notice on the deck.
//!
//! `AgentEvent::SteeringWithheld` reached the plain door and stopped there:
//! the deck's fold no-oped the arm, and the stderr line the notice otherwise
//! relies on is swallowed under the alternate screen. So on the first-class
//! surface a user whose repository's memories, rules, skills, commands and
//! agents had all been held back was told nothing at all.
//!
//! Pinned here: that the row names the remedy the *withholding authority*
//! actually admits, and that the row exists at all. `STELLA_TRUST_PROJECT=1`
//! printed against an org-managed ceiling tells a user who has already set
//! that flag to set it again.

use super::*;
use stella_protocol::Withholder;

fn withheld(withheld_by: Withholder) -> AgentEvent {
    AgentEvent::SteeringWithheld {
        withheld_by,
        memories: 3,
        records: 1,
        skills: 0,
        commands: 2,
        agents: 1,
    }
}

fn rows(withheld_by: Withholder) -> String {
    let mut model = SessionModel::new();
    model.apply(&withheld(withheld_by));
    assert_eq!(
        model.transcript.len(),
        1,
        "the fold produced no row: {:?}",
        model.transcript
    );
    transcript_lines(&model, false, 100)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The counts reach the transcript, and a zero count is left out rather than
/// printed as `0 skills` — the row says what was withheld, not what was not.
#[test]
fn the_deck_transcript_carries_what_was_withheld() {
    let text = rows(Withholder::ProjectUntrusted);
    assert!(text.contains("project steering not loaded"), "{text}");
    assert!(text.contains("3 memories"), "{text}");
    assert!(text.contains("1 context record"), "singular form: {text}");
    assert!(text.contains("2 commands"), "{text}");
    assert!(text.contains("1 agent"), "{text}");
    assert!(
        !text.contains("0 skills"),
        "an empty count is silent: {text}"
    );
}

/// The remedy differs by authority, which is the whole reason the event
/// carries one — and it is the half a second copy of this sentence would
/// eventually get wrong.
#[test]
fn the_remedy_names_the_authority_that_can_actually_lift_it() {
    let untrusted = rows(Withholder::ProjectUntrusted);
    assert!(
        untrusted.contains("STELLA_TRUST_PROJECT=1"),
        "an untrusted checkout is the user's to trust: {untrusted}"
    );
    assert!(
        untrusted.contains("auto_trust_project"),
        "and trusts it for good, not for one launch: this row recurs on every \
         launch of every untrusted repo, so a remedy that expires when the \
         process does is the wrong shape of answer: {untrusted}"
    );

    let managed = rows(Withholder::ManagedCeiling);
    assert!(
        managed.contains("managed settings forbid it"),
        "a managed ceiling is not: {managed}"
    );
    assert!(
        managed.contains("STELLA_TRUST_PROJECT does not lift it"),
        "and says so outright rather than leaving the flag to be retried: {managed}"
    );
}

/// **The witness.** A refused steering candidate draws its own row.
///
/// Same failure as the withheld notice above, one plane over. These
/// refusals went to stderr, which under the deck is the drawn frame. The
/// bytes landed between rows and scrolled the screen out from under
/// `ratatui`'s diff. The status bar came back drawn over itself.
///
/// The transcript, not `Inbound::Notice`, on the rule
/// `command_deck::steering` already draws. A notice dies at the next
/// keystroke and stays dead for the session. Each of these lines names a
/// remedy, and a remedy has to be there to scroll back to.
#[test]
fn a_refused_candidate_renders_its_headline_and_its_remedy() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::SteeringDropped {
        advisory: "a skill matching this turn did not fit the skill budget: seat-loser — raise \
                   `skills.max_skills`"
            .to_string(),
    });
    assert_eq!(
        model.transcript.len(),
        1,
        "the fold produced no row: {:?}",
        model.transcript
    );

    let text = transcript_lines(&model, false, 120)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        text.contains("steering"),
        "the row is headed as steering: {text}"
    );
    assert!(
        text.contains("seat-loser"),
        "the handle that lost its seat is named: {text}"
    );
    assert!(
        text.contains("skills.max_skills"),
        "and so is the knob that widens the budget: {text}"
    );
}

/// The em dash splits the line. A narrow frame then keeps what was refused
/// and drops the advice.
#[test]
fn the_headline_is_what_was_refused_and_the_detail_is_the_remedy() {
    let line = crate::textline::steering_dropped(
        "a tool did not fit what this turn's records left of the steering allowance: bash — \
         raise `context.steering.max_tokens`",
    );
    assert!(line.body.contains("bash"), "{line:?}");
    assert!(
        !line.body.contains("raise"),
        "the remedy left the head: {line:?}"
    );
    assert_eq!(
        line.detail.as_deref(),
        Some("raise `context.steering.max_tokens`"),
        "{line:?}"
    );
}

/// A line with no remedy clause is still whole, never cut short.
///
/// `drop_message` writes an em dash into every advisory today. This pins the
/// fallback if one ever stops. It keeps the whole text, and splits on no
/// separator that is not there.
#[test]
fn an_advisory_with_no_remedy_clause_keeps_its_whole_text() {
    let line = crate::textline::steering_dropped("the plane refused something");
    assert_eq!(line.body, "the plane refused something");
    assert_eq!(line.detail, None);
}
