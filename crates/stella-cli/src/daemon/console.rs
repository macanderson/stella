// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Bounded, ordered supervised consoles (#1588).
//!
//! #1552 shipped the console as two fd-level redirects: the OS wrote every
//! byte of the child's stdout and stderr to two sidecar files. Two known
//! defects shipped with that: nothing bounded either file (a `stella
//! monitor` loop writes until the disk says no), and `stella daemon logs`
//! could not interleave the streams chronologically, because two raw byte
//! files carry no ordering information between them.
//!
//! # The shape of the fix
//!
//! A supervised child interposes itself between its own output and the
//! console files ([`install_bounded`]): each stream's fd becomes a pipe, and
//! a pump thread copies the pipe into the file. That single writer is what
//! makes both fixes possible — it can enforce a byte budget, and it can note
//! what it wrote where, and when.
//!
//! - **The bound** is head + ring: the first [`Geometry::head`] bytes of a
//!   stream are preserved verbatim forever, and the rest of the file — up to
//!   [`Geometry::cap`] — is a ring the newest bytes overwrite in place. The
//!   two things people actually read survive: how the run started, and what
//!   it was doing when they looked. The file NEVER exceeds `cap`, live or
//!   after; overflow costs the middle, marked with an elision note at read
//!   time. `STELLA_CONSOLE_CAP_BYTES` tunes the per-stream cap.
//! - **The order** is a shared index (`console-index.jsonl`, 0600 like the
//!   consoles): one record per pump write, naming the stream, the cumulative
//!   span, the file offset it landed at, and a wall-clock stamp. Readers —
//!   the launching parent's follow loop, `daemon attach`, `daemon logs` —
//!   follow the index instead of raw byte offsets, which is also what makes
//!   them safe against the ring wrapping mid-read (a raw `Tail` would stall
//!   at the cap forever, or silently replay overwritten bytes).
//!
//! The console files themselves stay byte-verbatim per stream and are never
//! merged: a `--output-format json` run's stdout must reach the `jq` on the
//! other side of the supervisor byte-identical, so the index is a third,
//! separate file and the prompt-free consoles keep their contract.
//!
//! # What the pump honestly costs
//!
//! The OS used to do the writing; now a thread does. A child killed with
//! `SIGKILL` — or panicking past the drain — can lose up to one pipe buffer
//! (~64 KiB) of its very last output, where the fd redirect lost nothing.
//! That is the price of a bound enforced while the run is alive, and it is
//! paid at the exact moment (a violent death) the tail was already the least
//! reliable part of the story. [`ConsoleGuard::drain`] closes the gap for
//! every ordinary exit by restoring the raw fds and joining the pumps.
//!
//! Sessions recorded before the index existed replay exactly as before:
//! every reader here falls back to raw tailing when there is no index.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use stella_store::supervised;

/// The index's name in the sidecar, beside the two consoles it orders.
pub(crate) const INDEX: &str = "console-index.jsonl";

/// Per-stream byte budget when `STELLA_CONSOLE_CAP_BYTES` says nothing.
/// Sized for a postmortem, not an archive: 8 MiB holds the head megabyte and
/// several thousand trailing events of the highest-volume format
/// (`stream-json`).
const DEFAULT_CAP: u64 = 8 * 1024 * 1024;

/// The floor under a configured cap. Below this the head+ring split stops
/// meaning anything (a single stream-json line can run to kilobytes).
const MIN_CAP: u64 = 64 * 1024;

/// Rewrite the index once it grows past this: it holds records for bytes the
/// ring has already overwritten, which no reader can use.
const INDEX_COMPACT_BYTES: u64 = 1024 * 1024;

/// Which console a byte belongs to. Serialized as `"o"`/`"e"` in the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) enum Stream {
    #[serde(rename = "o")]
    Stdout,
    #[serde(rename = "e")]
    Stderr,
}

/// One stream's bound: `head` bytes preserved, ring over `[head, cap)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Geometry {
    pub(crate) head: u64,
    pub(crate) cap: u64,
    /// Bytes already in the file when the pump attached — output written
    /// before `main` reached the install point (env announcements, a very
    /// early panic). They sit at the front of the head region and have no
    /// index records; readers treat them as head bytes, which they are.
    pub(crate) start: u64,
}

