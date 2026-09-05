//! One `git worktree` command at a time, per repo.
//!
//! Git keeps worktree notes in `.git/worktrees/<name>/`. It takes no lock
//! while it writes them. Two `git worktree add` calls that start together can
//! read each other's half-written files.
//!
//! Run 9255 caught that. The second add died on
//! `failed to read .git/worktrees/<sibling>/commondir: Success`. The errno is
//! `Success` because the read came up short. The fleet lost a whole task.
//!
//! [`run_worktree`] is the one door every `git worktree …` call here goes
//! through. No two of them run against one repo root at once. Workers are not
//! held apart. Only the tree cutting is, so the turns in those trees still run
//! side by side.
//!
//! There are two ways to collide, so there are two locks.
//!
//! One is for this process. Each repo root gets a `tokio::sync::Mutex`, handed
//! out by a shared map. A [`WorktreeManager`](super::WorktreeManager) and a
//! [`Gc`](crate::gc::Gc) on one repo take the same one. This half always
//! holds.
//!
//! The other is for a second `stella`. It is a lock file under
//! `<repo_root>/.stella/private/`. Making a file with `O_CREAT|O_EXCL` is
//! atomic on every disk we run on, and it needs no new crate.
//! `stella-graph`'s `ManifestLock` is the same trick, and this copies it.
//! This half is best effort. If the file cannot be made, or the wait runs out,
//! the command goes ahead. Losing a task to a slow peer would be worse.
//!
//! [`Fleet`](crate::fleet::Fleet) holds its own locks over quick writes and
//! never across an `.await`. This one is held across the git call, which is
//! the point.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex as SyncMutex};
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use super::{GitCli, GitError, GitOutput};

/// How long to honour another process's lock file. It is long because the
/// holder is checking out a tree, and a big repo takes minutes.
const LOCK_WAIT: Duration = Duration::from_secs(300);

/// A lock file older than this was left by a process that died. Break it.
const LOCK_STALE: Duration = Duration::from_secs(1800);

/// How often a waiter tries the create again.
const LOCK_POLL: Duration = Duration::from_millis(25);

/// How long to wait before the one retry of a flaky git call.
const RETRY_PAUSE: Duration = Duration::from_millis(50);

/// The mutexes, one per repo root. Nothing is ever dropped from the map. A run
/// drives few repos, and dropping a key would hand a waiting caller a fresh
/// lock.
static REPO_LOCKS: LazyLock<SyncMutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>>> =
    LazyLock::new(|| SyncMutex::new(HashMap::new()));

/// The map key for `repo_root`. It is the real path when the path resolves, so
/// two spellings of one repo share a lock. A `/var` symlink on macOS and a
/// relative path are both that case. A root that is not there yet keeps the
/// path as given.
fn lock_key(repo_root: &Path) -> PathBuf {
    std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf())
}

fn repo_mutex(repo_root: &Path) -> Arc<AsyncMutex<()>> {
    let mut registry = REPO_LOCKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(registry.entry(lock_key(repo_root)).or_default())
}

/// Held for one `git worktree` call. Dropped in field order: the lock file
/// first, then the mutex.
struct WorktreeGuard {
    /// `None` when the lock file could not be taken. The module docs say why
    /// that goes ahead.
    _file: Option<LockFile>,
    _process: OwnedMutexGuard<()>,
}

impl WorktreeGuard {
    async fn acquire(repo_root: &Path) -> Self {
        let process = repo_mutex(repo_root).lock_owned().await;
        // A root that is not on disk is a stand-in a test passed in, and there
        // is nothing to lock. Asked here, or the create below would make
        // `/repo` for real on a machine where the tests run as root.
        let file = if repo_root.is_dir() {
            LockFile::acquire(&lock_path(repo_root)).await
        } else {
            None
        };
        Self {
            _file: file,
            _process: process,
        }
    }
}

/// The half that holds off a second `stella`. Deleted on drop.
struct LockFile {
    path: PathBuf,
}

impl Drop for LockFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Where the lock file lives. `.stella/private/` is owner-only, the generated
/// `.stella/.gitignore` covers it, and the fleet ledger sits there too.
fn lock_path(repo_root: &Path) -> PathBuf {
    repo_root
        .join(".stella")
        .join("private")
        .join("worktree.lock")
}

impl LockFile {
    /// Take the lock file. Give up after [`LOCK_WAIT`]. Give up at once when
    /// the folder cannot be made at all, which a read-only tree hits.
    async fn acquire(path: &Path) -> Option<Self> {
        Self::acquire_within(path, LOCK_WAIT, LOCK_STALE).await
    }

    /// [`Self::acquire`] with both bounds passed in, so a test can prove them
    /// in milliseconds rather than in minutes.
    async fn acquire_within(path: &Path, wait: Duration, stale: Duration) -> Option<Self> {
        let parent = path.parent()?;
        create_private_dir(parent).ok()?;
        let deadline = Instant::now() + wait;
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(_) => {
                    return Some(Self {
                        path: path.to_path_buf(),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if is_stale(path, stale) {
                        // If another waiter breaks it first, the next turn of
                        // the loop races for it as usual.
                        let _ = std::fs::remove_file(path);
                        continue;
                    }
                    if Instant::now() >= deadline {
                        return None;
                    }
                    tokio::time::sleep(LOCK_POLL).await;
                }
                Err(_) => return None,
            }
        }
    }
}

/// Make `dir` owner-only. Same shape as `stella-cli`'s settings helper. On
/// unix the mode is set as the folder is made, so it is never open to other
/// users. Other systems get their own default.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Is this lock file old enough to count as junk? A date we cannot read, or
/// one in the future, says no. Keeping a lock we cannot age out costs one slow
/// call. Breaking one we should have kept costs the race.
fn is_stale(path: &Path, stale: Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > stale)
}

/// Could a peer's writes have caused this git failure? Then it is worth one
/// retry. A real clash, such as `'<path>' already exists`, is not on the list.
/// It fails once and is reported once.
fn is_transient(stderr: &str) -> bool {
    let lowered = stderr.to_ascii_lowercase();
    // `index.lock` and `unable to create` are git's words for a held index.
    // `commondir` is the short read from run 9255.
    ["index.lock", "unable to create", "commondir"]
        .iter()
        .any(|needle| lowered.contains(needle))
}

/// Run one `git worktree …` call with this repo's lock held. Retry once when
/// git failed for a reason [`is_transient`] knows.
///
/// If the retry fails too, the caller sees the first error. The retry is there
/// to beat a clash, not to rename the error when it did not help.
pub(crate) async fn run_worktree(
    git: &dyn GitCli,
    repo_root: &Path,
    args: &[&str],
) -> Result<GitOutput, GitError> {
    let _guard = WorktreeGuard::acquire(repo_root).await;
    let first = git.run(repo_root, args).await?;
    if first.success || !is_transient(&first.stderr) {
        return Ok(first);
    }
    tokio::time::sleep(RETRY_PAUSE).await;
    let second = git.run(repo_root, args).await?;
    if second.success {
        Ok(second)
    } else {
        Ok(first)
    }
}

#[cfg(test)]
mod tests;
