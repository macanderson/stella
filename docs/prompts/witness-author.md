---
id: prompt-witness-author
title: "witness_author — the effective prompt"
status: living
---

# `witness_author`

Writing the witness test that arms the flip oracle. This is the role that
makes "verified done, not claimed done" mechanical: it authors a test that must
**fail on the current code** and **pass once the goal is met**, and that
fail→pass flip is what credits the work.

The stage is **demand-driven and runs after execution** — once the warrant has
read the executed diff and found something worth proving. The canonical stage
order is triage → recall → research → plan → scope → **execute → witness** →
verify → verdict (`stage_rank`, `crates/stella-pipeline/src/replay.rs`).

Reachable only when no `--test-command` is configured; a configured command
makes the pipeline its own oracle and authors nothing.

| | |
|---|---|
| Call role | `ModelCallRole::WitnessAuthor` (`"witness_author"`) |
| Dispatch | **engine turn** — a real tool loop, not a raw completion |
| Tools | the witness set only: `read_file`, `glob` (names only), `create_witness_test` |
| System prompt | `WITNESS_SYSTEM_PROMPT`, `crates/stella-pipeline/src/witness.rs` |
| User message | `witness_prompt`, same file |
| Sent from | `crates/stella-pipeline/src/pipeline/witness_stage.rs` |
| Output cap | `None` — inherits the engine base |
| Override | `agents.verifier.prompt` — **not wired** |

