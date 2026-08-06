//! Does *this change* warrant a witness test, and if not, why not.
//!
//! Design: [`docs/spec/witness-protocol.md`](../../../../docs/spec/witness-protocol.md) §7.
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
    /// The turn changed no files at all *and never asked to* — a question, a
    /// lookup, an explanation. There is no behavior to prove. Both halves are
    /// load-bearing; [`ChangeSignals`] says why.
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
    /// [`crate::triage::resolve_witness`] has always kept the verifier for
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

/// The synthetic diff line `Pipeline::gather_diff` appends for a file the
/// turn created or modified outside git's view (`git diff` cannot see
/// untracked files). Built and parsed through the two functions below so the
/// writer and this module's reader can never drift apart.
pub(crate) const UNTRACKED_CHANGE_PREFIX: &str = "+ untracked change: ";

/// Render one untracked-change marker line (no trailing newline).
pub(crate) fn untracked_change_line(path: &str, added_lines: u32) -> String {
    format!("{UNTRACKED_CHANGE_PREFIX}{path} (+{added_lines} lines)")
}

/// The paths named by untracked-change markers in `diff`.
///
/// These exist because an untracked file has no `+++`/`---` header for
/// [`changed_paths`] to read — so before this parser, a turn that edited
/// `README.md` *and created a new source file* classified as
/// [`NoWitnessReason::DocsOnly`]: the marker line carried the source file
/// but no path rule ever saw it, and the change skipped both the witness and
/// the reviewer with a verdict asserting prose-only. Feeding these paths into
/// the same "every path must agree" rules closes that hole.
fn untracked_marker_paths(diff: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in diff.lines() {
        let Some(rest) = line.strip_prefix(UNTRACKED_CHANGE_PREFIX) else {
            continue;
        };
        // `{path} (+{n} lines)` — strip the trailing size note.
        let path = rest
            .rsplit_once(" (+")
            .map_or(rest, |(path, _)| path)
            .trim();
        if !path.is_empty() && !paths.iter().any(|seen| seen == path) {
            paths.push(path.to_string());
        }
    }
    paths
}

/// What the turn *did*, as distinct from what the diff *shows*.
///
/// Every field is a count the pipeline kept while the turn ran, and that is
/// what earns them a say here. A probe that comes back empty may simply have
/// been unable to look; a record of what was dispatched cannot be. So these
/// are the only inputs [`warrant`] is willing to read as evidence of
/// *absence* — the same asymmetry
/// [`crate::verify::LadderInputs::nothing_was_attempted`] is built on.
///
/// `Default` is the all-quiet input: nothing observed, nothing dispatched.
/// Callers name the fields they exercise and take the rest from it, so adding
/// a signal does not rewrite every literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChangeSignals {
    /// `FileChange` events the turn emitted, from the registry's
    /// `FileTouchPort`. Non-zero is positive proof the tree moved even when
    /// nothing can render *how*.
    ///
    /// Read it as a one-way signal only. Inside a best-of-N or witness
    /// candidate the engine emits no `FileChange` events at all
    /// (`crate::pipeline::verify_probes` documents why), so a zero here is
    /// routinely a candidate's silence rather than a quiet workspace — which
    /// is precisely why it cannot be the sole guard below.
    pub file_changes: u32,
    /// Tool calls this turn that were *capable* of changing the workspace:
    /// every dispatched call except those whose tool the registry advertises
    /// as `read_only`. A call whose tool is unknown counts as mutating, so an
    /// unrecognized name can never be the reason a turn is written off.
    pub mutating_actions: u32,
    /// The subset of `mutating_actions` whose effects the diff CANNOT be
    /// trusted to account for (per the crate-private `diff_accountable_mutator`): shell calls,
    /// process spawns, repo pushes, MCP and custom tools — anything able to
    /// act outside the file-CRUD surface the diff probes observe.
    ///
    /// This is what closes the #1701 shape the empty-paths guard alone left
    /// open: a system-configuration turn that ALSO edited one README parses
    /// as a docs-only diff, and with only the two signals above the module
    /// waived both the witness and the reviewer over ten dispatched shell
    /// calls whose effects landed in `/etc`. A path rule may only waive what
    /// the diff fully explains, and an opaque call is by definition something
    /// it does not.
    pub opaque_actions: u32,
}

/// Whether a *mutating* tool call's effects are fully accountable to the
/// diff and untracked-file probes — file CRUD inside the workspace tree,
/// plus the session-local bookkeeping tools that touch no workspace at all
/// (the task board, memory and exploration writes under `.stella/`).
///
/// Everything else — the shell, process control, project-command verbs
/// (which run arbitrary configured commands), repo mutations, media and
/// issue tools, MCP servers, custom scripts, and any name this list has
/// never heard of — is opaque: it can act where no diff probe looks, so a
/// dispatched call fails closed as [`ChangeSignals::opaque_actions`] and the
/// warrant buys the test.
///
/// Name-based on purpose, mirroring the built-in catalog
/// (`stella-tools/src/catalog.rs`): the pipeline sees tools as schemas, and
/// a capability flag on the wire would let a third-party tool declare its
/// own effects collectable. Only names this pipeline can vouch for are
/// listed, and the cost of a missing entry is an unnecessary witness — one
/// model call — never an unverified change.
pub(crate) fn diff_accountable_mutator(name: &str) -> bool {
    matches!(
        name,
        "write_file"
            | "edit_file"
            | "apply_edits"
            | "delete_file"
            | "save_exploration"
            | "save_memory"
            | "cite_memory"
            | "task_create"
            | "task_start"
            | "task_complete"
            | "task_cancel"
            | "task_assign"
    )
}

