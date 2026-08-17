---
id: backlog-self-driving
title: "Backlog Self-Driving — the loop that owns a queue, not a task"
status: proposed
---

# Backlog Self-Driving — the loop that owns a queue, not a task

Status: proposal, unbuilt. Phases in §9.

**Reads on top of:** [`doc:pipeline-as-plugins`](../spec/pipeline-as-plugins.md) §10
(self-driving is a *host*, not a wrapper — that decision is upstream of this
document and is not reopened here) and
[`doc:agent-native-delivery`](agent-native-delivery.md) (the issue kernel, the
provider manifests, and the residue gate — designed, unbuilt, and a hard
dependency of §3.1 and §5).

**Tracked by:** #3546 (Track D remainder) is the nearest open work and is
subsumed by B0+B2 below. Epic #1280 (agent-native delivery) was closed
`not_planned` on 2026-08-10 as *policy*, frozen behind #2374; #2374 closed
`completed` on 2026-08-14, so the freeze that closed it has lifted, and #2374's
own closing sentence names this document's subject as the weakest area in the
product: *"software developer lifecycle, creating prs/worktrees and working in
rhythm with developer drivers."*

---

## 0. Thesis

> Stella's self-driving loop can already decide **how much** work to do and
> **whether it is done**. It cannot decide **what** the work is, **do** it, or
> **ship** it. Those three verbs live outside the product, in Claude Code
> slash-command markdown, and that is the whole gap.

The loop is not missing a feature. It is missing a *surface*: everything a host
would need to call in order to drive delivery, as opposed to bookkeeping about
delivery.

---

## 1. How close are we? — the honest answer

Every row below is a claim about this tree at `eb7c2dd`, with the file that
establishes it. Read the file before believing the row.

### 1.1 Built, shipping, and good

| Capability | Where | State |
|---|---|---|
| Cycle sizing from the real machine (supply × demand × calibration → tier + knobs) | `crates/stella-autonomy/src/lib.rs` (`plan_cycle`), probes in `crates/stella-cli/src/self_driving_cmd/probes.rs` | Done. Property-tested, no threshold file. |
| AIMD ceilings moved only from evidence | `crates/stella-autonomy/src/lib.rs` (`calibrate`) | Done. Additive-increase / multiplicative-decrease, provably convergent. |
| The aperture ladder — 9 lenses, then `watch` | `crates/stella-autonomy/src/lib.rs` (`LENSES`, `advance`, `WATCH`) | Done. "No defects" is always a statement about a lens. |
| Finding dedup by content digest | `crates/stella-autonomy/src/lib.rs` (`finding_digest`) | Done. Byte-for-byte contract with existing `seen.txt`. |
| Dry-streak oracle advancing the ladder | `crates/stella-autonomy/src/lib.rs` (`dry_streak`) | Done. |
| Self-diagnosis: `STUCK` / `STARVED` / `NOISY` / `FRAGILE` | `crates/stella-autonomy/src/lib.rs` (`metrics`, `starved`) | Done. Named pathologies, not a health score. |
| Run liveness resolved by a reader, from heartbeats | `crates/stella-autonomy/src/lib.rs` (`fold_runs`) | Done. |
| A declared host-verb surface, checked in both directions | `crates/stella-autonomy/src/surface.rs` (`HOST_SURFACE`, `HOST_SURFACE_VERSION = 1`), parity test in `crates/stella-cli/src/self_driving_cmd/surface.rs` | Done. 18 verbs. |
| Durable loop state, ledger, run lifecycle | `crates/stella-cli/src/self_driving_cmd/state.rs` | Done. `~/.stella/self-driving/<slug>/`. |
| Process supervision + OS service registration | `crates/stella-cli/src/daemon.rs`, `daemon/service.rs` | Done. Survives the terminal, and (installed) the reboot. |
| Read-side dashboard over the same folds | `crates/stella-observatory/src/self_driving.rs` | Done. Cannot disagree with the terminal — both link `stella-autonomy`. |
| Worktree isolation for candidate work | `crates/stella-cli/src/candidate_ws.rs` | Done. Never touches the user's tree or the stash. |
| Cooperative dispatch leases across processes | `crates/stella-fleet` ledger, `crates/stella-cli/src/fleet_claims.rs` | Done. The right primitive for issue claims (§6.3). |

This half is genuinely strong, and it is the half that is hardest to get right.
Nothing below proposes changing it.

