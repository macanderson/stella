//! The shell-hook decision plane (#2684): stdout JSON from a
//! settings-declared hook carries the extension bus's decision vocabulary.
//!
//! Before this module, a `PreToolUse` shell hook had exactly one verb —
//! exit non-zero to block — while in-process extensions on the
//! [`crate::bus::HookBus`] spoke a four-way vocabulary
//! ([`crate::bus::HookDecision`]: `allow` / `deny` / `require_approval` /
//! `modify`). This module gives the shell surface the SAME vocabulary
//! rather than a parallel one: a hook prints one JSON decision document on
//! stdout — `{"action":"deny","reason":…}`,
//! `{"action":"modify","payload":{"input":…}}`, … — and
//! [`run_decision_hooks`] folds a matched chain of them exactly the way
//! [`crate::bus::HookBus::emit_blocking`] folds a policy chain: the first
//! `deny`/`require_approval` short-circuits, `modify` folds into the
//! payload and the chain continues, a chain that only modified ends
//! `allow`.
//!
//! # The failure posture (OXA-2056)
//!
//! A hook that exits non-zero, never runs, or prints a *malformed*
//! decision (JSON with an `action` key that does not deserialize) is an
//! **evaluation failure**, not a decision — [`DecisionRun::evaluation`]
//! carries it as `Err`, and [`resolve_precedence`] turns any errored
//! evaluation into an unconditional deny, whatever the
//! enforcement-softening flag says. Stdout that is not a decision at all
//! (plain prose, or JSON without an `action` key) is informational, as it
//! always was: the legacy contract — exit zero allows, non-zero blocks —
//! is unchanged for hooks that never print a decision.
//!
//! # Precedence lives here, not in a fork
//!
//! [`resolve_precedence`] moved into this module from
//! `stella-tools::registry::approval` (which re-exports it, so the wave-1
//! #2676 seam is unchanged) because two producers now feed it — the bus
//! blocking chains and this shell surface — and the engine-side consumer
//! (`driver::user_hooks`) sits below `stella-tools` in the dependency
//! graph. One pure function, two feeders, zero copies.
//!
//! # What stays outside
//!
//! Spawning the hook is still the [`HookRunner`] port's job, and resolving
//! a `require_approval` to a human answer is the [`ApprovalRoute`]
//! port's — implemented by `stella-tools::hook_bridge` over the #2676
//! `ApprovalBroker` (emit-before-park, TTL, `approval.*` audit events).
//! This module is pure fold and vocabulary; no I/O (invariant #2).
//!
//! # Seams
//!
//! - **#2716 (`AuthzGate` / `ToolContract`)**: today the tool metadata a
//!   payload carries is the one advertised bit (`HookToolInfo::read_only`);
//!   the fuller contract joins the payload — and this fold — when it
//!   exists.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{HookExecError, HookPayload, HookRunner, Hooks, select_matchers};
use crate::bus::HookDecision;

/// What one hook's stdout said, decision-wise.
#[derive(Debug, Clone, PartialEq)]
pub enum DecisionParse {
    /// Stdout carried no decision document — prose, nothing, or JSON
    /// without an `action` key. Informational output, legacy semantics.
    NoDecision,
    /// A well-formed decision.
    Decision(HookDecision),
    /// Stdout tried to speak the protocol (JSON object with an `action`
    /// key) and got it wrong. Fails the evaluation — a hook that
    /// half-speaks policy must not silently allow.
    Malformed { error: String },
}

