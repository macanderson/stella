//! The plugin manifest — participation is declared, never inferred.
//!
//! This module is slice A of #3245: the `[loop]` block and its grade ladder,
//! plus the `[requirements]`, `[oracle]`, `[subloop]`, and `[roles]` blocks,
//! parsed from TOML and validated as one consent document. The rules it
//! enforces are the epic's, restated where each is checked:
//!
//! - **Undeclared = none.** A manifest with no `[loop]` block participates
//!   at [`Participation::None`] — a content bundle.
//! - **Unknown keys are a load error** (the #1400 rule this crate inherits):
//!   every table denies unknown fields, so a typo'd grant fails loudly at
//!   install instead of silently granting nothing.
//! - **An undeclared hook is never invoked.** [`LoopGrant::permits_hook`] is
//!   the authoritative filter; a host must route every hook dispatch for a
//!   plugin through it, so a process that registers for an event its
//!   manifest never named is simply not called.
//! - **An undeclared wrapper point is never dispatched**, for the same reason
//!   and through the same shape: [`LoopGrant::points`] names the socket points
//!   this plugin answers and [`LoopGrant::permits_point`] is the filter. Before
//!   #3501 a manifest could not say this at all, so a host learned that a
//!   wrapper answers `after_turn` and refuses `before_turn` by *getting the
//!   refusal at run time* — the "manifest that quietly does nothing" failure
//!   this crate exists to prevent, one level up.
//! - **The grades are a monotone ladder** — each includes the ones below it
//!   ([`Participation::includes`]), and the powers that separate the rungs
//!   (`Stop`, `max_holds`, `[requirements]`, `[oracle]`) are rejected below
//!   the rung that grants them.
//!
//! Parsing is pure — a `&str` in, a value or a typed error out. Reading the
//! manifest off disk, prompting for install consent, clamping `max_holds`,
//! and binding the grants to the engine's gates are all the host's job
//! (#3245 §3: no plugin code in-process, and this crate holds no I/O either).

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::consent::{Capability, validate_capabilities};
use crate::error::ManifestError;
use crate::evidence::OracleCheck;
use crate::host_call::HostCall;
use crate::package::{
    RecordContribution, SkillContribution, ToolContribution, validate_contributions,
};
use crate::runtime::Runtime;
use crate::wire::WrapperPoint;
use crate::wrapper::Wrapper;

/// How much of a say in the turn loop a plugin has declared (#3245 §2).
///
/// A monotone ladder: each grade includes every grade below it, which is what
/// [`Participation::includes`] encodes and the derived ordering makes
/// mechanical. The variants are declared weakest-first so `Ord` *is* the
/// ladder — reordering them would silently invert every gate built on it.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Participation {
    /// A content bundle — skills, commands, agents, custom tools. No say in
    /// the loop at all, and the default when no `[loop]` block is declared.
    #[default]
    None,
    /// May subscribe to the turn event stream; cannot influence it.
    Observer,
    /// May act at declared hook points — inject context, rewrite tool input,
    /// decide permissions. May not touch completion.
    Steering,
    /// Additionally binds the Stop gate: at each would-be completion the
    /// host invokes the plugin's verdict hook, and `done: false` re-enters
    /// the loop, bounded by `max_holds`. The strongest grant.
    Arbiter,
}

impl Participation {
    /// Whether this grade includes `other` on the ladder — `arbiter`
    /// includes `steering`, which includes `observer`, which includes
    /// `none`. Every grade includes itself.
    #[must_use]
    pub fn includes(self, other: Participation) -> bool {
        self >= other
    }
}

impl std::fmt::Display for Participation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Participation::None => "none",
            Participation::Observer => "observer",
            Participation::Steering => "steering",
            Participation::Arbiter => "arbiter",
        })
    }
}

/// A hook point a plugin may declare in `[loop] hooks` — re-exported from
/// [`stella_protocol::hook`], which is where the vocabulary lives.
///
/// The grant is *for* the engine's dispatch points, so the set has to be the
/// engine's set exactly. Until #3310 this crate held its own copy of the
/// enum, because the two crates that spell the vocabulary may not depend on
/// each other in either direction: `stella-core` never learns plugins exist
/// (#3245 open question 3), and this crate must not pull in the engine.
/// Sharing one type through the crate *underneath* both settles it without
/// either edge — and turns "keep the two sets identical" from a review
/// obligation into a fact the compiler enforces.
///
/// **The PascalCase is load-bearing, and the casing split from
/// [`Participation`] is deliberate.** These five strings are not this
/// crate's to choose: `"PreToolUse"` is already what a user types in
/// `.stella/settings.json` to register a shell hook (README.md §Lifecycle
/// hooks). Spelling the same event `"pre_tool_use"` in a plugin manifest
/// would fork one concept's name across two files a user edits — strictly
/// worse than the inconsistency it would resolve. [`Participation`] is
/// lowercase because its vocabulary is this crate's own invention (#3245 §2)
/// and nothing outside spells it. The rule for a future block: **a value
/// that mirrors an existing user-facing string keeps that string's casing;
/// a value this crate coins is lowercase.**
pub use stella_protocol::hook::HookEvent;

/// The `[loop]` block — THE registration #3245's directive asks for.
///
/// Absent from a manifest, it defaults to no participation and no hooks,
/// which is why the struct (not just its fields) implements `Default`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopGrant {
    /// The declared grade on the ladder. Undeclared = `none`.
    #[serde(default)]
    pub participation: Participation,
    /// The hook points the plugin wants, exhaustively — an undeclared hook
    /// is never invoked, even if the plugin's process registers for it.
    #[serde(default)]
    pub hooks: Vec<HookEvent>,
    /// The wrapper socket points the plugin answers, exhaustively — an
    /// undeclared point is never dispatched, even if the plugin's process
    /// would happily answer it (#3501).
    ///
    /// Empty is a complete answer and the default: a plugin may declare a
    /// `[wrapper]` stage order — "run these stages, skip that ceremony" —
    /// while contributing nothing at any point of the turns it orders. What it
    /// may *not* do is leave the host to find out by refusal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<WrapperPoint>,
    /// The host capabilities the plugin may ask for mid-point, exhaustively —
    /// an undeclared call is refused, even if the plugin's process asks for it
    /// (`doc:wrapper-socket` §6b, #3540).
    ///
    /// Empty is a complete answer and the default: a plugin that only ever
    /// answers out of what the request handed it declares nothing here. What it
    /// may not do is ask for a capability a human never read at install — which
    /// is [`LoopGrant::permits_call`], the same authoritative filter
    /// [`LoopGrant::permits_hook`] is for hooks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<HostCall>,
    /// The most host calls per point the plugin asks for.
    ///
    /// **An ask, never an authority** — the [`LoopGrant::max_holds`] discipline,
    /// one layer down. A conversation can hang where a single exchange could
    /// not, so the host clamps this against its own ceiling
    /// (`stella_runtime::wrapper::DEFAULT_HOST_MAX_CALLS`) and a spent allowance
    /// refuses further calls *to the plugin* rather than killing it. Absent
    /// means "the host's ceiling", never "unbounded".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_calls: Option<u32>,
    /// Arbiter only: the most completion-vetoes per turn the plugin asks
    /// for. The host clamps it; a spent allowance completes the turn with
    /// the unmet requirements reported, not silently dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_holds: Option<u32>,
}

