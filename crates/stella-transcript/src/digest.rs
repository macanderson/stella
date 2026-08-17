//! Digests, chips and output folding — everything that turns a node into the
//! one line a collapsed reader sees.
//!
//! The rule that governs this module: **a collapsed digest is a summary, not
//! truncated content.** Cutting the first eighty characters off a tool result
//! and calling it a digest produces a line that is simultaneously unreadable and
//! misleading — it looks like the output and is not. So a digest is composed
//! from named fields (verb, object, duration, status, line count), and the only
//! place raw content appears is inside an expanded body.

use crate::model::{Accounting, Call, Output, Status, Step, ToolKind};

/// Lines of a result body shown before the fold control.
pub const HEAD_LINES: usize = 3;

/// Lines kept at the *tail* of a long output when the body is folded.
///
/// Errors live at the tail. A head-only fold on a 400-line build log hides the
/// one line the reader opened the transcript for, so a folded long output shows
/// both ends and elides the middle.
pub const TAIL_LINES: usize = 2;

/// Outputs at least this long get the head…tail treatment rather than head-only.
pub const TAIL_FOLD_THRESHOLD: usize = 12;

/// The visible portion of a folded output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFold {
    /// Every line of the body *after* echo suppression — what an expanded
    /// output renders.
    ///
    /// This exists because both renderers previously re-derived it from
    /// [`crate::model::Output::lines`] when expanded, and both got it wrong the
    /// same way: they printed the echoed first line they had just announced as
    /// hidden. One field, computed once, removes the opportunity.
    pub body: Vec<String>,
    /// Lines shown above the fold.
    pub head: Vec<String>,
    /// Lines shown below the fold, for a head…tail fold. Empty otherwise.
    pub tail: Vec<String>,
    /// How many lines the fold control hides.
    pub hidden: usize,
    /// Whether the first line was suppressed as an echo of the invocation.
    pub echo_hidden: bool,
}

impl OutputFold {
    /// Whether a `▸ N more lines` control is needed at all.
    #[must_use]
    pub fn has_more(&self) -> bool {
        self.hidden > 0
    }

    /// The fold control's label.
    #[must_use]
    pub fn more_label(&self) -> String {
        match self.hidden {
            1 => "▸ 1 more line".to_string(),
            n => format!("▸ {n} more lines"),
        }
    }
}

/// Compute what a folded output shows.
///
/// `invocation` is the call's header object; when the first line of the result
/// merely repeats it — the shell echoing back the command, a tool restating its
/// path — that line is dropped and `echo_hidden` is set, so the surface can say
/// so with a marker instead of silently losing a line.
#[must_use]
pub fn fold_output(output: &Output, invocation: &str) -> OutputFold {
    let mut lines: &[String] = &output.lines;
    let mut echo_hidden = false;
    if let Some(first) = lines.first()
        && is_echo(first, invocation)
    {
        lines = &lines[1..];
        echo_hidden = true;
    }

    let body = lines.to_vec();
    let total = body.len();
    if total <= HEAD_LINES {
        return OutputFold {
            head: body.clone(),
            body,
            tail: Vec::new(),
            hidden: output.clipped,
            echo_hidden,
        };
    }

    if total >= TAIL_FOLD_THRESHOLD {
        let head = body[..HEAD_LINES].to_vec();
        let tail = body[total - TAIL_LINES..].to_vec();
        return OutputFold {
            head,
            tail,
            hidden: total - HEAD_LINES - TAIL_LINES + output.clipped,
            body,
            echo_hidden,
        };
    }

    OutputFold {
        head: body[..HEAD_LINES].to_vec(),
        tail: Vec::new(),
        hidden: total - HEAD_LINES + output.clipped,
        body,
        echo_hidden,
    }
}

/// Whether an output line is just the invocation coming back.
///
/// Deliberately conservative: an exact match after trimming, or the invocation
/// behind a shell prompt sigil. A fuzzy rule would eventually eat a real first
/// line of output, and losing content is a worse failure than showing one
/// redundant line.
#[must_use]
pub fn is_echo(line: &str, invocation: &str) -> bool {
    let line = line.trim();
    let invocation = invocation.trim();
    if invocation.is_empty() || line.is_empty() {
        return false;
    }
    let stripped = line
        .strip_prefix("$ ")
        .or_else(|| line.strip_prefix("> "))
        .or_else(|| line.strip_prefix('$'))
        .unwrap_or(line)
        .trim();
    stripped == invocation
}

/// A right-aligned muted chip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    /// The chip's text.
    pub text: String,
    /// A tone token the renderer maps to a colour: `mute`, `ok`, `warn`, `err`,
    /// `cost`.
    pub tone: &'static str,
}

impl Chip {
    /// A muted chip, the default weight.
    #[must_use]
    pub fn mute(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: "mute",
        }
    }

    /// A chip carrying a tone.
    #[must_use]
    pub fn toned(text: impl Into<String>, tone: &'static str) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }
}

