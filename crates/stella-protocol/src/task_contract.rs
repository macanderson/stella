// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A task's definition of done — the wire half of "tasks close on checks, not
//! on model self-report" (`design/tui-v2/SPEC.md` §7.1).
//!
//! # The claim this type exists to make true
//!
//! A board task used to close because something said so. [`crate::TaskStatus`]
//! is a field, `task_complete` sets it, and the caller with the strongest
//! incentive to set it is the model whose work is being judged. Nothing in the
//! type system distinguished "this task passed its checks" from "the model said
//! it did" — they were the same bytes.
//!
//! So the closing condition is not stored here. [`TaskContract::closure`]
//! *derives* it from the checks, every time it is asked, and there is no field
//! anywhere in this module that a caller can set to mean "done". A task with an
//! unsatisfied contract has no representation that says otherwise.
//!
//! # Two shapes, and the rule is the type
//!
//! SPEC 7.1: *contracts are required only for tasks that produce diffs;
//! read-only tasks close on completion of their events.* Both halves are
//! structural rather than validated:
//!
//! - [`TaskContract::ReadOnly`] has no field a check could go in, so a
//!   read-only task cannot carry one. That matters more than it looks — the
//!   spec's reason for the rule is that a required contract on a task with
//!   nothing to prove produces *fake checks written to satisfy the UI*, and a
//!   variant with nowhere to put them is the only version of that rule which
//!   cannot be worked around.
//! - [`TaskContract::DefinitionOfDone`] holds a [`DefinitionOfDone`], which is
//!   non-empty by construction. There is no way to build one with zero checks,
//!   in Rust or over the wire — its `Deserialize` rejects an empty list rather
//!   than accepting a contract that promises nothing.
//!
//! A validation function would have been the smaller change and the weaker one:
//! it holds only where someone remembers to call it, and the call site that
//! forgets is the one that ships.
//!
//! # Deterministic where it can be, and it says which
//!
//! Every check names the [`Judge`] that decides it. That is not decoration: it
//! is the split SPEC 1's first thesis is about.
//! A check a machine settles costs `$0.00`; one a model settles costs a call,
//! and the contract says out loud which it bought.
//!
//! Per check, and deliberately not summed into a ratio. A `det %` over a
//! turn's work was specified once and removed: it has no source, and a
//! number nothing measures is worse on a receipt than an absent one. What
//! survives is the fact each check already carries — a reader can see that a
//! contract is all `review` without being told a percentage nobody computed.
//!
//! [`CheckMechanism`] is an **open** vocabulary over a closed [`CheckKind`],
//! the shape [`crate::StageName`] already uses and for the same reason: a
//! verification plugin contributes mechanisms this host has never heard of, and
//! a closed enum would either reject them or silently retype them as something
//! they are not. The closed half survives because the question that must stay
//! answerable — *did a machine decide this, or did a model?* — is answerable
//! for a contributed mechanism too: whoever contributes it declares its judge.

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

/// Who settles a check.
///
/// The axis SPEC 1's first thesis prices: deterministic work never reaches a
/// model and costs `$0.00`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Judge {
    /// A machine decided: an exit status, a count, a graph query. No model call,
    /// and the same inputs give the same answer on any run.
    Deterministic,
    /// A model decided, because the property is not reducible to a check.
    ///
    /// SPEC 7.1 permits this "only when irreducible", which is a review
    /// judgement rather than something a type can enforce. What the type does
    /// is make the choice **visible**: a contract full of model-judged checks
    /// is a contract that proves very little, and it says so on its face.
    Model,
}

impl Judge {
    /// Whether this judge costs a model call.
    #[must_use]
    pub const fn is_deterministic(self) -> bool {
        matches!(self, Self::Deterministic)
    }
}

