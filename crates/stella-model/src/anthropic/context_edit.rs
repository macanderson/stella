//! Server-side context editing: clearing stale thinking blocks and tool
//! results before the model reads them.
//!
//! Context editing **deletes**; compaction summarizes. The Messages API drops
//! the oldest tool results (and, separately, thinking blocks) out of the
//! history it shows the model, keeping the most recent `keep` of them, and
//! replaces each cleared result with placeholder text so the model knows
//! something was removed rather than silently seeing a shorter conversation.
//!
//! # Why this is a policy and not a switch
//!
//! Clearing is not free, and the naive reading — "less context, less money" —
//! is wrong often enough to be dangerous. **Clearing invalidates the prompt
//! cache from the point of the edit**, and a cache *write* is priced above a
//! fresh read, an order of magnitude above a cache *read*. A benchmark
//! session running at a 91.7% cache-hit rate can therefore spend more by
//! clearing than by carrying the tokens, and it will do so quietly.
//!
//! Two defaults follow from that, both taken from the vendor's own guidance:
//!
//! * **`trigger` is a floor, not a target.** Nothing clears until the input
//!   crosses it, so short conversations — the majority of trials — pay
//!   nothing and keep a whole cached prefix.
//! * **`clear_at_least` is what makes an edit worth its invalidation.**
//!   Without it the API may clear a few hundred tokens and bill a full cache
//!   re-write to do it. With it, an edit either frees enough to pay for
//!   itself or does not happen.
//!
//! # Ordering
//!
//! `clear_thinking` must be listed before `clear_tool_uses` in `edits`; the
//! API rejects the other order. [`ContextManagement::new`] is the only
//! constructor, so the order cannot be got wrong by a caller.

use serde::Serialize;

/// The beta that unlocks `context_management` on the Messages API.
pub(crate) const CONTEXT_MANAGEMENT_BETA: &str = "context-management-2025-06-27";

/// How many tool uses survive an edit. The vendor default; the most recent
/// calls are the ones the model is actually still reasoning about, and
/// clearing them is how an agent loses the file it just read.
const KEEP_TOOL_USES: u32 = 3;

/// Minimum tokens an edit must free to be worth the cache invalidation it
/// causes. Below this the re-write costs more than the carried tokens would
/// have.
const CLEAR_AT_LEAST_TOKENS: u32 = 5_000;

/// `context_management` on the request.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextManagement {
    pub(crate) edits: Vec<ContextEdit>,
}

/// One editing strategy. Untagged-by-hand rather than `#[serde(tag)]` because
/// the two strategies do not share a field shape and the API is picky about
/// which keys may appear beside which `type`.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(untagged)]
pub(crate) enum ContextEdit {
    /// `{"type":"clear_thinking_20251015","keep":{…}}`
    ClearThinking {
        #[serde(rename = "type")]
        kind: &'static str,
        keep: ThinkingKeep,
    },
    /// `{"type":"clear_tool_uses_20250919", …}`
    ClearToolUses {
        #[serde(rename = "type")]
        kind: &'static str,
        trigger: TokenThreshold,
        keep: ToolUseKeep,
        clear_at_least: TokenThreshold,
        /// Whether the tool *call* arguments are cleared alongside the result.
        /// Left off: the arguments are what let the model remember it already
        /// ran `pytest -k foo`, and re-running a command because its call
        /// vanished costs more than the arguments did.
        clear_tool_inputs: bool,
    },
}

/// How many thinking turns survive. `All` is the current-model default and the
/// cache-preserving choice; a numeric keep trades cache for context room.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(untagged)]
pub(crate) enum ThinkingKeep {
    /// `"all"` — clears nothing, so the cached prefix survives intact.
    All(&'static str),
    /// `{"type":"thinking_turns","value":N}`
    Turns {
        #[serde(rename = "type")]
        kind: &'static str,
        value: u32,
    },
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub(crate) struct TokenThreshold {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) value: u32,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolUseKeep {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) value: u32,
}

impl ContextManagement {
    /// The editing policy for a session, or `None` to send no
    /// `context_management` at all.
    ///
    /// `trigger_tokens` is the input size at which clearing begins. It is the
    /// whole safety of the feature: below it the conversation is untouched and
    /// fully cached, so a short trial can never be made more expensive by
    /// having this enabled.
    ///
    /// `thinking_turns` selects the thinking policy: `None` keeps every
    /// thinking block (cache-optimal, and what a current model does by
    /// default), `Some(n)` keeps the last `n` turns and accepts the
    /// invalidation.
    pub(crate) fn new(trigger_tokens: u32, thinking_turns: Option<u32>) -> Self {
        let keep = match thinking_turns {
            None => ThinkingKeep::All("all"),
            Some(value) => ThinkingKeep::Turns {
                kind: "thinking_turns",
                value,
            },
        };
        Self {
            // Thinking first: the API rejects the reverse order.
            edits: vec![
                ContextEdit::ClearThinking {
                    kind: "clear_thinking_20251015",
                    keep,
                },
                ContextEdit::ClearToolUses {
                    kind: "clear_tool_uses_20250919",
                    trigger: TokenThreshold {
                        kind: "input_tokens",
                        value: trigger_tokens,
                    },
                    keep: ToolUseKeep {
                        kind: "tool_uses",
                        value: KEEP_TOOL_USES,
                    },
                    clear_at_least: TokenThreshold {
                        kind: "input_tokens",
                        value: CLEAR_AT_LEAST_TOKENS,
                    },
                    clear_tool_inputs: false,
                },
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_is_listed_before_tool_uses() {
        // Not cosmetic: the API rejects `clear_tool_uses` ahead of
        // `clear_thinking`, and a caller cannot reorder them because `new` is
        // the only way to build the struct.
        let cm = ContextManagement::new(100_000, Some(1));
        let json = serde_json::to_value(&cm).unwrap();
        let edits = json["edits"].as_array().unwrap();
        assert_eq!(edits[0]["type"], "clear_thinking_20251015");
        assert_eq!(edits[1]["type"], "clear_tool_uses_20250919");
    }

    #[test]
    fn an_edit_declares_what_makes_it_worth_its_cache_invalidation() {
        let cm = ContextManagement::new(30_000, None);
        let json = serde_json::to_value(&cm).unwrap();
        let clear = &json["edits"][1];
        assert_eq!(clear["trigger"]["value"], 30_000);
        assert_eq!(clear["clear_at_least"]["value"], CLEAR_AT_LEAST_TOKENS);
        // Keeping the newest calls is what stops an agent losing the file it
        // just read.
        assert_eq!(clear["keep"]["value"], KEEP_TOOL_USES);
        // Tool *inputs* survive: they are how the model knows what it already
        // ran, and they are small.
        assert_eq!(clear["clear_tool_inputs"], false);
    }

    /// `keep: "all"` is a bare string, not an object — the shape Claude Code
    /// itself sends, and the one that preserves the prompt cache.
    #[test]
    fn keeping_all_thinking_serializes_as_the_bare_string() {
        let cm = ContextManagement::new(100_000, None);
        let json = serde_json::to_value(&cm).unwrap();
        assert_eq!(json["edits"][0]["keep"], "all");
    }

    #[test]
    fn a_numeric_thinking_keep_serializes_as_the_object_shape() {
        let cm = ContextManagement::new(100_000, Some(2));
        let json = serde_json::to_value(&cm).unwrap();
        assert_eq!(json["edits"][0]["keep"]["type"], "thinking_turns");
        assert_eq!(json["edits"][0]["keep"]["value"], 2);
    }
}
