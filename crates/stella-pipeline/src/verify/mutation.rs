//! Trivial-mutant generation for the witness mutation check (#870).
//!
//! A fail→pass flip proves the witness *reacted* to the change; it does not
//! prove the witness *constrains* it. The dynamic tautology — a witness that
//! stays green under any change to the target lines — slips past both the
//! flip and the static assertion-density screen (#863). The check here is
//! the cheapest dynamic probe that catches it: take the lines the candidate
//! changed, break each one in a classically-wrong way (negate a comparison,
//! flip a boolean connective, off-by-one a bound), and ask whether the
//! witness notices. A witness that stays green while the change is broken
//! under it is testing something other than the change.
//!
//! This module is the pure half: reading the candidate's own unified diff
//! and proposing at most [`MAX_MUTANTS`] single-line mutants. Running them
//! is the host's job (`MutationProbe`), and everything degrades open — a
//! diff with no mutable line, an unparseable hunk, an unknown language all
//! produce fewer (or zero) mutants, never a fabricated verdict.

use crate::ports::LineMutation;

/// What the mutation audit observed about the authored witness (#870).
///
/// Three values because the question has three answers, and #2607 is the
/// record of what happens when it is spelled with two. This was a
/// `witness_tautological: bool` on the ladder inputs, and `false` meant both
/// "the audit ran and the witness constrains the change" and "no audit ever
/// ran" — so deleting the code that computes it was not a compile error, not
/// a test failure, and not visible in a verdict: it was a silent downgrade to
/// "everything is fine" wearing the ladder's strongest badge. A guard whose
/// benign default is indistinguishable from its answer is a guard that can
/// stop being fed without anyone noticing.
///
/// Deliberately shaped like [`crate::verify::coverage::DiffCoverage`], the
/// other half of the same pair: `Unmeasured` is a statement about the
/// instrument and never a finding about the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MutationAudit {
    /// No mutant was ever *observed*: the probe was not wired, the diff
    /// proposed no mutable line, or every proposed mutant came back
    /// [`crate::ports::MutantOutcome::Unavailable`]. The default, and never a
    /// finding about the witness.
    #[default]
    Unmeasured,
    /// At least one mutant broke the changed lines and the witness went red —
    /// it constrains the change, and the audit stopped there (a killed mutant
    /// is proof; the remaining ones are not paid for).
    Constrains,
    /// Mutants WERE observed and the witness stayed green under every one of
    /// them: it reacts to the change without constraining it. The flip stands
    /// as an observation, but it may not buy a deterministic pass.
    Tautological,
}

impl MutationAudit {
    /// The finding of an audit that DID observe at least one mutant run:
    /// `killed` says whether any of them sent the witness red.
    ///
    /// The `observed > 0` precondition belongs to the caller, and it is the
    /// whole distinction this type exists to keep — an audit that observed
    /// nothing is [`MutationAudit::Unmeasured`] and must never arrive here as
    /// a `false`.
    #[must_use]
    pub fn from_killed(killed: bool) -> Self {
        if killed {
            MutationAudit::Constrains
        } else {
            MutationAudit::Tautological
        }
    }

    /// The audit's finding as the wire snapshot spells it (#865):
    /// `Some(true)` = constrains, `Some(false)` = tautological, `None` =
    /// never measured. The wire shape predates this enum and is deliberately
    /// unchanged — it was already tri-state, which is precisely why the
    /// *provenance* stayed honest while the ladder input could not.
    #[must_use]
    pub fn recorded(self) -> Option<bool> {
        match self {
            MutationAudit::Unmeasured => None,
            MutationAudit::Constrains => Some(true),
            MutationAudit::Tautological => Some(false),
        }
    }

    /// A sentence a verifier (or a human reading a verdict) can act on, in
    /// the same register as [`crate::verify::coverage::DiffCoverage::explain`]:
    /// what was observed, and what it does NOT mean.
    #[must_use]
    pub fn explain(self) -> &'static str {
        match self {
            MutationAudit::Unmeasured => {
                "witness_mutation=unmeasured (no mutant run was observed for this witness, so \
                 whether it constrains the change is UNKNOWN — this is not a finding that it \
                 does not)"
            }
            MutationAudit::Constrains => {
                "witness_mutation=constrains (the witness went red when a changed line was \
                 broken under it)"
            }
            MutationAudit::Tautological => {
                "witness_mutation=tautological (the witness stayed green under EVERY observed \
                 mutant of the changed lines — it reacts to the change without constraining it, \
                 which is not a finding that the change is wrong)"
            }
        }
    }

    /// Whether this finding may carry a deterministic fast-submit — that is,
    /// whether the ladder may *skip the verifier*.
    ///
    /// `Unmeasured` credits, for the reason every probe in this ladder
    /// degrades open: the mutation probe is optional, and an absent one must
    /// leave the pre-#870 ladder rather than manufacture a tautology finding.
    /// The downgrade requires positive evidence, never its absence — which is
    /// why the *guard* against an unfed audit is a wiring test
    /// (`pipeline/tests/verification_hardening/guard_wiring.rs`) and not this
    /// method.
    #[must_use]
    pub fn credits_a_deterministic_pass(self) -> bool {
        !matches!(self, MutationAudit::Tautological)
    }
}

/// Ceiling on proposed mutants: bounded cost, one witness run per mutant,
/// spent only on a candidate about to be credited a deterministic pass (the
/// pre-submit audit gates it) — in best-of-N that is per fast-submitting
/// candidate, not once per run.
pub const MAX_MUTANTS: usize = 3;

