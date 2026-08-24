// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The one place that decides whether a model string clears
//! [`AgentEngineConfig::allowed_models`](super::AgentEngineConfig::allowed_models).
//!
//! The list bound three surfaces and not the two that could spend money
//! without anyone typing a model name: a plugin seat map, and `/model default`
//! (#4618, #4659). Both refused to be fixed by copying the check, because the
//! check is a *claim* the setting's doc comment makes — one copy per surface is
//! one place per surface for the claim to stop being true. So the matching
//! rule and the sentence that reports it live here, and every surface calls
//! them.
//!
//! Pure string comparison, deliberately: the setting is hand-written, so the
//! only reliable normalization is the caller's own resolved spec, which it
//! passes alongside the raw string it was handed.

/// Whether `allowed` admits a model.
///
/// An empty list is no restriction — that is what "unset" means, and it is why
/// every caller may ask unconditionally instead of branching on emptiness
/// first.
///
/// A non-empty list admits an entry that equals either the resolved
/// `provider/slug` spec or the raw string the surface was handed. Both, because
/// a bare seeded slug (`glm-5.2`) is a legal way to write the setting and a
/// legal way to type the command, and matching only the resolved form would
/// refuse a list that spells its entries the way the operator types them.
#[must_use]
pub(crate) fn admits(allowed: &[String], full_spec: &str, requested: &str) -> bool {
    allowed.is_empty() || allowed.iter().any(|a| a == full_spec || a == requested)
}

/// Why `full_spec` was refused: the model, the setting that refused it under
/// both of its spellings, and the vocabulary it had to choose from.
///
/// The vocabulary is the half a refusal is useless without — a restriction the
/// operator cannot see is one they have to go and read a file to satisfy.
#[must_use]
pub(crate) fn denial(allowed: &[String], full_spec: &str) -> String {
    format!(
        "`{full_spec}` is not in this workspace's allowed model list \
         (`[models].allowed` / `agent_engine_config.allowed_models`) — allowed: {}",
        allowed.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|e| (*e).to_string()).collect()
    }

    /// An unset list restricts nothing, which is what lets every surface ask
    /// without first checking whether there is anything to ask about.
    #[test]
    fn an_empty_list_admits_everything() {
        assert!(admits(&[], "anthropic/claude-opus-5", "claude-opus-5"));
    }

    /// The two spellings a surface can offer both match, and neither is
    /// invented from the other: a list written in bare slugs admits a typed
    /// `provider/slug`, and a list written in full specs admits a bare slug.
    #[test]
    fn either_spelling_matches() {
        let full = list(&["anthropic/claude-opus-5"]);
        assert!(admits(&full, "anthropic/claude-opus-5", "claude-opus-5"));

        let bare = list(&["claude-opus-5"]);
        assert!(admits(&bare, "anthropic/claude-opus-5", "claude-opus-5"));
    }

    #[test]
    fn an_off_list_model_is_refused_and_the_vocabulary_is_named() {
        let allowed = list(&["anthropic/claude-opus-5", "zai/glm-5.2"]);
        assert!(!admits(&allowed, "openai/gpt-5.5", "openai/gpt-5.5"));

        let denial = denial(&allowed, "openai/gpt-5.5");
        assert!(denial.contains("openai/gpt-5.5"), "{denial}");
        assert!(denial.contains("allowed model list"), "{denial}");
        assert!(
            denial.contains("anthropic/claude-opus-5, zai/glm-5.2"),
            "a refusal must show what it would accept: {denial}"
        );
    }
}
