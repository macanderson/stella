//! The tool-foundry adoption ledger — which self-authored tools this
//! workspace has approved, and what proof each one carries.
//!
//! Its own module rather than more methods on the already-oversized `lib.rs`,
//! per the working rule the file-size gate enforces.
//!
//! # Why this is in the store and not in the manifest
//!
//! A self-authored tool is two files in the repository, and the repository is
//! writable by everything that can run a shell — including the agent whose
//! capabilities are being extended. A flag in the manifest saying "approved"
//! would be a permission the subject of the permission can grant itself. The
//! ledger lives in `.stella/private/store.db` instead, next to the receipts,
//! and it records the two facts a manifest cannot be trusted to assert:
//!
//! - **the proof**: the capability witness that flipped
//!   (`stella_tools::foundry_witness`), kept in re-runnable form — the input
//!   it ran with and the value it asserted — so the claim can be checked
//!   again later rather than taken on the word of a one-time run;
//! - **the approval**: [`AdoptedTool::enabled`], which only a human sets, and
//!   which re-adoption always clears.
//!
//! Persisting here is also what makes an adopted tool survive a restart as an
//! *adopted* tool. The manifest survives on its own — it is a file — but "this
//! was proven and approved" is not in the file, so without this table every
//! restart would either re-approve silently or re-prove from scratch.
//!
//! # The reuse metric
//!
//! Issue #830 asks for the number of self-authored tools that pass a witness
//! and then get **reused**, with tools that were authored and never called
//! tracked as the cost. That is a fold over receipts the store already keeps
//! (the `tool_calls` projection), not a
//! counter this module has to maintain: a counter can drift from the log, and
//! a fold cannot. [`Store::foundry_reuse`] joins each adoption against the
//! calls recorded *after* it, so a name that meant something else before
//! adoption cannot inflate the number.

use rusqlite::{OptionalExtension, params};

use crate::{Result, Store};

/// One workspace's record that a foundry-authored tool was adopted.
///
/// Mirrors `stella_tools::foundry_gate`'s gate input — the tools crate reads
/// this type directly rather than restating it, so the ledger's shape and the
/// gate's shape cannot drift.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptedTool {
    /// Tool name, matching the manifest's `name` and the registry key.
    pub name: String,
    /// The detector signature the tool was authored from, e.g. `jq <str> <path>`.
    pub signature: String,
    /// SHA-256 of the adopted manifest's complete bytes.
    pub manifest_digest: String,
    /// SHA-256 of the adopted script's complete bytes.
    pub script_digest: String,
    /// One-line rendering of the witness verdict, for the report.
    pub witness: String,
    /// The witness input, as JSON — half of what makes the proof re-runnable.
    pub witness_input: String,
    /// The value the witness asserted the tool's output contains.
    pub witness_expect: String,
    /// Whether it is enabled. Always `false` at adoption. Under `auto`
    /// autonomy the enabling decision is the autonomy pipeline's, standing in
    /// for the human behind the network-denial, breaker, and rollback
    /// controls; under the manual protocol it stays a human's alone.
    pub enabled: bool,
    /// When it was adopted.
    pub adopted_at: String,
    /// Why the tool is disabled, when a *mechanism* disabled it — the circuit
    /// breaker's recorded verdict. Empty for a fresh adoption, a
    /// human `--disable`, and every pre-v41 row: `enabled` says whether the
    /// tool is offered, this says which mechanism turned it off and why.
    pub disabled_reason: String,
}

/// One adopted tool with the use it has actually seen since — the #830
/// success metric, and its cost column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundryReuse {
    /// The adoption record.
    pub tool: AdoptedTool,
    /// Calls recorded since adoption. Zero is the false-start case: a tool
    /// that was authored, proven, and never reached for.
    pub calls: i64,
    /// How many of those calls failed.
    pub errors: i64,
    /// When it was last called, if ever.
    pub last_used: Option<String>,
}

