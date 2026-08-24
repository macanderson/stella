// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a human said yes to, written down beside the package (#3514).
//!
//! # The defect this closes
//!
//! `stella plugin install` renders [`stella_plugin::consent_text`], asks, and
//! copies. Then it forgot: nothing on disk recorded *which* manifest the
//! answer was about. A plugin's process runs as the user and owns its install
//! directory, so it could rewrite its own `plugin.toml` — `participation =
//! "observer"` with no hooks becomes `arbiter` with `hooks = ["PreToolUse",
//! "Stop"]` and `env = ["GITHUB_TOKEN"]` — and the next session's
//! [`super::roster::read_tier`] would parse the new file and
//! `LoopGrant::permits_hook`, "the authoritative filter", would faithfully
//! authorise a grant no human ever saw. `install` refusing to overwrite an
//! existing directory does not help, because nothing re-enters `install`.
//!
//! A receipt is the missing half of the transaction: the SHA-256 of the
//! `plugin.toml` bytes the consent document was rendered from, the tier the
//! answer was given for, and when. A manifest that no longer digests to its
//! receipt is not the one that was accepted, and the package is withheld until
//! it is installed again — which re-renders the document and asks again.
//!
//! # What a receipt is not
//!
//! It is **not a sandbox and does not pretend to be one.** A plugin's process
//! runs as the user, so it can delete or rewrite a receipt exactly as it can
//! rewrite a manifest; nothing short of an OS boundary changes that, and
//! `doc:pipeline-as-plugins` does not claim one. What it buys is that the two
//! files must be kept in agreement, and the only thing that writes a receipt
//! is the code path that shows a human the document. Silent self-escalation
//! becomes a refusal a user can see and act on.
//!
//! This is the sibling of
//! [`super::package::reconciles_with_disk`](super::package), and the two
//! divide the package between them: that one re-checks the `tools/`,
//! `skills/` and `rules/` **directories** against the manifest on every load,
//! this one checks the **manifest** against the consent transaction. Neither
//! subsumes the other — a package can grow an undeclared tool without touching
//! `plugin.toml`, and it can widen `[loop]` without touching a directory.
//!
//! # Why the two tiers answer differently
//!
//! `~/.stella/plugins` holds what the operator installed, and
//! [`super::roster`]'s module docs state the invariant that makes this
//! enforceable: *nothing arrives in it except through `stella plugin install`'s
//! consent transaction.* So a user-tier package with **no receipt at all** is
//! a package whose receipt went missing, and it is refused.
//!
//! `<workspace>/.stella/plugins` is different by design. It holds whatever a
//! `git clone` carried in, and shipping a plugin with a repository — the way a
//! repository already ships `.stella/rules` — is a supported thing to do
//! (`super::roster::read_project_tier`). The consent transaction for a package
//! that arrived that way is `STELLA_TRUST_PROJECT`, which is asked before any
//! of it loads. So an **unreceipted** project package still loads behind that
//! gate; a **receipted** one — installed here by `stella plugin install`, which
//! writes a receipt into this tier too — must still match it.
//!
//! A project-tier package can therefore escape this check by deleting its
//! receipt. What stands behind it there is the trust flag the workspace was
//! opened under, which is the gate that decides whether any of its code runs
//! at all.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::roster::PluginScope;

/// The directory inside a tier that holds one receipt per installed package.
///
/// A leading dot, for the reason `install`'s staging tree has one:
/// [`super::roster::read_tier`] skips a dot entry, so the receipts cannot
/// themselves be read as a package.
const RECEIPTS_DIR: &str = ".consent";

/// One install's consent transaction, as a fact on disk.
///
/// Serde-first so the file is a value a test asserts on rather than a format
/// two functions agree about by hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConsentReceipt {
    /// The manifest `name` the accepted document named. Carried as well as
    /// digested so a receipt cannot be moved onto another package and pass by
    /// coincidence, and so a human reading the file can tell what it is about.
    pub(crate) plugin: String,
    /// Lowercase hex SHA-256 of the `plugin.toml` bytes
    /// [`stella_plugin::consent_text`] was rendered from.
    pub(crate) manifest_sha256: String,
    /// The tier the grant was accepted for, as [`PluginScope::as_str`] spells
    /// it — so a receipt copied between tiers does not answer for the other.
    pub(crate) scope: String,
    /// Unix seconds at which the answer was given.
    pub(crate) consented_at: u64,
}

/// Whether the manifest on disk is the one a grant was accepted for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConsentState {
    /// A receipt covers exactly these manifest bytes.
    Receipted,
    /// No receipt covers this package. Whether that is a refusal depends on
    /// the tier — see this module's header.
    Unreceipted,
    /// A receipt exists and this manifest is not the one it covers: either the
    /// bytes changed, the name changed, or the receipt was written for the
    /// other tier.
    Drifted,
}

