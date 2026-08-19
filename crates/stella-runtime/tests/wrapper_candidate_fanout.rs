//! **The witness for `candidate_fanout`** (#3844, `doc:pipeline-as-plugins`
//! §3/§7 item 4): a plugin asks the host for N isolated writable workspaces,
//! the host — not the plugin — creates them and spends a worker turn in each,
//! and the plugin names the winner the host then applies.
//!
//! Every test here fails before the change for one reason: **the socket could
//! not express the ask.** `HostCall` was a closed set of three, all read-only
//! or single-tracked; `RoundInput::candidate` carried at most one grant, built
//! once before the round loop and reused unchanged; and the only grant a host
//! ever minted addressed the shared live work tree
//! (`stella_plugin::HOST_TREE_HANDLE`), never an isolated one. So
//! `plugins/stella-candidates` could not be written at all — the strongest
//! plugin the socket permitted was bounded retry with correction over the one
//! real tree, which is a different operation.
//!
//! The plugins below are `sh` scripts with no JSON library, for
//! `wrapper_host_call.rs`'s reason: a capability only Rust can reach is a Rust
//! API with extra steps (`doc:pipeline-as-plugins` §5 commitment 2).
//!
//! **What the plugin never touches, and the tests assert it:** the substrate
//! records every operation it was asked to perform, so a test reads what the
//! host was actually asked to do — how many workspaces, at which seat, on what
//! share of the budget, and which one was applied — and confirms the only
//! three things the plugin contributed are the three fields
//! `CandidateFanoutArgs` has.
//!
//! `cfg(unix)` is the same declared gap the rest of this suite carries, tracked
//! in #3497.

#![cfg(unix)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use stella_plugin::{
    BeforeTurnRequest, HostCallRefusal, HostStage, PROTOCOL_VERSION, PluginManifest, StageName,
};
use stella_protocol::candidate::CandidateHandle;
use stella_protocol::event::ModelCallRole;
use stella_runtime::wrapper::{
    CandidateFanoutError, CandidateFanouts, CandidateReport, CandidateWork, CandidateWorkspace,
    CandidateWorkspaces, DEFAULT_HOST_MAX_CALLS, HostCallGate, HostPlanes, SubprocessWrapper,
    TurnWrapper, admissible,
};

/// What a human consents to at install: this plugin answers `before_turn`, may
/// ask for both halves of the capability, and declares three role intents.
///
/// `scout` and `nowhere` are declared deliberately. The seat rule has to be
/// tested against roles the *manifest permits*, or it proves only that the
/// manifest check works.
const CANDIDATES_MANIFEST: &str = r#"
name = "candidates-wrapper"
description = "asks the host to try the goal three ways and lands the best one"

[loop]
participation = "steering"
points = ["before_turn"]
calls = ["candidate_fanout", "adopt_candidate"]
max_fanout_width = 3

[subloop]
stages = ["research"]

[roles.attempt]
tier = "worker"

[roles.scout]
tier = "research"

[roles.nowhere]
tier = "archivist"

[wrapper]
id = "candidates-v1"

[[wrapper.stages]]
name = "research"
"#;

/// One thing the host asked the isolation substrate to do.
#[derive(Debug, Clone, PartialEq)]
enum Op {
    Created(String),
    Worked {
        handle: String,
        seat: ModelCallRole,
        instruction: String,
        budget_usd: Option<f64>,
    },
    Adopted(String),
    Removed(String),
}

/// The host's isolation substrate, recorded rather than real.
///
/// It records every operation, which is how these tests answer the question
/// that matters: **who created the workspaces and who spent the turns?** A
/// plugin that had reached for a worktree or a provider itself would leave
/// nothing here.
#[derive(Default, Clone)]
struct Substrate {
    ops: Arc<Mutex<Vec<Op>>>,
    minted: Arc<Mutex<u32>>,
    /// When set, `create` refuses from this ordinal on — the partial-creation
    /// case, which is not the same event as "nothing could be created".
    creates_until: Option<u32>,
    /// When true, `adopt` refuses. The user's tree stays untouched, and the
    /// losing candidates must survive.
    adoption_fails: bool,
}

