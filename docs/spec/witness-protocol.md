---
id: witness-protocol
title: "The Witness Protocol, adapted"
status: living
---

# The Witness Protocol, adapted

Status: approved for implementation — §4 and §7 land in `stella-pipeline`; §5 records what is deliberately declined

## Purpose

The Witness Protocol v0.1 draft specifies a pipeline in which autonomous agents
ship production software with "done" certified by machinery rather than by
anyone's confidence. It names Stella directly in its adoption path.

This document is not that specification. It is the *adoption decision*: which
of its ideas Stella takes, which it declines, and why. It exists so the
declined half is not re-litigated every time someone reads the draft, and so
the adopted half is anchored to defects that are real in this codebase rather
than to vocabulary borrowed from a different kind of system.

The short version: Stella already has the draft's hardest-won mechanism — a
deterministic oracle that cannot be talked out of a failure. What it lacks is a
**disciplined feedback channel**, and that is what this document adds.

## 1. What Stella already has, and keeps

The L-E11 ladder is not replaced. Three of its properties are the reason the
draft's machinery has something to attach to at all:

- **The flip oracle.** Only a fail→pass flip of the same normalized command
  counts, and `Flipped` is reachable only through `Failing` of that same
  command. The draft's "family" concept is a generator plus *an oracle*; this
  is the oracle. Replacing it would be discarding the strongest asset in the
  crate to install new vocabulary.
- **Tamper exclusion.** The witness artifact's full filesystem identity is
  pinned and re-checked at verify time, and a mismatch aborts the candidate
  before any model verifier can weigh in. The draft calls this an authority
  boundary; Stella already treats it as one.
- **Deterministic-first laddering.** A red test never costs a verifier call, and
  the verifier never overrides a deterministic failure.

Anything below that would weaken these is out of scope by construction.

## 2. The defects this document fixes

Each is present in the shipping code, and each is already named in
[`ROADMAP.md`](../../ROADMAP.md).

**D1 and D3 are fixed by §4 of this document.** D2, D4, and D5 are recorded
here because they are real and because the airlock's machinery makes two of
them cheap to reach later — but they are separate changes and do not land
together. One logical change per PR.

**D1 — The failure channel leaks the detector.** On a deterministic failure the
worker receives `"touched tests failed after execution: {tail}"` — the raw
test-runner output. That tail carries the assertion, the literal expected and
actual values, and the test's name. The worker is entitled to know *what is
wrong with its code*; it is not entitled to the detector's fingerprint, because
the fingerprint is what makes special-casing cheaper than fixing. With a
revision budget and a single visible witness, that is a reconstruction path.

**D2 — A flip is credited on command identity alone.** The oracle matches the
normalized *command*, not the *failure*. A test that fails for reason A,
then fails for unrelated reason B, then passes, credits a flip that no single
defect ever explains. ROADMAP §2 names this.

**D3 — Verdict evidence is unstructured and unreferenced.**
`VerifierEvidence::evidence_refs` exists on the wire and is populated at zero
construction sites. A verdict therefore asserts a summary no reader can go
check. ROADMAP §4 names this.

**D4 — A verdict is not replayable.** `Verdict` carries `passed`,
`deterministic`, and a prose `summary`. The `LadderInputs` that produced it —
the flip state, the touched-test result, the diff size against its budget — are
discarded. Nobody can later ask *why* a run passed. ROADMAP §6 names this.

**D5 — The verifier reads worker-authored text as prose.** `verifier_prompt`
interpolates the diff directly into the prompt body. Diff content is authored
by the party being judged, so a comment addressed to the reviewer arrives as
undelimited instruction text.

## 3. Principles adopted

From the draft, three principles carry over intact:

- **P3 — Metered disclosure.** Every bit crossing from the verification side to
  the worker is deliberate, graded, and logged. The feedback channel is a
  security boundary, not plumbing. (Fixes D1.)
- **P6 — Replayable evidence.** A verdict that cannot be reproduced from what
  it carries is an anecdote. (Fixes D3, D4.)
- **P1 — Separation of authorship.** Whatever writes the code does not author
  what grades it. Stella already routes the witness author and verifier through
  `Role::Verifier`'s cross-family preference; this document does not weaken that.

## 4. The Feedback Airlock

The one new mechanism. The principle: **leak the defect, never the detector.**

### 4.1 The disclosure ladder

A failure brief is emitted at a grain, and the grain is a control surface:

| Grain | Brief contains |
|-------|----------------|
| `L0` | That verification failed, and nothing else |
| `L1` | + which criterion or command failed |
| `L2` | + a symptom class, phrased against observable behavior |
| `L3` | + a regenerated reproduction the worker can run itself |

Default grain is `L3`, because convergence comes from iterating against a real
failure and velocity matters. The grain drops when the same failure fingerprint
(§4.2) repeats, on the reasoning that a worker which has seen the same brief
twice and still fails is not being helped by a third copy of it — it is being
given more surface to fit.

### 4.2 Failure fingerprints

