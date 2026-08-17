//! The deterministic half of the `self-driving` delivery loop.
//!
//! `self-driving` is a cycle: fix a batch of defects, audit what is left, file
//! what it cannot fix, benchmark against the comparator, ship, and repeat.
//! Everything a machine can decide without a model lives here, so the model
//! never has to re-derive it and cannot get it subtly wrong: the AIMD
//! controller that sizes a cycle to its machine, the aperture ladder that
//! keeps "no defects" a statement about a lens rather than the code, the
//! dry-streak oracle that advances the ladder, the digest normalization the
//! dedup set rests on, and the folds that turn the ledger into evidence.
//!
//! No I/O (invariant 2): every function here is synchronous over owned data.
//! The probes that read the machine, the files that hold the state, and the
//! processes that run tools all live in `stella-cli` (`self_driving_cmd`), which
//! feeds their results in and writes the decisions out. That split is what
//! makes this module property-testable — and it is the same telemetry the
//! observatory reads back read-only (`stella-observatory/src/self_driving.rs`).
//!
//! Ported from `scripts/self-driving.sh` (#1548); the shell driver now delegates
//! these decisions instead of carrying a second copy of them.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

// ---------------------------------------------------------------------------
// The dedup digest — the key the loop's termination story rests on
// ---------------------------------------------------------------------------

/// Digest a finding's description into the dedup key recorded in `seen.txt`.
///
/// Normalize before hashing: lowercase, collapse whitespace, and replace
/// `:<digits>` with `:L`. The same defect described twice with a shifted line
/// number is one finding, and a digest that disagreed would break the oracle —
/// the loop would re-file it and the dry streak would never build.
///
/// Sixteen hex characters, matching what `shasum -a 256 | cut -c1-16`
/// produced: `seen.txt` files written by the shell driver keep deduplicating.
pub fn finding_digest(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut pending_space = false;
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            pending_space = true;
            continue;
        }
        if pending_space {
            if !normalized.is_empty() {
                normalized.push(' ');
            }
            pending_space = false;
        }
        if c == ':' && chars.peek().is_some_and(|n| n.is_ascii_digit()) {
            while chars.peek().is_some_and(|n| n.is_ascii_digit()) {
                chars.next();
            }
            // ":L" with a capital L, byte-for-byte what the shell driver's
            // sed wrote (its lowercasing ran BEFORE the substitution) — a
            // different spelling here would re-report every finding in every
            // existing seen.txt as new.
            normalized.push_str(":L");
            continue;
        }
        for lower in c.to_lowercase() {
            normalized.push(lower);
        }
    }

    let digest = Sha256::digest(normalized.as_bytes());
    let mut hex = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

// ---------------------------------------------------------------------------
// The AIMD controller — the only thing that moves the ceilings
// ---------------------------------------------------------------------------

/// Hard bounds the controller may never cross, whatever the evidence says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AimdLimits {
    /// The batch ceiling can never exceed this (default 20).
    pub batch_max: u32,
    /// A resource failure can never halve the batch below this (default 2).
    pub batch_min: u32,
    /// Worktree parallelism can never exceed this (default 4).
    pub parallel_max: u32,
}

impl Default for AimdLimits {
    fn default() -> Self {
        Self {
            batch_max: 20,
            batch_min: 2,
            parallel_max: 4,
        }
    }
}

/// The controller's learned state, persisted as `calibration.json`.
///
/// `extra` carries every key this module does not own — `last_clean_head`
/// among them — because rebuilding the document from the owned keys silently
/// discarded them once already, leaving watch mode unable to tell "nothing
/// changed" from "never looked" and so unable to ever sleep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Calibration {
    pub batch_ceiling: u32,
    pub parallel_ceiling: u32,
    #[serde(default)]
    pub clean_run: u32,
    #[serde(default)]
    pub note: String,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Calibration {
    /// The optimistic seed: start at the hard max rather than crawling up.
    ///
    /// A ceiling that starts low and only rises after several clean cycles
    /// wastes a capable machine for a week; AIMD's decrease is fast enough
    /// that starting high costs at most one degraded cycle.
    pub fn seeded(limits: &AimdLimits) -> Self {
        Self {
            batch_ceiling: limits.batch_max,
            parallel_ceiling: 2,
            clean_run: 0,
            note: "seeded".to_string(),
            extra: serde_json::Map::new(),
        }
    }
}

