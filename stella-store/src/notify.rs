//! **Persist-until-read notifications.** A notification is a message that
//! must survive until the user has actually seen it — an OAuth login
//! finishing, another session blocking on input, a background run erroring —
//! not a transient toast that vanishes with the frame.
//!
//! Storage mirrors the session registry: **one JSON file per notification**
//! under `data_dir()/notifications/`, written atomically. Producers in
//! different processes never contend (each mints its own file), and marking
//! read rewrites only that one file. The deck shows an unread badge and an
//! inbox overlay; a notification leaves the unread set only when the user
//! marks it read there. Read notifications are swept by
//! [`NotificationStore::prune`].

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::sessions::now_ms;
use crate::{Result, StoreError};

/// One persistent message. `read` is the whole lifecycle: false → shown in
/// the badge/inbox, true → kept only until pruned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    /// Unique id (`ntf-<ms>-<pid>-<seq>`), minted by [`Notification::new`].
    pub id: String,
    pub created_at_ms: u64,
    /// One-line headline (the badge/inbox row).
    pub title: String,
    /// The message that must persist until read.
    pub body: String,
    /// Where it came from — a session id from the registry, a server name,
    /// etc. Free-form; the inbox shows it dimmed.
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub read: bool,
}

impl Notification {
    pub fn new(
        title: impl Into<String>,
        body: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let now = now_ms();
        Self {
            id: format!(
                "ntf-{now}-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ),
            created_at_ms: now,
            title: title.into(),
            body: body.into(),
            source: source.into(),
            read: false,
        }
    }
}

/// The notification directory handle; every operation is a direct
/// filesystem op, so any number of sessions can produce and one can read.
#[derive(Debug, Clone)]
pub struct NotificationStore {
    dir: PathBuf,
}

impl NotificationStore {
    /// The standard store at `data_dir()/notifications`.
    pub fn open_default() -> Self {
        Self::open(crate::usage::data_dir().join("notifications"))
    }

    /// A store rooted at `dir` (tests point this at a temp dir).
    pub fn open(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path_for(&self, id: &str) -> PathBuf {
        let safe: String = id
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.dir.join(format!("{safe}.json"))
    }

    /// Persist `notification` (its own file — concurrent producers never
    /// clobber each other).
    pub fn push(&self, notification: &Notification) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| StoreError(format!("cannot create {}: {e}", self.dir.display())))?;
        let json = serde_json::to_string_pretty(notification)
            .map_err(|e| StoreError(format!("cannot serialize notification: {e}")))?;
        let path = self.path_for(&notification.id);
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        std::fs::write(&tmp, json)
            .map_err(|e| StoreError(format!("cannot write {}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, &path)
            .map_err(|e| StoreError(format!("cannot replace {}: {e}", path.display())))?;
        Ok(())
    }

    /// All notifications, newest first. Unreadable files are skipped.
    pub fn list(&self) -> Vec<Notification> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut items: Vec<Notification> = entries
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    return None;
                }
                serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()
            })
            .collect();
        items.sort_by_key(|n| std::cmp::Reverse(n.created_at_ms));
        items
    }

    /// How many are still unread (the badge count).
    pub fn unread_count(&self) -> usize {
        self.list().iter().filter(|n| !n.read).count()
    }

    /// Mark one read; returns whether it existed.
    pub fn mark_read(&self, id: &str) -> Result<bool> {
        let path = self.path_for(id);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(false);
        };
        let Ok(mut notification) = serde_json::from_str::<Notification>(&text) else {
            return Ok(false);
        };
        if !notification.read {
            notification.read = true;
            self.push(&notification)?;
        }
        Ok(true)
    }

    /// Mark everything read (the inbox's `R`).
    pub fn mark_all_read(&self) -> Result<usize> {
        let mut marked = 0;
        for notification in self.list() {
            if !notification.read {
                marked += usize::from(self.mark_read(&notification.id)?);
            }
        }
        Ok(marked)
    }

    /// Sweep **read** notifications older than `max_age_ms`. Unread ones are
    /// never pruned — "persists until read" is the contract.
    pub fn prune(&self, max_age_ms: u64) -> Result<usize> {
        let cutoff = now_ms().saturating_sub(max_age_ms);
        let mut removed = 0;
        for notification in self.list() {
            if notification.read && notification.created_at_ms < cutoff {
                match std::fs::remove_file(self.path_for(&notification.id)) {
                    Ok(()) => removed += 1,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(StoreError(format!("cannot prune notification: {e}"))),
                }
            }
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> (PathBuf, NotificationStore) {
        let dir = std::env::temp_dir().join(format!("stella-notify-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (dir.clone(), NotificationStore::open(dir))
    }

    #[test]
    fn push_list_and_mark_read_lifecycle() {
        let (dir, store) = temp_store("lifecycle");

        let a = Notification::new("login ok", "github: OAuth login completed", "mcp");
        let b = Notification::new("needs input", "session ses-1 is waiting", "ses-1");
        store.push(&a).unwrap();
        store.push(&b).unwrap();

        assert_eq!(store.unread_count(), 2);
        let listed = store.list();
        assert_eq!(listed.len(), 2);

        assert!(store.mark_read(&a.id).unwrap());
        assert!(!store.mark_read("ntf-nope").unwrap());
        assert_eq!(store.unread_count(), 1);
        // The read flag persisted to disk.
        assert!(store.list().iter().any(|n| n.id == a.id && n.read));

        assert_eq!(store.mark_all_read().unwrap(), 1);
        assert_eq!(store.unread_count(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_never_touches_unread() {
        let (dir, store) = temp_store("prune");

        let mut old_read = Notification::new("done", "old + read", "");
        old_read.created_at_ms = 1;
        old_read.read = true;
        store.push(&old_read).unwrap();

        let mut old_unread = Notification::new("still waiting", "old + UNREAD", "");
        old_unread.id = format!("{}-u", old_unread.id);
        old_unread.created_at_ms = 1;
        store.push(&old_unread).unwrap();

        assert_eq!(store.prune(1_000).unwrap(), 1);
        let left = store.list();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, old_unread.id);
        assert!(!left[0].read, "persist-until-read must survive pruning");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