impl Geometry {
    /// The per-stream geometry this process would write with, honouring
    /// `STELLA_CONSOLE_CAP_BYTES` (clamped to [`MIN_CAP`]).
    fn from_env(start: u64) -> Self {
        let cap = std::env::var("STELLA_CONSOLE_CAP_BYTES")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map_or(DEFAULT_CAP, |cap| cap.max(MIN_CAP));
        Self {
            // An eighth of the budget answers "how did this run start";
            // the rest is the tail people attach for.
            head: cap / 8,
            cap,
            start,
        }
    }

    /// Ring width. At least 1 by construction (`head` is `cap / 8`).
    fn ring(&self) -> u64 {
        self.cap - self.head
    }

    /// Where cumulative byte `n` lands in the file: itself until the file
    /// first fills, then ringing over `[head, cap)`.
    pub(crate) fn file_offset(&self, n: u64) -> u64 {
        if n < self.cap {
            n
        } else {
            self.head + (n - self.head) % self.ring()
        }
    }

    /// Whether cumulative byte `n` is still readable once `total` bytes have
    /// been written: head bytes always, ring bytes until lapped.
    pub(crate) fn survives(&self, n: u64, total: u64) -> bool {
        n < self.head || n >= total.saturating_sub(self.ring()).max(self.head)
    }

    /// Split a write at cumulative `start` of `len` bytes into contiguous
    /// file segments — one cut where the append region meets the cap, and
    /// one wherever the ring wraps.
    fn segments(&self, start: u64, len: u64) -> Vec<(u64, u64, u64)> {
        let mut out = Vec::new();
        let mut n = start;
        let end = start + len;
        while n < end {
            let offset = self.file_offset(n);
            // Every placement is contiguous up to the cap — the append
            // region ends there and the ring wraps there.
            let take = (self.cap - offset).min(end - n);
            out.push((offset, n - start, take));
            n += take;
        }
        out
    }
}

/// One index line. `Geometry` lines declare a stream's bound; `Write` lines
/// place a span of cumulative bytes at a file offset; `Closed` marks the
/// pump's orderly exit, after which any further bytes are raw appends by the
/// restored fd (readers finish with a raw sweep from `total`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IndexRecord {
    Geometry {
        s: Stream,
        g: Geometry,
    },
    Write {
        s: Stream,
        c: u64,
        n: u64,
        t_ms: u64,
    },
    Closed {
        s: Stream,
        total: u64,
    },
}

/// The shared, mutex-guarded index the two pump threads append to.
///
/// Compaction happens here, under the same lock as every append: once the
/// file outgrows [`INDEX_COMPACT_BYTES`], records whose bytes the ring has
/// already overwritten are dropped — they describe bytes no reader can
/// fetch — and the geometry lines are re-emitted first so a reader arriving
/// mid-life still learns the bound before any placement.
struct Index {
    path: PathBuf,
    entries: Vec<IndexRecord>,
    written: u64,
}

impl Index {
    /// Open the sidecar's index, loading whatever an earlier life wrote so
    /// compaction preserves history instead of wiping it.
    fn open(sidecar: &Path) -> Self {
        let path = sidecar.join(INDEX);
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        Self {
            entries: existing
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect(),
            written: existing.len() as u64,
            path,
        }
    }

    /// The last geometry a stream declared, and its cumulative high-water
    /// mark — what a re-attaching writer continues from.
    fn last_state(&self, stream: Stream) -> Option<(Geometry, u64)> {
        let geometry = self.entries.iter().rev().find_map(|record| match record {
            IndexRecord::Geometry { s, g } if *s == stream => Some(*g),
            _ => None,
        })?;
        let total = self
            .entries
            .iter()
            .rev()
            .find_map(|record| match record {
                IndexRecord::Write { s, c, n, .. } if *s == stream => Some(c + n),
                IndexRecord::Closed { s, total } if *s == stream => Some(*total),
                _ => None,
            })
            .unwrap_or(geometry.start);
        Some((geometry, total))
    }

