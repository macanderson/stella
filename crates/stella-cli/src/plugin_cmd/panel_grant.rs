// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Whether a human allowed this plugin to draw on their screen, written down
//! beside the package — SPEC 12.4's half of the panel protocol (#5056).
//!
//! # Why the install receipt is not this
//!
//! [`super::receipt`] records that somebody accepted a package: its tools, its
//! skills, its hooks, the process it runs inside a turn. A panel is a
//! different grant, and SPEC 12 opens by saying why — a plugin with a panel
//! "changes what a person **sees and touches**, so the panel is where a plugin
//! becomes a piece of software with a face, and where the strongest limits
//! belong". Each of these needs a record the receipt cannot be:
//!
//!   * **It can be withdrawn without uninstalling.** `deny` leaves the
//!     package's tools and hooks exactly where they were and takes away the
//!     rectangle. Folding the answer into the receipt would make the only way
//!     to stop a panel drawing a `stella plugin remove`.
//!   * **It can be given to a package nobody installed.** A project-tier
//!     plugin arrives with a `git clone` and has no install transaction at all
//!     ([`super::receipt`]'s tier asymmetry). It can still be asked for a
//!     panel, and this is where that answer lives.
//!   * **It is asked again when the declaration changes.** A grant is keyed on
//!     the same digest a receipt is, so a manifest that grows a surface, a
//!     slash name or a wider `[panel.process]` is a different document and is
//!     [`PanelGrantState::Drifted`] until someone reads the new one.
//!
//! # Undecided withholds
//!
//! The one asymmetry with [`super::receipt::ConsentState`]: a package with no
//! panel record does **not** draw, in either tier. The receipt's carve-out
//! exists because an unreceipted project package is the normal state of a
//! plugin that arrived by clone, and `STELLA_TRUST_PROJECT` is the transaction
//! standing behind it. There is no equivalent standing behind a panel: the
//! trust flag answers "may this repository's code run at all", and SPEC 12.4
//! asks a second question on top of it, per plugin, about the screen. So
//! absence is withholding rather than admission, and the operator is told so
//! with the command that resolves it.
//!
//! # What this is not
//!
//! [`super::receipt`]'s disclaimer applies here word for word: a plugin's
//! process runs as the user and can delete or rewrite this file exactly as it
//! can rewrite its own manifest. What the record buys is that the two files
//! must be kept in agreement, and that the only things that write one are the
//! two code paths that show a human the handshake.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::receipt::digest;
use super::roster::PluginScope;

/// The directory inside a tier that holds one panel grant per package.
///
/// A leading dot, for [`super::receipt`]'s reason: `super::roster::read_tier`
/// skips a dot entry, so the grants cannot themselves be read as a package.
const GRANTS_DIR: &str = ".panel";

/// What a person answered when they were shown a panel handshake.
///
/// Two arms and no third: SPEC 12.4 asks `[a]llow [d]eny`, and "not yet
/// answered" is the absence of a record rather than a value inside one. A
/// stored `Undecided` would be a file saying nothing, and the two states would
/// then have to be kept from disagreeing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PanelVerdict {
    /// The panel may be leased a rectangle and asked for frames.
    Allow,
    /// It may not. The rest of the package is untouched.
    Deny,
}

impl PanelVerdict {
    /// The word `stella plugin list` prints.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allowed",
            Self::Deny => "denied",
        }
    }
}

/// One panel grant, as a fact on disk.
///
/// Serde-first so the file is a value a test asserts on rather than a format
/// two functions agree about by hand ([`super::receipt::ConsentReceipt`]'s
/// reasoning).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PanelGrantRecord {
    /// The manifest `name` the handshake named. Carried as well as digested so
    /// a grant cannot be moved onto another package and pass by coincidence.
    pub(crate) plugin: String,
    /// Lowercase hex SHA-256 of the `plugin.toml` bytes the handshake was
    /// rendered from — the same digest [`super::receipt`] records, and the
    /// signature the handshake showed.
    pub(crate) manifest_sha256: String,
    /// The tier the answer was given for, as [`PluginScope::as_str`] spells it.
    pub(crate) scope: String,
    /// Unix seconds at which the answer was given.
    pub(crate) decided_at: u64,
    /// What was answered.
    pub(crate) verdict: PanelVerdict,
}

/// Whether this package's panel may draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelGrantState {
    /// A grant covers exactly these manifest bytes and says allow.
    Allowed,
    /// A grant covers exactly these manifest bytes and says deny.
    Denied,
    /// Nobody has been asked. Withholds, in both tiers — see this module's
    /// header.
    Undecided,
    /// A grant exists and this manifest is not the one it covers: the bytes
    /// changed, the name changed, or it was written for the other tier.
    Drifted,
}

