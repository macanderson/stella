//! The `[oracle]` block's evidence half — the numbers a plugin's own oracle
//! reports, and the checks over them that decide a requirement.
//!
//! [`crate::manifest`] declares *what* done means (`[requirements]`, one named
//! entry per clause) and *who* establishes it (`[oracle]`, a process the
//! plugin runs and reports the result of — never one the host runs, #3511).
//! Until this module, the only thing that could turn evidence into a verdict
//! was [`FlipPolicy::Required`] — a fail→pass flip — so the grammar could carry
//! exactly one definition of done: a witness test went from red to green.
//!
//! That is a real limitation and it was found by trying to write a plugin that
//! does not fit it (`doc:pipeline-as-plugins` §6.1, the D-1 falsifier): a
//! **performance budget**, where done means a named benchmark's p50 did not
//! regress past a recorded budget. Its oracle observes no flip — the benchmark
//! passes before and after; what changes is a number — and its verdict rule is
//! a threshold, which had nowhere in the manifest to live. A threshold that
//! lives inside the oracle binary is exactly the arrangement §6 rejects: the
//! plugin would be deciding done, and a user could not see the budget before
//! installing it, nor a reviewer diff a change to it.
//!
//! So the widening is two named additions, both closed:
//!
//! - [`FlipPolicy::NotApplicable`] — this oracle's evidence is not a flip. It
//!   is strictly *more* demanding than `required`, not an escape from it:
//!   with no flip to decide anything, **every** requirement must be decided by
//!   a check, or the manifest is refused
//!   ([`ManifestError::UndecidableRequirement`]).
//! - `measurements` + `[[oracle.checks]]` — the oracle declares the names of
//!   the numbers it reports, and each check states one rule over one of them,
//!   in the same closed comparison grammar [`crate::wrapper`] already uses
//!   (`<measurement> <op> <integer>`). A check naming an undeclared
//!   measurement is a load error, for the reason a condition naming an
//!   unpublished signal is: a manifest that quietly does nothing is worse than
//!   one that refuses to load.
//!
//! The measurement namespace is the **plugin's**, not the host's, which is the
//! one place this differs from [`crate::wrapper::Signal`] and is deliberate:
//! the host cannot enumerate every benchmark anyone will ever budget. What
//! stays closed is what matters — the comparison vocabulary, the shape of a
//! rule, and the requirement that every name a rule reads was declared in the
//! same manifest a human consented to. [`crate::Role`]'s `tier` is the
//! existing precedent for an open vocabulary the host resolves.
//!
//! # Scope
//!
//! Nothing here runs an oracle or parses its output — and neither does the
//! host (#3511). [`Oracle::unmet`] is the pure evaluator: the plugin runs its
//! own oracle and reports the numbers on its `after_turn` response, the host
//! decodes that report and hands them in, and the rule applied to them is the
//! one the manifest declared and a human consented to. That is the same
//! division of labour [`crate::program`] has with signal values, with the
//! plugin standing where the producing process always stood.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::ManifestError;
use crate::oracle::{FlipPolicy, Oracle};
use crate::wrapper::CompareOp;

/// One `[[oracle.checks]]` entry — the rule that decides one requirement.
///
/// Kept as the author's own text so the manifest round-trips byte-for-byte
/// (invariant 4), while [`OracleCheck::rule`] hands back the parsed form —
/// the [`crate::WrapperStage::condition`] shape.
///
/// Several checks may name the same requirement, and they conjoin: the
/// requirement is met when every check on it holds. That falls out of the
/// list shape and needs no syntax, which is why it is allowed — a grammar
/// grows a named predicate, never an expression language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OracleCheck {
    /// Which `[requirements]` entry this check decides. Must name one that
    /// the same manifest declares.
    pub requirement: String,
    /// The rule, as written: `<measurement> <op> <integer>`.
    #[serde(rename = "check")]
    pub rule: String,
}

