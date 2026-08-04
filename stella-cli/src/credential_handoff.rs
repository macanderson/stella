//! One-shot credential handoff from a trusted launcher.
//!
//! A benchmark runner must not put a provider key in Stella's environment:
//! model-authored code can otherwise recover it from `env` or from
//! `/proc/$PPID/environ`, even when ordinary child commands remove the
//! variable. The Harbor adapter sends the selected provider's secrets over an
//! inherited anonymous pipe and sets only the non-secret descriptor number and
//! target environment-variable names. Startup consumes and closes that FD
//! before loading project env files, resolving configuration, or creating any
//! runtime/model-controlled subprocess.
//!
//! ## Why the handoff carries a set, not a single value
//!
//! Every provider but one authenticates with exactly one secret, and the seam
//! was built for exactly one: one name, one value, one line. Bedrock is the
//! exception — SigV4 needs an access key id *and* a secret access key, plus a
//! session token for temporary credentials — so a one-secret seam could not
//! express it at all, and the launcher refused Bedrock rather than guess
//! (#1301).
//!
//! The wire format generalizes without changing what a single-secret caller
//! sends: [`HANDOFF_TARGET_ENV`] is a comma-separated list of target names,
//! and the descriptor holds one value per name, newline-separated, in the same
//! order. One name and one line is the pre-existing format byte-for-byte, so
//! every existing launcher keeps working untouched.
//!
//! ## What deliberately does *not* travel here
//!
//! Bedrock's region (`AWS_REGION`) is routing, not a credential: it selects an
//! endpoint host and is disclosed in a benchmark manifest like any other route
//! decision. Sending it down the secret seam would put a public value behind
//! machinery built to keep values out of `/proc`, and would imply a secrecy
//! this value does not have. It arrives as an ordinary environment variable
//! and is picked up by the normal chain in `config::provider_aux`.
//!
//! Nothing that sources credentials *ambiently* is supported either — an AWS
//! profile file, SSO cache, IMDS/container-role endpoint, or web-identity
//! token file. Each of those would have the sealed process reach back out to
//! host state the launcher spent its whole design keeping out, which is the
//! opposite of what a one-descriptor handoff is for. Explicit credentials
//! only; see `bench/harbor_adapter/README.md` for the same list stated from
//! the launcher's side.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::OnceLock;

use stella_model::credential::ApiKey;

use crate::startup::StartupPhase;

/// Descriptor number holding the credential bytes. The Harbor adapter uses
/// stdin (fd 0), which Docker Compose connects to an anonymous host pipe.
pub(crate) const HANDOFF_FD_ENV: &str = "STELLA_CREDENTIAL_HANDOFF_FD";
/// Provider credential variables these values stand in for, comma-separated
/// and in the same order as the descriptor's lines. The *names* are not a
/// secret and let normal provider routing retain its existing precedence.
const HANDOFF_TARGET_ENV: &str = "STELLA_CREDENTIAL_HANDOFF_TARGET";

const MAX_CREDENTIAL_BYTES: u64 = 64 * 1024;

/// How many values one handoff may carry. Bedrock's three (access key id,
/// secret access key, session token) is the largest real set; the cap exists
/// so a malformed target list cannot turn a stray multi-line payload into an
/// unbounded parse.
const MAX_HANDOFF_VALUES: usize = 8;

/// Only credential slots Stella's built-in providers already recognize may be
/// populated through the launcher seam. This prevents a caller from turning
/// the mechanism into an arbitrary inherited-FD-to-environment bridge.
const ALLOWED_TARGETS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "XAI_API_KEY",
    "DEEPSEEK_API_KEY",
    "ZAI_API_KEY",
    "OPENROUTER_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "VERTEX_ACCESS_TOKEN",
    "LOCAL_API_KEY",
    // Bedrock's explicit-credential set. `AWS_REGION` is deliberately absent —
    // it is routing, not a secret; see the module docs.
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
];

static HANDOFF: OnceLock<BTreeMap<String, String>> = OnceLock::new();