impl Substrate {
    fn ops(&self) -> Vec<Op> {
        self.ops
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn record(&self, op: Op) {
        self.ops
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(op);
    }
}

#[async_trait]
impl CandidateWorkspaces for Substrate {
    async fn create(&self, label: &str) -> Result<CandidateWorkspace, CandidateFanoutError> {
        let ordinal = {
            let mut minted = self
                .minted
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *minted += 1;
            *minted
        };
        if self.creates_until.is_some_and(|limit| ordinal > limit) {
            return Err(CandidateFanoutError::NotCreated {
                reason: "the isolation substrate is out of room".to_string(),
            });
        }
        self.record(Op::Created(label.to_string()));
        Ok(CandidateWorkspace {
            handle: CandidateHandle::new(format!("candidate-{ordinal}")),
            root: format!("/tmp/stella-candidates/candidate-{ordinal}"),
        })
    }

    async fn work(
        &self,
        workspace: &CandidateWorkspace,
        work: CandidateWork,
    ) -> Result<CandidateReport, CandidateFanoutError> {
        self.record(Op::Worked {
            handle: workspace.handle.to_string(),
            seat: work.seat,
            instruction: work.instruction.clone(),
            budget_usd: work.budget_usd,
        });
        // The second candidate is the good one, so a test that asserts the
        // plugin's choice is asserting a choice and not an ordering.
        let good = workspace.handle.as_str() == "candidate-2";
        Ok(CandidateReport {
            report: if good {
                "green: the flip test passes".to_string()
            } else {
                "red: the flip test still fails".to_string()
            },
            completed: true,
            cost_usd: 0.05,
            files_changed: if good { 2 } else { 7 },
            lines_changed: if good { 40 } else { 900 },
        })
    }

    async fn adopt(&self, workspace: &CandidateWorkspace) -> Result<(), CandidateFanoutError> {
        if self.adoption_fails {
            return Err(CandidateFanoutError::NotAdopted {
                handle: workspace.handle.clone(),
                reason: "the real tree has uncommitted changes".to_string(),
            });
        }
        self.record(Op::Adopted(workspace.handle.to_string()));
        Ok(())
    }

