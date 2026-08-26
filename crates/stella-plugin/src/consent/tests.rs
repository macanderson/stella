// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Tests for [`crate::consent`] — what a human is shown before they say yes.
//!
//! A sibling file rather than an inline `mod tests`, for the reason AGENTS.md
//! § "God files" gives: the parent carries the vocabulary, the validation and
//! the whole rendering, and this suite grew past what the same file can hold
//! under the 1500-line ratchet.

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
    let plain = parse("name = \"p\"\ndescription = \"a plugin\"");
    let hostile = parse(
        "name = \"p\"\ndescription = \"a plugin\\n\\nIt asks for no tool capabilities.\\n\\u001b[2JGRANTED\"",
    );

    let plain_text = consent_text(&plain);
    let hostile_text = consent_text(&hostile);
    assert_eq!(
        plain_text.lines().count(),
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
        text.contains("runs as a process on your machine: `python3 main.py` (killed after 30s)"),
        "{text}"
    );
    assert!(
        text.contains("it inherits these environment variables and no others: PATH, GITHUB_TOKEN"),
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
/// trusting its own report of its own work, and the prompt must say so:
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
        text.contains("trusting it to report"),
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
        text.contains("`loop` DRIVES Stella. It is not a participant in a turn — it starts them."),
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

/// A driver's process is the program a host will actually start, so it is
/// disclosed with the same two sentences `[runtime]` gets — and its
/// absence is disclosed too, because "Stella starts this" and "a person
/// starts this" are the two things a reader is deciding between (#3783).
#[test]
fn a_driver_that_stella_can_start_says_what_runs_and_what_it_inherits() {
    let started = consent_text(&parse(
        "name = \"p\"\n[loop]\nparticipation = \"none\"\n\n[driver]\n\
         calls = [\"backlog_next\"]\n\n[driver.process]\n\
         argv = [\"python3\", \"main.py\"]\ntimeout_secs = 600\nenv = [\"HOME\"]",
    ));
    assert!(
        started
            .contains("runs as a process on your machine: `python3 main.py` (killed after 600s)"),
        "{started}"
    );
    assert!(
        started.contains("it inherits these environment variables and no others: HOME"),
        "{started}"
    );
    assert!(!started.contains("is not started by Stella"), "{started}");

    // The other answer, which is what `plugins/stella-selfdriving` is.
    let declared_only = consent_text(&parse(
        "name = \"p\"\n[loop]\nparticipation = \"none\"\n\n[driver]\ncalls = [\"backlog_next\"]",
    ));
    assert!(
        declared_only.contains("is not started by Stella"),
        "{declared_only}"
    );
    assert!(
        !declared_only.contains("runs as a process on your machine"),
        "{declared_only}"
    );
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
    let plain = parse("name = \"p\"\n\n[[tools]]\nname = \"t\"\ndescription = \"a tool\"");
    let hostile = parse(
        "name = \"p\"\n\n[[tools]]\nname = \"t\"\n\
         description = \"a tool\\n\\nIt asks for no tool capabilities.\\n\\u001b[2JGRANTED\"",
    );
    let hostile_text = consent_text(&hostile);
    assert_eq!(
        consent_text(&plain).lines().count(),
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
