//! The TOML context-record surface — the shape `stella ingest` extracts into.
//!
//! These are the typed serde bodies behind `docs/design/adaptive-context/context-record-examples/*.toml`
//! and the format decided by ADR 0011 (*context records are TOML*). Ingest never
//! mints a published record directly: it mints [`Proposal`]s, each carrying the
//! [`Record`] it would create, the evidence for it, and a refutation verdict, for
//! a human to `keep`/`edit`/`ignore`.
//!
//! ## Purity
//!
//! Like the rest of `stella-core`, this module does no I/O. It defines the
//! record shape, merges file-level [`Defaults`] into each record, and stamps the
//! content-derived identity ([`Record::stamp`]). The TOML (de)serialization, the
//! model call that produces the claims, and the filesystem probes that refute
//! them all live in the `stella-cli` ingest command. Timestamps are RFC-3339
//! **strings** here so the crate stays free of a `toml` datetime dependency; the
//! CLI normalizes bare TOML datetimes to strings on the way in.
//!
//! ## Identity is derived, never allocated (README "identity is derived")
//!
//! [`Record::stamp`] fills `record_id` and `record_hash` from the record's own
//! content, exactly as the lifecycle ledger does: a hand-authored record omits
//! both and the loader stamps them on first validation, so re-ingesting unchanged
//! content is a no-op rather than a new revision. `record_hash` is the ADR 0004
//! canonical hash (RFC 8785 JSON over the record, `record_hash` removed from the
//! preimage) — the on-disk TOML surface never enters it, which is what makes the
//! Markdown→TOML move hash-neutral (ADR 0011).
//!
//! ## Vocabularies
//!
//! [`Origin`] and [`RecordStatus`] are the frozen enums from
//! [`super::super::context_record`] (ADR 0009) reused verbatim. The surface's own
//! axes — [`Force`], [`EnforcementMode`], [`TruthBasis`], [`ProbeKind`],
//! [`LinkRelation`] — are defined here because they are this surface's schema and
//! have no counterpart in the lifecycle types.

use serde::{Deserialize, Serialize};

use super::super::context_record::hash::{RecordHashError, record_hash};
use super::super::context_record::kind::{
    Origin, RecordProposalKind, RecordProposalStatus, RecordStatus,
};

/// The schema tag every context-record file carries in its header.
pub const SCHEMA_TAG: &str = "context-record/v0.1";

/// One `.toml` file's worth of context records or proposals.
///
/// A file is either a set of published `[[record]]`s (the `01`–`06` examples) or
/// a set of `[[proposal]]`s (`stella ingest` output, `05`). Both carry the same
/// header and `[defaults]`, so one container parses either; ingest only ever
/// writes the proposal form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextFile {
    /// The schema tag, [`SCHEMA_TAG`].
    pub schema: String,
    /// The record-set slug (`acme.web`) — the citation namespace for every
    /// record in the file.
    pub set_id: String,
    /// The ingest run that produced this file, if it is ingest output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingest_run_id: Option<String>,
    /// File-level fields inherited by every record unless it overrides them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<Defaults>,
    /// Published records (`[[record]]`).
    #[serde(default, rename = "record", skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<Record>,
    /// Proposals awaiting review (`[[proposal]]`).
    #[serde(default, rename = "proposal", skip_serializing_if = "Vec::is_empty")]
    pub proposals: Vec<Proposal>,
}

impl ContextFile {
    /// A fresh ingest-output file: schema tag, run id, and shared defaults, with
    /// no proposals yet.
    pub fn new_ingest(
        set_id: impl Into<String>,
        ingest_run_id: impl Into<String>,
        defaults: Defaults,
    ) -> Self {
        Self {
            schema: SCHEMA_TAG.to_string(),
            set_id: set_id.into(),
            ingest_run_id: Some(ingest_run_id.into()),
            defaults: Some(defaults),
            records: Vec::new(),
            proposals: Vec::new(),
        }
    }
}