    async fn remove(&self, workspace: &CandidateWorkspace) -> Result<(), CandidateFanoutError> {
        self.record(Op::Removed(workspace.handle.to_string()));
        Ok(())
    }
}

fn manifest() -> PluginManifest {
    PluginManifest::from_toml_str(CANDIDATES_MANIFEST).expect("the manifest loads")
}

/// The host a driver would assemble: the plugin's declared role intents bound
/// to this host's isolation substrate, behind the gate the manifest declares.
fn host(
    manifest: &PluginManifest,
    substrate: Substrate,
    budget_usd: Option<f64>,
) -> (Arc<HostCallGate>, Arc<CandidateFanouts<Substrate>>) {
    let plane =
        Arc::new(CandidateFanouts::declare(manifest, substrate).with_budget_usd(budget_usd));
    let gate = Arc::new(HostCallGate::declare(
        manifest.loop_grant.clone(),
        DEFAULT_HOST_MAX_CALLS,
        Box::new(HostPlanes::none().with_candidate_fanout(Arc::clone(&plane))),
    ));
    (gate, plane)
}

fn plugin(script: &str, gate: Arc<HostCallGate>) -> SubprocessWrapper {
    SubprocessWrapper::declare(
        vec!["/bin/sh".into(), "-c".into(), script.into()],
        Vec::new(),
        Duration::from_secs(10),
    )
    .expect("the transport is declared with a program and a budget")
    .wrapper
    .serving(gate)
}

fn before() -> BeforeTurnRequest {
    BeforeTurnRequest {
        protocol_version: PROTOCOL_VERSION,
        wrapper: "candidates-v1".into(),
        stage: StageName::Host(HostStage::Research),
        round: 0,
        goal: "make tests/test_flip.py pass".into(),
        candidate: None,
        published: Vec::new(),
    }
}

/// **The witness.** Best-of-N, expressed entirely on the socket: the plugin
/// asks for three attempts, reads the per-candidate evidence, names the winner,
/// and the host lands it.
///
/// Five claims, and the middle three are what `#3844` said could not be said:
///
/// 1. the plugin got three candidates back through the wire, each with a
///    handle, a root and the evidence it scores on;
/// 2. the **host** created three isolated workspaces — the plugin named no
///    path and spawned nothing;
/// 3. the **host** spent three writing turns, every one at the worker's seat;
/// 4. the plugin's adoption named a handle, never a directory, and the host
///    applied exactly that one;
/// 5. the losers were discarded, so a losing attempt leaves nothing behind.
#[tokio::test]
async fn a_plugin_asks_for_three_attempts_scores_them_and_the_host_lands_the_winner() {
    let manifest = manifest();
    let substrate = Substrate::default();
    let (gate, plane) = host(&manifest, substrate.clone(), Some(0.30));

    // The plugin picks the candidate whose report is green and whose diff is
    // smallest — a real (if crude) selection, made from the wire's evidence.
    let script = r#"
read -r request
printf '%s\n' '{"call":"candidate_fanout","id":7,"args":{"role":"attempt","instruction":"make tests/test_flip.py pass","width":3}}'
read -r answer
winner=""
case "$answer" in
  *'"candidate":"candidate-2","root":"/tmp/stella-candidates/candidate-2","report":"green'*) winner="candidate-2" ;;
esac
if [ -z "$winner" ]; then
  printf '{"point":"before_turn","body":{"protocol_version":1,"context":[{"label":"candidates","text":"no green candidate"}]}}\n'
  exit 0
fi
printf '{"call":"adopt_candidate","id":8,"args":{"candidate":"%s"}}\n' "$winner"
read -r applied
case "$applied" in
  *'"adopted":"candidate-2"'*) note="landed candidate-2 of 3" ;;
  *) note="the adoption did not land" ;;
esac
printf '{"point":"before_turn","body":{"protocol_version":1,"context":[{"label":"candidates","text":"%s"}]}}\n' "$note"
"#;

    let response = plugin(script, Arc::clone(&gate))
        .before_turn(before())
        .await
        .expect("the plugin asked twice, was answered twice, and answered the point");

    let messages = admissible(&manifest, response)
        .expect("the contribution is within what the manifest declared")
        .into_messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].content, "landed candidate-2 of 3",
        "the plugin scored three real candidates and saw its winner applied"
    );

    let ops = substrate.ops();
    let created: Vec<_> = ops
        .iter()
        .filter(|op| matches!(op, Op::Created(_)))
        .collect();
    assert_eq!(
        created.len(),
        3,
        "the host created one isolated workspace per candidate: {ops:?}"
    );

    let worked: Vec<_> = ops
        .iter()
        .filter_map(|op| match op {
            Op::Worked {
                seat,
                instruction,
                budget_usd,
                ..
            } => Some((*seat, instruction.clone(), *budget_usd)),
            _ => None,
        })
        .collect();
    assert_eq!(worked.len(), 3, "one writing turn per candidate");
    for (seat, instruction, budget) in &worked {
        assert_eq!(
            *seat,
            ModelCallRole::Worker,
            "a candidate is the work, so it is booked at the worker's seat"
        );
        assert_eq!(instruction, "make tests/test_flip.py pass");
        // $0.30 over three candidates: the carve divided, not handed to each.
        assert!(
            budget.is_some_and(|share| (share - 0.10).abs() < 1e-9),
            "each candidate gets its share of the fan-out's carve, not all of it: {budget:?}"
        );
    }

    assert!(
        ops.contains(&Op::Adopted("candidate-2".to_string())),
        "the host applied the candidate the plugin named: {ops:?}"
    );
    assert!(
        !ops.iter()
            .any(|op| matches!(op, Op::Adopted(handle) if handle != "candidate-2")),
        "exactly one candidate is applied: {ops:?}"
    );
    for loser in ["candidate-1", "candidate-3"] {
        assert!(
            ops.contains(&Op::Removed(loser.to_string())),
            "a losing attempt is discarded rather than left on disk: {ops:?}"
        );
    }
    assert!(
        ops.contains(&Op::Removed("candidate-2".to_string())),
        "the winner's scratch copy goes too — its bytes are on the real tree, and the table \
         that addressed it has just been emptied, so leaving it leaks a worktree nothing can \
         reach: {ops:?}"
    );

    let spends = plane.spends();
    assert_eq!(
        spends.len(),
        1,
        "the fan-out's spend is visible, not hidden"
    );
    assert_eq!(spends[0].role, "attempt");
    assert_eq!(spends[0].seat, ModelCallRole::Worker);
    assert_eq!(spends[0].requested_width, 3);
    assert_eq!(spends[0].width, 3);
    assert_eq!(spends[0].completed, 3);
    assert!((spends[0].cost_usd - 0.15).abs() < 1e-9);

    assert!(
        plane.live().is_empty(),
        "adoption empties the table, so a second adoption cannot apply a second diff"
    );
    assert!(
        gate.refusals().is_empty(),
        "declared calls inside the allowance are performed: {:?}",
        gate.refusals()
    );
}

