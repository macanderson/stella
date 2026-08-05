//! [`ScriptedProvider`] — the one-provider-serves-every-role test double used
//! across almost every pipeline test. Split out of `tests.rs` to keep that
//! file under the size gate; nothing here is orchestration, just a fixture.

use super::*;

/// A scripted provider serving triage, plan, worker, and verifier calls from
/// one ordered queue (the resolver hands the same provider to every role).
pub(super) struct ScriptedProvider {
    pub(super) script: TokioMutex<VecDeque<CompletionResult>>,
    /// Every prompt this provider was asked to complete, in order — so a test
    /// can assert what actually reached the model, not just what came back.
    seen: std::sync::Mutex<Vec<String>>,
    /// The same requests with per-message roles preserved, so a test can
    /// assert the system/user split of a management call (#1434) — the joined
    /// text above cannot show which half a sentence landed in.
    shapes: std::sync::Mutex<Vec<Vec<(stella_protocol::MessageRole, String)>>>,
}
impl ScriptedProvider {
    pub(super) fn new(results: Vec<CompletionResult>) -> Self {
        Self {
            script: TokioMutex::new(results.into_iter().collect()),
            seen: std::sync::Mutex::new(Vec::new()),
            shapes: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The message text of each request, one joined string per call.
    pub(super) fn prompts(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    /// Each request's messages as `(role, content)` pairs, in call order.
    pub(super) fn shapes(&self) -> Vec<Vec<(stella_protocol::MessageRole, String)>> {
        self.shapes.lock().unwrap().clone()
    }
}
#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        "scripted"
    }
    async fn complete_ref(
        &self,
        req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        self.seen.lock().unwrap().push(
            req.messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        self.shapes.lock().unwrap().push(
            req.messages
                .iter()
                .map(|m| (m.role, m.content.clone()))
                .collect(),
        );
        let mut q = self.script.lock().await;
        q.pop_front()
            .ok_or_else(|| ProviderError::Terminal("scripted provider exhausted".into()))
    }
}
