//! Workspace skills engine — filesystem-first reusable knowledge
//! (`<workspace>/.stella/skills/`, "skill.md files
//! (ADR-008 filesystem-first)").
//!
//! A **skill** is a reusable procedure, convention, or learned preference —
//! "this repo formats SQL with X", "the user prefers tables over prose for
//! comparisons". It is distinct from a [`crate::rules`] *rule*: a rule is a
//! constraint/guardrail that can hard-block a tool call (Tier 2); a skill is
//! know-how the model *applies* when relevant. Skills are never enforced —
//! they are selected, injected as context, and followed.
//!
//! This module is the local model the CLI's skill machinery operates on. It
//! covers, per the user requirement ("make sure skills can be created and do
//! get created automatically when something has been observed enough like a
//! style preference etc — skill selection and skill search are super
//! important"):
//!
//!   1. **Loading** ecosystem-compatible `SKILL.md` files (both the
//!      `<dir>/<slug>/SKILL.md` `npx skills` layout and the flat
//!      `<dir>/<slug>.md` layout), with per-file tolerance — a malformed
//!      file is skipped with a typed [`SkillDiagnostic`], never fatal.
//!   2. **Selection** ([`select_skills`]) — the "super important" part: pure
//!      lexical + domain scoring of the loaded skills against the current
//!      prompt, returning the top-k with an inspectable *why* (matched terms
//!      and domains).
//!   3. **Rendering** ([`render_skills_section`]) — the volatile context
//!      block the CLI injects **after** the byte-stable system prefix, so
//!      prompt-cache hits on the prefix are preserved (L-E8: recalled context
//!      is a live query at turn start that rides as a
//!      volatile message after the stable system block, never a cached prompt
//!      block).
//!   4. **Auto-creation** ([`mine_skill_candidates`], [`decide_auto_creation`])
//!      — mining recurring observations (style preferences, reflection
//!      lessons) into new skill files once something has been "observed
//!      enough", capped so it feels magical, not spammy.
//!   5. **Install vocabulary** ([`SkillInstallProposal`], [`InstallDecision`])
//!      — the typed shape the registry-search/install glue and a future TUI
//!      speak, so both sides agree on one contract.
//!
//! # No I/O in this module
//!
//! Discovering skill files means reading directories and file contents — real
//! I/O, which `stella-core` never performs directly. [`SkillSource`] is the
//! injectable discovery port, mirroring [`crate::rules::RuleSource`]. A
//! concrete implementation backed by real `std::fs` calls belongs to
//! `stella-cli`/`stella-tools`, never here. Everything downstream — frontmatter
//! parsing, precedence merging, selection, rendering, and candidate mining —
//! is plain synchronous logic over owned data, unit-tested below against a
//! fake `SkillSource`, no real files required.
//!
//! Built-in/seed skills the CLI ships are, L-L2,
//! embedded as `include_str!` compile-time data on the CLI side — nothing here
//! resolves anything relative to the binary's install path. This module
//! operates purely on already-loaded content regardless of where it came from.
//!
//! # Reuse of `crate::rules` and `crate::mining`
//!
//! Frontmatter splitting is shared: [`crate::rules::parse_frontmatter`] is
//! already `pub` and does exactly the BOM-strip + `---` fence + single-line
//! `key: value` parse this format needs (no YAML dependency), so it is reused
//! rather than duplicated. The lexical mining helpers (`terms`/`jaccard`/
//! `slugify`/`hash8`, clustering, representative selection) live once in
//! `crate::mining`, shared with the rules miner — they were briefly two
//! identical private copies, which is exactly the divergence trap the shared
//! module closes.

use std::collections::{HashMap, HashSet};

// Types

/// Where a skill came from — informs display and the small selection
/// tie-break that nudges freshly-learned preferences up (see
/// [`select_skills`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillOrigin {
    /// Authored under the workspace's `.stella/skills/`.
    Workspace,
    /// Authored under the user-global skills directory.
    User,
    /// Written by [`decide_auto_creation`] from mined observations — carried
    /// back through an `origin: auto` frontmatter marker.
    AutoCreated,
    /// Installed from a registry into the `<dir>/<slug>/SKILL.md` layout.
    Installed,
}

