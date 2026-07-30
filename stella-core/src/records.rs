//! The published-record engine: loading `.toml` context records, deciding what
//! they steer, and proving they are still true.
//!
//! [`super::ingest`] mints **proposals**. This module is what happens after a human
//! keeps one: a `.stella/rules/*.toml` file becomes [`LoadedRecord`]s, each with a
//! verified identity, a citable [`handle`], a resolved truth [`sweep`]
//! disposition, and — for a `hard`-mode record that clears the
//! [`bridge`] gate — an armed Tier-2 guard.
//!
//! The chain the Context PR spec (`docs/context-pr.md` §4) asks for, and where
//! each link lives:
//!
//! | Stage | Here |
//! | --- | --- |
//! | published record → typed record | [`load_context_file`] |
//! | identity verified, hand-edit detected | [`load_context_file`] + [`RecordFinding::HashMismatch`] |
//! | still true? | [`sweep`] |
//! | schema-valid, non-conflicting? | [`validate`] |
//! | reaches the model, citably | [`handle`] + [`render`] |
//! | blocks a tool call | [`bridge`] |
//!
//! # Purity
//!
//! No I/O, like the rest of `stella-core`. TOML *deserialization* happens here
//! (the crate already depends on `toml`), but reading the file, walking the rule
//! directories, running a probe against a real filesystem, and writing a decision
//! ledger all belong to `stella-cli`. Everything in this module is a deterministic
//! function of its inputs, which is why the whole engine is testable from a string
//! literal.
//!
//! # Findings are attached, never swallowed
//!
//! A record with a problem still loads, carrying a [`RecordFinding`]. That is
//! deliberate, and it is the same principle the ingest gate follows: a loader that
//! silently dropped a malformed record would make a governed policy file quietly
//! stop steering, which is indistinguishable from the policy having been deleted.
//! A loader that silently *fixed* one would launder a hand-edit into an approved
//! record. So the record loads, the finding travels with it, and the caller —
//! selection, `stella context validate`, the guard gate — decides what a finding
//! of that severity means. [`Severity::Blocking`] findings must never reach the
//! model.

use super::ingest::record::{ContextFile, Defaults, ProbeKind, Record, SharingScope};

pub mod bridge;
pub mod clock;
pub mod decision;
pub mod handle;
pub mod registry;
pub mod render;
pub mod sweep;
pub mod trust;
pub mod validate;

pub use bridge::{
    BlockingRefusal, GuardDecision, declared_hard_guard, guard_for, rule_from_record,
};
pub use decision::{Decision, DecisionEvent, should_repropose};
pub use handle::assign_handles;
pub use registry::{Entry, Facts, Registry};
pub use render::{Channel, RenderInput, RenderedChannel, render_channel};
pub use sweep::{Disposition, ExpiryAction, SweepInput, disposition, honored_probe, probe_is_due};
pub use trust::Trust;
pub use validate::{Conflict, check_record, detect_conflicts, is_suspended, validate_records};

/// One record as the engine sees it: the typed record, where it came from, the
/// name it can be cited by, and everything wrong with it.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedRecord {
    /// The record, with file [`Defaults`] merged in and identity stamped.
    pub record: Record,
    /// The record-set slug from the file header — the citation namespace, and the
    /// tiebreaker when two sets both end a lineage in the same word.
    pub set_id: String,
    /// The file (or opaque source label) this record was read from. Carried into
    /// provenance so `stella context explain` can name the authority.
    pub source: String,
    /// Which authority published it — stamped from the directory the file was read
    /// out of, never from anything the record says about itself. [`load_context_file`]
    /// leaves this at [`Trust::Project`]; [`registry::load`] stamps the user tier,
    /// which is the one place that knows the difference.
    pub trust: Trust,
    /// The `^handle` the model cites this record by. Empty until
    /// [`assign_handles`] has seen the whole loaded set — a handle is only
    /// unambiguous relative to its neighbours.
    pub handle: String,
    /// Everything the loader noticed. Empty is the good case.
    pub findings: Vec<RecordFinding>,
}

impl LoadedRecord {
    /// The worst thing wrong with this record, or `None` when nothing is.
    pub fn severity(&self) -> Option<Severity> {
        self.findings.iter().map(RecordFinding::severity).max()
    }

    /// Whether this record may steer the model at all. A [`Severity::Blocking`]
    /// finding — a forbidden-data leak, an unparseable guard, a refuted claim —
    /// disqualifies it; anything softer is reported and still applied.
    pub fn is_selectable(&self) -> bool {
        self.record.status.is_none_or(|status| {
            matches!(status, super::context_record::kind::RecordStatus::Active)
        }) && self.severity() != Some(Severity::Blocking)
    }
}

