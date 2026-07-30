//! Which models on this OpenAI-compatible dialect may receive image
//! parts, and what happens to the ones that may not.
//!
//! Split out of `zai/tests.rs` rather than appended to it: that file is
//! already 1,685 lines and grandfathered over the 1,500-line ratchet
//! (`scripts/check-file-size.sh`), and image capability is a self-contained
//! question worth finding on its own.

use super::*;

/// The reported failure: a screenshot pasted into the deck while the worker
/// was `glm-5.2` became an `image_url` part, and Z.ai answered
/// `1210: messages.content.type is invalid, allowed values: ['text']` — a
/// *terminal* provider error that killed the turn. A text-only GLM must get
/// the degrade note instead, so the turn survives and the model can say what
/// it cannot see.
#[test]
fn a_text_only_glm_degrades_an_image_instead_of_killing_the_turn() {
    use stella_protocol::{Attachment, AttachmentSource};
    let shot = Attachment {
        name: "pasted-1785442896326.png".into(),
        media_type: "image/png".into(),
        byte_len: 3,
        source: AttachmentSource::Data {
            base64: "aW1n".into(),
        },
    };
    for model in ["glm-5.2", "glm-4.6", "glm-5", "zai/glm-5.2", "GLM-5.2"] {
        let message = CompletionMessage::user_with_attachments("what is this", vec![shot.clone()]);
        let ZaiContent::Parts(parts) = user_content(&message, model) else {
            panic!("{model}: attachments always produce a parts array");
        };
        assert!(
            parts
                .iter()
                .all(|p| matches!(p, ZaiContentPart::Text { .. })),
            "{model} is text-only — no image part may reach the wire: {parts:?}"
        );
        let [ZaiContentPart::Text { text }, ..] = parts.as_slice() else {
            unreachable!()
        };
        assert!(
            text.contains("pasted-1785442896326.png"),
            "{model}: the note names the file so the model can offer to read it: {text}"
        );
    }
}

/// The other half of the asymmetry: the deny is scoped to GLM text models.
/// A GLM vision variant still sends the image, and so does every non-GLM slug
/// this OpenAI-compatible dialect carries for OpenRouter — guessing "no" there
/// would silently blind Opus/Gemini/GPT behind the gateway.
#[test]
fn vision_glms_and_gateway_models_still_send_the_image() {
    for model in [
        "glm-4v",
        "glm-4.1v",
        "glm-4.5v",
        "glm-4.5v-flash",
        "z-ai/glm-4.5v",
        "anthropic/claude-opus-5",
        "openai/gpt-5",
        "google/gemini-3-pro",
        "moonshotai/kimi-k3",
    ] {
        assert!(
            model_ingests_images(model),
            "{model} must keep its image parts"
        );
    }
    for model in ["glm-5.2", "glm-4.6", "glm-5", "glm-4-plus"] {
        assert!(!model_ingests_images(model), "{model} is text-only");
    }
}