/// How a cycle ended, as the controller cares: `Ok` is "completed within its
/// resources"; `ResourceFail` is a killed compiler, OOM, ENOSPC, or thrash.
/// A red gate is neither — the batch was wrong, not too big.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CycleOutcome {
    Ok,
    ResourceFail,
}

/// One AIMD step. Additive increase on a clean cycle (+2 batch; one more
/// worktree only after three consecutive clean cycles, because parallelism is
/// the knob that hurts most when it is wrong), multiplicative decrease on a
/// resource failure (halve, and drop straight back to serial) — the
/// controller shape that provably converges instead of oscillating.
pub fn calibrate(cal: &mut Calibration, outcome: CycleOutcome, limits: &AimdLimits) {
    match outcome {
        CycleOutcome::Ok => {
            cal.clean_run += 1;
            cal.batch_ceiling = (cal.batch_ceiling + 2).min(limits.batch_max);
            if cal.clean_run.is_multiple_of(3) {
                cal.parallel_ceiling = (cal.parallel_ceiling + 1).min(limits.parallel_max);
            }
            cal.note = format!("clean run {}", cal.clean_run);
        }
        CycleOutcome::ResourceFail => {
            cal.clean_run = 0;
            cal.batch_ceiling = (cal.batch_ceiling / 2).max(limits.batch_min);
            cal.parallel_ceiling = 1;
            cal.note = "resource failure — halved".to_string();
        }
    }
}

// ---------------------------------------------------------------------------
// The aperture ladder — why this loop never finishes
// ---------------------------------------------------------------------------

/// What mechanically backs a lens (#1549).
///
/// A lens whose tooling is only a word handed to the model is as good as the
/// model's unaided attention that cycle — exactly the non-determinism the
/// ladder exists to remove. So every lens declares either the concrete
/// command a cycle runs and interprets, or that it is model-only, on the
/// record. No lens is silently a no-op: an aperture with no tooling must say
/// so, so the model knows it is working unaided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tooling {
    /// A command the cycle runs; `interpret` says what its output means as
    /// findings.
    Command {
        run: &'static str,
        interpret: &'static str,
    },
    /// No mechanical backing exists yet; the model audits unaided and the
    /// note says what tooling would close the gap.
    ModelOnly { note: &'static str },
}

/// One rung of the aperture ladder: a distinct question about the code, not a
/// deeper pass of the previous one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lens {
    pub name: &'static str,
    pub tooling: Tooling,
    /// True for lenses that spend real money or hours; the governor opens
    /// them only on the heavy tier, like the head-to-head bench arm.
    pub heavy_only: bool,
}

