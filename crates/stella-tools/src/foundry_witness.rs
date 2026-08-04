//! The capability witness — a tool-foundry tool's proof that it is a *new*
//! capability, and not a name wrapped around one the agent already had.
//!
//! This is issue #830's witness slice, and it is the pipeline's witness
//! protocol pointed at a different subject. There, a witness is a test that
//! **fails on the current tree and passes on the changed one**, and that
//! fail→pass flip is what makes "done" a measurement rather than a claim
//! (`stella_pipeline::witness`). Here the change under proof is not a diff —
//! it is *the tool existing*. So the two trees become two worlds:
//!
//! - **without the tool**: the session's real executor chain, exactly as it is
//!   today. The witness call must FAIL here.
//! - **with the tool**: that same chain with one thing added — the candidate
//!   ([`crate::custom::CustomToolSet`]). The witness call must SUCCEED here,
//!   and produce the value it recorded.
//!
//! Same executor, same input, one difference. If the call answers in both
//! worlds, the tool is not a new capability and the foundry has proposed a
//! synonym; if it answers in neither, it does not work. Only the flip earns
//! adoption.
//!
//! # Fails now, and could fail meaningfully
//!
//! `stella_pipeline::witness::density` makes the case that failing first is
//! necessary and nowhere near sufficient: a test that fails only because a
//! symbol does not exist yet is "a compile check wearing a test's name", and
//! it flips green without constraining anything. The lazy version of this
//! module has exactly that shape — "the tool is not registered, therefore the
//! call errors" is true of *every* name that has never been registered,
//! including nonsense ones. It would credit a witness for a tool that runs and
//! prints nothing.
//!
//! So the flip is only half of a verdict here. The other half is an assertion
//! on a value the tool actually produced ([`WitnessCase::expect`]), and the
//! same three vacuous shapes are screened before any of it counts:
//!
//! - **Nothing to assert on.** The tool succeeded and printed nothing, so
//!   there is no observable difference between it and a no-op.
//! - **An assertion over a constant.** The tool's whole output is one of its
//!   own inputs echoed back. That holds for any implementation that can spell
//!   its argument, so its flip proves plumbing, not capability. (Deliberately
//!   narrow — *equality* with an input, not containment. A `grep`-shaped tool
//!   legitimately prints lines containing its pattern.)
//! - **An answer that does not reproduce.** The expectation is recorded from
//!   one run and re-checked against a second. A tool whose output never
//!   repeats can be adopted but never re-verified, so its witness is a
//!   one-time observation rather than a standing proof.
//!
//! # The witness input comes from the artifact, not from the session
//!
//! [`crate::foundry_gate::FoundryProvenance::witness_input`] is stamped into
//! the manifest at authoring time, taken from the receipts the tool was mined
//! from. Adoption can therefore happen long after those receipts have rolled
//! out of the detector's window — the artifact carries its own evidence. A
//! proof that needs the original evidence to still be in scope is a proof with
//! an expiry date, and this one does not have one.
//!
//! # This runs the candidate
//!
//! Proving a witness executes the tool, twice, in the real workspace. That is
//! a side effect and it is not hidden: `prove` is only ever reached from an
//! explicit `stella tools --adopt <name>`, a human-initiated command about one
//! named tool, and the run happens while the tool is still withheld by
//! [`crate::foundry_gate`]. #830's guardrail is that nothing self-authored
//! executes with side effects until it has a green witness *and* one human
//! approval; the approval to run the proof is the adopt command itself, and
//! the approval to let the model reach for it afterwards is a separate
//! `--enable`.

use std::path::Path;

use serde_json::Value;
use stella_core::ports::ToolExecutor;
use stella_protocol::tool::ToolOutput;

use crate::custom::{CustomTool, CustomToolSet};

/// Longest recorded expectation. A witness asserts on a *value*, not on a
/// transcript: capping keeps the ledger row small and stops a chatty tool
/// from pinning its entire first-run output as a re-verification target.
const MAX_EXPECT_CHARS: usize = 200;

