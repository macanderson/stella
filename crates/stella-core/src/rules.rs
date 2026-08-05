//! Workspace rules engine (ported from
//! `apps/cli/src/rules/{types,loader,enforce,promote}.ts`).
//!
//! A rule is a binding instruction for the agent. The engine models two tiers:
//! **Tier 1** soft adherence, a rule rendered into the system prompt
//! ([`render_rules_section`]); and **Tier 2** hard enforcement, a rule carrying
//! a [`RuleGuard`] checked at the tool boundary ([`evaluate_guards`]) to block a
//! violating tool call before it runs and return the rule text so the model can
//! self-correct. Rules are authored as markdown under `.stella/rules/*.md`
//! (ADR-008 filesystem-first).
//!
//! Status: wired into the shipping CLI (issue #103). The production
//! [`RuleSource`] lives in `stella-cli`'s `rules` module: the on-disk rule
//! files (fs-backed) merged — through this module's precedence merge — with
//! extension-authored rules read from the workspace store
//! (`stella_store::Store::list_rules`). Each session driver loads rules
//! once at assembly, renders the Tier-1 section into its system prompt, and
//! arms Tier-2 guards at the tool boundary by registering an
//! [`evaluate_guards`] policy handler on the tool registry's
//! `tool.call.requested` blocking chain ([`crate::bus`],
//! `stella-tools::registry`) — a violation denies the call and returns the
//! rule text to the model.
//!
//! # No I/O in this module
//!
//! Discovering rule files means reading a directory and its file contents —
//! real I/O, which `stella-core` never performs directly. [`RuleSource`] is
//! the injectable discovery port, mirroring how [`crate::ports::ToolExecutor`]
//! is the injectable *execution* port: a concrete implementation backed by
//! real `std::fs` calls belongs to `stella-cli` (or `stella-tools`), never
//! here. Everything downstream of a `RuleSource` — frontmatter parsing,
//! precedence merging, Tier-1 rendering, Tier-2 enforcement, and candidate
//! mining — is plain synchronous logic over owned data, unit-tested below
//! against a fake `RuleSource`, no real files required.
//!
//! # Deliberately out of scope
//!
//! `apps/cli/src/rules/promote.ts`'s interactive candidate-promotion
//! *workflow* — mining lessons out of local traces/fleet-memory, then
//! prompting a human to approve writing a new rule file — needs a live user
//! prompt / TUI surface that doesn't exist in `stella-core`, correctly so:
//! this crate has no I/O and no UI. Concretely, out of scope here:
//!
//!   - `observationsFromTrace`/`observationsFromMemory` (TS): the adapters
//!     that pull [`RawObservation`]s out of `TurnTrace`/`MemoryRecord`.
//!     Those types don't have a Rust home yet (they land with the trace
//!     store and `stella-fleet` in later phases); once they do, porting
//!     those adapters is a small, mechanical follow-up — [`mine_candidates`]
//!     below already accepts the neutral `RawObservation` shape they would
//!     produce.
//!   - The actual interactive approve/write flow (`stella rules promote`):
//!     prompting the human, calling a filesystem port to check
//!     `already-exists`, and writing [`render_rule_markdown`]'s output to
//!     disk. That belongs to `stella-cli`.
//!
//! What IS ported: the full mining algorithm — lexical clustering, salience
//! override, dedup against existing rules, guard inference from consistent
//! file evidence, and ranking (all pure decision logic, see
//! [`mine_candidates`]) — plus the pure half of `promoteCandidate`:
//! rendering a candidate's exact rule-file content
//! ([`render_rule_markdown`]) and deciding what a promotion attempt *would*
//! do given the caller's own `approve`/`file_exists` facts
//! ([`decide_promotion`]).

use std::collections::HashMap;

use crate::glob::match_glob;

mod metadata;

use metadata::metadata_from_frontmatter;
pub use metadata::{
    RuleEnforcement, RuleMetadata, RuleMetadataError, RuleOrigin, RuleRecordKind,
    render_rule_metadata,
};

// Types (ports `rules/types.ts`)

/// A machine-enforceable guard that blocks a tool call violating the rule
/// (TS: `RuleGuard`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleGuard {
    /// Canonical tool the guard applies to (`Bash`, `Write`, `Edit`,
    /// `Read`), or `*`/`None` for any tool.
    pub tool: Option<String>,
    /// Block a file tool (`Write`/`Edit`/`Read`) whose path matches this
    /// glob.
    pub deny_path_glob: Option<String>,
    /// Block a `Bash` command matching this glob.
    pub deny_command_glob: Option<String>,
}