impl FoundryReuse {
    /// Whether this adoption is a false start: proven, and never once used.
    pub fn is_false_start(&self) -> bool {
        self.calls == 0
    }
}

impl Store {
    /// Record an adoption. **Always lands disabled**, whatever the caller
    /// passes.
    ///
    /// Adoption is the machine's half of the decision and enablement is the
    /// human's, so this writer cannot grant the second one. Re-adopting an
    /// existing tool — the path a tampered or intentionally-edited tool takes
    /// back to being usable — therefore *revokes* its approval: the new bytes
    /// were never the approved ones, and inheriting the old flag would let an
    /// edit ride in on a decision made about different code.
    pub fn adopt_foundry_tool(&self, tool: &AdoptedTool) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO foundry_tools \
               (name, signature, manifest_digest, script_digest, witness, witness_input, \
                witness_expect, enabled, adopted_at, enabled_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, CURRENT_TIMESTAMP, NULL) \
             ON CONFLICT(name) DO UPDATE SET \
               signature = excluded.signature, \
               manifest_digest = excluded.manifest_digest, \
               script_digest = excluded.script_digest, \
               witness = excluded.witness, \
               witness_input = excluded.witness_input, \
               witness_expect = excluded.witness_expect, \
               enabled = 0, \
               adopted_at = CURRENT_TIMESTAMP, \
               enabled_at = NULL, \
               disabled_reason = ''",
            params![
                tool.name,
                tool.signature,
                tool.manifest_digest,
                tool.script_digest,
                tool.witness,
                tool.witness_input,
                tool.witness_expect,
            ],
        )?;
        Ok(())
    }

    /// Enable or disable an adopted tool — the one human decision in the
    /// protocol. Returns `false` when no such adoption exists, so a caller can
    /// tell "flipped it" from "there was nothing to flip" rather than
    /// reporting success for a no-op.
    pub fn set_foundry_tool_enabled(&self, name: &str, enabled: bool) -> Result<bool> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE foundry_tools SET enabled = ?2, \
               enabled_at = CASE WHEN ?2 = 1 THEN CURRENT_TIMESTAMP ELSE NULL END, \
               disabled_reason = '' \
             WHERE name = ?1",
            params![name, i64::from(enabled)],
        )?;
        Ok(changed > 0)
    }

    /// Forget an adoption entirely — used when the adopted files are removed,
    /// so the ledger does not keep an approval alive for a tool that is gone.
    pub fn forget_foundry_tool(&self, name: &str) -> Result<bool> {
        let conn = self.lock();
        let removed = conn.execute("DELETE FROM foundry_tools WHERE name = ?1", params![name])?;
        Ok(removed > 0)
    }

    /// Every adoption in this workspace, by name.
    pub fn adopted_foundry_tools(&self) -> Result<Vec<AdoptedTool>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT name, signature, manifest_digest, script_digest, witness, witness_input, \
                    witness_expect, enabled, adopted_at, disabled_reason \
             FROM foundry_tools ORDER BY name ASC",
        )?;
        let rows = stmt.query_map([], row_to_adopted)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// One adoption by name.
    pub fn adopted_foundry_tool(&self, name: &str) -> Result<Option<AdoptedTool>> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT name, signature, manifest_digest, script_digest, witness, witness_input, \
                        witness_expect, enabled, adopted_at, disabled_reason \
                 FROM foundry_tools WHERE name = ?1",
                params![name],
                row_to_adopted,
            )
            .optional()?;
        Ok(row)
    }

    /// The #830 success metric: every adoption with the calls it has drawn
    /// since being adopted.
    ///
    /// Counted from `adopted_at` forward on purpose. A foundry tool is named
    /// after the shell shape it replaced, and the workspace may well have had
    /// something by that name before — counting the whole history would credit
    /// an adoption for calls made before it existed, which is precisely the
    /// direction a self-improvement metric must not be wrong in.
    pub fn foundry_reuse(&self) -> Result<Vec<FoundryReuse>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT f.name, f.signature, f.manifest_digest, f.script_digest, f.witness, \
                    f.witness_input, f.witness_expect, f.enabled, f.adopted_at, f.disabled_reason, \
                    (SELECT COUNT(*) FROM tool_calls t \
                      WHERE t.name = f.name AND t.ts >= f.adopted_at AND t.state != 'running'), \
                    (SELECT COUNT(*) FROM tool_calls t \
                      WHERE t.name = f.name AND t.ts >= f.adopted_at AND t.state = 'error'), \
                    (SELECT MAX(t.ts) FROM tool_calls t \
                      WHERE t.name = f.name AND t.ts >= f.adopted_at) \
             FROM foundry_tools f ORDER BY f.name ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(FoundryReuse {
                tool: row_to_adopted(r)?,
                calls: r.get(10)?,
                errors: r.get(11)?,
                last_used: r.get(12)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

/// One row of a foundry tool's append-only version history — digests and
/// metadata only; the bytes are fetched separately by
/// [`Store::foundry_version_bytes`], because a listing does not need them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundryVersion {
    /// Tool name.
    pub name: String,
    /// 1-based version, counting up per tool.
    pub version: i64,
    /// SHA-256 of this version's manifest bytes.
    pub manifest_digest: String,
    /// SHA-256 of this version's script bytes.
    pub script_digest: String,
    /// Why this version was recorded: `adopt`, `rollback to vN`, …
    pub reason: String,
    /// When it was recorded.
    pub created_at: String,
}

/// One recorded launch of a foundry-built tool — what the circuit breaker
/// folds over and what `stella tools --status` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundryInvocation {
    /// Tool name.
    pub name: String,
    /// SHA-256 of the script that ran — ties the outcome to a version.
    pub script_digest: String,
    /// The `tool_gaps.jsonl` gap id the tool was authored from, or empty.
    pub gap_id: String,
    /// Wall-clock duration of the launch.
    pub duration_ms: i64,
    /// Whether the process exited 0.
    pub ok: bool,
    /// Whether the launch was killed at its timeout.
    pub timed_out: bool,
    /// Captured stdout size in bytes.
    pub output_bytes: i64,
}

