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
use crate::manifest::{OracleProcessSource, Participation, PluginManifest};

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
    lines.extend(oracle_self_report(manifest));
    lines.push(String::new());
    lines.extend(capability_grant(manifest));
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
        "Say in your turn loop: {}",
        grade_sentence(grant.participation)
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

    if let Some(subloop) = &manifest.subloop {
        let stages: Vec<String> = subloop.stages.iter().map(|stage| one_line(stage)).collect();
        lines.push(format!(
            "  - spends model calls on these stages as child turns: {}",
            stages.join(", ")
        ));
    }

    if let Some(wrapper) = &manifest.wrapper {
        let stages: Vec<String> = wrapper
            .stages
            .iter()
            .map(|stage| stage.name.to_string())
            .collect();
        lines.push(format!(
            "  - wraps every turn as variant `{}`, running: {}",
            one_line(&wrapper.id),
            stages.join(", ")
        ));
    }

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

/// What each grade means, in the user's terms rather than the ladder's.
fn grade_sentence(participation: Participation) -> &'static str {
    match participation {
        Participation::None => {
            "none — it never runs inside a turn. It is a content bundle (skills, \
             commands, agents, tools)"
        }
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
}