impl LoopGrant {
    /// Whether the host may invoke this plugin at `hook` — the authoritative
    /// filter behind "an undeclared hook is never invoked".
    ///
    /// Both conditions are checked even though [`validate`] already rejects
    /// hooks below `steering`: this function is the last line, and a grant
    /// constructed by hand (not through [`PluginManifest::from_toml_str`])
    /// must still never leak a dispatch.
    ///
    /// [`validate`]: PluginManifest::from_toml_str
    #[must_use]
    pub fn permits_hook(&self, hook: HookEvent) -> bool {
        self.participation.includes(Participation::Steering) && self.hooks.contains(&hook)
    }

    /// Whether the host may dispatch this plugin at `point` — the same
    /// authoritative filter [`LoopGrant::permits_hook`] is for hooks, for the
    /// wrapper socket's points.
    ///
    /// Both conditions are checked here too, and for the same reason: a grant
    /// assembled by hand rather than through
    /// [`PluginManifest::from_toml_str`] must still never leak a dispatch.
    #[must_use]
    pub fn permits_point(&self, point: WrapperPoint) -> bool {
        self.participation.includes(Participation::Steering) && self.points.contains(&point)
    }

    /// Whether the host may perform `call` when this plugin asks for it —
    /// the same authoritative filter again, for the host-call channel
    /// (`doc:wrapper-socket` §6b).
    ///
    /// A plugin **asks**; it never reaches. This is the function that makes the
    /// difference real: the host retrieves only what the grant a human consented
    /// to permits, and an undeclared ask is refused with a typed reason the
    /// plugin is told, exactly as an undeclared hook is simply never invoked.
    ///
    /// Both conditions are checked here too — a grant assembled by hand rather
    /// than through [`PluginManifest::from_toml_str`] must still never leak a
    /// capability.
    #[must_use]
    pub fn permits_call(&self, call: HostCall) -> bool {
        self.participation.includes(Participation::Steering) && self.calls.contains(&call)
    }
}

/// The `[oracle]` block — the witness protocol as a wire contract, and the
/// evidence protocol beside it.
///
/// # The plugin runs this and reports the result. Stella does not.
///
/// This doc comment used to read "the HOST runs this; the plugin never grades
/// its own work (the #2584 discipline, stated for plugins)", and
/// [`crate::consent_text`] repeated it to a user about to install. **No host
/// code has ever executed it** — `grep -rn OracleCommand crates/ --include=*.rs`
/// outside this crate returns only tests, while the flip and the measurements
/// `stella_runtime::wrapper::judge` decides on arrive verbatim from the
/// plugin's own `after_turn` response ([`crate::ObservedEvidence`]). A plugin
/// reporting a favourable number it never earned was believed, on the strength
/// of a sentence that was not true (#3511).
///
/// The maintainer settled that as Option 2 on 2026-08-17: the manifest stops
/// claiming the oracle is host-run, and the consent text says plainly that the
/// plugin reports its own evidence. The block stays because it is still the
/// author's declaration of *what will run* — it is what a user is shown at
/// install, and it is the field a host that later executes the oracle itself
/// would read — but it declares an intent, never a host-enforced fact.
///
/// # What is still structural, and what is not
///
/// Unchanged: `judge` is synchronous, I/O-free and total, so "a verification
/// plugin quietly calls a model to decide done" remains impossible by
/// construction; the *rule* is the manifest's and only the host evaluates it;
/// a check conjoins with the flip and can only narrow done (#3510); and the
/// tamper finding is host-owned and not a field a plugin can write
/// ([`crate::ObservedEvidence`], #3499).
///
/// Not true, and no longer claimed here: that the evidence was **earned**.
/// Whoever consents to a verification plugin is trusting its honesty about its
/// own work, which is exactly what the install prompt now says.
///
/// Arbiter-only: the oracle exists to decide requirements, and below `arbiter`
/// there are none to decide.
///
/// Two shapes of evidence, and a manifest may declare either or both: a
/// fail→pass flip ([`FlipPolicy`]), and numbers the oracle reports which
/// declared checks compare against a budget ([`crate::OracleCheck`]). The
/// second exists because the first can only express one definition of done —
/// see `evidence.rs` for the falsifier that established that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Oracle {
    /// The argv the plugin declares it runs as its oracle. Never a shell
    /// string — the #1400 rule, same as every hook.
    ///
    /// **Declared, not dispatched.** Nothing in Stella executes this today
    /// (see the type's own doc comment and #3511); it is shown at install and
    /// is what a host that took the oracle over would read.
    ///
    /// **Optional when `[runtime]` is declared**, and absent then means "the
    /// oracle is this plugin's own process" (#3501). It was mandatory until
    /// Track C built one plugin three times and every one of them wrote its
    /// `[runtime].argv` out a second time here, byte for byte: a grammar that
    /// forces a redundant declaration teaches every author a redundant
    /// concept, and it made three manifests differ in four lines where two
    /// would do. [`PluginManifest::oracle_process`] is the resolved answer, so
    /// a host never has to know which of the two shapes was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<OracleCommand>,
    /// Whether the host must observe a fail→pass flip before crediting.
    pub flip: FlipPolicy,
    /// How the host detects tampering with the witness artifacts.
    ///
    /// Defaulted rather than mandatory since #3499. Tamper snapshotting is
    /// host-side (`doc:pipeline-as-plugins` §4 A10) and there is exactly one
    /// policy, so a manifest restating it said nothing a host did not already
    /// know — and a `flip = "not-applicable"` oracle, which has no flip to
    /// protect, still had to write the line. Declaring it explicitly remains
    /// legal and is how a future second policy will be selected.
    #[serde(default)]
    pub tamper: TamperPolicy,
    /// The names of the numbers this oracle reports. Non-blank and unique; a
    /// check may only read a name declared here, which is the evidence half
    /// of "a rule reading something nothing publishes is a load error".
    ///
    /// A declared measurement no check reads is allowed: reporting a number
    /// for the trace is legitimate, and only *deciding* on an undeclared one
    /// is the silence this crate refuses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub measurements: Vec<String>,
    /// The `[[oracle.checks]]` entries — the verdict rule, as data.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<OracleCheck>,
}

