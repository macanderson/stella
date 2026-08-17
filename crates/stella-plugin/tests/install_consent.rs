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

const REVIEW_PACKAGE: &str = include_str!("fixtures/review-package.toml");

fn review_package() -> PluginManifest {
    PluginManifest::from_toml_str(REVIEW_PACKAGE).expect("a package manifest must load")
}

/// **The #3565 witness.** A package's tools, skills and records are in the
/// document **this crate** renders, so a `stella-serve` host, an embedded host
/// and `stella plugin install` all show the same one.
///
/// This is the sharpest gap the consent surface has had. A contributed tool is
/// executable code entering the agent's tool surface, which the model may call
/// on its own initiative for the rest of the session — and until the manifest
/// could declare it, `consent_text` had no way to see it, so an embedding host
/// showed a document that omitted it entirely. Only `stella plugin install`
/// rendered an inventory of its own, off the directory.
#[test]
fn the_consent_text_shows_everything_the_package_ships() {
    let manifest = review_package();
    let text = consent_text(&manifest);

    for tool in &manifest.tools {
        assert!(
            text.contains(&tool.name) && text.contains(tool.description.trim()),
            "`{}` is missing from the consent text:\n{text}",
            tool.name
        );
    }
    for skill in &manifest.skills {
        assert!(text.contains(&skill.slug), "{text}");
    }
    for record in &manifest.records {
        assert!(text.contains(&record.lineage), "{text}");
        assert!(text.contains(record.statement.trim()), "{text}");
    }

    // A tool is not merely listed — the three facts a user needs are said.
    assert!(
        text.contains("executable code this package ships"),
        "the consent text must say a contributed tool is code:\n{text}"
    );
    assert!(
        text.contains("the model may call on its own initiative"),
        "and that nothing asks the user first:\n{text}"
    );
    assert!(
        text.contains("runs as `vera`, not as you"),
        "and that it is authorized as the plugin:\n{text}"
    );
    // A record's power is different in kind, and is disclosed as such.
    assert!(
        text.contains("steer every future turn"),
        "a contributed record's quiet reach must be stated:\n{text}"
    );
    assert!(
        text.contains("can never deny a tool call"),
        "and its ceiling, so a user is not told it may block:\n{text}"
    );
}

/// **The reconciliation witness (#3565 item 2).** The declaration is checked
/// against the host's own read of the package, in both directions, so the
/// document above is provably complete rather than merely well-intentioned.
#[test]
fn a_package_whose_declaration_and_directories_disagree_is_refused() {
    let manifest = review_package();
    let honest = stella_plugin::PackageListing {
        tools: vec!["vera_review".into()],
        skills: vec!["house-style".into()],
        records: vec!["ctx.acme.web.no-force-push".into()],
    };
    manifest
        .reconcile(&honest)
        .expect("a package that ships exactly what it declares installs");

    let mut smuggled = honest.clone();
    smuggled.tools.push("deploy".into());
    let refusal = manifest
        .reconcile(&smuggled)
        .expect_err("a tool no [[tools]] entry names cannot be consented to")
        .to_string();
    assert!(refusal.contains("deploy"), "{refusal}");

    let refusal = manifest
        .reconcile(&stella_plugin::PackageListing::empty())
        .expect_err("a declaration with nothing behind it is refused too")
        .to_string();
    assert!(refusal.contains("vera_review"), "{refusal}");
    assert!(refusal.contains("house-style"), "{refusal}");
    assert!(refusal.contains("ctx.acme.web.no-force-push"), "{refusal}");
}

/// A package's records may steer and may never deny. Enforcement is granted
/// only through the repository's own hash-chained promotion ledger, and a
/// package's files are outside the tree that ledger is verified against —
/// so the refusal happens at load, before any host has to remember the rule.
#[test]
fn a_package_may_not_ship_a_record_that_denies_a_tool_call() {
    let err = PluginManifest::from_toml_str(
        "name = \"vera\"\n\n[[records]]\nlineage = \"ctx.acme.web.no-force-push\"\n\
         statement = \"Never force-push to a shared branch.\"\nenforcement = \"blocking\"",
    )
    .unwrap_err();
    assert!(
        matches!(err, stella_plugin::ManifestError::PluginRecordCannotEnforce { ref lineage }
            if lineage == "ctx.acme.web.no-force-push"),
        "got {err:?}"
    );
}

/// Invariant 4 for the package tables too: what a host renders consent from
/// must survive the wire a `stella-serve` host reads it over.
#[test]
fn a_package_declaration_round_trips_through_toml_and_json() {
    let parsed = review_package();
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
