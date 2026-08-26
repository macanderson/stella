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
//! Video is the one kind with a rung between native and the note. A dialect
//! that carries images but not video gets the clip as sampled stills plus a
//! note saying they are stills (`crate::keyframes`), because an image-capable
//! model shown eight frames can answer most questions about a clip and a note
//! naming the filename can answer none. The note remains the floor: no
//! decoder, an unreadable container, or bytes that cannot be staged all land
//! back on it (#3340). Both sources reach the decoder — a path directly, an
//! inline payload through a temp file that lives for one extraction (#4800) —
//! so the model's answer does not depend on whether the clip was pasted or
//! `@`-mentioned.
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

use crate::keyframes::{
    FfmpegSampler, FrameSampler, MAX_SAMPLED_FRAMES, SampleFailure, SampledVideo, sampling_note,
    unsampled_note,
};

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
///
/// One attachment is usually one part, but not always — a sampled video is
/// several images plus a note — so this is a flat map, and an adapter must
/// not assume the two slices line up index for index.
pub(crate) fn wire_parts(attachments: &[Attachment], caps: DialectCaps) -> Vec<WirePart> {
    wire_parts_with(attachments, caps, &FfmpegSampler)
}

/// [`wire_parts`] against a caller-supplied frame sampler.
///
/// The seam exists so the video fan-out can be witnessed without a decoder
/// installed on the machine running the test: a fake sampler settles what the
/// resolve path does with frames, and `crate::keyframes`'s own tests settle
/// which frames get asked for. Neither needs `ffmpeg`, which is exactly the
/// property a probe-and-degrade dependency has to have to be testable at all.
fn wire_parts_with(
    attachments: &[Attachment],
    caps: DialectCaps,
    sampler: &dyn FrameSampler,
) -> Vec<WirePart> {
    attachments
        .iter()
        .flat_map(|attachment| resolve_one(attachment, caps, sampler))
        .collect()
}

/// A payload hydrated into the one wire form its kind consumes: inlined
/// (lossy UTF-8) content for text-like files, base64 for the binary kinds,
/// sampled stills for a video a dialect can only see as images.
/// Hydrating straight to the consumed form is what lets [`PATH_CACHE`] pay:
/// the expensive transform (read + encode, or a decoder invocation) is what
/// gets memoized, not just the raw bytes.
#[derive(Clone)]
enum HydratedBody {
    Text(String),
    Base64(String),
    Frames(SampledVideo),
}

impl HydratedBody {
    /// Retained heap bytes — what [`PathCache`] budgets against. Base64 is
    /// ~4/3 of the file it came from, so this is measured on the hydrated
    /// string rather than inferred from the file length.
    fn retained_bytes(&self) -> usize {
        match self {
            HydratedBody::Text(text) => text.len(),
            HydratedBody::Base64(encoded) => encoded.len(),
            HydratedBody::Frames(video) => video.retained_bytes(),
        }
    }
}

/// Which hydrated form a lookup wants.
///
/// The cache is keyed by path, and the same path can be attached under
/// different media types (and a video sampled at a different ceiling), so the
/// form is part of freshness: serving base64 where inlined text belongs — or
/// four frames where eight were asked for — is a wrong answer, not a stale
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    Text,
    Base64,
    /// Stills sampled from a video, at most this many.
    Frames(usize),
}

/// What a cache entry is filed under.
///
/// Two key spaces, kept apart by the type rather than by a naming convention
/// on one of them: a file has a path and an identity that changes when its
/// bytes do, while an inline payload has neither and IS its bytes. Folding
/// the second into the first — a synthetic `data:<digest>` path — would put a
/// string nothing on disk answers to into a `PathBuf`, where the next reader
/// has to be told it is not a path (#4800).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum CacheKey {
    /// A file on disk, validated against its `(mtime, len)` fingerprint.
    Path(PathBuf),
    /// An inline payload, keyed by the SHA-256 of its base64. The digest is
    /// the whole identity: identical bytes are the same entry, and there is
    /// no fingerprint to re-check because nothing can edit them underneath us.
    Inline(String),
}

