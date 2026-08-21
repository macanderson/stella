// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The Session tab's incremental transcript fold.
//!
//! Split out of [`super`] rather than grown there: `views/session.rs` is one of
//! the crate's grandfathered god files and is closed to new lines (AGENTS.md
//! § God files), and the fold is a self-contained concern — it turns a
//! transcript plus a set of view flags into `Line`s and their row ranges, and
//! touches no `Frame`, no layout and no key routing. A pure move, plus the tail
//! cache described on [`SessionFold::tail`].

use std::collections::HashSet;
use std::ops::Range;

use ratatui::text::Line;

use crate::model::{FileState, TranscriptEntry};
use crate::render::{EntryView, entry_lines, reasoning_is_live, streaming_lines};

use super::{FoldPlan, digest_line};

/// Everything the settled prefix was folded under. Any change invalidates it,
/// so each term is a thing that can silently alter an already-rendered row:
/// the agent, thinking/expand-all overlays and their revision, the pane width,
/// how many entries have been evicted off the front, the file-mutation count
/// (an inline diff can go stale without anything being appended), the set of
/// folded turns (which changes on its own when a turn finishes under the
/// fold-all overlay), and how many leading entries have moved into the
/// terminal's scrollback (accessible mode — those must stop being drawn).
type FoldKey = (String, bool, bool, u64, usize, usize, u64, u64, usize);

/// Incremental transcript fold for the Session tab.
///
/// Everything before the last entry is *settled* — streaming deltas only ever
/// mutate the final entry — so settled entries fold (markdown, labels, wrap)
/// exactly once and are cached with their visual-row ranges; only the tail
/// entry re-folds per frame. The cache invalidates whole when anything that
/// changes how settled entries render changes: focused agent, the thinking
/// toggle, a ctrl+o expansion, the pane width, or the retention cap evicting
/// a chunk of the front (which shifts every retained index). This turns the
/// old O(whole-history) fold per frame into O(tail) — typing latency no
/// longer grows with session length.
#[derive(Debug, Clone, Default)]
pub struct SessionFold {
    // Visible to `super`'s tests, which assert the cache's tearing and eviction
    // behaviour on the fields themselves rather than through a rendered frame —
    // the states they pin (a half-extended prefix, a cleared key) are by
    // definition ones no frame ever draws.
    pub(super) key: Option<FoldKey>,
    settled: usize,
    pub(super) prefix: Vec<Line<'static>>,
    pub(super) entry_rows: Vec<Range<usize>>,
    /// The last entry's rows, plus the streaming preview under them.
    ///
    /// Re-folded per frame *unless* [`Self::tail_key`] says the last entry
    /// cannot have changed since the previous frame — see [`rewritable`] for
    /// what makes that answerable.
    tail: Vec<Line<'static>>,
    /// How many of [`Self::tail`]'s leading rows belong to the entry rather than
    /// to the streaming preview appended after it. The preview re-folds every
    /// frame (it is a live buffer by definition); the entry's rows above it are
    /// what the cache keeps.
    tail_entry_rows: usize,
    /// The fold key and entry index [`Self::tail`]'s entry rows were built
    /// under, when they are reusable at all. `None` re-folds — either nothing is
    /// cached, or the tail is an entry the model can still rewrite.
    tail_key: Option<(FoldKey, usize)>,
}

/// Whether the model can still rewrite `entry` in place, and so whether its
/// folded rows may be cached across frames.
///
/// Exactly two kinds can be: [`TranscriptEntry::Text`] and
/// [`TranscriptEntry::Reasoning`] coalesce streaming deltas into a buffer they
/// already pushed (`SessionModel`'s `push_text`, `push_reasoning`, and
/// `push_progress_line`, which rewrites a region of the last `Text`). Those are
/// the only three `last_mut()` call sites in the model; every other entry is
/// pushed whole and never touched again — front eviction moves indices, which
/// the `evicted` term of [`FoldKey`] already invalidates on.
///
/// That asymmetry is the whole cache. A [`TranscriptEntry::ToolResult`] is
/// immutable once folded *and* is the expensive entry — SPEC 6.4 wants its body
/// syntax-highlighted once when the event arrives, and while it sat in the live
/// tail it was re-lexed on every single frame instead. Keying on a content hash
/// or a length would be guessing at mutation; keying on "this kind cannot
/// mutate" is a property of the model, which is why this is a predicate over
/// kinds rather than a fingerprint over content.
fn rewritable(entry: &TranscriptEntry) -> bool {
    matches!(
        entry,
        TranscriptEntry::Text(_) | TranscriptEntry::Reasoning(_)
    )
}