### 1.2 Designed, unbuilt

| Capability | Where | State |
|---|---|---|
| The `Issue` kernel, provider manifests, `kind = "exec"` | [`doc:agent-native-delivery`](agent-native-delivery.md) §3–§4 | Design only. Epic closed `not_planned` under a freeze that has since lifted. |
| The residue gate — a prose follow-up becomes undischargeable | [`doc:agent-native-delivery`](agent-native-delivery.md) §7 | Design only. This is the mechanism that makes "file new tickets" a *guarantee* rather than a habit. |
| Backlog dedup + decay | [`doc:agent-native-delivery`](agent-native-delivery.md) §10.1 | Design only. |
| Self-driving as a plugin that actually drives | #3546, [`doc:pipeline-as-plugins`](../spec/pipeline-as-plugins.md) §10 D6 | Manifest exists (`plugins/stella-selfdriving/plugin.toml`); it starts nothing. |

One correction to the record, because a design doc is a claim and not a fact:
[`doc:agent-native-delivery`](agent-native-delivery.md) §11.1 lands the provider
transports in `stella-tools` "generalizing `issue_ops.rs`". **There is no
`issue_ops.rs` in this tree** — `find . -name 'issue_ops*'` is empty. That
phase therefore starts from nothing, not from a refactor, which makes B1 below
larger than §11.1 implies.

### 1.3 Never designed, and absent

| Capability | Evidence of absence |
|---|---|
| **Any verb that does delivery work.** All 18 rows of `HOST_SURFACE` are bookkeeping: size a cycle, read state, stamp a phase, record a finding. None of them fixes anything, opens anything, or ships anything. | `crates/stella-autonomy/src/surface.rs` |
| **A provider-agnostic issue port.** `gh` is invoked literally, in the binary, to rank the queue. | `crates/stella-cli/src/self_driving_cmd.rs:378`, `:389`, `:889` |
| **PR management of any kind** — open, watch CI, answer a review, resolve a conflict, merge. | No `gh pr` and no `create_pull_request` anywhere under `crates/` or `scripts/` outside TUI test fixtures. |
| **Autonomous release.** `scripts/release.sh` is a human-driven script that refuses a tag that already exists. | `scripts/release.sh:143` |
| **Structural self-curation.** Tools, context records, and skills are curated by a human or by a prompt, never proposed from loop evidence. | `/self-driving:evolve` is `scripts/self-driving/commands/self-driving/evolve.md` — prose. |
| **A work generator.** The ladder terminates *into* `watch` and stays there until an external event. Nothing manufactures the next question. | `advance` returns `WATCH` once `LENSES` is exhausted (`crates/stella-autonomy/src/lib.rs:340`). |

### 1.4 The verdict

**The loop's skeleton is excellent and its organs are missing.** Concretely:
you can run a perpetual delivery loop against this repository *today*, and
people do — but the thing making the decisions is Claude Code, reading eight
markdown files under `scripts/self-driving/commands/`, and the thing doing the
work is Claude Code's agent loop. Stella supplies the arithmetic.

So the answer to "how close are we" is: **the deterministic half is roughly
done; the autonomous half does not exist inside the product.** Stella cannot
today read an issue, fix it, prove the fix, open a PR, drive it green, merge
it, file what it found on the way, and pick up the next one — with no Claude
Code and no human in the loop. That is one coherent gap, not five, and §2 says
why.

---

## 2. The diagnosis

### 2.1 "Make it a plugin" is right, and is not sufficient

Self-driving being a plugin is already settled —
[`doc:pipeline-as-plugins`](../spec/pipeline-as-plugins.md) §10 chose *host*
over *wrapper*, `plugins/stella-selfdriving/plugin.toml` is the consent
document, and #3546 tracks finishing the move. This design does not reopen
that; it adopts it.

But it is worth being exact about what packaging would and would not fix,
because "it should be a plugin" is a tempting whole answer and it is not one.

A host plugin drives Stella **through the host surface**. The host surface is
`HOST_SURFACE` — 18 verbs, every one of them bookkeeping. There is no verb for
"give me the next unit of work", none for "do this unit", none for "open the
PR", none for "is it merged yet". A plugin bound strictly to the declared
surface, as #3546's definition of done requires, therefore **cannot deliver
anything.** It can size a cycle it has no way to run.

