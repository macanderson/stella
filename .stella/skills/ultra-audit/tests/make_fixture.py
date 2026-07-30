#!/usr/bin/env python3
"""Build a realistic audit result for testing score.py and render_report.py.

Seeded from a real recorded round so the delta arithmetic is exercised against real numbers
rather than round ones, and deliberately hostile in three ways that have each broken a report
before: a finding whose text contains `</script>`, one containing `${...}`, and one containing
non-ASCII and a double-encoded UTF-8 sequence.

    python3 tests/make_fixture.py [--history PATH] [--out result.json]
"""
from __future__ import annotations

import argparse
import json
import os
import statistics

DIMS = ["formatting", "language", "documentation", "file_names", "symbol_names",
        "file_organization", "system_architecture", "data_storage", "vendor_lock_avoidance",
        "security", "durability", "maintainability", "loop_correctness",
        "token_efficiency", "agent_performance", "compute_efficiency", "end_user_experience"]

# A prior round's real shape, used when no history file is available.
FALLBACK_PRIOR = {"formatting": 88, "language": 79, "documentation": 74, "file_names": 84,
                  "symbol_names": 82, "file_organization": 73, "system_architecture": 77,
                  "data_storage": 78, "vendor_lock_avoidance": 81, "security": 80,
                  "durability": 79, "maintainability": 76, "loop_correctness": 83,
                  "token_efficiency": 81, "agent_performance": 82, "compute_efficiency": 78,
                  "end_user_experience": 75}

# Per-dimension panel votes: (opus, fable, sonnet). Two are contested on purpose
# (spread >= 10) so the contested path is exercised end to end.
VOTES = {
    "formatting": (89, 90, 88), "language": (77, 80, 78), "documentation": (72, 74, 71),
    "file_names": (85, 84, 86), "symbol_names": (83, 82, 84),
    "file_organization": (70, 72, 71), "system_architecture": (76, 79, 74),
    "data_storage": (79, 78, 80), "vendor_lock_avoidance": (82, 81, 83),
    "security": (70, 82, 76),                      # contested: spread 12
    "durability": (80, 81, 79), "maintainability": (74, 77, 75),
    "loop_correctness": (84, 85, 83), "token_efficiency": (80, 82, 81),
    "agent_performance": (78, 84, 90),             # contested: spread 12
    "compute_efficiency": (79, 78, 80), "end_user_experience": (73, 76, 74),
}