/// Which enforcement tier a rule sits at — computed from whether it carries
/// a [`RuleGuard`], not a stored field, so it can never drift out of sync
/// with `guard` (see `types.ts`'s doc comment: "always injected... Tier 1.
/// When it carries a guard, also hard-enforced... Tier 2").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleTier {
    /// Injected into the system prompt only; the model is asked, not
    /// forced.
    Prompt,
    /// Prompt-injected AND hard-blocked at the tool boundary via `guard`.
    Guarded,
}

/// One workspace rule (TS: `Rule`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Rule name — the filename stem or frontmatter `name`.
    pub id: String,
    /// Short description shown in `rules list`.
    pub description: String,
    /// The rule statement injected into the system prompt.
    pub text: String,
    /// Optional hard guard (Tier 2). Absent ⇒ prompt-only (Tier 1).
    pub guard: Option<RuleGuard>,
    /// Where the rule came from (a file path, or any opaque source label —
    /// TS: `source: string`).
    pub source: String,
    /// Optional, Git-reviewable context-as-code metadata. Metadata-free rules
    /// remain first-class rules during the staged migration.
    pub metadata: Option<RuleMetadata>,
    /// Metadata issues retained alongside a still-loadable rule. Keeping
    /// these separate lets a future read-only linter explain invalid metadata
    /// without changing legacy prompt or guard behavior.
    pub metadata_errors: Vec<RuleMetadataError>,
}

impl Rule {
    /// This rule's enforcement tier — see [`RuleTier`].
    pub fn tier(&self) -> RuleTier {
        if self.guard.is_some() {
            RuleTier::Guarded
        } else {
            RuleTier::Prompt
        }
    }
}

// Discovery port + frontmatter parsing (ports `rules/loader.ts`)

/// One markdown file's raw content, already read from disk by a
/// [`RuleSource`] implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleFile {
    /// The file's path (or any opaque source label the implementation
    /// wants to carry through into [`Rule::source`]).
    pub path: String,
    pub contents: String,
}

/// The filesystem discovery port for rule files.
/// A real implementation (owned by `stella-cli`/`stella-tools`)
/// walks each directory in `dirs`, in the given order, and returns every
/// `.md` file's contents — files within one directory sorted by name,
/// directories skipped silently if they don't exist (mirrors
/// `loadMarkdownRegistry`'s `existsSync` skip in `markdown-registry.ts`).
/// Order matters: [`load_rules`] merges by rule id with **later entries
/// overriding earlier ones**, so the directories must already be in
/// precedence order when passed to [`RuleSource::read_rule_files`] (see
/// [`rule_search_dirs`]).
pub trait RuleSource: Send + Sync {
    fn read_rule_files(&self, dirs: &[String]) -> Vec<RuleFile>;
}

/// Frontmatter split from a markdown file's body (TS: `Frontmatter`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    pub data: HashMap<String, String>,
    /// Keys which appeared more than once, in first-duplicate order. The
    /// last scalar still wins in `data` for legacy compatibility; consumers
    /// that need schema validation can reject the ambiguity explicitly.
    pub duplicate_keys: Vec<String>,
    /// Keys that appeared **indented under another key** — a YAML nested
    /// mapping this single-line parser cannot represent.
    ///
    /// Recorded rather than acted on here, for the same reason as
    /// `duplicate_keys`: this parser is shared with skills and extensions, and
    /// what a nested key *means* differs per consumer. [`rule_from_file`]
    /// refuses to load a rule that has any.
    ///
    /// Why this matters (ADR 0011, Consequences): the parser strips
    /// indentation, so `docs/design/adaptive-context/context-pr.md` §6.1's own example
    ///
    /// ```text
    /// scope:
    ///   repository_id: repo_stella
    /// ```
    ///
    /// used to promote `repository_id` to the top level as a sibling of
    /// `record_id`, leave `scope` empty, and report nothing. The record loaded,
    /// wearing a scope it did not have. That is the same failure shape as a
    /// guard script printing OK while skipping most of its inputs — the output
    /// says success and the work did not happen.
    pub nested_keys: Vec<String>,
    pub body: String,
}