/// Consume a configured handoff at single-threaded process startup.
///
/// The descriptor is owned by this function and is closed on every read path.
/// No raw credential is copied into the process environment. Errors contain
/// descriptor/target metadata only, never credential bytes.
///
/// Takes the [`StartupPhase`] token because it scrubs the handoff metadata out
/// of the process environment, which is only sound before the process grows a
/// second thread (#1140).
pub(crate) fn consume_at_startup(phase: &StartupPhase) -> Result<(), String> {
    phase.assert_before_runtime("credential_handoff::consume_at_startup");
    let Some(fd_raw) = std::env::var_os(HANDOFF_FD_ENV) else {
        return Ok(());
    };
    let target = std::env::var(HANDOFF_TARGET_ENV)
        .map_err(|_| format!("{HANDOFF_TARGET_ENV} is required with {HANDOFF_FD_ENV}"))?;
    let targets = validate_targets(&target)?;
    let fd: i32 = fd_raw
        .to_string_lossy()
        .parse()
        .map_err(|_| format!("{HANDOFF_FD_ENV} must be a non-negative descriptor"))?;
    if fd < 0 {
        return Err(format!(
            "{HANDOFF_FD_ENV} must be a non-negative descriptor"
        ));
    }

    // Disable same-UID memory inspection before the first credential byte is
    // copied into this process. The descriptor is already validated and is
    // owned/closed by `read_and_close_fd`, including if hardening fails.
    let payload = read_and_close_fd(fd)?;
    let credentials = pair_targets_with_values(&targets, &payload)?;

    // SAFETY: main calls this before env-file loading, clap, the Tokio runtime,
    // or any thread creation — and the `StartupPhase` parameter plus the
    // assertion at the top of this function are what hold that true rather
    // than this comment. Remove the non-secret control metadata as soon as it
    // has served its purpose so children cannot mistake it for a second usable
    // handoff.
    unsafe {
        std::env::remove_var(HANDOFF_FD_ENV);
        std::env::remove_var(HANDOFF_TARGET_ENV);
    }

    HANDOFF
        .set(credentials)
        .map_err(|_| "credential handoff was initialized more than once".to_string())
}

/// Split the descriptor payload into one value per declared target.
///
/// Strict on purpose, and every rule here is fail-closed: a count mismatch, an
/// empty value, or a duplicate name aborts rather than resolving a partial
/// set. A Bedrock handoff missing its secret access key would otherwise
/// construct an adapter that signs nothing and 403s on its first call, with a
/// provider error standing in for a launcher bug.
fn pair_targets_with_values(
    targets: &[String],
    payload: &str,
) -> Result<BTreeMap<String, String>, String> {
    let values: Vec<&str> = payload.split('\n').collect();
    if values.len() != targets.len() {
        return Err(format!(
            "credential handoff declared {} target(s) but the descriptor carried {} value(s)",
            targets.len(),
            values.len()
        ));
    }
    let mut credentials = BTreeMap::new();
    for (target, value) in targets.iter().zip(values) {
        if value.is_empty() {
            return Err(format!("credential handoff for {target} was empty"));
        }
        if credentials
            .insert(target.clone(), value.to_string())
            .is_some()
        {
            return Err(format!(
                "credential handoff named target `{target}` more than once"
            ));
        }
    }
    Ok(credentials)
}

