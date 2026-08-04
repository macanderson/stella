//! Finding your way around a long transcript: where the failures are, where
//! one turn ends and the next begins, and which entries match a query.
//!
//! The rail redesign made a session *readable*; this makes it **navigable**.
//! Those are different problems. Reading is served by what a row looks like —
//! a scannable margin, rhythm, a diff you can audit. Navigating is served by
//! being able to skip: to the thing that broke, past the turns you already
//! understand, to the one mention of `auth.rs` twenty minutes back. Scrolling
//! is the only answer a transcript has to any of those questions today, and
//! scrolling does not scale past a few screens.
//!
//! Everything here is a pure function of `&[TranscriptEntry]`. The deck owns
//! the keys and the state; this owns the answers, so both are testable without
//! a terminal.

use std::ops::Range;

use crate::model::TranscriptEntry;

/// Whether an entry represents something that went wrong.
///
/// Deliberately broader than "a tool returned non-zero": a judge that rejected
/// the work, a goal that went unmet, and a hard error are all things a reader
/// jumping through failures wants to land on. [`TranscriptEntry::Retry`] is
/// **not** included — a retry that eventually succeeded is noise on this path,
/// and one that didn't is followed by an entry that is a failure in its own
/// right.
#[must_use]
pub fn is_failure(entry: &TranscriptEntry) -> bool {
    use TranscriptEntry as E;
    matches!(
        entry,
        E::ToolResult { ok: false, .. }
            | E::Error { .. }
            | E::JudgeVerdict { passed: false, .. }
            | E::GoalVerdict { met: false, .. }
    )
}

/// The next failure at or after `from` when `forward`, or at or before it when
/// not — wrapping once so repeated presses cycle rather than dead-ending.
///
/// `from` is exclusive, so pressing the key again from a landed-on failure
/// advances instead of sticking. `None` only when the transcript holds no
/// failure at all, which is the one case where the key should do nothing
/// rather than move the selection somewhere arbitrary.
#[must_use]
pub fn seek_failure(
    transcript: &[TranscriptEntry],
    from: Option<usize>,
    forward: bool,
) -> Option<usize> {
    let n = transcript.len();
    if n == 0 {
        return None;
    }
    // With no selection, start just outside the end we are walking away from,
    // so the first press lands on the first (or last) failure rather than
    // skipping it.
    let start = match from {
        Some(i) => i,
        None if forward => n - 1,
        None => 0,
    };
    (1..=n)
        .map(|step| {
            if forward {
                (start + step) % n
            } else {
                (start + n - (step % n)) % n
            }
        })
        .find(|&i| is_failure(&transcript[i]))
}

/// The text fields a search matches against for one entry, **borrowed**.
///
/// Matching is over *content*, not over the rendered row: a reader searching
/// `auth.rs` means the path, not the pixels, and rendering can elide, wrap, or
/// truncate the very substring being looked for. This is also why the full
/// tool output is searched while only a preview line is drawn — the answer to
/// "did it ever mention a deadlock" lives in the ninety collapsed lines.
///
/// Field-wise rather than one joined string, because a joined string is a copy:
/// a tool result retains up to `OUTPUT_BUDGET` (16 KiB) of output, the search
/// bar re-runs on every keystroke, and the transcript holds up to
/// `MAX_TRANSCRIPT_ENTRIES` of them. Concatenating to scan meant a keystroke
/// could copy tens of megabytes it then threw away.
#[must_use]
pub fn entry_fields(entry: &TranscriptEntry) -> Vec<&str> {
    use TranscriptEntry as E;
    match entry {
        E::User(t) | E::Text(t) | E::Reasoning(t) => vec![t],
        E::ToolStart {
            name, input, raw, ..
        } => vec![name, input, raw],
        E::ToolResult {
            name,
            summary,
            full,
            ..
        } => vec![name, summary, full],
        E::Retry { reason, .. } => vec![reason],
        E::ProviderFallback { from, to, reason } => vec![from, to, reason],
        E::ContextRecall { labels, .. } => labels.iter().map(String::as_str).collect(),
        E::ContextWrite { provider, .. } => vec![provider],
        E::MediaComplete { label, path, .. } => vec![label, path],
        E::JudgeVerdict { summary, .. } | E::ScopeReview { summary, .. } => vec![summary],
        E::GoalVerdict { reasoning, .. } => vec![reasoning],
        // The child's id and task are the searchable part; its summary never
        // reaches the transcript, by design.
        E::SubAgent {
            agent_id,
            instruction_preview,
            ..
        } => vec![agent_id, instruction_preview],
        E::AskUser { question, .. } => vec![question],
        // The tool name is the only free text on the row; the two counts are
        // numbers a search over the transcript has no way to ask for.
        E::HunkReview { tool, .. } => vec![tool],
        E::Commit { sha, message } => vec![sha, message],
        E::Pr { url, .. } => vec![url],
        E::TaskUpdate { active, .. } => active.as_deref().into_iter().collect(),
        E::Error { message, .. } => vec![message],
        E::Complete { model, .. } => vec![model],
        E::Evicted { .. }
        | E::Stage(_)
        | E::Compaction { .. }
        | E::BudgetTick { .. }
        | E::MediaProgress { .. } => Vec::new(),
    }
}

