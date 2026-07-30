//! Three-scope settings capture, overlay, trust restoration, and authority merge.

use std::sync::atomic::{AtomicBool, Ordering};

use stella_tools::policy::ToolPolicy;

use super::authority::{
    apply_tool_ceiling, managed_tool_ceiling, restore_project_prompts, restore_project_tools,
};
use super::managed::managed_settings_path;
use super::*;

/// What each settings scope says about tools, kept APART instead of merged.
///
/// [`Settings::tool_policy`] answers *whether* a tool is on — the only question
/// enforcement asks. A settings editor has to answer a second one: *who said
/// so*, because "bash is off" and "your org turned bash off" are different
/// facts, and only one of them is something the operator can change. Merging
/// the scopes destroys exactly that distinction, so this type never does.
#[derive(Debug, Clone, Default)]
pub struct ToolScopePolicies {
    /// The org-managed ceiling, [`managed_tool_ceiling`]'s output — the same
    /// value [`Settings::load`] folds into the merged settings. A tool it
    /// denies is denied for good: neither user nor project can grant it, and
    /// the editor must render it LOCKED rather than as a switch that appears
    /// to work and silently does nothing.
    pub managed: ToolPolicy,
    /// The user scope's own switches (`~/.stella/settings.json`).
    pub user: ToolPolicy,
    /// The project scope's own switches (`<workspace>/.stella/settings.json`),
    /// as written — trust restoration is not applied, because this is a report
    /// of what the file says, not of what the runtime honored.
    pub project: ToolPolicy,
}

/// Append `extra`'s matchers onto `base`, per event. `None + None` stays
/// `None` so a hook-free session carries no hooks handle at all.
fn concat_hooks(base: &mut Option<Hooks>, extra: &Hooks) {
    let target = base.get_or_insert_with(Hooks::default);
    let join = |dst: &mut Option<Vec<_>>, src: &Option<Vec<_>>| {
        if let Some(src) = src {
            dst.get_or_insert_with(Vec::new).extend(src.iter().cloned());
        }
    };
    join(&mut target.session_start, &extra.session_start);
    join(&mut target.pre_tool_use, &extra.pre_tool_use);
    join(&mut target.post_tool_use, &extra.post_tool_use);
}