/// Decide from the change what the prompt could never tell us.
///
/// `signals` exists to distinguish "genuinely changed nothing" from "changed
/// something this run cannot see" — the same honesty guard
/// `verification_honest_diff` exists for. A turn that touched files, or merely
/// *asked* to, but produced no readable diff is [`WitnessWarrant::Required`],
/// never [`NoWitnessReason::NothingChanged`].
pub fn warrant(diff: &str, signals: ChangeSignals) -> WitnessWarrant {
    let mut paths = changed_paths(diff);

    if paths.is_empty() {
        // A turn that demonstrably touched files — or dispatched a call able
        // to — is the one case that must not read as "nothing happened". An
        // untracked-only change lands here too (its markers are not diff
        // headers) and stays `Required`: the marker names a path but shows no
        // content, and a change this module cannot read is one it does not
        // waive.
        //
        // `mutating_actions` is the half added by #1701, and it is the half
        // that works inside a candidate. A system-configuration task —
        // `apt-get install nginx`, then `/etc/nginx`, then `service nginx
        // restart` — cannot land its effects under the candidate root, so the
        // empty diff is the *expected* outcome and `file_changes` is
        // structurally zero there. With only those two signals the module
        // waived the witness and the run completed `passed: true,
        // deterministic: true` over ten mutating calls it had itself
        // dispatched. The dispatch record is the one channel that cannot go
        // dark, so it is what keeps this branch honest.
        return if signals.file_changes == 0
            && signals.mutating_actions == 0
            && diff.trim().is_empty()
        {
            WitnessWarrant::NotRequired(NoWitnessReason::NothingChanged)
        } else {
            WitnessWarrant::Required
        };
    }
    // Untracked files the turn created or modified join the same
    // every-path-must-agree rules: a new source file riding beside a docs
    // edit makes the change a source change, exactly as a tracked one would.
    for path in untracked_marker_paths(diff) {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }

    // Every path rule below waives on the premise that the diff IS the
    // change. An opaque dispatched call — a shell command, a spawned
    // process — breaks that premise: its effects can land where no diff
    // probe looks (#1701's trace put them in `/etc`), and one README edit
    // beside them must not turn "ten shell calls" into "documentation
    // only". With the premise gone, the waivers are gone with it.
    if signals.opaque_actions > 0 {
        return WitnessWarrant::Required;
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

/// Both the post-image (`+++ b/path`) and pre-image (`--- a/path`) paths from
/// a unified diff, with `/dev/null` dropped and the `a/`/`b/` prefixes
/// stripped — a deletion still reports what it removed, and a rename
/// contributes both its old and new path to the "every path must agree"
/// rules.
/// `pub(crate)` so the authored-diff channel can assert, at its own seam, that
/// what it renders is parsed here as real paths. That property is invisible
/// from inside this module and silent when it breaks — the parser simply
/// returns nothing — so the test that guards it has to live next to the
/// producer.
pub(crate) fn changed_paths(diff: &str) -> Vec<String> {
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
///
/// A CLOSED list of names, on purpose. This used to also match every `*.yml`,
/// `*.yaml`, and `*.lock` — but "yaml" is a syntax, not a category, and most
/// yaml in the wild is behavior: a `docker-compose.yml`, a Helm values file,
/// or an app's own config changes what runs, and each was completing with a
/// PASS asserting "the build and gate verify these" while no build, gate, test
/// or reviewer had seen it. A name this list has never heard of falls through
/// to `Required`, exactly like every other rule here — when in doubt, the
/// module buys the test.
fn is_config(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    // Dependency manifests and lockfiles (verified by the build), the build's
    // own entry points, and CI pipeline definitions (verified by the gate
    // itself running). Each entry names a file whose format has exactly one
    // consumer; anything reusable-syntax lands on `Required`.
    const MANIFESTS: &[&str] = &[
        "cargo.toml",
        "cargo.lock",
        "go.mod",
        "go.sum",
        "package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "bun.lock",
        "bun.lockb",
        "poetry.lock",
        "uv.lock",
        "gemfile.lock",
        "composer.lock",
        "makefile",
        ".gitignore",
        ".gitlab-ci.yml",
        "azure-pipelines.yml",
    ];
    lower.starts_with(".github/") || MANIFESTS.contains(&name)
}

#[cfg(test)]
mod tests;