Which is exactly the shape of what exists. `scripts/self-driving.sh` is 996
lines that call the bookkeeping verbs, and every actual decision — what to fix,
how to fix it, what to file, what to ship — was pushed out into the eight
slash-command markdown files, because the surface offered nowhere else to put
it. The bash is not the problem, and moving the bash into
`plugins/stella-selfdriving/` would not change one thing about what the loop can
do. **The judgement half lives outside the product because the surface gave it
no way in.**

So packaging is B-tier work and the surface is the design. Do the surface, and
the plugin becomes thin and honest — which is what a host plugin should be. Do
the packaging alone, and we relocate 996 lines of bash and still cannot
self-drive a backlog.

### 2.2 The split this commits to: **the plugin decides, the binary does**

One level up from the split the codebase already runs on (`stella-autonomy`
decides, `stella-cli` probes):

| | Owns | Why there |
|---|---|---|
| **`stella-autonomy`** (leaf, pure) | Cycle plan, ladder, dedup, AIMD, and — new — the loop step machine (§3.7), the PR state machine (§3.3), and the supply model (§4). | Property-testable over owned data. A governor that reads the machine itself cannot be handed a fake in a test. |
| **`stella-cli`** (the binary) | Every new verb in §3. The issue port, the run execution, the git/forge effects, the curation writes. | Invariant 2 keeps all of it out of the engine; invariant 1 keeps the forge behind a port. |
| **`plugins/stella-selfdriving`** (the host) | *Policy only*: when to run, how many at once, in what order, when to stop, what to escalate. A loop over declared verbs and nothing else. | It is the piece an operator should be able to fork, replace, or write in another language without forking Stella. |

The load-bearing consequence: **the judgement half becomes Stella's own agent
loop.** `work` (§3.2) runs a unit of backlog through Stella's staged pipeline —
the same triage → … → verify → verdict path a `stella run` takes. If Stella's
loop is not good enough to drive Stella's own backlog, that is a finding about
Stella, and it should be measured rather than routed around by borrowing
somebody else's agent. Today it is routed around.

---

## 3. The delivery surface

Five new verb groups. Each is `HOST_SURFACE`-declared in the same PR that adds
it (the build fails otherwise), each binds to a capability in the plugin
manifest, and each emits a shape a host can parse.

`HOST_SURFACE_VERSION` goes to `2` **once**, at B1, and not per group: every
change here is additive, and the bump rule (`surface.rs`) says additions do not
bump. It moves only because B1 changes what `queue` emits — see §3.1.

### 3.1 `backlog` — the issue port

The provider-agnostic replacement for three hardcoded `gh` call sites. Built on
[`doc:agent-native-delivery`](agent-native-delivery.md) §3–§4 without
modification: four states, four classes, title, description, comments, parent
edge, and everything else in a source-tracked TOML manifest under
`.stella/issues/`.

| Verb | Emits | What it does |
|---|---|---|
| `backlog next` | `query-envelope` | The ranked readiness queue. Supersedes `queue`, which stays as a deprecated alias for one minor version. |
| `backlog claim <key>` | `json` | Takes a cooperative lease on an issue (§6.3) and returns it, or reports who holds it. |
| `backlog file` | `json` | Files a finding as an issue, deduped by fingerprint, through the bound provider. |
| `backlog close <key>` | `json` | Closes with a receipt naming the evidence (§5). |
| `backlog link <key>` | `json` | Binds `execution_id` ↔ issue key so spend and trace are attributable per issue. |

`queue`'s emitted shape changes from a `gh`-shaped row to an `Issue`-shaped row,
which is the one breaking change in this document and the reason for the single
version bump. A host written against v1 must not silently read the new shape.

**Why this is the first thing built.** "Link it to GitHub issues, or any issue
provider configured" is the user-facing ask, and it is also the only piece every
other verb group depends on: `work` needs a unit, `sweep` needs somewhere to
file, `deliver` needs something to close.

### 3.2 `work` — one unit of backlog, through Stella's own loop

| Verb | Emits | What it does |
|---|---|---|
| `work start --issue <key>` | `json` | Resolve context for the issue, create the isolated worktree, run the staged pipeline against it, return the outcome and the diff. |
| `work status` | `query-envelope` | The in-flight unit: stage, spend, elapsed, checkpoint. |
| `work abandon` | `text` | Release the claim and the worktree, recording why. Abandonment is a *recorded outcome*, never a silent drop. |