/// Parse a hook's stdout for a decision document.
///
/// The whole trimmed stdout is tried first; if that is not JSON, the last
/// non-empty line is tried, so a hook that logs progress before printing
/// its decision still works. Only a JSON **object carrying an `action`
/// key** is treated as a decision attempt — anything else is
/// [`DecisionParse::NoDecision`], which keeps a `cat`-style hook (whose
/// stdout echoes the payload, itself a JSON object without `action`) on
/// the legacy path.
pub fn parse_decision(stdout: &str) -> DecisionParse {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return DecisionParse::NoDecision;
    }
    let candidate = match serde_json::from_str::<Value>(trimmed) {
        Ok(value) => Some(value),
        Err(_) => trimmed
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .and_then(|line| serde_json::from_str::<Value>(line).ok()),
    };
    let Some(value) = candidate else {
        return DecisionParse::NoDecision;
    };
    if !value
        .as_object()
        .is_some_and(|object| object.contains_key("action"))
    {
        return DecisionParse::NoDecision;
    }
    match serde_json::from_value::<HookDecision>(value) {
        Ok(decision) => DecisionParse::Decision(decision),
        Err(error) => DecisionParse::Malformed {
            error: error.to_string(),
        },
    }
}

/// Why a hook chain's evaluation failed — the `Err` arm of
/// [`DecisionRun::evaluation`], each variant naming the hook so the deny
/// reason a model (or a human) reads points at the command to fix.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HookEvaluationFailure {
    /// The hook ran and exited non-zero. `detail` is `": <stderr>"` when
    /// the hook printed any, empty otherwise — so the operator's own words
    /// survive into the deny reason.
    #[error("hook `{command}` exited {exit_code}{detail}")]
    HookFailed {
        command: String,
        exit_code: i32,
        detail: String,
    },
    /// The hook never produced an exit code (spawn failure, timeout,
    /// abort).
    #[error(transparent)]
    NeverRan(#[from] HookExecError),
    /// The hook printed a malformed decision document.
    #[error("hook `{command}` printed a malformed decision: {error}")]
    MalformedDecision { command: String, error: String },
}

/// The folded result of running one event's decision-aware hooks.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionRun {
    /// The chain's folded decision — never `Modify` (modifications fold
    /// into [`Self::rewritten_input`] / [`Self::modify_payload`] and the
    /// chain continues, mirroring [`crate::bus::PolicyOutcome`]) — or the
    /// evaluation failure that ends it.
    pub evaluation: Result<HookDecision, HookEvaluationFailure>,
    /// For `PreToolUse`: the tool input after any `modify` rewrites.
    /// `None` means untouched.
    pub rewritten_input: Option<Value>,
    /// The last `modify` decision's raw payload, for events whose
    /// modifiable surface is not a tool input (`PreCompact` reads
    /// `instructions` out of it). `None` when nothing modified.
    pub modify_payload: Option<Value>,
    /// Concatenated non-decision stdout, the same informational channel
    /// [`super::run_hooks`] collects.
    pub output: String,
    /// Non-fatal problems observed along the way (a `modify` that dropped
    /// the `input` field, for one). Callers surface these as warnings.
    pub diagnostics: Vec<String>,
}

impl DecisionRun {
    fn allow() -> Self {
        Self {
            evaluation: Ok(HookDecision::Allow),
            rewritten_input: None,
            modify_payload: None,
            output: String::new(),
            diagnostics: Vec::new(),
        }
    }

    /// Feed this run into [`resolve_precedence`] — never a caller-side
    /// re-implementation of the ladder. `operator` and `softened` are the
    /// caller's to supply; an engine consulting only workspace hooks
    /// passes [`OperatorPosture::NoOpinion`] and `false`.
    pub fn verdict(&self, operator: &OperatorPosture, softened: bool) -> GateVerdict {
        let failure_text = self.evaluation.as_ref().err().map(ToString::to_string);
        let evaluation = match (&self.evaluation, &failure_text) {
            (Ok(decision), _) => Ok(decision),
            (Err(_), Some(text)) => Err(text.as_str()),
            // Unreachable: `failure_text` is `Some` exactly when the
            // evaluation is `Err` — spelled out so the match is total.
            (Err(_), None) => Err("hook evaluation failed"),
        };
        resolve_precedence(operator, evaluation, softened)
    }
}

