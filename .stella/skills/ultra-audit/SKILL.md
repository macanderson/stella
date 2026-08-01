---
name: ultra-audit
description: Run the most exhaustive engineering audit available on any codebase in any language — 100% file coverage, fix what is safely fixable, cross-model refutation where no model grades its own work, a blind three-model scoring panel, and a self-contained HTML report scored against an absolute bar where 100 is the Rust project itself. Use when the user asks to audit, deeply review, eval, score, or "find and fix what's broken" in a workspace, or wants an engineering scorecard or a repeatable quality number that is comparable across rounds.
---

# ultra-audit

An exhaustive, scored, self-fixing engineering audit that works in any language and produces
one self-contained HTML file.

Run it with `/ultra-audit` (audits the current repo), `/ultra-audit <path>`, or
`/ultra-audit <path> --depth max`.

Four things distinguish it from `/code-review`, which is what to use for a quick check:

| | |
|---|---|
| **Complete** | Every source file is owned by exactly one auditing agent, and coverage is *computed*, not claimed. |
| **Cross-model** | No model grades its own work. Fixes, findings and scores are each judged by a different frontier model than produced them. |
| **Calibrated** | Three models score the same evidence blind; the reported number is the median and the spread is published as a confidence signal. |
| **Absolute** | 100 is reserved for the Rust project itself and is never awarded. The bar does not move between rounds, so the number is comparable over time. |

---

## Before you launch — say these three things to the user

**1. Typing in the prompt will kill the run.** Subagents are children of the turn that launched
them. If the user types anything while the workflow runs, the turn ends and every in-flight
agent dies mid-read, having written nothing. This has destroyed three prior audit runs. Tell
them plainly, and tell them `/workflows` shows live status without touching the run.

**2. The cost, in real numbers.** `build_workflow.py` prints an agent count, a wave count and a
wall-clock range. Quote them. Roughly: `standard` ~120 agents / 1.5–3h, `deep` (default) ~180
agents / 2.5–4h, `max` ~240 agents / 3–5h on a 300k-line repo. This is not a quick check.

**3. What it will change.** At `full` fix authority agents edit code. Work in a worktree, never
the user's checkout.

Then, **while it runs, narrate every phase transition.** Silence is what provokes the interrupt
that kills the run, so progress reporting is a survival mechanism, not a courtesy. `python3
scripts/progress.py` is a safe pull-based status check — run it between phases and report a
one-line summary each time.

## Before anything else — read these

1. **`reference/rubric.md`** — the 17 dimensions, the exact weights, and what 100 means. Never
   invent weights; a re-scored run with different weights is worthless.
2. **`reference/model-panel.md`** — which model does what, and the five rules that make the
   cross-model claims true rather than decorative.
3. **`reference/operations.md`** — the failure modes that have actually broken these runs.
   Section 0 is the one that matters most. Skipping this file costs hours.

## The procedure

### Step 0 — isolate, detect, and baseline

Work in a git worktree (`EnterWorktree`), never the user's checkout.

```sh
python3 scripts/detect.py <root> --out gate.json
```

Read its output and **sanity-check the gate commands** — a wrong gate produces a confidently
wrong baseline. Note three things it decides:

- **fix authority** (`full` / `tested-files-only` / `text-only` / `none`) — derived from whether
  a bad edit could actually be caught. Do not raise it by hand.
- **the project's own gate target** (`make gate`, `just ci`) — prefer it if one exists.
- **cache traps** — the forced form of each command. A cached lint run reports zero warnings
  whether or not there are any.

Then measure the baseline on a **pristine, separate** worktree of the same commit, *before* any
agent edits anything:

```sh
git worktree add <tmp>/baseline-check <HEAD-sha> --detach
```

Run the gate there, in its forced form, checking **exit codes** rather than grepping output
(this shell forces ANSI colour, so pattern-matching silently misses matches). Save the result to
`baseline.txt`. This is the only thing that lets you tell "the audit broke it" from "it shipped
broken" — repos land red far more often than anyone expects. Remove the worktree afterwards.

### Step 1 — partition, with a coverage proof

```sh
python3 scripts/partition.py gate.json --out units.json
```

It splits on directory seams to ~13k LOC per agent and **refuses to proceed** unless ownership
is disjoint and complete. Read the coverage proof. Then **edit `units.json` to add a `note` per
unit** saying why that unit matters and what to scrutinise — per-unit notes measurably improved
finding quality on prior runs.