/// One cached path payload, valid while the file's `(mtime, len)`
/// fingerprint and the requested [`Form`] both hold. `last_used` is the
/// cache's logical clock at the last hit, which is what makes eviction
/// least-recently-used rather than arbitrary.
struct CachedPayload {
    modified: Option<SystemTime>,
    len: u64,
    /// The form this entry was hydrated for. Recorded rather than inferred
    /// from `body`, because a short video sampled at a ceiling of eight
    /// yields three frames and is indistinguishable, by its body alone, from
    /// the same video sampled at a ceiling of three.
    form: Form,
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

/// Bounded, least-recently-used cache of hydrated attachment payloads. The
/// conversation replays every turn, so without it every model call re-read
/// and re-encoded every attachment for the rest of the session — invisible
/// for a screenshot, pathological for a large payload on a long session.
///
/// A [`AttachmentSource::Path`] entry is keyed by path and validated against
/// the file's `(mtime, len)` on every lookup, so an edited file re-hydrates
/// on its next turn. An inline entry is keyed by the digest of its own bytes
/// and has no fingerprint to check, because nothing can edit a payload the
/// message is carrying.
///
/// A miss is a re-read, never a failure, so eviction is always safe: the
/// cache is a latency optimization with no correctness role. That is what
/// lets the budget be enforced bluntly.
#[derive(Default)]
struct PathCache {
    entries: HashMap<CacheKey, CachedPayload>,
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
        key: &CacheKey,
        modified: Option<SystemTime>,
        len: u64,
        form: Form,
    ) -> Option<HydratedBody> {
        let entry = self.entries.get_mut(key)?;
        // The stored form must also be the one this kind consumes — the same
        // path re-attached under a different media type recomputes.
        let fresh = entry.modified == modified && entry.len == len && entry.form == form;
        if !fresh {
            return None;
        }
        self.tick += 1;
        entry.last_used = self.tick;
        Some(entry.body.clone())
    }

