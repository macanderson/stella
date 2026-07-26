// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `stella-serve` binary — run the engine service from environment config.
//!
//! ```text
//! STELLA_SERVE_BIND        address to bind (default 127.0.0.1:8080; container: 0.0.0.0:8080)
//! STELLA_SERVE_TOKEN_FILE  path to a file holding the bearer token (wins over
//!                          STELLA_SERVE_TOKEN when both are set)
//! STELLA_SERVE_TOKEN       bearer token every request must present
//! STELLA_SERVE_TOOLS       must be `remote` (the default) — all tool execution is
//!                          remoted to the host; a local tool surface is never served
//! ```
//!
//! One of `STELLA_SERVE_TOKEN_FILE` / `STELLA_SERVE_TOKEN` is required. The
//! file form exists because an environment variable is the leakiest place to
//! put a shared secret: it is visible in `/proc/<pid>/environ` to anything
//! running as the same user, inherited by every child process, and routinely
//! echoed by container inspection and orchestrator dashboards. A file can be
//! mounted as a secret and read once. **The file wins** when both are set —
//! the more deliberate, less leaky supply is never silently overridden by a
//! stray variable left in an image.
//!
//! `stella-serve healthcheck` probes `/healthz` on the bind port and exits 0/1,
//! so a container HEALTHCHECK needs no extra tooling in the runtime image.

use std::process::ExitCode;
use std::time::Duration;

use stella_serve::{ServeConfig, serve};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const DEFAULT_BIND: &str = "127.0.0.1:8080";

/// Deadline covering the healthcheck's whole connect + write + read. Kept
/// strictly under the container `HEALTHCHECK --timeout=5s`
/// (`packaging/docker/Dockerfile.serve`) so a wedged peer is reported by us,
/// as a clean non-zero exit, instead of being killed mid-probe by Docker.
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Cap on bytes read back. Only the status line is inspected, so anything
/// beyond a kilobyte is noise — and without a cap, a foreign listener on the
/// port could make the probe buffer without limit.
const HEALTH_PROBE_MAX_BYTES: u64 = 1024;

/// Bearer tokens shorter than this are **warned about, not rejected**.
///
/// 32 characters is roughly 128 bits at base64-ish density — the point below
/// which "guess the token" stops being hypothetical for a service that may be
/// bound off-loopback. It is a warning rather than a startup failure on
/// purpose: a hard minimum would turn an upgrade into an outage for every
/// existing deployment carrying a shorter token, and a service that refuses to
/// start is not more secure than one that starts and says so. That trade is
/// the reason this item sat deferred; the warning is the half that can land
/// without breaking anyone.
const MIN_TOKEN_LEN: usize = 32;