This is the verb that does not exist today in any form, and it is the whole
autonomous half. Three properties it must have, each of which is an existing
mechanism rather than new invention:

- **Isolation is `candidate_ws.rs`'s shadow worktree**, not a branch checkout of
  the operator's tree. The loop must be able to run while a human works in the
  same clone.
- **Done is the pipeline's verdict**, not the model's claim: the flip oracle,
  tamper exclusion, `ladder_decision`. Self-driving gets no weaker definition of
  done than a `stella run` gets, and gets no new terminal state.
- **Abort is at safe boundaries** (invariant 6). A budget ceiling reached
  mid-tool waits for the tool.

### 3.3 `deliver` — branch, PR, CI, review, merge

| Verb | Emits | What it does |
|---|---|---|
| `deliver open` | `json` | Branch, commit, push, open the PR as a draft, with `Closes #N` in **both** the body and a commit trailer (AGENTS.md § *Closing the issue on merge*). |
| `deliver observe` | `json` | One read of the forge: CI conclusions, review threads, mergeability, base drift. Pure read, no decisions. |
| `deliver next` | `json` | The deterministic transition: given the observation and the PR's recorded state, the single next action. |
| `deliver merge` | `json` | Merge when and only when `deliver next` says `Merge`. |

The state machine is pure and lives in `stella-autonomy`:

```
Draft ─push─> CiPending ─┬─ green ──> ReadyForReview ──> Approved ──> Merged
                         ├─ red ────> CiRed ──(fix)──> CiPending
                         ├─ conflict > Conflicted ──(rebase)──> CiPending
                         └─ base red > BaseBroken ──(wait)──> CiPending
ReviewChangesRequested ──(address)──> CiPending
Any ──(budget|ceiling|repeat-failure)──> Escalated   [terminal, human]
```

Two design commitments, both learned from failure modes this repository has
already paid for:

- **`CiRed` and `BaseBroken` are different states.** A failure that reproduces
  on the base branch is not this PR's failure, and treating it as one burns
  every cycle on somebody else's breakage — which is precisely the first
  diagnostic question `/self-driving:evolve` asks under `STUCK`
  (`scripts/self-driving/commands/self-driving/evolve.md`). The machine
  distinguishes them structurally so the loop cannot get this wrong by
  forgetting to ask.
- **`Escalated` is terminal and reachable from everywhere.** A loop that cannot
  give up is a loop that pushes the same broken fix forever. Repeat-failure
  counting is in the pure machine; the ceiling is policy.

`deliver next` **buys no model call.** Deciding *what to do* is arithmetic over
observed facts; only *doing* it (writing the fix for a red CI) is a `work`
invocation. This mirrors `ladder_decision`, which is terminal at every outcome
and escalates to no model.

### 3.4 `sweep` — where the next question comes from

| Verb | Emits | What it does |
|---|---|---|
| `sweep audit` | `json` | Run the open lens's tooling, digest the findings, emit only the ones not in `seen.txt`. |
| `sweep regress` | `json` | Re-run the witnesses named by receipts on issues this loop closed; report any green→red. |
| `sweep meta` | `json` | Fold the ledger; emit each named pathology as a fileable finding against the loop itself. |

§4 is the argument for why these three, and no fewer.

### 3.5 `curate` — tools, context records, skills

| Verb | Emits | What it does |
|---|---|---|
| `curate propose` | `json` | Emit a proposal — a custom tool, a context record, a skill — with the evidence that motivated it. |
| `curate list` | `query-envelope` | Pending proposals and their evidence counts. |
| `curate accept <id>` | `json` | Apply a proposal **if the workspace's declared authority permits it** (§7). |

### 3.6 What deliberately stays out of the surface

- **No `release` verb at B0–B6.** §6.4.
- **No verb that edits the loop's own grant.** The loop may propose an authority
  change (§7); the surface offers no way for it to apply one.
- **No verb that breaks or steals another worker's claim.** The lease expires on
  its own; `fleet_claims.rs` already declines to offer this and gives the
  reason.
- **No mode flags.** Invariant 9: a parameter scopes, never selects. `curate
  accept` and `curate reject` are two verbs.

### 3.7 The loop step machine

