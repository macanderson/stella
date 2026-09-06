// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Context steering for one fleet worker. The block the attempt opens with.
//! The handle that lets the turn ask again as it runs.
//!
//! A worker holds its `SessionMemory` open for the whole turn. The engine
//! asks the plane again at each step boundary. A fleet attempt is the longest
//! turn nobody watches. It drifts onto files no one named up front. Without
//! the handle it runs on the context its first sentence asked for.

use std::path::Path;

use stella_protocol::CompletionMessage;

use crate::memory::{OpeningRecall, SessionMemory};
use crate::rules::ResolvedRules;
use crate::settings::AuthorityPolicy;

/// One attempt's live steering handle.
///
/// It holds the session memory open for the turn and nothing else. Drop it
/// when the turn ends. It is rooted at the attempt's own tree. For an
/// isolated task that tree is a linked worktree, and the fleet deletes it. A
/// lesson written through this handle would go with it. So
/// `mine_attempt_lesson` opens its own handle at the invocation root.
pub(super) struct WorkerSteering {
    memory: Option<SessionMemory>,
}

impl WorkerSteering {
    /// Render this attempt's opening block into `messages`. Keep the handle
    /// behind it.
    ///
    /// The block carries recalled frames, the chosen skills, the matched
    /// context records, and today's date. The stable prefix holds the
    /// memories and the enforced rules. It leaves today's date out on
    /// purpose, because the date rides here. A worker with the prefix alone
    /// gets told to treat anything that may have moved since as unproven,
    /// with nothing to measure "since" against.
    ///
    /// Rooted at the attempt's own `root`, not the workspace root. An
    /// isolated task runs in a linked worktree, and parallel workers must not
    /// share one SQLite writer. What that root can offer differs by task, and
    /// both answers are right. A fresh worktree carries
    /// `.stella/rules/*.toml`, the one tracked part of `.stella/`. It still
    /// reaches the user-wide `~/.stella/skills`. It has no
    /// `.stella/private/context.db`, so there the block is records and date
    /// and costs no lookup at all.
    ///
    /// The A/B recall control is armed here, as in every other driver.
    /// Parallel workers do not spoil the schedule. The counter is durable and
    /// each process claims its own number, so the arms weave into one
    /// workspace-wide order. That is the case
    /// `SessionMemory::arm_recall_control` names when it lists a fleet task.
    ///
    /// What comes back is what the attempt still owes the block. First, the
    /// recall telemetry, which the caller sends once its event channel
    /// exists. Second, the turn scopes this attempt's skills ask for, which
    /// the caller mounts over the tool stack. Both ride
    /// `crate::memory::inject_opening_recall`. So this worker cannot be the
    /// one door where a chosen skill's `allowed-tools` grant and `effort` get
    /// dropped.
    ///
    /// `ledger` is the attempt's share of the steering allowance: the block
    /// spends here, and the attempt's tool stack — assembled after this call
    /// returns — takes what is left.
    pub(super) async fn open(
        root: &Path,
        authority: &AuthorityPolicy,
        active_rules: &ResolvedRules,
        prompt: &str,
        messages: &mut Vec<CompletionMessage>,
        ledger: &stella_core::steering::ledger::SteeringLedger,
    ) -> (Self, OpeningRecall) {
        // `warn: false`, the choice the live grid makes. With `--watch` a
        // grid owns the terminal, and a store warning per worker would paint
        // N copies of the same line over it.
        let Some(mut memory) =
            SessionMemory::open_for_session(root, false, authority, active_rules)
        else {
            return (Self { memory: None }, OpeningRecall::default());
        };
        memory.arm_recall_control();
        // A fleet attempt recalls before its engine has messages. There is
        // no talk yet to read touched paths from, so the empty anchor set is
        // the right argument. It is the scope the prompt alone always gave.
        let recalled = memory.recall_block_reported(prompt, &[]).await;
        let recall = crate::memory::inject_opening_recall(messages, recalled, ledger);
        (
            Self {
                memory: Some(memory),
            },
            recall,
        )
    }

