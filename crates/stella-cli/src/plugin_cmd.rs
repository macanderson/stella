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

pub(crate) mod configure;
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

    // What the package *ships* — tools, skills, records — is declared in the
    // manifest and rendered by `consent_text` with everything else (#3565).
    // The host's job is the other half of that: check its own read of the
    // directories against the declaration and refuse any disagreement, so the
    // document below is provably complete. A `tools/` entry no `[[tools]]`
    // names is executable code entering the model's surface that nobody
    // consented to; a declaration with nothing behind it teaches a reader that
    // the document is decorative. Both are refusals, and both come before
    // anything is printed as though it were the truth.
    manifest
        .reconcile(&package::Inventory::of_package(source).listing())
        .map_err(|mismatch| {
            format!("`{name}` cannot be installed: {mismatch}\n\nNothing was copied.")
        })?;

    // Where a `[[configure]]` table would land, resolved BEFORE the consent
    // document is printed. A tier this package cannot configure is a refusal,
    // and a refusal has to come before the words that describe the change as
    // though it were going to happen — the `reconcile` rule above, applied to
    // the other half of the document.
    let config_target = if manifest.configure.is_empty() {
        None
    } else {
        Some(
            configure::ConfigTarget::resolve(workspace_root, scope).map_err(|reason| {
                format!("`{name}` cannot be installed: {reason}\n\nNothing was copied.")
            })?,
        )
    };

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

    stage_and_commit(source, &tier, &destination)?;

    // The configuration write (#3999). Last, because it is the only step that
    // reaches outside the tier — and rolled back whole on failure, on
    // `stage_and_commit`'s reasoning: a package installed with its declared
    // configuration half-applied is a state neither `list` nor `remove` can
    // reason about, and it would sit under a name that is now taken.
    //
    // The order inside is load-bearing. The journal is written BEFORE the
    // config, so there is no instant in which the workspace is configured and
    // nothing on disk knows how to undo it; and the config write is what can
    // fail, which is why it is the step with an undo behind it rather than the
    // one with a rollback in front.
    if let Some(target) = &config_target
        && let Err(reason) = install_configuration(&manifest, target, &destination)
    {
        discard(&destination);
        return Err(format!(
            "`{name}` could not configure {}: {reason}\n\nIt was not installed.",
            target.path().display()
        ));
    }

    println!(
        "installed `{name}` ({}) into {}",
        scope.as_str(),
        destination.display()
    );
    if let Some(target) = &config_target {
        println!(
            "  set {} value(s) in {} — `stella plugin remove {name}` puts them back",
            manifest.configure.len(),
            target.path().display()
        );
    }
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