impl PanelGrantState {
    /// Whether a panel in this state may be leased a rectangle.
    ///
    /// **The authoritative panel gate**, read by
    /// `super::roster::PluginRoster::panel_routes`. Exactly one arm admits, so
    /// a state added later is refused until somebody decides what it means.
    #[must_use]
    pub(crate) fn admits(self) -> bool {
        matches!(self, Self::Allowed)
    }

    /// The line an operator is told when this state withholds a panel, or
    /// `None` when it does not.
    ///
    /// Pure, so the wording is assertable without a terminal — and it names the
    /// command that resolves it, because a rectangle that silently never
    /// appears is indistinguishable from a plugin that is broken.
    #[must_use]
    pub(crate) fn notice(self, plugin: &str) -> Option<String> {
        let cause = match self {
            Self::Allowed => return None,
            Self::Denied => "you denied it",
            Self::Undecided => "nobody has been asked yet",
            Self::Drifted => {
                "its `plugin.toml` has changed since the panel was granted, so what it \
                 would draw with is not what anyone read"
            }
        };
        Some(format!(
            "! `{plugin}` declares a panel and is not drawing one — {cause}. \
             Run `stella plugin panel {plugin}` to read the handshake and decide."
        ))
    }
}

/// Where `entry`'s panel grant lives inside `tier`, or `None` for a directory
/// name that cannot hold one.
///
/// The name is third-party text used as a file name, so it is checked as one —
/// [`super::checked_name`]'s argument, one directory over. A name that fails
/// has no grant rather than a grant somewhere else.
fn grant_path(tier: &Path, entry: &str) -> Option<PathBuf> {
    super::checked_name(entry)
        .ok()
        .map(|entry| tier.join(GRANTS_DIR).join(format!("{entry}.json")))
}

