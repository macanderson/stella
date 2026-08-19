//! What a plugin *configures* — the settings half of the manifest (#3999).
//!
//! [`crate::package`] answers "what arrives on my machine besides a process?".
//! This module answers the question a downstream distribution asks next:
//! **what does installing this change about how stella already behaves?**
//!
//! The concrete case is the one that produced the issue. A distribution needs
//! stella to sign commits, pull requests and issues in its own name rather than
//! stella's, and the signature is configuration — so the change is a config
//! write, not a fork. Before this table there was no way for a package to make
//! that write: installing a plugin copied a declaration and a README, and
//! nothing ran. The remedy a distribution was left with was "tell your users to
//! hand-edit `stella.toml`", which they do not do, and which nothing undoes when
//! the plugin is removed.
//!
//! # Declarative, so the revert is exact
//!
//! A `[[configure]]` entry is **data, not a script**. That is the whole design,
//! and it buys the one property an install hook has to have:
//!
//! - **The consent document can state it.** A script's effect can only be
//!   described by its author; a key and a value can be *shown*, and what a human
//!   reads at install is the literal change.
//! - **Uninstall can undo it.** The host records what each declared key held
//!   before it wrote, and `stella plugin remove` puts exactly that back. A hook
//!   that wrote config and an uninstall that did not restore it would leave a
//!   workspace signing in the name of a plugin it no longer has.
//! - **It cannot do a second thing.** An arbitrary install script is an
//!   unbounded grant wearing a narrow purpose; a list of key/value pairs is
//!   bounded by construction.
//!
//! # One entry is one leaf, never a table
//!
//! [`ConfigureEntry::value`] is a scalar or an array of scalars, and a table is
//! a load error ([`ManifestError::ConfigureValueNotScalar`]). Two reasons, and
//! both are about the revert:
//!
//! - A table-valued entry would let one consented line replace a whole section,
//!   including keys the human never read — the same completeness defect
//!   [`crate::package`]'s reconciliation exists to prevent, on the config
//!   surface.
//! - Restoring a leaf is exact: it was present with a value, or it was absent.
//!   Restoring a section means diffing two trees and re-merging, which is
//!   precisely the operation that gets a revert subtly wrong.
//!
//! # This crate declares; it never writes
//!
//! Nothing here touches a file — the crate performs no I/O (#3245 §3), so
//! validating a declaration and applying one are deliberately different jobs in
//! different crates. The host reads `stella.toml`, applies, journals the prior
//! values and reverts; this module decides only whether a declaration is one a
//! human could be asked to accept.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::error::ManifestError;
use crate::manifest::PluginManifest;

/// One configuration key a package sets for as long as it is installed.
///
/// Declared in `[[configure]]`, rendered verbatim by [`crate::consent_text`],
/// applied by the host at install and reverted at removal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigureEntry {
    /// The dotted path of the key in `stella.toml` —
    /// `self_driving.attribution.signature`, not `[self_driving.attribution]`.
    ///
    /// Every segment is a TOML *bare* key (`A-Za-z0-9_-`), enforced by
    /// [`validate_configure`]. Quoted and dotted-inside-quotes forms are
    /// refused rather than supported: they exist so TOML can express keys with
    /// spaces and dots in them, and a package that needs one is asking to write
    /// a key this configuration surface does not have.
    pub key: String,
    /// The value to set — a scalar, or an array of scalars. A table is a load
    /// error; see this module's header for why.
    pub value: toml::Value,
    /// Why the package needs it, in the author's words. Shown at consent
    /// beside the literal key and value.
    ///
    /// Required rather than optional, on
    /// [`ToolContribution::description`](crate::ToolContribution)'s reasoning:
    /// "this package sets `self_driving.attribution.signature`" is not
    /// something a human can consent to. The key and value say *what*; only the
    /// author can say *why*.
    pub purpose: String,
}

impl ConfigureEntry {
    /// The top-level section this key writes into — the first dotted segment.
    ///
    /// Borrowed from [`Self::key`] rather than stored, because a second copy of
    /// a prefix is a second thing that can disagree with the key it came from.
    #[must_use]
    pub fn section(&self) -> &str {
        self.key.split('.').next().unwrap_or(&self.key)
    }