#[tokio::main]
async fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        None => run().await,
        Some("healthcheck") => healthcheck().await,
        Some(other) => {
            eprintln!("stella-serve: unknown argument `{other}` (expected none, or `healthcheck`)");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> ExitCode {
    let bind = std::env::var("STELLA_SERVE_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_string());
    let addr = match bind.parse() {
        Ok(addr) => addr,
        Err(err) => {
            eprintln!("stella-serve: invalid STELLA_SERVE_BIND `{bind}`: {err}");
            return ExitCode::FAILURE;
        }
    };
    let token = match resolve_token() {
        Ok(token) => token,
        Err(err) => {
            eprintln!("stella-serve: {err}");
            return ExitCode::FAILURE;
        }
    };
    if token.chars().count() < MIN_TOKEN_LEN {
        eprintln!(
            "stella-serve: warning: the configured bearer token is shorter than \
             {MIN_TOKEN_LEN} characters — it is the only authentication this service has. \
             Generate one with `openssl rand -base64 32`. Starting anyway."
        );
    }
    // Server mode is remote-only: the engine holds no local tool surface, so any
    // other value is a misconfiguration we refuse rather than silently ignore.
    if let Ok(tools) = std::env::var("STELLA_SERVE_TOOLS")
        && tools != "remote"
    {
        eprintln!(
            "stella-serve: STELLA_SERVE_TOOLS=`{tools}` is unsupported; only `remote` is served — every tool and model call is remoted to the host"
        );
        return ExitCode::FAILURE;
    }

    let config = ServeConfig { bind: addr, token };
    match serve(config, |bound| {
        println!("stella-serve listening on {bound}")
    })
    .await
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("stella-serve: server error: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Resolve the bearer token from `STELLA_SERVE_TOKEN_FILE` (preferred) or
/// `STELLA_SERVE_TOKEN`.
///
/// A token file's trailing newline is stripped — `echo secret > token` and
/// `printf secret > token` must configure the same service, and a token that
/// silently differs by one byte from what an operator sees in the file is a
/// support case nobody enjoys. Only the trailing `\n`/`\r\n` goes: interior
/// whitespace is left alone, because it could genuinely be part of a token and
/// guessing would be worse than honouring it.
///
/// An unreadable or empty token file is a hard error rather than a fall-back
/// to `STELLA_SERVE_TOKEN`. Silently serving under a *different* credential
/// than the operator mounted is precisely the failure that makes a secret
/// rotation look like it worked.
fn resolve_token() -> Result<String, String> {
    resolve_token_from(|name| std::env::var_os(name))
}

/// The resolution itself, over an injected lookup — pure with respect to the
/// process environment, so precedence and the newline trim are unit-testable
/// without `set_var` (which is `unsafe` under edition 2024 and races across
/// parallel test threads). Same shape as `stella-model`'s
/// `VertexAddressing::resolve_from`.
fn resolve_token_from(
    lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Result<String, String> {
    if let Some(path) = lookup("STELLA_SERVE_TOKEN_FILE")
        && !path.is_empty()
    {
        let path = std::path::PathBuf::from(path);
        let raw = std::fs::read_to_string(&path).map_err(|e| {
            format!(
                "cannot read STELLA_SERVE_TOKEN_FILE `{}`: {e}",
                path.display()
            )
        })?;
        let token = raw
            .strip_suffix('\n')
            .map(|t| t.strip_suffix('\r').unwrap_or(t))
            .unwrap_or(&raw);
        if token.is_empty() {
            return Err(format!(
                "STELLA_SERVE_TOKEN_FILE `{}` is empty — it must hold the bearer token",
                path.display()
            ));
        }
        return Ok(token.to_string());
    }
    match lookup("STELLA_SERVE_TOKEN").and_then(|v| v.into_string().ok()) {
        Some(token) if !token.is_empty() => Ok(token),
        _ => Err(
            "STELLA_SERVE_TOKEN_FILE or STELLA_SERVE_TOKEN is required — the bearer token \
             every request must present"
                .to_string(),
        ),
    }
}

async fn healthcheck() -> ExitCode {
    let bind = std::env::var("STELLA_SERVE_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_string());
    let port = bind
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(8080);
    match probe_health(port).await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => {
            eprintln!("stella-serve: healthcheck: /healthz did not return 200");
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("stella-serve: healthcheck: could not reach the server: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Probe `/healthz` over loopback and report whether it answered 200.
///
/// Only the port is taken from `STELLA_SERVE_BIND`; the host is always
/// loopback. That is deliberate — the probe runs *inside* the container, where
/// the documented bind is `0.0.0.0:8080` and loopback is the cheapest way in.
/// The corollary is that a deployment binding a single non-loopback address
/// (or IPv6-only `[::1]`) will see this report failure even with a healthy
/// server; such a deployment must supply its own healthcheck.
async fn probe_health(port: u16) -> std::io::Result<bool> {
    let response = tokio::time::timeout(HEALTH_PROBE_TIMEOUT, async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
        stream
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        let mut buf = Vec::with_capacity(HEALTH_PROBE_MAX_BYTES as usize);
        stream
            .take(HEALTH_PROBE_MAX_BYTES)
            .read_to_end(&mut buf)
            .await?;
        Ok::<_, std::io::Error>(buf)
    })
    .await
    .map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("no /healthz response within {HEALTH_PROBE_TIMEOUT:?}"),
        )
    })??;
    // Decoded leniently: a foreign listener's bytes are not ours to trust, and
    // a capped read can land mid-character. Either way the status line simply
    // fails to match, which is a clean `false` rather than an error.
    let response = String::from_utf8_lossy(&response);
    // Match the status line exactly rather than searching the whole line for
    // "200": a container orchestrator restarts the process on our verdict, so
    // it must not be swayed by a `200` appearing anywhere else in the head.
    Ok(response
        .lines()
        .next()
        .is_some_and(|status| status.starts_with("HTTP/1.1 200")))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::ffi::OsString;

    fn lookup_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let pairs: Vec<(String, OsString)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), OsString::from(*v)))
            .collect();
        move |name: &str| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    fn temp_token_file(name: &str, contents: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("stella-serve-token-{name}-{}", std::process::id()));
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn neither_variable_set_is_a_named_error_not_an_unauthenticated_server() {
        let err = resolve_token_from(lookup_from(&[])).unwrap_err();
        assert!(err.contains("STELLA_SERVE_TOKEN_FILE"), "{err}");
        assert!(err.contains("STELLA_SERVE_TOKEN"), "{err}");
    }

    #[test]
    fn the_inline_variable_still_works_on_its_own() {
        let token = resolve_token_from(lookup_from(&[("STELLA_SERVE_TOKEN", "inline-token")]))
            .expect("the pre-existing supply must keep working");
        assert_eq!(token, "inline-token");
    }

    /// The documented precedence, pinned: a mounted secret file is the more
    /// deliberate supply and must not be shadowed by a stray variable left in
    /// an image.
    #[test]
    fn the_token_file_wins_over_the_inline_variable() {
        let path = temp_token_file("precedence", "from-the-file\n");
        let token = resolve_token_from(lookup_from(&[
            ("STELLA_SERVE_TOKEN", "from-the-env"),
            ("STELLA_SERVE_TOKEN_FILE", path.to_str().unwrap()),
        ]))
        .unwrap();
        assert_eq!(
            token, "from-the-file",
            "one trailing newline is stripped so `echo secret > token` works"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn only_one_trailing_newline_is_stripped_from_a_token_file() {
        let crlf = temp_token_file("crlf", "tok\r\n");
        assert_eq!(
            resolve_token_from(lookup_from(&[(
                "STELLA_SERVE_TOKEN_FILE",
                crlf.to_str().unwrap()
            )]))
            .unwrap(),
            "tok"
        );
        let _ = std::fs::remove_file(&crlf);

        // A token that genuinely ends in whitespace is honoured, not guessed at.
        let spaced = temp_token_file("spaced", "tok \n");
        assert_eq!(
            resolve_token_from(lookup_from(&[(
                "STELLA_SERVE_TOKEN_FILE",
                spaced.to_str().unwrap()
            )]))
            .unwrap(),
            "tok "
        );
        let _ = std::fs::remove_file(&spaced);
    }

    /// An unreadable or empty token file must NOT fall back to the inline
    /// variable: serving under a different credential than the operator
    /// mounted is how a rotation looks like it worked when it did not.
    #[test]
    fn an_unusable_token_file_fails_instead_of_falling_back() {
        let missing =
            std::env::temp_dir().join(format!("stella-serve-token-absent-{}", std::process::id()));
        let _ = std::fs::remove_file(&missing);
        let err = resolve_token_from(lookup_from(&[
            ("STELLA_SERVE_TOKEN", "from-the-env"),
            ("STELLA_SERVE_TOKEN_FILE", missing.to_str().unwrap()),
        ]))
        .unwrap_err();
        assert!(err.contains("cannot read STELLA_SERVE_TOKEN_FILE"), "{err}");

        let empty = temp_token_file("empty", "");
        let err = resolve_token_from(lookup_from(&[
            ("STELLA_SERVE_TOKEN", "from-the-env"),
            ("STELLA_SERVE_TOKEN_FILE", empty.to_str().unwrap()),
        ]))
        .unwrap_err();
        assert!(err.contains("is empty"), "{err}");
        let _ = std::fs::remove_file(&empty);
    }

    /// A short token is a warning, never a refusal — a startup hard-fail would
    /// turn an upgrade into an outage for every existing deployment, which is
    /// exactly why the minimum-length item stayed deferred.
    #[test]
    fn a_short_token_resolves_successfully_and_is_only_warned_about() {
        let token = resolve_token_from(lookup_from(&[("STELLA_SERVE_TOKEN", "short")])).unwrap();
        assert_eq!(token, "short");
        assert!(
            token.chars().count() < MIN_TOKEN_LEN,
            "fixture must be short"
        );
    }
}