/// The three refusals, each a different question and each reaching the plugin
/// as a value it can branch on — `child_turn.rs`'s shape, at the capability
/// whose seat rule is inverted.
///
/// `nowhere` is the interesting one: its tier is declared, so the manifest
/// check passes, and it still resolves to no seat this host serves. Telling a
/// plugin author "undeclared" there would send them to edit a manifest that is
/// already correct.
#[tokio::test]
async fn the_three_refusals_are_distinguishable_and_none_of_them_creates_a_workspace() {
    let manifest = manifest();
    let substrate = Substrate::default();
    let plane = CandidateFanouts::declare(&manifest, substrate.clone());

    let undeclared = plane
        .resolve("auditor")
        .expect_err("a role intent the manifest never declared");
    assert_eq!(undeclared.refusal, HostCallRefusal::Undeclared);
    assert!(
        undeclared.detail.contains("attempt"),
        "the refusal names what the manifest did declare: {undeclared}"
    );

    let unavailable = plane
        .resolve("nowhere")
        .expect_err("a tier this host binds to no responsibility");
    assert_eq!(unavailable.refusal, HostCallRefusal::Unavailable);
    assert!(
        unavailable.detail.contains("archivist"),
        "the refusal names the tier that resolved nowhere: {unavailable}"
    );

    let forbidden = plane
        .resolve("scout")
        .expect_err("a writing turn booked anywhere but the worker's seat");
    assert_eq!(forbidden.refusal, HostCallRefusal::Forbidden);
    assert!(
        forbidden.detail.contains("research"),
        "the refusal names the seat it would have been misbooked at: {forbidden}"
    );

    assert_eq!(
        plane.resolve("attempt").expect("the worker tier resolves"),
        ModelCallRole::Worker
    );
    assert!(
        substrate.ops().is_empty(),
        "a refusal the plugin could never have bought creates nothing: {:?}",
        substrate.ops()
    );
}

/// The width is an ask, never an authority, and the clamp is *reported* rather
/// than silently applied: a plugin that asked for eight and got three must be
/// able to say so, or it will report having scored a set it never saw.
#[tokio::test]
async fn a_width_over_the_ceiling_is_clamped_and_the_clamp_is_reported_back() {
    let manifest = manifest();
    let substrate = Substrate::default();
    let (gate, plane) = host(&manifest, substrate.clone(), None);

    let script = r#"
read -r request
printf '%s\n' '{"call":"candidate_fanout","id":1,"args":{"role":"attempt","instruction":"try it","width":8}}'
read -r answer
case "$answer" in
  *'"requested":8'*) asked="8" ;;
  *) asked="?" ;;