The plugin's policy loop needs somewhere deterministic to ask "what now", or
every host re-derives the cycle and they drift — the exact failure
`stella-autonomy` was extracted to end (#1613). So the step function is pure and
lives beside the governor:

```rust
pub enum LoopStep {
    Plan,                       // size the cycle to the machine
    Claim { budget: Batch },    // draw from `backlog next`
    Work { issue: IssueKey },
    Deliver { pr: PrId },
    Sweep { lens: &'static str },
    Curate,
    Watch { until: WakeCondition },
    Halt { reason: HaltReason },
}

pub fn step(state: &LoopState, obs: &Observation, now: Timestamp) -> LoopStep;
```

`Halt` exists and is reachable — for a spent budget, a revoked grant, an
operator stop, or an `Escalated` PR the policy declines to abandon. **A loop
that cannot halt is not autonomous, it is unsupervised**, and those are
different things.

---

## 4. Why it never runs out of work

The user's requirement — *"in theory it shouldn't ever run out of work and could
go forever"* — is achievable, and it is worth being precise about why, because
the trivially-true version of it ("loop forever") is worthless.

### 4.1 Four supplies, four clocks

| Supply | Refills when | Terminates? |
|---|---|---|
| **Queue** — open issues in the tracker | A human or an agent files one | Yes, drains |
| **Audit** — the aperture ladder | A lens opens against a moved baseline (§4.2) | No |
| **Regression** — witnesses for closed work | Any merge can break any past fix (§4.3) | No |
| **Meta** — the loop's own pathologies | The ledger grows | No |

Only the first drains. That is the entire termination story, and it is the
reason `sweep` has three verbs and not one.

### 4.2 The ladder re-arms against a moved baseline

Today `advance` walks `LENSES` and returns `WATCH` when the ladder is exhausted;
`watch` then sleeps until `main` moves, a defect is filed, or CI goes red. That
is correctly non-terminating and it is *low-yield*: the ladder never reopens on
its own.

The fix is small and follows from what the ladder already means. A lens dry at
commit *X* says nothing about commit *X+200* — "no defects" is a statement about
a lens **and a baseline**. So:

- The dedup set is keyed `(digest)` globally, as today, so re-passing a lens
  **cannot re-file** anything already seen. This is what makes re-arming safe,
  and it is why the digest's byte-for-byte contract (`finding_digest`) must not
  be touched.
- The ladder resets to `rubric` when the baseline has moved by a declared
  threshold since the sweep that exhausted it — *N* merged commits or *D* days,
  recorded as `last_clean_head` in `calibration.json`, which is already carried
  through `Calibration::extra` and was already load-bearing enough that dropping
  it broke watch mode once.

A re-pass therefore yields exactly the findings the new code introduced, and
nothing else. That is not make-work; it is the only honest reading of what a dry
lens ever meant.

### 4.3 A closed issue is a claim with an expiry

This is the half the user singled out — *"re-auditing and verifying work is
super important"* — and it is the strongest available answer to the
[`doc:agent-native-delivery`](agent-native-delivery.md) §10.1 objection that an
agent inflates a backlog faster than it drains one.

When the loop closes an issue it records a **receipt** naming the witness test
that proved the fix (the pipeline already authors one and tracks its fail→pass
flip). `sweep regress` re-runs those witnesses against current `main` on a
schedule. A witness that goes green→red is a **regression**, filed as a new
issue with the closed issue as its parent.

Three things fall out, and all three are worth more than the sweep costs:

1. **New work with a guaranteed truth value.** A regression finding is not a
   model's opinion — a test that passed and now fails is a fact. Compare an
   audit finding, which is a claim needing triage.
2. **Verification of the loop's own past claims.** Every `done` this loop
   emitted becomes falsifiable *after the fact*, which is the only way a
   perpetual loop can be trusted to have been right earlier.
3. **A measurement of the witness discipline itself.** An issue closed with no
   witness cannot be swept. The count of unsweepable closures is a direct,
   unflattering metric of how often `done` was a claim rather than a proof, and
   it should be published rather than smoothed.

### 4.4 The honest counter-argument

**Infinite supply is not infinite value, and this design can produce a very
expensive make-work machine.** The failure mode is real, it is named in
[`doc:agent-native-delivery`](agent-native-delivery.md) §10.1, and no mechanism
above prevents it on its own. Three mitigations, none of which is a solved
problem:

- **`NOISY` already exists and must gate widening, not just report.** The signal
  fires when the loop files far more than it discovers. Under `NOISY` the
  policy must *narrow* — stop re-arming the ladder, drain the queue only —
  rather than open another lens. Today `metrics` reports it and nothing acts on
  it, which is invariant 10's shape of defect (an emitted signal with no
  consumer) sitting in the loop's own machinery.