/// The ladder, widest-yield first. Exhausting it drops the loop into
/// [`WATCH`] rather than ending it: "no more defects" is always a statement
/// about the lens, never about the code.
pub const LENSES: &[Lens] = &[
    Lens {
        name: "rubric",
        tooling: Tooling::Command {
            run: "/ultraudit (deep) or /reaudit (fast)",
            interpret: "the report's findings, deduped by digest against seen.txt",
        },
        heavy_only: false,
    },
    Lens {
        name: "properties",
        tooling: Tooling::Command {
            run: "rg --files-without-match 'proptest' -g '*.rs' \
                  crates/stella-core/src crates/stella-context/src \
                  crates/stella-fleet/src crates/stella-pipeline/src \
                  crates/stella-tui/src",
            interpret: "each listed file that holds pure decision logic and has \
                        no property test is a finding",
        },
        heavy_only: false,
    },
    Lens {
        name: "invariants",
        tooling: Tooling::Command {
            run: "make invariants",
            interpret: "numbering/citation violations mechanically; the semantic \
                        half — does the code still honor each numbered invariant \
                        in AGENTS.md — is read by the model against that list",
        },
        heavy_only: false,
    },
    Lens {
        name: "concurrency",
        tooling: Tooling::ModelOnly {
            note: "no mechanical backing yet — unaided review of async, locking, \
                   and atomicity; loom or tokio-console would close the gap",
        },
        heavy_only: false,
    },
    Lens {
        name: "performance",
        tooling: Tooling::Command {
            run: "scripts/self-driving.sh bench loop, plus the prompt-cache golden \
                  fixtures (cargo test -p stella-model)",
            interpret: "a regression against the previous loop-bench run or a \
                        golden diff is a finding",
        },
        heavy_only: true,
    },
    Lens {
        name: "supply-chain",
        tooling: Tooling::Command {
            run: "make supply-chain",
            interpret: "every advisory, ban, source, or license finding is a \
                        defect (this is a separate CI job, so running it in a \
                        cycle is additive, not duplicated work)",
        },
        heavy_only: false,
    },
    Lens {
        name: "security",
        tooling: Tooling::ModelOnly {
            note: "no mechanical backing yet — unaided review of injection, \
                   path traversal, and secret handling; cargo-deny's advisory \
                   half overlaps but does not read this repo's own code",
        },
        heavy_only: false,
    },
    Lens {
        name: "docs",
        tooling: Tooling::Command {
            run: "make doc-links && make doc-report && \
                  scripts/check-command-docs.sh",
            interpret: "stale citations, uncited documents, and undocumented \
                        commands are findings",
        },
        heavy_only: false,
    },
    Lens {
        name: "soak",
        tooling: Tooling::Command {
            run: "scripts/self-driving.sh bench h2h (the long task list; rehearse \
                  with --rig --dry-run first)",
            interpret: "aborts, stalls, and stuck-loop detections across the \
                        long run are findings",
        },
        heavy_only: true,
    },
];

/// The terminal rung: not a lens, a mode. Cheap sentinels, no spend, waking
/// when something invalidates the last clean sweep.
pub const WATCH: &str = "watch";

/// Look a lens up by name.
pub fn lens(name: &str) -> Option<&'static Lens> {
    LENSES.iter().find(|l| l.name == name)
}

/// The lens after `current`, or [`WATCH`] when the ladder is exhausted (or
/// `current` is not a lens at all — an unknown aperture has nothing left to
/// open, and sleeping beats guessing).
pub fn advance(current: &str) -> &'static str {
    let mut found = false;
    for l in LENSES {
        if found {
            return l.name;
        }
        if l.name == current {
            found = true;
        }
    }
    WATCH
}

// ---------------------------------------------------------------------------
// The ledger — one record per completed cycle
// ---------------------------------------------------------------------------

/// One appended line of `ledger.jsonl`.
///
/// `extra` preserves keys future writers add, so a fold never destroys what
/// it did not understand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CycleRecord {
    pub cycle: u64,
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub ended_at: String,
    #[serde(default)]
    pub ended_at_unix: i64,
    #[serde(default)]
    pub fixed: u64,
    #[serde(default)]
    pub filed: u64,
    #[serde(default)]
    pub new_findings: u64,
    #[serde(default)]
    pub bench: String,
    #[serde(default)]
    pub gate: String,
    #[serde(default)]
    pub prs: Vec<String>,
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub aperture: String,
    /// The tool that produced this cycle's findings for the open lens
    /// (#1549) — the declared backing by default, or whatever the driver
    /// reports it actually ran. A cycle report that cannot say which tool
    /// looked is a cycle that may as well not have looked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lens_tool: Option<String>,
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub minutes: u64,
    /// A cycle is DRY when the audit found nothing that was not already
    /// known. Fixing things does not make a cycle dry; discovering nothing
    /// does.
    #[serde(default)]
    pub dry: bool,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// How many consecutive dry cycles the CURRENT lens has produced, walking the
/// ledger backwards.
///
/// The streak is scoped to the lens: a record from a different aperture ends
/// the count, so a freshly opened lens starts at zero instead of inheriting
/// the dry tail that advanced its predecessor — otherwise every lens after
/// the first would advance on a single dry audit. And the dry cycles must be
/// consecutive: a discovering cycle between two dry ones restarts the count,
/// so dry-noise-dry never advances.
pub fn dry_streak(rows: &[CycleRecord], lens: &str) -> u32 {
    let mut streak = 0;
    for rec in rows.iter().rev() {
        if rec.aperture != lens || !rec.dry {
            break;
        }
        streak += 1;
    }
    streak
}

