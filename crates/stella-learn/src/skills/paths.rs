//! One way to spell a skill path.
//!
//! Skill paths reach this module as plain text. `SkillFile`'s `path` is one. So
//! are the two dirs in `LoadSkillsOptions`. So are `target_dir` and
//! `occupied_paths`.
//!
//! Three readers take those strings apart. The slug is the last chunk after a
//! `/`. The tier is the dir a file sits in. The guard joins a path with `/`,
//! then looks for it on disk. All three read `/`. Nothing said so.
//!
//! The one real source spells them with `Path::display()`. On Windows they come
//! back with `\`. Now the slug is the whole path. Now each file looks like a
//! `Workspace` file. Worse, the guard finds no match. So a new skill is written
//! over one the user wrote by hand.
//!
//! [`to_slash`] fixes this at the edge. It folds one mark, and the caller says
//! which one. A Unix file name may hold a `\`. A Unix caller must keep it.

use std::borrow::Cow;

/// Rewrite `separator` to `/`. Borrows when there is nothing to rewrite.
///
/// The caller names the mark to fold. It is not `std::path::MAIN_SEPARATOR`, for
/// two reasons. `stella-core` takes no file API at all (AGENTS.md rule 1). And
/// the caller is the half that knows what its own platform put in the string. A
/// Unix caller passes `/` and gets its input back whole, `\` in names and all.
pub fn to_slash(path: &str, separator: char) -> Cow<'_, str> {
    if separator == '/' || !path.contains(separator) {
        return Cow::Borrowed(path);
    }
    Cow::Owned(path.replace(separator, "/"))
}

/// Whether two skill paths name one file, reading `\` and `/` alike.
///
/// This is the guard in [`super::decide_auto_creation`]. It is the one reader
/// whose miss kills a file: the write lands on a skill the user wrote, and no
/// later run brings it back. So it folds both marks. It folds them everywhere,
/// not just where `\` splits a path.
///
/// The price on Unix is small. Two files whose paths differ only in `\` against
/// `/` read as one, so a write that could have gone ahead is refused. Nothing is
/// lost. That is why the slug and the tier still read the rule instead.
pub fn same_skill_path(a: &str, b: &str) -> bool {
    // Compared byte-wise: both separators are ASCII, UTF-8 puts no ASCII byte
    // inside a multi-byte character, and equal lengths plus equal bytes is the
    // whole test.
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .all(|(x, y)| x == y || (is_separator(x) && is_separator(y)))
}

fn is_separator(byte: u8) -> bool {
    byte == b'/' || byte == b'\\'
}

#[cfg(test)]
mod tests {
    use super::super::{LoadSkillsOptions, SkillOrigin, default_origin_for, skill_name_from_path};
    use super::*;

    fn windows_options() -> LoadSkillsOptions {
        LoadSkillsOptions {
            workspace_skills_dir: r"C:\ws\.stella\skills".to_string(),
            user_skills_dir: r"C:\Users\ada\.stella\skills".to_string(),
        }
    }

    fn folded(options: &LoadSkillsOptions) -> LoadSkillsOptions {
        LoadSkillsOptions {
            workspace_skills_dir: to_slash(&options.workspace_skills_dir, '\\').into_owned(),
            user_skills_dir: to_slash(&options.user_skills_dir, '\\').into_owned(),
        }
    }

    #[test]
    fn a_unix_separator_leaves_the_path_alone() {
        let path = "/w/.stella/skills/foo.md";
        assert!(matches!(to_slash(path, '/'), Cow::Borrowed(_)));
        assert_eq!(to_slash(path, '/'), path);
    }

    #[test]
    fn a_backslash_in_a_unix_file_name_survives() {
        // `a\b.md` is one legal file name on Unix, so a Unix caller — which
        // passes its own `/` — gets it back whole.
        assert_eq!(to_slash(r"/w/skills/a\b.md", '/'), r"/w/skills/a\b.md");
    }

    #[test]
    fn a_windows_path_folds_to_slashes() {
        assert_eq!(
            to_slash(r"C:\ws\.stella\skills\foo.md", '\\'),
            "C:/ws/.stella/skills/foo.md"
        );
    }

    #[test]
    fn a_folded_path_reads_the_slug_and_the_origin() {
        let options = folded(&windows_options());

        let workspace = to_slash(r"C:\ws\.stella\skills\foo\SKILL.md", '\\').into_owned();
        assert_eq!(skill_name_from_path(&workspace), "foo");
        assert_eq!(
            default_origin_for(&workspace, &options),
            SkillOrigin::Workspace
        );

        let user = to_slash(r"C:\Users\ada\.stella\skills\bar.md", '\\').into_owned();
        assert_eq!(skill_name_from_path(&user), "bar");
        assert_eq!(default_origin_for(&user, &options), SkillOrigin::User);
    }

    #[test]
    fn an_unfolded_windows_path_is_what_the_readers_get_wrong() {
        // What a caller that skips the fold hands them, kept as the record of
        // what this module is for: the slug is the whole path, and a user-global
        // skill reports as the workspace's own.
        let user = r"C:\Users\ada\.stella\skills\bar.md";
        assert_eq!(
            skill_name_from_path(user),
            r"C:\Users\ada\.stella\skills\bar"
        );
        assert_eq!(
            default_origin_for(user, &windows_options()),
            SkillOrigin::Workspace
        );
    }

    #[test]
    fn separators_compare_alike_and_names_still_differ() {
        assert!(same_skill_path(
            r"C:\ws\.stella\skills\foo.md",
            "C:/ws/.stella/skills/foo.md"
        ));
        assert!(!same_skill_path(
            "C:/ws/.stella/skills/foo.md",
            "C:/ws/.stella/skills/bar.md"
        ));
        assert!(!same_skill_path("/w/skills/foo.md", "/w/skills/foo.mdx"));
    }
}
