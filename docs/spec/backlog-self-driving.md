---
id: backlog-self-driving
title: "Backlog Self-Driving — the loop that owns a queue, not a task"
status: living
---

# Backlog Self-Driving — the loop that owns a queue, not a task

Status: **B0 built, B1–B7 designed and unbuilt.** Phases in §9. The driver
channel §3.0 specifies exists — `stella_plugin::driver` is its wire half,
`stella_runtime::wrapper::DriverCallGate` its host half, and
`plugins/stella-selfdriving/plugin.toml` declares the `[driver]` grant a human
reads at install. **No capability on it is served yet**: every one of the
eighteen verbs §3.1–§3.5 names answers `unsupported` and the driver degrades,
which is what B1–B6 land family by family.

**Reads on top of:** [`doc:pipeline-as-plugins`](pipeline-as-plugins.md) §10
(self-driving is a *host*, not a wrapper — that decision is upstream of this
document and is not reopened here); [`doc:wrapper-socket`](wrapper-socket.md)
§6b (the host-call channel — *"a plugin may ask, never reach"* — whose second
dispatch context is the mechanism this design turns on, §2.1); and
[`doc:agent-native-delivery`](agent-native-delivery.md) (the issue kernel, the
provider manifests, and the residue gate — designed, unbuilt, and a hard
dependency of §3.1 and §5).

**Tracked by:** #3599 is the epic. #3546 (Track D remainder) is the nearest open
work: its `AuthzGate` half is closed structurally by B0 (§2.1) and its
"move the shell driver" half is subsumed by B2. Epic #1280 (agent-native delivery) was closed
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