/// One workspace skill, parsed from a `SKILL.md`/`<slug>.md` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// Skill name — the frontmatter `name:` or the path-derived slug.
    pub name: String,
    /// Short description (frontmatter `description:`) — the primary signal
    /// [`select_skills`] scores the prompt against.
    pub description: String,
    /// Domain tags (frontmatter `domains:`, comma-separated or `[a, b]`),
    /// as authored — matched case-insensitively.
    pub domains: Vec<String>,
    /// The markdown body: the actual procedure/convention/preference.
    pub body: String,
    /// The file this was loaded from (or any opaque source label).
    pub source_path: String,
    /// Provenance — see [`SkillOrigin`].
    pub origin: SkillOrigin,
}

// Discovery port + parsing

/// One skill file's raw content, already read from disk by a [`SkillSource`]
/// implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillFile {
    /// The file's path — used both as [`Skill::source_path`] and, when no
    /// `name:` frontmatter is present, to derive the slug (both the
    /// `<dir>/<slug>/SKILL.md` and flat `<dir>/<slug>.md` layouts).
    pub path: String,
    pub content: String,
}

/// The filesystem discovery port for skill files,
/// mirroring [`crate::rules::RuleSource`]. A real implementation
/// (owned by `stella-cli`/`stella-tools`) walks each directory in `roots`, in
/// the given order, and returns every skill file's contents — discovering
/// **both** layouts: `<root>/<slug>/SKILL.md` (the `npx skills` ecosystem
/// layout; installed skills land this way) and flat `<root>/<slug>.md`.
/// Directories that don't exist are skipped silently. Order matters:
/// [`load_skills`] merges by skill name with **later roots overriding
/// earlier ones**, so the roots must already be in precedence order — see
/// [`skill_search_dirs`], where the workspace directory comes last so it wins
/// over a user-global skill of the same name.
pub trait SkillSource: Send + Sync {
    fn read_skill_files(&self, roots: &[String]) -> Vec<SkillFile>;
}

/// Why one skill file could not be loaded — a skill needs a name, a
/// description, and a body. Typed so the CLI can report it precisely instead
/// of silently dropping the file (house style: inspectable outputs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillProblem {
    /// No `name:` frontmatter and the path yields no usable slug.
    MissingName,
    /// No `description:` frontmatter (required by the `SKILL.md` shape).
    MissingDescription,
    /// The markdown body is empty — a skill with no procedure is useless.
    EmptyBody,
}

/// A malformed skill file, skipped during loading (never fatal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDiagnostic {
    pub path: String,
    pub problem: SkillProblem,
}

/// The result of a load: the merged skills plus every file that was skipped
/// and why. [`load_skills`] returns just the skills for the common path;
/// callers wanting to surface problems use [`load_skills_with_diagnostics`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadedSkills {
    pub skills: Vec<Skill>,
    pub diagnostics: Vec<SkillDiagnostic>,
}

/// Where to look for skills. Unlike a TS loader, `stella-core` never defaults
/// these from `cwd()`/`homedir()` itself — no I/O, not even the trivial kind —
/// so the caller always supplies both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadSkillsOptions {
    /// `<workspace>/.stella/skills` — highest precedence.
    pub workspace_skills_dir: String,
    /// The user-global skills directory (e.g. `~/.stella/skills`).
    pub user_skills_dir: String,
}

/// The skill directories in precedence order, **lowest first**: user-global,
/// then workspace. [`load_skills`] merges later-over-earlier, so the
/// workspace directory (last) wins on a name collision — "workspace beats
/// user-global".
pub fn skill_search_dirs(opts: &LoadSkillsOptions) -> Vec<String> {
    vec![
        opts.user_skills_dir.clone(),
        opts.workspace_skills_dir.clone(),
    ]
}

