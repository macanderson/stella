//! How the loop signs what it writes.
//!
//! Everything an autonomous loop puts into a shared repository should say so.
//! A commit, a pull request description, an issue, an issue comment — a person
//! reading any of them a month later needs to know whether a human wrote it,
//! and that is not a thing to leave to a reader's guess about tone.
//!
//! # The rule is exact, because "roughly a footer" is not a contract
//!
//! [`sign`] appends the signature as a **horizontal rule footer**: a blank
//! line, `---`, a newline, then the text. So a signed body always ends
//!
//! ```text
//! <body>
//!
//! ---
//! created by stella*
//! ```
//!
//! Trailing whitespace on the body is removed first, so the result does not
//! depend on whether the caller happened to end with a newline — precisely the
//! difference that would otherwise produce two near-identical formats across
//! four surfaces with no way to tell which is right.
//! `the_signature_is_a_horizontal_rule_footer` is the witness.
//!
//! **The separator is fixed and the text is not.** A distribution changes what
//! the footer *says*; it does not get to change whether there is a rule above
//! it, because that is what makes a signed body recognisable at a glance
//! across every surface and every deployment.
//!
//! # Configurable per surface, and separately
//!
//! The four surfaces are separate fields rather than one string, because they
//! are read in different places and a deployment will want different words:
//! a commit trailer is scanned by tooling, an issue comment is read by a
//! person. A field left empty signs nothing — the loop stays silent on that
//! surface rather than falling back to a default the operator did not choose.
//!
//! The branch prefix lives here too. It is the same question wearing a
//! different hat: *what does this repository see when the loop touches it?*

use serde::{Deserialize, Serialize};

/// What the footer says, unless a workspace changes it.
///
/// One string for all four surfaces by default: a reader should recognise the
/// same mark on a commit, a pull request, an issue and a comment without
/// having to learn four phrasings. A deployment that wants them to differ sets
/// them separately.
pub const SIGNATURE: &str = "created by stella*";

/// What separates a body from its signature: a blank line, then a rule.
///
/// Fixed rather than configurable — see the module docs.
const SEPARATOR: &str = "\n\n---\n";

/// The default branch prefix.
///
/// Every branch the loop opens is namespaced, so a human scanning `git branch`
/// can tell at a glance which refs are theirs. It is also what keeps the loop's
/// branches out of the fleet's namespace, where `stella fleet gc` would believe
/// it owned them.
pub const DEFAULT_BRANCH_PREFIX: &str = "stella/";

/// What the loop appends to each thing it writes, and how it names branches.
///
/// Source-tracked so it travels with the repository, and rewritable by an
/// installed plugin: a downstream distribution signs in its own name, and that
/// is a configuration change rather than a fork.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Attribution {
    /// Appended to every commit message the loop causes to be written.
    pub commit: String,
    /// Appended to every pull request description it opens.
    pub pull_request: String,
    /// Appended to every issue it files.
    pub issue: String,
    /// Appended to every issue comment it posts.
    pub issue_comment: String,
    /// Prefix for every branch it creates.
    ///
    /// A prefix that does not end in `/` is used verbatim rather than
    /// corrected: some teams namespace with `-`, and silently rewriting an
    /// operator's choice is worse than honouring an unusual one.
    pub branch_prefix: String,
}

impl Default for Attribution {
    fn default() -> Self {
        Self {
            commit: SIGNATURE.to_owned(),
            pull_request: SIGNATURE.to_owned(),
            issue: SIGNATURE.to_owned(),
            issue_comment: SIGNATURE.to_owned(),
            branch_prefix: DEFAULT_BRANCH_PREFIX.to_owned(),
        }
    }
}

impl Attribution {
    /// The branch prefix, falling back to the default when unset.
    ///
    /// An empty prefix is treated as unset rather than as "no prefix": an
    /// unnamespaced branch is how the loop's refs end up indistinguishable
    /// from a human's, and an operator who genuinely wants that can say so
    /// with a prefix of their own choosing.
    #[must_use]
    pub fn branch_prefix(&self) -> &str {
        if self.branch_prefix.trim().is_empty() {
            DEFAULT_BRANCH_PREFIX
        } else {
            &self.branch_prefix
        }
    }
}

