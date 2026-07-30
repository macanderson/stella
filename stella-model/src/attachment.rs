//! Attachment → wire-part resolution shared by every provider adapter.
//!
//! One function ([`wire_parts`]) turns a user message's
//! [`Attachment`]s into dialect-neutral [`WirePart`]s, so classification,
//! payload hydration, text inlining, and graceful degradation live in ONE
//! place and each adapter only maps parts onto its own JSON shapes.
//!
//! ## The degradation contract
//!
//! An attachment a dialect cannot ingest natively (audio on Anthropic, video
//! on OpenAI, an arbitrary binary anywhere) NEVER fails the request. It
//! becomes a [`WirePart::Text`] note describing exactly what was attached and
//! why it is not visible, so the model can tell the user instead of the turn
//! erroring out. The same applies to a payload file that cannot be read —
//! the conversation replays every turn, so a hard error here would brick the
//! session permanently.
//!
//! [`DialectCaps`] is what normally keeps a part an adapter has no wire shape
//! for from ever reaching that adapter. The two are edited independently
//! though — switching a cap on is a one-line change — so adapters back the
//! caps up with [`unsupported_part_note`] on the arms their caps exclude
//! today. A half-finished caps edit then degrades like any other unsupported
//! attachment instead of aborting the process mid-turn.
//!
//! ## Size and cost
//!
//! Stella imposes no size cap of its own. Payloads are hydrated from disk at
//! request-build time and handed to the provider as base64; a payload larger
//! than the provider's own request limit surfaces as that provider's error.
//! Text-like files are inlined in full for the same reason.
//!
//! Two consequences worth knowing before adding a caller. The read is
//! `std::fs::read` — synchronous, on whatever runtime thread is building the
//! request — so a large attachment blocks a tokio worker for the length of
//! the read. And because the conversation replays every turn, a
//! `AttachmentSource::Path` attachment would be re-read AND re-base64-encoded
//! on every model call for the rest of the session; [`PATH_CACHE`] exists so
//! that transform runs once per file *version* instead — an unchanged file
//! costs a `stat` per turn, an edited one re-hydrates on its next turn. The
//! first hydration of each version still blocks the runtime thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use stella_protocol::{Attachment, AttachmentKind, AttachmentSource};

/// What a dialect can ingest natively. Anything switched off degrades to a
/// descriptive [`WirePart::Text`] note.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DialectCaps {
    pub images: bool,
    pub pdfs: bool,
    pub audio: bool,
    pub video: bool,
}

/// One dialect-neutral content part resolved from an attachment. Binary
/// payloads arrive already base64-encoded.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum WirePart {
    Image {
        media_type: String,
        base64: String,
    },
    Pdf {
        name: String,
        base64: String,
    },
    Audio {
        media_type: String,
        base64: String,
    },
    Video {
        media_type: String,
        base64: String,
    },
    /// Inlined text — a text-like file's contents, or a degrade note for
    /// anything the dialect cannot ingest.
    Text {
        text: String,
    },
}

impl WirePart {
    /// A phrase naming what this part carries, for a degrade note the model
    /// reads aloud. Media types and file names come along because they are
    /// what lets the model suggest a workable format.
    fn describe(&self) -> String {
        match self {
            WirePart::Image { media_type, .. } => format!("an image ({media_type})"),
            WirePart::Pdf { name, .. } => format!("a PDF document ({name})"),
            WirePart::Audio { media_type, .. } => format!("an audio file ({media_type})"),
            WirePart::Video { media_type, .. } => format!("a video ({media_type})"),
            WirePart::Text { .. } => "a text note".to_string(),
        }
    }
}

/// The note an adapter emits for a part its dialect has no wire shape for —
/// the belt to [`DialectCaps`]'s braces.
///
/// Reaching this means a caps const advertises a kind the adapter's mapping
/// never learned to encode, which is a wiring mistake rather than anything
/// the user did. It still degrades: the module's promise is that an
/// attachment never fails the request, and a panic here would abort the
/// process mid-turn over a config flag.
pub(crate) fn unsupported_part_note(part: &WirePart, dialect: &str) -> String {
    format!(
        "[the user attached {}, but the {dialect} cannot carry it on the wire; acknowledge \
         the attachment and suggest a provider or format that can be read]",
        part.describe()
    )
}

