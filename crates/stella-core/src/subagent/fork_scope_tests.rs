// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Forked-skill scoping tests (#2682): `allowed_tools` grants and `effort`
//! overrides on [`SubAgentSpec`], checked on the wire shape the child
//! actually sees. Split from `tests.rs`, whose scaffolding
//! ([`ScriptedProvider`], [`MixedTools`], result builders) it borrows —
//! that file sits against the 1500-line ratchet and is closed to growth.

use std::sync::Mutex;
use std::sync::atomic::Ordering;

use async_trait::async_trait;

use stella_protocol::{BudgetMode, CompletionRequestRef, CompletionResult, ProviderError};
use tokio::sync::mpsc;

use super::tests::{MixedTools, NoSleep, ScriptedProvider, text_result, tool_call_result};
use super::*;

// ---- forked-skill scoping: allowed_tools + effort (#2682) ---------------

/// A scripted provider that also records what each request advertised — the
/// tool names and the effort — so a spec override is checked on the wire
/// shape the child actually sees, not on intent.
struct RecordingProvider {
    script: Mutex<Vec<Result<CompletionResult, ProviderError>>>,
    seen_tools: Mutex<Vec<Vec<String>>>,
    seen_effort: Mutex<Vec<Option<stella_protocol::ReasoningEffort>>>,
}

impl RecordingProvider {
    fn new(script: Vec<Result<CompletionResult, ProviderError>>) -> Self {
        Self {
            script: Mutex::new(script),
            seen_tools: Mutex::new(Vec::new()),
            seen_effort: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "recording"
    }

    async fn complete_ref(
        &self,
        request: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        self.seen_tools
            .lock()
            .unwrap()
            .push(request.tools.iter().map(|s| s.name.clone()).collect());
        self.seen_effort.lock().unwrap().push(request.effort);
        let mut script = self.script.lock().unwrap();
        if script.is_empty() {
            return Err(ProviderError::Terminal("script exhausted".into()));
        }
        script.remove(0)
    }
}

/// #2682: a child scoped by `allowed_tools` sees only the granted schemas
/// AND cannot execute outside them by guessing a name — the grant is
/// structural (`GrantedTools`), not prompt-side.
#[tokio::test]
async fn a_grant_scoped_child_cannot_see_or_call_outside_its_grant() {
    let parent_provider = ScriptedProvider::new(vec![]);
    // The child tries the un-granted read first, then the granted write,
    // then answers.
    let child_provider = RecordingProvider::new(vec![
        Ok(tool_call_result("read_file", "c1", 0.001)),
        Ok(tool_call_result("write_file", "c2", 0.001)),
        Ok(text_result("done", 0.001)),
    ]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let spec = SubAgentSpec {
        agent_id: "skill-child".into(),
        instruction: "run the skill".into(),
        // Write access on, so the refusal below is unambiguously the
        // GRANT's, not the read-only view's.
        write_access: true,
        allowed_tools: Some(vec!["write_file".to_string()]),
        ..SubAgentSpec::default()
    };
    let outcome = parent
        .run_sub_agent(SubAgentHost::new(&child_provider), &spec, &mut budget, &tx)
        .await;
    assert!(
        matches!(outcome, SubAgentOutcome::Completed(_)),
        "the child completes despite the refused call: {outcome:?}"
    );

    // The advertised surface was the grant, on every call.
    for advertised in child_provider.seen_tools.lock().unwrap().iter() {
        assert_eq!(
            advertised,
            &vec!["write_file".to_string()],
            "the child must only ever see the granted schemas"
        );
    }
    // The un-granted call was refused at execution time, the granted one ran.
    assert_eq!(
        tools.reads.load(Ordering::SeqCst),
        0,
        "an un-granted tool must not execute even when called by name"
    );
    assert_eq!(tools.writes.load(Ordering::SeqCst), 1);
}

/// #2682: a spec-pinned effort reaches the child's requests; absent, the
/// child inherits the parent's.
#[tokio::test]
async fn a_forked_skills_effort_overrides_the_parents_and_absent_inherits() {
    use stella_protocol::ReasoningEffort;

    let parent_provider = ScriptedProvider::new(vec![]);
    let tools = MixedTools::default();
    let config = EngineConfig {
        effort: Some(ReasoningEffort::Low),
        ..EngineConfig::default()
    };
    let parent = Engine::with_sleeper(&parent_provider, &tools, config, &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    // Override: the child asks for High and gets it.
    let child_provider = RecordingProvider::new(vec![Ok(text_result("ok", 0.001))]);
    let spec = SubAgentSpec {
        agent_id: "skill-child".into(),
        instruction: "run".into(),
        effort: Some(ReasoningEffort::High),
        ..SubAgentSpec::default()
    };
    let _ = parent
        .run_sub_agent(SubAgentHost::new(&child_provider), &spec, &mut budget, &tx)
        .await;
    assert_eq!(
        child_provider.seen_effort.lock().unwrap().as_slice(),
        &[Some(ReasoningEffort::High)],
        "the spec's effort must reach the child's request"
    );

    // Inherit: no spec effort, the parent's Low flows through.
    let child_provider = RecordingProvider::new(vec![Ok(text_result("ok", 0.001))]);
    let spec = SubAgentSpec {
        agent_id: "skill-child".into(),
        instruction: "run".into(),
        ..SubAgentSpec::default()
    };
    let _ = parent
        .run_sub_agent(SubAgentHost::new(&child_provider), &spec, &mut budget, &tx)
        .await;
    assert_eq!(
        child_provider.seen_effort.lock().unwrap().as_slice(),
        &[Some(ReasoningEffort::Low)],
        "an unset spec effort must inherit the parent's"
    );
}