- **Yield is the metric, not throughput.** Merged-PRs-per-dollar and
  regression-catches-per-sweep. A loop optimizing cycles completed is measuring
  its own exhaust.
- **Decay.** An agent-filed issue nobody pulled in *D* days is evidence the
  filing was wrong. §10.1's decay is a dependency here, not an optional extra.

I do not think these are sufficient, and this is the part of the design I would
most want measured before the loop is trusted unattended for long stretches.
§9's B4 therefore ships the sweep behind a per-supply switch that defaults to
queue-only.

---

## 5. Re-audit, and what makes a `done` falsifiable later

§4.3 covers regression of *fixed* work. The other direction — work claimed done
that was never really done — is [`doc:agent-native-delivery`](agent-native-delivery.md)
§7's residue gate, and this design depends on it rather than restating it.

The one thing worth adding here is *why it matters more for an autonomous loop
than for a human-driven run*: a human reading "…tests pass, though the 429 path
still isn't handled" files it or remembers it. An unattended loop has neither
option — the transcript is the least durable surface in the system, and there is
no human at the end of the run. **For a perpetual loop, the residue gate is not
hygiene; it is the only thing standing between the loop and quietly destroying
its own findings at the rate it produces them.** That makes B5 non-optional in a
way §11's phasing (where it is P4 of 8) does not convey.

---

## 6. The authority envelope

An unattended process that pushes branches, merges PRs, and spends money needs
its authority written down, bound, and revocable. Four parts.

### 6.1 The grant must actually bind

`plugins/stella-selfdriving/plugin.toml` declares `bash` / `write_file` /
`process_spawn` at destructive/high, and **no `AuthzGate` rule is derived from
it under `Principal::Plugin`** — the defect #3546 was triaged P1 for. A consent
document making a promise the enforcement plane never received is worse than no
document. This is B0 and it gates everything else: nothing in §3 should run
unattended before the grant it consented to is enforced.

### 6.2 Budget

Today: one environment variable, `SELF_DRIVING_BUDGET_USD`, per cycle. That is
not enough envelope for a loop that never stops. Required: per-unit, per-cycle,
and per-day ceilings; all three consulted only at safe boundaries (invariant 6);
all three recorded in the ledger so `metrics` can fold spend against yield.

### 6.3 Concurrency

Issue claims extend the fleet ledger's cooperative lease from paths to issue
keys, exactly as [`doc:agent-native-delivery`](agent-native-delivery.md) §10.4
prescribes. A tracker's assignee field is not a lock; the ledger's lease is one,
it is cross-process, and it expires on its own.

### 6.4 The stop, and the release question

Three independent stops, because the operator will not always be at the machine
that runs the loop:

1. **Signal** — `SIGTERM` to the supervisor, halting at the next safe boundary.
2. **File** — a sentinel in the state directory, for a machine an operator can
   reach but not attach to.
3. **Tracker** — a declared label on a declared issue. This is the one that
   matters, and it follows from
   [`doc:agent-native-delivery`](agent-native-delivery.md) §2.2: in an
   agent-only shop the tracker is where a human steers, so it must be able to
   stop the loop, not merely describe work to it.

**Releases stay human by default, and this is a deliberate refusal.** A release
is the one action in this document that is irreversible, externally visible, and
not fixable by another cycle. `scripts/release.sh` stays the path. An
autonomous release arm is B7, opt-in per workspace, and even then gated on
conditions the loop cannot fake: `main` green, the post-merge canary quiet for a
declared window, and a changelog derivable from merged PRs. I would ship B0–B6
and leave B7 unbuilt until the loop has a measured track record, and I would
rather say that now than discover it from a bad release.

---

## 7. Curation: the loop may propose any authority, and grant itself none

The user's ask includes the loop "curating its own set of tools and context
records and skills". The mechanism should be sharp about one line:

> **The loop proposes. A declared authority accepts. The loop is never the
> authority for its own grant.**