    fn append(&mut self, record: IndexRecord, geometry: impl Fn(Stream) -> Option<Geometry>) {
        let line = match serde_json::to_string(&record) {
            Ok(line) => line,
            Err(_) => return,
        };
        self.entries.push(record);
        self.written += line.len() as u64 + 1;
        if self.written > INDEX_COMPACT_BYTES {
            self.compact(&geometry);
            return;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        if let Ok(mut file) = options.open(&self.path) {
            let _ = writeln!(file, "{line}");
        }
    }

    /// Rewrite the file keeping geometry, close markers, and only the write
    /// records whose bytes still survive their stream's ring.
    fn compact(&mut self, geometry: &impl Fn(Stream) -> Option<Geometry>) {
        let total_of = |records: &[IndexRecord], stream: Stream| {
            records
                .iter()
                .rev()
                .find_map(|r| match r {
                    IndexRecord::Write { s, c, n, .. } if *s == stream => Some(c + n),
                    _ => None,
                })
                .unwrap_or(0)
        };
        let out_total = total_of(&self.entries, Stream::Stdout);
        let err_total = total_of(&self.entries, Stream::Stderr);
        self.entries.retain(|record| match record {
            IndexRecord::Write { s, c, n, .. } => {
                let total = if *s == Stream::Stdout {
                    out_total
                } else {
                    err_total
                };
                // ANY surviving byte keeps the record: a span straddling the
                // head boundary keeps its preserved prefix readable even
                // after the ring reclaims its tail. Spans are contiguous, so
                // first-or-last surviving covers every case.
                geometry(*s).is_none_or(|g| g.survives(*c, total) || g.survives(c + n - 1, total))
            }
            _ => true,
        });
        let mut body = String::new();
        for record in &self.entries {
            if let Ok(line) = serde_json::to_string(record) {
                body.push_str(&line);
                body.push('\n');
            }
        }
        self.written = body.len() as u64;
        let _ = stella_store::write_sensitive_file_atomic(&self.path, body.as_bytes());
    }
}

/// The bounded, indexed writer for ONE stream — everything the pump thread
/// does per chunk, with the fd plumbing factored out so tests can drive it
/// against ordinary files.
struct BoundedWriter {
    file: std::fs::File,
    stream: Stream,
    geometry: Geometry,
    /// Cumulative bytes ever written on this stream, `start` included.
    total: u64,
    index: Arc<Mutex<Index>>,
    shared: Arc<SharedGeometry>,
}

/// Both streams' geometry, shared with the index so compaction can ask
/// "does this record's byte still survive?" without a circular borrow.
#[derive(Default)]
struct SharedGeometry {
    out: Mutex<Option<Geometry>>,
    err: Mutex<Option<Geometry>>,
}

impl SharedGeometry {
    fn get(&self, stream: Stream) -> Option<Geometry> {
        match stream {
            Stream::Stdout => *self.out.lock().unwrap_or_else(|p| p.into_inner()),
            Stream::Stderr => *self.err.lock().unwrap_or_else(|p| p.into_inner()),
        }
    }

    fn set(&self, stream: Stream, geometry: Geometry) {
        match stream {
            Stream::Stdout => {
                *self.out.lock().unwrap_or_else(|p| p.into_inner()) = Some(geometry);
            }
            Stream::Stderr => {
                *self.err.lock().unwrap_or_else(|p| p.into_inner()) = Some(geometry);
            }
        }
    }
}

impl BoundedWriter {
    fn attach(
        file: std::fs::File,
        stream: Stream,
        index: Arc<Mutex<Index>>,
        shared: Arc<SharedGeometry>,
    ) -> std::io::Result<Self> {
        // A resumed session (#1586) re-attaches to a console an earlier life
        // already wrote. The file's layout is physical: the earlier
        // geometry stands whatever this process's env says, and the
        // cumulative total continues from the index rather than the file
        // length — after a wrap the two stopped agreeing.
        let file_len = file.metadata()?.len();
        let prior = index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .last_state(stream);
        let (geometry, total) = match prior {
            Some((geometry, total)) => (geometry, total.max(file_len.min(geometry.cap))),
            None => (Geometry::from_env(file_len), file_len),
        };
        shared.set(stream, geometry);
        let mut writer = Self {
            file,
            stream,
            geometry,
            total,
            index,
            shared,
        };
        writer.record(IndexRecord::Geometry {
            s: stream,
            g: geometry,
        });
        Ok(writer)
    }

    fn record(&mut self, record: IndexRecord) {
        let shared = self.shared.clone();
        self.index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .append(record, move |stream| shared.get(stream));
    }

