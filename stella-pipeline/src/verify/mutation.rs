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

/// Ceiling on proposed mutants: bounded cost, one witness run per mutant,
/// spent only on the winning candidate (the pre-submit audit gates it).
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
