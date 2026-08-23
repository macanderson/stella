//! `custom` — developer-defined **script tools**, discovered from the
//! filesystem with zero registration ceremony.
//!
//! A developer gives the agent a new tool by dropping a TOML manifest next to
//! a script. At CLI startup every manifest under `.stella/tools/` (workspace)
//! and `~/.stella/tools/` (user-global) is discovered automatically and
//! advertised to the model alongside the native tools ([`crate::registry`]).
//! No flags, no registry edits — a bash script plus a ten-line manifest is a
//! working agent tool. Rich or stateful integrations use an MCP server
//! (`stella-mcp`) instead; this layer is the low-ceremony floor beneath it.
//!
//! # Manifest format (`<name>.toml`, one tool per file)
//!
//! ```toml
//! name = "lint_fix"                    # the name the model sees: ^[a-z][a-z0-9_]{1,63}$
//! description = "Run the lint auto-fixer on a path and report what changed"
//! command = ["./scripts/lint-fix.sh"] # argv array, spawned DIRECTLY (no shell)
//! timeout_ms = 60000                   # optional; default 30s, hard cap 10min
//!
//! [env]                                # optional extra env vars for the child
//! LINT_PROFILE = "strict"
//!
//! [input_schema]                       # JSON Schema written as TOML, converted verbatim
//! type = "object"
//! [input_schema.properties.path]
//! type = "string"
//! description = "Directory or file to fix"
//! [input_schema.properties.dry_run]
//! type = "boolean"
//! ```
//!
//! # Execution contract — this IS the developer API
//!
//! When the model calls a custom tool, the CLI:
//!
//! - Spawns `command` **directly** via [`tokio::process::Command`] from the
//!   argv array — never through a shell. Tool *input* therefore has no
//!   injection surface. The manifest itself is workspace-trusted, at the same
//!   trust level as a `package.json` script or a Makefile target: whoever can
//!   write `.stella/tools/` can already run code in the repo.
//! - Sets the child's working directory to the **workspace root**, so a
//!   relative `command[0]` like `./scripts/lint-fix.sh` resolves against it.
//!   That holds for a plugin's tool too, because a contributed linter acts on
//!   the *user's* repository. A package therefore names the scripts it ships
//!   itself with `${plugin_dir}`, which discovery expands to the installed
//!   package directory in every `command` element and every `[env]` value
//!   before the tool is advertised — the same placeholder, expanded by the
//!   same host, that `stella_plugin`'s `Runtime::argv` already documents for a
//!   plugin's own process. The expansion is **only** for a contributed
//!   manifest: under `.stella/tools/` there is no package for it to name, so
//!   it is left verbatim rather than quietly becoming the empty string.
//! - Delivers the model's input JSON to the child **two ways**, so trivial
//!   scripts need no JSON parser:
//!   1. The whole input object is written to the child's **stdin** as one JSON
//!      document, then stdin is closed (the child sees EOF).
//!   2. Each top-level **scalar** property (string / number / bool) is exported
//!      as `STELLA_INPUT_<UPPER_SNAKE_KEY>` — e.g. `path` → `STELLA_INPUT_PATH`.
//!      Nested objects and arrays are delivered on stdin only.
//! - Applies the manifest's optional `[env]` table on top of the inherited
//!   environment, then removes credential-bearing variables from both. A
//!   custom tool receives normal task configuration, never Stella's provider
//!   keys or repository/AWS tokens.
//!
//! Outcome mapping:
//!
//! - **Exit 0** → [`ToolOutput::Ok`] with captured stdout (middle-out truncated
//!   past a byte cap, tail budget ≥ head budget per lesson L-S3). A **silent**
//!   success (exit 0, empty stdout) does not render as the empty string: it
//!   renders a deterministic input-stamped line — `` custom tool `name` exited
//!   0 with no output (input sha256/8 xxxxxxxx) `` — because an
//!   every-call-identical output is the exact shape the engine's stagnation
//!   detector kills a loop over, whatever the arguments (#3303). The digest is
//!   over the same JSON document the child received on stdin, so an
//!   identical-input repeat still renders identically.
//! - **Non-zero exit** → [`ToolOutput::Error`] carrying the exit code and the
//!   stderr tail (also truncated).
//! - **Timeout** → the process group is killed (SIGKILL to `-pid`) and a
//!   named error is returned.
//! - **Cancellation** (the driving future dropped mid-wait, e.g. Esc) → the
//!   same group kill, armed as an RAII guard
//!   (`crate::exec::GroupKillGuard`), so nothing survives the turn.
//! - **Spawn failure** (e.g. missing script) → a named error naming the path
//!   that was tried, so the developer can fix the manifest.
//!
//! # Discovery precedence
//!
//! Workspace (`<root>/.stella/tools/`) is scanned before user-global
//! (`~/.stella/tools/`, or `$STELLA_HOME/tools/` when the home is
//! redirected); on a name collision the workspace tool
//! wins and the global one is reported as a [`ToolDiagnostic`]. A malformed
//! manifest never aborts discovery — it becomes a typed per-file diagnostic so
//! `stella tools` can show developers exactly which file is broken and why. A
//! manifest whose `name` collides with a reserved built-in — live or retired
//! ([`crate::catalog::is_reserved`]) — is likewise skipped with a diagnostic.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use stella_core::ports::{DispatchAdmission, Principal, ToolExecutor, admit_dispatch};
use stella_protocol::tool::{ToolOutput, ToolSchema};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Tool names that a custom manifest may not claim — every name in the
/// canonical [`crate::catalog`], which covers every built-in registered in
/// [`crate::registry::ToolRegistry`]. A custom tool must never shadow one,
/// or the decorator chain (native ← custom ← mcp) would route the wrong
/// executor and a manifest named e.g. `delegate` or `save_state` could silently
/// replace a built-in.
///
/// This is an alias, not a copy: declaring a tool in the catalog reserves its
/// name automatically, so the two can no longer drift apart.
///
/// **Not the whole refusal set.** [`parse_manifest`] also refuses a name
/// Stella *retired* ([`crate::catalog::is_retired`]) — see
/// [`crate::catalog::is_reserved`] for why, and note that the two are
/// deliberately separate messages because the operator's remedy differs.
pub const RESERVED_NAMES: &[&str] = crate::catalog::ALL_NAMES;