// ---------------------------------------------------------------------------
// The governor — the plan is a function of the machine
// ---------------------------------------------------------------------------

/// What the box has right now, as the probes report it. Every field is a
/// point-in-time reading; `stella-cli` owns how each is measured (and lets an
/// operator pin any of them via `SELF_DRIVING_PROBE_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Supply {
    pub cpu: u32,
    /// Integer part of the 1-minute load average; the governor compares
    /// magnitudes, not decimals.
    pub load1: u32,
    pub mem_total_gb: u64,
    pub mem_free_gb: u64,
    pub disk_free_gb: u64,
    pub on_battery: bool,
    /// Something else — a benchmark match, another build — owns this box.
    pub busy: bool,
}

/// How much work is waiting, and how urgent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Demand {
    pub open_defects: u32,
    pub p0: u32,
}

/// Below these the machine cannot host the work at all, whatever the learned
/// ceilings say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Floors {
    pub disk_gb: u64,
    pub mem_gb: u64,
}

impl Default for Floors {
    fn default() -> Self {
        Self {
            disk_gb: 15,
            mem_gb: 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Light,
    Normal,
    Heavy,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Light => "light",
            Tier::Normal => "normal",
            Tier::Heavy => "heavy",
        }
    }
}

/// Where the gate compiles: locally scoped to the impacted crates, pushed to
/// CI, or the whole workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildScope {
    Impacted,
    Ci,
    Workspace,
}

impl BuildScope {
    pub fn as_str(self) -> &'static str {
        match self {
            BuildScope::Impacted => "impacted",
            BuildScope::Ci => "ci",
            BuildScope::Workspace => "workspace",
        }
    }
}

/// Which comparator arm this cycle can afford.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchArm {
    Off,
    Loop,
    H2h,
}

impl BenchArm {
    pub fn as_str(self) -> &'static str {
        match self {
            BenchArm::Off => "off",
            BenchArm::Loop => "loop",
            BenchArm::H2h => "h2h",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditDepth {
    Deep,
    Fast,
}

impl AuditDepth {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditDepth::Deep => "deep",
            AuditDepth::Fast => "fast",
        }
    }
}

/// The governor's decision: a tier and the concrete knobs, plus the reason —
/// because a governor whose decisions cannot be explained gets overridden and
/// then ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CyclePlan {
    pub tier: Tier,
    pub batch: u32,
    pub parallel: u32,
    pub local_build: bool,
    pub scope: BuildScope,
    pub audit: AuditDepth,
    pub bench: BenchArm,
    pub reason: String,
}

