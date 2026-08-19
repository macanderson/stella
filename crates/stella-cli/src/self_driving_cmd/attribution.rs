//! Loading the loop's self-identification.
//!
//! [`stella_autonomy::Attribution`] is the model and [`stella_autonomy::sign`]
//! the rule; this is the one place that reads it off disk.
//!
//! # Why a file rather than a setting
//!
//! It is **source-tracked**, at `.stella/attribution.json`, so it travels with
//! the repository: every clone and every machine signs the same way, which is
//! the whole point of a signature. A per-user setting would make attribution
//! depend on who happened to start the loop.
//!
//! It is also the seam a downstream distribution turns. An installed plugin
//! rewrites this file to sign in its own name and namespace its own branches —
//! a configuration change rather than a fork. The install/uninstall hooks that
//! would let a plugin do that automatically are not built yet and are tracked
//! separately; today a plugin's installer writes the file.
//!
//! # Absent means default, never means silent
//!
//! A workspace with no file signs with [`Attribution::default`], which
//! identifies the loop rather than saying nothing. An operator who wants
//! silence on a surface says so by setting that surface to an empty string —
//! an explicit choice, recorded in a tracked file, rather than the accident of
//! never having written one.

use std::path::Path;

use stella_autonomy::Attribution;

/// Where a workspace declares how the loop signs.
const MANIFEST: &str = ".stella/attribution.json";

/// Read the workspace's attribution, falling back to the default.
///
/// A malformed file yields the default **and says so on stderr**, rather than
/// failing the verb: an unparseable signature should not stop the loop from
/// doing its work, but it must not silently become no signature either — that
/// is how an unattributed commit reaches a shared branch.
#[must_use]
pub(super) fn load(root: &Path) -> Attribution {
    let path = root.join(MANIFEST);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Attribution::default();
    };
    match serde_json::from_str(&raw) {
        Ok(attribution) => attribution,
        Err(error) => {
            eprintln!(
                "warning: {} could not be read ({error}); signing with the defaults",
                path.display()
            );
            Attribution::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn write(root: &Path, body: &str) {
        let path = root.join(MANIFEST);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, body).expect("write");
    }

    /// A workspace that has said nothing still identifies the loop.
    #[test]
    fn an_absent_manifest_signs_with_the_defaults() {
        let a = load(workspace().path());
        assert_eq!(a, Attribution::default());
        assert_eq!(a.branch_prefix(), "stella/");
    }

    /// The seam a downstream distribution turns: one file, and every surface
    /// plus the branch namespace changes with it.
    #[test]
    fn a_distribution_can_rewrite_every_surface_and_the_prefix() {
        let ws = workspace();
        write(
            ws.path(),
            r#"{
                "commit": "Created by oxagen.",
                "pull_request": "Opened by oxagen.",
                "issue": "Filed by oxagen.",
                "issue_comment": "Posted by oxagen.",
                "branch_prefix": "oxagen/"
            }"#,
        );

        let a = load(ws.path());
        assert_eq!(a.commit, "Created by oxagen.");
        assert_eq!(a.pull_request, "Opened by oxagen.");
        assert_eq!(a.issue, "Filed by oxagen.");
        assert_eq!(a.issue_comment, "Posted by oxagen.");
        assert_eq!(a.branch_prefix(), "oxagen/");
    }

    /// Rewriting one surface leaves the others identifying the loop, rather
    /// than blanking them.
    #[test]
    fn a_partial_rewrite_does_not_blank_the_rest() {
        let ws = workspace();
        write(ws.path(), r#"{"commit": "Created by oxagen."}"#);

        let a = load(ws.path());
        assert_eq!(a.commit, "Created by oxagen.");
        assert_eq!(a.issue, "Filed by stella.");
    }

    /// A malformed file must not become "no signature".
    #[test]
    fn a_malformed_manifest_falls_back_to_signing_not_to_silence() {
        let ws = workspace();
        write(ws.path(), "{ this is not json");

        let a = load(ws.path());
        assert_eq!(a, Attribution::default());
        assert!(
            !a.commit.is_empty(),
            "a broken file must not silence the loop"
        );
    }
}
