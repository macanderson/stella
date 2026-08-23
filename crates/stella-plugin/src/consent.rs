//! The install-consent surface: what a plugin declares it will be allowed to
//! do, and the text a human reads before saying yes.
//!
//! # Why this exists, and why here
//!
//! `doc:pipeline-as-plugins` §A1 and the run playbook's D-3 place one hard
//! requirement on the authority vocabulary, ahead of any loader: the grant a
//! plugin needs must be **expressible**, and a user must be able to **see it
//! before install**. The most dangerous plugin anyone will install is the
//! self-driving loop, which holds `gh`, the AWS CLI, `brew`, a line in
//! `~/.zshrc` and a daemon — as a shell script running with full user
//! authority today. Packaging it as a plugin *relocates* that authority; it
//! grants nothing new. But a relocation nobody can see is a grant, so the
//! declaration and its rendering are part of the vocabulary, not a later
//! surface concern.
//!
//! This crate is the home because it is the plugin-contract crate and it is
//! pure: a consent document is exactly "what did this plugin declare?", which
//! is the one decision this crate owns (README § Boundary). `stella-core`
//! could not hold it — the engine must never learn plugins exist (#3245 open
//! question 3) — and putting it in the CLI would tie the consent text to one
//! surface when `stella-serve` and an embedded host need the same words.
//!
//! # The vocabulary is the gate's, deliberately
//!
//! [`Capability`] grades itself in [`stella_protocol::RiskLevel`] — the same
//! type `ToolContract` carries and `stella_core::ports::RiskCeiling` enforces
//! a grant in. That is the #3310 argument reused, not a new dependency: the
//! grade a user is shown at install and the grade a gate refuses on must be
//! one type, or the consent text is a second vocabulary that can disagree
//! with the enforcement it describes.
//!
//! # What this module does not claim
//!
//! Nothing here enforces anything. It renders a declaration. The engine-side
//! caller a granted capability is attributed to is
//! `stella_core::ports::Principal::Plugin` — named there, spelled nowhere in
//! this crate, because the two crates may not depend on each other in either
//! direction and a hand-kept copy of that string would be the mirror #3310
//! removed. Binding a declared capability to an `AuthzGate` rule is the
//! loader's job (`doc:pipeline-as-plugins` §A4).

use serde::{Deserialize, Serialize};

use crate::error::ManifestError;
use crate::host_call::HostCall;
use crate::manifest::{Participation, PluginManifest};
use crate::oracle::OracleProcessSource;
use crate::wire::{WIRE_FIELDS, WrapperPoint};

/// How bad one honest call of a tool is — re-exported from
/// [`stella_protocol`], which is where the vocabulary lives.
///
/// The same argument [`crate::HookEvent`] carries, pointed at the
/// authorization plane instead of the hook plane: a capability's grade and
/// the grade `stella_core::ports::RiskCeiling` refuses on must be one type.
/// Two enums spelling four grades in two crates that may not depend on each
/// other is the mirror #3310 removed — and here the drift would be a consent
/// prompt showing a user a grade the gate does not enforce.
pub use stella_protocol::RiskLevel;

/// One capability a plugin asks for, graded in the gate's own vocabulary.
///
/// A capability is a **request**, never a grant: the manifest states what the
/// plugin wants and why, the consent prompt shows it, and only a loader that
/// a human said yes to turns it into a rule an `AuthzGate` enforces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    /// The tool, as `stella_protocol::ToolContract::name` spells it —
    /// `"bash"`, `"write_file"`, `"mcp__github__create_pull_request"`.
    ///
    /// Free text here on purpose: this crate has no registry to check it
    /// against and inventing one would be a second source of truth for the
    /// tool catalog. A name matching no registered tool is a dead request,
    /// which the host detects at install; this crate only requires it to be
    /// non-empty.
    pub tool: String,
    /// How bad one honest call of that tool is, in the grade a
    /// `RiskCeiling` refuses on. Declared rather than inferred: a plugin that
    /// under-grades itself is making a checkable claim, and a host comparing
    /// this against the registered contract's own grade catches it.
    pub risk: RiskLevel,
    /// Why the plugin needs it, in the author's words. Shown at consent,
    /// flattened to one line by [`consent_text`] and otherwise verbatim.
    ///
    /// Required, not optional: a capability with no stated reason is an
    /// unreviewable grant, and "the manifest asked for `bash`" is not a thing
    /// a human can consent to.
    pub purpose: String,
    /// How the plugin says it will limit the capability — argv prefixes,
    /// paths, hosts. Free text, shown verbatim.
    ///
    /// **This is the plugin's claim, not Stella's enforcement**, and
    /// [`consent_text`] says so out loud wherever it prints one. A consent
    /// prompt that rendered a self-declared limit as though the gate enforced
    /// it would be worse than omitting it: the user would consent to a
    /// narrower grant than the one they are actually giving.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
}

/// The widest grade in a capability list, or `None` when nothing is asked
/// for.
///
/// The headline number of a consent prompt: a user skimming ten capabilities
/// should not have to find the worst one themselves.
#[must_use]
pub fn highest_risk(capabilities: &[Capability]) -> Option<RiskLevel> {
    capabilities.iter().map(|capability| capability.risk).max()
}

/// The cross-field rules for `[[capabilities]]`, kept beside the type they
/// govern exactly as [`crate::wrapper`]'s are.
pub(crate) fn validate_capabilities(capabilities: &[Capability]) -> Result<(), ManifestError> {
    let mut seen = std::collections::HashSet::with_capacity(capabilities.len());
    for capability in capabilities {
        let tool = capability.tool.trim();
        if tool.is_empty() {
            return Err(ManifestError::EmptyCapabilityTool);
        }
        if capability.purpose.trim().is_empty() {
            return Err(ManifestError::EmptyCapabilityPurpose {
                tool: capability.tool.clone(),
            });
        }
        for scope in &capability.scope {
            if scope.trim().is_empty() {
                return Err(ManifestError::EmptyCapabilityScope {
                    tool: capability.tool.clone(),
                });
            }
        }
        if !seen.insert(tool) {
            return Err(ManifestError::DuplicateCapability {
                tool: capability.tool.clone(),
            });
        }
    }
    Ok(())
}

