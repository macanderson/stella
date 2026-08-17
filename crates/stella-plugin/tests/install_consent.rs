//! A1's acceptance (`doc:pipeline-as-plugins` §4 A1, run playbook §3 D-3):
//! the widest grant anyone will ask for is **expressible** in the manifest
//! vocabulary and **showable** to a human before install.
//!
//! The requirement A1 carries is not "may self-driving have `gh`, AWS, `brew`
//! and `~/.zshrc`" — D-3 already settled that making it a plugin relocates
//! authority it holds today rather than granting anything new. The
//! requirement is that the authority system can *say* it and a user can *see*
//! it, which is what these tests hold. `fixtures/self-driving.toml` is the
//! declaration; [`stella_plugin::consent_text`] is the seeing.
//!
//! Read alongside `stella-core`'s
//! `a_plugin_is_a_principal_of_its_own_and_never_reads_as_its_host`: that test
//! is the other half — the caller such a grant is attributed to.

use stella_plugin::{PluginManifest, RiskLevel, consent_text, highest_risk};

const SELF_DRIVING: &str = include_str!("fixtures/self-driving.toml");

fn self_driving() -> PluginManifest {
    PluginManifest::from_toml_str(SELF_DRIVING).expect("the widest grant must be expressible")
}

/// **Expressible.** Every capability §10 names is declarable, and the grade
/// vocabulary reaches the top: an irreversible act is `destructive`, not the
/// highest thing the type happens to spell.
#[test]
fn the_widest_grant_is_expressible_in_the_manifest_vocabulary() {
    let manifest = self_driving();
    let tools: Vec<&str> = manifest
        .capabilities
        .iter()
        .map(|capability| capability.tool.as_str())
        .collect();
    assert_eq!(
        tools,
        ["bash", "write_file", "mcp__aws__ec2", "process_spawn"],
        "declaration order is preserved"
    );
    assert_eq!(
        highest_risk(&manifest.capabilities),
        Some(RiskLevel::Destructive)
    );
}

/// **Showable.** The consent text names every tool, every stated purpose and
/// every claimed limit — nothing is elided, summarised away, or hidden behind
/// a "and 3 more". A grant a user cannot read in full is not consented to.
#[test]
fn the_consent_text_shows_every_capability_it_declares() {
    let manifest = self_driving();
    let text = consent_text(&manifest);

    for capability in &manifest.capabilities {
        assert!(
            text.contains(&capability.tool),
            "`{}` is missing from the consent text:\n{text}",
            capability.tool
        );
        assert!(
            text.contains(capability.purpose.trim()),
            "the purpose of `{}` is missing from the consent text:\n{text}",
            capability.tool
        );
        for scope in &capability.scope {
            assert!(
                text.contains(scope.trim()),
                "a claimed limit of `{}` is missing from the consent text:\n{text}",
                capability.tool
            );
        }
    }

    assert!(
        text.contains("graded DESTRUCTIVE"),
        "the worst grade must be the headline, not a thing to find:\n{text}"
    );
}

/// The loop half is shown too, and specifically the powers that are easy to
/// grant without noticing: a plugin that can refuse to let your turn end, and
/// one that runs a program of its own.
#[test]
fn the_consent_text_shows_the_powers_inside_the_turn() {
    let text = consent_text(&self_driving());

    assert!(text.contains("arbiter, the strongest grant"), "{text}");
    assert!(
        text.contains("may refuse to let a finished turn end, up to 3 times per turn"),
        "{text}"
    );
    assert!(text.contains("Stop, PreToolUse, PostToolUse"), "{text}");
    assert!(
        text.contains("${plugin_dir}/bin/selfdriving-oracle verify"),
        "the program it runs is named, with its argv:\n{text}"
    );
    assert!(text.contains("killed after 900s"), "{text}");
}

/// The consent text tells the user the thing `Principal::Plugin` exists to
/// make true: this caller is not them. Without that sentence the prompt reads
/// as "may I do these things on your behalf", which is a different question.
#[test]
fn the_consent_text_says_the_plugin_is_a_caller_of_its_own() {
    let text = consent_text(&self_driving());
    assert!(text.contains("attributed to it, not to you"), "{text}");
    assert!(
        text.contains("Nothing above is granted until you accept."),
        "{text}"
    );
}

/// A capability with no stated reason is not consentable, so it does not
/// load — the rule that keeps a consent prompt from listing a bare tool name.
#[test]
fn a_capability_with_no_stated_purpose_does_not_load() {
    let err = PluginManifest::from_toml_str(
        "name = \"p\"\n\n[[capabilities]]\ntool = \"bash\"\nrisk = \"high\"\npurpose = \"  \"",
    )
    .unwrap_err();
    assert!(
        matches!(err, stella_plugin::ManifestError::EmptyCapabilityPurpose { ref tool } if tool == "bash"),
        "got {err:?}"
    );
}

/// One entry per tool. Two entries for `bash` would make the effective grant
/// the union of two lines a user read separately — the exact shape a wide
/// grant hides in.
#[test]
fn the_same_tool_cannot_be_requested_twice() {
    let err = PluginManifest::from_toml_str(
        "name = \"p\"\n\n[[capabilities]]\ntool = \"bash\"\nrisk = \"low\"\npurpose = \"read logs\"\n\n[[capabilities]]\ntool = \"bash\"\nrisk = \"destructive\"\npurpose = \"everything else\"",
    )
    .unwrap_err();
    assert!(
        matches!(err, stella_plugin::ManifestError::DuplicateCapability { ref tool } if tool == "bash"),
        "got {err:?}"
    );
}

/// Invariant 4, for the type that will cross the host boundary as JSON when a
/// host renders install consent over the serve wire.
#[test]
fn the_widest_grant_round_trips_through_toml_and_json() {
    let parsed = self_driving();

    let toml_text = toml::to_string(&parsed).unwrap();
    assert_eq!(
        PluginManifest::from_toml_str(&toml_text).unwrap(),
        parsed,
        "TOML round-trip diverged"
    );

    let json = serde_json::to_string(&parsed).unwrap();
    assert_eq!(
        serde_json::from_str::<PluginManifest>(&json).unwrap(),
        parsed,
        "JSON round-trip diverged"
    );
}