/// The chips for one step's accounting.
///
/// Order is fixed — duration, size, tokens, cost — because a right-aligned chip
/// row whose columns move between steps is unreadable at a glance, which is the
/// only reason to render chips rather than prose in the first place.
#[must_use]
pub fn step_chips(step: &Step) -> Vec<Chip> {
    let mut chips = Vec::new();
    if let Some(call) = &step.call {
        if call.speculated {
            chips.push(Chip::toned("⚡ spec", "warn"));
        }
        if call.duration_ms > 0 {
            chips.push(Chip::mute(format_duration(call.duration_ms)));
        }
        let lines = call.output.line_count();
        if lines > 0 && !call.tool.is_mutation() {
            chips.push(Chip::mute(format!("{lines} ln")));
        }
    }
    chips.extend(accounting_chips(step.accounting));
    chips
}

/// The chips for a token/cost rollup.
#[must_use]
pub fn accounting_chips(acc: Accounting) -> Vec<Chip> {
    let mut chips = Vec::new();
    if acc.tokens_in > 0 || acc.tokens_out > 0 {
        chips.push(Chip::mute(format!(
            "in {} · out {}",
            format_tokens(acc.tokens_in),
            format_tokens(acc.tokens_out)
        )));
    }
    if acc.cached_in > 0 {
        chips.push(Chip::toned("cache", "ok"));
    }
    if acc.micros > 0 {
        chips.push(Chip::toned(format_cost(acc.micros), "cost"));
    }
    chips
}

/// `7.0k`, `388`, `41.2k` — the shape a reader scans, not a raw integer.
#[must_use]
pub fn format_tokens(tokens: u64) -> String {
    if tokens < 1_000 {
        return tokens.to_string();
    }
    let whole = tokens / 1_000;
    let tenth = (tokens % 1_000) / 100;
    format!("{whole}.{tenth}k")
}

/// `226ms`, `1.4s`, `0:41` — resolution chosen by magnitude.
#[must_use]
pub fn format_duration(ms: u64) -> String {
    if ms < 1_000 {
        return format!("{ms}ms");
    }
    if ms < 60_000 {
        return format!("{}.{}s", ms / 1_000, (ms % 1_000) / 100);
    }
    format!("{}:{:02}", ms / 60_000, (ms % 60_000) / 1_000)
}

/// `$0.0008` from whole micro-dollars, with no float arithmetic anywhere in the
/// path — a rendered cost must be identical on every re-render.
#[must_use]
pub fn format_cost(micros: u64) -> String {
    format!("${}.{:04}", micros / 1_000_000, (micros % 1_000_000) / 100)
}

/// `0:04` — a step's elapsed offset from the start of its turn.
#[must_use]
pub fn format_offset(ms: u64) -> String {
    format!("{}:{:02}", ms / 60_000, (ms % 60_000) / 1_000)
}

/// Elapsed offsets for a turn's steps, with repeats dropped.
///
/// Two steps that land in the same second print the offset once; the second gets
/// an empty string. A column of identical timestamps is pure noise, and the
/// fixed-width gutter means dropping one costs no alignment.
#[must_use]
pub fn offsets(steps: &[Step]) -> Vec<String> {
    let mut out = Vec::with_capacity(steps.len());
    let mut previous: Option<String> = None;
    for step in steps {
        let text = format_offset(step.offset_ms);
        if previous.as_deref() == Some(text.as_str()) {
            out.push(String::new());
        } else {
            previous = Some(text.clone());
            out.push(text);
        }
    }
    out
}

/// What a mutation call did to the files it touched.
///
/// A bare `+N −M` is the right answer for an edit and the wrong one for a
/// creation or a deletion: `+0 −2` on a `delete_file` makes a reader parse
/// arithmetic to recover a fact the tool name already stated. So the label is
/// chosen by status, and the counts ride along for the surfaces that want them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
    /// Lines added across the call's files.
    pub added: usize,
    /// Lines removed across the call's files.
    pub removed: usize,
    /// The dominant file status — what the call did.
    pub kind: crate::model::FileStatus,
    /// How many files the call touched.
    pub files: usize,
}

impl Delta {
    /// The digest label.
    #[must_use]
    pub fn label(&self) -> String {
        use crate::model::FileStatus;
        match self.kind {
            FileStatus::Modified => format!("+{} −{}", self.added, self.removed),
            FileStatus::New if self.files > 1 => {
                format!("{} new files · +{}", self.files, self.added)
            }
            FileStatus::New => format!("new file · +{}", self.added),
            FileStatus::Deleted if self.files > 1 => {
                format!("{} files deleted · −{}", self.files, self.removed)
            }
            FileStatus::Deleted => format!("deleted · −{}", self.removed),
        }
    }
}