/// Run every decision-aware hook registered for `payload.event`, in
/// matcher order then action order, folding stdout decisions per the
/// module docs. The payload later hooks receive on stdin reflects earlier
/// `modify` rewrites, exactly as later bus handlers see a modified event
/// payload.
pub async fn run_decision_hooks(
    runner: &dyn HookRunner,
    hooks: Option<&Hooks>,
    payload: &HookPayload,
) -> DecisionRun {
    let Some(hooks) = hooks else {
        return DecisionRun::allow();
    };
    let matchers = hooks.matchers_for(payload.event);
    let tool_name = payload.tool.as_ref().map(|t| t.name.as_str());
    let selected = select_matchers(payload.event, matchers, tool_name);
    if selected.is_empty() {
        return DecisionRun::allow();
    }

    let mut current = payload.clone();
    let mut payload_json = serde_json::to_string(&current).unwrap_or_else(|_| "{}".to_string());
    let mut run = DecisionRun::allow();
    let mut outputs: Vec<String> = Vec::new();

    for matcher in selected {
        for action in &matcher.hooks {
            let result = match runner.run(action, &payload_json, &current.cwd).await {
                Ok(result) => result,
                Err(err) => {
                    run.evaluation = Err(HookEvaluationFailure::NeverRan(err));
                    run.output = outputs.join("\n");
                    return run;
                }
            };
            if result.exit_code != 0 {
                // OXA-2056: a non-zero exit is an evaluation failure, not
                // a decision — whatever stdout says. The stderr rides the
                // failure so the operator's words reach the deny reason.
                let trimmed_stderr = result.stderr.trim();
                run.evaluation = Err(HookEvaluationFailure::HookFailed {
                    command: action.command.clone(),
                    exit_code: result.exit_code,
                    detail: if trimmed_stderr.is_empty() {
                        String::new()
                    } else {
                        format!(": {trimmed_stderr}")
                    },
                });
                run.output = outputs.join("\n");
                return run;
            }
            match parse_decision(&result.stdout) {
                DecisionParse::NoDecision => {
                    let trimmed = result.stdout.trim();
                    if !trimmed.is_empty() {
                        outputs.push(trimmed.to_string());
                    }
                }
                DecisionParse::Malformed { error } => {
                    run.evaluation = Err(HookEvaluationFailure::MalformedDecision {
                        command: action.command.clone(),
                        error,
                    });
                    run.output = outputs.join("\n");
                    return run;
                }
                DecisionParse::Decision(HookDecision::Allow) => {}
                DecisionParse::Decision(HookDecision::Modify { payload: modify }) => {
                    if let Some(tool) = current.tool.as_mut() {
                        match modify.get("input") {
                            Some(new_input) => {
                                tool.input = new_input.clone();
                                run.rewritten_input = Some(new_input.clone());
                                payload_json = serde_json::to_string(&current)
                                    .unwrap_or_else(|_| "{}".to_string());
                            }
                            None => {
                                // Mirror `gate_tool_call`: a modify that
                                // dropped the field is a broken hook —
                                // keep the original input, leave a trace.
                                run.diagnostics.push(format!(
                                    "hook `{}` returned a modify decision without an `input` \
                                     field; original input kept",
                                    action.command
                                ));
                            }
                        }
                    }
                    run.modify_payload = Some(modify);
                }
                DecisionParse::Decision(terminal) => {
                    // Deny or RequireApproval: short-circuit, exactly as
                    // the bus chain stops at the first such decision.
                    run.evaluation = Ok(terminal);
                    run.output = outputs.join("\n");
                    return run;
                }
            }
        }
    }
    run.output = outputs.join("\n");
    run
}

/// Operator-level posture for one call — the top of the precedence ladder.
///
/// The engine and the registry both pass [`OperatorPosture::NoOpinion`]
/// today: operator tool switches (`tools.<name>: "off"`) act upstream by
/// withholding the tool from the surface entirely (`stella-tools::policy`),
/// so a denied tool never reaches these gates. The variant exists because
/// the precedence contract must hold for producers that DO carry an
/// operator posture — #2684's hook bridge and #2716's `AuthzGate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatorPosture {
    /// The operator has not spoken; the gate evaluation decides.
    NoOpinion,
    /// The operator forbids the call. Nothing downstream can override it.
    Deny { reason: String },
}