/// A mechanism this host knows how to run.
///
/// Closed, and deliberately small. It is not the mechanism vocabulary — it is
/// the set *this host* can carry out itself, which is a smaller and honest
/// claim (the argument [`crate::StageKind`] makes about stages).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    /// A code-graph query: "no inbound references to the deleted symbol".
    /// Deterministic — the index answers it.
    Graph,
    /// The workspace's own test runner, scoped to what the task touched.
    Unit,
    /// An external command's exit status: a linter, a build, a bench.
    Harness,
    /// A model reads the diff and judges a property no command can express.
    /// The one known kind whose judge is [`Judge::Model`].
    Review,
}

impl CheckKind {
    /// The wire spelling.
    #[must_use]
    pub const fn as_wire_str(self) -> &'static str {
        match self {
            Self::Graph => "graph",
            Self::Unit => "unit",
            Self::Harness => "harness",
            Self::Review => "review",
        }
    }

    /// Resolve a wire spelling, or `None` if this host does not know it.
    #[must_use]
    pub fn from_wire_str(name: &str) -> Option<Self> {
        match name {
            "graph" => Some(Self::Graph),
            "unit" => Some(Self::Unit),
            "harness" => Some(Self::Harness),
            "review" => Some(Self::Review),
            _ => None,
        }
    }

    /// Who settles this kind.
    ///
    /// Fixed per kind rather than declared per check, because it is a property
    /// of the mechanism and not of the author: a `unit` check that claimed to
    /// be model-judged, or a `review` that claimed to be deterministic, would
    /// be describing something other than what it runs.
    #[must_use]
    pub const fn judge(self) -> Judge {
        match self {
            Self::Graph | Self::Unit | Self::Harness => Judge::Deterministic,
            Self::Review => Judge::Model,
        }
    }
}

/// How a check is carried out: one of this host's, or a contributed mechanism.
///
/// Encodes as a plain string either way, so every known kind writes exactly the
/// byte it always did. See the module docs for why the vocabulary is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckMechanism {
    /// A mechanism this host runs itself.
    Known(CheckKind),
    /// A mechanism something outside the host contributed, under its own word,
    /// with the judge that contributor declared.
    ///
    /// Never a name [`CheckKind::from_wire_str`] resolves — [`CheckMechanism::new`]
    /// is the only constructor that can produce this arm, and it normalizes, so
    /// `Contributed("unit")` cannot exist to decode as one thing and re-encode
    /// as another (invariant 4).
    Contributed { name: String, judge: Judge },
}

impl CheckMechanism {
    /// Resolve a mechanism name, preferring this host's own vocabulary.
    ///
    /// `judge` is consulted only when the name is not one of the host's — a
    /// known kind's judge is a property of what it runs, not a claim a caller
    /// may override.
    #[must_use]
    pub fn new(name: &str, judge: Judge) -> Self {
        CheckKind::from_wire_str(name).map_or_else(
            || Self::Contributed {
                name: name.to_owned(),
                judge,
            },
            Self::Known,
        )
    }

    /// The kind this is, or `None` for a contributed mechanism.
    #[must_use]
    pub const fn kind(&self) -> Option<CheckKind> {
        match self {
            Self::Known(kind) => Some(*kind),
            Self::Contributed { .. } => None,
        }
    }

    /// The name as it appears on the wire and on screen.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Known(kind) => kind.as_wire_str(),
            Self::Contributed { name, .. } => name.as_str(),
        }
    }

    /// Who settles a check run this way.
    #[must_use]
    pub const fn judge(&self) -> Judge {
        match self {
            Self::Known(kind) => kind.judge(),
            Self::Contributed { judge, .. } => *judge,
        }
    }
}

/// The wire form of a mechanism: its name, plus the judge a contributed one
/// declares.
///
/// A known kind writes no judge — it has one by definition, and a second copy
/// on the wire is a second thing to disagree with.
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
struct MechanismWire {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    judge: Option<Judge>,
}