This is not a hedge — it is the same principle `stella-pipeline` already
enforces structurally, where `Roster::apply` rejects `Verdict` and
`DistressGuidance` as `NotAssignable` so that *no configuration* can put a model
back in the judgement seat (#2584). Curation is the same shape: the loop
generating a proposal is fine and valuable; the loop deciding that the proposal
is good is a model grading its own homework, and the repository has already
decided that pattern is not acceptable.

What each proposal is, and what accepts it:

| Proposal | Evidence that motivates it | Accepted by |
|---|---|---|
| **Custom tool** (`.stella/tools/*.toml`) | The same shell incantation reconstructed *N* times across cycles, from the ledger | Deterministic rule + human in `regulated` |
| **Context record** (`.stella/rules/*.toml`) | A recurring correction — the loop learned the same constraint repeatedly | **Human always in `regulated`.** This repo *is* `regulated`, and `promotions.jsonl` is a hash-chained ledger verified by `stella context validate` in CI. An autonomous promoter must not weaken that. |
| **Skill** (`.stella/skills/<slug>/SKILL.md`) | Recurring reflection lessons sharing a digest | Deterministic rule (already the shape skill mining uses) |

Two consequences worth stating plainly. First, in a `regulated` workspace the
loop is **not** 100% autonomous over its own context, and that is correct rather
than a limitation to engineer away — the governance mode exists precisely to put
a human on this. Second, "100% autonomous" is therefore achievable for
*delivery* (find → fix → prove → ship → file) and deliberately not for
*self-modification of its own steering*. If that trade is unacceptable, the
place to change it is the governance mode, explicitly, in a PR a human reads —
not by giving `curate accept` a broader default.

---

## 8. What buys a model call, and what does not

The pipeline's discipline, carried forward without relaxation:

| Decision | Deterministic | Model |
|---|---|---|
| How big is this cycle | `plan_cycle` | — |
| Which issue is next | `rank_defects` over the readiness queue | — |
| Is this finding new | `finding_digest` | — |
| What is the next PR action | `deliver next` | — |
| Has the ladder gone dry | `dry_streak` | — |
| Is the work done | flip oracle + `ladder_decision` | — |
| Should the loop halt | `step` | — |
| **Fixing the code** | — | ✔ `work` |
| **Writing the witness** | — | ✔ (verifier resolution, never the worker) |
| **Writing an issue body** | — | ✔ |
| **Interpreting an audit lens's output** | — | ✔ where the lens is `ModelOnly` |

Every row in the left column that could plausibly have been a model call and is
not is a place this loop is cheaper and more replayable than the alternative.
That is the product argument, and it is worth protecting when a future phase is
tempted to ask a model something the arithmetic already answers.

---

## 9. Implementation plan

Each phase is independently shippable, independently valuable, and named with
its witness — a test that fails on `main` and passes with the change.

| Phase | Deliverable | Witness | Unblocks |
|---|---|---|---|
| **B0** | **Bind the grant.** Derive `AuthzGate` rules from an installed manifest under `Principal::Plugin`; `plugins/stella-selfdriving` enforced, not merely rendered. Closes the real half of #3546. | A plugin whose manifest omits `process_spawn` is *denied* the spawn at runtime, not merely undocumented. | everything unattended |
| **B1** | **The issue port.** `Issue` kernel in `stella-protocol`; `IssueProvider` port; GitHub as a shipped manifest under `.stella/issues/`; `backlog` verbs; `queue` → deprecated alias. `HOST_SURFACE_VERSION` → 2. | The ranked queue is produced against a fixture provider with **no `gh` on `PATH`**. | "any issue provider" |
| **B2** | **`work` + the loop step machine.** `LoopStep`/`step` pure in `stella-autonomy`; `work start/status/abandon` in `stella-cli`; the plugin becomes a policy loop over declared verbs; the eight slash commands and `scripts/self-driving.sh` retire **only after** `scripts/test-self-driving.sh` is green against the new path with every assertion intact. | One issue goes from `backlog next` to a verified diff with no Claude Code and no human. | the headline |
| **B3** | **`deliver`.** `PrState` pure; open/observe/next/merge; `Escalated` reachable and terminal; `CiRed` vs `BaseBroken` distinguished. | A PR whose CI is red *on its base branch* transitions to `BaseBroken`, and the loop does not push a fix. |  PR rhythm (#2374's named weakness) |
| **B4** | **Supply.** Ladder re-arm on baseline delta; `sweep regress` over closed-issue receipts; `sweep meta`. Per-supply switch, default queue-only. | A lens dry at `HEAD` re-opens after the declared baseline delta and yields **only** digests absent from `seen.txt`. | never runs out |
| **B5** | **The residue gate** ([`doc:agent-native-delivery`](agent-native-delivery.md) §7) in `warn`, plus fingerprint dedup and decay. | A run stating a follow-up in prose and claiming `done` fails the gate; the same run with the item `filed` passes. | filing is a guarantee |
| **B6** | **`curate`.** Proposals from ledger evidence; acceptance gated on declared authority; `regulated` keeps the human on context records. | A skill proposal reaching the recurrence threshold is *proposed* and, under `regulated`, **not** applied. | self-curation |
| **B7** | **`release`,** opt-in, gated on green `main` + quiet canary + derivable changelog. | Deliberately deferred — see §6.4. | shipping |

**Landing map** (each crate's boundary decides, not convenience):

| Concern | Crate |
|---|---|
| `LoopStep`, `step`, `PrState`, supply model, re-arm rule | `stella-autonomy` (pure; new modules — the crate has no god files and must keep none) |
| `Issue`, `IssueState`, `IssueClass`, `PrId`, receipts | `stella-protocol` (types only) |
| Residue detection, discharge rules | `stella-core` (pure, no I/O) |
| The residue gate stage | `stella-pipeline`, beside `verify`/`witness` |
| `backlog`/`work`/`deliver`/`sweep`/`curate` verbs, the forge adapter, provider manifests | `stella-cli` (`self_driving_cmd/` — note `self_driving_cmd.rs` is 1005 lines and new logic lands in siblings) |
| Issue claims | `stella-fleet` ledger |
| The policy loop | `plugins/stella-selfdriving/` |

**Sequencing note.** B0 → B1 → B2 is a hard chain. B3 and B4 are independent of
each other and both need B2. B5 is independent of everything after B1 and could
land early if the residue gate is wanted sooner than the autonomy.

---

## 10. Failure modes this introduces

| Mode | Mitigation | Honest residual risk |
|---|---|---|
| Backlog inflation | §4.4: `NOISY` gates widening, decay, yield metric | **Unsolved.** The mitigations are untested at scale. |
| The loop pushes a bad fix repeatedly | Repeat-failure counting in `PrState`; `Escalated` terminal | Low |
| Spend runs away overnight | §6.2 three-tier ceilings, ledger-recorded | Low, once built |
| Two workers on one issue | §6.3 ledger lease | Low — the primitive exists |
| Tracker text as instruction | Issue text enters as data, never `directive` ([`doc:agent-native-delivery`](agent-native-delivery.md) §10.2) | Low |
| Gate-gaming — the loop stops saying the sentences that trip the residue gate | §10.5's structural Pass 2; suppression telemetry | **Partly mitigable only** |
| The loop merges something that breaks `main` | `main-canary.yml` files an issue; `sweep regress` catches the witness | Medium — the canary is post-merge by construction |
| A regression sweep that is mostly flaky tests | Flakes are a finding about the test suite, filed as such | Medium |

---

## 11. Open questions

1. **Does `work` reuse `stella run` or the pipeline API directly?** Spawning a
   child gives process isolation and the existing budget/supervision path;
   linking gives checkpointing and cheaper context reuse. Lean: spawn at B2 for
   the isolation, revisit once measured.
2. **Baseline-delta threshold for the ladder re-arm.** *N* commits or *D* days
   is a guess, the same guess §12.5 of the companion admits about decay. It
   should be derived from observed finding yield per re-pass, once B4 has data.
3. **Does the plugin or the binary own the retry ceiling?** Policy says plugin;
   but a ceiling the plugin owns is a ceiling a forked plugin can remove.
4. **Multi-repo.** One loop draining several repositories' backlogs is a natural
   next ask and is not designed here.
5. **What happens to `/self-driving:*` slash commands after B2?** They become a
   second, divergent implementation the moment the plugin can drive. Deleting
   them is right; the migration for people who use them today is not designed.
6. **Is `sweep regress`'s cost justified?** It re-runs tests that passed. The
   answer is empirical and B4 should be built to measure it, not to assume it.

---

## 12. Non-goals

- **Building a tracker.** Stella writes to the customer's.
- **Replacing the human as the steering authority.** §6.4, §7.
- **Autonomous release by default.** §6.4.
- **A new definition of done.** The pipeline's verdict is the only one.
- **Project management.** The loop drains a readiness queue; what belongs in the
  queue stays a human's call.
- **Making the loop unstoppable.** `Halt` is a first-class outcome.
