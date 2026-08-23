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
    description is not evidence. This repo refuses to call a task done
    without proof — and it holds a plugin's self-reported evidence to no less
    a standard: state which check ran and what it found, the same discipline
    this rule asks of a PR description. (See AGENTS.md's opening and
    `doc:pipeline-as-plugins` on why "the check ran" used to mean two
    different things depending on the path, and no longer does: the
    built-in staged pipeline that once ran the check itself is deleted from
    this workspace (#3865), so host-run verification no longer exists here
    at all. The only path left is an installed verification plugin's
    self-reported evidence (Oxagen's Vera is the private reference one),
    which Stella evaluates against the plugin's declared rule and never
    re-runs or re-checks. Neither relaxes this rule: it is about how *this
    repository* reviews *its own* changes.)
  - **Measure honestly, especially when it costs us.** A benchmark number that
    flatters Stella because of a measurement artifact is worse than a loss:
    it spends the exact reputation the project exists to build. Compare like
    for like, name and exclude operational aborts explicitly, and report the
    unflattering number when it is the true one.
  - **The agent-systems architecture is the crown.** The agent loop, the
    staged pipeline, the context plane, and the port boundaries get more
    scrutiny than anything else in the tree. Determinism, replayability, and
    "ports, not direct dependencies" are not negotiable there for any deadline.
  - **"Now" vs. "right" is the maintainer's call, not yours.** When you truly
    cannot have both, state which one you are giving up and why, and let a
    human choose. Deciding it silently is how a reference implementation
    stops being one.
- **Every factual claim ships with the evidence that establishes it, or it is
  not made.** This is the general rule the bench rule below is one instance of,
  and it governs everything you say to a human: what a build contains, when a
  commit landed, why a run behaved as it did, whether a fix is present, what a
  test proves. State the claim, name the command or file and line that shows
  it, and — this is the half that gets skipped — **check that the evidence can
  actually distinguish the claim from its opposite.** Evidence consistent with
  both answers is not evidence; it is a coincidence you found agreeable.
  - **An inference is not an observation, and must be labelled.** "The JSON
    shows X, so the build predates Y" is a chain with a premise in it. Say
    which link you verified and which you assumed, every time.
  - **A commit message is a claim, not a fact.** So is a doc page, a code
    comment, a PR description, and anything this file says. On 2026-08-09 a
    session read "#2531 made `flip_achieved` tri-state" in a commit body and
    reported the fix as present; the field was still `pub flip_achieved: bool`
    in `verify.rs`, and the two signals it cited to date a benchmark run were
    both present on *either* side of that commit. The conclusion was
    unfalsifiable by the evidence offered and stated as fact anyway. Read the
    code the claim is about.
  - **"I do not know" is a complete answer** and is always cheaper than the
    alternative. When the evidence runs out, say where it ran out and what
    would settle it. Guessing confidently at a question a human asked because
    they already suspected the answer is the single fastest way to become
    worthless to them.
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
    read the **full** `proof` reason strings; they are truncated everywhere
    else, and the truncated half usually carries the reason.
  - **Run the cheap control before any bisect.** Re-run the failing task alone
    on the *old* SUT. One trial either establishes the regression or ends the
    investigation — and it is how the 11-commit bisect above got cancelled.
  - **A five-task solve rate cannot resolve anything smaller than a
    catastrophe.** Treat it as a guardrail; the mechanism metrics are the
    measurement. Comparisons need repeats per task, and any two runs differing
    by more than one commit are confounded and must be reported as such.
- **Write the thing; do not announce that you are writing it.** Content-free
  prose is a defect in this repository, in exactly the way an unexplained
  `#[allow]` is, and it is fixed the moment you see it — not filed, not
  deferred, not left because the file you opened was about something else.
  **If you touch a file, you own its prose and its comments.** A doc comment,
  a module header, a shell script's banner and a `.md` page are all text a
  human reads, and they are held to one bar.
  - **The test is deletion.** Cut the clause. If the reader lost nothing, it
    was carrying nothing, and it goes. `Two things stated rather than
    hidden:` is the specimen this rule is named for — strip it and every
    sentence after it still says what it said.
  - **What goes:** announcing a list instead of writing it (`Two things
    follow`, `Both halves matter`, `Three reasons to know`); telling the
    reader which item to care about instead of putting it first (`the part
    that matters`, `and the second is the hard one`); prose about the prose
    (`stated rather than hidden`, `worth naming`); the `X, not Y` tail where
    `Y` is a foil nobody proposed (`, not decoration`); and tired metaphor
    standing in for the plain word (`load-bearing` → *required*, `belt and
    braces` → *checked twice*).
  - **Enforced by `make prose`** (`scripts/check-prose.py`), a down-only
    ratchet over `scripts/prose-baseline.txt`. `make prose-report` names
    every remaining line and what to write instead. `make prose-update`
    refuses to raise a count or add a file, so a red gate is cleared by
    deleting the prose. The baseline records debt older than the guard, it
    is meant to reach empty, and adding a line to it is the expedient this
    file forbids. A backticked span or a fenced block is exempt: naming a
    banned construction in order to ban it is a citation.
- **AGENTS.md is the orientation document.** Commands, architectural
  invariants, workspace routing, testing approach, and gotchas all live there
  (imported above). When this file and AGENTS.md disagree, AGENTS.md wins —
  and the disagreement is itself a bug: fix it in the same PR you noticed it.
- **The built-in tool surface is a working surface plus a coordination
  surface, and nothing else.** The working half is one shell (`bash`), the
  file CRUD quartet (`read_file` / `write_file` / `edit_file` /
  `delete_file`), and one unified `search`; the coordination half is
  sub-agent delegation, the task board, the scratch state plane, the
  environment probe, and one question back to whoever is driving
  (`ask_question`, #4212). Declared once in
  `crates/stella-tools/src/catalog.rs` — that table is the count, and the
  number is deliberately not written here, because the last three times it
  was, it drifted the moment a tool landed.

  The rule is **one tool per job, not a fixed total**: one shell rather than a
  family of structured runners, one search rather than a `grep`/`glob`/
  `graph_query` triple the model has to choose between. A capability that is
  not something an agent fundamentally cannot work without ships as an MCP
  server or a workspace custom tool, never as a new built-in. The
  single-purpose rule (AGENTS.md invariant #9) governs every tool regardless
  of origin: a parameter may scope an operation, never select one — a mode
  flag like `update_task(delete=true)` is two tools and gets split.

  The twelve-tool surface this replaces (#3244) cut the working half
  entirely. It was restored because an agent that cannot read a file, search,
  or run a command is not an agent; the lesson kept is the single-purpose
  discipline, not the count.
- **Nothing left behind.** Every bug, defect, idea, missing test, piece of
  unwired code, or logical next step you notice and do not fix inside your
  current change MUST be filed as a GitHub issue before you finish — written
  as a handoff a fresh agent could execute without your session's context
  (problem, file paths, repro/verify steps, constraints, definition of done).
  Search for an existing issue first; link, don't duplicate. The full policy
  is AGENTS.md § "Nothing left behind — every finding becomes a fix or a
  GitHub issue". Never end a turn with untracked half-finished work.
- **Every Sourcery ❌ gets a fix or an answer before the PR is mergeable.**
  Sourcery reviews every PR, and when the PR links issues it posts an
  "Assessment against linked issues" table as a `sourcery-ai` comment — one
  row per objective, `✅` for met, `❌` with an explanation for partial or
  missing. After opening a PR, and again after every later push to it, read
  that comment (it lands within a few minutes;
  `gh pr view <n> --json comments --jq '.comments[] | select(.author.login == "sourcery-ai") | .body'`)
  and settle every `❌` row before the session ends:
  - **Fix it** when the objective belongs to the PR — push the commits that
    satisfy it, then re-read the table Sourcery posts for the new head.
  - **Answer it** when it does not belong: deliberately out of scope,
    deferred into a filed issue, or a misreading of the diff. Post a PR
    comment naming the row and the reason, and file the follow-up issue
    where one is owed ("Nothing left behind" above). Sourcery's verdict is
    a claim like any other review comment and can be wrong about your diff —
    the rebuttal still goes on the PR, where the next reviewer finds it.

  A `❌` with neither a fix pushed nor a comment answering it is untracked
  half-finished work, and the PR stays unmergeable until it has one or the
  other.
- **CI builds and tests; this laptop does not.** Never run `make gate`,
  `make check`, `make test`, `cargo build --workspace`, `cargo test
  --workspace`, or clippy over the workspace on the maintainer's machine.
  Push the branch and read the workflow run instead — that is what CI is
  for, and a workspace build here competes with every other session on the
  box. A local compile is allowed only when it is **targeted**: one crate,
  one test filter, for a change you are iterating on right now
  (`cargo test -p stella-core loop_detect`). `cargo fmt --check` and the
  toolchain-free guards (`make guards-fast`) compile nothing and are always
  fine. A PR's evidence is its CI run, so cite the run — a green workspace
  build on this laptop is not evidence, it is a cost.
