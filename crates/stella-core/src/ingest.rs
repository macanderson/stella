//! Ingestion candidate discovery — deciding which markdown in a workspace is
//! worth turning into context records, and which is a trap.
//!
//! A workspace's durable knowledge is scattered across `CLAUDE.md`, `AGENTS.md`,
//! agent-tool rule directories, memory folders, READMEs and design docs. All of
//! it is a *candidate*. Very little of it should be ingested without being
//! looked at first.
//!
//! # Why this is not "ingest every .md"
//!
//! This repository has 154 tracked markdown files. Eighteen of them contain the
//! word "superseded", including one whose third line reads *"do not implement
//! from it"*. Ingesting markdown indiscriminately turns a retired design
//! document into confidently-stated steering — a record that is worse than no
//! record at all, because without it an agent looks the answer up and with it
//! the agent does the wrong thing on every turn the record is injected.
//!
//! So discovery is broad and *offering* is tiered. Everything is found;
//! [`Tier`] decides what gets proposed first, what gets proposed with a
//! caveat, and what stays out of the way until asked for.
//!
//! # Purity
//!
//! Like the rest of `stella-core` this module performs no I/O. Callers do the
//! walking and hand each file's path and leading bytes to [`classify`], which
//! is a deterministic function of its inputs. That keeps the policy — which is
//! the part worth arguing about — unit-testable without a fixture tree.

pub mod freshness;
pub mod gate;
pub mod record;

pub use freshness::{Retention, retention_for};
pub use gate::{GateOutcome, gate_proposal};
pub use record::{
    AppliesTo, ContextFile, Defaults, Enforcement, EnforcementMode, Force, Link, LinkRelation,
    Probe, ProbeKind, Proposal, Provenance, Quarantine, Record, RecordKind, Refutation, SCHEMA_TAG,
    SharingScope, Steering, Truth, TruthBasis, Validation, Verdict,
};

#[cfg(test)]
mod tests;

/// How deep a workspace scan descends before giving up.
///
/// Deep enough to reach `packages/<name>/docs/memories/` in a monorepo, shallow
/// enough that a stray `node_modules` slipping past [`is_excluded_segment`] does
/// not turn discovery into a full-disk crawl.
pub const MAX_DEPTH: usize = 4;

/// How many leading lines [`classify`] reads when looking for status markers.
///
/// Status lives at the top of a document — in frontmatter, a subtitle, or a
/// blockquote under the title. Scanning the whole body would flag every
/// document that merely *discusses* deprecation.
pub const HEAD_LINES: usize = 15;

/// A bullet list this ratio of `.md`-linking entries or more, with at least
/// [`MIN_INDEX_BULLETS`] entries, is a table of contents rather than content.
pub const INDEX_LINK_RATIO: f64 = 0.6;

/// Below this many bullets, a high link ratio is not evidence of anything — a
/// two-item list of links is a normal thing for a real document to contain.
pub const MIN_INDEX_BULLETS: usize = 3;

/// What an ingestion candidate is worth, and therefore when to offer it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// `AGENTS.md` or `CLAUDE.md` — steering the user already wrote, for an
    /// agent, on purpose. The only tier named unprompted on first run.
    Primary,
    /// The document exists to instruct, but is not one of the two files
    /// everybody recognises: `CONTRIBUTING.md`, an agent tool's rules
    /// directory, a memories folder. Offered as a suggestion, never assumed.
    Instructional,
    /// The document describes the repository rather than instructing anyone —
    /// READMEs, design notes, architecture docs. Real knowledge, but written to
    /// be read by people and liable to drift. Offer, ranked, user picks.
    Descriptive,
    /// The document records what used to be true: changelogs, and anything
    /// carrying a supersession marker. Find it, do not offer it, produce it on
    /// request.
    Historical,
    /// Not a candidate at all. Legal boilerplate, generated output, vendored
    /// trees, and tables of contents whose every bullet duplicates a record
    /// some other file already produces.
    Skip,
}

