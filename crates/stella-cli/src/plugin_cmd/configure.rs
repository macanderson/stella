// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Applying and reverting a package's `[[configure]]` table (#3999) — the host
//! half of [`stella_plugin::ConfigureEntry`].
//!
//! `stella-plugin` decides whether a declaration is one a human could be asked
//! to accept; it performs no I/O, so it cannot write one. This module is the
//! other side: it writes the declared keys into the tier's `stella.toml` after
//! consent, records what each key held first, and puts exactly that back when
//! the package is removed.
//!
//! # The revert is the feature, so it is stored, not recomputed
//!
//! An uninstall that does not restore leaves a workspace configured in the name
//! of a plugin it no longer has — signing its commits, in the motivating case.
//! Recomputing "what it must have been" is not possible: a default is not the
//! same as what the file held, and the file may have held nothing at all.
//!
//! So install writes a **journal** beside the installed package
//! ([`REVERT_FILE`]) holding, per key, the prior value as its literal TOML text
//! and the ancestor table that did not exist before. Removal parses it and
//! reverses each write. The journal lives inside the package directory because
//! that scopes its lifetime exactly: it exists while the install does, it is
//! deleted with it, and a second install of the same package into the same tier
//! cannot inherit a stale one.
//!
//! **A journal is never trusted on its own.** It is third-party-adjacent data
//! sitting in a directory whose contents arrived from a package, so a planted
//! one would otherwise be a primitive for editing arbitrary config at
//! `stella plugin remove` time. [`revert`] therefore takes the keys from the
//! **manifest** and consults the journal only for the prior values of those
//! keys — so the worst a planted journal can do is get a value wrong on a key
//! the human already consented to.
//!
//! # `stella.toml` only, and a refusal rather than a fallback
//!
//! A tier still on `settings.json` is refused ([`ConfigTarget::resolve`]).
//! Writing the JSON instead would mean a second writer for the same setting,
//! and *creating* a `stella.toml` beside an existing `settings.json` is worse
//! than either: the TOML file takes precedence for the whole tier, so a
//! one-key write would silently disable every setting in the JSON.
//!
//! # What the human read is what gets written
//!
//! The value is rendered by [`stella_plugin::ConfigureEntry::rendered_value`]
//! — the same call the consent document printed — and that text is parsed into
//! the document. There is no second serializer that could disagree with the one
//! a user was shown.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use stella_plugin::{ConfigureEntry, PluginManifest};
use toml_edit::{DocumentMut, Item};

use super::roster::PluginScope;

/// The journal an install leaves beside the package so removal can reverse it.
///
/// A leading dot for [`super::staging_name`]'s reason: `roster::read_tier`
/// skips dot-entries, so this file can never be mistaken for a plugin, and
/// `checked_name` would refuse it as a plugin name.
pub(crate) const REVERT_FILE: &str = ".stella-configured.json";

/// One key's prior state — everything needed to undo one write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PriorValue {
    /// The dotted key, as the manifest declared it.
    pub(crate) key: String,
    /// What the key held before, as its literal TOML text (`"old"`, `7`,
    /// `["a"]`), or `None` when the key was absent.
    ///
    /// Text rather than a decoded value so the journal is symmetric with the
    /// write path and readable by a human debugging one: both directions parse
    /// the same TOML fragment, so no conversion between two value models can
    /// lose a type.
    pub(crate) value: Option<String>,
    /// The shallowest ancestor table that did not exist before this write, as a
    /// dotted path — removed wholesale on revert.
    ///
    /// Without it a revert leaves the headers behind: writing
    /// `self_driving.attribution.signature` into a file with no `[self_driving]`
    /// creates two tables, and deleting only the leaf leaves an empty
    /// `[self_driving.attribution]` that the workspace never had. Recorded
    /// rather than inferred, because "this table is empty now" does not
    /// distinguish one we created from one the user left empty on purpose.
    pub(crate) created: Option<String>,
}

/// The journal file's whole contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConfigJournal {
    /// The manifest name that wrote it — checked on revert, so a journal that
    /// somehow belongs to another package is ignored rather than replayed.
    pub(crate) plugin: String,
    /// The config file the writes landed in.
    pub(crate) config: PathBuf,
    /// One entry per declared key, in manifest order.
    pub(crate) prior: Vec<PriorValue>,
}