/// The experiment: one input, and the value the tool must produce from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitnessCase {
    /// The tool input, one observed value per required schema property.
    pub input: Value,
    /// A substring the tool's output must contain — recorded from the first
    /// run and re-checked on the second.
    pub expect: String,
}

/// Why a candidate witness could not prove anything, whatever the flip did.
/// Each variant names a shape, because the name is what a reader needs in
/// order to decide whether to fix the tool or drop it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VacuousWitness {
    /// The manifest carries no usable witness input, so no call can be made.
    #[error(
        "the manifest carries no runnable witness input ({why}): without one there is nothing to \
         run in either world, and a tool that cannot be exercised cannot be proven"
    )]
    NoWitnessInput {
        /// What was missing or malformed.
        why: String,
    },
    /// The tool succeeded and produced nothing.
    #[error(
        "the tool succeeded but produced no output: there is nothing to assert on, so its flip \
         would prove only that a process exited 0"
    )]
    EmptyOutput,
    /// The tool's entire output is one of its own inputs, echoed.
    #[error(
        "the tool's whole output is its own input echoed back (`{value}`): that holds for any \
         implementation that can spell its argument, so the flip proves plumbing, not capability"
    )]
    EchoesItsInput {
        /// The echoed value.
        value: String,
    },
}

/// The outcome of running a capability witness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WitnessVerdict {
    /// The flip happened and the assertion held twice. This is the only
    /// verdict that earns adoption.
    Proven(WitnessCase),
    /// The call answered *without* the tool, so the tool is not a new
    /// capability. Carries what the existing surface said.
    NoFlip {
        /// How the capability was already reachable.
        how: String,
    },
    /// The call failed *with* the tool too.
    Refuted {
        /// The failure the tool returned.
        why: String,
    },
    /// The tool answered twice and disagreed with itself.
    Unstable {
        /// The value recorded from the first run.
        expected: String,
        /// What the second run said instead.
        second: String,
    },
    /// The witness could not have proven anything even had it flipped.
    Vacuous(VacuousWitness),
}

impl WitnessVerdict {
    /// Whether this verdict earns adoption.
    pub fn is_proven(&self) -> bool {
        matches!(self, Self::Proven(_))
    }

    /// A one-line rendering for the adoption ledger and the CLI report.
    pub fn summary(&self) -> String {
        match self {
            Self::Proven(case) => format!("proven — output contains `{}`", case.expect),
            Self::NoFlip { how } => format!("no flip — {how}"),
            Self::Refuted { why } => format!("refuted — {why}"),
            Self::Unstable { expected, second } => {
                format!("unstable — first run said `{expected}`, second said `{second}`")
            }
            Self::Vacuous(reason) => format!("vacuous — {reason}"),
        }
    }
}

/// Build the witness case's input from a tool's manifest, checking it can
/// actually drive the tool: an object, and a value for every required schema
/// property. Pure, so the "is this artifact provable at all" question is
/// answerable without running anything.
pub fn witness_input(tool: &CustomTool) -> Result<Value, VacuousWitness> {
    let provenance = tool
        .foundry
        .as_ref()
        .ok_or(VacuousWitness::NoWitnessInput {
            why: "the manifest has no [foundry] table".to_string(),
        })?;
    let input = &provenance.witness_input;
    let Some(object) = input.as_object() else {
        return Err(VacuousWitness::NoWitnessInput {
            why: "`witness_input` is not a table".to_string(),
        });
    };
    if object.is_empty() {
        return Err(VacuousWitness::NoWitnessInput {
            why: "`witness_input` is empty".to_string(),
        });
    }
    // Every hole the script expands must have a value, or `set -u` fails the
    // run for a reason that has nothing to do with the capability.
    if let Some(required) = tool.input_schema.get("required").and_then(Value::as_array) {
        let missing: Vec<&str> = required
            .iter()
            .filter_map(Value::as_str)
            .filter(|name| !object.contains_key(*name))
            .collect();
        if !missing.is_empty() {
            return Err(VacuousWitness::NoWitnessInput {
                why: format!(
                    "`witness_input` has no value for required {}",
                    missing.join(", ")
                ),
            });
        }
    }
    Ok(input.clone())
}

