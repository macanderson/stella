// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Paths, which are the thing you most want and most cannot have —
//! `docs/design/diagnostics.md` §5.4.
//!
//! The single most-wanted log field in a coding agent, and the most dangerous:
//! a path carries a home directory name, a client name, a project name, and
//! sometimes a secret in a filename. [`PathClass`] is the compromise, and it is
//! enough for nearly every real diagnosis:
//!
//! ```json
//! { "class": "inside_workspace", "depth": 4, "ext": "rs" }
//! ```
//!
//! "The write was denied because the path was outside the workspace, four
//! levels deep, a `.rs` file" answers the operational question. The bytes of
//! the path do not.
//!
//! ## One place this is stricter than the design sketch
//!
//! §5.4 shows `"ext": "rs"` taken from the path. A raw extension is still
//! attacker-chosen text — `secrets.API_KEY_sk_live_…` is a legal filename, and
//! `read_file` takes its argument from a model — so an unrestricted `ext` would
//! be a content channel wearing a three-letter disguise. [`Extension`] is
//! therefore a **closed vocabulary** (§5.2's third row): a recognised extension
//! records as itself, and anything else records as `other`. The design's
//! example is unchanged for the case it shows, and the hole it would have left
//! is closed.

use std::path::{Component, Path, PathBuf};

use crate::field::{FieldValue, Loggable, StaticStr};
use crate::log_enum;

log_enum! {
    /// Where a path lives, which is the part of it that explains a decision.
    pub enum PathKind {
        /// Under the workspace root — the paths the agent is meant to touch.
        InsideWorkspace => "inside_workspace",
        /// Under the user's home directory but outside the workspace. The
        /// class that explains most denied writes.
        Home => "home",
        /// Under the system temp directory.
        Temp => "temp",
        /// Absolute, and none of the above: `/etc`, `/usr`, another user's
        /// home, a mounted volume.
        AbsoluteOther => "absolute_other",
        /// Relative, so it has no meaning without the working directory that
        /// resolved it — which is itself a fact worth recording.
        Relative => "relative",
    }
}

log_enum! {
    /// A file extension, from a closed vocabulary.
    ///
    /// The list is the extensions a coding agent actually makes decisions
    /// about. Anything unrecognised is [`Extension::Other`], which is the
    /// whole safety argument: an extension is attacker-chosen text, so it
    /// reaches a record only by matching something already written down here.
    ///
    /// Adding an entry is a one-line change and a deliberate one — the review
    /// question is "can this string carry content?", and for a fixed spelling
    /// the answer is no.
    pub enum Extension {
        Rs => "rs",
        Toml => "toml",
        Md => "md",
        Json => "json",
        Yaml => "yaml",
        Yml => "yml",
        Lock => "lock",
        Sh => "sh",
        Py => "py",
        Ts => "ts",
        Tsx => "tsx",
        Js => "js",
        Jsx => "jsx",
        Go => "go",
        C => "c",
        H => "h",
        Cpp => "cpp",
        Hpp => "hpp",
        Java => "java",
        Rb => "rb",
        Php => "php",
        Sql => "sql",
        Html => "html",
        Css => "css",
        Txt => "txt",
        Log => "log",
        Jsonl => "jsonl",
        Db => "db",
        Csv => "csv",
        Env => "env",
        Pem => "pem",
        Key => "key",
        Png => "png",
        Jpg => "jpg",
        Svg => "svg",
        /// A file with no extension at all — `Makefile`, `LICENSE`, a binary.
        None => "none",
        /// Anything not listed above. Deliberately unspecific: the alternative
        /// is echoing a filename fragment into a log.
        Other => "other",
    }
}

