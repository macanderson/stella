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
//! Removing a plugin deletes its directory — in **every** tier that holds the
//! name, resolved by manifest name rather than by directory name, because
//! those are the two ways a `remove` that returned `Ok` still left the plugin
//! dispatching (see [`remove`]). The roster is recomputed from disk on every
//! load, so there is no second place a stale grant could survive. See
//! [`roster`]'s module docs for why plugin hooks are derived rather than
//! written into a settings file's `hooks` block, which is the shape that makes
//! an uninstall unable to finish.

use std::path::{Path, PathBuf};

use crate::settings::{Settings, Toggle};

pub(crate) mod package;
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

    let tier = tier_dir(workspace_root, scope)?;
    let destination = tier.join(name);
    // Two ways this tier can already hold the name, and they are not the same
    // question. The claim is the one that matters — a package installed in a
    // directory of another name is what `list` shows and what a host routes —
    // and the path is what the copy below would collide with.
    let mut notices = Vec::new();
    let claimed = roster::read_tier(&tier, scope, &mut notices)
        .into_iter()
        .find(|plugin| plugin.manifest.name == manifest.name)
        .map(|plugin| plugin.dir);
    for notice in &notices {
        eprintln!("{notice}");
    }
    if let Some(dir) = claimed.filter(|dir| *dir != destination) {
        return Err(format!(
            "`{name}` is already installed at {} (a directory of another name declares it) — \
             run `stella plugin remove {name}` first",
            dir.display()
        ));
    }
    if destination.exists() {
        return Err(format!(
            "`{name}` is already installed at {} — run `stella plugin remove {name}` first",
            destination.display()
        ));
    }

    println!("{}", stella_plugin::consent_text(&manifest));

    // What the package *ships* — tools, skills, records — read off the source
    // directory, because only the host can see one (`package::Inventory::
    // consent_addendum` argues the seam). Printed inside the same transaction
    // and above the same single y/N: a contributed tool is executable code
    // entering the model's surface, and consenting to it after the fact is not
    // consenting to it.
    let inventory = package::Inventory::of_package(source);
    if let Some(addendum) = inventory.consent_addendum(name) {
        println!("\n{addendum}");
    }

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

    stage_and_commit(source, &tier, &destination)?;
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
    // Installing into an untrusted workspace is legitimate — the copy is the
    // operator's own act — but the loader will refuse to read the tier back
    // (`roster::read_project_tier`), so the plugin sits on disk doing nothing.
    // Said here rather than left for the user to discover: the whole point of
    // gating the tier is defeated if "installed and inert" looks identical to
    // "installed and running".
    if scope == PluginScope::Project && !crate::settings::project_code_execution_trusted() {
        println!(
            "  ! this workspace is not trusted to run code, so `{name}` will not load. \
             Set STELLA_TRUST_PROJECT=1 to let this repo's plugins run."
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
    for (plugin, inventory) in package::inventories(&roster) {
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
        // What it contributes, not merely what it declares: a user asking
        // "where did this tool come from?" is asking this listing, and a
        // package's tools/skills/records are otherwise attributable only by
        // reading a source path out of `stella tools`.
        for (label, named) in [
            ("tools", &inventory.tools),
            ("skills", &inventory.skills),
            ("records", &inventory.records),
        ] {
            if !named.is_empty() {
                println!("  ships {label}: {}", named.join(", "));
            }
        }
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

    // What the child will actually be handed, asked of the same judgement the
    // socket enforces with. A declared name that is refused as a model
    // credential is reported here rather than discovered as a missing variable
    // at the first dispatch — the plugin author's fix is to ask the host for a
    // role, and they can only take it if they are told.
    let mut warned: Vec<&str> = Vec::new();
    for route in &routes {
        if warned.contains(&route.plugin.as_str()) {
            continue;
        }
        let refused = process::refused_credentials(&route.env_allowlist);
        if !refused.is_empty() {
            warned.push(&route.plugin);
            println!(
                "\n  ! `{}` asks to inherit {} — refused: a plugin never receives the \
                 credential that pays for the agent. Declare a [roles] tier and let the \
                 host make the call.",
                route.plugin,
                refused.join(", ")
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

/// Uninstall `name` from **every** tier that holds it.
///
/// # Why it does not stop at the first tier
///
/// The same name can be installed in both, and that is the ordinary case:
/// project scope exists so a workspace can pin a different build of a plugin
/// the operator installed globally. A `remove` that deleted the project copy,
/// printed "removed", and returned `Ok` left the user-tier copy on disk, in
/// the roster, and dispatched by [`PluginRoster::hook_routes`] on every tool
/// call — the user told a third party's process was gone while it was still
/// wired into the loop. Uninstall is the one operation whose failure is a
/// security failure (see this module's header), so it removes every copy and
/// names each one it removed.
///
/// # Why the manifest name, not the directory name
///
/// The manifest's `name` is the identity everywhere else — the principal, the
/// `plugins.<name>` switch, what `list` prints, what a route carries — so a
/// directory `pkg/` whose manifest says `vera` is listed and routed as `vera`.
/// Keyed on the directory name, `remove vera` found nothing and said `vera`
/// was not installed while `list` was showing it: an unremovable plugin. The
/// literal `<tier>/<name>` is still swept up as well, so a package whose
/// manifest has stopped parsing does not become unremovable in turn.
fn remove(workspace_root: &Path, name: &str) -> Result<(), String> {
    let name = checked_name(name)?;
    let mut removed = 0usize;
    // Project first: it is the tier that shadows, so it is the one a user in
    // a workspace means by an unqualified name — and so the order the copies
    // are reported in matches the precedence they had.
    for scope in [PluginScope::Project, PluginScope::User] {
        let Ok(tier) = tier_dir(workspace_root, scope) else {
            continue;
        };
        let mut notices = Vec::new();
        let installs = roster::read_tier(&tier, scope, &mut notices);
        for notice in &notices {
            eprintln!("{notice}");
        }
        for dir in removable_dirs(&tier, name, &installs) {
            remove_plugin_dir(&tier, &dir)?;
            println!(
                "removed `{name}` ({}) from {}",
                scope.as_str(),
                dir.display()
            );
            removed += 1;
        }
    }
    if removed == 0 {
        return Err(format!(
            "`{name}` is not installed in either scope — `stella plugin list` shows what is"
        ));
    }
    Ok(())
}

/// Every directory in `tier` that `remove <name>` must delete, in a
/// deterministic order.
///
/// Both keys, deduplicated: every install whose *manifest* declares the name
/// (there can be more than one — see
/// [`roster::read_tier`]'s collision notice, where only the last is in force
/// and the others are installed and inert), plus the literal `<tier>/<name>`
/// when it exists, which is how a package whose manifest no longer parses is
/// still reachable.
fn removable_dirs(tier: &Path, name: &str, installs: &[roster::InstalledPlugin]) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = installs
        .iter()
        .filter(|plugin| plugin.manifest.name == name)
        .map(|plugin| plugin.dir.clone())
        .collect();
    let by_path = tier.join(name);
    if by_path.is_dir() && !dirs.contains(&by_path) {
        dirs.push(by_path);
    }
    dirs
}

/// Delete one installed package, after proving it is one.
///
/// Two checks, both about the same hazard: `remove` deletes a tree, so the
/// path it deletes must be a real directory this tier owns. A symlink is
/// refused rather than followed — the tier is a directory third-party content
/// lands in, and `remove_dir_all` down a link would delete whatever it names.
fn remove_plugin_dir(tier: &Path, dir: &Path) -> Result<(), String> {
    if dir.parent() != Some(tier) {
        return Err(format!(
            "refusing to remove {}: it is not a direct child of {}",
            dir.display(),
            tier.display()
        ));
    }
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!(
            "{} is a symlink, not an installed package — delete it by hand if you meant to",
            dir.display()
        ));
    }
    std::fs::remove_dir_all(dir).map_err(|e| format!("cannot remove {}: {e}", dir.display()))
}

/// Copy a package into the tier, then make it visible in one step.
///
/// # Why install is not `copy_tree` into the final directory
///
/// [`copy_tree`] creates its destination first and copies in `read_dir` order,
/// so a failure part-way — a symlink in the package, a full disk, a permission
/// error on the third file — left `<tier>/<name>` behind holding whatever had
/// been copied so far. `plugin.toml` sorts early and is small, so the usual
/// residue was a directory the roster loads and routes, with an `argv` naming
/// a file that was never copied: a live hook dispatch into nothing. Worse, the
/// name was then taken, so every later attempt to install it was refused as
/// "already installed" and the only repair was deleting the directory by hand.
///
/// Staging into a sibling and `rename`-ing makes the failed install leave
/// nothing: the plugin appears under its name complete or not at all. The
/// staging directory is inside the tier so the rename is within one
/// filesystem, which is what makes it atomic.
fn stage_and_commit(source: &Path, tier: &Path, destination: &Path) -> Result<(), String> {
    std::fs::create_dir_all(tier).map_err(|e| format!("cannot create {}: {e}", tier.display()))?;
    let staging = tier.join(staging_name());
    if let Err(reason) = copy_tree(source, &staging) {
        discard(&staging);
        return Err(reason);
    }
    if let Err(error) = std::fs::rename(&staging, destination) {
        discard(&staging);
        return Err(format!(
            "cannot move the staged copy into {}: {error}",
            destination.display()
        ));
    }
    Ok(())
}

/// A name for the staging directory that no plugin can have and no concurrent
/// install can collide with.
///
/// The leading dot is load-bearing twice: [`checked_name`] refuses it as a
/// plugin name, and [`roster::read_tier`] skips it — so a tree that is
/// mid-copy is never loaded, listed or routed. The pid and counter are what
/// make the exists-check above meaningful under concurrency: two installs of
/// different plugins into one tier stage into different directories and each
/// renames its own.
fn staging_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!(
        ".staging-{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Drop a staging tree, best effort.
///
/// The error that brought us here is the one the caller reports; a second
/// failure removing the scratch copy would replace a diagnosis with a
/// housekeeping complaint. What matters is that the tree is not under a
/// plugin's name, so even a leaked one is inert.
fn discard(staging: &Path) {
    let _ = std::fs::remove_dir_all(staging);
}

/// Copy a plugin package into a directory of our choosing — the staging tree
/// [`stage_and_commit`] renames into place, never the installed name itself.
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
