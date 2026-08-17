//! The information model a transcript is rendered from.
//!
//! One tree, two renderers. The shape is deliberately *not* the journal's flat
//! event stream: the defect this crate exists to end is that a tool call and
//! its result were siblings in that stream, so every surface printed the tool's
//! name twice and the command up to three times. Here a [`Call`] owns its
//! [`Output`] — there is no way to render one without the other, and no second
//! place for the invocation string to live.
//!
//! Nesting, top down:
//!
//! ```text
//! Run
//! └─ Turn      one user request → agent response cycle
//!    ├─ Prompt   the user's message (role badge: YOU)
//!    ├─ Prose    assistant reasoning between calls (role badge: AGENT)
//!    ├─ Step     one model iteration; carries token/cost accounting
//!    │  └─ Call  one tool invocation — call + result are ONE node
//!    └─ Answer   the agent's final message
//! ```

use serde::{Deserialize, Serialize};

/// A stable address for one foldable node, so a fold state survives a re-render
/// and a keyboard cursor can name what it is on.
///
/// Indices are positional within the parent, which is sound because a rendered
/// transcript is immutable: a run that is still streaming only ever appends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum NodeId {
    /// The `n`th turn of the run.
    Turn(usize),
    /// The `s`th step of the `t`th turn.
    Step {
        /// Turn index.
        turn: usize,
        /// Step index within the turn.
        step: usize,
    },
    /// The output body of the `s`th step of the `t`th turn.
    Output {
        /// Turn index.
        turn: usize,
        /// Step index within the turn.
        step: usize,
    },
    /// The `p`th prose block of the `t`th turn.
    Prose {
        /// Turn index.
        turn: usize,
        /// Prose-block index within the turn.
        prose: usize,
    },
    /// The `f`th file diff inside the `s`th step of the `t`th turn.
    File {
        /// Turn index.
        turn: usize,
        /// Step index within the turn.
        step: usize,
        /// File index within the step's call.
        file: usize,
    },
    /// The `h`th hunk of the `f`th file diff of a step.
    Hunk {
        /// Turn index.
        turn: usize,
        /// Step index within the turn.
        step: usize,
        /// File index within the step's call.
        file: usize,
        /// Hunk index within the file diff.
        hunk: usize,
    },
}

impl NodeId {
    /// The turn this node lives in — every node has exactly one.
    #[must_use]
    pub fn turn(self) -> usize {
        match self {
            NodeId::Turn(t)
            | NodeId::Step { turn: t, .. }
            | NodeId::Output { turn: t, .. }
            | NodeId::Prose { turn: t, .. }
            | NodeId::File { turn: t, .. }
            | NodeId::Hunk { turn: t, .. } => t,
        }
    }

    /// A DOM-safe / stable string form, used as an HTML `id` and as the key a
    /// TUI cursor round-trips through a saved session.
    #[must_use]
    pub fn key(self) -> String {
        match self {
            NodeId::Turn(t) => format!("t{t}"),
            NodeId::Step { turn, step } => format!("t{turn}s{step}"),
            NodeId::Output { turn, step } => format!("t{turn}s{step}o"),
            NodeId::Prose { turn, prose } => format!("t{turn}p{prose}"),
            NodeId::File { turn, step, file } => format!("t{turn}s{step}f{file}"),
            NodeId::Hunk {
                turn,
                step,
                file,
                hunk,
            } => format!("t{turn}s{step}f{file}h{hunk}"),
        }
    }
}

/// How a turn, step or call ended.
///
/// The rendering contract attached to this is asymmetric on purpose: success is
/// quiet (a small green tick, a digest that collapses itself), failure is loud
/// (a tinted digest that [pins itself open](crate::fold::FoldState) and stays
/// open after the run completes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Finished, nothing to look at.
    Ok,
    /// Finished, but something the reader should see — a non-fatal warning
    /// count, a partial result.
    Warn,
    /// Failed. Renders loud and stays expanded.
    Error,
    /// Still executing. Renders expanded so the reader watches it happen.
    Running,
}

