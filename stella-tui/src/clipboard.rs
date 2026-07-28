//! Clipboard capture for `⌃V`: pull an image (or, failing that, text) off
//! the system clipboard.
//!
//! Terminals cannot deliver a *bitmap* through bracketed paste — `⌘V` of a
//! screenshot arrives as nothing at all. `⌃V` is the explicit affordance:
//! the shell asks the OS clipboard directly (via `arboard`), encodes any
//! image as PNG into the workspace's attachment store
//! (`.stella/attachments/`), and hands back an [`Attachment`] the composer
//! shows as a chip. When the clipboard holds text instead, the caller falls
//! back to a normal paste so `⌃V` is a universal paste key.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use stella_protocol::Attachment;

/// Where pasted payloads are stored, relative to the working directory the
/// session runs in.
pub fn default_attachments_dir() -> PathBuf {
    PathBuf::from(".stella").join("attachments")
}

/// What `⌃V` found on the clipboard.
#[derive(Debug, PartialEq, Eq)]
pub enum ClipboardPaste {
    /// An image, stored to disk and ready to attach.
    Image(Attachment),
    /// Plain text — the caller pastes it like any bracketed paste.
    Text(String),
    /// Nothing usable.
    Empty,
}

/// Read the system clipboard: an image wins over text. The image payload is
/// PNG-encoded and written under `dir` so the attachment (and the session
/// transcript replaying it every turn) has a stable file to hydrate from.
pub fn capture(dir: &Path) -> Result<ClipboardPaste, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?;
    match clipboard.get_image() {
        Ok(image) => store_image(dir, &image).map(ClipboardPaste::Image),
        Err(arboard::Error::ContentNotAvailable) => match clipboard.get_text() {
            Ok(text) if !text.is_empty() => Ok(ClipboardPaste::Text(text)),
            _ => Ok(ClipboardPaste::Empty),
        },
        Err(e) => Err(format!("clipboard image read failed: {e}")),
    }
}

/// How many pasted images the store keeps. `⌃V` is append-only and nothing
/// else ever deletes, so without a cap the workspace accretes every
/// screenshot ever pasted. 100 is far more than any live session replays,
/// so pruning only ever touches pastes from long-dead sessions.
const MAX_STORED_PASTES: usize = 100;

/// PNG-encode an RGBA clipboard bitmap and write it to the attachment store.
fn store_image(dir: &Path, image: &arboard::ImageData<'_>) -> Result<Attachment, String> {
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(
            &mut encoded,
            u32::try_from(image.width).map_err(|_| "image too wide".to_string())?,
            u32::try_from(image.height).map_err(|_| "image too tall".to_string())?,
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("png encode failed: {e}"))?;
        writer
            .write_image_data(&image.bytes)
            .map_err(|e| format!("png encode failed: {e}"))?;
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let name = format!("pasted-{millis}.png");
    let path = dir.join(&name);
    write_owner_only(&path, &encoded)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    prune_old_pastes(dir);
    Ok(Attachment::from_path(
        name,
        "image/png",
        encoded.len() as u64,
        path.to_string_lossy(),
    ))
}

/// Write `bytes` readable by the owner only. Pasted screenshots routinely
/// contain secrets (tokens in terminal output, dashboards), and
/// `.stella/attachments/` sits inside the workspace where any tool or other
/// local user could otherwise read them under a default umask.
fn write_owner_only(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(bytes)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}

/// Drop all but the newest [`MAX_STORED_PASTES`] pasted images. Only files
/// this module minted (`pasted-<millis>.png`) are candidates — anything else
/// in the directory is left alone. The millis names are fixed-width for the
/// next few centuries, so name order is age order. Best-effort: a prune
/// failure never fails the paste that triggered it.
fn prune_old_pastes(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut pastes: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("pasted-") && n.ends_with(".png"))
        })
        .collect();
    if pastes.len() <= MAX_STORED_PASTES {
        return;
    }
    pastes.sort();
    let excess = pastes.len() - MAX_STORED_PASTES;
    for stale in &pastes[..excess] {
        let _ = std::fs::remove_file(stale);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `capture` needs a real OS clipboard; only the pure encode/store half is
    // unit-tested.
    #[test]
    fn store_image_writes_a_png_attachment() {
        let dir = tempfile::tempdir().unwrap();
        let image = arboard::ImageData {
            width: 2,
            height: 1,
            bytes: vec![255, 0, 0, 255, 0, 255, 0, 255].into(),
        };
        let att = store_image(dir.path(), &image).expect("stored");
        assert_eq!(att.media_type, "image/png");
        assert!(att.name.starts_with("pasted-") && att.name.ends_with(".png"));
        let stella_protocol::AttachmentSource::Path { path } = &att.source else {
            panic!("expected path source");
        };
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(&bytes[1..4], b"PNG");
        assert_eq!(att.byte_len, bytes.len() as u64);
    }

    #[cfg(unix)]
    #[test]
    fn stored_paste_is_owner_readable_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let image = arboard::ImageData {
            width: 1,
            height: 1,
            bytes: vec![0, 0, 0, 255].into(),
        };
        let att = store_image(dir.path(), &image).expect("stored");
        let stella_protocol::AttachmentSource::Path { path } = &att.source else {
            panic!("expected path source");
        };
        let mode = std::fs::metadata(path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "paste must not be world-readable");
    }

    #[test]
    fn prune_keeps_the_newest_pastes_and_leaves_foreign_files() {
        let dir = tempfile::tempdir().unwrap();
        for millis in 0..MAX_STORED_PASTES + 5 {
            let name = format!("pasted-{:013}.png", millis);
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        std::fs::write(dir.path().join("notes.txt"), b"keep me").unwrap();

        prune_old_pastes(dir.path());

        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let pastes: Vec<&String> = names.iter().filter(|n| n.starts_with("pasted-")).collect();
        assert_eq!(pastes.len(), MAX_STORED_PASTES);
        // The five oldest are gone; the newest survives.
        assert_eq!(*pastes[0], format!("pasted-{:013}.png", 5));
        assert!(names.contains(&"notes.txt".to_string()));
    }
}
