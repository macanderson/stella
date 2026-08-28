// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One stdin reader for every mid-turn prompt, so a prompt that times out
//! cannot eat the line the person types next.
//!
//! Both interactive surfaces — approvals ([`crate::interactive::TtyAskUserIo`])
//! and `ask_question` ([`crate::question::TtyLineIo`]) — used to call
//! `spawn_blocking(|| stdin().read_line(..))` directly, and a blocking read
//! cannot be cancelled once it has started. Their brokers bound the park with a
//! TTL, but `tokio::time::timeout` drops the *future*; the closure stays parked
//! inside `read_line`. The person then types their next message and it is
//! consumed by the dead prompt (#4219).
//!
//! `ask_question` made it materially worse: a 30-minute TTL against approvals'
//! two minutes, and several reads per call, so a cancel mid-batch could strand
//! a reader at any of them — each one leaking another parked thread that would
//! swallow another line.
//!
//! # What replaces it
//!
//! One long-lived reader task, asked for a line rather than reading on its own.
//! Two properties follow, and they are the whole fix:
//!
//! - **At most one parked read exists process-wide.** The task reads only when
//!   somebody asks, and it will not ask again until the previous read
//!   finishes. A hundred timed-out prompts leak nothing; the old shape leaked a
//!   blocking-pool thread per timeout.
//! - **A line whose asker has gone is kept, not dropped.** When the oneshot
//!   send fails the value comes back, and it is handed to the next asker. This
//!   is the half that makes the person's next message survive: "the line goes
//!   nowhere" would still lose it, and losing it is the symptom they actually
//!   see.
//!
//! # EOF is reported, never decided here
//!
//! The two surfaces disagree about what end-of-input means — `ask_question`
//! cancels the flow, an approval reads it as an empty answer — so this returns
//! `Ok(None)` and lets each keep its own policy where a reader can see it.
//! Collapsing them here would change one of the two silently.

use std::sync::OnceLock;

use tokio::sync::{mpsc, oneshot};

/// Where lines come from: real stdin in production, a script under test.
///
/// The seam is here rather than above the read because the read is the thing
/// that could not be cancelled — [`crate::question::QuestionIo`] and
/// [`crate::interactive::AskUserIo`] were already injectable and the hazard
/// lived underneath both of them (#4219).
pub trait LineSource: Send + 'static {
    /// Block until a line arrives. `Ok(None)` is end of input.
    fn read_line(&mut self) -> Result<Option<String>, String>;
}

/// The process's real standard input.
pub struct Stdin;

impl LineSource for Stdin {
    fn read_line(&mut self) -> Result<Option<String>, String> {
        use std::io::BufRead as _;
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            Ok(0) => Ok(None),
            Ok(_) => Ok(Some(line)),
            Err(e) => Err(e.to_string()),
        }
    }
}

type Answer = Result<Option<String>, String>;

/// A handle to the one reader. Cheap to clone; every clone asks the same task.
#[derive(Clone)]
pub struct StdinLines {
    ask: mpsc::Sender<oneshot::Sender<Answer>>,
}

impl StdinLines {
    /// Start a reader over `source`.
    ///
    /// The task lives as long as any handle does. It is not a leak to leave it
    /// running: it holds no blocking thread while nobody is asking, which is
    /// the property the direct-`spawn_blocking` shape could not offer.
    pub fn new<S: LineSource>(mut source: S) -> Self {
        let (ask, mut asked) = mpsc::channel::<oneshot::Sender<Answer>>(16);
        tokio::spawn(async move {
            // A line read for an asker that has since timed out. Held for the
            // next asker rather than dropped — see the module doc.
            let mut held: Option<Answer> = None;
            while let Some(reply) = asked.recv().await {
                if let Some(answer) = held.take() {
                    if let Err(unsent) = reply.send(answer) {
                        held = Some(unsent);
                    }
                    continue;
                }
                // `source` moves in and back out so one owner reads it, which
                // is what keeps a second read from ever starting concurrently.
                let (answer, returned) = match tokio::task::spawn_blocking(move || {
                    let answer = source.read_line();
                    (answer, source)
                })
                .await
                {
                    Ok((answer, source)) => (answer, Some(source)),
                    Err(e) => (Err(e.to_string()), None),
                };
                let Some(returned) = returned else {
                    // The blocking task panicked or was cancelled with the
                    // runtime. There is no source to read from any more, so
                    // answer this asker and stop rather than spin.
                    let _ = reply.send(answer);
                    return;
                };
                source = returned;
                if let Err(unsent) = reply.send(answer) {
                    held = Some(unsent);
                }
            }
        });
        Self { ask }
    }