/// Timeout applied when a manifest omits `timeout_ms`. Public so
/// [`crate::validate`] can explain the defaulting it mirrors.
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// Hard ceiling on `timeout_ms`; a manifest cannot ask for more. Public so
/// [`crate::validate`] can warn about the clamp it mirrors.
pub const MAX_TIMEOUT_MS: u64 = 600_000;
/// Byte cap on captured stdout/stderr before head+tail elision. Inherited
/// from the retired native shell tool's budget (#1889) so a manifest tool's
/// output window did not shrink when that tool left the surface: 2.2x
/// `exec::MAX_OUTPUT_BYTES` (30k), preserving #616's ratio.
pub(crate) const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// A parsed, validated custom tool ready to advertise and execute.
#[derive(Debug, Clone)]
pub struct CustomTool {
    /// Tool name the model sees; validated against `^[a-z][a-z0-9_]{1,63}$`
    /// and guaranteed not to be a [`RESERVED_NAMES`] entry.
    pub name: String,
    /// Human description advertised to the model.
    pub description: String,
    /// argv to spawn directly (no shell). `command[0]` is the program.
    ///
    /// On a tool a package contributed, `${plugin_dir}` is already expanded
    /// here — discovery resolves it against [`PluginToolDir::package_dir`], so
    /// nothing downstream has to know a placeholder existed.
    pub command: Vec<String>,
    /// Resolved timeout (default applied, clamped to [`MAX_TIMEOUT_MS`]).
    pub timeout_ms: u64,
    /// JSON Schema for the tool's input, converted verbatim from TOML.
    pub input_schema: Value,
    /// Extra environment variables applied to the child process. Values carry
    /// the same `${plugin_dir}` expansion [`Self::command`] does.
    pub env: HashMap<String, String>,
    /// Manifest file this tool was loaded from (for diagnostics / listing).
    pub source: PathBuf,
    /// The `[foundry]` table, present only on manifests Stella authored for
    /// itself. `None` for every hand-written manifest, which is what keeps
    /// [`crate::foundry_gate`] governing self-authored tools and nothing else.
    pub foundry: Option<crate::foundry_gate::FoundryProvenance>,
    /// The manifest's `read_only` claim (#3287). A **claim**, not a fact:
    /// it rides the tool's [`stella_protocol::ToolContract`] (via
    /// [`CustomToolSet`]'s `contracts()`), never the advertised
    /// [`ToolSchema`] — the dispatch consumers of `read_only` trust the
    /// schema bit directly, and an unreviewed script must not buy concurrent
    /// dispatch or a place inside a verifier's fence with a field it filled
    /// in itself.
    pub claimed_read_only: bool,
    /// The manifest's `risk` claim (#3287). Recorded for display/policy; the
    /// effective grade stays [`stella_protocol::RiskLevel::High`] unless the
    /// claim is `Destructive`, which *raises* it — self-reports may only
    /// ever make a tool look more dangerous, never less.
    pub claimed_risk: Option<stella_protocol::RiskLevel>,
    /// The manifest's `idempotent` claim (#3287) — carried into the
    /// contract's `idempotent` field with declared provenance.
    pub claimed_idempotent: bool,
    /// The manifest's optional `[output_schema]` (#3287) — carried into the
    /// contract so the registry-side output check (#3285) can hold a script
    /// tool to its own promise.
    pub output_schema: Option<Value>,
    /// The plugin whose package shipped this manifest, when one did (#3380).
    ///
    /// `None` for every tool the *user* wrote — `.stella/tools/*.toml` and
    /// `~/.stella/tools/*.toml`. `Some(name)` is the manifest `name` of the
    /// installed plugin whose `<plugin_dir>/tools/` the file was read from,
    /// and it is set by discovery from the directory it scanned, never by
    /// the manifest: a package cannot name itself something it is not.
    ///
    /// This is the provenance the other two package properties are built on.
    /// Retraction reads it as "which tools disappear when this plugin does"
    /// — though nothing has to *do* that, because the tools are derived from
    /// the installed package on every load and a deleted package contributes
    /// nothing to the next one. Authorization reads it through
    /// [`Self::principal`], which is the half that has teeth.
    pub contributed_by: Option<String>,
}

impl CustomTool {
    /// The caller an [`stella_core::ports::AuthzGate`] must see for a call
    /// into this tool, given the `caller` the session's stack acts as.
    ///
    /// # A plugin's tool is the plugin's, not the human's
    ///
    /// The model asks for the call, but the code that runs is a third
    /// party's, delivered by a package a user consented to install and
    /// carrying whatever authority *that* consent granted — not the
    /// operator's. A gate handed [`Principal::User`] for a plugin's script
    /// could not express "this plugin may not touch the network" at all,
    /// because it would be answering about the human. So a contributed tool
    /// authorizes as [`Principal::Plugin`], for the same reason a plugin's
    /// hook dispatch does (`doc:pipeline-as-plugins` §A1), and the session's
    /// own principal is passed through untouched for every tool the user
    /// wrote themselves.
    ///
    /// Note what this does **not** do: it never *widens*. A plugin tool
    /// called from a sub-agent lane does not inherit that lane's principal
    /// and a user tool never becomes a plugin's — the substitution is total
    /// and one-directional, decided by provenance discovery stamped from the
    /// directory it read.
    pub fn principal(&self, caller: &Principal) -> Principal {
        match &self.contributed_by {
            Some(plugin) => Principal::Plugin(plugin.clone()),
            None => caller.clone(),
        }
    }

