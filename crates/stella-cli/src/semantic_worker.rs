// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A child fills the search index. The session only reads its counts.
//! The child owns the pass and can outlive the deck. A failed pass leaves saved
//! vectors in place. Search can use those rows or read files. No prompt waits
//! for the child. The graph lease keeps two workers from doing the same work.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use stella_embed::{Embedder, EmbedderEnv, Resolution};
use stella_tools::search::backfill::{BackfillOutcome, backfill_workspace_vectors};
use stella_tools::search::engine::ChunkWarmOutcome;
use stella_tools::search::readiness::{IndexReadiness, measure};
use stella_tools::search::semantic::WarmOutcome;
use tokio::io::AsyncWriteExt;

use crate::agent::InitLine;

/// Send the settings and keys through stdin. Keys stay out of argv and env.
#[derive(Serialize, Deserialize)]
#[serde(remote = "EmbedderEnv")]
struct WorkerConfig {
    url: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
    dims: Option<String>,
    floor: Option<String>,
    voyage_api_key: Option<String>,
    openai_api_key: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Request {
    #[serde(with = "WorkerConfig")]
    embedder: EmbedderEnv,
}

const MAX_REQUEST_BYTES: u64 = 64 * 1024;

/// Runs before the CLI starts any other thread or runtime.
pub(crate) fn run() -> ExitCode {
    match run_inner() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner() -> Result<(), String> {
    crate::credential_handoff::harden_process_memory()?;
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read indexing configuration")?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return Err("indexing configuration exceeds 64 KiB".into());
    }
    let request: Request =
        serde_json::from_slice(&bytes).map_err(|_| "invalid indexing configuration")?;
    let embedder = match stella_embed::resolve(&request.embedder) {
        Resolution::Configured(embedder) => embedder,
        _ => return Err("no usable embedder configured for indexing".into()),
    };
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    match runtime.block_on(backfill_workspace_vectors(
        &root,
        embedder.as_ref(),
        &mut |_| {},
    )) {
        BackfillOutcome::Ran {
            files: WarmOutcome::Failed { reason, .. },
            ..
        }
        | BackfillOutcome::Ran {
            chunks: ChunkWarmOutcome::Failed { reason, .. },
            ..
        }
        | BackfillOutcome::Unavailable(reason) => Err(reason),
        BackfillOutcome::Ran { .. } | BackfillOutcome::Busy => Ok(()),
    }
}

/// Start a child and read its counts. If this task stops, the child keeps its
/// lease and runs to the end of the pass. A failed spawn leaves prompts free.
pub(crate) async fn monitor(
    root: &Path,
    status: &mut (dyn FnMut(InitLine) + Send),
    readiness: &mut (dyn FnMut(IndexReadiness) + Send),
) {
    let env = crate::credential_handoff::embedder_env();
    let Resolution::Configured(embedder) = stella_embed::resolve(&env) else {
        return;
    };
    let fingerprint = embedder.fingerprint().id();
    let result = observe(root, env, &fingerprint, status, readiness).await;
    let measured = coverage(root.to_path_buf(), fingerprint, true).await;
    readiness(measured);
    if result.is_ok() && measured.total_files > 0 {
        status(InitLine::Step(format!(
            "· semantic index: {} of {} files covered; prompts remain available",
            measured.indexed_files(),
            measured.total_files
        )));
    }
    if let Err(reason) = result {
        status(InitLine::Step(format!(
            "! semantic indexing stopped: {reason}. Search uses stored vectors and lexical fallback; prompts remain available."
        )));
    }
}

async fn observe(
    root: &Path,
    env: EmbedderEnv,
    fingerprint: &str,
    status: &mut (dyn FnMut(InitLine) + Send),
    readiness: &mut (dyn FnMut(IndexReadiness) + Send),
) -> Result<(), String> {
    let program = std::env::current_exe().map_err(|e| e.to_string())?;
    let child = spawn(&program, root, env).await?;
    // stdin is closed by spawn after the request. The only stderr is a final
    // failure; no live pipe connects the worker's progress to this process.
    let output = child.wait_with_output();
    tokio::pin!(output);
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            result = &mut output => {
                let output = result.map_err(|e| e.to_string())?;
                return if output.status.success() { Ok(()) } else {
                    let reason = String::from_utf8_lossy(&output.stderr);
                    Err(if reason.trim().is_empty() {
                        format!("worker exited with {}", output.status)
                    } else { reason.trim().to_owned() })
                };
            }
            _ = interval.tick() => {
                let measured = coverage(root.to_path_buf(), fingerprint.to_owned(), false).await;
                readiness(measured);
                if measured.total_files > 0 {
                    let label = if measured.is_degraded() { "degraded" } else { "available" };
                    status(InitLine::Progress(format!(
                        "· semantic search {label}: {} of {} files embedded; index worker running",
                        measured.indexed_files(), measured.total_files
                    )));
                }
            }
        }
    }
}

async fn spawn(
    program: &Path,
    root: &Path,
    env: EmbedderEnv,
) -> Result<tokio::process::Child, String> {
    let bytes = serde_json::to_vec(&Request { embedder: env })
        .map_err(|_| "cannot encode indexing configuration")?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return Err("indexing configuration exceeds 64 KiB".into());
    }
    let mut command = tokio::process::Command::new(program);
    command
        .arg("index-worker")
        .current_dir(root)
        .env("STELLA_NO_ENV_FILE", "1")
        .env_remove(crate::daemon::SUPERVISED_ENV)
        .env_remove(crate::credential_handoff::HANDOFF_FD_ENV)
        .env_remove("STELLA_CREDENTIAL_HANDOFF_TARGET")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(false);
    stella_tools::subprocess_env::scrub_sensitive_env(&mut command);
    for name in stella_embed::ENV_VARS {
        command.env_remove(name);
    }
    #[cfg(unix)]
    // SAFETY: setsid is async-signal-safe and uses no shared Rust state.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    #[cfg(windows)]
    command.creation_flags(0x0000_0200 | 0x0000_0008); // NEW_PROCESS_GROUP | DETACHED_PROCESS
    let mut child = command
        .spawn()
        .map_err(|e| format!("cannot start worker: {e}"))?;
    let result = match child.stdin.take() {
        Some(mut input) => input.write_all(&bytes).await,
        None => Err(std::io::Error::other("worker stdin unavailable")),
    };
    if let Err(error) = result {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return Err(format!("cannot send indexing configuration: {error}"));
    }
    Ok(child)
}

async fn coverage(root: PathBuf, fingerprint: String, settled: bool) -> IndexReadiness {
    tokio::task::spawn_blocking(move || {
        let Ok(path) = stella_store::workspace_private_sqlite_path(&root, "codegraph.db") else {
            return IndexReadiness::unknown();
        };
        let Ok(graph) = stella_graph::CodeGraph::open(&root, &path) else {
            return IndexReadiness::unknown();
        };
        let measured = measure(&graph, &fingerprint, settled);
        graph.shutdown();
        measured
    })
    .await
    .unwrap_or_else(|_| IndexReadiness::unknown())
}

#[cfg(test)]
mod tests;