esac
ran=$(printf '%s' "$answer" | tr ',' '\n' | grep -c '"candidate":"candidate-')
printf '{"point":"before_turn","body":{"protocol_version":1,"context":[{"label":"candidates","text":"asked %s, ran %s"}]}}\n' "$asked" "$ran"
"#;

    let response = plugin(script, Arc::clone(&gate))
        .before_turn(before())
        .await
        .expect("the plugin asked and was answered");
    let messages = admissible(&manifest, response)
        .expect("within the declared grant")
        .into_messages();
    assert_eq!(
        messages[0].content, "asked 8, ran 3",
        "the ask and the clamp are both on the wire"
    );

    assert_eq!(
        substrate
            .ops()
            .iter()
            .filter(|op| matches!(op, Op::Created(_)))
            .count(),
        3,
        "the host's ceiling decides how many workspaces exist, not the plugin's ask"
    );
    assert_eq!(plane.spends()[0].requested_width, 8);
    assert_eq!(plane.spends()[0].width, 3);
}

/// The allowance bounds the whole run, not the point. A ceiling that refreshed
/// per point would be no ceiling at all across N rounds — the shape a hold
/// allowance that resets would have.
#[tokio::test]
async fn the_fanout_allowance_is_spent_for_the_whole_run() {
    let manifest = manifest();
    let plane = CandidateFanouts::declare(&manifest, Substrate::default()).with_max_fanouts(1);
    let args = |width: u32| stella_plugin::CandidateFanoutArgs {
        role: "attempt".to_string(),
        instruction: "try it".to_string(),
        width,
    };
    use stella_runtime::wrapper::CandidateFanoutPlane;

    plane.fanout(args(1)).await.expect("the first fan-out runs");
    let spent = plane
        .fanout(args(1))
        .await
        .expect_err("the second is over the allowance");
    assert_eq!(spent.refusal, HostCallRefusal::AllowanceSpent);
    assert!(spent.detail.contains('1'), "{spent}");
}

/// Adoption is fenced against the host's own table, and the fence is what keeps
/// `HOST_TREE_HANDLE` un-adoptable for free: it names no entry in any table, so
/// a plugin that spells it back is told the same "no such candidate" it has
/// always been told. A refused adoption must also leave the run's candidates
/// where they were — the losers are the only copies of work that might still be
/// wanted.
#[tokio::test]
async fn a_handle_the_host_does_not_hold_adopts_nothing_and_discards_nothing() {
    use stella_runtime::wrapper::CandidateFanoutPlane;

    let manifest = manifest();
    let substrate = Substrate::default();
    let plane = CandidateFanouts::declare(&manifest, substrate.clone());
    plane
        .fanout(stella_plugin::CandidateFanoutArgs {
            role: "attempt".to_string(),
            instruction: "try it".to_string(),
            width: 2,
        })
        .await
        .expect("the fan-out runs");

    for spelled in [stella_plugin::HOST_TREE_HANDLE, "candidate-99"] {
        let refused = plane
            .adopt(stella_plugin::AdoptCandidateArgs {
                candidate: CandidateHandle::new(spelled),
            })
            .await
            .expect_err("a handle this host does not hold");
        assert_eq!(refused.refusal, HostCallRefusal::Unavailable);
        assert!(
            refused.detail.contains("candidate-1"),
            "the refusal names the candidates that do exist: {refused}"
        );
    }

    assert_eq!(
        plane.live().len(),
        2,
        "a refused adoption leaves the run's candidates exactly where they were"
    );
    assert!(
        !substrate
            .ops()
            .iter()
            .any(|op| matches!(op, Op::Adopted(_) | Op::Removed(_))),
        "nothing was applied and nothing was thrown away: {:?}",
        substrate.ops()
    );
}