/// The process an oracle runs as, with the host-enforced bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OracleCommand {
    /// Program and arguments. `${plugin_dir}` interpolation is the host's
    /// concern; this crate only requires the list to be non-empty.
    pub argv: Vec<String>,
    /// Seconds the oracle is allowed before it is killed. Must be at least
    /// 1 — a zero timeout kills the oracle before it runs. Enforced by
    /// whoever runs the program, which today is the plugin (#3511).
    pub timeout_secs: u64,
}

/// The program declared as this manifest's oracle, with the declaration it
/// came from.
///
/// [`Oracle::command`] and [`Runtime`] are two ways to name one program, and a
/// reader must not have to know which one an author chose —
/// [`PluginManifest::oracle_process`] resolves it once. Its one shipped caller
/// is [`crate::consent_text`], which names the program at install; nothing runs
/// it (#3511). Borrowed rather than owned so resolving costs nothing;
/// `${plugin_dir}` interpolation stays the host's job, exactly as it is for
/// either declaration on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OracleProcess<'a> {
    /// Program and arguments.
    pub argv: &'a [String],
    /// Seconds it is allowed before it is killed.
    pub timeout_secs: u64,
    /// Which block named it — the one thing a caller may legitimately want to
    /// distinguish, because "runs a program of its own" and "runs itself
    /// again" are different sentences at an install prompt.
    pub source: OracleProcessSource,
}

/// Which declaration an [`OracleProcess`] was resolved from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleProcessSource {
    /// `[oracle] command` named its own program.
    OracleCommand,
    /// `[oracle]` named none, so the oracle is the plugin's own `[runtime]`
    /// process.
    Runtime,
}

/// Whether a fail→pass flip is required before the oracle's requirement is
/// credited. Closed, so an unknown value is a load error rather than a
/// silently weaker contract; a further relaxation adds a variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FlipPolicy {
    /// The host credits the oracle only on a fail-before / pass-after flip.
    ///
    /// The flip is the one the **plugin reports** having seen
    /// ([`ObservedEvidence::flip`](crate::ObservedEvidence)); the host does not
    /// watch it happen (#3511). What the host does with the report is still
    /// its own: this policy conjoins with every declared check, so a check can
    /// only narrow done and never stand in for the flip (#3510).
    Required,
    /// This oracle's evidence is not a flip: its measurements are what decide
    /// its requirements. A performance budget is the reference case — the
    /// benchmark passes before and after, and what changed is a number
    /// (`doc:pipeline-as-plugins` §6.1).
    ///
    /// **Not a weaker contract.** With no flip to decide anything, every
    /// requirement must be decided by a declared check or the manifest is
    /// refused ([`ManifestError::UndecidableRequirement`]), so this trades
    /// one host-evaluated rule for another rather than dropping one.
    NotApplicable,
}

/// How the host detects witness-artifact tampering. One variant today, for
/// the same reason as [`FlipPolicy`].
///
/// **This names what the *host* does, not what the plugin does.** Snapshotting
/// artifact identity is host-side by design (`doc:pipeline-as-plugins` §4 A10),
/// which is why the finding it produces — [`TamperFinding`](crate::TamperFinding)
/// — is not part of what a plugin may report: an
/// [`ObservedEvidence`](crate::ObservedEvidence) has no field for it, and the
/// host merges its own answer in before `judge` runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TamperPolicy {
    /// The host snapshots artifact identity at authoring time and refuses
    /// the flip if it changed by verify time — the `witness.rs` discipline.
    ///
    /// The default, because it is the only thing a host does: a manifest
    /// declaring nothing here is asking for the check every host performs, not
    /// opting out of one.
    #[default]
    #[serde(rename = "artifact-identity")]
    ArtifactIdentity,
}

/// The `[subloop]` block — stages the host runs as bounded child turns over
/// the sub-agent primitive. Order is declared; budgets are carved and
/// reports capped by the host's child-turn bounds, not the plugin's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subloop {
    /// The stage names, in execution order. Non-empty, no duplicates.
    pub stages: Vec<String>,
}

/// One `[roles.<name>]` entry — a routing *intent*, never a credential or a
/// URL. The host resolves the tier against the user's BYOK providers
/// (parity invariant 8), soft-failing to the session default with a notice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Role {
    /// The intent the host resolves — `"cheap"` in #3245's examples. An
    /// open vocabulary by design (the roster shape): the host, not the
    /// manifest, decides what a tier maps to, so this crate only requires
    /// it to be non-empty.
    pub tier: String,
}

