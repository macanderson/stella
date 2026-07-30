# Operations — the failure modes, and what fixes them

Every numbered item here cost real time on a real run. Read this before launching; skipping it
costs hours.

## 0. READ THIS FIRST — typing in the prompt kills the run

**Subagents are children of the turn that launched them.** If the user interrupts — or types
anything at all while the workflow is running — the turn ends and *every in-flight agent dies
mid-read*. They had not yet written their findings, so the work is simply gone.

This has destroyed three separate audit runs. On one, all 24 agents died at once, minutes
before any of them reached the write-fixes stage; the run looked healthy right up to the
moment it produced nothing.

It is also the failure mode most likely to recur, because the natural human response to a
silent 90-minute run is to type "status" — which is precisely the thing that kills it.

**Therefore, in this order:**

1. **Tell the user, before launching, in plain words:** the run takes N minutes; typing in the
   prompt will kill it; `/workflows` shows live status without touching the run.
2. **Make the progress narration frequent enough that they never want to type.** Report after
   every phase transition, and at least every few minutes during the long phases. Silence is
   what provokes the interrupt. This is not politeness — it is how the run survives.
3. If it does get killed, recover with `Workflow({scriptPath, resumeFromRunId: 'wf_…'})`;
   agents whose `(prompt, opts)` are unchanged replay from cache. Then salvage per §2.

The first fifteen minutes of a healthy run look identical to a dead one — nothing has
finished yet. Prove liveness with `scripts/progress.py`, which compares `started` against
`result` counts and reports growing transcripts, rather than waiting for a first completion.

## 1. Long-lived agents die; short ones don't

**Symptom.** Recurring `API Error: Connection closed mid-response`, arriving in bursts —
roughly every 20 minutes, taking out 4–5 in-flight agents at once. Each dies, restarts from
zero, and is still running when the next burst lands, so it never converges. The run looks
alive (transcripts growing) while producing almost nothing.

**It is not correlated with unit size the way you would guess.** On the run where this was
diagnosed, a 4k-LOC unit died 6 times while a 19.5k-LOC unit passed on attempt 3. The
correlated variable is **how long the agent has been alive**, and secondarily how long its
final response is — the drop lands during the long terminal `StructuredOutput` call.

**Fix, in order of effect:**

1. **Partition units to ~12–15k LOC** so an agent finishes inside the window.
2. **Cap the write-up in the prompt.** "Report your 12 most significant findings, not every
   nit. Each `issue` 2–3 sentences. Call `StructuredOutput` as soon as analysis is done; do
   not narrate findings in prose first." Depth of *reading* is unaffected — only the length of
   the terminal response, which is what actually breaks.
3. Give every partition **disjoint, explicitly enumerated** file ownership.

**Detect it early.** A few minutes in, run `scripts/progress.py <transcript-dir>`. It counts
distinct `started` vs `result` keys in `journal.jsonl` and reports the retry count directly. If
starts far exceed results, agents are being retried.

## 2. Salvage before you restart

Do not throw away finished agents. `journal.jsonl` has one line per completed agent:

```python
# NOTE: the key is "result", not "value"
[json.loads(l)["result"] for l in open(f"{d}/journal.jsonl")
 if json.loads(l).get("type") == "result"]
```

Compact those into the next script as an embedded constant. **Splice it in with a script, not
by writing it out yourself** — the payload is often 80–300KB, and pasting it through the
conversation burns enormous context for zero benefit:

```python
tpl = open("workflow.template.js").read()
open("workflow.js","w").write(tpl.replace("/*__SALVAGE__*/ null", open("salvaged.json").read()))
```

Then `node --check workflow.js` before launching.

`resumeFromRunId` replays cached agents whose `(prompt, opts)` are unchanged — but if you are
restructuring the prompts (the usual reason to restart), most will miss. Embedding salvaged
results is the reliable path.

## 3. A cached build reports zero warnings

This is the single most common way an audit reports a green gate that isn't. Every incremental
toolchain **suppresses diagnostics it already emitted on a previous run**, so a clean result on
a warm cache is not evidence of anything.

