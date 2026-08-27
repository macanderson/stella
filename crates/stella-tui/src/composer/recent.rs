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

/// One command the palette ran, and when.
///
/// The stamp is what lets a `recent` row say `2m ago` (SPEC 10). It is
/// optional because the file predates it: a list written before #5213 is a
/// bare JSON array of names, and an entry read from one has a name and no
/// stamp. An age nobody recorded is not rendered — never guessed from the
/// file's mtime or from the entry's position, both of which would put a
/// confident wrong number on the row.
///
/// Read and written by hand through `serde_json::Value` rather than a derive:
/// this crate carries `serde_json` and not `serde`, and one small struct is not
/// the reason to add a derive dependency to a rendering crate (AGENTS.md, "No
/// new dependencies casually"). The by-hand reader is also what lets one pass
/// accept both file shapes — see this module's `read_entries`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recent {
    pub name: String,
    /// Unix milliseconds when this command was last run.
    pub at_ms: Option<u64>,
}

/// Unix milliseconds now, or `None` if the clock is before the epoch.
///
/// A clock read in a crate that is otherwise a pure fold needs a word: this
/// module is already the one that writes to the filesystem, so it is the one
/// place in the composer that is not pure by construction. Keeping the read
/// here rather than threading `now_ms` through [`super::handle_slash_popup_key`]
/// leaves that shared public signature — and every surface that calls it —
/// alone. [`Recents::record_at`] is the same function with the clock injected,
/// which is what the tests drive.
fn now_ms() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

/// The commands this workspace ran from the palette, most recent first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recents {
    entries: Vec<Recent>,
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
        let mut entries = read_entries(&path);
        entries.truncate(MAX_RECENT);
        Self {
            entries,
            path: Some(path),
            dirty: false,
        }
    }

    /// The commands and their stamps, most recent first.
    #[must_use]
    pub fn entries(&self) -> &[Recent] {
        &self.entries
    }

    /// Record `name` as the command just run, stamped now.
    pub fn record(&mut self, name: &str) {
        let at = now_ms();
        self.record_stamped(name, at);
    }

    /// [`Self::record`] with the clock injected — the deterministic half, and
    /// what the tests drive.
    pub fn record_at(&mut self, name: &str, at_ms: u64) {
        self.record_stamped(name, Some(at_ms));
    }

    /// The command moves to the front and the list keeps its cap. A submission
    /// that is not a slash command is ignored, so prose typed at the composer
    /// never reaches the section.
    fn record_stamped(&mut self, name: &str, at_ms: Option<u64>) {
        let name = name.trim();
        if !name.starts_with('/') {
            return;
        }
        self.entries.retain(|kept| kept.name != name);
        self.entries.insert(
            0,
            Recent {
                name: name.to_string(),
                at_ms,
            },
        );
        self.entries.truncate(MAX_RECENT);
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
        let array = serde_json::Value::Array(
            self.entries
                .iter()
                .map(|entry| {
                    let mut object = serde_json::Map::new();
                    object.insert(
                        "name".to_string(),
                        serde_json::Value::String(entry.name.clone()),
                    );
                    // Omitted rather than written as null when unknown, so a
                    // list round-tripped from the pre-#5213 shape does not
                    // grow a field that says nothing.
                    if let Some(at) = entry.at_ms {
                        object.insert("at_ms".to_string(), serde_json::Value::from(at));
                    }
                    serde_json::Value::Object(object)
                })
                .collect::<Vec<_>>(),
        );
        let Ok(text) = serde_json::to_string(&array) else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, text);
    }
}

