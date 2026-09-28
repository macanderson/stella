//! Reads one `steering-record/v1` file from an Oxagen steering repo.
//!
//! The file is YAML frontmatter between two `---` lines, a blank line,
//! and the body. The body is the rule. This reader follows Oxagen's
//! `record.ts`.
//!
//! Oxagen sets `id` and `hash` when a steering PR merges. This module
//! keeps both as they are. It never hashes them again.
//!
//! A file with the wrong `schema` fails first. The module does no I/O.

use serde::{Deserialize, Deserializer};
use serde_norway::Value;

use super::super::context_record::kind::{Origin, RecordStatus};
use super::record::{AppliesTo, Force, Provenance, Record, RecordKind, SharingScope, Steering, Tier};

/// The schema tag each steering record has.
pub const STEERING_SCHEMA: &str = "steering-record/v1";

/// The longest label Oxagen takes, counted in UTF-16 units.
pub const LABEL_MAX: usize = 36;

/// Why a steering record file was refused.
///
/// A serde error keeps only its text. So the error stays `Clone` and can
/// be compared.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SteeringRecordError {
    /// The file has no bytes.
    #[error("the file is empty")]
    EmptyFile,
    /// The file starts with a UTF-8 byte order mark.
    #[error("the file starts with a byte order mark. Save it as UTF-8 without one.")]
    ByteOrderMark,
    /// The file has CRLF line endings.
    #[error("line {line} has a carriage return. Save the file with LF line endings.")]
    CarriageReturn {
        /// The line of the first carriage return, counting from 1.
        line: usize,
    },
    /// The last byte of the file is not a newline.
    #[error("the file does not end with a newline")]
    MissingFinalNewline,
    /// The first line is not `---`.
    #[error("the first line is not `---`, so the file has no frontmatter")]
    MissingFence,
    /// No later line is `---`.
    #[error("no `---` line closes the frontmatter")]
    MissingClosingFence,
    /// The YAML is bad, or a field has the wrong type.
    #[error("the frontmatter does not parse: {0}")]
    Frontmatter(String),
    /// The frontmatter is not a map.
    #[error("the frontmatter is not a YAML mapping")]
    NotAMapping,
    /// A mapping key is not a string.
    #[error("the frontmatter has a key that is not a string")]
    NonStringKey,
    /// A value carries a YAML tag such as `!foo`.
    #[error("the frontmatter uses a YAML tag. Remove it.")]
    Tagged,
    /// The frontmatter has no `schema` field.
    #[error("the frontmatter has no `schema` field")]
    MissingSchema,
    /// The `schema` field names another format.
    #[error("the schema is `{found}`, and this reader reads only `steering-record/v1`")]
    WrongSchema {
        /// The schema the file declared.
        found: String,
    },
    /// The `kind` field is not a v1 kind.
    #[error("`{0}` is not a steering-record/v1 kind")]
    UnknownKind(String),
    /// A field holds a value v1 does not allow.
    #[error("`{field}` is `{value}`, and it must be one of {expected}")]
    InvalidValue {
        /// The field.
        field: &'static str,
        /// The value the file holds.
        value: String,
        /// The values v1 allows.
        expected: &'static str,
    },
    /// A field breaks some other v1 rule.
    #[error("`{field}` {detail}")]
    Invalid {
        /// The field.
        field: &'static str,
        /// What is wrong with it.
        detail: &'static str,
    },
    /// The body has no text.
    #[error("the body is empty, and the body is the record's statement")]
    EmptyBody,
    /// Stella has no record kind for this kind.
    #[error("Stella has no record kind for a `{0}` record, so it does not load one")]
    UnsupportedKind(&'static str),
}

/// A steering record's kind, as v1 spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SteeringKind {
    /// A rule about the business the code serves.
    BusinessRule,
    /// A rule about the code itself.
    CodeRule,
    /// A gate with an effect of require or forbid.
    Constraint,
    /// A sequence of steps.
    Procedure,
    /// A skill folder's record.
    Skill,
    /// A claim you can check.
    Fact,
    /// A soft preference.
    Preference,
    /// A memory a steering PR promoted.
    Memory,
}