impl Serialize for CheckMechanism {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        MechanismWire {
            name: self.as_str().to_owned(),
            judge: match self {
                Self::Known(_) => None,
                Self::Contributed { judge, .. } => Some(*judge),
            },
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for CheckMechanism {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let wire = MechanismWire::deserialize(d)?;
        // A contributed mechanism with no declared judge would leave the
        // det/model split unanswerable for it, and the split is the whole
        // reason the field exists. Refuse rather than guess: guessing
        // `Deterministic` would price a model call at $0.00, and guessing
        // `Model` would make a plugin's own graph query look like one.
        match CheckKind::from_wire_str(&wire.name) {
            Some(kind) => Ok(Self::Known(kind)),
            None => {
                let judge = wire.judge.ok_or_else(|| {
                    de::Error::custom(format!(
                        "contributed check mechanism {:?} declares no judge; \
                         a mechanism this host cannot run must say whether a \
                         machine or a model settles it",
                        wire.name
                    ))
                })?;
                Ok(Self::Contributed {
                    name: wire.name,
                    judge,
                })
            }
        }
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for CheckMechanism {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "CheckMechanism".into()
    }

    /// An object with a free-form `name`, **not** an enum of the four.
    ///
    /// The schema is the contract a non-Rust reader validates against, and a
    /// closed enum there would make a plugin's own mechanism a schema
    /// violation — re-closing on the wire exactly the vocabulary this type
    /// opened. The four the host runs ride along as `examples`, which
    /// documents without constraining.
    ///
    /// `judge` is optional here rather than required, because a known name
    /// carries no judge on the wire: the mechanism defines it. A reader that
    /// sees a name it does not recognise and no `judge` is looking at a
    /// malformed message, and `Deserialize` rejects it for the same reason —
    /// the det/model split has to stay answerable for every check.
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // `subschema_for` both registers `Judge` in `$defs` and returns the
        // reference to it. A hand-written `$ref` would compile and then dangle,
        // because nothing would have put the definition there.
        let judge = generator.subschema_for::<Judge>();
        let mut schema = schemars::json_schema!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "judge": judge
            },
            "required": ["name"]
        });
        schema.insert(
            "description".to_owned(),
            serde_json::Value::String(
                "How a check is settled — an OPEN vocabulary. The mechanisms this host runs \
                 itself are listed in `examples` and imply their own judge; any other name is a \
                 contributed mechanism and MUST carry `judge`, so a consumer can always tell \
                 whether a machine or a model decided."
                    .to_owned(),
            ),
        );
        schema.insert(
            "examples".to_owned(),
            serde_json::Value::Array(
                [
                    CheckKind::Graph,
                    CheckKind::Unit,
                    CheckKind::Harness,
                    CheckKind::Review,
                ]
                .iter()
                .map(|k| serde_json::json!({ "name": k.as_wire_str() }))
                .collect(),
            ),
        );
        schema
    }
}

/// Where a check stands.
///
/// [`CheckOutcome::Passed`] and [`CheckOutcome::Failed`] both carry evidence
/// because an outcome without it is exactly the self-report this module exists
/// to replace — "it passed" is a claim, and "42 tests, 0 failures" is the thing
/// that makes it checkable by someone else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum CheckOutcome {
    /// Not run yet.
    Pending,
    /// Ran and passed. `evidence` is what the judge saw.
    Passed { evidence: String },
    /// Ran and did not pass. `evidence` is what the judge saw.
    Failed { evidence: String },
}

impl CheckOutcome {
    /// Whether this outcome closes its clause.
    #[must_use]
    pub const fn passed(&self) -> bool {
        matches!(self, Self::Passed { .. })
    }
}

/// One clause of a task's definition of done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Check {
    /// What must be true, in the author's words: "no inbound refs to the
    /// removed symbol", "the auth suite is green".
    pub statement: String,
    /// How it is settled.
    pub mechanism: CheckMechanism,
    /// Where it stands. Defaults to [`CheckOutcome::Pending`] so a plan can be
    /// written before anything has run.
    #[serde(default = "pending")]
    pub outcome: CheckOutcome,
}

fn pending() -> CheckOutcome {
    CheckOutcome::Pending
}

impl Check {
    /// A check that has not run yet.
    #[must_use]
    pub fn new(statement: impl Into<String>, mechanism: CheckMechanism) -> Self {
        Self {
            statement: statement.into(),
            mechanism,
            outcome: CheckOutcome::Pending,
        }
    }

