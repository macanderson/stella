// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Scope-review approvals for supervised runs (#1585).
//!
//! A supervised child's stdin is a file and its stdout is a log, so the stdio
//! approval gate has nobody to ask — and before this module, supervision
//! therefore *removed* a capability: the same `stella run` that would have
//! stopped and asked in the foreground instead died at the named headless
//! scope-review error. Durability and interactivity were an either/or.
//!
//! The transport is the session sidecar, because it is the one place both
//! sides can already find: the child parks its [`ScopeProposal`] in
//! [`supervised::APPROVAL_REQUEST`] and flips its registry record to
//! [`SessionStatus::NeedsInput`]; whichever terminal is attached — the parent
//! that launched the run, or a later `stella daemon attach` — renders the
//! prompt, and writes the typed [`ScopeDecision`] to
//! [`supervised::APPROVAL_ANSWER`]. The child polls, applies the decision,
//! removes both files, and flips its record back.
//!
//! # Parked, not failed — and never silently forever
//!
//! With nobody attached the child stays parked. That is deliberate (the run's
//! budget and worktree are worth less than the plan the human asked to see),
//! but it is bounded by *discoverability*, not by a timer: the record reads
//! `Needs Input` in `stella daemon list`, and a `stella daemon stop` — or any
//! `SIGTERM` — unparks it immediately as an abort, which is the same answer
//! the stdio gate gives on EOF. The interrupt check is what keeps `stop`'s
//! escalation grace honest: a parked child that ignored `SIGTERM` would be
//! `SIGKILL`ed at the deadline and recorded as a crash it did not have.
//!
//! # Why the prompt never touches the console files
//!
//! The two console files are byte-verbatim stdout/stderr —
//! `stella run --output-format json` is piped into `jq` through the
//! supervisor, and a prompt folded into either stream corrupts the parse on
//! the caller's side. The attached terminal renders the prompt from the
//! request file onto its own stderr instead; the consoles carry only what the
//! run itself wrote.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use colored::Colorize;
use stella_pipeline::{ApprovalGate, ScopeDecision, decision_from_line};
use stella_protocol::ScopeProposal;
use stella_store::{SessionRegistry, SessionStatus, supervised};

/// How often the parked child re-checks for an answer (and for an interrupt).
/// Human-latency polling: cheap against a wait that is minutes long, and well
/// inside the 8s stop grace the interrupt check has to answer within.
const ANSWER_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// The [`ApprovalGate`] a supervised child runs the pipeline with: park the
/// proposal in the sidecar, wait for an attached terminal's decision.
pub(crate) struct SidecarApprovalGate {
    sidecar: PathBuf,
    registry: SessionRegistry,
    id: String,
    /// Injected rather than read directly so the park loop's interrupt
    /// behaviour is testable without raising a process-global signal flag.
    /// Production passes [`interrupted`].
    interrupted: fn() -> bool,
}

/// The production interrupt probe: has this process been asked to stop?
fn interrupted() -> bool {
    crate::signals::interrupted_exit_code().is_some()
}

impl SidecarApprovalGate {
    /// The gate for this process's own supervised session, or `None` when the
    /// process is not a supervised child.
    pub(crate) fn for_supervised_child() -> Option<Self> {
        let id = super::supervised_id()?;
        let registry = SessionRegistry::open_default();
        Some(Self::new(registry.sidecar_dir(&id), registry, id))
    }

    /// A gate over an explicit sidecar and registry — the testable
    /// constructor, and the one everything else is built from.
    pub(crate) fn new(sidecar: PathBuf, registry: SessionRegistry, id: String) -> Self {
        Self {
            sidecar,
            registry,
            id,
            interrupted,
        }
    }

    /// Replace the interrupt probe (tests only — production keeps the signal
    /// flag).
    #[cfg(test)]
    pub(crate) fn with_interrupt_probe(mut self, probe: fn() -> bool) -> Self {
        self.interrupted = probe;
        self
    }

    /// Remove both transport files, tolerating either being already gone.
    fn clear_files(&self) {
        let _ = std::fs::remove_file(self.sidecar.join(supervised::APPROVAL_REQUEST));
        let _ = std::fs::remove_file(self.sidecar.join(supervised::APPROVAL_ANSWER));
    }
}