/// Why a candidate landed in its [`Tier`].
///
/// Kept as a list rather than collapsed into the tier so a review surface can
/// say *"demoted: carries a supersession marker"* instead of silently ranking
/// something last. A candidate with no signals is [`Tier::Descriptive`] by
/// default — ordinary prose about the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    /// `AGENTS.md` or `CLAUDE.md` — the two files people actually write
    /// steering into, and the only ones offered unprompted on first run.
    ///
    /// Separate from [`Signal::InstructionalName`] because the first-run
    /// conversation is much better when it names two familiar files than when
    /// it presents a list of nine things the user has to triage.
    PrimaryInstructionFile,
    /// Filename whose whole purpose is instruction (`CONTRIBUTING.md`,
    /// `GEMINI.md`, `copilot-instructions.md`…).
    InstructionalName,
    /// Lives under an agent tool's configuration directory (`.claude/rules/`,
    /// `.agents/`, `.cursor/rules/`…). The directory carries the meaning.
    AgentToolDirectory,
    /// Lives in a `memories/` or `memory/` directory. Both spellings occur.
    MemoryDirectory,
    /// The head of the document says it has been replaced or withdrawn. Carries
    /// the matched phrase so a reviewer can judge it rather than trust it.
    SupersededMarker(String),
    /// Filename that records history rather than current state.
    HistoricalName,
    /// The body is mostly a bullet list of links to other markdown files. Its
    /// claims all belong to the documents it points at.
    IndexOfSiblings,
    /// Legal or ceremonial boilerplate with no steering content.
    Boilerplate,
    /// Inside a dependency, build output, or vendored tree.
    Excluded,
}

/// One discovered document and the decision about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Workspace-relative path, forward-slashed.
    pub path: String,
    /// What to do with it.
    pub tier: Tier,
    /// Why — in the order the checks fired.
    pub signals: Vec<Signal>,
}

impl Candidate {
    /// `true` when this document is named unprompted on first run.
    ///
    /// Deliberately only [`Tier::Primary`]. A first-run dialog that opens with
    /// two familiar filenames is a question; one that opens with nine files and
    /// a ranking is a chore, and the honest answer to a chore is "not now".
    pub fn is_offered_by_default(&self) -> bool {
        self.tier == Tier::Primary
    }

    /// `true` when this document is worth showing once the user asks for more.
    pub fn is_suggestion(&self) -> bool {
        matches!(self.tier, Tier::Instructional | Tier::Descriptive)
    }
}

/// The two-step first-run conversation, as data.
///
/// Step one names what the user already wrote for an agent. Step two answers
/// "anything else?" — and is only worth rendering if [`Plan::suggestions`] is
/// non-empty.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    /// `AGENTS.md` / `CLAUDE.md`, in path order.
    pub primary: Vec<Candidate>,
    /// Everything else worth offering, best first, capped at
    /// [`MAX_SUGGESTIONS`].
    pub suggestions: Vec<Candidate>,
    /// How many further suggestions were ranked but not shown.
    ///
    /// Reported rather than silently dropped — a truncated list that does not
    /// say it is truncated reads as "this is everything", which is how a scan
    /// quietly loses the file somebody was looking for.
    pub hidden_suggestions: usize,
}

impl Plan {
    /// `true` when there is nothing to open a conversation about.
    pub fn is_empty(&self) -> bool {
        self.primary.is_empty() && self.suggestions.is_empty()
    }
}

/// How many suggestions are worth showing before the list stops being a
/// question and becomes a chore.
///
/// Running the scan on this repository produced 148 lines of suggestions, which
/// is not a prompt anybody reads — it is a wall people dismiss. The rest stay
/// reachable by naming a path.
pub const MAX_SUGGESTIONS: usize = 10;

/// Directory depth, used to rank suggestions.
///
/// A root `README.md` is far more likely to describe the project than
/// `bench/harbor_adapter/README.md`, and depth says so without reading either.
fn depth_of(path: &str) -> usize {
    path.matches('/').count()
}

/// Split classified candidates into the first-run conversation's two steps.
///
/// Anything [`Tier::Historical`] or [`Tier::Skip`] is dropped here rather than
/// shown and demoted: a first-run dialog is not the place to explain why a
/// changelog is a bad idea. Those remain reachable by naming the path
/// explicitly, which is always allowed.
pub fn plan(candidates: Vec<Candidate>) -> Plan {
    let mut plan = Plan::default();
    for candidate in candidates {
        if candidate.is_offered_by_default() {
            plan.primary.push(candidate);
        } else if candidate.is_suggestion() {
            plan.suggestions.push(candidate);
        }
    }
    // Instructional before descriptive, then shallowest first: a root README
    // describes the project, one four directories down describes a corner of it.
    plan.suggestions.sort_by(|a, b| {
        a.tier
            .cmp(&b.tier)
            .then_with(|| depth_of(&a.path).cmp(&depth_of(&b.path)))
            .then_with(|| a.path.cmp(&b.path))
    });
    plan.hidden_suggestions = plan.suggestions.len().saturating_sub(MAX_SUGGESTIONS);
    plan.suggestions.truncate(MAX_SUGGESTIONS);
    plan
}