    /// Whether this clause is settled in the affirmative.
    #[must_use]
    pub const fn passed(&self) -> bool {
        self.outcome.passed()
    }
}

/// A task's definition of done: **at least one** check, always.
///
/// The non-emptiness is the point, and it is why this is a struct with a
/// private head rather than a `Vec`. SPEC 7.1 requires a contract on every
/// diff-producing task; a `Vec` that happens to be empty is that requirement
/// silently unmet, and the only way to catch it is a validator someone has to
/// remember to run.
///
/// On the wire it is a plain JSON array, so the shape is what a reader would
/// expect; the emptiness rule is enforced on the way in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(into = "Vec<Check>")]
pub struct DefinitionOfDone {
    head: Check,
    tail: Vec<Check>,
}

impl From<DefinitionOfDone> for Vec<Check> {
    fn from(dm: DefinitionOfDone) -> Self {
        let mut out = Vec::with_capacity(dm.tail.len() + 1);
        out.push(dm.head);
        out.extend(dm.tail);
        out
    }
}

impl DefinitionOfDone {
    /// A contract from one check and any number of further ones.
    #[must_use]
    pub fn new(head: Check, tail: Vec<Check>) -> Self {
        Self { head, tail }
    }

    /// A contract from a list, or `None` if the list is empty.
    ///
    /// The fallible constructor for callers holding a `Vec` — the one place
    /// "a diff-producing task with no checks" is rejected in Rust.
    #[must_use]
    pub fn from_vec(checks: Vec<Check>) -> Option<Self> {
        let mut it = checks.into_iter();
        let head = it.next()?;
        Some(Self {
            head,
            tail: it.collect(),
        })
    }

    /// Every check, in order.
    pub fn iter(&self) -> impl Iterator<Item = &Check> {
        std::iter::once(&self.head).chain(self.tail.iter())
    }

    /// Every check, mutably — how a runner records an outcome.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Check> {
        std::iter::once(&mut self.head).chain(self.tail.iter_mut())
    }

    /// How many clauses this contract has. Never zero.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tail.len() + 1
    }

    /// Always `false` — stated so callers do not reach for `len() == 0` and
    /// readers do not wonder.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }
}

impl<'de> Deserialize<'de> for DefinitionOfDone {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let checks = Vec::<Check>::deserialize(d)?;
        Self::from_vec(checks).ok_or_else(|| {
            de::Error::custom(
                "a definition of done needs at least one check; a task that \
                 produces diffs and promises nothing cannot be closed by \
                 anything but a self-report",
            )
        })
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for DefinitionOfDone {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "DefinitionOfDone".into()
    }

    /// A non-empty **array** of checks, not the `{head, tail}` struct.
    ///
    /// Hand-written because `serde(into = "Vec<Check>")` moves the wire shape
    /// away from the Rust shape, and a derived schema would describe the
    /// latter — publishing a contract no message this type emits could ever
    /// satisfy. `minItems: 1` is the non-emptiness rule stated where a
    /// non-Rust reader can enforce it, so the guarantee survives the crate
    /// boundary instead of stopping at it.
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = schemars::json_schema!({
            "type": "array",
            "minItems": 1,
            "items": generator.subschema_for::<Check>()
        });
        schema.insert(
            "description".to_owned(),
            serde_json::Value::String(
                "What a diff-producing task means by done: at least one check, always. An empty \
                 array is refused rather than accepted as a contract that promises nothing."
                    .to_owned(),
            ),
        );
        schema
    }
}

/// What a task means by done (SPEC 7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case", tag = "kind", content = "checks")]
pub enum TaskContract {
    /// The task produces no diff. It closes when its own events are done, and
    /// it carries no checks — there is nowhere here to put one.
    ReadOnly,
    /// The task produces a diff. It closes when every check passes.
    DefinitionOfDone(DefinitionOfDone),
}

