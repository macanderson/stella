// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Where the substrate selector is read from when there is no settings file
//! to read it from (#4510).
//!
//! `candidate_isolation` reaches [`CandidateSubstrate::for_session`] through
//! [`Settings::load`], and `Settings::load` answers `Settings::default()` under
//! `STELLA_NO_SETTINGS=1`
//! ([`filesystem_settings_disabled`](crate::settings::filesystem_settings_disabled)).
//! Every benchmark launcher sets that variable — which left the one environment
//! the copy substrate was built for, a task whose tests execute gitignored
//! fixtures, unable to select it. The default is not a bad answer there; it is
//! the *only* answer, which is a different thing and is the defect.
//!
//! So the selector also has an environment spelling. It is not a second
//! setting: [`CandidateIsolation::from_token`] parses both, so the two cannot
//! drift into accepting different words.
//!
//! # Why the environment outranks the file
//!
//! Because it is the narrower statement. A `stella.toml` describes the
//! workspace for every run; the variable describes *this* launch, in the same
//! way `--model` outranks a configured default. A launcher that exports it has
//! said something about the process it is about to start, and a file that
//! disagrees is describing a different occasion.
//!
//! # Why a malformed value is not a refusal
//!
//! The settings deserializer rejects an unknown token loudly, and it can: a
//! parse error there stops a `Settings::load` a caller is holding a `Result`
//! for. This read happens where the answer is a substrate rather than a
//! `Result`, and the safe direction is fixed by what the two shapes do —
//! copy-tree promotion overwrites the tree it lands on. So a value this build
//! cannot read gets the worktree substrate, out loud: the operator is told
//! their word was not understood *and* which substrate is running, which is the
//! pair a silent fallback withholds.
//!
//! [`CandidateSubstrate::for_session`]: super::CandidateSubstrate::for_session
//! [`Settings::load`]: crate::settings::Settings::load

use crate::settings::CandidateIsolation;

/// The environment spelling of `[run] candidate_isolation`.
///
/// Registered in the Harbor adapter's `_CLAIM_CONTAINER_ENV`
/// (`bench/harbor_adapter/stella_harbor/__init__.py`), whose ambient check
/// fails closed: an unregistered `STELLA_*` variable refuses the run rather
/// than reaching the container, so a benchmark arm exporting this without that
/// registration would select nothing at all.
pub(super) const ISOLATION_ENV: &str = "STELLA_CANDIDATE_ISOLATION";

/// The substrate this launch selects, and whatever the operator must be told
/// about how that was decided.
///
/// `env` is the raw variable, unset or set; `from_settings` is what the scope
/// chain resolved (which is [`CandidateIsolation::Worktree`] whenever the chain
/// could not be read at all).
///
/// An empty or whitespace-only variable reads as unset rather than as
/// `"worktree"`. `FOO=` is how a shell spells "I am not passing this", and
/// letting it silently outrank a workspace that wrote `copy-tree` down would
/// make an unset variable louder than a stated policy.
pub(super) fn selected(
    env: Option<&str>,
    from_settings: CandidateIsolation,
) -> (CandidateIsolation, Option<String>) {
    let Some(raw) = env.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return (from_settings, None);
    };
    match CandidateIsolation::from_token(raw) {
        Ok(chosen) => (chosen, None),
        Err(why) => (
            CandidateIsolation::Worktree,
            Some(format!(
                "{ISOLATION_ENV}: {why}. Candidates run on git worktrees"
            )),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **#4510's witness.** The variable reaches the selector with no settings
    /// file behind it, which is the whole state a benchmark launcher runs in.
    #[test]
    fn the_environment_selects_copy_tree_where_no_settings_scope_can() {
        let (chosen, notice) = selected(Some("copy-tree"), CandidateIsolation::Worktree);
        assert_eq!(chosen, CandidateIsolation::CopyTree);
        assert!(notice.is_none(), "{notice:?}");
    }

    /// The narrower statement wins in both directions, or it is not a
    /// per-launch override — it is a second default.
    #[test]
    fn the_environment_outranks_a_workspace_that_says_otherwise() {
        assert_eq!(
            selected(Some("worktree"), CandidateIsolation::CopyTree).0,
            CandidateIsolation::Worktree
        );
    }

    #[test]
    fn an_unset_or_empty_variable_leaves_the_workspace_to_decide() {
        assert_eq!(
            selected(None, CandidateIsolation::CopyTree).0,
            CandidateIsolation::CopyTree
        );
        assert_eq!(
            selected(Some(""), CandidateIsolation::CopyTree).0,
            CandidateIsolation::CopyTree
        );
        assert_eq!(
            selected(Some("  "), CandidateIsolation::CopyTree).0,
            CandidateIsolation::CopyTree
        );
    }

    /// A word this build cannot read must not become the substrate that
    /// overwrites a tree, and must not be swallowed either.
    #[test]
    fn a_token_that_did_not_parse_falls_back_out_loud() {
        let (chosen, notice) = selected(Some("copytree"), CandidateIsolation::CopyTree);
        assert_eq!(chosen, CandidateIsolation::Worktree);
        let notice = notice.expect("a fallback the operator is not told about is a silent drop");
        assert!(notice.contains(ISOLATION_ENV), "{notice}");
        assert!(notice.contains("copytree"), "{notice}");
        assert!(
            notice.contains("copy-tree"),
            "names the word it accepts: {notice}"
        );
    }

    /// The settings key and the variable parse one vocabulary. Two `match`
    /// arms over the same two words is how a token gets accepted on one
    /// surface and refused on the other.
    #[test]
    fn both_spellings_of_the_setting_read_the_same_tokens() {
        for token in ["worktree", "copy-tree"] {
            let from_env = selected(Some(token), CandidateIsolation::Worktree).0;
            let from_file: CandidateIsolation =
                serde_json::from_value(serde_json::Value::String(token.to_string()))
                    .expect("the settings deserializer accepts what the variable accepts");
            assert_eq!(from_env, from_file, "{token}");
        }
        assert!(
            serde_json::from_value::<CandidateIsolation>(serde_json::json!("copytree")).is_err(),
            "and refuses what the variable refuses"
        );
    }
}