/// Render a manifest's declared say in the loop and its declared capabilities
/// into the text an install prompt shows a human.
///
/// Pure and deterministic: the same manifest renders the same bytes, so a
/// host can diff two versions of a plugin's consent and show a user only what
/// changed. No I/O, no environment, no clock.
///
/// # The prose is the plugin's; the structure is ours
///
/// Every author-supplied string — the name, the description, a purpose, a
/// scope, an oracle argument — is flattened to a single line and stripped of
/// control characters before it reaches the output (`one_line`, whose own doc
/// comment argues the two hazards separately). That is a security property,
/// not tidiness: a `purpose`
/// containing a newline and the text `It asks for no capabilities.` would
/// otherwise forge a line of Stella's own prompt, and an ANSI escape would
/// repaint the terminal the user is consenting in. The number of lines this
/// function emits is decided by the manifest's *structure* alone, which
/// `author_prose_cannot_forge_a_line_of_the_prompt` pins.
///
/// # The sentence that must not be soft
///
/// For a plugin declaring an `[oracle]`, `oracle_self_report` is the most
/// important paragraph this function emits, and it is the one that used to be
/// false. Verification is a paid plugin's job now, and base Stella does not
/// verify: the flip and the numbers a verdict turns on are what the plugin's
/// own process **reported**, and the host neither runs the oracle nor checks
/// the report (#3511). A user is trusting the plugin's honesty about its own
/// work, so they are told so in those words before they install — the same
/// argument `capability_grant`'s claimed-limit disclaimer makes on the other
/// half of the document, pointed at the half that decides done.
///
/// # The document is complete, which took a manifest change to become true
///
/// `package_contributions` renders what the package *ships* — tools, skills,
/// context records. Until #3565 those were discovered by directory convention
/// and nothing declared them, so this function could not see them and an
/// embedding host showed a document that omitted executable code entering the
/// agent's tool surface. What makes the completeness real is not this
/// rendering but [`PluginManifest::reconcile`]: the host checks its own read of
/// the package against the declaration and refuses any disagreement, so a
/// directory cannot hold a contribution these bytes do not name.
#[must_use]
pub fn consent_text(manifest: &PluginManifest) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("Install `{}`?", one_line(&manifest.name)));
    if let Some(description) = &manifest.description {
        let description = one_line(description);
        if !description.is_empty() {
            lines.push(description);
        }
    }

    lines.push(String::new());
    lines.extend(loop_say(manifest));
    lines.extend(driver_say(manifest));
    lines.extend(oracle_self_report(manifest));
    lines.extend(data_that_leaves_the_process(manifest));
    lines.push(String::new());
    lines.extend(capability_grant(manifest));
    lines.extend(package_contributions(manifest));
    lines.extend(configuration_changes(manifest));
    lines.push(String::new());
    lines.push(format!(
        "Every tool call `{}` makes is attributed to it, not to you: the \
         authorization gate sees it as a caller of its own, and may refuse it \
         regardless of what you are allowed to do yourself.",
        one_line(&manifest.name)
    ));
    lines.push("Nothing above is granted until you accept.".into());

    lines.join("\n")
}

/// The "what it does inside your turn" half: the grade, and every power the
/// grade unlocked that this manifest actually declared.
fn loop_say(manifest: &PluginManifest) -> Vec<String> {
    let grant = &manifest.loop_grant;
    let mut lines = vec![format!(
        "Say in your turn loop: {}{}",
        grade_sentence(grant.participation),
        what_a_none_grade_is(manifest)
    )];

    if grant.hooks.is_empty() {
        lines.push("  - runs at no hook point".into());
    } else {
        let hooks: Vec<String> = grant.hooks.iter().map(ToString::to_string).collect();
        lines.push(format!(
            "  - runs at these hook points: {}",
            hooks.join(", ")
        ));
    }

    if grant.participation.includes(Participation::Arbiter) {
        lines.push(match grant.max_holds {
            Some(1) => "  - may refuse to let a finished turn end, once per turn".into(),
            Some(holds) => {
                format!("  - may refuse to let a finished turn end, up to {holds} times per turn")
            }
            None => "  - may refuse to let a finished turn end, as often as the host allows".into(),
        });
    }

    if let Some(requirements) = &manifest.requirements {
        lines.push("  - holds a turn open until it can say each of these is met:".into());
        // A `BTreeMap`, so the order is the manifest's own and is stable.
        for (name, description) in requirements {
            lines.push(format!(
                "      {}: {}",
                one_line(name),
                one_line(description)
            ));
            // A budget a user cannot read before installing is not
            // meaningfully declared: the whole gain of stating a verdict rule
            // as data is that changing it is a visible change to a file
            // somebody consented to (`doc:pipeline-as-plugins` §6.1).
            for check in manifest.oracle.iter().flat_map(|oracle| &oracle.checks) {
                if check.requirement == *name {
                    lines.push(format!("          decided by: {}", one_line(&check.rule)));
                }
            }
        }
    }

    // Resolved rather than read off `[oracle] command`, because the oracle may
    // be the plugin's own `[runtime]` process (#3501) — and what a human is
    // consenting to is the program that runs, not which block happened to name
    // it.
    if let Some(oracle) = manifest.oracle_process() {
        let argv: Vec<String> = oracle.argv.iter().map(|arg| one_line(arg)).collect();
        // "a program of its own" is a different thing to consent to than "the
        // process you already agreed to, run again", so the sentence says which
        // one this is.
        let what = match oracle.source {
            OracleProcessSource::OracleCommand => "runs a program of its own as the oracle",
            OracleProcessSource::Runtime => "runs its own process again as the oracle",
        };
        lines.push(format!(
            "  - {what} that decides those: `{}` (killed after {}s)",
            argv.join(" "),
            oracle.timeout_secs
        ));
    }

    // A subprocess a user cannot see before installing is the grant this whole
    // module exists to make visible, and the environment slice is half of it:
    // "runs `python3 main.py`" and "runs `python3 main.py`, and hands it your
    // `GITHUB_TOKEN`" are different things to consent to.
    if let Some(runtime) = &manifest.runtime {
        let argv: Vec<String> = runtime.argv.iter().map(|arg| one_line(arg)).collect();
        lines.push(format!(
            "  - runs as a process on your machine: `{}` (killed after {}s)",
            argv.join(" "),
            runtime.timeout_secs
        ));
        lines.push(if runtime.env.is_empty() {
            "      it inherits NO environment variables".into()
        } else {
            let names: Vec<String> = runtime.env.iter().map(|name| one_line(name)).collect();
            format!(
                "      it inherits these environment variables and no others: {}",
                names.join(", ")
            )
        });
    }

    // The host-call channel, which until now was declared in the manifest and
    // shown to nobody — `LoopGrant::calls` says a plugin "may not ask for a
    // capability a human never read at install", and this is the line that
    // makes the second half of that sentence true.
    if !grant.calls.is_empty() {
        let calls: Vec<String> = grant.calls.iter().map(ToString::to_string).collect();
        lines.push(format!(
            "  - asks the host for these capabilities while answering a point: {}",
            calls.join(", ")
        ));
    }

    // Two of those capabilities are not like the others, and the list above
    // renders them in the same breath as `recall`. A fan-out buys N *writing*
    // worker turns off one ask, and an adoption puts one of their diffs on the
    // reader's own tree — the two facts a human is consenting to, said in
    // words rather than left to be inferred from a capability's name (#3844).
    if grant.calls.contains(&HostCall::CandidateFanout) {
        lines.push(match grant.max_fanout_width {
            Some(width) => format!(
                "  - runs up to {width} isolated attempt(s) at the goal per fan-out, each a full \
                 model-spending turn that writes in its own workspace"
            ),
            None => "  - runs as many isolated attempts at the goal per fan-out as the host \
                     allows, each a full model-spending turn that writes in its own workspace"
                .to_string(),
        });
    }
    if grant.calls.contains(&HostCall::AdoptCandidate) {
        lines.push(
            "  - applies one of those attempts to your real work tree, and discards the rest"
                .to_string(),
        );
    }

    if let Some(subloop) = &manifest.subloop {
        let stages: Vec<String> = subloop.stages.iter().map(|stage| one_line(stage)).collect();
        lines.push(format!(
            "  - spends model calls on these stages as child turns: {}",
            stages.join(", ")
        ));
    }
    lines.extend(role_spend(manifest));

    if let Some(wrapper) = &manifest.wrapper {
        // A contributed stage is named as the plugin's own rather than listed
        // beside the host's twelve as though Stella had always had it (#3963).
        // Consent is a claim about what installing this changes, and "it runs
        // a stage of its own invention" is exactly the part a reader cannot
        // recover from the name alone once the vocabulary is open.
        let stages: Vec<String> = wrapper
            .stages
            .iter()
            .map(|stage| {
                let name = one_line(stage.name.as_str());
                if stage.name.is_contributed() {
                    format!("{name} (this plugin's own stage)")
                } else {
                    name
                }
            })
            .collect();
        lines.push(format!(
            "  - wraps every turn as variant `{}`, running: {}",
            one_line(&wrapper.id),
            stages.join(", ")
        ));
    }

    lines
}

