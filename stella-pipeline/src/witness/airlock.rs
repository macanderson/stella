//! The feedback airlock: the one channel from verification back to the worker.
//!
//! Design: [`docs/design/witness-protocol.md`](../../../../docs/design/witness-protocol.md) §4.
//!
//! # Leak the defect, never the detector
//!
//! Before this module, a deterministic failure went back to the worker as
//! `"touched tests failed after execution: {stderr_tail}"` — the raw
//! test-runner output, verbatim, on every revision. That tail carries the
//! runtime values an assertion compared, the inputs that reached it, and the
//! test's identifier. The worker is entitled to know *what is wrong with its
//! code*; handing it the detector's fingerprint for free is what makes
//! special-casing cheaper than fixing.
//!
//! What this module is honest about: with a visible witness the worker can run
//! the test itself and read the same bytes. The airlock does not seal what a
//! shell could reach. What it does buy is that the *revision loop* no longer
//! trains on detector output for free, that disclosure is a graded control
//! surface which tightens when a worker thrashes, and that every disclosure is
//! recorded rather than incidental.
//!
//! # Closed vocabulary, not model prose
//!
//! The source draft has a model author the symptom description. Stella derives
//! it mechanically from a closed [`SymptomClass`] vocabulary instead: it is
//! free, it is deterministic (so a verdict stays replayable), and a fixed
//! sentence drawn from an enum cannot quote the assertion that produced it. The
//! scrubber ([`scrub`]) is still applied, because the reproduction hint at
//! [`DisclosureGrain::Reproduction`] is derived from sealed material.
//!
//! # The pure/orchestration split
//!
//! Like its siblings, everything here is a synchronous function over owned
//! data. Nothing in this module runs a process, calls a model, or touches the
//! filesystem.

use std::fmt;

use sha2::{Digest, Sha256};

use crate::ports::TestInvocation;

/// How much the worker is told about a verification failure.
///
/// Ordered weakest-to-strongest so a grain can be compared and stepped down
/// (`Outcome < Command < Symptom < Reproduction`). Each grain is a superset of
/// the one below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum DisclosureGrain {
    /// `L0` — that verification failed, and nothing else.
    Outcome,
    /// `L1` — which command failed.
    Command,
    /// `L2` — a symptom class phrased against observable behavior.
    Symptom,
    /// `L3` — a reproduction the worker can run itself. The default, because
    /// convergence comes from iterating against a real failure.
    #[default]
    Reproduction,
}

impl DisclosureGrain {
    /// The `L0`–`L3` label used in logs and evidence refs, so a reader can see
    /// at which grain a run was disclosing without decoding the variant name.
    pub fn label(self) -> &'static str {
        match self {
            Self::Outcome => "L0",
            Self::Command => "L1",
            Self::Symptom => "L2",
            Self::Reproduction => "L3",
        }
    }

    /// One rung down the ladder, saturating at [`DisclosureGrain::Outcome`].
    pub fn tightened(self) -> Self {
        match self {
            Self::Reproduction => Self::Symptom,
            Self::Symptom => Self::Command,
            Self::Command | Self::Outcome => Self::Outcome,
        }
    }
}

/// A coarse, closed classification of *how* a test run failed.
///
/// Deliberately small and deliberately not model-authored: every variant maps
/// to one fixed sentence, so the symptom text can never quote the runtime
/// values that produced it. Classification reads the runner's output but the
/// output itself never crosses into a brief.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymptomClass {
    /// The code under test did not build.
    BuildFailure,
    /// An assertion or expectation compared two values and rejected them.
    Assertion,
    /// The process crashed — a panic, an abort, or a signal.
    Crash,
    /// The run exceeded its time budget.
    Timeout,
    /// A non-zero exit with nothing more specific recoverable from the output.
    ExitFailure,
}

impl SymptomClass {
    /// The fixed, behavior-facing sentence disclosed at
    /// [`DisclosureGrain::Symptom`].
    ///
    /// Every string here is a compile-time literal. That is the property that
    /// makes this grain safe by construction rather than by scrubbing.
    pub fn sentence(self) -> &'static str {
        match self {
            Self::BuildFailure => "the change does not compile",
            Self::Assertion => "an expected behavior did not hold at runtime",
            Self::Crash => "the code panicked or aborted while under test",
            Self::Timeout => "the run did not finish within its time budget",
            Self::ExitFailure => "verification exited non-zero without a recoverable diagnosis",
        }
    }

    /// Classify a runner's output. Ordered most-specific first: a build
    /// failure that also prints the word `assertion` is a build failure, since
    /// nothing ran.
    pub fn classify(output: &str) -> Self {
        let lower = output.to_ascii_lowercase();
        if lower.contains("error[e")
            || lower.contains("could not compile")
            || lower.contains("syntaxerror")
            || lower.contains("cannot find")
            || lower.contains("compilation failed")
        {
            return Self::BuildFailure;
        }
        if lower.contains("timed out") || lower.contains("timeout") {
            return Self::Timeout;
        }
        if lower.contains("panicked at")
            || lower.contains("segmentation fault")
            || lower.contains("fatal error")
            || lower.contains("core dumped")
        {
            return Self::Crash;
        }
        if lower.contains("assertion")
            || lower.contains("assert_eq")
            || lower.contains("assertionerror")
            || lower.contains("expected")
        {
            return Self::Assertion;
        }
        Self::ExitFailure
    }
}

