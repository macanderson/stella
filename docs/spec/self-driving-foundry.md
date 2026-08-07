---
id: self-driving-foundry
title: "Self-driving campaigns — the manifest, the twin arena, and the foundry loop"
status: proposed
---

# Self-driving campaigns — the manifest, the twin arena, and the foundry loop

**Status: proposed.** This is the design for the next generation of
self-driving mode: a durable, manifest-governed outer loop that races
digital twins of Stella (different branches, configs, and eventually
model weights) against each other on Terminal-Bench, promotes winners on
statistical evidence, harvests every trial into a training corpus, and —
in its final phase — trains and re-evaluates open-weight models in a
closed loop that can, in principle, run forever and still exit
deterministically.

It is the engineering counterpart of the "foundry" concept
(capture → verify → distill → evaluate → deploy; see
`docs/papers/stella-foundry.html`, a future-state concept page) and the
composition layer over the self-improvement track
(#830–#836, especially #832 stella-maintains-stella, #835 capability
curriculum, #836 weight-space adapters).

The single most important finding from the survey that produced this
document: **every primitive already exists in tree as a first slice.**
This spec is mostly about seams — a manifest, a state machine, and one
supervisor — not new machinery.

---

## 1. Vocabulary

| Term | Meaning |
|---|---|
| **campaign** | One manifest-governed pursuit of one objective ("perfect TB 2.1"). The unit that starts, suspends, resumes, and exits. Long-lived: weeks to unbounded. |
| **generation** | One turn of the outer loop inside a campaign: plan → forge → arena → judge → promote → harvest → (train) → measure → gate. Monotonic counter, never reused. |
| **twin** | One runnable identity of Stella: a git commit, plus a config overlay, plus a model/weights binding. Twins are immutable once forged; a change is a new twin. |
| **champion** | The incumbent twin. There is exactly one per campaign at any ledger position. |
| **challenger** | A twin forged this generation to beat the champion, carrying a falsifiable hypothesis. |
| **axis** | The single dimension a challenger varies: `code` (a branch), `config` (settings/prompt policy), or `weights` (a trained checkpoint served as the worker model). |
| **trial** | One task attempt by one twin in the arena — Harbor's unit, with its `stella-events.jsonl` as ground truth. |
| **promotion** | Champion replacement, granted only by the statistical gate (§5.4). |
| **suspension** | A named, probed, non-terminal stop (credits exhausted, rig unreachable, power loss). Suspensions never exit a campaign. |
| **exit** | Termination through a manifest predicate, recorded with evidence. The only way a campaign ends. |

---

## 2. The parts bin — what exists today

The design builds on surveyed, working code. Paths are load-bearing;
each row is a dependency of a later section.

| Primitive | Where | State |
|---|---|---|
| Self-driving cycle loop (plan, fix batch, audit aperture, tickets, bench, AIMD calibration) | `scripts/self-driving.sh`, `stella self-driving …`, pure core in `crates/stella-core/src/self_driving.rs` | Shipped. No manifest, no in-flight resume, env-var configured. |
| Service supervision (launchd/systemd, `RunAtLoad`, opt-in `KeepAlive`, resolver shim, `resume-all`) | `stella daemon install/uninstall/resume-all`, `crates/stella-cli/src/daemon/service.rs` | Shipped (#1587). The self-driving shell loop still carries its own macOS-only duplicate installer. |
| Branch-pinned SUT builds, cached by commit | `arenabench/arenabench/sut.py`, `sut_build.py` (detached worktree + zigbuild), GUI branch picker | Shipped. `sut_ref` is match-level only and does not round-trip TOML (§5.2). |
| Match configuration and artifacts (seats, attempts, `stella-events.jsonl` per trial, `result.json`, reward at `verifier_result.rewards.reward`) | `arenabench/arenabench/{config,model,runner,telemetry}.py` | Shipped. |
| Paired statistical comparison with guard metrics and a `GuardBlocked` verdict | `crates/stella-core/src/comparison.rs` + `self_tuning::select_winner` | Shipped, deterministic, unused by arenabench (§5.4). |
| Byte-exact model-call reconstruction, digest-verified | `crates/stella-store/src/reconstruct.rs` | Shipped. |
| Full-transcript trace capture with reward labels | `crates/stella-cli/src/trace.rs` (#1042), `crates/stella-pipeline/src/reward.rs` (#1043) | Shipped; `trace_capture` defaults off, so no corpus accumulates. |
| SFT dataset exporter with named acceptance predicate and manifest | `stella dataset export`, `crates/stella-cli/src/dataset_cmd.rs` (#872) | Shipped; predates reward labels and transcript reconstruction — carries neither. |
| Flip proof, tamper exclusion, verdict evidence | `crates/stella-protocol/src/proof.rs`, `ladder.rs`; doc:witness-protocol, doc:verification-gate | Shipped. |
| Local/OpenAI-compatible serving of arbitrary weights | reserved `local` provider (`crates/stella-model/src/factory.rs`), custom providers in `settings.json` | Shipped. |
| Event-stream replay and equivalence checking | `crates/stella-pipeline/src/replay.rs`; doc:replay-golden-trajectories | Shipped (stream conformance, not model-response replay). |
| Closed-loop playbook prose | `docs/playbooks/self-improving-model.html` | Written; not code. |

Gaps, in one line each: no manifest; no generation layer above cycles; no
resume of in-flight work; per-seat SUT pinning missing; the exporter is
blind to rewards and transcripts; no preference-pair emitter; no trainer
port; no weights registry; two daemon surfaces.

---

## 3. Architecture overview

```text
                       campaign manifest (.stella/self-driving/campaigns/<name>.toml)
                                        │  validated, content-hashed
                                        ▼
   ┌──────────────────────── campaign supervisor (stella daemon, KeepAlive) ───────────────────────┐
   │                                                                                               │
   │   campaign ledger (~/.stella/self-driving/<slug>/campaigns/<name>/ledger.jsonl)               │
   │   ── pure fold ⇒ position: generation G, stage S, lease, spend, exit-predicate state          │
   │                                                                                               │
   │   PLAN ─▶ FORGE ─▶ ARENA ─▶ JUDGE ─▶ PROMOTE ─▶ HARVEST ─▶ TRAIN* ─▶ MEASURE ─▶ GATE ─┐       │
   │    ▲    (twins)  (paired   (stats   (champion   (traces →  (weights  (champion  (exit │       │
   │    │              trials)   gate)    swap)       corpus)    twin)     on eval)  preds)│       │
   │    └──────────────────────────────── continue ────────────────────────────────────────┘       │
   │                                                              │ exit record                    │
   └──────────────────────────────────────────────────────────────┼────────────────────────────────┘
                                                                  ▼
                                              TARGET_REACHED | MODEL_CONSTRAINED |
                                              BUDGET_EXHAUSTED | OPERATOR_STOP
```

`TRAIN` runs every N generations when the weights axis is enabled;
otherwise the loop skips from `HARVEST` to `MEASURE`.

Placement follows the house rule (AGENTS.md invariant #2 — no I/O in the
engine):

- **`stella-core/src/self_driving/campaign.rs`** — the pure half. Ledger
  record types, the fold to a `CampaignPosition`, stage-transition
  legality, exit-predicate evaluation, suspension classification,
  idempotency-key derivation. Property-tested like the rest of
  `self_driving.rs`.
- **`stella-cli/src/self_driving_cmd/campaign.rs`** — the I/O half.
  Manifest load/validate, ledger append (single writer, same discipline
  as `state.rs`), stage executors that shell out to arenabench, the
  trainer port, and git.
- **arenabench** — gains per-seat SUT pinning and a comparison bridge; it
  remains the only thing that touches Harbor and containers.
- **Existing cycles are the PLAN/FORGE engine for the `code` axis.** A
  generation does not replace the fix-batch/audit/tickets loop; it wraps
  it and gives it a selection pressure.

---

## 4. The campaign manifest

### 4.1 Location and lifecycle

`.stella/self-driving/campaigns/<name>.toml`, **tracked in git** — the
same reasoning as `.stella/rules/`: intent only steers if it travels with
the repository. Runtime state never lives here; it stays under
`~/.stella/self-driving/<slug>/campaigns/<name>/`.

The manifest is content-hashed at campaign start. Every ledger append
records the hash it ran under. Editing the manifest mid-campaign is
allowed but explicit: the next `GATE` notices the hash change, journals a
`manifest_changed` record, and re-validates before continuing — so intent
changes are auditable, never silent.

`stella self-driving campaign validate <name>` checks the schema, the
split seed, predicate well-formedness, and cross-field rules (a `weights`
axis requires a `[training]` block; a guard metric must be a known
dimension) — and runs in CI like `stella context validate`.

### 4.2 Schema

```toml
schema = 1

[campaign]
name = "tb21-perfect"
# Free text, but not decoration: verbatim in `status`, in every report
# header, and in the prompt preamble of PLAN-stage agents (fixed for the
# life of the campaign — prompt bytes stay stable, invariant #7).
intent = """
Drive Stella to the best Terminal-Bench 2.1 score reachable with the
current frontier model, then keep going with trained open weights.
Every trial feeds the training corpus.
"""

[objective]
benchmark = "terminal-bench-2.1"     # arenabench dataset key, digest-pinned
metric = "solve_rate"
target = 1.0                          # "perfect"; the gate needs confidence, not luck
confidence = 0.95
attempts_per_task = 3                 # repeats — a single pass is not evidence

[exit]                                # §10. The ONLY ways a campaign ends.
target_reached = true                 # objective met at confidence on the eval split
plateau_generations = 8               # no promotion on ANY axis for N generations
budget_usd = 5000                     # cumulative from the ledger, all stages
generations = 0                       # 0 = unbounded
wall_clock_days = 0                   # 0 = unbounded

[twins]
champion = "main"                     # starting ref; thereafter the ledger owns it
axes = ["code", "config", "weights"]
max_challengers_per_generation = 2
hypothesis_required = true            # every challenger states a falsifiable claim

[bench]
tasks = "train-split"                 # named split (§7.4); selection never sees eval
attempts = 3
concurrency = 4
rig = "ec2:i-07d46341dcc9a31b3"       # or "local"; rig lifecycle stays trap-guarded
record_video = false

[promotion]                           # consumed by the comparison bridge (§5.4)
primary = "solve_rate"
guards = [
  { metric = "priced_cost", tolerance = 0.15 },
  { metric = "clock_time",  tolerance = 0.25 },
]
min_paired_tasks = 8
exclude_operational_aborts = true     # excluded from denominators, named in reports

[dataset]
capture = true                        # forces trace_capture on for campaign runs
emit = ["sft", "dpo"]
output = "~/.stella/foundry/datasets/tb21-perfect"
accept = { min_reward = 1.0, require_verified_reconstruction = true, witness_intact = true }

[dataset.splits]
seed = 20260807                       # fixed at creation, journaled; changing it is a new campaign
eval_fraction = 0.2                   # task-level split of the benchmark

[training]                            # optional; required iff "weights" ∈ twins.axes
enabled = true
every_generations = 4
trainer = "command:scripts/train/lora.sh"   # the Trainer port (§8.1)
base = "qwen3-coder-32b"
serve = { provider = "local", base_url = "http://gpu-node:8000/v1" }
verifier = "anthropic/claude-fable-5"       # never the tuned weights (§8.3)

[durability]                          # §9
heartbeat_secs = 60
lease_secs = 900
backoff = { initial_secs = 60, max_secs = 3600 }

[budget]
per_generation_usd = 150
per_trial_usd = 10
```

### 4.3 Why a manifest and not flags

Three properties flags cannot give:

1. **Intent is communicable and durable.** The `intent` string and the
   objective are read by the PLAN stage's agents, shown in `status`, and
   stamped into every dataset manifest — the purpose of the loop is a
   reviewable artifact, not tribal knowledge in a shell history.
2. **Exit is deterministic.** Predicates live in one place, are evaluated
   by a pure function over the ledger, and every evaluation is journaled.
   Two people reading the same ledger and manifest compute the same
   answer to "why did it stop?" — or "why is it still running?".
3. **Reproducibility.** The manifest hash in every ledger record ties each
   generation's behavior to the exact intent it served.

---

## 5. The twin arena

### 5.1 Twin identity

```text
twin_id = blake3(axis, base_commit, delta_ref, config_digest, weights_id)
```

A twin record in the ledger carries: the axis, parent twin, git commit
(resolved, never a floating ref — the arena already pins by commit), the
config overlay digest, the weights binding (provider id, base URL, model
id, checkpoint digest for `weights` twins), the forging generation, and
the hypothesis card:

```json
{
  "hypothesis": "loop-detector stagnation rung fires too late on TB long-horizon tasks",
  "predicted": { "metric": "solve_rate", "delta": "+0.05" },
  "evidence": ["trace:exec-841#step-19", "match:g12-arena/jobs/..."],
  "axis": "code"
}
```

The JUDGE stage writes the measured delta back onto the card. Over time
the ledger accumulates a hypothesis → outcome corpus — exactly the
substrate #834 (causal self-model) needs, produced as a side effect.

### 5.2 Arena changes (the one real gap)

Everything downstream of ref resolution is already per-commit; the pin
just cannot be expressed per seat. Three changes, all in arenabench:

1. **`sut_ref` moves to the seat.** `Contestant` gains an optional
   `sut_ref` overriding the match-level default
   (`arenabench/arenabench/model.py` — today a single match-level string
   consumed once in `runner.py`). Two Stella seats may then run two
   branches; the builder dedupes by commit, so champion vs challenger
   costs one extra build, not one per seat.
2. **`sut_ref` round-trips TOML.** `match_from_toml` reads it,
   `match_to_toml_dict` writes it (today neither does) — a committed
   match file must be able to express a pinned twin duel, and "download
   this match" must stop silently dropping the pin.
3. **Provenance becomes per-arm.** `provenance.py`'s single
   `sut_ref/sut_commit/sut_sha256` triple and the `comparability_key`
   bake in one SUT; both become per-contestant so a two-twin match
   records both identities and comparisons refuse mismatched keys per
   arm rather than per match.

A `weights` twin needs no arena change at all: it is an ordinary seat
whose engine points at the served checkpoint
(`engine.api = "local"`-equivalent custom provider + `base_url` +
`model`), with the verifier role pinned to the frontier model (§8.3).

### 5.3 What a generation races

- Champion vs each challenger, **same tasks, same attempts, same
  dataset digest, same Harbor version** — the comparability key per arm
  must agree on everything except the twin under test.
- Task list drawn from the **train split only** (§7.4), sampled
  deterministically from the campaign seed + generation number, so a
  re-run of generation G races the same panel.
- Operational aborts (auth failures, credit exhaustion, rig death) are
  classified via the adapter's exit-cause machinery, **excluded from
  denominators, counted, and named in the generation report** — the
  standing bench-honesty rule, now enforced by code rather than
  discipline.

### 5.4 The promotion gate

Bridge arenabench's per-trial metrics into
`stella_core::comparison::ArmTrials` and let the existing engine decide:

- **Pairing is enforced** — a task counts only when every arm ran it;
  unpaired tasks are reported, never silently dropped.
- **Winner needs two independent bars**: a confident lift on the primary
  metric (`self_tuning::select_winner` — sample floor plus significance)
  and no guard-metric regression past tolerance. `GuardBlocked` is a
  loss, not a win with an asterisk.
- The verdict, its evidence, and the full per-task pairing table are
  journaled. A challenger that loses is retired, its hypothesis card
  annotated; nothing is deleted.

Two honesty rails are structural, encoded from the bench scars of
2026-08-07:

- **Conclusions come from the trace.** The JUDGE stage joins
  `result.json` verdicts against each trial's `stella-events.jsonl`
  (reward at `verifier_result.rewards.reward`, `(role, model)` census
  from `step_usage`) before believing them. A trial whose surface verdict
  and trace disagree is quarantined, not scored.
- **The cheap control runs before any conclusion.** A challenger "win"
  that rests on champion regressions triggers a control re-run of the
  champion on exactly the regressed tasks before the gate may promote —
  one extra trial batch instead of a wrong promotion.

### 5.5 Where challengers come from

The PLAN stage is Stella driving Stella — the existing self-driving cycle
machinery pointed at a generation-scoped goal:

- **`code` axis**: mine the previous generations' failing traces
  (loop-detector fires, budget aborts, wrong-file edits, timeout shapes)
  into defect hypotheses; run fix-batch cycles in worktrees; each
  resulting branch is a challenger. The aperture ladder and AIMD
  calibration stay exactly as they are — they govern *how much* work a
  cycle takes on.
- **`config` axis**: propose overlay deltas (prompt policy, tool
  switches, effort pins, budget shapes) as declarative diffs against the
  champion's overlay — cheap to forge, often high-yield.
- **`weights` axis**: candidates come from TRAIN (§8), not from PLAN.

`max_challengers_per_generation` bounds spend; the hypothesis card makes
every challenger falsifiable and every generation legible.

---

## 6. The generation state machine

### 6.1 Ledger and fold

One append-only `ledger.jsonl` per campaign (same single-writer
discipline as `state.rs`; atomic tmp+rename for the live `campaign.json`
pointer, heartbeats included). Every record carries
`(campaign, generation, stage, seq, manifest_hash, ts)` plus a typed
payload; the fold is a pure function in `stella-core` returning:

```text
CampaignPosition {
  generation, stage, stage_attempt,
  champion: TwinId, live_challengers: Vec<TwinId>,
  spend: SpendLedger,            // per stage, generation, campaign
  lease: { host, pid, heartbeat },
  suspension: Option<Suspension>,
  exit: Option<ExitRecord>,
}
```

Stage transitions have a legality table (mirroring
`stage_rank`/`stage_transition_legal` in the pipeline's replay module);
an illegal append is refused at write time, not discovered at read time.

### 6.2 Idempotency and resume

Every stage execution derives an idempotency key
`(campaign, generation, stage, attempt)` and journals `stage_started`
with its inputs (task panel, twin set, match spec digest) **before** any
side effect, and `stage_completed` with outputs after. Resume is:

1. Fold the ledger → position.
2. If `stage_started` has no matching `stage_completed`: re-enter that
   stage with the same inputs. Stage executors are written to be
   re-runnable:
   - **ARENA**: a match whose results file exists and whose spec digest
     matches is ingested, not re-run; an incomplete match is relaunched
     as a new match id with the same panel and the old id journaled as
     abandoned. (Salvaging completed trials out of an abandoned match is
     an optimization, §13.)
   - **FORGE**: SUT builds are cached by commit — a re-run is a cache
     hit. Trainer runs declare their own resumability (§8.1).
   - **HARVEST/MEASURE/JUDGE/GATE**: pure or idempotent by construction
     (recompute from artifacts; appends carry the idempotency key so a
     duplicate is a no-op).
3. Otherwise: enter the next stage per the legality table.

No stage ever reads the wall clock to decide position. Time appears in
records as evidence, never as control flow — this is what makes a
two-week power loss indistinguishable from a two-second crash.

### 6.3 What this does NOT change

Cycles, apertures, tickets, the gate, witness discipline — untouched. A
generation is a harness around existing behavior. In particular the
"nothing left behind" rule still runs inside every PLAN cycle: findings
that don't become challengers become GitHub issues, exactly as today.

---

## 7. The data plane — every trial becomes training signal

### 7.1 Capture

`[dataset] capture = true` forces `trace_capture` on for every
campaign-launched run (arena seats included — the adapter already ships
the binary and env into the container; it gains the trace flag and pulls
`traces.jsonl` back with the other artifacts). Traces are the full,
digest-verified, secret-redacted transcript records of
`crates/stella-cli/src/trace.rs` — reward-labeled per #1043.

### 7.2 Harvest

The HARVEST stage folds, per trial:

- the trace record (verified `prompt_messages` per call, tool uses,
  reward label, stage trajectory);
- the arena verdict (`result.json`, joined to the trace, quarantining
  disagreements);
- the flip evidence where present (`ProofStep::Oracle`,
  `LadderSnapshot.flip_achieved`, `witness_intact` — doc:witness-protocol);

into corpus increments with full lineage: every emitted example names its
`(campaign, generation, match, trial, execution_id)` and the digests of
its sources. "When a regulator asks what the model learned from, there's
an answer" is a property of the pipeline, not a slogan.

Two exporter upgrades fall out immediately (they are Phase 0, §12,
because they are valuable independent of campaigns):

- `stella dataset export` gains `reward` (from
  `stella-pipeline/src/reward.rs`) and `prompt_messages` (from
  `Store::reconstruct_call`, gated on `is_verified()`), closing the two
  gaps it shipped with.
- A **preference-pair emitter**: two twins attempting the same task in
  the same generation with divergent rewards are a natural DPO pair —
  same prompt, chosen/rejected trajectories, plus the paired metadata
  (twin ids, rewards, reward policy). Best-of-N attempts by one twin pair
  the same way. The arena is not just the evaluator; it is the second
  half of the dataset.

### 7.3 Acceptance

The acceptance predicate is named, echoed verbatim into the dataset
manifest (the `dataset_cmd.rs` pattern), and defaults strict:
`min_reward = 1.0` (deterministic flips or benchmark-verified solves
only), `require_verified_reconstruction = true` (no unverified bytes),
`witness_intact = true` (tamper-excluded). Discarded-not-punished stays:
`Unverifiable` trajectories are dropped, never labeled negative.

### 7.4 Splits and contamination

The benchmark's task set is split **once, at campaign creation**, by the
manifest seed: train split (selection pressure + training data) and eval
split (exit measurement only). Structural rules:

- Nothing derived from an eval-split trial ever enters the corpus — the
  HARVEST stage refuses by task id, and the dataset manifest records the
  refusal count.
- Champions are MEASURED on the eval split; challengers are selected on
  the train split. Selection noise cannot Goodhart the exit metric.
- The full-suite number that would be *published* is a separate,
  explicit run (the existing preregistered evidence path in `bench/`),
  and any published claim must disclose the campaign trained and
  selected on the train split. A perfect campaign score is an internal
  signal; the honest external number comes from held-out measurement.

### 7.5 Privacy boundary

Nothing here creates egress. Traces and datasets live in
`.stella/private/` and the manifest-named output dir (0700/0600),
redact-at-writer (`redact_secrets` over every string leaf after
assembly), `redacted: true` stamped per record. The content-free gate
(`crates/stella-store/src/content_free.rs`) is unaffected because none
of this enters a hub table — and this spec re-states its standing
caveat: the live `stream-json` stdout stream carries prompt text
(`BlockRegistered.content`); campaign tooling must never point that
stream at anything but local disk. Human sign-off before a dataset is
used for training remains required, as `stella dataset` already states.

---

## 8. The training loop

### 8.1 The trainer is a port

`stella-core` gets a `Trainer` port; the CLI ships one adapter:
`command:` — an executable contract, because training stacks (TRL,
axolotl, torchtune, a cloud job) churn faster than this repo:

```text
trainer run  --dataset <dir> --base <id> --out <dir>    # blocking; resumable if it says so
trainer probe --out <dir>                               # {absent | running | complete | failed}
```

Contract: deterministic inputs in (dataset manifest digest, base id,
hyperparams file), a **weights card** out:

```json
{
  "weights_id": "tb21-perfect/gen-12/adapter",
  "base": "qwen3-coder-32b",
  "dataset_digest": "…",
  "trainer": { "kind": "lora", "impl": "scripts/train/lora.sh", "version": "…" },
  "artifacts": { "path": "…", "digest": "…" },
  "metrics": { "loss": "…" }
}
```

Cards land in a weights registry (`~/.stella/foundry/weights/`, a
JSONL index + artifact dirs). The TRAIN stage journals the card digest;
`probe` is what makes a multi-hour training run survive supervisor
restarts without re-spending GPU time.

### 8.2 Serving and evaluation

A completed card forges a `weights` twin: a custom provider block
(OpenAI-compatible, `base_url` from the manifest) with the checkpoint's
model id. It then enters the next generation's ARENA like any other
challenger — same paired trials, same gate, same guards. **Auto-train and
auto-evaluate is not a new mechanism; it is the same promotion gate
applied to a different axis.** A tuned model that cannot beat the
champion on held-out pairing does not deploy — the parity gate from the
foundry concept, implemented as §5.4.

### 8.3 Independence

The verifier/judge role never runs on candidate weights — it stays
pinned to the manifest's frontier `verifier` (the playbook's L-E11 rule,
and the same principle as #1795's verifier ≠ worker). A model must never
grade its own training progress. Reward labels in the corpus carry the
`RewardPolicy` they were computed under, so cross-campaign pooling stays
renormalizable.

### 8.4 The closed loop, stated plainly

Generation G's trials → corpus increment → (every N generations) train →
weights twin → generation G+k's arena → promotion or retirement →
G+k's trials → … Champion lineage, dataset lineage, and weights lineage
all live in one ledger, so "which proofs trained the model that won
generation 40" is a query, not an archaeology project.

---

## 9. Durability — the loop that will not stop

### 9.1 One supervisor

The campaign runs under the existing daemon surface, period:

```bash
stella self-driving campaign start tb21-perfect
# ≡ stella daemon install --label selfdrive-tb21-perfect --keep-alive \
#     -- self-driving campaign run tb21-perfect
```

`RunAtLoad` covers reboot (including power restored after weeks);
`KeepAlive` + throttle covers crashes; the resolver shim covers the
binary moving under a brew upgrade; systemd linger covers logout on
Linux. The shell loop's private macOS-only launchd installer is retired
in the same change — two service-registration paths is one too many.

### 9.2 Leases and heartbeats

The supervisor heartbeats the live pointer (`campaign.json`) every
`heartbeat_secs` and holds a lease `(host, pid, started_at)`. On start:

- fresh lease held by a live process → refuse to double-drive (the
  existing "one loop per state dir" pidfile rule, made crash-safe);
- expired lease → journal `lease_adopted { previous }` and take over.

Observers (Observatory, `status`) render a stale heartbeat as
*suspended: presumed dead*, mirroring today's staleness fold — but now
the successor resumes the work instead of merely reporting the corpse.

### 9.3 Cold-boot recovery protocol

Every start — first boot, crash restart, or power restored — runs the
same sequence; there is no separate "crash path" to rot:

1. Fold the ledger → position (§6.1).
2. Re-validate the manifest hash; journal a change if edited.
3. **World revalidation**, each check a named probe:
   repo reachable at the expected remote; credentials probe per
   provider; rig reachable/startable; SUT cache present for live twins
   (rebuild on miss — cached by commit, so this is cheap or correct,
   never wrong); dataset export digest intact; disk/memory floors via
   the existing supply probes; trainer `probe` for any in-flight TRAIN.
4. Any failed probe → **suspension**, not exit (§9.4).
5. Re-enter the first incomplete stage per §6.2.

### 9.4 Suspension taxonomy

Named, typed, journaled, each with a probe and a backoff:

| Suspension | Probe | Notes |
|---|---|---|
| `provider_credit` / `provider_auth` | cheap authenticated call | The operational-abort classes from the bench ledger — never scored, never fatal. |
| `rig_unreachable` | EC2 describe/SSH poll | Rig stop stays trap-guarded; a dead rig suspends, it does not exit. |
| `network_down` | remote git ls-remote | |
| `disk_low` / `mem_low` | existing supply probes | AIMD calibration may also shrink the next generation instead of suspending. |
| `red_main` | gate status on main | PLAN-stage rule already: fixing main becomes the batch. |
| `trainer_unavailable` | trainer `probe` | TRAIN suspends; ARENA generations on other axes may continue — suspension is per-stage-dependency, not global, where safe. |
| `operator_pause` | explicit `campaign pause` | Distinct from stop: holds position, exits nothing. |

Backoff: exponential from `initial_secs` to `max_secs`, then **steady
retry at the ceiling forever**. There is no give-up branch. A suspension
that persists for a month is a month of hourly probes — that is the
designed behavior, and `status` says so in plain language.

### 9.5 Chaos witnesses

Durability claims get the same treatment as any feature: a harness (in
the spirit of `scripts/test-self-driving.sh`) that `kill -9`s the
supervisor at randomized stage boundaries and mid-ARENA, truncates a
ledger tail (torn-write healing, as the session journal already does),
expires leases, and asserts the fold + recovery protocol lands in the
same position every time. "Survives weeks of power loss" reduces to
"recovery is a pure function of the ledger" — which is testable without
waiting weeks.

---

## 10. Deterministic exit

### 10.1 The taxonomy

| Exit | Fires when | Evidence in the exit record |
|---|---|---|
| `TARGET_REACHED` | Champion meets `objective.target` at `confidence` on the **eval split**, sustained for 2 consecutive MEASURE stages | the two measurement tables, task-level |
| `MODEL_CONSTRAINED` | No promotion on **any enabled axis** for `plateau_generations` | per-axis last-promotion table; the answer to "as perfect as the model allows" |
| `BUDGET_EXHAUSTED` | Ledger spend ≥ `budget_usd` (or generation/wall-clock caps if set) | the spend fold |
| `OPERATOR_STOP` | `stella self-driving campaign stop <name> --reason …` | the signed stop record |

`MODEL_CONSTRAINED` is the predicate that makes "perfect, or as perfect
as the model permits" well-defined: with the weights axis enabled, even
"the model" is inside the search, so a plateau means the whole system —
code, config, and trainable weights — has stopped improving at the
stated confidence. That is a theorem about the campaign, not a shrug.

### 10.2 Mechanics

`evaluate_exit(manifest, position) -> Option<ExitRecord>` is pure, runs
at every GATE, and its evaluation (fired or not, with the operand
values) is journaled — so "why is it still running" has the same
auditable answer as "why did it stop". The exit record ends the daemon
(`campaign stop` semantics: finish the journal, uninstall the service,
leave everything else in place). `status` always shows distance-to-exit:
current champion score vs target, generations since last promotion vs
plateau, spend vs budget.

---

## 11. Invariants

Same contract as AGENTS.md's list: the numbering is an address —
append, never renumber.

1. **A campaign exits only through a manifest predicate.** Every failure
   is a named suspension with a probe and a backoff. There is no other
   terminal state.
2. **Resume is a pure function of the ledger.** No stage consults the
   wall clock for position; a two-week outage and a two-second crash
   recover identically.
3. **A twin is immutable and fully identified.** Axis, commit, config
   digest, weights digest — comparisons refuse arms whose comparability
   keys differ in anything but the declared delta.
4. **Promotion requires the paired statistical verdict.** Crowns,
   solve-rate eyeballing, and single runs never promote. `GuardBlocked`
   is a loss.
5. **Judgments join the trace.** No ledger conclusion may rest on a
   surface projection; verdict/trace disagreement quarantines the trial.
6. **Operational aborts are excluded and named.** Never in a
   denominator, always in the report.
7. **Eval-split data never enters the corpus,** and challengers are
   never selected on the eval split. The split seed is fixed at campaign
   creation.
8. **The verifier never runs on candidate weights.**
9. **No new egress.** All campaign artifacts are local; dataset writers
   redact at the writer; the content-free gate's scope is unchanged.
10. **One supervisor surface.** Campaign processes run under
    `stella daemon`; no parallel service-registration path may return.

---

## 12. Phasing — each slice lands with its witness

Ordered so every phase is independently valuable and independently
provable; no phase depends on a later one.

**Phase 0 — seams (no new subsystem):**
per-seat `sut_ref` + TOML round-trip + per-arm provenance (arenabench);
`dataset export` gains `reward` + verified `prompt_messages`; the
`TrialMetrics → ArmTrials` comparison bridge. *Witnesses:* a two-branch
match TOML round-trips and races two commits; an exported record carries
a reward label that matches the trial's flip evidence; the bridge
reproduces `comparison.rs` fixtures from arena artifacts.

**Phase 1 — manifest + durable shell:** manifest schema + `validate`;
campaign ledger, fold, lease, recovery protocol; `campaign
start/stop/pause/status` on the `stella daemon` surface; retire the
duplicate shell installer. The loop body at this phase is just the
existing cycle command. *Witnesses:* the chaos harness (§9.5) — kill,
truncate, adopt, resume to identical position; exit fires from a
synthetic ledger for each predicate and from no other input.

**Phase 2 — the twin arena:** FORGE/ARENA/JUDGE/PROMOTE on the `code` +
`config` axes; hypothesis cards; control re-runs; generation reports.
This phase alone delivers "one agent drives Stella at Terminal-Bench and
promotes what works." *Witnesses:* a rigged fixture arena (canned trial
artifacts) drives a full generation deterministically; promotion refuses
a confounded arm (differing comparability key) and a `GuardBlocked` lift.

**Phase 3 — the data plane:** capture-on-campaign, HARVEST, splits +
contamination refusal, SFT + DPO emitters with lineage. *Witnesses:*
an eval-split trial is refused with a journaled count; a DPO pair's two
trajectories reconstruct verified and share a prompt; lineage from a
sampled example resolves back to its trial artifacts by digest.

**Phase 4 — the closed loop:** Trainer port + `command:` adapter,
weights registry, weights twins, TRAIN/MEASURE stages, plateau exit
armed across all axes. *Witnesses:* a stub trainer (instant,
deterministic "weights") flows card → twin → arena → gate end-to-end;
`probe` resumes a "running" training across a supervisor kill;
`MODEL_CONSTRAINED` fires on a synthetic no-promotion ledger.

**Phase 5 — soak:** a real multi-week campaign on the rig with weekly
operator review; Observatory campaign view; salvage/optimization
backlog (§13).

---

## 13. Open questions (decisions the maintainer owns)

1. **Base model and trainer stack** for Phase 4 (`qwen3-coder`-class vs
   smaller; LoRA vs full FT; local GPU box vs cloud job) — the port
   isolates the choice, but the first adapter has to pick one.
2. **Published-number policy** (§7.4): with TB2.1 tasks in the training
   corpus, the externally quotable number needs a preregistered stance —
   held-out-split only, or a disjoint suite (Frontier-Bench) as the
   public eval.
3. **Trial salvage** from abandoned matches (§6.2) — worth the
   complexity only if ARENA interruption proves common in soak.
4. **Fleet scale-out** — multiple rigs racing generations in parallel is
   a natural extension (the ledger fold already tolerates it via leases
   per stage), deliberately out of scope until a single-rig campaign has
   soaked.
5. **Governance** — whether campaign manifests join `stella context
   validate`'s CI surface as a first-class governed record kind, like
   `.stella/rules/`.
