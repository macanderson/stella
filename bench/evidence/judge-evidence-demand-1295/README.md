# #1295 — asking for corroboration when only the judge approved

**Decision: the ask is switched ON, gated on a tracked command existing.**
The gate is the finding. The half of the question that a benchmark answers —
*does it convert near-misses into passes?* — is **not answered here**, because
no paid model call was possible in the environment this was prepared in. What
follows separates what was measured from what was not, and says which is which
at every step.

---

## 1. What the issue asked, and what the code actually keys on

> On a benchmark run with the git baseline on, count how often "only the judge
> approved this" actually happens. If it is now a minority of turns, switch the
> behaviour on […]. If it is still most turns, leave it off and write down the
> new number.

The premise is that #1225 — a git repository in every task folder before Stella
starts — should have made the condition rare. It does not, and the reason is
visible in the predicate rather than in any run:

```rust
// stella-pipeline/src/verify.rs
pub fn judge_pass_stands_alone(&self) -> bool {
    !self.flip_achieved && self.touched_tests_passed != Some(true)
}
```

Both terms are observations of **the tracked command**. `#1225` turns on the
*diff* and *file-touch* channels — and `judge_pass_stands_alone` deliberately
excludes both, because a readable diff proves the tree *changed* while a pass
claims the change is *right*. So a git baseline cannot move this rate at all.
What can is whatever resolves a tracked command:
`PipelineConfig::test_command`, or an authored witness (which needs #1007's
independent author, and candidate isolation, which needs the git baseline).

This flips the shape of the original measurement. "The condition held on most
turns" was not a property of Terminal-Bench tasks being hard to verify. On a
run with no tracked command it holds on **every** judge pass, unconditionally,
and — the part that decides everything — **no worker could have cleared it on
any turn**, because the only two facts that clear it are observations of a
command that does not exist. The ask bought a turn everywhere and evidence
nowhere.

Hence the gate: raise the ask only when `Pipeline::effective_test_command`
resolved. Pinned by `no_tracked_command_means_no_ask_at_all` in
`stella-pipeline/src/pipeline/tests/judge_evidence_demand.rs`.

## 2. What was measured

### 2.1 The published 89-trial baseline ran entirely on the arm where the ask is unreachable

`bench/evidence/tb21-hh10-20260731/trials.jsonl`, read directly:

| | |
|---|---|
| trials | 89 |
| `assurance_arm` = `witness-off` | **89 / 89** |
| distinct `engine_posture_sha256` | 1 |

`witness-off` is the control arm (`posture.py`): one model for every role, so
no author independent of the worker exists and the authored-witness rung
**cannot fire on any task**. With no `--test-command` either, that run resolved
no tracked command on any of 89 trials. Every judge pass in it satisfied
`judge_pass_stands_alone` by construction — which is exactly the "most turns"
the issue reports, and it is a property of the posture, not of the workload.

### 2.2 The event corpus cannot answer the rate question, and says so

34 trial streams under `docs/design/stella-bench-handoff/bundle/rig-runs/`
yield 14 `judge_verdict` events (21 of 34 trials emitted none at all — they
died before verification). Of those 14: 12 passes, **0 deterministic**.

None carries a `ladder` snapshot — they predate #865/#1043 — so
`stands_alone` and `tracked_command` are *not recorded* on any of them.
`analyze.py` reports that as unscorable rather than as zero, which matters:
"0% standalone" and "the field did not exist yet" are the same number and
opposite claims, and the flattering one is the wrong one.

What the pre-ladder streams *can* say, they say in prose: **10 of 14** verdict
summaries state `flip oracle not armed (no test command)`, and the same 10 are
on the `Unverifiable` rung — every evidence channel blind. Consistent with
§2.1, and independent of it.

Reproduce:

```bash
for j in docs/design/stella-bench-handoff/bundle/rig-runs/jobs/*/; do
  python3 analyze.py extract "$j" --arm "$(basename "$j")" -o /tmp/$(basename "$j").jsonl
done
python3 analyze.py report /tmp/*.jsonl
```

### 2.3 #1225's git baseline is skipped on most task images, because they have no git

Sampled by running `command -v git` in the task image of 12 of the 21
Terminal-Bench 2.1 tasks that could be pulled here:

