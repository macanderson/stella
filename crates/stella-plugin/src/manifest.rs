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

use crate::error::ManifestError;
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

/// A hook point a plugin may declare in `[loop] hooks`.
///
/// The names mirror the engine's shipped hook events
/// (`stella-core::hooks::HookEvent`) exactly — same set, same wire strings —
/// because the grant is *for* those dispatch points. The engine's enum is
/// deliberately not imported: `stella-core` never learns plugins exist
/// (#3245 open question 3 settles the direction), and this crate is a leaf.
/// The host maps between the two by name; keeping the sets identical is a
/// review obligation on any PR that grows either (#3310 tracks unifying the
/// two in a shared home so the mirror stops being manual).
///
/// **The PascalCase is load-bearing, and the casing split from
/// [`Participation`] is deliberate.** These five strings are not this
/// crate's to choose: `"PreToolUse"` is already what a user types in
/// `.stella/settings.json` to register a shell hook (README.md §Lifecycle
/// hooks), because `stella-core`'s enum carries no `rename_all`. Spelling
/// the same event `"pre_tool_use"` in a plugin manifest would fork one
/// concept's name across two files a user edits — strictly worse than the
/// inconsistency it would resolve. [`Participation`] is lowercase because
/// its vocabulary is this crate's own invention (#3245 §2) and nothing
/// outside spells it. The rule for a future block: **a value that mirrors
/// an existing user-facing string keeps that string's casing; a value this
/// crate coins is lowercase.** `wire_strings_are_pinned_on_both_sides`
/// fails if either half drifts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookEvent {
    /// Once, before the turn begins.
    SessionStart,
    /// Before each tool call — input rewriting and permission decisions.
    PreToolUse,
    /// After each tool call.
    PostToolUse,
    /// The turn is about to complete. Declaring this is what an arbiter's
    /// verdict hook rides on, and it is rejected below that grade.
    Stop,
    /// An overflow-summarization round is about to run.
    PreCompact,
}

impl std::fmt::Display for HookEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            HookEvent::SessionStart => "SessionStart",
            HookEvent::PreToolUse => "PreToolUse",
            HookEvent::PostToolUse => "PostToolUse",
            HookEvent::Stop => "Stop",
            HookEvent::PreCompact => "PreCompact",
        })
    }
}

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
}

/// The `[oracle]` block — the witness protocol as a wire contract.
///
/// The HOST runs this; the plugin never grades its own work (the #2584
/// discipline, stated for plugins). Arbiter-only: the oracle exists to
/// decide requirements, and below `arbiter` there are none to decide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Oracle {
    /// The argv the host executes to run the oracle. Never a shell string —
    /// the #1400 rule, same as every hook.
    pub command: OracleCommand,
    /// Whether the host must observe a fail→pass flip before crediting.
    pub flip: FlipPolicy,
    /// How the host detects tampering with the witness artifacts.
    pub tamper: TamperPolicy,
}

/// The process an oracle runs as, with the host-enforced bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OracleCommand {
    /// Program and arguments. `${plugin_dir}` interpolation is the host's
    /// concern; this crate only requires the list to be non-empty.
    pub argv: Vec<String>,
    /// Seconds the host allows the oracle before killing it. Must be at
    /// least 1 — a zero timeout kills the oracle before it runs.
    pub timeout_secs: u64,
}

/// Whether a fail→pass flip is required before the oracle's requirement is
/// credited. One variant today, deliberately: `#3245` defines only
/// `"required"`, and an unknown value must be a load error rather than a
/// silently weaker contract — a future relaxation adds a variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlipPolicy {
    /// The host credits the oracle only on an observed fail-before /
    /// pass-after flip.
    Required,
}

/// How the host detects witness-artifact tampering. One variant today, for
/// the same reason as [`FlipPolicy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TamperPolicy {
    /// The host snapshots artifact identity at authoring time and refuses
    /// the flip if it changed by verify time — the `witness.rs` discipline.
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
    /// Arbiter only: the host-run oracle.
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
            if oracle.command.argv.is_empty() {
                return Err(ManifestError::EmptyOracleArgv);
            }
            if oracle.command.timeout_secs == 0 {
                return Err(ManifestError::ZeroOracleTimeout);
            }
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
            max_holds: None,
        };
        assert!(!smuggled.permits_hook(HookEvent::PreToolUse));
    }
}