    /// The schema advertised to the model — the same shape a built-in emits.
    ///
    /// `read_only`/`speculation_safe` stay `false` **regardless of the
    /// manifest's claims**: this schema feeds the engine's dispatch
    /// grouping and the read-only fences, which trust the bit directly. The
    /// claims live on [`Self::contract`].
    pub fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.name.clone(),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
            read_only: false,
            speculation_safe: false,
        }
    }

    /// The manifest's self-reported claims as a short display prefix
    /// (#3287): `"[claims read-only] "`, `"[claims risk: destructive] "`, a
    /// combination, or `""` when the manifest claims nothing. The word
    /// "claims" is the point — a listing must show the self-report as a
    /// self-report, never as a fact Stella vouches for.
    pub fn claims_label(&self) -> String {
        let mut claims = Vec::new();
        if self.claimed_read_only {
            claims.push("read-only".to_string());
        }
        if let Some(risk) = self.claimed_risk {
            claims.push(format!("risk: {}", risk.as_str()));
        }
        if self.claimed_idempotent {
            claims.push("idempotent".to_string());
        }
        if claims.is_empty() {
            String::new()
        } else {
            format!("[claims {}] ", claims.join(", "))
        }
    }

    /// The governance contract for this tool (#3287):
    /// [`stella_protocol::Provenance::Declared`] with the manifest's claims
    /// attached. The contract's embedded schema carries the `read_only`
    /// *claim* verbatim — preserved for display and policy, buying nothing
    /// (`ToolContract::trusted_read_only` stays false) — while the
    /// *advertised* schema from [`Self::schema`] keeps the bit off.
    pub fn contract(&self) -> stella_protocol::ToolContract {
        let mut claimed_schema = self.schema();
        claimed_schema.read_only = self.claimed_read_only;
        let mut contract = stella_protocol::ToolContract::declared(claimed_schema)
            .with_idempotent(self.claimed_idempotent);
        // Raise-only: a self-report may make a tool look more dangerous,
        // never less. `declared` already graded it High.
        if self.claimed_risk == Some(stella_protocol::RiskLevel::Destructive) {
            contract.risk = stella_protocol::RiskLevel::Destructive;
        }
        if let Some(output_schema) = &self.output_schema {
            contract = contract.with_output_schema(output_schema.clone());
        }
        contract
    }
}

/// A manifest that could not be loaded, kept so the CLI can surface it to the
/// developer instead of silently dropping the tool.
#[derive(Debug, Clone)]
pub struct ToolDiagnostic {
    /// The manifest file (or directory) the problem is about.
    pub path: PathBuf,
    /// Human-readable reason, safe to print in `stella tools`.
    pub reason: String,
}

/// The result of scanning the tool directories: everything that loaded, plus a
/// typed diagnostic for everything that did not.
///
/// A `DiscoveryReport` is a **gated** result. The only way to obtain one from
/// the filesystem is [`crate::foundry_gate::gate_report`] — see
/// [`UngatedDiscovery`] for why.
#[derive(Debug, Clone, Default)]
pub struct DiscoveryReport {
    pub tools: Vec<CustomTool>,
    pub diagnostics: Vec<ToolDiagnostic>,
}

/// What a directory scan found, before the tool-foundry gate has ruled on it.
///
/// # Why discovery does not simply return the tools
///
/// The gate ([`crate::foundry_gate`]) decides whether a *self-authored* tool
/// may register: whether the workspace adopted it, whether a human enabled it,
/// and whether its bytes still match the ones its witness ran against. That
/// decision is worthless if a caller can skip it, and skipping it is the easy
/// mistake — a new discovery site is three lines and reads as complete.
///
/// It happened. The gate first shipped applied at the session's discovery
/// path, while the plain `stella tools` listing and the best-of-N candidate
/// surface each discovered tools on their own paths and offered whatever they
/// found. The listing advertised tools the session withheld. Patching those
/// two sites fixed the instances and left the shape: a fourth path would have
/// been ungated too, silently, and fail-**open**.
///
/// So the tools do not leave this type. `Vec<CustomTool>` — the thing a
/// [`CustomToolSet`] needs in order to execute anything — can only be got by
/// handing this to the gate. Forgetting is a compile error rather than an
/// unapproved capability, and a caller who does not care about foundry tools
/// still has to say so (an empty ledger withholds them, which is the safe
/// direction).
///
/// # What it will tell you without gating
///
/// Information, never capability: what was found ([`names`](Self::names)), what
/// was skipped and why ([`diagnostics`](Self::diagnostics)), and whether any of
/// it is self-authored at all ([`has_foundry_authored`](Self::has_foundry_authored),
/// which lets a host skip opening the ledger when there is nothing to rule on).
/// None of those hand back something runnable.
#[derive(Debug, Clone, Default)]
pub struct UngatedDiscovery {
    tools: Vec<CustomTool>,
    diagnostics: Vec<ToolDiagnostic>,
}

impl UngatedDiscovery {
    /// The names of every manifest that loaded — including ones the gate will
    /// go on to withhold, because a withheld tool still occupies its name on
    /// disk. This is the question a duplicate-name check wants.
    pub fn names(&self) -> Vec<&str> {
        self.tools.iter().map(|tool| tool.name.as_str()).collect()
    }

    /// Manifests that could not be loaded, and why.
    pub fn diagnostics(&self) -> &[ToolDiagnostic] {
        &self.diagnostics
    }

