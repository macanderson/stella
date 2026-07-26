// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `stella-serve` binary — run the engine service from environment config.
//!
//! ```text
//! STELLA_SERVE_BIND   address to bind (default 127.0.0.1:8080; container: 0.0.0.0:8080)
//! STELLA_SERVE_TOKEN  bearer token every request must present (required)
//! STELLA_SERVE_TOOLS  must be `remote` (the default) — all tool execution is
//!                     remoted to the host; a local tool surface is never served
//! ```
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
    let token = match std::env::var("STELLA_SERVE_TOKEN") {
        Ok(token) if !token.is_empty() => token,
        _ => {
            eprintln!(
                "stella-serve: STELLA_SERVE_TOKEN is required — the bearer token every request must present"
            );
            return ExitCode::FAILURE;
        }
    };
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
