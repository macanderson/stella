//! Prompt-text → multimodal-attachment extraction.
//!
//! The one chokepoint that makes attachments work on *every* input surface:
//! any token in a user prompt that names an existing image / audio / video /
//! PDF file on disk is attached to the message as a payload the model can
//! actually ingest. That covers the deck (whose prompt queue is text-shaped:
//! a `⌃V` clipboard image arrives here as its stored payload path), the
//! plain stdin REPL, and headless one-shot runs — with zero changes to how
//! prompts are queued, edited, or persisted.
//!
//! Text-like files (source, markdown, JSON…) are deliberately NOT attached:
//! reading those is what the agent's own tools are for, and silently
//! inlining every path-shaped token would bloat context. Unknown binaries
//! are skipped for the same reason. There is no size cap — Stella imposes no
//! limit of its own on attachment payloads; only providers do.

use std::collections::HashSet;
use std::path::PathBuf;

use stella_protocol::{
    Attachment, AttachmentKind, CompletionMessage, classify_media_type, media_type_for_path,
};

/// Attachments per prompt — a runaway-token backstop, not a size limit.
const MAX_ATTACHMENTS: usize = 32;

/// Bytes of any one `@`-mentioned text file that reach the prompt. Generous
/// enough for an ordinary source file, small enough that a mistaken `@` on a
/// vendored bundle degrades visibly instead of eating the context window.
const MAX_MENTION_BYTES: usize = 64 * 1024;

/// Bytes across *all* `@` mentions in one prompt. Bounds the pathological
/// `@a @b @c …` case that each individually clears the per-file cap.
const MAX_MENTION_TOTAL_BYTES: usize = 256 * 1024;

/// `@` mentions honoured per prompt, mirroring [`MAX_ATTACHMENTS`].
const MAX_MENTIONS: usize = 16;

/// Build the user [`CompletionMessage`] for a prompt, resolving `@path`
/// mentions against `root` (#1262).
///
/// There is deliberately no root-less form. An earlier draft kept one for
/// "callers with no workspace", but every caller has one, and a root is not a
/// convenience here — it is the confinement boundary. A root-less variant is
/// exactly the shape in which a prompt inlines `/etc/shadow`, so the type
/// system should not offer it.
///
/// # Why `@` earns what a bare token does not
///
/// [`extract_path_attachments`] deliberately refuses to inline text files, and
/// that refusal is right: a *bare* path-shaped token is a weak signal — prose
/// mentions filenames constantly — and reading files is what the agent's own
/// tools are for.
///
/// An explicit `@` is a different signal entirely. The user typed a sigil to
/// say "this file, specifically". That is the strongest statement available
/// about what the task concerns, and honouring it saves the read turn on the
/// one file the user already knows is relevant. So this **adds** a stronger
/// signal; it does not weaken the existing default. A bare token to a text
/// file is still not inlined, and there is a test that says so.
///
/// # Confinement and degradation
///
/// A mention is resolved under `root` and refused if it escapes — the same
/// posture as every tool path. A mention that does not resolve is left alone
/// as literal text rather than erroring, because `@` appears in ordinary
/// prose and in every email address; failing a prompt over `ops@oxagen.sh`
/// would be absurd. Oversized content truncates with the elision marked
/// in-band, so a clipped file never reads to the model as a complete one.
pub fn user_message_in(text: impl Into<String>, root: &std::path::Path) -> CompletionMessage {
    let text = text.into();
    let mut attachments = extract_path_attachments(&text);
    let mentions = scan_mentions(&text, root);

    // A media file named with `@` attaches rather than inlines. The bare-token
    // scan above cannot see it — `@shot.png` is not a path — and it resolves
    // relative to the process cwd, not the workspace, so the mention pass
    // (which resolves under `root`) is the only one that can find it.
    attachments.extend(mentions.attachments);
    attachments.dedup_by(|a, b| a.source == b.source);
    attachments.truncate(MAX_ATTACHMENTS);

    let content = match mentions.inlined {
        Some(block) => format!("{text}\n\n{block}"),
        None => text,
    };
    CompletionMessage::user_with_attachments(content, attachments)
}

/// What one pass over the prompt's `@` mentions produced.
#[derive(Default)]
struct Mentions {
    /// The joined `<file>` blocks, or `None` when nothing text-like resolved.
    inlined: Option<String>,
    /// Media mentions, which attach rather than inline.
    attachments: Vec<Attachment>,
}