/// The folded verdict of [`resolve_precedence`] — what the dispatch does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateVerdict {
    /// Proceed.
    Allow,
    /// Park and ask a human; the answer decides.
    RequireApproval { reason: String },
    /// Refuse without asking anyone.
    Deny { reason: String },
}

/// Fold an operator posture and a gate/hook evaluation into one verdict.
///
/// The precedence is fixed: **operator deny > gate/hook `RequireApproval` >
/// any allow**. A hook or plugin `Allow` (or `Modify`) can never override
/// an operator deny, and an approval requirement always beats an allow.
///
/// An errored evaluation (`Err`) is an unconditional deny — fail closed.
/// `_enforcement_softened` is deliberately accepted and ignored: the
/// parameter exists so a caller carrying a softening switch must hand it
/// over, and the signature is the proof that no value of it can soften an
/// evaluation failure (the OXA-2056 shape, spec item 5 of #2676).
///
/// Pure and synchronous over its inputs by design — fed by the bus
/// blocking chains (`stella-tools::registry::approval`, which re-exports
/// it), by the shell-hook surface above, and eventually by #2716's
/// `AuthzGate`. The property tests in the approval module pin the ladder
/// independently of any producer.
pub fn resolve_precedence(
    operator: &OperatorPosture,
    evaluation: Result<&HookDecision, &str>,
    _enforcement_softened: bool,
) -> GateVerdict {
    if let OperatorPosture::Deny { reason } = operator {
        return GateVerdict::Deny {
            reason: format!("denied by operator policy: {reason}"),
        };
    }
    match evaluation {
        Err(error) => GateVerdict::Deny {
            reason: format!("policy evaluation failed ({error}); failing closed"),
        },
        Ok(HookDecision::Deny { reason }) => GateVerdict::Deny {
            reason: reason.clone(),
        },
        Ok(HookDecision::RequireApproval { reason }) => GateVerdict::RequireApproval {
            reason: reason.clone(),
        },
        Ok(HookDecision::Allow | HookDecision::Modify { .. }) => GateVerdict::Allow,
    }
}

/// One parked approval question, as the route sees it: the parked tool
/// call plus the metadata the asking surface renders. Crosses the crate
/// boundary (`stella-tools::hook_bridge` implements the route), so it is
/// serde round-trippable by contract (invariant #4).
///
/// Two producers build one of these today: a shell hook's
/// `require_approval` decision (`driver::user_hooks`) and an
/// [`crate::ports::AuthzGate`]'s `RequireApproval` (#3288, resolved by
/// `stella-tools`' gated tool set). Named for the route, not for either
/// producer — the type was `ApprovalRouteRequest` until the gate became
/// its second feeder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRouteRequest {
    /// The tool whose dispatch is parked.
    pub tool: String,
    /// The tool's advertised `read_only` bit.
    pub read_only: bool,
    /// The hook's reason, verbatim from its `require_approval` decision.
    pub reason: String,
}

/// How a routed approval resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ApprovalRouteResolution {
    /// A human granted this single call.
    Approved,
    /// The call must not run — a human refused, the flow timed out, or no
    /// interactive surface exists to ask.
    Denied { reason: String },
}

