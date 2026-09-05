// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Which installed plugins take part in every turn.
//!
//! Installing a plugin puts it on disk. It does not start it. The
//! `active_plugins` list in settings is what makes one join each turn. Take a
//! name out and the turn goes back to what it was.
//!
//! A name here is a plugin name. A plugin also has a wrapper id, in its own
//! `[wrapper] id`. This module maps the first onto the second, in order. That
//! is the same text `--pipeline` takes, so one path binds both.
//!
//! The order is the list's own order. Install order is not used. It is
//! invisible, it differs from machine to machine, and it would make the
//! prompt depend on the order a person typed commands. AGENTS.md rule 7 asks
//! for a byte-stable prompt, and a repeated benchmark run needs the same one
//! twice. Each stage's band gives the coarse order across plugins; this list
//! settles the rest.
//!
//! A name that resolves to nothing gets a line. Silence would read as "it ran
//! and did nothing", and nobody can tell those apart afterwards.

use crate::plugin_cmd::roster::PluginRoster;

/// What the standing set resolves to.
pub(crate) struct Standing {
    /// The wrapper ids to run, joined the way `--pipeline` writes them, or
    /// `None` when the set names nothing that can wrap a turn.
    pub(crate) pipeline: Option<String>,
    /// One line per name that contributed nothing, for a caller to print.
    pub(crate) notices: Vec<String>,
}

/// Resolve the names in `active_plugins` against what is installed.
///
/// The pure half, so a test can drive it without a plugin directory. Every
/// answer is decided here; the caller only reads files and prints lines.
pub(crate) fn standing(active: &[String], roster: &PluginRoster) -> Standing {
    let mut ids: Vec<String> = Vec::new();
    let mut named: Vec<&str> = Vec::new();
    let mut notices = Vec::new();

    for name in active {
        let name = name.trim();
        if name.is_empty() {
            notices.push(
                "`active_plugins` holds an empty name, which switches nothing on".to_string(),
            );
            continue;
        }
        // A second copy is a typo, not a request to run a plugin twice: a
        // repeat would compose against itself and spend a second gate's
        // allowance. Dropped rather than refused, because a settings typo must
        // not stop every run on the machine.
        if named.contains(&name) {
            notices.push(format!(
                "`active_plugins` names \"{name}\" more than once; it runs once, where it \
                 first appears"
            ));
            continue;
        }
        named.push(name);

        let Some(installed) = roster.get(name) else {
            notices.push(format!(
                "`active_plugins` names \"{name}\", which is not installed here or has been \
                 switched off under `plugins` — it adds nothing to this turn"
            ));
            continue;
        };
        let Some(wrapper) = installed.manifest.wrapper.as_ref() else {
            notices.push(format!(
                "plugin \"{name}\" is switched on but declares no [wrapper] block, so it adds \
                 no stage to a turn"
            ));
            continue;
        };
        ids.push(wrapper.id.clone());
    }

    Standing {
        pipeline: (!ids.is_empty()).then(|| ids.join(",")),
        notices,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use stella_plugin::PluginManifest;

    use super::{Standing, standing};
    use crate::plugin_cmd::panel_grant::PanelGrantState;
    use crate::plugin_cmd::receipt::ConsentState;
    use crate::plugin_cmd::roster::{InstalledPlugin, PluginRoster, PluginScope};

    fn manifest(name: &str, wrapper_id: Option<&str>) -> PluginManifest {
        let wrapper = wrapper_id.map_or_else(String::new, |id| {
            format!(
                "\n[runtime]\nargv = [\"/bin/true\"]\ntimeout_secs = 30\n\n\
                 [wrapper]\nid = \"{id}\"\n\n[[wrapper.stages]]\nname = \"execute\"\n"
            )
        });
        let text = format!(
            "name = \"{name}\"\n[loop]\nparticipation = \"steering\"\npoints = \
             [\"before_turn\"]\n{wrapper}"
        );
        PluginManifest::from_toml_str(&text).expect("the fixture manifest must load")
    }

    fn roster(plugins: Vec<PluginManifest>) -> PluginRoster {
        let installed = plugins
            .into_iter()
            .map(|manifest| InstalledPlugin {
                dir: PathBuf::from("/ws/.stella/plugins").join(&manifest.name),
                manifest,
                scope: PluginScope::User,
                consent: ConsentState::Receipted,
                panel_grant: PanelGrantState::Allowed,
            })
            .collect();
        PluginRoster::compose(installed, Vec::new(), &BTreeMap::new())
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_string()).collect()
    }

    /// Two switched-on plugins resolve to their wrapper ids, in list order.
    ///
    /// **Witness.** Fails before this module: there was no standing set, so a
    /// turn with no `--pipeline` bound nothing whatever settings held.
    #[test]
    fn the_standing_set_resolves_to_its_wrapper_ids_in_order() {
        let roster = roster(vec![
            manifest("stella-plan", Some("plan-v1")),
            manifest("vera", Some("vera-v1")),
        ]);
        let answer: Standing = standing(&names(&["vera", "stella-plan"]), &roster);
        assert_eq!(answer.pipeline.as_deref(), Some("vera-v1,plan-v1"));
        assert!(answer.notices.is_empty(), "{:?}", answer.notices);
    }

    /// An empty set leaves the turn bare.
    #[test]
    fn an_empty_set_wraps_nothing() {
        let roster = roster(vec![manifest("vera", Some("vera-v1"))]);
        let answer = standing(&[], &roster);
        assert!(answer.pipeline.is_none());
        assert!(answer.notices.is_empty());
    }

    /// A name nothing answers to is reported by name.
    #[test]
    fn a_name_that_is_not_installed_gets_a_line() {
        let roster = roster(vec![manifest("vera", Some("vera-v1"))]);
        let answer = standing(&names(&["ghost"]), &roster);
        assert!(answer.pipeline.is_none());
        assert_eq!(answer.notices.len(), 1);
        assert!(answer.notices[0].contains("ghost"), "{:?}", answer.notices);
    }

    /// A plugin with no `[wrapper]` block adds no stage, and says so.
    #[test]
    fn a_plugin_with_no_wrapper_block_gets_a_line() {
        let roster = roster(vec![manifest("hooks-only", None)]);
        let answer = standing(&names(&["hooks-only"]), &roster);
        assert!(answer.pipeline.is_none());
        assert_eq!(answer.notices.len(), 1);
        assert!(
            answer.notices[0].contains("[wrapper]"),
            "{:?}",
            answer.notices
        );
    }

    /// A repeated name runs once and is reported.
    #[test]
    fn a_repeated_name_runs_once() {
        let roster = roster(vec![manifest("vera", Some("vera-v1"))]);
        let answer = standing(&names(&["vera", "vera"]), &roster);
        assert_eq!(answer.pipeline.as_deref(), Some("vera-v1"));
        assert_eq!(answer.notices.len(), 1);
    }
}