/// Propose up to [`MAX_MUTANTS`] single-line mutants from a unified diff,
/// excluding `excluded_paths` (the witness's own files — mutating the test
/// to check the test would prove nothing). Lines are drawn from the diff's
/// ADDED side: those are the candidate's claim, and the thing the witness
/// must be sensitive to.
pub fn mutants_from_diff(diff: &str, excluded_paths: &[String]) -> Vec<LineMutation> {
    let mut mutants = Vec::new();
    let mut current_path: Option<String> = None;
    let mut new_line: u32 = 0;
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ ") {
            let path = path.strip_prefix("b/").unwrap_or(path).trim();
            current_path = (path != "/dev/null").then(|| path.to_string());
            continue;
        }
        if line.starts_with("@@") {
            // `@@ -a,b +c,d @@` — the new-file cursor starts at c.
            new_line = line
                .split('+')
                .nth(1)
                .and_then(|rest| {
                    rest.split([',', ' '])
                        .next()
                        .and_then(|n| n.parse::<u32>().ok())
                })
                .unwrap_or(0);
            continue;
        }
        match line.as_bytes().first() {
            Some(b'+') => {
                let content = &line[1..];
                if let Some(path) = &current_path
                    && mutants.len() < MAX_MUTANTS
                    && !excluded_paths.iter().any(|p| p == path)
                    && let Some(mutated) = mutate_line(content)
                {
                    mutants.push(LineMutation {
                        path: path.clone(),
                        line: new_line,
                        original: content.to_string(),
                        mutated,
                    });
                }
                new_line += 1;
            }
            Some(b'-') => {}
            _ => new_line += 1,
        }
        if mutants.len() >= MAX_MUTANTS {
            break;
        }
    }
    mutants
}

/// The classic single-token breaks, first applicable wins. Comment-looking
/// lines are skipped — breaking prose proves nothing about the witness.
fn mutate_line(content: &str) -> Option<String> {
    let trimmed = content.trim_start();
    if trimmed.is_empty()
        || trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with('*')
    {
        return None;
    }
    // Ordered longest-token-first so `<=` is seen before `<`, `==` before
    // `=`. Each swap is its own inverse family; one swap per mutant.
    const SWAPS: &[(&str, &str)] = &[
        ("==", "!="),
        ("!=", "=="),
        ("<=", ">"),
        (">=", "<"),
        ("&&", "||"),
        ("||", "&&"),
        ("< ", ">= "),
        ("> ", "<= "),
        ("true", "false"),
        ("false", "true"),
        ("+ 1", "+ 2"),
        ("- 1", "- 2"),
    ];
    for (from, to) in SWAPS {
        if content.contains(from) {
            return Some(content.replacen(from, to, 1));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "\
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,4 +10,5 @@
 fn clamp(x: u32, hi: u32) -> u32 {
-    if x > hi { hi } else { x }
+    if x >= hi { hi } else { x }
+    // boundary now inclusive
 }
";

    /// The tri-state's whole point (#2607): "the audit never ran" and "the
    /// audit ran and cleared the witness" are different values, so a ladder
    /// input that stopped being fed cannot impersonate a finding. Both still
    /// credit a fast-submit — the probe is optional and degrades open — but a
    /// reader, a verdict snapshot, and a test can now tell them apart.
    #[test]
    fn an_unmeasured_audit_is_not_the_same_value_as_a_cleared_one() {
        assert_ne!(MutationAudit::Unmeasured, MutationAudit::Constrains);
        assert_eq!(MutationAudit::default(), MutationAudit::Unmeasured);
        assert_eq!(MutationAudit::Unmeasured.recorded(), None);
        assert_eq!(MutationAudit::Constrains.recorded(), Some(true));
        assert_eq!(MutationAudit::Tautological.recorded(), Some(false));
        assert!(MutationAudit::Unmeasured.credits_a_deterministic_pass());
        assert!(MutationAudit::Constrains.credits_a_deterministic_pass());
        assert!(!MutationAudit::Tautological.credits_a_deterministic_pass());
    }

    #[test]
    fn mutants_come_from_added_lines_with_correct_positions() {
        let mutants = mutants_from_diff(DIFF, &[]);
        assert_eq!(mutants.len(), 1, "the comment line must not be mutated");
        let m = &mutants[0];
        assert_eq!(m.path, "src/lib.rs");
        assert_eq!(m.line, 11, "hunk starts at 10; the context line is 10");
        assert_eq!(m.original, "    if x >= hi { hi } else { x }");
        assert_eq!(m.mutated, "    if x < hi { hi } else { x }");
    }

    #[test]
    fn witness_files_are_never_mutated() {
        let mutants = mutants_from_diff(DIFF, &["src/lib.rs".to_string()]);
        assert!(mutants.is_empty());
    }

    #[test]
    fn a_diff_with_no_mutable_line_degrades_to_no_mutants() {
        let prose = "--- a/README.md\n+++ b/README.md\n@@ -1 +1,2 @@\n+Some prose.\n+# A heading\n";
        assert!(mutants_from_diff(prose, &[]).is_empty());
    }

    #[test]
    fn the_mutant_cap_is_respected() {
        let mut diff = String::from("--- a/a.rs\n+++ b/a.rs\n@@ -1 +1,9 @@\n");
        for i in 0..9 {
            diff.push_str(&format!("+let x{i} = a == b;\n"));
        }
        assert_eq!(mutants_from_diff(&diff, &[]).len(), MAX_MUTANTS);
    }

    #[test]
    fn comparison_boolean_and_off_by_one_families_all_fire() {
        for (line, expect) in [
            ("if a == b {", "if a != b {"),
            ("while a && b {", "while a || b {"),
            ("let ready = true;", "let ready = false;"),
            ("let end = start + 1;", "let end = start + 2;"),
        ] {
            assert_eq!(mutate_line(line).as_deref(), Some(expect), "for {line}");
        }
        assert_eq!(mutate_line("// a == b in prose"), None);
    }
}