impl Extension {
    /// Classify the extension of `path` against the closed vocabulary.
    #[must_use]
    pub fn of(path: &Path) -> Self {
        let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
            return Self::None;
        };
        let lowered = ext.to_ascii_lowercase();
        [
            Self::Rs,
            Self::Toml,
            Self::Md,
            Self::Json,
            Self::Yaml,
            Self::Yml,
            Self::Lock,
            Self::Sh,
            Self::Py,
            Self::Ts,
            Self::Tsx,
            Self::Js,
            Self::Jsx,
            Self::Go,
            Self::C,
            Self::H,
            Self::Cpp,
            Self::Hpp,
            Self::Java,
            Self::Rb,
            Self::Php,
            Self::Sql,
            Self::Html,
            Self::Css,
            Self::Txt,
            Self::Log,
            Self::Jsonl,
            Self::Db,
            Self::Csv,
            Self::Env,
            Self::Pem,
            Self::Key,
            Self::Png,
            Self::Jpg,
            Self::Svg,
        ]
        .into_iter()
        .find(|known| known.as_static() == lowered)
        .unwrap_or(Self::Other)
    }
}

/// The anchors a path is classified against.
///
/// Explicit rather than ambient, so classification is a pure function of its
/// inputs and a test can pin every class without touching the environment —
/// the same reason invariant 2 pushes I/O to the edges. [`PathContext::detect`]
/// is the one constructor that reads the environment, and callers at an I/O
/// boundary are where it belongs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathContext {
    workspace_root: Option<PathBuf>,
    home: Option<PathBuf>,
    temp: Option<PathBuf>,
}

impl PathContext {
    /// Anchors supplied by the caller. Nothing is read from the environment.
    #[must_use]
    pub fn new(
        workspace_root: Option<PathBuf>,
        home: Option<PathBuf>,
        temp: Option<PathBuf>,
    ) -> Self {
        Self {
            workspace_root,
            home,
            temp,
        }
    }

    /// Anchors for the running process: the given workspace root, `$HOME` (or
    /// `%USERPROFILE%`), and the platform temp directory.
    #[must_use]
    pub fn detect(workspace_root: Option<PathBuf>) -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from);
        Self {
            workspace_root,
            home,
            temp: Some(std::env::temp_dir()),
        }
    }
}

/// What a record says about a path: where it lives, how deep it is, and what
/// kind of file it is. Never its bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathClass {
    class: PathKind,
    depth: u32,
    ext: Extension,
}

impl PathClass {
    /// Classify `path` against `context`.
    ///
    /// `depth` counts the normal components *below the anchor* for a path that
    /// has one (workspace, home, temp) and the components of the whole path
    /// otherwise. Under an anchor it answers "how far outside/inside", which is
    /// the operational question; without one there is nothing to be relative
    /// to.
    #[must_use]
    pub fn classify(path: &Path, context: &PathContext) -> Self {
        let ext = Extension::of(path);
        let anchored = [
            (context.workspace_root.as_deref(), PathKind::InsideWorkspace),
            (context.temp.as_deref(), PathKind::Temp),
            (context.home.as_deref(), PathKind::Home),
        ]
        .into_iter()
        .find_map(|(anchor, class)| {
            let anchor = anchor?;
            let rest = path.strip_prefix(anchor).ok()?;
            Some((class, depth_of(rest)))
        });

        // Temp before home on purpose: on macOS the per-user temp directory
        // lives under the home directory, and reporting a scratch file as
        // `home` would send a reader looking in the wrong place. The workspace
        // beats both, because a workspace inside `$HOME` is the normal case.
        let (class, depth) = anchored.unwrap_or_else(|| {
            let class = if path.is_absolute() {
                PathKind::AbsoluteOther
            } else {
                PathKind::Relative
            };
            (class, depth_of(path))
        });

        Self { class, depth, ext }
    }

    #[must_use]
    pub const fn class(&self) -> PathKind {
        self.class
    }

    #[must_use]
    pub const fn depth(&self) -> u32 {
        self.depth
    }

    #[must_use]
    pub const fn extension(&self) -> Extension {
        self.ext
    }
}