/// Resolve every `@` mention in `text` under `root`, splitting them into text
/// to inline and media to attach.
///
/// One pass rather than two: both halves need the same resolution (confined,
/// canonicalized, deduplicated), and running the media scan over a
/// sigil-stripped copy of the prompt resolved paths against the process cwd
/// instead of the workspace — so `@shot.png` silently attached nothing.
fn scan_mentions(text: &str, root: &std::path::Path) -> Mentions {
    // Canonicalize once. `strip_prefix` below must run against the SAME form
    // the resolved path is in, and on macOS a temp dir under `/var` resolves
    // to `/private/var` — so comparing a resolved path to an unresolved root
    // never matched, and every inlined file showed an absolute path.
    let Ok(root) = std::fs::canonicalize(root) else {
        return Mentions::default();
    };
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut out = Mentions::default();
    let mut blocks = Vec::new();
    let mut total = 0_usize;

    for raw in text.split_whitespace() {
        if blocks.len() + out.attachments.len() >= MAX_MENTIONS {
            break;
        }
        let Some(token) = raw.strip_prefix('@') else {
            continue;
        };
        // Same trailing-punctuation trim as the media scan: "see @lib.rs."
        // names `lib.rs`, not `lib.rs.`.
        let token = token.trim_end_matches([')', '>', ']', '}', '"', '\'', '`', ',', ';', ':']);
        if token.is_empty() {
            continue;
        }
        let Some(path) = resolve_in_root(&root, token) else {
            continue;
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() || !seen.insert(path.clone()) {
            continue;
        }

        // Media attaches; everything else is a candidate for inlining.
        if let Some(media_type) = media_type_for_path(token)
            && attachable(classify_media_type(media_type))
        {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| token.to_string());
            out.attachments.push(Attachment::from_path(
                name,
                media_type,
                meta.len(),
                path.to_string_lossy(),
            ));
            continue;
        }

        if total >= MAX_MENTION_TOTAL_BYTES {
            continue;
        }
        // Binary files are not prompt content. `read_to_string` failing on
        // invalid UTF-8 is the check — no separate sniffing needed.
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        let remaining = MAX_MENTION_TOTAL_BYTES.saturating_sub(total);
        let (body, elided) = clip(&body, MAX_MENTION_BYTES.min(remaining));
        total += body.len();
        let shown = path.strip_prefix(&root).unwrap_or(&path).display();
        let mut block = format!("<file path=\"{shown}\">\n{body}");
        if !body.ends_with('\n') {
            block.push('\n');
        }
        if elided > 0 {
            block.push_str(&format!("[… {elided} bytes elided …]\n"));
        }
        block.push_str("</file>");
        blocks.push(block);
    }

    out.inlined = (!blocks.is_empty()).then(|| blocks.join("\n"));
    out
}

/// Clip to `max` bytes on a char boundary, returning the kept text and how
/// many bytes were dropped.
fn clip(text: &str, max: usize) -> (&str, usize) {
    if text.len() <= max {
        return (text, 0);
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], text.len() - end)
}

/// Resolve `token` under an already-canonicalized `root`, refusing anything
/// that escapes it.
///
/// Absolute mentions are allowed only when they already live under `root`;
/// `..` is rejected by the same canonicalized-prefix check. Confinement is
/// the reason this takes a root at all — an unconfined `@` is a prompt-side
/// arbitrary-file-read.
fn resolve_in_root(root: &std::path::Path, token: &str) -> Option<PathBuf> {
    let candidate = expand_home(token);
    let joined = if candidate.is_absolute() {
        candidate
    } else {
        root.join(candidate)
    };
    let resolved = std::fs::canonicalize(&joined).ok()?;
    resolved.starts_with(root).then_some(resolved)
}

/// Scan prompt text for tokens naming existing media/document files. Each
/// hit becomes a path-sourced [`Attachment`] (payload hydrated at
/// request-build time), deduplicated by canonical path.
pub fn extract_path_attachments(text: &str) -> Vec<Attachment> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut out = Vec::new();
    for raw in text.split_whitespace() {
        if out.len() >= MAX_ATTACHMENTS {
            break;
        }
        let token = raw
            .trim_start_matches(['(', '<', '[', '{', '"', '\'', '`'])
            .trim_end_matches([
                ')', '>', ']', '}', '"', '\'', '`', ',', '.', ';', ':', '!', '?',
            ]);
        if token.is_empty() {
            continue;
        }
        let Some(media_type) = media_type_for_path(token) else {
            continue;
        };
        if !attachable(classify_media_type(media_type)) {
            continue;
        }
        let path = expand_home(token);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if !seen.insert(canonical) {
            continue;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| token.to_string());
        out.push(Attachment::from_path(
            name,
            media_type,
            meta.len(),
            path.to_string_lossy(),
        ));
    }
    out
}

/// Kinds worth attaching from a path mention — the model can ingest these
/// payloads (or a provider degrade note names them); text-like files stay
/// the tools' job.
fn attachable(kind: AttachmentKind) -> bool {
    matches!(
        kind,
        AttachmentKind::Image | AttachmentKind::Audio | AttachmentKind::Video | AttachmentKind::Pdf
    )
}

