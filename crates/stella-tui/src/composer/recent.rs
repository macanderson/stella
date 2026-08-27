// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The palette's `recent` section — SPEC 10's third band, and the reason it
//! needs somewhere to write.
//!
//! A `recent` list that empties every time the deck restarts is a list of what
//! you did in the last five minutes, which you already remember. So the names
//! ride a small JSON array in the workspace's own private directory
//! (`.stella/private/palette-recent.json`), resolved by whoever launches the
//! deck and handed in as a path — the smallest store that survives a restart
//! and the only one this crate can reach without taking a dependency on
//! `stella-store` (see AGENTS.md § the `.stella/` directory).
//!
//! Both file operations are best-effort and silent. A palette that refused to
//! open because a convenience list would not parse has traded a feature for an
//! ornament; a torn write reads back as malformed and starts the list again,
//! which costs a user five command names.

use std::path::{Path, PathBuf};

/// Commands the `recent` section shows. Short on purpose: a shortcut list long
/// enough to need reading is a second copy of the menu below it.
pub const MAX_RECENT: usize = 5;

/// The commands this workspace ran from the palette, most recent first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recents {
    names: Vec<String>,
    /// Where the list is kept, or `None` for a surface that was handed no
    /// path — it keeps an in-session list and writes nothing.
    path: Option<PathBuf>,
    /// Set by [`Self::record`], cleared by [`Self::flush`], so the deck can
    /// call `flush` on every keystroke and pay for a write only on a dispatch.
    dirty: bool,
}

impl Recents {
    /// The list kept in `path`, seeded with whatever is already there.
    #[must_use]
    pub fn kept_in(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut names = read_names(&path);
        names.truncate(MAX_RECENT);
        Self {
            names,
            path: Some(path),
            dirty: false,
        }
    }

    /// The commands, most recent first.
    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Record `name` as the command just run: it moves to the front, and the
    /// list keeps its cap. A submission that is not a slash command is
    /// ignored, so prose typed at the composer never reaches the section.
    pub fn record(&mut self, name: &str) {
        let name = name.trim();
        if !name.starts_with('/') {
            return;
        }
        self.names.retain(|kept| kept != name);
        self.names.insert(0, name.to_string());
        self.names.truncate(MAX_RECENT);
        self.dirty = true;
    }

    /// Write the list back when it has changed and it has somewhere to go.
    pub fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let Ok(text) = serde_json::to_string(&self.names) else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, text);
    }
}

/// The names already in `path`, or none — a missing, unreadable or malformed
/// file is an empty list.
fn read_names(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_command_leads_and_a_repeat_moves_rather_than_duplicates() {
        let mut recents = Recents::default();
        recents.record("/plan");
        recents.record("/diff");
        recents.record("/plan");
        assert_eq!(recents.names(), ["/plan", "/diff"]);
    }

    #[test]
    fn the_list_keeps_its_cap_and_drops_the_oldest() {
        let mut recents = Recents::default();
        for i in 0..MAX_RECENT + 3 {
            recents.record(&format!("/cmd{i}"));
        }
        assert_eq!(recents.names().len(), MAX_RECENT);
        assert_eq!(recents.names()[0], format!("/cmd{}", MAX_RECENT + 2));
        assert!(!recents.names().iter().any(|n| n == "/cmd0"));
    }

    #[test]
    fn prose_is_not_a_command_and_is_not_recorded() {
        let mut recents = Recents::default();
        recents.record("fix the parser");
        assert!(recents.names().is_empty());
    }

    /// **The witness (#5048).** The list survives the process that wrote it —
    /// which is the whole difference between a `recent` section and a list of
    /// what you did in the last five minutes.
    #[test]
    fn the_list_survives_the_session_that_wrote_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("private").join("palette-recent.json");

        let mut session = Recents::kept_in(&path);
        session.record("/plan");
        session.record("/graph");
        session.flush();

        let next = Recents::kept_in(&path);
        assert_eq!(next.names(), ["/graph", "/plan"]);
    }

    #[test]
    fn a_surface_with_no_path_keeps_an_in_session_list_and_writes_nothing() {
        let mut recents = Recents::default();
        recents.record("/plan");
        recents.flush();
        assert_eq!(recents.names(), ["/plan"]);
    }

    #[test]
    fn a_malformed_file_reads_as_an_empty_list_rather_than_refusing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("palette-recent.json");
        std::fs::write(&path, "{ this is not a list").expect("write");
        assert!(Recents::kept_in(&path).names().is_empty());
    }
}