/// Size a cycle to the machine it will actually run on: supply (what the box
/// has right now) x demand (what the queue needs) x calibration (what
/// previous cycles proved this box survives) -> a tier and concrete knobs.
///
/// The rung order is the contract: floors and contention outrank everything,
/// including heavy-class hardware; demand overrides supply in exactly one
/// direction (a P0 is worth a degraded cycle, never a skipped one — it
/// shrinks the batch to what will fit rather than standing down); and demand
/// never upgrades the tier or widens the batch.
pub fn plan_cycle(
    supply: Supply,
    demand: Demand,
    ceilings: &Calibration,
    floors: Floors,
) -> CyclePlan {
    let mut tier = Tier::Normal;
    let mut local_build = true;
    let mut bench = BenchArm::Loop;
    let mut audit = AuditDepth::Deep;
    let mut scope = BuildScope::Impacted;
    let mut batch = ceilings.batch_ceiling;
    let mut parallel = ceilings.parallel_ceiling;

    let reason: String;
    if supply.disk_free_gb < floors.disk_gb {
        tier = Tier::Light;
        local_build = false;
        scope = BuildScope::Ci;
        bench = BenchArm::Off;
        reason = format!(
            "disk {}GB < {}GB floor — a workspace build needs 10-20GB and would die as a killed compiler",
            supply.disk_free_gb, floors.disk_gb
        );
    } else if supply.busy {
        tier = Tier::Light;
        local_build = false;
        scope = BuildScope::Ci;
        bench = BenchArm::Off;
        parallel = 1;
        reason = "another build or benchmark owns this box — a concurrent cargo run corrupts a measured match"
            .to_string();
    } else if supply.mem_free_gb < floors.mem_gb {
        tier = Tier::Light;
        local_build = false;
        scope = BuildScope::Ci;
        bench = BenchArm::Off;
        parallel = 1;
        reason = format!(
            "only {}GB free of {}GB — a workspace build here swap-thrashes",
            supply.mem_free_gb, supply.mem_total_gb
        );
    } else if supply.on_battery {
        tier = Tier::Light;
        parallel = 1;
        bench = BenchArm::Off;
        reason = "on battery — shed compute, keep the cycle useful".to_string();
    } else if supply.load1 >= supply.cpu {
        tier = Tier::Light;
        parallel = 1;
        bench = BenchArm::Off;
        reason = format!(
            "load {} at or above {} cores — the box is already saturated; a bench timed here is a number the loop cannot trust",
            supply.load1, supply.cpu
        );
    } else if supply.mem_total_gb >= 32
        && supply.disk_free_gb >= 100
        && supply.load1 < supply.cpu / 2
    {
        tier = Tier::Heavy;
        scope = BuildScope::Workspace;
        bench = BenchArm::H2h;
        reason = format!(
            "{}GB / {} cores / {}GB free and idle — this box can host the full workspace and a measured match",
            supply.mem_total_gb, supply.cpu, supply.disk_free_gb
        );
    } else {
        reason = format!(
            "{}GB / {} cores / {}GB free — ordinary cycle at the calibrated ceiling",
            supply.mem_total_gb, supply.cpu, supply.disk_free_gb
        );
    }
    let mut reason = reason;

    if demand.p0 > 0 && tier == Tier::Light {
        batch = demand.p0;
        audit = AuditDepth::Fast;
        let _ = write!(
            reason,
            "; but {} P0 open — running a minimal cycle anyway",
            demand.p0
        );
    }

    // An empty queue does not deserve a big batch, whatever the box can
    // afford.
    if demand.open_defects > 0 && batch > demand.open_defects {
        batch = demand.open_defects;
    }
    batch = batch.max(1);
    if tier == Tier::Light {
        batch = batch.min(5);
    }

    CyclePlan {
        tier,
        batch,
        parallel,
        local_build,
        scope,
        audit,
        bench,
        reason,
    }
}

// ---------------------------------------------------------------------------
// Metrics — the loop's evidence about ITSELF
// ---------------------------------------------------------------------------

/// A named pathology the meta-cycle acts on. Each names a SPECIFIC failure
/// mode rather than a vague "performance is bad", because the fix differs
/// completely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signal {
    pub code: &'static str,
    pub text: &'static str,
}

/// The ledger folded into the numbers `/self-driving:evolve` argues from.
#[derive(Debug, Clone, PartialEq)]
pub struct Metrics {
    pub cycles: u64,
    pub fixed: u64,
    pub filed: u64,
    pub new_findings: u64,
    pub zero_fix_cycles: u64,
    pub red_gate_cycles: u64,
    pub signals: Vec<Signal>,
}

/// Fold the ledger. The thresholds are shared verbatim with what the shell
/// driver reported and what the observatory renders, so the terminal and the
/// dashboard never disagree about whether the loop is in trouble.
pub fn metrics(rows: &[CycleRecord]) -> Metrics {
    let n = rows.len() as u64;
    let fixed: u64 = rows.iter().map(|r| r.fixed).sum();
    let filed: u64 = rows.iter().map(|r| r.filed).sum();
    let new_findings: u64 = rows.iter().map(|r| r.new_findings).sum();
    let zero_fix = rows.iter().filter(|r| r.fixed == 0).count() as u64;
    let red = rows.iter().filter(|r| r.gate == "red").count() as u64;

    let mut signals = Vec::new();
    if n > 0 {
        let tail_zero = rows.len() >= 3 && rows[rows.len() - 3..].iter().all(|r| r.fixed == 0);
        if zero_fix >= 3 && tail_zero {
            signals.push(Signal {
                code: "STUCK",
                text: "3 consecutive zero-fix cycles — the blocker is structural, not the queue",
            });
        }
        if n >= 5 && 2 * new_findings < n && filed > 2 * n {
            signals.push(Signal {
                code: "NOISY",
                text: "filing far more than it discovers — dedup is probably leaking",
            });
        }
        if red >= 2.max(n / 3) {
            signals.push(Signal {
                code: "FRAGILE",
                text: "a third of cycles end on a red gate — batches are too large or too mixed",
            });
        }
    }

    Metrics {
        cycles: n,
        fixed,
        filed,
        new_findings,
        zero_fix_cycles: zero_fix,
        red_gate_cycles: red,
        signals,
    }
}

