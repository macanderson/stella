//! Path confinement by *held directory descriptors* rather than by string
//! resolution (#938).
//!
//! [`crate::resolve_within_root`] answers a question about a name and then
//! hands the name back. Everything between that answer and the `open` that
//! follows it is a window: the resolver walks up to the deepest *existing*
//! ancestor and re-appends the components that do not exist yet, so for a
//! brand-new file the interior directories were never validated at all. Plant
//! a symlink at one of them — from the model's own `bash` tool running
//! concurrently, or from a `build.rs` in the repository under audit, both
//! ordinary operating conditions for this product — and `create_dir_all`
//! walks straight through it. `O_NOFOLLOW` on the final component does not
//! help, because the final component is not the one that moved.
//!
//! [`RootHandle`] closes it structurally. The workspace root is opened once
//! and the descriptor is kept; every component after it is opened
//! `openat(dirfd, name, O_DIRECTORY | O_NOFOLLOW)` off the descriptor before
//! it. A name is resolved exactly once, by the kernel, at the moment it is
//! used — there is no second resolution to race, and no string that a rename
//! can re-point after we have looked at it.
//!
//! # What confines the walk
//!
//! Not a `starts_with` comparison. Three properties, in this order:
//!
//! * **The descriptor stack.** `..` pops the stack instead of opening `".."`,
//!   and popping past the root is refused. Opening `".."` would be wrong:
//!   a directory renamed out of the tree while we hold its descriptor still
//!   answers `..` — with its *new* parent, outside the root.
//! * **`O_NOFOLLOW` on every interior component.** The kernel refuses a
//!   symlink rather than following one behind us.
//! * **Bounded, re-confined symlink expansion.** A symlink is not simply
//!   rejected — Stella works in trees that symlink internally (pnpm, Bazel),
//!   and `resolve_within_root` allowed a write through an in-root link — so
//!   the link is read with `readlinkat` and its target spliced back onto the
//!   pending components. A relative target is spliced as-is and every
//!   component of it is walked under the same rules. An absolute target is
//!   canonicalized *once, as a hint*, rebased onto the root, and then re-walked
//!   from the held root descriptor: the string is thrown away, so a swap
//!   between the `canonicalize` and the walk changes nothing about what gets
//!   opened. Expansion is capped at [`MAX_SYMLINK_EXPANSIONS`].
//!
//! # Deliberate differences from `resolve_within_root`
//!
//! * A **dangling relative** symlink inside the root now resolves (the write
//!   creates the target it names, inside the root) where the string resolver
//!   refused it. The string resolver refused because `canonicalize` could not
//!   tell it where the link pointed; `readlinkat` can, and a link that points
//!   inside the root is confined whether or not its target exists yet. A
//!   dangling *absolute* target is still refused — nothing can prove it lands
//!   inside the root.
//! * The empty path, `.`, and any path resolving to the root directory itself
//!   are refused: these open a *file*, and the root is not one.
//! * **Expansion stops at the leaf for the operations that mean the link
//!   itself.** [`RootHandle::stat`], [`RootHandle::read_link`] and
//!   [`RootHandle::remove_file`] resolve every *parent* under the same rules
//!   and then leave the last component alone. Reading and writing still expand
//!   it — those want the real file — but unlinking must not, or deleting
//!   `vendor/config.toml` removes what it points at (#940).
//!
//! # What this does not do
//!
//! It confines the tools in this crate. It is not a sandbox for the
//! subprocesses they spawn — `bash` can still write wherever the user's own
//! account can, and always could. Isolating that is structural (a container),
//! not a matter of how a path is resolved.
//!
//! Non-Unix has no `openat`, so [`RootHandle`] falls back to
//! [`crate::resolve_within_root`] there — the same weaker boundary the threat
//! model already concedes off Unix, behind the same API.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// How many symlink expansions one resolution may perform before it is
/// declared a loop. `ELOOP`'s usual kernel budget, for the same reason.
pub const MAX_SYMLINK_EXPANSIONS: usize = 40;

