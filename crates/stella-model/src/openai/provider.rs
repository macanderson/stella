//! The [`Provider`] port impl for [`OpenAiProvider`] — the whole of this
//! adapter's public surface, and nothing else.
//!
//! Split out of `openai.rs` rather than grown inside it: that file is a
//! grandfathered god file closed to growth (AGENTS.md § God files), and a
//! trait impl cannot be spread across modules, so the block that must absorb
//! every future port method is precisely the block that had to leave. It is a
//! pure delegation seam — `id`/`model` answer from fields, and both completion
//! methods forward to `OpenAiProvider::complete_inner` — so nothing that
//! reasons about SSE, pricing, or the wire dialect moved with it.

use async_trait::async_trait;
use stella_protocol::{
    CompletionRequestRef, CompletionResult, Provider, ProviderError, ToolCallObserver,
};

use super::OpenAiProvider;

#[async_trait]
impl Provider for OpenAiProvider {
    fn id(&self) -> &str {
        "openai"
    }

    fn model(&self) -> Option<&str> {
        Some(&self.model)
    }

    async fn complete_ref(
        &self,
        req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        self.complete_inner(req, None).await
    }

    async fn complete_observed_ref(
        &self,
        req: CompletionRequestRef<'_>,
        observer: &dyn ToolCallObserver,
    ) -> Result<CompletionResult, ProviderError> {
        self.complete_inner(req, Some(observer)).await
    }
}