    /// Whether the scan found nothing at all — no tools and no diagnostics.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty() && self.diagnostics.is_empty()
    }

    /// Whether any manifest here claims foundry authorship, i.e. whether the
    /// gate has anything to rule on. A host uses this to avoid reading the
    /// adoption ledger for the overwhelmingly common workspace that has no
    /// self-authored tools at all.
    pub fn has_foundry_authored(&self) -> bool {
        self.tools.iter().any(|tool| {
            tool.foundry
                .as_ref()
                .is_some_and(|p| p.is_foundry_authored())
        })
    }

    /// Hand the contents to the gate. Crate-private: this is the seam the type
    /// exists to protect, and [`crate::foundry_gate::gate_report`] is its one
    /// caller.
    pub(crate) fn into_parts(self) -> (Vec<CustomTool>, Vec<ToolDiagnostic>) {
        (self.tools, self.diagnostics)
    }

    /// Build one from parts already in hand — for a producer that found
    /// manifests some way other than a directory scan. Crate-private, and
    /// harmless either way: constructing one grants nothing, since getting the
    /// tools back out still means going through the gate.
    #[cfg(test)]
    pub(crate) fn from_parts(tools: Vec<CustomTool>, diagnostics: Vec<ToolDiagnostic>) -> Self {
        Self { tools, diagnostics }
    }
}

/// Wire shape of a manifest file. Kept private — callers see [`CustomTool`],
/// which only exists once every field has been validated.
#[derive(Deserialize)]
struct RawManifest {
    name: String,
    description: String,
    command: Vec<String>,
    timeout_ms: Option<u64>,
    #[serde(default)]
    env: HashMap<String, String>,
    // The `toml` deserializer populates this straight into a JSON value, so a
    // JSON-Schema-shaped TOML table converts verbatim (integers, floats,
    // bools, strings, arrays and nested tables all round-trip).
    #[serde(default)]
    input_schema: Option<Value>,
    // Absent on every hand-written manifest. Unknown fields are ignored by
    // this deserializer, so adding it cannot reject a manifest that predates
    // it — a `[foundry]` table only ever *adds* the gate's constraints.
    #[serde(default)]
    foundry: Option<crate::foundry_gate::FoundryProvenance>,
    // The #3287 claims. All default-absent so every pre-#3287 manifest
    // parses unchanged; each is a self-report the contract records with
    // declared provenance, never a fact the dispatch plane acts on.
    #[serde(default)]
    read_only: bool,
    #[serde(default)]
    risk: Option<stella_protocol::RiskLevel>,
    #[serde(default)]
    idempotent: bool,
    #[serde(default)]
    output_schema: Option<Value>,
}