/// The controller-shape signal, computed against the calibration rather than
/// the ledger: a ceiling that stayed low through five clean runs means the
/// decrease never decayed and the loop is starving a healthy machine.
pub fn starved(cal: &Calibration) -> Option<Signal> {
    (cal.batch_ceiling <= 4 && cal.clean_run >= 5).then_some(Signal {
        code: "STARVED",
        text: "ceiling stayed low through 5 clean runs — the decrease never decayed",
    })
}

// ---------------------------------------------------------------------------
// The defect queue — ranked P0 > P1 > P2, oldest first inside a rank
// ---------------------------------------------------------------------------

/// One open issue as `gh issue list --json number,title,labels,createdAt,url`
/// reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueIssue {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub labels: Vec<IssueLabel>,
    #[serde(rename = "createdAt", default)]
    pub created_at: String,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssueLabel {
    pub name: String,
}

impl QueueIssue {
    fn has_label(&self, name: &str) -> bool {
        self.labels.iter().any(|l| l.name == name)
    }

    /// P0 ranks 0, P1 ranks 1, P2 ranks 2, everything else 3 — an aged P1 is
    /// a worse thing to be carrying than a fresh one, so age only breaks ties
    /// inside a rank.
    pub fn priority_rank(&self) -> u8 {
        ["P0", "P1", "P2"]
            .iter()
            .position(|p| self.has_label(p))
            .map_or(3, |i| i as u8)
    }
}

/// Filter to defects and rank them. `bug` and `triage` both count: an
/// untriaged issue is a defect nobody has classified yet, not a non-defect.
/// Feature work is deliberately excluded — this loop closes defects, and
/// mixing the two makes the batch unreviewable.
pub fn rank_defects(issues: Vec<QueueIssue>) -> Vec<QueueIssue> {
    let mut defects: Vec<QueueIssue> = issues
        .into_iter()
        .filter(|i| i.has_label("bug") || i.has_label("triage"))
        .collect();
    defects.sort_by(|a, b| {
        a.priority_rank()
            .cmp(&b.priority_rank())
            .then_with(|| a.created_at.cmp(&b.created_at))
    });
    defects
}

// ---------------------------------------------------------------------------
// The run fold — running / completed / cancelled / crashed
// ---------------------------------------------------------------------------

/// Seconds without a heartbeat after which a `running` run is reported crashed.
///
/// Generous on purpose: a single model call inside an audit phase legitimately
/// runs for minutes, and calling a live run dead is a worse error than
/// noticing a dead one late. `SELF_DRIVING_STALE_AFTER_SECS` overrides it.
///
/// Public because every surface that resolves a run's liveness needs the same
/// number, and this constant is the only copy — `stella-cli`'s env default and
/// the observatory's dashboard both read it. Two of the three used to hold
/// their own `900` (#1613).
pub const DEFAULT_STALE_AFTER_SECS: i64 = 900;

/// Whether a run whose last record says `running` really is.
///
/// The status a run reports is not simply the last thing it wrote, and only a
/// reader can make the correction, because the process that would have written
/// the truth is gone. Both ways of being wrong are named rather than collapsed
/// into a bool: they have different causes and a surface may want to say which
/// happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// Holds the live pointer and its heartbeat is current.
    Live,
    /// The live pointer names a different run, or no run at all — nothing is
    /// driving this one.
    Orphaned,
    /// Holds the live pointer, but has not drawn breath inside the window.
    Stale,
}

