---
id: audit/2026-08-parity-gap-followup
title: "Parity follow-up to the July 2026 audit — August 2026"
status: archived
---

# Parity follow-up to the July 2026 audit — August 2026

Scope: re-check every claim in `2026-07-reference-grade-audit.md` against the
tree at `428173c6`, one month on. Two questions only:

1. Of the six things the audit said Claude Code does better, which are still true?
2. What is left to build to close them?

Method: each claim verified at a named file and line, not from issue state.
Issue state is reported separately because it disagrees with the code in
places — twelve of the audit's thirteen issues are closed, but three of the
weaknesses the score tables named were never given an issue at all, and those
are the ones still open.

---

## 1. The audit's own issues

| Issue | Subject | State |
|---|---|---|
| #909 | No measured evidence vs a competitor | **closed — delivered** |
| #915 | Supply-chain job installs its tools unpinned | closed |
| #921 | Transcript deep-clone per model call | closed |
| #922 | No sub-agent primitive | **closed — shipped** |
| #925 | `token_cost` counts chars, `estimated_input_tokens` bytes | closed |
| #927 | False "sole telemetry-egress exception" claim | closed |
| #928 | Seven CLI surfaces with no reference page | closed (residual: #993) |
| #929 | No task-shaped docs entry point | closed |
| #930 | `stella-serve` has no observability | closed |
| #931 | API exposes turns but not sessions | **closed — shipped** |
| #935 | TUI never positions the terminal cursor | closed |
| #936 | ~2,400 lines of unreachable REPL | closed |
| #910 | Release builds are not reproducible | **open — but done** |

`#910` is the only issue still open, and it appears to be open by oversight
rather than by remaining work. All five points of its own "proposed shape" are
implemented:

- `scripts/repro-build.sh` remaps `$CARGO_HOME` → `/cargo`, the sysroot →
  `/rustc/<hash>`, and the workspace → `/stella`, and refuses to run on
  anything but the pinned toolchain.
- It sets `SOURCE_DATE_EPOCH` from the commit timestamp (`repro-build.sh:169`).
- Both release paths build through it — `release.yml:116` and
  `scripts/release.sh` — and `make repro-wiring` asserts that on every PR.
- `release.yml`'s `verify-reproducible` job rebuilds on a second runner with a
  different `CARGO_HOME`, `RUSTUP_HOME`, `TMPDIR`, checkout path, and
  `rust-src` presence, and `exit 1`s on a SHA mismatch. The `release` job
  declares `needs: [build, verify-reproducible]`, so a non-reproducible build
  cannot publish.
- `install.sh` verifies SHA256SUMS and, optionally, the build-provenance
  attestation.

The issue's stated acceptance — "two independent runners produce byte-identical
release binaries for the same commit, and CI enforces it" — is met. **Action:
verify on the next release and close it.** Dimension 5 (determinism, 78) should
re-score materially higher on the next audit round.

---

## 2. The competitive gap list, re-checked

The audit's "Where it does not" section, item by item.

### Closed since the audit

**Sub-agent primitive — shipped.** The `task` tool is real and model-facing:
`crates/stella-tools/src/subagent.rs:161` declares it, delegating a self-contained
question to a child that sees only the prompt it is handed. Backed by
`crates/stella-core/src/subagent.rs` with carved budgets and forwarded metering.
`stella-parity` records it as `Shipped` on the CLI with the witness
`the_production_tool_stack_forwards_sub_agent_spend`.

**Sessions over HTTP — shipped.** `POST /v1/sessions`, `GET|DELETE
/v1/sessions/{id}`, `POST /v1/sessions/{id}/turns`, and
`/v1/sessions/{id}/checkpoint` are all routed
(`crates/stella-serve/src/observe/event.rs:123-132`), with server-owned history on a
byte-stable prefix — so an API consumer now gets the same prompt-cache
discount the CLI gets. Witness:
`a_session_threads_history_across_turns_on_a_stable_prefix`.

**Measurement — delivered.** `bench/evidence/tb21-hh10-20260731/` holds a
preregistration, a run manifest, per-trial records, and — the part that
actually answers the audit — a `comparator/` arm with its own `score.json`
and `trials.jsonl`. The claim "it has never been run" is no longer true.

### Still open, verified in the tree

**Grep has no context lines and no output modes.** Untouched. The schema at
`crates/stella-tools/src/grep.rs:187-194` accepts exactly three fields — `pattern`,
`path`, `glob` — and the tool returns `file:line:text` and nothing else. There
is no `-A/-B/-C`, no `files_with_matches` or `count` mode, no case-insensitivity
flag, no file-type filter, no multiline. Every hit still costs a second turn to
read around, which is a per-search context tax on every task the agent runs.