    /// Ask for the next line.
    ///
    /// Dropping the returned future — which is what a TTL timeout does — is
    /// safe: the read in flight finishes on its own and its line goes to
    /// whoever asks next.
    pub async fn next_line(&self) -> Answer {
        let (reply, answer) = oneshot::channel();
        self.ask
            .send(reply)
            .await
            .map_err(|_| "input reader stopped".to_string())?;
        answer
            .await
            .map_err(|_| "input reader stopped".to_string())?
    }
}

/// The process-wide reader over real stdin.
///
/// A singleton because stdin is one: the bug this module fixes is what happens
/// when several callers each treat it as private. Started on first use, so a
/// run that never prompts never spawns it.
pub fn stdin_lines() -> &'static StdinLines {
    static LINES: OnceLock<StdinLines> = OnceLock::new();
    LINES.get_or_init(|| StdinLines::new(Stdin))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc as std_mpsc;
    use std::time::Duration;

    use super::*;

    /// A source the test feeds by hand, so "the read is still parked" is a
    /// state the test can hold rather than a race it has to win.
    struct Scripted(std_mpsc::Receiver<Option<String>>);

    impl LineSource for Scripted {
        fn read_line(&mut self) -> Result<Option<String>, String> {
            self.0.recv().map_err(|_| "closed".to_string())
        }
    }

    fn scripted() -> (StdinLines, std_mpsc::Sender<Option<String>>) {
        let (tx, rx) = std_mpsc::channel();
        (StdinLines::new(Scripted(rx)), tx)
    }

    /// **The witness (#4219).** A prompt that times out does not swallow the
    /// line the person types next — the next consumer reads it.
    ///
    /// This is the exact sequence from the issue: a prompt starts, its TTL
    /// fires while the read is still parked, the person types, and the line
    /// must reach whoever asks next. Before this module the parked closure
    /// consumed it and the caller that timed out threw it away.
    #[tokio::test]
    async fn a_timed_out_prompt_hands_its_line_to_the_next_asker() {
        let (lines, feed) = scripted();

        // A prompt with a very short TTL, expiring while nothing has been
        // typed. The read is parked inside the source at this point.
        let timed_out = tokio::time::timeout(Duration::from_millis(50), lines.next_line()).await;
        assert!(timed_out.is_err(), "the prompt must actually time out");

        // Now the person types. The old shape fed this to the dead prompt.
        feed.send(Some("the next message\n".into())).unwrap();

        let next = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .expect("the next asker must not block on a line already read");
        assert_eq!(next.unwrap().as_deref(), Some("the next message\n"));
    }

    /// Several timed-out prompts in a row lose nothing and start no second
    /// read. `ask_question` issues one read per question, so a cancel
    /// mid-batch used to strand a reader at each of them.
    #[tokio::test]
    async fn a_batch_of_timeouts_still_delivers_one_line_to_the_next_asker() {
        let (lines, feed) = scripted();

        for _ in 0..3 {
            let out = tokio::time::timeout(Duration::from_millis(20), lines.next_line()).await;
            assert!(out.is_err());
        }
        feed.send(Some("survived\n".into())).unwrap();

        let next = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .expect("a held line is delivered without a new read");
        assert_eq!(next.unwrap().as_deref(), Some("survived\n"));

        // And exactly one line was consumed: the second ask has nothing held
        // and parks on the source, which the timeout proves.
        let starved = tokio::time::timeout(Duration::from_millis(50), lines.next_line()).await;
        assert!(
            starved.is_err(),
            "three timeouts must not have banked three lines"
        );
    }

    /// Ordinary use is unchanged: ask, get the line.
    #[tokio::test]
    async fn a_prompt_that_does_not_time_out_reads_its_own_line() {
        let (lines, feed) = scripted();
        feed.send(Some("yes\n".into())).unwrap();
        assert_eq!(lines.next_line().await.unwrap().as_deref(), Some("yes\n"));
    }

    /// End of input is reported as `Ok(None)`, so each surface keeps its own
    /// policy — `ask_question` cancels, an approval reads it as empty.
    #[tokio::test]
    async fn end_of_input_is_reported_rather_than_decided() {
        let (lines, feed) = scripted();
        feed.send(None).unwrap();
        assert_eq!(lines.next_line().await.unwrap(), None);
    }
}
