//! Tool-foundry gating — the authority boundary a self-authored tool must
//! cross before it can register, and the tamper baseline that keeps it there.
//!
//! This is the third slice of self-improvement issue #830. The first mined
//! capability gaps and *proposed* tools; the second
//! ([`crate::foundry_author`]) *authored* a staged manifest+script pair under
//! `.stella/tools/proposed/`, inert because discovery's non-recursive scan
//! cannot see that subdirectory. Inertness-by-obscurity is a fine guardrail
//! for a staging area and a bad one for an adopted tool: the moment a manifest
//! lands in `.stella/tools/` it registers, and "a human moved a file" is the
//! only record that it was ever meant to.
//!
//! This module replaces that with a **standing gate**. A foundry-authored
//! manifest sitting in `.stella/tools/` registers only when the workspace's
//! adoption ledger ([`AdoptedTool`], persisted in `stella-store`) says three
//! things at once:
//!
//! 1. the tool was **adopted** — its capability witness flipped
//!    ([`crate::foundry_witness`]);
//! 2. a human **enabled** it afterwards — adoption alone leaves it off, which
//!    is #830's "new tools land disabled behind the same authority/scope gate
//!    as any capability";
//! 3. the bytes on disk are still the bytes that were adopted.
//!
//! # Why a byte check and not just a flag
//!
//! Point 3 is the load-bearing one, and it is borrowed wholesale from the
//! pipeline's witness protocol. A witness test is deliberately *visible* to
//! the worker that must satisfy it, and its integrity comes from tamper
//! exclusion rather than secrecy: the filesystem identity of the artifact is
//! snapshotted, and a flip is credited only while those bytes are unchanged
//! (`stella_pipeline::witness::witness_identity_matches`). An adopted tool has
//! exactly that shape. Its script is a file in the repository that anything
//! with a shell can rewrite, and the proof that justified adopting it was a
//! proof about *that script's behavior*. Re-pointing the manifest at a new
//! command, or rewriting the script under an approved manifest, does not
//! inherit the approval — so both are digested at adoption and re-checked at
//! every discovery.
//!
//! # Scope: foundry tools only
//!
//! A hand-written manifest is untouched by all of this. A developer who drops
//! `deploy.toml` into their own repository has already made the trust decision
//! this gate exists to record, at the same level as a `package.json` script or
//! a Makefile target. The gate keys off the typed `[foundry]` table the
//! authoring pass stamps into the manifests it writes, so it constrains
//! exactly the tools Stella wrote for itself and nothing a person wrote.
//!
//! # Composition with the rest of the tool authority stack
//!
//! This gate is a *narrowing* layer, never a widening one — it can only
//! withhold. Everything else that governs tools still applies on top:
//! `authority.project_custom_tools_allowed` decides whether repository-scoped
//! manifests are read at all, and [`crate::policy::ToolPolicy`] can switch an
//! adopted-and-enabled tool back off by name or through its `custom` group.

use std::collections::BTreeMap;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::custom::{CustomTool, DiscoveryReport, ToolDiagnostic};

/// The `authored_by` value the foundry stamps, and the only value this gate
/// recognizes as its own. A manifest carrying a `[foundry]` table with any
/// other author is treated as hand-written — the gate governs what Stella
/// wrote, and claiming otherwise is not something a stranger's manifest gets
/// to opt into.
pub const AUTHORED_BY: &str = "stella-tool-foundry";

/// Machine-readable provenance stamped into every foundry-authored manifest.
///
/// This is a typed table (`[foundry]`), not the comment header the authoring
/// pass also writes. Comments are for the human reviewing the staged pair;
/// this is for the gate, and it has to survive a round trip through the same
/// parser discovery uses. It also carries the witness input, which is what
/// makes an authored tool provable *from the artifact alone* — by the time
/// someone adopts a staged tool the receipts it was mined from may have
/// rolled out of the detector's window, and a proof that needs the original
/// evidence to still be in scope is a proof that expires.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FoundryProvenance {
    /// Always [`AUTHORED_BY`] for a manifest this workspace's foundry wrote.
    pub authored_by: String,
    /// The detector signature the tool was synthesized from, e.g.
    /// `jq <str> <path>`.
    pub signature: String,
    /// How many invocations of that shape the detector saw.
    pub occurrences: u32,
    /// The input the capability witness runs with — one observed value per
    /// schema property, taken from the receipts the tool was mined from.
    /// Absent (an empty object) makes the tool unprovable and therefore
    /// unadoptable, which is the honest outcome rather than a witness over
    /// invented arguments.
    #[serde(default)]
    pub witness_input: serde_json::Value,
}