impl SteeringKind {
    /// Every v1 kind, in the order the schema lists them.
    pub const ALL: [Self; 8] = [
        Self::BusinessRule,
        Self::CodeRule,
        Self::Constraint,
        Self::Procedure,
        Self::Skill,
        Self::Fact,
        Self::Preference,
        Self::Memory,
    ];

    /// The v1 spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BusinessRule => "business-rule",
            Self::CodeRule => "code-rule",
            Self::Constraint => "constraint",
            Self::Procedure => "procedure",
            Self::Skill => "skill",
            Self::Fact => "fact",
            Self::Preference => "preference",
            Self::Memory => "memory",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }

    /// The Stella kind this kind loads as.
    ///
    /// Both rule kinds load as a rule. A skill gets `None`, since Stella
    /// has no skill kind.
    pub fn record_kind(self) -> Option<RecordKind> {
        match self {
            Self::BusinessRule | Self::CodeRule => Some(RecordKind::Rule),
            Self::Constraint => Some(RecordKind::Constraint),
            Self::Procedure => Some(RecordKind::Procedure),
            Self::Fact => Some(RecordKind::Fact),
            Self::Preference => Some(RecordKind::Preference),
            Self::Memory => Some(RecordKind::Memory),
            Self::Skill => None,
        }
    }
}

/// Who a steering record reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SteeringScope {
    /// The workspace's own repository.
    Workspace,
    /// The linked repositories `repos` names.
    Repository,
    /// Every workspace in the organization.
    Organization,
}

impl SteeringScope {
    /// The v1 spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Repository => "repository",
            Self::Organization => "organization",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [Self::Workspace, Self::Repository, Self::Organization]
            .into_iter()
            .find(|scope| scope.as_str() == value)
    }

    /// The audience this scope loads as.
    ///
    /// Stella has no workspace audience. A workspace record steers the repo
    /// it is read from. So `workspace` loads as `Repository`.
    pub fn sharing_scope(self) -> SharingScope {
        match self {
            Self::Workspace | Self::Repository => SharingScope::Repository,
            Self::Organization => SharingScope::Organization,
        }
    }
}

/// A constraint's effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SteeringEffect {
    /// The constraint requires an action.
    Require,
    /// The constraint forbids an action.
    Forbid,
}

impl SteeringEffect {
    /// The v1 spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Require => "require",
            Self::Forbid => "forbid",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [Self::Require, Self::Forbid]
            .into_iter()
            .find(|effect| effect.as_str() == value)
    }
}

/// When a steering record reaches a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SteeringLoad {
    /// On every turn.
    Always,
    /// On a turn that touches a path `applies_to` names.
    Match,
    /// On a turn the gateway judges relevant.
    Relevant,
    /// Only when a person or an agent mentions the record.
    Mention,
}

impl SteeringLoad {
    /// The v1 spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Match => "match",
            Self::Relevant => "relevant",
            Self::Mention => "mention",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [Self::Always, Self::Match, Self::Relevant, Self::Mention]
            .into_iter()
            .find(|load| load.as_str() == value)
    }

    /// The Stella tier for this load.
    ///
    /// Stella has no way to load on a mention. So `mention` loads like
    /// `relevant`.
    pub fn tier(self) -> Tier {
        match self {
            Self::Always => Tier::Pinned,
            Self::Match => Tier::Scoped,
            Self::Relevant | Self::Mention => Tier::Retrieved,
        }
    }
}

/// Where a steering record came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SteeringSource {
    /// An Oxagen proposal.
    Proposal,
    /// An agent run's memories.
    Run,
    /// An import from another format.
    Import,
}

impl SteeringSource {
    /// The v1 spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposal => "proposal",
            Self::Run => "run",
            Self::Import => "import",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [Self::Proposal, Self::Run, Self::Import]
            .into_iter()
            .find(|source| source.as_str() == value)
    }
}