    /// Write one chunk at the ring-mapped offsets and index it.
    fn write_chunk(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        for (offset, within, len) in self.geometry.segments(self.total, chunk.len() as u64) {
            let piece = &chunk[within as usize..(within + len) as usize];
            if self.file.seek(SeekFrom::Start(offset)).is_err() {
                return;
            }
            if self.file.write_all(piece).is_err() {
                return;
            }
        }
        let record = IndexRecord::Write {
            s: self.stream,
            c: self.total,
            n: chunk.len() as u64,
            t_ms: wall_ms(),
        };
        self.total += chunk.len() as u64;
        self.record(record);
    }

    /// The pump's orderly end: mark where raw appends resume.
    fn close(&mut self) {
        let _ = self.file.flush();
        self.record(IndexRecord::Closed {
            s: self.stream,
            total: self.total,
        });
    }

    /// The file position raw appends must continue from after the fds are
    /// restored — the ring cursor, not the file's end.
    fn resume_offset(&self) -> u64 {
        self.geometry.file_offset(self.total)
    }
}

fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The live pumps, held by `main` for the life of the supervised child.
pub(crate) struct ConsoleGuard {
    #[cfg(unix)]
    streams: Vec<PumpedStream>,
}

#[cfg(unix)]
struct PumpedStream {
    /// The console file's original descriptor, to restore with `dup2`.
    saved_fd: i32,
    /// The fd the pipe replaced (1 or 2).
    target_fd: i32,
    pump: Option<std::thread::JoinHandle<u64>>,
}

impl ConsoleGuard {
    /// Restore both fds and drain both pumps — the last thing a supervised
    /// child does, after its final print. Restoring first closes the pipes'
    /// only write ends, so each pump sees EOF, writes its `Closed` marker,
    /// and exits; anything printed after this lands raw at the ring cursor,
    /// which the marker tells readers to sweep.
    pub(crate) fn drain(self) {
        #[cfg(unix)]
        for mut stream in self.streams {
            let Some(pump) = stream.pump.take() else {
                continue;
            };
            // SAFETY: both fds are alive and owned by this process; `dup2`
            // replacing the pipe write end with the saved console fd is the
            // documented way to un-interpose, and the pump still holds its
            // read end so no descriptor is closed twice.
            unsafe {
                libc::dup2(stream.saved_fd, stream.target_fd);
            }
            if let Ok(resume_at) = pump.join() {
                // Post-drain appends continue at the ring cursor rather than
                // the file end, or a wrapped console would grow past its cap
                // on its very last lines.
                // SAFETY: `saved_fd` is the console file, still open; seeking
                // it is ordinary file I/O.
                unsafe {
                    libc::lseek(
                        stream.target_fd,
                        i64::try_from(resume_at).unwrap_or(0),
                        libc::SEEK_SET,
                    );
                }
            }
            // SAFETY: the duplicate made at install time is no longer needed
            // once dup2 has copied it back over the target.
            unsafe {
                libc::close(stream.saved_fd);
            }
        }
    }
}

/// Interpose the bounded pumps on this process's stdout and stderr.
///
/// Called once, in the supervised child, before its first byte of ordinary
/// output. `None` on any failure: the console then stays a raw fd redirect,
/// which is exactly the pre-#1588 behaviour — unbounded, but never broken.
pub(crate) fn install_bounded(sidecar: &Path) -> Option<ConsoleGuard> {
    #[cfg(not(unix))]
    {
        let _ = sidecar;
        None
    }
    #[cfg(unix)]
    {
        let index = Arc::new(Mutex::new(Index::open(sidecar)));
        let shared = Arc::new(SharedGeometry::default());
        let mut streams = Vec::new();
        for (target_fd, stream, name) in [
            (1, Stream::Stdout, supervised::STDOUT_LOG),
            (2, Stream::Stderr, supervised::STDERR_LOG),
        ] {
            let stream = pump_fd(
                sidecar.join(name),
                target_fd,
                stream,
                index.clone(),
                shared.clone(),
            )?;
            streams.push(stream);
        }
        Some(ConsoleGuard { streams })
    }
}

/// Replace `target_fd` with a pipe and spawn the pump that empties it into
/// the bounded console file. Answers `None` on any syscall failure, leaving
/// the fd exactly as it was.
#[cfg(unix)]
fn pump_fd(
    console: PathBuf,
    target_fd: i32,
    stream: Stream,
    index: Arc<Mutex<Index>>,
    shared: Arc<SharedGeometry>,
) -> Option<PumpedStream> {
    use std::os::unix::io::FromRawFd;

    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&console).ok()?;
    let mut writer = BoundedWriter::attach(file, stream, index, shared).ok()?;