/// What each declared stage costs: the model tier its `[roles.<name>]` intent
/// asks to be routed at (#3514).
///
/// Empty for a manifest declaring no `[roles]`, on
/// `the_scope_disclaimer_appears_only_when_a_scope_was_declared`'s reasoning —
/// a manifest written before this section renders byte-for-byte what it
/// rendered before.
///
/// # Why the omission was a defect rather than a gap
///
/// `WrapperError::UndeclaredRole` refuses a role intent the manifest never
/// declared, and justifies the refusal with "the roles a human consented to at
/// install are the roles a wrapper may spend". That cited a document which
/// named the stages and nothing about what they cost, so a user consented to a
/// stage list without being told any of it spends at a premium tier — on their
/// own BYOK key.
///
/// The tier is stated as the plugin's *ask*, not as a model, because that is
/// what it is: [`crate::Role`] is a routing intent an open-vocabulary host
/// resolves against the user's own providers, soft-failing to the session
/// default. Rendering it as a model would be `capability_grant`'s claimed-limit
/// error in the other direction — a promise this crate cannot keep.
fn role_spend(manifest: &PluginManifest) -> Vec<String> {
    let Some(roles) = &manifest.roles else {
        return Vec::new();
    };
    if roles.is_empty() {
        return Vec::new();
    }
    let mut lines = vec!["  - spends each stage's model calls at a tier it names:".into()];
    // A `BTreeMap`, so the order is the manifest's own and is stable.
    for (name, role) in roles {
        lines.push(format!(
            "      {}: the `{}` tier",
            one_line(name),
            one_line(&role.tier)
        ));
    }
    lines.push(
        "      A tier is the name this plugin asks for, never a model: your own provider \
         configuration decides what each one resolves to, and your key pays for it."
            .into(),
    );
    lines
}

/// The "what it does *outside* your turns" half: that this plugin drives
/// Stella, and every capability family it may ask for while driving.
///
/// A separate paragraph from [`loop_say`] rather than another bullet under it,
/// because the two are different consents and reading them as one is exactly
/// the confusion `doc:backlog-self-driving` §3.0 keeps the ladder out of: a
/// grade says how much say a plugin has *inside* a turn, and this says what it
/// may do to your repository between them.
///
/// Rendered by family rather than verb by verb — "pushes branches, opens pull
/// requests, reads CI, and merges" is the sentence a human weighs, and
/// `deliver_merge` on its own is not. The verbs are printed beside it so the
/// declaration stays checkable against the block.
fn driver_say(manifest: &PluginManifest) -> Vec<String> {
    let Some(driver) = &manifest.driver else {
        return Vec::new();
    };
    let mut lines = vec![
        String::new(),
        format!(
            "Say outside your turn loop: `{}` DRIVES Stella. It is not a participant in a \
             turn — it starts them.",
            one_line(&manifest.name)
        ),
    ];

    if driver.calls.is_empty() {
        // A driver that asks for nothing is a coherent declaration and a
        // remarkable one, so it is said rather than left as an absence.
        lines.push("  - asks the host for no capability at all".into());
        return lines;
    }

    for family in driver.families() {
        let verbs: Vec<String> = driver
            .calls
            .iter()
            .filter(|call| call.family() == family)
            .map(ToString::to_string)
            .collect();
        lines.push(format!(
            "  - {} ({})",
            family.consent_sentence(),
            verbs.join(", ")
        ));
    }

    lines.push(match driver.max_calls {
        Some(max) => format!("  - asks for up to {max} of those per driver session"),
        None => "  - asks for as many of those per driver session as the host allows".into(),
    });
    lines.push(
        "  - every one of those is performed BY Stella on request: this plugin holds no \
         credential, no provider and no forge token of its own."
            .into(),
    );
    lines
}

/// The disclosure a verification plugin's install prompt turns on: the
/// evidence behind its verdict is the plugin's own report, and Stella does not
/// check it.
///
/// Empty for a manifest with no `[oracle]`, on
/// `the_scope_disclaimer_appears_only_when_a_scope_was_declared`'s reasoning —
/// a disclaimer printed where there is no claim to qualify teaches a reader to
/// skip the one that matters.
///
/// # Why this is the strongest sentence in the document
///
/// The manifest used to document `[oracle]` as host-run and this prompt
/// repeated it, so a user approved a verification plugin on the strength of a
/// guarantee no code provided: nothing executes `[oracle] command`, and
/// `stella_runtime::wrapper::judge` decides on the flip and the measurements
/// that arrived verbatim on the plugin's own `after_turn` response (#3511,
/// settled as Option 2 on 2026-08-17). The sin was never self-reported
/// evidence — a plugin running its own oracle is a reasonable design. The sin
/// was telling a user it was host-verified when it was not, which is a
/// consent defect rather than a documentation one, and is why the correction
/// lands here and not only in a doc comment.
///
/// It states the two halves separately on purpose. What Stella still does —
/// evaluate the declared rule, and own the tamper finding a plugin cannot
/// write ([`crate::ObservedEvidence`]) — is real, and a disclosure that read
/// as "none of this means anything" would mislead in the opposite direction.
fn oracle_self_report(manifest: &PluginManifest) -> Vec<String> {
    if manifest.oracle.is_none() {
        return Vec::new();
    }
    let name = one_line(&manifest.name);
    vec![
        String::new(),
        format!(
            "`{name}` decides when your turn is done, and it reports its own evidence for \
             that. Stella does not run the oracle itself and does not check what comes \
             back: whether a test went fail→pass, and every number a declared check \
             compares against a budget, are what `{name}`'s own process said happened."
        ),
        format!(
            "Stella applies the rule above to those reported claims and will not credit a \
             requirement they leave undecided — but it cannot tell an earned result from a \
             typed one. Installing `{name}` means trusting it to report honestly about its \
             own work."
        ),
    ]
}