impl SessionFold {
    /// Bring the cache up to date for this frame. `expand_all` is the
    /// no-selection ctrl+o overlay: every expandable entry folds as if
    /// individually expanded (it participates in the cache key, so toggling
    /// it invalidates exactly once).
    /// `streaming` is the in-flight `TextDelta` preview
    /// ([`SessionModel::streaming_text`](crate::model::SessionModel)): folded
    /// into the live tail after the last entry, so it re-wraps per frame
    /// like the tail does and vanishes without residue when the
    /// authoritative `Text` clears it — never a settled entry.
    #[allow(clippy::too_many_arguments)] // mirrors the fold's inputs one to one; a struct would just add a second shape
    pub(super) fn refresh(
        &mut self,
        agent: &str,
        transcript: &[TranscriptEntry],
        files: &[FileState],
        streaming: &str,
        thinking: bool,
        expanded: &HashSet<usize>,
        expand_all: bool,
        expanded_rev: u64,
        width: usize,
        plan: &FoldPlan,
        flushed: usize,
    ) {
        // Front-eviction shifts every retained index, so the settled prefix
        // no longer describes the entries now occupying 0..settled. The
        // marker's cumulative count grows on every pass, so keying on it
        // invalidates exactly when the front moves — the shrink check alone
        // misses an eviction whose survivors still outnumber `settled`.
        let evicted = match transcript.first() {
            Some(TranscriptEntry::Evicted { count }) => *count,
            _ => 0,
        };
        // A settled tool result's inline diff resolves against `files` at
        // fold time, and a later mutation can stale it (freshness gate in
        // `entry_lines`) without appending anything — the total mutation
        // count is the only fingerprint that moves, so it keys the cache.
        let file_gen: u64 = files.iter().map(|f| u64::from(f.changes)).sum();
        let key = (
            agent.to_string(),
            thinking,
            expand_all,
            expanded_rev,
            width,
            evicted,
            file_gen,
            plan.signature,
            flushed,
        );
        // Invalidation CLEARS the key; the commit happens after the fold loop
        // below. A panic inside `entry_lines` would otherwise leave `prefix`
        // extended with no matching `entry_rows` range and no `settled` bump,
        // and the next frame — key still matching — would resume at the same
        // index and double-append. With the commit moved after the loop, a
        // caught panic leaves `key = None` and the cache rebuilds from zero
        // (see `crate::panel_guard` for why that matters).
        if self.key.as_ref() != Some(&key) || self.settled > transcript.len().saturating_sub(1) {
            self.key = None;
            self.settled = 0;
            self.prefix.clear();
            self.entry_rows.clear();
        }
        let target = transcript.len().saturating_sub(1);
        while self.settled < target {
            let i = self.settled;
            let start = self.prefix.len();
            if i < flushed {
                // Already ordinary terminal output above this pane
                // (`accessible::Scrollback`). Drawing it again would have a
                // reader hear the conversation twice — once as it arrived in
                // scrollback, once as part of a pane that repaints. It still
                // gets a (zero-width) row range so every index-keyed
                // affordance — selection, search, scroll-into-view — stays
                // aligned with the transcript rather than shifting by
                // however much has been flushed.
                self.entry_rows.push(start..start);
                self.settled += 1;
                continue;
            }
            if let Some(d) = plan.digests.get(&i) {
                self.prefix.extend(digest_line(d, true, width));
            } else if plan.hides(i) {
                // Swallowed by the turn above. It still gets a row range —
                // the digest's — so that selecting, scrolling to, or
                // searching a hidden entry lands on the line standing in for
                // it rather than on nothing.
                let digest_rows = self
                    .entry_rows
                    .iter()
                    .rev()
                    .find(|r| !r.is_empty())
                    .cloned()
                    .unwrap_or(start..start);
                self.entry_rows.push(digest_rows);
                self.settled += 1;
                continue;
            } else {
                entry_lines(
                    &transcript[i],
                    EntryView::at(files, transcript, i),
                    thinking,
                    expand_all || expanded.contains(&i),
                    false,
                    width,
                    &mut self.prefix,
                );
            }
            // The tearing window: `prefix` is extended, `entry_rows` is not.
            #[cfg(test)]
            crate::panel_guard::fail_if_armed("session fold");
            self.entry_rows.push(start..self.prefix.len());
            self.settled += 1;
        }
        // The tail entry's own rows, kept from the previous frame when nothing
        // about them can have changed. `key` covers every view input; `target`
        // covers a new entry arriving; [`rewritable`] covers the entry being
        // rewritten under us. A tail that is `Text` or `Reasoning` is never
        // cached and re-folds exactly as it always did — it is a live buffer,
        // and it is also the cheap one.
        //
        // SPEC 6.4 asks that a body be highlighted once when its event arrives
        // and never per frame. Settled entries already were; the tail was not,
        // so a `read_file` result — the entry whose whole body is lexed — paid
        // to re-lex it on every repaint for as long as it was the newest thing
        // on screen. `crate::syntax::lex_count` is what holds this now.
        let tail_key = transcript
            .last()
            .is_some_and(|last| !rewritable(last))
            .then(|| (key.clone(), target));
        self.key = Some(key);
        let reuse = tail_key.is_some() && self.tail_key == tail_key;
        if reuse {
            self.tail.truncate(self.tail_entry_rows);
        } else {
            self.tail.clear();
            if let Some(last) = transcript.last() {
                // The tail obeys the same plan: a last entry inside a folded turn
                // must not reappear below the digest that already stands for it.
                if let Some(d) = plan.digests.get(&target) {
                    self.tail.extend(digest_line(d, true, width));
                } else if !plan.hides(target) {
                    entry_lines(
                        last,
                        EntryView::at(files, transcript, target),
                        thinking,
                        expand_all || expanded.contains(&target),
                        reasoning_is_live(transcript, streaming),
                        width,
                        &mut self.tail,
                    );
                }
            }
            self.tail_entry_rows = self.tail.len();
            self.tail_key = tail_key;
        }
        streaming_lines(streaming, files, thinking, width, &mut self.tail);
    }