/// Append `signature` to `body` as a horizontal rule footer.
///
/// The result ends `<body>\n\n---\n<signature>`. Trailing whitespace on the
/// body is normalized first, so two callers whose bodies differ only in how
/// they terminated produce identical output.
///
/// An empty signature returns the body untouched — including its own trailing
/// whitespace, because a caller that signs nothing has not asked for its text
/// to be reformatted.
///
/// # Examples
///
/// The join is exactly one blank line and a horizontal rule, whatever the body
/// ended with — so two callers that differ only in trailing whitespace produce
/// the identical result:
///
/// ```
/// use stella_autonomy::sign;
///
/// assert_eq!(
///     sign("body", "created by stella*"),
///     "body\n\n---\ncreated by stella*"
/// );
/// assert_eq!(
///     sign("body\n\n", "created by stella*"),
///     "body\n\n---\ncreated by stella*"
/// );
/// ```
///
/// An empty body gets the rule with nothing above it:
///
/// ```
/// use stella_autonomy::sign;
///
/// assert_eq!(sign("", "created by stella*"), "---\ncreated by stella*");
/// ```
#[must_use]
pub fn sign(body: &str, signature: &str) -> String {
    if signature.trim().is_empty() {
        return body.to_owned();
    }
    let trimmed = body.trim_end();
    if trimmed.is_empty() {
        // An empty body gets the footer without a leading blank line: a rule
        // with nothing above it is a rule separating nothing.
        return format!("---\n{}", signature.trim());
    }
    format!("{trimmed}{SEPARATOR}{}", signature.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The witness.** A blank line, a rule, then the text — whatever the body
    /// ended with, so the four surfaces cannot drift into four near-identical
    /// formats.
    #[test]
    fn the_signature_is_a_horizontal_rule_footer() {
        for body in [
            "the body",
            "the body\n",
            "the body\n\n",
            "the body\n\n\n   \t\n",
            "the body   ",
        ] {
            assert_eq!(
                sign(body, SIGNATURE),
                "the body\n\n---\ncreated by stella*",
                "body {body:?} must produce the same footer"
            );
        }
    }

    /// A multi-line body keeps its own interior line breaks; only the join is
    /// normalized.
    #[test]
    fn interior_line_breaks_are_left_alone() {
        assert_eq!(
            sign("first\n\nsecond\n", "sig"),
            "first\n\nsecond\n\n---\nsig"
        );
    }

    /// An operator who clears a surface's signature gets silence on it, not a
    /// default they did not choose.
    #[test]
    fn an_empty_signature_signs_nothing_and_reformats_nothing() {
        assert_eq!(sign("the body\n\n", ""), "the body\n\n");
        assert_eq!(sign("the body", "   "), "the body");
    }

    /// A rule with nothing above it separates nothing, so an empty body gets
    /// the footer without a leading blank line.
    #[test]
    fn an_empty_body_gets_the_footer_without_a_leading_blank_line() {
        assert_eq!(sign("", "sig"), "---\nsig");
        assert_eq!(sign("  \n ", "sig"), "---\nsig");
    }

    /// The default is `stella/`, and it is what an unset prefix falls back to.
    #[test]
    fn the_branch_prefix_defaults_to_stella() {
        assert_eq!(Attribution::default().branch_prefix(), "stella/");

        let unset = Attribution {
            branch_prefix: String::new(),
            ..Attribution::default()
        };
        assert_eq!(unset.branch_prefix(), "stella/");
    }

    /// An unusual prefix is honoured rather than corrected.
    #[test]
    fn an_operator_prefix_is_used_verbatim() {
        let theirs = Attribution {
            branch_prefix: "oxagen-".to_owned(),
            ..Attribution::default()
        };
        assert_eq!(theirs.branch_prefix(), "oxagen-");
    }

    /// A distribution changes what the footer says; it does not change that
    /// there is a rule above it. That is what makes a signed body recognisable
    /// across every surface and every deployment.
    #[test]
    fn the_separator_is_fixed_even_when_the_text_is_not() {
        assert!(sign("body", "Created by oxagen.").starts_with("body\n\n---\n"));
        assert!(sign("body", "anything at all").starts_with("body\n\n---\n"));
    }

    /// The four surfaces are independent: changing one does not move another.
    #[test]
    fn each_surface_signs_separately() {
        let a = Attribution {
            commit: "c".into(),
            pull_request: "p".into(),
            issue: "i".into(),
            issue_comment: "n".into(),
            ..Attribution::default()
        };
        assert_eq!(sign("x", &a.commit), "x\n\n---\nc");
        assert_eq!(sign("x", &a.pull_request), "x\n\n---\np");
        assert_eq!(sign("x", &a.issue), "x\n\n---\ni");
        assert_eq!(sign("x", &a.issue_comment), "x\n\n---\nn");
    }

    /// A partial manifest fills in the defaults for what it did not mention —
    /// so a plugin rewriting one surface does not silently blank the rest.
    #[test]
    fn a_partial_manifest_keeps_the_other_surfaces() {
        let parsed: Attribution =
            serde_json::from_str(r#"{"commit":"Created by oxagen."}"#).expect("deserialize");
        assert_eq!(parsed.commit, "Created by oxagen.");
        assert_eq!(parsed.issue, SIGNATURE);
        assert_eq!(parsed.branch_prefix(), "stella/");
    }

    #[test]
    fn attribution_round_trips_through_json() {
        let a = Attribution::default();
        let json = serde_json::to_string(&a).expect("serialize");
        let back: Attribution = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(a, back);
    }
}