/// File-level fields inherited by every record.
///
/// Provenance lives here rather than on each record so a file's records can be
/// hand-edited and regrouped without re-stating where they came from (README,
/// "provenance lives on the record, not on the file" — the default is the
/// ergonomic, any record may override).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Defaults {
    /// Default audience.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharing_scope: Option<SharingScope>,
    /// Default semantic origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// Default lifecycle status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<RecordStatus>,
    /// Default recurring-review interval (ISO-8601 duration, e.g. `P90D`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_every: Option<String>,
    /// Default provenance for every record in the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

/// Where a record came from. Merged from [`Defaults`] with per-record overrides.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    /// What kind of source produced it (`document`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
    /// The source path or URI (`CLAUDE.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_uri: Option<String>,
    /// The source's content digest, so a later re-ingest can tell it changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_digest: Option<String>,
    /// The `[start, end]` source line range this record was extracted from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_lines: Option<Vec<u32>>,
    /// The VCS remote of the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The commit the source was read at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The ingest run, when provenance is carried at the record level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingest_run_id: Option<String>,
    /// When extraction happened (RFC-3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extracted_at: Option<String>,
    /// Which model performed the extraction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extractor: Option<String>,
}

impl Provenance {
    /// Overlay `over` onto `self`, field by field: a value set in `over` wins,
    /// an absent one keeps the default. This is how a per-record provenance
    /// override merges onto the file defaults.
    pub fn overlaid_with(&self, over: &Provenance) -> Provenance {
        Provenance {
            source_kind: over
                .source_kind
                .clone()
                .or_else(|| self.source_kind.clone()),
            source_uri: over.source_uri.clone().or_else(|| self.source_uri.clone()),
            source_digest: over
                .source_digest
                .clone()
                .or_else(|| self.source_digest.clone()),
            source_lines: over
                .source_lines
                .clone()
                .or_else(|| self.source_lines.clone()),
            repo: over.repo.clone().or_else(|| self.repo.clone()),
            commit: over.commit.clone().or_else(|| self.commit.clone()),
            ingest_run_id: over
                .ingest_run_id
                .clone()
                .or_else(|| self.ingest_run_id.clone()),
            extracted_at: over
                .extracted_at
                .clone()
                .or_else(|| self.extracted_at.clone()),
            extractor: over.extractor.clone().or_else(|| self.extractor.clone()),
        }
    }
}

/// A proposal: the record ingest would create, the evidence, and the verdict.
///
/// Ingest mints proposals rather than published records because the content may
/// be authoritative while the *extraction* is always inferred (`05` header) —
/// some fraction of any ingested document is stale the moment it is read, and
/// splitting prose into atomic claims is a model call even for an explicit
/// instruction. The reviewer decides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    /// Stable `<slug>-<hash8>` identity, so re-ingesting the same claim lands in
    /// the same lineage and a decline can be remembered against it.
    pub candidate_id: String,
    /// The kind of record proposed (`directive` or `knowledge`).
    pub proposal_kind: RecordProposalKind,
    /// Whether it cleared extraction's own gate.
    pub status: RecordProposalStatus,
    /// The extractor's confidence, `0..=100`.
    pub confidence: u8,
    /// When the proposal was produced (RFC-3339).
    pub observed_at: String,
    /// Why it is (or would be) eligible — e.g. `explicit_instruction`. Open
    /// vocabulary: imported content is gated differently from mined behavior, so
    /// an explicit instruction in a tracked file does not need the distinct-task
    /// threshold a mined pattern must clear.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eligibility: Option<String>,
    /// Why it was dismissed, when `status = dismissed`
    /// (`quarantined_executable`, `compound_claim`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed_reason: Option<String>,
    /// The record this proposal would create.
    pub record: Record,
    /// The refutation verdict from the truth probe, when one ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refutation: Option<Refutation>,
    /// The quarantine record, when executable content was refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantine: Option<Quarantine>,
    /// The atomicity-validator finding, when the claim was compound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation: Option<Validation>,
}