    let mut fds = [0i32; 2];
    // SAFETY: `pipe` writes two fresh descriptors into the array; both are
    // owned below (read end by the pump's File, write end by dup2/close).
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return None;
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);
    // SAFETY: `target_fd` currently names the console file the OS redirect
    // opened; duplicating it keeps that file reachable for the writer and
    // for the drain-time restore.
    let saved_fd = unsafe { libc::dup(target_fd) };
    if saved_fd < 0 {
        // SAFETY: both pipe ends were created above and not yet shared.
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
        return None;
    }
    // SAFETY: replacing this process's own stdout/stderr with the pipe's
    // write end, then closing the now-duplicated original write end.
    unsafe {
        if libc::dup2(write_fd, target_fd) < 0 {
            libc::close(read_fd);
            libc::close(write_fd);
            libc::close(saved_fd);
            return None;
        }
        libc::close(write_fd);
    }
    // SAFETY: `read_fd` is the pipe's read end, exclusively ours; the File
    // takes ownership and closes it when the pump exits.
    let mut pipe = unsafe { std::fs::File::from_raw_fd(read_fd) };
    let pump = std::thread::Builder::new()
        .name(format!("console-pump-{target_fd}"))
        .spawn(move || {
            let mut buf = [0u8; 64 * 1024];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => writer.write_chunk(&buf[..read]),
                }
            }
            writer.close();
            writer.resume_offset()
        })
        .ok()?;

    Some(PumpedStream {
        saved_fd,
        target_fd,
        pump: Some(pump),
    })
}

// ── the read side ─────────────────────────────────────────────────────────

/// A reader's view of one stream: its geometry, how much of it it has
/// relayed, and the raw fallback it uses until (and after) the index speaks.
struct FollowedStream {
    tail: super::Tail,
    geometry: Option<Geometry>,
    /// Cumulative bytes relayed. While raw, this equals the tail's offset —
    /// the head region is append-pure, so the two coordinate systems agree
    /// exactly until the first wrap, which only the indexed path can cross.
    consumed: u64,
    /// Set by a `Closed` record: from here the writer is raw again.
    closed_at: Option<u64>,
    noted_elision: bool,
}

/// Follows both consoles in true order via the index, falling back to raw
/// tailing for pre-index sessions. One `Follower` replaces the PAIR of raw
/// tails the follow/attach loops used, because ordering is a property of the
/// pair.
pub(crate) struct Follower {
    sidecar: PathBuf,
    index_offset: u64,
    out: FollowedStream,
    err: FollowedStream,
}

impl Follower {
    pub(crate) fn open(sidecar: &Path) -> Result<Self, String> {
        Ok(Self {
            sidecar: sidecar.to_path_buf(),
            index_offset: 0,
            out: FollowedStream::open(sidecar.join(supervised::STDOUT_LOG))?,
            err: FollowedStream::open(sidecar.join(supervised::STDERR_LOG))?,
        })
    }