The loop is not missing a feature, and it is not missing a plugin — it already
is one. It is missing a **channel**: since #3590 the plugin platform has a
well-designed way to hand a capability to a plugin that participates in a turn,
and no way at all to hand one to a plugin that *drives* turns. Self-driving is
the second kind, so the only route left to it is a CLI verb surface that does
bookkeeping and nothing else.

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
| Self-driving as a plugin that actually drives | #3546, [`doc:pipeline-as-plugins`](pipeline-as-plugins.md) §10 D6 | Manifest exists (`plugins/stella-selfdriving/plugin.toml`) and now declares a `[driver]` grant a human reads at install. It still starts nothing (B2), and every capability it declares answers `unsupported` (B1–B6) — but it **can** hold one, which it could not before B0. |
| The host-call channel: `recall`, `child_turn`, `run_test`, `candidate_fanout`, `adopt_candidate` | `crates/stella-plugin/src/host_call.rs`, `crates/stella-runtime/src/wrapper/host_call.rs` | **Built** (#3590), and reachable only from inside a turn. |
| The driver channel: the same apparatus, opened by a driver session | `crates/stella-plugin/src/driver.rs`, `crates/stella-runtime/src/wrapper/driver_call.rs` | **Built** (B0). A driver at grade `none` holds capability through `[driver]`, and an undeclared ask is refused with a code rather than a death. Serves no verb yet. |

One correction to the record, because a design doc is a claim and not a fact:
[`doc:agent-native-delivery`](agent-native-delivery.md) §11.1 used to land the
provider transports in `stella-tools` "generalizing `issue_ops.rs`", a file
that has never existed in this tree — `find . -name 'issue_ops*'` is empty.
That phase starts from nothing, not from a refactor, which makes B1 below
larger than §11.1 implied. §11.1 and §4.5 now say so (#3600).

### 1.3 Never designed, and absent

| Capability | Evidence of absence |
|---|---|
| **Any verb that does delivery work.** All 18 rows of `HOST_SURFACE` are bookkeeping: size a cycle, read state, stamp a phase, record a finding. None of them fixes anything, opens anything, or ships anything. | `crates/stella-autonomy/src/surface.rs` |
| **A provider-agnostic issue port.** `gh` is invoked literally, in the binary, to rank the queue. | `crates/stella-cli/src/self_driving_cmd.rs:378`, `:389`, `:889` |
| **PR management of any kind** — open, watch CI, answer a review, resolve a conflict, merge. | No `gh pr` and no `create_pull_request` anywhere under `crates/` or `scripts/` outside TUI test fixtures. |
| **Autonomous release.** `scripts/release.sh` is a human-driven script that refuses a tag that already exists. | `scripts/release.sh:143` |
| **Structural self-curation.** Tools, context records, and skills are curated by a human or by a prompt, never proposed from loop evidence. | `/self-driving:evolve` is `scripts/self-driving/commands/self-driving/evolve.md` — prose. |
| **A work generator.** The ladder terminates *into* `watch` and stays there until an external event. Nothing manufactures the next question. | `advance` returns `WATCH` once `LENSES` is exhausted (`crates/stella-autonomy/src/lib.rs:340`). |
| **Any way for the self-driving plugin to ask the host for a capability.** The host-call channel exists and carries exactly the right capability (`child_turn`), and self-driving is structurally excluded from it. | `LoopGrant::permits_call` (`crates/stella-plugin/src/manifest.rs:213`) requires `participation >= Steering`; `plugins/stella-selfdriving/plugin.toml` declares `participation = "none"`. §2.1. |

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

### 2.1 The plugin platform grants capability only to *in-turn* plugins

> **Resolved by B0.** This section is the diagnosis as it stood before the
> driver channel existed, kept because the argument for the channel's shape is
> the argument for why the `Participation` ladder was the wrong place to fix
> it. What follows is now history for the first half — `[driver]` grants
> capability to a plugin at grade `none` — and still live for the second: the
> channel exists and serves nothing yet.

Self-driving already **is** a plugin. `plugins/stella-selfdriving/plugin.toml`
exists, [`doc:pipeline-as-plugins`](pipeline-as-plugins.md) §10 chose
*host* over *wrapper*, and #3546 tracks finishing the move. So "it should be a
plugin" cannot be the missing step. The useful question is the next one down:
**a plugin gets capability from the host by what mechanism, and does
self-driving qualify?**

Since #3590 there is a precise answer, and it is the whole diagnosis.

**The mechanism is the host-call channel** (`crates/stella-plugin/src/host_call.rs`),
whose principle is exactly right: *a plugin may **ask** the host for a
capability, and may never reach for one.* It is a closed enum of three
capabilities, and one of them is `child_turn` — "a bounded turn at a declared
role intent… the host resolves it, carves the budget, runs the turn and settles
once", performed over the host's own sub-agent dispatcher so the plugin holds no
provider and no credential. **That is the "do the work" capability**, already
built, already gated, already on the wire.

**Self-driving is structurally excluded from it, twice over.**

```rust
pub fn permits_call(&self, call: HostCall) -> bool {
    self.participation.includes(Participation::Steering) && self.calls.contains(&call)
}
// crates/stella-plugin/src/manifest.rs:213
```

1. **By grade.** `plugins/stella-selfdriving/plugin.toml` declares
   `participation = "none"`, and `None.includes(Steering)` is false. Every host
   call it could ask for is refused before the manifest's `calls` list is even
   consulted.
2. **By construction, which is the deeper one.** A host call may be made "during
   `before_turn` or `after_turn` and nowhere else". Those are wrapper points —
   they happen *inside a turn*. Self-driving is a host: it never runs inside a
   turn, it decides which turns exist. Raising its participation grade would not
   help, because there is no turn for it to be inside of.

So the `Participation` ladder — `none` → `observer` → `steering` → `arbiter` —
is a ladder of **in-turn influence**, and self-driving is not on it at all.
`none` is the honest grade for what the loop does and a dead end for what it
needs. The plugin platform, as built, has a rich and well-designed way to hand
capability to a plugin that participates in a turn, and **no way at all to hand
capability to a plugin that drives turns.**

That is why the judgement half ended up as eight Claude Code slash-command
markdown files. It is not that somebody chose badly; it is that the only channel
into the host is a channel a driver cannot stand in. The remaining route is the
CLI verb surface — `HOST_SURFACE`, 18 verbs, all of them bookkeeping: size a
cycle, read state, stamp a phase, record a finding. Not one fixes, opens, or
ships anything. A driver restricted to it can size a cycle it has no way to run.

**The correction, then, is not to package self-driving as a plugin — it is
already one — but to give the plugin platform a second channel: the same "may
ask, never reach" discipline, for out-of-turn drivers.** §3 is that channel.

Before the phases, here is what changes what this work costs:

- **It is smaller than it looks.** The wire shapes, the refusal-as-a-value
  discipline, the manifest gate, the consent rendering, and the subprocess
  transport all exist and are exercised by `stella-research`. The driver channel
  is a second dispatch context over the same machinery, not a second platform.
- **It closes #3546's real defect as a side effect rather than as separate
  work.** That issue's P1 half is that the declared grant binds to no
  `AuthzGate` rule, so consent promises what nothing enforces. A driver channel
  gated by `permits_call` inverts that: capability arrives *only* through the
  grant, so there is no unbound path left to forget to bind.

### 2.2 The split this commits to: **the plugin decides, the binary does**

One level up from the split the codebase already runs on (`stella-autonomy`
decides, `stella-cli` probes):

| | Owns | Why there |
|---|---|---|
| **`stella-autonomy`** (leaf, pure) | Cycle plan, ladder, dedup, AIMD, and — new — the loop step machine (§3.7), the PR state machine (§3.3), and the supply model (§4). | Property-testable over owned data. A governor that reads the machine itself cannot be handed a fake in a test. |
| **`stella-cli`** (the binary) | Every new verb in §3. The issue port, the run execution, the git/forge effects, the curation writes. | Invariant 2 keeps all of it out of the engine; invariant 1 keeps the forge behind a port. |
| **`plugins/stella-selfdriving`** (the driver) | *Policy only*: when to run, how many at once, in what order, when to stop, what to escalate. A loop that asks for declared capabilities and nothing else. | It is the piece an operator should be able to fork, replace, or write in another language without forking Stella. |

The consequence that everything else depends on: **the judgement half becomes
Stella's own agent loop.** `work` (§3.2) runs a unit of backlog through Stella's turn loop — the
same path a `stella run` takes, with whatever the installed plugins add over it
and nothing else. If Stella's
loop is not good enough to drive Stella's own backlog, that is a finding about
Stella, and it should be measured rather than routed around by borrowing
somebody else's agent. Today it is routed around.

---

## 3. The driver channel, and the five capabilities on it

### 3.0 The channel

A second dispatch context for the host-call channel, for plugins whose
`participation` is `none` because they drive turns rather than sit inside one.
Same wire, same closed enum, same `permits_call` filter, same
refusal-as-a-value discipline — the *only* change is that the conversation is
opened by a **driver session** rather than by a wrapper point:

```text
host  → { "point": "drive", "body": { "session": "…", "state": { … } } }
plugin→ { "call": "backlog_next", "id": 1, "args": { "limit": 5 } }
host  → { "result": 1, "ok": { "issues": [ … ] } }
plugin→ { "call": "work_start",   "id": 2, "args": { "issue": "…" } }
host  → { "result": 2, "ok": { "verdict": "…", "diff": … } }
plugin→ { "point": "drive", "body": { "next": { "sleep": { "secs": 900 } } } }  ← ends it
```

Three properties carried over deliberately, because each already earned its
place:

- **A refusal is a value.** A driver denied `deliver_merge` degrades to opening
  the PR and stopping, and says so. It is not killed mid-cycle.
- **Capability arrives only through the grant.** `permits_call` is the one
  filter, so consent and enforcement are the same object — §2.1's last point.
- **The plugin holds no credential.** Every model call, every forge write, every
  test run is the host's, performed on request. A driver that could reach
  directly would be ambient authority wearing a manifest.

The `Participation` ladder is not extended and not renumbered. Driving is a
different axis from in-turn influence, and conflating them would make `arbiter`
— already "the strongest grant" over a turn's completion — silently also mean
"may merge to `main`". A separate `[driver]` block, with its own declared call
list, keeps the two consents legible to the human reading them.

**`HOST_SURFACE` is not widened.** The 18 CLI verbs stay exactly as they are and
keep serving what they were built for — an operator, a shell driver, the
Observatory. Delivery capability arrives on the channel instead, which is why
`HOST_SURFACE_VERSION` moves only once, at B1, and only because `queue`'s row
shape changes (§3.1). This is a change of mind from an earlier draft of this
document, which proposed adding all five groups as CLI subcommands; the channel
is better on every axis that matters — gated by construction rather than by a
guard somebody must remember to write, refusable without dying, and already
built.

Each of the five below is a variant on the driver channel's call enum, declared
in the manifest's `[driver] calls` list, and refused with a `HostCallRefusal`
code the driver can branch on when undeclared.

**The `args` and `ok` tables in the sketch above are illustrative, and B0 ships
neither.** A call is `{"call": …, "id": …}` and a served answer is
`{"result": …, "ok": {}}`, because no verb is implemented yet and an argument
table written ahead of the code that reads it is a wire contract nothing can
be checked against — it would also change at the phase that implemented it, so
the channel would break once per family. Each verb brings its own arguments and
its own result, typed, in the phase that serves it. What B0 fixes in place is
the part consent depends on: which capabilities exist, which of them a driver
declared, and what a driver is told when it asks for one it did not.

### 3.1 `backlog` — the issue port

The provider-agnostic replacement for three hardcoded `gh` call sites. Built on
[`doc:agent-native-delivery`](agent-native-delivery.md) §3–§4 without
modification: four states, four classes, title, description, comments, parent
edge, and everything else in a source-tracked TOML manifest under
`.stella/issues/`.

| Call | Returns | What it does |
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

### 3.1a The backlog convention — learned, never assumed

`backlog file` is what makes "nothing left behind" a guarantee rather than a
habit. But a tracker is not a bag of text: every repository the loop attaches to
already has a classification scheme and a writing standard, and **filing into it
wrongly is worse than not filing at all** — the queue the loop ranks is the
queue the loop just polluted.

**The defect is a feedback loop, and it is live in this repository today.**
`rank_defects` counts an issue as a defect if it carries `bug` **or** `triage` —
an untriaged issue is a defect nobody has classified yet. Meanwhile
`.github/workflows/issue-triage.yml` adds `triage` to any issue opened without
one of its `TYPE_LABELS`, and the comment above that rule names the exact case:
*"Issues opened outside the template (`gh issue create`, the API) carry no labels
at all — those are exactly the ones this catches."* A loop filing through the API
**is** that case. So an unlabelled filing is stamped `triage` by the workflow and
read back next cycle as an untriaged defect the loop is responsible for
triaging. The loop manufactures its own queue, and every measure of whether it is
gaining or losing ground on the backlog silently includes its own exhaust.

**Three sources, in precedence order**, because two of them will disagree and the
tie has to break the same way every time:

| Source | What it is | Why it ranks here |
|---|---|---|
| **Enforced** | Automation acts on it — `issue-triage.yml`'s `TYPE_LABELS`, the `triage` lifecycle | The only source that is a *fact* rather than a claim. A workflow that stamps an untyped issue is not describing a convention, it **is** one — it will do that to the loop's filings whatever any document says. |
| **Declared** | Issue templates, the label set, a `CONTRIBUTING` section, the handoff format in `scripts/self-driving/commands/self-driving/tickets.md` | Authoritative about intent, silent about practice. It may describe a rule nothing enforces and everyone ignores. |
| **Observed** | The distribution over recent issues | The only source that is a guess. Fills an axis the two above leave undefined; never overrides either. Recorded as `Observed` precisely so a human can see which parts nobody actually stated. |

The result binds as a source-tracked **convention manifest** beside the provider
manifest under `.stella/issues/`, for the same reason that one is tracked: a
convention that was *inferred* must be readable by a human before it steers a
filing. **The loop never infers and files in one motion.**

**Conform or refuse, never best-effort.** A filing that cannot be classified
under the bound convention is an error, not a degraded post. That is the whole
mechanism — it is what stops the loop poisoning the queue it ranks, and it is why
`conform` returns every violation at once rather than the first: a filing refused
one reason at a time is a filing refused four times.

**Where no standard exists, the loop proposes one and grants itself nothing.** A
proposed convention is **inert** — `conform` refuses every filing against it,
including one that would be conformant if the convention were bound. This is §7's
rule pointed at classification: a loop that invented a taxonomy and then filed
under it has granted itself the authority to define how this repository
classifies work. Acceptance is a human's, and under governance mode `regulated`
it stays one.

The model is portable and this repository's vocabulary is not baked into it:
axes (`type`, `priority`, `area` here), each with membership, a requirement
(`ExactlyOne` / `AtMostOne` / `Any`) and its source; plus **reserved** labels the
loop may never apply itself. `triage` is this repository's one reserved label —
it marks an issue that arrived from outside without a type, and the loop knows
what it found, so applying it would be claiming to be a stranger to its own
filing. Note the interesting failure is not "missing" but **ambiguous**: two
priority labels on one issue is not a stricter filing, it is a filing whose rank
is decided by whichever label the ranker happens to test first.

Lands in `crates/stella-autonomy/src/convention.rs` — pure, and over borrowed
label text rather than a `stella_protocol::Issue`, so the crate keeps the
no-workspace-dependency property that lets the Observatory link its folds. That
is the same trade `rank_defects` already makes, and the one mapping from a
tracker's shape into this one lives in the caller.

### 3.2 `work` — one unit of backlog, through Stella's own loop

| Call | Returns | What it does |
|---|---|---|
| `work start --issue <key>` | `json` | Resolve context for the issue, create the isolated worktree, run Stella's turn loop against it — with whatever the installed plugins wrap it in — and return the outcome and the diff. |
| `work status` | `query-envelope` | The in-flight unit: stage, spend, elapsed, checkpoint. |
| `work abandon` | `text` | Release the claim and the worktree, recording why. Abandonment is a *recorded outcome*, never a silent drop. |

This is the verb that does not exist today in any form, and it is the whole
autonomous half. Three properties it must have, each of which is an existing
mechanism rather than new invention:

- **Isolation is `candidate_ws.rs`'s shadow worktree**, not a branch checkout of
  the operator's tree. The loop must be able to run while a human works in the
  same clone.
- **Done is whatever the installed plugins make it**, and this design invents no
  definition of its own. The staged pipeline that once ran the flip oracle,
  tamper exclusion and `ladder_decision` is **deleted from this workspace**
  (#3852/#3865) and `--pipeline classic` is refused outright, so there is no
  built-in verdict left to inherit. Self-driving therefore gets exactly what a
  `stella run` gets on the same machine — no weaker, and no new terminal state —
  which with no verification plugin installed is the turn's own outcome.

  **Say the consequence out loud rather than letting the old sentence imply
  otherwise:** on a workspace with no verification plugin, a `work` outcome means
  *the turn finished*, not *the change is proven*. What makes an autonomous merge
  safe is not this stage — it is `deliver` (§3.3), which gates on CI observed on
  the forge. That is a weaker guarantee than the deleted pipeline offered and it
  is the true one; a design doc that kept promising the stronger one would be
  the expedient this repository treats as a defect.
- **Abort is at safe boundaries** (invariant 6). A budget ceiling reached
  mid-tool waits for the tool.

### 3.3 `deliver` — branch, PR, CI, review, merge

| Call | Returns | What it does |
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

| Call | Returns | What it does |
|---|---|---|
| `sweep audit` | `json` | Run the open lens's tooling, digest the findings, emit only the ones not in `seen.txt`. |
| `sweep regress` | `json` | Re-run the witnesses named by receipts on issues this loop closed; report any green→red. |
| `sweep meta` | `json` | Fold the ledger; emit each named pathology as a fileable finding against the loop itself. |

§4 is the argument for why these three, and no fewer.

### 3.5 `curate` — tools, context records, skills

| Call | Returns | What it does |
|---|---|---|
| `curate propose` | `json` | Emit a proposal — a custom tool, a context record, a skill — with the evidence that motivated it. |
| `curate list` | `query-envelope` | Pending proposals and their evidence counts. |
| `curate accept <id>` | `json` | Apply a proposal **if the workspace's declared authority permits it** (§7). |

### 3.6 What deliberately stays off the channel

- **No `release` call at B0–B6.** §6.4.
- **No call that edits the driver's own grant.** The loop may propose an
  authority change (§7); the channel offers no way for it to apply one. A
  capability that could widen `[driver] calls` would make the grant advisory.
- **No call that breaks or steals another worker's claim.** The lease expires on
  its own; `fleet_claims.rs` already declines to offer this and gives the
  reason.
- **No mode flags.** Invariant 9: a parameter scopes, never selects. `curate
  accept` and `curate reject` are two calls, not one with a boolean.

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
    Blocked { reason: BlockReason, clears_when: Clearance },
}

pub fn step(state: &LoopState, obs: &LoopObservation) -> LoopStep;
```

`Blocked` exists and is reachable — for a spent budget, a revoked grant, an
operator stop, or an `Escalated` PR the policy declines to abandon. **A loop
that cannot stop is not autonomous, it is unsupervised**, and those are
different things.

**But a block is a park, not a death, and it clears itself.** The loop resumes
the moment the thing blocking it goes away, and *nobody has to tell it that it
may*. There is no resume input, no acknowledgement and no operator gesture in
the contract: a human who raises the ceiling, restores the grant, drops the stop
flag or merges the escalated PR has already said everything that needs saying,
and asking them to also say "go" is asking twice.

This is structural rather than promised — **the machine holds no latch.** `step`
is a total function of the current state and the current observation, so a block
is recomputed from scratch on every poll and simply stops being returned when
its cause is gone. There is no `blocked` flag that could be left set, which
means there is no state an operator could have to clear.
`a_cleared_block_resumes_with_no_resume_signal` is the witness, and it asserts
all four reasons, because a latch on any one of them would strand the loop
exactly there.

- **Every `BlockReason` names a `Clearance`** — the observable that will let it
  go. `BlockReason::clearance()` is total, so a new reason cannot be added
  without answering "and what would un-block it?". A block with no clearance is
  by definition one that needs a human to say "go".
- **The one obligation on a host is to keep polling.** A host that exits the
  process on `Blocked` makes resumption impossible no matter what the machine
  returns, which is why the variant carries its clearance rather than reading as
  a terminal state. This is the same shape as `watch` mode: cheap sentinels, and
  the loop wakes itself.

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
  through `Calibration::extra` and was already required closely enough that
  dropping it broke watch mode once.

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

Each of the following is worth more than the sweep costs:

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

### 6.5 The doctrine — whose decision system the loop is running

Self-driving stands in for a person, and **a person deciding what to do next is
not running a universal algorithm.** They are applying a system they developed.
One maintainer's standing instruction is *"when two designs both work, ship the
one still correct in five years under a different maintainer"* — that is this
repository's, and it is written down in `CLAUDE.md`. Another's is *"ship the
smallest diff that closes the ticket"*. **Both are legitimate, and a loop that
hardcodes either is a loop that can only replace one of them.**

So the tie-breakers are configuration: a `[self_driving.doctrine]` block in
`stella.toml`, source-tracked with the rest of the workspace's configuration,
because the person who starts the loop is the person who decides how it decides —
and that is only true if what it will do is legible *before* it does it. It is
not a manifest under `.stella/issues/` beside the provider and convention ones:
everything configurable about stella comes from `stella.toml`, and a second
config system is a second place to search.

**The line is machine-readability, and it is drawn to avoid a fifth steering
plane.** This repository already carries four that do not know about each other
(#3243), and the fastest way to make it five is a free-text "how should I decide
things" field. So:

| Belongs in `[self_driving.doctrine]` | Belongs in a context record |
|---|---|
| Closed enums and numbers a **pure machine** branches on — `foreign_breakage`, `contention`, `abandon_escalated` | Prose instruction for the **judgement half** — "prefer the architecturally sound option", "match the neighbourhood", "no new dependencies casually" |
| No model ever reads it. An operator's declared preference becomes *testable*: you can assert `FileAndWait` does not adopt, and the assertion cannot be talked out of. | Already has a home (`.stella/rules/*.toml`, `doc:context-pr`), already governed, already versioned. |

The test is invariant 5's: **does a caller branch on it?** A pure machine
branching on a closed enum belongs; a sentence a model reads does not. A
`[self_driving.doctrine]` field that wants to say something prose-shaped **is** a
context record, and the answer to that request is a pointer, not a field.

Two axes this table used to name are deliberately absent, and both absences say
something about where a knob belongs. **Queue order** is not an operator's to
declare: the ranking is `backlog::ranked_keys` over
`stella_autonomy::priority::TriagePolicy`, whose `ladder`, `defect_kinds` and
`excluded_kinds` are the vocabulary an operator already has for it, and a second
one in `[self_driving.doctrine]` would be two answers to one question. **The
`deliver` ceilings** are not declared at all: `batch_ceiling` and
`parallel_ceiling` live on the AIMD `Calibration`/`Limits` types
(`crates/stella-autonomy/src/lib.rs`) and are *moved by the controller* in
response to what the loop survives. Putting them in a doctrine block would imply
an operator can pin them, which is the opposite of how AIMD works — `Limits` is
where an operator bounds the range.

#### The loop exhausts its permitted channels before it parks

A blockage is not automatically somebody else's problem. The case that matters is
a **red base the loop did not cause**: it blocks everyone, and a loop that waits
for its author has a throughput equal to somebody else's response time. But a
loop wandering into unrelated code is also how an autonomous system becomes a
liability — so this is exactly a doctrine axis, not a rule:

- `file_and_adopt` *(default)* — make it visible, then fix it at the top of the
  queue. A loop that files and waits has handed its own throughput to somebody
  else's response time.
- `file_and_wait` — make it visible, leave the fix to a human.
- `ignore` — neither. Present for the operator who wants a silent loop, and
  deliberately **not** the default: a system that notices breakage and says
  nothing is the worst of the three.

Three orderings inside that are required:

1. **File first, always, under every policy that files at all.** The ticket is
   what makes the breakage visible and attributable *even if the fix never
   lands*, so filing is never contingent on fixing being attempted.
2. **Check contention before adopting.** Two workers on one broken `main`
   produce a merge conflict on top of an outage.
3. **Unblock before claiming.** A red base makes every PR the loop produces fail
   CI, so it is dealt with before more are made.

And a red base stops *merges*, not *authoring* — so even under `file_and_wait`,
where the fix is somebody else's, the loop carries on with ordinary work once it
has filed rather than parking.

#### Collision avoidance, and the signal nobody checks

Contention evidence is **heuristic**, which is why how much it weighs is also
doctrine. Four signals, and only one is authoritative:

| Signal | Weight |
|---|---|
| **Ledger claim** | Authoritative — a lease with an owner and an expiry, held for as long as a turn is in flight. The only signal that carries a *time*, and therefore the only one that survives the ambiguity below. |
| Open PR | Strong hint. |
| Remote branch | A name that resembles the work. |
| **Worktree on this machine** | The one nobody remembers to check — and the one most likely to be the loop's *own other session*. |

`ContentionPolicy` weighs them: `defer` on any signal (the default — a duplicated
fix costs two workers and a conflict, a deferred one costs a poll interval);
`claims_only` for the solo operator whose remote is full of their own stale
branches, where deferring on branch names would deadlock the loop against itself;
`proceed` regardless, recorded and never silent. A deferral carries the evidence
that caused it, because "deferred" with no evidence is indistinguishable from
never having looked.

##### Why the loop takes a claim, and not only reads one

This section used to say that two self-driving processes against one clone see
each other **only** in the worktree signal. That was true, and it was the
defect: it made the worktree signal answer a question it cannot answer.

A crashed run leaves a registered worktree at
`.stella/private/self-driving/stella-<key>-<slug>`. So does a live peer running
the same verb against the same clone — same root, same shape, same string. Read
as a name, the two are one case, and either reading is wrong for the other: keep
them and the loop defers its own wreckage forever, because a deferral writes no
`spent` entry and `work::start`'s recovery is one step further on than the
deferral ever reaches (#4300); drop them and a live peer stops deferring, which
is the thing this section exists to prevent.

The question is *liveness*, so it is asked of the signal that carries liveness.
The loop **takes** the `issue:<n>` lease before a turn starts, heartbeats it
while the turn runs, and releases it when the turn settles. A crashed run's
lease lapses with nothing having to clean it up; a live peer's does not. That is
what lets the claim-time probe exclude the loop's own worktrees root and still
defer to a peer standing in exactly the same place — and it is what makes
`claims_only` a real policy rather than one that could never defer, since before
a producer existed nothing in a self-driving workspace ever wrote a claim for it
to read.

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

In a `regulated` workspace the loop is **not** 100% autonomous over its own
context, and that is correct rather than a limitation to engineer away — the
governance mode exists precisely to put a human on this. "100% autonomous" is
therefore achievable for *delivery* (find → fix → prove → ship → file) and
deliberately not for *self-modification of its own steering*. If that trade
is unacceptable, the place to change it is the governance mode, explicitly,
in a PR a human reads — not by giving `curate accept` a broader default.

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
| **B0** — **built** | **The driver channel** (§3.0). A second dispatch context for the existing host-call machinery, opened by a driver session rather than a wrapper point; a `[driver]` block with its own `calls` list and its own `permits_call`, *without* touching the `Participation` ladder; the grant rendered at install consent. `stella-plugin/src/driver.rs` is the wire half and `stella-runtime/src/wrapper/driver_call.rs` the host half. **A call carries no arguments and returns no payload yet** — no verb is implemented, so no argument or result table can be written honestly; each lands with the verb that needs it. `plugins/stella-selfdriving` declares its `[driver]` grant but is still not a program the host runs, which is B2. | `an_undeclared_call_is_refused_and_the_session_keeps_running` (`stella-runtime/src/wrapper/driver_call.rs`): a driver whose manifest omits a call is refused it with a `HostCallRefusal` code, is not charged for it, and **keeps running**; a declared one is served. Both directions, because either alone is half a gate. Plus `a_driver_holds_capabilities_without_a_participation_grade` (`stella-plugin/src/manifest.rs`) — the defect itself, that `participation = "none"` made every capability unreachable. | everything below |
| **B1** — *port landed* | **The issue port.** `Issue` kernel in `stella-protocol`; `IssueProvider` port; GitHub as a shipped manifest under `.stella/issues/`; the `backlog` calls on the channel; the CLI's `queue` row reshaped, `HOST_SURFACE_VERSION` → 2. | The ranked queue is produced against a fixture provider with **no `gh` on `PATH`**. | "any issue provider" |
| **B2** | **`work` + the loop step machine.** `LoopStep`/`step` pure in `stella-autonomy`; `work_start`/`work_status`/`work_abandon` served over the channel from `stella-cli`, built on the existing `child_turn` dispatcher rather than a second one; the plugin becomes a policy loop over declared calls; the eight slash commands and `scripts/self-driving.sh` retire **only after** `scripts/test-self-driving.sh` is green against the new path with every assertion intact. | One issue goes from `backlog next` to a verified diff with no Claude Code and no human. | the headline |
| **B3** | **`deliver`.** `PrState` pure; open/observe/next/merge; `Escalated` reachable and terminal; `CiRed` vs `BaseBroken` distinguished. | A PR whose CI is red *on its base branch* transitions to `BaseBroken`, and the loop does not push a fix. |  PR rhythm (#2374's named weakness) |
| **B4** | **Supply.** Ladder re-arm on baseline delta; `sweep regress` over closed-issue receipts; `sweep meta`. Per-supply switch, default queue-only. | A lens dry at `HEAD` re-opens after the declared baseline delta and yields **only** digests absent from `seen.txt`. | never runs out |
| **B5** | **The residue gate** ([`doc:agent-native-delivery`](agent-native-delivery.md) §7) in `warn`, plus fingerprint dedup and decay. | A run stating a follow-up in prose and claiming `done` fails the gate; the same run with the item `filed` passes. | filing is a guarantee |
| **B6** | **`curate`.** Proposals from ledger evidence; acceptance gated on declared authority; `regulated` keeps the human on context records. | A skill proposal reaching the recurrence threshold is *proposed* and, under `regulated`, **not** applied. | self-curation |
| **B7** | **`release`,** opt-in, gated on green `main` + quiet canary + derivable changelog. | Deliberately deferred — see §6.4. | shipping |

**B1's first slice has landed**: the [`Issue`] kernel and the
[`IssueProvider`] port (`crates/stella-protocol/src/issue.rs`), the GitHub
adapter behind it (`crates/stella-cli/src/issue_provider.rs`), and both backlog
readers moved onto the port
(`crates/stella-cli/src/self_driving_cmd/backlog.rs`). GitHub is now an adapter
rather than the answer, and the witness ranks a real defect queue from a fixture
provider that runs no subprocess and holds no credential. The remaining
`backlog` calls — `claim`, `file`, `close`, `link` — are the write half and are
deliberately absent: a write path nothing calls is the unwired code AGENTS.md's
"nothing left behind" rule exists to prevent.

One thing the slice found that the design did not anticipate: **`demand` carried
a second definition of "defect".** The governor counted its open-defect and P0
numbers with two extra `gh issue list --jq length` calls whose `--label bug`
filter was a rival to `rank_defects`'s own label rule — two definitions of one
word, in two languages, able to disagree about the very backlog the same cycle
drew its batch from. Both numbers are now folds of one ranking of one read.

**Landing map** (each crate's boundary decides, not convenience):

| Concern | Crate |
|---|---|
| `LoopStep`, `step`, `PrState`, supply model, re-arm rule | `stella-autonomy` (pure; new modules — the crate has no god files and must keep none) |
| `Issue`, `IssueState`, `IssueClass`, `PrId`, receipts | `stella-protocol` (types only) |
| Residue detection, discharge rules | `stella-core` (pure, no I/O) |
| The residue gate stage | **Homeless.** `stella-pipeline` is deleted (#3852/#3865) and this row named it; B5 has to pick a home before it can be built — a wrapper plugin's own side, or a `stella-cli` check over the turn's transcript. Tracked in #3901's sweep of the same stale claim. |
| `backlog`/`work`/`deliver`/`sweep`/`curate` verbs, the forge adapter, provider manifests | `stella-cli` (`self_driving_cmd/` — `self_driving_cmd.rs` is close enough to the 1500-line ceiling that new logic lands in siblings; it carries no baseline entry, so a crossing fails the gate rather than being grandfathered) |
| Issue claims | `stella-fleet` ledger |
| The driver channel: dispatch context, `[driver]` block, `permits_call` | `stella-plugin` (wire + gate), `stella-runtime` (`src/wrapper/`, beside the existing host-call dispatch) |
| The policy loop | `plugins/stella-selfdriving/` |

**Sequencing note.** B0 → B1 → B2 is a hard chain, and B0 is now the one the
other two require: it is the phase that makes a self-driving plugin able to
hold any capability at all. It is also the smallest of the three, because the wire shapes, the
refusal codes, the manifest gate, the consent rendering and the subprocess
transport all exist and are exercised by `stella-research` — B0 adds a dispatch
context, not a platform. B3 and B4 are independent of
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

1. **Does `work_start` reuse the existing `child_turn` dispatcher unchanged?**
   `child_turn` is specified as "a bounded turn at a declared role intent" and
   was built for a wrapper asking mid-turn; a driver asking for a whole unit of
   delivery is a longer, coarser ask against the same dispatcher. Whether that
   is one capability with a wider budget carve or genuinely a second one is the
   first thing B2 has to settle, and it decides how much of B0 is reuse.
2. **What is a driver session's lifetime, and who owns the heartbeat?** The
   wrapper channel's conversation is bounded by a turn, which gives it a natural
   end. A driver session has none — it is the thing that outlives everything. The
   loop already resolves liveness from heartbeats rather than pids (`fold_runs`),
   so the machinery exists; where it attaches on the channel does not.
3. **A `[driver]` block, or a new axis on `[loop]`?** §3.0 argues for a separate
   block so `arbiter` never silently also means "may merge to `main`". The cost
   is a second consent surface for a human to read, and two blocks that can
   disagree about the same plugin.
4. **Baseline-delta threshold for the ladder re-arm.** *N* commits or *D* days
   is a guess, the same guess §12.5 of the companion admits about decay. It
   should be derived from observed finding yield per re-pass, once B4 has data.
5. **Does the plugin or the binary own the retry ceiling?** Policy says plugin;
   but a ceiling the plugin owns is a ceiling a forked plugin can remove.
6. **Multi-repo.** One loop draining several repositories' backlogs is a natural
   next ask and is not designed here.
7. **What happens to `/self-driving:*` slash commands after B2?** They become a
   second, divergent implementation the moment the plugin can drive. Deleting
   them is right; the migration for people who use them today is not designed.
8. **Is `sweep regress`'s cost justified?** It re-runs tests that passed. The
   answer is empirical and B4 should be built to measure it, not to assume it.

---

## 12. Non-goals

- **Building a tracker.** Stella writes to the customer's.
- **Replacing the human as the steering authority.** §6.4, §7.
- **Autonomous release by default.** §6.4.
- **A new definition of done.** Self-driving takes whatever the turn loop and the
  installed plugins yield, and adds nothing of its own — see §3.2 on what that
  is worth when nothing verifying is installed.
- **Project management.** The loop drains a readiness queue; what belongs in the
  queue stays a human's call.
- **Making the loop unstoppable.** `Halt` is a first-class outcome.