| ships `git` | no `git` |
|---|---|
| `fix-ocaml-gc`, `modernize-scientific-stack` | `adaptive-rejection-sampler`, `circuit-fibsqrt`, `dna-assembly`, `nginx-request-logging`, `write-compressor`, `pypi-server`, `vulnerable-secret`, `model-extraction-relu-logits`, `dna-insert`, `overfull-hbox` |

**2 of 12.** On the other ten the adapter logs
`stella-adapter: git baseline skipped: git is not installed` — which is
`git_baseline.py` behaving exactly as documented (every guard exits 0; the
adapter provisions no utilities). Confirmed live in the run logs of both arms.

This qualifies the issue's premise. "#1225 fixed that: every task folder now
gets a git repository before Stella starts" holds only where the image already
has git. Everywhere else the diff probe stays blind, candidate isolation stays
unavailable, and — because an authored witness requires a disposable candidate
— the witness rung cannot fire either. On ~83% of sampled images the tracked
command is unreachable no matter which arm runs.

## 3. What was NOT measured, and why

**The live two-arm benchmark did not run.** Both arms are implemented,
scripted (`run.sh`), and were launched; neither produced a verification turn.
Two independent blockers, the second decisive:

1. **Task containers have no TLS egress here.** The only outbound path in this
   environment is a host-loopback proxy, unreachable from a container's network
   namespace — and the adapter blanks `HTTP(S)_PROXY` and pins
   `provider_proxy: disabled` as a deliberate security control. Defeating that
   to obtain a number would trade a security property for a measurement, so it
   was not done. `host_run.sh` exists as the answer to this one: same pipeline,
   same posture, same task workspaces and instructions, run outside the
   container where egress works, with no verifier and therefore **no reward**.
2. **The API key has no credit balance.** A direct call to the Messages API
   returns `Your credit balance is too low to access the Anthropic API`. No
   model call is possible from this environment, in a container or on the host,
   so `host_run.sh` cannot rescue the measurement either.

Both harnesses are committed and ready. On a funded host with container egress:

```bash
TB_ROOT=/abs/scratch STELLA_BINARY=/abs/…/stella ANTHROPIC_API_KEY=… \
  bench/evidence/judge-evidence-demand-1295/run.sh
```

Two incidental environment findings, recorded so the next person does not
rediscover them: the frozen seed catalog carries `claude-sonnet-5`,
`claude-sonnet-4-6` and `claude-fable-5` for `anthropic` but **not**
`claude-opus-5`, so an opus witness author is unresolvable in a container
(`STELLA_CATALOG_AUTO_REFRESH=0`) and a treatment arm naming it is correctly
refused by `require_independent_witness`; and the adapter refuses to start when
the host carries unregistered `STELLA_*` variables, which a development shell
easily has — `run.sh` scrubs them.

## 4. Why ON is the right setting given what is and is not known

The instruction was to leave it on if it works well and turn it off if the same
problem persists. Neither branch was reached empirically, so the setting rests
on this instead:

- **The measured failure cannot recur.** The problem was a turn spent on every
  task. With the gate, a run with no tracked command never raises the ask —
  the two arms are behaviourally identical there, and that is a unit test, not
  a hope. §2.1 and §2.3 say that describes the entire published baseline and
  ~83% of sampled task images.
- **The worst case is bounded and small.** One ask per candidate, drawn from
  the same `max_revisions` budget a real failure spends, only on a run already
  headed for an unverified pass.
- **The upside is unmeasured.** Whether the ask converts near-misses where a
  command *does* exist is exactly what §3 could not test. That is the open
  question, and it is the one the next funded run should answer.

Flipping to off is a one-line change to `PipelineConfig::default`, or a
per-run `pipeline_judge_evidence_demand = "off"` with no rebuild.

## 5. Files

| | |
|---|---|
| `analyze.py` | `extract` a job/host directory into `turns.jsonl`; `report` the rates. Restates `judge_pass_stands_alone` over the wire snapshot and refuses to score turns that lack one. |
| `run.sh` | Two-arm Harbor run. The arm is `STELLA_JUDGE_EVIDENCE_DEMAND`, which lands in the hashed posture, so a trial's arm is a property of its digest. |
| `host_run.sh` | The same two arms outside the task container, for an environment that cannot give a container egress. No verifier, therefore no reward — rate only. |