/// Why a confined filesystem operation did not happen.
///
/// The split matters to callers: an escape is a *refusal* and reads as
/// "path `x` escapes workspace root", while an [`RootError::Io`] is the file
/// tool's ordinary "failed to read `x`" and should not accuse the model of
/// trying to break out.
#[derive(Debug)]
pub enum RootError {
    /// The path named a location outside the workspace root — by climbing
    /// above it, by being absolute, or by going through a symlink that
    /// leaves it.
    Escapes(String),
    /// An ordinary filesystem failure: the file is missing, is a directory,
    /// is not readable.
    Io(io::Error),
}

impl RootError {
    /// True when this was a confinement refusal rather than a filesystem
    /// failure. Callers use it to pick between their two error messages.
    pub fn is_escape(&self) -> bool {
        matches!(self, Self::Escapes(_))
    }

    fn escapes(reason: impl Into<String>) -> Self {
        Self::Escapes(reason.into())
    }
}

impl std::fmt::Display for RootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Escapes(reason) => write!(f, "{reason}"),
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for RootError {}

impl From<io::Error> for RootError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// What a confined `stat` saw. Not [`std::fs::Metadata`]: that cannot be
/// built from an `fstatat` result on stable Rust, and the tools only ever ask
/// these three questions of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryStat {
    kind: EntryKind,
    len: u64,
}

/// The three answers the file tools distinguish. `Symlink` is reachable only
/// by a swap racing the walk's own `readlinkat`, and is reported honestly
/// rather than collapsed into `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

impl EntryStat {
    pub fn kind(&self) -> EntryKind {
        self.kind
    }
    pub fn is_file(&self) -> bool {
        self.kind == EntryKind::File
    }
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
    }
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// A file opened for writing through a confined walk, plus the two facts the
/// durable-write sequence needs and cannot recover afterwards: whether this
/// open *created* the entry, and which directory holds it.
#[derive(Debug)]
pub struct WriteTarget {
    /// The open file. Not truncated — [`crate::durable_write`] rewrites in
    /// place and truncates only after the new bytes have landed.
    pub file: std::fs::File,
    /// True when this open created the entry, so its directory entry is new
    /// state that has to reach the disk. Answered by `O_EXCL` rather than by
    /// a `stat` that would race the open.
    pub created: bool,
    #[cfg(unix)]
    parent: std::os::fd::OwnedFd,
}

impl WriteTarget {
    /// `fsync` the directory holding the file, so a newly created entry
    /// survives a power loss. Best-effort, and on the descriptor the walk
    /// already holds — re-opening the parent *by path* would be a second
    /// resolution of exactly the kind this module exists to remove.
    pub fn sync_parent(&self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd as _;
            // SAFETY: `self.parent` is an open directory descriptor owned by
            // this struct; `fsync` neither consumes nor closes it.
            unsafe { libc::fsync(self.parent.as_raw_fd()) };
        }
    }
}

/// The workspace root, held open.
///
/// Every path handed to a method is workspace-relative and is resolved by
/// walking descriptors from this one. See the module header for what confines
/// the walk.
pub struct RootHandle {
    #[cfg(unix)]
    dir: std::os::fd::OwnedFd,
    /// The canonical root. Used for *display*, and as the rebase target when
    /// an absolute symlink has to be translated into a root-relative walk. It
    /// is never re-opened by name.
    canon_root: PathBuf,
}

impl RootHandle {
    /// The canonical workspace root. Display and diagnostics only — opening
    /// it again by this path would reintroduce the race.
    pub fn path(&self) -> &Path {
        &self.canon_root
    }

    /// Whether `rel` is *definitely* an escape, asked without opening the
    /// leaf and without requiring the interior directories to exist yet.
    ///
    /// A filesystem error is not an answer to this question — a path whose
    /// parent has not been created is not an escape — so only a refusal is
    /// reported. Callers use it to reject early (before a 64 MB download
    /// crosses the wire); the authority remains the walk performed by the
    /// operation itself.
    pub fn is_confined(&self, rel: &str) -> Result<(), RootError> {
        match self.resolve_leaf(rel, false) {
            Ok(_) => Ok(()),
            Err(RootError::Escapes(reason)) => Err(RootError::Escapes(reason)),
            Err(RootError::Io(_)) => Ok(()),
        }
    }