    /// The key's segments, in order — what a host walks to reach the leaf.
    ///
    /// Always at least two for an entry that loaded: a single-segment key is
    /// refused by [`validate_configure`], because a bare root key is not a leaf
    /// inside a section and this surface has none worth writing.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.key.split('.')
    }

    /// The value as one line, for a consent document — TOML's own rendering, so
    /// what a human reads is what the file will hold.
    ///
    /// `toml` serializes a bare value only as part of a document, so this goes
    /// through a one-key table and takes the right-hand side. The fallback is
    /// deliberately the debug form rather than a panic: a consent line that
    /// renders a value oddly is a cosmetic defect, and refusing to print the
    /// document at all would be a real one.
    #[must_use]
    pub fn rendered_value(&self) -> String {
        let mut table = toml::map::Map::new();
        table.insert("v".to_string(), self.value.clone());
        toml::to_string(&toml::Value::Table(table))
            .ok()
            .and_then(|rendered| {
                rendered
                    .split_once('=')
                    .map(|(_, value)| value.trim().to_string())
            })
            .filter(|rendered| !rendered.is_empty())
            .unwrap_or_else(|| format!("{:?}", self.value))
    }
}

/// The `stella.toml` sections a package may never write, and why each one.
///
/// # Why a refusal list rather than an allow list
///
/// The real gate on a `[[configure]]` entry is the human who reads the key, the
/// value and the purpose at install and says yes. An allow list would add a
/// second gate that has to be widened for every legitimate new key — and the
/// motivating case, a distribution's own attribution, would have been blocked
/// by it until someone remembered to add a line.
///
/// So the list below is not "the dangerous settings". It is the narrower and
/// more defensible thing: **the sections where a consent line cannot honestly
/// convey the effect.** `providers.anthropic.base_url = "https://…"` reads as a
/// routine endpoint and silently redirects every model call and the credential
/// that pays for it; `authority.project_prompts = "off"` reads as a preference
/// and disarms the gate that decides whether a repository may steer a session
/// at all. Consent is not meaningful there, so the answer is no rather than a
/// louder prompt.
///
/// **The rule for adding an entry** is that sentence, not a feeling about risk:
/// a section belongs here when a reader who understood the line would still not
/// understand what it does. A section that is merely consequential —
/// `agents.default_model` changes what a turn costs — stays writable, because
/// the consent line says so plainly and the human is the right decider.
///
/// This is a denial list, so it fails *open*: a new section is writable until
/// someone puts it here. That is the cost of the choice above and it is real —
/// `configure_refusals_each_state_a_reason` keeps the reasons reviewable, but
/// nothing can make the list complete on its own.
pub const REFUSED_SECTIONS: &[(&str, &str)] = &[
    (
        "providers",
        "it names model endpoints and the credential that pays for them, so a line \
         a reader would accept as routine can redirect every model call this \
         machine makes",
    ),
    (
        "authority",
        "it is the ceiling that decides whether a repository may steer a session \
         at all, and a package switching it reads as a preference rather than as \
         the disarming it is",
    ),
    (
        "hooks",
        "an entry there is a shell command run on the agent's own events, which is \
         the arbitrary install script this table exists to be instead of",
    ),
    (
        "plugins",
        "it is the switch that retracts a plugin, so a package writing it could \
         switch itself on against the user's own setting — or switch another \
         package off",
    ),
    (
        "mcp",
        "it names external tool servers whose tools join the agent's surface, so \
         the effect of one line is a set of tools the consent document never \
         listed",
    ),
    (
        "meta",
        "it declares the schema version and scope the rest of the file is read \
         under, so writing it changes the meaning of settings the package never \
         named",
    ),
];

/// Whether `value` is a scalar or an array of scalars — the shape one revertible
/// leaf can hold.
fn is_leaf_value(value: &toml::Value) -> bool {
    match value {
        toml::Value::String(_)
        | toml::Value::Integer(_)
        | toml::Value::Float(_)
        | toml::Value::Boolean(_)
        | toml::Value::Datetime(_) => true,
        // One level of nesting only. An array of arrays is still not a table,
        // but it is no longer something a consent line renders as one readable
        // value, which is the property this is really guarding.
        toml::Value::Array(items) => items.iter().all(|item| {
            !matches!(item, toml::Value::Table(_) | toml::Value::Array(_)) && is_leaf_value(item)
        }),
        toml::Value::Table(_) => false,
    }
}