    /// Relay what is new, in write order, and answer how many bytes moved.
    ///
    /// One call is bounded — the raw path moves at most one [`super::Tail`]
    /// chunk per stream — so a caller that must see everything loops until
    /// this answers zero, and a live loop treats nonzero as "come straight
    /// back".
    pub(crate) fn pump(
        &mut self,
        out: &mut impl Write,
        err: &mut impl Write,
    ) -> Result<usize, String> {
        let mut moved = 0;
        let batch = self.new_records()?;
        // The writer's totals as of this batch's end, per stream — what the
        // lap guard in `relay` compares against. Computed before relaying:
        // a record early in the batch may describe bytes a record later in
        // the same batch has already overwritten.
        let total_after = |stream: Stream| {
            batch
                .iter()
                .rev()
                .find_map(|record| match record {
                    IndexRecord::Write { s, c, n, .. } if *s == stream => Some(c + n),
                    IndexRecord::Closed { s, total } if *s == stream => Some(*total),
                    _ => None,
                })
                .unwrap_or(0)
        };
        let totals = (total_after(Stream::Stdout), total_after(Stream::Stderr));
        for record in batch {
            match record {
                IndexRecord::Geometry { s, g } => self.stream_mut(s).adopt(g),
                IndexRecord::Write { s, c, n, .. } => {
                    let console = self.console_path(s);
                    let total = match s {
                        Stream::Stdout => totals.0,
                        Stream::Stderr => totals.1,
                    };
                    let sink: &mut dyn Write = match s {
                        Stream::Stdout => out,
                        Stream::Stderr => err,
                    };
                    moved += self.stream_mut(s).relay(&console, c, n, total, sink)?;
                }
                IndexRecord::Closed { s, total } => {
                    self.stream_mut(s).closed_at = Some(total);
                }
            }
        }
        // Raw sweeps: streams with no geometry yet (pre-index session or a
        // child that has not reached install), and streams whose pump has
        // closed (post-drain stragglers land as raw appends at the cursor).
        moved += self.out.raw_sweep(out)?;
        moved += self.err.raw_sweep(err)?;
        Ok(moved)
    }

    fn stream_mut(&mut self, stream: Stream) -> &mut FollowedStream {
        match stream {
            Stream::Stdout => &mut self.out,
            Stream::Stderr => &mut self.err,
        }
    }

    fn console_path(&self, stream: Stream) -> PathBuf {
        self.sidecar.join(match stream {
            Stream::Stdout => supervised::STDOUT_LOG,
            Stream::Stderr => supervised::STDERR_LOG,
        })
    }

    /// Index records appended since the last call. A file shorter than the
    /// last offset means the writer compacted: re-read from the top and let
    /// per-stream `consumed` skip everything already relayed.
    fn new_records(&mut self) -> Result<Vec<IndexRecord>, String> {
        let path = self.sidecar.join(INDEX);
        let Ok(mut file) = std::fs::File::open(&path) else {
            return Ok(Vec::new());
        };
        let len = file
            .metadata()
            .map_err(|e| format!("cannot stat the console index: {e}"))?
            .len();
        if len < self.index_offset {
            self.index_offset = 0;
        }
        if file.seek(SeekFrom::Start(self.index_offset)).is_err() {
            return Ok(Vec::new());
        }
        let mut buf = String::new();
        file.read_to_string(&mut buf)
            .map_err(|e| format!("cannot read the console index: {e}"))?;
        // Only complete lines: a record mid-append parses on the next pass.
        let complete = buf.rfind('\n').map_or(0, |at| at + 1);
        self.index_offset += complete as u64;
        Ok(buf[..complete]
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect())
    }
}

impl FollowedStream {
    fn open(path: PathBuf) -> Result<Self, String> {
        Ok(Self {
            tail: super::Tail::open(&path)?,
            geometry: None,
            consumed: 0,
            closed_at: None,
            noted_elision: false,
        })
    }

    fn adopt(&mut self, geometry: Geometry) {
        self.geometry = Some(geometry);
        self.closed_at = None;
        // Whatever the raw tail already relayed is relayed; indexed records
        // below this mark are skipped rather than replayed.
        self.consumed = self.consumed.max(self.tail.offset);
    }