impl Liveness {
    /// Whether a `running` record should be reported as `crashed`.
    #[must_use]
    pub fn is_crashed(self) -> bool {
        !matches!(self, Liveness::Live)
    }
}

/// Resolve a `running` run against the live pointer and the clock.
///
/// Pure by construction — the caller supplies the clock — which is what lets
/// the terminal and the dashboard share one answer instead of each reading
/// `SystemTime::now()` behind its own copy of the rule.
///
/// The heartbeat is the witness, never the pid: every self-driving subcommand is
/// its own short-lived process, so a recorded pid is dead moments after it is
/// written and would declare every run crashed.
#[must_use]
pub fn liveness(
    run_id: &str,
    live_run_id: &str,
    live_heartbeat_unix: i64,
    now_unix: i64,
    stale_after_secs: i64,
) -> Liveness {
    if live_run_id.is_empty() || live_run_id != run_id {
        return Liveness::Orphaned;
    }
    if now_unix - live_heartbeat_unix > stale_after_secs {
        return Liveness::Stale;
    }
    Liveness::Live
}

/// One folded run for the `run list` report.
#[derive(Debug, Clone, PartialEq)]
pub struct RunRow {
    pub run_id: String,
    pub status: String,
    pub at_unix: i64,
    pub cycles: u64,
    pub fixed: u64,
    pub filed: u64,
    pub new_findings: u64,
    /// `"<phase> (<age>s ago)"` for the live run, empty otherwise.
    pub phase: String,
}

/// Fold `runs.jsonl` by `run_id` (last write wins) and resolve the one live
/// status the file cannot state: a run whose last record says `running` but
/// whose heartbeat has gone stale is CRASHED, not running — and so is one the
/// live pointer has moved on from. Only a reader can know that, because the
/// process that would have written it is gone.
///
/// The records are open maps rather than a struct because `runs.jsonl` is an
/// open ledger: any `key=value` a writer chooses rides along, and a fold that
/// dropped unknown keys would destroy what it did not understand.
pub fn fold_runs(
    records: &[serde_json::Value],
    cycles: &[CycleRecord],
    live: Option<&serde_json::Value>,
    now_unix: i64,
    stale_after_secs: i64,
) -> Vec<RunRow> {
    use std::collections::BTreeMap;

    let mut folded: BTreeMap<String, serde_json::Map<String, serde_json::Value>> = BTreeMap::new();
    for rec in records {
        let Some(obj) = rec.as_object() else { continue };
        let Some(id) = obj.get("run_id").and_then(|v| v.as_str()) else {
            continue;
        };
        let slot = folded.entry(id.to_string()).or_default();
        for (k, v) in obj {
            slot.insert(k.clone(), v.clone());
        }
    }

    let live_id = live
        .and_then(|l| l.get("run_id"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    let mut out = Vec::new();
    for (id, rec) in folded {
        let mut status = rec
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let mut phase = String::new();

        if status == "running" {
            let heartbeat = live
                .and_then(|l| l.get("heartbeat_unix"))
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            let verdict = liveness(&id, live_id, heartbeat, now_unix, stale_after_secs);
            if verdict != Liveness::Orphaned {
                let live = live.expect("a non-orphaned run holds the live pointer");
                phase = format!(
                    "{} ({}s ago)",
                    live.get("phase").and_then(|v| v.as_str()).unwrap_or("?"),
                    now_unix - heartbeat
                );
            }
            if verdict.is_crashed() {
                status = "crashed".to_string();
            }
        }

        let mine: Vec<&CycleRecord> = cycles.iter().filter(|c| c.run_id == id).collect();
        out.push(RunRow {
            run_id: id,
            status,
            at_unix: rec
                .get("at_unix")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0),
            cycles: mine.len() as u64,
            fixed: mine.iter().map(|c| c.fixed).sum(),
            filed: mine.iter().map(|c| c.filed).sum(),
            new_findings: mine.iter().map(|c| c.new_findings).sum(),
            phase,
        });
    }
    out.sort_by_key(|r| -r.at_unix);
    out
}

#[cfg(test)]
mod tests;