/// How a parked approval question reaches the #2676 approval flow — the
/// port `driver::user_hooks` parks a shell hook's `require_approval` on,
/// and the one an [`crate::ports::AuthzGate`]'s `RequireApproval` resolves
/// through (#3288) — so `stella-core` never touches the broker (which
/// lives in `stella-tools`, above it in the dependency graph). The
/// production implementation
/// (`stella-tools::hook_bridge::BrokerApprovalRoute`) calls
/// `ApprovalBroker::resolve`: emit-before-park, bounded TTL wait,
/// `approval.granted`/`denied`/`expired` audit events. An engine with no
/// route attached refuses the call with a grant-path message instead of
/// asking — the same headless posture the broker itself takes. Whether a
/// human exists to ask is the responder's derivation
/// (`stella-tty::human_can_answer`, read once at assembly time by the
/// surface that attaches a responder), never re-derived per producer.
#[async_trait]
pub trait ApprovalRoute: Send + Sync {
    /// Resolve one parked question. Implementations must bound their own
    /// wait (the broker's TTL does); the engine awaits this without a
    /// timeout of its own, exactly as the registry's gates await the
    /// broker.
    async fn resolve(&self, request: &ApprovalRouteRequest) -> ApprovalRouteResolution;
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::super::{HookAction, HookExecResult, HookMatcher};
    use super::*;

    /// A scripted, no-I/O `HookRunner` keyed by command; records every
    /// payload it was handed so tests can assert what later hooks saw.
    struct FakeRunner {
        scripted: HashMap<String, Result<HookExecResult, HookExecError>>,
        payloads: Mutex<Vec<String>>,
    }