This is the highest-leverage item on the list. The tool already shells to
ripgrep and already builds an argv — context and output modes are additional
`rg` flags plus schema fields, not new machinery.

**No `@`-file mention.** `crates/stella-cli/src/attachments.rs` scans prompt text for
path-shaped tokens, but only attaches *media* — images, audio, video, PDF. Text
files are deliberately excluded (`attachments.rs:11-15`: "reading those is what
the agent's own tools are for"). That reasoning is sound as a default and wrong
as an absolute: the user naming a file is a much stronger signal than the agent
guessing, and it saves the read turn.

**No stdin prompts.** `stella run` takes the prompt as a required positional
`String` (`crates/stella-cli/src/cli.rs:221`). Nothing reads a piped body; the only
`stdin` reads in the CLI are the interactive REPL's line loop
(`crates/stella-cli/src/agent.rs:815`) and `stella auth set --stdin`. So
`cat spec.md | stella run` is not expressible, and neither is a heredoc.

Related and worth fixing in the same change: a prompt beginning with `-` is
parsed as a flag.

**No per-invocation tool policy.** `crates/stella-tools/src/policy.rs:4` states the
constraint outright — a tool is on "unless something says otherwise, and there
is exactly one way to say otherwise — a `tools` entry in `settings.json`". The
global flag set (`crates/stella-cli/src/cli.rs:87-192`) carries `--model`, `--budget`,
`--turn-budget`, `--output-format` and friends, and no tool switch. So
"run this one task read-only" means editing settings and editing them back.

The policy model itself is already the right shape for this: precedence is
name → group → `*`, and `ToolPolicy::deny_all_from` composes scopes by union of
denials. A CLI flag would be one more scope folded in at the lowest authority,
not a new mechanism.

**No plan mode — partially closed.** Scope review exists and is genuinely good:
`crates/stella-pipeline/src/scope.rs` gates a plan behind user approval when it crosses
any of three thresholds (more than 5 steps, 8 files, or $1.00 estimated), and
headless runs must opt into the bypass explicitly rather than auto-approving.

What is missing is the *user-invoked* half. Scope review fires on the model's
estimate of blast radius; there is no way for the user to say "plan this, touch
nothing, show me" for a task the estimator scores as small. Note that #1220 is
already open on the estimator's weakness here — blast radius is currently
approximated by `plan.len()`.

**No per-hunk diff approval.** `crates/stella-tui/src/diff.rs` renders diffs; it has no
accept/reject/stage path. Approval is per-plan (scope review) or per-tool-call
(the hook bus), never per-hunk.

---

## 3. Score-table weaknesses with no issue behind them

These were named in the audit's dimension tables but never got an issue number,
and so were never worked. All three verified still open.

**Tool inputs are still hand-destructured (dimension 6, 84).** `grep.rs:201-208`
is representative: `input.get("pattern").and_then(|v| v.as_str())`, and the
`None` arm reports `missing required field`. A caller that passes
`{"pattern": 42}` is told the field is missing. Every tool in the registry
repeats this shape. The fix is one shared destructure helper that distinguishes
absent from wrong-typed, not per-tool edits.

**`stella-serve` has no backpressure (dimension 23, 78).** Still unbounded, and
the code says so at `crates/stella-serve/src/server.rs:648-650`: turn reclamation
bounds the *count* of pinned turns, but "the frame channel itself is unbounded,
so *one* abandoned turn can still buffer arbitrarily much before it settles."
The comment even names the fix — a bounded channel in `Session`.

**Ten tree-sitter grammars compile unconditionally (dimension 15, 86).**
`crates/stella-graph/Cargo.toml:24-33` pulls rust, python, typescript, javascript,
sequel, go, java, c, and php with no feature gates. Every build pays for every
grammar.

### Verified as improved since the audit

**Fuzz/property coverage (dimension 28, 72) — addressed.** The audit named three
hand-enumerated surfaces. All three now have property tests:
`crates/stella-serve/tests/http.rs` (the SSE decoder),
`crates/stella-tools/tests/path_confinement_race.rs` (the path resolver), and
`crates/stella-core/src/loop_detect.rs` (the step loop). Seventeen `proptest!` sites
across the workspace.

**Exit codes (dimension 2, 87) — partially addressed.** `crates/stella-cli/src/main.rs:464-468`
now exits `128 + signal` for an interrupted run, so a wrapping script can tell
"the user stopped this" from "this failed". Every non-signal failure is still a
single `ExitCode::FAILURE`, so the audit's specific complaint stands in reduced
form. Note the interaction recorded elsewhere: a verification verdict of
`VerificationFailed` also exits non-zero, which makes an honest "I could not
verify this" indistinguishable from a crash to any harness reading exit status.

### Verified as unchanged

**Auth is still one static bearer token (dimension 18, 78).** No scopes, no
rotation, no mTLS — `crates/stella-serve/src/http.rs:92-102` parses one
`Authorization: Bearer` header, and `hostguard.rs:23` treats it as the whole
perimeter.

**The OS sandbox still covers `bash` alone (dimension 17, 82).**
`crates/stella-tools/src/sandbox.rs:13-23` — Seatbelt on macOS, bubblewrap on Linux,
selected by `STELLA_BASH_SANDBOX`. No other tool runs confined.

> **Superseded (#1300).** This finding was resolved by removal, not by
> extension: `sandbox.rs` and `STELLA_BASH_SANDBOX` are gone, so nothing runs
> confined in-process and the setting no longer implies otherwise. Isolation
> is the container Stella runs in — `docs/spec/remote-sandboxes.md` §2. The
> file path above is retained as the audit read it and no longer resolves.

---

## 4. What is left to build

Ordered by value per unit of work.

### Tier 1 — small, high leverage

1. **Grep context lines and output modes.** Add `-A/-B/-C`, `output_mode`
   (`content` / `files_with_matches` / `count`), `-i`, and a type filter to
   `crates/stella-tools/src/grep.rs`. The argv builder already exists; these are flags
   and schema fields. Removes a per-search turn tax from every task.
2. **Stdin prompts, and leading-dash prompts.** Read the prompt from stdin when
   it is piped or when the positional is `-`. Fixes `cat x | stella run` and the
   `stella run "--flag-shaped prompt"` failure in one change.
3. **`@`-file mention for text files.** Extend `attachments.rs` to inline
   user-named text files. Keep the existing default for un-named paths — the
   change is that an explicit `@` is a stronger signal than a bare token.
4. **Per-invocation tool policy.** A `--tools`/`--allow`/`--deny` global flag
   folded into `ToolPolicy` as a lowest-authority scope. Makes `--tools '*:off,read_file:on'`
   a read-only run without editing settings.
5. **Close #910.** Not a build task — the work has landed and CI enforces it.
   Confirm on the next tagged release that `verify-reproducible` passes, then
   close. This is the single cheapest point on the whole scorecard.

### Tier 2 — real work, clear shape

6. **User-invoked plan mode.** A flag and a deck toggle that forces the scope
   gate on regardless of estimate, with edits refused until approval. The gate,
   the plan card, and the `ApprovalGate` port all already exist — this is a
   trigger path, not a new subsystem. Pairs with #1220.
7. **Bounded frame channel in `stella-serve`.** The fix the code comment
   already names.
8. **Shared tool-input destructure helper.** Absent vs wrong-typed, once,
   applied across the registry.
9. **Feature-gate the tree-sitter grammars.**

### Tier 3 — larger

10. **Per-hunk diff approval** in the TUI diff view.
11. **API-surface parity**, which `stella-parity` already enumerates as nine
    `Deferred` rows: sub-agents over HTTP, the goal loop, the pipeline's
    verification ladder and its approval gate (#932), engine-config knobs
    beyond `max_steps`, lifecycle hooks, calibration, soft stop, goal-met halt,
    and checkpoint read-back.
12. **Auth beyond one bearer token** — scopes and rotation.

---

## 5. The one structural gap

Every numbered issue from the July audit was worked — twelve closed, and the
thirteenth (#910) finished but left open. Every *unnumbered* weakness in the
same document was not, and neither was any of the five competitive gaps, none
of which has a tracking issue today.

The correlation is exact, and it is not about difficulty: bounding a channel
and adding `-C` to a grep schema are both smaller than reproducible builds. It
is about whether the finding got a number.

`stella-parity` is the repository's own answer to this failure mode: a
capability that ships on one surface and not the other cannot stay silent,
because an undeclared row fails `cargo test --workspace` in the PR that
introduced it. That instrument is pointed inward, at CLI-vs-API drift. Nothing
points it at the competitive surface, which is why "grep has no context lines"
survived a month of otherwise diligent issue-closing.

The cheapest durable fix is to file the five gaps as issues so they are visible
at all. The better one is a second matrix — same three instruments — whose rows
are competitor-facing capabilities rather than internal surfaces.
