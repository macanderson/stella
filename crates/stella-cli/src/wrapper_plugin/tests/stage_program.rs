// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What this host's published signals do to a first-party plugin's declared
//! stage program.
//!
//! The join #3547 is about: [`super::super::pre_turn_signals`] is where the
//! values come from, `plugins/*/plugin.toml` is where the conditions reading
//! them are written, and `Wrapper::resolve` is what puts the two together.
//! Neither half can witness the defect alone — the manifests parse, the
//! signals are honest, and the stage still never runs.
//!
//! Read against the **shipped** manifests rather than fixtures, on purpose: a
//! fixture would grade the resolver, and what shipped inert was the file in
//! `plugins/`.

use std::path::{Path, PathBuf};

use stella_plugin::{HostStage, PluginManifest, StageName};

fn shipped(plugin: &str) -> PluginManifest {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins")
        .join(plugin)
        .join("plugin.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} ships in this repository: {error}", path.display()));
    PluginManifest::from_toml_str(&text).expect("the shipped manifest loads")
}

/// The stages this host's own signals resolve a manifest's program to.
fn stages_on_this_host(manifest: &PluginManifest) -> Vec<StageName> {
    let wrapper = manifest.wrapper.as_ref().expect("[wrapper]");
    wrapper
        .resolve(&super::super::pre_turn_signals(false, false))
        .expect("the shipped stage order resolves")
        .stages()
        .to_vec()
}

/// **Witness for #3547.** The stage each first-party plugin exists to
/// contribute at actually runs, against the signals this host actually
/// publishes.
///
/// Both manifests gated their one real stage on a triage signal —
/// `research` on `questions > 0`, `plan` on `plans` — transcribed from a
/// built-in whose crate was deleted in #3865. No shipping host runs a triage
/// stage, so `pre_turn_signals` publishes every triage signal false or zero,
/// honestly; `Wrapper::resolve` dropped the stage; and the plugin was
/// installed, selected, dispatched and structurally unable to contribute
/// anything. For `plan-v1` that was the whole plugin, since `plan` is the
/// only stage it answers at (`before_turn_stages`).
///
/// The assertion is the intersection of the two halves, which is why it is
/// here rather than in either: it reads the host's published values and the
/// shipped file, and fails on a build where either one goes back.
#[test]
fn the_stage_each_first_party_plugin_answers_at_runs_on_this_host() {
    for (plugin, stage) in [
        ("stella-research", HostStage::Research),
        ("stella-plan", HostStage::Plan),
    ] {
        let manifest = shipped(plugin);
        let answers_at = &manifest.loop_grant.before_turn_stages;
        assert!(
            !answers_at.is_empty(),
            "{plugin} names its stages exhaustively (#3543)"
        );
        assert!(
            answers_at
                .iter()
                .any(|name| name == &StageName::Host(stage)),
            "{plugin} is expected to answer at {stage:?}: {answers_at:?}"
        );
        assert!(
            stages_on_this_host(&manifest).contains(&StageName::Host(stage)),
            "{plugin} declares a stage this host will never ask it to contribute at"
        );
    }
}

/// The rule the manifests now follow, asserted rather than trusted to prose:
/// no shipped stage condition reads a signal only a triage stage produces.
///
/// A condition on a **host** fact (`test-command`, `candidates`,
/// `budget-metered`) is fine and is what `doc:wrapper-socket` §5 points an
/// author at — this fails only on a triage signal, which is the one class
/// nothing publishes a real value for.
#[test]
fn no_shipped_manifest_gates_a_stage_on_a_signal_nothing_produces() {
    let plugins = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    let entries = std::fs::read_dir(&plugins).expect("plugins/ ships in this repository");
    let mut checked = 0;
    for entry in entries.flatten() {
        let manifest_path = entry.path().join("plugin.toml");
        if !manifest_path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&manifest_path).expect("a shipped manifest reads");
        let manifest = PluginManifest::from_toml_str(&text)
            .unwrap_or_else(|error| panic!("{} loads: {error}", manifest_path.display()));
        let Some(wrapper) = manifest.wrapper.as_ref() else {
            continue;
        };
        checked += 1;
        for stage in &wrapper.stages {
            let Some(condition) = stage.condition().expect("a shipped condition parses") else {
                continue;
            };
            assert!(
                condition.signal().publisher().is_none(),
                "{} gates `{}` on `{}`, which only a triage stage publishes — \
                 no shipping host runs one, so the stage never runs (#3547)",
                manifest_path.display(),
                stage.name,
                condition.signal(),
            );
        }
    }
    assert!(
        checked >= 2,
        "the first-party wrappers were read: {checked}"
    );
}
