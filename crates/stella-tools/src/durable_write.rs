//! Durable replacement of a file's contents on the agent's primary
//! source-mutation path.
//!
//! `write_file` and `edit_file` — the agent's two ways of changing a file —
//! both used to call `tokio::fs::write` straight onto the target, which opens with
//! `O_TRUNC`: the old bytes are gone before the new ones are written. A
//! crash, an OOM kill, or a Ctrl-C in that window left the user's own source
//! file truncated, with the replacement nowhere on disk. That is the one
//! failure this module exists to remove.
//!
//! # Why this is not the shared atomic helper
//!
//! Stella's own state files go through [`stella_store::durable::write_atomic`]
//! — temp + fsync + rename — because for a file Stella owns, an atomic
//! replacement is strictly the best outcome.
//!
//! A rename cannot be used here, and #617 ruled it out explicitly. Rename
//! replaces the **inode**, and everything carried by the inode rather than by
//! the path goes with it:
//!
//! - the **mode** — a script silently loses `+x`, a config loses its 0600;
//! - the **owner** — a non-root process cannot recreate a file owned by
//!   someone else, so a rename quietly changes who owns the user's file;
//! - **hard links** — every other name for that file keeps pointing at the
//!   *old* content, so the edit appears not to have happened;
//! - the identity an **editor or watcher** holds open. A tool watching an
//!   inode (an LSP server, a hot-reloader, `tail -f`) follows the file it
//!   opened, not the name; after a rename it is watching a file nothing will
//!   ever write to again.
//!
//! Stella's state files are Stella's, and none of that applies to them. A
//! user's source file is not Stella's, and all of it does. So on this path
//! the inode is preserved and the write is done **in place**: open the
//! existing file for writing without `O_TRUNC`, write the new bytes, then
//! truncate to the new length, then `fsync`.
//!
//! # What that costs, stated honestly
//!
//! An in-place rewrite is **not atomic**. A concurrent reader can observe a
//! half-written file, which a rename would have made impossible. What it does
//! guarantee, and what the plain `tokio::fs::write` it replaced did not:
//!
//! - the file is never observed **empty** — truncation happens after the new
//!   bytes are written, not before, so a crash mid-write leaves the new
//!   content plus at worst a stale tail, never a zero-length source file;
//! - the bytes reach the disk — `fsync` before the tool reports success, so
//!   "wrote 412 bytes" is not a claim about the page cache;
//! - the inode, and with it mode, owner, links and watchers, survives. The
//!   one bit the inode does not preserve on its own is set-user-ID /
//!   set-group-ID, which the kernel clears on any write by an unprivileged
//!   process; the mode captured at open is re-applied after the write, so a
//!   setgid script keeps working.
//!
//! A brand-new file has no inode to preserve, so it is simply created (and
//! its directory entry `fsync`ed, which is what makes the creation itself
//! durable).
//!
//! POSIX ACLs, extended attributes and SELinux labels also live on the inode
//! and, because the inode never changes, are preserved here for free — the
//! one thing the previous rename-based implementation could not promise.

#[cfg(test)]
use std::path::Path;
use std::sync::Arc;

use crate::rootfd::{RootError, RootHandle};

/// Replace `rel`'s contents with `bytes`, durably and in place, through a
/// **held root descriptor** rather than a resolved path string (#938).
///
/// This is the form every model-steered write uses. The path is walked
/// component by component off `handle`, each one opened relative to the one
/// above it, so there is no interval in which a name this function already
/// approved can be re-pointed at something else. `create_parents` makes the
/// walk create the interior directories as it goes — the replacement for a
/// `create_dir_all` that would re-resolve the whole prefix.
///
/// Runs the blocking filesystem work on a blocking worker: the
/// open/write/truncate/fsync sequence has no async equivalent that fsyncs,
/// and doing it on a reactor thread would stall the runtime.
pub async fn write_file_durably_at(
    handle: Arc<RootHandle>,
    rel: String,
    bytes: Vec<u8>,
    create_parents: bool,
) -> Result<(), RootError> {
    tokio::task::spawn_blocking(move || write_blocking_at(&handle, &rel, &bytes, create_parents))
        .await
        .map_err(|error| {
            RootError::Io(std::io::Error::other(format!("write task failed: {error}")))
        })?
}

fn write_blocking_at(
    handle: &RootHandle,
    rel: &str,
    bytes: &[u8],
    create_parents: bool,
) -> Result<(), RootError> {
    use std::io::Write as _;

    // The sequence the module header argues for: `open_write` opens without
    // `O_TRUNC` and reports whether it created the entry, so the old bytes
    // are never gone before the new ones are written.
    let target = handle.open_write(rel, create_parents)?;
    let mut file = &target.file;
    let mode_before = captured_mode(file)?;
    file.write_all(bytes)?;
    file.set_len(bytes.len() as u64)?;
    restore_mode(file, mode_before)?;
    file.sync_all()?;
    if target.created {
        // The parent descriptor the walk already holds — re-opening the parent
        // by path would be the second resolution this whole change removes.
        target.sync_parent();
    }
    Ok(())
}