/// Derive a skill's slug from its path, honoring both layouts: for a
/// `<dir>/<slug>/SKILL.md` file the slug is the *parent directory* name; for
/// a flat `<dir>/<slug>.md` file it is the filename stem. The frontmatter
/// `name:` always overrides this (see [`skill_from_file_with_origin`]).
fn skill_name_from_path(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    if base.eq_ignore_ascii_case("SKILL.md") {
        return path.rsplit('/').nth(1).unwrap_or("").to_string();
    }
    base.strip_suffix(".md").unwrap_or(base).to_string()
}

/// Parse a `domains:` value: a bracketed inline list `[a, b]` or a bare
/// comma-separated `a, b`. Values are trimmed and unquoted; empties dropped.
/// Case is preserved as authored (matching is case-insensitive elsewhere).
fn parse_domains(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    // Dedup case-insensitively (keeping first occurrence) so a skill authored
    // with a repeated tag (`domains: sql, sql`) doesn't get its domain boost
    // double-counted in `select_skills` — matching `union_domains`.
    let mut seen = std::collections::HashSet::new();
    inner
        .split(',')
        .map(|part| {
            part.trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .trim()
                .to_string()
        })
        .filter(|d| !d.is_empty())
        .filter(|d| seen.insert(d.to_ascii_lowercase()))
        .collect()
}

/// Read an explicit `origin:` frontmatter marker into a [`SkillOrigin`].
/// `None` when absent or unrecognized, so the caller can fall back to the
/// directory-derived default.
fn origin_from_marker(marker: Option<&String>) -> Option<SkillOrigin> {
    match marker.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("auto" | "autocreated" | "auto-created") => Some(SkillOrigin::AutoCreated),
        Some("installed") => Some(SkillOrigin::Installed),
        Some("user") => Some(SkillOrigin::User),
        Some("workspace") => Some(SkillOrigin::Workspace),
        _ => None,
    }
}

/// Parse one skill file into a [`Skill`], tagging it with `default_origin`
/// unless the frontmatter carries an explicit `origin:` marker (which wins).
/// Returns a typed [`SkillDiagnostic`] instead of a [`Skill`] when the file
/// is missing a name, a description, or a body.
pub fn skill_from_file_with_origin(
    path: &str,
    raw: &str,
    default_origin: SkillOrigin,
) -> Result<Skill, SkillDiagnostic> {
    let fm = crate::rules::parse_frontmatter(raw);
    let diag = |problem| SkillDiagnostic {
        path: path.to_string(),
        problem,
    };

    let name = fm
        .data
        .get("name")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| skill_name_from_path(path));
    if name.trim().is_empty() {
        return Err(diag(SkillProblem::MissingName));
    }

    let description = fm
        .data
        .get("description")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| diag(SkillProblem::MissingDescription))?;

    if fm.body.trim().is_empty() {
        return Err(diag(SkillProblem::EmptyBody));
    }

    let domains = fm
        .data
        .get("domains")
        .map(|s| parse_domains(s))
        .unwrap_or_default();
    let origin = origin_from_marker(fm.data.get("origin")).unwrap_or(default_origin);

    Ok(Skill {
        name: name.trim().to_string(),
        description,
        domains,
        body: fm.body.trim().to_string(),
        source_path: path.to_string(),
        origin,
    })
}

/// Parse one skill file, defaulting its origin to [`SkillOrigin::Workspace`]
/// (used standalone and in tests; [`load_skills`] supplies a directory-derived
/// default instead).
pub fn skill_from_file(path: &str, raw: &str) -> Result<Skill, SkillDiagnostic> {
    skill_from_file_with_origin(path, raw, SkillOrigin::Workspace)
}