/// Resolve a message's attachments into wire parts for a dialect. Infallible
/// by design: unreadable payloads and unsupported kinds come back as
/// [`WirePart::Text`] notes rather than errors (see module docs).
pub(crate) fn wire_parts(attachments: &[Attachment], caps: DialectCaps) -> Vec<WirePart> {
    attachments
        .iter()
        .map(|attachment| resolve_one(attachment, caps))
        .collect()
}

/// A payload hydrated into the one wire form its kind consumes: inlined
/// (lossy UTF-8) content for text-like files, base64 for the binary kinds.
/// Hydrating straight to the consumed form is what lets [`PATH_CACHE`] pay:
/// the expensive transform (read + encode) is what gets memoized, not just
/// the raw bytes.
#[derive(Clone)]
enum HydratedBody {
    Text(String),
    Base64(String),
}

impl HydratedBody {
    /// Retained heap bytes — what [`PathCache`] budgets against. Base64 is
    /// ~4/3 of the file it came from, so this is measured on the hydrated
    /// string rather than inferred from the file length.
    fn retained_bytes(&self) -> usize {
        match self {
            HydratedBody::Text(text) => text.len(),
            HydratedBody::Base64(encoded) => encoded.len(),
        }
    }
}

/// One cached path payload, valid while the file's `(mtime, len)`
/// fingerprint holds. `last_used` is the cache's logical clock at the last
/// hit, which is what makes eviction least-recently-used rather than
/// arbitrary.
struct CachedPayload {
    modified: Option<SystemTime>,
    len: u64,
    body: HydratedBody,
    last_used: u64,
}

/// Total hydrated bytes [`PATH_CACHE`] will hold. The cache is a
/// process-wide `static` on a deck session that runs for hours, so an
/// unbudgeted one is a leak with extra steps: every distinct path ever
/// attached stayed resident for the life of the process, at ~4/3 the file's
/// size once base64-encoded, with nothing evicting and no per-entry ceiling.
/// A handful of screenshots is invisible; a session that attaches a few
/// hundred, or one video, is not.
///
/// 64 MiB holds hundreds of screenshots — the case the cache exists for —
/// while capping the worst case at a fixed, nameable number.
const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// Bounded, least-recently-used cache of hydrated
/// [`AttachmentSource::Path`] payloads. The conversation replays every turn,
/// so without it every model call re-read and re-encoded every path
/// attachment for the rest of the session — invisible for a screenshot,
/// pathological for a large payload on a long session. Keyed by path and
/// validated against the file's `(mtime, len)` on every lookup, so an edited
/// file re-hydrates on its next turn.
///
/// A miss is a re-read, never a failure, so eviction is always safe: the
/// cache is a latency optimization with no correctness role. That is what
/// lets the budget be enforced bluntly.
#[derive(Default)]
struct PathCache {
    entries: HashMap<PathBuf, CachedPayload>,
    /// Sum of `entries`' `retained_bytes`, maintained incrementally so the
    /// budget check never walks the map.
    bytes: usize,
    /// Monotonic logical clock stamped onto an entry on insert and on hit.
    tick: u64,
}

impl PathCache {
    /// Look up a fresh entry, stamping it as most-recently-used. `None` on a
    /// miss OR on a stale fingerprint — the caller re-hydrates either way.
    fn get_fresh(
        &mut self,
        path: &Path,
        modified: Option<SystemTime>,
        len: u64,
        want_text: bool,
    ) -> Option<HydratedBody> {
        let entry = self.entries.get_mut(path)?;
        // The stored form must also be the one this kind consumes — the same
        // path re-attached under a different media type recomputes.
        let fresh = entry.modified == modified
            && entry.len == len
            && matches!(
                (&entry.body, want_text),
                (HydratedBody::Text(_), true) | (HydratedBody::Base64(_), false)
            );
        if !fresh {
            return None;
        }
        self.tick += 1;
        entry.last_used = self.tick;
        Some(entry.body.clone())
    }