/// Record that a human answered `verdict` for the panel `manifest` declares.
///
/// Called from the two paths that render
/// [`stella_plugin::panel_handshake_text`] and nowhere else: a grant written
/// anywhere but the path that shows the handshake would be a grant for a
/// document nobody read.
pub(crate) fn record(
    tier: &Path,
    scope: PluginScope,
    entry: &str,
    plugin: &str,
    manifest: &[u8],
    verdict: PanelVerdict,
) -> Result<(), String> {
    let path = grant_path(tier, entry)
        .ok_or_else(|| format!("`{}` cannot hold a panel grant", entry.escape_debug()))?;
    let record = PanelGrantRecord {
        plugin: plugin.to_string(),
        manifest_sha256: digest(manifest),
        scope: scope.as_str().to_string(),
        decided_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()),
        verdict,
    };
    let body = serde_json::to_string_pretty(&record)
        .map_err(|error| format!("cannot render the panel grant: {error}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    std::fs::write(&path, body).map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// Drop `entry`'s panel grant, best effort.
///
/// Best effort on [`super::receipt::forget`]'s reasoning: it is only called
/// where the package itself is already gone or being discarded, and a leaked
/// grant admits nothing on its own — `check` reads the manifest beside it, and
/// there is none.
pub(crate) fn forget(tier: &Path, entry: &str) {
    if let Some(path) = grant_path(tier, entry) {
        let _ = std::fs::remove_file(path);
    }
}

/// What a human answered about the panel of the package at `dir` inside
/// `tier`, whose manifest bytes are `manifest`.
///
/// `name` is checked against the record as well as the digest, which the
/// digest already covers — comparing it explicitly is what lets a mismatch be
/// reported as drift rather than as a hash that happens not to match
/// ([`super::receipt::check`]).
#[must_use]
pub(crate) fn check(
    tier: &Path,
    scope: PluginScope,
    dir: &Path,
    name: &str,
    manifest: &[u8],
) -> PanelGrantState {
    let Some(entry) = dir.file_name().and_then(|entry| entry.to_str()) else {
        return PanelGrantState::Undecided;
    };
    let Some(path) = grant_path(tier, entry) else {
        return PanelGrantState::Undecided;
    };
    let Ok(body) = std::fs::read_to_string(&path) else {
        return PanelGrantState::Undecided;
    };
    // An unreadable grant is drift, not absence. Both withhold, so unlike the
    // receipt's version of this rule nothing is reachable by writing garbage —
    // but the two states say different things to the operator, and "something
    // wrote a file there" is the true one.
    let Ok(record) = serde_json::from_str::<PanelGrantRecord>(&body) else {
        return PanelGrantState::Drifted;
    };
    if record.plugin != name
        || record.scope != scope.as_str()
        || record.manifest_sha256 != digest(manifest)
    {
        return PanelGrantState::Drifted;
    }
    match record.verdict {
        PanelVerdict::Allow => PanelGrantState::Allowed,
        PanelVerdict::Deny => PanelGrantState::Denied,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tier() -> tempfile::TempDir {
        tempfile::tempdir().expect("a temp tier")
    }

    /// The round trip: what `record` writes, `check` reads back — and only for
    /// the exact bytes, the exact name and the exact tier.
    #[test]
    fn a_grant_answers_only_for_the_manifest_the_scope_and_the_name_it_covers() {
        let tier = tier();
        let dir = tier.path().join("hello");
        let manifest = b"name = \"hello\"\n[panel]\nsurfaces = [\"settings\"]\n";
        record(
            tier.path(),
            PluginScope::User,
            "hello",
            "hello",
            manifest,
            PanelVerdict::Allow,
        )
        .expect("write");

        assert_eq!(
            check(tier.path(), PluginScope::User, &dir, "hello", manifest),
            PanelGrantState::Allowed
        );
        assert_eq!(
            check(
                tier.path(),
                PluginScope::User,
                &dir,
                "hello",
                b"name = \"hello\"\n[panel]\nsurfaces = [\"settings\", \"overlay\"]\n"
            ),
            PanelGrantState::Drifted,
            "a panel that grew a surface is not the one that was granted"
        );
        assert_eq!(
            check(tier.path(), PluginScope::Project, &dir, "hello", manifest),
            PanelGrantState::Drifted,
            "a grant answers for the tier it was written in"
        );
        assert_eq!(
            check(tier.path(), PluginScope::User, &dir, "other", manifest),
            PanelGrantState::Drifted,
            "and for the package it names"
        );
    }

    /// **The withholding witness.** Only an explicit allow admits, and the
    /// three states that do not each say why and name the way through.
    #[test]
    fn only_an_explicit_allow_admits_and_every_refusal_names_the_command() {
        let tier = tier();
        let dir = tier.path().join("hello");
        let manifest = b"name = \"hello\"\n";

        // Never asked — withheld in both tiers, unlike an install receipt.
        let undecided = check(tier.path(), PluginScope::User, &dir, "hello", manifest);
        assert_eq!(undecided, PanelGrantState::Undecided);
        assert!(!undecided.admits());
        assert_eq!(
            check(tier.path(), PluginScope::Project, &dir, "hello", manifest),
            PanelGrantState::Undecided,
            "a plugin that arrived with a `git clone` is still asked about its screen"
        );

        record(
            tier.path(),
            PluginScope::User,
            "hello",
            "hello",
            manifest,
            PanelVerdict::Deny,
        )
        .expect("write");
        let denied = check(tier.path(), PluginScope::User, &dir, "hello", manifest);
        assert_eq!(denied, PanelGrantState::Denied);
        assert!(!denied.admits());

        for state in [
            PanelGrantState::Undecided,
            PanelGrantState::Denied,
            PanelGrantState::Drifted,
        ] {
            let notice = state.notice("hello").expect("a withheld panel is spoken");
            assert!(
                notice.contains("stella plugin panel hello"),
                "{state:?}: {notice}"
            );
        }
        assert!(
            PanelGrantState::Allowed.notice("hello").is_none(),
            "an admitted panel is not something to tell anyone about"
        );
    }

    /// A corrupt grant is drift rather than absence: something wrote a file at
    /// that path, and the operator is told the true reason.
    #[test]
    fn an_unreadable_grant_is_drift_rather_than_absence() {
        let tier = tier();
        std::fs::create_dir_all(tier.path().join(GRANTS_DIR)).expect("dir");
        std::fs::write(tier.path().join(GRANTS_DIR).join("hello.json"), "{").expect("garbage");
        assert_eq!(
            check(
                tier.path(),
                PluginScope::User,
                &tier.path().join("hello"),
                "hello",
                b"name = \"hello\"\n"
            ),
            PanelGrantState::Drifted
        );
    }

    /// `forget` removes exactly the one grant, and is silent about one that was
    /// never there.
    #[test]
    fn forgetting_a_grant_removes_only_that_one() {
        let tier = tier();
        for name in ["hello", "gauge"] {
            record(
                tier.path(),
                PluginScope::User,
                name,
                name,
                b"x",
                PanelVerdict::Allow,
            )
            .expect("write");
        }
        forget(tier.path(), "hello");
        forget(tier.path(), "never-granted");
        assert_eq!(
            check(
                tier.path(),
                PluginScope::User,
                &tier.path().join("hello"),
                "hello",
                b"x"
            ),
            PanelGrantState::Undecided
        );
        assert_eq!(
            check(
                tier.path(),
                PluginScope::User,
                &tier.path().join("gauge"),
                "gauge",
                b"x"
            ),
            PanelGrantState::Allowed
        );
    }
}