impl Store {
    /// Append one version row — the exact bytes alongside their digests —
    /// returning the version number it landed as. Append-only: rollback
    /// appends a copy of the target version rather than editing history.
    pub fn record_foundry_version(
        &self,
        name: &str,
        manifest: &[u8],
        script: &[u8],
        manifest_digest: &str,
        script_digest: &str,
        reason: &str,
    ) -> Result<i64> {
        let conn = self.lock();
        let version: i64 = conn.query_row(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM foundry_tool_versions WHERE name = ?1",
            params![name],
            |r| r.get(0),
        )?;
        conn.execute(
            "INSERT INTO foundry_tool_versions \
               (name, version, manifest_digest, script_digest, manifest_bytes, script_bytes, \
                reason) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                name,
                version,
                manifest_digest,
                script_digest,
                manifest,
                script,
                reason
            ],
        )?;
        Ok(version)
    }

    /// One tool's version history, oldest first, without the bytes.
    pub fn foundry_versions(&self, name: &str) -> Result<Vec<FoundryVersion>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT name, version, manifest_digest, script_digest, reason, created_at \
             FROM foundry_tool_versions WHERE name = ?1 ORDER BY version ASC",
        )?;
        let rows = stmt.query_map(params![name], |r| {
            Ok(FoundryVersion {
                name: r.get(0)?,
                version: r.get(1)?,
                manifest_digest: r.get(2)?,
                script_digest: r.get(3)?,
                reason: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// One version's exact bytes: `(manifest, script)`. `None` when the tool
    /// has no such version.
    pub fn foundry_version_bytes(
        &self,
        name: &str,
        version: i64,
    ) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT manifest_bytes, script_bytes FROM foundry_tool_versions \
                 WHERE name = ?1 AND version = ?2",
                params![name, version],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row)
    }

    /// Append one launch's telemetry row.
    pub fn record_foundry_invocation(&self, invocation: &FoundryInvocation) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO foundry_invocations \
               (name, script_digest, gap_id, duration_ms, ok, timed_out, output_bytes) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                invocation.name,
                invocation.script_digest,
                invocation.gap_id,
                invocation.duration_ms,
                i64::from(invocation.ok),
                i64::from(invocation.timed_out),
                invocation.output_bytes,
            ],
        )?;
        Ok(())
    }

    /// The most recent `limit` outcomes for one tool, **newest first** — the
    /// circuit breaker's input. A fold over the log rather than a maintained
    /// counter, for the same reason as [`Store::foundry_reuse`].
    pub fn recent_foundry_outcomes(&self, name: &str, limit: usize) -> Result<Vec<bool>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT ok FROM foundry_invocations WHERE name = ?1 \
             ORDER BY rowid DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![name, limit as i64],
            |r| Ok(r.get::<_, i64>(0)? != 0),
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Disable a tool with the mechanism's recorded reason — the circuit
    /// breaker's write. A human `--disable` goes through
    /// [`Store::set_foundry_tool_enabled`], which records no reason.
    pub fn disable_foundry_tool_with_reason(&self, name: &str, reason: &str) -> Result<bool> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE foundry_tools SET enabled = 0, enabled_at = NULL, disabled_reason = ?2 \
             WHERE name = ?1",
            params![name, reason],
        )?;
        Ok(changed > 0)
    }

    /// Re-pin an adoption to restored bytes — the rollback write.
    ///
    /// Restoring a prior version re-digests the files on disk, and the
    /// adoption record has to pin those digests or the gate would withhold
    /// the restored tool as tampered. Re-enables and clears any breaker
    /// verdict: a rolled-back tool is a *new version* in the breaker's eyes,
    /// which is exactly how a tripped breaker is reset. Returns `false` when
    /// no such adoption exists.
    pub fn repin_foundry_tool(
        &self,
        name: &str,
        manifest_digest: &str,
        script_digest: &str,
    ) -> Result<bool> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE foundry_tools SET manifest_digest = ?2, script_digest = ?3, \
               enabled = 1, enabled_at = CURRENT_TIMESTAMP, disabled_reason = '', \
               adopted_at = CURRENT_TIMESTAMP \
             WHERE name = ?1",
            params![name, manifest_digest, script_digest],
        )?;
        Ok(changed > 0)
    }

    /// The most recent shell commands this workspace ran, oldest first —
    /// the gap detector's feeder. Reads the `tool_calls` projection
    /// for finished `bash` calls and extracts each recorded input's
    /// `command`; rows whose input carries none (or was never recorded) are
    /// skipped rather than guessed at.
    pub fn recent_shell_invocations(&self, limit: usize) -> Result<Vec<(String, bool)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT args_json, state FROM tool_calls \
             WHERE name = 'bash' AND state IN ('ok', 'error') \
             ORDER BY rowid DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (args_json, state) = row?;
            let Ok(args) = serde_json::from_str::<serde_json::Value>(&args_json) else {
                continue;
            };
            let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
                continue;
            };
            out.push((command.to_string(), state == "ok"));
        }
        // The query walked newest-first to bound the scan; the detector wants
        // history in the order it happened.
        out.reverse();
        Ok(out)
    }
}

/// The ten leading columns every reader above selects, in one place so the
/// column order and the struct cannot drift.
fn row_to_adopted(r: &rusqlite::Row<'_>) -> rusqlite::Result<AdoptedTool> {
    Ok(AdoptedTool {
        name: r.get(0)?,
        signature: r.get(1)?,
        manifest_digest: r.get(2)?,
        script_digest: r.get(3)?,
        witness: r.get(4)?,
        witness_input: r.get(5)?,
        witness_expect: r.get(6)?,
        enabled: r.get::<_, i64>(7)? != 0,
        adopted_at: r.get(8)?,
        disabled_reason: r.get(9)?,
    })
}

#[cfg(test)]
mod tests;
