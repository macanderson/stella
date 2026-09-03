//! The last look before a write.
//!
//! `edit_file` reads a file. It works out the whole new file from that copy.
//! Then it writes it back. Anything written in the gap is wiped out, and the
//! call still says "replaced 1 occurrence(s)".
//!
//! Other writers are normal here. The agent's own `bash` runs beside its file
//! tools. A code formatter or a git hook needs no agent at all.
//!
//! So a tool asks here first. It re-reads the file through the same
//! descriptor. If disk holds other bytes, it refuses and writes nothing.
//!
//! This is not a compare-and-swap. No portable one exists. A write can still
//! land between the check and the rename. The check shrinks the gap to those
//! few microseconds.
//!
//! One extra read per call is the cost. Not wiping out someone else's work is
//! worth it.
//!
//! A file that cannot be re-read counts as changed. It was readable a moment
//! ago through this same descriptor. A tool that cannot see the bytes has no
//! ground to replace them. A brief failure costs a retry, which is the safe
//! way to fail.

use std::sync::Arc;

use crate::rootfd::RootHandle;

/// A test's window between the read and the write.
///
/// The race is real. It is also rare: the two syscalls are microseconds apart.
/// A closure the tool calls in that window makes it happen every run. Then a
/// witness can assert what happens instead of hoping to catch it.
#[cfg(test)]
pub(crate) type Seam = Arc<dyn Fn() + Send + Sync>;

/// What the last look found in place of the bytes the tool read.
pub(crate) enum Drift {
    /// Disk holds other bytes. `fresh` is the text there now, or `None` when
    /// those bytes are not UTF-8 and cannot be echoed back.
    Rewritten { fresh: Option<String> },
    /// The file could not be re-read at all. It was deleted, replaced by a
    /// directory, or is not readable. The message is the raw error.
    Unreadable(String),
}

impl Drift {
    /// The clause a refusal puts after the path. It names what happened.
    pub(crate) fn because(&self) -> String {
        match self {
            Self::Rewritten { .. } => "the file CHANGED (out-of-band modification)".to_string(),
            Self::Unreadable(error) => format!("the file could not be re-read ({error})"),
        }
    }

    /// The text on disk now, when there is any to hand back.
    pub(crate) fn fresh(&self) -> Option<&str> {
        match self {
            Self::Rewritten { fresh } => fresh.as_deref(),
            Self::Unreadable(_) => None,
        }
    }
}

/// Confirm `path` still hashes to `expected`, reading through `handle`.
///
/// `expected` is a hex sha256 of the bytes the caller read, in the form
/// [`crate::staleness::hex_sha256`] makes. [`crate::read::ReadLedger`] stores
/// the same form. So a caller may pass its own copy's hash, or the ledger's.
///
/// `handle` is the descriptor the write will walk. The bytes checked here and
/// the bytes about to go are one file. A path resolved twice can name two.
pub(crate) async fn confirm(
    handle: &Arc<RootHandle>,
    path: &str,
    expected: &str,
) -> Result<(), Drift> {
    let (handle, rel) = (Arc::clone(handle), path.to_string());
    let bytes = match tokio::task::spawn_blocking(move || handle.read(&rel)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => return Err(Drift::Unreadable(error.to_string())),
        Err(error) => return Err(Drift::Unreadable(format!("read task failed: {error}"))),
    };
    if crate::staleness::hex_sha256(&bytes) == expected {
        return Ok(());
    }
    Err(Drift::Rewritten {
        fresh: String::from_utf8(bytes).ok(),
    })
}

#[cfg(test)]
mod tests;