/// Strip one pair of matching surrounding quotes (`"…"` or `'…'`).
pub(crate) fn strip_matched_quotes(value: &str) -> &str {
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

/// Split `---\n…\n---\nbody` into single-line key/value frontmatter plus
/// body text. No frontmatter fence ⇒ the whole (trimmed) input is the body
/// with empty data. Ports `parseFrontmatter` in `markdown-registry.ts`
/// (leading-BOM strip, quote-stripping on values), and additionally
/// flattens a YAML block sequence — a key with an empty scalar followed by
/// `- item` lines — onto that key as a comma-separated value, so list-typed
/// fields reach consumers in one shape no matter how the author wrote them.
///
/// A key **indented under another key** is recorded in
/// [`Frontmatter::nested_keys`] rather than silently promoted to the top level.
/// See that field's docs for why the silent promotion was a defect and not a
/// convenience.
pub fn parse_frontmatter(raw: &str) -> Frontmatter {
    let text = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    if !text.starts_with("---") {
        return Frontmatter {
            body: text.trim().to_string(),
            ..Frontmatter::default()
        };
    }
    let Some(rel_end) = text.get(3..).and_then(|rest| rest.find("\n---")) else {
        return Frontmatter {
            body: text.trim().to_string(),
            ..Frontmatter::default()
        };
    };
    let end = 3 + rel_end;
    let header = text[3..end].trim();
    let after_fence = &text[end + 4..];
    let body = after_fence
        .strip_prefix("\r\n")
        .or_else(|| after_fence.strip_prefix('\n'))
        .unwrap_or(after_fence)
        .trim()
        .to_string();

    let mut data = HashMap::new();
    let mut duplicate_keys = Vec::new();
    let mut nested_keys = Vec::new();
    // The key whose scalar value was empty on its own line — the head of a
    // possible YAML block sequence (`tools:` followed by `- Read` lines).
    let mut pending_list_key: Option<String> = None;
    // The indentation the block's own keys sit at, taken from the first key seen.
    // Anything deeper is a nested mapping. Read from the file rather than assumed
    // to be zero so a frontmatter block someone indented wholesale still parses.
    let mut base_indent: Option<usize> = None;
    for line in header.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        // A `- item` line under an empty-valued key is a block-sequence
        // element: flatten it onto that key. Without a pending key the
        // line falls through to the scalar path (and is skipped when it
        // has no colon), exactly as before.
        if let Some(item) = trimmed.strip_prefix("- ")
            && let Some(key) = &pending_list_key
        {
            let item = strip_matched_quotes(item.trim());
            if !item.is_empty() {
                let entry: &mut String = data.entry(key.clone()).or_default();
                if !entry.is_empty() {
                    entry.push_str(", ");
                }
                entry.push_str(item);
            }
            continue;
        }
        let Some(colon) = trimmed.find(':') else {
            continue;
        };
        let key = trimmed[..colon].trim();
        let value = strip_matched_quotes(trimmed[colon + 1..].trim());
        if key.is_empty() {
            continue;
        }
        let base = *base_indent.get_or_insert(indent);
        if indent > base {
            // A nested mapping. Record it and DO NOT promote it: writing it to
            // `data` is what made a mangled record look like a valid one.
            if !nested_keys.iter().any(|seen| seen == key) {
                nested_keys.push(key.to_string());
            }
            continue;
        }
        if data.contains_key(key) && !duplicate_keys.iter().any(|seen| seen == key) {
            duplicate_keys.push(key.to_string());
        }
        data.insert(key.to_string(), value.to_string());
        pending_list_key = value.is_empty().then(|| key.to_string());
    }
    Frontmatter {
        data,
        duplicate_keys,
        nested_keys,
        body,
    }
}

fn file_stem(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    base.strip_suffix(".md").unwrap_or(base).to_string()
}