| Toolchain | The trap | Force a real run |
|---|---|---|
| `cargo clippy` | silent on a cached build | `touch` every unit's root source file first |
| `cargo doc` | additionally bails early | re-run until every crate documents |
| `tsc --incremental` / project refs | `.tsbuildinfo` skips unchanged files | delete `.tsbuildinfo` / `--force` |
| `eslint` | `--cache` reuses prior verdicts | drop `--cache`, or delete the cache file |
| `ruff` / `mypy` | `.ruff_cache` / `.mypy_cache` | `--no-cache` / `--cache-dir=$(mktemp -d)` |
| `go build`/`vet` | build cache | `go clean -cache` is heavy; `-count=1` for tests |
| `pytest` | `-p no:cacheprovider` for a clean collect | plus `--cache-clear` |
| `gradle`/`maven` | build cache + daemon | `--rerun-tasks` / `-o` off |

`scripts/detect.py` emits the forced form of each command it finds. Use those, not the
convenient ones.

**Related:** if `CLICOLOR_FORCE` is set (it is, in this user's zshrc), subprocess output carries
ANSI escapes, so `rg '^error'` silently misses matches. **Check gates by exit code**, not by
grepping coloured output.

## 4. Baseline on a pristine tree, before any agent runs

Repos land red far more often than anyone expects. If you only measure after the audit, you
will charge pre-existing breakage to the audit — or, worse, credit the audit for a green tree
that was already green.

```sh
git worktree add <tmp>/baseline-check <HEAD-sha> --detach
```

Measure the full gate there. Pass the measured numbers into the Verify phase's prompt so its
agent can classify each failure as regression vs pre-existing instead of guessing. Remove the
worktree afterwards (`git worktree remove --force`) so it does not pollute `git worktree list`.

**If the project is not a git repository**, there is no pristine baseline and no way to revert a
bad fix. Run with `--no-fix` (audit only, zero write authority) and say so in the report. Do not
grant fix authority to a tree you cannot roll back.

## 5. Fix authority is only as safe as the language's gate

In Rust, an agent's bad edit usually fails to compile and the Verify phase catches it. **In a
dynamically typed tree, a bad edit ships silently** — there is no compiler, and the tests may
not cover the line. Fix authority is therefore not a constant; it is a function of what gate
exists behind it:

| Tree has | Fix authority |
|---|---|
| Static types + a compile gate (Rust, Go, typed TS with `strict`, Java) | full: the documented safe-fix list |
| Tests with real coverage of the file, no type gate | fixes only inside files the tests actually exercise |
| Neither | **comments, docs, dead-code removal and error-message text only** |

`scripts/detect.py` decides this per-tree and writes it into every unit prompt. Do not override
it upward by hand.

## 6. Keep discovery blind to prior scores

Feed the previous scorecard **only** to the synthesis phase. If unit, lens, or panel agents see
the old numbers they anchor, and the run degenerates into confirming last time's result.

Where a prompt must mention a prior finding (Phase 0 has to), frame it as *a lead to check, not
a fact* — some prior findings were fixed, some fixed shallowly, and some fixes introduced new
defects. Say that explicitly in the prompt.

## 7. Recompute every number yourself

Never report an agent's arithmetic. `scripts/score.py` recomputes:

- the weighted overall from the panel's own per-dimension scores;
- the **median and spread** across the three panelists (a panelist that returns a mean instead
  of a median, or silently drops a dimension, is common);
- the **prior round's** published number from its recorded scores — if that does not reproduce,
  the weights drifted and the whole comparison is invalid.

Treat any disagreement as an error to investigate, not a rounding difference.

## 8. Every agent must set `model` explicitly

Omitting `opts.model` inherits the session model. One omission silently collapses part of the
panel to a single model, and every cross-model guarantee in the report becomes false while
still being printed. After the run, always:

```sh
python3 scripts/progress.py <transcript-dir> --provenance
```

It reads the **actual** model out of each agent transcript and cross-checks it against the slot
the prompt claimed. Do not publish the report until this is clean.

## 9. Forbid build tooling outside the Verify phase

Concurrent agents contend on the build lock (`cargo`'s target lock, a single `node_modules`, a
Gradle daemon, a shared venv) and will serialise the entire run behind one build. Worse, in
JS/Python monorepos an agent that runs an install can mutate the lockfile underneath every
other agent. Unit, lens, refute, panel and synthesis agents are told: **static reading only, no
build, no test, no install.** Only Verify runs tooling, and it runs alone.

## 10. Monitors that fire too often are auto-stopped

One notification per completed agent is 200+ messages on a `max`-depth run. Watch phase
transitions only, or bucket by `total / 5`. The reliable progress mechanism is
`scripts/progress.py`, which is a pull, not a push.

## 11. The HTML report must be genuinely self-contained

No CDN, no web font, no remote image, no `fetch`. The report gets opened months later, from a
different machine, possibly offline, and possibly attached to an email. Inline everything; if a
chart needs a library, it is the wrong chart. `scripts/render_report.py` emits hand-rolled
inline SVG for exactly this reason, and its self-containment is asserted by its own test.

## 12. Write the report before you are sure the run is finished

A run that dies in the final phase with everything else complete should still produce a report.
`render_report.py` accepts a partial result and marks the missing sections `incomplete` rather
than failing. Render early, render often; an audit with no artifact is an audit that did not
happen.

## 13. Agent count is a wave count, not just a size decision

Workflow concurrency is capped at `min(16, cores − 2)` — on this machine, 8. Forty agents is
five sequential waves, not forty things happening at once. Two consequences:

- **Estimate wall-clock as `ceil(agents / cap) × per-agent-minutes`**, per phase. Tell the user
  that number before launching, not a vague "about an hour".
- A wave that starts inside a connection-drop burst (§1) **dies as a unit** — which is why the
  drops appear to take out 4–5 agents at once. More, smaller units means a dead wave costs less.

## 14. The workflow script cannot touch the filesystem

Scripts have no filesystem access — no glob, no stat, no read. `Date.now()`, `Math.random()`
and argless `new Date()` throw as well, because they would break resume.

Everything the harness needs about the tree must therefore be scouted by the parent session and
**spliced into the script text** (or passed via `args`): the unit list, the file inventory, the
gate commands, the measured baseline, the prior scorecard, the timestamp. That division of
labour is why `detect.py` and `partition.py` are separate steps rather than script logic.

## 15. Compute every delta at full precision, before rounding

A published overall of 79 → 80 looks like +1. The real movement was **+0.86** (78.92 → 79.79).
Subtracting rounded values inflates or erases movement and has already produced one wrong
number in a shipped report.

Round only for display, and only at the last moment. `score.py` carries two decimals
internally and prints both the rounded headline and the precise delta.

## 16. Pin the commit, and re-check it before publishing

Two distinct incidents: an audit **scored an unmerged branch while reporting on `main`**, and a
long run finished against a `main` that had since broken (new commits landed mid-run with a
fresh compile error).

State the SHA you actually scored, in the report and in the history record. Before publishing,
re-check that `origin/main` has not moved under you, and say so if it has. A score against an
unstated tree is not comparable to anything.

## 17. Diagnose your own tooling before diagnosing a dead agent

A journal parser reading the wrong key returns `null` for every completed agent, which is
indistinguishable from mass agent death and has already triggered one full false-alarm
investigation. Two specific traps:

- the result key is **`result`**, not `value`;
- there is **no `agent_end` event type** — a Monitor watching for one silently reports nothing
  forever.

Validate the parser against a known-good journal before trusting what it says about the run.

## 18. Monitors leak; stop them explicitly

A Monitor armed for a run outlives the run. Stop each one with `TaskStop` when the phase it was
watching completes. Three prior runs left monitors armed after finishing.

## 19. Splice the report data disk-to-disk, and escape the closing tag

The findings payload is 80–300KB of JSON. **Never route it through the conversation** — read
the template and the data from disk, write the joined file to disk:

```python
tpl  = open('report_template.html').read()
data = json.load(open('report.json'))
js   = json.dumps(data, ensure_ascii=False).replace('</', r'<\/')   # load-bearing
out  = tpl.replace('__DATA__', js)
assert '__DATA__' not in out, 'placeholder survived the splice'
```

The `</` → `<\/` escape is **load-bearing, not defensive**: any finding whose text contains a
closing tag (`</div>`, `</script>`, a snippet of HTML in a code sample) would otherwise
terminate the `<script>` block and produce a blank page. `render_report.py` does this and
asserts it; its test suite includes a finding containing `</script>` for exactly this reason.

Then **validate the generated file rather than eyeballing it**: extract the inline script and
run `node --check` on it, assert every `id="…"` mount point the renderer targets is present,
and open it. A report that renders blank is worse than no report.

## 20. A cheaper model ignores guidance a frontier model would honour

Observed directly: a constraint placed fourth in a list of operating rules was simply shrugged
off by a worker-tier model that a frontier model had followed. If any agent in this harness
runs below the frontier tier, its constraints must be **rule #1, imperative, and phrased as a
stop condition** ("if you are about to X and have not done Y, STOP"), not a preference buried
in a paragraph.

This is also the reason `haiku` never judges a finding or scores a dimension here — see
`model-panel.md`.