/// Whether a contract is satisfied, and why not when it is not.
///
/// Three-valued on purpose. A read-only task is not "done because its checks
/// passed" — it has none — and collapsing that into the same `true` a satisfied
/// contract returns is how a surface ends up claiming a read of a file was
/// verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closure {
    /// Every check passed. The task has earned its close.
    Earned,
    /// Checks remain. The task may not close.
    Outstanding {
        /// Clauses that have not run.
        pending: usize,
        /// Clauses that ran and did not pass.
        failed: usize,
    },
    /// There is no contract to satisfy; closure is decided by the task's own
    /// events, not here.
    NotContracted,
}

impl Closure {
    /// Whether a close is permitted right now.
    ///
    /// [`Closure::NotContracted`] permits one because a read-only task has
    /// nothing to prove — the caller still decides *when*, from the task's
    /// events; this only declines to stand in the way.
    #[must_use]
    pub const fn permits_close(self) -> bool {
        matches!(self, Self::Earned | Self::NotContracted)
    }
}

impl TaskContract {
    /// Whether the contract is satisfied — **derived, never stored**.
    ///
    /// There is no setter and no cached field. The only way to make this return
    /// [`Closure::Earned`] is to pass the checks, which is the whole of SPEC 1's
    /// second thesis expressed as a function signature.
    #[must_use]
    pub fn closure(&self) -> Closure {
        let Self::DefinitionOfDone(checks) = self else {
            return Closure::NotContracted;
        };
        let mut pending = 0;
        let mut failed = 0;
        for check in checks.iter() {
            match check.outcome {
                CheckOutcome::Passed { .. } => {}
                CheckOutcome::Pending => pending += 1,
                CheckOutcome::Failed { .. } => failed += 1,
            }
        }
        if pending == 0 && failed == 0 {
            Closure::Earned
        } else {
            Closure::Outstanding { pending, failed }
        }
    }

