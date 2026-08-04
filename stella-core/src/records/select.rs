//! Per-turn `applies_to` selection for the volatile channel.
//!
//! [`Steering::applies_to`][super::super::ingest::record::Steering] has always
//! been parsed, validated ([`super::validate`] checks its task vocabulary and
//! uses its paths for conflict detection), and displayed (`stella context
//! explain` prints it as the record's scope) — but until this module nothing
//! ever *matched* it against a turn, so a scoped record steered exactly like an
//! unscoped one. This is the missing consumer: given the facts of one turn, is
//! this record worth its tokens right now?
//!
//! # What it gates, and what it deliberately does not
//!
//! Selection applies to the **volatile channel only** — `may`/`info` records
//! and anything the truth sweep demoted. Those are re-rendered per turn by
//! design, so per-turn facts can drive them. `must`/`should` records ride the
//! byte-stable cached prefix, which is built once per session and must not
//! vary with the turn ([`super::render`] on why); their `applies_to` still
//! feeds conflict detection and `stella context explain`, and gates them here
//! the day the sweep demotes them into the volatile channel.
//!
//! # Matching semantics
//!
//! An absent or empty `applies_to` matches every turn — same reading as
//! [`super::validate`]'s conflict detection, where an unscoped record overlaps
//! everything. A non-empty one applies when **any populated dimension
//! matches** (disjunctive across dimensions, like within them): a record
//! scoped `{ paths = ["deny.toml"], tasks = ["install"] }` is relevant to a
//! turn that names the file *or* to one that is installing something —
//! requiring both would hide a licensing constraint from the `cargo add` turn
//! it exists for.
//!
//! Per dimension:
//!
//! - **paths** match the path-shaped tokens the turn's prompt names, with the
//!   same `match_glob` dialect guards and hooks use — `*` is the only
//!   wildcard and it crosses `/`. A pattern with no literal part (`*`, `**`)
//!   constrains nothing and matches unconditionally, echoing the guard lint
//!   that flags such globs as matching every value.
//! - **tasks** and single-word **keywords** match as whole words of the prompt
//!   text, case-insensitively — `ci` must not fire on "circle". A turn carries
//!   no explicit task label today, so the prompt's own words are the signal;
//!   when a labelled task plane exists it belongs in [`TurnFacts`].
//! - Multi-word **keywords** (`"docker compose"`) match as case-insensitive
//!   substrings, because word-splitting would never reassemble them.

use super::super::glob::match_glob;
use super::super::ingest::record::AppliesTo;

/// The facts of one turn that scoped records are matched against.
///
/// Assembled by the caller (the CLI's recall plane), because discovering them —
/// tokenizing a prompt, deciding what counts as path-shaped — is I/O-adjacent
/// policy, while matching is pure and lives here.
#[derive(Debug, Clone, Copy, Default)]
pub struct TurnFacts<'a> {
    /// The turn's prompt/goal text, matched by tasks and keywords.
    pub text: &'a str,
    /// Path-shaped tokens the prompt names (`deny.toml`, `src/api/mod.rs`),
    /// workspace-relative, matched by path globs.
    pub paths: &'a [String],
}

/// Whether a record scoped by `applies_to` applies to this turn.
///
/// `None` and empty both mean unscoped — the record applies unconditionally.
pub fn applies_this_turn(applies_to: Option<&AppliesTo>, facts: &TurnFacts<'_>) -> bool {
    let Some(applies) = applies_to.filter(|applies| !applies.is_empty()) else {
        return true;
    };
    paths_match(&applies.paths, facts.paths)
        || applies
            .tasks
            .iter()
            .any(|task| word_in_text(facts.text, task))
        || applies.keywords.iter().any(|keyword| {
            if keyword.split_whitespace().nth(1).is_some() {
                contains_ignore_case(facts.text, keyword)
            } else {
                word_in_text(facts.text, keyword)
            }
        })
}

/// Any pattern matching any turn path. A pattern with no literal part (`*`,
/// `**`) matches even a turn that names no paths at all: it constrains
/// nothing, so it cannot be the reason a record stays out.
fn paths_match(patterns: &[String], turn_paths: &[String]) -> bool {
    patterns.iter().any(|pattern| {
        let unconstrained = !pattern.is_empty() && pattern.chars().all(|c| c == '*');
        unconstrained || turn_paths.iter().any(|path| match_glob(pattern, path))
    })
}