/// Where a tier's configuration is written, once it is known to be writable.
#[derive(Debug)]
pub(crate) struct ConfigTarget {
    path: PathBuf,
    /// Whether the write must be owner-only (the user tier, which can hold
    /// credentials) rather than the committable project-scope mode. Carried on
    /// the target because [`ConfigPlan::commit`] is the only place that knows
    /// the bytes and the path, but not the scope they came from.
    user_private: bool,
}

impl ConfigTarget {
    /// The `stella.toml` for `scope`, or the reason a package may not configure
    /// this tier.
    ///
    /// Refuses a tier that is still on `settings.json` rather than falling back
    /// to it — see this module's header for why creating a TOML file beside a
    /// JSON one is the worst of the three options.
    pub(crate) fn resolve(workspace_root: &Path, scope: PluginScope) -> Result<Self, String> {
        // The user tier can hold credentials, so its file is owner-only in an
        // owner-only directory; the project tier is a committable, reviewed
        // file that keeps the ordinary mode — see [`ConfigPlan::commit`].
        let user_private = scope == PluginScope::User;
        let (path, json, remedy) = match scope {
            PluginScope::Project => (
                crate::settings::toml_config::project_toml_path(workspace_root),
                crate::settings::project_settings_path(workspace_root),
                "run `stella migrate config` in this workspace first",
            ),
            PluginScope::User => {
                let root = crate::paths::stella_root().ok_or_else(|| {
                    "no home directory is discoverable, so there is no user-scope \
                     configuration to write"
                        .to_string()
                })?;
                (
                    root.join("stella.toml"),
                    root.join("settings.json"),
                    "run `stella migrate config` first",
                )
            }
        };
        if !path.exists() && json.exists() {
            return Err(format!(
                "this package configures settings, and {} is still JSON. Creating {} \
                 beside it would take precedence over the whole file and silently \
                 disable every setting in it — {remedy}.",
                json.display(),
                path.display(),
            ));
        }
        Ok(Self { path, user_private })
    }