#[async_trait]
impl ApprovalGate for SidecarApprovalGate {
    /// Park `proposal` in the sidecar and wait for a human's decision.
    ///
    /// Fail-closed on every broken edge — an unwritable request, an
    /// unparseable answer, an interrupt — because the gate's contract is that
    /// nothing larger than the thresholds runs without a human's yes, and
    /// every one of those edges is a missing yes.
    async fn review(&self, proposal: &ScopeProposal) -> ScopeDecision {
        // A leftover answer from an earlier gate (or a crashed predecessor)
        // must not answer THIS question: clear before asking.
        self.clear_files();
        let request = match serde_json::to_vec_pretty(proposal) {
            Ok(bytes) => bytes,
            Err(_) => return ScopeDecision::Abort,
        };
        if stella_store::write_sensitive_file_atomic(
            &self.sidecar.join(supervised::APPROVAL_REQUEST),
            &request,
        )
        .is_err()
        {
            return ScopeDecision::Abort;
        }
        let _ = self.registry.set_status(&self.id, SessionStatus::NeedsInput);

        let decision = loop {
            if (self.interrupted)() {
                // The same answer the stdio gate gives on EOF: asked to stop
                // is a no. Unparking here is what lets `stop`'s SIGTERM grace
                // land as a clean cancel instead of an escalated kill.
                break ScopeDecision::Abort;
            }
            match std::fs::read_to_string(self.sidecar.join(supervised::APPROVAL_ANSWER)) {
                Ok(json) => {
                    // Unparseable is abort, not retry: the writer publishes
                    // atomically, so a bad parse is a bad document, and a gate
                    // that waits for a better one waits forever.
                    break serde_json::from_str::<ScopeDecision>(&json)
                        .unwrap_or(ScopeDecision::Abort);
                }
                Err(_) => tokio::time::sleep(ANSWER_POLL).await,
            }
        };

        self.clear_files();
        let _ = self.registry.set_status(&self.id, SessionStatus::InProgress);
        decision
    }
}

/// The one-shot pipeline's approval gate, selected by capability — the three
/// surfaces a `stella run` can answer scope review on, as one value the call
/// site can borrow for the pipeline's lifetime.
///
/// An enum rather than a `Box<dyn ApprovalGate>` so the selection is a total
/// match the compiler re-checks when a capability is added — precisely the
/// spot a squash-merge once collapsed to a bare `is_text` check.
pub(crate) enum OneShotApprovalGate {
    Stdio(stella_pipeline::StdioApprovalGate),
    Sidecar(SidecarApprovalGate),
    Headless(stella_pipeline::AlwaysAbortGate),
}

impl OneShotApprovalGate {
    /// The gate `capability` names. `Sidecar` degrades to headless when this
    /// process turns out not to be a supervised child after all — the two are
    /// computed from the same predicate, so that arm is a defensive
    /// impossibility, and failing closed is what a missing approver means.
    pub(crate) fn select(capability: crate::agent::PipelineApprovalCapability) -> Self {
        use crate::agent::PipelineApprovalCapability as Capability;
        match capability {
            Capability::Stdio => Self::Stdio(stella_pipeline::StdioApprovalGate),
            Capability::Sidecar => match SidecarApprovalGate::for_supervised_child() {
                Some(gate) => Self::Sidecar(gate),
                None => Self::Headless(stella_pipeline::AlwaysAbortGate),
            },
            Capability::Unavailable => Self::Headless(stella_pipeline::AlwaysAbortGate),
        }
    }
}

#[async_trait]
impl ApprovalGate for OneShotApprovalGate {
    async fn review(&self, proposal: &ScopeProposal) -> ScopeDecision {
        match self {
            Self::Stdio(gate) => gate.review(proposal).await,
            Self::Sidecar(gate) => gate.review(proposal).await,
            Self::Headless(gate) => gate.review(proposal).await,
        }
    }