    /// Every check, or an empty iterator for a read-only task.
    pub fn checks(&self) -> impl Iterator<Item = &Check> {
        // `Option`'s iterator, so read-only yields nothing without a second
        // code path for callers to get wrong.
        match self {
            Self::ReadOnly => None,
            Self::DefinitionOfDone(dm) => Some(dm),
        }
        .into_iter()
        .flat_map(DefinitionOfDone::iter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(statement: &str) -> Check {
        Check::new(statement, CheckMechanism::Known(CheckKind::Unit))
    }

    fn passed(statement: &str) -> Check {
        let mut c = unit(statement);
        c.outcome = CheckOutcome::Passed {
            evidence: "42 tests, 0 failures".into(),
        };
        c
    }

    // ── invariant 4: everything crossing a boundary round-trips ──────────────

    #[test]
    fn a_contract_round_trips_byte_for_byte() {
        let contract = TaskContract::DefinitionOfDone(DefinitionOfDone::new(
            passed("the auth suite is green"),
            vec![
                Check::new(
                    "no inbound refs to the removed symbol",
                    CheckMechanism::Known(CheckKind::Graph),
                ),
                Check::new(
                    "the migration reads as reversible",
                    CheckMechanism::new("vera:reversibility", Judge::Model),
                ),
            ],
        ));
        let json = serde_json::to_string(&contract).expect("serialize");
        let back: TaskContract = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(contract, back);
        assert_eq!(json, serde_json::to_string(&back).expect("re-serialize"));
    }

    #[test]
    fn a_read_only_contract_round_trips() {
        let json = serde_json::to_string(&TaskContract::ReadOnly).expect("serialize");
        let back: TaskContract = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, TaskContract::ReadOnly);
    }

    /// A known mechanism must not decode into the contributed arm, or it would
    /// re-encode carrying a `judge` it did not arrive with — a value that does
    /// not survive its own round trip.
    #[test]
    fn a_known_mechanism_never_decodes_as_contributed() {
        let m = CheckMechanism::new("unit", Judge::Model);
        assert_eq!(m, CheckMechanism::Known(CheckKind::Unit));
        assert_eq!(
            m.judge(),
            Judge::Deterministic,
            "a known kind's judge is a property of what it runs, not a caller's claim"
        );
        let json = serde_json::to_string(&m).expect("serialize");
        assert!(!json.contains("judge"), "{json}");
    }

    // ── SPEC 7.1: the rule is the type ──────────────────────────────────────

    /// The acceptance criterion: a diff-producing task with no checks is
    /// rejected. Not by a validator someone must remember to call — by the only
    /// constructor there is.
    #[test]
    fn a_definition_of_done_contract_cannot_be_empty() {
        assert!(DefinitionOfDone::from_vec(Vec::new()).is_none());

        let err =
            serde_json::from_str::<TaskContract>(r#"{"kind":"definition_of_done","checks":[]}"#)
                .expect_err("an empty contract must not deserialize");
        assert!(
            err.to_string().contains("at least one check"),
            "the refusal should say why: {err}"
        );
    }

    /// The other half of SPEC 7.1, and the reason it is a variant rather than a
    /// flag: a read-only task has nowhere to put a check, so the fake checks
    /// the spec warns about have nowhere to go.
    #[test]
    fn a_read_only_task_has_nowhere_to_put_a_check() {
        assert_eq!(TaskContract::ReadOnly.checks().count(), 0);
        assert_eq!(TaskContract::ReadOnly.closure(), Closure::NotContracted);
    }

    // ── SPEC 1 thesis 2: closure is derived, never asserted ─────────────────

    #[test]
    fn closure_is_earned_only_when_every_check_passes() {
        let mut dm = DefinitionOfDone::new(unit("a"), vec![unit("b")]);
        let contract = TaskContract::DefinitionOfDone(dm.clone());
        assert_eq!(
            contract.closure(),
            Closure::Outstanding {
                pending: 2,
                failed: 0
            }
        );
        assert!(!contract.closure().permits_close());

        for check in dm.iter_mut() {
            check.outcome = CheckOutcome::Passed {
                evidence: "ok".into(),
            };
        }
        let contract = TaskContract::DefinitionOfDone(dm);
        assert_eq!(contract.closure(), Closure::Earned);
        assert!(contract.closure().permits_close());
    }

    /// A failed check is not a pending one, and neither closes the task. The
    /// distinction is what a surface needs to tell "not yet" from "no".
    #[test]
    fn a_failed_check_is_counted_apart_from_a_pending_one() {
        let mut failing = unit("the suite is green");
        failing.outcome = CheckOutcome::Failed {
            evidence: "3 failures in auth::redirect".into(),
        };
        let contract =
            TaskContract::DefinitionOfDone(DefinitionOfDone::new(failing, vec![unit("b")]));
        assert_eq!(
            contract.closure(),
            Closure::Outstanding {
                pending: 1,
                failed: 1
            }
        );
    }

    /// A read-only task stands out of the way rather than claiming a pass it
    /// did not earn: `permits_close`, but never `Earned`.
    #[test]
    fn a_read_only_task_permits_close_without_claiming_it_was_earned() {
        let closure = TaskContract::ReadOnly.closure();
        assert!(closure.permits_close());
        assert_ne!(closure, Closure::Earned);
    }

    // ── the det/model split (SPEC 1 thesis 1, SPEC 6.1's `det %`) ───────────

    /// A contributed mechanism carries its judge, so the split stays answerable
    /// for a plugin's own check.
    #[test]
    fn a_contributed_mechanism_declares_its_judge() {
        let m = CheckMechanism::new("vera:flip-oracle", Judge::Deterministic);
        assert_eq!(m.kind(), None);
        assert_eq!(m.judge(), Judge::Deterministic);
        assert_eq!(m.as_str(), "vera:flip-oracle");

        let json = serde_json::to_string(&m).expect("serialize");
        let back: CheckMechanism = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(m, back);
    }

    /// Refused rather than guessed. Defaulting would price a model call at
    /// `$0.00` or make a plugin's graph query look like one, and both lie on
    /// the receipt.
    #[test]
    fn a_contributed_mechanism_without_a_judge_is_refused() {
        let err = serde_json::from_str::<CheckMechanism>(r#"{"name":"vera:flip"}"#)
            .expect_err("a judgeless contributed mechanism must not deserialize");
        assert!(err.to_string().contains("declares no judge"), "{err}");
    }
}