/// How bad a [`RecordFinding`] is. Ordered, so `max()` over a record's findings
/// is its severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Worth saying, changes nothing. A hand-authored record that just had its
    /// identity stamped for the first time.
    Note,
    /// The record still steers, but something about it is wrong or unproven — an
    /// unrecognized task name, an equal-precedence conflict resolved by falling
    /// back to the less restrictive behavior.
    Warning,
    /// The record must not steer anything until a human resolves it.
    Blocking,
}

/// Something the engine noticed about a record.
///
/// Every variant names a *specific* defect rather than a generic "invalid",
/// because the output of a check is only actionable if it says what to do
/// (`docs/context-pr.md` §12: "check output must be concrete and actionable").
#[derive(Debug, Clone, PartialEq)]
pub enum RecordFinding {
    /// The record was hand-authored without `record_id`/`record_hash`, and the
    /// loader stamped them from its content. Not a problem: identity is derived
    /// (ADR 0004), so this is the designed path for a human-written record.
    IdentityStamped,
    /// The stored `record_hash` does not match the recomputed canonical hash —
    /// somebody edited the record's semantics by hand. The loader adopts the
    /// recomputed identity, which mints a **new revision**; it never writes the
    /// old hash back over the new content (§14, "hand-edit drift").
    HashMismatch {
        /// The hash the file claimed.
        stored: String,
        /// The hash the content actually has, and the record's identity from now on.
        recomputed: String,
    },
    /// `applies_to.tasks` named a task outside the ratified vocabulary. The record
    /// still loads, but the task will never match anything, so scoring is skewed
    /// by a name nobody will notice is wrong.
    UnknownTask(String),
    /// A guard field cannot be evaluated as written — an unparseable glob, an
    /// empty pattern, an unbounded scope. A guard that cannot be evaluated is a
    /// guard that silently never fires.
    GuardLint(String),
    /// The record carries content that must never enter a Git-tracked policy file
    /// (§10): a secret, a credential, raw tool output.
    ForbiddenData {
        /// Which field the forbidden content was found in.
        field: &'static str,
    },
    /// The record asked for a gated probe (`command_succeeds`, `http_ok`) on an
    /// origin where gated probes are never honored. Reported, not honored.
    GatedProbeRefused(ProbeKind),
    /// The record asked to block at the tool boundary and was refused. See
    /// [`BlockingRefusal`] for which precondition failed.
    BlockingRefused(BlockingRefusal),
    /// The record overlaps another at equal precedence with different
    /// enforcement, and nothing here resolves it (§12: never silently resolved).
    Conflict {
        /// The other record's handle.
        with: String,
        /// What overlaps, and what differs.
        detail: String,
    },
}

impl RecordFinding {
    /// This finding's severity — see [`Severity`].
    pub fn severity(&self) -> Severity {
        match self {
            Self::IdentityStamped => Severity::Note,
            Self::UnknownTask(_)
            | Self::GatedProbeRefused(_)
            | Self::BlockingRefused(_)
            | Self::Conflict { .. }
            | Self::HashMismatch { .. } => Severity::Warning,
            Self::GuardLint(_) | Self::ForbiddenData { .. } => Severity::Blocking,
        }
    }
}

impl std::fmt::Display for RecordFinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IdentityStamped => {
                write!(f, "identity stamped from content on first load")
            }
            Self::HashMismatch { stored, recomputed } => write!(
                f,
                "record_hash mismatch: file says {stored}, content hashes to {recomputed} \
                 — loaded as a new revision, the stored hash was not honored"
            ),
            Self::UnknownTask(task) => write!(
                f,
                "applies_to.tasks names \"{task}\", which is not a recognized task \
                 — it will never match, so it silently skews relevance"
            ),
            Self::GuardLint(detail) => write!(f, "guard cannot be evaluated: {detail}"),
            Self::ForbiddenData { field } => write!(
                f,
                "{field} contains data that must not enter a Git-tracked record \
                 (secret, credential, or raw tool text)"
            ),
            Self::GatedProbeRefused(kind) => write!(
                f,
                "probe kind {} runs a command or reaches the network, which is never \
                 honored on an imported or inferred record",
                kind.as_str()
            ),
            Self::BlockingRefused(refusal) => write!(f, "{refusal}"),
            Self::Conflict { with, detail } => write!(
                f,
                "conflicts with ^{with} at equal precedence: {detail} \
                 — falling back to the less restrictive behavior until an owner resolves it"
            ),
        }
    }
}

