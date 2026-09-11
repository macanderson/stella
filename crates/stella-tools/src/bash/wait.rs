// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What `bash` does about a command that waits.
//!
//! Two rungs read a `sleep` out of the command text before anything runs.
//! Past [`SLEEP_ADVISORY_THRESHOLD_SECS`] the call runs, and
//! [`sleep_advisory`] names the wait in the result. Past
//! [`SLEEP_REFUSAL_THRESHOLD_SECS`] the call does not start at all.
//! [`blocking_wait_refusal`] declines it and no shell is spawned.
//!
//! Both rungs ask [`blocking_sleep_seconds`] the same question. They differ
//! in what the answer buys.
//!
//! The upper rung exists because the lower one cannot save its own call. That
//! note rides the result of the call it fires on. By the time the model reads
//! it, the wait is paid for (`#3753`).
//!
//! This sits beside [`super`] for the reason [`super::words`] does. `bash.rs`
//! is at the 1500-line ceiling. A wait is also its own subject, apart from
//! the tool's spawn, timeout and policy body.

use stella_core::shell_text::blocking_sleep_seconds;

/// The per-call rung (`#2022`): a `sleep` this long is named in the result
/// the model reads.
///
/// The bound is low on purpose. It catches the shape on the call that made
/// it, before any accumulation.
///
/// It cannot see the turn. Loop detection reads interleaved calls as
/// progress. The budget guard counts spend, and idling costs $0. Closing that
/// gap is the engine's job, over the seconds a whole turn asked for
/// (`stella_core`'s stall rung, `driver::loop_escalation`).
///
/// The bound also keeps a retry backoff quiet. [`blocking_sleep_seconds`]
/// counts a sleep beside real work. So `sleep 2 && curl` reports two seconds
/// and is ignored. A `sleep 490` next to a backgrounded install reports 490
/// and is named.
///
/// A wait this short is named and still runs.
///
/// Both rungs read the command text and never a measured elapsed time. That
/// keeps them deterministic for the loop detector. A timing in
/// [`stella_protocol::tool::ToolOutput`] would make identical calls look
/// distinct and defeat it, so none is embedded here.
const SLEEP_ADVISORY_THRESHOLD_SECS: u64 = 30;

/// A footer naming a `sleep` past the advisory bound, with a remedy the agent
/// can perform.
///
/// **The remedy has to name a tool that exists.** This note once pointed at
/// `read_output` and `wait_for`. Those went with the cut to twelve built-in
/// tools (`#3244`), and the restore left them out. Every long `sleep` then
/// handed the model a directive with no tool behind it. That is worse than
/// silence. An instruction nobody can follow teaches the model to discount
/// the next one.
///
/// The wording now stays inside what the surface offers. A short poll in a
/// loop is something `bash` can do on its own, and it returns as soon as the
/// condition holds.
pub(super) fn sleep_advisory(command: &str) -> Option<String> {
    let secs = blocking_sleep_seconds(command)?;
    if secs < SLEEP_ADVISORY_THRESHOLD_SECS {
        return None;
    }
    Some(format!(
        "\n\nnote: this call spent {secs}s inside `sleep`, and the whole interval was charged \
         to the turn whether or not the thing you are waiting for finished early. If you are \
         waiting on something, poll for the condition instead of sleeping through it — a \
         bounded retry loop that checks and exits as soon as the check passes (for example \
         `for i in $(seq 30); do <check> && break; sleep 1; done`) costs a fraction of a blind \
         wait."
    ))
}

/// How long a `sleep` may be before the call is declined rather than named.
/// See [`blocking_wait_refusal`] for why the bound is
/// [`super::DEFAULT_TIMEOUT_SECS`].
const SLEEP_REFUSAL_THRESHOLD_SECS: u64 = super::DEFAULT_TIMEOUT_SECS;