A fingerprint is a stable hash of the *normalized* failure: runner output with
timings, temporary paths, memory addresses, and line-number noise removed. Two
runs that failed the same way share a fingerprint; two runs that failed
differently do not.

What it is used for today: the disclosure grain. Repetition of the *same*
fingerprint is what tightens the ladder, so a worker thrashing against one
failure stops being handed more surface to fit, while a worker making genuine
progress — new failure each round — keeps full disclosure.

What it is deliberately **not** used for yet: tightening the flip oracle to
require that the failure it credits is the failure it first saw (ROADMAP §2,
listed under D2 above). The mechanism now exists, but wiring it would reject a
legitimate sequence that ordinary iteration produces constantly — a test fails
on an assertion, the worker's next edit fails to compile, the edit after that
passes. Under strict fingerprint matching that flip goes uncredited. The
oracle's existing command-identity rule stays until there is evidence that
false flips from *changed* failure modes are a real source of bad verdicts,
rather than a hypothesis with a cheap-looking fix.

### 4.3 The scrubber is the load-bearing part

A symptom class is prose, and prose describing a failure will happily quote the
assertion that produced it. So the redactor does not trust the description: a
brief is scrubbed against the sealed material it was derived from, and a brief
that still contains the test's identifier, its literal expected/actual values,
or the witness path **degrades one grain rather than being emitted with a hole
in it**. Failing closed is the whole point; a redactor that emits on a
best-effort basis is a leak with a ceremony attached.

Every brief is recorded alongside the material it was redacted from, so
disclosure is auditable after the fact rather than merely asserted.

## 5. What is declined, and why

Recorded so it is not re-proposed.

**Two trust domains on separate credentials and infrastructure.** Stella is a
single local BYOK process. Workspace separation and type separation are
achievable and worth having; separate infrastructure, separate credentials, and
non-shared provider caches are not, in-process. Claiming them would be exactly
the ceremony the draft says it exists to replace.

**The shadow sandbox, synthetic twin data, contract doubles, shadow traffic,
canary ramps, and expand/contract migration law.** These assume the pipeline
deploys a running service. Stella verifies a change to a repository. It has no
production to mirror and no rollout to gate.

**The attestation as an underwritten warranty.** The draft's economics section
prices a warranty off an escape rate and a fidelity score. Stella accumulates
neither. What is adopted is the *replayable* half — verdict provenance (§2 D4)
— without the signing, the actuarial claim, or the product tier.

**Dual independent Examiners with divergence-stops-build.** Two full exam
derivations per run multiplies model spend on a tool whose pitch is a hard
per-run budget. The existing cross-family verifier routing is the affordable
version of the same idea, and it already ships.

**Replacing the single witness with generated exam families.** The draft's P5
targets multi-attempt overfitting. Stella's revision budget is small and its
witness dies with the candidate workspace, so the exposure is bounded — and the
mechanism that actually detects overfitting here is the fingerprint (§4.2), at
a fraction of the cost. If escape data later shows workers fitting the witness,
this is the first thing to revisit.

**Criteria ratification with a frozen `AC_HASH`.** Not declined on merit —
declined as *already spoken for*. `stella-core::context_record::contract`
already carries a typed acceptance model (`ArtifactContract`, `Requirement`,
`RequirementKind::{COMMAND, SEMANTIC_VERIFIER}`, `ContractValidation`,
`contract_hash`) implementing the Context Graph Protocol's lifecycle §8.12–8.14.
It is unwired today, and its own module docs say an executor interprets it in a
later phase. Introducing a second, parallel criteria model beside it would
leave the workspace with two acceptance vocabularies that disagree. When
criteria land, they land there.

## 6. What would change this decision

The declined half is declined against today's evidence, not forever. The
signals that should reopen it:

- A worker observed passing a witness it special-cased — reopens exam families.
- Escapes concentrated in "the criteria never asked for it" — reopens
  ratification, via the CGP contract model.
- Stella growing a deployment surface — reopens everything in §5's second
  paragraph.

Until one of those is observed, building for them is speculation with a
maintenance cost.

## 7. Proportionate verification: escalate on evidence

Everything above is about the *heavy* path. This section is about not taking it
when it isn't warranted.

Stella's contributor rule has always been nuanced — ship a witness test, **or a
stated reason there isn't one**; pure refactors, docs, and CI changes don't need
one. The pipeline held itself to a stricter rule than it held people to.

### 7.1 Predict-then-commit is the bug

The pipeline decided how much ceremony to buy by **predicting** difficulty once,
up front, from the prompt — the single worst moment to verifier, because no work
has happened yet. Two consequences:

- **Triage was a paid model call on every prompt, including `hi`.** The
  deterministic greeting table existed and was exact-match safe, but it was
  consulted *after* the call had already gone out. A greeting cost a
  classification round-trip that could not change its own answer, plus up to
  `triage_latency_ceiling` of dead air on a wedged provider.
- **The one escape hatch guessed from wording.** `triage::resolve_witness`
  keyword-matches removal verbs to skip witness authoring. Its reasoning is
  right — a removal's proof is its diff — but it has to be extremely narrow,
  because a false positive ships a real behavior change unproven and the only
  evidence available at that moment is the phrasing of a request.

