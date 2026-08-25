//! The whistle socket itself: a `UnixListener` bound inside a session's
//! sidecar directory, bridging `stella whistle`'s sender into this
//! session's [`super::tap::Whistleable`] tap.
//!
//! `#[cfg(unix)]` at the module level (`crate::whistle`) — `tokio::net`
//! offers no Windows counterpart, and Stella ships no Windows binary today
//! (AGENTS.md's Windows section). `crate::whistle::cmd` reports that gap
//! explicitly on a non-Unix build rather than this module silently doing
//! nothing.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::net::UnixListener;

use super::tap::Whistleable;
use super::wire::{WhistleAck, WhistleRequest, read_frame, write_frame};

/// A live session's whistle listener. Dropping this unbinds and removes the
/// socket file — best-effort throughout, the same posture
/// `crate::agent::presence::SessionPresence` takes with the registry: a
/// listener that fails to bind, or fails to clean up, never disturbs the
/// run it is attached to.
pub(crate) struct WhistleListener {
    socket_path: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl WhistleListener {
    /// Bind and start accepting on `session_id`'s whistle socket
    /// (`registry.whistle_socket_path`), pushing every delivered message
    /// into `tap`. `None` on any failure to prepare the sidecar or bind —
    /// a session that cannot be reached this way simply is not whistleable,
    /// which is exactly what `stella whistle` reports back to its caller.
    pub(crate) fn spawn(
        registry: &stella_store::SessionRegistry,
        session_id: &str,
        tap: Arc<dyn Whistleable>,
    ) -> Option<Self> {
        // Owner-only (0700), matching every other sidecar writer
        // (`SessionRegistry::prepare_sidecar`) — the directory permission is
        // what actually keeps another local user from even resolving the
        // socket path, never mind connecting to it.
        registry.prepare_sidecar(session_id).ok()?;
        let socket_path = registry.whistle_socket_path(session_id);
        Self::spawn_at(&socket_path, tap)
    }

    /// As [`Self::spawn`], but at an explicit path with its parent already
    /// prepared — the seam tests use, to avoid touching the real
    /// `~/.stella`.
    pub(crate) fn spawn_at(socket_path: &Path, tap: Arc<dyn Whistleable>) -> Option<Self> {
        // A prior process of this same session id can leave a stale socket
        // file behind — a crash never gets to unlink it, and `bind` fails
        // `AddrInUse` on a stale path exactly as it would on a live one.
        // Unlinking first is safe: ids are self-minted and never reused
        // (`ses-<ms>-<pid>`), so there is nothing live at this path to race.
        let _ = std::fs::remove_file(socket_path);
        let listener = UnixListener::bind(socket_path).ok()?;
        // Defense in depth beyond the sidecar directory's own 0700: the
        // socket file itself is owner-only too, so a misconfigured or
        // relocated sidecar cannot widen who can inject text into a
        // running session.
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600));
        }
        let task = tokio::spawn(accept_loop(listener, tap));
        Some(Self {
            socket_path: socket_path.to_path_buf(),
            task,
        })
    }
}

impl Drop for WhistleListener {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// One request per connection: accept, read the frame, push it, ack, close.
/// A connection that sends garbage or hangs up early is simply dropped —
/// whistle has one caller (`stella whistle`, the same binary), so there is
/// nobody to report a malformed frame to on this side of the socket.
async fn accept_loop(listener: UnixListener, tap: Arc<dyn Whistleable>) {
    loop {
        let Ok((mut stream, _addr)) = listener.accept().await else {
            return;
        };
        let tap = Arc::clone(&tap);
        tokio::spawn(async move {
            let Ok(request) = read_frame::<_, WhistleRequest>(&mut stream).await else {
                return;
            };
            tap.push(request.text);
            let _ = write_frame(&mut stream, &WhistleAck { delivered: true }).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixStream;

    struct RecordingTap(std::sync::Mutex<Vec<String>>);

    impl Whistleable for RecordingTap {
        fn push(&self, text: String) {
            self.0.lock().unwrap().push(text);
        }
    }

    /// A message sent over a real Unix socket reaches the tap, and the
    /// sender gets an ack — the witness for the whole cross-process path
    /// this module exists to build: no listener like this existed before,
    /// so this fails on `main` and passes with it.
    #[tokio::test]
    async fn a_message_sent_over_the_socket_reaches_the_tap_and_is_acked() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("whistle.sock");
        let tap = Arc::new(RecordingTap(std::sync::Mutex::new(Vec::new())));
        let _listener =
            WhistleListener::spawn_at(&socket_path, tap.clone() as Arc<dyn Whistleable>)
                .expect("bind must succeed against a fresh temp path");

        let mut stream = UnixStream::connect(&socket_path).await.unwrap();
        write_frame(
            &mut stream,
            &WhistleRequest {
                text: "stop the compile".to_string(),
            },
        )
        .await
        .unwrap();
        let ack: WhistleAck = read_frame(&mut stream).await.unwrap();
        assert!(ack.delivered);

        // The push crosses a spawned task, not the connection future this
        // test awaited — give the accept loop's task a moment.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(tap.0.lock().unwrap().as_slice(), ["stop the compile"]);
    }

    /// Dropping the listener must remove its socket file, or a crashed
    /// session's stale path would linger until something else unlinks it.
    #[tokio::test]
    async fn dropping_the_listener_removes_the_socket_file() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("whistle.sock");
        let tap: Arc<dyn Whistleable> = Arc::new(RecordingTap(std::sync::Mutex::new(Vec::new())));
        let listener = WhistleListener::spawn_at(&socket_path, tap).unwrap();
        assert!(socket_path.exists());
        drop(listener);
        assert!(!socket_path.exists());
    }
}