/// Directory names that are never worth descending into.
///
/// `node_modules` is the one that matters: a depth-4 scan of a JavaScript
/// workspace without this exclusion reaches tens of thousands of files, nearly
/// all of them a dependency's README.
const EXCLUDED_SEGMENTS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".cache",
    "coverage",
];

/// The two files people actually write agent steering into.
///
/// `AGENTS.md` is the cross-tool convention; `CLAUDE.md` is Claude Code's. Both
/// are written *as instructions to an agent*, which is what makes them the only
/// ones worth surfacing without being asked.
pub const PRIMARY_INSTRUCTION_NAMES: &[&str] = &["agents.md", "claude.md"];

/// Other filenames whose purpose is to instruct, offered only as suggestions.
const INSTRUCTIONAL_NAMES: &[&str] = &[
    "agent.md",
    "contributing.md",
    "gemini.md",
    "copilot-instructions.md",
];

/// Directory segments belonging to an agent tool's configuration.
const AGENT_TOOL_SEGMENTS: &[&str] = &[".claude", ".agents", ".cursor", ".stella", ".codex"];

/// Filenames that record history rather than current state.
const HISTORICAL_NAMES: &[&str] = &["changelog.md", "history.md", "releases.md", "changes.md"];

/// Filenames that carry no steering content at all.
const BOILERPLATE_NAMES: &[&str] = &[
    "license.md",
    "licence.md",
    "licensing.md",
    "notice.md",
    "code_of_conduct.md",
    "cla.md",
    "security.md",
];

/// Phrases that mean *this document has been replaced or withdrawn*.
///
/// Note the trailing `d` on `superseded`. A live document routinely says it
/// "supersedes" an older draft, and matching the shorter stem would demote the
/// canonical spec in favour of the draft it replaced — precisely backwards.
const SUPERSESSION_PHRASES: &[&str] = &[
    "superseded",
    "do not implement",
    "no longer accurate",
    "obsolete",
    "deprecated",
    "out of date",
    "historical reference",
];

/// `true` if any path segment is one discovery never descends into.
pub fn is_excluded_segment(path: &str) -> bool {
    path.split('/').any(|seg| EXCLUDED_SEGMENTS.contains(&seg))
}

/// `true` if `path` is within [`MAX_DEPTH`] directories of the workspace root.
///
/// A root-level `README.md` is depth 0.
pub fn is_within_depth(path: &str) -> bool {
    path.trim_matches('/').split('/').count().saturating_sub(1) <= MAX_DEPTH
}

/// `true` for the file extensions discovery considers at all.
pub fn is_markdown(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".md") || lower.ends_with(".mdx")
}

fn file_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase()
}

fn segments(path: &str) -> Vec<String> {
    path.split('/').map(|s| s.to_ascii_lowercase()).collect()
}

/// The supersession phrase found in the document's head, if any.
///
/// Only the first [`HEAD_LINES`] lines are read. A document that discusses
/// deprecation in its body is not itself deprecated, and scanning further would
/// flag every migration guide in the tree.
pub fn supersession_marker(content: &str) -> Option<String> {
    content.lines().take(HEAD_LINES).find_map(|line| {
        let lower = line.to_ascii_lowercase();
        SUPERSESSION_PHRASES
            .iter()
            .find(|phrase| lower.contains(**phrase))
            .map(|phrase| (*phrase).to_string())
    })
}

