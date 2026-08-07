// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Bounded-console tests (#1588): the ring arithmetic as arithmetic, and the
//! writer/reader pair against real files with deliberately tiny bounds. The
//! fd plumbing (`pipe`/`dup2`) is glue over these and is exercised by every
//! supervised run; hijacking the test runner's own stdout to unit-test it
//! would be the tail wagging the dog.

use std::sync::{Arc, Mutex};

use super::*;

fn tiny() -> Geometry {
    Geometry {
        head: 16,
        cap: 64,
        start: 0,
    }
}

#[test]
fn placement_appends_until_the_cap_then_rings_over_the_tail_region() {
    let g = tiny();
    assert_eq!(g.file_offset(0), 0);
    assert_eq!(g.file_offset(63), 63);
    // Byte 64 is the first past the cap: it lands at the ring's start.
    assert_eq!(g.file_offset(64), 16);
    assert_eq!(g.file_offset(64 + 47), 15 + 48); // last ring slot
    assert_eq!(g.file_offset(64 + 48), 16); // and around again
}

#[test]
fn the_head_always_survives_and_the_ring_survives_until_lapped() {
    let g = tiny();
    let total = 200; // well past one lap
    for n in 0..16 {
        assert!(g.survives(n, total), "head byte {n} must survive forever");
    }
    // The newest ring-width of bytes survives…
    for n in (total - 48)..total {
        assert!(g.survives(n, total), "recent byte {n} must survive");
    }
    // …and the middle is gone.
    assert!(!g.survives(16, total));
    assert!(!g.survives(total - 49, total));
}

#[test]
fn writes_split_at_the_cap_and_at_the_wrap() {
    let g = tiny();
    // A write straddling the first fill: append to the cap, then ring.
    assert_eq!(g.segments(60, 8), vec![(60, 0, 4), (16, 4, 4)]);
    // A write straddling the ring's end wraps to the ring's start.
    let near_wrap = 64 + 46; // lands at offset 62
    assert_eq!(g.segments(near_wrap, 4), vec![(62, 0, 2), (16, 2, 2)]);
}

/// A writer over `sidecar` with the tiny geometry, seeded through the index
/// exactly as a resumed attach would find it — which also proves geometry is
/// adopted from the file's history rather than this process's environment.
fn tiny_writer(sidecar: &Path, stream: Stream) -> BoundedWriter {
    let seed = serde_json::to_string(&IndexRecord::Geometry {
        s: stream,
        g: tiny(),
    })
    .unwrap();
    let path = sidecar.join(INDEX);
    let mut existing = std::fs::read_to_string(&path).unwrap_or_default();
    existing.push_str(&seed);
    existing.push('\n');
    std::fs::write(&path, existing).unwrap();

    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(console_of(sidecar, stream))
        .unwrap();
    let index = Arc::new(Mutex::new(Index::open(sidecar)));
    let shared = Arc::new(SharedGeometry::default());
    BoundedWriter::attach(file, stream, index, shared).unwrap()
}

/// The #1588 cap witness: a stream that exceeds its bound keeps its first
/// bytes and its newest bytes, and the file itself never grows past the cap.
/// On `main` the console is an unbounded fd redirect: the file grows without
/// limit and this test has nothing to hold it to.
#[test]
fn an_overflowing_console_keeps_its_head_and_tail_within_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = tiny_writer(dir.path(), Stream::Stdout);

    // 5 chunks of 40 bytes = 200 total against a 64-byte cap.
    for i in 0..5u8 {
        let chunk: Vec<u8> = (0..40u8).map(|j| b'A' + i + (j % 3)).collect();
        writer.write_chunk(&chunk);
    }
    writer.close();

    let file_len = std::fs::metadata(dir.path().join(supervised::STDOUT_LOG))
        .unwrap()
        .len();
    assert!(
        file_len <= 64,
        "the console must never exceed its cap: {file_len} bytes"
    );

    let mut out = Vec::new();
    let mut err = Vec::new();
    assert!(
        replay_logs(dir.path(), 9999, &mut out, &mut err).unwrap(),
        "an indexed console replays through the index"
    );
    let expected_head: Vec<u8> = (0..16u8).map(|j| b'A' + (j % 3)).collect();
    assert!(
        out.starts_with(&expected_head),
        "the head must survive an overflow: {:?}",
        &out[..16.min(out.len())]
    );
    // The generator's last byte: i=4, j=39, and 39 % 3 == 0.
    let expected_last: u8 = b'A' + 4;
    assert_eq!(
        out.last().copied(),
        Some(expected_last),
        "the newest byte must survive an overflow"
    );
    assert!(
        out.len() < 200,
        "an overflowed console cannot replay bytes the ring reclaimed"
    );
}

/// What the recorder accumulates: each contiguous burst, tagged by stream.
type RecordedBursts = Arc<Mutex<Vec<(Stream, Vec<u8>)>>>;

