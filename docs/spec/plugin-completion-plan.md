---
id: plugin-completion-plan
title: "The five plugins that did not get built: an audit, a spec, and where each one ships"
status: proposed
---

# The five plugins that did not get built

**Status:** proposed, written 2026-08-19, against `main` at `f9789df17`.

`doc:pipeline-as-plugins` §3 names **nine plugins** that replace one staged
pipeline and one built-in autonomous loop. §7 states the bar plainly: *"a
side-by-side benchmark holds before the built-in path is deleted."* The
built-in path was deleted on 2026-08-19 (#3865). This document answers the
question that deletion makes urgent — **which of the nine actually exist** —
and specifies the work for the ones that do not.

Everything asserted about today's tree was read out of it and is cited so it
can be checked. Where a claim is an inference rather than an observation, it
says so.

---

## 0. The one-sentence answer

**Four of the nine are built and graded, one of those four cannot do its job on
any host running today, three more exist but are ungated from this repository,
and the single most important plugin in the plan — the one carrying the
project's headline claim — is an empty repository with a README in it.**

---

## 1. The audit

| # | Plugin (§3) | Where it should be | State | Evidence |
|---|---|---|---|---|
| 1 | **vera** | `oxageninc/vera`, private | **NOT BUILT** | `gh api repos/oxageninc/vera/contents` returns exactly one entry, `README.md`; repo size 3 KB; last push 2026-08-17 |
| 2 | **stella-plan** | this repo, `plugins/` | **BUILT, graded** | `plugins/stella-plan/{plugin.toml,main.py}`; `crates/stella-runtime/tests/plan_plugin_{conformance,dispatch,hostcall}.rs` |
| 3 | **stella-research** | this repo, `plugins/` | **BUILT, graded** | `plugins/stella-research/{plugin.toml,main.py}`; `crates/stella-runtime/tests/research_plugin_{conformance,dispatch,recall}.rs` |
| 4 | **stella-candidates** | this repo, `plugins/` | **NOT BUILT** | no `plugins/stella-candidates` directory exists |
| 5 | **stella-selfdriving** | this repo, `plugins/` | **DECLARATION ONLY** | `plugins/stella-selfdriving/` holds `plugin.toml` and `README.md` and nothing else — no program, no `[runtime]`, `participation = "none"`. Its own README says so: *"It is not the extraction."* |
| 6 | **stella-goal** | this repo, `plugins/` | **BUILT, graded, INERT** | `plugins/stella-goal/{plugin.toml,main.py}` and three test harnesses exist; but `crates/stella-cli/src/wrapper_plugin.rs:519` documents *"No `verifier` seat is bound"*, which is #3838 — the plugin's one job degrades to `HostCallRefusal::Unavailable` on every shipping door |
| 7 | **example-rs** | `stella-examples`, `plugins/verify-rs` | **BUILT, ungated here** | present in `macanderson/stella-examples`; workflow `plugins.yml`; **nothing in this repository references it** (`rg 'verify-rs' --glob '!docs/**'` returns no non-doc hit) |
| 8 | **example-py** | `stella-examples`, `plugins/verify-py` | **BUILT, ungated here** | same |
| 9 | **example-ts** | `stella-examples`, `plugins/verify-ts` | **BUILT, ungated here** | same |

Two qualifications on that table, both of which make it worse rather than
better.

**The three examples are stale, not merely ungated.** #3523 is open and says
so: *"Track C's three example plugins still carry the manifest redundancies
#3499/#3501 removed."* So the artefacts that exist to prove the surface is a
platform were written against a manifest grammar the host no longer requires.

**`doc:pipeline-as-plugins` §9 rule 4 is unmet.** It asks that CI run all three
*"in `stella-examples` **and as a smoke check in `stella` itself**, so a
protocol change that breaks a non-Rust plugin fails the PR that made it rather
than being discovered by a user."* The second half does not exist. This is the
same argument `plugins/README.md` makes for keeping the first-party plugins
in-tree — *"a vector living in another repository cannot fail the PR that broke
it"* — applied to Track C and left undone.

---

## 2. What the audit means, stated plainly

### 2.1 Stella ships no verification path today

This is the finding that outranks the rest, and it is a composition of three
facts each individually recorded as correct:

1. `stella run --pipeline classic` is refused
   (`crates/stella-cli/src/wrapper_plugin.rs:204`, `classic_removed_message`),
   because the built-in staged pipeline was deleted (#3865).
2. `AGENTS.md`'s opening states the only remaining verification path is an
   installed verification plugin, and names Oxagen's Vera as the reference one.
3. `oxageninc/vera` contains one file, `README.md`.

**Inference, labelled:** from 1–3 it follows that no user of any build from
this tree can obtain a witness-verified turn by any route, because the only
route requires a plugin that does not exist. I verified each of the three
premises directly; I did not exhaustively search for a fourth, unrecorded
verification path, and none appears in `plugins/` or in the `--pipeline`
resolver.

`doc:pipeline-as-plugins` §7 anticipated exactly this and recorded it as a
deliberate maintainer call — the built-in path *"had already stopped being the
default (#3381) and stopped being reachable from most surfaces"* — so the
remaining risk was judged to be *"carrying dead, unreferenced code rather than
losing a working feature."* That judgement is defensible about the **code**. It
is not a judgement about the **claim**, and the claim is what the project trades
on. §7 itself says extraction *"is still real, open work — deleting the built-in
path does not mark it done."*

### 2.2 Three of the four "built" plugins are steering-grade toys, by design

`stella-research` and `stella-plan` are `participation = "steering"`,
`before_turn` only. They are the right first extractions and they work. But
neither one is what the pipeline was *for*: no oracle, no verdict, no hold.
Only `stella-goal` reaches `arbiter`, and #3838 means its `after_turn` cannot
reach a verifier. So **no plugin in existence today exercises the `judge` /
`again` half of the socket against a real measurement.** The half of the
wrapper contract that the whole extraction was justified by is proven only by
fixtures (`crates/stella-plugin/tests/fixtures/{arbiter,perf-budget}.toml`).

### 2.3 Two epic children look stale-open, and one looks genuinely landed

Reported as observations, not as closures — I did not run the gate to confirm:

- **#3804** ("`wrapper_plugin.rs` is 1516 lines, 16 over the ceiling"): the file
  is now **1233 lines** with the tests split into
  `crates/stella-cli/src/wrapper_plugin/tests.rs`, and `rg 'wrapper_plugin'
  scripts/file-size-baseline.txt` returns nothing — so it was fixed by a split
  and not by a baseline entry, which is the outcome the issue asked for. It is
  still open.
- **#3844** ("wrapper socket has no capability for isolated, N-wide candidate
  fan-out"): `crates/stella-runtime/src/wrapper/candidate_fanout.rs` (864
  lines), `crates/stella-cli/src/candidate_workspaces.rs` (583 lines) and
  `crates/stella-runtime/tests/wrapper_candidate_fanout.rs` all exist, and
  `wrapper_plugin.rs` serves the `CandidateFanouts` plane on `stella run`'s
  door (`:491`, `:577`, `:617`). `doc:pipeline-as-plugins` §7 already says this
  landed as #3892. The issue is still open. **The capability appears present;
  the consumer — `plugins/stella-candidates` — is what is absent.**

Both want a maintainer verify-and-close rather than more work. Leaving them
open makes epic #3848 read as further from done than it is, which is the
direction that misleads in the opposite way to the usual: it hides that the
remaining blocker is *writing a plugin*, not *building a capability*.

---

## 3. Prerequisites that block more than one plugin

These are the items where the socket, not the plugin author, is the blocker.
Each already has an issue; this section states which plugin each one gates, so
the sequencing in §6 is derivable rather than asserted.

| Prereq | Issue | Gates |
|---|---|---|
| A `ChildTurns` binding serving the `verifier` role intent | #3838 | stella-goal (its only job), vera (witness authoring) |
| `stella run` raises `host_max_holds` / the `ChildTurns` ceiling to the manifest's ask | #3841 | stella-goal (`max_holds = 7`, capped at 3), vera |
| `--pipeline <variant>` composes more than one plugin's contribution | #3801 | every realistic install — research + plan + vera is three plugins, not a choice of one |
| ~~`[loop] max_calls` separates per-point from whole-run budget~~ **landed** — `[loop] max_child_turns` is the whole-run key | #3839 | vera, stella-candidates |
| A verifier's free-text reasoning reaches the next round's worker | #3840 | stella-goal, vera |
| The role table is plugin-populated (`EngineRole` closed at six) | #3472, #3492 | vera (contributes `worker` + independent `verifier`) |
| `Principal::Plugin` is constructed and a `[[capabilities]]` entry binds to an `AuthzGate` rule | #3482 | stella-selfdriving — **hard** gate, see §4.3 |
| A plugin-contributed tool can run a script the package ships (`${plugin_dir}` interpolation) | #3579 | vera, stella-candidates |

Two of these deserve to be called out as more than a checklist row.

**#3801 is the one that decides whether this is a platform.** A `--pipeline`
selection binds one manifest (`WrapperDispatch::bind`). So today a user chooses
grounding *or* planning *or* verification. The pipeline being replaced ran all
of them in one turn. Until composition lands, the plugin path is strictly less
capable than the thing it replaced, no matter how many plugins get written —
and no benchmark against the deleted path can be honest, because the plugin arm
cannot be assembled.

**#3482 is the one that decides whether the marketplace is safe.**
`doc:pipeline-as-plugins` §A1 says it without hedging: *"a marketplace shipped
on top of a system that cannot distinguish an installed plugin from its
operator grants every plugin the operator's authority."* `Principal::Plugin`
exists as a variant (#A1, landed) and nothing constructs it for a capability
binding. See `doc:plugin-marketplace` §7, which treats this as a hard
precondition on distribution rather than a nice-to-have.

---

## 4. The five specs

### 4.1 Verification: split it in two — `stella-witness` (open) and `vera` (commercial)

**Recommendation, and it is the one substantive departure from
`doc:pipeline-as-plugins` §8 in this document.** §8 plans a single private
plugin. I recommend two:

- **`plugins/stella-witness`** — first-party, AGPL, in this repository. Carries
  the *nucleus* §8 already enumerates as portable: `FlipOracle`, `FlipState`,
  `ladder_decision`, the evidence builders, `strip_witness_hunks`, the witness
  prompt construction, `parse_test_invocation`, `runner_probe`, the three
  acceptance validators, and `witness/airlock.rs`. Plus the property test §8
  says to carry over first,
  `flip_requires_a_prior_failing_observation`.
- **`oxageninc/vera`** — private, commercial. The superset: verifier-independence
  enforcement across a roster, tamper hardening beyond
  `TamperPolicy::ArtifactIdentity`, multi-oracle composition, the durable flip
  record feeding a fine-tuning corpus, org policy, and the paid support surface.

**Three reasons this beats one private plugin, in descending order of how much
they should matter:**

1. **The code being extracted is already AGPL and already public.** It shipped
   in `crates/stella-pipeline` in this repository's history until 2026-08-19.
   Moving it, and only it, into a private repository is a relicense by
   deletion. Nothing forbids the copyright holder from doing that; it is
   nonetheless the kind of move that costs a reference implementation the
   reputation it exists to build, and `CLAUDE.md` names spending that
   reputation as the worst available trade.
2. **The headline claim needs an open referent.** `AGENTS.md`'s first paragraph
   defines "verified done, not claimed done" as *"a property of the path that
   produced the evidence."* If the only such path is a paid private artefact,
   the open-source project's central claim is unfalsifiable by anyone who has
   not bought something. An open `stella-witness` makes the claim checkable,
   which is the entire argument for the project existing.
3. **It de-risks the deletion that already happened.** §7's bar — a side-by-side
   benchmark before the built-in path is deleted — can still be met
   *retroactively*, because the built-in path is readable at the commit before
   `a6d3db4f6` and can be checked out and benchmarked against `stella-witness`.
   That is the cheapest available repair of the one process failure this
   extraction has on its record.

**What the split costs:** Oxagen gives up the simple story "verification is the
paid thing." What it gets instead is the story the project can actually
defend — *verification is open; verification at organisational scale is paid* —
which is the same line every successful open-core infrastructure project draws,
and the only one compatible with §2.1 above.

**This is a maintainer's call, per `CLAUDE.md`'s "now vs. right" rule, and I am
naming it rather than deciding it.** If the answer is one private plugin, then
§2.1 needs a corresponding correction in `README.md` and `AGENTS.md`: the open
product does not verify, and should stop implying it does.

**Manifest sketch for `stella-witness`:**

```toml
name = "stella-witness"
description = "Author a failing witness test, watch it flip, and hold the turn open until it does."

[loop]
participation = "arbiter"
hooks = ["Stop"]
points = ["after_turn"]
calls = ["child_turn", "candidate_fanout"]
max_holds = 4

[requirements]
witness_flips = "A test that failed before the change passes after it."
witness_untampered = "The worker did not modify the witness files."

[oracle]
flip = "required"
tamper = "artifact-identity"

[roles.verifier]
# independent of the worker — the roster already refuses a responsibility whose
# agent is the worker's (`roster.rs:656-660`)

[runtime]
argv = ["python3", "${plugin_dir}/main.py"]
```

**Witnesses this plugin owes:**
- `flip_requires_a_prior_failing_observation`, ported unchanged (§8's own ask).
- A tamper case: a worker that edits the witness file scores `Undecided`, never
  a flip. `crates/stella-runtime/tests/host_owned_tamper.rs` is the host half
  and already passes; this is the plugin half.
- A conformance harness at `crates/stella-runtime/tests/witness_plugin_conformance.rs`,
  matching the shape of the three that exist.

**Blocked on:** #3838 (a verifier seat to author the witness in), #3841 (four
holds, not three), #3579 (`${plugin_dir}` for the tools it ships).

### 4.2 `plugins/stella-candidates` — best-of-N

**The capability is built; the plugin is not** (§2.3). This is the cheapest
remaining plugin relative to its value, and it is the one that proves the
`again?` point, which nothing currently does.

**Shape:** `participation = "arbiter"`, points `["before_turn", "again"]`,
`calls = ["candidate_fanout"]`, `[loop] max_fanout_width = N`. `before_turn`
asks for N isolated writable workspaces; each runs a full worker turn rooted in
its own `git worktree`
(`crates/stella-cli/src/candidate_workspaces.rs`); the plugin reports each
candidate's measurements as `ObservedEvidence`; `judge` picks deterministically
against declared `[[oracle.checks]]`; `again` either adopts the winner or asks
for another round.

**The design question that must be answered before it is written:** what
decides "best"? The grammar carries *"a verdict over an aggregate the oracle
computes, not a quantifier the host evaluates"* (§6.1). So the plugin must
compute one number per candidate and the manifest must declare the comparison.
The obvious first cut is *the candidate whose declared test command passes and
whose diff is smallest*, which is expressible today. A model-scored ranking is
**not** expressible and must not be smuggled in through the oracle process —
that is §6's failure mode arriving through the side door, and it should be
refused in review.

**Blocked on:** nothing in the socket, per §2.3. Blocked on #3844 being
verified and closed, which is bookkeeping.

**Witness:** two candidates, one of which fails its test command; the plugin
adopts the other and the losing worktree is discarded. Then the anti-vacuity
half: both pass, and the smaller diff wins.

### 4.3 `plugins/stella-selfdriving` — the program behind the declaration

Today this directory is a consent document and says so. The host side is being
built underneath it on a different track: #3599's B0 (the driver channel,
`crates/stella-plugin/src/driver.rs`, `DriverCall` with 15 verbs), B2 (`work`,
1f7c579b8) and B3 (`deliver`, 9304d3341) have landed as commits, though the
epic's checkboxes are all still unticked.

**So the remaining work for the plugin itself is narrower than
`doc:pipeline-as-plugins` §10 makes it sound**, and it is exactly this: write
the program, give the manifest a `[runtime]`, and delete `scripts/self-driving.sh`
*only* when `make self-driving-test` is green with every assertion intact (§10
D5 — an assertion relaxed to make the move pass is a bug in the move).

**The hard gate remains #3482**, and it remains the right gate. §10 is correct
that this plugin holds `gh`, the AWS CLI, `brew`, a line in `~/.zshrc`, a
daemon, and the power to merge — and correct that packaging *relocates* rather
than grants that authority. But relocation is exactly when the authority
becomes installable by someone who did not write it, which is when a declared
`[[capabilities]]` entry needs to bind to a real `AuthzGate` rule rather than
print nicely. **Do not ship a runnable `stella-selfdriving` before #3482.** It
is the most dangerous plugin anyone will install, which §10 already identifies
as the reason it is the right forcing function for the authority work.

**Witness:** the one #3599 B0 already names, pointed at this manifest — a
driver whose manifest omits a call is refused it with a `HostCallRefusal` code
and keeps running; a driver that declares it is served. Both directions.

### 4.4 `plugins/stella-goal` — repair, not build

The plugin is written. It cannot work. The repair is entirely host-side and
entirely already-filed: #3838 (bind a `verifier` seat), #3841 (honour
`max_holds = 7`), #3840 (carry the verifier's own words into the next round).

**One additional thing this plugin owes that is not filed:** `plugins/README.md`
and the plugin's own header both explain at length that its designed home is
`stella run --pipeline goal-v1` and that `stella goal` refuses it (#3832). That
is correct and well-documented. What is missing is the check that it *works* on
its designed home — the three existing harnesses
(`goal_plugin_{conformance,dispatch,hostcall}.rs`) grade the wire and the
refusal; none of them can grade a completed supervision round, because #3838
means none can happen. **The witness for #3838 should therefore be written
against this plugin, not against a fixture**, or the fix lands with the plugin
still inert and nobody notices.

### 4.5 Track C — make the three examples fail the PR that breaks them

Two pieces of work, both small, both overdue.

**a. Refresh the three manifests** (#3523). They predate #3499 (tamper split)
and #3501 (`[loop] points`, optional `[oracle] command`). `stella-examples`
already ships `plugins/ci/generate-manifests.py` and
`plugins/ci/check-manifests-identical.py`, so the regeneration path exists;
what is missing is a run of it against the current grammar.

**b. Add the smoke check §9 rule 4 asks for, in *this* repository.** The
mechanism is already proven four times over: `crates/stella-runtime/tests/`
spawns a plugin through the host's own `SubprocessWrapper` and grades it
against committed vectors. Add a fifth harness that does the same for the three
examples, resolving them from a **pinned `stella-examples` commit SHA** checked
out by CI. A pinned SHA makes the bump a reviewable diff, which is the property
that makes the check meaningful rather than a moving target.

Rust and Python run in CI unconditionally. TypeScript needs a build step
(`argv = ["node", "${plugin_dir}/dist/main.js"]`), so gate it on Node being
present and **report the skip out loud** — a silently skipped language check is
how "we support three languages" becomes false without any PR being red.

---

## 5. Where each plugin ships, and why

`plugins/README.md` already states the governing rule, and it is the right one:
a plugin that must **move in lockstep with this repository's wire contract**
lives in this repository, because *"a vector living in another repository cannot
fail the PR that broke it."* A plugin that exists to prove the surface is
third-party-usable must **not** live here, or it proves nothing.

| Plugin | Repository | Why |
|---|---|---|
| `stella-research`, `stella-plan`, `stella-goal` | **`macanderson/stella` — `plugins/`** (unchanged) | first-party stage extractions; graded by in-tree harnesses on every PR |
| **`stella-witness`** | **`macanderson/stella` — `plugins/`** | §4.1: the open referent for the project's central claim; also the heaviest consumer of the wire contract, so it must break the PR that breaks it |
| **`stella-candidates`** | **`macanderson/stella` — `plugins/`** | consumes `candidate_fanout`, a capability that lives here and is still moving |
| **`stella-selfdriving`** | **`macanderson/stella` — `plugins/`** (unchanged) | consumes `DriverCall`, which is still growing verb by verb through #3599 |
| **`vera`** | **`oxageninc/vera`, private** | commercial superset; depends on the published wire contract, not on this tree's internals |
| `verify-rs`, `verify-py`, `verify-ts` | **`macanderson/stella-examples` — `plugins/`** (unchanged) | third-party-shaped by design; pinned by SHA from this repo's CI (§4.5b) |

**The one rule that keeps this from rotting:** a plugin in this repository is
graded by a harness in `crates/stella-runtime/tests/`; a plugin outside it is
graded against the **published wire corpus** (`docs/wire/wrapper.wire.json`,
already generated and gate-checked by `make wire-schema`). Those are two
different contracts and they should stay that way. In-tree plugins may read the
tree; out-of-tree plugins may read only the corpus. If Vera ever needs
something the corpus does not carry, that is a bug in the corpus, and finding it
is one of the things a private plugin is genuinely good for.

**A prerequisite for the out-of-tree half that does not exist yet.** The wire
carries `PROTOCOL_VERSION: u32 = 1` on every message
(`crates/stella-plugin/src/wire.rs:79`), but **the manifest does not declare
which protocol version a plugin speaks**, so an incompatible pair is discovered
at first dispatch rather than refused at install. That is tolerable while every
plugin ships in the same commit as the host. It is not tolerable the moment a
plugin is distributed. See `doc:plugin-marketplace` §5, which treats this as
prerequisite M-3.

---

## 6. The implementation plan

Ordered by dependency, and deliberately not by size. Each slice names its
witness, because `CLAUDE.md`'s rule is that correctness is demonstrated or it
is not claimed.

### Phase 0 — clear the bookkeeping (hours, not days)

- **P0.1** Verify and close #3804 and #3844 (§2.3), or state why they are still
  open. Nothing below is blocked on this; it is blocked on it being *legible*.
- **P0.2** Tick #3599's landed phases (B0, B2, B3) or say why the commits do not
  satisfy them. An epic whose body contradicts its own merge history is a
  claim, not a tracker.

### Phase 1 — unblock the arbiter half (the real critical path)

Nothing that matters can be built until a plugin can spend a verifier call and
hold a round.

- **P1.1 — #3838.** Bind a `ChildTurns` seat serving the `verifier` role intent.
  *Witness: `plugins/stella-goal` completes one supervision round end-to-end —
  written against the plugin, not a fixture (§4.4).*
- **P1.2 — #3841.** `stella run`'s door raises `host_max_holds` and the
  `ChildTurns` ceiling to the installed manifest's ask, or reports the gap
  before the run ends. *Witness: a manifest asking `max_holds = 7` gets seven,
  and one asking for more than the ceiling is told so at bind time, not at
  round four.*
- **P1.3 — #3840.** A verifier's free-text reasoning reaches the next round's
  worker. *Witness: the held-open round's prompt contains the verifier's own
  sentence, not the manifest's `[requirements]` prose.*

### Phase 2 — composition, which decides whether any of this is usable

- **P2.1 — #3801.** `--pipeline <variant>` composes more than one plugin's
  contribution. *Witness: `research-v1` + `plan-v1` bound together in one
  selection, both stages observable in the trace fold, and a stage-graph
  conflict between two manifests refused at bind time rather than resolved
  silently.*

This is one slice and it is the largest single piece of work in the plan.
Everything downstream of it is a plugin; it is the last piece of *platform*.

### Phase 3 — the plugins, in parallel once Phase 2 lands

- **P3.1 — `plugins/stella-candidates`** (§4.2). Independent of Phase 1;
  buildable as soon as P0.1 confirms the capability. *Witness: §4.2's two-case
  pair.*
- **P3.2 — `plugins/stella-witness`** (§4.1). Needs P1.1–P1.3 and #3579.
  *Witnesses: the ported property test, the tamper case, a conformance harness.*
- **P3.3 — Track C refresh + in-tree smoke check** (§4.5). Independent of
  everything above. *Witness: a deliberate breaking change to the wire corpus
  turns the new harness red in this repository, on the PR that made it.*
- **P3.4 — benchmark, retroactively** (§4.1 reason 3). Check out the commit
  before `a6d3db4f6`, and run the deleted built-in path against
  `stella-witness` on Terminal-Bench 2.1 with repeats per task. Report the
  unflattering number if that is the true one. *This closes §7's bar late
  rather than never, and it is the only remaining repair of the one process
  failure on this extraction's record.*

### Phase 4 — the dangerous one, gated on authority

- **P4.1 — #3482.** `Principal::Plugin` is constructed and a declared
  `[[capabilities]]` entry binds to an `AuthzGate` rule. *Witness: an installed
  plugin calling a capability its manifest did not declare is refused by the
  gate, with the refusal naming the plugin — and one that declared it is
  served.*
- **P4.2 — `plugins/stella-selfdriving` becomes a program** (§4.3), and
  `scripts/self-driving.sh` retires only when `make self-driving-test` is green
  with every assertion intact.

**P4.1 is also the precondition on distribution**, so it is where this plan and
`doc:plugin-marketplace` join.

---

## 7. Definition of done for this document

- All nine of §3's plugins exist, and each is graded by something that runs on
  a PR — in-tree harness for the six that ship here, pinned-SHA smoke check for
  the three that ship in `stella-examples`, published wire corpus for Vera.
- One `--pipeline` selection can bind grounding, planning and verification
  together, so the plugin path is not strictly less capable than the path it
  replaced.
- A verification plugin exists that anybody can read, install and check
  (§4.1) — or `README.md` and `AGENTS.md` are corrected to stop implying one
  does.
- The retroactive side-by-side benchmark (§6 P3.4) is run and reported, whatever
  it says.
- No plugin holds a capability it declared and the gate does not enforce
  (#3482).
- `make gate` green throughout, and no entry added to
  `scripts/file-size-baseline.txt` to accommodate any of it.

---

## 8. What this document did not verify

Stated so it is not read as more thorough than it is.

- **I did not run `make gate`, `cargo test`, or any benchmark.** Every claim
  above is from reading the tree, the GitHub tracker, and `git log`. File sizes
  are `wc -l`; test *existence* is `ls`; test *passing* is asserted nowhere.
- **I did not read the three example plugins' source** in `stella-examples`,
  only the directory listing and the workflow filename. The staleness claim in
  §1 is #3523's, cited, not independently re-derived.
- **I did not read `oxageninc/vera`'s README**, only the repository's file
  listing and size. "Charter only" is an inference from a 3 KB repository whose
  sole entry is `README.md`.
- **I did not confirm that #3599's B0/B2/B3 commits fully satisfy their phase
  definitions** — only that commits naming those phases are on `main`. P0.2
  exists because that gap should be closed by whoever wrote them.