/// The default origin for a file at `path`, from which configured directory it
/// lives under (frontmatter markers override this in
/// [`skill_from_file_with_origin`]). Longest-matching directory wins so a
/// workspace dir nested under the user dir is still attributed correctly;
/// anything under neither is treated as workspace-local.
fn default_origin_for(path: &str, opts: &LoadSkillsOptions) -> SkillOrigin {
    let ws = opts.workspace_skills_dir.trim_end_matches('/');
    let us = opts.user_skills_dir.trim_end_matches('/');
    let under =
        |dir: &str| !dir.is_empty() && (path == dir || path.starts_with(&format!("{dir}/")));
    match (under(ws), under(us)) {
        (true, true) => {
            if ws.len() >= us.len() {
                SkillOrigin::Workspace
            } else {
                SkillOrigin::User
            }
        }
        (true, false) => SkillOrigin::Workspace,
        (false, true) => SkillOrigin::User,
        (false, false) => SkillOrigin::Workspace,
    }
}

/// Load every skill visible from `opts`, merged by name across sources, and
/// report every file that was skipped and why. Precedence: a later root
/// (workspace) overrides an earlier one (user) on a name collision, keeping
/// the first-seen ordering position (same semantics as JS `Map.set`).
pub fn load_skills_with_diagnostics(
    source: &dyn SkillSource,
    opts: &LoadSkillsOptions,
) -> LoadedSkills {
    let dirs = skill_search_dirs(opts);
    let files = source.read_skill_files(&dirs);

    let mut order: Vec<String> = Vec::new();
    let mut by_name: HashMap<String, Skill> = HashMap::new();
    let mut diagnostics: Vec<SkillDiagnostic> = Vec::new();

    for file in files {
        let default_origin = default_origin_for(&file.path, opts);
        match skill_from_file_with_origin(&file.path, &file.content, default_origin) {
            Ok(skill) => {
                if !by_name.contains_key(&skill.name) {
                    order.push(skill.name.clone());
                }
                by_name.insert(skill.name.clone(), skill);
            }
            Err(diag) => diagnostics.push(diag),
        }
    }

    let skills = order
        .into_iter()
        .filter_map(|name| by_name.remove(&name))
        .collect();
    LoadedSkills {
        skills,
        diagnostics,
    }
}

/// Load every skill visible from `opts`, merged by name (workspace beats
/// user-global). Malformed files are skipped silently; use
/// [`load_skills_with_diagnostics`] to also see why.
pub fn load_skills(source: &dyn SkillSource, opts: &LoadSkillsOptions) -> Vec<Skill> {
    load_skills_with_diagnostics(source, opts).skills
}

// Selection — the "super important" part (pure lexical + domain scoring)

/// How [`select_skills`] scores and trims. All scalar, so cheap to copy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectionConfig {
    /// Top-k cap on the returned skills.
    pub max_skills: usize,
    /// Minimum score a skill must reach to be selected at all.
    pub min_score: f64,
    /// Score added per matched active domain. Set high enough that a domain
    /// match beats a merely-weak lexical overlap (the product requirement:
    /// domain relevance should surface a skill the wording alone would miss).
    pub domain_boost: f64,
    /// Small bonus for an [`SkillOrigin::AutoCreated`] skill — a freshly-mined
    /// preference is, by construction, currently relevant; this is the
    /// recency/`AutoCreated` tie-break, never large enough to select a skill
    /// that is otherwise below `min_score`.
    pub auto_created_bonus: f64,
}

impl Default for SelectionConfig {
    fn default() -> Self {
        Self {
            max_skills: 5,
            min_score: 0.08,
            domain_boost: 0.5,
            auto_created_bonus: 0.02,
        }
    }
}

/// A skill chosen by [`select_skills`], with the score and the **why** —
/// which prompt terms and which active domains it matched — so the selection
/// is inspectable rather than a black box.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectedSkill {
    pub skill: Skill,
    pub score: f64,
    /// Prompt terms that overlapped the skill's name+description (sorted).
    pub matched_terms: Vec<String>,
    /// Active domains the skill is tagged with (as authored on the skill).
    pub matched_domains: Vec<String>,
}

