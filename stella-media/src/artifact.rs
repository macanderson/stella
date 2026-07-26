//! The artifact store: the single writer to `.stella/artifacts/`, so the
//! agent cannot overwrite arbitrary paths via a generation tool. Every
//! generated file lands as `<id>.<ext>` *inside* the caller-supplied root, and
//! a manifest row records its provenance.
//!
//! Path-traversal safety is structural: ids are generated here (never
//! caller-supplied), and the filename is sanitized to `[a-z0-9_-]` — every
//! other byte, including `.` and both path separators, is dropped, so neither
//! a separator nor a `..` can survive into a filename fragment and no id or
//! label can escape the root. The sanitizer is exercised directly by a
//! hostile-input test even though the generated-id path already makes escape
//! impossible.
//!
//! The manifest is `manifest.json` (a JSON array). Writes are atomic against
//! process death: the store reads the current array, inserts or replaces the
//! row for that path, serializes to a sibling temp file, `fsync`s it, then
//! `rename`s it over the manifest — so a crash mid-write never leaves a
//! partially written or corrupt manifest. That read → upsert → rename cycle
//! runs under an advisory lock on `manifest.json.lock`, so two stores over one
//! root (in one process or two) cannot each read the pre-image and lose a row.
//! The *directory* is never `fsync`ed, so a rename the kernel has not yet
//! flushed is still lost to power loss: this is crash atomicity, not power-loss
//! durability.
//!
//! The artifact file itself is written *outside* that lock (`create_new` +
//! `fsync`, then the manifest upsert), so two processes racing on one
//! deterministic video id can still interleave — the loser may read the
//! winner's not-yet-written file and report a spurious digest mismatch.
//! Recorded rather than fixed: one CLI process per workspace is the shipped
//! shape, and widening the lock to cover the file write needs a lock handle
//! threaded through `save_with_id` (a second `File::lock` on the same path
//! from one process would block on itself).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use stella_protocol::{MediaArtifactRef, MediaKind};

use crate::error::MediaError;
use crate::provider::MediaArtifact;

const MANIFEST_NAME: &str = "manifest.json";

/// One recorded artifact in `manifest.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub id: String,
    pub kind: MediaKind,
    /// Path relative to the artifact root, e.g. `med_ab12cd34ef56.png`.
    pub path: String,
    /// Lowercase hex SHA-256 of the file bytes.
    pub sha256: String,
    pub label: String,
    /// Unix seconds at write time.
    pub created_at: u64,
    pub byte_size: u64,
}

/// A filesystem-jailed writer + manifest for one workspace's
/// `.stella/artifacts/` directory.
pub struct ArtifactStore {
    root: PathBuf,
    /// Monotonic component of generated ids, so two saves of byte-identical
    /// content in the same process still get distinct filenames.
    seq: AtomicU64,
}

