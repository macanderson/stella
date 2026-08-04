---
id: 07-agent-projection
title: "What the agent actually sees"
status: living
---

# What the agent actually sees

The records in `01`–`06` are governance artifacts: git-tracked, hashable,
reviewable, auditable. None of that belongs in a prompt. This file shows the
**projection** — the bytes the model receives — beside the machinery that turns
a handle in that projection back into an attribution signal.

Measured across the ten live records in this directory:

| | bytes | per record |
| --- | --- | --- |
| full record | 6274 | 627 |
| projection | 1013 | 101 |

**The agent sees ~16% of the record.**

## The rendered block

It arrives in **two blocks**, because `force` decides the channel.

### Cached — every turn, byte-identical

`must` and `should` records. This block sits in the system prefix, built once per
session and reused verbatim (`crates/stella-cli/src/agent.rs:698`). It does not depend on
the prompt, so it never changes and never invalidates the cache.

```text
## Workspace rules

### Must
- This repository uses pnpm exclusively; npm and yarn must not be used. ^pkg-manager [enforced]
- Development runs on Node 22.x. ^node-version
- To ship an agent capability, write its contract file and place it with the other
  contracts before implementing the capability. ^ship-a-capability

### Should
- A PR description's first paragraph states why the change exists and what breaks
  without it. ^pr-descriptions
```

### Volatile — selected per turn, alongside memories

`may` and `info` records, chosen by relevance and injected after the stable prefix
(`inject_recall_block`). Facts are only worth tokens when they apply, which is how
memories already behave — so they share the channel.

```text
## Relevant context
- Capability contract files are stored in the capabilities/contracts/ directory.
  ^capability-contract-location
- An agent capability's contract is written as a YAML file. ^capability-contract-format
```

Two facts, not three. The selector also chose `^staging-endpoint`, which the token
budget then dropped — so it never reached the model, and the volatile block is what
*survived* selection rather than what was selected. That gap is invisible unless
the ledger distinguishes the two stages; see "Why the handle exists" below.

Of the ten live records here, three others were never selected: two facts whose
`applies_to` did not match an endpoint task, and one `personal` record scoped to
PR review.

Note what the split buys. A `must` record can never be silently dropped by a
relevance miss — a mistyped task name costs you scoring accuracy, not enforcement.
Only facts are ever selected, and a fact going missing is survivable.

Everything else — `record_id`, `record_hash`, all of `[record.provenance]`,
`[record.truth]`, `applies_to`, `precedence`, `status` — stays out.

### Why each omission

**`applies_to` and `precedence` are consumed by the selector.** They decide
whether a record appears at all and which of two conflicting records won. Passing
them through afterward is waste: the answer is already baked into what is in the
prompt.

**Provenance is expanded by the engine, not the agent.** The agent emits
`^pkg-manager`; the engine already knows the file, commit, and merged PR behind
that handle. The commit sha never needs to enter the prompt.

**`record_hash` is unverifiable by a model.** It is an integrity field for the
loader.

**`confidence` invites false precision.** Whether to defer is already encoded in
which heading the record renders under.

### Why grouping by force matters

The current renderer (`crates/stella-core/src/rules.rs:387`) emits every rule under one
header — `## Workspace rules (binding — follow exactly; guarded rules are
hard-blocked)` — so a `may`-force preference and a `must`-force constraint are
indistinguishable, and a fact about a staging URL is presented as something to
"follow exactly." Grouping amortises the force marker across many records instead
of repeating it per line, and stops the renderer overstating what a record is.

### Byte-stability is a hard constraint

This is what forces the two-block split. The system prefix is built once per
session and reused verbatim under a prompt-cache contract
(`crates/stella-cli/src/agent.rs:698`). Anything selected per turn cannot live there
without rebuilding the cache every turn — which is why `must`/`should` are
unconditional and only facts are relevance-gated.

So the cached block must be byte-identical across turns. No timestamps, no
"verified 3 weeks ago", no confidence that drifts as citations accumulate —
anything carrying a clock invalidates the cache every turn.

Staleness is therefore resolved **before** rendering, never expressed in it. A
record is fresh enough to inject at its stated force, demoted to a weaker force,
or dropped. The decision reaches the prompt; the reasoning does not.

## Why the handle exists

Not for explainability. The handle is the **attribution key**, and without it the
feedback loop cannot close.

`ContextUseKind` (`crates/stella-core/src/context_record/context_use.rs:19`) is a
three-stage funnel:

| Stage | Means | Deterministic? |
| --- | --- | --- |
| `selected` | the selector chose it | yes — the engine decided |
| `rendered` | it survived into the prompt bytes | yes — the engine emitted it |
| `cited` | the agent named it in its output | yes to **observe**, but see below |

