//! How the loop signs what it writes.
//!
//! Everything an autonomous loop puts into a shared repository should say so.
//! A commit, a pull request description, an issue, an issue comment — a person
//! reading any of them a month later needs to know whether a human wrote it,
//! and that is not a thing to leave to a reader's guess about tone.
//!
//! # The rule is exact, because "roughly one blank line" is not a contract
//!
//! [`sign`] appends the signature **exactly one line break after the last
//! character of the body**. Not two, not "a blank line", not "whatever the
//! caller already had". Trailing whitespace on the body is removed first, so
//! the result does not depend on whether the caller happened to end with a
//! newline — which is precisely the kind of difference that produces two
//! near-identical formats across four surfaces and no way to tell which is
//! right. `exactly_one_line_break_separates_body_and_signature` is the witness.
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
            commit: "Created by stella.".to_owned(),
            pull_request: "Created by stella.".to_owned(),
            issue: "Filed by stella.".to_owned(),
            issue_comment: "Posted by stella.".to_owned(),
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

/// Append `signature` exactly one line break after the last character of
/// `body`.
///
/// An empty signature returns the body untouched — including its own trailing
/// whitespace, because a caller that signs nothing has not asked for its text
/// to be reformatted.
///
/// # Examples
///
/// The join is exactly one line break, whatever the body ended with — so two
/// callers that differ only in trailing whitespace produce the identical
/// result:
///
/// ```
/// use stella_autonomy::sign;
///
/// assert_eq!(sign("body", "Created by stella."), "body\nCreated by stella.");
/// assert_eq!(sign("body\n\n", "Created by stella."), "body\nCreated by stella.");
/// ```
#[must_use]
pub fn sign(body: &str, signature: &str) -> String {
    if signature.trim().is_empty() {
        return body.to_owned();
    }
    let trimmed = body.trim_end();
    if trimmed.is_empty() {
        // A body that is entirely empty gets the signature alone rather than a
        // leading blank line.
        return signature.trim().to_owned();
    }
    format!("{trimmed}\n{}", signature.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The witness.** Exactly one line break, whatever the body ended with —
    /// so the four surfaces cannot drift into four near-identical formats.
    #[test]
    fn exactly_one_line_break_separates_body_and_signature() {
        let sig = "Created by stella.";

        for body in [
            "the body",
            "the body\n",
            "the body\n\n",
            "the body\n\n\n   \t\n",
            "the body   ",
        ] {
            assert_eq!(
                sign(body, sig),
                "the body\nCreated by stella.",
                "body {body:?} must produce exactly one line break"
            );
        }
    }

    /// A multi-line body keeps its own interior line breaks; only the join is
    /// normalized.
    #[test]
    fn interior_line_breaks_are_left_alone() {
        assert_eq!(sign("first\n\nsecond\n", "sig"), "first\n\nsecond\nsig");
    }

    /// An operator who clears a surface's signature gets silence on it, not a
    /// default they did not choose.
    #[test]
    fn an_empty_signature_signs_nothing_and_reformats_nothing() {
        assert_eq!(sign("the body\n\n", ""), "the body\n\n");
        assert_eq!(sign("the body", "   "), "the body");
    }

    #[test]
    fn an_empty_body_gets_the_signature_alone() {
        assert_eq!(sign("", "sig"), "sig");
        assert_eq!(sign("  \n ", "sig"), "sig");
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
        assert_eq!(sign("x", &a.commit), "x\nc");
        assert_eq!(sign("x", &a.pull_request), "x\np");
        assert_eq!(sign("x", &a.issue), "x\ni");
        assert_eq!(sign("x", &a.issue_comment), "x\nn");
    }

    /// A partial manifest fills in the defaults for what it did not mention —
    /// so a plugin rewriting one surface does not silently blank the rest.
    #[test]
    fn a_partial_manifest_keeps_the_other_surfaces() {
        let parsed: Attribution =
            serde_json::from_str(r#"{"commit":"Created by oxagen."}"#).expect("deserialize");
        assert_eq!(parsed.commit, "Created by oxagen.");
        assert_eq!(parsed.issue, "Filed by stella.");
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