/// Two sinks that share one recorder, so a test can observe the ORDER writes
/// reached the pair — the thing `daemon logs` could never reconstruct before
/// the index existed.
#[derive(Clone, Default)]
struct Recorder(RecordedBursts);

struct RecorderSink(Recorder, Stream);

impl Write for RecorderSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut log = self.0.0.lock().unwrap();
        if let Some((last_stream, bytes)) = log.last_mut()
            && *last_stream == self.1
        {
            bytes.extend_from_slice(buf);
        } else {
            log.push((self.1, buf.to_vec()));
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The #1588 ordering witness: `daemon logs` replays the two streams in the
/// order they were written, not as two monolithic blocks. On `main` the logs
/// command prints the full stdout tail and then the full stderr tail — the
/// module doc there says plainly it cannot do better, and this test would
/// observe exactly that two-block shape.
#[test]
fn logs_replay_interleaves_the_two_streams_chronologically() {
    let dir = tempfile::tempdir().unwrap();
    let mut out_writer = tiny_writer(dir.path(), Stream::Stdout);
    let mut err_writer = tiny_writer(dir.path(), Stream::Stderr);

    out_writer.write_chunk(b"one\n");
    err_writer.write_chunk(b"two\n");
    out_writer.write_chunk(b"three\n");
    err_writer.write_chunk(b"four\n");
    out_writer.close();
    err_writer.close();

    let recorder = Recorder::default();
    let mut out = RecorderSink(recorder.clone(), Stream::Stdout);
    let mut err = RecorderSink(recorder.clone(), Stream::Stderr);
    assert!(replay_logs(dir.path(), 9999, &mut out, &mut err).unwrap());

    let log = recorder.0.lock().unwrap();
    let sequence: Vec<(Stream, String)> = log
        .iter()
        .map(|(s, b)| (*s, String::from_utf8_lossy(b).to_string()))
        .collect();
    assert_eq!(
        sequence,
        vec![
            (Stream::Stdout, "one\n".to_string()),
            (Stream::Stderr, "two\n".to_string()),
            (Stream::Stdout, "three\n".to_string()),
            (Stream::Stderr, "four\n".to_string()),
        ],
        "the index is the chronology the raw files never had"
    );
}

/// A live follower keeps relaying byte-identically PAST the cap — the exact
/// place a raw byte-offset `Tail` stalls forever (the writer stops moving
/// the end of the file once the ring engages).
#[test]
fn a_live_follower_survives_the_ring_wrapping_under_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = tiny_writer(dir.path(), Stream::Stdout);
    // The follower opens both consoles; this run's stderr just never spoke.
    std::fs::write(dir.path().join(supervised::STDERR_LOG), b"").unwrap();
    let mut follower = Follower::open(dir.path()).unwrap();
    let mut out = Vec::new();
    let mut err = Vec::new();

    let mut sent = Vec::new();
    for i in 0..10u8 {
        let chunk: Vec<u8> = (0..30u8).map(|j| b'a' + ((i + j) % 26)).collect();
        writer.write_chunk(&chunk);
        sent.extend_from_slice(&chunk);
        follower.pump(&mut out, &mut err).unwrap();
    }

    assert_eq!(
        out, sent,
        "a follower that keeps up must relay every byte, wraps included"
    );
    assert!(err.is_empty());
}

/// The `-n` trim keeps each stream's NEWEST lines after the interleaved
/// replay, matching what the raw tails did.
#[test]
fn logs_replay_honours_the_line_budget_per_stream() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = tiny_writer(dir.path(), Stream::Stdout);
    writer.write_chunk(b"1\n2\n3\n4\n");
    writer.close();

    let mut out = Vec::new();
    let mut err = Vec::new();
    assert!(replay_logs(dir.path(), 2, &mut out, &mut err).unwrap());
    assert_eq!(String::from_utf8_lossy(&out), "3\n4\n");
}

/// Pre-index sessions replay exactly as before: no index, no change.
#[test]
fn a_session_without_an_index_falls_back_to_raw_tails() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(supervised::STDOUT_LOG), b"legacy\n").unwrap();
    let mut out = Vec::new();
    let mut err = Vec::new();
    assert!(
        !replay_logs(dir.path(), 40, &mut out, &mut err).unwrap(),
        "no index means the caller keeps the legacy path"
    );

    // And the follower's raw sweep relays the legacy console verbatim.
    std::fs::write(dir.path().join(supervised::STDERR_LOG), b"").unwrap();
    let mut follower = Follower::open(dir.path()).unwrap();
    follower.pump(&mut out, &mut err).unwrap();
    assert_eq!(String::from_utf8_lossy(&out), "legacy\n");
}

