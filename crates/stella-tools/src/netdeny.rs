//! Spawn-time network denial for foundry-built tools.
//!
//! A tool the foundry authored for itself runs with the network **denied by
//! default** — that is the standing control that replaces the human in the
//! autonomous adopt path, alongside the per-call re-digest
//! ([`crate::foundry_gate::recheck_before_launch`]), the circuit breaker, and
//! versioned rollback. The denial is enforced by the operating system at
//! spawn, not by anything the script is asked to respect:
//!
//! - **macOS**: `sandbox-exec -p` with a profile that allows everything
//!   except `network*`. Deprecated in name for years, still shipped and
//!   still what the OS's own daemons use; if it disappears in some future
//!   release, [`available`] answers `false` and autonomy degrades rather
//!   than pretending.
//! - **Linux**: `unshare -r -n` — a new user namespace (so no privilege is
//!   needed) and a new, empty network namespace. The child sees only an
//!   interface-less loopback-down world; any connect fails with
//!   `ENETUNREACH`.
//!
//! # A control is claimed only where it holds
//!
//! [`available`] actually runs the wrapper once (against `true`) and caches
//! the answer. A platform where neither mechanism works — containers that
//! seccomp-block `unshare`, exotic OSes — reports `false`, and the autonomy
//! pipeline responds by degrading to draft-only (files written, nothing
//! adopted). The one thing this module never does is claim a control it
//! cannot enforce: there is no "best effort" mode, because a network denial
//! that only usually holds is a different, weaker fact than the one recorded
//! at adoption.
//!
//! A tool an operator has explicitly allowlisted (`foundry.network_allowlist`
//! in settings) spawns unwrapped — a control, not a ceremony: the grant is a
//! reviewable line in a settings file.

use std::sync::OnceLock;

/// The macOS Seatbelt profile: everything a normal child process may do,
/// except any form of network access.
#[cfg(target_os = "macos")]
const MACOS_PROFILE: &str = "(version 1)\n(allow default)\n(deny network*)";

/// Wrap `argv` so the spawned process runs with the network denied, or `None`
/// when this platform has no mechanism to offer. A `Some` answer is a claim
/// about the *shape* of the wrapper; whether it actually works on this
/// machine is [`available`]'s question.
pub fn wrap(argv: &[String]) -> Option<Vec<String>> {
    #[cfg(target_os = "macos")]
    {
        let mut wrapped = vec![
            "/usr/bin/sandbox-exec".to_string(),
            "-p".to_string(),
            MACOS_PROFILE.to_string(),
        ];
        wrapped.extend(argv.iter().cloned());
        Some(wrapped)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut wrapped = vec![
            "unshare".to_string(),
            "-r".to_string(),
            "-n".to_string(),
            "--".to_string(),
        ];
        wrapped.extend(argv.iter().cloned());
        Some(wrapped)
    }
    #[cfg(not(unix))]
    {
        let _ = argv;
        None
    }
}

/// Whether the wrapper actually works on this machine — probed once by
/// running it against `true` and cached for the life of the process.
///
/// `false` is a real answer, not a failure: the autonomy pipeline reads it
/// as "degrade to draft-only", and the spawn path reads it as "there is no
/// isolation to add". Neither fakes the control.
pub fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(probe)
}

/// One real run of the wrapper. Kept separate from [`available`] so a test
/// can call the probe directly without poisoning the cache.
fn probe() -> bool {
    let Some(argv) = wrap(&["true".to_string()]) else {
        return false;
    };
    std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wrapper preserves the tool's own argv verbatim at the tail — the
    /// bytes the gate digested are the bytes that run, merely inside a
    /// namespace/profile.
    #[test]
    fn wrapping_preserves_the_original_argv() {
        let argv = vec!["./tool.sh".to_string(), "arg one".to_string()];
        let Some(wrapped) = wrap(&argv) else {
            // A platform with no mechanism wraps nothing — also a contract.
            return;
        };
        assert!(wrapped.len() > argv.len());
        assert_eq!(&wrapped[wrapped.len() - 2..], argv.as_slice());
    }

    /// The probe is a real execution, and on the two supported Unixes it
    /// answers something rather than erroring. (Whether it answers `true`
    /// depends on the machine — a seccomp-restricted container legitimately
    /// says no — so the assertion is that the probe *resolves*, and the
    /// network-denial witness in stella-cli asserts the strong half wherever
    /// the mechanism is live.)
    #[test]
    fn the_probe_resolves() {
        let _ = probe();
    }

    /// Where the mechanism reports available, it must actually deny: a
    /// `/dev/tcp` connect that succeeds under the wrapper would mean the
    /// control is fake, which is the one unrecoverable outcome.
    #[test]
    fn an_available_wrapper_really_denies_the_network() {
        if !available() {
            return; // nothing is claimed on this machine, so nothing to hold.
        }
        // `bash -c` because /dev/tcp is a bash-ism; 1.1.1.1:53 answers fast
        // when reachable. Under the wrapper the connect must fail.
        let argv = vec![
            "bash".to_string(),
            "-c".to_string(),
            "exec 3<>/dev/tcp/1.1.1.1/53 && echo REACHED".to_string(),
        ];
        let wrapped = wrap(&argv).expect("available implies a wrapper");
        let output = std::process::Command::new(&wrapped[0])
            .args(&wrapped[1..])
            .output()
            .expect("the wrapper itself must spawn");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains("REACHED"),
            "the network-denial wrapper let a TCP connect through"
        );
    }
}