    async fn confirm(&self, question: &str) -> bool {
        match self {
            Self::Stdio(gate) => gate.confirm(question).await,
            Self::Sidecar(gate) => gate.confirm(question).await,
            Self::Headless(gate) => gate.confirm(question).await,
        }
    }
}

/// The attached-terminal side: answer a parked scope review, if one is
/// pending and this terminal can.
///
/// Called from the follow loops — the launching parent's and `stella daemon
/// attach`'s — between console pumps. `interactive` is whether this process's
/// stdin has a human on it; a piped attach (`… | head`) must never answer a
/// question nobody read, so it only *says* the run is parked (`noted`
/// dedupes, and re-arms when the request clears so a second review is not
/// silent).
///
/// The prompt goes to stderr: stdout is the byte-verbatim relay of the run's
/// own output, and a prompt inside it would corrupt a machine-format caller.
/// Reading stdin blocks this thread until the human answers, exactly as the
/// stdio gate does — the console keeps buffering in the sidecar meanwhile,
/// and the loop drains it on the next pass.
pub(crate) fn forward_pending_approval(sidecar: &Path, interactive: bool, noted: &mut bool) {
    let request = sidecar.join(supervised::APPROVAL_REQUEST);
    let Ok(json) = std::fs::read_to_string(&request) else {
        *noted = false;
        return;
    };
    let Ok(proposal) = serde_json::from_str::<ScopeProposal>(&json) else {
        // A half-published or foreign document. The child owns the file's
        // lifecycle; the reader just declines to render it.
        return;
    };
    if !interactive {
        if !*noted {
            eprintln!(
                "{} this run is parked on a scope review — answer it with {}",
                "▸ needs input".yellow().bold(),
                "stella daemon attach".cyan()
            );
            *noted = true;
        }
        return;
    }

    use std::io::{BufRead, Write};
    eprintln!();
    eprintln!("  ┌─ Scope Review ──────────────────────────────");
    eprintln!("  │ {}", proposal.summary);
    for (i, step) in proposal.steps.iter().enumerate() {
        eprintln!("  │   {}. {}", i + 1, step);
    }
    if let Some(cost) = proposal.estimated_cost_usd {
        eprintln!("  │ est. cost: ${cost:.4}");
    }
    eprintln!("  └──────────────────────────────────────────────");
    eprint!("  Approve? [y/N, or type what to change]: ");
    let _ = std::io::stderr().flush();

    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() || line.is_empty() {
        // EOF mid-attach. Unlike the stdio gate this terminal is not the only
        // chance to answer — the run stays parked for the next attach — so
        // an absent answer writes nothing rather than aborting someone
        // else's turn.
        *noted = false;
        return;
    }
    let decision = decision_from_line(&line);
    let Ok(bytes) = serde_json::to_vec_pretty(&decision) else {
        return;
    };
    if stella_store::write_sensitive_file_atomic(&sidecar.join(supervised::APPROVAL_ANSWER), &bytes)
        .is_err()
    {
        eprintln!(
            "  {} could not deliver the answer — the run is still parked",
            "!".yellow()
        );
        return;
    }
    *noted = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry_in(dir: &Path) -> SessionRegistry {
        SessionRegistry::open(dir.join("sessions"))
    }

    fn parked_record(registry: &SessionRegistry) -> stella_store::SessionRecord {
        let record = stella_store::SessionRecord::new("/ws", "t");
        registry.upsert(&record).unwrap();
        registry.prepare_sidecar(&record.id).unwrap();
        record
    }

    fn proposal() -> ScopeProposal {
        ScopeProposal {
            summary: "widen the frobnicator".into(),
            steps: vec!["a".into(), "b".into()],
            estimated_files: 9,
            estimated_cost_usd: Some(1.25),
            ..ScopeProposal::default()
        }
    }

    /// The #1585 witness, request → parked → answered → resumed: the gate
    /// parks the run as `NeedsInput` with the proposal on disk, and an answer
    /// written the way an attached terminal writes one unparks it with that
    /// decision and an `InProgress` record. On `main` this transport does not
    /// exist and a supervised run's gate can only abort.
    #[tokio::test]
    async fn a_parked_review_is_answered_through_the_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry_in(dir.path());
        let record = parked_record(&registry);
        let sidecar = registry.sidecar_dir(&record.id);
        let gate = SidecarApprovalGate::new(sidecar.clone(), registry.clone(), record.id.clone());

        let review = tokio::spawn(async move { gate.review(&proposal()).await });

        // Wait for the park: request file present, record flipped.
        let request = sidecar.join(supervised::APPROVAL_REQUEST);
        for _ in 0..200 {
            if request.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let parked: ScopeProposal =
            serde_json::from_str(&std::fs::read_to_string(&request).unwrap()).unwrap();
        assert_eq!(parked.summary, "widen the frobnicator");
        assert_eq!(
            registry.get(&record.id).unwrap().status,
            SessionStatus::NeedsInput,
            "a parked run must be discoverable as Needs Input"
        );

        // The attached terminal answers with a revision note.
        let answer = serde_json::to_vec(&ScopeDecision::Revise {
            note: "docs only".into(),
        })
        .unwrap();
        stella_store::write_sensitive_file_atomic(
            &sidecar.join(supervised::APPROVAL_ANSWER),
            &answer,
        )
        .unwrap();

        assert_eq!(
            review.await.unwrap(),
            ScopeDecision::Revise {
                note: "docs only".into()
            }
        );
        assert_eq!(
            registry.get(&record.id).unwrap().status,
            SessionStatus::InProgress,
            "an answered review hands the record back to the run"
        );
        assert!(
            !request.exists() && !sidecar.join(supervised::APPROVAL_ANSWER).exists(),
            "the transport cleans up after itself"
        );
    }

    /// `stop`'s grace period depends on this: a parked child asked to stop
    /// unparks as an abort instead of ignoring `SIGTERM` until the `SIGKILL`
    /// escalation records it as a crash.
    #[tokio::test]
    async fn an_interrupt_unparks_as_an_abort() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry_in(dir.path());
        let record = parked_record(&registry);
        let gate = SidecarApprovalGate::new(
            registry.sidecar_dir(&record.id),
            registry.clone(),
            record.id.clone(),
        )
        .with_interrupt_probe(|| true);

        assert_eq!(gate.review(&proposal()).await, ScopeDecision::Abort);
    }

    /// A crashed predecessor's answer must not approve the next run's plan.
    #[tokio::test]
    async fn a_stale_answer_does_not_answer_a_new_question() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry_in(dir.path());
        let record = parked_record(&registry);
        let sidecar = registry.sidecar_dir(&record.id);
        // The stale approval, left over from a previous life.
        stella_store::write_sensitive_file_atomic(
            &sidecar.join(supervised::APPROVAL_ANSWER),
            &serde_json::to_vec(&ScopeDecision::Approve).unwrap(),
        )
        .unwrap();
        let gate = SidecarApprovalGate::new(sidecar, registry.clone(), record.id.clone())
            .with_interrupt_probe(|| true);

        // With the stale answer cleared at park time, the interrupted gate
        // falls through to abort rather than adopting the ghost's yes.
        assert_eq!(gate.review(&proposal()).await, ScopeDecision::Abort);
    }

    /// A piped attach notes the parked run once and answers nothing.
    #[test]
    fn a_non_interactive_follower_notes_but_never_answers() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry_in(dir.path());
        let record = parked_record(&registry);
        let sidecar = registry.sidecar_dir(&record.id);
        stella_store::write_sensitive_file_atomic(
            &sidecar.join(supervised::APPROVAL_REQUEST),
            &serde_json::to_vec(&proposal()).unwrap(),
        )
        .unwrap();

        let mut noted = false;
        forward_pending_approval(&sidecar, false, &mut noted);
        assert!(noted, "the parked state is said out loud, once");
        forward_pending_approval(&sidecar, false, &mut noted);
        assert!(
            !sidecar.join(supervised::APPROVAL_ANSWER).exists(),
            "a follower with no human on stdin must not answer"
        );

        // The request clearing re-arms the note for a later, second review.
        std::fs::remove_file(sidecar.join(supervised::APPROVAL_REQUEST)).unwrap();
        forward_pending_approval(&sidecar, false, &mut noted);
        assert!(!noted);
    }
}