/// A step's one-line digest: verb, object, and the outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepDigest {
    /// The tool's name.
    pub verb: String,
    /// The object of the verb, elided to fit.
    pub object: String,
    /// The status the digest is tinted by.
    pub status: Status,
    /// What the call did to its files, for a mutation call.
    pub delta: Option<Delta>,
    /// The right-aligned chips.
    pub chips: Vec<Chip>,
    /// The gutter monogram.
    pub monogram: char,
    /// The tool's colour class.
    pub class: &'static str,
}

/// Build a step's digest.
#[must_use]
pub fn step_digest(step: &Step, object_width: usize) -> StepDigest {
    let Some(call) = &step.call else {
        return StepDigest {
            verb: "think".to_string(),
            object: String::new(),
            status: Status::Ok,
            delta: None,
            chips: accounting_chips(step.accounting),
            monogram: '·',
            class: "xx",
        };
    };
    let delta = if call.tool.is_mutation() && !call.files.is_empty() {
        let (added, removed) = call.line_delta();
        Some(Delta {
            added,
            removed,
            // The first file's status is the call's: a single-purpose tool
            // (invariant #9) cannot create one file and delete another.
            kind: call.files[0].status,
            files: call.files.len(),
        })
    } else {
        None
    };
    StepDigest {
        verb: call.tool.label().to_string(),
        object: elide(&call.header_object, object_width),
        status: call.status,
        delta,
        chips: step_chips(step),
        monogram: call.tool.monogram(),
        class: call.tool.class(),
    }
}

/// Shorten a string to `width` display cells with a middle ellipsis.
///
/// Middle rather than trailing because the two ends of an invocation are the
/// informative parts: `pdflatex … | grep Overfull` says what ran and what was
/// looked for, while a trailing cut says only `pdflatex -interaction=nonst…`.
#[must_use]
pub fn elide(text: &str, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width || width < 5 {
        return text.to_string();
    }
    let keep = width - 3;
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let mut out: String = chars[..head].iter().collect();
    out.push_str(" … ");
    out.extend(&chars[chars.len() - tail..]);
    out
}

/// The first sentence of a prose block — what it folds to.
///
/// A sentence ends at `.`, `!` or `?` followed by whitespace. When no such
/// boundary exists inside a reasonable measure the text is elided instead, so a
/// wall of unpunctuated reasoning still folds to one line.
#[must_use]
pub fn first_sentence(text: &str) -> String {
    let trimmed = text.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    for (i, &ch) in chars.iter().enumerate() {
        if matches!(ch, '.' | '!' | '?') && chars.get(i + 1).is_none_or(|next| next.is_whitespace())
        {
            return chars[..=i].iter().collect();
        }
    }
    elide(trimmed, 96)
}

/// A collapsed turn's digest line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnDigest {
    /// The turn's name.
    pub name: String,
    /// Status.
    pub status: Status,
    /// First line of the user's prompt.
    pub prompt_line: String,
    /// First line of the agent's answer, empty while the turn is running.
    pub answer_line: String,
    /// The rollup chips: step count, wall time, tokens, cost.
    pub chips: Vec<Chip>,
}

/// Build a turn's collapsed digest.
#[must_use]
pub fn turn_digest(turn: &crate::model::Turn, width: usize) -> TurnDigest {
    // A turn's wall time reads as `m:ss` rather than as a step's `1.4s`: turns
    // run for minutes, and a column of `41.0s`/`132.0s` is harder to compare at
    // a glance than `0:41`/`2:12`.
    let mut chips = vec![
        Chip::mute(format!("{} steps", turn.steps.len())),
        Chip::mute(format_offset(turn.duration_ms)),
    ];
    chips.extend(accounting_chips(turn.rollup()));
    TurnDigest {
        name: turn.name.clone(),
        status: turn.status,
        prompt_line: elide(first_line(&turn.prompt), width),
        answer_line: turn
            .answer
            .as_deref()
            .map(|a| elide(first_line(a), width))
            .unwrap_or_default(),
        chips,
    }
}

fn first_line(text: &str) -> &str {
    text.trim().lines().next().unwrap_or("").trim()
}

/// The `$` prompt bar for a `bash` call — but **only when the digest elided the
/// command**, so the full text is reachable without being stated twice.
///
/// This is deduplication rule 1 at its sharpest edge. The obvious rendering
/// shows a short command in the header and the full one in a prompt bar below,
/// and for a command that fits it prints the identical string twice — which is
/// the defect this whole crate exists to end. `shown` is what the header
/// actually rendered; when that is the whole invocation there is nothing left
/// for a prompt bar to add and it is not drawn.
#[must_use]
pub fn command_bar<'a>(call: &'a Call, shown: &str) -> Option<&'a str> {
    if !matches!(call.tool, ToolKind::Bash) {
        return None;
    }
    (shown.trim() != call.header_object.trim()).then_some(call.header_object.as_str())
}