fn guard_from(data: &HashMap<String, String>) -> Option<RuleGuard> {
    // A key present with a blank value (`guard-tool:` and nothing after the
    // colon) is not a guard condition. Keeping it as `Some("")` made the rule
    // advertise Tier 2 — `tier()` said `Guarded`, `render_rules_section`
    // stamped it "[enforced]" — while `guard_matches` could never fire (`""`
    // equals no tool and matches no path), and `guards_to_deny` handed the
    // external permission gate a nonsense empty deny entry. Blank ⇒ absent, so
    // the tier a rule advertises is the tier it actually has.
    let field = |snake: &str, kebab: &str| -> Option<String> {
        data.get(snake)
            .or_else(|| data.get(kebab))
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let tool = field("guard_tool", "guard-tool");
    let deny_path_glob = field("guard_deny_path", "guard-deny-path");
    let deny_command_glob = field("guard_deny_command", "guard-deny-command");
    if tool.is_none() && deny_path_glob.is_none() && deny_command_glob.is_none() {
        return None;
    }
    Some(RuleGuard {
        tool,
        deny_path_glob,
        deny_command_glob,
    })
}

/// Why a rule file did not load at all.
///
/// Distinct from [`RuleMetadataError`], which describes a rule that loaded with
/// unusable metadata: these refuse the file. The distinction is the point —
/// legacy metadata-free rules must keep working, so only a defect that makes the
/// *rule itself* untrustworthy is fatal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleFileError {
    /// No frontmatter `name` and no usable filename stem.
    MissingId,
    /// Frontmatter but no rule statement.
    EmptyStatement,
    /// A nested frontmatter mapping. This parser is single-line by contract
    /// (ADR 0011: new fields land in the TOML schema, and this parser is not
    /// extended), so a nested key cannot be represented — and promoting it
    /// silently produced a record wearing a scope it did not have.
    NestedFrontmatterKeys(Vec<String>),
}

impl std::fmt::Display for RuleFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingId => write!(f, "no rule name: add `name:` frontmatter"),
            Self::EmptyStatement => write!(
                f,
                "no rule statement: the body after the frontmatter fence is empty"
            ),
            Self::NestedFrontmatterKeys(keys) => write!(
                f,
                "nested frontmatter key(s) {} — this markdown reader parses single-line \
                 `key: value` only, so a nested mapping cannot be loaded without silently \
                 flattening it. Write the field as a single line, or author the record as \
                 TOML under .stella/rules/*.toml (ADR 0011)",
                keys.join(", ")
            ),
        }
    }
}

impl std::error::Error for RuleFileError {}

/// Parse one rule file's raw content into a [`Rule`]. `None` when the file
/// cannot be loaded at all — "a rule needs a name and a statement" (TS:
/// `ruleFromFile`), plus the nesting refusal. Use [`rule_from_file_checked`]
/// when the caller can report *why*.
pub fn rule_from_file(path: &str, raw: &str) -> Option<Rule> {
    rule_from_file_checked(path, raw).ok()
}

/// [`rule_from_file`] with the reason it failed.
pub fn rule_from_file_checked(path: &str, raw: &str) -> Result<Rule, RuleFileError> {
    let fm = parse_frontmatter(raw);
    // Checked before anything else is read off `fm.data`: a file with a nested
    // mapping has already lost information, and every field decision made from
    // it after that point is made on a record the author did not write.
    if !fm.nested_keys.is_empty() {
        return Err(RuleFileError::NestedFrontmatterKeys(fm.nested_keys));
    }
    let id = fm
        .data
        .get("name")
        .cloned()
        .unwrap_or_else(|| file_stem(path));
    if id.is_empty() {
        return Err(RuleFileError::MissingId);
    }
    if fm.body.trim().is_empty() {
        return Err(RuleFileError::EmptyStatement);
    }
    let (metadata, metadata_errors) = match metadata_from_frontmatter(&fm) {
        Ok(metadata) => (metadata, Vec::new()),
        Err(errors) => (None, errors),
    };
    Ok(Rule {
        id,
        description: fm.data.get("description").cloned().unwrap_or_default(),
        text: fm.body.trim().to_string(),
        guard: guard_from(&fm.data),
        source: path.to_string(),
        metadata,
        metadata_errors,
    })
}

/// Where to look for rules, lowest → highest precedence (TS:
/// `LoadRulesOptions`). Unlike the TS loader, `stella-core` never defaults
/// these from `process.cwd()`/`homedir()` itself — no I/O, not even the
/// trivial kind — so the caller always supplies both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadRulesOptions {
    /// Project root.
    pub cwd: String,
    /// The user rules directory (normally `~/.stella/rules`).
    pub user_rules_dir: String,
}

/// The three rule directories in precedence order — user, Claude Code
/// interop, stella project rules — matching `loader.ts`'s comment exactly:
/// a later directory overrides an earlier one by rule id.
pub fn rule_search_dirs(opts: &LoadRulesOptions) -> Vec<String> {
    let cwd = opts.cwd.trim_end_matches('/');
    vec![
        opts.user_rules_dir.clone(),
        format!("{cwd}/.claude/rules"),
        format!("{cwd}/.stella/rules"),
    ]
}