The `selected` → `rendered` gap is a real and silent bug class: a record chosen
by the selector and then dropped by a token budget looks identical, from the
ledger, to one that was never chosen. `MissingContextKind::NotRendered` exists to
name exactly that.

**`cited` is unreachable today.** `render_rules_section` emits no identifier, so
the model cannot name what it followed, so the third stage of the funnel can never
be populated. The handle is what makes that stage observable at all.

## Injected is not useful

The record type that carries the judgement is `ContextUseFeedback`
(`context_use.rs:260`), and its most important field is not the verdict:

```rust
/// Whether the record had a real opportunity to influence the task.
pub had_opportunity: bool,
```

Without it, silence is ambiguous — a record that was rendered and not cited might
have been ignored, or might simply have been irrelevant to that turn. Punishing a
record for being irrelevant would decay every specific rule toward retirement
purely because most turns do not touch its domain.

With it, you get a clean deterministic negative:

> `had_opportunity = true` **and** not cited **and** no observable effect
> → this record cost tokens and steered nothing.

That is the signal worth acting on, and it is available without asking a model to
judge anything.

## Where determinism stops

`selected`, `rendered`, and `cited` are all deterministic to record. **"Helpful"
is not** — it is a judgement, and the schema already treats it as one:

- `ContextUseEvaluation` — `helpful` / `not_helpful` / `neutral`
- `attribution_confidence` — a `Confidence`, not a boolean
- `evaluation_method` — **required for a `not_helpful` verdict**, so a negative
  judgement must say how it was reached
- `observable_effect_refs` — "post-task, observable only"

That last field is the determinism anchor. It is the difference between *the model
said this helped* and *here is the diff hunk that changed because of it*.

### The confabulation hazard

A model asked which context it used will produce plausible citations whether or
not they influenced anything. Self-reported citation measures **plausibility, not
influence**.

This matters concretely because promotion is citation-gated: `stella memory
promote` requires more than ten consecutive positive citations since the last
negative remark. If those citations are confabulated, the gate promotes on vibes —
a signal that looks like it measures usefulness while measuring resemblance.

Mitigations, in order of strength:

1. **Require an observable effect for a positive.** A citation with an empty
   `observable_effect_refs` is `neutral`, not `helpful`.
2. **Prefer mechanical compliance where it exists.** For a guarded rule, the guard
   firing or not firing is ground truth and needs no self-report at all.
3. **Use the deterministic negative.** `had_opportunity && !cited` needs no model
   judgement and is the cheapest reliable signal in the system.
4. **Withhold occasionally.** The only genuinely causal signal is a counterfactual
   — drop a record from the projection and see whether behaviour changes. Safe for
   `info` and `should`; not for `must`.

## One turn, end to end

Records rendered: the seven above. Task: add an endpoint under `src/api/`.

| Handle | Stage reached | had_opportunity | Evaluation | Why |
| --- | --- | --- | --- | --- |
| `^pkg-manager` | `rendered` | no | `neutral` | task ran no package manager; silence is not a negative |
| `^ship-a-capability` | `cited` | yes | `helpful` | agent wrote the contract first; `observable_effect_refs` points at the contract file in the diff |
| `^capability-contract-location` | `cited` | yes | `helpful` | contract landed in the right directory |
| `^capability-contract-format` | `rendered` | yes | `not_helpful` | had its chance and was not followed — contract was written as JSON; `evaluation_method` records how that was determined |
| `^pr-descriptions` | `rendered` | no | `neutral` | no PR opened this turn |
| `^staging-endpoint` | `selected` | — | — | selected, then dropped by the token budget → `MissingContextKind::NotRendered` |
| `^node-version` | `rendered` | no | `neutral` | no toolchain interaction |

Three things this table is meant to make obvious:

- **Two of the seven were genuinely evaluable.** The rest had no opportunity, and
  recording them as negatives would be noise dressed as data.
- **`^capability-contract-format` is the interesting row.** It had an opportunity,
  was not followed, and that is a real signal about the record — possibly that it
  is stale, possibly that it is badly worded, possibly that it should be enforced
  rather than advisory.
- **`^staging-endpoint` never reached the model at all.** Without the
  `selected`/`rendered` distinction that is invisible, and the record would look
  like it was ignored when it was never shown.

## Open question

Handle collisions. `^pkg-manager` is the last segment of
`ctx.acme.web.pkg-manager`. Two record sets in one workspace can both end in
`pkg-manager`, and the agent has no way to disambiguate. Either the handle carries
enough of the lineage to be unique within a rendered block, or the renderer
detects collisions and lengthens only the handles that need it.