    /// Total visual rows (settled prefix + live tail).
    pub fn total(&self) -> usize {
        self.prefix.len() + self.tail.len()
    }

    /// The visual-row range entry `idx` occupies (the live tail entry spans
    /// everything past the prefix).
    pub fn rows_of(&self, idx: usize) -> Range<usize> {
        if idx < self.entry_rows.len() {
            self.entry_rows[idx].clone()
        } else {
            self.prefix.len()..self.total()
        }
    }

    /// Materialize just the rows in `window` — ≤ one viewport of clones.
    pub(super) fn window_lines(&self, window: Range<usize>) -> Vec<Line<'static>> {
        window
            .filter_map(|r| {
                if r < self.prefix.len() {
                    self.prefix.get(r).cloned()
                } else {
                    self.tail.get(r - self.prefix.len()).cloned()
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SessionModel;
    use crate::syntax::lex_count;

    /// A `read_file` result whose body is a numbered Rust listing — the entry
    /// SPEC 6.4's budget is about, since every line of it is lexed.
    fn read_result(lines: usize) -> TranscriptEntry {
        let body: String = (1..=lines)
            .map(|i| format!("{i:>6}\tlet x{i} = {i};\n"))
            .collect();
        TranscriptEntry::ToolResult {
            call_id: "c1".into(),
            name: "read_file".into(),
            path: Some("src/lib.rs".into()),
            ok: true,
            summary: "ok".into(),
            full: body,
            duration_ms: 7,
            speculated: false,
            diff: None,
        }
    }

    fn refresh(fold: &mut SessionFold, transcript: &[TranscriptEntry], streaming: &str) {
        fold.refresh(
            "lead",
            transcript,
            &[],
            streaming,
            false,
            &HashSet::new(),
            true,
            0,
            100,
            &FoldPlan::default(),
            0,
        );
    }

    /// Fold `transcript` `frames` times the way the Session tab does, and
    /// report how many body lines were lexed doing it.
    fn lexed_over(transcript: &[TranscriptEntry], frames: usize) -> usize {
        let mut fold = SessionFold::default();
        let before = lex_count::snapshot();
        for _ in 0..frames {
            refresh(&mut fold, transcript, "");
        }
        lex_count::snapshot() - before
    }

    fn text_of(fold: &SessionFold) -> String {
        fold.window_lines(0..fold.total())
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// **The SPEC 6.4 budget, as a counter.**
    ///
    /// "Highlight once when the event arrives, cache `Vec<Line<'static>>`.
    /// Never highlight per frame." Settled entries always honoured that — they
    /// fold once into `prefix` and are never revisited. The **tail** did not:
    /// `refresh` cleared it and re-folded the last entry on every frame, so the
    /// newest tool result — the one a reader is actually watching — paid to
    /// re-lex its whole body on every repaint, for as long as it stayed the
    /// newest thing on screen.
    ///
    /// Measured as a ratio rather than an absolute, so the test says nothing
    /// about how many lines a preview happens to show: ten frames of an
    /// unchanged transcript must lex exactly what one frame lexes. On the old
    /// code this was ten times as many.
    #[test]
    fn an_unchanged_tail_is_highlighted_once_not_once_per_frame() {
        let transcript = [read_result(40)];
        let one = lexed_over(&transcript, 1);
        assert!(one > 0, "the fixture lexed nothing, so this proves nothing");
        let ten = lexed_over(&transcript, 10);
        assert_eq!(
            ten, one,
            "ten frames of an unchanged transcript lexed {ten} lines where one \
             frame lexed {one} — the live tail is being highlighted per frame \
             again (SPEC 6.4)"
        );
    }

    /// The cache is not a freeze: a new entry re-folds the tail, and the one it
    /// displaced settles into the prefix rather than vanishing.
    #[test]
    fn a_new_entry_still_reaches_the_frame() {
        let mut fold = SessionFold::default();
        let mut transcript = vec![read_result(4)];
        refresh(&mut fold, &transcript, "");
        let first = fold.total();
        transcript.push(read_result(4));
        refresh(&mut fold, &transcript, "");
        assert!(
            fold.total() > first,
            "a second result did not reach the frame: {} rows",
            fold.total()
        );
    }

    /// The streaming preview still re-folds under a cached tail.
    ///
    /// It rides *after* the tail entry's rows and is a live buffer by
    /// definition, so reusing the entry's rows must not take the preview with
    /// them — which is what `tail_entry_rows` exists to keep apart.
    #[test]
    fn a_cached_tail_still_refolds_the_streaming_preview_under_it() {
        let mut fold = SessionFold::default();
        let transcript = [read_result(4)];
        refresh(&mut fold, &transcript, "first draft");
        assert!(text_of(&fold).contains("first draft"));
        refresh(&mut fold, &transcript, "second draft");
        let text = text_of(&fold);
        assert!(
            text.contains("second draft"),
            "the preview froze under a cached tail:\n{text}"
        );
        assert!(
            !text.contains("first draft"),
            "the previous preview was never cleared:\n{text}"
        );
    }

    /// A tail the model **can** rewrite is never cached.
    ///
    /// `Text` and `Reasoning` coalesce streaming deltas into a buffer they
    /// already pushed, so their rows change without the transcript's length or
    /// any view flag moving. Caching one would freeze a streaming answer on its
    /// first delta — the worst thing this optimisation could do, and the reason
    /// [`rewritable`] is a statement about the model rather than a hash of the
    /// content.
    #[test]
    fn a_streaming_answer_is_never_frozen_by_the_cache() {
        let mut model = SessionModel::default();
        let mut fold = SessionFold::default();
        for delta in ["first", " second", " third"] {
            model.apply(&stella_protocol::AgentEvent::Text { text: delta.into() });
            refresh(&mut fold, &model.transcript, "");
        }
        let text = text_of(&fold);
        assert!(
            text.contains("third"),
            "the streaming answer froze on an earlier delta:\n{text}"
        );
    }
}