impl Settings {
    /// Read and parse one scope exactly once. A missing file is represented by
    /// an empty captured snapshot; malformed content remains a named error.
    fn load_scope(path: &Path) -> Result<Self, String> {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
        };
        let scope: Settings = serde_json::from_str(&contents)
            .map_err(|error| format!("invalid settings file {}: {error}", path.display()))?;
        for (id, entry) in &scope.providers {
            if let Some(stated) = &entry.id
                && stated != id
            {
                return Err(format!(
                    "settings file {}: providers.{id} declares id `{stated}` — the \
                     entry's id must match its key",
                    path.display()
                ));
            }
        }
        Ok(scope)
    }

    /// Overlay one already-parsed scope without touching the filesystem.
    fn overlay_scope(&mut self, scope: &Settings) {
        for (id, entry) in &scope.providers {
            self.providers.entry(id.clone()).or_default().overlay(entry);
        }
        if let Some(hooks) = &scope.hooks {
            concat_hooks(&mut self.hooks, hooks);
        }
        if let Some(mcp) = &scope.mcp {
            let target = self.mcp.get_or_insert_with(McpSettings::default);
            if let Some(url) = &mcp.registry_url {
                target.registry_url = Some(url.clone());
            }
        }
        // Tool switches merge PER KEY, later scope wins — the same shape the
        // two hardcoded fields had, now over an open map. A scope that does
        // not mention a key leaves the lower scope's value alone, so a
        // project narrowing `bash` never resets a user's `{"mcp": "off"}`.
        if let Some(tools) = &scope.tools {
            let target = self.tools.get_or_insert_with(ToolsSettings::default);
            for (key, &toggle) in &tools.entries {
                target.entries.insert(key.clone(), toggle);
            }
        }
        if let Some(engine) = &scope.agent_engine_config {
            self.agent_engine_config
                .get_or_insert_with(AgentEngineConfig::default)
                .overlay(engine);
        }
        if let Some(authority) = scope.managed_authority {
            self.managed_authority = Some(authority);
        }
        // Last-wins, and only when the scope actually declares it — an absent
        // key must not reset a lower scope's `"on"` back to the default.
        //
        // This was missing entirely, which made `enable_recap` inert: every
        // scope parsed it, `overlay_scope` dropped it, and `recap_enabled()`
        // read `None` off the merged value no matter what any file said. The
        // accessor's own tests passed throughout because they call it on a
        // directly-deserialized `Settings`, never on a merged one — the merge
        // is the only place the field was lost.
        if let Some(recap) = scope.enable_recap {
            self.enable_recap = Some(recap);
        }
        // Appearance (`ui.theme`): whole-block last-wins — a higher-precedence
        // scope that declares `ui` replaces the lower one's. Personal
        // preference, no credential/egress authority, so no trust restoration.
        // (Like `enable_recap` above, this must be listed explicitly or the
        // merge silently drops it.)
        if let Some(ui) = &scope.ui {
            self.ui = Some(ui.clone());
        }
        // Adaptive-context config: whole-block last-wins (a higher-precedence
        // scope that declares `context` replaces a lower one's). Inert in
        // Phase 0 — nothing reads it — so no trust restoration is needed (it
        // carries no credential or egress authority).
        if let Some(context) = &scope.context {
            self.context = Some(context.clone());
        }
        // External CGP providers merge per-ENTRY (like `providers`), so a
        // project scope can enable an entry the user scope declared without
        // restating its transport — but a higher scope's entry replaces the
        // lower one's wholesale. Field-level merging here would let a project
        // file inherit a user's `egress_consent` while swapping the `url`,
        // silently reusing consent granted for a different endpoint.
        for (id, entry) in &scope.context_providers {
            self.context_providers.insert(id.clone(), entry.clone());
        }
    }

    fn merge_snapshots(scopes: &[&Settings]) -> Self {
        let mut merged = Self::default();
        for scope in scopes {
            merged.overlay_scope(scope);
        }
        merged
    }

    /// Resolve authority from three immutable snapshots captured by
    /// [`Settings::load`]. This function performs no I/O, so the bytes used to
    /// compute ceilings are the same bytes used to produce the merged config.
    pub(super) fn merge_captured_scopes(
        user: &Settings,
        managed: &Settings,
        project: &Settings,
        trust: ProjectTrust,
    ) -> Self {
        let trusted_only = Self::merge_snapshots(&[user, managed]);
        let mut merged = Self::merge_snapshots(&[user, managed, project]);
        // Captured from the managed snapshot only: the ceiling is what the ORG
        // said, and no later fold can grow it or shrink it. `AuthorityPolicy`
        // reads it rather than re-deriving, so the two can never disagree.
        let ceiling =
            managed_tool_ceiling(managed.managed_authority.as_ref(), managed.tools.as_ref());
        let authority = AuthorityPolicy::compute(
            managed.managed_authority.as_ref(),
            &ceiling,
            trust.credentials,
        );

        if !trust.hooks && project.hooks.is_some() {
            merged.hooks = trusted_only.hooks.clone();
        }
        // `context_providers` sits on the SAME code-execution boundary as
        // hooks and `.stella/mcp.toml`, and was missing from it. An `enabled`
        // stdio entry spawns its `command` at admission time (the conformance
        // suite runs on its own connection first, so the process starts before
        // anything has vetted it) — the identical `git clone && stella` RCE
        // `load_mcp_plan` gates against. An http entry is no safer: the same
        // untrusted file supplies the `egress_consent` that lets the query
        // payload, which carries workspace content, leave the machine. So an
        // untrusted project scope keeps whatever the user/managed scopes
        // declared and contributes nothing of its own.
        if !trust.hooks && !project.context_providers.is_empty() {
            merged.context_providers = trusted_only.context_providers.clone();
        }
        if !trust.credentials {
            for (id, project_entry) in &project.providers {
                let touches_credentials = project_entry.base_url.is_some()
                    || project_entry.api_key.is_some()
                    || project_entry.api_key_env.is_some();
                if !touches_credentials {
                    continue;
                }
                let trusted_entry = trusted_only.providers.get(id);
                if let Some(effective) = merged.providers.get_mut(id) {
                    effective.base_url = trusted_entry.and_then(|entry| entry.base_url.clone());
                    effective.api_key = trusted_entry.and_then(|entry| entry.api_key.clone());
                    effective.api_key_env =
                        trusted_entry.and_then(|entry| entry.api_key_env.clone());
                }
            }
            if project
                .mcp
                .as_ref()
                .and_then(|mcp| mcp.registry_url.as_ref())
                .is_some()
                && let Some(mcp) = merged.mcp.as_mut()
            {
                mcp.registry_url = trusted_only
                    .mcp
                    .as_ref()
                    .and_then(|trusted| trusted.registry_url.clone());
            }
            restore_project_tools(&mut merged, &trusted_only, project);
        }
        if !authority.project_prompts_allowed {
            restore_project_prompts(&mut merged, &trusted_only, project);
        }
        apply_tool_ceiling(&mut merged, &ceiling);
        merged.managed_authority = managed.managed_authority;
        merged.enterprise_telemetry = managed.enterprise_telemetry.clone();
        merged.authority_policy = authority;
        merged
    }

    /// Load and merge the standard scope chain for `workspace_root`.
    /// Missing files are the common case and skipped silently; an existing
    /// file that fails to parse is a hard error naming the file.
    ///
    /// **The project scope is a trust boundary.** A cloned repo's
    /// `.stella/settings.json` is untrusted input, and two kinds of entry in
    /// it can act on your behalf without you asking:
    ///
    /// - **Hooks** run arbitrary shell commands automatically.
    /// - **Credential routing** — a provider entry's `base_url`, `api_key`,
    ///   or `api_key_env`, and the `mcp.registry_url` — decides *where your
    ///   API key is sent* and *where server configs are fetched from*.
    ///   Overriding a built-in provider's `base_url` (or repointing its
    ///   `api_key_env` at another env var) silently exfiltrates the real
    ///   key to an attacker-controlled host on the first model call. That
    ///   violates the "outbound traffic only to the user-chosen provider"
    ///   invariant just as surely as a phone-home would.
    /// - **External context providers** — a `context_providers` entry spawns a
    ///   command (stdio) or opens an egress-consented connection (http) at
    ///   session start, and grants itself the consent that lets workspace
    ///   content leave the machine.
    ///
    /// The user and org-managed scopes always load. Project hooks and
    /// credential-routing fields load only when explicitly trusted; project
    /// tool switches and replacement prompts are likewise restored from the
    /// trusted scopes while untrusted. Managed denials remain ceilings even
    /// after explicit repository trust.
    pub fn load(workspace_root: &Path) -> Result<Self, String> {
        if filesystem_settings_disabled() {
            return Ok(Self::default());
        }

        let user = match user_settings_path() {
            Some(path) => Self::load_scope(&path)?,
            None => Self::default(),
        };
        let managed_path = managed_settings_path();
        let managed = Self::load_managed_scope(&managed_path)?;
        let project_path = project_settings_path(workspace_root);
        let project = Self::load_scope(&project_path)?;
        let trust = project_trust();
        let merged = Self::merge_captured_scopes(&user, &managed, &project, trust);

        // One launch loads the chain several times over — `Config::load`,
        // `settings_check::validate_at_launch`, `discover_configured_providers`
        // (itself reached from the catalog bootstrap), and every `/models`
        // render. Each pass re-derives the same verdict from the same file, so
        // printing the block every time turned one accurate notice into three
        // or four identical ones, which reads as a fault rather than a notice.
        // Latch it: the trust boundary is a property of the process, not of
        // the call.
        static ANNOUNCED: AtomicBool = AtomicBool::new(false);
        let announce = !ANNOUNCED.swap(true, Ordering::Relaxed);

        if announce && !trust.hooks && project.hooks.is_some() {
            eprintln!(
                "  ! project hooks in {} were NOT loaded — set STELLA_PROJECT_HOOKS=1 \
                 (or STELLA_TRUST_PROJECT=1) to trust this repo's hooks",
                project_path.display()
            );
        }

        if announce && !trust.hooks && !project.context_providers.is_empty() {
            eprintln!(
                "  ! project context providers in {} were NOT loaded — set \
                 STELLA_TRUST_PROJECT=1 to let this repo run its context sources \
                 (a stdio source runs a command on your machine; an http one can \
                 send workspace content off it)",
                project_path.display()
            );
        }

        if announce && !trust.credentials {
            let mut redacted: Vec<String> = Vec::new();
            for (id, pentry) in &project.providers {
                let touches_credentials = pentry.base_url.is_some()
                    || pentry.api_key.is_some()
                    || pentry.api_key_env.is_some();
                if !touches_credentials {
                    continue;
                }
                // `id` is attacker-controlled repo text — escape it so it
                // can't smuggle terminal control sequences into stderr.
                redacted.push(format!("providers.{}", id.escape_debug()));
            }

            let project_registry = project.mcp.as_ref().and_then(|m| m.registry_url.as_ref());
            if project_registry.is_some() {
                redacted.push("mcp.registry_url".to_string());
            }

            if !redacted.is_empty() {
                eprintln!(
                    "  ! credential-routing fields in {} were IGNORED ({}) — set \
                     STELLA_TRUST_PROJECT=1 to let this repo redirect where your API key \
                     is sent",
                    project_path.display(),
                    redacted.join(", "),
                );
            }
        }
        Ok(merged)
    }

    /// Read the same three scope files [`Settings::load`] reads, but keep
    /// their `tools` sections apart — see [`ToolScopePolicies`] for why the
    /// merged answer is not enough.
    ///
    /// Cheap local reads, and deliberately re-read on every call rather than
    /// cached: the editor's whole job is to change these files, and a stale
    /// snapshot would attribute a switch to the scope that used to carry it.
    pub fn load_tool_scopes(workspace_root: &Path) -> Result<ToolScopePolicies, String> {
        if filesystem_settings_disabled() {
            return Ok(ToolScopePolicies::default());
        }
        let user = match user_settings_path() {
            Some(path) => Self::load_scope(&path)?,
            None => Self::default(),
        };
        let managed = Self::load_managed_scope(&managed_settings_path())?;
        let project = Self::load_scope(&project_settings_path(workspace_root))?;
        let own = |scope: &Settings| {
            scope
                .tools
                .as_ref()
                .map(ToolsSettings::policy)
                .unwrap_or_default()
        };
        Ok(ToolScopePolicies {
            managed: managed_tool_ceiling(
                managed.managed_authority.as_ref(),
                managed.tools.as_ref(),
            ),
            user: own(&user),
            project: own(&project),
        })
    }

    /// Merge the files at `paths`, later paths taking precedence. Split out
    /// from [`Settings::load`] so tests can drive the merge over fixtures
    /// without touching `$HOME` or `/etc`.
    #[cfg(test)]
    pub fn load_from(paths: &[PathBuf]) -> Result<Self, String> {
        let mut merged = Settings::default();
        for path in paths {
            let scope = Self::load_scope(path)?;
            merged.overlay_scope(&scope);
        }
        Ok(merged)
    }
}