/// `true` iff `name` matches `^[a-z][a-z0-9_]{1,63}$` (a lowercase letter then
/// 1–63 of `[a-z0-9_]`). Hand-rolled to avoid pulling in `regex`.
fn is_valid_tool_name(name: &str) -> bool {
    let len = name.len();
    if !(2..=64).contains(&len) {
        return false;
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Parse and validate one manifest's text. `source` is only used to stamp the
/// resulting [`CustomTool`]; it is not read. Returns a human-readable error on
/// any validation failure — the caller turns it into a [`ToolDiagnostic`].
pub fn parse_manifest(text: &str, source: &Path) -> Result<CustomTool, String> {
    let raw: RawManifest = toml::from_str(text).map_err(|e| format!("invalid TOML: {e}"))?;

    if !is_valid_tool_name(&raw.name) {
        return Err(format!(
            "invalid tool name `{}` — must match ^[a-z][a-z0-9_]{{1,63}}$",
            raw.name
        ));
    }
    if RESERVED_NAMES.contains(&raw.name.as_str()) {
        return Err(format!(
            "tool name `{}` is reserved by a built-in and cannot be redefined",
            raw.name
        ));
    }
    if crate::catalog::is_retired(&raw.name) {
        // A retired name that is ALSO a live group key needs a different
        // sentence. `task` is the case: the delegation tool moved to
        // `delegate` (#3192), but `task` still addresses the whole
        // coordination group, so `{"task": "off"}` withholds seven tools
        // rather than "nothing". Saying otherwise would tell an operator
        // their working config is inert.
        let reason = if crate::catalog::groups().contains(&raw.name.as_str()) {
            format!(
                "it is the switch key for the `{name}` group, so an operator's \
                 `\"tools\": {{\"{name}\": \"off\"}}` already withholds every tool in \
                 that group — a manifest answering to it would silently join them",
                name = raw.name
            )
        } else {
            format!(
                "Stella dispatched a built-in called `{name}` and retired it, so the \
                 model still carries that tool's priors and an operator's \
                 `\"tools\": {{\"{name}\": \"off\"}}` would address this manifest \
                 instead of nothing",
                name = raw.name
            )
        };
        return Err(format!(
            "tool name `{name}` is reserved: {reason} — pick a name Stella has never \
             used (#3237)",
            name = raw.name
        ));
    }
    if raw.description.trim().is_empty() {
        return Err("`description` is required and must be non-empty".to_string());
    }
    if raw.command.is_empty() || raw.command[0].trim().is_empty() {
        return Err("`command` must be a non-empty argv array".to_string());
    }

    let input_schema = match raw.input_schema {
        Some(v) if v.is_object() => v,
        Some(_) => {
            return Err("`input_schema` must be a table (JSON Schema object)".to_string());
        }
        None => serde_json::json!({ "type": "object", "properties": {} }),
    };

    let timeout_ms = match raw.timeout_ms {
        None | Some(0) => DEFAULT_TIMEOUT_MS,
        Some(t) => t.min(MAX_TIMEOUT_MS),
    };

    let output_schema = match raw.output_schema {
        Some(v) if v.is_object() => Some(v),
        Some(_) => {
            return Err("`output_schema` must be a table (JSON Schema object)".to_string());
        }
        None => None,
    };

    Ok(CustomTool {
        name: raw.name,
        description: raw.description,
        command: raw.command,
        timeout_ms,
        input_schema,
        env: raw.env,
        source: source.to_path_buf(),
        foundry: raw.foundry,
        claimed_read_only: raw.read_only,
        claimed_risk: raw.risk,
        claimed_idempotent: raw.idempotent,
        output_schema,
        // Discovery stamps this from the directory it read; a manifest may
        // not claim to have been shipped by a plugin.
        contributed_by: None,
    })
}

/// Discover custom tools for `workspace_root`, resolving the user-global
/// location through `stella-home` (`$STELLA_HOME`, else `~/.stella`). Thin
/// env-reading wrapper over [`discover_in`] — tests inject a root directly
/// rather than mutating the process env.
pub fn discover(workspace_root: &Path) -> UngatedDiscovery {
    discover_in_scopes(workspace_root, stella_home::stella_home().as_deref(), true)
}

/// Discover custom tools from the workspace directory then the (optional)
/// user-global stella root (`~/.stella`, or wherever `STELLA_HOME` points).
/// Workspace wins on name collisions; a `None` root skips the global scan
/// entirely. Never fails: unreadable or malformed manifests become
/// diagnostics, not errors.
pub fn discover_in(workspace_root: &Path, user_root: Option<&Path>) -> UngatedDiscovery {
    discover_in_scopes(workspace_root, user_root, true)
}

/// Discover custom tools from the allowed configuration scopes.
///
/// `include_workspace` must come from the host's authority policy. When it is
/// false, only the user-global directory is scanned; repository manifests are
/// neither parsed nor reported as diagnostics.
pub fn discover_in_scopes(
    workspace_root: &Path,
    user_root: Option<&Path>,
    include_workspace: bool,
) -> UngatedDiscovery {
    discover_with_plugins(workspace_root, user_root, include_workspace, &[])
}

/// One installed plugin's contributed tool directory: the manifest `name`
/// every tool found under it is attributed to, the package it was installed
/// as, and `<plugin_dir>/tools`.
///
/// The host resolves these — only it knows what is installed, which tier a
/// package came from, whether the workspace is trusted to run its code, and
/// whether an operator switched the plugin off. This crate is handed the
/// answer and never asks.
#[derive(Debug, Clone)]
pub struct PluginToolDir {
    /// The plugin's manifest `name`, as [`CustomTool::contributed_by`] and
    /// [`stella_core::ports::Principal::Plugin`] spell it.
    pub plugin: String,
    /// `<plugin_dir>` — the installed package's own root, and what
    /// `${plugin_dir}` expands to in the manifests found under [`Self::dir`].
    ///
    /// Carried alongside `dir` rather than derived from its parent: the host
    /// is the one that knows where a package was installed, and a layout
    /// re-derived here would be a second place to keep it true.
    pub package_dir: PathBuf,
    /// `<plugin_dir>/tools`. A missing directory is a plugin that ships no
    /// tools, which is the ordinary case and not a diagnostic.
    pub dir: PathBuf,
}

/// Expand `${plugin_dir}` through a contributed tool's `command` argv and
/// `[env]` values.
///
/// The substitution itself is `stella_plugin::expand_plugin_dir`: the
/// placeholder is a convention that crate *specifies* (`Runtime::argv`), and
/// carrying it out is the host's job in every case because the plugin crate
/// resolves no paths. It lived here as a second copy of the string literal
/// until #4301, which is the shape where one copy gains an escape rule and the
/// others silently do not.
///
/// Called only for a manifest read out of a package directory. A user's own
/// `.stella/tools/` manifest is left verbatim: no package is in scope there,
/// and expanding the placeholder to nothing would silently turn
/// `${plugin_dir}/x.sh` into a different path rather than leaving a visible
/// spawn failure naming what the manifest actually asked for.
fn expand_package_dir(tool: &mut CustomTool, package_dir: &Path) {
    for arg in &mut tool.command {
        *arg = stella_plugin::expand_plugin_dir(arg, package_dir);
    }
    for value in tool.env.values_mut() {
        *value = stella_plugin::expand_plugin_dir(value, package_dir);
    }
}

/// [`discover_in_scopes`], plus the tools installed plugins contribute
/// (#3380).
///
/// # Precedence: the user's own name always wins, and never silently
///
/// Plugin directories are scanned **last**, after the workspace's and the
/// user's own, so a plugin manifest claiming a name one of those already
/// defined is dropped with a [`ToolDiagnostic`] naming both files. That is
/// the same rule and the same machinery `.stella/tools` already beats
/// `~/.stella/tools` with — a plugin does not get a stronger claim on a name
/// than the user's own two scopes have on each other's.
///
/// The direction is the security-relevant half. Letting a package capture
/// `deploy` from under the script a developer wrote would mean an install
/// silently re-pointed a name they already call, and the model — which
/// chooses by description alone — would never see the substitution. Refusing
/// the *plugin* copy makes the worst case "the tool I installed is missing,
/// and `stella plugin list` says why".
///
/// Plugin-versus-plugin collisions resolve the same way: the caller supplies
/// the directories in roster order (name-ordered), so the first plugin to
/// claim a name keeps it and the second is reported.
pub fn discover_with_plugins(
    workspace_root: &Path,
    user_root: Option<&Path>,
    include_workspace: bool,
    plugin_dirs: &[PluginToolDir],
) -> UngatedDiscovery {
    let mut report = UngatedDiscovery::default();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Workspace first so it wins collisions, then user-global.
    let mut dirs: Vec<(PathBuf, Option<&PluginToolDir>)> = Vec::new();
    if include_workspace {
        dirs.push((workspace_root.join(".stella").join("tools"), None));
    }
    if let Some(user_root) = user_root {
        dirs.push((user_root.join("tools"), None));
    }
    // Last, so a plugin never takes a name the user's own manifests defined.
    for contributed in plugin_dirs {
        dirs.push((contributed.dir.clone(), Some(contributed)));
    }

    for (dir, contributor) in dirs {
        load_dir(&dir, contributor, &mut seen, &mut report);
    }
    report
}

/// Scan one directory for `*.toml` manifests, appending tools and diagnostics
/// to `report`. Absent directories are silently ignored (having no tools dir is
/// normal); files are processed in sorted order for deterministic output.
///
/// `contributor` is the package this directory belongs to, or `None` for the
/// user's own scopes. Its `plugin` becomes [`CustomTool::contributed_by`] on
/// everything found here, which is why provenance cannot be forged: it comes
/// from the directory being read, not from anything inside the file. Its
/// `package_dir` is what [`expand_package_dir`] resolves `${plugin_dir}`
/// against, so the same fact decides both, and neither can apply to a
/// manifest the user wrote.
fn load_dir(
    dir: &Path,
    contributor: Option<&PluginToolDir>,
    seen: &mut std::collections::HashSet<String>,
    report: &mut UngatedDiscovery,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return, // no such directory → nothing to load, not an error
    };

    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("toml"))
        .collect();
    paths.sort();

    for path in paths {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                report.diagnostics.push(ToolDiagnostic {
                    path: path.clone(),
                    reason: format!("could not read manifest: {e}"),
                });
                continue;
            }
        };
        match parse_manifest(&text, &path) {
            Ok(mut tool) => {
                if seen.contains(&tool.name) {
                    report.diagnostics.push(ToolDiagnostic {
                        path: path.clone(),
                        reason: match contributor {
                            // Never "and the plugin's copy won": a package
                            // silently replacing a name the user already
                            // defined is the one outcome the precedence rule
                            // exists to forbid, so the message says whose
                            // copy is running.
                            Some(contributor) => format!(
                                "tool `{}` is already defined by a manifest you installed \
                                 yourself — the copy the `{plugin}` plugin ships is ignored, \
                                 and yours is the one that runs",
                                tool.name,
                                plugin = contributor.plugin
                            ),
                            None => format!(
                                "tool `{}` already defined by an earlier (workspace) manifest \
                                 — this one is ignored",
                                tool.name
                            ),
                        },
                    });
                    continue;
                }
                seen.insert(tool.name.clone());
                if let Some(contributor) = contributor {
                    tool.contributed_by = Some(contributor.plugin.clone());
                    expand_package_dir(&mut tool, &contributor.package_dir);
                }
                report.tools.push(tool);
            }
            Err(reason) => report.diagnostics.push(ToolDiagnostic { path, reason }),
        }
    }
}