/// One memory a `source: run` record cites.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteeringMemory {
    /// The agent that formed the memory. It may be null.
    #[serde(deserialize_with = "nullable")]
    pub agent: Option<String>,
    /// The run that formed the memory. It may be null.
    #[serde(deserialize_with = "nullable")]
    pub run: Option<String>,
    /// What the memory says.
    pub statement: String,
    /// The evidence behind it.
    pub evidence: Vec<String>,
}

/// A steering record's provenance block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteeringProvenance {
    /// What produced the record.
    pub source: SteeringSource,
    /// Where it came from, such as `oxagen:proposal/prp_01K5RW2P`.
    pub uri: String,
    /// The agent that proposed it, when one did.
    pub agent: Option<String>,
    /// The memories a `source: run` record cites. Empty when missing.
    pub memories: Vec<SteeringMemory>,
}

/// One parsed `steering-record/v1` file, with every field.
///
/// A missing list is empty here. A list the file gives must have items. So
/// empty means missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteeringRecord {
    /// The idea the record states, such as `a-intel.core-platform.refunds-over-100`.
    pub lineage: String,
    /// A short name for people.
    pub label: String,
    /// A longer summary.
    pub description: Option<String>,
    /// The kind.
    pub kind: SteeringKind,
    /// A skill's name.
    pub name: Option<String>,
    /// A constraint's effect.
    pub effect: Option<SteeringEffect>,
    /// How hard the record steers.
    pub force: Force,
    /// Who the record reaches.
    pub scope: SteeringScope,
    /// The linked repositories a `repository` record reaches.
    pub repos: Vec<String>,
    /// The tools the record is about.
    pub tools: Vec<String>,
    /// The skills the record is about.
    pub skills: Vec<String>,
    /// The toolbelt the record is about.
    pub toolbelt: Option<String>,
    /// The path globs the record is about.
    pub applies_to: Vec<String>,
    /// When the record reaches a turn, as the file declares it.
    pub load: Option<SteeringLoad>,
    /// `active` or `archived`.
    pub status: RecordStatus,
    /// `user` or `inferred`.
    pub origin: Origin,
    /// Where the record came from.
    pub provenance: SteeringProvenance,
    /// Oxagen's record id, when Oxagen has stamped one.
    pub id: Option<String>,
    /// Oxagen's record hash, when Oxagen has stamped one.
    pub hash: Option<String>,
    /// The body, without the newlines at each end.
    pub statement: String,
}

impl SteeringRecord {
    /// Parse one steering record file.
    ///
    /// It checks the bytes, the fences, the YAML, the schema, the fields,
    /// and the body. It returns the first failure.
    pub fn parse(text: &str) -> Result<Self, SteeringRecordError> {
        check_encoding(text)?;
        let (frontmatter, body) = split(text)?;
        let value: Value = serde_norway::from_str(&frontmatter)
            .map_err(|err| SteeringRecordError::Frontmatter(err.to_string()))?;
        refuse_tags_and_keys(&value)?;
        let Value::Mapping(mut mapping) = value else {
            return Err(SteeringRecordError::NotAMapping);
        };
        // Check the schema, then drop it. The typed read maps the rest.
        match mapping.shift_remove("schema") {
            None => return Err(SteeringRecordError::MissingSchema),
            Some(Value::String(schema)) if schema == STEERING_SCHEMA => {}
            Some(other) => {
                return Err(SteeringRecordError::WrongSchema {
                    found: describe(&other),
                });
            }
        }
        let raw: RawFrontmatter = serde_norway::from_value(Value::Mapping(mapping))
            .map_err(|err| SteeringRecordError::Frontmatter(err.to_string()))?;
        if body.trim().is_empty() {
            return Err(SteeringRecordError::EmptyBody);
        }
        let statement = body.trim_matches('\n').to_string();
        raw.into_record(statement)
    }

    /// The load this record uses.
    ///
    /// A declared load wins. Else `must` and `should` load `always`. The
    /// rest load `relevant`.
    pub fn effective_load(&self) -> SteeringLoad {
        self.load.unwrap_or(if self.force.is_always_injected() {
            SteeringLoad::Always
        } else {
            SteeringLoad::Relevant
        })
    }