impl OracleCheck {
    /// The parsed rule.
    ///
    /// Infallible in the sense that matters: validation already rejected any
    /// manifest whose check text does not parse, so a check reached through a
    /// validated [`crate::PluginManifest`] always answers `Ok`.
    ///
    /// # Errors
    ///
    /// [`ManifestError::UnparsableCheck`] for text outside the grammar — the
    /// same [`ManifestError`] validation would return, for a hand-constructed
    /// value that never went through the constructor.
    pub fn rule(&self) -> Result<MeasurementRule, ManifestError> {
        let unparsable = |reason: String| ManifestError::UnparsableCheck {
            requirement: self.requirement.clone(),
            check: self.rule.clone(),
            reason,
        };

        // Whitespace-separated, exactly as a wrapper condition is tokenised,
        // and for the same reason: every accepted rule writes its operator as
        // its own token, so `p50<=105` lands here rather than being reported
        // as the undeclared measurement `"p50<=105"`.
        let tokens: Vec<&str> = self.rule.split_whitespace().collect();
        let [measurement, op, value] = tokens.as_slice() else {
            return Err(unparsable(
                "expected a rule of the form \"<measurement> <op> <number>\", \
                 with spaces around the operator"
                    .to_string(),
            ));
        };
        let op = CompareOp::from_wire(op).ok_or_else(|| {
            unparsable(format!(
                "unknown comparison \"{op}\"; the operators are {}",
                CompareOp::ALL
                    .iter()
                    .map(|o| o.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
        let value = value.parse::<u64>().map_err(|_| {
            unparsable(format!(
                "\"{value}\" is not a non-negative whole number. Budgets are whole numbers by \
                 design (#3488): express a fractional or signed quantity in an integer unit your \
                 own oracle chooses and reports, the way the reference fixture reports percent of \
                 baseline — 105 is five percent slower, 97 is three percent faster"
            ))
        })?;
        Ok(MeasurementRule {
            measurement: (*measurement).to_string(),
            op,
            value,
        })
    }
}

/// A parsed check — one measurement, one comparison, one literal.
///
/// # The literal is a whole number, and that is the ruling rather than a gap
///
/// The numbers are non-negative integers, the same literal type the wrapper
/// grammar compares against. A plugin whose quantity is fractional or signed
/// **declares it in an integer unit its own oracle chooses and reports** — the
/// reference fixture reports "percent of baseline", so `105` is five percent
/// slower and `97` is three percent faster. That unit is the plugin's choice
/// by design, exactly as the measurement *namespace* is: the host cannot
/// enumerate every benchmark anyone will budget, and it cannot enumerate their
/// units either.
///
/// A float was weighed and refused (#3488, `doc:pipeline-as-plugins` §6.1).
/// `NaN` makes every comparison false, so a broken oracle reporting one would
/// silently *satisfy* a `<=` budget — in the one code path that decides
/// whether a turn may end. `-0.0` and rounding equality are the same hazard in
/// smaller print. The falsifier that produced this grammar did not need
/// fractions, and CLAUDE.md's rule is to widen only what the falsifier needed.
///
/// So an author writing `<= 0.5` is refused, and
/// [`ManifestError::UnparsableCheck`] names the remedy rather than only the
/// rejection — the cost of the ruling is one sentence per author, paid once,
/// at load, in a message they read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasurementRule {
    /// The measurement read. Declared in the same `[oracle]` block.
    pub measurement: String,
    /// The comparison applied.
    pub op: CompareOp,
    /// The literal compared against — the budget.
    pub value: u64,
}

impl MeasurementRule {
    /// Whether the reported value satisfies this rule.
    #[must_use]
    pub fn holds(&self, reported: u64) -> bool {
        self.op.apply(reported, self.value)
    }
}

impl std::fmt::Display for MeasurementRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} {}",
            self.measurement,
            self.op.as_str(),
            self.value
        )
    }
}

/// A check that did not hold — one unmet clause of the definition of done,
/// carrying everything a hold message needs to be attributable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmetCheck {
    /// The `[requirements]` entry this check was decided against.
    pub requirement: String,
    /// The rule that was applied.
    pub rule: MeasurementRule,
    /// What the oracle actually reported for the rule's measurement.
    pub reported: u64,
}

impl std::fmt::Display for UnmetCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} was {}, budget {} {}",
            self.requirement,
            self.rule.measurement,
            self.reported,
            self.rule.op.as_str(),
            self.rule.value
        )
    }
}