/// Case-insensitive whole-word match: `word` appears in `text` delimited by
/// non-alphanumeric characters. ASCII case-folding only — task names come from
/// the ratified [`KNOWN_TASKS`][super::KNOWN_TASKS] vocabulary and keywords
/// are ASCII identifiers in practice.
fn word_in_text(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    text.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .any(|token| token.eq_ignore_ascii_case(word))
}

/// Case-insensitive substring match, for multi-word keywords.
fn contains_ignore_case(text: &str, needle: &str) -> bool {
    text.to_lowercase().contains(&needle.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scoped(paths: &[&str], tasks: &[&str], keywords: &[&str]) -> AppliesTo {
        AppliesTo {
            paths: paths.iter().map(ToString::to_string).collect(),
            tasks: tasks.iter().map(ToString::to_string).collect(),
            keywords: keywords.iter().map(ToString::to_string).collect(),
        }
    }

    fn facts<'a>(text: &'a str, paths: &'a [String]) -> TurnFacts<'a> {
        TurnFacts { text, paths }
    }

    #[test]
    fn absent_or_empty_scope_applies_to_every_turn() {
        let no_facts = facts("", &[]);
        assert!(applies_this_turn(None, &no_facts));
        assert!(applies_this_turn(Some(&AppliesTo::default()), &no_facts));
    }

    #[test]
    fn a_path_glob_matches_a_turn_that_names_the_file() {
        let scope = scoped(&["deny.toml"], &[], &[]);
        let named = vec!["deny.toml".to_string()];
        assert!(applies_this_turn(
            Some(&scope),
            &facts("widen the allowlist in deny.toml", &named)
        ));
        assert!(!applies_this_turn(
            Some(&scope),
            &facts("refactor the parser", &[])
        ));
    }

    #[test]
    fn a_directory_glob_matches_files_beneath_it() {
        let scope = scoped(&["src/api/**"], &[], &[]);
        let inside = vec!["src/api/handler.rs".to_string()];
        let outside = vec!["src/cli/main.rs".to_string()];
        assert!(applies_this_turn(Some(&scope), &facts("", &inside)));
        assert!(!applies_this_turn(Some(&scope), &facts("", &outside)));
    }

    #[test]
    fn an_all_star_path_pattern_constrains_nothing() {
        // `paths = ["**"]` spells "everywhere" — it must apply even on a turn
        // that names no path at all, matching the guard lint's reading that an
        // all-star glob has no literal part and matches every value.
        let scope = scoped(&["**"], &["install"], &[]);
        assert!(applies_this_turn(
            Some(&scope),
            &facts("summarize the readme", &[])
        ));
    }

    #[test]
    fn a_task_matches_as_a_whole_word_of_the_prompt() {
        let scope = scoped(&[], &["ci"], &[]);
        assert!(applies_this_turn(
            Some(&scope),
            &facts("why is CI red on main?", &[])
        ));
        assert!(
            !applies_this_turn(Some(&scope), &facts("draw a circle", &[])),
            "\"ci\" must not fire inside \"circle\""
        );
    }

    #[test]
    fn dimensions_are_disjunctive_across_not_conjunctive() {
        // The licensing dogfood record: relevant when the turn names the file
        // OR when it is installing something. Requiring both would hide it
        // from the `cargo add` turn it exists for.
        let scope = scoped(&["deny.toml"], &["install"], &[]);
        assert!(applies_this_turn(
            Some(&scope),
            &facts("install leftpad for the parser", &[])
        ));
    }

    #[test]
    fn keywords_match_words_and_multi_word_keywords_match_substrings() {
        let word = scoped(&[], &[], &["staging"]);
        assert!(applies_this_turn(
            Some(&word),
            &facts("deploy this to Staging tonight", &[])
        ));
        assert!(!applies_this_turn(
            Some(&word),
            &facts("restage the commit", &[])
        ));

        let phrase = scoped(&[], &[], &["docker compose"]);
        assert!(applies_this_turn(
            Some(&phrase),
            &facts("bring the stack up with Docker Compose", &[])
        ));
        assert!(!applies_this_turn(
            Some(&phrase),
            &facts("compose a docker image name", &[])
        ));
    }

    #[test]
    fn a_scoped_record_sits_out_a_turn_with_no_matching_fact() {
        let scope = scoped(&["rust-toolchain.toml"], &["build", "ci", "release"], &[]);
        assert!(!applies_this_turn(
            Some(&scope),
            &facts("write a haiku about the ocean", &[])
        ));
    }
}