/// A stable identity for *how* a run failed, as opposed to *that* it failed.
///
/// Two runs that failed the same way share a fingerprint; two runs that failed
/// differently do not. Computed over normalized output ([`normalize_failure`])
/// so that timings, temporary paths, addresses, and line-number churn do not
/// split one recurring failure into many.
///
/// A fingerprint is verification-side only. It is never disclosed to the
/// worker, which is why it may be derived from sealed material.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FailureFingerprint(String);

impl FailureFingerprint {
    /// Fingerprint one failing run's output.
    pub fn of(output: &str) -> Self {
        let normalized = normalize_failure(output);
        let digest = Sha256::digest(normalized.as_bytes());
        // 16 hex chars is ample to separate the handful of distinct failures a
        // single task produces, and short enough to read in a log line.
        Self(
            digest
                .iter()
                .take(8)
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        )
    }

    /// The hex digest.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FailureFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Strip the parts of runner output that change between two runs of the *same*
/// failure, so a fingerprint identifies the failure rather than the run.
///
/// Removed: ANSI escapes, durations, hex addresses, process and thread ids,
/// absolute paths, and the `:line:col` suffix on source locations. Kept: the
/// failing test's identity and the shape of the diagnosis, which is what makes
/// two occurrences the *same* failure.
pub fn normalize_failure(output: &str) -> String {
    let mut normalized = String::with_capacity(output.len());
    for raw_line in strip_ansi(output).lines() {
        if raw_line.trim().is_empty() {
            continue;
        }
        let rebuilt: Vec<String> = raw_line.split_whitespace().map(normalize_token).collect();
        normalized.push_str(&rebuilt.join(" "));
        normalized.push('\n');
    }
    normalized
}

/// Replace one whitespace-delimited token with a stable placeholder when it is
/// run-specific. Path handling keeps the final component so two failures in
/// different files stay distinct while a temp-directory prefix does not split
/// one failure across runs.
fn normalize_token(token: &str) -> String {
    if token.starts_with("0x")
        && token.len() > 2
        && token[2..].chars().all(|c| c.is_ascii_hexdigit())
    {
        return "<addr>".to_string();
    }
    // A duration: `1.23s`, `0.05ms`, `12s`.
    let numeric_prefix: String = token
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if !numeric_prefix.is_empty() {
        let suffix = &token[numeric_prefix.len()..];
        if matches!(suffix, "s" | "ms" | "us" | "ns" | "s," | "ms," | "secs") {
            return "<dur>".to_string();
        }
        if suffix.is_empty() {
            return "<num>".to_string();
        }
    }
    let trimmed = token.trim_matches(|c: char| matches!(c, '(' | ')' | ',' | '\'' | '"' | '`'));
    if trimmed.contains('/') || trimmed.contains('\\') {
        let separator = if trimmed.contains('/') { '/' } else { '\\' };
        let last = trimmed.rsplit(separator).next().unwrap_or(trimmed);
        // Drop a `:line:col` suffix so inserting a line above a failure does
        // not look like a different failure.
        let base = last.split(':').next().unwrap_or(last);
        if base.is_empty() {
            return "<path>".to_string();
        }
        return format!("<path>/{base}");
    }
    trimmed.to_string()
}

/// Remove ANSI CSI/OSC escape sequences. Runners colorize by default and the
/// same failure rendered with and without color must fingerprint identically.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            // CSI: consume through the final byte in `@`..=`~`.
            Some('[') => {
                for next in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&next) {
                        break;
                    }
                }
            }
            // OSC: consume through BEL or ST.
            Some(']') => {
                while let Some(next) = chars.next() {
                    if next == '\u{7}' {
                        break;
                    }
                    if next == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Everything the verification side knows about one failure. Never crosses to
/// the worker — [`redact`] converts it into a [`FailureBrief`], and only the
/// brief is disclosable.
#[derive(Debug, Clone)]
pub struct SealedFailure<'a> {
    /// The user-facing command whose run failed.
    pub command: &'a str,
    /// The parsed invocation. Its argument vector names the exact test, which
    /// is the single most reconstruction-useful string in this struct.
    pub invocation: Option<&'a TestInvocation>,
    /// The raw runner output. The detector's fingerprint lives here.
    pub output: &'a str,
    /// Paths of the witness artifacts backing this run, withheld from every
    /// grain.
    pub witness_paths: &'a [String],
}

impl SealedFailure<'_> {
    /// The failure's identity, for the oracle and the grain policy.
    pub fn fingerprint(&self) -> FailureFingerprint {
        FailureFingerprint::of(self.output)
    }

    /// The closed-vocabulary classification disclosed at
    /// [`DisclosureGrain::Symptom`].
    pub fn symptom(&self) -> SymptomClass {
        SymptomClass::classify(self.output)
    }
}