/// What one `[[oracle.checks]]` entry says about one set of reported numbers.
///
/// **The whole per-check semantics of the grammar, and the only copy of it**
/// (#3515). It lives here beside [`MeasurementRule::holds`] because it *is*
/// the grammar's own meaning, and it is a plain enum rather than a `Result`
/// because both of its callers have to answer for every arm:
///
/// - [`Oracle::unmet`] folds it into `Result<Vec<UnmetCheck>, ManifestError>`,
///   the shape a caller with somewhere to return an error wants.
/// - `stella_runtime::wrapper::judge` folds it into `UnmetRequirement` /
///   `UndecidedReason`, and **must** be total: it is the function that decides
///   whether a turn may end, so it has no `Result` to escape through and a
///   missing number has to become a named abstention rather than a silence
///   (`doc:wrapper-socket` §4).
///
/// Those two report differently and that is all they do differently. Before
/// this type they each walked the checks themselves — `judge` calling
/// `check.rule()`, reading `measurements`, and applying `holds` in its own
/// loop — so the evaluator the plugin crate documented as canonical had no
/// production caller at all, and the copy that actually bound a verdict was in
/// another crate where a change to either was invisible to the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// The oracle reported the measurement and the rule holds over it.
    Held,
    /// The oracle reported the measurement and the rule does not hold.
    Failed {
        /// The rule that was applied.
        rule: MeasurementRule,
        /// What the oracle reported for its measurement.
        reported: u64,
    },
    /// The rule parsed, and the oracle did not report the number it reads.
    ///
    /// Never "the budget held": a missing number decides nothing, which is the
    /// `crate::program` discipline pointed at evidence.
    MeasurementMissing {
        /// The measurement the rule reads and the report does not carry.
        measurement: String,
    },
    /// The check's text is outside the grammar.
    ///
    /// Reachable only for an [`Oracle`] that did not come from
    /// [`crate::PluginManifest::from_toml_str`] — validation rejects
    /// unparsable text at load — and still not "the budget held".
    Unreadable {
        /// Why the text does not parse, in the words an author needs: the
        /// `reason` field of [`ManifestError::UnparsableCheck`] rather than
        /// its whole rendering, because every caller already holds the
        /// requirement and the check text it would otherwise repeat.
        reason: String,
    },
}

impl OracleCheck {
    /// Apply this check to what the oracle reported.
    ///
    /// Total: every shape of check and every shape of report has an answer
    /// here, and none of them is "ask someone". See [`CheckOutcome`] for why
    /// that totality is the property both callers need.
    #[must_use]
    pub fn outcome(&self, reported: &BTreeMap<String, u64>) -> CheckOutcome {
        let rule = match self.rule() {
            Ok(rule) => rule,
            // `rule()` returns exactly this variant; the second arm renders
            // whatever a future one would say rather than claiming totality it
            // does not have.
            Err(ManifestError::UnparsableCheck { reason, .. }) => {
                return CheckOutcome::Unreadable { reason };
            }
            Err(other) => {
                return CheckOutcome::Unreadable {
                    reason: other.to_string(),
                };
            }
        };
        match reported.get(&rule.measurement).copied() {
            None => CheckOutcome::MeasurementMissing {
                measurement: rule.measurement,
            },
            Some(reported) if !rule.holds(reported) => CheckOutcome::Failed { rule, reported },
            Some(_) => CheckOutcome::Held,
        }
    }
}

impl Oracle {
    /// Whether any check decides `requirement`.
    #[must_use]
    pub fn decides(&self, requirement: &str) -> bool {
        self.checks.iter().any(|c| c.requirement == requirement)
    }