/// A parsed, validated plugin manifest — the new blocks of #3245 §2.
///
/// Construct through [`PluginManifest::from_toml_str`]; a value that came
/// from it has passed every cross-field rule in this module. The platform
/// fields #1400 specifies (engine-compat range, lifecycle, overlay) are
/// imported by reference there and join this struct when their slices land —
/// `deny_unknown_fields` means a manifest using them before then fails
/// loudly instead of half-loading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    /// The plugin's identity — what every grant, deck chip, and hold
    /// attribution hangs off. Non-empty.
    pub name: String,
    /// One line for humans; shown at install consent and in `stella app
    /// list`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The `[loop]` registration. Absent = no participation.
    #[serde(rename = "loop", default, skip_serializing_if = "is_default_grant")]
    pub loop_grant: LoopGrant,
    /// Arbiter only: the enumerable definition of done. Keys are the names
    /// a hold cites; values are the human-readable statement of each
    /// requirement. A `BTreeMap` so iteration order is deterministic
    /// (invariant 7's discipline — anything that reaches a prompt or a
    /// journal must not depend on hash order).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requirements: Option<BTreeMap<String, String>>,
    /// Arbiter only: the oracle the plugin declares it runs, and the rule the
    /// host evaluates against what it reports back (#3511).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oracle: Option<Oracle>,
    /// Steering and above: declared stages run as bounded child turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subloop: Option<Subloop>,
    /// Routing intents for subloop stages. Requires `[subloop]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roles: Option<BTreeMap<String, Role>>,
    /// Steering and above: the wrapper's stage order and the conditions
    /// under which each stage runs (#3381). Absent = this plugin is not a
    /// turn-loop wrapper.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapper: Option<Wrapper>,
    /// Observer and above: the process this plugin runs as, and the exact
    /// environment slice it inherits (`doc:pipeline-as-plugins` §A5). Absent =
    /// this plugin ships no process, so no hook grant it holds can be
    /// dispatched anywhere.
    ///
    /// Deliberately **not** gated on `steering` even though only a steering
    /// grade reaches a hook point: an `observer` is invoked too — it receives
    /// the turn event stream — and that invocation is a process start like any
    /// other. See [`Runtime`] for why there is no `language` field beside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<Runtime>,
    /// What the plugin asks to reach *outside* the turn — the tools it wants,
    /// each graded in the gate's own [`stella_protocol::RiskLevel`], with the
    /// reason a human reads at install (`doc:pipeline-as-plugins` §A1).
    ///
    /// Deliberately gated on **no** participation grade. The `[loop]` ladder
    /// governs a plugin's say in the turn; this governs what it may touch,
    /// and the two are orthogonal: a `none`-grade content bundle shipping one
    /// custom tool that runs `git push` is asking for more of the world than
    /// an `observer` that only watches. Tying the capability list to the
    /// ladder would let the widest grant hide behind the weakest grade.
    ///
    /// Absent = it asks for nothing, which [`crate::consent_text`] says in
    /// those words rather than by omission.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<Capability>,
    /// The script tools this package ships into the agent's tool surface
    /// (#3565). See [`ToolContribution`] and [`crate::package`] for why these
    /// three tables declare *names* rather than paths, and why the host's own
    /// read is checked against them by [`PluginManifest::reconcile`].
    ///
    /// Gated on **no** participation grade, for [`Self::capabilities`]'
    /// reason exactly: the ladder governs a plugin's say in the turn, and a
    /// `none`-grade content bundle shipping one tool that runs `git push` is
    /// asking for more of the world than an `observer` that only watches.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolContribution>,
    /// The skills this package ships (#3565).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<SkillContribution>,
    /// The context records this package ships (#3565). Advisory only — see
    /// [`crate::RecordEnforcement`] for the governance argument.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<RecordContribution>,
}

/// `skip_serializing_if` needs a named predicate; "the grant is the default"
/// is exactly "no `[loop]` block was written", so omitted and default
/// round-trip to the same TOML.
fn is_default_grant(grant: &LoopGrant) -> bool {
    *grant == LoopGrant::default()
}

impl PluginManifest {
    /// Parse and validate a manifest from TOML text.
    ///
    /// The only constructor that vouches for a manifest: parsing enforces
    /// the shape rules (unknown keys, unknown hook names, unknown grades all
    /// fail here) and validation enforces the cross-field rules (each one
    /// documented on its [`ManifestError`] variant). Pure — no I/O, no
    /// environment.
    pub fn from_toml_str(text: &str) -> Result<Self, ManifestError> {
        let manifest: PluginManifest = toml::from_str(text)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// The program this manifest declares as its oracle, resolved across the
    /// two declarations that can name it. Declared, not dispatched — see
    /// [`Oracle`] and #3511.
    ///
    /// `None` when the manifest declares no `[oracle]` at all. For a manifest
    /// that came from [`PluginManifest::from_toml_str`] the converse holds: a
    /// declared oracle always resolves, because validation refuses an oracle
    /// that names neither a `command` nor a `[runtime]` to be
    /// ([`ManifestError::OracleCommandRequired`]). A hand-built manifest can
    /// still answer `None` with an oracle present, which is why this returns an
    /// `Option` rather than asserting.
    #[must_use]
    pub fn oracle_process(&self) -> Option<OracleProcess<'_>> {
        let oracle = self.oracle.as_ref()?;
        match &oracle.command {
            Some(command) => Some(OracleProcess {
                argv: &command.argv,
                timeout_secs: command.timeout_secs,
                source: OracleProcessSource::OracleCommand,
            }),
            None => self.runtime.as_ref().map(|runtime| OracleProcess {
                argv: &runtime.argv,
                timeout_secs: runtime.timeout_secs,
                source: OracleProcessSource::Runtime,
            }),
        }
    }

