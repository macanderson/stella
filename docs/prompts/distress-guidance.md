---
id: prompt-distress-guidance
title: "distress_guidance — retired, no prompt exists"
status: archived
---

# `distress_guidance`

**This role issues no call, and there is no prompt to transcribe.** The
constants this page once carried verbatim — `GUIDANCE_INSTRUCTIONS` and
`guidance_prompt`, both in `crates/stella-pipeline/src/verify.rs` — were deleted
in #2584, along with the `verifier_stage.rs` module that sent them. The page
stays because `ModelCallRole::DistressGuidance` is still a variant
(`crates/stella-protocol/src/event/call_role.rs`), and this document set's
contract is one page per variant.

| | |
|---|---|
| Call role | `ModelCallRole::DistressGuidance` (`"distress_guidance"`) |
| Dispatch | **none** — nothing in the workspace issues this call |
| Instructions | deleted (#2584) |
| Payload | deleted (#2584) |
| Assignable | **no** — `default_agent` returns `None`, and `Roster::apply` rejects the key as `NotAssignable` |
| Why the token survives | decoding sessions recorded before #2584 |

## What it was

Course-correction handed to a worker that was demonstrably stuck: the *second*
deterministic test failure a candidate accumulated in the revise loop,
consecutive or not (#868 chose the cumulative ledger). A verifier-tier model read
goal + diff + failing evidence and returned at most six lines of "what you are
most plausibly doing wrong", which rode with the next revision prompt.

## Why it is gone

The failure that triggered it was **already deterministic** — the sealed,
redacted brief for the test that went red names the command, its exit code, and
its captured output. A reviewer could add nothing to that except a way to be
wrong about it.

The specific hazard is that a claim appended to a measurement inherits the
measurement's authority, and the worker receiving both cannot tell them apart.
This role was the worst-placed instance of it in the pipeline, for the reason the
old page itself flagged: guidance text flowed *back into the worker's next
revision prompt*, so it was steering, not commentary. On the `fix-git` task that
narration talked a worker into resetting `master` and destroying a
correctly-recovered commit — twice — to satisfy a claim no measurement supported.

What replaced it is nothing: `crates/stella-pipeline/src/pipeline.rs` now revises
on `RevisionCause::Deterministic(&brief.message())` and buys no call. The removal
is structural rather than defaulted-off — see `roster.rs`'s module docs for why
an authority a config key could restore is one a deployment will restore.

## What survives

`GUIDANCE_DIFF_BUDGET_TOKENS` and `DiffScope::EvidenceNamed` still exist in
`crates/stella-pipeline/src/verify/diff_render.rs`, but that module now has no
production caller (tracked separately). Do not read their presence as evidence
that this call still happens.

The one remaining channel from verification back to the worker is the **evidence
demand** (`evidence_demand_prompt`, `crates/stella-pipeline/src/verify.rs`),
which is deliberately not a review: a fixed template over the tracked command,
making no claim about the change and offering no reading of the diff. It is
issued to the worker under `ModelCallRole::Worker`, so it has no page of its own
here — see [worker.md](worker.md).

## Related

- [verdict.md](verdict.md) — the other role #2584 removed from the pipeline
- [worker.md](worker.md) — receives the evidence demand that replaced this
