//! Splitting a document into the slices extraction hands to the model, one call
//! each.
//!
//! ## Why this exists
//!
//! Extraction used to cap the document instead of dividing it: everything past
//! the cap was dropped before the model ever saw it, and the run reported
//! success anyway. This repository's own `AGENTS.md` was extracted at 54% (#2407)
//! — the cut landed mid-table, and the god-file rules, the glossary, the
//! code-style conventions, the testing approach and the whole gotchas section
//! could not produce a record and could not have. The documents the feature
//! exists to read are exactly the documents that exceeded the cap.
//!
//! Raising the cap only moves that edge. The reply is roughly the size of the
//! text that produced it — every claim restates its sentence and carries a dozen
//! schema fields — so a bigger input buys a cut-off *reply* instead, and the
//! smallest output ceiling in the seeded catalog is 30,000 tokens. So the
//! document is divided and the claims merged: [`TARGET_CHARS`] is what one call
//! is asked to read, and a document of any length is covered by as many calls as
//! it takes.
//!
//! ## Where the seams go
//!
//! On markdown headings, never on a character offset. Cutting AGENTS.md at
//! 24,000 characters severed a table row mid-cell, and a slice that opens in the
//! middle of a sentence gives the extractor nothing to interpret it against.
//! Headings are the seam the document itself declares, so a slice begins where a
//! section begins and carries the heading path it sits under ([`Chunk::path`])
//! for the slices that continue an oversized section.
//!
//! Fenced code blocks are tracked, because a `# comment` inside a shell fence is
//! not a heading and splitting there produces an unterminated fence in both
//! halves. Only ATX headings (`## Title`) open a section; a setext heading is not
//! a seam, which costs a larger section that the size splitter below then divides
//! anyway — a missed seam degrades to a coarser cut, never to a lost character.
//!
//! ## What is guaranteed
//!
//! Concatenating every chunk's [`Chunk::text`] reproduces the input exactly, up
//! to the whole-document ceiling in [`bound`]. That property is what makes "the
//! whole file is extracted" checkable rather than asserted, and it is the
//! property the tests pin.

/// The characters one model call is asked to read.
///
/// This is the cap extraction started with, restored to the job it was always
/// right for. 24,000 was never a bad *call* size — it was a catastrophic
/// *document* size, and the defect was applying it to the wrong noun. Sized so
/// the reply fits every seeded model rather than only the frontier ones: the
/// output budget is scaled at one token per input character (measured, not
/// guessed — see `extract::starting_output_budget`), and the smallest per-model
/// output ceiling in the seeded catalog is 30,000 tokens. A slice at this size
/// asks for at most 24,000, which every catalog model can answer in one reply.
///
/// A chunk may exceed this only when a single unbreakable line does.
pub(super) const TARGET_CHARS: usize = 24_000;

/// The whole document ceiling — the point at which ingest stops reading and says
/// so.
///
/// Six times the 120,000 single-call cap this replaces, and thirty times the
/// 24,000 that motivated #2407. An upper bound still has to exist: chunks are
/// paid model calls, so a document is a real bill and an unbounded one is an
/// unbounded bill. But it is set where no instruction file lives — 720,000
/// characters is a few hundred pages — so the documents ingest exists for
/// (`AGENTS.md`, `CLAUDE.md`, contributor guides, specs) are covered whole with
/// an order of magnitude to spare, rather than being trimmed to fit.
///
/// A document that still exceeds it is truncated *and reported*, which is the
/// half the old cap was missing.
pub(super) const MAX_DOCUMENT_CHARS: usize = 720_000;

/// One slice of a document, and where in the document it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Chunk {
    /// The slice's text, verbatim. Concatenating these in order reproduces the
    /// bounded document.
    pub text: String,
    /// The heading path in effect where this slice begins, outermost first —
    /// `["Architecture", "Ports"]` for a slice inside `### Ports` under
    /// `## Architecture`. Empty for a document's preamble.
    ///
    /// Carried so a slice that continues an oversized section can tell the model
    /// what it is reading. A slice that opens on its own heading repeats it here,
    /// which is harmless and keeps the field's meaning one thing.
    pub path: Vec<String>,
}