/// Merge parsed rule files by id, preserving each id's *first* insertion
/// position but keeping its *latest* value — the same semantics as JS
/// `Map.set` on an existing key (TS: `[...registry.values()]` after
/// `loadMarkdownRegistry`'s merge loop).
fn merge_rule_files(files: Vec<RuleFile>) -> Vec<Rule> {
    let mut order: Vec<String> = Vec::new();
    let mut by_id: HashMap<String, Rule> = HashMap::new();
    for file in files {
        if let Some(rule) = rule_from_file(&file.path, &file.contents) {
            if !by_id.contains_key(&rule.id) {
                order.push(rule.id.clone());
            }
            by_id.insert(rule.id.clone(), rule);
        }
    }
    order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect()
}

/// Load every workspace rule visible from `opts.cwd`, merged by id across
/// sources via `source` (TS: `loadRules`).
pub fn load_rules(source: &dyn RuleSource, opts: &LoadRulesOptions) -> Vec<Rule> {
    let dirs = rule_search_dirs(opts);
    let files = source.read_rule_files(&dirs);
    merge_rule_files(files)
}

// Enforcement (ports `rules/enforce.ts`)

/// The system-prompt section listing active rules (Tier 1: soft adherence;
/// TS: `renderRulesSection`). Empty string when there are no rules.
pub fn render_rules_section(rules: &[Rule]) -> String {
    if rules.is_empty() {
        return String::new();
    }
    let mut lines = vec![
        String::new(),
        "## Workspace rules (binding — follow exactly; guarded rules are hard-blocked)".to_string(),
    ];
    for r in rules {
        let suffix = if r.guard.is_some() {
            "  [enforced]"
        } else {
            ""
        };
        lines.push(format!("- {}{suffix}", r.text));
    }
    lines.join("\n")
}

/// The permission-gate deny entry a guard produces, e.g.
/// `Edit(migrations/**)` or a bare `Bash` (TS: `guardDenyEntry`).
///
/// Returns a single entry, preferring the path glob. A guard carrying BOTH a
/// path and a command glob is lossy here (the command half is dropped) — use
/// [`guards_to_deny`], which emits one entry per glob, when completeness
/// against an external gate matters.
pub fn guard_deny_entry(rule: &Rule) -> Option<String> {
    let guard = rule.guard.as_ref()?;
    let tool = guard.tool.as_deref().unwrap_or("*");
    let pattern = guard
        .deny_path_glob
        .as_deref()
        .or(guard.deny_command_glob.as_deref());
    Some(match pattern {
        Some(p) => format!("{tool}({p})"),
        None => tool.to_string(),
    })
}

/// Deny entries + their human reasons, for interop with an external
/// string-keyed permission gate (TS: `RuleDenies`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleDenies {
    pub deny: Vec<String>,
    pub reasons: HashMap<String, String>,
}

fn violation_reason(rule: &Rule) -> String {
    format!("rule \"{}\" — {}", rule.id, rule.text)
}

/// Convert guarded rules into gate deny entries + their human reasons
/// (Tier 2; TS: `guardsToDeny`).
pub fn guards_to_deny(rules: &[Rule]) -> RuleDenies {
    let mut deny = Vec::new();
    let mut reasons = HashMap::new();
    for rule in rules {
        let Some(guard) = rule.guard.as_ref() else {
            continue;
        };
        let tool = guard.tool.as_deref().unwrap_or("*");
        let reason = violation_reason(rule);
        // Emit one entry PER configured glob. A guard may carry both a path and
        // a command condition (`evaluate_guards` enforces both); the single
        // `guard_deny_entry` only surfaces the path, so without this the
        // external string-gate would silently miss the command half — the two
        // enforcement surfaces would then disagree.
        let mut pushed = false;
        for pat in [
            guard.deny_path_glob.as_deref(),
            guard.deny_command_glob.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            let entry = format!("{tool}({pat})");
            reasons.insert(entry.clone(), reason.clone());
            deny.push(entry);
            pushed = true;
        }
        if !pushed {
            reasons.insert(tool.to_string(), reason.clone());
            deny.push(tool.to_string());
        }
    }
    RuleDenies { deny, reasons }
}