    /// Turn this steering record into a Stella [`Record`].
    ///
    /// `id` and `hash` pass through as they are. An effect becomes a tag,
    /// such as `effect:forbid`. No guard is set from it. A skill fails with
    /// `UnsupportedKind`.
    pub fn to_record(&self) -> Result<Record, SteeringRecordError> {
        let kind = self
            .kind
            .record_kind()
            .ok_or_else(|| SteeringRecordError::UnsupportedKind(self.kind.as_str()))?;
        let applies_to = (!self.applies_to.is_empty()).then(|| AppliesTo {
            paths: self.applies_to.clone(),
            ..AppliesTo::default()
        });
        Ok(Record {
            lineage_id: self.lineage.clone(),
            record_id: self.id.clone(),
            record_hash: self.hash.clone(),
            kind,
            statement: self.statement.clone(),
            tags: self
                .effect
                .map(|effect| vec![format!("effect:{}", effect.as_str())])
                .unwrap_or_default(),
            origin: Some(self.origin),
            sharing_scope: Some(self.scope.sharing_scope()),
            status: Some(self.status),
            supersedes_record_id: None,
            provenance: Some(Provenance {
                source_kind: Some(self.provenance.source.as_str().to_string()),
                source_uri: Some(self.provenance.uri.clone()),
                ..Provenance::default()
            }),
            steering: Some(Steering {
                force: self.force,
                precedence: None,
                applies_to,
                tier: Some(self.effective_load().tier()),
            }),
            enforcement: None,
            truth: None,
            links: Vec::new(),
        })
    }
}

/// The frontmatter as YAML gives it.
///
/// `parse` has checked and dropped `schema`. Each optional field uses
/// `some`. So a `null` fails and does not read as missing.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFrontmatter {
    lineage: String,
    label: String,
    #[serde(default, deserialize_with = "some")]
    description: Option<String>,
    kind: String,
    #[serde(default, deserialize_with = "some")]
    name: Option<String>,
    #[serde(default, deserialize_with = "some")]
    effect: Option<String>,
    force: String,
    scope: String,
    #[serde(default, deserialize_with = "some")]
    repos: Option<Vec<String>>,
    #[serde(default, deserialize_with = "some")]
    tools: Option<Vec<String>>,
    #[serde(default, deserialize_with = "some")]
    skills: Option<Vec<String>>,
    #[serde(default, deserialize_with = "some")]
    toolbelt: Option<String>,
    #[serde(default, deserialize_with = "some")]
    applies_to: Option<Vec<String>>,
    #[serde(default, deserialize_with = "some")]
    load: Option<String>,
    status: String,
    origin: String,
    provenance: RawProvenance,
    #[serde(default, deserialize_with = "some")]
    id: Option<String>,
    #[serde(default, deserialize_with = "some")]
    hash: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProvenance {
    source: String,
    uri: String,
    #[serde(default, deserialize_with = "some")]
    agent: Option<String>,
    #[serde(default, deserialize_with = "some")]
    memories: Option<Vec<SteeringMemory>>,
}