/// Map a JSON input key to its `STELLA_INPUT_*` env var name: uppercased, with
/// any non-`[A-Z0-9_]` byte replaced by `_`.
fn env_var_name(key: &str) -> String {
    let mut out = String::with_capacity("STELLA_INPUT_".len() + key.len());
    out.push_str("STELLA_INPUT_");
    for c in key.chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_uppercase());
        } else {
            out.push('_');
        }
    }
    out
}

/// Render a scalar JSON value as the string form exported to `STELLA_INPUT_*`.
/// Returns `None` for arrays, objects and null (delivered on stdin only).
fn scalar_env_value(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Spawn `cmd`, retrying briefly while the kernel refuses with `ETXTBSY`.
///
/// `ETXTBSY` means the executable is still open for writing somewhere — for a
/// custom tool that is almost always a freshly written script whose write
/// descriptor another thread's `fork` momentarily duplicated (`O_CLOEXEC`
/// closes at *exec*, not at fork, so the window between a concurrent fork and
/// its exec keeps the file "busy"). The condition is transient by
/// construction, so a short bounded backoff (~300ms total) resolves it; a
/// script genuinely held open for writing still fails after the last attempt
/// with the original error. (#749)
async fn spawn_retrying_etxtbsy(cmd: &mut Command) -> std::io::Result<tokio::process::Child> {
    let mut delay = Duration::from_millis(10);
    for _ in 0..5 {
        match cmd.spawn() {
            Err(e) if is_etxtbsy(&e) => {
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
            other => return other,
        }
    }
    cmd.spawn()
}

/// True when `e` is the unix `ETXTBSY` ("text file busy") condition.
fn is_etxtbsy(e: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        e.raw_os_error() == Some(libc::ETXTBSY)
    }
    #[cfg(not(unix))]
    {
        let _ = e;
        false
    }
}

/// Run one custom tool against the model's `input`, from `workspace_root`.
/// Never returns `Err` — every failure mode is a named [`ToolOutput::Error`],
/// because tool failures are model-visible data, not engine faults.
async fn run_custom(tool: &CustomTool, input: &Value, workspace_root: &Path) -> ToolOutput {
    // A self-authored tool was approved for the bytes it had when the gate
    // ruled on it, once, at discovery. The script is a file in the repository
    // and this is the point of use, so the approval is checked against what is
    // about to be spawned rather than against what was scanned.
    if let Err(message) = crate::foundry_gate::recheck_before_launch(tool, workspace_root) {
        return ToolOutput::error(message);
    }

    let mut cmd = Command::new(&tool.command[0]);
    cmd.args(&tool.command[1..]);
    cmd.current_dir(workspace_root);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    // Reap the direct child if the driving future is dropped (tokio keeps it
    // running by default). A backstop only — it does NOT reach whatever the
    // script itself spawned; the `GroupKillGuard` below is what covers those,
    // on every platform since #3550.
    cmd.kill_on_drop(true);

    // Manifest env (workspace-trusted) first …
    for (k, v) in &tool.env {
        cmd.env(k, v);
    }
    // A custom tool spawns a model-invoked command, so it is a full command
    // path like the hook runner — it must also strip the surrounding git env
    // and forced-color vars, not only credentials.
    // Manifest env is deliberately set BEFORE the scrub (a credential-shaped
    // manifest entry is stripped like an inherited one).
    crate::subprocess_env::scrub_spawn_env(&mut cmd);
    // The STELLA_INPUT_* exports come AFTER the scrub: an input property
    // named `api_token` exports as STELLA_INPUT_API_TOKEN, whose `_TOKEN`
    // suffix the credential scrub matches — scrubbing after the export
    // silently deleted it, breaking this module's "each scalar is exported"
    // contract while the same value flowed to the child verbatim on stdin
    // anyway (the env copy protected nothing). Keys come from the model but
    // are namespaced under STELLA_INPUT_, so they cannot clobber PATH, any
    // inherited variable, or a host credential name.
    if let Some(obj) = input.as_object() {
        for (key, value) in obj {
            if let Some(v) = scalar_env_value(value) {
                cmd.env(env_var_name(key), v);
            }
        }
    }

    // New process group so a timeout kills the whole child tree.
    crate::exec::detach_into_own_process_group(&mut cmd);

    let mut child = match spawn_retrying_etxtbsy(&mut cmd).await {
        Ok(c) => c,
        Err(e) => {
            return ToolOutput::error(format!(
                "custom tool `{}` failed to spawn `{}` (cwd {}): {e}",
                tool.name,
                tool.command[0],
                workspace_root.display()
            ));
        }
    };

    let pid = child.id().unwrap_or(0) as i32;
    // Cancellation backstop: a dropped future (Esc, the engine's tool
    // timeout, a fleet stop) must not leave the detached group running.
    let mut guard = crate::exec::GroupKillGuard::arm(pid);

    // Deliver the input as one JSON document on stdin, concurrently with
    // draining stdout/stderr, so a chatty child cannot deadlock the write.
    if let Some(mut stdin) = child.stdin.take() {
        let payload = serde_json::to_vec(input).unwrap_or_default();
        tokio::spawn(async move {
            let _ = stdin.write_all(&payload).await;
            let _ = stdin.shutdown().await;
            // stdin dropped here → child sees EOF.
        });
    }

    let timeout = Duration::from_millis(tool.timeout_ms);
    // Capped capture, like every other spawn path: a manifest tool is
    // repository-authored, so its output volume is not Stella's to trust —
    // `wait_with_output` would hold all of it before the truncation below
    // ever ran. See `crate::exec::MAX_CAPTURE_BYTES`.
    let output = match tokio::time::timeout(
        timeout,
        crate::exec::wait_with_capped_output(child, crate::exec::MAX_CAPTURE_BYTES),
    )
    .await
    {
        Ok(Ok(output)) => {
            guard.disarm();
            output
        }
        // Wait failure leaves the child's state unknown — the still-armed
        // guard kills the group on return rather than leak it.
        Ok(Err(e)) => {
            return ToolOutput::error(format!("custom tool `{}` failed: {e}", tool.name));
        }
        Err(_) => {
            guard.kill_now();
            return ToolOutput::error(format!(
                "custom tool `{}` timed out after {}ms",
                tool.name, tool.timeout_ms
            ));
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.success() {
        let content = crate::exec::truncate_middle_capped(&stdout, MAX_OUTPUT_BYTES);
        // A manifest that declares `[output_schema]` promises its stdout is
        // JSON matching it (#3287): the whole stdout is parsed into the
        // structured half (`Ok.data`) and checked with the same subset
        // checker the registry runs (#3285). A breach is the tool's own
        // defect — `Internal`, never model misuse.
        let Some(expected) = &tool.output_schema else {
            // A silent success (exit 0, empty stdout) renders the
            // deterministic input-stamped line, never the empty string —
            // an every-call-identical output is what the stagnation
            // detector kills a loop over (#3303). A schema-declaring
            // manifest never takes this arm: its empty stdout is a
            // contract breach reported below.
            if output.stdout.is_empty() {
                return ToolOutput::ok(silent_success_stamp(&tool.name, input));
            }
            return ToolOutput::ok(content);
        };
        let parsed: Value = match serde_json::from_str(stdout.trim()) {
            Ok(parsed) => parsed,
            Err(e) => {
                return ToolOutput::classified_error(
                    stella_protocol::ErrorClass::Internal,
                    format!(
                        "custom tool `{}` violated its output contract: it declares an \
                         output_schema but its stdout is not JSON ({e})",
                        tool.name
                    ),
                );
            }
        };
        if let Some(violation) = crate::registry::validate::check(expected, &parsed) {
            return ToolOutput::classified_error(
                stella_protocol::ErrorClass::Internal,
                format!(
                    "custom tool `{}` violated its output contract: {violation}",
                    tool.name
                ),
            );
        }
        ToolOutput::ok_with_data(content, parsed)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_string());
        let tail = crate::exec::truncate_middle_capped(&stderr, MAX_OUTPUT_BYTES);
        ToolOutput::error(format!(
            "custom tool `{}` exited with code {code}\n[stderr]\n{tail}",
            tool.name
        ))
    }
}

/// The line a **silent** success (exit 0, empty stdout) renders instead of the
/// empty string. A silent success is byte-identical on every call, whatever
/// the arguments — the stagnation detector keys on exactly that shape and
/// would kill a legitimate work loop as stuck (#3303, the surviving half of
/// #3176). There is no content to digest (the content is the child's stdout
/// by contract), so the stamp is input-derived: the sha256/8 of the same JSON
/// document the child received on stdin. Deterministic by contract — never a
/// timing, never randomness (#2706) — so an identical-input repeat still
/// renders identically and genuine stagnation is still caught.
///
/// Named (rather than inlined at the render site) so
/// [`crate::foundry_witness`] can recognize the harness's own stamp and keep
/// classifying a silent tool's evidence as [no
/// output](crate::foundry_witness::VacuousWitness::EmptyOutput): the stamp
/// exists for the loop detector, and must not double as something a
/// capability witness can assert on — it would hold for any process that
/// exits 0.
pub(crate) fn silent_success_stamp(name: &str, input: &Value) -> String {
    let payload = serde_json::to_vec(input).unwrap_or_default();
    // `digest` always returns 64 ASCII hex chars, so the slice cannot panic.
    let identity = &crate::foundry_gate::digest(&payload)[..8];
    format!("custom tool `{name}` exited 0 with no output (input sha256/8 {identity})")
}

/// A [`ToolExecutor`] that adds discovered custom tools on top of an inner
/// executor (the native [`crate::registry::ToolRegistry`], or the MCP-merged
/// set once that lands). Composes exactly like the CLI's `InteractiveToolSet`:
/// `schemas()` is the inner's plus the customs', and `execute()` routes an
/// exact custom-name match, otherwise falls through to the inner executor.
pub struct CustomToolSet<'a> {
    inner: Inner<'a>,
    tools: Vec<CustomTool>,
    workspace_root: PathBuf,
}

/// The wrapped executor, held either by borrow (the session's per-turn tool
/// chain, a stack local) or owned via `Arc` (a best-of-N candidate, whose
/// chain is built dynamically and outlives every borrow — the workspace owns
/// `Arc<ToolRegistry>` + this set together with no self-reference).
enum Inner<'a> {
    Borrowed(&'a dyn ToolExecutor),
    Owned(std::sync::Arc<dyn ToolExecutor>),
}

impl Inner<'_> {
    fn get(&self) -> &dyn ToolExecutor {
        match self {
            Inner::Borrowed(inner) => *inner,
            Inner::Owned(inner) => inner.as_ref(),
        }
    }
}