    /// The checks that did not hold, in declaration order.
    ///
    /// A fold over [`OracleCheck::outcome`] — the shared kernel — into the
    /// shape a caller that *has* somewhere to return an error wants: the two
    /// arms that decide nothing become an `Err` here, where
    /// `stella_runtime::wrapper::judge` folds the same kernel into a named
    /// abstention because it is total (#3515). Pure either way: the plugin
    /// runs its own oracle and reports the numbers, the host decodes that
    /// report and hands them in, and the manifest's rule is applied to them
    /// (#3511). An empty answer means every declared check held — which is not
    /// by itself "done" whenever the flip policy also has something to say.
    ///
    /// # Errors
    ///
    /// [`ManifestError::MeasurementNotReported`] when the oracle declared a
    /// measurement, a check reads it, and the reported set does not contain
    /// it. A missing number is never read as a satisfied budget — the
    /// `crate::program` discipline, applied to evidence.
    ///
    /// [`ManifestError::UnparsableCheck`] only for an [`Oracle`] that did not
    /// come from [`crate::PluginManifest::from_toml_str`]; validation rejects
    /// unparsable text at load.
    pub fn unmet(
        &self,
        reported: &BTreeMap<String, u64>,
    ) -> Result<Vec<UnmetCheck>, ManifestError> {
        let mut unmet = Vec::new();
        for check in &self.checks {
            match check.outcome(reported) {
                CheckOutcome::Held => {}
                CheckOutcome::Failed { rule, reported } => unmet.push(UnmetCheck {
                    requirement: check.requirement.clone(),
                    rule,
                    reported,
                }),
                CheckOutcome::MeasurementMissing { measurement } => {
                    return Err(ManifestError::MeasurementNotReported {
                        requirement: check.requirement.clone(),
                        measurement,
                    });
                }
                // Rebuilt rather than re-parsed: the kernel already carries
                // the `reason`, and the other two fields are this check's own,
                // so the error is byte-identical to the one `rule()` returns.
                CheckOutcome::Unreadable { reason } => {
                    return Err(ManifestError::UnparsableCheck {
                        requirement: check.requirement.clone(),
                        check: check.rule.clone(),
                        reason,
                    });
                }
            }
        }
        Ok(unmet)
    }