/// What actually crosses to the worker. Constructed only by [`redact`], so
/// there is exactly one code path from sealed material to worker-visible text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureBrief {
    /// The grain this brief was emitted at — after any scrub-driven
    /// tightening, so it reports what was actually disclosed.
    pub grain: DisclosureGrain,
    /// The failing command, from [`DisclosureGrain::Command`] up.
    pub command: Option<String>,
    /// The closed-vocabulary symptom, from [`DisclosureGrain::Symptom`] up.
    pub symptom: Option<SymptomClass>,
    /// How to reproduce it, at [`DisclosureGrain::Reproduction`].
    pub reproduction: Option<String>,
}

impl FailureBrief {
    /// The evidence body handed to the worker's revision turn.
    ///
    /// Deliberately headerless: the caller wraps this in `revision_prompt`,
    /// which already states that verification failed. At
    /// [`DisclosureGrain::Outcome`] this is a single line and that is the
    /// whole disclosure.
    pub fn message(&self) -> String {
        let mut lines = Vec::new();
        if let Some(symptom) = self.symptom {
            lines.push(format!("Symptom: {}.", symptom.sentence()));
        }
        if let Some(command) = &self.command {
            lines.push(format!("Failing check: `{command}`."));
        }
        if let Some(reproduction) = &self.reproduction {
            lines.push(format!("Reproduce it yourself with: {reproduction}"));
        }
        lines.push(
            "Diagnose the underlying cause and fix it. Do not special-case the check.".to_string(),
        );
        lines.join("\n")
    }

    /// Whether this brief disclosed anything beyond the bare fact of failure.
    pub fn is_bare(&self) -> bool {
        self.command.is_none() && self.symptom.is_none() && self.reproduction.is_none()
    }

    /// Pointers a reader can follow to check this verdict rather than take it
    /// on faith — the `evidence_refs` that were empty at every construction
    /// site before the airlock existed.
    pub fn evidence_refs(&self, fingerprint: &FailureFingerprint) -> Vec<String> {
        let mut refs = vec![
            format!("disclosure:{}", self.grain.label()),
            format!("failure:{fingerprint}"),
        ];
        if let Some(symptom) = self.symptom {
            refs.push(format!("symptom:{symptom:?}"));
        }
        refs
    }
}

/// Why a candidate brief was rejected by the scrubber.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LeakKind {
    /// The text named a witness artifact.
    #[error("brief names witness artifact `{0}`")]
    WitnessPath(String),
    /// The text named the exact test the detector runs.
    #[error("brief names the sealed test selector `{0}`")]
    TestSelector(String),
    /// The text quoted a long literal span of the runner's output.
    #[error("brief quotes {0} characters of sealed runner output")]
    QuotedOutput(usize),
}

impl LeakKind {
    /// A short, fixed token naming *which* rule rejected the text — safe for
    /// the `outcome` field of a policy event, which carries tokens and never
    /// content.
    pub fn token(&self) -> &'static str {
        match self {
            Self::WitnessPath(_) => "witness_path",
            Self::TestSelector(_) => "test_selector",
            Self::QuotedOutput(_) => "quoted_output",
        }
    }
}

/// The shortest run of sealed output that counts as a quote.
///
/// Short spans collide constantly on ordinary English and on the command name,
/// which the brief is *supposed* to carry; 24 characters is long enough that a
/// match is a copied diagnostic rather than a coincidence.
const QUOTE_SPAN: usize = 24;