    /// The cross-field rules, separated from parsing so each check can say
    /// *why* in its own error rather than a deserializer's.
    fn validate(&self) -> Result<(), ManifestError> {
        if self.name.trim().is_empty() {
            return Err(ManifestError::EmptyName);
        }

        let grant = &self.loop_grant;
        let participation = grant.participation;

        // A set rather than a prefix scan, so a long declaration stays linear.
        // `insert` returning false on the first repeat *is* the prefix scan's
        // answer — the earliest element that some earlier element equals — so
        // which duplicate gets named is unchanged.
        let mut seen_hooks = HashSet::with_capacity(grant.hooks.len());
        for hook in &grant.hooks {
            if !seen_hooks.insert(*hook) {
                return Err(ManifestError::DuplicateHook { hook: *hook });
            }
        }
        if !grant.hooks.is_empty() && !participation.includes(Participation::Steering) {
            return Err(ManifestError::HooksRequireSteering { participation });
        }

        // The points get the identical treatment the hooks just had, because
        // they are the identical rule for the other dispatch surface.
        let mut seen_points = HashSet::with_capacity(grant.points.len());
        for point in &grant.points {
            if !seen_points.insert(*point) {
                return Err(ManifestError::DuplicatePoint { point: *point });
            }
        }
        if !grant.points.is_empty() && !participation.includes(Participation::Steering) {
            return Err(ManifestError::PointsRequireSteering { participation });
        }

        // And the calls get it a third time, because "declared, deduplicated,
        // and above the grade that grants it" is the same rule for every
        // dispatch surface — the host-call channel included.
        let mut seen_calls = HashSet::with_capacity(grant.calls.len());
        for call in &grant.calls {
            if !seen_calls.insert(*call) {
                return Err(ManifestError::DuplicateCall { call: *call });
            }
        }
        if !grant.calls.is_empty() {
            if !participation.includes(Participation::Steering) {
                return Err(ManifestError::CallsRequireSteering { participation });
            }
            // A call happens *during* a point. Declaring one with no point to
            // make it from is a manifest that quietly does nothing, which this
            // crate refuses on principle rather than leaving to be discovered
            // as a silence at run time.
            if grant.points.is_empty() {
                return Err(ManifestError::CallsRequirePoints);
            }
        }
        match grant.max_calls {
            Some(_) if grant.calls.is_empty() => {
                return Err(ManifestError::MaxCallsRequiresCalls);
            }
            // Zero is not "ask for none" — that is an empty `calls` list. It is
            // a declaration that contradicts itself, and the `max_holds` rule
            // for the same shape one rung up.
            Some(0) => return Err(ManifestError::ZeroMaxCalls),
            _ => {}
        }

        if grant.hooks.contains(&HookEvent::Stop) && !participation.includes(Participation::Arbiter)
        {
            return Err(ManifestError::StopHookRequiresArbiter { participation });
        }
        if participation == Participation::Arbiter && !grant.hooks.contains(&HookEvent::Stop) {
            return Err(ManifestError::ArbiterMustDeclareStop);
        }

        match grant.max_holds {
            Some(_) if participation != Participation::Arbiter => {
                return Err(ManifestError::MaxHoldsRequiresArbiter { participation });
            }
            Some(0) => return Err(ManifestError::ZeroMaxHolds),
            _ => {}
        }

        match (&self.requirements, participation) {
            (Some(_), p) if p != Participation::Arbiter => {
                return Err(ManifestError::RequirementsRequireArbiter { participation: p });
            }
            // The guard above already returned for every grade below
            // arbiter, so this arm only ever sees an arbiter's table.
            (Some(requirements), _) => {
                if requirements.is_empty() {
                    return Err(ManifestError::ArbiterRequiresRequirements);
                }
                for (name, description) in requirements {
                    if description.trim().is_empty() {
                        return Err(ManifestError::EmptyRequirement { name: name.clone() });
                    }
                }
            }
            (None, Participation::Arbiter) => {
                return Err(ManifestError::ArbiterRequiresRequirements);
            }
            (None, _) => {}
        }

        if let Some(oracle) = &self.oracle {
            if participation != Participation::Arbiter {
                return Err(ManifestError::OracleRequiresArbiter { participation });
            }
            match &oracle.command {
                Some(command) => {
                    if command.argv.is_empty() {
                        return Err(ManifestError::EmptyOracleArgv);
                    }
                    if command.timeout_secs == 0 {
                        return Err(ManifestError::ZeroOracleTimeout);
                    }
                }
                // No command of its own means the oracle is the plugin's own
                // process, so there must be one. `[runtime]`'s own argv and
                // timeout bounds are checked below, where they are declared.
                None if self.runtime.is_none() => {
                    return Err(ManifestError::OracleCommandRequired);
                }
                None => {}
            }
            if !grant.points.contains(&WrapperPoint::AfterTurn) {
                return Err(ManifestError::OracleRequiresAfterTurn);
            }
            oracle.validate_evidence(self.requirements.as_ref())?;
        }

        if let Some(subloop) = &self.subloop {
            if !participation.includes(Participation::Steering) {
                return Err(ManifestError::SubloopRequiresSteering { participation });
            }
            if subloop.stages.is_empty() {
                return Err(ManifestError::EmptyStages);
            }
            // Same set-instead-of-prefix-scan as the hooks above. The two
            // checks stay interleaved in one pass on purpose: hoisting the
            // blank check into a pass of its own would re-order the two
            // errors for a list that contains both.
            let mut seen_stages = HashSet::with_capacity(subloop.stages.len());
            for stage in &subloop.stages {
                if stage.trim().is_empty() {
                    return Err(ManifestError::EmptyStageName);
                }
                if !seen_stages.insert(stage) {
                    return Err(ManifestError::DuplicateStage {
                        stage: stage.clone(),
                    });
                }
            }
        }

        if let Some(wrapper) = &self.wrapper {
            if !participation.includes(Participation::Steering) {
                return Err(ManifestError::WrapperRequiresSteering { participation });
            }
            wrapper.validate()?;
        }

        if let Some(runtime) = &self.runtime {
            if !participation.includes(Participation::Observer) {
                return Err(ManifestError::RuntimeRequiresObserver { participation });
            }
            runtime.validate()?;
        }

        if let Some(roles) = &self.roles {
            if self.subloop.is_none() {
                return Err(ManifestError::RolesRequireSubloop);
            }
            for (name, role) in roles {
                if role.tier.trim().is_empty() {
                    return Err(ManifestError::EmptyRoleTier { name: name.clone() });
                }
            }
        }

        validate_capabilities(&self.capabilities)?;
        validate_contributions(self)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<PluginManifest, ManifestError> {
        PluginManifest::from_toml_str(text)
    }

    #[test]
    fn undeclared_loop_block_is_grade_none_with_no_hooks() {
        let m = parse("name = \"bundle\"").unwrap();
        assert_eq!(m.loop_grant.participation, Participation::None);
        assert!(m.loop_grant.hooks.is_empty());
    }

    #[test]
    fn the_ladder_is_monotone_and_every_grade_includes_itself() {
        use Participation::*;
        let ladder = [None, Observer, Steering, Arbiter];
        for (i, higher) in ladder.iter().enumerate() {
            for (j, lower) in ladder.iter().enumerate() {
                assert_eq!(higher.includes(*lower), i >= j, "{higher} vs {lower}");
            }
        }
    }

    #[test]
    fn unknown_top_level_key_is_a_load_error() {
        let err = parse("name = \"x\"\nsurprise = 1").unwrap_err();
        assert!(matches!(err, ManifestError::Parse(_)));
    }

    #[test]
    fn unknown_hook_name_is_a_load_error() {
        let err = parse(
            "name = \"x\"\n[loop]\nparticipation = \"steering\"\nhooks = [\"PostToolUse\", \"OnMerge\"]",
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::Parse(_)));
    }

    #[test]
    fn unknown_grade_is_a_load_error() {
        let err = parse("name = \"x\"\n[loop]\nparticipation = \"root\"").unwrap_err();
        assert!(matches!(err, ManifestError::Parse(_)));
    }