/// Normal components only: `.` and `..` do not deepen a path, and a root or
/// prefix component is not a level.
fn depth_of(path: &Path) -> u32 {
    let count = path
        .components()
        .filter(|component| matches!(component, Component::Normal(_)))
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

impl Loggable for PathClass {
    fn to_field(&self) -> FieldValue {
        FieldValue::Map(vec![
            (
                StaticStr::new("class"),
                FieldValue::Str(StaticStr::new(self.class.as_static())),
            ),
            (
                StaticStr::new("depth"),
                FieldValue::Uint(u64::from(self.depth)),
            ),
            (
                StaticStr::new("ext"),
                FieldValue::Str(StaticStr::new(self.ext.as_static())),
            ),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Fields;

    fn context() -> PathContext {
        PathContext::new(
            Some(PathBuf::from("/home/ada/work/stella")),
            Some(PathBuf::from("/home/ada")),
            Some(PathBuf::from("/tmp")),
        )
    }

    /// The record §5.4 specifies, byte for byte.
    #[test]
    fn a_workspace_path_records_the_shape_the_design_specifies() {
        let class = PathClass::classify(
            Path::new("/home/ada/work/stella/stella-diag/src/path.rs"),
            &context(),
        );
        let fields = Fields::new().with("path", class);
        assert_eq!(
            serde_json::to_string(&fields).expect("serialize"),
            r#"{"path":{"class":"inside_workspace","depth":3,"ext":"rs"}}"#
        );
    }

    #[test]
    fn each_anchor_gets_its_own_class() {
        let context = context();
        let cases = [
            (
                "/home/ada/work/stella/src/lib.rs",
                PathKind::InsideWorkspace,
            ),
            ("/home/ada/.ssh/id_ed25519", PathKind::Home),
            ("/tmp/stella-abc/scratch.txt", PathKind::Temp),
            ("/etc/passwd", PathKind::AbsoluteOther),
            ("src/lib.rs", PathKind::Relative),
        ];
        for (path, expected) in cases {
            assert_eq!(
                PathClass::classify(Path::new(path), &context).class(),
                expected,
                "{path}"
            );
        }
    }

    /// The whole point: the bytes of the path must not survive classification.
    #[test]
    fn no_component_of_the_path_reaches_the_record() {
        let class = PathClass::classify(
            Path::new("/home/ada/clients/acme-corp/merger-notes/SECRET_deal.txt"),
            &context(),
        );
        let json = serde_json::to_string(&Fields::new().with("path", class)).expect("serialize");
        for leaked in ["ada", "acme-corp", "merger-notes", "SECRET_deal"] {
            assert!(
                !json.contains(leaked),
                "`{leaked}` reached the record: {json}"
            );
        }
        assert!(json.contains(r#""class":"home""#), "{json}");
        assert!(json.contains(r#""depth":4"#), "{json}");
    }

    /// An extension is attacker-chosen text. Anything outside the reviewed
    /// vocabulary must collapse to `other` rather than echo.
    #[test]
    fn an_unrecognised_extension_collapses_to_other() {
        assert_eq!(
            Extension::of(Path::new("/tmp/x/secrets.API_KEY_sk_live_1234")),
            Extension::Other
        );
        assert_eq!(Extension::of(Path::new("/tmp/x/Makefile")), Extension::None);
        assert_eq!(Extension::of(Path::new("/tmp/x/lib.RS")), Extension::Rs);
    }

    /// macOS puts the per-user temp directory under `$HOME`; a scratch file
    /// classified as `home` would send a reader looking in the wrong place.
    #[test]
    fn temp_wins_over_home_when_temp_is_nested_inside_it() {
        let context = PathContext::new(
            None,
            Some(PathBuf::from("/Users/ada")),
            Some(PathBuf::from("/Users/ada/Library/Caches/TemporaryItems")),
        );
        let class = PathClass::classify(
            Path::new("/Users/ada/Library/Caches/TemporaryItems/stella/x.log"),
            &context,
        );
        assert_eq!(class.class(), PathKind::Temp);
        assert_eq!(class.depth(), 2);
    }

    #[test]
    fn traversal_segments_do_not_deepen_a_path() {
        let context = context();
        let class = PathClass::classify(Path::new("/home/ada/work/stella/./a/../b/c.rs"), &context);
        assert_eq!(class.class(), PathKind::InsideWorkspace);
        assert_eq!(class.depth(), 3, "`.` and `..` are not levels");
    }
}