### 7.2 Escalate on evidence instead

The same principle the evidence ladder already applies to *verification* —
spend only when the evidence is genuinely inconclusive — applied to the
pipeline itself.

**Deterministic answers come before paid ones.** Anything resolvable without a
model is resolved without a model, and *then* the call is made. A route the
code already describes as never depending on a model answer must not pay for
one.

**The change is the evidence, not the prompt.** After execution there is a
diff, and `witness::warrant` reads it. A docs-only edit is docs-only whether
the prompt said "document the parser" or "make the README less confusing"; no
phrasing changes what the diff is. The recognized reasons are `NothingChanged`,
`DocsOnly`, `TestsOnly`, `ConfigOnly`, `CommentsOnly`, and `PureRemoval`.

**Fail closed.** Anything mixed, unrecognized, or invisible to the diff
machinery falls through to "witness required". An unnecessary witness costs one
model call; a missing one ships unverified behavior. Where the warrant is
unsure, it buys the test.

**An empty diff is not a clean turn.** `NothingChanged` is the one reason that
rests on an *absence*, so it is the one that needs a signal which cannot go
dark. A probe reporting nothing may have been unable to look — or may have
looked at the wrong place: a system-configuration task installs into `/etc`,
because that is where the service will read, and the candidate root it was
handed stays empty by construction. Both of the warrant's original guards said
"clean" for that shape, and the run completed `deterministic: true` over ten
mutating calls. So the warrant also reads `mutating_actions` — the pipeline's
tally of the calls it dispatched itself, not a probe into the world — and
waives the witness only when nothing changed *and nothing tried*. The ladder
carries the same rule one rung further: dispatched mutating calls plus a
readable, empty diff and no other observation resolves to `Unverifiable`, an
abstention, never a pass. It is deliberately not a failure — the work may be
entirely correct and merely uncollected, and no revision can make an
un-snapshot-able workspace observable.

**No test needed is not the same as no review needed.** A removal's proof is
its diff, but deleting the *wrong* thing is a real mistake a reader catches and
no test would have covered — so `TestsOnly` and `PureRemoval` keep the
independent reviewer even though they skip the witness. Prose, comments, and
manifests carry no behavior for a reviewer to reason about, so a review call
there is spend with no question to answer.

**Say why.** When no test is warranted, the reason is recorded on the verdict —
the pipeline's half of the contract contributors are already held to. The run
scores `Unverified`, never `DeterministicPass`: it is honestly complete, and a
change with nothing to prove must not outrank a flip-verified sibling in
best-of-N.

### 7.3 Authoring is demand-driven

The warrant reads a diff, so it can only answer after execution. While
authoring ran *before* execute, that left the saving half-collected: the
warrant spared the verifier call but not the author call that had already been
paid. A docs edit still bought a test for prose.

Authoring now runs after execution, gated on the warrant. A change with
nothing to prove dispatches no author turn at all, and `NotRequired` is
answered from the diff rather than from the prompt.

**The hazard this creates.** After execution an implementation exists, and an
author allowed to read it writes a test that restates it. Such a test fails on
the old code and passes on the new, so the flip oracle confirms it — while
proving only that the code equals itself. Reordering naively would have kept
the ceremony and hollowed out the evidence.

**Two trees, not one.** The author works in a *second*
`CandidateWorkspacePort::create` snapshot, taken when the warrant asks for a
witness. Because a candidate's edits never leave its own workspace until
adoption, that snapshot sees the pre-execution tree — so the author's input is
byte-identical to what it was when this stage ran first: the goal, the recalled
frames, and the unmodified code. The reorder buys cost, never leverage.

That snapshot is also where the artifact must FAIL. The accepted file is then
grafted into the candidate that executed — create-only, following no link on
either side, one file — and re-fingerprinted there, so tamper exclusion pins
the bytes that will actually run. The pass is observed in the candidate. Fail
in the pre-execution tree, pass in the post-execution one: a flip across two
code states rather than one tree observed twice.

**What the reorder costs.** A witnessed candidate now takes two snapshots
instead of one. A snapshot is a `git worktree add` and a patch apply; an author
turn is a model call. So the trade is a cheap local operation *only when a
witness is warranted*, against a model call *whenever it is not* — and prose,
comments, config, and no-op turns are common enough on real work to make that
lopsided in the right direction.

**What it changes about failure.** Because the work already exists when the
author is asked, a witness that cannot be *produced* no longer discards it. A
useless witness, an unavailable author, or an artifact that cannot be grafted
leaves the candidate on the unauthored ladder — where previously the whole
candidate was thrown away and the task re-executed from scratch. Fail-closed is
unchanged for artifacts that cannot be *trusted*: a tracked-file mutation, a
symlink artifact, or a runner mismatch still aborts, now after the worker's
turn rather than before it.

`resolve_witness` still runs before execution and still guesses from wording.
It is the deterministic ceiling on whether authoring is *permitted*; the
warrant is the evidence-based gate on whether it is *bought*.