/// A tool invocation the agent is about to make, checked against guarded
/// rules. There is no TS equivalent by this name — `enforce.ts` stops at
/// producing deny-entry strings and leaves matching to a separate
/// `settings/permissions-gate.ts`; `stella-core` has no such second module
/// to hand off to, so [`evaluate_guards`] below folds `guardsToDeny` and
/// the gate's `matchGlob`-based deny check into one typed, directly
/// consultable decision for the step-driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProposedAction<'a> {
    /// Canonical tool name (e.g. `"Edit"`, `"Bash"`, `"Write"`, `"Read"`).
    pub tool: &'a str,
    /// Present for file tools (`Write`/`Edit`/`Read`).
    pub path: Option<&'a str>,
    /// Present for `Bash`.
    pub command: Option<&'a str>,
}

/// One guarded rule a [`ProposedAction`] violated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleViolation {
    pub rule_id: String,
    /// `rule "<id>" — <text>` — the human-readable reason returned to the
    /// model so it can self-correct (same format as `guardsToDeny`'s
    /// `reasons` map).
    pub reason: String,
}

/// The typed result of checking a [`ProposedAction`] against every guarded
/// rule — every violation, not just the first, so the step-driver can log
/// (or surface to the model) the full set of reasons a call was rejected.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuardCheck {
    pub violations: Vec<RuleViolation>,
}

impl GuardCheck {
    /// `true` when at least one guard blocked the action.
    pub fn is_blocked(&self) -> bool {
        !self.violations.is_empty()
    }

    /// The violation the step-driver should actually cite when blocking —
    /// the first one found, mirroring first-match-wins deny semantics
    /// (`evaluateLocalPermission` in `permissions-gate.ts`).
    pub fn primary(&self) -> Option<&RuleViolation> {
        self.violations.first()
    }
}

fn guard_matches(guard: &RuleGuard, action: &ProposedAction<'_>) -> bool {
    let tool = guard.tool.as_deref().unwrap_or("*");
    if tool != "*" && tool != action.tool {
        return false;
    }
    match (&guard.deny_path_glob, &guard.deny_command_glob) {
        // A guard carrying BOTH globs denies when EITHER matches — a rule
        // author who wrote two deny conditions meant both of them, not
        // "path wins, command silently ignored".
        (Some(p), Some(c)) => {
            action.path.is_some_and(|path| match_glob(p, path))
                || action.command.is_some_and(|cmd| match_glob(c, cmd))
        }
        (Some(p), None) => action.path.is_some_and(|path| match_glob(p, path)),
        (None, Some(c)) => action.command.is_some_and(|cmd| match_glob(c, cmd)),
        // A guard with no path/command glob blocks the whole tool (TS:
        // `guardDenyEntry` emits the bare tool name in this case).
        (None, None) => true,
    }
}

/// Check `action` against every guarded rule (Tier 2 enforcement). Rules
/// with no `guard` (Tier 1, prompt-only) never appear here — they cannot
/// block anything structurally, only [`render_rules_section`] sees them.
pub fn evaluate_guards(rules: &[Rule], action: &ProposedAction<'_>) -> GuardCheck {
    let violations = rules
        .iter()
        .filter_map(|rule| {
            let guard = rule.guard.as_ref()?;
            guard_matches(guard, action).then(|| RuleViolation {
                rule_id: rule.id.clone(),
                reason: violation_reason(rule),
            })
        })
        .collect();
    GuardCheck { violations }
}

// Rule-promotion data model + mining (ports the pure half of `promote.ts`)

/// Where one occurrence of a candidate lesson came from (TS:
/// `RuleEvidence["source"]`). `TraceReasoning` is reserved for parity with
/// the TS union — `observationsFromTrace` (not yet ported, see module
/// docs) only ever produces `TraceFinding` today, deliberately: free-form
/// verifier reasoning is too verbose to cluster reliably on term overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceSource {
    TraceFinding,
    TraceReasoning,
    Memory,
}

/// One recurrence of a candidate lesson, with enough context to audit the
/// mining (TS: `RuleEvidence`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleEvidence {
    pub source: EvidenceSource,
    /// e.g. `trace:<turnId>#verifier<round>.finding<i>` or `memory:<id>` (TS:
    /// `ref` — renamed, `ref` is a Rust keyword).
    pub reference: String,
    pub occurred_at: u64,
    /// The lesson text as it appeared at this occurrence, truncated to 160
    /// chars.
    pub snippet: String,
}