/// Cap the document at [`MAX_DOCUMENT_CHARS`], returning the kept text and the
/// number of characters dropped.
///
/// Truncates on a line boundary when one is available, because a document cut
/// mid-line ends in a fragment the extractor would read as a whole claim.
pub(super) fn bound(content: &str) -> (String, usize) {
    let total = content.chars().count();
    if total <= MAX_DOCUMENT_CHARS {
        return (content.to_string(), 0);
    }
    let kept: String = content.chars().take(MAX_DOCUMENT_CHARS).collect();
    // Prefer the last complete line; fall back to the hard cut for a document
    // with no newline in 720,000 characters, where there is no boundary to find.
    let kept = match kept.rfind('\n') {
        Some(at) if at > 0 => kept[..at].to_string(),
        _ => kept,
    };
    let dropped = total - kept.chars().count();
    (kept, dropped)
}

/// How many characters of `content` extraction will not read.
pub(super) fn dropped_chars(content: &str) -> usize {
    bound(content).1
}

/// Divide `content` into slices of at most [`TARGET_CHARS`] characters, cutting
/// at heading seams where possible and inside a section only when one section is
/// larger than a whole slice.
///
/// Empty input yields no chunks: there is nothing to ask a model about.
pub(super) fn split(content: &str) -> Vec<Chunk> {
    if content.trim().is_empty() {
        return Vec::new();
    }
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut open: Option<Chunk> = None;

    for section in sections(content) {
        let len = section.text.chars().count();
        // The section fits in a slice of its own. Extend the open slice if it
        // still has room, so a document of many short sections does not become
        // one call per heading.
        if len <= TARGET_CHARS {
            match open.as_mut() {
                Some(chunk) if chunk.text.chars().count() + len <= TARGET_CHARS => {
                    chunk.text.push_str(&section.text);
                }
                _ => {
                    if let Some(chunk) = open.take() {
                        chunks.push(chunk);
                    }
                    open = Some(Chunk {
                        text: section.text,
                        path: section.path,
                    });
                }
            }
            continue;
        }
        // One section larger than a slice: flush what is open and divide the
        // section itself, every piece carrying the section's heading path.
        if let Some(chunk) = open.take() {
            chunks.push(chunk);
        }
        for piece in divide(&section.text) {
            chunks.push(Chunk {
                text: piece,
                path: section.path.clone(),
            });
        }
    }
    if let Some(chunk) = open.take() {
        chunks.push(chunk);
    }
    chunks
}

/// One heading-delimited section of a document.
#[derive(Debug)]
struct Section {
    text: String,
    path: Vec<String>,
}

/// Split `content` at ATX headings that are not inside a fenced code block.
///
/// Every character of the input lands in exactly one section, in order — the
/// preamble before the first heading included, as a section with an empty path.
fn sections(content: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let mut current = String::new();
    // The heading titles enclosing the *current* section, indexed by depth-1.
    let mut stack: Vec<String> = Vec::new();
    let mut path = Vec::new();
    let mut fence: Option<Fence> = None;

    for line in lines_with_endings(content) {
        if let Some(open) = fence.as_ref() {
            if closes(line, open) {
                fence = None;
            }
            current.push_str(line);
            continue;
        }
        if let Some(open) = opens_fence(line) {
            fence = Some(open);
            current.push_str(line);
            continue;
        }
        let Some((level, title)) = atx_heading(line) else {
            current.push_str(line);
            continue;
        };
        if !current.is_empty() {
            out.push(Section {
                text: std::mem::take(&mut current),
                path: path.clone(),
            });
        }
        stack.truncate(level - 1);
        // A heading that skips a level (`#` then `###`) leaves a gap; pad it so
        // depth stays the index and the path never silently reparents.
        while stack.len() < level - 1 {
            stack.push(String::new());
        }
        stack.push(title);
        path = stack.iter().filter(|t| !t.is_empty()).cloned().collect();
        current.push_str(line);
    }
    if !current.is_empty() {
        out.push(Section {
            text: current,
            path,
        });
    }
    out
}

