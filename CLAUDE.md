# CLAUDE.md

@AGENTS.md

## Hard rules for every session

- **The bar is reference-grade Rust, and there is no second bar.** This
  repository is trying to be the best-engineered open-source agent in the
  world and the implementation other people cite when they argue about
  correctness. So when two designs both work, ship the one that is still
  correct in five years under a different maintainer, a different provider,
  and a model that does not exist yet — never the one that is faster to write
  today. Concretely:
  - **Match the best Rust in the world, not just the nearest file.**
    AGENTS.md's "match the neighborhood" is the floor, not the ceiling. For
    anything non-obvious, find the canonical crate that already solved your
    shape (`tokio`, `serde`, `rust-analyzer`, `cargo`, `ripgrep`) and follow
    its structure; name the exemplar in the PR description so the choice is
    reviewable instead of personal taste.
  - **An expedient is a defect, not a tradeoff.** An `#[allow]` with no
    comment saying why the lint is wrong *here*, an `unwrap` on runtime data,
    a widened `deny.toml` allow-list or a raised `scripts/file-size-baseline.txt`
    ceiling to turn a gate green, a `TODO` standing in for the actual design —
    each one is a bug against the PR that introduced it. If the correct fix is
    bigger than the session, file the issue (rule below) and **say so out loud
    in your final message**. Shipping the shortcut quietly is the one
    unrecoverable move.
  - **Correctness is demonstrated or it is not claimed.** A witness test, a
    property, a golden diff someone actually read — a sentence in a PR
    description is not evidence. Stella refuses to call a task done without
    proof; this repo does not get to hold itself to less than the contract it
    enforces on its users.
  - **Measure honestly, especially when it costs us.** A benchmark number that
    flatters Stella because of a measurement artifact is worse than a loss:
    it spends the exact reputation the project exists to build. Compare like
    for like, name and exclude operational aborts explicitly, and report the
    unflattering number when it is the true one.
  - **The agent-systems architecture is the crown.** The agent loop, the
    staged pipeline, the context plane, and the port boundaries get more
    scrutiny than anything else in the tree. Determinism, replayability, and
    "ports, not concretions" are not negotiable there for any deadline.
  - **"Now" vs. "right" is the maintainer's call, not yours.** When you truly
    cannot have both, state which one you are giving up and why, and let a
    human choose. Deciding it silently is how a reference implementation
    stops being one.
- **A bench conclusion comes from the trace, never from a surface signal.**
  The measurement machinery is younger than the thing it measures, and its
  summary layer is a set of projections that each have their own bugs. On
  2026-08-07 every surface reading was wrong, in a different direction each
  time: a 5/5 → 3/5 "regression" was a flaky task that also fails on the old
  binary; a trial showing `✗ failed` had scored reward 1.0 (the verdict cell is
  either/or, so it *hides* the exception when a task passes); a
  `WITNESS CONFIRMED` was a false proof off a `ModuleNotFoundError`; and "no
  witness authored" had three stacked structural causes, each concealed by the
  one above it. So:
  - **Open `stella-events.jsonl` before claiming anything.** It is ground
    truth; the UI and `result.json` are projections of it. Census
    `(role, model)` from `step_usage` before believing a claim about which
    model ran; join `tool_start` → `tool_result` for real tool wall clock;
    read the **full** `proof` reason strings, because they are truncated
    everywhere else and the truncated half is usually the part that matters.
  - **Run the cheap control before any bisect.** Re-run the failing task alone
    on the *old* SUT. One trial either establishes the regression or ends the
    investigation — and it is how the 11-commit bisect above got cancelled.
  - **A five-task solve rate cannot resolve anything smaller than a
    catastrophe.** Treat it as a guardrail; the mechanism metrics are the
    measurement. Comparisons need repeats per task, and any two runs differing
    by more than one commit are confounded and must be reported as such.
- **AGENTS.md is the orientation document.** Commands, architectural
  invariants, workspace routing, testing approach, and gotchas all live there
  (imported above). When this file and AGENTS.md disagree, AGENTS.md wins —
  and the disagreement is itself a bug: fix it in the same PR you noticed it.
- **Nothing left behind.** Every bug, defect, idea, missing test, piece of
  unwired code, or logical next step you notice and do not fix inside your
  current change MUST be filed as a GitHub issue before you finish — written
  as a handoff a fresh agent could execute without your session's context
  (problem, file paths, repro/verify steps, constraints, definition of done).
  Search for an existing issue first; link, don't duplicate. The full policy
  is AGENTS.md § "Nothing left behind — every finding becomes a fix or a
  GitHub issue". Never end a turn with untracked half-finished work.
