// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Starting an installed driver plugin — the host half of the driver channel
//! (#3783).
//!
//! `stella-runtime` has been able to *hold* a driver session since #3634:
//! spawn the process, hand it a [`DriveRequest`], serve the capabilities its
//! grant permits, and read back what it wants to happen next. Nothing in the
//! shipping binary built one, so `[driver]` was a grant a human could read at
//! install and no code path could turn into a running program. This module is
//! that path, and [`crate::wrapper_plugin`] is the shape it copies: resolve
//! argv against the installed package directory, resolve the manifest's
//! environment allowlist through the *host's* ambient read, declare the
//! transport, and report every name the socket withheld.
//!
//! # The two halves, and why they are separate here too
//!
//! [`resolve`] finds the plugin and binds its process; [`ResolvedDriver::serving`]
//! attaches the capability gate. The wrapper socket splits them because the
//! gate needs a provider that does not exist yet at bind time. Here the reason
//! is smaller and still real: the gate is the value that must outlive the
//! session, because [`DriverCallGate::refusals`] is the only place a refused
//! ask is recorded, and a refusal nobody reads is the silence AGENTS.md's
//! rule #10 exists to refuse.
//!
//! # What a session does today
//!
//! Every capability answers `unsupported` ([`NoDriverCapabilities`]), so a
//! driver's first ask degrades rather than doing anything. That is the real
//! state until #3599's B1–B6 land, and this module does not paper over it: the
//! refusals are printed, in the driver's own vocabulary, so an operator sees
//! exactly which asks this build could not serve.
//!
//! Scheduling is absent. One invocation opens one session and
//! reports what the driver said to do next; who re-opens it after a
//! [`DriveNext::Sleep`] is #3599's B2 `LoopStep` machine, and inventing a
//! sleeper here would be inventing the loop this channel exists to be driven
//! by.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use stella_plugin::{DriveNext, DriveRequest, PluginManifest};
use stella_runtime::wrapper::{
    DEFAULT_DRIVER_MAX_CALLS, DriverCallGate, NoDriverCapabilities, SubprocessDriver,
};

use crate::plugin_cmd::roster::PluginRoster;

/// An installed driver, bound to the process the host will start.
///
/// `Debug` is safe to print: [`SubprocessDriver`]'s own is hand-written to
/// name the environment variables it carries and never their values.
#[derive(Debug)]
pub(crate) struct ResolvedDriver {
    /// The manifest that declared it — the grant the gate is built from.
    manifest: PluginManifest,
    /// The transport, before any gate is attached.
    driver: SubprocessDriver,
}

/// A driver with its capability gate attached, ready to open a session.
pub(crate) struct BoundDriver {
    driver: SubprocessDriver,
    gate: Arc<DriverCallGate>,
}

impl ResolvedDriver {
    /// Attach the capability gate the manifest's grant is checked against.
    ///
    /// The gate is kept beside the driver rather than moved into it, for
    /// [`crate::wrapper_plugin::BoundWrapper`]'s reason: nothing else can read
    /// [`DriverCallGate::refusals`] once the session has ended.
    pub(crate) fn serving(self) -> BoundDriver {
        let grant = self.manifest.driver.clone().unwrap_or_default();
        let gate = Arc::new(DriverCallGate::declare(
            grant,
            DEFAULT_DRIVER_MAX_CALLS,
            Box::new(NoDriverCapabilities),
        ));
        BoundDriver {
            driver: self.driver.serving(Arc::clone(&gate)),
            gate,
        }
    }

    /// The program this driver will be started as — what a caller reports
    /// before spending anything on it.
    pub(crate) fn program(&self) -> &str {
        self.driver.program()
    }
}

impl BoundDriver {
    /// Open one session and read back what the driver wants to happen next.
    ///
    /// # Errors
    ///
    /// A message a user can act on. `stella-cli` is a binary, so a `String`
    /// here is the finished product rather than an unnamed error (AGENTS.md
    /// rule 5) — and every `DriverError` already names the program.
    pub(crate) fn open(&self, session: &str) -> Result<DriveNext, String> {
        // A runtime per session, matching `self_driving_cmd::hooks`'s dispatch:
        // this door is otherwise synchronous, and a driver session is one
        // bounded subprocess rather than something that needs the
        // multi-threaded scheduler.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("could not start a runtime for the driver: {error}"))?;
        let response = runtime
            .block_on(self.driver.drive(DriveRequest::new(session)))
            .map_err(|error| error.to_string())?;
        Ok(response.next)
    }

    /// Whether this session will hold a channel open for capability asks at
    /// all — the gate's own answer, so a caller reports what the grant
    /// actually bought rather than what the manifest asked for.
    ///
    /// `false` means the driver declared no capability the host may perform,
    /// and the transport closes its stdin rather than waiting for asks that
    /// can never come (#3561's shape, in this dispatch context).
    pub(crate) fn offers_calls(&self) -> bool {
        self.gate.offers_calls()
    }

    /// Every ask this session could not serve, in the order they were made,
    /// in `RefusedDriverCall`'s own words rather than a second phrasing of
    /// them.
    pub(crate) fn refusals(&self) -> Vec<String> {
        self.gate
            .refusals()
            .iter()
            .map(ToString::to_string)
            .collect()
    }
}

