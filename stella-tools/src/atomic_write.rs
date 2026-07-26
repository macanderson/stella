//! Durable replacement of a file's contents on the agent's primary
//! source-mutation path.
//!
//! `write_file`, `edit_file`, `apply_edits` and `save_memory` all used to
//! call `tokio::fs::write` straight onto the target, which opens with
//! `O_TRUNC`: the old bytes are gone before the new ones are written. A
//! crash, an OOM kill, or a Ctrl-C in that window left the user's own source
//! file truncated, with the replacement nowhere on disk. That is the one
//! failure this module exists to remove.
//!
//! # Why this is not simply temp-then-rename
//!
//! Rename-over-target is the standard atomic replace, but it swaps the
//! **inode**. Everything carried by the old inode rather than by the path is
//! lost with it: the mode (a script silently loses `+x`), the owner, and any
//! hard links — the other names for that file keep pointing at the *old*
//! content, so the write appears not to have happened. On a user's own
//! source tree that trade is not automatically worth making, which is why
//! the audit declined the mechanical version of this fix.
//!
//! So the strategy is chosen per target:
//!
//! - **No existing file** — temp + rename, created honouring the umask,
//!   exactly as `tokio::fs::write` would have.
//! - **A plain file we own with one link** — temp + rename, with the mode
//!   captured *before* the temp is created and re-applied to it, so `+x`,
//!   setgid and sticky survive. This is the common case.
//! - **More than one link, or an owner that is not us** — rewritten in
//!   place, preserving the inode, because a rename here would sever a hard
//!   link or silently change the owner (a non-root process cannot create a
//!   file owned by someone else). The write goes in first and the file is
//!   truncated to the new length *after*, so a crash leaves the new content
//!   plus a stale tail rather than an empty file. That is weaker than atomic
//!   but strictly better than today, and it is the only option that keeps
//!   the inode.
//!
//! # What a rename still does not carry
//!
//! POSIX ACLs, extended attributes and SELinux labels live on the inode and
//! are not reconstructed here. A target that needs them preserved should be
//! a hard-linked or foreign-owned file, which takes the in-place path above.
//! This is recorded rather than silently claimed.

use std::path::{Path, PathBuf};

/// Replace `path`'s contents with `bytes`, durably.
///
/// Returns `Err` with a message shaped for the tools' existing
/// `ToolOutput::Error { message }`. Runs the blocking filesystem work on a
/// blocking worker: the mode/owner/link inspection this needs has no async
/// equivalent, and doing it on a reactor thread would stall the runtime.
pub async fn replace_file_atomically(path: PathBuf, bytes: Vec<u8>) -> Result<(), String> {
    tokio::task::spawn_blocking(move || replace_blocking(&path, &bytes))
        .await
        .map_err(|error| format!("write task failed: {error}"))?
}

/// [`replace_file_atomically`] for callers that are already synchronous and
/// off the reactor — settings persistence, which runs on the CLI's own thread
/// and has no runtime to hand blocking work to.
///
/// Same strategy selection, same guarantees. Exposed so those callers get the
/// mode/hard-link/owner reasoning above rather than reimplementing a weaker
/// temp-then-rename that silently drops a file's permissions.
pub fn replace_file_atomically_blocking(path: &Path, bytes: &[u8]) -> Result<(), String> {
    replace_blocking(path, bytes)
}

#[cfg(unix)]
fn replace_blocking(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;

    // `symlink_metadata`, not `metadata`: callers resolve through
    // `resolve_within_root`, which canonicalizes, so a symlink has already
    // been followed to its in-root target — but this must not re-follow one
    // that appeared since, and it must see the link count of the file
    // itself.
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            rename_into_place(path, bytes, None)
        }
        Err(error) => Err(format!("cannot inspect {}: {error}", path.display())),
        Ok(metadata) => {
            let owned_by_us = metadata.uid() == unsafe { libc::geteuid() };
            if metadata.is_file() && metadata.nlink() == 1 && owned_by_us {
                use std::os::unix::fs::PermissionsExt as _;
                rename_into_place(path, bytes, Some(metadata.permissions().mode() & 0o7777))
            } else {
                // A hard link, a foreign owner, or not a regular file at all
                // (a fifo or device the user deliberately pointed at). The
                // inode has to survive.
                rewrite_in_place(path, bytes)
            }
        }
    }
}