/// Select the skills most relevant to `prompt`, given the currently active
/// `active_domains`. Pure scoring: lexical Jaccard overlap between the prompt
/// and each skill's name+description, plus a per-matched-domain boost, plus
/// the small `AutoCreated` tie-break. Only skills scoring at least
/// `config.min_score` are returned, top-k by `config.max_skills`, highest
/// first (ties broken by name for determinism).
pub fn select_skills(
    skills: &[Skill],
    prompt: &str,
    active_domains: &[String],
    config: &SelectionConfig,
) -> Vec<SelectedSkill> {
    let prompt_terms: HashSet<String> = terms(prompt).into_iter().collect();

    let mut selected: Vec<SelectedSkill> = Vec::new();
    for skill in skills {
        let haystack = format!("{} {}", skill.name, skill.description);
        let skill_terms: HashSet<String> = terms(&haystack).into_iter().collect();

        let mut matched_terms: Vec<String> =
            prompt_terms.intersection(&skill_terms).cloned().collect();
        matched_terms.sort();

        let matched_domains: Vec<String> = skill
            .domains
            .iter()
            .filter(|d| active_domains.iter().any(|a| a.eq_ignore_ascii_case(d)))
            .cloned()
            .collect();

        let lexical = jaccard(&prompt_terms, &skill_terms);
        let domain_score = matched_domains.len() as f64 * config.domain_boost;
        let recency = if skill.origin == SkillOrigin::AutoCreated {
            config.auto_created_bonus
        } else {
            0.0
        };
        let score = lexical + domain_score + recency;

        // A pure-lexical match hanging off a single shared term is noise
        // ("about", "something") no matter what jaccard says — require at
        // least two shared terms unless a domain tag corroborates.
        let corroborated = !matched_domains.is_empty() || matched_terms.len() >= 2;

        if corroborated && score >= config.min_score {
            selected.push(SelectedSkill {
                skill: skill.clone(),
                score,
                matched_terms,
                matched_domains,
            });
        }
    }

    selected.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.skill.name.cmp(&b.skill.name))
    });
    selected.truncate(config.max_skills);
    selected
}

// Rendering — the volatile context block injected after the stable prefix

/// Bytes-per-token divisor — [`stella_protocol::tokens`]'s constant, so this
/// module's budget arithmetic and the receipts plane's costs are the same
/// scale. It used to share only the divisor while counting chars against the
/// estimator's bytes; #925 removed that last split, since "the same divisor
/// over a different unit" is still two answers to one question.
const BYTES_PER_TOKEN: f64 = stella_protocol::BYTES_PER_TOKEN;
/// Per-skill body budget before the body is truncated with a marker.
const SKILL_BODY_TOKEN_BUDGET: u64 = 400;
/// Total budget for the whole injected section — once exceeded, remaining
/// skills are dropped with an explicit note.
const SKILLS_SECTION_TOKEN_BUDGET: u64 = 1500;
/// Appended to a body cut to fit `SKILL_BODY_TOKEN_BUDGET`.
const SKILL_BODY_TRUNCATION_MARKER: &str =
    "\n[skill body truncated to fit the context budget — open the skill file for the full text]";
/// Appended when `SKILLS_SECTION_TOKEN_BUDGET` is hit and further skills are
/// dropped.
const SKILLS_SECTION_OMISSION_MARKER: &str =
    "\n[additional lower-ranked skills omitted to fit the context budget]\n";

/// Truncate `text` to `budget` tokens, appending
/// [`SKILL_BODY_TRUNCATION_MARKER`] when it was cut. Returned unchanged when
/// already within budget.
///
/// The budget is a byte budget (the shared rule's unit), and the cut is then
/// snapped **back** to the nearest char boundary at or below it — never
/// forward, so the result is always within budget rather than one character
/// over. Slicing a `String` at a non-boundary would panic, and a body of CJK
/// or emoji is exactly where a naive byte cut lands mid-character.
fn truncate_to_tokens(text: &str, budget: u64) -> String {
    if stella_protocol::estimate_tokens(text) <= budget {
        return text.to_string();
    }
    let max_bytes = ((budget as f64) * BYTES_PER_TOKEN) as usize;
    let cut = floor_char_boundary(text, max_bytes);
    format!("{}{SKILL_BODY_TRUNCATION_MARKER}", &text[..cut])
}