    /// Relay the indexed span `[c, c + n)`, skipping what raw tailing
    /// already delivered and noting — once — anything the ring took first.
    /// `writer_total` is the stream's cumulative end as of the batch this
    /// record arrived in, which is what the lap guard needs: a reader that
    /// fell a full ring behind must mark the gap, never replay newer bytes
    /// as older ones.
    fn relay(
        &mut self,
        console: &Path,
        c: u64,
        n: u64,
        writer_total: u64,
        sink: &mut dyn Write,
    ) -> Result<usize, String> {
        let Some(geometry) = self.geometry else {
            return Ok(0);
        };
        let end = c + n;
        if end <= self.consumed {
            return Ok(0);
        }
        let from = self.consumed.max(c);
        if from > self.consumed && !self.noted_elision {
            // A gap: the writer compacted records this reader never saw —
            // it lagged a full ring behind. Say so once, on stderr, never
            // inside the byte-verbatim relay.
            eprintln!(
                "[console: a lagging reader missed {} bytes]",
                from - self.consumed
            );
            self.noted_elision = true;
        }
        let mut file = std::fs::File::open(console)
            .map_err(|e| format!("cannot read {}: {e}", console.display()))?;
        let mut relayed = 0usize;
        let mut at = from;
        while at < end {
            if !geometry.survives(at, writer_total.max(end)) {
                at += 1;
                continue;
            }
            let offset = geometry.file_offset(at);
            let contiguous = (end - at).min(geometry.cap - offset);
            let mut chunk = vec![0u8; contiguous as usize];
            if file.seek(SeekFrom::Start(offset)).is_err() {
                break;
            }
            match file.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let _ = sink.write_all(&chunk[..read]);
                    relayed += read;
                    at += read as u64;
                }
            }
        }
        let _ = sink.flush();
        self.consumed = self.consumed.max(at);
        self.tail.offset = self
            .geometry
            .map_or(self.consumed, |g| g.file_offset(self.consumed));
        Ok(relayed)
    }

    /// Raw tailing, used before geometry arrives and after `Closed`.
    fn raw_sweep(&mut self, sink: &mut impl Write) -> Result<usize, String> {
        match (self.geometry, self.closed_at) {
            // Pre-index: exactly the pre-#1588 behaviour.
            (None, _) => {
                let moved = self.tail.pump(sink)?;
                self.consumed += moved as u64;
                Ok(moved)
            }
            // Post-drain: stragglers appended at the ring cursor.
            (Some(geometry), Some(total)) => {
                if self.consumed < total {
                    // Whatever the index placed but this reader has not yet
                    // relayed was handled above; from `total` on it is raw.
                    return Ok(0);
                }
                self.tail.offset = geometry.file_offset(self.consumed);
                let moved = self.tail.pump(sink)?;
                self.consumed += moved as u64;
                Ok(moved)
            }
            // Indexed and live: the index is the only truthful reader.
            (Some(_), None) => Ok(0),
        }
    }
}