/// What of the user's own work is handed to the plugin's process, at each
/// point it is granted (#3514).
///
/// # The disclosure that matters most for a BYOK product
///
/// Every other section here enumerates a *power* — a grade, a hook, an argv, an
/// environment slice, a capability. None of them names the **data**, and the
/// wire ships the user's prompt, the model's whole reply, the tools the turn
/// ran and the files it changed, as JSON on a third party's standard input. A
/// plugin declaring no `[[capabilities]]` renders "It asks for no tool
/// capabilities.", which reads as harmless and left a prompt-exfiltrating
/// plugin indistinguishable from a benign one at the one moment a user gets to
/// refuse.
///
/// # It reads off [`WIRE_FIELDS`] because a hand-written list rots
///
/// The sentences belong to [`crate::wire`], beside the types they describe, and
/// `every_wire_field_is_in_the_table` destructures those types against the table
/// — so a field added to the wire and disclosed to nobody fails a test. Spelling
/// the list out here would make this document complete exactly until the next
/// field landed.
///
/// # Empty when nothing is dispatched, per point
///
/// `LoopGrant::permits_point` is the filter `stella_runtime::wrapper` actually
/// dispatches on, so it is the filter here: telling a user their prompt crosses
/// to a plugin that answers no point would disclose something that does not
/// happen, which is `a_declared_scope_is_labelled_as_the_plugins_claim`'s error
/// with the sign flipped.
fn data_that_leaves_the_process(manifest: &PluginManifest) -> Vec<String> {
    // No `[runtime]` and no `[wrapper]` is no process of the plugin's own, so
    // there is no other end of a pipe for any of this to reach.
    if manifest.runtime.is_none() && manifest.wrapper.is_none() {
        return Vec::new();
    }
    let disclosed: Vec<(WrapperPoint, &'static str)> = WIRE_FIELDS
        .iter()
        .filter(|field| manifest.loop_grant.permits_point(field.point))
        .filter_map(|field| field.disclosure.map(|sentence| (field.point, sentence)))
        .collect();
    if disclosed.is_empty() {
        return Vec::new();
    }

    let name = one_line(&manifest.name);
    let mut lines = vec![
        String::new(),
        format!(
            "Stella hands `{name}`'s own process your work, as JSON on that process's \
             standard input. What crosses:"
        ),
    ];
    let mut rendering = None;
    for (point, sentence) in disclosed {
        if rendering != Some(point) {
            lines.push(format!("  - {} (`{point}`):", point_moment(point)));
            rendering = Some(point);
        }
        lines.push(format!("      - {sentence}"));
    }
    lines.push(
        "  Once it has been handed over, nothing in Stella bounds what that process does \
         with it: a plugin asking for no tool capability at all can still send every line \
         of it anywhere it can reach."
            .into(),
    );
    lines
}

/// When each point's request is sent, in a reader's terms rather than the
/// socket's. Exhaustive by construction: a third [`WrapperPoint`] does not
/// compile until it has words here.
fn point_moment(point: WrapperPoint) -> &'static str {
    match point {
        WrapperPoint::BeforeTurn => "before each turn runs",
        WrapperPoint::AfterTurn => "once each turn has finished",
    }
}

/// The "what it may reach outside your turn" half — the capability list, its
/// headline grade, and the one sentence that keeps a self-declared scope from
/// reading as an enforced one.
fn capability_grant(manifest: &PluginManifest) -> Vec<String> {
    let capabilities = &manifest.capabilities;
    let Some(worst) = highest_risk(capabilities) else {
        return vec!["It asks for no tool capabilities.".into()];
    };

    let mut lines = vec![
        format!(
            "The widest thing it asks for is graded {} — {}.",
            worst.as_str().to_uppercase(),
            risk_blurb(worst)
        ),
        "Capabilities it is asking for:".into(),
    ];
    // Manifest order, not sorted: the author's ordering is information, and a
    // host diffing two versions of a consent prompt wants a stable rendering
    // of the same declaration rather than a tidied one.
    for capability in capabilities {
        lines.push(format!(
            "  - `{}` ({}) — {}",
            one_line(&capability.tool),
            capability.risk.as_str(),
            one_line(&capability.purpose)
        ));
        for scope in &capability.scope {
            lines.push(format!("      claimed limit: {}", one_line(scope)));
        }
    }
    if capabilities
        .iter()
        .any(|capability| !capability.scope.is_empty())
    {
        lines.push(String::new());
        lines.push(
            "A claimed limit is the plugin's own statement about how it will use a \
             capability. The gate enforces the tool and the grade; it does not verify \
             the claim."
                .into(),
        );
    }

    lines
}

/// The "what arrives on your machine besides a process" half — the tools,
/// skills and context records the package ships (#3565).
///
/// Empty for a package that declares none, on
/// `the_scope_disclaimer_appears_only_when_a_scope_was_declared`'s reasoning
/// and, here, one stronger: the line count of this document is decided by the
/// manifest's structure alone, and a manifest that predates these tables must
/// render byte-for-byte what it rendered before.
///
/// # Why this is in the crate and not in the host
///
/// It was in the host, and that was the defect (#3565). `stella plugin
/// install` rendered the inventory it read off the package directory, beside
/// this document, so the two together were complete — but only for that one
/// surface. A `stella-serve` host or an embedded host calling
/// [`consent_text`] got a document that omitted **executable code entering the
/// agent's tool surface**, which is the sharpest possible omission from a
/// consent prompt. The manifest now declares what the package ships and the
/// host reconciles its own read against that declaration
/// ([`PluginManifest::reconcile`]), so these bytes are complete for every host
/// and the directory can no longer contain a contribution the document does
/// not name.
///
/// # The three are not levelled, because the powers are not the same
///
/// A **tool** is executable code the model may call on its own initiative,
/// running as the plugin rather than as the user — the loudest line here, and
/// it says all three of those things. A **skill** is text selected into a
/// prompt and is never enforced. A **record** is the quiet one and the reason
/// this section does not summarise: it steers *every* future turn it matches,
/// without ever appearing in a transcript as an action, which is a different
/// shape of power from a tool rather than a smaller amount of the same one.
fn package_contributions(manifest: &PluginManifest) -> Vec<String> {
    if manifest.tools.is_empty() && manifest.skills.is_empty() && manifest.records.is_empty() {
        return Vec::new();
    }
    let name = one_line(&manifest.name);
    let mut lines = vec![String::new(), format!("`{name}` also installs:")];

    if !manifest.tools.is_empty() {
        lines.push(format!(
            "  - {} the model may call on its own initiative. Each one is executable code \
             this package ships, and every call runs as `{name}`, not as you:",
            count(manifest.tools.len(), "tool")
        ));
        // Manifest order, not sorted — `capability_grant`'s argument: the
        // author's ordering is information, and a host diffing two versions of
        // a consent document wants a stable rendering rather than a tidied one.
        for tool in &manifest.tools {
            lines.push(format!(
                "      `{}` — {}",
                one_line(&tool.name),
                one_line(&tool.description)
            ));
        }
    }

    if !manifest.skills.is_empty() {
        lines.push(format!(
            "  - {}, injected into your prompts when they match. A skill is never enforced:",
            count(manifest.skills.len(), "skill")
        ));
        for skill in &manifest.skills {
            lines.push(format!(
                "      `{}` — {}",
                one_line(&skill.slug),
                one_line(&skill.description)
            ));
        }
    }

    if !manifest.records.is_empty() {
        lines.push(format!(
            "  - {}, which steer every future turn in this workspace that they match — \
             quietly, without appearing in a transcript as anything the agent did:",
            count(manifest.records.len(), "context record")
        ));
        for record in &manifest.records {
            lines.push(format!(
                "      `{}` — {}",
                one_line(&record.lineage),
                one_line(&record.statement)
            ));
        }
        lines.push(
            "      They steer and nothing more: a record a plugin ships can never deny a \
             tool call. That authority comes only from a promotion you record in this \
             repository's own ledger."
                .into(),
        );
    }

    lines.push(
        "  None of it is copied into your own .stella/ — it is read from the package on \
         every load, so `stella plugin remove` takes all of it away."
            .into(),
    );
    lines
}

