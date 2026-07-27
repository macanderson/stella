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

/// One cached path payload, valid while the file's `(mtime, len)`
/// fingerprint holds.
struct CachedPayload {
    modified: Option<SystemTime>,
    len: u64,
    body: HydratedBody,
}

/// Process-wide cache of hydrated [`AttachmentSource::Path`] payloads. The
/// conversation replays every turn, so without it every model call re-read
/// and re-encoded every path attachment for the rest of the session —
/// invisible for a screenshot, pathological for a large payload on a long
/// session. Keyed by path and validated against the file's `(mtime, len)` on
/// every lookup, so an edited file re-hydrates on its next turn. Bounded by
/// what the user actually attaches: one live entry per distinct path,
/// replaced in place when the file changes.
static PATH_CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedPayload>>> = OnceLock::new();

/// The path cache's guard. A poisoned lock is recovered rather than
/// propagated — the worst a poisoned cache can hold is a stale entry the
/// fingerprint check re-validates anyway, and this module's contract is that
/// an attachment never aborts a turn.
fn path_cache() -> std::sync::MutexGuard<'static, HashMap<PathBuf, CachedPayload>> {
    let cache = PATH_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
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
    if let Some(entry) = path_cache().get(Path::new(path)) {
        // The stored form must also be the one this kind consumes — the same
        // path re-attached under a different media type recomputes.
        let fresh = entry.modified == modified
            && entry.len == len
            && matches!(
                (&entry.body, want_text),
                (HydratedBody::Text(_), true) | (HydratedBody::Base64(_), false)
            );
        if fresh {
            return Ok(entry.body.clone());
        }
    }
    let bytes = std::fs::read(path)?;
    let body = if want_text {
        HydratedBody::Text(String::from_utf8_lossy(&bytes).into_owned())
    } else {
        HydratedBody::Base64(BASE64.encode(&bytes))
    };
    path_cache().insert(
        PathBuf::from(path),
        CachedPayload {
            modified,
            len,
            body: body.clone(),
        },
    );
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