/// `stella daemon logs` over a bounded console: the surviving story, in true
/// order (#1588's second defect).
///
/// When every indexed byte still survives, the replay is exact: one pass in
/// index order, each span onto its own stream, `lines` trimming each stream
/// to its newest lines. When the ring has taken the middle, each stream
/// first prints its preserved head and an elision marker (to stderr, never
/// into the stream bytes), then the surviving tail replays in true order.
/// With no index at all — a pre-#1588 session — the caller falls back to the
/// old per-stream tails, which is also the documented answer for why THOSE
/// cannot interleave.
pub(crate) fn replay_logs(
    sidecar: &Path,
    lines: usize,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<bool, String> {
    let Ok(index) = std::fs::read_to_string(sidecar.join(INDEX)) else {
        return Ok(false);
    };
    let records: Vec<IndexRecord> = index
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    if records.is_empty() {
        return Ok(false);
    }

    let mut streams: std::collections::HashMap<Stream, (Option<Geometry>, u64)> =
        std::collections::HashMap::new();
    for record in &records {
        match record {
            IndexRecord::Geometry { s, g } => {
                streams.entry(*s).or_insert((None, 0)).0 = Some(*g);
            }
            IndexRecord::Write { s, c, n, .. } => {
                streams.entry(*s).or_insert((None, 0)).1 = c + n;
            }
            IndexRecord::Closed { .. } => {}
        }
    }

    // Record-driven, in index order — the index IS the chronology. Each
    // record's span is intersected with what still survives (the head, and
    // the newest ring-width of bytes); dead middles become one elision
    // marker per stream, on stderr, never inside the replayed bytes.
    let mut ordered: Vec<(Stream, Vec<u8>)> = Vec::new();
    let mut marked: std::collections::HashMap<Stream, bool> = std::collections::HashMap::new();
    // Bytes written before the pump attached have no records; they are the
    // front of the head region and replay first, per stream.
    for (&stream, &(geometry, _total)) in &streams {
        let Some(geometry) = geometry else { continue };
        let pre_pump = geometry.start.min(geometry.head);
        if pre_pump > 0 {
            let bytes = read_span(&console_of(sidecar, stream), geometry, 0, pre_pump)?;
            ordered.push((stream, bytes));
        }
    }
    for record in &records {
        let IndexRecord::Write { s, c, n, .. } = record else {
            continue;
        };
        let Some(&(Some(geometry), total)) = streams.get(s) else {
            continue;
        };
        let end = c + n;
        let tail_start = total.saturating_sub(geometry.ring()).max(geometry.head);
        // The survivable set is [0, head) ∪ [tail_start, total); a span is
        // contiguous, so it intersects that set in at most two pieces, in
        // cumulative order.
        let head_piece = (*c, end.min(geometry.head));
        let tail_piece = ((*c).max(tail_start), end);
        for (from, to) in [head_piece, tail_piece] {
            if from >= to {
                continue;
            }
            let bytes = read_span(&console_of(sidecar, *s), geometry, from, to)?;
            ordered.push((*s, bytes));
        }
        // The span's dead middle, if any: its intersection with the
        // reclaimed region `[head, tail_start)`.
        let dead_from = (*c).max(geometry.head);
        let dead_to = end.min(tail_start);
        if dead_from < dead_to && !marked.get(s).copied().unwrap_or(false) {
            eprintln!(
                "[console: {} exceeded its {}-byte bound — the middle was reclaimed; \
                 head and newest bytes follow in order]",
                stream_name(*s),
                geometry.cap,
            );
            marked.insert(*s, true);
        }
    }

    // `-n`: each stream keeps its newest `lines` lines — computed per
    // stream, but emitted in ONE pass over the index order, because that
    // order is the point. A per-stream emit loop here would regroup the
    // replay into the whole-stdout-then-whole-stderr shape the index exists
    // to end.
    let newest = |stream: Stream| -> usize {
        ordered
            .iter()
            .filter(|(s, _)| *s == stream)
            .map(|(_, b)| bytecount_lines(b))
            .sum()
    };
    let mut skip_out = newest(Stream::Stdout).saturating_sub(lines);
    let mut skip_err = newest(Stream::Stderr).saturating_sub(lines);
    for (s, bytes) in &ordered {
        let (sink, skip): (&mut dyn Write, &mut usize) = match s {
            Stream::Stdout => (out, &mut skip_out),
            Stream::Stderr => (err, &mut skip_err),
        };
        emit_after_skipping(bytes, skip, sink);
    }
    let _ = out.flush();
    let _ = err.flush();
    Ok(true)
}

fn console_of(sidecar: &Path, stream: Stream) -> PathBuf {
    sidecar.join(match stream {
        Stream::Stdout => supervised::STDOUT_LOG,
        Stream::Stderr => supervised::STDERR_LOG,
    })
}

fn stream_name(stream: Stream) -> &'static str {
    match stream {
        Stream::Stdout => "stdout",
        Stream::Stderr => "stderr",
    }
}

/// Read cumulative `[from, end)` through the ring mapping.
fn read_span(console: &Path, geometry: Geometry, from: u64, end: u64) -> Result<Vec<u8>, String> {
    let mut file = std::fs::File::open(console)
        .map_err(|e| format!("cannot read {}: {e}", console.display()))?;
    let mut bytes = Vec::with_capacity((end - from) as usize);
    let mut at = from;
    while at < end {
        let offset = geometry.file_offset(at);
        let contiguous = (end - at).min(geometry.cap - offset);
        let mut chunk = vec![0u8; contiguous as usize];
        if file.seek(SeekFrom::Start(offset)).is_err() {
            break;
        }
        match file.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                bytes.extend_from_slice(&chunk[..read]);
                at += read as u64;
            }
        }
    }
    Ok(bytes)
}

fn bytecount_lines(bytes: &[u8]) -> usize {
    let newlines = bytes.iter().filter(|b| **b == b'\n').count();
    newlines + usize::from(bytes.last().is_some_and(|b| *b != b'\n'))
}

/// Write `bytes`, dropping the first `skip` lines across calls.
fn emit_after_skipping(bytes: &[u8], skip: &mut usize, sink: &mut dyn Write) {
    let mut rest = bytes;
    while *skip > 0 {
        match rest.iter().position(|b| *b == b'\n') {
            Some(at) => {
                rest = &rest[at + 1..];
                *skip -= 1;
            }
            None => return,
        }
    }
    let _ = sink.write_all(rest);
}

#[cfg(test)]
mod tests;