    /// Retain `body` under `key`, evicting least-recently-used entries until
    /// it fits. A payload larger than the whole budget is simply not
    /// retained — caching it could only be paid for by evicting everything
    /// else, and it would still be the next thing evicted.
    fn store(
        &mut self,
        key: CacheKey,
        modified: Option<SystemTime>,
        len: u64,
        form: Form,
        body: &HydratedBody,
    ) {
        // Replacing an entry frees its old bytes first, so a re-hydrated
        // file is never double-counted against the budget.
        self.remove(&key);
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
            key,
            CachedPayload {
                modified,
                len,
                form,
                body: body.clone(),
                last_used: self.tick,
            },
        );
    }

    fn remove(&mut self, key: &CacheKey) {
        if let Some(old) = self.entries.remove(key) {
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

/// Hydrate a path payload in the form `form` selects, through [`PATH_CACHE`].
/// IO errors propagate for the caller to degrade to a note.
fn hydrate_path(path: &str, form: Form) -> std::io::Result<HydratedBody> {
    let meta = std::fs::metadata(path)?;
    let modified = meta.modified().ok();
    let len = meta.len();
    let key = CacheKey::Path(PathBuf::from(path));
    if let Some(body) = path_cache().get_fresh(&key, modified, len, form) {
        return Ok(body);
    }
    let bytes = std::fs::read(path)?;
    let body = match form {
        Form::Text => HydratedBody::Text(String::from_utf8_lossy(&bytes).into_owned()),
        // A sampled video never reaches here: `sample_video` owns that form's
        // hydration because its expensive half is a decoder invocation, not a
        // read. Returning the raw base64 would be wrong rather than slow, so
        // it is not offered.
        Form::Base64 | Form::Frames(_) => HydratedBody::Base64(BASE64.encode(&bytes)),
    };
    path_cache().store(key, modified, len, form, &body);
    Ok(body)
}

/// Sample a video on disk into stills, memoized per file version through
/// [`PATH_CACHE`] like any other hydration.
///
/// The memoization is what makes this affordable at all: the conversation
/// replays every turn, so without it `ffmpeg` would run once per model call
/// for the rest of the session on a file that has not changed.
fn sample_video(
    path: &str,
    sampler: &dyn FrameSampler,
    max_frames: usize,
) -> Result<SampledVideo, SampleFailure> {
    let form = Form::Frames(max_frames);
    let meta = std::fs::metadata(path).ok();
    let fingerprint = meta.as_ref().map(|m| (m.modified().ok(), m.len()));
    let key = CacheKey::Path(PathBuf::from(path));
    if let Some((modified, len)) = fingerprint
        && let Some(HydratedBody::Frames(video)) = path_cache().get_fresh(&key, modified, len, form)
    {
        return Ok(video);
    }
    let video = sampler.sample(Path::new(path), max_frames)?;
    if let Some((modified, len)) = fingerprint {
        path_cache().store(
            key,
            modified,
            len,
            form,
            &HydratedBody::Frames(video.clone()),
        );
    }
    Ok(video)
}

/// Sample an inline video payload into stills by staging it as a file the
/// decoder can seek in, memoized on the payload's own digest (#4800).
///
/// `ffmpeg` reads files, and a pasted attachment has bytes and no path, so
/// the bytes are written to an owner-only temp file for the length of one
/// extraction and deleted on the way out — including on every failure path,
/// because [`tempfile::NamedTempFile`] deletes on drop.
///
/// The digest is what makes this affordable on a replayed conversation. The
/// path cache validates a file against `(mtime, len)`, which an inline
/// payload does not have; hashing its base64 gives the same guarantee from
/// the bytes themselves, so identical payloads are one entry and `ffmpeg`
/// runs once rather than once per turn. Hashing on every turn is the cost of
/// that, and it is the right trade by a wide margin: it is a linear pass over
/// bytes already resident in memory, against a decoder invocation measured in
/// seconds.
fn sample_inline_video(
    base64: &str,
    suffix: &str,
    sampler: &dyn FrameSampler,
    max_frames: usize,
) -> Result<SampledVideo, SampleFailure> {
    use sha2::{Digest, Sha256};

    let form = Form::Frames(max_frames);
    // Base64 rather than hex: this string is a map key, never shown to
    // anyone, and the encoder is already in scope.
    let key = CacheKey::Inline(BASE64.encode(Sha256::digest(base64.as_bytes())));
    // No fingerprint: the digest IS the identity, so `(None, 0)` is not a
    // missing check but the statement that there is nothing else to check.
    if let Some(HydratedBody::Frames(video)) = path_cache().get_fresh(&key, None, 0, form) {
        return Ok(video);
    }

    let bytes = BASE64
        .decode(base64)
        .map_err(|_| SampleFailure::Unstageable)?;
    let staged = tempfile::Builder::new()
        .prefix("stella-video-")
        .suffix(suffix)
        .tempfile()
        .map_err(|_| SampleFailure::Unstageable)?;
    std::fs::write(staged.path(), &bytes).map_err(|_| SampleFailure::Unstageable)?;

    let video = sampler.sample(staged.path(), max_frames)?;
    // Explicit rather than left to the drop at end of scope: the frames are
    // in memory now and the payload can be large, so the file goes as soon as
    // it has served its purpose. A failed close is not worth failing a turn
    // over — the temp directory is the OS's to reap.
    drop(staged);

    path_cache().store(key, None, 0, form, &HydratedBody::Frames(video.clone()));
    Ok(video)
}

/// The file extension to stage an inline payload under, taken from the
/// attachment's own name. `ffmpeg` sniffs the container from content, so this
/// is a hint rather than a requirement — but a demuxer given a matching
/// extension has one less thing to guess, and a name with no extension is a
/// perfectly good `""`.
fn stage_suffix(name: &str) -> String {
    match name.rsplit_once('.') {
        // Bounded and charset-checked: the name arrives from the user and
        // becomes part of a filename, so anything that is not a plain
        // alphanumeric extension is dropped rather than sanitized.
        Some((_, ext))
            if !ext.is_empty()
                && ext.len() <= 8
                && ext.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            format!(".{}", ext.to_ascii_lowercase())
        }
        _ => String::new(),
    }
}

/// The wire parts one attachment resolves to.
///
/// Almost always exactly one. A video sampled into stills is the exception —
/// it fans out to one [`WirePart::Image`] per frame plus the note that says
/// they are frames — which is why this returns a `Vec` rather than a
/// `WirePart`.
fn resolve_one(
    attachment: &Attachment,
    caps: DialectCaps,
    sampler: &dyn FrameSampler,
) -> Vec<WirePart> {
    let kind = attachment.kind();
    let ingestible = match kind {
        AttachmentKind::Text => true,
        AttachmentKind::Image => caps.images,
        AttachmentKind::Pdf => caps.pdfs,
        AttachmentKind::Audio => caps.audio,
        AttachmentKind::Video => caps.video,
        AttachmentKind::Binary => false,
    };
    // A video this dialect cannot carry, on a dialect that CAN carry images,
    // takes the sampled rung of the degrade ladder before the note. Checked
    // before the `!ingestible` bail below, which is where every other
    // unsupported kind still lands.
    if kind == AttachmentKind::Video && !caps.video && caps.images {
        return sampled_video_parts(attachment, sampler);
    }
    // A kind this dialect cannot carry degrades before any hydration: the
    // note describes what was attached, which needs no payload — reading and
    // encoding bytes only to discard them would be pure waste on every turn.
    if !ingestible {
        return vec![WirePart::Text {
            text: degrade_note(attachment, kind),
        }];
    }
    let form = if matches!(kind, AttachmentKind::Text) {
        Form::Text
    } else {
        Form::Base64
    };
    let body = match &attachment.source {
        AttachmentSource::Path { path } => match hydrate_path(path, form) {
            Ok(body) => body,
            Err(err) => {
                return vec![WirePart::Text {
                    text: format!(
                        "[attachment {} was provided by the user but its payload could not \
                         be read: {err}]",
                        attachment.label()
                    ),
                }];
            }
        },
        // An inline `Data` source already carries the caller's base64: the
        // decode runs as validation (and, for text kinds, produces the
        // content), and the binary kinds forward the caller's string verbatim
        // instead of paying a decode+re-encode round trip. Never cached —
        // the payload is already in memory on the message.
        AttachmentSource::Data { base64 } => match BASE64.decode(base64) {
            Ok(bytes) if form == Form::Text => {
                HydratedBody::Text(String::from_utf8_lossy(&bytes).into_owned())
            }
            Ok(_) => HydratedBody::Base64(base64.clone()),
            Err(err) => {
                return vec![WirePart::Text {
                    text: format!(
                        "[attachment {} was provided by the user but its inline payload is \
                         not valid base64: {err}]",
                        attachment.label()
                    ),
                }];
            }
        },
    };
    let part = match (kind, body) {
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
    };
    vec![part]
}

/// A video resolved for a dialect that sees images but not video: one
/// [`WirePart::Image`] per sampled still, then the note saying they are
/// stills.
///
/// The note comes last on purpose. It is the model's instruction for how to
/// read the images above it, and an instruction that arrives before its
/// subject is one the model has to hold in mind rather than apply.
///
/// The floor never moves: every failure below — no decoder, an unreadable
/// container, bytes that cannot be staged — returns exactly one text note,
/// which is what a video attachment produced on these dialects before
/// sampling existed.
///
/// Both sources reach the decoder, by different routes. A path is handed over
/// as-is; an inline payload is staged as a temp file for the length of one
/// extraction (#4800). A user who pastes a clip and a user who `@`-mentions
/// one are asking the same question, and answering only the second would make
/// the model's capability depend on which way the bytes arrived.
fn sampled_video_parts(attachment: &Attachment, sampler: &dyn FrameSampler) -> Vec<WirePart> {
    let sampled = match &attachment.source {
        AttachmentSource::Path { path } => sample_video(path, sampler, MAX_SAMPLED_FRAMES),
        AttachmentSource::Data { base64 } => sample_inline_video(
            base64,
            &stage_suffix(&attachment.name),
            sampler,
            MAX_SAMPLED_FRAMES,
        ),
    };
    let video = match sampled {
        Ok(video) => video,
        Err(failure) => {
            return vec![WirePart::Text {
                text: unsampled_note(&attachment.label(), failure),
            }];
        }
    };
    let mut parts: Vec<WirePart> = video
        .frames
        .iter()
        .map(|frame| WirePart::Image {
            media_type: frame.media_type.clone(),
            base64: frame.base64.clone(),
        })
        .collect();
    parts.push(WirePart::Text {
        text: sampling_note(&attachment.label(), &video),
    });
    parts
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
        cache.store(CacheKey::Path(path(0)), None, 0, Form::Base64, &body(third));
        cache.store(CacheKey::Path(path(1)), None, 0, Form::Base64, &body(third));
        // Touch 0 so 1 — not insertion-order-oldest 0 — is the LRU victim.
        assert!(
            cache
                .get_fresh(&CacheKey::Path(path(0)), None, 0, Form::Base64)
                .is_some(),
            "entry 0 is fresh and must hit"
        );
        cache.store(CacheKey::Path(path(2)), None, 0, Form::Base64, &body(third));

        assert!(
            cache.entries.contains_key(&CacheKey::Path(path(0))),
            "the recently used entry must survive"
        );
        assert!(
            !cache.entries.contains_key(&CacheKey::Path(path(1))),
            "the least recently used entry is the one evicted"
        );
        assert!(
            cache.entries.contains_key(&CacheKey::Path(path(2))),
            "the new entry lands"
        );
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
        cache.store(
            CacheKey::Path(keeper.clone()),
            None,
            0,
            Form::Base64,
            &HydratedBody::Base64("x".into()),
        );

        cache.store(
            CacheKey::Path(PathBuf::from("/tmp/whale.mp4")),
            None,
            0,
            Form::Base64,
            &HydratedBody::Base64("x".repeat(MAX_CACHE_BYTES + 1)),
        );

        assert!(
            !cache
                .entries
                .contains_key(&CacheKey::Path(PathBuf::from("/tmp/whale.mp4"))),
            "an over-budget payload must not be retained"
        );
        assert!(
            cache.entries.contains_key(&CacheKey::Path(keeper)),
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
                CacheKey::Path(path.clone()),
                None,
                len as u64,
                Form::Text,
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

    /// A sampler that answers from a fixture, so the resolve path's fan-out
    /// is witnessed without `ffmpeg` on the machine running the test.
    struct FakeSampler {
        result: Result<SampledVideo, SampleFailure>,
        calls: std::cell::Cell<usize>,
    }

    impl FakeSampler {
        fn frames(n: usize) -> Self {
            Self {
                result: Ok(SampledVideo {
                    duration_ms: 20_000,
                    frames: (0..n)
                        .map(|i| crate::keyframes::SampledFrame {
                            at_ms: (i as u64 + 1) * 2_000,
                            media_type: "image/jpeg".into(),
                            base64: format!("frame{i}"),
                        })
                        .collect(),
                }),
                calls: std::cell::Cell::new(0),
            }
        }

        fn failing(failure: SampleFailure) -> Self {
            Self {
                result: Err(failure),
                calls: std::cell::Cell::new(0),
            }
        }
    }

    impl FrameSampler for FakeSampler {
        fn sample(&self, _path: &Path, _max_frames: usize) -> Result<SampledVideo, SampleFailure> {
            self.calls.set(self.calls.get() + 1);
            self.result.clone()
        }
    }

    const IMAGES_ONLY: DialectCaps = DialectCaps {
        images: true,
        pdfs: true,
        audio: false,
        video: false,
    };

    /// **The witness (#3340).** On a dialect that carries images but not
    /// video, a video attachment resolves to one image part per sampled frame
    /// plus a note saying they are frames — where before it was a single text
    /// note and nothing the model could look at.
    #[test]
    fn a_video_on_an_image_capable_dialect_resolves_to_sampled_frames() {
        let (att, _guard) = file_attachment("clip.mp4", "video/mp4", b"\x00\x00\x00 ftypisom");
        let sampler = FakeSampler::frames(3);
        let parts = wire_parts_with(std::slice::from_ref(&att), IMAGES_ONLY, &sampler);

        assert_eq!(parts.len(), 4, "three frames and the note: {parts:?}");
        for (i, part) in parts[..3].iter().enumerate() {
            assert_eq!(
                part,
                &WirePart::Image {
                    media_type: "image/jpeg".into(),
                    base64: format!("frame{i}"),
                }
            );
        }
        let WirePart::Text { text } = &parts[3] else {
            panic!("the note rides last, after the frames it describes: {parts:?}");
        };
        assert!(text.contains("clip.mp4"), "{text}");
        assert!(text.contains("3 still frames"), "{text}");
        assert!(text.contains("NOT the video"), "{text}");
        assert!(text.contains("audio was not transcribed"), "{text}");
    }

    /// The floor never moved: a dialect with no image shape either, and a
    /// dialect that carries video natively, both behave exactly as before.
    #[test]
    fn sampling_fires_only_where_images_ride_but_video_does_not() {
        let (att, _guard) = file_attachment("clip.mp4", "video/mp4", b"\x00\x00\x00 ftypisom");

        let sampler = FakeSampler::frames(3);
        let parts = wire_parts_with(std::slice::from_ref(&att), NONE, &sampler);
        assert!(
            matches!(&parts[0], WirePart::Text { text } if text.contains("cannot ingest video")),
            "no images either: today's note, unchanged — {parts:?}"
        );
        assert_eq!(parts.len(), 1);

        let sampler = FakeSampler::frames(3);
        let parts = wire_parts_with(std::slice::from_ref(&att), ALL, &sampler);
        assert!(
            matches!(&parts[0], WirePart::Video { .. }),
            "native video is untouched — {parts:?}"
        );
        assert_eq!(sampler.calls.get(), 0, "and the sampler is never consulted");
    }

    /// Degradation stays total. Every way sampling can fail lands back on one
    /// text note — the model is told what it cannot see and why, and the turn
    /// survives.
    #[test]
    fn an_unsampleable_video_degrades_to_a_note_naming_the_reason() {
        let (att, _guard) = file_attachment("clip.mp4", "video/mp4", b"\x00\x00\x00 ftypisom");
        for failure in [
            SampleFailure::ToolMissing,
            SampleFailure::UnreadableDuration,
            SampleFailure::NoFrames,
        ] {
            let sampler = FakeSampler::failing(failure);
            let parts = wire_parts_with(std::slice::from_ref(&att), IMAGES_ONLY, &sampler);
            let [WirePart::Text { text }] = parts.as_slice() else {
                panic!("expected exactly one note for {failure:?}, got {parts:?}");
            };
            assert!(text.contains("clip.mp4"), "{text}");
            assert!(text.contains(failure.reason()), "{text}");
        }
    }

    /// A sampler that records the path it was handed, so a test can check
    /// what happened to the file after the call returned.
    struct StagingSampler {
        seen: std::cell::RefCell<Vec<PathBuf>>,
        existed: std::cell::Cell<bool>,
    }

    impl StagingSampler {
        fn new() -> Self {
            Self {
                seen: std::cell::RefCell::new(Vec::new()),
                existed: std::cell::Cell::new(false),
            }
        }
    }

    impl FrameSampler for StagingSampler {
        fn sample(&self, path: &Path, _max_frames: usize) -> Result<SampledVideo, SampleFailure> {
            self.existed.set(path.exists());
            self.seen.borrow_mut().push(path.to_path_buf());
            Ok(SampledVideo {
                duration_ms: 4_000,
                frames: vec![crate::keyframes::SampledFrame {
                    at_ms: 2_000,
                    media_type: "image/jpeg".into(),
                    base64: "inline".into(),
                }],
            })
        }
    }

    fn inline_video(name: &str, payload: &[u8]) -> Attachment {
        Attachment {
            name: name.into(),
            media_type: "video/mp4".into(),
            byte_len: payload.len() as u64,
            source: AttachmentSource::Data {
                base64: BASE64.encode(payload),
            },
        }
    }

    /// **The witness (#4800).** A pasted video is staged as a temp file and
    /// sampled, where before it took the text note. Pasting a clip and
    /// `@`-mentioning one ask the same question, and the answer must not
    /// depend on how the bytes arrived.
    #[test]
    fn an_inline_video_is_staged_and_sampled_like_one_on_disk() {
        let att = inline_video("pasted.mp4", b"\x00\x00\x00 ftypisom-inline-1");
        let sampler = StagingSampler::new();
        let parts = wire_parts_with(std::slice::from_ref(&att), IMAGES_ONLY, &sampler);

        assert_eq!(parts.len(), 2, "one frame and the note: {parts:?}");
        assert_eq!(
            parts[0],
            WirePart::Image {
                media_type: "image/jpeg".into(),
                base64: "inline".into(),
            }
        );
        assert!(
            matches!(&parts[1], WirePart::Text { text } if text.contains("NOT the video")),
            "{parts:?}"
        );
        assert!(
            sampler.existed.get(),
            "the payload was on disk while the decoder ran"
        );
    }

    /// The staged file lives for exactly one extraction. A payload the user
    /// pasted is not left in the temp directory for anything else to read.
    #[test]
    fn the_staged_file_is_gone_once_sampling_returns() {
        let att = inline_video("pasted.mp4", b"\x00\x00\x00 ftypisom-inline-2");
        let sampler = StagingSampler::new();
        wire_parts_with(std::slice::from_ref(&att), IMAGES_ONLY, &sampler);

        let staged = sampler.seen.borrow().clone();
        assert_eq!(staged.len(), 1, "one extraction, one staged file");
        assert!(
            !staged[0].exists(),
            "the staged payload outlived the call: {}",
            staged[0].display()
        );
    }

    /// The staged name carries the attachment's own extension, so a demuxer
    /// has one less thing to guess.
    #[test]
    fn the_staged_file_keeps_the_attachments_extension() {
        for (name, want) in [
            ("clip.mp4", ".mp4"),
            ("CLIP.MOV", ".mov"),
            ("no-extension", ""),
            // A name that is not a plain extension contributes nothing rather
            // than being sanitized into a filename.
            ("odd.tar.gz~", ""),
            ("weird.a/b", ""),
        ] {
            assert_eq!(stage_suffix(name), want, "{name}");
        }
    }

    /// An inline payload has no `(mtime, len)` to validate, so the cache keys
    /// it on the digest of its own bytes. Witness: the same payload twice is
    /// one extraction, and a different payload is a second one.
    #[test]
    fn an_inline_video_is_sampled_once_per_distinct_payload() {
        let first = inline_video("a.mp4", b"\x00\x00\x00 ftypisom-inline-3");
        let same = inline_video("renamed.mp4", b"\x00\x00\x00 ftypisom-inline-3");
        let other = inline_video("b.mp4", b"\x00\x00\x00 ftypisom-inline-4");
        let sampler = StagingSampler::new();

        wire_parts_with(std::slice::from_ref(&first), IMAGES_ONLY, &sampler);
        wire_parts_with(std::slice::from_ref(&same), IMAGES_ONLY, &sampler);
        assert_eq!(
            sampler.seen.borrow().len(),
            1,
            "identical bytes are one entry, whatever the attachment is called"
        );

        wire_parts_with(std::slice::from_ref(&other), IMAGES_ONLY, &sampler);
        assert_eq!(
            sampler.seen.borrow().len(),
            2,
            "different bytes are a different entry"
        );
    }

    /// A payload that is not valid base64 cannot be staged, and degradation
    /// stays total: one note, naming the reason, never an error.
    #[test]
    fn an_unstageable_inline_payload_degrades_to_a_note() {
        let att = Attachment {
            name: "broken.mp4".into(),
            media_type: "video/mp4".into(),
            byte_len: 4,
            source: AttachmentSource::Data {
                base64: "not!valid!base64".into(),
            },
        };
        let sampler = FakeSampler::frames(3);
        let parts = wire_parts_with(&[att], IMAGES_ONLY, &sampler);
        let [WirePart::Text { text }] = parts.as_slice() else {
            panic!("expected exactly one note, got {parts:?}");
        };
        assert!(text.contains("broken.mp4"), "{text}");
        assert!(text.contains(SampleFailure::Unstageable.reason()), "{text}");
        assert_eq!(sampler.calls.get(), 0, "no decoder was invoked");
    }

    /// The conversation replays every turn, so an uncached sampler would run
    /// `ffmpeg` on an unchanged file once per model call for the rest of the
    /// session. Witness: two resolutions of the same video, one extraction.
    #[test]
    fn a_video_is_sampled_once_per_file_version_not_once_per_turn() {
        let (att, _guard) = file_attachment("cached.mp4", "video/mp4", b"\x00\x00\x00 ftypisom");
        let sampler = FakeSampler::frames(2);

        let first = wire_parts_with(std::slice::from_ref(&att), IMAGES_ONLY, &sampler);
        let second = wire_parts_with(std::slice::from_ref(&att), IMAGES_ONLY, &sampler);

        assert_eq!(first, second);
        assert_eq!(
            sampler.calls.get(),
            1,
            "the second turn must be served from the path cache"
        );
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