impl Status {
    /// Whether a node in this state refuses to be folded away.
    ///
    /// This is the whole of the "a failed step remains expanded after run
    /// completion" acceptance check: it is a property of the *status*, not of
    /// when the fold was computed, so no later collapse-everything pass can
    /// take it back.
    #[must_use]
    pub fn pins_open(self) -> bool {
        matches!(self, Status::Error | Status::Running)
    }

    /// The single glyph both renderers use.
    #[must_use]
    pub fn glyph(self) -> &'static str {
        match self {
            Status::Ok => "✓",
            Status::Warn => "⚠",
            Status::Error => "✗",
            Status::Running => "…",
        }
    }

    /// The stable class/color token: `ok`, `warn`, `err`, `run`.
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Error => "err",
            Status::Running => "run",
        }
    }
}

/// Which tool a call invoked, as far as *rendering* is concerned.
///
/// Deliberately not the tool registry's enumeration: the registry's job is to
/// dispatch, this one's is to pick a color, a monogram and a body renderer. A
/// tool this crate has never heard of is [`ToolKind::Other`] and renders as a
/// plain command + output, which is the correct degradation — an MCP server's
/// tool must not render as an error just because it is unknown here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// A shell command. Body renders with a `$` prompt bar.
    Bash,
    /// A file read. Body renders as a numbered excerpt.
    ReadFile,
    /// A new file. Body renders as an all-green diff.
    WriteFile,
    /// An in-place edit. Body renders as a unified diff with word highlights.
    EditFile,
    /// A removal. Body renders a red header and folds the final contents.
    DeleteFile,
    /// A search.
    Search,
    /// Anything else — MCP tools, task-board tools, custom script tools.
    Other(String),
}

impl ToolKind {
    /// Map a wire tool name onto a render kind.
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name {
            "bash" => ToolKind::Bash,
            "read_file" => ToolKind::ReadFile,
            "write_file" => ToolKind::WriteFile,
            "edit_file" => ToolKind::EditFile,
            "delete_file" => ToolKind::DeleteFile,
            "search" => ToolKind::Search,
            other => ToolKind::Other(other.to_string()),
        }
    }

    /// The name as the reader sees it in the call header.
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            ToolKind::Bash => "bash",
            ToolKind::ReadFile => "read_file",
            ToolKind::WriteFile => "write_file",
            ToolKind::EditFile => "edit_file",
            ToolKind::DeleteFile => "delete_file",
            ToolKind::Search => "search",
            ToolKind::Other(name) => name,
        }
    }

    /// The stable short class both renderers key their accent color off.
    ///
    /// Short because the TUI spine has three columns to spend and the HTML uses
    /// it as a CSS class: `bash`, `rf`, `wf`, `ef`, `df`, `sr`, `xx`.
    #[must_use]
    pub fn class(&self) -> &'static str {
        match self {
            ToolKind::Bash => "bash",
            ToolKind::ReadFile => "rf",
            ToolKind::WriteFile => "wf",
            ToolKind::EditFile => "ef",
            ToolKind::DeleteFile => "df",
            ToolKind::Search => "sr",
            ToolKind::Other(_) => "xx",
        }
    }

    /// The one-character monogram in the timeline spine gutter.
    ///
    /// This is what makes the "skimmable by gutter icons alone" acceptance
    /// check achievable: a 40-step run has a *shape* — a column of `$` with two
    /// `±` in it reads as "mostly ran things, edited twice" without a word of
    /// the transcript being read.
    #[must_use]
    pub fn monogram(&self) -> char {
        match self {
            ToolKind::Bash => '$',
            ToolKind::ReadFile => '◇',
            ToolKind::WriteFile => '◆',
            ToolKind::EditFile => '±',
            ToolKind::DeleteFile => '✗',
            ToolKind::Search => '⌕',
            ToolKind::Other(_) => '•',
        }
    }

    /// Whether this call's body is a diff rather than free text.
    #[must_use]
    pub fn is_mutation(&self) -> bool {
        matches!(
            self,
            ToolKind::WriteFile | ToolKind::EditFile | ToolKind::DeleteFile
        )
    }
}

