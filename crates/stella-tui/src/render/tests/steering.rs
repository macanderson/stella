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