/// Divide one oversized section into pieces of at most [`TARGET_CHARS`].
///
/// Cuts at a blank line first (a paragraph is the next-largest unit the document
/// declares), then at any line boundary, and only splits a line when one line is
/// itself longer than a whole slice — a minified or generated file, where there
/// is no boundary left to respect.
fn divide(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut open = String::new();
    // The character position in `open` just after the last blank line, or 0 when
    // there has been none — where a cut would fall on a paragraph boundary.
    let mut last_break: Option<usize> = None;

    for line in lines_with_endings(text) {
        let len = line.chars().count();
        if len > TARGET_CHARS {
            if !open.is_empty() {
                out.push(std::mem::take(&mut open));
                last_break = None;
            }
            out.extend(hard_split(line));
            continue;
        }
        if open.chars().count() + len > TARGET_CHARS {
            match last_break {
                // Cut at the paragraph boundary, carrying the trailing partial
                // paragraph into the next piece.
                Some(at) if at > 0 => {
                    let byte = char_to_byte(&open, at);
                    let tail = open[byte..].to_string();
                    open.truncate(byte);
                    out.push(std::mem::take(&mut open));
                    open = tail;
                }
                _ => out.push(std::mem::take(&mut open)),
            }
            last_break = None;
        }
        open.push_str(line);
        if line.trim().is_empty() {
            last_break = Some(open.chars().count());
        }
    }
    if !open.is_empty() {
        out.push(open);
    }
    out
}

/// Split a single line longer than a whole slice, on character boundaries.
fn hard_split(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut piece = String::new();
    for ch in line.chars() {
        if piece.chars().count() == TARGET_CHARS {
            out.push(std::mem::take(&mut piece));
        }
        piece.push(ch);
    }
    if !piece.is_empty() {
        out.push(piece);
    }
    out
}

/// The byte offset of character index `at` in `text`.
fn char_to_byte(text: &str, at: usize) -> usize {
    text.char_indices()
        .nth(at)
        .map_or(text.len(), |(byte, _)| byte)
}

/// Iterate lines *with* their line endings, so the pieces concatenate back to
/// the input. `str::lines` drops the terminator, which would silently rewrite
/// the document as it is sliced.
fn lines_with_endings(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let end = rest.find('\n').map_or(rest.len(), |at| at + 1);
        let (line, tail) = rest.split_at(end);
        rest = tail;
        Some(line)
    })
}

/// An open code fence: which character it uses and how many of them, since a
/// fence is closed only by at least as many of the same character.
struct Fence {
    marker: char,
    width: usize,
}

/// The fence this line opens, if it opens one.
fn opens_fence(line: &str) -> Option<Fence> {
    let trimmed = line.trim_start();
    for marker in ['`', '~'] {
        let width = trimmed.chars().take_while(|c| *c == marker).count();
        if width >= 3 {
            return Some(Fence { marker, width });
        }
    }
    None
}

/// Does this line close `open`? A closing fence carries no info string, but an
/// over-strict check would swallow the rest of the document, so the marker and
/// width are what is required.
fn closes(line: &str, open: &Fence) -> bool {
    let trimmed = line.trim();
    let width = trimmed.chars().take_while(|c| *c == open.marker).count();
    width >= open.width && trimmed.chars().all(|c| c == open.marker)
}

/// The level and title of an ATX heading line (`## Title`), or `None`.
fn atx_heading(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim_start();
    // Four spaces of indent is an indented code block, not a heading.
    if line.len() - trimmed.len() >= 4 {
        return None;
    }
    let level = trimmed.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &trimmed[level..];
    if !rest.starts_with([' ', '\t']) && !rest.trim().is_empty() {
        return None;
    }
    let title = rest.trim().trim_end_matches('#').trim().to_string();
    Some((level, title))
}

#[cfg(test)]
mod tests;