    #[test]
    fn duplicate_hooks_are_rejected() {
        let err = parse(
            "name = \"x\"\n[loop]\nparticipation = \"steering\"\nhooks = [\"PreToolUse\", \"PreToolUse\"]",
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ManifestError::DuplicateHook {
                hook: HookEvent::PreToolUse
            }
        ));
    }

    /// The set-based duplicate checks must name the same offender the prefix
    /// scan they replaced did — the *first element that repeats an earlier
    /// one*, in declaration order — and must keep the blank-stage check
    /// interleaved with them. Both are order-sensitive, so both are pinned
    /// by a list that would answer differently under a re-ordered pass.
    #[test]
    fn the_first_repeat_in_declaration_order_is_the_one_reported() {
        let hooks = parse(
            "name = \"x\"\n[loop]\nparticipation = \"steering\"\nhooks = [\"PreToolUse\", \"PostToolUse\", \"PostToolUse\", \"PreToolUse\"]",
        )
        .unwrap_err();
        assert!(
            matches!(
                hooks,
                ManifestError::DuplicateHook {
                    hook: HookEvent::PostToolUse
                }
            ),
            "the earlier-repeating PreToolUse pair must not preempt the \
             PostToolUse repeat that occurs first, got {hooks:?}"
        );

        let head = "name = \"x\"\n[loop]\nparticipation = \"steering\"\n\n[subloop]\n";

        // A duplicate before a blank: the duplicate wins, which only holds
        // while the two checks share one pass.
        let dupe_first = parse(&format!("{head}stages = [\"plan\", \"plan\", \" \"]")).unwrap_err();
        assert!(
            matches!(dupe_first, ManifestError::DuplicateStage { ref stage } if stage == "plan"),
            "got {dupe_first:?}"
        );

        // A blank before a duplicate: the blank wins, for the same reason.
        let blank_first =
            parse(&format!("{head}stages = [\"plan\", \" \", \"plan\"]")).unwrap_err();
        assert!(
            matches!(blank_first, ManifestError::EmptyStageName),
            "got {blank_first:?}"
        );
    }

    /// The casing split documented on [`HookEvent`] is a decision, so it is
    /// pinned: `HookEvent` mirrors the PascalCase a user already types in
    /// `.stella/settings.json`, `Participation` is lowercase because this
    /// crate coined it. A `rename_all` added to either — the tidying this
    /// spelling invites — fails here rather than silently invalidating every
    /// shipped manifest.
    #[test]
    fn wire_strings_are_pinned_on_both_sides() {
        for (hook, wire) in [
            (HookEvent::SessionStart, "SessionStart"),
            (HookEvent::PreToolUse, "PreToolUse"),
            (HookEvent::PostToolUse, "PostToolUse"),
            (HookEvent::Stop, "Stop"),
            (HookEvent::PreCompact, "PreCompact"),
        ] {
            assert_eq!(serde_json::to_value(hook).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<HookEvent>(wire.into()).unwrap(),
                hook
            );
            assert_eq!(hook.to_string(), wire, "Display must match the wire string");
        }

        for (grade, wire) in [
            (Participation::None, "none"),
            (Participation::Observer, "observer"),
            (Participation::Steering, "steering"),
            (Participation::Arbiter, "arbiter"),
        ] {
            assert_eq!(serde_json::to_value(grade).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<Participation>(wire.into()).unwrap(),
                grade
            );
            assert_eq!(
                grade.to_string(),
                wire,
                "Display must match the wire string"
            );
        }
    }

    #[test]
    fn observer_may_not_declare_hooks() {
        let err =
            parse("name = \"x\"\n[loop]\nparticipation = \"observer\"\nhooks = [\"PostToolUse\"]")
                .unwrap_err();
        assert!(matches!(err, ManifestError::HooksRequireSteering { .. }));
    }

    #[test]
    fn steering_may_not_declare_the_stop_hook() {
        let err = parse("name = \"x\"\n[loop]\nparticipation = \"steering\"\nhooks = [\"Stop\"]")
            .unwrap_err();
        assert!(matches!(err, ManifestError::StopHookRequiresArbiter { .. }));
    }

    #[test]
    fn an_arbiter_must_declare_stop() {
        let err = parse(
            "name = \"x\"\n[loop]\nparticipation = \"arbiter\"\nhooks = [\"PreToolUse\"]\n\n[requirements]\nr = \"a requirement\"",
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::ArbiterMustDeclareStop));
    }

    #[test]
    fn max_holds_below_arbiter_is_rejected_and_zero_is_rejected_at_arbiter() {
        let below =
            parse("name = \"x\"\n[loop]\nparticipation = \"steering\"\nmax_holds = 2").unwrap_err();
        assert!(matches!(
            below,
            ManifestError::MaxHoldsRequiresArbiter { .. }
        ));

        let zero = parse(
            "name = \"x\"\n[loop]\nparticipation = \"arbiter\"\nhooks = [\"Stop\"]\nmax_holds = 0\n\n[requirements]\nr = \"a requirement\"",
        )
        .unwrap_err();
        assert!(matches!(zero, ManifestError::ZeroMaxHolds));
    }

    #[test]
    fn requirements_below_arbiter_are_rejected() {
        let err = parse(
            "name = \"x\"\n[loop]\nparticipation = \"steering\"\n\n[requirements]\nr = \"a requirement\"",
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ManifestError::RequirementsRequireArbiter { .. }
        ));
    }

    #[test]
    fn an_arbiter_without_requirements_is_rejected_in_both_shapes() {
        let absent = parse("name = \"x\"\n[loop]\nparticipation = \"arbiter\"\nhooks = [\"Stop\"]")
            .unwrap_err();
        assert!(matches!(absent, ManifestError::ArbiterRequiresRequirements));

        let empty = parse(
            "name = \"x\"\n[loop]\nparticipation = \"arbiter\"\nhooks = [\"Stop\"]\n\n[requirements]",
        )
        .unwrap_err();
        assert!(matches!(empty, ManifestError::ArbiterRequiresRequirements));
    }

    #[test]
    fn an_empty_requirement_description_is_rejected() {
        let err = parse(
            "name = \"x\"\n[loop]\nparticipation = \"arbiter\"\nhooks = [\"Stop\"]\n\n[requirements]\nr = \"  \"",
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::EmptyRequirement { .. }));
    }

    #[test]
    fn an_oracle_below_arbiter_is_rejected() {
        let err = parse(
            "name = \"x\"\n[loop]\nparticipation = \"steering\"\n\n[oracle]\ncommand = { argv = [\"oracle\"], timeout_secs = 10 }\nflip = \"required\"\ntamper = \"artifact-identity\"",
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::OracleRequiresArbiter { .. }));
    }

    #[test]
    fn oracle_argv_and_timeout_bounds_are_enforced() {
        let head = "name = \"x\"\n[loop]\nparticipation = \"arbiter\"\nhooks = [\"Stop\"]\n\n[requirements]\nr = \"a requirement\"\n\n[oracle]\n";
        let tail = "flip = \"required\"\ntamper = \"artifact-identity\"";

        let empty_argv = parse(&format!(
            "{head}command = {{ argv = [], timeout_secs = 10 }}\n{tail}"
        ))
        .unwrap_err();
        assert!(matches!(empty_argv, ManifestError::EmptyOracleArgv));

        let zero_timeout = parse(&format!(
            "{head}command = {{ argv = [\"o\"], timeout_secs = 0 }}\n{tail}"
        ))
        .unwrap_err();
        assert!(matches!(zero_timeout, ManifestError::ZeroOracleTimeout));
    }

    #[test]
    fn an_unknown_flip_or_tamper_value_is_a_load_error() {
        let err = parse(
            "name = \"x\"\n[loop]\nparticipation = \"arbiter\"\nhooks = [\"Stop\"]\n\n[requirements]\nr = \"a requirement\"\n\n[oracle]\ncommand = { argv = [\"o\"], timeout_secs = 10 }\nflip = \"optional\"\ntamper = \"artifact-identity\"",
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::Parse(_)));
    }

    /// The flip vocabulary's wire strings, pinned on both sides. `kebab-case`
    /// replaced `lowercase` when `not-applicable` joined the enum, which is
    /// invisible for `Required` and would silently rename any future variant
    /// spelled in two words — every shipped manifest naming it would stop
    /// loading. Pinning both spellings makes that a red test instead.
    #[test]
    fn flip_policy_wire_strings_are_pinned() {
        for (policy, wire) in [
            (FlipPolicy::Required, "required"),
            (FlipPolicy::NotApplicable, "not-applicable"),
        ] {
            assert_eq!(serde_json::to_value(policy).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<FlipPolicy>(wire.into()).unwrap(),
                policy
            );
        }
    }

    #[test]
    fn a_subloop_below_steering_is_rejected() {
        let err = parse(
            "name = \"x\"\n[loop]\nparticipation = \"observer\"\n\n[subloop]\nstages = [\"triage\"]",
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::SubloopRequiresSteering { .. }));
    }

    #[test]
    fn subloop_stage_lists_are_validated() {
        let head = "name = \"x\"\n[loop]\nparticipation = \"steering\"\n\n[subloop]\n";

        let empty = parse(&format!("{head}stages = []")).unwrap_err();
        assert!(matches!(empty, ManifestError::EmptyStages));

        let dupe = parse(&format!("{head}stages = [\"plan\", \"plan\"]")).unwrap_err();
        assert!(matches!(dupe, ManifestError::DuplicateStage { .. }));

        let blank = parse(&format!("{head}stages = [\"plan\", \" \"]")).unwrap_err();
        assert!(matches!(blank, ManifestError::EmptyStageName));
    }

    #[test]
    fn roles_without_a_subloop_are_rejected_and_tiers_must_be_non_empty() {
        let orphaned = parse(
            "name = \"x\"\n[loop]\nparticipation = \"steering\"\n\n[roles.triage]\ntier = \"cheap\"",
        )
        .unwrap_err();
        assert!(matches!(orphaned, ManifestError::RolesRequireSubloop));

        let blank_tier = parse(
            "name = \"x\"\n[loop]\nparticipation = \"steering\"\n\n[subloop]\nstages = [\"triage\"]\n\n[roles.triage]\ntier = \"\"",
        )
        .unwrap_err();
        assert!(matches!(blank_tier, ManifestError::EmptyRoleTier { .. }));
    }

    #[test]
    fn an_empty_name_is_rejected() {
        let err = parse("name = \" \"").unwrap_err();
        assert!(matches!(err, ManifestError::EmptyName));
    }

    #[test]
    fn permits_hook_is_declared_and_graded_even_on_a_hand_built_grant() {
        // Through the constructor: only the declared hooks pass.
        let m =
            parse("name = \"x\"\n[loop]\nparticipation = \"steering\"\nhooks = [\"PreToolUse\"]")
                .unwrap();
        assert!(m.loop_grant.permits_hook(HookEvent::PreToolUse));
        assert!(!m.loop_grant.permits_hook(HookEvent::PostToolUse));
        assert!(!m.loop_grant.permits_hook(HookEvent::Stop));

        // Hand-built below steering with hooks smuggled in: still filtered.
        let smuggled = LoopGrant {
            participation: Participation::Observer,
            hooks: vec![HookEvent::PreToolUse],
            points: vec![WrapperPoint::BeforeTurn],
            calls: vec![HostCall::Recall],
            max_calls: None,
            max_holds: None,
        };
        assert!(!smuggled.permits_hook(HookEvent::PreToolUse));
        assert!(!smuggled.permits_point(WrapperPoint::BeforeTurn));
        assert!(!smuggled.permits_call(HostCall::Recall));
    }

    /// **Witness for #3501 item 2.** A manifest declares the socket points it
    /// implements, and a point it did not declare is never dispatched — the
    /// filter [`LoopGrant::permits_hook`] already is for hooks. Before this,
    /// `[loop]` could not express the answer at all, so a host learned that a
    /// wrapper refuses `before_turn` by asking and being refused at run time.
    #[test]
    fn an_undeclared_point_is_never_dispatched() {
        let m =
            parse("name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"after_turn\"]")
                .expect("a declared point set must load");
        assert_eq!(m.loop_grant.points, vec![WrapperPoint::AfterTurn]);
        assert!(m.loop_grant.permits_point(WrapperPoint::AfterTurn));
        assert!(
            !m.loop_grant.permits_point(WrapperPoint::BeforeTurn),
            "before_turn was never declared, so it is never dispatched"
        );

        // Undeclared entirely: a plugin that answers nowhere.
        let silent = parse("name = \"x\"\n[loop]\nparticipation = \"steering\"").unwrap();
        assert!(silent.loop_grant.points.is_empty());
        for point in [WrapperPoint::BeforeTurn, WrapperPoint::AfterTurn] {
            assert!(!silent.loop_grant.permits_point(point));
        }
    }

    #[test]
    fn point_declarations_are_graded_and_deduplicated_like_hooks() {
        let below =
            parse("name = \"x\"\n[loop]\nparticipation = \"observer\"\npoints = [\"before_turn\"]")
                .unwrap_err();
        assert!(matches!(below, ManifestError::PointsRequireSteering { .. }));

        let dupe = parse(
            "name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"after_turn\", \"after_turn\"]",
        )
        .unwrap_err();
        assert!(matches!(
            dupe,
            ManifestError::DuplicatePoint {
                point: WrapperPoint::AfterTurn
            }
        ));

        let unknown =
            parse("name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"judge\"]")
                .unwrap_err();
        assert!(
            matches!(unknown, ManifestError::Parse(_)),
            "`judge` is a host function, not a point a plugin can answer; got {unknown:?}"
        );
    }

    /// **Witness for #3540.** A manifest declares the host capabilities it may
    /// ask for, and one it did not declare is refused — the filter
    /// [`LoopGrant::permits_hook`] already is for hooks and
    /// [`LoopGrant::permits_point`] is for points. Before this the `[loop]`
    /// block could not express the answer at all, because there was nothing on
    /// the wire to ask with.
    #[test]
    fn an_undeclared_host_call_is_never_performed() {
        let m = parse(
            "name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"before_turn\"]\ncalls = [\"recall\"]",
        )
        .expect("a declared call set must load");
        assert_eq!(m.loop_grant.calls, vec![HostCall::Recall]);
        assert!(m.loop_grant.permits_call(HostCall::Recall));
        assert!(
            !m.loop_grant.permits_call(HostCall::ChildTurn),
            "child_turn was never declared, so the host never performs it"
        );

        // Undeclared entirely: a plugin that asks for nothing.
        let silent =
            parse("name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"before_turn\"]")
                .unwrap();
        assert!(silent.loop_grant.calls.is_empty());
        for call in [HostCall::Recall, HostCall::ChildTurn, HostCall::RunTest] {
            assert!(!silent.loop_grant.permits_call(call));
        }
    }

    #[test]
    fn call_declarations_are_graded_and_deduplicated_like_hooks() {
        let below =
            parse("name = \"x\"\n[loop]\nparticipation = \"observer\"\ncalls = [\"recall\"]")
                .unwrap_err();
        assert!(matches!(below, ManifestError::CallsRequireSteering { .. }));

        let dupe = parse(
            "name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"before_turn\"]\ncalls = [\"recall\", \"recall\"]",
        )
        .unwrap_err();
        assert!(matches!(
            dupe,
            ManifestError::DuplicateCall {
                call: HostCall::Recall
            }
        ));

        let unknown = parse(
            "name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"before_turn\"]\ncalls = [\"read_file\"]",
        )
        .unwrap_err();
        assert!(
            matches!(unknown, ManifestError::Parse(_)),
            "the capability set is closed, not an RPC surface; got {unknown:?}"
        );
    }

    /// The allowance is an ask with a shape: it needs calls to bound, and zero
    /// contradicts the calls it is declared beside. And a call with no point to
    /// make it from is the manifest that quietly does nothing.
    #[test]
    fn the_host_call_allowance_must_be_a_coherent_ask() {
        let orphan_calls =
            parse("name = \"x\"\n[loop]\nparticipation = \"steering\"\ncalls = [\"recall\"]")
                .unwrap_err();
        assert!(matches!(orphan_calls, ManifestError::CallsRequirePoints));

        let orphan_allowance =
            parse("name = \"x\"\n[loop]\nparticipation = \"steering\"\nmax_calls = 4").unwrap_err();
        assert!(matches!(
            orphan_allowance,
            ManifestError::MaxCallsRequiresCalls
        ));

        let zero = parse(
            "name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"before_turn\"]\ncalls = [\"recall\"]\nmax_calls = 0",
        )
        .unwrap_err();
        assert!(matches!(zero, ManifestError::ZeroMaxCalls));

        let asked = parse(
            "name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"before_turn\"]\ncalls = [\"recall\"]\nmax_calls = 4",
        )
        .expect("a coherent ask loads");
        assert_eq!(asked.loop_grant.max_calls, Some(4));
    }

    /// **Witness for #3501 item 1.** The oracle may be the plugin's own
    /// process, so a manifest declaring `[runtime]` no longer writes the same
    /// argv twice — and the resolver answers the same program either way.
    #[test]
    fn an_oracle_without_a_command_is_the_plugins_own_process() {
        let head = "name = \"x\"\n[loop]\nparticipation = \"arbiter\"\nhooks = [\"Stop\"]\npoints = [\"after_turn\"]\n\n[requirements]\nr = \"a requirement\"\n\n[oracle]\nflip = \"required\"\n";
        let runtime =
            "\n[runtime]\nargv = [\"python3\", \"${plugin_dir}/main.py\"]\ntimeout_secs = 30\n";

        let same_process = parse(&format!("{head}{runtime}")).expect(
            "an [oracle] with no command must load when [runtime] declares the process it is",
        );
        let resolved = same_process
            .oracle_process()
            .expect("the oracle resolves to the runtime's process");
        assert_eq!(resolved.argv, ["python3", "${plugin_dir}/main.py"]);
        assert_eq!(resolved.timeout_secs, 30);
        assert_eq!(resolved.source, OracleProcessSource::Runtime);

        // A command of its own still wins, and still says so.
        let own = parse(&format!(
            "{head}command = {{ argv = [\"oracle\"], timeout_secs = 10 }}\n{runtime}"
        ))
        .expect("a declared command must still load");
        let resolved = own.oracle_process().expect("the declared command resolves");
        assert_eq!(resolved.argv, ["oracle"]);
        assert_eq!(resolved.timeout_secs, 10);
        assert_eq!(resolved.source, OracleProcessSource::OracleCommand);

        // Neither: there is no program to run, and a manifest that names none
        // is refused rather than loaded into a host that would find out later.
        let neither = parse(head).unwrap_err();
        assert!(matches!(neither, ManifestError::OracleCommandRequired));
    }

    /// An `[oracle]` whose evidence can never arrive is the undecidable
    /// contract #3499 named, one level up: the evidence rides on the
    /// `after_turn` response, and an undeclared point is never dispatched.
    #[test]
    fn an_oracle_must_declare_the_point_its_evidence_arrives_at() {
        let err = parse(
            "name = \"x\"\n[loop]\nparticipation = \"arbiter\"\nhooks = [\"Stop\"]\n\n[requirements]\nr = \"a requirement\"\n\n[oracle]\ncommand = { argv = [\"o\"], timeout_secs = 10 }\nflip = \"required\"",
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::OracleRequiresAfterTurn));
    }

    /// **The `[oracle] tamper` half of #3499.** The policy names what the
    /// *host* does, so a manifest no longer has to restate the only thing a
    /// host does — while a manifest that states it explicitly still loads and
    /// still means the same thing.
    #[test]
    fn the_tamper_policy_is_the_hosts_and_need_not_be_restated() {
        let head = "name = \"x\"\n[loop]\nparticipation = \"arbiter\"\nhooks = [\"Stop\"]\npoints = [\"after_turn\"]\n\n[requirements]\nr = \"a requirement\"\n\n[oracle]\ncommand = { argv = [\"o\"], timeout_secs = 10 }\nflip = \"required\"\n";

        let silent = parse(head).expect("an [oracle] with no tamper line must load");
        let oracle = silent.oracle.expect("the block must be carried");
        assert_eq!(oracle.tamper, TamperPolicy::ArtifactIdentity);

        let explicit = parse(&format!("{head}tamper = \"artifact-identity\""))
            .expect("declaring it explicitly must keep working");
        assert_eq!(
            explicit.oracle.expect("the block must be carried").tamper,
            TamperPolicy::ArtifactIdentity
        );
    }
}
