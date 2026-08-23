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

impl Oracle {
    /// Whether any check decides `requirement`.
    #[must_use]
    pub fn decides(&self, requirement: &str) -> bool {
        self.checks.iter().any(|c| c.requirement == requirement)
    }

    /// The checks that did not hold, in declaration order.
    ///
    /// The whole evaluator for the evidence half of the grammar, and pure: the
    /// plugin runs its own oracle and reports the numbers, the host decodes
    /// that report and hands them in, and this applies the manifest's rule to
    /// them. Evaluating the rule is the host's half; running the oracle and
    /// vouching for what it produced are the plugin's (#3511). An empty answer
    /// means every declared check held — which is not by itself "done"
    /// whenever the flip policy also has something to say.
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
            let rule = check.rule()?;
            let value = reported.get(&rule.measurement).copied().ok_or_else(|| {
                ManifestError::MeasurementNotReported {
                    requirement: check.requirement.clone(),
                    measurement: rule.measurement.clone(),
                }
            })?;
            if !rule.holds(value) {
                unmet.push(UnmetCheck {
                    requirement: check.requirement.clone(),
                    rule,
                    reported: value,
                });
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