/// Case-insensitive substring test that allocates nothing when both sides are
/// ASCII — which every realistic search query and almost every tool payload is.
/// `needle` must already be lowercased.
///
/// The fallback still lowercases the haystack, because ASCII case folding is
/// wrong for anything else; it just no longer runs on every entry of every
/// transcript on every keystroke.
fn contains_ci(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if !haystack.is_ascii() || !needle.is_ascii() {
        return haystack.to_lowercase().contains(needle);
    }
    let (hay, ndl) = (haystack.as_bytes(), needle.as_bytes());
    hay.len() >= ndl.len()
        && hay
            .windows(ndl.len())
            .any(|w| w.iter().zip(ndl).all(|(a, b)| a.to_ascii_lowercase() == *b))
}

/// Entry indices whose text contains `query`, case-insensitively.
///
/// Case-insensitive without a case-sensitivity toggle: in a transcript the
/// query is almost always a path, an identifier, or an error fragment, and
/// guessing the casing of something you are trying to *find* is a poor thing
/// to require. An empty query matches nothing rather than everything — an
/// empty search bar should not claim every row.
#[must_use]
pub fn search_hits(transcript: &[TranscriptEntry], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return Vec::new();
    }
    let needle = query.to_lowercase();
    transcript
        .iter()
        .enumerate()
        .filter(|(_, e)| entry_fields(e).iter().any(|f| contains_ci(f, &needle)))
        .map(|(i, _)| i)
        .collect()
}

/// Byte ranges of every case-insensitive occurrence of `query` in `haystack`.
/// Used to underline the matched substring inside a rendered row.
#[must_use]
pub fn match_ranges(haystack: &str, query: &str) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    // Lowercasing can change byte length for some scripts, which would make
    // offsets found in the lowered copy meaningless against the original. Fall
    // back to exact matching when it does — a rare miss beats a mis-sliced
    // string, and `char_indices` keeps every offset on a boundary regardless.
    let (hay, needle) = (haystack.to_lowercase(), query.to_lowercase());
    let (hay, needle) = if hay.len() == haystack.len() {
        (hay, needle)
    } else {
        (haystack.to_string(), query.to_string())
    };
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(found) = hay[at..].find(&needle) {
        let start = at + found;
        let end = start + needle.len();
        if !haystack.is_char_boundary(start) || !haystack.is_char_boundary(end) {
            break;
        }
        out.push(start..end);
        at = end.max(start + 1);
        if at >= hay.len() {
            break;
        }
    }
    out
}

/// One turn: the prompt that started it and everything it produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    /// Entry range, `start` being the [`TranscriptEntry::User`] that opened it.
    pub rows: Range<usize>,
    /// Whether the turn has come to rest. Only a settled turn may be folded —
    /// collapsing work that is still arriving would hide the very thing the
    /// reader is watching.
    pub finished: bool,
}