/// One key/value row behind the `args` toggle.
///
/// Only the arguments *not already shown* in the call header reach this — see
/// [`Call::extra_args`]. An empty vector means the header said everything and
/// the toggle is not rendered at all, which is the dedup rule's teeth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgRow {
    /// The argument name.
    pub key: String,
    /// Its value, already flattened to one display string by the caller.
    pub value: String,
}

/// Per-step token and money accounting.
///
/// Rendered as right-aligned muted chips, subordinate to the digest — never as
/// prose, and never at the same weight as the work it cost.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accounting {
    /// Input tokens billed.
    pub tokens_in: u64,
    /// Output tokens billed.
    pub tokens_out: u64,
    /// Input tokens served from the provider's prompt cache.
    pub cached_in: u64,
    /// Cost in whole micro-dollars, so the model carries no float.
    ///
    /// Integer because a rendered cost is compared and summed across steps, and
    /// a `f64` sum of forty steps is not reproducible across a re-render.
    pub micros: u64,
}

impl Accounting {
    /// Sum for a turn/run rollup.
    #[must_use]
    pub fn merged(self, other: Self) -> Self {
        Self {
            tokens_in: self.tokens_in + other.tokens_in,
            tokens_out: self.tokens_out + other.tokens_out,
            cached_in: self.cached_in + other.cached_in,
            micros: self.micros + other.micros,
        }
    }

    /// Whether there is anything worth rendering a chip for.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self == Self::default()
    }
}

/// A tool call's result body, before folding.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Output {
    /// The result text, split into lines by the caller (no trailing newline).
    pub lines: Vec<String>,
    /// Lines the producer dropped before this crate ever saw them, if the
    /// transport clipped the body. Rendered as an honest "clipped" marker so a
    /// reader never mistakes a transport limit for the end of the output.
    pub clipped: usize,
}

impl Output {
    /// Build from a raw result blob.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        Self {
            lines: text.lines().map(str::to_string).collect(),
            clipped: 0,
        }
    }

    /// Total lines the reader is told about.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.lines.len() + self.clipped
    }

    /// Whether there is nothing at all to show.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.iter().all(|l| l.trim().is_empty()) && self.clipped == 0
    }
}

/// One file's before/after, from which a rendered diff is derived.
///
/// The *inputs*, not the rendered hunks: computing the diff is
/// [`crate::file_diff`]'s job, and keeping the model at this level is what lets
/// a caller feed a `write_file` (empty `before`) and a `delete_file` (empty
/// `after`) through the same path as an `edit_file`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    /// Display path, workspace-relative.
    pub path: String,
    /// Contents before the call. Empty for a new file.
    pub before: String,
    /// Contents after the call. Empty for a deletion.
    pub after: String,
    /// What happened to the file as a whole.
    pub status: FileStatus,
}

/// The status dot on a file header bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    /// Existed before, exists after.
    Modified,
    /// Created by this call.
    New,
    /// Removed by this call.
    Deleted,
}

impl FileStatus {
    /// The stable class token: `mod`, `new`, `gone`.
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            FileStatus::Modified => "mod",
            FileStatus::New => "new",
            FileStatus::Deleted => "gone",
        }
    }
}

/// One tool invocation and its result, as a single node.
///
/// The invariant this type exists to enforce: **the invocation string lives
/// here once**. `header_object` is what the call header prints; `extra_args`
/// is everything the header did not say. There is no third field a renderer
/// could reach for, which is why "no command string appears more than once per
/// call" is structurally true rather than a review question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    /// Which tool ran.
    pub tool: ToolKind,
    /// The object of the verb: the command line for `bash`, the path for a file
    /// tool, the query for a search. Rendered once, in the header.
    pub header_object: String,
    /// Every other argument, for the `args` toggle. Empty means no toggle.
    pub args: Vec<ArgRow>,
    /// The result body.
    pub output: Output,
    /// Files this call changed, if any.
    pub files: Vec<FileChange>,
    /// How the call ended.
    pub status: Status,
    /// Wall time.
    pub duration_ms: u64,
    /// Whether the engine ran this speculatively — renders as a `⚡ spec`
    /// badge, never as a sentence.
    pub speculated: bool,
}

