// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The end-of-turn service assertion (#2764): a turn that is about to declare
//! itself finished while a service it started is still listening gets told
//! so, once, before the declaration stands.
//!
//! # The failure this closes
//!
//! #2666 came from bench trials (kv-store-grpc, pypi-server) where the model
//! did the right thing — started the service it was asked for, saw it come
//! up, said so — and the grader found it dead. Two of the three causes were
//! mechanical and are fixed: `start_process` children were SIGKILLed on
//! `ProcessTable::drop`, and `bash` hung on a backgrounded grandchild holding
//! its pipe open. The third is not mechanical, and is this module: **nothing
//! ever asked the agent about the service before it stopped.**
//!
//! A run can now leave a service up correctly, which means "still running" is
//! no longer evidence of a mistake — and that is exactly why it needs saying
//! out loud rather than acting on. Two very different turns end the same way:
//!
//! - the service IS the deliverable and staying up is the whole point, and
//! - the model started something, never checked it was reachable, and
//!   declared done anyway.
//!
//! Nothing in the transcript distinguishes them, and the engine cannot: only
//! the model knows which turn it just had. So the assertion states the fact
//! and hands the decision back.
//!
//! # What it deliberately does not do
//!
//! **It never stops anything.** `restart_process` stays the only path that kills
//! a started child on Stella's own initiative (`stella-tools`'s `process`
//! module doc), because #2666's finding is precisely that a surviving service
//! can be the correct final state. A gate that tidied up would re-introduce
//! the bug it came from, wearing a helpful face.
//!
//! It also does not judge. There is no heuristic here about which services
//! *ought* to be up — the state is reported, one line per handle, and the
//! model answers.
//!
//! # The shape: one nudge, then the declaration stands
//!
//! The mechanism is the prove-it gate's ([`super::confident_zero`], #2663),
//! for the same reason and with the same bound: the completing step's
//! declaration and a system-authored reply are appended, the turn continues,
//! and the nudge is issued **at most once per turn** — detected from its own
//! prefix on the transcript, so it needs no state field and survives a
//! checkpoint resume for free. Asked and answered, whatever the answer: a
//! model that confirms the service should stay up is not asked again, and the
//! second declaration completes.
//!
//! Unlike the prove-it gate this is not opt-in. It fires wherever a turn
//! started a service, because the question is about state that outlives the
//! turn on every surface — a conversational session leaves the same orphan a
//! headless run does.

use stella_protocol::{AgentEvent, CompletionMessage};

use crate::event_sender::EventSender;
use crate::ports::LiveService;

/// The nudge's own marker, prefix-detected on the transcript. Shares the
/// once-per-turn window with the prove-it nudge — see
/// [`super::confident_zero::nudge_turn_start`], which must skip both or each
/// gate's message reopens the other's window (the `gate-ab` regression, #2663).
pub(super) const SERVICES_PREFIX: &str = "Before ending this turn:";