/// One context record — the smallest quotable unit of agent context.
///
/// Atomic by construction: a record is atomic if it can receive exactly one
/// refutation verdict (README, "atomicity is functional, not stylistic"). Its
/// three orthogonal axes — [`Steering`], [`Enforcement`], [`Truth`] — separate
/// *how hard it pushes*, *how a violation is caught*, and *whether it is still
/// true*, which the current engine conflates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// The stable lineage slug (`ctx.acme.web.pkg-manager`), unchanged across
    /// revisions.
    pub lineage_id: String,
    /// Content-derived id — stamped by [`Record::stamp`], omitted when hand-authored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_id: Option<String>,
    /// Canonical record hash — stamped by [`Record::stamp`], omitted when hand-authored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_hash: Option<String>,
    /// The record's semantic kind.
    pub kind: RecordKind,
    /// The single-sentence claim.
    pub statement: String,
    /// Free-form browsing tags (never used for matching).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Merged origin. Populated by [`Record::stamp`] from [`Defaults`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// Merged audience. Populated by [`Record::stamp`] from [`Defaults`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharing_scope: Option<SharingScope>,
    /// Merged lifecycle status. Populated by [`Record::stamp`] from [`Defaults`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<RecordStatus>,
    /// Merged provenance. Populated by [`Record::stamp`] from [`Defaults`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    /// How hard the record steers and when it is injected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steering: Option<Steering>,
    /// How a violation is detected and what happens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforcement: Option<Enforcement>,
    /// Whether the claim is still accurate and how you would know.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truth: Option<Truth>,
    /// Typed links to other records (`supports`, `requires`, `contradicts`, …).
    #[serde(default, rename = "link", skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
}

impl Record {
    /// Merge `defaults` into this record and stamp its content-derived identity.
    ///
    /// Merge first (origin/sharing/status/provenance fall back to the file
    /// defaults), then hash: the hash covers the *resolved* record, so two files
    /// that express the same record with different default/override splits stamp
    /// the same id. Two passes, exactly as the lifecycle records do — the id is
    /// part of what the final hash covers, so a record whose hash did not cover
    /// its own id could be re-filed under another id without the hash noticing.
    pub fn stamp(&mut self, defaults: &Defaults) -> Result<(), RecordHashError> {
        self.origin = self.origin.or(defaults.origin);
        self.sharing_scope = self.sharing_scope.or(defaults.sharing_scope);
        self.status = self.status.or(defaults.status);
        self.provenance = Some(match (&self.provenance, &defaults.provenance) {
            (Some(rec), Some(def)) => def.overlaid_with(rec),
            (Some(rec), None) => rec.clone(),
            (None, Some(def)) => def.clone(),
            (None, None) => Provenance::default(),
        });

        // Pass 1: hash with identity empty to derive the id.
        self.record_id = None;
        self.record_hash = None;
        let seed = record_hash(self)?;
        let digest = seed.strip_prefix("sha256:").unwrap_or(&seed);
        self.record_id = Some(format!("rec_{}_{}", slug(&self.lineage_id), &digest[..12]));
        // Pass 2: seal, now that the id is part of the preimage.
        self.record_hash = Some(record_hash(self)?);
        Ok(())
    }

    /// The truth probe that may be re-run to re-check this record's claim, if
    /// any. Returns the record's own probe only when it is **honored** for the
    /// record's origin — a gated probe (`command_succeeds`/`http_ok`) is never
    /// honored on an imported/inferred record, so it is filtered out and can
    /// never run during a staleness sweep. `None` when there is no probe or it
    /// is gated. Origin defaults to `imported` (ingest output) when unset.
    pub fn honored_probe(&self) -> Option<&Probe> {
        let probe = self.truth.as_ref()?.probe.as_ref()?;
        let origin = self.origin.unwrap_or(Origin::Imported);
        super::gate::probe_honored(origin, probe.kind).then_some(probe)
    }

    /// The record's `on_expiry` policy string (`stale`/`drop`/`block`), if set —
    /// what to do when a re-check refutes the claim.
    pub fn on_expiry(&self) -> Option<&str> {
        self.truth.as_ref()?.on_expiry.as_deref()
    }
}