/// The largest index `<= max` that is a char boundary of `text`.
///
/// `str::floor_char_boundary` is still unstable, and the cast-to-usize budget
/// above can land anywhere inside a multi-byte character. Walking back at most
/// three bytes is enough: UTF-8 characters are at most four bytes, so a
/// continuation run is never longer than that.
fn floor_char_boundary(text: &str, max: usize) -> usize {
    if max >= text.len() {
        return text.len();
    }
    let mut cut = max;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

/// Render the selected skills into the markdown block the CLI injects as a
/// **volatile context message after the byte-stable system prefix** — never
/// baked into the cached system block, so prompt-cache hits on the prefix are
/// preserved ( L-E8). Each skill contributes its name,
/// description, and body; bodies over `SKILL_BODY_TOKEN_BUDGET` are
/// truncated with a marker, and once the running total exceeds
/// `SKILLS_SECTION_TOKEN_BUDGET` the remaining (lower-ranked) skills are
/// dropped with a note. At least the top skill always renders. Empty input ⇒
/// empty string (inject nothing).
pub fn render_skills_section(selected: &[SelectedSkill]) -> String {
    if selected.is_empty() {
        return String::new();
    }
    let mut out =
        String::from("\n## Applicable skills (selected for this task — apply the relevant ones)\n");
    let mut used_tokens: u64 = 0;
    let mut rendered_any = false;

    for sel in selected {
        let body = truncate_to_tokens(&sel.skill.body, SKILL_BODY_TOKEN_BUDGET);
        let block = format!(
            "\n### {}\n{}\n\n{}\n",
            sel.skill.name, sel.skill.description, body
        );
        let block_tokens = stella_protocol::estimate_tokens(&block);
        if rendered_any && used_tokens + block_tokens > SKILLS_SECTION_TOKEN_BUDGET {
            out.push_str(SKILLS_SECTION_OMISSION_MARKER);
            break;
        }
        out.push_str(&block);
        used_tokens += block_tokens;
        rendered_any = true;
    }
    out
}

// Auto-creation — mining observations into new skills (the user requirement)

/// One mineable observation — a recurring style preference or reflection
/// lesson — already extracted from whatever store it came from. Mirrors
/// [`crate::rules::RawObservation`] but carries `domains` (skills are
/// domain-tagged) and no guard/file machinery (skills never hard-block).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillObservation {
    /// The lesson/preference text as observed.
    pub text: String,
    /// When it was observed (for evidence ordering / recency).
    pub occurred_at: u64,
    /// Domain tags carried by this observation, unioned into the candidate.
    pub domains: Vec<String>,
    /// Already elevated past a raw observation (e.g. an explicit user
    /// preference), decided by the caller — a single salient observation is
    /// enough to mine a candidate.
    pub salient: bool,
    /// An opaque reference (e.g. `trace:<turn>#lesson<i>` or `memory:<id>`)
    /// recorded in the candidate's evidence for auditing.
    pub reference: String,
}

/// Mining thresholds, mirroring [`crate::rules::MineConfig`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkillMineConfig {
    /// Minimum recurrences before a non-salient cluster becomes a candidate.
    pub min_occurrences: usize,
    /// Jaccard term-overlap threshold to cluster two observations as "the
    /// same".
    pub min_similarity: f64,
    /// Max candidates returned, ranked by score.
    pub limit: usize,
}

impl Default for SkillMineConfig {
    fn default() -> Self {
        Self {
            min_occurrences: 3,
            min_similarity: 0.5,
            limit: 10,
        }
    }
}

/// One recurrence backing a candidate, for auditing the mining.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillEvidence {
    pub reference: String,
    pub occurred_at: u64,
    /// The observation text at this occurrence, truncated to 160 chars.
    pub snippet: String,
}