impl ConsentState {
    /// Whether a package in `scope` carrying this state may load.
    #[must_use]
    pub(crate) fn admits(self, scope: PluginScope) -> bool {
        match self {
            Self::Receipted => true,
            Self::Drifted => false,
            // The tier asymmetry, in one expression, argued in the module
            // header: the user tier has exactly one way in, the project tier
            // has two and the other one is gated by `STELLA_TRUST_PROJECT`.
            Self::Unreceipted => scope == PluginScope::Project,
        }
    }

    /// The line a user is told when this state withholds a package, or `None`
    /// when it does not.
    ///
    /// Pure, so the wording is assertable without a terminal — and it names
    /// the two commands that resolve it, because "your plugin stopped loading"
    /// is unanswerable without them.
    #[must_use]
    pub(crate) fn notice(self, scope: PluginScope, plugin: &str, dir: &Path) -> Option<String> {
        if self.admits(scope) {
            return None;
        }
        let cause = match self {
            Self::Drifted => "its `plugin.toml` has changed since it was installed",
            Self::Unreceipted | Self::Receipted => "there is no record of anyone consenting to it",
        };
        Some(format!(
            "  ! {} was NOT loaded — {cause}, so the grant it now declares is one no human \
             accepted. Re-consent with `stella plugin remove {plugin}` and `stella plugin \
             install`.",
            dir.display()
        ))
    }
}

