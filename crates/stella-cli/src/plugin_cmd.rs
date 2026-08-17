// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella plugin install|list|remove` — the loader
//! (`doc:pipeline-as-plugins` §A4).
//!
//! Three verbs, one for each thing a human does with a plugin, and each one
//! doing exactly that thing (invariant 9 — no `install --remove`).
//!
//! # Install is a consent transaction, not a copy
//!
//! Nothing is copied until a human has read what the plugin declared and said
//! yes. The words are [`stella_plugin::consent_text`]'s, deliberately: the
//! same bytes a `stella-serve` host or an embedded host would show, rendered
//! by the crate that owns the declaration, so a future second install surface
//! cannot describe the same manifest differently. This module renders nothing
//! of its own about the grant.
//!
//! With no human present (a pipe, a CI job, `--output-format json`), install
//! **refuses** unless `--yes` was passed. Defaulting to yes would make
//! "installed silently by a script" the normal path for the most dangerous
//! thing a user can add to their machine; defaulting to no with an explicit
//! override keeps the automated case possible and deliberate.
//!
//! # Uninstall actually uninstalls
//!
//! Removing a plugin deletes its directory, and the roster is recomputed from
//! disk on every load — so there is no second place a stale grant could
//! survive. See [`roster`]'s module docs for why plugin hooks are derived
//! rather than written into a settings file's `hooks` block, which is the
//! shape that makes an uninstall unable to finish.

use std::path::{Path, PathBuf};

use crate::settings::{Settings, Toggle};

pub(crate) mod process;
pub(crate) mod roster;

use roster::{MANIFEST_FILE, PluginRoster, PluginScope};

/// `stella plugin <cmd>`.
#[derive(Debug, clap::Subcommand)]
pub enum PluginCmd {
    /// Install a plugin from a local directory, after showing what it asks
    /// for.
    Install {
        /// The directory holding the plugin's `plugin.toml`.
        #[arg(value_name = "DIR")]
        dir: PathBuf,
        /// Install for this workspace only (`.stella/plugins`, the default)
        /// or for every workspace (`~/.stella/plugins`).
        #[arg(long, value_enum, default_value_t = ScopeArg::Project)]
        scope: ScopeArg,
        /// Accept the declared grant without prompting. Required when no
        /// human is present to answer.
        #[arg(long)]
        yes: bool,
    },
    /// List installed plugins, what each is allowed to do, and where it came
    /// from.
    List,
    /// Remove an installed plugin, by manifest name.
    Remove {
        /// The plugin's `name`, as `stella plugin list` prints it.
        #[arg(value_name = "NAME")]
        name: String,
    },
}

/// The tier `--scope` selects. Mirrors [`PluginScope`], which is not a clap
/// type because the roster must not depend on the CLI's argument vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ScopeArg {
    /// `<workspace>/.stella/plugins`.
    Project,
    /// `~/.stella/plugins`.
    User,
}

impl From<ScopeArg> for PluginScope {
    fn from(arg: ScopeArg) -> Self {
        match arg {
            ScopeArg::Project => PluginScope::Project,
            ScopeArg::User => PluginScope::User,
        }
    }
}

/// Run `stella plugin <cmd>`. Offline: local files only, no API key.
pub fn run_plugin(cmd: &PluginCmd) -> Result<(), String> {
    let root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let settings = Settings::load(&root).unwrap_or_default();
    match cmd {
        PluginCmd::Install { dir, scope, yes } => {
            install(&root, dir, (*scope).into(), *yes, &settings)
        }
        PluginCmd::List => list(&root, &settings),
        PluginCmd::Remove { name } => remove(&root, name),
    }
}

/// The directory a tier installs into, or a reason it cannot be resolved.
fn tier_dir(workspace_root: &Path, scope: PluginScope) -> Result<PathBuf, String> {
    match scope {
        PluginScope::Project => Ok(stella_home::resolve_project_plugins_dir(workspace_root)),
        PluginScope::User => stella_home::resolve_user_plugins_dir(crate::paths::stella_root())
            .ok_or_else(|| {
                "no home directory is discoverable, so there is no user scope to install into \
                 — use --scope project"
                    .to_string()
            }),
    }
}