/// Split a transcript into turns.
///
/// A turn opens at a user prompt and runs to the next one. It is *finished*
/// when it holds a [`TranscriptEntry::Complete`] or an
/// [`TranscriptEntry::Error`], or when a later turn has already opened —
/// whatever happened to it, it is over.
///
/// Two shapes deliberately do not open a turn. A **steering** message arrives
/// mid-turn as a `User` entry marked with a prefix (`model.rs` writes it);
/// treating it as a boundary would saw one turn in half at exactly the moment
/// the reader most wants it whole. And anything before the first prompt —
/// an `Evicted` marker, a resumed session's replayed tail — belongs to no turn
/// and is left unfolded rather than being adopted by the first one.
#[must_use]
pub fn turns(transcript: &[TranscriptEntry]) -> Vec<Turn> {
    let starts: Vec<usize> = transcript
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, TranscriptEntry::User(t) if !is_steering(t)))
        .map(|(i, _)| i)
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(k, &start)| {
            let end = starts.get(k + 1).copied().unwrap_or(transcript.len());
            let last = k + 1 == starts.len();
            let settled = transcript[start..end].iter().any(|e| {
                matches!(
                    e,
                    TranscriptEntry::Complete { .. } | TranscriptEntry::Error { .. }
                )
            });
            Turn {
                rows: start..end,
                finished: settled || !last,
            }
        })
        .collect()
}

/// The marker `SessionModel` puts on a prompt that arrived mid-turn.
const STEERING_PREFIX: &str = "(steered mid-turn)";

fn is_steering(text: &str) -> bool {
    text.starts_with(STEERING_PREFIX)
}

/// The turn containing `idx`, if any.
#[must_use]
pub fn turn_at(turns: &[Turn], idx: usize) -> Option<&Turn> {
    turns.iter().find(|t| t.rows.contains(&idx))
}

/// What a folded turn says about itself in one line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TurnDigest {
    /// The prompt, flattened to a single line.
    pub prompt: String,
    /// Tool calls made.
    pub tools: usize,
    /// Distinct files mutated.
    pub files: usize,
    /// Failures inside the turn — the number that decides whether a reader
    /// unfolds it.
    pub failures: usize,
    /// Total tool time, milliseconds.
    pub duration_ms: u64,
}