/// The SHA-256 a receipt records, lowercase hex.
#[must_use]
pub(crate) fn digest(manifest: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(manifest);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Where `entry`'s receipt lives inside `tier`, or `None` for a directory name
/// that cannot be one.
///
/// The name is third-party text used as a file name, so it is checked as one —
/// [`super::checked_name`]'s argument, one directory over. A name that fails
/// has no receipt rather than a receipt somewhere else.
fn receipt_path(tier: &Path, entry: &str) -> Option<PathBuf> {
    super::checked_name(entry)
        .ok()
        .map(|entry| tier.join(RECEIPTS_DIR).join(format!("{entry}.json")))
}

/// Record that a human accepted `manifest` for the package installed as
/// `entry` in `tier`.
///
/// Called from `install` and nowhere else: a receipt written anywhere but the
/// path that renders the consent document would be a receipt for a document
/// nobody read.
pub(crate) fn record(
    tier: &Path,
    scope: PluginScope,
    entry: &str,
    plugin: &str,
    manifest: &[u8],
) -> Result<(), String> {
    let path = receipt_path(tier, entry)
        .ok_or_else(|| format!("`{}` cannot hold a consent receipt", entry.escape_debug()))?;
    let receipt = ConsentReceipt {
        plugin: plugin.to_string(),
        manifest_sha256: digest(manifest),
        scope: scope.as_str().to_string(),
        consented_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()),
    };
    let body = serde_json::to_string_pretty(&receipt)
        .map_err(|error| format!("cannot render the consent receipt: {error}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    std::fs::write(&path, body).map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// Drop `entry`'s receipt, best effort.
///
/// Best effort because it is only ever called where the package itself is
/// already gone or being discarded: a leaked receipt names nothing and admits
/// nothing, whereas an error here would replace the reason the caller is
/// reporting with a housekeeping complaint.
pub(crate) fn forget(tier: &Path, entry: &str) {
    if let Some(path) = receipt_path(tier, entry) {
        let _ = std::fs::remove_file(path);
    }
}

/// Whether `manifest` — the bytes of the package installed at `dir` inside
/// `tier` — is what a receipt covers.
///
/// `name` is the manifest's own `name`, checked against the receipt as well as
/// the digest: the digest already covers it, and comparing it explicitly is
/// what lets a mismatch be reported as drift rather than as a hash that
/// happens not to match.
#[must_use]
pub(crate) fn check(
    tier: &Path,
    scope: PluginScope,
    dir: &Path,
    name: &str,
    manifest: &[u8],
) -> ConsentState {
    let Some(entry) = dir.file_name().and_then(|entry| entry.to_str()) else {
        return ConsentState::Unreceipted;
    };
    let Some(path) = receipt_path(tier, entry) else {
        return ConsentState::Unreceipted;
    };
    let Ok(body) = std::fs::read_to_string(&path) else {
        return ConsentState::Unreceipted;
    };
    // An unreadable receipt is drift, not absence: something wrote a file at
    // that path, and treating a corrupt one as "never installed" would let the
    // project tier's carve-out be reached by writing garbage.
    let Ok(receipt) = serde_json::from_str::<ConsentReceipt>(&body) else {
        return ConsentState::Drifted;
    };
    let matches = receipt.plugin == name
        && receipt.scope == scope.as_str()
        && receipt.manifest_sha256 == digest(manifest);
    if matches {
        ConsentState::Receipted
    } else {
        ConsentState::Drifted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tier() -> tempfile::TempDir {
        tempfile::tempdir().expect("a temp tier")
    }

    /// The round trip: what `record` writes, `check` accepts — and only for
    /// the exact bytes, the exact name and the exact tier.
    #[test]
    fn a_receipt_admits_only_the_manifest_the_scope_and_the_name_it_covers() {
        let tier = tier();
        let dir = tier.path().join("vera");
        let manifest = b"name = \"vera\"\n";
        record(tier.path(), PluginScope::User, "vera", "vera", manifest).expect("write");

        assert_eq!(
            check(tier.path(), PluginScope::User, &dir, "vera", manifest),
            ConsentState::Receipted
        );
        assert_eq!(
            check(
                tier.path(),
                PluginScope::User,
                &dir,
                "vera",
                b"name = \"vera\"\n[loop]\nparticipation = \"arbiter\"\n"
            ),
            ConsentState::Drifted,
            "a widened manifest is not the one that was accepted"
        );
        assert_eq!(
            check(tier.path(), PluginScope::Project, &dir, "vera", manifest),
            ConsentState::Drifted,
            "a receipt answers for the tier it was written in"
        );
        assert_eq!(
            check(tier.path(), PluginScope::User, &dir, "other", manifest),
            ConsentState::Drifted,
            "and for the package it names"
        );
    }

    /// A package with no receipt beside it: absent, not drifted — the state
    /// the two tiers answer differently about.
    #[test]
    fn a_package_with_no_receipt_is_unreceipted_and_only_the_project_tier_admits_it() {
        let tier = tier();
        let state = check(
            tier.path(),
            PluginScope::User,
            &tier.path().join("vera"),
            "vera",
            b"name = \"vera\"\n",
        );
        assert_eq!(state, ConsentState::Unreceipted);
        assert!(!state.admits(PluginScope::User));
        assert!(
            state.admits(PluginScope::Project),
            "a plugin that arrived with a `git clone` is gated by STELLA_TRUST_PROJECT, not by \
             a receipt it never had"
        );
        assert!(
            ConsentState::Drifted
                .notice(PluginScope::Project, "vera", Path::new("/t/vera"))
                .is_some(),
            "but drift is refused in both tiers"
        );
    }

    /// A corrupt receipt is drift. Treating it as absence would make the
    /// project tier's carve-out reachable by writing garbage over the file.
    #[test]
    fn an_unreadable_receipt_is_drift_rather_than_absence() {
        let tier = tier();
        std::fs::create_dir_all(tier.path().join(RECEIPTS_DIR)).expect("dir");
        std::fs::write(tier.path().join(RECEIPTS_DIR).join("vera.json"), "{").expect("garbage");
        assert_eq!(
            check(
                tier.path(),
                PluginScope::Project,
                &tier.path().join("vera"),
                "vera",
                b"name = \"vera\"\n"
            ),
            ConsentState::Drifted
        );
    }

    /// The notice names the package, the cause, and both commands that resolve
    /// it — and says nothing at all when the package is admitted.
    #[test]
    fn the_refusal_names_the_cause_and_the_way_back() {
        let notice = ConsentState::Drifted
            .notice(
                PluginScope::User,
                "vera",
                Path::new("/home/dev/.stella/plugins/vera"),
            )
            .expect("drift is refused");
        assert!(notice.contains("plugin.toml` has changed"), "{notice}");
        assert!(notice.contains("stella plugin remove vera"), "{notice}");
        assert!(notice.contains("stella plugin install"), "{notice}");

        assert!(
            ConsentState::Receipted
                .notice(PluginScope::User, "vera", Path::new("/t"))
                .is_none(),
            "an admitted package is not something to tell anyone about"
        );
    }

    /// `forget` removes exactly the one receipt, and is silent about one that
    /// was never there.
    #[test]
    fn forgetting_a_receipt_removes_only_that_one() {
        let tier = tier();
        for name in ["vera", "lint-gate"] {
            record(tier.path(), PluginScope::User, name, name, b"x").expect("write");
        }
        forget(tier.path(), "vera");
        forget(tier.path(), "never-installed");
        assert_eq!(
            check(
                tier.path(),
                PluginScope::User,
                &tier.path().join("vera"),
                "vera",
                b"x"
            ),
            ConsentState::Unreceipted
        );
        assert_eq!(
            check(
                tier.path(),
                PluginScope::User,
                &tier.path().join("lint-gate"),
                "lint-gate",
                b"x"
            ),
            ConsentState::Receipted
        );
    }
}