/// A manifest `name` is used as a directory name, so it is checked as one.
///
/// The name is third-party text: `../../.ssh` would have `install` write
/// outside the plugins directory and `remove` delete outside it. Rejected
/// rather than sanitized — a plugin whose name needs rewriting to be safe
/// should be told, not quietly renamed into something its own manifest does
/// not say.
fn checked_name(name: &str) -> Result<&str, String> {
    let bad = name.is_empty()
        || name == "."
        || name == ".."
        || name.starts_with('.')
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.trim() != name;
    if bad {
        return Err(format!(
            "`{}` is not usable as a plugin directory name: a plugin name must be a plain \
             file name — no path separators, no leading dot, no surrounding whitespace",
            name.escape_debug()
        ));
    }
    Ok(name)
}

fn install(
    workspace_root: &Path,
    source: &Path,
    scope: PluginScope,
    yes: bool,
    settings: &Settings,
) -> Result<(), String> {
    let manifest = roster::load_manifest(source)?.ok_or_else(|| {
        format!(
            "{} holds no {MANIFEST_FILE}, so it is not a plugin",
            source.display()
        )
    })?;
    let name = checked_name(&manifest.name)?;

    let destination = tier_dir(workspace_root, scope)?.join(name);
    if destination.exists() {
        return Err(format!(
            "`{name}` is already installed at {} — run `stella plugin remove {name}` first",
            destination.display()
        ));
    }

    println!("{}", stella_plugin::consent_text(&manifest));

    // The consent text lists the environment allowlist as the manifest wrote
    // it, because that crate has no credential vocabulary and must not grow
    // one. The host does, and it refuses part of that list — so the correction
    // is printed here, beside the claim it corrects. Left unsaid, the prompt
    // would describe a program that is not the one about to run.
    if let Some(runtime) = &manifest.runtime {
        let refused = process::refused_credentials(&runtime.env);
        if !refused.is_empty() {
            println!(
                "\nCorrection: `{name}` asks to inherit {} — it will NOT get {}. A plugin \
                 never receives the credential that pays for the agent; it names a [roles] \
                 tier and the host makes the call.",
                refused.join(", "),
                if refused.len() == 1 { "it" } else { "them" }
            );
        }
    }

    // Shadowing is legitimate — pinning a workspace to a different build is
    // the reason project scope exists — but it is invisible unless said, and
    // a user who thinks they will now have two copies running has the wrong
    // model of what they are consenting to. Printed with the declaration, not
    // after the answer.
    let (before, _) = PluginRoster::load(workspace_root, settings);
    if let Some(existing) = before.get(name) {
        println!(
            "\nNote: `{name}` is already installed ({}) at {} — this {} copy will shadow it whole.",
            existing.scope.as_str(),
            existing.dir.display(),
            scope.as_str()
        );
    }

    if !yes {
        // `true` for the first input: this is a plain text command, so the
        // only questions are whether stdio is a terminal.
        if !crate::interactive::human_is_present(true) {
            return Err(
                "nothing here can ask you to accept that grant — no terminal is attached. \
                 Re-run with --yes if you have read the declaration above and accept it."
                    .to_string(),
            );
        }
        if !confirm()? {
            println!("not installed.");
            return Ok(());
        }
    }

    copy_tree(source, &destination)?;
    println!(
        "installed `{name}` ({}) into {}",
        scope.as_str(),
        destination.display()
    );
    if matches!(settings.plugins.get(name), Some(Toggle::Off)) {
        println!(
            "  ! `plugins.{name}` is set to \"off\" in your settings, so it will not run. \
             Delete that line to let it start."
        );
    }
    Ok(())
}

/// Ask once, on stdout, and accept only an explicit yes.
fn confirm() -> Result<bool, String> {
    use std::io::{BufRead as _, Write as _};

    print!("\nInstall it? [y/N] ");
    std::io::stdout()
        .flush()
        .map_err(|e| format!("cannot write the prompt: {e}"))?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("cannot read your answer: {e}"))?;
    let answer = line.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}

