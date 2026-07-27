//! Does *this change* warrant a witness test, and if not, why not.
//!
//! Design: [`docs/design/witness-protocol.md`](../../../../docs/design/witness-protocol.md) §7.
//!
//! # Escalate on evidence, do not predict up front
//!
//! Stella's contributor rule has always been nuanced: ship a witness test, *or*
//! a stated reason there isn't one. Pure refactors, docs, and CI changes don't
//! need one. The pipeline held itself to a stricter rule than it held people
//! to, and the one escape hatch it had ([`crate::triage::resolve_witness`])
//! guessed from the *prompt* — before any work existed — by keyword-matching
//! removal verbs. That mechanism has to be narrow, because a false positive
//! ships a real behavior change unproven, and the only evidence available at
//! that moment is the wording of a request.
//!
//! This module answers the same question from the **change itself**. After
//! execution there is a diff, and a diff is evidence rather than a guess. A
//! docs-only edit is docs-only whether the prompt said "document the parser" or
//! "make the README less confusing"; no phrasing changes what the diff is.
//!
//! # Fail closed
//!
//! Every rule here must be *certain* to return [`WitnessWarrant::NotRequired`].
//! Anything mixed, anything unrecognized, and anything the diff machinery could
//! not see falls through to [`WitnessWarrant::Required`]. The asymmetry is
//! deliberate: an unnecessary witness costs one model call, while a missing one
//! ships unverified behavior. When this module is unsure, it buys the test.

/// Why a change legitimately needs no witness test. Each variant is a *stated
/// reason*, recorded in the verdict — the pipeline's half of the same contract
/// contributors are held to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoWitnessReason {
    /// The turn changed no files at all — a question, a lookup, an
    /// explanation. There is no behavior to prove.
    NothingChanged,
    /// Only documentation changed. Prose has no runtime behavior to flip.
    DocsOnly,
    /// Only test files changed. The change *is* the test; authoring a second
    /// test to prove the first one exists is circular.
    TestsOnly,
    /// Only build, CI, or dependency manifests changed. These are verified by
    /// the build and the gate, not by a unit test.
    ConfigOnly,
    /// Every changed line is a comment or blank. Nothing executable moved.
    CommentsOnly,
    /// The change only removes code. A witness must fail on the old code and
    /// pass on the new, and there is nothing to write that against when the
    /// subject stops existing — a removal's proof is its diff.
    PureRemoval,
}

impl NoWitnessReason {
    /// The sentence recorded in the verdict, so a reader sees *why* no test was
    /// written rather than an unexplained absence.
    pub fn sentence(self) -> &'static str {
        match self {
            Self::NothingChanged => "no files changed; there is no behavior to prove",
            Self::DocsOnly => "documentation only; prose has no runtime behavior to flip",
            Self::TestsOnly => "test files only; the change is itself the test",
            Self::ConfigOnly => {
                "build, CI, or dependency manifests only; the build and gate verify these"
            }
            Self::CommentsOnly => "comments and blank lines only; nothing executable changed",
            Self::PureRemoval => "removal only; a removal's proof is its diff",
        }
    }
}

impl NoWitnessReason {
    /// Whether an independent reviewer still adds something, even though no
    /// witness test is warranted.
    ///
    /// The split is about what a reviewer can *catch* that a test cannot. A
    /// removal's proof is its diff — but deleting the *wrong* thing is a real
    /// mistake a reader spots and no test would have covered, which is why
    /// [`crate::triage::resolve_witness`] has always kept the judge for
    /// deletions. Test-only changes are the same shape: nothing to prove, but
    /// plenty to get wrong. Prose, comments, and manifests carry no behavior
    /// for a reviewer to reason about, so a review call there is spend without
    /// a question to answer.
    pub fn warrants_independent_review(self) -> bool {
        match self {
            Self::TestsOnly | Self::PureRemoval => true,
            Self::NothingChanged | Self::DocsOnly | Self::ConfigOnly | Self::CommentsOnly => false,
        }
    }
}

/// Whether this change needs an authored witness test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WitnessWarrant {
    /// Behavior may have changed. Prove it.
    Required,
    /// No test is warranted, with the reason to record.
    NotRequired(NoWitnessReason),
}

impl WitnessWarrant {
    /// Whether a witness test is warranted.
    pub fn is_required(self) -> bool {
        matches!(self, Self::Required)
    }

    /// The stated reason, when no test is warranted.
    pub fn reason(self) -> Option<NoWitnessReason> {
        match self {
            Self::Required => None,
            Self::NotRequired(reason) => Some(reason),
        }
    }
}

/// One changed line's role, for the comments-only rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineRole {
    Blank,
    Comment,
    Code,
}