/// Temp + fsync + rename, optionally re-applying a captured mode.
#[cfg(unix)]
fn rename_into_place(path: &Path, bytes: &[u8], mode: Option<u32>) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    // pid + counter, so two processes and two threads cannot collide on one
    // temp name. The suffix is distinctive rather than a bare `.tmp`: these
    // land beside arbitrary user source files, and `foo.rs.tmp` is something
    // a build watcher might well pick up.
    let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let mut temp_name = path.file_name().unwrap_or_default().to_os_string();
    temp_name.push(format!(".stella-tmp.{}.{sequence}", std::process::id()));
    let temp = path.with_file_name(temp_name);

    let mut options = std::fs::OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        // A fresh file honours the umask when we have no mode to carry,
        // matching what `tokio::fs::write` would have produced.
        .mode(mode.unwrap_or(0o666))
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options
        .open(&temp)
        .map_err(|error| format!("cannot create {}: {error}", temp.display()))?;

    let result = (|| {
        file.write_all(bytes)
            .map_err(|error| format!("cannot write {}: {error}", temp.display()))?;
        if let Some(mode) = mode {
            // `.mode()` is masked by the umask; `set_permissions` is not.
            // This is what actually preserves `+x` on a 0755 script, and the
            // setgid/sticky bits in the 0o7000 range.
            //
            // It must run AFTER the write, not before: POSIX clears the
            // set-user-ID and set-group-ID bits on write, so a mode applied
            // first loses exactly the bits hardest to notice missing. It
            // must also run BEFORE the fsync, so the metadata change is
            // covered by it.
            file.set_permissions(std::fs::Permissions::from_mode(mode))
                .map_err(|error| format!("cannot preserve mode on {}: {error}", temp.display()))?;
        }
        // The rename is only atomic with respect to *ordering* if the bytes
        // are on disk first; without this a power loss can publish an empty
        // file over a good one.
        file.sync_all()
            .map_err(|error| format!("cannot fsync {}: {error}", temp.display()))?;
        drop(file);
        std::fs::rename(&temp, path)
            .map_err(|error| format!("cannot replace {}: {error}", path.display()))?;
        if let Some(parent) = path.parent() {
            sync_directory(parent);
        }
        Ok(())
    })();
    if result.is_err() {
        // Never leave a temp file beside the user's source on failure.
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Rewrite through the existing inode, for a target whose identity must
/// survive. Write first, truncate after: a crash mid-write leaves the new
/// content plus a stale tail, never a zero-length file.
#[cfg(unix)]
fn rewrite_in_place(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(false)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    file.set_len(bytes.len() as u64)
        .map_err(|error| format!("cannot truncate {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("cannot fsync {}: {error}", path.display()))?;
    Ok(())
}

/// fsync the directory so the rename itself is durable, not just the bytes.
/// Best-effort: a filesystem that refuses to open a directory for sync is
/// not a reason to fail a write that already landed.
#[cfg(unix)]
fn sync_directory(dir: &Path) {
    if let Ok(handle) = std::fs::File::open(dir) {
        let _ = handle.sync_all();
    }
}

/// Non-unix has no mode, uid or link count to carry, so the strategy
/// collapses to temp + rename. NTFS inherits a new file's ACL from its
/// parent directory — which is where the target already lives — so the
/// effective permissions survive the swap.
#[cfg(not(unix))]
fn replace_blocking(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let mut temp_name = path.file_name().unwrap_or_default().to_os_string();
    temp_name.push(format!(".stella-tmp.{}.{sequence}", std::process::id()));
    let temp = path.with_file_name(temp_name);

    let attempt = (|| -> Result<(), String> {
        let mut file = std::fs::File::create(&temp)
            .map_err(|error| format!("cannot create {}: {error}", temp.display()))?;
        file.write_all(bytes)
            .map_err(|error| format!("cannot write {}: {error}", temp.display()))?;
        file.sync_all()
            .map_err(|error| format!("cannot fsync {}: {error}", temp.display()))?;
        drop(file);
        std::fs::rename(&temp, path)
            .map_err(|error| format!("cannot replace {}: {error}", path.display()))
    })();

    match attempt {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            // `rename` maps to MoveFileEx(MOVEFILE_REPLACE_EXISTING), which
            // fails when the destination is open in another process. Failing
            // the tool outright would be a regression against the plain
            // write this replaced, so fall back to it.
            std::fs::write(path, bytes).map_err(|_| error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stella-atomic-write-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    async fn write(path: &Path, content: &str) -> Result<(), String> {
        replace_file_atomically(path.to_path_buf(), content.as_bytes().to_vec()).await
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
    async fn no_temp_file_is_left_behind() {
        let dir = temp_dir();
        let path = dir.join("clean.txt");
        write(&path, "one").await.expect("write");
        let strays: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("stella-tmp"))
            .collect();
        assert!(strays.is_empty(), "temp files left behind: {strays:?}");
    }

    /// The regression the audit named: rename-over-target replaces the
    /// inode, so an executable script silently loses `+x`. Nothing in the
    /// tree covered this before.
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
        assert_eq!(mode, 0o755, "the execute bit must survive the replace");
        assert!(std::fs::read_to_string(&path).unwrap().contains("new"));
    }

    /// `.mode()` on the temp is masked by the umask; `set_permissions` is
    /// not. A setgid bit proves the second one is doing the work.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_setgid_bit_survives_the_replace() {
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
        assert_eq!(after, 0o2755, "setgid must survive the replace");
    }

    /// A rename would sever the link, leaving the other name pointing at the
    /// old content — the write would look like it never happened. The
    /// in-place path keeps the inode so every name sees the new bytes.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_hard_linked_file_keeps_its_inode_and_every_name_sees_the_write() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = temp_dir();
        let primary = dir.join("linked.txt");
        let alias = dir.join("alias.txt");
        std::fs::write(&primary, "old content").unwrap();
        std::fs::hard_link(&primary, &alias).unwrap();
        let inode_before = std::fs::metadata(&primary).unwrap().ino();

        write(&primary, "new").await.expect("write");

        assert_eq!(std::fs::read_to_string(&primary).unwrap(), "new");
        assert_eq!(
            std::fs::read_to_string(&alias).unwrap(),
            "new",
            "the other name must see the write, not the old content"
        );
        assert_eq!(
            std::fs::metadata(&primary).unwrap().ino(),
            inode_before,
            "the inode must survive so the link is not severed"
        );
    }
}