/// An adoption the substrate could not apply leaves the user's tree untouched
/// **and** every candidate intact. Discarding the losers because the winner
/// would not apply is the one irreversible move available here, and it is the
/// one this test forbids.
#[tokio::test]
async fn an_adoption_that_fails_throws_no_candidate_away() {
    use stella_runtime::wrapper::CandidateFanoutPlane;

    let manifest = manifest();
    let substrate = Substrate {
        adoption_fails: true,
        ..Substrate::default()
    };
    let plane = CandidateFanouts::declare(&manifest, substrate.clone());
    plane
        .fanout(stella_plugin::CandidateFanoutArgs {
            role: "attempt".to_string(),
            instruction: "try it".to_string(),
            width: 2,
        })
        .await
        .expect("the fan-out runs");

    let failed = plane
        .adopt(stella_plugin::AdoptCandidateArgs {
            candidate: CandidateHandle::new("candidate-1"),
        })
        .await
        .expect_err("the substrate refused to apply it");
    assert_eq!(failed.refusal, HostCallRefusal::Failed);
    assert!(failed.detail.contains("uncommitted"), "{failed}");

    assert_eq!(plane.live().len(), 2, "both candidates survive");
    assert!(
        !substrate
            .ops()
            .iter()
            .any(|op| matches!(op, Op::Removed(_))),
        "a failed adoption throws nothing away: {:?}",
        substrate.ops()
    );

    // And the end-of-run sweep is what does clear them, reporting rather than
    // swallowing whatever will not go.
    assert!(plane.discard_all().await.is_empty());
    assert_eq!(
        substrate
            .ops()
            .iter()
            .filter(|op| matches!(op, Op::Removed(_)))
            .count(),
        2
    );
    assert!(plane.live().is_empty());
}

/// A substrate that runs out of room part-way through is a partial fan-out, not
/// a failure: the candidates that exist are real and scoreable, and the plugin
/// is told what it got. Only a fan-out with *nothing* in it is an error, because
/// there is nothing to score.
#[tokio::test]
async fn a_partial_fanout_reports_what_exists_and_an_empty_one_is_an_error() {
    use stella_runtime::wrapper::CandidateFanoutPlane;

    let manifest = manifest();
    let partial = CandidateFanouts::declare(
        &manifest,
        Substrate {
            creates_until: Some(2),
            ..Substrate::default()
        },
    );
    let result = partial
        .fanout(stella_plugin::CandidateFanoutArgs {
            role: "attempt".to_string(),
            instruction: "try it".to_string(),
            width: 3,
        })
        .await
        .expect("two of three is still a fan-out");
    assert_eq!(result.requested, 3);
    assert_eq!(result.candidates.len(), 2);
    assert_eq!(partial.spends()[0].width, 2);

    let empty = CandidateFanouts::declare(
        &manifest,
        Substrate {
            creates_until: Some(0),
            ..Substrate::default()
        },
    );
    let failure = empty
        .fanout(stella_plugin::CandidateFanoutArgs {
            role: "attempt".to_string(),
            instruction: "try it".to_string(),
            width: 3,
        })
        .await
        .expect_err("a fan-out with nothing in it");
    assert_eq!(failure.refusal, HostCallRefusal::Failed);
    assert!(failure.detail.contains("out of room"), "{failure}");
}

/// A host with no isolation substrate answers `unavailable` — a declared gap the
/// plugin is told about, never a silence it waits on. Both halves answer it,
/// because they are one plane.
#[tokio::test]
async fn a_host_with_no_substrate_declares_the_gap_rather_than_falling_silent() {
    use stella_runtime::wrapper::HostCapabilities;

    let planes = HostPlanes::none();
    for args in [
        stella_plugin::HostCallArgs::CandidateFanout(stella_plugin::CandidateFanoutArgs {
            role: "attempt".to_string(),
            instruction: "try it".to_string(),
            width: 2,
        }),
        stella_plugin::HostCallArgs::AdoptCandidate(stella_plugin::AdoptCandidateArgs {
            candidate: CandidateHandle::new("candidate-1"),
        }),
    ] {
        let refused = planes.call(args).await.expect_err("no substrate");
        assert_eq!(refused.refusal, HostCallRefusal::Unavailable);
        assert!(refused.detail.contains("isolation substrate"), "{refused}");
    }
}