/// `true` when the body is a table of contents pointing at sibling documents.
///
/// The trap this exists for: a memories directory usually carries an index
/// listing every entry as a bullet. Ingesting it mints one duplicate claim per
/// file the index names, and those duplicates look exactly like real records.
pub fn is_index_of_siblings(content: &str) -> bool {
    let bullets: Vec<&str> = content
        .lines()
        .map(str::trim_start)
        .filter(|line| line.starts_with("- ") || line.starts_with("* "))
        .collect();
    if bullets.len() < MIN_INDEX_BULLETS {
        return false;
    }
    let linked = bullets
        .iter()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("](") && (lower.contains(".md)") || lower.contains(".md#"))
        })
        .count();
    (linked as f64) / (bullets.len() as f64) >= INDEX_LINK_RATIO
}

/// Classify one discovered markdown document.
///
/// `path` is workspace-relative and forward-slashed; `content` is the file's
/// text (only its head and bullet structure are examined, so passing a
/// truncated prefix is fine as long as it covers the first [`HEAD_LINES`]
/// lines).
///
/// Signals are collected in check order and the tier is the strongest verdict
/// among them — [`Tier::Skip`] beats [`Tier::Historical`] beats
/// [`Tier::Instructional`], and a document with nothing to say about itself is
/// [`Tier::Descriptive`].
///
/// Note that a supersession marker demotes an instructional file too: a
/// retired `AGENTS.md` is exactly as misleading as a retired design note, and
/// arguably more so, because its whole voice is imperative.
pub fn classify(path: &str, content: &str) -> Candidate {
    let mut signals = Vec::new();
    let name = file_name(path);
    let segs = segments(path);

    if is_excluded_segment(path) {
        signals.push(Signal::Excluded);
    }
    if BOILERPLATE_NAMES.contains(&name.as_str()) {
        signals.push(Signal::Boilerplate);
    }
    if is_index_of_siblings(content) {
        signals.push(Signal::IndexOfSiblings);
    }
    if HISTORICAL_NAMES.contains(&name.as_str()) {
        signals.push(Signal::HistoricalName);
    }
    if let Some(phrase) = supersession_marker(content) {
        signals.push(Signal::SupersededMarker(phrase));
    }
    if PRIMARY_INSTRUCTION_NAMES.contains(&name.as_str()) {
        signals.push(Signal::PrimaryInstructionFile);
    }
    if INSTRUCTIONAL_NAMES.contains(&name.as_str()) {
        signals.push(Signal::InstructionalName);
    }
    // The directory carries the meaning here, so a file named anything at all
    // under `.claude/rules/` is instructional. Both spellings of the memory
    // directory occur in the wild; match either.
    if segs
        .iter()
        .any(|s| AGENT_TOOL_SEGMENTS.contains(&s.as_str()))
    {
        signals.push(Signal::AgentToolDirectory);
    }
    if segs.iter().any(|s| s == "memories" || s == "memory") {
        signals.push(Signal::MemoryDirectory);
    }

    let tier = tier_for(&signals);
    Candidate {
        path: path.to_string(),
        tier,
        signals,
    }
}

/// Resolve collected signals into a single verdict.
///
/// Split out from [`classify`] so the precedence between signals is one
/// readable expression rather than a chain of early returns.
fn tier_for(signals: &[Signal]) -> Tier {
    let has = |want: &Signal| signals.iter().any(|s| s == want);
    let superseded = signals
        .iter()
        .any(|s| matches!(s, Signal::SupersededMarker(_)));

    if has(&Signal::Excluded) || has(&Signal::Boilerplate) || has(&Signal::IndexOfSiblings) {
        return Tier::Skip;
    }
    if superseded || has(&Signal::HistoricalName) {
        return Tier::Historical;
    }
    if has(&Signal::PrimaryInstructionFile) {
        return Tier::Primary;
    }
    if has(&Signal::InstructionalName)
        || has(&Signal::AgentToolDirectory)
        || has(&Signal::MemoryDirectory)
    {
        return Tier::Instructional;
    }
    Tier::Descriptive
}

/// Classify a whole scan, dropping anything that is not markdown or is deeper
/// than [`MAX_DEPTH`], and returning candidates sorted by tier then path.
///
/// Sorting by tier puts the documents worth offering at the front, which is the
/// order a first-run dialog wants to render.
pub fn classify_all<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = files
        .into_iter()
        .filter(|(path, _)| is_markdown(path) && is_within_depth(path))
        .map(|(path, content)| classify(path, content))
        .collect();
    out.sort_by(|a, b| a.tier.cmp(&b.tier).then_with(|| a.path.cmp(&b.path)));
    out
}