impl ArtifactStore {
    /// Open (creating if absent) an artifact store rooted at `root`. `root`
    /// is trusted — it is the session's pinned `.stella/artifacts/` path, not
    /// anything derived from model output.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, MediaError> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|e| {
            MediaError::Artifact(format!(
                "cannot create artifact root {}: {e}",
                root.display()
            ))
        })?;
        Ok(Self {
            root,
            seq: AtomicU64::new(0),
        })
    }

    /// The artifact root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Persist a [`MediaArtifact`], using its declared extension and label.
    /// The id is generated; returns the citation-friendly
    /// [`MediaArtifactRef`].
    pub fn save_artifact(&self, art: &MediaArtifact) -> Result<MediaArtifactRef, MediaError> {
        self.save_with_ext(&art.bytes, art.kind, &art.extension, &art.label)
    }

    /// Persist raw bytes, deriving the extension from the kind
    /// (`Image`→`png`, `Svg`→`svg`, `Video`→`mp4`).
    pub fn save(
        &self,
        bytes: &[u8],
        kind: MediaKind,
        label: &str,
    ) -> Result<MediaArtifactRef, MediaError> {
        self.save_with_ext(bytes, kind, default_extension(kind), label)
    }

    /// Persist raw bytes with an explicit extension (for adapters that emit
    /// e.g. `jpeg`). Id is generated here.
    pub fn save_with_ext(
        &self,
        bytes: &[u8],
        kind: MediaKind,
        extension: &str,
        label: &str,
    ) -> Result<MediaArtifactRef, MediaError> {
        let id = self.generate_id(bytes);
        self.save_with_id(&id, bytes, kind, extension, label)
    }

    /// Persist under a specific id — used for video, whose artifact id was
    /// assigned at submit so events and the final file share one identity.
    /// The id is sanitized here, so even a hostile id cannot escape the root.
    pub fn save_with_id(
        &self,
        id: &str,
        bytes: &[u8],
        kind: MediaKind,
        extension: &str,
        label: &str,
    ) -> Result<MediaArtifactRef, MediaError> {
        let safe_id = sanitize_component(id);
        let safe_ext = sanitize_component(extension);
        let filename = format!("{safe_id}.{safe_ext}");
        let dest = self.root.join(&filename);

        // Structural jail check: the resolved parent must be the root.
        if dest.parent() != Some(self.root.as_path()) {
            return Err(MediaError::Artifact(format!(
                "refusing to write outside the artifact root: {}",
                dest.display()
            )));
        }

        let sha256 = sha256_hex(bytes);
        let entry = ManifestEntry {
            id: safe_id.clone(),
            kind,
            path: filename.clone(),
            sha256: sha256.clone(),
            label: label.to_string(),
            created_at: now_unix_secs(),
            byte_size: bytes.len() as u64,
        };
        let artifact = MediaArtifactRef {
            id: safe_id,
            kind,
            path: filename.clone(),
            label: label.to_string(),
        };

        // `create_new`, not `write`: a plain write truncates whatever is
        // already at this path. Video ids are derived deterministically from
        // the provider job id, so polling a completed job twice — the normal
        // resume flow — lands on the same filename, and the truncating write
        // plus an appending manifest left two contradictory rows for one
        // path with different digests. On unix this is O_EXCL|O_CREAT, so it
        // also refuses to follow a symlink planted at `dest`.
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)
        {
            Ok(mut file) => {
                use std::io::Write as _;
                file.write_all(bytes).map_err(|e| {
                    MediaError::Artifact(format!("cannot write {}: {e}", dest.display()))
                })?;
                // The manifest row is a durability claim about these bytes.
                file.sync_all().map_err(|e| {
                    MediaError::Artifact(format!("cannot fsync {}: {e}", dest.display()))
                })?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = std::fs::read(&dest).map_err(|e| {
                    MediaError::Artifact(format!(
                        "cannot read existing artifact {}: {e}",
                        dest.display()
                    ))
                })?;
                let existing_sha = sha256_hex(&existing);
                if existing_sha != sha256 {
                    return Err(MediaError::Artifact(format!(
                        "artifact {filename} already exists with different content \
                         (existing sha256 {existing_sha}, new {sha256}) — refusing to overwrite"
                    )));
                }
                // Same bytes: the idempotent re-save. Still upsert the row,
                // so a crash between the write and the manifest append heals
                // on the next poll rather than leaving an unlisted file.
                self.upsert_manifest(&entry)?;
                return Ok(artifact);
            }
            Err(e) => {
                return Err(MediaError::Artifact(format!(
                    "cannot write {}: {e}",
                    dest.display()
                )));
            }
        }

        self.upsert_manifest(&entry)?;
        Ok(artifact)
    }

    /// Read the manifest back (for a `gen list`-style listing). A missing
    /// manifest is an empty list, not an error.
    pub fn entries(&self) -> Result<Vec<ManifestEntry>, MediaError> {
        let path = self.root.join(MANIFEST_NAME);
        match std::fs::read_to_string(&path) {
            Ok(text) if text.trim().is_empty() => Ok(Vec::new()),
            Ok(text) => serde_json::from_str(&text).map_err(|e| {
                MediaError::Artifact(format!("manifest {} is corrupt: {e}", path.display()))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(MediaError::Artifact(format!(
                "cannot read manifest {}: {e}",
                path.display()
            ))),
        }
    }

    /// Generate a collision-resistant, filesystem-safe id: `med_` + 16 hex
    /// chars of `SHA-256(bytes ‖ seq ‖ nanos)`. Content need not be unique;
    /// the seq+time salt keeps identical bytes from sharing a filename.
    fn generate_id(&self, bytes: &[u8]) -> String {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher.update(seq.to_le_bytes());
        hasher.update(nanos.to_le_bytes());
        let digest = hasher.finalize();
        let mut hex = String::with_capacity(16);
        push_hex(&mut hex, &digest[..8]);
        format!("med_{hex}")
    }

    /// Insert or replace the row for `entry.path` via read → mutate →
    /// temp-write → atomic rename, so the manifest is never observed
    /// half-written and one path never has two rows.
    ///
    /// Replace, not append: `path` is physically unique inside the root, so
    /// a second row for it can only ever contradict the first. Appending is
    /// what left a re-saved artifact with two rows carrying different
    /// digests for the same file.
    ///
    /// The whole read-modify-write is held under an advisory lock file, and
    /// the temp name carries pid + counter. Without both, two processes (or
    /// two `ArtifactStore` handles) could each read the pre-image and lose a
    /// row, or collide mid-write on one fixed temp path. `JobStore` in this
    /// crate already solved exactly this; [`mutation_lock`] is that solution
    /// lifted so both callers share one.
    ///
    /// The whole array is rewritten per save, which is O(n) in rows and fine
    /// at CLI volumes; it is the reason the manifest wants a bound or an
    /// append-only format before it becomes a long-lived log.
    fn upsert_manifest(&self, entry: &ManifestEntry) -> Result<(), MediaError> {
        let _lock = mutation_lock(&self.root.join(format!("{MANIFEST_NAME}.lock")))?;

        let mut entries = self.entries()?;
        match entries.iter_mut().find(|row| row.path == entry.path) {
            Some(existing) => *existing = entry.clone(),
            None => entries.push(entry.clone()),
        }
        let body = serde_json::to_string_pretty(&entries)
            .map_err(|e| MediaError::Artifact(format!("cannot serialize manifest: {e}")))?;

        let final_path = self.root.join(MANIFEST_NAME);
        let sequence = NEXT_MANIFEST_TEMP.fetch_add(1, Ordering::Relaxed);
        let tmp_path = self.root.join(format!(
            ".{MANIFEST_NAME}.tmp.{}.{sequence}",
            std::process::id()
        ));
        let commit = (|| {
            use std::io::Write as _;
            let mut file = std::fs::File::create(&tmp_path).map_err(|e| {
                MediaError::Artifact(format!(
                    "cannot write temp manifest {}: {e}",
                    tmp_path.display()
                ))
            })?;
            file.write_all(body.as_bytes()).map_err(|e| {
                MediaError::Artifact(format!(
                    "cannot write temp manifest {}: {e}",
                    tmp_path.display()
                ))
            })?;
            file.sync_all().map_err(|e| {
                MediaError::Artifact(format!(
                    "cannot fsync temp manifest {}: {e}",
                    tmp_path.display()
                ))
            })?;
            drop(file);
            std::fs::rename(&tmp_path, &final_path).map_err(|e| {
                MediaError::Artifact(format!(
                    "cannot commit manifest {}: {e}",
                    final_path.display()
                ))
            })
        })();
        if commit.is_err() {
            let _ = std::fs::remove_file(&tmp_path);
        }
        commit
    }
}

/// Counter for manifest temp names, so a second writer in this process
/// cannot collide with the first.
static NEXT_MANIFEST_TEMP: AtomicU64 = AtomicU64::new(0);

/// Take an advisory lock on `path`, creating it if absent, and hold it for
/// the caller's whole read-modify-write. Shared by [`ArtifactStore`] and
/// [`crate::jobs::JobStore`] — both rewrite a whole JSON document from a
/// pre-image, which is a lost-update race without this.
pub(crate) fn mutation_lock(path: &std::path::Path) -> Result<std::fs::File, MediaError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| MediaError::Artifact(format!("cannot create store dir: {e}")))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| MediaError::Artifact(format!("cannot open lock {}: {e}", path.display())))?;
    file.lock()
        .map_err(|e| MediaError::Artifact(format!("cannot lock {}: {e}", path.display())))?;
    Ok(file)
}