    /// The cross-field rules for the evidence half, called from the manifest's
    /// `validate` once the oracle's own shape rules have passed.
    ///
    /// `requirements` is the manifest's table, which the earlier arbiter rules
    /// have already forced to be present and non-empty by the time an oracle
    /// exists; it is passed as an `Option` so this function states the rule it
    /// enforces rather than depending on that ordering.
    pub(crate) fn validate_evidence(
        &self,
        requirements: Option<&BTreeMap<String, String>>,
    ) -> Result<(), ManifestError> {
        let mut seen: Vec<&str> = Vec::with_capacity(self.measurements.len());
        for measurement in &self.measurements {
            if measurement.trim().is_empty() {
                return Err(ManifestError::EmptyMeasurementName);
            }
            if seen.contains(&measurement.as_str()) {
                return Err(ManifestError::DuplicateMeasurement {
                    measurement: measurement.clone(),
                });
            }
            seen.push(measurement);
        }

        let declared = |name: &str| requirements.is_some_and(|r| r.contains_key(name));
        for check in &self.checks {
            if !declared(&check.requirement) {
                return Err(ManifestError::CheckWithoutRequirement {
                    requirement: check.requirement.clone(),
                });
            }
            let rule = check.rule()?;
            if !seen.contains(&rule.measurement.as_str()) {
                return Err(ManifestError::UnknownMeasurement {
                    requirement: check.requirement.clone(),
                    check: check.rule.clone(),
                    measurement: rule.measurement,
                    declared: seen.join(", "),
                });
            }
        }

        // With no flip to decide anything, an unchecked requirement is a
        // clause of the definition of done that nothing can ever establish.
        // This is what keeps `not-applicable` from being a way to hold the
        // Stop gate on vibes.
        if self.flip == FlipPolicy::NotApplicable {
            for name in requirements.into_iter().flat_map(BTreeMap::keys) {
                if !self.decides(name) {
                    return Err(ManifestError::UndecidableRequirement {
                        requirement: name.clone(),
                    });
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PluginManifest;

    /// An arbiter whose oracle reports numbers instead of observing a flip —
    /// the falsifier's shape, with the parts each test varies spliced in.
    fn budget_manifest(measurements: &str, checks: &str, flip: &str) -> String {
        format!(
            "name = \"perf\"\n\
             [loop]\n\
             participation = \"arbiter\"\n\
             hooks = [\"Stop\"]\n\
             points = [\"after_turn\"]\n\
             [requirements]\n\
             within-budget = \"the benchmark is inside its budget\"\n\
             [oracle]\n\
             command = {{ argv = [\"bench\"], timeout_secs = 60 }}\n\
             flip = \"{flip}\"\n\
             tamper = \"artifact-identity\"\n\
             measurements = [{measurements}]\n\
             {checks}"
        )
    }

    fn parse(measurements: &str, checks: &str, flip: &str) -> Result<Oracle, ManifestError> {
        PluginManifest::from_toml_str(&budget_manifest(measurements, checks, flip))
            .map(|m| m.oracle.expect("[oracle] declared"))
    }

    const CHECK: &str =
        "[[oracle.checks]]\nrequirement = \"within-budget\"\ncheck = \"p50 <= 105\"";

    #[test]
    fn a_budget_decides_a_requirement_in_both_directions() {
        let oracle = parse("\"p50\"", CHECK, "not-applicable").unwrap();
        let within = BTreeMap::from([("p50".to_string(), 103)]);
        assert!(oracle.unmet(&within).unwrap().is_empty());

        let over = BTreeMap::from([("p50".to_string(), 118)]);
        let unmet = oracle.unmet(&over).unwrap();
        assert_eq!(
            unmet,
            vec![UnmetCheck {
                requirement: "within-budget".into(),
                rule: MeasurementRule {
                    measurement: "p50".into(),
                    op: CompareOp::LessOrEqual,
                    value: 105,
                },
                reported: 118,
            }]
        );
        assert_eq!(
            unmet[0].to_string(),
            "within-budget: p50 was 118, budget <= 105"
        );
    }

    /// **The witness for #3515.** The one evaluator answers all four shapes,
    /// and answers them without a `Result` — which is what lets
    /// `stella_runtime::wrapper::judge` fold the same code while staying
    /// total. Before this existed, `judge` walked the checks itself and the
    /// evaluator this crate called canonical had no production caller.
    #[test]
    fn the_check_kernel_is_total_over_all_four_shapes() {
        let oracle = parse("\"p50\"", CHECK, "not-applicable").unwrap();
        let check = &oracle.checks[0];

        assert_eq!(
            check.outcome(&BTreeMap::from([("p50".to_string(), 103)])),
            CheckOutcome::Held
        );
        assert_eq!(
            check.outcome(&BTreeMap::from([("p50".to_string(), 118)])),
            CheckOutcome::Failed {
                rule: MeasurementRule {
                    measurement: "p50".into(),
                    op: CompareOp::LessOrEqual,
                    value: 105,
                },
                reported: 118,
            }
        );
        assert_eq!(
            check.outcome(&BTreeMap::new()),
            CheckOutcome::MeasurementMissing {
                measurement: "p50".into(),
            },
            "a number nobody reported decides nothing; reading it as a satisfied \
             budget is the failure this arm exists to name"
        );

        // Only reachable for a check that did not come through
        // `from_toml_str`, which is why it is constructed by hand here.
        let unvalidated = OracleCheck {
            requirement: "within-budget".into(),
            rule: "p50 <= half".into(),
        };
        let CheckOutcome::Unreadable { reason } =
            unvalidated.outcome(&BTreeMap::from([("p50".to_string(), 1)]))
        else {
            panic!("text outside the grammar must not read as a held budget");
        };
        assert!(
            reason.contains("not a non-negative whole number"),
            "{reason}"
        );
    }

    /// A number the oracle failed to report is an error, never a satisfied
    /// budget — the `SignalNotProduced` discipline pointed at evidence.
    #[test]
    fn an_unreported_measurement_is_an_error_not_a_pass() {
        let oracle = parse("\"p50\"", CHECK, "not-applicable").unwrap();
        let err = oracle.unmet(&BTreeMap::new()).unwrap_err();
        assert!(
            matches!(
                err,
                ManifestError::MeasurementNotReported { ref measurement, .. } if measurement == "p50"
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn a_check_naming_an_undeclared_measurement_is_a_load_error() {
        let err = parse("\"p99\"", CHECK, "not-applicable").unwrap_err();
        assert!(
            matches!(err, ManifestError::UnknownMeasurement { ref measurement, .. } if measurement == "p50"),
            "got {err:?}"
        );
    }

    #[test]
    fn a_check_naming_an_undeclared_requirement_is_a_load_error() {
        let orphan = "[[oracle.checks]]\nrequirement = \"nonexistent\"\ncheck = \"p50 <= 105\"";
        let err = parse("\"p50\"", orphan, "required").unwrap_err();
        assert!(
            matches!(err, ManifestError::CheckWithoutRequirement { ref requirement } if requirement == "nonexistent"),
            "got {err:?}"
        );
    }

    /// The rule that keeps `not-applicable` from being an escape hatch: with
    /// no flip, a requirement no check decides could never be established.
    #[test]
    fn not_applicable_requires_every_requirement_to_be_checked() {
        let err = parse("\"p50\"", "", "not-applicable").unwrap_err();
        assert!(
            matches!(err, ManifestError::UndecidableRequirement { ref requirement } if requirement == "within-budget"),
            "got {err:?}"
        );

        // The same manifest under `required` loads: the flip decides it.
        assert!(parse("\"p50\"", "", "required").is_ok());
    }

    #[test]
    fn measurement_names_are_non_blank_and_unique() {
        let blank = parse("\"p50\", \"  \"", CHECK, "not-applicable").unwrap_err();
        assert!(matches!(blank, ManifestError::EmptyMeasurementName));

        let dupe = parse("\"p50\", \"p50\"", CHECK, "not-applicable").unwrap_err();
        assert!(
            matches!(dupe, ManifestError::DuplicateMeasurement { ref measurement } if measurement == "p50"),
            "got {dupe:?}"
        );
    }

    /// The ruling #3488 settled, asserted where an author meets it: a
    /// fractional budget is refused, and the refusal says what to write
    /// instead. "Not a non-negative whole number" alone tells an author their
    /// syntax is wrong and leaves them to invent a unit — which is the
    /// interoperability tax the issue was filed about, since each author
    /// invents a different one.
    #[test]
    fn a_fractional_budget_is_refused_with_the_remedy_named() {
        let check = "[[oracle.checks]]\nrequirement = \"within-budget\"\ncheck = \"p50 <= 0.5\"";
        let err = parse("\"p50\"", check, "not-applicable").unwrap_err();
        let ManifestError::UnparsableCheck { reason, .. } = &err else {
            panic!("a fractional budget must not parse, got {err:?}");
        };
        assert!(
            reason.contains("integer unit your own oracle chooses"),
            "the refusal must name the remedy, got {reason:?}"
        );
        assert!(
            reason.contains("percent of baseline"),
            "the refusal must name the worked example the fixture ships, got {reason:?}"
        );

        // The same budget expressed in the unit the message names loads, which
        // is what makes the remedy a remedy rather than an apology.
        let scaled = "[[oracle.checks]]\nrequirement = \"within-budget\"\ncheck = \"p50 <= 1005\"";
        assert!(parse("\"p50\"", scaled, "not-applicable").is_ok());
    }

    #[test]
    fn unparsable_check_text_is_a_load_error() {
        for text in ["p50<=105", "p50 <= many", "p50 =< 105", "p50"] {
            let check =
                format!("[[oracle.checks]]\nrequirement = \"within-budget\"\ncheck = \"{text}\"");
            let err = parse("\"p50\"", &check, "not-applicable").unwrap_err();
            assert!(
                matches!(err, ManifestError::UnparsableCheck { .. }),
                "{text:?} must not parse, got {err:?}"
            );
        }
    }

    /// Several checks on one requirement conjoin — every one of them must
    /// hold, and each failure is reported on its own so a hold message can
    /// name all of them.
    #[test]
    fn checks_on_one_requirement_conjoin() {
        let both = "[[oracle.checks]]\nrequirement = \"within-budget\"\ncheck = \"p50 <= 105\"\n\
                    [[oracle.checks]]\nrequirement = \"within-budget\"\ncheck = \"p99 <= 130\"";
        let oracle = parse("\"p50\", \"p99\"", both, "not-applicable").unwrap();

        let reported = BTreeMap::from([("p50".to_string(), 118), ("p99".to_string(), 140)]);
        let unmet = oracle.unmet(&reported).unwrap();
        assert_eq!(unmet.len(), 2, "both failures must be reported");

        let one_bad = BTreeMap::from([("p50".to_string(), 100), ("p99".to_string(), 140)]);
        assert_eq!(oracle.unmet(&one_bad).unwrap().len(), 1);
    }
}