/// Run the capability witness for one candidate tool.
///
/// `inner` is the session's real executor chain *without* the candidate — the
/// world the flip is measured against. `root` is the workspace root the
/// candidate's subprocess is spawned with, matching how a registered custom
/// tool runs.
pub async fn prove(inner: &dyn ToolExecutor, tool: &CustomTool, root: &Path) -> WitnessVerdict {
    let input = match witness_input(tool) {
        Ok(input) => input,
        Err(reason) => return WitnessVerdict::Vacuous(reason),
    };

    // ── World A: without the tool ────────────────────────────────────────
    // Advertisement first. An executor that already offers this name means
    // the capability exists whatever `execute` goes on to return, and a
    // custom tool taking the name would shadow it rather than add anything.
    if inner
        .schemas()
        .iter()
        .any(|schema| schema.name == tool.name)
    {
        return WitnessVerdict::NoFlip {
            how: format!("`{}` is already an advertised tool", tool.name),
        };
    }
    // Then the call itself. Because the name is unadvertised, an error here
    // is an *absence* error by construction — the flip fails for the right
    // reason without this module having to pattern-match an error string.
    if let ToolOutput::Ok { content } = inner.execute(&tool.name, &input).await {
        return WitnessVerdict::NoFlip {
            how: format!(
                "the existing tool surface already answers `{}`: {}",
                tool.name,
                one_line(&content)
            ),
        };
    }

    // ── World B: with the tool ───────────────────────────────────────────
    let candidate = CustomToolSet::new(inner, vec![tool.clone()], root.to_path_buf());
    let first = match candidate.execute(&tool.name, &input).await {
        ToolOutput::Ok { content } => content,
        ToolOutput::Error { message } => {
            return WitnessVerdict::Refuted {
                why: one_line(&message),
            };
        }
    };
    let expect = match assertable_value(&first, &input) {
        Ok(expect) => expect,
        Err(reason) => return WitnessVerdict::Vacuous(reason),
    };

    // The recorded expectation has to be a standing claim, not a one-time
    // observation, or nothing can ever re-check it.
    let second = match candidate.execute(&tool.name, &input).await {
        ToolOutput::Ok { content } => content,
        ToolOutput::Error { message } => {
            return WitnessVerdict::Unstable {
                expected: expect,
                second: format!("error: {}", one_line(&message)),
            };
        }
    };
    if !second.contains(&expect) {
        return WitnessVerdict::Unstable {
            expected: expect,
            second: one_line(&second),
        };
    }

    WitnessVerdict::Proven(WitnessCase { input, expect })
}

/// The value a witness asserts on: the first non-empty line of the tool's
/// output, capped. Screens the two vacuous shapes an output can have.
fn assertable_value(output: &str, input: &Value) -> Result<String, VacuousWitness> {
    let Some(line) = output.lines().map(str::trim).find(|l| !l.is_empty()) else {
        return Err(VacuousWitness::EmptyOutput);
    };
    let expect: String = line.chars().take(MAX_EXPECT_CHARS).collect();
    // Equality, not containment: a tool that prints exactly what it was given
    // demonstrates argument passing. One that prints lines *containing* the
    // argument (grep, jq over a matched key) is doing real work.
    if let Some(object) = input.as_object() {
        for value in object.values() {
            let rendered = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if !rendered.is_empty() && rendered.trim() == expect {
                return Err(VacuousWitness::EchoesItsInput { value: expect });
            }
        }
    }
    Ok(expect)
}

/// Collapse text to one bounded line for a verdict message.
fn one_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    line.chars().take(MAX_EXPECT_CHARS).collect()
}

#[cfg(test)]
mod tests;