/// Default file extension per media kind.
fn default_extension(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Image => "png",
        MediaKind::Svg => "svg",
        MediaKind::Video => "mp4",
    }
}

/// Reduce a path component to a safe filename fragment: keep only
/// `[a-z0-9_-]` (lowercased), drop everything else — in particular path
/// separators and the `.` runs that could form `..`. Guarantees the result
/// contains no separator and is not a traversal token.
fn sanitize_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        let lowered = ch.to_ascii_lowercase();
        if lowered.is_ascii_alphanumeric() || matches!(lowered, '_' | '-') {
            out.push(lowered);
        }
        // '.', '/', '\\', and everything else are dropped: a dropped '.'
        // means `..` and `.` can never survive into a filename fragment.
    }
    if out.is_empty() { "x".to_string() } else { out }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    push_hex(&mut hex, &digest[..]);
    hex
}

/// Append `bytes` to `out` as lowercase hex. Spelled out rather than
/// `format!("{byte:02x}")` per byte: that allocates and drops a `String` for
/// every byte of every digest, on a path that runs twice per saved artifact.
fn push_hex(out: &mut String, bytes: &[u8]) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for &byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> (TempDir, ArtifactStore) {
        let dir = TempDir::new().unwrap();
        let store = ArtifactStore::open(dir.path().join("artifacts")).unwrap();
        (dir, store)
    }

    #[test]
    fn save_writes_inside_root_and_records_a_manifest_row() {
        let (_dir, store) = store();
        let art_ref = store
            .save(b"\x89PNG fake bytes", MediaKind::Image, "a-logo")
            .unwrap();

        assert!(art_ref.path.ends_with(".png"));
        assert!(store.root().join(&art_ref.path).exists());

        let entries = store.entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, art_ref.id);
        assert_eq!(entries[0].label, "a-logo");
        assert_eq!(entries[0].byte_size, b"\x89PNG fake bytes".len() as u64);
        assert_eq!(entries[0].sha256.len(), 64);
    }

    #[test]
    fn identical_bytes_saved_twice_get_distinct_files() {
        let (_dir, store) = store();
        let a = store.save(b"same", MediaKind::Image, "x").unwrap();
        let b = store.save(b"same", MediaKind::Image, "x").unwrap();
        assert_ne!(a.id, b.id, "seq+time salt must break the tie");
        assert!(store.root().join(&a.path).exists());
        assert!(store.root().join(&b.path).exists());
        assert_eq!(store.entries().unwrap().len(), 2);
    }

    #[test]
    fn save_artifact_uses_declared_extension() {
        let (_dir, store) = store();
        let art = MediaArtifact {
            kind: MediaKind::Video,
            bytes: b"mp4 bytes".to_vec(),
            extension: "mp4".into(),
            label: "teaser".into(),
            model: "cogvideox".into(),
            cost_usd: 2.0,
        };
        let art_ref = store.save_artifact(&art).unwrap();
        assert!(art_ref.path.ends_with(".mp4"));
    }

    #[test]
    fn hostile_id_cannot_escape_the_root() {
        let (_dir, store) = store();
        // A path-traversal id must be neutralized to a flat filename inside
        // the root — the file lands in the root, and nothing is written to
        // the parent.
        let art_ref = store
            .save_with_id("../../etc/passwd", b"x", MediaKind::Image, "png", "evil")
            .unwrap();
        assert!(!art_ref.path.contains('/'));
        assert!(!art_ref.path.contains(".."));
        let written = store.root().join(&art_ref.path);
        assert!(written.exists());
        assert_eq!(written.parent(), Some(store.root()));
        // The traversal target must not exist.
        assert!(!store.root().join("../../etc/passwd").exists());
    }

    #[test]
    fn hostile_extension_is_sanitized() {
        let (_dir, store) = store();
        let art_ref = store
            .save_with_id("med_1", b"x", MediaKind::Image, "../sh", "x")
            .unwrap();
        assert!(!art_ref.path.contains('/'));
        assert!(!art_ref.path.contains(".."));
    }

    #[test]
    fn sanitize_component_strips_separators_and_dots() {
        assert_eq!(sanitize_component("../../etc/passwd"), "etcpasswd");
        assert_eq!(sanitize_component("med_ab12-CD"), "med_ab12-cd");
        assert_eq!(sanitize_component("...."), "x");
        assert_eq!(sanitize_component("a/b\\c"), "abc");
    }

    #[test]
    fn missing_manifest_reads_as_empty() {
        let (_dir, store) = store();
        assert!(store.entries().unwrap().is_empty());
    }

    #[test]
    fn manifest_survives_many_appends_in_order() {
        let (_dir, store) = store();
        for i in 0..5 {
            store
                .save(format!("bytes-{i}").as_bytes(), MediaKind::Image, "x")
                .unwrap();
        }
        let entries = store.entries().unwrap();
        assert_eq!(entries.len(), 5);
        // No stray temp file left behind after the atomic renames. The name
        // now carries pid + counter, so this scans rather than probing one
        // fixed path.
        let strays: Vec<_> = std::fs::read_dir(store.root())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp."))
            .collect();
        assert!(strays.is_empty(), "temp manifests left behind: {strays:?}");
    }

    #[test]
    fn corrupt_manifest_is_a_named_error() {
        let (_dir, store) = store();
        std::fs::write(store.root().join(MANIFEST_NAME), "{ not an array").unwrap();
        let err = store.entries().unwrap_err();
        assert!(matches!(err, MediaError::Artifact(_)));
    }

    /// #617 — the resume flow. Video ids come from `artifact_id_for`, which
    /// hashes the provider job id, so polling a completed job twice lands on
    /// the same filename with the same bytes. `jobs.remove` only runs after
    /// the save, so an interrupted first poll *guarantees* a second one.
    ///
    /// Before the fix this truncated the file and appended a second manifest
    /// row for the same path.
    #[test]
    fn saving_the_same_id_twice_with_identical_bytes_is_idempotent() {
        let (_dir, store) = store();
        let bytes = b"the same video bytes";

        let first = store
            .save_with_id("med_abc", bytes, MediaKind::Video, "mp4", "clip")
            .expect("first save");
        let second = store
            .save_with_id("med_abc", bytes, MediaKind::Video, "mp4", "clip")
            .expect("a re-poll of a completed job must not fail");

        assert_eq!(first.path, second.path);
        let entries = store.entries().unwrap();
        assert_eq!(entries.len(), 1, "one file must have exactly one row");
        assert_eq!(entries[0].sha256, sha256_hex(bytes));
        assert_eq!(
            std::fs::read(store.root().join(&first.path)).unwrap(),
            bytes
        );
    }

    /// The other half: same id, *different* bytes is a genuine collision.
    /// Refuse it by name rather than silently destroying the first artifact,
    /// and leave the original file intact.
    #[test]
    fn saving_the_same_id_with_different_bytes_refuses_and_keeps_the_original() {
        let (_dir, store) = store();
        let first = store
            .save_with_id("med_abc", b"original", MediaKind::Video, "mp4", "clip")
            .expect("first save");

        let err = store
            .save_with_id("med_abc", b"different", MediaKind::Video, "mp4", "clip")
            .expect_err("a colliding id must not silently overwrite");
        match err {
            MediaError::Artifact(message) => {
                assert!(
                    message.contains("refusing to overwrite"),
                    "the refusal must say what it refused: {message}"
                );
            }
            other => panic!("expected a named Artifact error, got {other:?}"),
        }

        assert_eq!(
            std::fs::read(store.root().join(&first.path)).unwrap(),
            b"original",
            "the first artifact must survive a rejected collision"
        );
        assert_eq!(store.entries().unwrap().len(), 1);
    }

    /// A same-path row is replaced rather than appended, so the manifest can
    /// never carry two contradictory digests for one file.
    #[test]
    fn a_same_path_row_is_replaced_not_duplicated() {
        let (_dir, store) = store();
        let entry = |label: &str| ManifestEntry {
            id: "med_x".into(),
            kind: MediaKind::Image,
            path: "med_x.png".into(),
            sha256: "deadbeef".into(),
            label: label.into(),
            created_at: 0,
            byte_size: 4,
        };
        store.upsert_manifest(&entry("first")).unwrap();
        store.upsert_manifest(&entry("second")).unwrap();

        let entries = store.entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "second");
    }
}