/// The #1616 witness: a panic ends the pump instead of leaving it to die
/// unjoined with the process.
///
/// What the drain does is restore the fd — which closes the pipe's last write
/// end, so the pump reads EOF, writes everything it holds, and records its
/// `Closed` marker. That marker is the assertion, because it is the one thing
/// that cannot happen by luck: a pump that was never drained is still blocked
/// on its pipe when the process exits, and up to a buffer of output (the panic
/// message, in a real supervised child) dies with it. Asserting instead that
/// the bytes reached the file would pass with the hook disarmed — a live pump
/// in a process that keeps running flushes them on its own schedule.
///
/// The pump is interposed on a private descriptor rather than on fd 1, so the
/// test runner's own stdout is never hijacked; everything downstream of the
/// `dup2` is the production path.
#[cfg(unix)]
#[test]
fn a_panic_drains_the_console_pump() {
    use std::os::unix::io::IntoRawFd;

    let dir = tempfile::tempdir().unwrap();
    let sidecar = dir.path();
    let console = sidecar.join(supervised::STDOUT_LOG);
    // A descriptor naming the console file — what the OS redirect hands a
    // supervised child, and what `pump_fd` expects to interpose on.
    let target_fd = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&console)
        .unwrap()
        .into_raw_fd();
    let stream = pump_fd(
        console.clone(),
        target_fd,
        Stream::Stdout,
        Arc::new(Mutex::new(Index::open(sidecar))),
        Arc::new(SharedGeometry::default()),
    )
    .unwrap();
    let guard = ConsoleGuard {
        pumps: Arc::new(Drainable {
            streams: Mutex::new(vec![stream]),
        }),
    };

    arm_panic_drain(&guard);
    let last_words = b"thread 'main' panicked: the supervised child died\n";
    // SAFETY: `target_fd` is this test's own descriptor, now the pipe's write
    // end, and the buffer outlives the call.
    unsafe {
        libc::write(target_fd, last_words.as_ptr().cast(), last_words.len());
    }

    let panicked = std::panic::catch_unwind(|| panic!("the supervised child died"));
    assert!(panicked.is_err());
    // Restore the hook this test installed process-wide before asserting, so
    // a failure below cannot leave the harness carrying it.
    let _ = std::panic::take_hook();

    let index = std::fs::read_to_string(sidecar.join(INDEX)).unwrap_or_default();
    let closed = index
        .lines()
        .filter_map(|line| serde_json::from_str::<IndexRecord>(line).ok())
        .any(|record| matches!(record, IndexRecord::Closed { s, .. } if s == Stream::Stdout));
    assert!(
        closed,
        "the panic hook must drain the pump: no Closed marker means it was \
         still blocked on the pipe when the panic unwound\n{index}"
    );
    assert!(
        String::from_utf8_lossy(&std::fs::read(&console).unwrap()).contains("the supervised child"),
        "and everything the pipe held must have reached the console file"
    );

    // SAFETY: the descriptor is this test's own and no longer used. The drain
    // restored it over the pipe, so this closes the console file, not the pump.
    unsafe {
        libc::close(target_fd);
    }
}

/// The other half of the drain's ordering contract (#1616): when the drain is
/// reached from a pump thread — that pump panicking, so the hook runs on its
/// stack — the fd is still restored, but the pump is NOT joined. A join there
/// is a thread waiting on itself, which hangs the process forever rather than
/// losing a message.
///
/// The stand-in pump never finishes, so a regression would block; the drain
/// therefore runs on its own thread and is awaited with a timeout, making the
/// failure an assertion rather than a wedged test binary.
#[cfg(unix)]
#[test]
fn a_drain_from_the_pump_thread_restores_the_fd_without_joining_itself() {
    use std::os::unix::io::IntoRawFd;

    let dir = tempfile::tempdir().unwrap();
    let console = dir.path().join(supervised::STDOUT_LOG);
    let open_console = || {
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&console)
            .unwrap()
            .into_raw_fd()
    };
    let saved_fd = open_console();
    let target_fd = open_console();

    let (release, blocked) = std::sync::mpsc::channel::<()>();
    let pump = std::thread::spawn(move || {
        let _ = blocked.recv();
        0u64
    });

    let (done, finished) = std::sync::mpsc::channel();
    let drainer = std::thread::spawn(move || {
        // The stream names THIS thread as its pump, which is what a pump
        // panicking inside the hook looks like from the drain's side.
        let drainable = Drainable {
            streams: Mutex::new(vec![PumpedStream {
                saved_fd,
                target_fd,
                pump: Some(pump),
                pump_thread: std::thread::current().id(),
            }]),
        };
        drainable.drain();
        let _ = done.send(());
    });

    finished
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("a drain reached from the pump thread must not join that pump");
    drop(release);
    drainer.join().unwrap();

    // SAFETY: the drain closed `saved_fd` after copying it over `target_fd`;
    // this closes the one surviving descriptor, which is this test's own.
    unsafe {
        libc::close(target_fd);
    }
}
