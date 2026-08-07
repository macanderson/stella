//! Joining the two diff channels.
//!
//! The pipeline reads two descriptions of what a turn did, and they answer
//! different questions. The configured diff diagnostic (`git diff`) reports how
//! the tree now differs from a commit — authoritative about what SURVIVED, and
//! structurally blind to an untracked file or to a workspace with no repository
//! at all. The authored channel reports what the file tools were asked to
//! write — authoritative about INTENT, and silent about whether a later step
//! clobbered it.
//!
//! Neither subsumes the other, so this module concatenates them rather than
//! choosing. The whole reason that is legal is that both halves are unified-diff
//! shaped: every consumer downstream parses the joined string with one parser
//! and cannot tell which half a hunk came from.

/// Labels the authored section when it is appended to a tree-state diff.
///
/// Starts with `#` on purpose. `witness::warrant::changed_paths` treats the
/// remainder of any `--- ` or `+++ ` line as a file path, and
/// `verify::mutation` reads `+`-prefixed lines as mutable content — a header in
/// either shape would be parsed as a bogus path or a bogus mutation target. A
/// comment prefix is inert to every parser that reads this text.
const AUTHORED_SECTION_HEADER: &str =
    "# --- changes recorded by the file tools at write time (authorship, not tree state) ---";