impl<'a> CustomToolSet<'a> {
    pub fn new(
        inner: &'a dyn ToolExecutor,
        tools: Vec<CustomTool>,
        workspace_root: PathBuf,
    ) -> Self {
        Self {
            inner: Inner::Borrowed(inner),
            tools,
            workspace_root,
        }
    }
}

impl CustomToolSet<'static> {
    /// Own the inner executor by `Arc` — for callers that must hold the whole
    /// chain (registry + customs) as one value, e.g. a boxed best-of-N
    /// candidate workspace. `workspace_root` re-roots custom-tool subprocesses
    /// (they spawn with it as cwd), so a snapshot root isolates them there.
    pub fn new_owned(
        inner: std::sync::Arc<dyn ToolExecutor>,
        tools: Vec<CustomTool>,
        workspace_root: PathBuf,
    ) -> Self {
        Self {
            inner: Inner::Owned(inner),
            tools,
            workspace_root,
        }
    }
}

#[async_trait]
impl ToolExecutor for CustomToolSet<'_> {
    fn schemas(&self) -> Vec<ToolSchema> {
        let mut schemas = self.inner.get().schemas();
        schemas.extend(self.tools.iter().map(CustomTool::schema));
        schemas
    }

    /// The inner surface's contracts plus one declared contract per custom
    /// tool (#3287) — the manifest's claims ride here, with untrusted
    /// provenance, never on the advertised schemas above.
    fn contracts(&self) -> Vec<stella_protocol::ToolContract> {
        let mut contracts = self.inner.get().contracts();
        contracts.extend(self.tools.iter().map(CustomTool::contract));
        contracts
    }

    /// A custom tool is dispatched HERE, never through the inner executor, so
    /// the inner executor's blocking policy chain and approval flow would
    /// never see it — the #2793 hole. The shared gate closes it: the same
    /// entry a built-in passes through, run before the script spawns, with a
    /// `modify` decision's rewritten input honoured exactly as the registry
    /// honours it. Names this set does not own fall through ungated on
    /// purpose: the inner executor gates them itself, and gating twice would
    /// ask a human twice for one call.
    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        if let Some(tool) = self.tools.iter().find(|t| t.name == name) {
            let admitted = admit_dispatch(self.dispatch_gate(), name, input).await;
            let input = match &admitted {
                DispatchAdmission::Admit => input,
                DispatchAdmission::AmendedInput(amended) => amended,
                DispatchAdmission::Refuse(refusal) => return refusal.clone(),
            };
            return run_custom(tool, input, &self.workspace_root).await;
        }
        self.inner.get().execute(name, input).await
    }

    /// Forwarded: the gate belongs to the base of the chain, and the
    /// dispatch above reads it back through this accessor. A wrapper below
    /// this one that fails to forward silently un-gates every custom tool in
    /// that session — which is why every decorator in the workspace forwards
    /// (see the port's contract).
    fn dispatch_gate(&self) -> Option<&dyn stella_core::ports::DispatchGate> {
        self.inner.get().dispatch_gate()
    }

    /// Forwarded: this is a decorator, and a decorator that let the default
    /// `0.0` stand would silently drop sub-agent spend out of the parent's
    /// budget (see the port's contract).
    fn drain_sub_agent_spend_usd(&self) -> f64 {
        self.inner.get().drain_sub_agent_spend_usd()
    }

    /// Forwarded for the same reason as the spend drain above: a swallowed
    /// wait request silently turns parked waits (#1471) back into
    /// model-step polling.
    fn drain_wait_request(&self) -> Option<stella_core::WaitRequest> {
        self.inner.get().drain_wait_request()
    }

    /// Forwarded: a decorator that let the empty default stand would silently
    /// turn the end-of-turn service assertion (#2764) off for every surface
    /// composed through it — the agent goes back to declaring a service done
    /// without ever being asked whether it is still listening.
    fn live_services(&self) -> Vec<stella_core::LiveService> {
        self.inner.get().live_services()
    }

    /// Forwarded: letting the empty default stand would silently serialize the
    /// inner executor's sibling spawns (see the port's contract). Custom
    /// script tools spawn workspace subprocesses and make no such claim.
    fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
        self.inner.get().parallel_safe_names()
    }
}

#[cfg(test)]
mod tests;