/// The configuration this package changes — the `[[configure]]` table, shown
/// as the literal keys and values the host will write (#3999).
///
/// # Why the value is rendered rather than described
///
/// Every other section of this document repeats an author's prose and labels it
/// as the author's, because this crate cannot check a claim about what a program
/// will do. A configuration change is the one thing here that is **not** a
/// claim: the key and the value are the change, so they are shown exactly as
/// they will land and the author's words sit beside them as the *reason* rather
/// than as the description.
///
/// # The sentence about removal is a promise this crate cannot keep
///
/// It is the host that records the prior value and puts it back
/// (`plugin_cmd::configure`), so this line describes behaviour that lives one
/// crate away — the [`oracle_self_report`] situation inverted. It is stated
/// anyway, and stated as a guarantee, because a user deciding whether to accept
/// a config write is deciding almost entirely on whether it is reversible; a
/// document that left it out would be asking for a permanent change while
/// meaning a temporary one. The obligation that creates on the host is
/// deliberate, and `plugin_cmd`'s revert tests are where it is discharged.
fn configuration_changes(manifest: &PluginManifest) -> Vec<String> {
    if manifest.configure.is_empty() {
        return Vec::new();
    }
    let name = one_line(&manifest.name);
    let mut lines = vec![
        String::new(),
        format!(
            "`{name}` also changes this workspace's configuration, setting {} for as \
             long as it is installed:",
            count(manifest.configure.len(), "value")
        ),
    ];
    // Manifest order, not sorted — `capability_grant`'s argument: the author's
    // ordering is information, and a host diffing two versions of a consent
    // document wants a stable rendering rather than a tidied one.
    for entry in &manifest.configure {
        lines.push(format!(
            "      {} = {}",
            one_line(&entry.key),
            one_line(&entry.rendered_value())
        ));
        lines.push(format!("          why: {}", one_line(&entry.purpose)));
    }
    lines.push(
        "  Whatever those keys hold now is recorded first, and `stella plugin remove` \
         puts every one of them back."
            .into(),
    );
    lines
}

/// What a `none`-grade package *is*, once the manifest has been asked.
///
/// A grade of `none` says one thing — this never runs inside a turn — and
/// three different kinds of package satisfy it. The ladder is orthogonal to
/// which one this is (`doc:pipeline-as-plugins` §10), so the sentence is
/// decided here from the tables the manifest actually declares rather than
/// asserted by [`grade_sentence`]:
///
/// - **A content bundle** ships tools, skills or records, and that is said,
///   because it is what a user is accepting.
/// - **A host** declares `[driver]`. [`driver_say`] already prints what it
///   drives and what it may do to the repository between turns, in full, two
///   lines below — so adding "content bundle" here would have the prompt
///   contradict itself, which is what it did between #3637 and #3537.
/// - **Neither**: a `none`-grade package whose whole substance is its
///   capability list. Saying nothing extra is the honest answer; the list is
///   immediately below and it is the entire risk.
///
/// Pure, like everything in this module: manifest fields only, no I/O.
fn what_a_none_grade_is(manifest: &PluginManifest) -> &'static str {
    if manifest.loop_grant.participation != Participation::None || manifest.driver.is_some() {
        return "";
    }
    if manifest.tools.is_empty() && manifest.skills.is_empty() && manifest.records.is_empty() {
        return "";
    }
    ". It is a content bundle"
}

/// `1 tool` / `2 tools` — a count that agrees in number with its noun.
fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// What each grade means, in the user's terms rather than the ladder's.
///
/// **The grade, and only the grade.** `none` used to end "It is a content
/// bundle (skills, commands, agents, tools)", which is a claim about what the
/// package *ships* rather than about its say in a turn, and the ladder cannot
/// see the difference: a content bundle and a host both have zero say
/// (#3537). `plugins/stella-selfdriving` is graded `none`, ships no skills, no
/// tools and no records, and asks for `bash`, `write_file` and an EC2 rig — so
/// a user reading top-down was told "content bundle" and then handed the
/// widest grant in the tree. [`loop_say`] decides that clause from the
/// manifest now, because the manifest is what knows.
fn grade_sentence(participation: Participation) -> &'static str {
    match participation {
        Participation::None => "none — it never runs inside a turn",
        Participation::Observer => {
            "observer — it may watch everything your turns do, and change none of it"
        }
        Participation::Steering => {
            "steering — it acts inside your turns: it may inject context, rewrite a \
             tool call's input, and decide whether a tool call is permitted"
        }
        Participation::Arbiter => {
            "arbiter, the strongest grant — everything steering may do, and it also \
             decides whether your turn is finished"
        }
    }
}

/// The one-clause gloss of a risk grade, restating
/// [`stella_protocol::RiskLevel`]'s own doc comments in the second person.
fn risk_blurb(risk: RiskLevel) -> &'static str {
    match risk {
        RiskLevel::Low => "it observes, and touches nothing outside the process",
        RiskLevel::Medium => {
            "real but bounded and locally reversible: writing inside the workspace, \
             spending a metered call, starting a process"
        }
        RiskLevel::High => {
            "it reaches outside your workspace, or costs something a `git checkout` \
             cannot undo"
        }
        RiskLevel::Destructive => "irreversible by the agent that did it",
    }
}