/// Join what the tree says with what the tools recorded.
///
/// The two answer different questions and neither subsumes the other: the probe
/// reports how the tree now differs from a commit (authoritative about what
/// SURVIVED, blind to untracked files and to workspaces with no repository),
/// while the authored channel reports what the agent asked to be written
/// (authoritative about INTENT, silent about whether a later step clobbered
/// it). Concatenating them — probe first, since on-disk state is the stronger
/// claim where it exists — is what lets a verifier see both.
///
/// The authored text is unified-diff shaped precisely so this concatenation is
/// legal: every consumer downstream (`changed_paths`, `changed_lines`,
/// `mutants_from_diff`) parses the joined string with one parser and cannot
/// tell which half a hunk came from, which is the point.
/// `already_rendered` names the untracked paths the probe half now carries the
/// CONTENT of, not merely a marker for. Those chunks are dropped from the
/// authored half: both halves would otherwise render the same file, spending a
/// token budget twice to say one thing, and leaving a verifier to wonder
/// whether it is looking at one change or two. The probe's copy is the one
/// kept, by the same precedence this module already applies — on-disk state is
/// the stronger claim about what survived.
///
/// This is the text-side analogue of the `max` (never a sum) that
/// `absorb_probe` applies to the two channels' line counts, and it exists for
/// the same reason: they are two views of one change, not two changes.
pub(super) fn splice_authored(
    probe_text: String,
    authored: &crate::ports::AuthoredChange,
    already_rendered: &[String],
) -> String {
    if authored.is_empty() {
        return probe_text;
    }
    // Reuses the verifier-prompt chunk dropper rather than a second parser:
    // one definition of "a chunk for this path" keeps the two callers from
    // disagreeing about what a chunk boundary is.
    let authored_text = crate::verify::strip_witness_hunks(&authored.text, already_rendered).diff;
    if authored_text.trim().is_empty() {
        return probe_text;
    }
    if probe_text.trim().is_empty() {
        return authored_text;
    }
    format!("{probe_text}\n{AUTHORED_SECTION_HEADER}\n{authored_text}")
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::ports::AuthoredChange;

    /// A created file, as the ledger renders it.
    ///
    /// The body carries a comparison on purpose: `verify::mutation` swaps
    /// operators, so a fixture with no operator would pass
    /// [`an_authored_create_yields_mutants_where_a_marker_yielded_none`] for the
    /// wrong reason — proving only that the content was unmutatable, not that the
    /// header was parsed.
    fn authored_create() -> AuthoredChange {
        AuthoredChange {
            text:
                "--- /dev/null\n+++ b/solution.py\n@@ -1,0 +1,2 @@\n+def ok(n):\n+    return n >= 2\n"
                    .to_string(),
            lines: 2,
        }
    }

    #[test]
    fn an_empty_authored_change_leaves_the_probe_text_untouched() {
        let probe = "diff --git a/x b/x\n".to_string();
        assert_eq!(
            splice_authored(probe.clone(), &AuthoredChange::default(), &[]),
            probe
        );
    }

    /// The duplication guard: once the probe carries an untracked file's
    /// content, the authored channel's copy of that same file is redundant and
    /// must be dropped. Both halves rendering it would spend the diff budget
    /// twice on one change and leave a verifier reading the same file twice.
    #[test]
    fn a_file_the_probe_already_rendered_is_not_repeated_by_the_authored_half() {
        let probe = "+ untracked change: solution.py (+2 lines)\n\
                     @@ -0,0 +1,2 @@\n+def ok(n):\n+    return n >= 2";
        let spliced = splice_authored(
            probe.to_string(),
            &authored_create(),
            &["solution.py".to_string()],
        );
        assert_eq!(spliced, probe, "the authored half added nothing: {spliced}");
        assert_eq!(
            spliced.matches("def ok(n):").count(),
            1,
            "the content appears exactly once: {spliced}"
        );
    }

    /// …and the drop is per path. A file only the tools saw is still spliced,
    /// which is the whole reason the authored channel exists.
    #[test]
    fn a_file_the_probe_did_not_render_is_still_spliced() {
        let probe = "+ untracked change: other.py (+1 lines)\n@@ -0,0 +1 @@\n+x = 1";
        let spliced = splice_authored(
            probe.to_string(),
            &authored_create(),
            &["other.py".to_string()],
        );
        assert!(spliced.contains(AUTHORED_SECTION_HEADER), "{spliced}");
        assert!(spliced.contains("def ok(n):"), "{spliced}");
    }

    /// The Terminal-Bench case: the probe could not look, so the authored diff is
    /// the entire answer and must not be buried under a joining header.
    #[test]
    fn a_blank_probe_yields_the_authored_diff_alone() {
        let spliced = splice_authored(String::new(), &authored_create(), &[]);
        assert!(spliced.starts_with("--- /dev/null"), "{spliced}");
        assert!(!spliced.contains(AUTHORED_SECTION_HEADER), "{spliced}");
    }

    /// Where both channels have something to say, the tree comes first — on-disk
    /// state is the stronger claim about what survived — and the authored section
    /// is labelled so a reader knows which question each half answers.
    #[test]
    fn both_channels_are_joined_with_the_tree_first() {
        let probe = "diff --git a/lib.rs b/lib.rs\n--- a/lib.rs\n+++ b/lib.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n";
        let spliced = splice_authored(probe.to_string(), &authored_create(), &[]);
        let tree = spliced.find("a/lib.rs").expect("tree half");
        let header = spliced.find(AUTHORED_SECTION_HEADER).expect("the label");
        let authored = spliced.find("b/solution.py").expect("authored half");
        assert!(tree < header && header < authored, "{spliced}");
    }

    /// The header must be inert to every parser that reads this text. A `--- ` or
    /// `+++ ` prefix would be read as a file path by `warrant::changed_paths`, and
    /// a `+` prefix as mutable content by `verify::mutation`.
    #[test]
    fn the_section_header_cannot_be_parsed_as_a_path_or_a_change() {
        assert!(AUTHORED_SECTION_HEADER.starts_with('#'));
        let paths = crate::witness::warrant::changed_paths(AUTHORED_SECTION_HEADER);
        assert!(paths.is_empty(), "{paths:?}");
        let mutants = crate::verify::mutation::mutants_from_diff(AUTHORED_SECTION_HEADER, &[]);
        assert!(mutants.is_empty(), "{mutants:?}");
    }

    /// The knock-on this channel repairs, part one: the mutation audit.
    ///
    /// A marker line carries no `+++` header, so `current_path` never binds and the
    /// audit emits nothing — passing vacuously on exactly the workspaces where it
    /// is the only check left. The authored render restores it.
    #[test]
    fn an_authored_create_yields_mutants_where_a_marker_yielded_none() {
        let marker = "+ untracked change: solution.py (+2 lines)";
        assert!(
            crate::verify::mutation::mutants_from_diff(marker, &[]).is_empty(),
            "the old shape audits nothing — this is the defect being fixed"
        );
        let mutants = crate::verify::mutation::mutants_from_diff(&authored_create().text, &[]);
        assert!(!mutants.is_empty(), "the authored shape is auditable");
        assert!(
            mutants.iter().all(|m| m.path == "solution.py"),
            "{mutants:?}"
        );
    }

    /// The knock-on this channel repairs, part two: the warrant's path rules.
    #[test]
    fn an_authored_create_names_its_path_to_the_warrant() {
        let paths = crate::witness::warrant::changed_paths(&authored_create().text);
        assert_eq!(paths, vec!["solution.py".to_string()]);
    }

    /// A deletion points its right side at `/dev/null`, which the mutant builder
    /// reads as "there is nothing here to mutate" rather than trying to break lines
    /// that are gone.
    #[test]
    fn an_authored_delete_is_not_mutated() {
        let deleted = AuthoredChange {
            text: "--- a/gone.rs\n+++ /dev/null\n@@ -1,1 +1,0 @@\n-fn main() {}\n".to_string(),
            lines: 1,
        };
        assert!(
            crate::verify::mutation::mutants_from_diff(&deleted.text, &[]).is_empty(),
            "a removed line is not a mutation target"
        );
    }
}