/// A TOML bare key: what a dotted segment must be to navigate a real table.
fn is_bare_key(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The cross-field rules for `[[configure]]`, kept beside the type they govern
/// exactly as [`crate::package`]'s and [`crate::consent`]'s are.
pub(crate) fn validate_configure(manifest: &PluginManifest) -> Result<(), ManifestError> {
    let mut seen = HashSet::with_capacity(manifest.configure.len());
    for entry in &manifest.configure {
        let key = entry.key.trim();
        if key.is_empty() {
            return Err(ManifestError::EmptyConfigureKey);
        }
        if entry.purpose.trim().is_empty() {
            return Err(ManifestError::EmptyConfigurePurpose {
                key: entry.key.clone(),
            });
        }

        let segments: Vec<&str> = key.split('.').collect();
        if !segments.iter().all(|segment| is_bare_key(segment)) {
            return Err(ManifestError::ConfigureKeyNotDotted {
                key: entry.key.clone(),
            });
        }
        // A single segment is a whole top-level section, not a leaf inside one.
        // Refused for `is_leaf_value`'s reason applied to the other side of the
        // `=`: this surface writes leaves, so that a revert is "put this one
        // value back" rather than "reconstruct this table".
        if segments.len() < 2 {
            return Err(ManifestError::ConfigureKeyNotDotted {
                key: entry.key.clone(),
            });
        }

        if let Some((_, reason)) = REFUSED_SECTIONS
            .iter()
            .find(|(section, _)| *section == segments[0])
        {
            return Err(ManifestError::ConfigureSectionRefused {
                key: entry.key.clone(),
                section: segments[0].to_string(),
                reason: (*reason).to_string(),
            });
        }

        if !is_leaf_value(&entry.value) {
            return Err(ManifestError::ConfigureValueNotScalar {
                key: entry.key.clone(),
            });
        }

        if !seen.insert(key) {
            return Err(ManifestError::DuplicateConfigureKey {
                key: entry.key.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<PluginManifest, ManifestError> {
        PluginManifest::from_toml_str(text)
    }

    /// The motivating declaration, end to end: a distribution's attribution
    /// override loads, keeps the author's value type, and is reachable by
    /// section.
    ///
    /// **Witness for #3999, half one.** Before this table a manifest could not
    /// say this at all.
    #[test]
    fn a_package_declares_the_configuration_it_needs() {
        let manifest = parse(
            "name = \"oxagen\"\n\n\
             [[configure]]\n\
             key = \"self_driving.attribution.signature\"\n\
             value = \"Generated by Oxagen\"\n\
             purpose = \"sign this workspace's commits in the distribution's name\"\n",
        )
        .expect("a configure declaration must load");
        assert_eq!(manifest.configure.len(), 1);
        let entry = &manifest.configure[0];
        assert_eq!(entry.key, "self_driving.attribution.signature");
        assert_eq!(entry.section(), "self_driving");
        assert_eq!(
            entry.segments().collect::<Vec<_>>(),
            ["self_driving", "attribution", "signature"]
        );
        assert_eq!(entry.rendered_value(), "\"Generated by Oxagen\"");
    }

    /// Every scalar TOML can hold survives the round trip and renders as itself
    /// — the consent document shows the value the file will hold, so a type
    /// this surface silently stringified would make the document a paraphrase.
    #[test]
    fn every_scalar_shape_loads_and_renders_as_itself() {
        for (literal, rendered) in [
            ("\"text\"", "\"text\""),
            ("7", "7"),
            ("true", "true"),
            ("[\"a\", \"b\"]", "[\"a\", \"b\"]"),
        ] {
            let manifest = parse(&format!(
                "name = \"x\"\n\n[[configure]]\nkey = \"a.b\"\nvalue = {literal}\npurpose = \"p\"\n"
            ))
            .unwrap_or_else(|error| panic!("{literal} must load: {error}"));
            assert_eq!(manifest.configure[0].rendered_value(), rendered);
        }
    }

    /// **The bound that makes the revert exact.** One entry is one leaf, so a
    /// consented line can never replace a section holding keys nobody read.
    #[test]
    fn a_table_valued_entry_is_refused() {
        let err = parse(
            "name = \"x\"\n\n[[configure]]\nkey = \"a.b\"\npurpose = \"p\"\n\
             [configure.value]\nnested = 1\n",
        )
        .unwrap_err();
        assert!(
            matches!(err, ManifestError::ConfigureValueNotScalar { ref key } if key == "a.b"),
            "{err:?}"
        );
        let text = err.to_string();
        assert!(text.contains("one key at a time"), "{text}");
    }

    /// The sections where consent cannot be meaningful are refused by name, and
    /// the refusal says which section and why — an author reading it can tell
    /// "not this key" from "not this way".
    #[test]
    fn a_refused_section_is_named_with_its_reason() {
        for (section, key) in [
            ("providers", "providers.anthropic.base_url"),
            ("authority", "authority.project_prompts"),
            ("hooks", "hooks.PreToolUse"),
            ("plugins", "plugins.oxagen"),
            ("mcp", "mcp.servers"),
            ("meta", "meta.schema_version"),
        ] {
            let err = parse(&format!(
                "name = \"x\"\n\n[[configure]]\nkey = \"{key}\"\nvalue = \"v\"\npurpose = \"p\"\n"
            ))
            .unwrap_err();
            let ManifestError::ConfigureSectionRefused {
                section: refused,
                reason,
                ..
            } = &err
            else {
                panic!("{key} must be refused by section: {err:?}");
            };
            assert_eq!(refused, section);
            assert!(!reason.is_empty(), "{key} refused with no reason");
            assert!(err.to_string().contains(section), "{err}");
        }
    }

    /// A section that is merely consequential stays writable: the consent line
    /// says what it does, and the human is the right decider. This is the other
    /// half of [`REFUSED_SECTIONS`]'s argument, and without it the list would
    /// drift toward "everything that could matter".
    #[test]
    fn a_consequential_but_legible_section_is_not_refused() {
        parse(
            "name = \"x\"\n\n[[configure]]\nkey = \"agents.default_model\"\n\
             value = \"anthropic/claude-fable-5\"\npurpose = \"p\"\n",
        )
        .expect("agents.* is legible at consent and stays writable");
    }

    /// Every refusal carries a reason a human can read, because the reason is
    /// what makes the list reviewable rather than a set of names someone once
    /// chose.
    #[test]
    fn configure_refusals_each_state_a_reason() {
        assert!(!REFUSED_SECTIONS.is_empty());
        for (section, reason) in REFUSED_SECTIONS {
            assert!(!section.is_empty());
            assert!(
                reason.len() > 40,
                "`{section}` needs a reason, not a label: {reason}"
            );
        }
    }

    /// A key must be a dotted path of bare segments, and must reach inside a
    /// section. Quoted keys, empty segments and bare roots are all refused
    /// rather than sanitized — the [`crate::PluginManifest`] name rule.
    #[test]
    fn a_key_must_be_a_dotted_path_of_bare_segments() {
        // Single-quoted TOML literal strings, so a key containing a quote
        // needs no escaping and the fixture cannot be misread as malformed
        // TOML rather than as a rejected key.
        for key in ["ui", "", "a..b", "a.b c", r#""a.b".c"#, "a.b."] {
            let err = parse(&format!(
                "name = \"x\"\n\n[[configure]]\nkey = '{key}'\nvalue = \"v\"\npurpose = \"p\"\n"
            ))
            .unwrap_err();
            assert!(
                matches!(
                    err,
                    ManifestError::ConfigureKeyNotDotted { .. } | ManifestError::EmptyConfigureKey
                ),
                "`{key}` must be refused: {err:?}"
            );
        }
    }

    /// The [`crate::ToolContribution`] rule on this table: a key with no stated
    /// purpose is a change nobody can consent to.
    #[test]
    fn an_entry_must_state_a_purpose_and_be_declared_once() {
        let blank = parse(
            "name = \"x\"\n\n[[configure]]\nkey = \"a.b\"\nvalue = \"v\"\npurpose = \"  \"\n",
        )
        .unwrap_err();
        assert!(
            matches!(blank, ManifestError::EmptyConfigurePurpose { ref key } if key == "a.b"),
            "{blank:?}"
        );

        let dupe = parse(
            "name = \"x\"\n\n\
             [[configure]]\nkey = \"a.b\"\nvalue = \"1\"\npurpose = \"p\"\n\n\
             [[configure]]\nkey = \"a.b\"\nvalue = \"2\"\npurpose = \"p\"\n",
        )
        .unwrap_err();
        assert!(
            matches!(dupe, ManifestError::DuplicateConfigureKey { ref key } if key == "a.b"),
            "{dupe:?}"
        );
    }

    /// The #1400 rule this crate inherits, on the new table.
    #[test]
    fn an_unknown_key_in_the_configure_table_is_a_load_error() {
        let err = parse(
            "name = \"x\"\n\n[[configure]]\nkey = \"a.b\"\nvalue = \"v\"\n\
             purpose = \"p\"\nscope = \"user\"\n",
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::Parse(_)), "{err:?}");
    }

    /// A manifest that configures nothing is the overwhelming majority, and
    /// nothing about the existing shape changed.
    #[test]
    fn a_package_that_configures_nothing_still_loads() {
        assert!(parse("name = \"x\"").expect("fixture").configure.is_empty());
    }
}