impl RawFrontmatter {
    fn into_record(self, statement: String) -> Result<SteeringRecord, SteeringRecordError> {
        let kind = SteeringKind::parse(&self.kind)
            .ok_or_else(|| SteeringRecordError::UnknownKind(self.kind.clone()))?;
        let force = one_of("force", &self.force, "must, should, may, or info", |value| {
            [Force::Must, Force::Should, Force::May, Force::Info]
                .into_iter()
                .find(|force| force.as_str() == value)
        })?;
        let scope = one_of(
            "scope",
            &self.scope,
            "workspace, repository, or organization",
            SteeringScope::parse,
        )?;
        let status = one_of("status", &self.status, "active or archived", |value| {
            match value {
                "active" => Some(RecordStatus::Active),
                "archived" => Some(RecordStatus::Archived),
                _ => None,
            }
        })?;
        let origin = one_of("origin", &self.origin, "user or inferred", |value| {
            match value {
                "user" => Some(Origin::User),
                "inferred" => Some(Origin::Inferred),
                _ => None,
            }
        })?;
        let effect = self
            .effect
            .as_deref()
            .map(|value| one_of("effect", value, "require or forbid", SteeringEffect::parse))
            .transpose()?;
        let load = self
            .load
            .as_deref()
            .map(|value| {
                one_of(
                    "load",
                    value,
                    "always, match, relevant, or mention",
                    SteeringLoad::parse,
                )
            })
            .transpose()?;
        let source = one_of(
            "provenance.source",
            &self.provenance.source,
            "proposal, run, or import",
            SteeringSource::parse,
        )?;

        if !is_lineage(&self.lineage) {
            return Err(invalid(
                "lineage",
                "must be lowercase letters, digits, dots, and hyphens, and start and end on a letter or digit",
            ));
        }
        let label_len = self.label.encode_utf16().count();
        if label_len == 0 || label_len > LABEL_MAX {
            return Err(invalid("label", "must be 1 to 36 characters long"));
        }
        if let Some(name) = &self.name
            && !is_name(name)
        {
            return Err(invalid(
                "name",
                "must be lowercase letters, digits, and hyphens, and start on a letter or digit",
            ));
        }
        if let Some(id) = &self.id
            && !is_record_id(id)
        {
            return Err(invalid(
                "id",
                "must be `rec_`, the lineage slug, `_`, and 12 lowercase hex digits",
            ));
        }
        if let Some(hash) = &self.hash
            && !is_record_hash(hash)
        {
            return Err(invalid(
                "hash",
                "must be `sha256:` and 64 lowercase hex digits",
            ));
        }
        let repos = present_list("repos", self.repos)?;
        let tools = present_list("tools", self.tools)?;
        let skills = present_list("skills", self.skills)?;
        let applies_to = present_list("applies_to", self.applies_to)?;
        if applies_to.iter().any(|glob| glob.is_empty()) {
            return Err(invalid("applies_to", "must not hold an empty path"));
        }
        let memories = present_list("provenance.memories", self.provenance.memories)?;

        if kind == SteeringKind::Constraint && effect.is_none() {
            return Err(invalid("effect", "is required on a constraint record"));
        }
        if kind == SteeringKind::Skill && self.name.is_none() {
            return Err(invalid("name", "is required on a skill record"));
        }
        if kind == SteeringKind::Skill && self.description.is_none() {
            return Err(invalid("description", "is required on a skill record"));
        }
        if scope == SteeringScope::Repository && repos.is_empty() {
            return Err(invalid("repos", "is required when the scope is repository"));
        }
        if source == SteeringSource::Run && memories.is_empty() {
            return Err(invalid(
                "provenance.memories",
                "is required when the source is run",
            ));
        }

        Ok(SteeringRecord {
            lineage: self.lineage,
            label: self.label,
            description: self.description,
            kind,
            name: self.name,
            effect,
            force,
            scope,
            repos,
            tools,
            skills,
            toolbelt: self.toolbelt,
            applies_to,
            load,
            status,
            origin,
            provenance: SteeringProvenance {
                source,
                uri: self.provenance.uri,
                agent: self.provenance.agent,
                memories,
            },
            id: self.id,
            hash: self.hash,
            statement,
        })
    }
}

/// Refuse the bytes Oxagen's `encodingIssues` refuses, in the same order.
fn check_encoding(text: &str) -> Result<(), SteeringRecordError> {
    if text.is_empty() {
        return Err(SteeringRecordError::EmptyFile);
    }
    if text.starts_with('\u{feff}') {
        return Err(SteeringRecordError::ByteOrderMark);
    }
    if let Some(at) = text.find('\r') {
        let line = text[..at].matches('\n').count() + 1;
        return Err(SteeringRecordError::CarriageReturn { line });
    }
    if !text.ends_with('\n') {
        return Err(SteeringRecordError::MissingFinalNewline);
    }
    Ok(())
}