fn list(workspace_root: &Path, settings: &Settings) -> Result<(), String> {
    let (roster, notices) = PluginRoster::load(workspace_root, settings);
    for notice in &notices {
        eprintln!("{notice}");
    }

    if roster.plugins().is_empty() {
        println!("no plugins installed — add one with `stella plugin install <dir>`");
    }
    for plugin in roster.plugins() {
        let grant = &plugin.manifest.loop_grant;
        println!(
            "{:<24} {:<8} {}",
            plugin.manifest.name,
            plugin.scope.as_str(),
            grant.participation
        );
        if let Some(description) = &plugin.manifest.description {
            println!("  {description}");
        }
        println!("  {}", plugin.dir.display());
        match &plugin.manifest.runtime {
            Some(runtime) => println!(
                "  runs: {} ({}s, env: {})",
                runtime.argv.join(" "),
                runtime.timeout_secs,
                if runtime.env.is_empty() {
                    "none".to_string()
                } else {
                    runtime.env.join(", ")
                }
            ),
            None => println!("  runs: no [runtime] — this plugin ships no process"),
        }
    }

    // Routes, not grants: what a host would actually dispatch. A plugin whose
    // grant produces no route (no `[runtime]`) is visible above and absent
    // here, which is the distinction that answers "I declared Stop and
    // nothing happens".
    let routes = roster.hook_routes();
    if !routes.is_empty() {
        println!("\nhook dispatches:");
        for route in &routes {
            println!(
                "  {:<24} {} -> {}",
                route.plugin,
                route.event,
                route.argv.join(" ")
            );
        }
    }

    // What the child would actually be handed, asked of the same builder that
    // will hand it over. A declared name that is refused as a model credential
    // is reported here rather than discovered as a missing variable at the
    // first dispatch — the plugin author's fix is to ask the host for a role,
    // and they can only take it if they are told.
    let mut warned: Vec<&str> = Vec::new();
    for route in &routes {
        if warned.contains(&route.plugin.as_str()) {
            continue;
        }
        let prepared = process::prepare_command(route, |name| std::env::var(name).ok());
        if !prepared.refused.is_empty() {
            warned.push(&route.plugin);
            println!(
                "\n  ! `{}` asks to inherit {} — refused: a plugin never receives the \
                 credential that pays for the agent. Declare a [roles] tier and let the \
                 host make the call.",
                route.plugin,
                prepared.refused.join(", ")
            );
        }
    }

    // Retractions are shown even though the roster has already dropped them:
    // "installed but switched off" is exactly the state a user needs told,
    // and silence about it looks identical to "not installed".
    for (name, toggle) in &settings.plugins {
        if matches!(toggle, Toggle::Off) {
            println!("\n`{name}` is retracted by `plugins.{name} = \"off\"` and will not run");
        }
    }
    Ok(())
}

fn remove(workspace_root: &Path, name: &str) -> Result<(), String> {
    let name = checked_name(name)?;
    // Project first: it is the tier that shadows, so it is the one a user in
    // a workspace means by an unqualified name.
    for scope in [PluginScope::Project, PluginScope::User] {
        let Ok(tier) = tier_dir(workspace_root, scope) else {
            continue;
        };
        let dir = tier.join(name);
        if !dir.is_dir() {
            continue;
        }
        std::fs::remove_dir_all(&dir)
            .map_err(|e| format!("cannot remove {}: {e}", dir.display()))?;
        println!(
            "removed `{name}` ({}) from {}",
            scope.as_str(),
            dir.display()
        );
        return Ok(());
    }
    Err(format!(
        "`{name}` is not installed in either scope — `stella plugin list` shows what is"
    ))
}

/// Copy a plugin package into place.
///
/// **Symlinks are refused, not followed.** A package is third-party content
/// and a symlink in it is a request to copy something the author does not
/// ship — `~/.ssh`, `/etc`, or a cycle. Refusing names the offending entry so
/// the author can fix the package; following would make install a file-read
/// primitive pointed at whatever the link says.
fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(source)
        .map_err(|e| format!("cannot read {}: {e}", source.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!(
            "{} is a symlink; a plugin package must contain only real files",
            source.display()
        ));
    }
    std::fs::create_dir_all(destination)
        .map_err(|e| format!("cannot create {}: {e}", destination.display()))?;
    let entries =
        std::fs::read_dir(source).map_err(|e| format!("cannot read {}: {e}", source.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", source.display()))?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let kind = entry
            .file_type()
            .map_err(|e| format!("cannot inspect {}: {e}", from.display()))?;
        if kind.is_symlink() {
            return Err(format!(
                "{} is a symlink; a plugin package must contain only real files",
                from.display()
            ));
        }
        if kind.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .map_err(|e| format!("cannot copy {} to {}: {e}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
