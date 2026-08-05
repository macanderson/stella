//! The two-message shape of a management-call prompt (#1434): a byte-stable
//! instruction block that rides as a SYSTEM message, and the per-call payload
//! that rides as the USER message.
//!
//! The split is what gives a management call a cacheable prefix at all. The
//! worker path keeps a byte-stable prompt prefix by discipline (the
//! prompt-cache stability invariant), and the provider adapters already mark
//! what they can — the Anthropic dialect stamps `cache_control` on every
//! system block it sends, Bedrock Converse emits its cache points, and the
//! OpenRouter/OpenAI dialects carry session-stable routing keys
//! (`session_id` / `prompt_cache_key`) on every request. But a prompt sent as
//! one user message gives that machinery nothing stable to mark: triage,
//! verdict and guidance re-billed their fixed instruction blocks at the
//! uncached input rate on every call, every round, every candidate. Splitting
//! instructions from payload puts the fixed bytes where the adapters' existing
//! plumbing can see them.
//!
//! Whether a given provider then actually serves the prefix from cache is the
//! provider's business — minimum prefix lengths apply (Anthropic ≥1024 tokens
//! on most models, OpenAI similar) — but below the split the answer was
//! structurally *never*, and the settings-supplied `agents.<role>.prompt`
//! override joins the same system prefix, so a tuned role clears the minimum
//! sooner, not later.

use stella_protocol::CompletionMessage;

/// One management prompt, split into the part a cache can serve and the part
/// it never could.
///
/// `instructions` is `&'static str` on purpose: byte-stability across calls
/// is the entire point of the split, and a static is the strongest structural
/// guarantee of it the type system offers — a later edit that tried to
/// interpolate something volatile into the instruction block would have to
/// change this type to do it.
pub struct ManagementPrompt {
    /// The fixed instruction block — identical bytes on every call of the
    /// same role for the life of the process.
    pub instructions: &'static str,
    /// The per-call payload: goal, evidence, rendered diff. Everything
    /// volatile lands here, AFTER the stable prefix.
    pub payload: String,
}

impl ManagementPrompt {
    /// The wire shape: `[system(instructions), user(payload)]`.
    ///
    /// Callers hand this to `metered_raw_call`, which may prepend a
    /// settings-supplied `agents.<role>.prompt` override as a further system
    /// message. That order — override first, then these instructions, then
    /// the payload — is stable for the life of a run, which is what keeps the
    /// joined system prefix byte-stable where a provider cache can read it.
    pub fn into_messages(self) -> Vec<CompletionMessage> {
        vec![
            CompletionMessage::system(self.instructions.to_string()),
            CompletionMessage::user(self.payload),
        ]
    }

    /// The complete prompt as one string — instructions, blank line, payload:
    /// exactly what the pre-split single-message prompt rendered.
    ///
    /// Tests assert over this so the contract they pin is the full text a
    /// model reads, not an accident of which half a sentence landed in.
    pub fn rendered(&self) -> String {
        format!("{}\n\n{}", self.instructions, self.payload)
    }
}