/// The file's mode before the write, so the set-user-ID/set-group-ID bits the
/// kernel strips can be put back. Asked of the open descriptor rather than of
/// the path — the whole point of this module is that a name may be re-pointed
/// between two resolutions.
#[cfg(unix)]
fn captured_mode(file: &std::fs::File) -> std::io::Result<u32> {
    use std::os::unix::fs::PermissionsExt as _;
    Ok(file.metadata()?.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn captured_mode(_file: &std::fs::File) -> std::io::Result<u32> {
    Ok(0)
}

/// Re-apply `mode` if the write changed it. A no-op in the common case; on a
/// setgid script it is the difference between preserving the mode and
/// silently clearing the bit that makes the script work.
#[cfg(unix)]
fn restore_mode(file: &std::fs::File, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let now = file.metadata()?.permissions().mode() & 0o7777;
    if now == mode {
        return Ok(());
    }
    file.set_permissions(std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn restore_mode(_file: &std::fs::File, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stella-durable-write-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// Drive the live implementation — the root-descriptor form every write
    /// in the product actually takes.
    ///
    /// These seven tests used to exercise a second, path-resolving copy of
    /// the same sequence, which had no callers left once every write moved to
    /// the `_at` form (#938). Pointing them here costs nothing and buys the
    /// thing a durability test is for: they now fail when the code that runs
    /// stops preserving an inode, a setgid bit, or a hard link.
    async fn write(path: &Path, content: &str) -> Result<(), String> {
        let parent = path.parent().expect("a parent directory");
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("a UTF-8 file name");
        let handle = RootHandle::open(parent).map_err(|error| error.to_string())?;
        write_blocking_at(&handle, name, content.as_bytes(), true)
            .map_err(|error| error.to_string())
    }

    #[tokio::test]
    async fn a_new_file_is_created_with_the_content() {
        let dir = temp_dir();
        let path = dir.join("fresh.rs");
        write(&path, "fn main() {}").await.expect("write");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fn main() {}");
    }

    #[tokio::test]
    async fn a_shorter_rewrite_leaves_no_tail_of_the_old_content() {
        let dir = temp_dir();
        let path = dir.join("shrink.txt");
        write(&path, "aaaaaaaaaaaaaaaaaaaaaaaa")
            .await
            .expect("first");
        write(&path, "bb").await.expect("second");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "bb");
    }

    #[tokio::test]
    async fn no_temp_file_is_left_beside_the_users_source() {
        let dir = temp_dir();
        let path = dir.join("clean.txt");
        write(&path, "one").await.expect("write");
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["clean.txt".to_string()]);
    }

    /// The #617 ruling in one assertion: the agent's edit path keeps the
    /// inode. Everything else in this module (mode, owner, links, ACLs,
    /// editors watching the file) follows from it.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_edit_preserves_the_inode() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = temp_dir();
        let path = dir.join("source.rs");
        std::fs::write(&path, "fn old() {}").unwrap();
        let before = std::fs::metadata(&path).unwrap().ino();

        write(&path, "fn new() {}").await.expect("write");

        assert_eq!(
            std::fs::metadata(&path).unwrap().ino(),
            before,
            "a rename here would replace the inode"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fn new() {}");
    }

    /// A regression the audit named: replacing the inode drops the mode, so
    /// an executable script silently loses `+x`.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_executable_script_keeps_its_execute_bit() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = temp_dir();
        let path = dir.join("build.sh");
        std::fs::write(&path, "#!/bin/sh\necho old\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

        write(&path, "#!/bin/sh\necho new\n").await.expect("write");

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o755, "the execute bit must survive the write");
        assert!(std::fs::read_to_string(&path).unwrap().contains("new"));
    }

    /// A setgid bit is the bit a rename-then-chmod implementation loses even
    /// when it tries to carry the mode, because POSIX clears it on write.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_setgid_bit_survives_the_write() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = temp_dir();
        let path = dir.join("shared.sh");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o2755)).unwrap();
        // Some filesystems refuse setgid; skip rather than fail spuriously.
        let before = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        if before != 0o2755 {
            return;
        }

        write(&path, "new").await.expect("write");

        let after = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(after, 0o2755, "setgid must survive the write");
    }

    /// A rename would sever the link, leaving the other name pointing at the
    /// old content — the write would look like it never happened.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_hard_linked_file_keeps_every_name_pointing_at_the_write() {
        let dir = temp_dir();
        let primary = dir.join("linked.txt");
        let alias = dir.join("alias.txt");
        std::fs::write(&primary, "old content").unwrap();
        std::fs::hard_link(&primary, &alias).unwrap();

        write(&primary, "new").await.expect("write");

        assert_eq!(std::fs::read_to_string(&primary).unwrap(), "new");
        assert_eq!(
            std::fs::read_to_string(&alias).unwrap(),
            "new",
            "the other name must see the write, not the old content"
        );
    }
}