/// A ranked rule-promotion candidate (TS: `RuleCandidate`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleCandidate {
    /// Stable id derived from the representative text — also the rule
    /// filename stem.
    pub id: String,
    /// The representative lesson text — becomes the promoted rule's body.
    pub text: String,
    /// One-line summary written as `description:` frontmatter.
    pub description: String,
    pub occurrences: usize,
    /// `true` when at least one occurrence came from an already-salient
    /// observation (the caller decides salience before handing in a
    /// [`RawObservation`] — see its doc comment).
    pub salient: bool,
    pub evidence: Vec<RuleEvidence>,
    /// Best-effort guard inferred from consistent file evidence.
    /// `None` ⇒ prompt-only (Tier 1).
    pub guard: Option<RuleGuard>,
    /// Ranking score, highest first.
    pub score: u32,
}

/// Mining thresholds (TS: `MineConfig`, defaults from `DEFAULT_CONFIG`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MineConfig {
    /// Minimum recurrences before a non-salient cluster becomes a
    /// candidate.
    pub min_occurrences: usize,
    /// Jaccard term-overlap threshold to cluster two lessons as "the
    /// same".
    pub min_similarity: f64,
    /// Max candidates returned, ranked by score.
    pub limit: usize,
}

impl Default for MineConfig {
    fn default() -> Self {
        Self {
            min_occurrences: 3,
            min_similarity: 0.5,
            limit: 10,
        }
    }
}

/// One mineable observation, already extracted from whatever domain-
/// specific store it came from. TS's `observationsFromTrace`/
/// `observationsFromMemory` build this shape from `TurnTrace`/
/// `MemoryRecord`; `stella-core` doesn't have those types yet (see module
/// docs), so callers construct `RawObservation` directly. `memory_kind` is
/// intentionally a loose `String` rather than a `MemoryRecord["memoryKind"]`
/// enum for the same reason — only the literal value `"gotcha"` is
/// inspected, by `infer_guard`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawObservation {
    pub text: String,
    pub source: EvidenceSource,
    pub reference: String,
    pub occurred_at: u64,
    pub files: Vec<String>,
    /// Already-elevated past a raw observation (TS: `isSalientMemory` —
    /// `memoryClass !== "OBSERVATION" || enforcementScore >= 90`), decided
    /// by the caller before construction.
    pub salient: bool,
    pub memory_kind: Option<String>,
}

/// The longest common leading directory across every path, or `None` if
/// they share none, or any path is a bare filename that can't anchor a
/// safe glob (TS: `commonDirPrefix`).
fn common_dir_prefix(paths: &[String]) -> Option<String> {
    let dirs: Vec<&str> = paths
        .iter()
        .map(|p| match p.rfind('/') {
            Some(idx) => &p[..idx],
            None => "",
        })
        .collect();
    if dirs.is_empty() || dirs.iter().any(|d| d.is_empty()) {
        return None;
    }
    let mut prefix = dirs[0].to_string();
    for d in &dirs[1..] {
        // Segment-aware containment. A raw `starts_with` treats `app/api2` as
        // being under `app/api`, so the inferred guard `app/api/**` would MISS
        // `app/api2/…` — one of the very files the guard was derived from. The
        // prefix must be either the whole dir or a parent *segment* of it, which
        // also makes the result independent of the input order.
        while !(*d == prefix.as_str() || d.starts_with(&format!("{prefix}/"))) {
            let cut = prefix.rfind('/')?;
            prefix.truncate(cut);
        }
    }
    if prefix.is_empty() {
        None
    } else {
        Some(prefix)
    }
}

/// Best-effort guard inference: when a cluster's `"gotcha"`-kind evidence
/// shares a common directory, propose blocking that directory. Anything
/// looser is left prompt-only rather than guessing a guard that could
/// wrongly hard-block unrelated work (TS: `inferGuard`).
fn infer_guard(cluster: &[RawObservation]) -> Option<RuleGuard> {
    let gotchas: Vec<&RawObservation> = cluster
        .iter()
        .filter(|o| o.memory_kind.as_deref() == Some("gotcha") && !o.files.is_empty())
        .collect();
    if gotchas.is_empty() {
        return None;
    }
    let files: Vec<String> = gotchas.iter().flat_map(|o| o.files.clone()).collect();
    common_dir_prefix(&files).map(|prefix| RuleGuard {
        tool: None,
        deny_path_glob: Some(format!("{prefix}/**")),
        deny_command_glob: None,
    })
}

