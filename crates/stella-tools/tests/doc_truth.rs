//! Doc-truth guardrails: shipped prose must not contradict the tool surface
//! the crate actually registers.
//!
//! #710 moved `bash` and the key-free web family to registered-by-default, and
//! `registry`'s `bash_is_registered_with_no_options_at_all` /
//! `the_key_free_web_family_is_registered_with_no_options_at_all` pin that in
//! code. The prose was never swept, so for every release after #710 the README,
//! `AGENTS.md`, the threat model, `docs/why-stella.md` and the model's own
//! system prompt all still said the shell was opt-in and that the default
//! surface had no shell at all (#615).
//!
//! That is worth a gate rather than a one-time fix. A false claim in a threat
//! model is worse than no claim — a reader budgets their risk against it — and
//! a false claim in the system prompt is read by the model every turn, which is
//! how "there is no shell unless the workspace enables it" survived a release
//! in which the shell was always present.
//!
//! These assert the *enabling* spelling is absent. Any doc telling a reader to
//! switch the shell ON to get one has inverted the posture; `"off"` is the only
//! correct instruction.

/// Every operator-facing statement of the shell's default posture.
const DOCS: &[(&str, &str)] = &[
    ("README.md", include_str!("../../../README.md")),
    ("AGENTS.md", include_str!("../../../AGENTS.md")),
    (
        "crates/stella-tools/README.md",
        include_str!("../README.md"),
    ),
    (
        "docs/why-stella.md",
        include_str!("../../../docs/why-stella.md"),
    ),
    (
        "docs/spec/threat-model.md",
        include_str!("../../../docs/spec/threat-model.md"),
    ),
    (
        "docs/spec/semantic-resolution-bridge.md",
        include_str!("../../../docs/spec/semantic-resolution-bridge.md"),
    ),
];

/// The enabling spellings, in the JSON and dotted forms both, plus the two
/// prose phrasings that shipped.
const INVERTED: &[&str] = &[
    r#""bash": "on""#,
    r#"bash": "on""#,
    r#"tools.bash: "on""#,
    "bash` is opt-in",
    "Bash is opt-in",
    "opt-in shell",
];

#[test]
fn shipped_docs_do_not_claim_the_shell_is_opt_in() {
    for (label, text) in DOCS {
        for claim in INVERTED {
            assert!(
                !text.contains(claim),
                "{label} still tells the reader to enable the shell ({claim:?}), but `bash` \
                 has been registered by default since #710 — see stella-tools' \
                 `bash_is_registered_with_no_options_at_all`. The switch is `\"bash\": \"off\"`."
            );
        }
    }
}

/// The same claim reached the model, not just the operator: the shared
/// `tool_steering!` block told every session the workspace had to enable the
/// shell. Asserted here rather than in `stella-cli` because this crate owns the
/// fact being described, and because the prompt's own test previously *pinned*
/// the false sentence.
#[test]
fn the_system_prompt_states_the_switch_in_its_real_polarity() {
    let prompt = include_str!("../../stella-cli/src/agent/prompt.rs");
    assert!(
        !prompt.contains("There is no shell unless the workspace enables it"),
        "the system prompt still tells the model there is no shell by default; \
         `bash` is registered by default since #710"
    );
    assert!(
        prompt.contains(r#"tools": {"bash": "off"}"#),
        "the system prompt should name the real switch (`\"bash\": \"off\"`) so a model \
         asked to bound its own surface gives the operator a working instruction"
    );
}