/// The ratified `applies_to.tasks` vocabulary (ADR 0011 field schema).
///
/// Closed on purpose, resolving the open question in
/// `docs/context-record-examples/README.md`: an open vocabulary means a typo
/// (`intall`) produces a record that matches nothing and reports nothing, which
/// looks identical to a record whose task simply never came up. A closed list with
/// a [`RecordFinding::UnknownTask`] warning keeps the typo visible while leaving
/// the record loadable.
pub const KNOWN_TASKS: &[&str] = &[
    "build", "ci", "deploy", "docs", "install", "lint", "migrate", "refactor", "release", "review",
    "run", "test",
];

/// Parse a `.toml` context file and resolve every record in it.
///
/// `source` is the path (or any opaque label — `store://…` works) recorded on each
/// record for provenance. Returns the parse error verbatim on malformed TOML: a
/// policy file that does not parse is a governance failure the caller must
/// surface, not a file to skip quietly.
///
/// Handles are **not** assigned here. Call [`assign_handles`] once over the whole
/// loaded set — a handle that is unique within one file can still collide with
/// another file's.
pub fn load_context_file(source: &str, contents: &str) -> Result<Vec<LoadedRecord>, LoadError> {
    let file: ContextFile = toml::from_str(contents).map_err(|err| LoadError {
        source: source.to_string(),
        detail: err.to_string(),
    })?;
    if file.schema != super::ingest::record::SCHEMA_TAG {
        return Err(LoadError {
            source: source.to_string(),
            detail: format!(
                "unknown schema \"{}\" (expected \"{}\")",
                file.schema,
                super::ingest::record::SCHEMA_TAG
            ),
        });
    }
    let defaults = file.defaults.clone().unwrap_or_default();
    Ok(file
        .records
        .into_iter()
        .map(|record| resolve(record, &defaults, &file.set_id, source))
        .collect())
}

/// Merge `defaults`, verify identity, and note anything wrong.
fn resolve(mut record: Record, defaults: &Defaults, set_id: &str, source: &str) -> LoadedRecord {
    let stored_hash = record.record_hash.clone();
    let mut findings = Vec::new();

    // Re-stamping IS the hash recomputation: `stamp` clears the stored identity,
    // merges defaults, and hashes the resolved record over the ADR 0004 canonical
    // preimage. Whatever the file claimed is therefore checked against what the
    // content actually is, with no second preimage implementation to drift.
    match record.stamp(defaults) {
        Ok(()) => {
            let recomputed = record.record_hash.clone().unwrap_or_default();
            match stored_hash {
                None => findings.push(RecordFinding::IdentityStamped),
                Some(stored) if stored != recomputed => {
                    findings.push(RecordFinding::HashMismatch { stored, recomputed });
                }
                Some(_) => {}
            }
        }
        Err(err) => findings.push(RecordFinding::GuardLint(format!(
            "record could not be canonicalized for hashing: {err}"
        ))),
    }

    // Provenance carries the file it was read from, so a record that arrived
    // without its own `source_uri` can still be traced back to an authority.
    if let Some(provenance) = record.provenance.as_mut()
        && provenance.source_uri.is_none()
    {
        provenance.source_uri = Some(source.to_string());
    }

    LoadedRecord {
        record,
        set_id: set_id.to_string(),
        source: source.to_string(),
        // The lower tier, until a caller that actually knows the directory says
        // otherwise. See [`Trust`] on why the default is the restrictive one.
        trust: Trust::default(),
        handle: String::new(),
        findings,
    }
}

/// A context file that could not be read as one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
    /// The file that failed.
    pub source: String,
    /// Why.
    pub detail: String,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.source, self.detail)
    }
}

impl std::error::Error for LoadError {}

/// The location a record of this sharing scope is published to, relative to the
/// substrate rule ADR 0011 leans on: **records live in files, and `sharing_scope`
/// selects which location** — the repository tree, or the user's own directory.
///
/// `organization` has no local location: an organization-scoped record is
/// published through a repository the organization owns, so it lands in the
/// repository tree like any other. Nothing here is a provider-hosted workspace
/// record, which is a separate channel (`docs/context-pr.md` §2).
pub fn publication_dir(scope: SharingScope) -> PublicationDir {
    match scope {
        SharingScope::Personal => PublicationDir::User,
        SharingScope::Repository | SharingScope::Organization => PublicationDir::Repository,
    }
}

/// Where a kept record is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationDir {
    /// `<repo>/.stella/rules/` — Git-tracked, reviewable, the Context PR payload.
    Repository,
    /// `~/.stella/rules/` — private to the user, never in a Context PR (§10).
    User,
}

#[cfg(test)]
mod tests;
