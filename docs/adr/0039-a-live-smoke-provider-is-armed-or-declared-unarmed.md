---
id: adr/0039-a-live-smoke-provider-is-armed-or-declared-unarmed
title: "ADR 0039: A live smoke provider is armed or declared unarmed"
status: implemented
---

# ADR 0039: A live smoke provider is armed or declared unarmed

- Status: accepted
- Date: 2026-09-07
- Decides: `#5595`
- Not part of the Phase 0 series.

## Context

`live-smoke.yml` makes one real call per provider adapter, once a week. It
exists for one reason. The Anthropic adapter shipped a broken `thinking` shape
for months. Only a live call found it (`#240`).

Nine adapters are armed. One can run. Six have no credential in this
repository's Actions secrets. Two sit behind an account with no money in it.
Run 34126157647 (2026-09-07) ends `1 passed; 8 failed`. So did the four runs
before it.

The job has been red every week since 2026-08-24. It is red for two causes at
once. A reader opens the log and sorts six "no credential" panics from two
balance refusals by hand. Red that always means the same thing is red nobody
reads. A real wire-shape regression would land in the middle of it.

The two green runs before that are worse. Run 32007329950 (2026-08-17) reports
`ok`. Its log shows eight providers printing `skipped — set STELLA_LIVE_SMOKE=1
to run`. Green meant one call was made. `#3856` took that away. It made a named
smoke that cannot run fail. It was right. It also left the job with no way to
say what a run covers.

Money would fix the symptom. Six keys and two top-ups, and nine live calls go
green. That is a purchase, not a design. It is not available today. This record
answers what the suite should say while that stays true.

## Decision

**A provider in `LIVE_PROVIDERS` is armed, or a row in `UNARMED` says why it is
not. There is no third state.** The table sits beside the matrix it speaks for,
in `crates/stella-model/tests/live_smoke/arming.rs`. A row names four things:
the provider, what is missing, the run that showed it, and the issue that owns
it. AGENTS.md asks for that same shape when an event has no consumer, or a
provider posture has no witness.

**The reason is re-asked on every run.** A row that stops matching fails the
job and names itself:

- A `NoCredential` row whose credential now resolves is stale. The run says
  which row to delete.
- An `Unfunded` row whose credential has gone is misfiled. That reason claims
  the key works.
- An `Unfunded` provider whose call succeeds has money again. The run says so
  and fails.

So a gap here cannot go quiet. Add `OPENAI_API_KEY` and the job turns red,
asking to be re-armed.

**An `Unfunded` provider still calls its endpoint.** An empty account is
refused before anything is billed. The call costs nothing. The refusal is
evidence. The smoke passes only on the `BILLING` verdict `resolve` already
derives. That is the verdict a reader of the log would act on. A rejected field
resolves to `WIRE SHAPE` and still fails. A revoked key resolves to
`CREDENTIAL` and still fails.

Anthropic is one of the two. It is the adapter `#240` was filed on. This is
what keeps it under the guard.

## Consequences

A green run now carries a claim a reader can check. One provider called live.
Eight declared, each with a reason and an issue. That is less coverage than
nine live calls. It is more information than eight red panics.

The six `NoCredential` providers have no coverage. They had none before. The
difference is that the tree says so, in a file review reaches.

The table is Rust, not YAML. The workflow still passes every secret, so adding
one is what trips the staleness check. A list in YAML would have to be read by
the suite anyway, and no test could reach it.

`#5595` stays open on the purchase. Six keys, and two balances. Its checklist
names what shipped and what money is still owed.

## Alternatives considered

**Skip a provider whose credential is absent.** `#3856` removed that. Its
reason holds: a run that contacted nothing reported nine passes in 0.01
seconds. A row is the opposite of a skip. It is written down, reviewed, and it
fails when it stops being true.

**Sniff the response body to auto-skip a billing failure.** The workflow header
rules this out, and it is right. A body-driven skip would paper over a real
regression. Nothing here skips on a body. `resolve` reads a body to name a
cause the status code cannot, which it did before this change. The row decides
whether that cause was predicted.

**Drop the eight tests until the keys exist.** A deleted test cannot fail.
Nothing would say the coverage went away.
