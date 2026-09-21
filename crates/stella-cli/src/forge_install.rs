// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Attach this workspace's forge to the session's tool registry.
//!
//! This is a free function, for the reason
//! [`crate::subagent::install_for_session`] is one. The registry lives in
//! `stella-tools`. That crate has no provider factory and must not grow one.
//! The adapters live here, so the wiring lives here too.
//!
//! [`crate::write_dirs::registry_rooted_at`] calls it. That is the one
//! assembly point every session path already goes through. The write grant
//! sits there on the same argument. Seven call sites would mean a new session
//! path can forget one, and it would then get a forge tool that declines.
//! Tracking that down means finding the path that did not call this.
//!
//! # What this decides
//!
//! Both answers are settled once per session rather than per call.
//!
//! **Whether there is a forge at all.** The tools stay registered either way.
//! Holding a schema back would make the prompt prefix depend on config. An
//! unattached tool answers by saying that no forge is set up. The probe is
//! `gh` on `PATH`, because `gh` is what both adapters shell out to. A provider
//! that cannot spawn its own command is not a provider.
//!
//! **What the footer says.** [`stella_autonomy::Attribution`] comes from
//! `[self_driving.attribution]` in `stella.toml`. It governs every session.
//! The self-driving loop is one of them and gets no special footer. So what an
//! artifact is signed with does not depend on which surface wrote it. The
//! record says what made it whether a person drove the session or the loop
//! did.
//!
//! # Not attaching is not a failure
//!
//! Nothing here returns an error. A workspace with no `gh` keeps the behaviour
//! it had. The forge tools decline, and `gh` in bash is not redirected. The
//! capability is absent, so nothing is taken away and nothing is promised.

use std::path::Path;
use std::sync::Arc;

use stella_tools::ToolRegistry;

/// Attach the workspace's forge, tracker and attribution to `registry`.
pub(crate) fn attach(root: &Path, registry: &ToolRegistry) {
    let attribution = crate::self_driving_cmd::config::load(root).attribution;
    if !gh_on_path() {
        // Attribution still lands. It costs nothing, and it means a host that
        // later attaches providers by another route inherits the configured
        // footer rather than the default one.
        registry.attach_forge(None, None, attribution);
        return;
    }
    registry.attach_forge(
        Some(Arc::new(
            crate::issue_provider::GhIssueProvider::for_workspace(root),
        )),
        Some(Arc::new(crate::pull_request_provider::GhPullRequests::new())),
        attribution,
    );
}

/// Is `gh` runnable?
///
/// The check runs `--version` rather than walking `PATH`. That asks the
/// question that counts, which is whether spawning it works. It also catches a
/// `gh` that is on `PATH` but cannot be run.
///
/// It does not probe auth. `gh auth status` is a network round trip on every
/// session start. A `gh` that is not logged in fails with an error, and the
/// tool passes that error on word for word. That helps more than a tool that
/// was quietly never there.
fn gh_on_path() -> bool {
    std::process::Command::new("gh")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A workspace with no `stella.toml` still signs, with the default footer.
    ///
    /// This catches a quiet failure. Missing config could yield an empty
    /// footer. Every artifact would then go out unsigned, and nothing would
    /// report a problem.
    #[test]
    fn an_unconfigured_workspace_signs_with_the_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = ToolRegistry::new(dir.path().to_path_buf());
        attach(dir.path(), &registry);
        assert_eq!(
            registry.forge_attribution(),
            stella_autonomy::Attribution::default()
        );
    }

    /// A configured footer reaches the tools, not just the self-driving loop.
    #[test]
    fn a_configured_footer_governs_an_ordinary_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("stella.toml"),
            "[self_driving.attribution]\nissue = \"Filed by the night shift.\"\n",
        )
        .expect("write stella.toml");

        let registry = ToolRegistry::new(dir.path().to_path_buf());
        attach(dir.path(), &registry);
        assert_eq!(
            registry.forge_attribution().issue,
            "Filed by the night shift."
        );
    }
}