/// Decide from the change what the prompt could never tell us.
///
/// `file_changes` is the count of `FileChange` events the turn emitted. It is
/// consulted only to distinguish "genuinely changed nothing" from "changed
/// something the diff machinery could not see" — the same honesty guard
/// `verification_honest_diff` exists for. A turn that touched files but
/// produced no readable diff is [`WitnessWarrant::Required`], never
/// [`NoWitnessReason::NothingChanged`].
pub fn warrant(diff: &str, file_changes: u32) -> WitnessWarrant {
    let paths = changed_paths(diff);

    if paths.is_empty() {
        // A blind diff over a turn that demonstrably touched files is the one
        // case that must not read as "nothing happened".
        return if file_changes == 0 && diff.trim().is_empty() {
            WitnessWarrant::NotRequired(NoWitnessReason::NothingChanged)
        } else {
            WitnessWarrant::Required
        };
    }

    // Every path must agree. A change that touches docs *and* source is a
    // source change that also updated its docs.
    if paths.iter().all(|path| is_docs(path)) {
        return WitnessWarrant::NotRequired(NoWitnessReason::DocsOnly);
    }
    if paths.iter().all(|path| is_test(path)) {
        return WitnessWarrant::NotRequired(NoWitnessReason::TestsOnly);
    }
    if paths.iter().all(|path| is_config(path)) {
        return WitnessWarrant::NotRequired(NoWitnessReason::ConfigOnly);
    }

    let (added, removed) = changed_lines(diff);
    if added
        .iter()
        .chain(removed.iter())
        .all(|line| !matches!(classify_line(line), LineRole::Code))
    {
        return WitnessWarrant::NotRequired(NoWitnessReason::CommentsOnly);
    }
    // Nothing executable was *introduced*: the change only takes code away.
    // Blank and comment additions are allowed, so removing a function and
    // leaving a note behind still reads as a removal.
    if !removed.is_empty()
        && added
            .iter()
            .all(|line| !matches!(classify_line(line), LineRole::Code))
    {
        return WitnessWarrant::NotRequired(NoWitnessReason::PureRemoval);
    }

    WitnessWarrant::Required
}

/// Post-image paths from a unified diff (`+++ b/path`), with `/dev/null`
/// dropped and the `b/` prefix stripped. Falls back to the pre-image path so a
/// deletion still reports what it removed.
fn changed_paths(diff: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in diff.lines() {
        let raw = if let Some(rest) = line.strip_prefix("+++ ") {
            rest
        } else if let Some(rest) = line.strip_prefix("--- ") {
            rest
        } else {
            continue;
        };
        let raw = raw.split('\t').next().unwrap_or(raw).trim();
        if raw == "/dev/null" || raw.is_empty() {
            continue;
        }
        let path = raw
            .strip_prefix("a/")
            .or_else(|| raw.strip_prefix("b/"))
            .unwrap_or(raw);
        if !paths.iter().any(|seen| seen == path) {
            paths.push(path.to_string());
        }
    }
    paths
}

/// Added and removed content lines, excluding the `+++`/`---` file headers.
fn changed_lines(diff: &str) -> (Vec<String>, Vec<String>) {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            added.push(rest.to_string());
        } else if let Some(rest) = line.strip_prefix('-') {
            removed.push(rest.to_string());
        }
    }
    (added, removed)
}

/// Classify one changed line. Language-agnostic on purpose: the comment
/// markers below cover every language Stella's test-runner vocabulary spans,
/// and anything unrecognized is [`LineRole::Code`] so the rule fails closed.
///
/// The markers split in two. Some *only* ever open a comment — nothing
/// executable in any language Stella supports begins with `//`, `/*`, or
/// `<!--`, so those match on prefix alone. The rest are ambiguous: they open a
/// comment in one language and real code in another. `*` continues a
/// block-comment line but also writes through a Rust/C pointer (`*p = 1;`);
/// `#` opens a shell/Python comment but also a Rust attribute (`#[derive]`), a
/// C preprocessor directive (`#define X`), or a JS private field
/// (`#balance = 0`); `--`/`;` open SQL/Lisp comments but also spell a decrement
/// or a statement. For those, a marker fused to a token is code; only a bare
/// marker or one trailed by whitespace is prose. Fusing the check to a
/// delimiter keeps the rule failing closed — an ambiguous line reads as
/// [`LineRole::Code`] and buys the test rather than silently waiving it.
fn classify_line(line: &str) -> LineRole {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return LineRole::Blank;
    }
    const UNAMBIGUOUS_MARKERS: &[&str] = &["//", "/*", "<!--"];
    if UNAMBIGUOUS_MARKERS
        .iter()
        .any(|marker| trimmed.starts_with(marker))
    {
        return LineRole::Comment;
    }
    // `*/` before `*` so a block-comment close is not first stripped to `/…`.
    const DELIMITED_MARKERS: &[&str] = &["#", "*/", "*", "-->", "--", ";"];
    for marker in DELIMITED_MARKERS {
        if let Some(rest) = trimmed.strip_prefix(marker)
            && (rest.is_empty() || rest.starts_with(char::is_whitespace))
        {
            return LineRole::Comment;
        }
    }
    LineRole::Code
}

/// Documentation: prose, not behavior.
fn is_docs(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".md")
        || lower.ends_with(".mdx")
        || lower.ends_with(".txt")
        || lower.ends_with(".rst")
        || lower.ends_with(".adoc")
        || lower.starts_with("docs/")
        || lower.contains("/docs/")
}

/// A test file — matched on the conventions the test-command vocabulary
/// already understands (`crate::witness::is_witness_test_path` covers the
/// authored-witness case; this is the broader "is this a test" question).
fn is_test(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    // `foo/tests.rs` is this workspace's dominant idiom, and `tests.rs` alone
    // matches none of the suffix rules below.
    name == "tests.rs"
        || name == "test.rs"
        || lower.starts_with("tests/")
        || lower.contains("/tests/")
        || lower.contains("/__tests__/")
        || lower.contains("/test/")
        || name.starts_with("test_")
        || name.ends_with("_test.rs")
        || name.ends_with("_test.go")
        || name.ends_with("_test.py")
        || name.ends_with("_tests.rs")
        || name.contains(".test.")
        || name.contains(".spec.")
}

/// Build, CI, and dependency manifests: verified by the build and the gate.
fn is_config(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    lower.starts_with(".github/")
        || name == "cargo.lock"
        || name == "cargo.toml"
        || name == "package-lock.json"
        || name == "pnpm-lock.yaml"
        || name == "makefile"
        || name == ".gitignore"
        || lower.ends_with(".yml")
        || lower.ends_with(".yaml")
        || lower.ends_with(".lock")
}

#[cfg(test)]
mod tests;
