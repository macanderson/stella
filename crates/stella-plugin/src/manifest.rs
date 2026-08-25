//! The plugin manifest — participation is declared, never inferred.
//!
//! This module is slice A of #3245: the `[loop]` block and its grade ladder,
//! plus the `[requirements]`, `[subloop]` and `[roles]` blocks, parsed from
//! TOML and validated as one consent document.
//!
//! What it keeps is the manifest *as a whole*: [`PluginManifest`], and every
//! rule that reads more than one block — a grade against a block, a block
//! against another block. A rule true of one block whatever else the manifest
//! says lives with that block, which is why `[oracle]` ([`crate::oracle`]),
//! `[runtime]`, `[wrapper]`, `[driver]` and the package blocks each own a
//! module. The rules below are the epic's, restated where each is checked:
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

use crate::configure::{ConfigureEntry, validate_configure};
use crate::consent::{Capability, validate_capabilities};
use crate::driver::DriverGrant;
use crate::error::ManifestError;
use crate::host_call::HostCall;
use crate::oracle::{Oracle, OracleProcess, OracleProcessSource};
use crate::package::{
    McpContribution, RecordContribution, SkillContribution, ToolContribution,
    validate_contributions,
};
use crate::runtime::Runtime;
use crate::wire::WrapperPoint;
use crate::wrapper::{StageName, Wrapper};

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
/// **The PascalCase is required, and the casing split from
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
    /// The stages this plugin answers `before_turn` at, exhaustively.
    ///
    /// **A narrowing of [`Self::points`], not a second grant** (#3543). A
    /// `[wrapper]` declares a *stage order* — the whole pipeline it wants run,
    /// its own contributions and the ceremony it is content to leave to
    /// others — and `[loop] points` declares which sockets it answers. Neither
    /// says which of those stages it has anything to say at, so the host asked
    /// it at every one: `plugins/stella-research` contributes at exactly one
    /// stage, declares the eight-stage classic order, and paid eight `python3`
    /// starts a round to answer `{"protocol_version":1}` seven times.
    ///
    /// Empty is the default and means "every stage this program runs", so a
    /// manifest written before this field existed is unchanged — which is the
    /// only compatible reading, since the alternative would silence a shipped
    /// plugin at every stage.
    ///
    /// Validated at load against the `[wrapper]` stage order: a name no stage
    /// declares is a load error, for the reason a condition naming an
    /// unpublished signal is. Enforced by [`LoopGrant::permits_stage`], which
    /// is authoritative in [`LoopGrant::permits_point`]'s sense — an
    /// undeclared stage is never dispatched, not merely usually skipped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub before_turn_stages: Vec<StageName>,
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
    /// The most host calls **per point** the plugin asks for.
    ///
    /// **An ask, never an authority** — the [`LoopGrant::max_holds`] discipline,
    /// one layer down. A conversation can hang where a single exchange could
    /// not, so the host clamps this against its own ceiling
    /// (`stella_runtime::wrapper::DEFAULT_HOST_MAX_CALLS`) and a spent allowance
    /// refuses further calls *to the plugin* rather than killing it. Absent
    /// means "the host's ceiling", never "unbounded".
    ///
    /// Per point is the whole of it: the gate is fresh on every `before_turn` /
    /// `after_turn` dispatch, because a plugin that spent its calls researching
    /// `before_turn` must still be able to ask when `after_turn` arrives. The
    /// **whole-run** budget a `child_turn` spends against is
    /// [`Self::max_child_turns`], which is a different number for a different
    /// reason (#3839).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_calls: Option<u32>,
    /// The `child_turn`s **for the whole run** the plugin asks for.
    ///
    /// **Its own key rather than a second meaning for [`Self::max_calls`]**, and
    /// the two differ in the axis they bound rather than only in size (#3839).
    /// `max_calls` is per point conversation and resets; this never resets,
    /// because what it bounds is how much of the user's money a plugin may
    /// spend, and a per-point reading of that is no bound at all across N
    /// rounds. An arbiter-grade plugin that holds rounds open and asks once per
    /// round is the case where they diverge: `plugins/stella-goal` asks for one
    /// call per point and eight turns per run, and before this key existed the
    /// honest `max_calls = 1` capped its *second round's* verifier turn at
    /// `AllowanceSpent`.
    ///
    /// An ask, never an authority — clamped against
    /// `stella_runtime::wrapper::DEFAULT_HOST_MAX_CHILD_TURNS`.
    ///
    /// Absent falls back to [`Self::max_calls`], which is the only reading that
    /// keeps a manifest written before the split honest: that number is the one
    /// a human consented to, and the host's job is to clamp an ask down rather
    /// than to widen one nobody made. Absent on *both* means "the host's
    /// ceiling", never "unbounded".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_child_turns: Option<u32>,
    /// The widest `candidate_fanout` the plugin asks for.
    ///
    /// **Its own key rather than a second meaning for [`Self::max_calls`]**,
    /// because the two bound different things with different blast radii:
    /// `max_calls` bounds how *chatty* a plugin may be inside one point, and
    /// every call it bounds is either read-only or single-tracked; this bounds
    /// how many **writing worker turns** one call may buy, so it multiplies
    /// model spend by N. One number clamping both would mean a plugin that
    /// wanted eight recalls had asked for eight candidates (#3844).
    ///
    /// An ask, never an authority — clamped against
    /// `stella_runtime::wrapper::DEFAULT_HOST_MAX_FANOUT_WIDTH`, and absent
    /// means "the host's ceiling" rather than "unbounded".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_fanout_width: Option<u32>,
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

    /// Whether the host may dispatch this plugin at `point` **for `stage`** —
    /// [`LoopGrant::permits_point`] narrowed by [`Self::before_turn_stages`]
    /// (#3543).
    ///
    /// The point filter is checked here too, so a caller that reaches for the
    /// narrower question cannot accidentally skip the wider one: this is a
    /// strengthening of `permits_point`, never a second door beside it.
    ///
    /// An empty stage list is "every stage this program runs", which is what
    /// keeps a manifest written before the field existed dispatching exactly
    /// as it did. [`WrapperPoint::AfterTurn`] is asked once per round about
    /// the round rather than once per stage, so it has no stage list to
    /// consult and is decided by the point alone.
    #[must_use]
    pub fn permits_stage(&self, point: WrapperPoint, stage: &StageName) -> bool {
        if !self.permits_point(point) {
            return false;
        }
        match point {
            WrapperPoint::BeforeTurn => {
                self.before_turn_stages.is_empty() || self.before_turn_stages.contains(stage)
            }
            WrapperPoint::AfterTurn => true,
        }
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
///
/// **`PartialEq` but not `Eq`, since #3999.** A `[[configure]]` value is a
/// `toml::Value`, which can be a float, and no float type is `Eq`. The
/// alternative was to refuse float-valued configuration so the derive could
/// stay — an API limit imposed by a marker trait rather than by anything about
/// configuration, which is the tail wagging the dog. Nothing used the stronger
/// bound: a manifest is compared, never hashed or used as a map key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// The `[driver]` registration — this plugin drives turns rather than
    /// sitting inside one, and these are the capabilities it may ask for while
    /// doing it (`doc:backlog-self-driving` §3.0).
    ///
    /// **Absent = not a driver**, which is the outer half of the gate: no
    /// `[driver]` block means no driver session is ever opened, so there is
    /// nothing for [`DriverGrant::permits_call`] to be consulted against. It is
    /// an `Option` rather than a defaulting struct for exactly that reason —
    /// "this plugin declared no driver capabilities" and "this plugin is not a
    /// driver" are different consents and must not share a representation.
    ///
    /// Independent of [`Self::loop_grant`] in both directions: a driver needs
    /// no [`Participation`] grade (there is no grade for driving, §3.0) and a
    /// grade buys no driver capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<DriverGrant>,
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
    /// Routing intents the host resolves against the user's own providers —
    /// for a `[subloop]` stage, or for a `[wrapper]` point naming one on its
    /// `before_turn` response. Requires one of those two: an intent nothing
    /// can spend is dead config in a document a human consented to (#3496).
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
    /// (#3565). See [`ToolContribution`] and [`crate::PackageListing`] for why these
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
    /// The MCP servers this package ships (#4733) — the `[[mcp]]` table.
    ///
    /// Gated on **no** participation grade, for [`Self::capabilities`]'
    /// reason: the ladder governs a plugin's say in the turn, and a
    /// `none`-grade bundle that starts a server on this machine is reaching
    /// further into the world than an `observer` that only watches.
    ///
    /// Declaring the servers by name is what makes this consentable at all.
    /// `[[configure]]` still refuses an `mcp` section (`crate::configure`'s
    /// `REFUSED_SECTIONS`) because *that* channel writes lines into the user's
    /// own file with no enumeration; this one names each server in the consent
    /// document and is reconciled against the package's `mcp.toml`, which is
    /// the property that refusal was asking for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp: Vec<McpContribution>,
    /// The configuration this package sets for as long as it is installed
    /// (#3999) — the `[[configure]]` table.
    ///
    /// Gated on **no** participation grade, for [`Self::capabilities`]'
    /// reason: the ladder governs a plugin's say in the turn, and a
    /// `none`-grade content bundle that changes how this workspace signs its
    /// commits is reaching further into the world than an `observer` that only
    /// watches.
    ///
    /// Declared, never applied here — this crate performs no I/O. The host
    /// writes it at install and puts back what it found at removal. See
    /// [`crate::ConfigureEntry`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub configure: Vec<ConfigureEntry>,
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
            // The loop-lifecycle pair is spellable here — it is one vocabulary
            // — and not routable to a plugin, so it is refused by name rather
            // than granted and never dispatched (#3599).
            if matches!(hook, HookEvent::PreIssueWork | HookEvent::PostIssueWork) {
                return Err(ManifestError::HookNotAvailableToPlugins { hook: *hook });
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
        // The per-capability ceilings get `max_calls`'s two rules against their
        // *own* capability rather than against the list as a whole: a plugin
        // that declares `recall` and a fan-out width has written a number that
        // bounds nothing, which is the manifest that quietly does nothing one
        // more time.
        match grant.max_fanout_width {
            Some(_) if !grant.calls.contains(&HostCall::CandidateFanout) => {
                return Err(ManifestError::MaxFanoutWidthRequiresFanout);
            }
            Some(0) => return Err(ManifestError::ZeroMaxFanoutWidth),
            _ => {}
        }
        match grant.max_child_turns {
            Some(_) if !grant.calls.contains(&HostCall::ChildTurn) => {
                return Err(ManifestError::MaxChildTurnsRequiresChildTurn);
            }
            Some(0) => return Err(ManifestError::ZeroMaxChildTurns),
            _ => {}
        }

        // The driver channel gets the same three rules and *not* the grade
        // check, which is the one asymmetry in this function and the whole
        // point of the block: a driver is not on the `Participation` ladder, so
        // there is no grade to be above (`doc:backlog-self-driving` §3.0). Nor
        // is there a `points` prerequisite — a driver call is made during a
        // driver session, and the `[driver]` block *is* the declaration that
        // this plugin has one.
        if let Some(driver) = &self.driver {
            let mut seen = HashSet::with_capacity(driver.calls.len());
            for call in &driver.calls {
                if !seen.insert(*call) {
                    return Err(ManifestError::DuplicateDriverCall { call: *call });
                }
            }
            match driver.max_calls {
                Some(_) if driver.calls.is_empty() => {
                    return Err(ManifestError::DriverMaxCallsRequiresCalls);
                }
                Some(0) => return Err(ManifestError::ZeroDriverMaxCalls),
                _ => {}
            }
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
            oracle.validate(
                participation,
                self.runtime.as_ref(),
                &grant.points,
                self.requirements.as_ref(),
            )?;
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

        // A `before_turn_stages` entry names a stage the host will never
        // dispatch unless this manifest's own `[wrapper]` orders it, so a name
        // that appears in neither is a narrowing that silently narrows to
        // nothing — the `UnknownMeasurement` argument, pointed at dispatch.
        // Checked in declaration order, and after `[wrapper]`'s own rules, so
        // an author fixing a stage list is not first told about a stage list.
        for stage in &self.loop_grant.before_turn_stages {
            let ordered = self
                .wrapper
                .as_ref()
                .is_some_and(|wrapper| wrapper.stages.iter().any(|s| &s.name == stage));
            if !ordered {
                return Err(ManifestError::UndispatchableStage {
                    stage: stage.to_string(),
                });
            }
        }

        if let Some(runtime) = &self.runtime {
            if !participation.includes(Participation::Observer) {
                return Err(ManifestError::RuntimeRequiresObserver { participation });
            }
            runtime.validate()?;
        }

        if let Some(roles) = &self.roles {
            // "A role must have something that could resolve it", not "a role
            // requires a subloop". `[subloop]` was the only such thing when
            // this rule was written; `BeforeTurnResponse::role` (#3380) made a
            // `[wrapper]` the second, and `stella_runtime::wrapper::admissible`
            // refuses an intent this table does not declare — so a wrapper
            // naming one has to declare it here. Refused when neither exists,
            // because then nothing can ever spend it (#3496).
            if self.subloop.is_none() && self.wrapper.is_none() {
                return Err(ManifestError::RolesResolveNowhere);
            }
            for (name, role) in roles {
                if role.tier.trim().is_empty() {
                    return Err(ManifestError::EmptyRoleTier { name: name.clone() });
                }
            }
        }

        validate_capabilities(&self.capabilities)?;
        validate_contributions(self)?;
        validate_configure(self)?;

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

    /// **The other half of "an undeclared hook is never invoked".** A plugin
    /// may spell the loop-lifecycle events — they are one vocabulary — and may
    /// not be routed at them: they are dispatched by the self-driving loop
    /// from the operator's own hooks settings, outside any turn. Refused by
    /// name, so an author learns it here rather than from a grant that
    /// silently never fires (#3599).
    #[test]
    fn a_plugin_may_not_be_routed_at_a_loop_lifecycle_hook() {
        for hook in ["PreIssueWork", "PostIssueWork"] {
            let err = parse(&format!(
                "name = \"x\"\n[loop]\nparticipation = \"steering\"\nhooks = [\"{hook}\"]"
            ))
            .unwrap_err();
            assert!(
                matches!(err, ManifestError::HookNotAvailableToPlugins { .. }),
                "{hook} must be refused: {err:?}"
            );
            let text = err.to_string();
            assert!(text.contains(hook), "{text}");
            assert!(text.contains("outside any turn"), "{text}");
        }
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
    fn roles_that_resolve_nowhere_are_rejected_and_tiers_must_be_non_empty() {
        let orphaned = parse(
            "name = \"x\"\n[loop]\nparticipation = \"steering\"\n\n[roles.triage]\ntier = \"cheap\"",
        )
        .unwrap_err();
        assert!(matches!(orphaned, ManifestError::RolesResolveNowhere));

        let blank_tier = parse(
            "name = \"x\"\n[loop]\nparticipation = \"steering\"\n\n[subloop]\nstages = [\"triage\"]\n\n[roles.triage]\ntier = \"\"",
        )
        .unwrap_err();
        assert!(matches!(blank_tier, ManifestError::EmptyRoleTier { .. }));
    }

    /// **The witness for #3496.** A `[wrapper]` that names a role intent on
    /// its `before_turn` response is a second thing that can resolve one, so
    /// `[roles]` beside it loads with no `[subloop]` to prop it up — while
    /// `[roles]` with neither is still refused, because the rule is "something
    /// must be able to spend this", not "declare any table you like".
    ///
    /// Three shipped manifests were declaring a `[subloop]` they never used to
    /// get past the old rule (`plugins/stella-plan`, `plugins/stella-goal`,
    /// and this crate's own reference fixture in
    /// `crates/stella-runtime/tests/wrapper_socket.rs`); all three drop it in
    /// the same change.
    #[test]
    fn a_wrapper_can_name_a_role_intent_without_declaring_a_subloop() {
        let wrapper_only = parse(
            "name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"before_turn\"]\n\n\
             [wrapper]\nid = \"x-v1\"\n\n[[wrapper.stages]]\nname = \"plan\"\n\n\
             [roles.planner]\ntier = \"plan\"",
        )
        .expect("a wrapper naming a role intent needs no subloop to resolve it");
        assert!(wrapper_only.subloop.is_none());
        assert!(wrapper_only.roles.is_some());

        // A tier is still a tier: widening which tables satisfy the rule does
        // not widen what the entries themselves may say.
        let blank_tier = parse(
            "name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"before_turn\"]\n\n\
             [wrapper]\nid = \"x-v1\"\n\n[[wrapper.stages]]\nname = \"plan\"\n\n\
             [roles.planner]\ntier = \" \"",
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
            before_turn_stages: Vec::new(),
            calls: vec![HostCall::Recall],
            max_calls: None,
            max_child_turns: None,
            max_fanout_width: None,
            max_holds: None,
        };
        assert!(!smuggled.permits_hook(HookEvent::PreToolUse));
        assert!(!smuggled.permits_point(WrapperPoint::BeforeTurn));
        assert!(!smuggled.permits_call(HostCall::Recall));
        // The stage filter is a strengthening of the point filter, so it
        // inherits the grade check rather than reopening it: an empty stage
        // list is "every stage", and a grade below steering still reaches
        // nothing (#3543).
        assert!(!smuggled.permits_stage(WrapperPoint::BeforeTurn, &StageName::new("execute")));
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

    /// **Witness for #3599 B0, the manifest half.** A driver holds its
    /// capabilities through a `[driver]` block that the `Participation` ladder
    /// neither grants nor gates.
    ///
    /// The required assertion is the first one: `participation = "none"` —
    /// the honest grade for a plugin that never runs inside a turn — used to
    /// make every capability unreachable, and that is the defect the phase
    /// exists to fix. The rest pin the asymmetry deliberately: no grade is
    /// required, no `points` prerequisite applies (a driver call is made during
    /// a driver session, not during a wrapper point), and the `[loop]` rules
    /// that *do* transfer — deduplicated, a coherent allowance — still hold.
    #[test]
    fn a_driver_holds_capabilities_without_a_participation_grade() {
        use crate::driver::DriverCall;
        let driving = parse(
            "name = \"x\"\n[loop]\nparticipation = \"none\"\n\n[driver]\ncalls = [\"backlog_next\", \"deliver_open\"]",
        )
        .expect("a driver at grade `none` loads");
        let grant = driving.driver.expect("the [driver] block is parsed");
        assert!(grant.permits_call(DriverCall::BacklogNext));
        assert!(grant.permits_call(DriverCall::DeliverOpen));
        // Declared is exhaustive, in the driver's context as in the wrapper's.
        assert!(!grant.permits_call(DriverCall::DeliverMerge));
        // And the ladder is untouched: the same manifest still takes no say in
        // any turn.
        assert_eq!(driving.loop_grant.participation, Participation::None);

        // Absent is not empty. "Not a driver" and "a driver that asks for
        // nothing" must not share a representation.
        assert!(
            parse("name = \"x\"")
                .expect("a bare manifest loads")
                .driver
                .is_none()
        );
        assert_eq!(
            parse("name = \"x\"\n[driver]")
                .expect("an empty [driver] block loads")
                .driver,
            Some(DriverGrant::default())
        );

        // The capability set is closed, not an RPC surface — and `release` is
        // deliberately not in it (§6.4).
        assert!(matches!(
            parse("name = \"x\"\n[driver]\ncalls = [\"release\"]").unwrap_err(),
            ManifestError::Parse(_)
        ));

        // The `[loop] calls` rules that transfer.
        assert!(matches!(
            parse("name = \"x\"\n[driver]\ncalls = [\"sweep_audit\", \"sweep_audit\"]")
                .unwrap_err(),
            ManifestError::DuplicateDriverCall {
                call: DriverCall::SweepAudit
            }
        ));
        assert!(matches!(
            parse("name = \"x\"\n[driver]\nmax_calls = 4").unwrap_err(),
            ManifestError::DriverMaxCallsRequiresCalls
        ));
        assert!(matches!(
            parse("name = \"x\"\n[driver]\ncalls = [\"sweep_audit\"]\nmax_calls = 0").unwrap_err(),
            ManifestError::ZeroDriverMaxCalls
        ));
    }
}