/// Summarize a turn for its folded one-line form.
///
/// The counts are chosen to answer "can I skip this?" without unfolding: what
/// it was asked to do, how much it did, whether anything broke. Files counts
/// *distinct* paths, since eight edits to one file is one file's worth of
/// change to review, not eight.
#[must_use]
pub fn digest(transcript: &[TranscriptEntry], turn: &Turn) -> TurnDigest {
    let mut d = TurnDigest::default();
    let mut paths: Vec<&str> = Vec::new();
    for entry in &transcript[turn.rows.clone()] {
        match entry {
            TranscriptEntry::User(t) if d.prompt.is_empty() => {
                d.prompt = t.split_whitespace().collect::<Vec<_>>().join(" ");
            }
            TranscriptEntry::ToolStart { .. } => d.tools += 1,
            TranscriptEntry::ToolResult {
                duration_ms, diff, ..
            } => {
                d.duration_ms += duration_ms;
                if let Some(r) = diff
                    && !paths.contains(&r.path.as_str())
                {
                    paths.push(&r.path);
                }
            }
            _ => {}
        }
        if is_failure(entry) {
            d.failures += 1;
        }
    }
    d.files = paths.len();
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::InlineDiffRef;

    fn tool_result(ok: bool) -> TranscriptEntry {
        TranscriptEntry::ToolResult {
            call_id: "c".into(),
            name: "cmd".into(),
            ok,
            summary: "s".into(),
            full: "f".into(),
            duration_ms: 10,
            speculated: false,
            diff: None,
        }
    }

    fn edit(path: &str, seq: u32) -> TranscriptEntry {
        TranscriptEntry::ToolResult {
            call_id: "c".into(),
            name: "edit_file".into(),
            ok: true,
            summary: "s".into(),
            full: "f".into(),
            duration_ms: 30,
            speculated: false,
            diff: Some(InlineDiffRef {
                path: path.into(),
                seq,
            }),
        }
    }

    /// Repeated presses cycle rather than dead-ending at the last failure —
    /// the key is "show me the next thing that broke", and a reader who has
    /// seen them all should come back round, not press a dead key.
    #[test]
    fn seek_failure_walks_both_ways_and_wraps() {
        let t = vec![
            TranscriptEntry::Text("a".into()),
            tool_result(false),
            TranscriptEntry::Text("b".into()),
            tool_result(false),
        ];
        assert_eq!(seek_failure(&t, Some(1), true), Some(3), "advances");
        assert_eq!(seek_failure(&t, Some(3), true), Some(1), "wraps forward");
        assert_eq!(seek_failure(&t, Some(1), false), Some(3), "wraps backward");
        // With nothing selected each direction enters from the far side, so
        // the first press lands on the nearest failure from where the reader
        // actually is: forward finds the first, backward finds the last.
        assert_eq!(seek_failure(&t, None, true), Some(1), "forward → first");
        assert_eq!(seek_failure(&t, None, false), Some(3), "backward → last");
    }

    /// A transcript with nothing wrong in it must leave the selection alone
    /// rather than moving it somewhere arbitrary.
    #[test]
    fn seek_failure_is_none_when_nothing_failed() {
        let t = vec![TranscriptEntry::Text("a".into()), tool_result(true)];
        assert_eq!(seek_failure(&t, None, true), None);
        assert_eq!(seek_failure(&t, Some(0), false), None);
    }

    /// A retry is not a failure: one that recovered is noise, and one that did
    /// not is followed by an entry that qualifies on its own.
    #[test]
    fn retries_are_not_failures_but_verdicts_are() {
        assert!(!is_failure(&TranscriptEntry::Retry {
            attempt: 2,
            reason: "rate limited".into(),
        }));
        assert!(is_failure(&TranscriptEntry::JudgeVerdict {
            passed: false,
            summary: "no".into(),
            deterministic: true,
        }));
        assert!(!is_failure(&TranscriptEntry::JudgeVerdict {
            passed: true,
            summary: "ok".into(),
            deterministic: true,
        }));
    }

    /// Search reaches the collapsed body of a tool result, which is the whole
    /// point: the answer to "was a deadlock ever mentioned" lives in the
    /// ninety lines the row does not draw.
    #[test]
    fn search_matches_hidden_output_case_insensitively() {
        let t = vec![
            TranscriptEntry::Text("nothing here".into()),
            TranscriptEntry::ToolResult {
                call_id: "c".into(),
                name: "cmd".into(),
                ok: true,
                summary: "Compiling".into(),
                full: "line one\nDEADLOCK detected\nline three".into(),
                duration_ms: 5,
                speculated: false,
                diff: None,
            },
        ];
        assert_eq!(search_hits(&t, "deadlock"), vec![1]);
        assert_eq!(search_hits(&t, "DeAdLoCk"), vec![1]);
        assert!(
            search_hits(&t, "").is_empty(),
            "an empty query claims nothing"
        );
    }

    /// The ASCII fast path must agree with the unicode fallback in every
    /// direction — it is the one that runs on essentially every keystroke, so
    /// a divergence would be a silently wrong search, not a slow one.
    #[test]
    fn case_insensitive_matching_agrees_across_the_ascii_and_unicode_paths() {
        assert!(contains_ci("Compiling stella-tui", "stella"));
        assert!(contains_ci("PANIC at src/x.rs", "panic at"));
        assert!(!contains_ci("nothing here", "panic"));
        assert!(!contains_ci("ab", "abc"), "needle longer than haystack");
        // Non-ASCII on either side falls back to full lowercasing.
        assert!(contains_ci("ÉCHEC du build", "échec"));
        assert!(contains_ci("straße", "stra"));
        assert!(!contains_ci("ÉCHEC", "echec"));
    }

    /// Searching reaches every field of an entry without ever concatenating
    /// them — a tool result's 16 KiB body must not be copied to be scanned.
    #[test]
    fn every_searchable_field_of_an_entry_is_reachable() {
        let start = TranscriptEntry::ToolStart {
            call_id: "c".into(),
            name: "edit_file".into(),
            input: "src/auth.rs".into(),
            raw: r#"{"path":"src/auth.rs"}"#.into(),
            path: Some("src/auth.rs".into()),
        };
        assert_eq!(
            entry_fields(&start),
            vec!["edit_file", "src/auth.rs", r#"{"path":"src/auth.rs"}"#]
        );
        let t = vec![start];
        for query in ["edit_file", "AUTH.rs", r#""path""#] {
            assert_eq!(search_hits(&t, query), vec![0], "no hit for {query:?}");
        }
        assert!(
            entry_fields(&TranscriptEntry::Stage(stella_protocol::StageKind::Verify)).is_empty()
        );
    }

    #[test]
    fn match_ranges_finds_every_occurrence_on_char_boundaries() {
        assert_eq!(match_ranges("aXbXc", "x"), vec![1..2, 3..4]);
        assert_eq!(match_ranges("héllo héllo", "héllo").len(), 2);
        assert!(match_ranges("abc", "").is_empty());
        // Overlapping-prefix queries must still terminate.
        assert_eq!(match_ranges("aaaa", "aa"), vec![0..2, 2..4]);
    }

    /// Steering does not open a turn. Splitting there would saw one turn in
    /// half at exactly the moment a reader most wants it whole.
    #[test]
    fn steering_does_not_start_a_new_turn() {
        let t = vec![
            TranscriptEntry::User("do the thing".into()),
            TranscriptEntry::User("(steered mid-turn) actually stop".into()),
            TranscriptEntry::Complete {
                model: "m".into(),
                cost_usd: 0.1,
            },
        ];
        let turns = turns(&t);
        assert_eq!(turns.len(), 1, "one turn, not two: {turns:?}");
        assert_eq!(turns[0].rows, 0..3);
        assert!(turns[0].finished);
    }

    /// The turn still arriving is unfinished; every earlier one is over
    /// whatever happened to it, because a later prompt already opened.
    #[test]
    fn only_the_last_turn_can_be_unfinished() {
        let t = vec![
            TranscriptEntry::User("one".into()),
            TranscriptEntry::Text("working".into()),
            TranscriptEntry::User("two".into()),
            TranscriptEntry::Text("still working".into()),
        ];
        let turns = turns(&t);
        assert_eq!(turns.len(), 2);
        assert!(turns[0].finished, "superseded by a later prompt");
        assert!(!turns[1].finished, "no Complete and nothing after it");
    }

    /// An error ends a turn as surely as a completion does — an aborted turn
    /// is finished, not perpetually in flight.
    #[test]
    fn an_error_finishes_a_turn() {
        let t = vec![
            TranscriptEntry::User("one".into()),
            TranscriptEntry::Error {
                message: "provider down".into(),
                retryable: false,
            },
        ];
        assert!(turns(&t)[0].finished);
    }

    /// Entries before the first prompt belong to no turn — a replayed tail or
    /// an eviction marker must not be adopted by the first turn and folded
    /// away with it.
    #[test]
    fn entries_before_the_first_prompt_belong_to_no_turn() {
        let t = vec![
            TranscriptEntry::Evicted { count: 400 },
            TranscriptEntry::Text("replayed".into()),
            TranscriptEntry::User("one".into()),
        ];
        let turns = turns(&t);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].rows, 2..3);
        assert!(turn_at(&turns, 0).is_none());
    }

    /// Files counts distinct paths: eight edits to one file is one file's
    /// worth of change to review.
    #[test]
    fn digest_counts_distinct_files_and_all_failures() {
        let t = vec![
            TranscriptEntry::User("  fix   the   bug  ".into()),
            TranscriptEntry::ToolStart {
                call_id: "c".into(),
                name: "edit_file".into(),
                input: "a.rs".into(),
                raw: "{}".into(),
                path: Some("a.rs".into()),
            },
            edit("a.rs", 1),
            edit("a.rs", 2),
            edit("b.rs", 1),
            tool_result(false),
        ];
        let turns = turns(&t);
        let d = digest(&t, &turns[0]);
        assert_eq!(d.prompt, "fix the bug", "whitespace flattened");
        assert_eq!(d.tools, 1);
        assert_eq!(d.files, 2, "a.rs twice counts once");
        assert_eq!(d.failures, 1);
        assert_eq!(d.duration_ms, 30 + 30 + 30 + 10);
    }
}