/// Flatten author-supplied text to one line: whitespace runs collapse to a
/// single space, control characters are dropped, and the result is trimmed.
///
/// The two things being defended against are different. Newlines let a
/// plugin's prose forge a line of the prompt around it — a `description`
/// ending `\n\nIt asks for no tool capabilities.` reads as Stella's own
/// reassurance. Control characters (an ANSI escape, a carriage return) let it
/// repaint or erase the terminal the consent is being given in. Neither is
/// hypothetical for text a third party wrote and a user is about to trust.
fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
        } else if !ch.is_control() {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> PluginManifest {
        PluginManifest::from_toml_str(text).expect("fixture must parse")
    }

    #[test]
    fn a_bundle_with_no_grant_says_so_in_both_halves() {
        let text = consent_text(&parse("name = \"notes\""));
        assert!(text.contains("Install `notes`?"), "{text}");
        assert!(
            text.contains("none — it never runs inside a turn"),
            "{text}"
        );
        assert!(text.contains("It asks for no tool capabilities."), "{text}");
    }

    #[test]
    fn the_headline_grade_is_the_widest_one_asked_for() {
        let capabilities = [
            Capability {
                tool: "read_file".into(),
                risk: RiskLevel::Low,
                purpose: "read the repo".into(),
                scope: Vec::new(),
            },
            Capability {
                tool: "bash".into(),
                risk: RiskLevel::Destructive,
                purpose: "run anything".into(),
                scope: Vec::new(),
            },
            Capability {
                tool: "write_file".into(),
                risk: RiskLevel::Medium,
                purpose: "write the repo".into(),
                scope: Vec::new(),
            },
        ];
        assert_eq!(highest_risk(&capabilities), Some(RiskLevel::Destructive));
        assert_eq!(highest_risk(&[]), None);
    }

    /// A self-declared scope must never read as an enforced one — the whole
    /// difference between informing a user and misleading them.
    #[test]
    fn a_declared_scope_is_labelled_as_the_plugins_claim() {
        let text = consent_text(&parse(
            "name = \"p\"\n\n[[capabilities]]\ntool = \"write_file\"\nrisk = \"destructive\"\npurpose = \"add the shim\"\nscope = [\"path ~/.zshrc\"]",
        ));
        assert!(text.contains("claimed limit: path ~/.zshrc"), "{text}");
        assert!(text.contains("does not verify the claim"), "{text}");
    }

    /// No scope declared, no disclaimer: the sentence must not appear where
    /// there is no claim for it to qualify.
    #[test]
    fn the_scope_disclaimer_appears_only_when_a_scope_was_declared() {
        let text = consent_text(&parse(
            "name = \"p\"\n\n[[capabilities]]\ntool = \"bash\"\nrisk = \"high\"\npurpose = \"run things\"",
        ));
        assert!(!text.contains("does not verify the claim"), "{text}");
    }

    /// **The injection witness.** Author prose is data, and the prompt's line
    /// structure belongs to Stella: a description carrying newlines, a
    /// forged reassurance and an ANSI escape must change no line count and
    /// leave no control byte in the output.
    #[test]
    fn author_prose_cannot_forge_a_line_of_the_prompt() {
        let honest = parse("name = \"p\"\ndescription = \"a plugin\"");
        let hostile = parse(
            "name = \"p\"\ndescription = \"a plugin\\n\\nIt asks for no tool capabilities.\\n\\u001b[2JGRANTED\"",
        );

        let honest_text = consent_text(&honest);
        let hostile_text = consent_text(&hostile);
        assert_eq!(
            honest_text.lines().count(),
            hostile_text.lines().count(),
            "prose changed the prompt's structure:\n{hostile_text}"
        );
        assert!(
            !hostile_text.chars().any(|ch| ch.is_control() && ch != '\n'),
            "a control character survived into the prompt: {hostile_text:?}"
        );
        assert!(
            hostile_text.contains("a plugin It asks for no tool capabilities. [2JGRANTED"),
            "the text is kept, flattened, not dropped: {hostile_text}"
        );
    }

    /// A process the plugin runs, and the environment slice it is handed,
    /// are both grants — so both are shown. An install prompt that named the
    /// argv and stayed silent about `GITHUB_TOKEN` would describe a narrower
    /// grant than the one being given, which is the failure
    /// [`a_declared_scope_is_labelled_as_the_plugins_claim`] guards on the
    /// other half of the document.
    #[test]
    fn the_process_and_the_environment_it_inherits_are_both_disclosed() {
        let text = consent_text(&parse(
            "name = \"p\"\n[loop]\nparticipation = \"observer\"\n\n[runtime]\nargv = [\"python3\", \"main.py\"]\ntimeout_secs = 30\nenv = [\"PATH\", \"GITHUB_TOKEN\"]",
        ));
        assert!(
            text.contains(
                "runs as a process on your machine: `python3 main.py` (killed after 30s)"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "it inherits these environment variables and no others: PATH, GITHUB_TOKEN"
            ),
            "{text}"
        );

        let sealed = consent_text(&parse(
            "name = \"p\"\n[loop]\nparticipation = \"observer\"\n\n[runtime]\nargv = [\"node\"]\ntimeout_secs = 5",
        ));
        assert!(
            sealed.contains("it inherits NO environment variables"),
            "the default-deny answer is stated, not left to omission: {sealed}"
        );
    }

    /// **The #3511 witness.** A user installing a verification plugin is
    /// trusting its honesty about its own work, and the prompt must say so:
    /// the flip and the measurements a verdict turns on come back from the
    /// plugin's process, and neither is run nor re-checked by Stella. This
    /// prompt claimed the opposite for as long as it existed
    /// (`manifest.rs`'s "the HOST runs this; the plugin never grades its own
    /// work"), which is the consent defect Option 2 exists to close.
    #[test]
    fn an_oracle_is_disclosed_as_the_plugins_own_report() {
        let text = consent_text(&parse(
            "name = \"vera\"\n[loop]\nparticipation = \"arbiter\"\nhooks = [\"Stop\"]\n\
             points = [\"after_turn\"]\n\n\
             [requirements]\nwitness = \"a failing test flips to passing\"\n\n\
             [oracle]\ncommand = { argv = [\"vera\", \"verify\"], timeout_secs = 60 }\n\
             flip = \"required\"",
        ));

        assert!(
            text.contains("reports its own evidence"),
            "the prompt must say the evidence is the plugin's own report: {text}"
        );
        assert!(
            text.contains(
                "Stella does not run the oracle itself and does not check what comes \
                 back"
            ),
            "the prompt must say Stella neither runs nor checks it: {text}"
        );
        assert!(
            text.contains("trusting it to report honestly about its own work"),
            "the prompt must name what the user is actually trusting: {text}"
        );
        assert!(
            !text.contains("never grades its own work"),
            "the retired host-run claim must not survive anywhere in the prompt: {text}"
        );
    }

    /// The other half of the witness, on
    /// `the_scope_disclaimer_appears_only_when_a_scope_was_declared`'s
    /// reasoning: no oracle, no self-report paragraph.
    #[test]
    fn the_self_report_disclosure_appears_only_when_an_oracle_was_declared() {
        let text = consent_text(&parse(
            "name = \"p\"\n[loop]\nparticipation = \"observer\"\n\n[runtime]\nargv = [\"node\"]\ntimeout_secs = 5",
        ));
        assert!(!text.contains("reports its own evidence"), "{text}");
    }

    /// **The driver paragraph, family by family** (#3729). This is the
    /// sentence a user reads before granting a plugin permission to push
    /// branches, merge, and spend model calls between their turns, and until
    /// this test nothing would have failed if a refactor had dropped it.
    ///
    /// Every family the block spans is named with its own consent sentence,
    /// and every declared verb is printed beside its family — the rendering
    /// is by family because "pushes branches, opens pull requests, reads CI,
    /// and merges" is what a human weighs, and the verbs are what keeps that
    /// summary checkable against the block.
    #[test]
    fn a_driver_grant_names_every_family_and_the_verbs_under_it() {
        use crate::driver::DriverFamily;

        let manifest = parse(
            "name = \"loop\"\n[loop]\nparticipation = \"none\"\n\n\
             [driver]\ncalls = [\"deliver_open\", \"backlog_next\", \"deliver_merge\"]",
        );
        let text = consent_text(&manifest);

        assert!(
            text.contains(
                "`loop` DRIVES Stella. It is not a participant in a turn — it starts them."
            ),
            "{text}"
        );
        let driver = manifest.driver.as_ref().expect("the block is declared");
        assert_eq!(
            driver.families(),
            vec![DriverFamily::Backlog, DriverFamily::Deliver],
            "the fixture spans two families, or this test proves less than it says"
        );
        for family in driver.families() {
            assert!(
                text.contains(family.consent_sentence()),
                "the `{family}` family's sentence is missing:\n{text}"
            );
        }
        assert!(text.contains("(backlog_next)"), "{text}");
        assert!(
            text.contains("(deliver_open, deliver_merge)"),
            "the verbs are printed beside their family, in declaration order:\n{text}"
        );
        assert!(
            text.contains(
                "performed BY Stella on request: this plugin holds no credential, no provider \
                 and no forge token of its own."
            ),
            "{text}"
        );
    }

    /// The ceiling a user is told about is the one the block declared, and an
    /// absent one gets its own sentence — "as many as the host allows" is a
    /// materially different consent from "up to four".
    #[test]
    fn a_declared_driver_ceiling_is_stated_and_so_is_its_absence() {
        let capped = consent_text(&parse(
            "name = \"p\"\n[loop]\nparticipation = \"none\"\n\n\
             [driver]\ncalls = [\"backlog_next\"]\nmax_calls = 4",
        ));
        assert!(
            capped.contains("asks for up to 4 of those per driver session"),
            "{capped}"
        );
        assert!(!capped.contains("as many of those"), "{capped}");

        let uncapped = consent_text(&parse(
            "name = \"p\"\n[loop]\nparticipation = \"none\"\n\n\
             [driver]\ncalls = [\"backlog_next\"]",
        ));
        assert!(
            uncapped.contains("asks for as many of those per driver session as the host allows"),
            "{uncapped}"
        );
        assert!(!uncapped.contains("asks for up to"), "{uncapped}");
    }

    /// A driver that asks for no capability at all is a coherent declaration,
    /// so it is said — and nothing else from the block is, because there is no
    /// ceiling to state and nothing for Stella to perform on its behalf.
    #[test]
    fn a_driver_that_asks_for_nothing_says_so_and_nothing_more() {
        let text = consent_text(&parse(
            "name = \"p\"\n[loop]\nparticipation = \"none\"\n\n[driver]\ncalls = []",
        ));

        assert!(text.contains("DRIVES Stella"), "{text}");
        assert!(
            text.contains("asks the host for no capability at all"),
            "{text}"
        );
        assert!(!text.contains("per driver session"), "{text}");
        assert!(!text.contains("performed BY Stella on request"), "{text}");
    }

    /// The anti-vacuity half, on
    /// `the_scope_disclaimer_appears_only_when_a_scope_was_declared`'s
    /// reasoning: absent is not empty. A plugin that is not a driver renders
    /// none of the paragraph, so the assertions above are about this block and
    /// not about boilerplate every prompt carries.
    #[test]
    fn a_plugin_that_does_not_drive_renders_no_driving_disclosure() {
        let text = consent_text(&parse(
            "name = \"p\"\n[loop]\nparticipation = \"steering\"\npoints = [\"before_turn\"]",
        ));

        assert!(!text.contains("DRIVES Stella"), "{text}");
        assert!(!text.contains("per driver session"), "{text}");
        assert!(!text.contains("performed BY Stella on request"), "{text}");
    }

    /// **The #3565 witness.** Everything a package ships is in the document
    /// the *crate* renders, so every host shows the same one — and the tool
    /// line says the three things a user most needs: it is executable code,
    /// the model calls it unprompted, and it runs as the plugin.
    ///
    /// Anti-vacuity is the second half: a manifest declaring nothing must
    /// render none of these sentences, or the assertions above would pass on
    /// boilerplate that is always printed.
    #[test]
    fn the_consent_text_names_every_tool_skill_and_record_the_package_ships() {
        let shipping = consent_text(&parse(
            "name = \"vera\"\n\n\
             [[tools]]\nname = \"lint_fix\"\ndescription = \"run the fixer\"\n\n\
             [[skills]]\nslug = \"house-style\"\ndescription = \"how we write\"\n\n\
             [[records]]\nlineage = \"ctx.vera.no-force-push\"\n\
             statement = \"never force-push a shared branch\"\n",
        ));
        for expected in [
            "`vera` also installs:",
            "1 tool the model may call on its own initiative",
            "executable code this package ships",
            "runs as `vera`, not as you",
            "`lint_fix` — run the fixer",
            "1 skill, injected into your prompts",
            "`house-style` — how we write",
            "1 context record, which steer",
            "`ctx.vera.no-force-push` — never force-push a shared branch",
            "can never deny a tool call",
            "stella plugin remove",
        ] {
            assert!(
                shipping.contains(expected),
                "missing {expected:?} in:\n{shipping}"
            );
        }

        let ships_nothing = consent_text(&parse("name = \"vera\""));
        for absent in [
            "also installs:",
            "executable code this package ships",
            "can never deny a tool call",
        ] {
            assert!(
                !ships_nothing.contains(absent),
                "a package that ships nothing must not say {absent:?}:\n{ships_nothing}"
            );
        }
    }

    /// Counts agree in number with their nouns — a consent document that
    /// reads as machine output is one a user skims.
    #[test]
    fn the_contribution_counts_agree_in_number() {
        let text = consent_text(&parse(
            "name = \"p\"\n\n\
             [[tools]]\nname = \"a\"\ndescription = \"d\"\n\n\
             [[tools]]\nname = \"b\"\ndescription = \"d\"\n",
        ));
        assert!(text.contains("2 tools the model may call"), "{text}");
    }

    /// The injection witness, pointed at the new tables: a package's prose is
    /// data here too, and cannot add a line to the document around it.
    #[test]
    fn a_contributions_prose_cannot_forge_a_line_of_the_prompt() {
        let honest = parse("name = \"p\"\n\n[[tools]]\nname = \"t\"\ndescription = \"a tool\"");
        let hostile = parse(
            "name = \"p\"\n\n[[tools]]\nname = \"t\"\n\
             description = \"a tool\\n\\nIt asks for no tool capabilities.\\n\\u001b[2JGRANTED\"",
        );
        let hostile_text = consent_text(&hostile);
        assert_eq!(
            consent_text(&honest).lines().count(),
            hostile_text.lines().count(),
            "prose changed the prompt's structure:\n{hostile_text}"
        );
        assert!(
            !hostile_text.chars().any(|ch| ch.is_control() && ch != '\n'),
            "a control character survived into the prompt: {hostile_text:?}"
        );
    }

    /// **The #3514 tier witness.** A stage list says what a plugin spends
    /// model calls on and said nothing about what those calls cost, so a user
    /// consented to `triage` without being told it runs at a premium tier on
    /// their own key — while `WrapperError::UndeclaredRole` cited this very
    /// document as the thing that authorised the role.
    ///
    /// Anti-vacuity is the second half, on
    /// [`the_scope_disclaimer_appears_only_when_a_scope_was_declared`]'s
    /// reasoning: a manifest declaring no `[roles]` renders none of it.
    #[test]
    fn the_consent_prompt_names_the_tier_a_role_spends_at() {
        let text = consent_text(&parse(
            "name = \"p\"\n[loop]\nparticipation = \"steering\"\n\n\
             [subloop]\nstages = [\"triage\"]\n\n\
             [roles.triage]\ntier = \"premium\"\n",
        ));
        assert!(
            text.contains("spends each stage's model calls at a tier it names:"),
            "{text}"
        );
        assert!(text.contains("triage: the `premium` tier"), "{text}");
        assert!(
            text.contains("your own provider configuration decides what each one resolves to"),
            "a tier is the plugin's ask, not a model this crate can promise: {text}"
        );

        let tierless = consent_text(&parse(
            "name = \"p\"\n[loop]\nparticipation = \"steering\"\n\n[subloop]\nstages = [\"triage\"]\n",
        ));
        assert!(
            !tierless.contains("at a tier it names"),
            "a manifest with no [roles] must render what it always did: {tierless}"
        );
    }

    /// **The #3514 disclosure witness.** The document enumerated powers and
    /// never the data, so a plugin whose process is handed the user's prompt
    /// and the model's whole reply rendered "It asks for no tool
    /// capabilities." — which reads as harmless. For a BYOK product whose
    /// promise is that prompts stay on the machine, this is the disclosure
    /// that matters most.
    ///
    /// The assertions are on the disclosure's own fixed phrasing, which lives
    /// in [`crate::wire::WIRE_FIELDS`] beside the fields it describes.
    #[test]
    fn a_wrapper_prompt_says_what_leaves_the_machine() {
        let text = consent_text(&parse(
            "name = \"vera\"\n[loop]\nparticipation = \"steering\"\n\
             points = [\"before_turn\", \"after_turn\"]\n\n\
             [wrapper]\nid = \"lite-v1\"\n[[wrapper.stages]]\nname = \"execute\"\n\n\
             [runtime]\nargv = [\"python3\", \"main.py\"]\ntimeout_secs = 30\n",
        ));
        for expected in [
            "Stella hands `vera`'s own process your work, as JSON on that process's standard \
             input. What crosses:",
            "before each turn runs (`before_turn`):",
            "the goal you typed for this turn, in full",
            "once each turn has finished (`after_turn`):",
            "the model's full reply for the turn — the same text you were shown",
            "the name of every tool the turn ran, in call order",
            "the workspace-relative path of every file the turn changed",
            "can still send every line of it anywhere it can reach",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }

        let no_process = consent_text(&parse(
            "name = \"p\"\n[loop]\nparticipation = \"steering\"\npoints = [\"before_turn\"]\n",
        ));
        assert!(
            !no_process.contains("as JSON on that process's standard input"),
            "a plugin with no process of its own is handed nothing: {no_process}"
        );
    }

    /// The other half of the filter: `permits_point` is what
    /// `stella_runtime::wrapper` dispatches on, so a plugin that declares a
    /// process and answers no point is handed none of this — and must not be
    /// disclosed as though it were.
    #[test]
    fn a_process_that_answers_no_point_discloses_no_data() {
        let text = consent_text(&parse(
            "name = \"p\"\n[loop]\nparticipation = \"observer\"\n\n\
             [runtime]\nargv = [\"node\"]\ntimeout_secs = 5\n",
        ));
        assert!(!text.contains("What crosses:"), "{text}");
        assert!(!text.contains("the goal you typed for this turn"), "{text}");
    }

    #[test]
    fn rendering_is_deterministic() {
        let manifest = parse(
            "name = \"p\"\n[loop]\nparticipation = \"steering\"\nhooks = [\"PreToolUse\", \"PostToolUse\"]\n\n[[capabilities]]\ntool = \"bash\"\nrisk = \"high\"\npurpose = \"run things\"",
        );
        assert_eq!(consent_text(&manifest), consent_text(&manifest));
    }

    #[test]
    fn one_line_collapses_runs_and_trims() {
        assert_eq!(one_line("  a \n\n b\tc  "), "a b c");
        assert_eq!(one_line("\u{1b}[31mred\u{1b}[0m"), "[31mred[0m");
        assert_eq!(one_line("   "), "");
    }

    /// A stage a plugin invented must not read as one Stella has always had
    /// (#3963). The vocabulary opened, so the name alone no longer tells a
    /// reader which of the two it is — and consent is exactly the place that
    /// distinction has to survive.
    #[test]
    fn a_contributed_stage_is_named_as_the_plugins_own() {
        let text = consent_text(&parse(
            "name = \"p\"\n[loop]\nparticipation = \"steering\"\n[wrapper]\nid = \"lite-v1\"\n\
             [[wrapper.stages]]\nname = \"triage-lite\"\n[[wrapper.stages]]\nname = \"execute\"\n",
        ));
        assert!(
            text.contains("triage-lite (this plugin's own stage)"),
            "{text}"
        );
        assert!(
            !text.contains("execute (this plugin's own stage)"),
            "a host stage is Stella's, and saying otherwise would be the same \
             misreading in the other direction: {text}"
        );
    }
}