    /// The handle to give `crate::memory::requery_for_turn`. `None` when
    /// this tree has no session memory to ask.
    pub(super) fn memory(&self) -> Option<&SessionMemory> {
        self.memory.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A workspace with one domain over `crates/stella-model`. One file
    /// under it to anchor against. One skill tagged with that domain, worded
    /// so it shares nothing with the prompt below.
    ///
    /// The A/B recall control is pinned off. At the shipped rate one turn in
    /// ten holds the whole plane back. That would fail this test one run in
    /// ten, for a reason it is not about.
    fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::create_dir_all(root.join(".stella")).unwrap();
        std::fs::write(
            root.join(".stella/settings.json"),
            r#"{"context":{"retrieval":{"ab_recall_rate":0}}}"#,
        )
        .unwrap();

        crate::domains::Domains {
            version: 1,
            inferred_by: "heuristic".into(),
            source_fingerprint: None,
            domains: vec![crate::domains::Domain {
                name: "model-adapters".into(),
                description: "provider adapters".into(),
                paths: vec!["crates/stella-model".into()],
            }],
        }
        .save(root)
        .expect("domains.toml writes");

        let anchored = root.join("crates").join("stella-model");
        std::fs::create_dir_all(&anchored).unwrap();
        std::fs::write(anchored.join("anthropic.rs"), "// adapter\n").unwrap();

        let skill_dir = root.join(".stella").join("skills").join("adapter-notes");
        std::fs::create_dir_all(&skill_dir).unwrap();
        // Zero word overlap with the prompt below. The only thing that can
        // select this skill is the domain tag, which is what makes both
        // checks below about drift and nothing else.
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: adapter-notes\ndescription: streaming dialect quirks per vendor\n\
             domains: model-adapters\n---\n\nAlways reuse an existing adapter's shape.\n",
        )
        .unwrap();

        dir
    }

    /// Workspace skills sit behind the project-trust line. An attempt that
    /// means to pick one opens with that grant.
    fn trusted() -> AuthorityPolicy {
        AuthorityPolicy {
            project_prompts_allowed: true,
            ..AuthorityPolicy::default()
        }
    }

    /// **The witness.** A fleet worker asks the steering plane again once its
    /// work moves, and picks up the skill its opening prompt could not have
    /// selected.
    ///
    /// It fails without this change, twice over. There is no `WorkerSteering`
    /// to build, and a fleet attempt closes its memory as soon as the opening
    /// block is rendered, so nothing lives long enough for
    /// `requery_for_turn` to take. Every selector fires once, against the
    /// prompt.
    ///
    /// The first check is what makes the second mean anything: an empty
    /// opening block would satisfy the drift check on its own.
    #[tokio::test]
    async fn a_fleet_worker_requeries_the_skill_its_opening_prompt_could_not() {
        use stella_core::ports::SteeringRequery as _;

        let dir = workspace();
        let prompt = "rename the changelog heading";
        let prefix = "STABLE SYSTEM PREFIX";
        let mut messages = vec![
            CompletionMessage::system(prefix),
            CompletionMessage::user(prompt),
        ];

        let (steering, recall) = WorkerSteering::open(
            dir.path(),
            &trusted(),
            &ResolvedRules::default(),
            prompt,
            &mut messages,
        )
        .await;

        assert!(
            !messages.iter().any(|m| m.content.contains("adapter-notes")),
            "phase 1: the opening prompt anchors nowhere near the skill's \
             domain, so its block must not carry the skill: {messages:?}"
        );

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<stella_protocol::AgentEvent>();
        let requery = crate::memory::requery_for_turn(
            steering.memory(),
            &messages,
            tx.into(),
            recall.produced,
        )
        .expect("an attempt with session memory has a re-query handle");

        let touched = vec!["crates/stella-model/anthropic.rs".to_string()];
        let drifted = stella_core::steering::TurnSignal {
            prompt,
            touched_paths: &touched,
            since_last_query: 5,
            ..Default::default()
        };
        let block = requery
            .requery(&drifted)
            .await
            .expect("the drift into the domain surfaces its skill");
        assert!(
            block.contains("adapter-notes"),
            "phase 2: the path this attempt touched selects the domain's \
             skill: {block}"
        );

        // The engine appends the block at the step boundary. Doing the same
        // here pins what the whole seam owes the prompt cache: the prefix is
        // byte-identical after the opening block and after the re-query.
        messages.push(CompletionMessage::user(block));
        assert_eq!(
            messages[0].content, prefix,
            "the stable prefix must not move across either injection"
        );
    }

    /// The control on the other side: an unchanged signal buys nothing. A
    /// worker that never drifts pays for no second lookup, which is what
    /// makes the handle safe to hold open for every attempt.
    #[tokio::test]
    async fn an_undrifted_fleet_worker_asks_for_nothing() {
        use stella_core::ports::SteeringRequery as _;

        let dir = workspace();
        let prompt = "rename the changelog heading";
        let mut messages = vec![CompletionMessage::user(prompt)];

        let (steering, recall) = WorkerSteering::open(
            dir.path(),
            &trusted(),
            &ResolvedRules::default(),
            prompt,
            &mut messages,
        )
        .await;

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<stella_protocol::AgentEvent>();
        let requery = crate::memory::requery_for_turn(
            steering.memory(),
            &messages,
            tx.into(),
            recall.produced,
        )
        .expect("an attempt with session memory has a re-query handle");

        let steady = stella_core::steering::TurnSignal {
            prompt,
            since_last_query: 5,
            ..Default::default()
        };
        assert!(
            requery.requery(&steady).await.is_none(),
            "an unchanged signal never moves the fingerprint, so it costs \
             the attempt nothing"
        );
    }
}