/// Once a raw provider credential lives in process memory, same-UID
/// repository code must not be able to ptrace Stella or read
/// `/proc/$PPID/mem`. `PR_SET_DUMPABLE=0` enforces both on Linux and also
/// disables core dumps containing the key. It does not affect ordinary
/// signals, stdout, or mounted-log writes used by Harbor.
#[cfg(target_os = "linux")]
fn harden_process_memory() -> Result<(), String> {
    // SAFETY: `prctl` is called at single-threaded startup with the documented
    // PR_SET_DUMPABLE integer arguments and no pointer parameters.
    let result = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(format!(
            "could not disable process dumpability for credential handoff: {}",
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn harden_process_memory() -> Result<(), String> {
    Ok(())
}

/// Resolve an in-memory handoff for one provider credential slot.
pub(crate) fn key_for(target: &str) -> Option<ApiKey> {
    HANDOFF.get()?.get(target).map(ApiKey::new)
}

/// Whether startup consumed a trusted credential handoff.
///
/// Callers use this value-only signal to disable all fallback credential-file
/// discovery. A launcher handoff is authoritative for the provider it names;
/// selecting any other provider must fail rather than silently consulting
/// task-image state.
pub(crate) fn is_present() -> bool {
    HANDOFF.get().is_some()
}

/// Parse and validate the comma-separated target list.
///
/// A single name is the whole pre-existing format and parses to a one-element
/// list, so nothing about an existing single-secret launcher changes.
fn validate_targets(raw: &str) -> Result<Vec<String>, String> {
    let targets: Vec<String> = raw.split(',').map(|name| name.trim().to_string()).collect();
    if targets.is_empty() || targets.iter().any(String::is_empty) {
        return Err(format!(
            "{HANDOFF_TARGET_ENV} must be a comma-separated list of provider credential names"
        ));
    }
    if targets.len() > MAX_HANDOFF_VALUES {
        return Err(format!(
            "{HANDOFF_TARGET_ENV} names more than {MAX_HANDOFF_VALUES} credentials"
        ));
    }
    for target in &targets {
        if !ALLOWED_TARGETS.contains(&target.as_str()) {
            return Err(format!(
                "credential handoff target `{target}` is not a built-in provider credential"
            ));
        }
    }
    Ok(targets)
}

#[cfg(unix)]
fn read_and_close_fd(fd: i32) -> Result<String, String> {
    read_and_close_fd_with_hardener(fd, harden_process_memory)
}

#[cfg(unix)]
fn read_and_close_fd_with_hardener(
    fd: i32,
    harden: impl FnOnce() -> Result<(), String>,
) -> Result<String, String> {
    use std::os::fd::FromRawFd;

    // SAFETY: the launcher explicitly transfers ownership of this descriptor
    // to Stella. Take ownership before hardening so `File` closes it when this
    // function returns, including the fail-closed hardening error path. Do not
    // read or allocate a credential buffer until memory inspection is off.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    harden()?;
    let mut bytes = Vec::new();
    file.take(MAX_CREDENTIAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("could not read credential handoff descriptor: {e}"))?;
    if bytes.len() as u64 > MAX_CREDENTIAL_BYTES {
        return Err("credential handoff exceeded the 64 KiB safety limit".to_string());
    }
    // The adapter writes newline-delimited values to stdin, one per declared
    // target. Strip exactly the final framing byte (and an optional CR), never
    // arbitrary whitespace that could hide a malformed credential — the
    // interior newlines are the record separator and `pair_targets_with_values`
    // owns them.
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    String::from_utf8(bytes).map_err(|_| "credential handoff was not valid UTF-8".to_string())
}

#[cfg(not(unix))]
fn read_and_close_fd(_fd: i32) -> Result<String, String> {
    Err("credential FD handoff is supported only on Unix".to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{Seek, Write};
    use std::os::fd::{FromRawFd, IntoRawFd};

    #[test]
    fn inherited_pipe_is_consumed_framed_and_closed() {
        let mut fds = [0_i32; 2];
        // SAFETY: valid two-element output buffer.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let mut writer = unsafe { std::fs::File::from_raw_fd(fds[1]) };
        writer.write_all(b"openrouter-test-secret\n").unwrap();
        drop(writer);

        let read_fd = fds[0];
        let value = read_and_close_fd(read_fd).unwrap();
        assert_eq!(value, "openrouter-test-secret");
        // Ownership of the read end is consumed by the reader. Its closure on
        // the fail path is proven deterministically in
        // `memory_hardening_precedes_read_and_closes_fd_on_failure`; both paths
        // close through the same `file` drop at function exit. Re-checking the
        // raw fd number here would be racy under the parallel harness — a
        // sibling test can reuse the freed number immediately.
    }

    #[test]
    fn target_allowlist_is_provider_specific() {
        assert!(validate_targets("OPENROUTER_API_KEY").is_ok());
        assert!(validate_targets("GITHUB_TOKEN").is_err());
        assert!(validate_targets("LD_PRELOAD").is_err());
        // Bedrock's explicit-credential set is in; its region is not — that
        // one is routing and travels as an ordinary variable.
        assert!(validate_targets("AWS_ACCESS_KEY_ID").is_ok());
        assert!(validate_targets("AWS_SECRET_ACCESS_KEY").is_ok());
        assert!(validate_targets("AWS_SESSION_TOKEN").is_ok());
        assert!(validate_targets("AWS_REGION").is_err());
        // Nor may an ambient AWS source be requested through this seam.
        assert!(validate_targets("AWS_PROFILE").is_err());
        assert!(validate_targets("AWS_WEB_IDENTITY_TOKEN_FILE").is_err());
    }

    #[test]
    fn one_target_is_still_exactly_the_single_secret_format() {
        let targets = validate_targets("OPENROUTER_API_KEY").unwrap();
        let paired = pair_targets_with_values(&targets, "openrouter-test-secret").unwrap();
        assert_eq!(paired.len(), 1);
        assert_eq!(
            paired.get("OPENROUTER_API_KEY").map(String::as_str),
            Some("openrouter-test-secret")
        );
    }

    #[test]
    fn a_bedrock_set_pairs_each_value_with_the_target_declared_at_its_position() {
        let targets =
            validate_targets("AWS_ACCESS_KEY_ID,AWS_SECRET_ACCESS_KEY,AWS_SESSION_TOKEN").unwrap();
        let paired =
            pair_targets_with_values(&targets, "access-key-id\nsecret-key\nsession-token").unwrap();

        assert_eq!(
            paired.get("AWS_ACCESS_KEY_ID").map(String::as_str),
            Some("access-key-id")
        );
        assert_eq!(
            paired.get("AWS_SECRET_ACCESS_KEY").map(String::as_str),
            Some("secret-key")
        );
        assert_eq!(
            paired.get("AWS_SESSION_TOKEN").map(String::as_str),
            Some("session-token")
        );
    }

    #[test]
    fn a_short_payload_is_refused_rather_than_resolving_a_partial_set() {
        // The failure this prevents: Bedrock builds with an access key id and
        // no secret, signs nothing, and the launcher bug arrives as a provider
        // 403 three minutes into a trial.
        let targets = validate_targets("AWS_ACCESS_KEY_ID,AWS_SECRET_ACCESS_KEY").unwrap();
        let err = pair_targets_with_values(&targets, "access-key-id").unwrap_err();
        assert!(err.contains("2 target(s)"), "got: {err}");
        assert!(!err.contains("access-key-id"), "must not echo bytes: {err}");
    }

    #[test]
    fn an_empty_value_inside_a_set_is_refused_by_name() {
        let targets = validate_targets("AWS_ACCESS_KEY_ID,AWS_SECRET_ACCESS_KEY").unwrap();
        let err = pair_targets_with_values(&targets, "access-key-id\n").unwrap_err();
        assert!(err.contains("AWS_SECRET_ACCESS_KEY"), "got: {err}");
    }

    #[test]
    fn a_repeated_target_is_refused() {
        let targets = validate_targets("AWS_ACCESS_KEY_ID,AWS_ACCESS_KEY_ID").unwrap();
        let err = pair_targets_with_values(&targets, "one\ntwo").unwrap_err();
        assert!(err.contains("more than once"), "got: {err}");
    }

    #[test]
    fn oversized_handoff_fails_without_echoing_bytes() {
        let mut fds = [0_i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // Avoid blocking on pipe capacity: use a temporary anonymous-file FD
        // for the pure size-boundary unit. Production Harbor uses a pipe.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
        let file = tempfile::tempfile().unwrap();
        (&file)
            .write_all(&vec![b'x'; MAX_CREDENTIAL_BYTES as usize + 1])
            .unwrap();
        (&file).rewind().unwrap();
        let err = read_and_close_fd(file.into_raw_fd()).unwrap_err();
        assert!(err.contains("64 KiB"));
        assert!(!err.contains("xxx"));
    }

    #[test]
    fn memory_hardening_precedes_read_and_closes_fd_on_failure() {
        let mut fds = [0_i32; 2];
        // SAFETY: valid two-element output buffer.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let (read_fd, write_fd) = (fds[0], fds[1]);

        // Invalid UTF-8 waits unread in the pipe. If a credential byte were
        // read before hardening, the error below would be the UTF-8 failure.
        // SAFETY: two-byte source buffer, matching count, valid fd.
        assert_eq!(
            unsafe { libc::write(write_fd, [0xff_u8, b'\n'].as_ptr().cast(), 2) },
            2
        );

        let err = read_and_close_fd_with_hardener(read_fd, || {
            Err("synthetic hardening failure".to_string())
        })
        .unwrap_err();

        // The hardening error wins over the invalid UTF-8 waiting in the FD,
        // proving no credential bytes were read first.
        assert_eq!(err, "synthetic hardening failure");

        // The fail-closed path still consumed ownership of the read end. Probe
        // the open-file description, not the freed fd *number*: with the last
        // reader gone, writing to the retained write end fails with EPIPE.
        // (A raw `fcntl(read_fd, F_GETFD) == -1` check would be racy under the
        // parallel test harness — a sibling test can reuse the number the
        // instant it is freed, and `fcntl` would then report that fd's flags.)
        // Rust ignores SIGPIPE process-wide, so the write returns EPIPE rather
        // than raising a signal.
        // SAFETY: one-byte source buffer, matching count, still-owned write fd.
        assert_eq!(
            unsafe { libc::write(write_fd, [0_u8].as_ptr().cast(), 1) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPIPE)
        );

        // SAFETY: we still own the write end; close it to avoid leaking the fd.
        assert_eq!(unsafe { libc::close(write_fd) }, 0);
    }
}