The author is resolved from the **verifier's** model, never the worker's
(L-E11, #1795). A worker that authored the test proving its own work would be
grading its own homework.

## Wire shape

```
[ system(WITNESS_SYSTEM_PROMPT)
  user(witness_prompt(goal, frames, structure, available_runners, workspace_root))
  … the author's tool loop … ]
```

This is the one verification role that runs a multi-step tool loop, which is
why moving the fixed block from the user message to the system message mattered
so much (#1786): as a user message it was re-billed uncached on every author
call, every repair call, **and every tool round-trip inside them**. As a system
message the provider prefix is system prompt + conversation, which clears
Anthropic's ~1024-token minimum inside the first round-trip — so from step two
on, the whole fixed block is cached rather than re-billed.

## System message (verbatim)

```text
You are the WITNESS AUTHOR for a coding agent: a precise test author who writes a minimal test that FAILS on the current code and will PASS once the goal you are given is correctly accomplished. The fail→pass flip of your test is what verifies the work. You never modify production code and never fix the problem yourself.

Hard requirements:
- Create ONE NEW test file. Never modify existing files, and never touch production code — the implementation is someone else's job.
- CHOOSE A RUNNER THIS REPOSITORY ALREADY USES. You cannot execute anything in this role, so you cannot discover a missing toolchain — and a command whose runner is not installed does not fail the test, it produces NO observation at all, which discards your witness and leaves the work unverified. Pick the ecosystem the repository listing evidences (a manifest such as `Cargo.toml`, `package.json`, `pyproject.toml`/`setup.py`, `go.mod`, or a `*.csproj`, plus existing tests written for it) and match the conventions of the tests already there. When the workspace has no language toolchain and `sh` is the only available runner, the witness is a POSIX shell script (see the available-runners section); if not even a shell is available, say so in prose and emit no TEST_COMMAND line rather than guessing.
- Put it where that runner collects it. Rust integration tests MUST live in `tests/` (cargo cannot run a test file under `src/`); Python, Vitest, Go and .NET may use their filename conventions.
- The test must fail NOW for the RIGHT reason (it exercises the missing/broken behavior), not because of a typo, a missing import, or a harness error.
- NAME EVERY FILE IN THE PROJECT BY A PATH RELATIVE TO THE WORKING DIRECTORY. Your test runs inside an isolated COPY of the project tree, rooted at a directory that is neither the project's own root nor the one you are reading now — so an absolute path into the project reads a tree the work under test never touches, fails identically for every change, and is REFUSED at creation. If the goal names a file absolutely, strip the project root and assert on the remainder. Absolute paths to the MACHINE (`/bin/sh`, `/usr/bin/openssl`) are fine; those are not the tree under test.
- ASSERT on a value the goal decides. A test with no assertions, one comparing constants (`assert_eq!(2, 2)`), one comparing a value to itself, or a bare `#[should_panic]` / `raises(Exception)` is REFUSED at creation — each of those flips green without constraining the change. Name the expected panic if a panic is what you mean to prove.
- Explore with `read_file` and `glob` (names only) to find the test directories and conventions; create with `create_witness_test`. No general write, edit, process, network, or external action is available in this role.
- The command must directly name this artifact and an exact test: for Rust use `cargo test --test <file-stem> <selector> -- --exact`; for Python/Vitest name the file path; for Go/.NET include an exact test filter. Never run a whole suite.
- End your reply with exactly one line:
TEST_COMMAND: <the direct, artifact-specific test command>
```

## User message (template)

Sections in this order, each conditional:

```text
## Test runners available in this workspace
{runners, comma-joined}
Each of these answered a version probe here, and your TEST_COMMAND must use one of them — any other runner discards your witness. The probe is program-level only: it cannot vouch for a subcommand or a package the program would have to fetch (`cargo nextest`, `npx vitest`), so prefer an invocation the repository listing below evidences the project already using.

This workspace has NO language test framework — `sh` is the only runner. Author the witness as ONE POSIX shell script that asserts observable state the goal decides (a served endpoint, a file or certificate the change must create, a git fact, a program's output), exiting non-zero NOW and zero once the goal is met. Name it so its intent is legible (e.g. `witness_check.sh`) and end with `TEST_COMMAND: sh <path-to-script>`. Do NOT write a pytest, cargo, or other framework test here: nothing exists to collect it.

## Repository structure
{repo_structure}

## Recalled context
- [{citation_label}] {content}

## Where your test runs — paths are RELATIVE
This project's own root is `{root}`, and the goal below may name files under it absolutely. Your test is NOT run there: it is run inside an isolated copy of this tree, with the working directory at that copy's root, and the copy the flip is verified in is a different directory again. So translate every project path out of `{root}` and assert on the remainder — the goal's `{root}/ssl/server.crt` is `ssl/server.crt` to you. An absolute path into the project is refused when you create the file; absolute paths to the machine (`/bin/sh`, `/usr/bin/openssl`) are fine.

## Goal
{goal}
```

The `sh`-only paragraph appears only when **every** available runner is `sh`
(#2064): a model asked for "a test" in a workspace with no framework tends to
write a pytest file anyway, into a tree with nothing to collect it. Saying the
shape explicitly turns a guess into a constraint derived from the probe.

## Prose over an enforced contract

Almost every promise in the system prompt is mechanically enforced somewhere
else, so the prose is guidance over a contract, not the contract itself:

| Promise | Enforced by |
|---|---|
| runner must be in the probed set | the pipeline discards a witness naming any other |
| relative paths only | `frame::screen_witness_frame` at the create boundary |
| must assert on something real | `create_witness_test` refuses tautologies at creation |
| must fail now | the pipeline's fail-check; a pass triggers [witness-repair.md](witness-repair.md) |
| `TEST_COMMAND:` line | `parse_witness_command` |
| worker must not touch it | tamper exclusion in the flip oracle |

`parse_witness_command` takes the **last** `TEST_COMMAND:` line,
case-insensitively, stripped of whitespace and backticks. It scans with
`get(..n)` rather than a bare byte slice, because a reply line opening with
multi-byte characters would put the marker's byte index mid-character and panic
— taking the whole run down over a witness author that explained itself in
Japanese.

## What is deliberately absent

- **The authoring snapshot's path.** The prompt names the *project* root, not
  the snapshot directory, which carries a pid and a sequence number. Naming the
  snapshot would put per-run noise into a prompt that wants to cache
  (invariant 7), and it is not the root the goal's paths are written against
  anyway (#2130).
- **A project-test-command anchor.** Structurally impossible: the stage is only
  reachable when `config.test_command` is `None`. The probed runner set is the
  toolchain fact the author gets instead (#1539).
- **The worker's transcript.** Split context, L-E6, same as the planner.

## Degradation

The stage's whole contract is that a witness which cannot be produced must not
cost the run. It degrades — never panics, never aborts — when:

- no supported test runner is available at all;
- no pristine baseline workspace was provided (a caller wiring a surface
  without a workspace — the one stage designed to degrade must not be the one
  that panics, #1789);
- the author produces no parseable `TEST_COMMAND`.

The witness is **scaffolding for one run**: it lives in the candidate workspace
and is discarded with it, so an already-satisfied test is never left behind in
the project's test tree. `stella run --keep-witness` promotes it instead.

## Related

- [witness-repair.md](witness-repair.md) — the bounded retry when it passes now
- [triage.md](triage.md) — `WITNESS: yes|no` gates this stage
- [verdict.md](verdict.md) — runs only when no flip settled the outcome
- `doc:witness-protocol` — the normative spec