/// A ranked auto-creation candidate — a skill the observations suggest
/// writing. [`render_skill_markdown`] turns it into a `SKILL.md` file;
/// [`decide_auto_creation`] decides whether to write it.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillCandidate {
    /// Slug — also the frontmatter `name:` and the filename stem.
    pub name: String,
    /// One-line summary written as `description:` frontmatter.
    pub description: String,
    /// Union of the cluster's domains (dedup'd case-insensitively).
    pub domains: Vec<String>,
    /// The representative lesson text — becomes the skill body.
    pub body: String,
    pub occurrences: usize,
    /// `true` when at least one backing observation was salient.
    pub salient: bool,
    pub evidence: Vec<SkillEvidence>,
    /// Ranking score, highest first.
    pub score: f64,
}

/// Mine observations into ranked auto-creation candidates. A cluster of
/// similar-enough observations (Jaccard ≥ `config.min_similarity`) qualifies
/// when either it recurred at least `config.min_occurrences` times, or any
/// occurrence is salient (a single strong signal — e.g. an explicit user
/// preference — is enough). Candidates whose text already matches an existing
/// skill are dropped. Deterministic across reruns on identical input.
pub fn mine_skill_candidates(
    observations: Vec<SkillObservation>,
    existing: &[Skill],
    config: &SkillMineConfig,
) -> Vec<SkillCandidate> {
    let clusters =
        crate::mining::cluster_observations(observations, config.min_similarity, |o| &o.text);
    let mut candidates: Vec<SkillCandidate> = Vec::new();

    for cluster in clusters {
        let salient = cluster.iter().any(|o| o.salient);
        if cluster.len() < config.min_occurrences && !salient {
            continue;
        }
        let Some(text) = crate::mining::representative_text(&cluster, |o| &o.text) else {
            continue;
        };
        if crate::mining::already_captured(
            &text,
            existing
                .iter()
                .map(|s| format!("{} {} {}", s.name, s.description, s.body)),
            config.min_similarity,
        ) {
            continue;
        }

        let domains = union_domains(&cluster);
        let mut sorted = cluster;
        sorted.sort_by_key(|o| std::cmp::Reverse(o.occurred_at));
        let occurrences = sorted.len();
        let evidence: Vec<SkillEvidence> = sorted
            .iter()
            .map(|o| SkillEvidence {
                reference: o.reference.clone(),
                occurred_at: o.occurred_at,
                snippet: o.text.chars().take(160).collect(),
            })
            .collect();

        let plural = if occurrences == 1 { "" } else { "s" };
        let salience_note = if salient {
            " (includes a salient signal)"
        } else {
            ""
        };
        let name = format!(
            "{}-{}",
            crate::mining::slugify(&text, "skill"),
            crate::mining::hash8(&text)
        );

        candidates.push(SkillCandidate {
            name,
            description: format!("Learned from {occurrences} observation{plural}{salience_note}."),
            domains,
            occurrences,
            salient,
            evidence,
            score: occurrences as f64 * 10.0 + if salient { 50.0 } else { 0.0 },
            body: text,
        });
    }

    candidates.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.name.cmp(&b.name))
    });
    candidates.truncate(config.limit);
    candidates
}

/// Render the exact `SKILL.md` content for `candidate` — the same frontmatter
/// shape [`skill_from_file`] parses back, including the `origin: auto` marker
/// so a reload tags it [`SkillOrigin::AutoCreated`], plus an `## Evidence`
/// section listing the backing occurrences. Writing this to disk is the I/O
/// half `stella-cli` owns; this half is pure and round-trips through the
/// parser above.
pub fn render_skill_markdown(candidate: &SkillCandidate) -> String {
    let mut out = String::from("---\n");
    out.push_str(&format!("name: {}\n", candidate.name));
    out.push_str(&format!("description: {}\n", candidate.description));
    if !candidate.domains.is_empty() {
        out.push_str(&format!("domains: [{}]\n", candidate.domains.join(", ")));
    }
    out.push_str("origin: auto\n");
    out.push_str("---\n\n");
    out.push_str(&candidate.body);
    out.push_str("\n\n## Evidence\n\n");
    for e in &candidate.evidence {
        out.push_str(&format!(
            "- `{}` (observed at {}): {}\n",
            e.reference, e.occurred_at, e.snippet
        ));
    }
    out
}