    /// Read `rel` whole.
    pub fn read(&self, rel: &str) -> Result<Vec<u8>, RootError> {
        use std::io::Read as _;
        let mut file = self.open_read(rel)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    /// Read `rel` whole and decode it as UTF-8.
    pub fn read_to_string(&self, rel: &str) -> Result<String, RootError> {
        let bytes = self.read(rel)?;
        String::from_utf8(bytes)
            .map_err(|error| RootError::Io(io::Error::new(io::ErrorKind::InvalidData, error)))
    }
}

/// [`RootHandle::read`] on a blocking worker, for the async tools.
///
/// The walk is a handful of `openat`s and the read is unbounded, so it goes
/// where every other blocking file operation in the crate goes rather than
/// stalling a reactor thread.
pub async fn read_async(handle: &Arc<RootHandle>, rel: &str) -> Result<Vec<u8>, RootError> {
    let (handle, rel) = (Arc::clone(handle), rel.to_string());
    tokio::task::spawn_blocking(move || handle.read(&rel))
        .await
        .map_err(|error| RootError::Io(io::Error::other(format!("read task failed: {error}"))))?
}

/// [`RootHandle::read_to_string`] on a blocking worker.
pub async fn read_to_string_async(
    handle: &Arc<RootHandle>,
    rel: &str,
) -> Result<String, RootError> {
    let (handle, rel) = (Arc::clone(handle), rel.to_string());
    tokio::task::spawn_blocking(move || handle.read_to_string(&rel))
        .await
        .map_err(|error| RootError::Io(io::Error::other(format!("read task failed: {error}"))))?
}

// --- One-shot helpers -------------------------------------------------------
//
// For the callers that touch exactly one file and have no handle to reuse:
// the registry's schema gate and its file-touch ledger, both of which look at
// a single path on a path that is already rare. Each pays for a `canonicalize`
// and an `open` of the root; a caller with a *list* of paths should open a
// [`RootHandle`] once and loop instead. All of them collapse a refusal and a
// failure into the same "no answer", because that is what these callers do
// with both: an unreadable file has no current content and no digest.

/// Open `root` and read one file under it, confined.
pub fn read_confined_bytes(root: &Path, rel: &str) -> Option<Vec<u8>> {
    RootHandle::open(root)
        .and_then(|handle| handle.read(rel))
        .ok()
}

/// [`read_confined_bytes`] decoded as UTF-8; `None` if it is not text.
pub fn read_confined(root: &Path, rel: &str) -> Option<String> {
    read_confined_bytes(root, rel).and_then(|bytes| String::from_utf8(bytes).ok())
}

/// [`read_confined_bytes`] decoded *lossily*, so a binary file still yields a
/// deterministic (if approximate) line count for the file-touch ledger rather
/// than reporting the touch as zero lines long.
pub fn read_confined_lossy(root: &Path, rel: &str) -> Option<String> {
    read_confined_bytes(root, rel).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// Does `rel` name something that exists inside `root`?
///
/// The create-vs-update question the ledger asks before a write. Answered
/// through the confined walk, so a swapped intermediate cannot make a create
/// look like an update by pointing at a file outside the workspace.
pub fn exists_confined(root: &Path, rel: &str) -> bool {
    RootHandle::open(root).is_ok_and(|handle| handle.stat(rel).is_ok())
}

// ---------------------------------------------------------------------------
// Unix: the real thing.
// ---------------------------------------------------------------------------

#[cfg(unix)]
use std::ffi::{CStr, CString, OsStr, OsString};
#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd};

/// One pending path component. `.` never becomes a step; it is dropped when
/// the components are parsed.
#[cfg(unix)]
enum Step {
    Name(OsString),
    Parent,
}

#[cfg(unix)]
impl RootHandle {
    /// Open `root` and hold it. One `canonicalize` and one `open` for the
    /// whole handle, replacing the three-to-five full canonicalize walks a
    /// single `write_file` used to perform.
    pub fn open(root: &Path) -> Result<Self, RootError> {
        let canon_root = root.canonicalize().map_err(RootError::Io)?;
        let name = CString::new(canon_root.as_os_str().as_encoded_bytes())
            .map_err(|_| RootError::escapes("workspace root path contains a NUL byte"))?;
        // No `O_NOFOLLOW` here: the root the operator configured is allowed to
        // BE a symlink, and it has just been canonicalized anyway. Everything
        // below it is the part that must not move.
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;
        // SAFETY: `name` is a NUL-terminated C string that outlives the call;
        // `open` returns a fresh descriptor which `OwnedFd` takes ownership of.
        let fd = unsafe { libc::open(name.as_ptr(), flags) };
        if fd < 0 {
            return Err(RootError::Io(io::Error::last_os_error()));
        }
        // SAFETY: `fd` is a freshly opened descriptor owned by nothing else.
        let dir = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok(Self { dir, canon_root })
    }

    /// Open `rel` for reading.
    pub fn open_read(&self, rel: &str) -> Result<std::fs::File, RootError> {
        let (parent, name) = self.resolve_leaf(rel, false)?;
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let fd = openat(parent.as_fd(), &name, flags, 0).map_err(RootError::Io)?;
        Ok(std::fs::File::from(fd))
    }

    /// Open `rel` for writing, creating it if absent, and — when
    /// `create_parents` — creating the interior directories *as part of the
    /// walk*, so each one is created relative to the descriptor above it
    /// rather than by a `create_dir_all` that re-resolves the whole prefix.
    pub fn open_write(&self, rel: &str, create_parents: bool) -> Result<WriteTarget, RootError> {
        let (parent, name) = self.resolve_leaf(rel, create_parents)?;
        let flags = libc::O_WRONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // `O_EXCL` first: it makes the kernel answer "did this open create the
        // file", which decides whether the directory entry needs its own
        // `fsync`. A `stat` before the open would only be a guess that races.
        match openat(
            parent.as_fd(),
            &name,
            flags | libc::O_CREAT | libc::O_EXCL,
            0o666,
        ) {
            Ok(fd) => Ok(WriteTarget {
                file: std::fs::File::from(fd),
                created: true,
                parent,
            }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let fd = openat(parent.as_fd(), &name, flags, 0).map_err(RootError::Io)?;
                Ok(WriteTarget {
                    file: std::fs::File::from(fd),
                    created: false,
                    parent,
                })
            }
            Err(error) => Err(RootError::Io(error)),
        }
    }

    /// `lstat` `rel` through the confined walk — the link itself, never its
    /// target.
    ///
    /// Resolved without expanding a final symlink, so a link reports
    /// [`EntryKind::Symlink`] rather than whatever it points at. Expanding it
    /// here is what let `delete_file` classify `vendor/config.toml` as an
    /// ordinary file and then remove the file it pointed to (#940).
    pub fn stat(&self, rel: &str) -> Result<EntryStat, RootError> {
        let (parent, name) = self.resolve_leaf_no_follow(rel)?;
        fstatat(parent.as_fd(), &name).map_err(RootError::Io)
    }

    /// Read the target of the symlink at `rel`, without following it.
    ///
    /// The target is returned verbatim, exactly as stored — it is a label for
    /// the operator ("this is where the link pointed"), not a path to act on,
    /// and resolving it would defeat the point of asking.
    pub fn read_link(&self, rel: &str) -> Result<std::ffi::OsString, RootError> {
        let (parent, name) = self.resolve_leaf_no_follow(rel)?;
        readlinkat(parent.as_fd(), &name).map_err(RootError::Io)
    }

    /// Unlink `rel` — the link itself when `rel` names a symlink.
    ///
    /// `unlinkat` not following a symlink only helps if the name it is handed
    /// is still the link's own name, so this resolves without expanding the
    /// leaf. With the expanding walk the name was already the target's real
    /// name in the target's real directory, and `unlinkat`'s no-follow
    /// guarantee bought nothing: deleting a link deleted its target.
    pub fn remove_file(&self, rel: &str) -> Result<(), RootError> {
        let (parent, name) = self.resolve_leaf_no_follow(rel)?;
        // SAFETY: `parent` is an open directory descriptor and `name` is a
        // NUL-terminated C string that outlives the call.
        let rc = unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) };
        if rc < 0 {
            return Err(RootError::Io(io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Walk `rel` and return the descriptor of the directory that will hold
    /// the final name, plus that name.
    ///
    /// The leaf is deliberately *not* opened here: which flags it wants is the
    /// caller's business, and the leaf is the one component `O_NOFOLLOW`
    /// already protected. What this does guarantee about it is that it is not
    /// a symlink *as of the `readlinkat` below* — a link is expanded, so the
    /// name returned is the real name in the real directory.
    fn resolve_leaf(&self, rel: &str, create_dirs: bool) -> Result<(OwnedFd, CString), RootError> {
        self.resolve_leaf_inner(rel, create_dirs, true)
    }

    /// [`Self::resolve_leaf`], but stopping at the leaf's own name.
    ///
    /// Every parent component is still walked under `O_NOFOLLOW`, so
    /// containment is enforced exactly as everywhere else — including through
    /// an intermediate symlinked directory pointing outside the root, which
    /// still rejects. Only the last component is left unexpanded, which is what
    /// "operate on the link, not its target" requires.
    fn resolve_leaf_no_follow(&self, rel: &str) -> Result<(OwnedFd, CString), RootError> {
        self.resolve_leaf_inner(rel, false, false)
    }

    fn resolve_leaf_inner(
        &self,
        rel: &str,
        create_dirs: bool,
        follow_leaf: bool,
    ) -> Result<(OwnedFd, CString), RootError> {
        let mut queue: std::collections::VecDeque<Step> = std::collections::VecDeque::new();
        splice(&mut queue, Path::new(rel), rel)?;

        // `stack[0]` is the root. `..` pops it rather than opening `".."`:
        // a directory renamed out of the tree under us still answers `..`,
        // and would answer with its new parent.
        let mut stack: Vec<OwnedFd> = vec![self.dir.try_clone().map_err(RootError::Io)?];
        let mut expansions = 0usize;

        loop {
            let Some(step) = queue.pop_front() else {
                return Err(RootError::escapes(format!(
                    "`{rel}` does not name a file inside the workspace root"
                )));
            };
            let name = match step {
                Step::Parent => {
                    if stack.len() == 1 {
                        return Err(RootError::escapes(format!(
                            "`{rel}` climbs above the workspace root"
                        )));
                    }
                    stack.pop();
                    continue;
                }
                Step::Name(name) => name,
            };
            let cname = cstring(&name, rel)?;
            let here = stack.last().expect("the root is never popped").as_fd();

            if queue.is_empty() {
                // The leaf. A caller that wants the link itself stops here:
                // the parents are already walked and confined, and expanding
                // this last name is precisely what it must not do.
                if !follow_leaf {
                    let parent = stack.pop().expect("the root is never popped");
                    return Ok((parent, cname));
                }
                // `readlinkat` asks "is this a symlink" without opening
                // anything, which is the whole point: opening it is exactly
                // what must not happen until we know where it goes.
                match readlinkat(here, &cname) {
                    Ok(target) => {
                        self.expand(&mut queue, &mut stack, &mut expansions, &target, rel)?;
                        continue;
                    }
                    // Not a symlink (`EINVAL`), or not there at all (`ENOENT`,
                    // the ordinary create case). Either way this is the name.
                    Err(_) => {
                        let parent = stack.pop().expect("the root is never popped");
                        return Ok((parent, cname));
                    }
                }
            }

            // An interior component: it must be a real directory, and the
            // kernel must refuse a symlink rather than follow one behind us.
            let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
            let opened = match openat(here, &cname, flags, 0) {
                Ok(fd) => fd,
                Err(error) if error.kind() == io::ErrorKind::NotFound && create_dirs => {
                    // This is what replaces `create_dir_all`: the directory is
                    // created relative to the descriptor we are standing on,
                    // so the prefix is never re-resolved. `AlreadyExists` is a
                    // benign concurrent create, not a failure.
                    match mkdirat(here, &cname, 0o777) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(RootError::Io(error)),
                    }
                    openat(here, &cname, flags, 0).map_err(RootError::Io)?
                }
                Err(error) => {
                    // Either a symlink — `O_NOFOLLOW` refused it — or a real
                    // failure. `readlinkat` tells the two apart without this
                    // code having to know which errno the platform picked
                    // (`ELOOP` on Linux and macOS, `EMLINK` on some BSDs).
                    match readlinkat(here, &cname) {
                        Ok(target) => {
                            self.expand(&mut queue, &mut stack, &mut expansions, &target, rel)?;
                            continue;
                        }
                        Err(_) => return Err(RootError::Io(error)),
                    }
                }
            };
            stack.push(opened);
        }
    }

    /// Splice a symlink's target back onto the pending components.
    ///
    /// A relative target is spliced as-is: it is interpreted against the
    /// directory whose descriptor is on top of the stack, which is exactly
    /// what the kernel would have done, except that every component of it is
    /// now walked under the same rules.
    ///
    /// An absolute target is the only place a *string* is consulted, and it is
    /// consulted as a hint: `canonicalize` says where the link claims to go,
    /// the claim is checked against the canonical root, and then the string is
    /// discarded and the remainder is re-walked from the held root descriptor.
    /// A swap between the two changes which components exist, not which
    /// descriptors the walk is allowed to reach.
    fn expand(
        &self,
        queue: &mut std::collections::VecDeque<Step>,
        stack: &mut Vec<OwnedFd>,
        expansions: &mut usize,
        target: &OsStr,
        rel: &str,
    ) -> Result<(), RootError> {
        *expansions += 1;
        if *expansions > MAX_SYMLINK_EXPANSIONS {
            return Err(RootError::Io(io::Error::from_raw_os_error(libc::ELOOP)));
        }
        let target = Path::new(target);
        if !target.is_absolute() {
            return splice_front(queue, target, rel);
        }
        let Ok(canon) = target.canonicalize() else {
            return Err(RootError::escapes(format!(
                "`{rel}` goes through a symlink to `{}`, which does not resolve inside the \
                 workspace root",
                target.display()
            )));
        };
        let Ok(inside) = canon.strip_prefix(&self.canon_root) else {
            return Err(RootError::escapes(format!(
                "`{rel}` goes through a symlink to `{}`, outside the workspace root",
                canon.display()
            )));
        };
        stack.truncate(1);
        splice_front(queue, inside, rel)
    }
}

/// Parse `path` into steps and append them. `.` is dropped; an absolute path
/// is a refusal, because a workspace-relative name that starts at `/` names
/// something that is not in the workspace.
#[cfg(unix)]
fn splice(
    queue: &mut std::collections::VecDeque<Step>,
    path: &Path,
    rel: &str,
) -> Result<(), RootError> {
    for step in steps_of(path, rel)? {
        queue.push_back(step);
    }
    Ok(())
}

/// The same, onto the front, preserving order.
#[cfg(unix)]
fn splice_front(
    queue: &mut std::collections::VecDeque<Step>,
    path: &Path,
    rel: &str,
) -> Result<(), RootError> {
    for step in steps_of(path, rel)?.into_iter().rev() {
        queue.push_front(step);
    }
    Ok(())
}

#[cfg(unix)]
fn steps_of(path: &Path, rel: &str) -> Result<Vec<Step>, RootError> {
    use std::path::Component;
    let mut steps = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => steps.push(Step::Name(name.to_os_string())),
            Component::CurDir => {}
            Component::ParentDir => steps.push(Step::Parent),
            Component::RootDir | Component::Prefix(_) => {
                return Err(RootError::escapes(format!(
                    "`{rel}` is absolute, so it names a location outside the workspace root"
                )));
            }
        }
    }
    Ok(steps)
}

#[cfg(unix)]
fn cstring(name: &OsStr, rel: &str) -> Result<CString, RootError> {
    use std::os::unix::ffi::OsStrExt as _;
    CString::new(name.as_bytes())
        .map_err(|_| RootError::escapes(format!("`{rel}` contains a NUL byte")))
}

#[cfg(unix)]
fn openat(dir: BorrowedFd<'_>, name: &CStr, flags: i32, mode: libc::mode_t) -> io::Result<OwnedFd> {
    // SAFETY: `dir` is an open descriptor for the duration of the borrow and
    // `name` is a NUL-terminated C string that outlives the call. The returned
    // descriptor is fresh and owned by nothing else.
    let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags, mode as libc::c_uint) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a freshly opened descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(unix)]