/// Apply a package's `[[configure]]` table, leaving nothing half-done (#3999).
///
/// Three steps in one place because their order is the correctness argument:
/// plan (reads, writes nothing), journal (durable undo), commit (the change).
/// A failure at the last step undoes itself from the plan it already holds,
/// rather than trusting that the file it just failed to write is readable.
fn install_configuration(
    manifest: &stella_plugin::PluginManifest,
    target: &configure::ConfigTarget,
    destination: &Path,
) -> Result<(), String> {
    let plan = configure::plan(target, manifest)?;
    configure::write_journal(destination, &plan.journal)?;
    if let Err(reason) = plan.commit() {
        configure::undo(&plan.journal, manifest);
        return Err(reason);
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
/// # Why one tier's failure does not abort the other
///
/// For the same reason it does not *stop* at the first tier. A `?` on the
/// per-directory delete returned before the user tier was read, so a project
/// copy that would not delete — a permission error on `remove_dir_all`, a
/// path the [`remove_plugin_dir`] guard refuses — left the user copy on disk
/// and wired into every tool call, which is precisely the outcome the
/// paragraph above says this function exists to prevent (#4302). Failures are
/// collected and reported at the end, naming every directory, while the
/// copies that did go are still removed and still reported.
fn remove(workspace_root: &Path, name: &str) -> Result<(), String> {
    let name = checked_name(name)?;
    let mut removed = 0usize;
    let mut failures: Vec<String> = Vec::new();
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
            // Before the directory goes, because the journal that says what to
            // put back lives inside it (#3999). An uninstall that deleted first
            // could not restore at all — and the manifest, which is what the
            // keys are taken from, would be gone with it.
            // The file the writes are allowed to land in is re-derived from
            // the tier being removed, not read from the journal: the journal
            // is package-controlled data, so trusting its `config` path would
            // make `remove` an arbitrary-file write. A tier no longer
            // resolvable to a `stella.toml` yields `None`, which `revert`
            // treats as "nothing can be put back" rather than redirected.
            let expected = configure::ConfigTarget::resolve(workspace_root, scope).ok();
            let reverted = installs
                .iter()
                .find(|plugin| plugin.dir == dir)
                .map(|plugin| {
                    configure::revert(
                        &dir,
                        &plugin.manifest,
                        expected.as_ref().map(configure::ConfigTarget::path),
                    )
                })
                .unwrap_or_default();

            // Collected rather than propagated: the next directory, and the
            // next tier, are still worth removing. The revert above has
            // already run for this one — the journal it reads lives inside
            // the directory (#3999), so it cannot be ordered after the
            // delete — which leaves a package whose keys are out of force
            // and whose files are still there. That is why the failure is an
            // error at the end rather than a notice: it needs a hand.
            if let Err(error) = remove_plugin_dir(&tier, &dir) {
                failures.push(error);
                continue;
            }
            println!(
                "removed `{name}` ({}) from {}",
                scope.as_str(),
                dir.display()
            );
            if !reverted.keys.is_empty()
                && let Some(config) = &reverted.config
            {
                println!(
                    "  put back {} in {}: {}",
                    reverted.keys.len(),
                    config.display(),
                    reverted.keys.join(", ")
                );
            }
            // The honest half. A `remove` that reported success while leaving a
            // package's configuration in force is the failure this whole
            // mechanism exists to prevent, so it is said out loud rather than
            // swallowed — with the keys named, because the remedy is to edit
            // them by hand and a user cannot do that without knowing which.
            if !reverted.unaccounted.is_empty() {
                eprintln!(
                    "  ! `{name}` set {} that could NOT be put back — edit them by hand: {}",
                    if reverted.unaccounted.len() == 1 {
                        "1 value"
                    } else {
                        "several values"
                    },
                    reverted.unaccounted.join(", ")
                );
            }
            removed += 1;
        }
    }
    // Before the not-installed check, and an error even when another tier's
    // copy did go: a copy still on disk is still in the roster and still
    // dispatched on every tool call, so a `remove` that exited 0 here would
    // tell the user a third party's process was gone while it was running.
    if !failures.is_empty() {
        return Err(format!(
            "`{name}`: {removed} {} removed, but {} could not be:\n  {}",
            if removed == 1 { "copy" } else { "copies" },
            if failures.len() == 1 {
                "one"
            } else {
                "several"
            },
            failures.join("\n  ")
        ));
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
/// when it is a real directory, which is how a package whose manifest no
/// longer parses is still reachable.
///
/// `symlink_metadata`, not `Path::is_dir`, which follows the link: a symlinked
/// tier entry is not a plugin — [`roster::read_tier`] skips it and says so —
/// so `remove` must not collect one either (#3530). Collected, it reached
/// [`remove_plugin_dir`]'s refusal and turned an unremarkable "not installed"
/// into a hard error about a package this CLI never installed.
fn removable_dirs(tier: &Path, name: &str, installs: &[roster::InstalledPlugin]) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = installs
        .iter()
        .filter(|plugin| plugin.manifest.name == name)
        .map(|plugin| plugin.dir.clone())
        .collect();
    let by_path = tier.join(name);
    let is_real_dir =
        std::fs::symlink_metadata(&by_path).is_ok_and(|meta| meta.file_type().is_dir());
    if is_real_dir && !dirs.contains(&by_path) {
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