/// The rung above the advisory. A `sleep` this long is the command, not part
/// of one, so the call is declined before the spawn.
///
/// [`sleep_advisory`] rides the result of the call it fires on. The wait is
/// paid for by the time the model reads the remedy. On the run this rule was
/// measured against, it never saved a second. The two calls it would have
/// named ran 489.5s and 280.4s, and the note arrived after both.
///
/// A refusal that arrives once the process has run is a report, not a fence.
/// [`super::shell_write_audit`] reads the text before the spawn for that
/// reason, and so does this (`#3753`).
///
/// The bound is [`super::DEFAULT_TIMEOUT_SECS`]. A wait that outlasts the
/// default limit for a whole command is the command. In arenabench match
/// `13f7f2bb533d` the calls that sleep at all wait 2s, 20s, 280s and 490s.
/// Any bound between the poll loop and the blind waits declines the same two
/// calls. This one is a constant the tool already has, not a number fitted to
/// that gap.
///
/// A polling loop survives the rung. [`blocking_sleep_seconds`] reads a
/// segment of exactly `sleep N`, so it sees one pass of
/// `…; <check> && break; sleep 1; done` and answers one second, not the
/// thirty of the worst case. Where the loop keyword shares the segment, as in
/// `do sleep 20;`, it sees no sleep at all. Both readings fall under the
/// bound, so neither shape is declined. The same panel measured such a loop
/// at 20.9s, against 280.4s for the blind wait it replaces.
///
/// That second reading is the predicate under-reading, and the direction is
/// the safe one here: a wait this rung cannot see is a wait it never
/// declines. It costs the advisory below, which stays silent on the same
/// shape.
pub(super) fn blocking_wait_refusal(command: &str) -> Option<String> {
    let secs = blocking_sleep_seconds(command)?;
    if secs < SLEEP_REFUSAL_THRESHOLD_SECS {
        return None;
    }
    Some(format!(
        "not executed — this call would spend {secs}s inside `sleep`, and the whole interval is \
         charged to this turn whether or not the thing you are waiting for finishes early. Poll \
         for the condition instead: a bounded retry loop that checks and exits as soon as the \
         check passes (for example `for i in $(seq 30); do <check> && break; sleep 10; done`) \
         returns the moment the thing is ready and costs a fraction of a blind wait. If you are \
         waiting on something you started in the background, poll for the evidence that it \
         finished — its log, its process, its output file — rather than sleeping for how long \
         you expect it to take."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `#2022` witness: the exact observed pathological shape
    /// (`sleep 300; echo done`, `sleep 120` alone) is caught and its
    /// accumulated seconds are named, so the advisory can fire on it.
    #[test]
    fn a_sleep_is_detected_and_summed() {
        assert_eq!(blocking_sleep_seconds("sleep 300; echo done"), Some(300));
        assert_eq!(blocking_sleep_seconds("sleep 120"), Some(120));
        assert_eq!(blocking_sleep_seconds("sleep 60"), Some(60));
        // Several sleeps in one call accumulate.
        assert_eq!(
            blocking_sleep_seconds("sleep 30 && sleep 30"),
            Some(60),
            "accumulated sleep across the whole call, not just the last segment"
        );
        assert_eq!(
            blocking_sleep_seconds("sleep 2.5"),
            Some(3),
            "rounds to the nearest second"
        );
    }

    /// A short wait stays unflagged, and the threshold is what keeps it that
    /// way. A retry backoff and a five-second pause before a `tail` both
    /// report their seconds; neither crosses the line.
    #[test]
    fn a_short_sleep_beside_real_work_is_not_flagged() {
        for command in [
            "sleep 2 && curl -s http://localhost:8080",
            "sleep 5; tail -f build.log",
            "echo waiting; sleep 5; ls",
            "for i in $(seq 30); do curl -sf http://localhost:8080 && break; sleep 1; done",
        ] {
            assert_eq!(
                sleep_advisory(command),
                None,
                "advice was appended for `{command}`"
            );
        }
        // A command that never calls `sleep` has nothing to report at all.
        assert_eq!(blocking_sleep_seconds("grep sleep /app/main.c"), None);
    }

    /// The two waits that cost `13f7f2bb533d` most of a quarter of its
    /// measured tool time. Both sit beside real work, so both were silent
    /// while the predicate demanded a command made of nothing but sleeps.
    ///
    /// `pytorch-model-cli` backgrounded a `pip install torch` and then blocked
    /// on a fixed 490s wait instead of on the install; `rstan-to-pystan` slept
    /// 280s and then tailed the apt log it was waiting for. Each one is what
    /// the advisory's own remedy describes — poll for the condition — and
    /// neither was ever told.
    #[test]
    fn a_long_sleep_beside_real_work_is_named() {
        let backgrounded_install = "timeout 500 pip install --quiet torch &\nBGPID=$!\nsleep 490\nwait $BGPID 2>/dev/null\necho DONE";
        let note = sleep_advisory(backgrounded_install).expect("over threshold");
        assert!(note.contains("490s"), "{note}");

        let blind_poll = "sleep 280; tail -30 apt_install.log; ps aux | grep apt";
        let note = sleep_advisory(blind_poll).expect("over threshold");
        assert!(note.contains("280s"), "{note}");
        assert!(note.contains("poll"), "{note}");
    }

    #[test]
    fn the_sleep_advisory_only_fires_past_the_threshold() {
        assert!(
            sleep_advisory("sleep 5").is_none(),
            "under threshold, no nudge"
        );
        let note = sleep_advisory("sleep 300; echo done").expect("over threshold");
        assert!(note.contains("300s"));
        // The remedy must be one the agent can actually perform, so this
        // pins that the note names polling and names NO tool the catalog does
        // not carry. `read_output` and `wait_for` went with the reduction to
        // twelve built-in tools (`#3244`) and the restore left them out.
        assert!(note.contains("poll"), "{note}");
        for gone in ["read_output", "wait_for", "start_process"] {
            assert!(
                !note.contains(gone),
                "the advisory names `{gone}`, which is not on the tool surface: {note}"
            );
        }
    }

    #[test]
    /// **The witness for `#3753`.** The two calls that dominated the panel's
    /// tool time are declined before they spawn. Both command strings are the
    /// ones the trials sent — arenabench match `13f7f2bb533d`,
    /// `pytorch-model-cli` and `rstan-to-pystan` — and between them they ran
    /// 770 of the 3,406 seconds of tool execution that match recorded. The
    /// advisory named them and could not save them: it is appended to the
    /// result of the call it fires on.
    #[test]
    fn a_wait_longer_than_a_whole_command_never_starts() {
        let backgrounded_install = "timeout 500 pip install --quiet torch &\nBGPID=$!\nsleep 490\nwait $BGPID 2>/dev/null\necho DONE";
        let refusal = blocking_wait_refusal(backgrounded_install).expect("490s is over the bound");
        assert!(refusal.starts_with("not executed"), "{refusal}");
        assert!(refusal.contains("490s"), "{refusal}");
        assert!(refusal.contains("Poll for the condition"), "{refusal}");

        let blind_poll = "sleep 280; tail -30 apt_install.log; ps aux | grep apt";
        let refusal = blocking_wait_refusal(blind_poll).expect("280s is over the bound");
        assert!(refusal.contains("280s"), "{refusal}");
    }

    /// The rung leaves the cheap pattern alone. A poll loop reports the
    /// seconds of one iteration, so it stays admissible however many
    /// iterations it is willing to run — the same match measured that loop at
    /// 20.9s against the 280.4s of the blind wait it replaces. The other
    /// waits below the bound keep running too.
    #[test]
    fn a_poll_loop_and_a_short_wait_still_run() {
        for command in [
            "for i in $(seq 1 25); do sleep 20; if ! ps aux | grep -q \"[a]pt-get install\"; then echo DONE; break; fi; done",
            "nohup apt-get install -y g++ > apt_install.log 2>&1 &\nsleep 2\necho started",
            "sleep 2 && curl -s http://localhost:8080",
            "echo waiting; sleep 5; ls",
            "grep sleep /app/main.c",
        ] {
            assert_eq!(
                blocking_wait_refusal(command),
                None,
                "{command} is below the bound and must still run"
            );
        }
    }

    /// Two rungs, ordered, both reachable: a wait between them is named and
    /// still runs, and only the upper one declines.
    #[test]
    fn the_advisory_and_the_refusal_are_two_rungs() {
        assert!(SLEEP_ADVISORY_THRESHOLD_SECS < SLEEP_REFUSAL_THRESHOLD_SECS);
        let between = "sleep 60; echo done";
        assert!(sleep_advisory(between).is_some(), "named at the lower rung");
        assert_eq!(blocking_wait_refusal(between), None, "and still runs");
    }
}