/// The entries already in `path`, or none — a missing, unreadable or malformed
/// file is an empty list.
///
/// Two shapes are accepted, per element rather than per file. The current one
/// is an object; the one written before #5213 is a bare name, and it is read
/// rather than discarded so an upgrade does not silently empty a user's list.
/// A legacy entry keeps its name and carries no stamp, which is exactly what
/// it knows — an age is never inferred from the file's mtime or from the
/// entry's position.
///
/// Deciding per element rather than per file means a half-migrated list — the
/// shape a torn write or a downgrade-then-upgrade leaves behind — reads every
/// entry it can instead of failing whole. Anything that is neither is skipped.
fn read_entries(path: &Path) -> Vec<Recent> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(&text)
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| match item {
            serde_json::Value::String(name) => Some(Recent {
                name: name.clone(),
                at_ms: None,
            }),
            serde_json::Value::Object(object) => object
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(|name| Recent {
                    name: name.to_string(),
                    at_ms: object.get("at_ms").and_then(serde_json::Value::as_u64),
                }),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The names alone, for the assertions that are about order.
    fn names(recents: &Recents) -> Vec<&str> {
        recents.entries().iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn the_newest_command_leads_and_a_repeat_moves_rather_than_duplicates() {
        let mut recents = Recents::default();
        recents.record("/plan");
        recents.record("/diff");
        recents.record("/plan");
        assert_eq!(names(&recents), ["/plan", "/diff"]);
    }

    #[test]
    fn the_list_keeps_its_cap_and_drops_the_oldest() {
        let mut recents = Recents::default();
        for i in 0..MAX_RECENT + 3 {
            recents.record(&format!("/cmd{i}"));
        }
        assert_eq!(recents.entries().len(), MAX_RECENT);
        assert_eq!(recents.entries()[0].name, format!("/cmd{}", MAX_RECENT + 2));
        assert!(!recents.entries().iter().any(|e| e.name == "/cmd0"));
    }

    #[test]
    fn prose_is_not_a_command_and_is_not_recorded() {
        let mut recents = Recents::default();
        recents.record("fix the parser");
        assert!(recents.entries().is_empty());
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
        assert_eq!(names(&next), ["/graph", "/plan"]);
    }

    #[test]
    fn a_surface_with_no_path_keeps_an_in_session_list_and_writes_nothing() {
        let mut recents = Recents::default();
        recents.record("/plan");
        recents.flush();
        assert_eq!(names(&recents), ["/plan"]);
    }

    /// **The witness (#5213).** A recorded command carries when it ran, and
    /// the stamp survives the restart with the name — an age the palette can
    /// only render if it was stored.
    #[test]
    fn a_recorded_command_carries_its_stamp_across_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("palette-recent.json");

        let mut session = Recents::kept_in(&path);
        session.record_at("/gates", 1_700_000_000_000);
        session.flush();

        let next = Recents::kept_in(&path);
        assert_eq!(next.entries()[0].name, "/gates");
        assert_eq!(next.entries()[0].at_ms, Some(1_700_000_000_000));
    }

    /// A repeat re-stamps rather than keeping the first run's time — the row
    /// says when the command last ran, which is what "recent" means.
    #[test]
    fn a_repeat_takes_the_newer_stamp() {
        let mut recents = Recents::default();
        recents.record_at("/gates", 1_000);
        recents.record_at("/gates", 9_000);
        assert_eq!(recents.entries().len(), 1);
        assert_eq!(recents.entries()[0].at_ms, Some(9_000));
    }

    /// A list written before this field existed is a bare array of names.
    /// It is read, not discarded — an upgrade must not silently empty the
    /// section — and its entries carry no stamp rather than a guessed one.
    #[test]
    fn the_pre_stamp_file_shape_is_read_and_carries_no_age() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("palette-recent.json");
        std::fs::write(&path, r#"["/plan","/graph"]"#).expect("write");

        let recents = Recents::kept_in(&path);
        assert_eq!(names(&recents), ["/plan", "/graph"]);
        assert!(recents.entries().iter().all(|e| e.at_ms.is_none()));
    }

    #[test]
    fn a_malformed_file_reads_as_an_empty_list_rather_than_refusing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("palette-recent.json");
        std::fs::write(&path, "{ this is not a list").expect("write");
        assert!(Recents::kept_in(&path).entries().is_empty());
    }
}