/// Load the roster and bind the installed driver plugin called `name`.
///
/// # Errors
///
/// Whatever [`bind_installed`] refuses, plus a failure to read the roster.
pub(crate) fn resolve(
    workspace_root: &Path,
    name: &str,
    warn: &mut dyn FnMut(String),
) -> Result<ResolvedDriver, String> {
    let settings = crate::settings::Settings::load(workspace_root).unwrap_or_default();
    let (roster, notices) = PluginRoster::load(workspace_root, &settings);
    // A plugin that did not load must never vanish silently: "I installed it
    // and nothing happened" is unanswerable without the reason.
    for notice in notices {
        warn(notice.trim_start_matches(" ! ").to_string());
    }
    bind_installed(&roster, name, warn)
}

/// Find the installed plugin called `name` and bind it to its process.
///
/// The pure half — pure of everything but the one ambient environment read the
/// allowlist needs, which belongs to the host because `stella-runtime` reads
/// none by contract (`crates/stella-runtime/tests/no_ambient_reads.rs`).
///
/// # Errors
///
/// A message naming what to do instead: which plugins *do* declare a driver,
/// that this one declares no `[driver.process]` and so is a declaration a
/// person starts by hand, or why the process cannot be started.
///
/// A refused environment name is **reported, not silently dropped**, exactly as
/// it is for a wrapper: [`SubprocessDriver::declare`] withholds model
/// credentials at the socket for every child the host spawns (#3512), and an
/// author whose manifest asked for one can only stop asking if they are told.
pub(crate) fn bind_installed(
    roster: &PluginRoster,
    name: &str,
    warn: &mut dyn FnMut(String),
) -> Result<ResolvedDriver, String> {
    bind_with(roster, name, warn, &mut |name| std::env::var(name).ok())
}

/// [`bind_installed`], with the environment lookup supplied.
///
/// The seam exists for the reason [`stella_plugin::Runtime::child_env`] takes
/// a lookup rather than reading the environment itself: what the socket
/// withholds from a child is decided by the *names* a manifest declared, and a
/// test that had to set `ANTHROPIC_API_KEY` in the test process to prove the
/// withholding would be mutating global state every other test shares.
fn bind_with(
    roster: &PluginRoster,
    name: &str,
    warn: &mut dyn FnMut(String),
    lookup: &mut dyn FnMut(&str) -> Option<String>,
) -> Result<ResolvedDriver, String> {
    let installed = roster
        .plugins()
        .iter()
        .find(|plugin| plugin.manifest.name == name && plugin.manifest.driver.is_some())
        .ok_or_else(|| {
            let mut available: Vec<&str> = roster
                .plugins()
                .iter()
                .filter(|plugin| plugin.manifest.driver.is_some())
                .map(|plugin| plugin.manifest.name.as_str())
                .collect();
            available.sort_unstable();
            if available.is_empty() {
                format!(
                    "no installed plugin called \"{name}\" declares a [driver] block — \
                     `stella plugin list` shows what is installed"
                )
            } else {
                format!(
                    "no installed plugin called \"{name}\" declares a [driver] block — \
                     installed drivers: {}",
                    available.join(", ")
                )
            }
        })?;

    let grant = installed
        .manifest
        .driver
        .as_ref()
        .expect("the search filtered on a declared [driver] block");
    let process = grant.process.as_ref().ok_or_else(|| {
        format!(
            "plugin \"{name}\" declares a [driver] grant but no [driver.process] block, so \
             Stella has no program to start — this is a declaration of what such a driver \
             may ask for, and the loop it describes is one you start yourself"
        )
    })?;

    warn_narrowed_driver_ceiling(&installed.manifest, warn);

    // `${plugin_dir}` is the host's substitution — this crate is where the
    // install directory is known — through the same shared expander every
    // other argv in an installed package goes through (#4301).
    let argv: Vec<String> = process
        .argv
        .iter()
        .map(|arg| stella_plugin::expand_plugin_dir(arg, &installed.dir))
        .collect();
    // The one ambient read on this path, and it belongs to the host:
    // `stella-runtime` reads no process environment by contract, so the CLI
    // resolves the manifest's allowlist and hands over pairs.
    let env = process.child_env(lookup);
    let admitted = SubprocessDriver::declare(argv, env, Duration::from_secs(process.timeout_secs))
        .map_err(|error| format!("driver \"{name}\" cannot be started: {error}"))?;
    for refused in &admitted.refused {
        warn(format!(
            "driver \"{name}\" asked for {refused} and will not get it — a plugin never \
             receives a model credential; every capability it needs is performed by Stella \
             on request"
        ));
    }
    Ok(ResolvedDriver {
        manifest: installed.manifest.clone(),
        driver: admitted.driver,
    })
}

/// Say so when this host will fund fewer capability asks than the manifest
/// declared (#3841's posture, in the driver's dispatch context).
///
/// Not a refusal: a driver capped below its ask still drives, just
/// with fewer asks per session. The defect the notice exists for is the
/// silence — a user who read "asks for up to 200 of those per driver session"
/// at install and gets the host's ceiling has been told nothing.
fn warn_narrowed_driver_ceiling(manifest: &PluginManifest, warn: &mut dyn FnMut(String)) {
    let Some(asked) = manifest.driver.as_ref().and_then(|grant| grant.max_calls) else {
        return;
    };
    if asked > DEFAULT_DRIVER_MAX_CALLS {
        warn(format!(
            "driver \"{}\" asks for up to {asked} capability calls per session; this host \
             funds {DEFAULT_DRIVER_MAX_CALLS} (a host default, not a setting you chose)",
            manifest.name
        ));
    }
}

#[cfg(test)]
mod tests;