/// Normalize a lineage id into the id-body slug (`ctx.acme.web.pkg-manager` →
/// `acme_web_pkg_manager`): dots and dashes to underscores, keeping only
/// `[a-z0-9_]`. Deterministic, so the `record_id` is stable across runs.
fn slug(lineage_id: &str) -> String {
    let trimmed = lineage_id.strip_prefix("ctx.").unwrap_or(lineage_id);
    trimmed
        .chars()
        .map(|c| match c {
            'A'..='Z' => c.to_ascii_lowercase(),
            'a'..='z' | '0'..='9' => c,
            _ => '_',
        })
        .collect()
}

/// How hard a record pushes behavior, and when it is injected.
///
/// `force` chooses the channel: `must`/`should` records are always injected and
/// ride the byte-stable system prefix; `may`/`info` records are relevance-selected
/// alongside memories (README, "two channels, chosen by force").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Steering {
    /// The steering force / injection channel.
    pub force: Force,
    /// Tie-break precedence when two records conflict (higher wins).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precedence: Option<u32>,
    /// When the record applies — drives selection for volatile records and
    /// scoring for cached ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applies_to: Option<AppliesTo>,
}

/// The conditions under which a record applies. All fields are optional and
/// disjunctive within a dimension; an empty `AppliesTo` matches everything.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AppliesTo {
    /// Glob path prefixes (`**`, `src/api/**`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// Task names (`install`, `build`, `ci`). Open vocabulary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<String>,
    /// Free keywords that raise relevance (`staging`, `docker`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
}

impl AppliesTo {
    /// True when nothing is constrained — the record applies unconditionally.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.tasks.is_empty() && self.keywords.is_empty()
    }
}

/// How a violation is detected and what happens. Deny-shaped by construction:
/// a guard can **block** a tool call at the boundary, never **cause** one
/// (README, "records describe; they never execute").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Enforcement {
    /// `hard` (block), `soft` (warn), or `none` (advisory).
    pub mode: EnforcementMode,
    /// The tool whose calls the guard scopes to (`Bash`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_tool: Option<String>,
    /// A command glob the guard denies (`npm *`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_deny_command: Option<String>,
    /// A path glob the guard denies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_deny_path: Option<String>,
    /// Violation severity (`error`, `warn`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    /// Prose the agent acts on when it violates (not a script the engine runs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_violation: Option<String>,
    /// How a soft check is judged (`model`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    /// The rubric a model-judged soft check applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rubric: Option<String>,
}

/// Whether the claim is still accurate, and how you would know.
///
/// `basis` says *why* it is believed (a decree is true because an owner said so;
/// a measured claim because the world agrees), and the [`Probe`] says how to
/// re-check without executing arbitrary code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Truth {
    /// Why the claim is believed true.
    pub basis: TruthBasis,
    /// Confidence in the claim, `0..=100`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<u8>,
    /// Who last confirmed a decree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_by: Option<String>,
    /// When it was last verified (RFC-3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<String>,
    /// When the claim became applicable (RFC-3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<String>,
    /// Time-to-live before the claim is considered stale (ISO-8601 duration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
    /// What to do when the TTL lapses (`stale`, `drop`, `block`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_expiry: Option<String>,
    /// Recurring re-verification interval (ISO-8601 duration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_every: Option<String>,
    /// The declarative probe that re-checks the claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<Probe>,
}

/// A declarative probe that answers "does this claim still hold?" without
/// executing arbitrary code. Gated kinds (`command_succeeds`, `http_ok`) are
/// **never** honored on an `imported` or `inferred` record — see
/// [`ProbeKind::is_gated`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    /// What the probe checks.
    pub kind: ProbeKind,
    /// A repo path (for `path_exists`/`path_absent`/`file_contains`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// A pattern to look for (`file_contains`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Whether the pattern is expected `present` (default) or `absent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<String>,
    /// A human note (for a `manual` probe).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A typed link from one record to another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Link {
    /// The relationship kind.
    pub relation: LinkRelation,
    /// The link target — a lineage id, or `evidence:<path>#Lnn`.
    pub target: String,
}