    /// The file itself, for a message that needs to name it.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// Read `path` as an editable document, or an empty one when it does not exist.
///
/// A malformed file is a named error rather than a silent reset — the
/// [`crate::settings::toml_io`] rule, restated here because this module writes
/// through the same file that one does.
fn load_document(path: &Path) -> Result<DocumentMut, String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => contents
            .parse::<DocumentMut>()
            .map_err(|e| format!("invalid config file {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// The value at a dotted path, as its literal TOML text, or `None` when no
/// value is there.
///
/// A *table* at the path answers `None`, deliberately: a manifest key is a leaf
/// ([`stella_plugin::ConfigureEntry`]), so a table sitting where one should be
/// is the workspace's own structure and not a value this module may claim to
/// restore.
fn value_text(doc: &DocumentMut, segments: &[&str]) -> Option<String> {
    let mut item: &Item = doc.as_item();
    for segment in segments {
        item = item.get(segment)?;
    }
    item.as_value().map(|value| value.to_string().trim().into())
}

/// The shallowest prefix of `segments` that names nothing in `doc` — the table
/// a write is about to create, and therefore the one a revert must remove.
fn first_missing_prefix(doc: &DocumentMut, segments: &[&str]) -> Option<String> {
    let mut item: &Item = doc.as_item();
    for (depth, segment) in segments.iter().enumerate() {
        match item.get(segment) {
            Some(child) => item = child,
            // The leaf itself is not a "created table" — removing the leaf on
            // revert is the ordinary path, so only a missing *parent* counts.
            None => return (depth + 1 < segments.len()).then(|| segments[..=depth].join(".")),
        }
    }
    None
}

/// Set a dotted key to `value`, creating the tables on the way.
///
/// # Why the tables are built rather than indexed into
///
/// `doc["a"]["b"] = value` looks like the same thing and is not: `toml_edit`'s
/// auto-vivification creates **inline** tables, so writing one key into a fresh
/// path rendered `self_driving = { attribution = { signature = "…" } }` at the
/// top of the document, above the user's own banner comment. Valid TOML, and
/// exactly the unreadability [`crate::settings::toml_io`]'s
/// `expand_inline_tables` exists to prevent — in a file whose whole reason for
/// being TOML is that a person reads it. Building the tables explicitly appends
/// a real `[self_driving.attribution]` at the end instead.
///
/// # Errors
///
/// When a segment on the way is an existing **non-table** — `self_driving = 1`
/// with `self_driving.attribution.signature` declared. Refused rather than
/// overwritten, because that write is not one this module could undo: the
/// journal records the value at the *leaf*, and there is no leaf here to
/// record. A destructive change with no recorded prior value is the one thing
/// the revert contract cannot survive.
fn set_value(
    doc: &mut DocumentMut,
    segments: &[&str],
    value: toml_edit::Value,
) -> Result<(), String> {
    let (leaf, parents) = segments
        .split_last()
        .expect("a key has at least one segment");
    let mut table = doc.as_table_mut();
    for segment in parents {
        let entry = table.entry(segment).or_insert_with(|| {
            let mut created = toml_edit::Table::new();
            // Implicit until proven otherwise, so an intermediate that holds
            // only other tables renders no bare header of its own — the
            // `hide_empty_parents` rule, which also keeps a reader from adding
            // a key under an empty header where it would silently belong to
            // the wrong table.
            created.set_implicit(true);
            Item::Table(created)
        });
        table = entry.as_table_mut().ok_or_else(|| {
            format!(
                "`{}` cannot be set: `{segment}` already holds a value rather than a \
                 table, and overwriting it is a change this install could not undo",
                segments.join(".")
            )
        })?;
    }
    table.insert(leaf, Item::Value(value));
    // The table now holds a direct key, so it needs a header to hang it on.
    table.set_implicit(false);
    Ok(())
}

/// Remove a dotted key, leaving its ancestors alone.
fn remove_key(doc: &mut DocumentMut, segments: &[&str]) {
    let mut item: &mut Item = doc.as_item_mut();
    for segment in &segments[..segments.len() - 1] {
        match item.get_mut(segment) {
            Some(child) => item = child,
            None => return,
        }
    }
    if let Some(table) = item.as_table_like_mut() {
        table.remove(segments[segments.len() - 1]);
    }
}

/// Parse one entry's rendered value into the document's value model.
///
/// The text is [`ConfigureEntry::rendered_value`]'s — the same string the
/// consent document showed — so what a human accepted is literally what is
/// parsed and stored.
fn parse_rendered(entry: &ConfigureEntry) -> Result<toml_edit::Value, String> {
    entry
        .rendered_value()
        .parse::<toml_edit::Value>()
        .map_err(|e| {
            format!(
                "`{}` declares a value this build cannot write ({}): {e}",
                entry.key,
                entry.rendered_value()
            )
        })
}

/// The whole change a `[[configure]]` table would make, computed without
/// touching the file.
///
/// **Planning and writing are separate so the journal can be durable first.**
/// Written the other way round — apply, then journal — a failure between the
/// two leaves a workspace configured with nothing on disk that knows how to
/// undo it, which is the exact state [`revert`] exists to make impossible.
#[derive(Debug)]
pub(crate) struct ConfigPlan {
    /// What undoes it.
    pub(crate) journal: ConfigJournal,
    /// The file's new contents.
    rendered: String,
    /// Where they go.
    path: PathBuf,
    /// Whether the write is the owner-only user tier's — carried from the
    /// [`ConfigTarget`] so [`ConfigPlan::commit`] can honor it.
    user_private: bool,
}

/// Read the target, record every prior value, and render the file that would
/// result.
///
/// One read-modify-write for the whole table, on
/// [`crate::settings::toml_io::save_sections`]'s reasoning: writing each key
/// separately would multiply the window in which a crash leaves the config
/// half-changed, and a half-applied package is exactly the state neither
/// install nor remove can reason about.
pub(crate) fn plan(target: &ConfigTarget, manifest: &PluginManifest) -> Result<ConfigPlan, String> {
    let mut doc = load_document(&target.path)?;
    let mut prior = Vec::with_capacity(manifest.configure.len());

    for entry in &manifest.configure {
        let segments: Vec<&str> = entry.segments().collect();
        let value = parse_rendered(entry)?;
        // Both reads happen before this entry's write, or the second would
        // describe the state this loop just produced rather than what it found.
        prior.push(PriorValue {
            key: entry.key.clone(),
            value: value_text(&doc, &segments),
            created: first_missing_prefix(&doc, &segments),
        });
        set_value(&mut doc, &segments, value)?;
    }

    Ok(ConfigPlan {
        journal: ConfigJournal {
            plugin: manifest.name.clone(),
            config: target.path.clone(),
            prior,
        },
        rendered: doc.to_string(),
        path: target.path.clone(),
        user_private: target.user_private,
    })
}

impl ConfigPlan {
    /// Write the planned document. Call only once the journal is on disk.
    ///
    /// Owner-only writes are the user tier's, which is where credentials can
    /// sit; a repository's own `stella.toml` is committed and reviewed like
    /// source, so it keeps the ordinary mode.
    pub(crate) fn commit(&self) -> Result<(), String> {
        crate::settings::write_config_file(&self.path, self.rendered.as_bytes(), self.user_private)
    }
}

/// Write the journal into the installed package directory.
pub(crate) fn write_journal(plugin_dir: &Path, journal: &ConfigJournal) -> Result<(), String> {
    let path = plugin_dir.join(REVERT_FILE);
    let text = serde_json::to_string_pretty(journal)
        .map_err(|e| format!("cannot record what to restore on removal: {e}"))?;
    std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Read a package's journal, or `None` when it has none.
///
/// An unreadable or unparseable journal is `None` rather than an error: it is
/// consulted during `remove`, and a package that cannot be uninstalled because
/// a scratch file went bad is a worse failure than one whose config is left for
/// the user to fix — which [`revert`] tells them about by name.
fn read_journal(plugin_dir: &Path) -> Option<ConfigJournal> {
    let text = std::fs::read_to_string(plugin_dir.join(REVERT_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Whether the tier's config file still holds what the package wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CurrentValue {
    /// The file holds exactly what the package wrote.
    Unchanged,
    /// It holds something else now — the literal TOML text, or `None` for a key
    /// that has since been deleted.
    Edited(Option<String>),
    /// The tier's `stella.toml` could not be resolved or read, so nothing is
    /// claimed either way. Distinct from [`Self::Edited(None)`]: "the key is
    /// gone" and "we could not look" are different facts, and reporting the
    /// second as the first would tell a user their config had been edited when
    /// it may not have been.
    Unknown,
}

/// One key an installed package configured, for [`super::list`] to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfiguredKey {
    /// The dotted key, as the manifest declared it.
    pub(crate) key: String,
    /// The value the package set, as the consent document rendered it.
    pub(crate) declared: String,
    /// What the key held before the install, `None` when it held nothing.
    ///
    /// This is the half a user cannot otherwise recover: the current value is
    /// visible in `stella.toml`, and what removing the package would restore is
    /// visible nowhere else.
    pub(crate) prior: Option<String>,
    /// Whether the file still holds what was written.
    pub(crate) current: CurrentValue,
}

/// What an installed package configured, and what each key replaced.
///
/// **Driven by the manifest, consulted from the journal** — [`revert`]'s rule,
/// for [`revert`]'s reason: the journal sits in a directory whose contents came
/// from a package, so it supplies prior values for keys a human consented to
/// and never the list of keys itself. A package with no journal (or one
/// belonging to another package) reports its declared keys with no prior value
/// rather than nothing at all, because "this package configures your workspace"
/// is true whether or not the undo record survived.
///
/// `config` is the tier's re-derived `stella.toml` ([`ConfigTarget::resolve`]),
/// never the journal's own path, so a planted journal cannot make `list` read
/// an arbitrary file.
pub(crate) fn configured(
    plugin_dir: &Path,
    manifest: &PluginManifest,
    config: Option<&Path>,
) -> Vec<ConfiguredKey> {
    if manifest.configure.is_empty() {
        return Vec::new();
    }
    let journal = read_journal(plugin_dir).filter(|journal| journal.plugin == manifest.name);
    let doc = config.and_then(|path| load_document(path).ok());

    manifest
        .configure
        .iter()
        .map(|entry| {
            let recorded = journal
                .as_ref()
                .and_then(|journal| journal.prior.iter().find(|prior| prior.key == entry.key));
            let declared = entry.rendered_value();
            let current = match &doc {
                None => CurrentValue::Unknown,
                Some(doc) => {
                    let segments: Vec<&str> = entry.segments().collect();
                    match value_text(doc, &segments) {
                        Some(text) if text == declared => CurrentValue::Unchanged,
                        other => CurrentValue::Edited(other),
                    }
                }
            };
            ConfiguredKey {
                key: entry.key.clone(),
                declared,
                prior: recorded.and_then(|recorded| recorded.value.clone()),
                current,
            }
        })
        .collect()
}

/// What a revert did, so the caller can say it in the words it uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Reverted {
    /// The keys put back, in manifest order.
    pub(crate) keys: Vec<String>,
    /// The config file they were put back in.
    pub(crate) config: Option<PathBuf>,
    /// Keys the manifest declared that no journal could account for — the
    /// honest half. Named rather than swallowed, because the alternative is a
    /// `remove` that reports success while leaving configuration behind.
    pub(crate) unaccounted: Vec<String>,
}

/// Put back what this package's install changed, before its directory goes.
///
/// **Driven by the manifest, not by the journal.** The keys come from
/// `manifest.configure` — the list a human consented to — and the journal
/// supplies only the prior value for each. A journal naming a key the manifest
/// does not declare is ignored, so a planted file cannot turn `remove` into a
/// primitive for rewriting arbitrary configuration.
///
/// **The destination is re-derived, not read from the journal.** `expected`
/// is the file this tier writes ([`ConfigTarget::resolve`]), and the journal's
/// own `config` path is honoured only when it names exactly that. The journal
/// is third-party data — it lives in the package's directory and a plugin runs
/// as the user, so a hostile package can rewrite it after install to name any
/// path on disk. Trusting `journal.config` as the write target would make
/// `remove` an arbitrary-file write; gating it against `expected` keeps the
/// worst a planted journal can do to a wrong value on a consented key in the
/// file it was always going to touch. A mismatch — or a tier no longer
/// resolvable to a `stella.toml` — leaves the keys unaccounted for the user to
/// fix by hand rather than redirected.
pub(crate) fn revert(
    plugin_dir: &Path,
    manifest: &PluginManifest,
    expected: Option<&Path>,
) -> Reverted {
    if manifest.configure.is_empty() {
        return Reverted::default();
    }

    let all_unaccounted = || Reverted {
        unaccounted: manifest
            .configure
            .iter()
            .map(|entry| entry.key.clone())
            .collect(),
        ..Reverted::default()
    };

    let Some(journal) = read_journal(plugin_dir).filter(|journal| journal.plugin == manifest.name)
    else {
        return all_unaccounted();
    };
    match expected {
        Some(expected) if same_config_file(&journal.config, expected) => undo(&journal, manifest),
        _ => all_unaccounted(),
    }
}

/// Whether two paths name the same config file, resolving symlinks and `..`
/// where the files exist so a journal path dressed up to point at the real
/// file still matches — and, more to the point, one dressed up to point
/// *elsewhere* still does not. Falls back to a literal comparison when a path
/// cannot be canonicalized (the destination need not exist yet); that fallback
/// is strict, refusing anything but the exact expected path.
fn same_config_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Reverse one journal's writes, with the journal already in hand.
///
/// The half of [`revert`] that an install's own failure path needs: it holds
/// the plan it just committed and must undo it without going back to disk for
/// a file it may not have written yet.
///
/// Writes to `journal.config` directly, so its two callers must have
/// established that path is trustworthy: `install_configuration` holds a
/// journal it just built from the resolved [`ConfigTarget`], and [`revert`]
/// only reaches here once `journal.config` has matched the re-derived target.
/// Do not call this with a journal read from disk without that check — the
/// path is otherwise package-controlled.
pub(crate) fn undo(journal: &ConfigJournal, manifest: &PluginManifest) -> Reverted {
    let mut done = Reverted::default();
    let path = journal.config.clone();
    let Ok(mut doc) = load_document(&path) else {
        done.unaccounted = manifest
            .configure
            .iter()
            .map(|entry| entry.key.clone())
            .collect();
        return done;
    };

    for entry in &manifest.configure {
        let Some(record) = journal.prior.iter().find(|prior| prior.key == entry.key) else {
            done.unaccounted.push(entry.key.clone());
            continue;
        };
        let segments: Vec<&str> = entry.segments().collect();
        match &record.value {
            // It held something: put that back, whatever it is now.
            Some(text) => match text
                .parse::<toml_edit::Value>()
                .map_err(|error| error.to_string())
                .and_then(|value| set_value(&mut doc, &segments, value))
            {
                Ok(()) => done.keys.push(entry.key.clone()),
                Err(_) => done.unaccounted.push(entry.key.clone()),
            },
            // It held nothing: remove the key, and the table this install
            // created to hold it.
            None => {
                match &record.created {
                    Some(created) => {
                        let ancestor: Vec<&str> = created.split('.').collect();
                        remove_key(&mut doc, &ancestor);
                    }
                    None => remove_key(&mut doc, &segments),
                }
                done.keys.push(entry.key.clone());
            }
        }
    }

    if done.keys.is_empty() {
        return done;
    }
    if crate::settings::write_config_file(&path, doc.to_string().as_bytes(), false).is_err() {
        done.unaccounted.extend(std::mem::take(&mut done.keys));
        return done;
    }
    // Named only on success, so a caller printing this never claims a
    // restoration that did not reach the disk.
    done.config = Some(path);
    done
}

#[cfg(test)]
mod tests;