impl FoundryProvenance {
    /// Whether this provenance is the foundry's own claim on the manifest.
    pub fn is_foundry_authored(&self) -> bool {
        self.authored_by == AUTHORED_BY
    }
}

/// The adoption ledger's row type, re-exported from where it is persisted.
///
/// Deliberately **not** restated here. The gate's authority input and the
/// store's row are the same fact — "this workspace approved these bytes" — and
/// two declarations of one fact drift silently: a column added on one side
/// reads as a field the other side simply never learned about. Adoption's own
/// invariants (a fresh adoption is disabled; re-adoption revokes) are enforced
/// by the writer in `stella_store::foundry`, so this crate holds the decision
/// and that one holds the record.
pub use stella_store::AdoptedTool;

/// The on-disk bytes of an adopted pair, reduced to their digests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDigests {
    /// SHA-256 of the manifest file.
    pub manifest: String,
    /// SHA-256 of the script `command[0]` names.
    pub script: String,
}

/// Whether a discovered tool may register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    /// Advertise and execute it.
    Register,
    /// Withhold it, with the reason to report.
    Withhold(WithholdReason),
}

impl GateDecision {
    /// Whether the tool may register.
    pub fn registers(&self) -> bool {
        matches!(self, Self::Register)
    }
}

/// Why a foundry-authored tool is being withheld. Each variant is a *stated
/// reason* surfaced as a [`ToolDiagnostic`], so a developer who wonders where
/// their tool went reads the answer instead of an absence — the same contract
/// the pipeline's `NoWitnessReason` holds itself to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WithholdReason {
    /// A foundry manifest with no adoption record at all — someone moved a
    /// staged manifest up by hand instead of adopting it.
    NotAdopted,
    /// Adopted, witness green, but no human has enabled it yet.
    AwaitingEnablement,
    /// The manifest's bytes no longer match what was adopted.
    ManifestTampered,
    /// The script's bytes no longer match what was adopted.
    ScriptTampered,
    /// The script named by `command[0]` could not be read, so the tamper
    /// check could not run. "Could not look" is not "looked and saw nothing",
    /// and an unverifiable tool does not register.
    ScriptUnreadable(String),
}

impl WithholdReason {
    /// The sentence reported to the developer.
    pub fn sentence(&self) -> String {
        match self {
            Self::NotAdopted => {
                "foundry-authored but never adopted — run `stella tools --adopt <name>` to prove \
                 its witness, or delete it. A manifest moved here by hand carries no proof that \
                 it does what it claims (#830)."
                    .to_string()
            }
            Self::AwaitingEnablement => {
                "adopted with a green witness but not enabled — a self-authored tool lands \
                 disabled. Enable it with `stella tools --enable <name>`."
                    .to_string()
            }
            Self::ManifestTampered => {
                "the manifest's bytes changed after adoption — the approval covered the adopted \
                 bytes, not these. Re-adopt it to prove the new definition."
                    .to_string()
            }
            Self::ScriptTampered => {
                "the script's bytes changed after adoption — the witness proved the old script's \
                 behavior, so the approval does not carry over. Re-adopt it."
                    .to_string()
            }
            Self::ScriptUnreadable(why) => format!(
                "the adopted script could not be read ({why}), so its bytes could not be checked \
                 against the adoption record"
            ),
        }
    }
}