/// Per-session auto-creation policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoCreateConfig {
    /// Max skills auto-created in one session. Kept small (default 2) on
    /// purpose: auto-creation must feel *magical, not spammy* — a session
    /// that silently spawns a dozen skill files would erode trust in the
    /// mechanism. The rest wait for the next session's mining pass.
    pub max_per_session: usize,
}

impl Default for AutoCreateConfig {
    fn default() -> Self {
        Self { max_per_session: 2 }
    }
}

/// Why an auto-creation was skipped — typed so the caller can log precisely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoCreateSkip {
    /// The per-session cap ([`AutoCreateConfig::max_per_session`]) was already
    /// reached.
    SessionCapReached { cap: usize },
    /// The target path is already occupied — never clobber a hand-edited
    /// (or previously auto-created) skill.
    FileExists { path: String },
}

/// The decision for one candidate: write it, or skip with a typed reason.
/// Never performs I/O — the caller writes [`render_skill_markdown`]'s output
/// to `path` when this is `Create`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoCreateDecision {
    Create { path: String },
    Skip { reason: AutoCreateSkip },
}

/// Decide whether to auto-create `candidate` as `<target_dir>/<name>.md`.
/// `occupied_paths` MUST be the paths present on disk, not the paths that
/// loaded successfully — a file absent from the loaded set is still a file
/// (#737). Cap first, then no-clobber. Pure: writing is the caller's job.
pub fn decide_auto_creation(
    candidate: &SkillCandidate,
    target_dir: &str,
    occupied_paths: &[String],
    created_this_session: usize,
    config: &AutoCreateConfig,
) -> AutoCreateDecision {
    if created_this_session >= config.max_per_session {
        return AutoCreateDecision::Skip {
            reason: AutoCreateSkip::SessionCapReached {
                cap: config.max_per_session,
            },
        };
    }
    let path = format!("{}/{}.md", target_dir.trim_end_matches('/'), candidate.name);
    if occupied_paths.iter().any(|p| p == &path) {
        return AutoCreateDecision::Skip {
            reason: AutoCreateSkip::FileExists { path },
        };
    }
    AutoCreateDecision::Create { path }
}

// Install-proposal vocabulary (registry search + confirmed install)

/// A proposal to install a skill from a registry, produced by the
/// search/install glue after querying the registry and shown to the user for
/// confirmation. Kept minimal — the actual `npx skills`-style subprocess and
/// the `ask_user` confirmation live in `stella-cli`; this is just the shared
/// shape the glue and a future TUI both speak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInstallProposal {
    /// What the user searched the registry for.
    pub query: String,
    /// A human-readable summary of what the registry returned (e.g. the top
    /// match's name + description), for the confirmation prompt.
    pub registry_result_summary: String,
    /// Where the glue would install it (e.g. the workspace skills dir).
    pub target_dir_hint: String,
}

/// The user's answer to a [`SkillInstallProposal`]. Minimal by design — the
/// glue maps this onto the actual install/no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallDecision {
    /// Proceed with the install into the proposal's target directory.
    Confirmed,
    /// Do nothing.
    Declined,
}

// Lexical helpers — shared with the rules miner via `crate::mining`

use crate::mining::{jaccard, terms};

/// Union of every domain across a cluster, dedup'd case-insensitively,
/// first-seen casing preserved, order stable.
fn union_domains(cluster: &[SkillObservation]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for o in cluster {
        for d in &o.domains {
            if !out.iter().any(|existing| existing.eq_ignore_ascii_case(d)) {
                out.push(d.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod migration_contract;
#[cfg(test)]
mod tests;
