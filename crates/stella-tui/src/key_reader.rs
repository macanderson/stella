// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The deck's key reader. A thread reads keys from crossterm, which blocks,
//! and sends them down a channel the run loop selects on. This module also
//! owns what the reader says when it stops.
//!
//! Suppose the reader hit a crossterm error and just broke out of its loop.
//! The channel would close. The run loop would read `None` and stop too.
//! `run_deck` would return `Ok(())`. The driver would report a clean exit.
//! A terminal that sends bytes crossterm cannot parse would look like the
//! user pressing quit. So would a stdin that closed under the deck. The deck
//! opts into the kitty keyboard protocol, so a parse error is a real risk.
//!
//! So every way the reader can stop is a message. An error is sent before the
//! thread ends. A channel that closes with no error sent is reported too,
//! since only a dying thread does that. [`event`] is the one place the run
//! loop reads the lane, so it cannot take either one for a quit.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{self as crossterm_event, Event};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// One message from the reader: a terminal event, or the error that stopped
/// it. An `Err` is always the last message.
pub type KeyRead = io::Result<Event>;

/// How long one poll waits before the loop checks the shutdown flag again.
/// Long enough that an idle deck does not spin. Short enough that a quit
/// joins the thread with no visible pause.
const POLL: Duration = Duration::from_millis(50);

/// Start the reader thread. It runs until `shutdown` is set or crossterm
/// fails. A failure is sent on the channel before the thread ends.
pub fn spawn(
    shutdown: Arc<AtomicBool>,
) -> (std::thread::JoinHandle<()>, UnboundedReceiver<KeyRead>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = std::thread::spawn(move || {
        pump(
            || match crossterm_event::poll(POLL)? {
                true => crossterm_event::read().map(Some),
                false => Ok(None),
            },
            &tx,
            &shutdown,
        );
    });
    (handle, rx)
}

/// The reader's loop over any source of events. The tests drive this seam.
///
/// `next` returns `Ok(Some(event))` for an event. It returns `Ok(None)` when a
/// poll timed out. It returns `Err` when the terminal cannot be read. The
/// loop forwards events until the run loop hangs up or `shutdown` is set. On
/// an error it sends the error and ends. So the receiver always learns why
/// the lane closed.
fn pump(
    mut next: impl FnMut() -> io::Result<Option<Event>>,
    tx: &UnboundedSender<KeyRead>,
    shutdown: &AtomicBool,
) {
    while !shutdown.load(Ordering::Relaxed) {
        match next() {
            Ok(Some(event)) => {
                if tx.send(Ok(event)).is_err() {
                    break;
                }
            }
            Ok(None) => {}
            Err(error) => {
                // The receiver may be gone already. Then there is nobody to
                // tell, and nothing is lost.
                let _ = tx.send(Err(error));
                break;
            }
        }
    }
}

/// How the run loop reads one message off the lane.
///
/// `Some(Ok(event))` is an event to dispatch. `Some(Err)` is the reader
/// saying why it stopped. `None` is the reader gone with no word. The last
/// two become the error `run_deck` returns. Each names the key reader, so
/// the driver's line says the reader ended and not that the user quit.
pub fn event(read: Option<KeyRead>) -> io::Result<Event> {
    match read {
        Some(Ok(event)) => Ok(event),
        Some(Err(error)) => Err(io::Error::new(
            error.kind(),
            format!("the deck's key reader stopped: {error}"),
        )),
        None => Err(io::Error::other(
            "the deck's key reader ended without reporting why (stdin closed, or the reader thread died)",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    /// **The reader half of the witness.** A source that fails hands the
    /// channel the error, then closes it, in that order. A reader that drops
    /// the error leaves the close as all the loop can see.
    #[test]
    fn a_failing_source_sends_its_error_before_the_channel_closes() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let shutdown = AtomicBool::new(false);
        let mut reads = vec![
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unparsed escape",
            )),
            Ok(Some(key('a'))),
        ]
        .into_iter();
        pump(
            || {
                reads
                    .next_back()
                    .expect("the source is asked twice at most")
            },
            &tx,
            &shutdown,
        );
        drop(tx);

        assert!(
            matches!(rx.try_recv(), Ok(Ok(Event::Key(_)))),
            "the key before the failure is delivered"
        );
        let error = rx
            .try_recv()
            .expect("the error is the next message")
            .expect_err("an error");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(
            rx.try_recv().is_err(),
            "nothing follows the error; the lane is closed"
        );
    }

    /// The shutdown flag ends the reader with no message. That is the run
    /// loop's own quit. It is not an error.
    #[test]
    fn shutdown_ends_the_reader_quietly() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let shutdown = AtomicBool::new(true);
        pump(|| panic!("never polled once shut down"), &tx, &shutdown);
        drop(tx);
        assert!(rx.try_recv().is_err());
    }

    /// **The run-loop half of the witness.** A closed lane is an error that
    /// names the reader, never a clean `Ok`. A reported error keeps its kind
    /// and its text.
    #[tokio::test]
    async fn a_closed_lane_is_an_error_naming_the_reader_not_a_clean_exit() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<KeyRead>();
        drop(tx);
        let error = event(rx.recv().await).expect_err("a closed lane is not Ok");
        assert!(error.to_string().contains("key reader"), "{error}");

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<KeyRead>();
        tx.send(Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unparsed escape",
        )))
        .unwrap();
        let error = event(rx.recv().await).expect_err("a reported error is returned");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("unparsed escape"), "{error}");
        assert!(error.to_string().contains("key reader"), "{error}");

        assert!(matches!(event(Some(Ok(key('q')))), Ok(Event::Key(_))));
    }
}