/// SHA-256 of `bytes`, lowercase hex — the workspace's one hashing primitive.
pub fn digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let finished = hasher.finalize();
    let mut hex = String::with_capacity(finished.len() * 2);
    for byte in finished.iter() {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Decide whether one discovered tool may register.
///
/// Pure: every filesystem read has already happened, and both the ledger row
/// and the observed digests arrive as data. `provenance` is `None` for a
/// hand-written manifest, which always registers — see the module docs.
///
/// `observed` is `None` when the script could not be read; that is a distinct
/// answer from "the digests disagree", and it is reported as such rather than
/// collapsed into tampering.
pub fn decide(
    provenance: Option<&FoundryProvenance>,
    observed: Result<&ArtifactDigests, String>,
    adopted: Option<&AdoptedTool>,
) -> GateDecision {
    // Not ours to govern. A person's own manifest is trusted at the level of
    // any other executable thing in their repository.
    let Some(provenance) = provenance.filter(|p| p.is_foundry_authored()) else {
        return GateDecision::Register;
    };
    let _ = provenance;

    let Some(adopted) = adopted else {
        return GateDecision::Withhold(WithholdReason::NotAdopted);
    };
    let observed = match observed {
        Ok(observed) => observed,
        Err(why) => return GateDecision::Withhold(WithholdReason::ScriptUnreadable(why)),
    };
    // Bytes before flags: a tampered manifest that also happens to be enabled
    // must report the tampering, which is the more serious answer.
    if observed.manifest != adopted.manifest_digest {
        return GateDecision::Withhold(WithholdReason::ManifestTampered);
    }
    if observed.script != adopted.script_digest {
        return GateDecision::Withhold(WithholdReason::ScriptTampered);
    }
    if !adopted.enabled {
        return GateDecision::Withhold(WithholdReason::AwaitingEnablement);
    }
    GateDecision::Register
}

/// Read the on-disk bytes behind one discovered tool and reduce them to
/// digests. `root` is the workspace root the manifest's relative `command[0]`
/// resolves against — the same root custom-tool subprocesses are spawned with.
pub fn observe(tool: &CustomTool, root: &Path) -> Result<ArtifactDigests, String> {
    let manifest = std::fs::read(&tool.source)
        .map_err(|e| format!("cannot read {}: {e}", tool.source.display()))?;
    let program = tool
        .command
        .first()
        .ok_or_else(|| "manifest has an empty command".to_string())?;
    let script_path = root.join(program);
    let script = std::fs::read(&script_path)
        .map_err(|e| format!("cannot read {}: {e}", script_path.display()))?;
    Ok(ArtifactDigests {
        manifest: digest(&manifest),
        script: digest(&script),
    })
}

/// Apply the gate to a whole [`DiscoveryReport`]: withheld tools leave the
/// tool list and arrive as diagnostics naming why.
///
/// Reporting rather than silently dropping is the point. A tool that vanishes
/// with no explanation is indistinguishable from a tool that was never
/// authored, and the difference — "your workspace has an unapproved
/// self-authored capability sitting in it" — is exactly what an operator
/// needs to see.
pub fn gate_report(
    report: DiscoveryReport,
    adopted: &[AdoptedTool],
    root: &Path,
) -> DiscoveryReport {
    let ledger: BTreeMap<&str, &AdoptedTool> = adopted
        .iter()
        .map(|record| (record.name.as_str(), record))
        .collect();

    let DiscoveryReport {
        tools,
        mut diagnostics,
    } = report;
    let mut kept = Vec::with_capacity(tools.len());
    for tool in tools {
        let observed = observe(&tool, root);
        let decision = decide(
            tool.foundry.as_ref(),
            observed.as_ref().map_err(String::clone),
            ledger.get(tool.name.as_str()).copied(),
        );
        match decision {
            GateDecision::Register => kept.push(tool),
            GateDecision::Withhold(reason) => diagnostics.push(ToolDiagnostic {
                path: tool.source.clone(),
                reason: format!("`{}` withheld: {}", tool.name, reason.sentence()),
            }),
        }
    }
    DiscoveryReport {
        tools: kept,
        diagnostics,
    }
}

#[cfg(test)]
mod tests;