HOSTILE = [
    {"severity": "critical", "dimension": "security", "file": "stella-tools/src/exec.rs",
     "line": 412, "found_by_model": "opus", "cross_model": True, "agreement": "unanimous-real",
     "issue": "The sanitizer strips `<script>` but not `</script >` with a space, so the "
              "guard at exec.rs:412 can be defeated by a single character. Any caller "
              "reaching render_untrusted() inherits the hole.",
     "remediation": "Parse rather than pattern-match; reject on any tag-like token.",
     "verdicts": [{"model": "fable", "refuted": False, "confidence": "high",
                   "reasoning": "Confirmed by reading exec.rs:400-430; the regex is "
                                "`</script>` literal and the HTML spec permits whitespace."},
                  {"model": "sonnet", "refuted": False, "confidence": "high",
                   "reasoning": "Reproduced the bypass by reading the tokenizer; no upstream "
                                "guard covers it."}]},
    {"severity": "high", "dimension": "token_efficiency", "file": "stella-core/src/prompt.rs",
     "line": 88, "found_by_model": "fable", "cross_model": True, "agreement": "majority-real",
     "issue": "The cached prefix interpolates `${elapsed_secs}` into the system prompt, so the "
              "prompt cache misses on every single turn. Every request pays full prefix cost.",
     "remediation": "Move the elapsed-time line below the cache breakpoint.",
     "verdicts": [{"model": "opus", "refuted": False, "confidence": "high",
                   "reasoning": "prompt.rs:88 splices a monotonic clock read into the cached "
                                "segment. Confirmed against the cache_control placement."},
                  {"model": "sonnet", "refuted": True, "confidence": "low",
                   "reasoning": "Believed the value was rounded to the hour; it is not."}]},
    {"severity": "high", "dimension": "language", "file": "stella-cli/src/help.rs", "line": 31,
     "found_by_model": "sonnet", "cross_model": True, "agreement": "contested",
     "issue": "User-facing strings contain double-encoded UTF-8 (Ã© for é) and a "
              "mojibake em-dash (â€”), visible in `stella --help` output.",
     "remediation": "Decode once at the boundary; add a test asserting no C3 A9 byte pair.",
     "verdicts": [{"model": "opus", "refuted": False, "confidence": "medium",
                   "reasoning": "Present at help.rs:31 and 47."},
                  {"model": "fable", "refuted": True, "confidence": "medium",
                   "reasoning": "The terminal renders it correctly under a UTF-8 locale, so "
                                "user impact is limited to non-UTF-8 terminals."}]},
]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--history", default=os.path.expanduser(
        "~/.claude/skills/reaudit/history/stella.json"))
    ap.add_argument("--out", default="result.json")
    a = ap.parse_args()

    prior = FALLBACK_PRIOR
    prior_round, prior_pub = 2, None
    if os.path.exists(a.history):
        runs = json.load(open(a.history)).get("runs", [])
        if runs:
            prior = runs[-1]["scores"]
            prior_round = runs[-1].get("round", len(runs))
            prior_pub = runs[-1].get("overall_rounded")

    median = {d: int(round(statistics.median(VOTES[d]))) for d in DIMS}
    spread = {d: max(VOTES[d]) - min(VOTES[d]) for d in DIMS}
    contested = [d for d in DIMS if spread[d] >= 10]

    dimension_scores = {}
    for d in DIMS:
        dimension_scores[d] = {
            "score": median[d],
            "grade": "B" if median[d] >= 80 else "C",
            "rationale": f"Scored on lens evidence for {d}; the panel median was {median[d]} "
                         f"with a spread of {spread[d]}.",
            "delta_rationale": (f"Moved from {prior.get(d)} because the {d} lens found "
                                f"concrete, cited change." if prior.get(d) != median[d]
                                else "Unchanged: nothing in this round bore on it."),
            "evidence": [f"stella-core/src/lib.rs:{100 + i}" for i in range(2)],
            **({"panel_resolution":
                f"Panel split {'/'.join(map(str, VOTES[d]))}. Read the code at the cited lines "
                f"and landed on {median[d]}: the optimistic panelist did not account for the "
                f"second code path."} if d in contested else {}),
        }

    result = {
        "run": {"root": "/Users/macanderson/Projects/stella", "commit": "ac5b344b2",
                "date": "2026-07-26", "depth": "deep", "round": prior_round + 1,
                "panel": ["opus", "fable", "sonnet"], "units": 34, "lens_jobs": 10,
                "files_owned": 538, "total_files": 538, "total_loc": 319154,
                "languages": [{"language": "Rust", "loc": 280141, "files": 475},
                              {"language": "Python", "loc": 34402, "files": 26},
                              {"language": "TypeScript", "loc": 2561, "files": 22}],
                "fix_authority": {"level": "full", "reason": "cargo provides a compile gate"},
                "llm_surface": True},
        "verify": {"fmt_clean": True, "lint_clean": True, "tests_pass": False,
                   "typecheck_clean": True,
                   "commands_run": ["cargo fmt --all", "cargo clippy --workspace",
                                    "cargo test --workspace"],
                   "regressions_found": ["stella-store::wal::truncate_is_atomic"],
                   "regressions_fixed": ["stella-store::wal::truncate_is_atomic"],
                   "reverted": ["stella-tui/src/deck.rs: dead-code removal was reachable"],
                   "preexisting": ["stella-store::wal::concurrent_writers (2 failures)"],
                   "unfixed": ["stella-store::wal::concurrent_writers"],
                   "test_summary": "3742 passed, 2 failed (both pre-existing)",
                   "final_state": "fmt and clippy clean; 2 pre-existing test failures remain."},
        "panel": {"votes": [{"model": m,
                             "scores": {d: VOTES[d][i] for d in DIMS},
                             "rationales": {d: f"{m} on {d}: cited evidence." for d in DIMS},
                             "headline": f"{m}: the store layer is the weakest link.",
                             "worst_dimension": "file_organization"}
                            for i, m in enumerate(["opus", "fable", "sonnet"])],
                  "median": median, "spread": spread, "contested": contested},
        "findings": {"total": 214, "severe": 51, "confirmed": HOSTILE,
                     "refuted": [{"severity": "high", "dimension": "durability",
                                  "file": "stella-store/src/wal.rs", "line": 210,
                                  "found_by_model": "opus", "agreement": "unanimous-refuted",
                                  "issue": "Claimed the WAL writes in place without a rename.",
                                  "verdicts": [{"model": "fable", "refuted": True,
                                                "already_fixed": True,
                                                "reasoning": "wal.rs:210 already uses "
                                                             "write-temp-then-rename."}]}],
                     "unverified": [{"severity": "high", "dimension": "compute_efficiency",
                                     "file": "stella-tui/src/render.rs", "line": 640,
                                     "issue": "Full-tree re-render on every keypress.",
                                     "found_by_model": "sonnet"}]},
        "regressionResults": [
            {"id": "shell-boundary-decorative", "status": "partially_fixed",
             "quality": "shallow", "test_pinned": False, "judged_by_model": "fable",
             "evidence": ["stella-tools/src/catalog.rs:133"],
             "reasoning": "start_process is gated now, but send_stdin still has no arm in "
                          "command_line_for, so the audit ledger stays blind on that path.",
             "residual_risk": "An operator's ledger shows a benign spawn and nothing else."},
            {"id": "prompt-cache-volatile-prefix", "status": "not_fixed", "quality": "wrong",
             "test_pinned": False, "judged_by_model": "sonnet",
             "evidence": ["stella-core/src/prompt.rs:88"],
             "reasoning": "The elapsed-time interpolation is still inside the cached prefix."},
            {"id": "wal-atomicity", "status": "fixed", "quality": "excellent",
             "test_pinned": True, "judged_by_model": "opus",
             "evidence": ["stella-store/src/wal.rs:210", "wal_tests.rs:88"],
             "reasoning": "Write-temp-then-rename landed with a test that fails if reverted."}],
        "fixReviews": [
            {"unit": "./stella-tui::part-2", "verdict": "unsound_fixes_present",
             "reviewed_count": 6, "author_model": "opus", "reviewer_model": "fable",
             "bad_fixes": [{"file": "stella-tui/src/deck.rs", "action": "revert",
                            "breaks_build": False,
                            "problem": "Removed a function as dead code; it is reached from "
                                       "the headless renderer via a trait object."}]},
            {"unit": "./stella-core::part-1", "verdict": "all_sound", "reviewed_count": 9,
             "author_model": "fable", "reviewer_model": "sonnet", "bad_fixes": []}],
        "critiques": [{"model": "fable",
                       "verdict": "The audit is broadly sound but too kind on "
                                  "agent_performance, where one panelist scored 90 on the "
                                  "strength of a benchmark that measures the wrong path.",
                       "miscalibrated": [{"dimension": "agent_performance",
                                          "direction": "too_high", "argued_score": 78,
                                          "why": "The cited benchmark excludes the blocking "
                                                 "store read on the render path."}],
                       "missed": ["No lens examined the release/packaging path.",
                                  "The Python subtree (34k lines) was audited by unit agents "
                                  "but no lens covered it specifically."],
                       "disputed": [{"claim": "documentation scored 72",
                                     "why": "Doc examples are not compiled, so the true "
                                            "figure is lower."}],
                       "agree_with": ["security is the right headline risk"],
                       "process_critique": "Two contested dimensions is a small sample; the "
                                           "panel agreed too readily elsewhere."}],
        "synthesis": {
            "overall_score": int(round(sum(median[d] for d in DIMS) / len(DIMS))),
            "dimension_scores": dimension_scores,
            "executive_summary": "The workspace is a strong, actively maintained Rust agent "
                                 "system with a real security boundary that is not yet fully "
                                 "enforced.\n\nThe store layer remains the weakest link.",
            "remediation_verdict": "Converging, but two of three prior findings were closed "
                                   "shallowly — the reported instance was fixed while the "
                                   "defect class stayed open.",
            "second_opinion_response": "Accepted the agent_performance challenge and moved it "
                                       "to the panel median rather than the optimistic vote. "
                                       "Overruled the documentation dispute: doc examples are "
                                       "compiled in CI for the three public crates.",
            "top_strengths": [{"title": "Cancellation is real",
                               "detail": "Interrupt propagates to child process groups."}],
            "top_risks": [{"title": "Sanitizer bypass in the exec boundary",
                           "severity": "critical", "dimension": "security",
                           "file": "stella-tools/src/exec.rs",
                           "detail": "A whitespace variant defeats the tag filter.",
                           "remediation": "Tokenize instead of pattern-matching.",
                           "agreement": "unanimous-real"},
                          {"title": "Prompt cache misses every turn",
                           "severity": "high", "dimension": "token_efficiency",
                           "file": "stella-core/src/prompt.rs",
                           "detail": "A clock read sits inside the cached prefix.",
                           "remediation": "Move it below the breakpoint.",
                           "agreement": "majority-real"}],
            "roadmap": [{"horizon": "now", "item": "Tokenize the exec sanitizer",
                         "why": "Closes a confirmed critical bypass.", "effort": "1 day",
                         "dimension": "security"},
                        {"horizon": "now", "item": "Move the clock read out of the prefix",
                         "why": "Recovers prompt-cache hits on every turn.",
                         "effort": "1 hour", "dimension": "token_efficiency"},
                        {"horizon": "next", "item": "Split the store god-module",
                         "why": "21k lines in one unit blocks review.", "effort": "1 week",
                         "dimension": "file_organization"},
                        {"horizon": "later", "item": "Compile doc examples in CI",
                         "why": "Documentation cannot rot if CI runs it.", "effort": "2 days",
                         "dimension": "documentation"}],
            "scoring_methodology": "Three frontier models scored the same evidence bundle "
                                   "blind; the reported value is the median, and the two "
                                   "contested dimensions were resolved by reading code.",
            "audit_limitations": "One severe finding went unverified at depth=deep. Two "
                                 "pre-existing test failures were not fixed. The panel shares "
                                 "training lineage, so agreement is weaker evidence than it "
                                 "looks.",
        },
        "unitResults": [{"unit": f"./unit-{i}", "loc": 12000, "n_files": 20,
                         "found_by_model": ["opus", "fable", "sonnet"][i % 3],
                         "scores": {d: median[d] for d in DIMS},
                         "summary": f"Unit {i} summary.",
                         "strengths": ["clear seams"],
                         "findings": [], "fixes_applied": [{"file": "x.rs", "change": "doc",
                                                            "rationale": "clarity"}]}
                        for i in range(6)],
        "xcutResults": [{"lens": k, "pass": p, "found_by_model": m,
                         "scores": {d: median[d] for d in DIMS},
                         "summary": f"{k} lens pass {p}.", "strengths": [], "findings": [],
                         "top_recommendations": [f"fix {k}"]}
                        for k, p, m in [("security", 1, "opus"), ("security", 2, "fable"),
                                        ("loop-correctness", 1, "fable"),
                                        ("loop-correctness", 2, "sonnet"),
                                        ("architecture", 1, "sonnet"),
                                        ("data-durability", 1, "opus"),
                                        ("efficiency", 1, "fable"),
                                        ("naming-consistency", 1, "sonnet"),
                                        ("ux-readiness", 1, "opus"),
                                        ("maintainability", 1, "fable")]],
        "sweepResults": [],
    }
    with open(a.out, "w") as fh:
        json.dump(result, fh, indent=1)
    print(f"-> {a.out}")
    print(f"prior round {prior_round} (published {prior_pub}); "
          f"contested dimensions: {contested}")
    print("hostile payloads included: </script>, ${...}, double-encoded UTF-8")


if __name__ == "__main__":
    main()
