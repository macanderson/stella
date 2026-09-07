// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `composes:` and `roles:` lines `stella plugin list` prints.
//!
//! The install prompt draws the same facts from the same value,
//! [`stella_plugin::TurnComposition`]. The two differ in one thing. Consent
//! is asked before a package is on disk. So it can only say what a seat with
//! no model costs. This report can read the `[seats]` table. It names the
//! model each seat holds.
//!
//! Pure over the strings, so the words can be checked with no terminal.
//! `plugin_cmd`'s `driver_lines` and `configure_lines` set that shape.
//! `plugin_cmd.rs` is near its size ceiling, so this report gets a file.

use std::collections::BTreeMap;

use stella_plugin::TurnComposition;

use super::roster::InstalledPlugin;

/// What one installed package adds to a turn, one line per fact.
///
/// Empty for a package with no `[wrapper]` stages and no `[roles]`. It adds
/// nothing to merge, and a heading over an empty list reads as a report.
///
/// `assignments` is the merged `[seats]` table. A role missing from it is
/// still printed, as one running on the session's model. The quiet version
/// of that is the defect: the seat spends, and no surface says so.
#[must_use]
pub(crate) fn composition_lines(
    plugin: &InstalledPlugin,
    assignments: &BTreeMap<String, String>,
) -> Vec<String> {
    let Some(composed) = TurnComposition::of(&plugin.manifest) else {
        return Vec::new();
    };
    let mut lines = Vec::new();

    if !composed.stages.is_empty() {
        lines.push(match &composed.pipeline {
            Some(pipeline) => format!("  composes: pipeline `{pipeline}`"),
            None => "  composes:".to_string(),
        });
        for stage in &composed.stages {
            let own = if stage.contributed {
                " (this package's own stage)"
            } else {
                ""
            };
            let when = match &stage.condition {
                Some(condition) => format!("when: {condition}"),
                None => "on every turn".to_string(),
            };
            lines.push(format!(
                "    {:<6} {}{own} — {when}",
                stage.band.as_str(),
                stage.name
            ));
        }
    }

    if !composed.roles.is_empty() {
        lines.push("  roles:".to_string());
        for role in &composed.roles {
            let runs_on = match assignments.get(&role.seat) {
                Some(model) => model.clone(),
                None => "your session's model (no `[seats]` line)".to_string(),
            };
            lines.push(format!(
                "    {} — tier `{}`, seat `{}` -> {runs_on}",
                role.role, role.tier, role.seat
            ));
        }
    }

    lines
}

/// The merged `[seats]` table. Empty when nothing assigns a seat.
#[must_use]
pub(crate) fn seat_assignments(settings: &crate::settings::Settings) -> BTreeMap<String, String> {
    settings
        .agent_engine_config
        .as_ref()
        .and_then(|config| config.seat_models.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_cmd::panel_grant::PanelGrantState;
    use crate::plugin_cmd::receipt::ConsentState;
    use crate::plugin_cmd::roster::PluginScope;
    use stella_plugin::PluginManifest;

    fn installed(toml: &str) -> InstalledPlugin {
        InstalledPlugin {
            manifest: PluginManifest::from_toml_str(toml).expect("fixture manifest parses"),
            scope: PluginScope::Project,
            dir: std::path::PathBuf::from("/tmp/pkg"),
            consent: ConsentState::Receipted,
            panel_grant: PanelGrantState::Undecided,
        }
    }

    #[test]
    fn a_package_that_adds_nothing_prints_nothing() {
        let lines = composition_lines(&installed("name = \"p\"\n"), &BTreeMap::new());
        assert!(lines.is_empty(), "{lines:?}");
    }

    /// The witness. The listing named the tools, skills, records and MCP
    /// servers a package ships. It said nothing about the stages the package
    /// puts in every turn. So "why did my turn gain a triage step" could only
    /// be answered by reading `plugin.toml`.
    #[test]
    fn the_listing_names_each_stage_its_band_and_its_condition() {
        let lines = composition_lines(
            &installed(
                "name = \"p\"\n[loop]\nparticipation = \"steering\"\n\n\
                 [wrapper]\nid = \"v\"\n\
                 [[wrapper.stages]]\nname = \"triage-lite\"\nband = \"early\"\n\
                 [[wrapper.stages]]\nname = \"plan\"\n",
            ),
            &BTreeMap::new(),
        );
        assert_eq!(lines[0], "  composes: pipeline `v`");
        assert!(
            lines[1].contains("early  triage-lite (this package's own stage) — on every turn"),
            "{lines:?}"
        );
        assert!(
            lines[2].contains("normal plan — on every turn"),
            "{lines:?}"
        );
    }

    /// A seat with no model is printed as one running on the session model.
    /// An assigned seat names the model it was given.
    #[test]
    fn a_role_reports_the_model_its_seat_resolves_to() {
        let package = installed(
            "name = \"acme\"\n[loop]\nparticipation = \"steering\"\n\n\
             [subloop]\nstages = [\"triage\", \"plan\"]\n\n\
             [roles.triage]\ntier = \"cheap\"\n\n\
             [roles.plan]\ntier = \"pro\"\n",
        );
        let unassigned = composition_lines(&package, &BTreeMap::new());
        assert!(
            unassigned.iter().any(|line| line.contains(
                "triage — tier `cheap`, seat `acme/triage` -> your session's model \
                 (no `[seats]` line)"
            )),
            "{unassigned:?}"
        );

        let assignments = BTreeMap::from([(
            "acme/plan".to_string(),
            "anthropic/claude-opus-5".to_string(),
        )]);
        let assigned = composition_lines(&package, &assignments);
        assert!(
            assigned.iter().any(|line| line
                .contains("plan — tier `pro`, seat `acme/plan` -> anthropic/claude-opus-5")),
            "{assigned:?}"
        );
        assert!(
            assigned
                .iter()
                .any(|line| line.contains("seat `acme/triage` -> your session's model")),
            "the seat with no model is still printed: {assigned:?}"
        );
    }
}