/// Reject `text` if it carries sealed material.
///
/// Fails closed: [`redact`] degrades a brief that does not pass rather than
/// emitting it with a hole in it. A best-effort redactor is a leak with a
/// ceremony attached.
pub fn scrub(text: &str, sealed: &SealedFailure<'_>) -> Result<(), LeakKind> {
    for path in sealed.witness_paths {
        if text.contains(path.as_str()) {
            return Err(LeakKind::WitnessPath(path.clone()));
        }
        if let Some(stem) = path.rsplit('/').next()
            && !stem.is_empty()
            && text.contains(stem)
        {
            return Err(LeakKind::WitnessPath(stem.to_string()));
        }
    }
    if let Some(invocation) = sealed.invocation {
        for selector in test_selectors(invocation) {
            if text.contains(selector.as_str()) {
                return Err(LeakKind::TestSelector(selector));
            }
        }
    }
    if let Some(span) = quoted_span(text, sealed.output, QUOTE_SPAN) {
        return Err(LeakKind::QuotedOutput(span));
    }
    Ok(())
}

/// The arguments that name *which* test runs, as opposed to which crate or
/// runner. These are the strings that make a brief reconstruction-useful.
fn test_selectors(invocation: &TestInvocation) -> Vec<String> {
    invocation
        .args
        .iter()
        .filter(|arg| {
            // Flags and package selectors describe the suite; a bare token
            // (or a pytest node id) names the test itself.
            !arg.starts_with('-') && arg.len() >= 3
        })
        .filter(|arg| !matches!(arg.as_str(), "test" | "run" | "nextest" | "exec"))
        .cloned()
        .collect()
}

/// The length of a run of sealed output that `text` reproduces verbatim, if
/// one reaches `minimum`. Compared on collapsed whitespace and folded case so
/// reflowing or recasing a quote does not evade the check.
fn quoted_span(text: &str, sealed_output: &str, minimum: usize) -> Option<usize> {
    let haystack = collapse_whitespace(text);
    let sealed = collapse_whitespace(sealed_output);
    if haystack.is_empty() || sealed.len() < minimum {
        return None;
    }
    // Every window of exactly `minimum` is enough to decide: any longer shared
    // run necessarily contains one, so finding none rules all of them out.
    (0..=sealed.len() - minimum)
        .filter_map(|start| {
            let end = start + minimum;
            (sealed.is_char_boundary(start) && sealed.is_char_boundary(end))
                .then(|| &sealed[start..end])
        })
        .find(|window| haystack.contains(window))
        .map(str::len)
}

/// Collapse every run of whitespace to one space and lowercase, so quoting
/// survives reformatting and case changes.
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

/// Build the brief the worker sees, at no more than `requested` grain.
///
/// The scrubber runs on every field that could carry sealed material. A field
/// that fails is dropped *and the grain steps down with it*, so the returned
/// brief's `grain` always reports what was really disclosed rather than what
/// was asked for.
pub fn redact(sealed: &SealedFailure<'_>, requested: DisclosureGrain) -> FailureBrief {
    let mut grain = requested;

    let reproduction = if grain >= DisclosureGrain::Reproduction {
        let candidate = format!("`{}`", sealed.command);
        match scrub(&candidate, sealed) {
            Ok(()) => Some(candidate),
            Err(_) => {
                grain = DisclosureGrain::Symptom;
                None
            }
        }
    } else {
        None
    };

    let symptom = (grain >= DisclosureGrain::Symptom).then(|| sealed.symptom());

    let command = if grain >= DisclosureGrain::Command {
        let candidate = sealed.command.to_string();
        match scrub(&candidate, sealed) {
            Ok(()) => Some(candidate),
            Err(_) => {
                grain = DisclosureGrain::Outcome;
                None
            }
        }
    } else {
        None
    };

    FailureBrief {
        grain,
        // A grain that fell to `Outcome` withholds the command it could not
        // scrub; one that merely lost its reproduction keeps it.
        command: (grain >= DisclosureGrain::Command)
            .then_some(command)
            .flatten(),
        symptom: (grain >= DisclosureGrain::Symptom)
            .then_some(symptom)
            .flatten(),
        reproduction,
    }
}

/// The disclosure grain a run has earned, given how many times it has now
/// produced the *same* failure.
///
/// A worker that has seen the same brief twice and still fails is not being
/// helped by a third copy of it — it is being given more surface to fit. The
/// ladder therefore tightens on repetition rather than on a raw revision
/// count, which is what makes it a response to thrashing rather than a tax on
/// ordinary iteration.
pub fn grain_for_repeats(repeats: u32) -> DisclosureGrain {
    match repeats {
        0 | 1 => DisclosureGrain::Reproduction,
        2 => DisclosureGrain::Symptom,
        3 => DisclosureGrain::Command,
        _ => DisclosureGrain::Outcome,
    }
}

#[cfg(test)]
mod tests;