/// One line per still-running service, in the order the executor reported
/// them. Named by handle first because that is the argument `restart_process`
/// takes, so a model that decides to stop one has the call already written.
fn roster(services: &[LiveService]) -> String {
    services
        .iter()
        .map(|service| match &service.name {
            Some(name) => format!("  - {} (`{}`): {}", name, service.handle, service.display),
            None => format!("  - `{}`: {}", service.handle, service.display),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The message itself. Deliberately two-sided: both answers are legitimate
/// endings, and a nudge that only offered one would train the model to stop
/// services that were the point of the task.
fn nudge(services: &[LiveService]) -> String {
    let count = services.len();
    let noun = if count == 1 { "process" } else { "processes" };
    format!(
        "{SERVICES_PREFIX} {count} {noun} you started {} still running:\n{}\n\nThis is not \
         necessarily wrong — a service you were asked to bring up should stay up, and it will \
         survive this turn. Decide which case this is. If it should keep running, confirm you \
         have checked it is actually reachable (connect to it, request from it, read its log) \
         and say so. If it was a leftover, say so and leave it running — a live service \
          cannot be stopped (#2864); `restart_process` replaces one but never leaves it \
          down. Then declare completion.",
        if count == 1 { "is" } else { "are" },
        roster(services)
    )
}

/// Ask the completing turn about the services it left up, at most once.
///
/// Returns `true` when the nudge was issued — the declaration and the reply
/// are already appended, and the caller lets the turn continue, exactly as
/// [`super::confident_zero::CompletionRuling::Nudged`] does. `false` means
/// proceed: nothing is running, or this turn has already been asked.
///
/// `declaration` is the completing step's own text, appended ahead of the
/// nudge so the transcript reads claim-then-question rather than leaving the
/// model's answer unattached to anything.
pub(super) fn check(
    messages: &mut Vec<CompletionMessage>,
    services: &[LiveService],
    declaration: &str,
    events: &EventSender,
) -> bool {
    if services.is_empty() {
        return false;
    }
    if already_asked(messages) {
        return false;
    }
    let _ = events.send(AgentEvent::Text {
        text: format!(
            "\n⚠ {} long-running {} still up at the end of this turn — asking the model to \
             confirm or stop {}.",
            services.len(),
            if services.len() == 1 {
                "process is"
            } else {
                "processes are"
            },
            if services.len() == 1 { "it" } else { "them" },
        ),
    });
    messages.push(CompletionMessage::assistant(declaration));
    messages.push(CompletionMessage::user(nudge(services)));
    true
}

/// Whether this turn already carries the assertion. Windowed with
/// [`super::confident_zero::nudge_turn_start`] so the nudge's own message —
/// a user message, which the plain turn window restarts at — cannot put the
/// marker outside the scan that looks for it.
fn already_asked(messages: &[CompletionMessage]) -> bool {
    let start = super::confident_zero::nudge_turn_start(messages);
    messages[start..]
        .iter()
        .any(|message| message.content.starts_with(SERVICES_PREFIX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::MessageRole;

    fn events() -> EventSender {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        EventSender::new(tx)
    }

    fn service(handle: &str, name: Option<&str>) -> LiveService {
        LiveService {
            handle: handle.into(),
            name: name.map(str::to_string),
            display: "python3 -m http.server 8080".into(),
        }
    }

    /// **The #2764 witness.** A turn that leaves a service up is asked about
    /// it exactly once: the first check nudges (appending claim-then-question),
    /// the second — same declaration, marker already on the transcript —
    /// stands down and lets the declaration complete.
    #[test]
    fn a_turn_that_leaves_a_service_up_is_asked_exactly_once() {
        let mut messages = vec![CompletionMessage::user("serve the docs on 8080")];
        let services = vec![service("proc-1", Some("docs"))];

        assert!(check(
            &mut messages,
            &services,
            "Done — the server is running.",
            &events()
        ));
        let asked = messages.last().expect("the question follows the claim");
        assert_eq!(asked.role, MessageRole::User);
        assert!(asked.content.starts_with(SERVICES_PREFIX));
        assert!(
            asked.content.contains("proc-1") && asked.content.contains("docs"),
            "the handle and its label must both be named: {}",
            asked.content
        );
        assert!(
            asked.content.contains("restart_process"),
            "the way to stop one has to be in the message: {}",
            asked.content
        );
        assert!(
            asked.content.contains("should stay up"),
            "and so does permission to leave it running — #2666's whole finding"
        );

        assert!(
            !check(
                &mut messages,
                &services,
                "Done — the server is running.",
                &events()
            ),
            "asked and answered: one assertion per turn, whatever the answer"
        );
    }

    /// A turn that started nothing — or stopped what it started — is never
    /// touched. The service list is what the executor reports as still
    /// running, so a stopped process is simply absent from it.
    #[test]
    fn nothing_running_never_nudges() {
        let mut messages = vec![CompletionMessage::user("what does retry.rs do?")];
        assert!(!check(
            &mut messages,
            &[],
            "It implements the retry policy.",
            &events()
        ));
        assert_eq!(
            messages.len(),
            1,
            "a clean turn's transcript must be left byte-identical"
        );
    }

    /// The window regression the prove-it gate already paid for once
    /// (`gate-ab`, #2663): this nudge is a user message, so a plain
    /// turn-start scan would restart *at* it and re-arm the gate on the next
    /// declaration — every completing step, for as long as the service runs.
    #[test]
    fn the_assertions_own_message_never_reopens_its_window() {
        let mut messages = vec![CompletionMessage::user("serve the docs on 8080")];
        let services = vec![service("proc-1", None)];
        assert!(check(&mut messages, &services, "Done.", &events()));
        // The model answers, does a little more work, and declares again.
        messages.push(CompletionMessage::assistant("Confirmed reachable on 8080."));
        assert!(
            !check(&mut messages, &services, "Done, confirmed.", &events()),
            "the answer must close the question, not restart it"
        );
    }

    /// A genuinely new turn — a fresh user message that is not itself a
    /// nudge — is asked again. The bound is per turn, not per session: a
    /// service left up two turns ago is still worth naming when THIS turn
    /// declares done.
    #[test]
    fn the_next_real_turn_is_asked_again() {
        let mut messages = vec![CompletionMessage::user("serve the docs on 8080")];
        let services = vec![service("proc-1", None)];
        assert!(check(&mut messages, &services, "Done.", &events()));
        messages.push(CompletionMessage::user("now add a health endpoint"));
        assert!(
            check(&mut messages, &services, "Added.", &events()),
            "a new turn gets its own assertion"
        );
    }

    /// Plural and singular both read correctly, and every handle is named —
    /// a count with one example would leave the model guessing which others.
    #[test]
    fn every_running_handle_is_named() {
        let mut messages = vec![CompletionMessage::user("bring up the stack")];
        let services = vec![
            service("proc-1", Some("api")),
            service("proc-2", None),
            service("proc-7", Some("worker")),
        ];
        assert!(check(&mut messages, &services, "Stack is up.", &events()));
        let asked = &messages.last().unwrap().content;
        for handle in ["proc-1", "proc-2", "proc-7"] {
            assert!(asked.contains(handle), "{handle} missing from: {asked}");
        }
        assert!(asked.contains("3 processes"), "{asked}");
    }
}