/// A three-valued refutation verdict, attached when a truth probe ran.
///
/// `unfalsifiable` must stay visible and must never be folded into `supported`
/// (README) — a refuter that reports OK for claims it never checked launders
/// unvalidated content with a validated stamp.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Refutation {
    /// `supported`, `refuted`, or `unfalsifiable`.
    pub verdict: Verdict,
    /// When the probe ran (RFC-3339).
    pub checked_at: String,
    /// The probe kind that produced the verdict (`none` when unfalsifiable).
    pub probe_kind: ProbeKind,
    /// A human-readable explanation of the verdict.
    pub detail: String,
    /// A suggested review action (`ignore`) for a refuted claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommend: Option<String>,
    /// Typed links recording what contradicted the claim.
    #[serde(default, rename = "link", skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
}

/// A quarantined field: the source asked for executable content and ingest
/// refused. The raw text is preserved for a human to read and ratify; it is
/// never a field the engine will execute.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quarantine {
    /// Why the content was quarantined.
    pub reason: String,
    /// Which field the content would have populated (`enforcement.autofix`).
    pub field: String,
    /// The verbatim content, preserved but never honored.
    pub raw: String,
    /// What ratification would require to honor it.
    pub requires: String,
    /// The origins on which it is never honored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub never_honored_when: Vec<String>,
}

/// An atomicity-validator finding: the claim carried more than one independently
/// refutable assertion, so it cannot receive a single verdict and must be split.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Validation {
    /// The validation rule that failed (`one_verdict_per_record`).
    pub rule: String,
    /// The verdict (`compound`).
    pub verdict: String,
    /// What was found.
    pub detail: String,
    /// The recommended action (`re-extract as N records`).
    pub action: String,
}

// ── Surface enums ────────────────────────────────────────────────────────────

/// A record's semantic kind — the flat surface vocabulary (README `kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    /// A durable recollection.
    Memory,
    /// A checkable claim about the world.
    Fact,
    /// A directive that steers behavior.
    Rule,
    /// A soft, often unfalsifiable, preference.
    Preference,
    /// A hard constraint (`require`/`forbid`).
    Constraint,
    /// A multi-step procedure.
    Procedure,
}

impl RecordKind {
    /// The canonical `snake_case` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Fact => "fact",
            Self::Rule => "rule",
            Self::Preference => "preference",
            Self::Constraint => "constraint",
            Self::Procedure => "procedure",
        }
    }

    /// The proposal kind a record of this semantic kind becomes: directives
    /// steer (`rule`/`constraint`/`preference`/`procedure`); `fact`/`memory`
    /// inform, so they are knowledge.
    pub fn proposal_kind(self) -> RecordProposalKind {
        match self {
            Self::Memory | Self::Fact => RecordProposalKind::Knowledge,
            Self::Rule | Self::Preference | Self::Constraint | Self::Procedure => {
                RecordProposalKind::Directive
            }
        }
    }
}

/// The steering force, which also selects the injection channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Force {
    /// Binding; always injected into the cached system prefix.
    Must,
    /// Strong; always injected into the cached system prefix.
    Should,
    /// Relevant-when-selected; rides the volatile block.
    May,
    /// Informational; rides the volatile block.
    Info,
}

impl Force {
    /// The canonical `snake_case` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Must => "must",
            Self::Should => "should",
            Self::May => "may",
            Self::Info => "info",
        }
    }

    /// Whether this force always injects (rides the cached prefix). `must` and
    /// `should` do; `may` and `info` are relevance-selected.
    pub fn is_always_injected(self) -> bool {
        matches!(self, Self::Must | Self::Should)
    }
}

/// How a violation is enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnforcementMode {
    /// Blocked at the tool boundary by a guard.
    Hard,
    /// Warned, not blocked.
    Soft,
    /// Advisory only.
    None,
}