/// Mine [`RawObservation`]s into ranked rule-promotion candidates. A
/// cluster of similar-enough observations (Jaccard ≥
/// `config.min_similarity`) qualifies when either it recurred at least
/// `config.min_occurrences` times, or any occurrence is already salient.
/// Candidates that duplicate an existing rule's text are dropped (TS:
/// `mineCandidates`).
pub fn mine_candidates(
    observations: Vec<RawObservation>,
    existing_rules: &[Rule],
    config: &MineConfig,
) -> Vec<RuleCandidate> {
    let clusters =
        crate::mining::cluster_observations(observations, config.min_similarity, |o| &o.text);
    let mut candidates: Vec<RuleCandidate> = Vec::new();

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
            existing_rules.iter().map(|r| r.text.as_str()),
            config.min_similarity,
        ) {
            continue;
        }

        let guard = infer_guard(&cluster);
        let mut sorted = cluster;
        sorted.sort_by_key(|e| std::cmp::Reverse(e.occurred_at));
        let occurrences = sorted.len();
        let evidence: Vec<RuleEvidence> = sorted
            .iter()
            .map(|o| RuleEvidence {
                source: o.source,
                reference: o.reference.clone(),
                occurred_at: o.occurred_at,
                snippet: o.text.chars().take(160).collect(),
            })
            .collect();

        let plural = if occurrences == 1 { "" } else { "s" };
        let salience_note = if salient {
            " (includes an already-salient memory)"
        } else {
            ""
        };

        candidates.push(RuleCandidate {
            id: format!(
                "{}-{}",
                crate::mining::slugify(&text, "lesson"),
                crate::mining::hash8(&text)
            ),
            description: format!(
                "Promoted from {occurrences} recurring observation{plural}{salience_note}."
            ),
            occurrences,
            salient,
            evidence,
            guard,
            score: (occurrences as u32) * 10 + if salient { 50 } else { 0 },
            text,
        });
    }

    candidates.sort_by_key(|c| std::cmp::Reverse(c.score));
    candidates.truncate(config.limit);
    candidates
}

/// Render the exact `.stella/rules/<id>.md` file content for `candidate` —
/// the same frontmatter shape [`rule_from_file`] parses back (mirrors the
/// frontmatter-building lines in `promoteCandidate`, minus the file write).
/// Writing this to disk is the I/O half `stella-cli` owns; this half is
/// pure and independently testable.
pub fn render_rule_markdown(candidate: &RuleCandidate) -> String {
    let mut lines = vec![
        "---".to_string(),
        format!("description: {}", candidate.description),
    ];
    if let Some(guard) = &candidate.guard {
        if let Some(tool) = &guard.tool {
            lines.push(format!("guard-tool: {tool}"));
        }
        if let Some(deny_path) = &guard.deny_path_glob {
            lines.push(format!("guard-deny-path: {deny_path}"));
        }
        if let Some(deny_command) = &guard.deny_command_glob {
            lines.push(format!("guard-deny-command: {deny_command}"));
        }
    }
    lines.push("---".to_string());
    lines.push(String::new());
    lines.push(candidate.text.clone());
    lines.push(String::new());
    lines.join("\n")
}

/// What should happen to a candidate's rule file (TS:
/// `PromoteResult["status"]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromoteStatus {
    /// The candidate should be written — the caller now writes
    /// [`render_rule_markdown`]'s output to disk.
    Written,
    /// `approve` was `false`; nothing should be touched (the decline
    /// path).
    Declined,
    /// A file already exists at the target path; leave it untouched
    /// rather than clobbering a hand-edit (idempotent re-promotion).
    AlreadyExists,
}

/// The pure half of `promoteCandidate`'s decision: given the caller's own
/// `approve` flag and its own I/O-derived `file_exists` fact (there is no
/// filesystem port for this in `stella-core` — see the module doc
/// comment), decide what should happen. Never writes anything itself.
pub fn decide_promotion(approve: bool, file_exists: bool) -> PromoteStatus {
    if !approve {
        PromoteStatus::Declined
    } else if file_exists {
        PromoteStatus::AlreadyExists
    } else {
        PromoteStatus::Written
    }
}

#[cfg(test)]
mod tests;