If the tree is very large, `--max-agents N` caps the fan-out and prints exactly which units are
excluded and what coverage drops to. If you use it, that gap must appear in the report.

### Step 2 — build and launch

```sh
python3 scripts/build_workflow.py --gate gate.json --units units.json \
        --baseline baseline.txt --depth deep --out audit.js
```

It refuses to emit a script that fails validation. Launch with the `Workflow` tool
(`scriptPath: audit.js`, run in background), then tell the user the estimate it printed.

Ten phases run: regression check → unit audits (pipelined straight into cross-model fix review)
→ cross-cutting lenses → fresh sweep → verify → cross-model refutation → blind scoring panel →
second opinion → synthesis.

### Step 3 — report progress while it runs

```sh
python3 scripts/progress.py                 # newest run; add --watch 90 to follow
```

It reports launched/finished/retried, per-phase bars, API-error clustering, and liveness. **The
first fifteen minutes look identical to a dead run** — nothing has finished yet — so use the
liveness line rather than waiting for a first result. If starts far exceed finishes, agents are
being retried; see `operations.md` §1 and §2 before restarting anything.

### Step 4 — verify the numbers and the model mix yourself

```sh
python3 scripts/score.py result.json --json scoring.json
python3 scripts/progress.py <transcript-dir> --provenance --json provenance.json
```

`score.py` recomputes the weighted overall, the panel median and spread from raw votes, and the
**prior round's published number** — if that last one does not reproduce, the weights drifted
and the comparison is invalid. It exits non-zero on any unreconciled problem.

`--provenance` reads the model **actually recorded in each agent transcript** and checks it
against the slot the prompt claimed. Do not publish a cross-model audit you have not confirmed
was cross-model.

Then re-run the gate yourself on the post-fix tree and compare against the Step 0 baseline. Do
not report a green gate you did not personally observe.

### Step 5 — render the report

```sh
python3 scripts/render_report.py result.json --scoring scoring.json \
        --baseline baseline.txt --provenance provenance.json
```

Writes `<root>/.ultra-audit/report-<date>-<sha>.html` — one self-contained file, light and dark,
printable — and validates it: the inline script must parse, there must be exactly one
`</script>`, no remote asset of any kind, every mount point present. Render even if the run died
late; a partial result renders and marks what is missing.

Open it and look at it before telling the user it is done.

### Step 6 — record and ship

```sh
python3 scripts/score.py result.json --record --date <YYYY-MM-DD> --report <path>
```

History lives in `~/.claude/audits/<repo>.json`, outside any skill directory, so the series
survives skill edits. It is what makes the *next* round's "did this actually land?" phase
possible.

Then commit, push, and open a **draft** PR. Never push to main. Lead the PR body with the score
movement at full precision, the did-it-land tally, and the gate table.

## Rules that are easy to get wrong

- **The bar is absolute**, not relative to the last run. A dimension rises only if the tree is
  genuinely better against the anchors — never because effort was spent. Dimensions are allowed
  to fall, and saying so is the point of the exercise.
- **Report deltas at full precision.** 79 → 80 looks like +1; the real movement was +0.86.
  Round only for the headline.
- **A fix that closes the reported instance but leaves the defect class open is
  `partially_fixed`** and earns no root-cause credit. This distinction is the single most
  valuable output of the run.
- **Only findings that survived cross-model refutation enter the risk register.** Findings that
  exceeded the depth cap are *unverified*, not confirmed, and must be labelled so.
- **State the commit you actually scored**, and re-check that the default branch has not moved
  under you before publishing. One prior audit scored an unmerged branch while reporting on
  `main`.
- **Never claim more than the panel supports.** These three models share training lineage;
  agreement between them is weaker evidence than three independent human reviewers. What the
  panel reliably catches is the confident, plausible, wrong finding a single model would
  otherwise defend into the report. Claim that, and not more.

## Tests

```sh
python3 tests/check_template.py     # harness invariants: valid JS, every agent sets a model,
                                    # every prompt carries the provenance header
python3 tests/make_fixture.py --out /tmp/r.json   # a realistic result, with hostile payloads
```

`node --check <file>.js` is **not** a usable syntax gate for the harness — the generated script
mixes an ESM `export` with a top-level `return`, and that combination exits 0 even when the file
is unbalanced. `check_template.py` wraps the body and checks it as `.mjs`, which does catch it.