impl Call {
    /// The arguments that are *not* already visible in the header.
    ///
    /// Deduplication rule 1 in code: an `args` row whose value is the string the
    /// header already printed is dropped, so the `args` toggle can never
    /// re-state the command. A call whose whole argument set is the header
    /// object therefore renders no toggle at all.
    #[must_use]
    pub fn extra_args(&self) -> Vec<&ArgRow> {
        self.args
            .iter()
            .filter(|row| row.value.trim() != self.header_object.trim())
            .collect()
    }

    /// Net line counts across every file this call touched.
    #[must_use]
    pub fn line_delta(&self) -> (usize, usize) {
        self.files.iter().fold((0, 0), |(add, del), change| {
            let diff = crate::file_diff::FileDiff::build(change);
            (add + diff.added, del + diff.removed)
        })
    }
}

/// One model iteration: the accounting, plus the call it requested.
///
/// A step with no call is legal — a model turn that produced only prose still
/// costs tokens, and hiding that is how per-step accounting stops adding up to
/// the turn rollup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// The tool call this iteration made, if it made one.
    pub call: Option<Call>,
    /// Token/cost accounting for the iteration.
    pub accounting: Accounting,
    /// Milliseconds from the start of the turn — rendered as an elapsed offset,
    /// once per step, and dropped when identical to the previous step's.
    pub offset_ms: u64,
}

impl Step {
    /// The status the digest is tinted by. A step with no call inherits `Ok`.
    #[must_use]
    pub fn status(&self) -> Status {
        self.call.as_ref().map_or(Status::Ok, |c| c.status)
    }

    /// Wall time of the step's call, or zero.
    #[must_use]
    pub fn duration_ms(&self) -> u64 {
        self.call.as_ref().map_or(0, |c| c.duration_ms)
    }
}

/// An assistant reasoning block between calls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prose {
    /// The full text. Folds to its first sentence.
    pub text: String,
    /// Which step this block precedes, so the renderer can interleave prose and
    /// steps in the order they happened.
    pub before_step: usize,
}

/// One user request → agent response cycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    /// A short name for the turn — the branch name, the goal slug.
    pub name: String,
    /// The user's message. Never folded away by default: it is the anchor
    /// everything below is read against.
    pub prompt: String,
    /// Reasoning blocks, interleaved with steps by `before_step`.
    pub prose: Vec<Prose>,
    /// The model iterations.
    pub steps: Vec<Step>,
    /// The agent's final message. `None` while the turn is still running.
    pub answer: Option<String>,
    /// How the turn ended.
    pub status: Status,
    /// Wall time of the whole turn.
    pub duration_ms: u64,
}

impl Turn {
    /// Token/cost rollup across the turn's steps.
    #[must_use]
    pub fn rollup(&self) -> Accounting {
        self.steps
            .iter()
            .fold(Accounting::default(), |acc, s| acc.merged(s.accounting))
    }

    /// Whether any step failed — a turn holding a failed step cannot present
    /// itself as quietly done.
    #[must_use]
    pub fn has_failure(&self) -> bool {
        self.status == Status::Error || self.steps.iter().any(|s| s.status() == Status::Error)
    }
}

/// A whole session: the turns, plus the identity bar above them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    /// Display name of the run.
    pub name: String,
    /// The model that served it, for the identity bar.
    pub model: String,
    /// Wall-clock start, already formatted by the caller — this crate performs
    /// no time formatting, because it would need a clock and it has none.
    pub started_at: String,
    /// The turns, oldest first.
    pub turns: Vec<Turn>,
}

impl Run {
    /// Token/cost rollup across every turn.
    #[must_use]
    pub fn rollup(&self) -> Accounting {
        self.turns
            .iter()
            .fold(Accounting::default(), |acc, t| acc.merged(t.rollup()))
    }

    /// Total steps across every turn.
    #[must_use]
    pub fn step_count(&self) -> usize {
        self.turns.iter().map(|t| t.steps.len()).sum()
    }
}