    /// Retain `body` for `path`, evicting least-recently-used entries until
    /// it fits. A payload larger than the whole budget is simply not
    /// retained — caching it could only be paid for by evicting everything
    /// else, and it would still be the next thing evicted.
    fn store(
        &mut self,
        path: PathBuf,
        modified: Option<SystemTime>,
        len: u64,
        body: &HydratedBody,
    ) {
        // Replacing an entry frees its old bytes first, so a re-hydrated
        // file is never double-counted against the budget.
        self.remove(&path);
        let size = body.retained_bytes();
        if size > MAX_CACHE_BYTES {
            return;
        }
        // Linear scan per eviction, which is right here: evictions are rare
        // (only when the budget is actually reached) and the map is small by
        // construction, so a heap would cost more bookkeeping than it saves.
        while self.bytes + size > MAX_CACHE_BYTES {
            let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
            else {
                // Nothing left to evict; `size <= MAX_CACHE_BYTES` above
                // guarantees the loop cannot reach here, but an empty map
                // must break rather than spin.
                return;
            };
            self.remove(&victim);
        }
        self.tick += 1;
        self.bytes += size;
        self.entries.insert(
            path,
            CachedPayload {
                modified,
                len,
                body: body.clone(),
                last_used: self.tick,
            },
        );
    }

    fn remove(&mut self, path: &Path) {
        if let Some(old) = self.entries.remove(path) {
            self.bytes = self.bytes.saturating_sub(old.body.retained_bytes());
        }
    }
}

static PATH_CACHE: OnceLock<Mutex<PathCache>> = OnceLock::new();

/// The path cache's guard. A poisoned lock is recovered rather than
/// propagated — the worst a poisoned cache can hold is a stale entry the
/// fingerprint check re-validates anyway, and this module's contract is that
/// an attachment never aborts a turn.
fn path_cache() -> std::sync::MutexGuard<'static, PathCache> {
    let cache = PATH_CACHE.get_or_init(|| Mutex::new(PathCache::default()));
    match cache.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Hydrate a path payload in the form `want_text` selects, through
/// [`PATH_CACHE`]. IO errors propagate for the caller to degrade to a note.
fn hydrate_path(path: &str, want_text: bool) -> std::io::Result<HydratedBody> {
    let meta = std::fs::metadata(path)?;
    let modified = meta.modified().ok();
    let len = meta.len();
    if let Some(body) = path_cache().get_fresh(Path::new(path), modified, len, want_text) {
        return Ok(body);
    }
    let bytes = std::fs::read(path)?;
    let body = if want_text {
        HydratedBody::Text(String::from_utf8_lossy(&bytes).into_owned())
    } else {
        HydratedBody::Base64(BASE64.encode(&bytes))
    };
    path_cache().store(PathBuf::from(path), modified, len, &body);
    Ok(body)
}