impl EnforcementMode {
    /// The canonical `snake_case` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hard => "hard",
            Self::Soft => "soft",
            Self::None => "none",
        }
    }
}

/// Why a claim is believed true. Distinct from [`Origin`]: `origin` says where a
/// record came from, `basis` says why it is believed. `measured` (not
/// `observed`) keeps the two axes unambiguous when both appear on one record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TruthBasis {
    /// True because an owner said so; unrefutable by construction.
    Decree,
    /// True because the world was measured and agreed.
    Measured,
    /// True because it follows from other records.
    Derived,
    /// Asserted without a machine-checkable basis.
    Asserted,
}

impl TruthBasis {
    /// The canonical `snake_case` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Decree => "decree",
            Self::Measured => "measured",
            Self::Derived => "derived",
            Self::Asserted => "asserted",
        }
    }
}

/// What a truth probe checks. Gated kinds run a command or reach the network and
/// are never honored on imported/inferred records — see [`ProbeKind::is_gated`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeKind {
    /// A path exists in the repo.
    PathExists,
    /// A path is absent from the repo.
    PathAbsent,
    /// A tracked file contains (or, with `expect = absent`, lacks) a pattern.
    FileContains,
    /// A human re-verifies on a schedule.
    Manual,
    /// A command exits zero. **Gated.**
    CommandSucceeds,
    /// An HTTP endpoint responds. **Gated.**
    HttpOk,
    /// No probe can judge this claim.
    None,
}

impl ProbeKind {
    /// The canonical `snake_case` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PathExists => "path_exists",
            Self::PathAbsent => "path_absent",
            Self::FileContains => "file_contains",
            Self::Manual => "manual",
            Self::CommandSucceeds => "command_succeeds",
            Self::HttpOk => "http_ok",
            Self::None => "none",
        }
    }

    /// Whether this probe runs a command or reaches the network. A gated probe
    /// is an exfiltration channel when it comes from an untrusted document (an
    /// `http_ok` pointed at an attacker-chosen host leaks anything in a query
    /// string), so it is honored only on a decreed record with a human
    /// `verified_by`, and **never** on an imported or inferred one.
    pub fn is_gated(self) -> bool {
        matches!(self, Self::CommandSucceeds | Self::HttpOk)
    }
}

/// A typed relationship between records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkRelation {
    /// The source was derived from the target.
    DerivedFrom,
    /// The source refines the target.
    Refines,
    /// The source requires the target.
    Requires,
    /// The source supports the target.
    Supports,
    /// The source contradicts the target.
    Contradicts,
}

impl LinkRelation {
    /// The canonical `snake_case` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DerivedFrom => "derived_from",
            Self::Refines => "refines",
            Self::Requires => "requires",
            Self::Supports => "supports",
            Self::Contradicts => "contradicts",
        }
    }
}

/// A three-valued refutation verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The probe ran and the claim held.
    Supported,
    /// The probe ran and the claim did not hold.
    Refuted,
    /// No probe could judge the claim.
    Unfalsifiable,
}

impl Verdict {
    /// The canonical `snake_case` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Refuted => "refuted",
            Self::Unfalsifiable => "unfalsifiable",
        }
    }
}

/// The file-surface audience vocabulary (README `sharing_scope`).
///
/// This is the surface spelling — `personal`/`repository`/`organization` — used
/// in the TOML files. It is deliberately kept distinct from the ratified
/// audience enum [`super::super::context_record::SharingScope`]
/// (`user`/`repository`/`workspace`/`organization`, ADR 0002): when a proposal
/// is promoted into the ledger, `personal` maps to that enum's `user`. The
/// surface keeps its own spelling because ADR 0011 leaves the field schema to be
/// settled separately, and these files are the schema reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharingScope {
    /// Private to the user; `~/.stella/rules/`. Maps to the ledger `user` audience.
    Personal,
    /// The repository's git tree.
    Repository,
    /// The organization.
    Organization,
}

impl SharingScope {
    /// The canonical `snake_case` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Repository => "repository",
            Self::Organization => "organization",
        }
    }
}

#[cfg(test)]
mod tests;