/// Split the file at the first two `---` lines.
///
/// Oxagen's `splitRecordFile` does the same.
fn split(text: &str) -> Result<(String, String), SteeringRecordError> {
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.first() != Some(&"---") {
        return Err(SteeringRecordError::MissingFence);
    }
    let close = lines
        .iter()
        .skip(1)
        .position(|line| *line == "---")
        .map(|offset| offset + 1)
        .ok_or(SteeringRecordError::MissingClosingFence)?;
    Ok((lines[1..close].join("\n"), lines[close + 1..].join("\n")))
}

/// Refuse a YAML tag, or a key that is not text, at any depth.
///
/// The YAML crate expands anchors first. So this check cannot see them.
/// Oxagen's steering PR checks refuse them.
fn refuse_tags_and_keys(value: &Value) -> Result<(), SteeringRecordError> {
    match value {
        Value::Tagged(_) => Err(SteeringRecordError::Tagged),
        Value::Sequence(items) => items.iter().try_for_each(refuse_tags_and_keys),
        Value::Mapping(mapping) => mapping.iter().try_for_each(|(key, value)| {
            if !matches!(key, Value::String(_)) {
                return Err(SteeringRecordError::NonStringKey);
            }
            refuse_tags_and_keys(value)
        }),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => Ok(()),
    }
}

/// A short name for a wrong `schema` value.
fn describe(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::Sequence(_) => "a list".to_string(),
        Value::Mapping(_) => "a mapping".to_string(),
        Value::Tagged(_) => "a tagged value".to_string(),
    }
}

fn one_of<T>(
    field: &'static str,
    value: &str,
    expected: &'static str,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<T, SteeringRecordError> {
    parse(value).ok_or_else(|| SteeringRecordError::InvalidValue {
        field,
        value: value.to_string(),
        expected,
    })
}

fn invalid(field: &'static str, detail: &'static str) -> SteeringRecordError {
    SteeringRecordError::Invalid { field, detail }
}

/// A list the file gives must hold at least one item.
fn present_list<T>(
    field: &'static str,
    list: Option<Vec<T>>,
) -> Result<Vec<T>, SteeringRecordError> {
    match list {
        Some(items) if items.is_empty() => Err(invalid(field, "must hold at least 1 item")),
        Some(items) => Ok(items),
        None => Ok(Vec::new()),
    }
}

/// `^[a-z0-9][a-z0-9.-]*[a-z0-9]$`.
fn is_lineage(value: &str) -> bool {
    let bytes = value.as_bytes();
    let edge = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    bytes.len() >= 2
        && bytes.first().is_some_and(edge)
        && bytes.last().is_some_and(edge)
        && bytes
            .iter()
            .all(|byte| edge(byte) || *byte == b'.' || *byte == b'-')
}

/// `^[a-z0-9][a-z0-9-]*$`.
fn is_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    let word = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    bytes.first().is_some_and(word) && bytes.iter().all(|byte| word(byte) || *byte == b'-')
}

/// `^rec_[a-z0-9_]+_[0-9a-f]{12}$`.
fn is_record_id(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("rec_") else {
        return false;
    };
    let bytes = rest.as_bytes();
    // At least one slug byte, the `_` separator, and 12 hex digits.
    if bytes.len() < 14 {
        return false;
    }
    let (head, digest) = bytes.split_at(bytes.len() - 12);
    let Some((separator, slug)) = head.split_last() else {
        return false;
    };
    *separator == b'_'
        && !slug.is_empty()
        && slug
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
        && digest.iter().all(is_lower_hex)
}

/// `^sha256:[0-9a-f]{64}$`.
fn is_record_hash(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|digest| digest.len() == 64 && digest.as_bytes().iter().all(is_lower_hex))
}

fn is_lower_hex(byte: &u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)
}

/// An optional field that may not be `null`.
///
/// A missing key gives `None`. A `null` fails as the wrong type.
fn some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// A required field whose value may be `null`.
fn nullable<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)
}

#[cfg(test)]
mod tests;