fn resolve_one(attachment: &Attachment, caps: DialectCaps) -> WirePart {
    let kind = attachment.kind();
    let ingestible = match kind {
        AttachmentKind::Text => true,
        AttachmentKind::Image => caps.images,
        AttachmentKind::Pdf => caps.pdfs,
        AttachmentKind::Audio => caps.audio,
        AttachmentKind::Video => caps.video,
        AttachmentKind::Binary => false,
    };
    // A kind this dialect cannot carry degrades before any hydration: the
    // note describes what was attached, which needs no payload — reading and
    // encoding bytes only to discard them would be pure waste on every turn.
    if !ingestible {
        return WirePart::Text {
            text: degrade_note(attachment, kind),
        };
    }
    let want_text = matches!(kind, AttachmentKind::Text);
    let body = match &attachment.source {
        AttachmentSource::Path { path } => match hydrate_path(path, want_text) {
            Ok(body) => body,
            Err(err) => {
                return WirePart::Text {
                    text: format!(
                        "[attachment {} was provided by the user but its payload could not \
                         be read: {err}]",
                        attachment.label()
                    ),
                };
            }
        },
        // An inline `Data` source already carries the caller's base64: the
        // decode runs as validation (and, for text kinds, produces the
        // content), and the binary kinds forward the caller's string verbatim
        // instead of paying a decode+re-encode round trip. Never cached —
        // the payload is already in memory on the message.
        AttachmentSource::Data { base64 } => match BASE64.decode(base64) {
            Ok(bytes) if want_text => {
                HydratedBody::Text(String::from_utf8_lossy(&bytes).into_owned())
            }
            Ok(_) => HydratedBody::Base64(base64.clone()),
            Err(err) => {
                return WirePart::Text {
                    text: format!(
                        "[attachment {} was provided by the user but its inline payload is \
                         not valid base64: {err}]",
                        attachment.label()
                    ),
                };
            }
        },
    };
    match (kind, body) {
        (AttachmentKind::Text, HydratedBody::Text(content)) => WirePart::Text {
            text: inline_text(attachment, &content),
        },
        (AttachmentKind::Image, HydratedBody::Base64(base64)) => WirePart::Image {
            media_type: attachment.media_type.clone(),
            base64,
        },
        (AttachmentKind::Pdf, HydratedBody::Base64(base64)) => WirePart::Pdf {
            name: attachment.name.clone(),
            base64,
        },
        (AttachmentKind::Audio, HydratedBody::Base64(base64)) => WirePart::Audio {
            media_type: attachment.media_type.clone(),
            base64,
        },
        (AttachmentKind::Video, HydratedBody::Base64(base64)) => WirePart::Video {
            media_type: attachment.media_type.clone(),
            base64,
        },
        // The form is chosen from the kind above, so these pairs cannot
        // happen — stay total anyway: a note is a fine answer, an abort is
        // not.
        (kind, _) => WirePart::Text {
            text: degrade_note(attachment, kind),
        },
    }
}

/// A text-like file inlined in full, framed so the model knows what it is
/// looking at and where it ends.
fn inline_text(attachment: &Attachment, content: &str) -> String {
    format!(
        "[attached file: {}]\n<attached-file name=\"{}\">\n{}\n</attached-file>",
        attachment.label(),
        attachment.name,
        content.trim_end_matches('\n'),
    )
}