    impl FakeRunner {
        fn new(scripted: HashMap<String, Result<HookExecResult, HookExecError>>) -> Self {
            Self {
                scripted,
                payloads: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl HookRunner for FakeRunner {
        async fn run(
            &self,
            action: &HookAction,
            payload_json: &str,
            _cwd: &str,
        ) -> Result<HookExecResult, HookExecError> {
            self.payloads
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(payload_json.to_string());
            self.scripted
                .get(&action.command)
                .cloned()
                .unwrap_or(Ok(HookExecResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                }))
        }
    }

    fn ok(exit_code: i32, stdout: &str, stderr: &str) -> Result<HookExecResult, HookExecError> {
        Ok(HookExecResult {
            exit_code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        })
    }

    fn pre_tool_hooks(commands: &[&str]) -> Hooks {
        Hooks {
            pre_tool_use: Some(vec![HookMatcher {
                matcher: Some("*".to_string()),
                hooks: commands.iter().map(|c| HookAction::new(*c)).collect(),
            }]),
            ..Hooks::default()
        }
    }

    fn payload() -> HookPayload {
        HookPayload::pre_tool_use("/proj", "bash", serde_json::json!({"command": "ls"}), false)
    }

    // ---- parse_decision ------------------------------------------------

    #[test]
    fn prose_and_actionless_json_are_no_decision() {
        assert_eq!(parse_decision(""), DecisionParse::NoDecision);
        assert_eq!(parse_decision("checked 3 files"), DecisionParse::NoDecision);
        // A `cat` hook echoes the payload — a JSON object with no `action`.
        assert_eq!(
            parse_decision(r#"{"event":"PreToolUse","cwd":"/p"}"#),
            DecisionParse::NoDecision
        );
    }

    #[test]
    fn each_action_spelling_parses_to_the_bus_enum() {
        assert_eq!(
            parse_decision(r#"{"action":"allow"}"#),
            DecisionParse::Decision(HookDecision::Allow)
        );
        assert_eq!(
            parse_decision(r#"{"action":"deny","reason":"nope"}"#),
            DecisionParse::Decision(HookDecision::Deny {
                reason: "nope".into()
            })
        );
        assert_eq!(
            parse_decision(r#"{"action":"require_approval","reason":"ask"}"#),
            DecisionParse::Decision(HookDecision::RequireApproval {
                reason: "ask".into()
            })
        );
        assert_eq!(
            parse_decision(r#"{"action":"modify","payload":{"input":{}}}"#),
            DecisionParse::Decision(HookDecision::Modify {
                payload: serde_json::json!({"input": {}})
            })
        );
    }

    #[test]
    fn a_log_line_before_the_decision_still_parses() {
        let parsed = parse_decision("checking policy…\n{\"action\":\"deny\",\"reason\":\"no\"}");
        assert_eq!(
            parsed,
            DecisionParse::Decision(HookDecision::Deny {
                reason: "no".into()
            })
        );
    }

    #[test]
    fn an_action_key_that_does_not_deserialize_is_malformed_not_ignored() {
        match parse_decision(r#"{"action":"denny","reason":"typo"}"#) {
            DecisionParse::Malformed { .. } => {}
            other => panic!("a broken decision must not silently allow: {other:?}"),
        }
        // `deny` without its required reason is also a protocol error.
        match parse_decision(r#"{"action":"deny"}"#) {
            DecisionParse::Malformed { .. } => {}
            other => panic!("{other:?}"),
        }
    }

    // ---- run_decision_hooks: the fold ---------------------------------

    #[tokio::test]
    async fn a_modify_decision_rewrites_the_input_and_the_chain_ends_allow() {
        let mut scripted = HashMap::new();
        scripted.insert(
            "rewrite".to_string(),
            ok(
                0,
                r#"{"action":"modify","payload":{"input":{"command":"ls -la"}}}"#,
                "",
            ),
        );
        let runner = FakeRunner::new(scripted);
        let run =
            run_decision_hooks(&runner, Some(&pre_tool_hooks(&["rewrite"])), &payload()).await;
        assert_eq!(run.evaluation, Ok(HookDecision::Allow));
        assert_eq!(
            run.rewritten_input,
            Some(serde_json::json!({"command": "ls -la"}))
        );
    }

    #[tokio::test]
    async fn later_hooks_see_the_rewritten_payload_on_stdin() {
        let mut scripted = HashMap::new();
        scripted.insert(
            "rewrite".to_string(),
            ok(
                0,
                r#"{"action":"modify","payload":{"input":{"command":"ls -la"}}}"#,
                "",
            ),
        );
        scripted.insert("observe".to_string(), ok(0, "", ""));
        let runner = FakeRunner::new(scripted);
        run_decision_hooks(
            &runner,
            Some(&pre_tool_hooks(&["rewrite", "observe"])),
            &payload(),
        )
        .await;
        let payloads = runner.payloads.lock().unwrap().clone();
        assert_eq!(payloads.len(), 2);
        assert!(
            payloads[1].contains("ls -la"),
            "the second hook must see the first hook's rewrite: {}",
            payloads[1]
        );
    }

    #[tokio::test]
    async fn deny_short_circuits_the_chain() {
        let mut scripted = HashMap::new();
        scripted.insert(
            "deny".to_string(),
            ok(0, r#"{"action":"deny","reason":"blocked path"}"#, ""),
        );
        scripted.insert("never".to_string(), ok(0, r#"{"action":"allow"}"#, ""));
        let runner = FakeRunner::new(scripted);
        let run = run_decision_hooks(
            &runner,
            Some(&pre_tool_hooks(&["deny", "never"])),
            &payload(),
        )
        .await;
        assert_eq!(
            run.evaluation,
            Ok(HookDecision::Deny {
                reason: "blocked path".into()
            })
        );
        assert_eq!(
            runner.payloads.lock().unwrap().len(),
            1,
            "nothing after a terminal decision runs"
        );
    }

    #[tokio::test]
    async fn a_nonzero_exit_is_an_evaluation_failure_carrying_the_stderr() {
        let mut scripted = HashMap::new();
        // Stdout carries a VALID allow — the non-zero exit must still fail
        // the evaluation (OXA-2056: the exit code outranks the words).
        scripted.insert(
            "broken".to_string(),
            ok(2, r#"{"action":"allow"}"#, "no shell here"),
        );
        let runner = FakeRunner::new(scripted);
        let run = run_decision_hooks(&runner, Some(&pre_tool_hooks(&["broken"])), &payload()).await;
        match run.evaluation {
            Err(HookEvaluationFailure::HookFailed {
                exit_code, detail, ..
            }) => {
                assert_eq!(exit_code, 2);
                assert!(detail.contains("no shell here"));
            }
            other => panic!("a non-zero exit is an evaluation failure: {other:?}"),
        }
    }

    #[tokio::test]
    async fn plain_stdout_stays_informational_and_the_chain_allows() {
        let mut scripted = HashMap::new();
        scripted.insert("log".to_string(), ok(0, "checked 3 files", ""));
        let runner = FakeRunner::new(scripted);
        let run = run_decision_hooks(&runner, Some(&pre_tool_hooks(&["log"])), &payload()).await;
        assert_eq!(run.evaluation, Ok(HookDecision::Allow));
        assert!(run.output.contains("checked 3 files"));
        assert_eq!(run.rewritten_input, None);
    }

    #[tokio::test]
    async fn a_modify_without_an_input_field_keeps_the_original_and_leaves_a_trace() {
        let mut scripted = HashMap::new();
        scripted.insert(
            "sloppy".to_string(),
            ok(0, r#"{"action":"modify","payload":{"typo":1}}"#, ""),
        );
        let runner = FakeRunner::new(scripted);
        let run = run_decision_hooks(&runner, Some(&pre_tool_hooks(&["sloppy"])), &payload()).await;
        assert_eq!(run.evaluation, Ok(HookDecision::Allow));
        assert_eq!(run.rewritten_input, None, "original input kept");
        assert_eq!(run.diagnostics.len(), 1);
        assert!(run.diagnostics[0].contains("without an `input` field"));
    }

    // ---- witness (c): OXA-2056 through the ladder ----------------------

    /// **The OXA-2056 witness for the shell surface**: a hook that exited
    /// non-zero denies through [`resolve_precedence`] for EVERY value of
    /// the enforcement-softening flag.
    #[tokio::test]
    async fn a_failed_hook_evaluation_denies_even_with_the_softening_flag_set() {
        let mut scripted = HashMap::new();
        scripted.insert("exit 3".to_string(), ok(3, "", "policy engine down"));
        let runner = FakeRunner::new(scripted);
        let run = run_decision_hooks(&runner, Some(&pre_tool_hooks(&["exit 3"])), &payload()).await;
        for softened in [false, true] {
            match run.verdict(&OperatorPosture::NoOpinion, softened) {
                GateVerdict::Deny { reason } => {
                    assert!(reason.contains("failing closed"), "{reason}");
                    assert!(
                        reason.contains("policy engine down"),
                        "the hook's own stderr reaches the deny reason: {reason}"
                    );
                }
                other => panic!(
                    "softened={softened}: an errored evaluation must deny unconditionally, \
                     got {other:?}"
                ),
            }
        }
    }

    // ---- witness (f): operator deny beats a shell hook's allow ---------

    #[tokio::test]
    async fn an_operator_deny_beats_a_shell_hooks_allow() {
        let mut scripted = HashMap::new();
        scripted.insert("permissive".to_string(), ok(0, r#"{"action":"allow"}"#, ""));
        let runner = FakeRunner::new(scripted);
        let run =
            run_decision_hooks(&runner, Some(&pre_tool_hooks(&["permissive"])), &payload()).await;
        assert_eq!(run.evaluation, Ok(HookDecision::Allow));
        let verdict = run.verdict(
            &OperatorPosture::Deny {
                reason: "org policy".into(),
            },
            false,
        );
        assert!(
            matches!(verdict, GateVerdict::Deny { .. }),
            "a hook Allow can never override an operator deny: {verdict:?}"
        );
    }

    // ---- serde round-trips (invariant #4) ------------------------------

    #[test]
    fn approval_request_and_resolution_round_trip_through_serde_json() {
        let request = ApprovalRouteRequest {
            tool: "bash".into(),
            read_only: false,
            reason: "hook wants a human".into(),
        };
        let json = serde_json::to_string(&request).unwrap();
        let back: ApprovalRouteRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(request, back);

        for resolution in [
            ApprovalRouteResolution::Approved,
            ApprovalRouteResolution::Denied {
                reason: "ask my lead".into(),
            },
        ] {
            let json = serde_json::to_string(&resolution).unwrap();
            let back: ApprovalRouteResolution = serde_json::from_str(&json).unwrap();
            assert_eq!(resolution, back);
        }
    }
}
