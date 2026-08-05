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