/// The note emitted for a kind this dialect cannot ingest. Names the
/// attachment precisely so the model can tell the user what it cannot see.
fn degrade_note(attachment: &Attachment, kind: AttachmentKind) -> String {
    let noun = match kind {
        AttachmentKind::Image => "images",
        AttachmentKind::Audio => "audio",
        AttachmentKind::Video => "video",
        AttachmentKind::Pdf => "PDF documents",
        AttachmentKind::Binary => "this file format",
        // Text inlines before this is called, so this arm is dead today. It
        // stays total anyway — a note is a fine answer, an abort is not.
        AttachmentKind::Text => "text files",
    };
    format!(
        "[the user attached {}, but the current provider cannot ingest {noun} natively; \
         acknowledge the attachment and suggest a provider or format that can be read]",
        attachment.label()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    const ALL: DialectCaps = DialectCaps {
        images: true,
        pdfs: true,
        audio: true,
        video: true,
    };
    const NONE: DialectCaps = DialectCaps {
        images: false,
        pdfs: false,
        audio: false,
        video: false,
    };

    fn file_attachment(
        name: &str,
        media_type: &str,
        payload: &[u8],
    ) -> (Attachment, tempfile::NamedTempFile) {
        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        file.write_all(payload).expect("write payload");
        let att = Attachment::from_path(
            name,
            media_type,
            payload.len() as u64,
            file.path().to_string_lossy(),
        );
        (att, file)
    }

    #[test]
    fn image_hydrates_from_disk_to_base64() {
        let (att, _guard) = file_attachment("shot.png", "image/png", b"\x89PNGdata");
        let parts = wire_parts(std::slice::from_ref(&att), ALL);
        assert_eq!(
            parts,
            vec![WirePart::Image {
                media_type: "image/png".into(),
                base64: BASE64.encode(b"\x89PNGdata"),
            }]
        );
    }

    #[test]
    fn inline_data_source_passes_through_decoded() {
        let att = Attachment {
            name: "clip.mp4".into(),
            media_type: "video/mp4".into(),
            byte_len: 4,
            source: AttachmentSource::Data {
                base64: BASE64.encode(b"vvvv"),
            },
        };
        let parts = wire_parts(&[att], ALL);
        assert_eq!(
            parts,
            vec![WirePart::Video {
                media_type: "video/mp4".into(),
                base64: BASE64.encode(b"vvvv"),
            }]
        );
    }

    #[test]
    fn texty_files_inline_their_content_regardless_of_caps() {
        let (att, _guard) = file_attachment("notes.md", "text/markdown", b"# Title\nbody\n");
        let parts = wire_parts(std::slice::from_ref(&att), NONE);
        let WirePart::Text { text } = &parts[0] else {
            panic!("expected text part, got {parts:?}");
        };
        assert!(text.contains("# Title\nbody"), "{text}");
        assert!(text.contains("notes.md"), "{text}");
    }

    #[test]
    fn unsupported_kinds_degrade_to_a_note_not_an_error() {
        let (att, _guard) = file_attachment("song.mp3", "audio/mpeg", b"ID3xxxx");
        let parts = wire_parts(std::slice::from_ref(&att), NONE);
        let WirePart::Text { text } = &parts[0] else {
            panic!("expected degrade note, got {parts:?}");
        };
        assert!(text.contains("song.mp3"), "{text}");
        assert!(text.contains("cannot ingest audio"), "{text}");
    }

    #[test]
    fn unknown_binary_always_degrades() {
        let (att, _guard) = file_attachment("bundle.zip", "application/zip", b"PK\x03\x04");
        let parts = wire_parts(std::slice::from_ref(&att), ALL);
        assert!(
            matches!(&parts[0], WirePart::Text { text } if text.contains("bundle.zip")),
            "{parts:?}"
        );
    }

    #[test]
    fn unreadable_payload_becomes_a_note_never_an_error() {
        let att = Attachment::from_path("gone.png", "image/png", 10, "/nonexistent/gone.png");
        let parts = wire_parts(&[att], ALL);
        let WirePart::Text { text } = &parts[0] else {
            panic!("expected note, got {parts:?}");
        };
        assert!(text.contains("could not be read"), "{text}");
        assert!(text.contains("gone.png"), "{text}");
    }

    /// The conversation replays every turn, so a path payload must hydrate
    /// once per file VERSION, not once per model call. Witness: with the
    /// `(mtime, len)` fingerprint pinned, a content change is served from the
    /// cache (the read+encode demonstrably did not run again); changing the
    /// fingerprint re-hydrates to the new content.
    #[test]
    fn path_payloads_hydrate_once_per_file_version() {
        use std::time::{Duration, SystemTime};
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("frame.png");
        let pinned = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let write_pinned = |payload: &[u8]| {
            std::fs::write(&path, payload).expect("write payload");
            std::fs::File::options()
                .write(true)
                .open(&path)
                .expect("reopen payload")
                .set_modified(pinned)
                .expect("pin mtime");
        };
        let att = Attachment::from_path("frame.png", "image/png", 4, path.to_string_lossy());
        let encoded = |att: &Attachment| match &wire_parts(std::slice::from_ref(att), ALL)[0] {
            WirePart::Image { base64, .. } => base64.clone(),
            other => panic!("expected image part, got {other:?}"),
        };

        write_pinned(b"aaaa");
        assert_eq!(encoded(&att), BASE64.encode(b"aaaa"));
        // Identical fingerprint (same len, pinned mtime): served from cache.
        write_pinned(b"bbbb");
        assert_eq!(encoded(&att), BASE64.encode(b"aaaa"));
        // A different length breaks the fingerprint: re-hydrated.
        std::fs::write(&path, b"cccccc").expect("rewrite payload");
        assert_eq!(encoded(&att), BASE64.encode(b"cccccc"));
    }

    /// The cache is a process-wide `static` on a session that runs for
    /// hours, so its budget is the difference between an optimization and a
    /// leak. Driven against [`PathCache`] directly rather than through the
    /// global: eviction is a property of the data structure, and the global
    /// is shared with every other test in this binary.
    #[test]
    fn the_path_cache_evicts_least_recently_used_instead_of_growing_forever() {
        let body = |n: usize| HydratedBody::Base64("x".repeat(n));
        let path = |n: usize| PathBuf::from(format!("/tmp/a{n}.png"));
        // Three entries, each a third of the budget plus a slack byte, so the
        // third cannot land until something leaves.
        let third = MAX_CACHE_BYTES / 3 + 1;

        let mut cache = PathCache::default();
        cache.store(path(0), None, 0, &body(third));
        cache.store(path(1), None, 0, &body(third));
        // Touch 0 so 1 — not insertion-order-oldest 0 — is the LRU victim.
        assert!(
            cache.get_fresh(&path(0), None, 0, false).is_some(),
            "entry 0 is fresh and must hit"
        );
        cache.store(path(2), None, 0, &body(third));

        assert!(
            cache.entries.contains_key(&path(0)),
            "the recently used entry must survive"
        );
        assert!(
            !cache.entries.contains_key(&path(1)),
            "the least recently used entry is the one evicted"
        );
        assert!(cache.entries.contains_key(&path(2)), "the new entry lands");
        assert!(
            cache.bytes <= MAX_CACHE_BYTES,
            "the budget is a bound, not a suggestion: {} bytes",
            cache.bytes
        );
    }

    /// A payload larger than the whole budget is hydrated and returned but
    /// never retained — caching it could only be paid for by evicting
    /// everything else, and it would be the next thing evicted anyway.
    #[test]
    fn a_payload_larger_than_the_whole_budget_is_never_retained() {
        let mut cache = PathCache::default();
        let keeper = PathBuf::from("/tmp/keeper.png");
        cache.store(keeper.clone(), None, 0, &HydratedBody::Base64("x".into()));

        cache.store(
            PathBuf::from("/tmp/whale.mp4"),
            None,
            0,
            &HydratedBody::Base64("x".repeat(MAX_CACHE_BYTES + 1)),
        );

        assert!(
            !cache.entries.contains_key(Path::new("/tmp/whale.mp4")),
            "an over-budget payload must not be retained"
        );
        assert!(
            cache.entries.contains_key(&keeper),
            "and it must not have evicted anything on its way out"
        );
        assert_eq!(cache.bytes, 1);
    }

    /// Re-hydrating a path must free the old payload's bytes before charging
    /// the new one. Getting this wrong leaks budget rather than memory — the
    /// counter drifts up until the cache evicts everything and never caches
    /// again, which is silent and would only show as a mystery slowdown.
    #[test]
    fn replacing_an_entry_does_not_double_count_its_bytes() {
        let mut cache = PathCache::default();
        let path = PathBuf::from("/tmp/edited.txt");
        for len in [10usize, 20, 30, 40] {
            cache.store(
                path.clone(),
                None,
                len as u64,
                &HydratedBody::Text("x".repeat(len)),
            );
            assert_eq!(
                cache.bytes, len,
                "only the live payload is charged to the budget"
            );
        }
        assert_eq!(cache.entries.len(), 1);
    }

    /// The cache stores the one form a kind consumes; the same path attached
    /// under another media type must recompute the other form, never serve
    /// base64 where inlined text belongs (or vice versa).
    #[test]
    fn a_cached_payload_recomputes_when_the_kind_wants_the_other_form() {
        let (as_image, _guard) = file_attachment("shot.png", "image/png", b"hello body");
        let parts = wire_parts(std::slice::from_ref(&as_image), ALL);
        assert!(matches!(&parts[0], WirePart::Image { .. }), "{parts:?}");

        let AttachmentSource::Path { path } = &as_image.source else {
            panic!("file_attachment builds a path source");
        };
        let as_text = Attachment::from_path("shot.txt", "text/plain", 10, path.clone());
        let parts = wire_parts(std::slice::from_ref(&as_text), ALL);
        let WirePart::Text { text } = &parts[0] else {
            panic!("expected inlined text, got {parts:?}");
        };
        assert!(text.contains("hello body"), "{text}");
    }

    #[test]
    fn pdf_maps_to_pdf_part_with_its_name() {
        let (att, _guard) = file_attachment("spec.pdf", "application/pdf", b"%PDF-1.7");
        let parts = wire_parts(std::slice::from_ref(&att), ALL);
        assert_eq!(
            parts,
            vec![WirePart::Pdf {
                name: "spec.pdf".into(),
                base64: BASE64.encode(b"%PDF-1.7"),
            }]
        );
    }
}