fn mkdirat(dir: BorrowedFd<'_>, name: &CStr, mode: libc::mode_t) -> io::Result<()> {
    // SAFETY: as `openat` above.
    let rc = unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), mode) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn readlinkat(dir: BorrowedFd<'_>, name: &CStr) -> io::Result<OsString> {
    use std::os::unix::ffi::OsStringExt as _;
    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: as `openat` above; `buf` is writable for `buf.len()` bytes.
    let read = unsafe {
        libc::readlinkat(
            dir.as_raw_fd(),
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if read < 0 {
        return Err(io::Error::last_os_error());
    }
    let read = read as usize;
    if read >= buf.len() {
        // Truncated: we cannot tell where the link goes, so we must not guess.
        return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
    }
    buf.truncate(read);
    Ok(OsString::from_vec(buf))
}

#[cfg(unix)]
fn fstatat(dir: BorrowedFd<'_>, name: &CStr) -> io::Result<EntryStat> {
    // SAFETY: `libc::stat` is a plain C struct with no invalid bit patterns;
    // `fstatat` fills it or fails without reading it.
    let mut raw: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: as `openat` above; `raw` is a valid writable `struct stat`.
    let rc = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            &mut raw,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    let kind = match raw.st_mode & libc::S_IFMT {
        libc::S_IFREG => EntryKind::File,
        libc::S_IFDIR => EntryKind::Dir,
        libc::S_IFLNK => EntryKind::Symlink,
        _ => EntryKind::Other,
    };
    Ok(EntryStat {
        kind,
        len: raw.st_size.max(0) as u64,
    })
}

// ---------------------------------------------------------------------------
// Non-Unix: the same API over the string resolver.
// ---------------------------------------------------------------------------

#[cfg(not(unix))]
impl RootHandle {
    pub fn open(root: &Path) -> Result<Self, RootError> {
        let canon_root = root.canonicalize().map_err(RootError::Io)?;
        Ok(Self { canon_root })
    }

    pub fn open_read(&self, rel: &str) -> Result<std::fs::File, RootError> {
        let full = self.resolve_leaf(rel, false)?;
        std::fs::File::open(full).map_err(RootError::Io)
    }

    pub fn open_write(&self, rel: &str, create_parents: bool) -> Result<WriteTarget, RootError> {
        let full = self.resolve_leaf(rel, create_parents)?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&full)
        {
            Ok(file) => Ok(WriteTarget {
                file,
                created: true,
            }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(false)
                    .open(&full)
                    .map_err(RootError::Io)?;
                Ok(WriteTarget {
                    file,
                    created: false,
                })
            }
            Err(error) => Err(RootError::Io(error)),
        }
    }

    pub fn stat(&self, rel: &str) -> Result<EntryStat, RootError> {
        let full = self.resolve_leaf(rel, false)?;
        let meta = std::fs::symlink_metadata(full).map_err(RootError::Io)?;
        let kind = if meta.is_dir() {
            EntryKind::Dir
        } else if meta.is_file() {
            EntryKind::File
        } else if meta.file_type().is_symlink() {
            EntryKind::Symlink
        } else {
            EntryKind::Other
        };
        Ok(EntryStat {
            kind,
            len: meta.len(),
        })
    }

    pub fn remove_file(&self, rel: &str) -> Result<(), RootError> {
        let full = self.resolve_leaf(rel, false)?;
        std::fs::remove_file(full).map_err(RootError::Io)
    }

    /// Off Unix there is no `openat`, so this is the old string resolution
    /// with the old window — see the module header and the threat model's
    /// "non-Unix platforms are materially weaker".
    fn resolve_leaf(&self, rel: &str, create_parents: bool) -> Result<PathBuf, RootError> {
        let full = crate::resolve_within_root(&self.canon_root, rel).ok_or_else(|| {
            RootError::escapes(format!(
                "`{rel}` names a location outside the workspace root"
            ))
        })?;
        if create_parents && let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(RootError::Io)?;
        }
        Ok(full)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stella-rootfd-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn a_nested_write_creates_its_parents_inside_the_root() {
        let root = scratch("nested");
        let handle = RootHandle::open(&root).expect("open root");
        let target = handle.open_write("a/b/c.txt", true).expect("write");
        assert!(target.created);
        assert!(root.join("a/b/c.txt").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn dotdot_cannot_climb_out_even_through_a_real_directory() {
        let root = scratch("dotdot");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let handle = RootHandle::open(&root).expect("open root");

        // `sub/..` lands back on the root, which is fine; one more level is not.
        assert!(handle.open_write("sub/../ok.txt", true).is_ok());
        let error = handle
            .open_write("sub/../../escaped.txt", true)
            .expect_err("must not climb out");
        assert!(error.is_escape(), "{error}");
        assert!(!root.parent().unwrap().join("escaped.txt").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_absolute_path_is_refused() {
        let root = scratch("absolute");
        let handle = RootHandle::open(&root).expect("open root");
        let error = handle
            .open_write("/etc/passwd", false)
            .expect_err("absolute must be refused");
        assert!(error.is_escape(), "{error}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// The counterpart of `lib.rs`'s
    /// `resolve_within_root_allows_write_through_an_in_root_symlink`: a tree
    /// that symlinks internally still works, and the target is the real file.
    #[test]
    fn a_write_through_an_in_root_symlink_lands_on_the_real_file() {
        let root = scratch("inlink");
        std::fs::create_dir_all(root.join("real")).unwrap();
        // Absolute target, exactly as the `lib.rs` test builds it — this is
        // the case that constrains the rebase branch.
        std::os::unix::fs::symlink(root.join("real"), root.join("alias")).unwrap();
        let handle = RootHandle::open(&root).expect("open root");

        handle.open_write("alias/new.txt", true).expect("allowed");

        assert!(
            root.join("real/new.txt").exists(),
            "must land on the target"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_symlink_to_an_outside_directory_is_refused() {
        let root = scratch("outlink-root");
        let outside = scratch("outlink-out");
        std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();
        let handle = RootHandle::open(&root).expect("open root");

        let error = handle
            .open_write("out/evil.txt", true)
            .expect_err("outward symlink must be refused");
        assert!(error.is_escape(), "{error}");
        assert!(!outside.join("evil.txt").exists());
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[test]
    fn a_dangling_absolute_symlink_is_refused() {
        let root = scratch("dangling");
        let missing = std::env::temp_dir().join(format!(
            "stella-rootfd-never-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::os::unix::fs::symlink(&missing, root.join("link")).unwrap();
        let handle = RootHandle::open(&root).expect("open root");

        let error = handle
            .open_write("link", false)
            .expect_err("a dangling absolute symlink must be refused");
        assert!(error.is_escape(), "{error}");
        assert!(!missing.exists(), "nothing may be created outside the root");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_symlink_cycle_ends_in_eloop_rather_than_spinning() {
        let root = scratch("cycle");
        std::os::unix::fs::symlink("b", root.join("a")).unwrap();
        std::os::unix::fs::symlink("a", root.join("b")).unwrap();
        let handle = RootHandle::open(&root).expect("open root");

        let error = handle.open_write("a", false).expect_err("cycle");
        match error {
            RootError::Io(error) => {
                assert_eq!(error.raw_os_error(), Some(libc::ELOOP), "{error}");
            }
            other => panic!("expected ELOOP, got {other}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn reads_writes_stats_and_unlinks_agree_on_the_same_file() {
        let root = scratch("crud");
        let handle = RootHandle::open(&root).expect("open root");

        {
            use std::io::Write as _;
            let target = handle.open_write("notes/today.md", true).expect("create");
            (&target.file).write_all(b"hello").expect("write");
            target.sync_parent();
        }
        assert_eq!(handle.read_to_string("notes/today.md").unwrap(), "hello");
        let stat = handle.stat("notes/today.md").expect("stat");
        assert!(stat.is_file());
        assert_eq!(stat.len(), 5);
        assert!(handle.stat("notes").expect("stat dir").is_dir());
        handle.remove_file("notes/today.md").expect("unlink");
        assert!(handle.stat("notes/today.md").is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn is_confined_answers_for_a_path_whose_parents_do_not_exist_yet() {
        let root = scratch("confined");
        let handle = RootHandle::open(&root).expect("open root");
        assert!(handle.is_confined("not/here/yet.txt").is_ok());
        assert!(handle.is_confined("../../etc/passwd").is_err());
        assert!(handle.is_confined("/etc/passwd").is_err());
        std::fs::remove_dir_all(&root).ok();
    }
}