/// Expand a leading `~/` against `$HOME`.
fn expand_home(token: &str) -> PathBuf {
    if let Some(rest) = token.strip_prefix("~/")
        && let Some(home) = crate::paths::home()
    {
        return home.join(rest);
    }
    PathBuf::from(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of #1262: a file the user explicitly named arrives in the
    /// prompt, so the agent does not spend a turn reading what it was just
    /// told to look at.
    #[test]
    fn an_at_mentioned_text_file_is_inlined() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn main() {}\n").unwrap();

        let msg = user_message_in("explain @lib.rs please", dir.path());
        assert!(
            msg.content.contains("explain @lib.rs please"),
            "{}",
            msg.content
        );
        assert!(
            msg.content.contains("<file path=\"lib.rs\">"),
            "mention must be inlined with its path: {}",
            msg.content
        );
        assert!(msg.content.contains("fn main() {}"), "{}", msg.content);
    }

    /// The existing default must not weaken. A bare path-shaped token is a
    /// weak signal and stays the tools' job; only the `@` sigil earns
    /// inlining.
    #[test]
    fn a_bare_text_path_is_still_not_inlined() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "SENTINEL_BODY\n").unwrap();

        let msg = user_message_in("look at lib.rs sometime", dir.path());
        assert!(
            !msg.content.contains("SENTINEL_BODY"),
            "a bare token must not inline: {}",
            msg.content
        );
        assert_eq!(msg.content, "look at lib.rs sometime");
    }

    /// An unconfined `@` is a prompt-side arbitrary-file-read.
    #[test]
    fn an_at_mention_escaping_the_root_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(dir.path().join("secret.txt"), "SENTINEL_SECRET\n").unwrap();

        let msg = user_message_in("read @../secret.txt", &root);
        assert!(
            !msg.content.contains("SENTINEL_SECRET"),
            "a mention must not escape the root: {}",
            msg.content
        );
    }

    /// `@` is everywhere in prose and in every email address; failing a prompt
    /// over one would be absurd, so an unresolvable mention stays literal.
    #[test]
    fn an_unresolvable_mention_is_left_as_literal_text() {
        let dir = tempfile::tempdir().unwrap();
        let msg = user_message_in("mail ops@oxagen.sh about @nope.rs", dir.path());
        assert_eq!(msg.content, "mail ops@oxagen.sh about @nope.rs");
    }

    /// A clipped file must never read to the model as a complete one.
    #[test]
    fn an_oversized_mention_truncates_with_the_elision_marked() {
        let dir = tempfile::tempdir().unwrap();
        let big = "x".repeat(MAX_MENTION_BYTES + 5_000);
        std::fs::write(dir.path().join("big.txt"), &big).unwrap();

        let msg = user_message_in("@big.txt", dir.path());
        assert!(
            msg.content.contains("bytes elided"),
            "{}",
            &msg.content[..200.min(msg.content.len())]
        );
        assert!(
            msg.content.len() < big.len(),
            "content must be shorter than the source file"
        );
    }

    /// Binary content is not prompt text — `read_to_string` failing is the
    /// check, and it must skip rather than poison the prompt.
    #[test]
    fn a_binary_mention_is_skipped_rather_than_inlined() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blob.bin"), [0xff_u8, 0xfe, 0x00, 0x01]).unwrap();
        let msg = user_message_in("@blob.bin", dir.path());
        assert_eq!(msg.content, "@blob.bin");
    }

    /// A media file named with `@` belongs on the attachment path, not the
    /// inline one — the bare-token scan cannot see it through the sigil.
    #[test]
    fn an_at_mentioned_image_attaches_instead_of_inlining() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("shot.png"), b"i").unwrap();

        let msg = user_message_in("what is in @shot.png", dir.path());
        assert_eq!(msg.attachments.len(), 1, "{:?}", msg.attachments);
        assert_eq!(msg.attachments[0].media_type, "image/png");
        assert!(!msg.content.contains("<file"), "{}", msg.content);
    }

    #[test]
    fn media_paths_in_a_prompt_become_attachments() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("shot.png");
        let pdf = dir.path().join("spec.pdf");
        std::fs::write(&img, b"i").unwrap();
        std::fs::write(&pdf, b"p").unwrap();
        let prompt = format!(
            "compare {} with the spec ({}) please",
            img.display(),
            pdf.display()
        );
        let msg = user_message_in(&prompt, dir.path());
        assert_eq!(msg.attachments.len(), 2, "{:?}", msg.attachments);
        assert_eq!(msg.attachments[0].media_type, "image/png");
        assert_eq!(msg.attachments[1].media_type, "application/pdf");
        assert_eq!(msg.content, prompt, "prompt text is untouched");
    }

    #[test]
    fn duplicates_missing_files_and_text_files_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("a.png");
        let md = dir.path().join("notes.md");
        std::fs::write(&img, b"i").unwrap();
        std::fs::write(&md, b"m").unwrap();
        let prompt = format!(
            "{img} {img} {missing} {md}",
            img = img.display(),
            missing = dir.path().join("gone.mp4").display(),
            md = md.display()
        );
        let atts = extract_path_attachments(&prompt);
        assert_eq!(atts.len(), 1, "{atts:?}");
        assert_eq!(atts[0].name, "a.png");
    }

    #[test]
    fn punctuation_around_paths_is_stripped() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("b.jpg");
        std::fs::write(&img, b"i").unwrap();
        let prompt = format!("look at \"{}\", then report", img.display());
        assert_eq!(extract_path_attachments(&prompt).len(), 1);
    }

    #[test]
    fn plain_prose_attaches_nothing() {
        assert!(
            extract_path_attachments("explain how video/mp4 handling works in http.rs").is_empty()
        );
    }
}
