---
id: prompt-witness-repair
title: "witness_repair — the effective prompt"
status: living
---

# `witness_repair`

Fixing a witness that did not fail on the current code. A test that passes
before the work is done witnesses nothing — only a fail→pass flip counts as
verification — so the pipeline spends exactly one repair turn asking for a
corrected test, and discards the witness if that turn also fails.

| | |
|---|---|
| Call role | `ModelCallRole::WitnessRepair` (`"witness_repair"`) |
| Dispatch | engine turn, **continuing the author's existing thread** |
| Tools | the same witness set: `read_file`, `glob`, `create_witness_test` |
| Built by | `witness_repair_prompt`, `crates/stella-pipeline/src/witness.rs` |
| Sent from | `crates/stella-pipeline/src/pipeline/witness_stage.rs` |
| Output cap | `None` — inherits the engine base |
| Override | `agents.verifier.prompt` — **not wired** |

## Wire shape

Unlike [plan-repair.md](plan-repair.md), this is **not** a fresh completion. It
is appended to the witness author's live conversation, so the model still has
`WITNESS_SYSTEM_PROMPT`, its own exploration, and the file it just wrote in
context:

```
[ system(WITNESS_SYSTEM_PROMPT)
  user(witness_prompt(...))
  … the author's original tool loop …
  assistant(the reply whose test passed)
  user(witness_repair_prompt(command))   ← this call
  … a second tool loop … ]
```

That is why the repair prompt is three sentences rather than a restatement:
there is nothing to re-establish. It only has to name what went wrong and what
replaces it.

## Prompt (template)

```text
Your witness test PASSED on the current, unmodified code — it proves nothing, because only a fail→pass flip counts as verification. Rewrite the test so it fails NOW for the right reason (it must exercise the behavior the goal will add or fix). Call `create_witness_test` again with the corrected file — it REPLACES your previous artifact, which is discarded. The command that just passed was:
{command}

End your reply with the corrected `TEST_COMMAND:` line.
```

`{command}` is the `TEST_COMMAND` the author emitted — the one the pipeline
ran and watched pass.

## The bound

Exactly one repair, and the bound is structural rather than a counter someone
remembers to check: the stage calls this path once. A second failure to produce
a failing test **discards the witness** and the pipeline continues without one.
It never loops.

Continuing without a witness is a real outcome, not a silent one — the run
reports itself unverified rather than crediting an unproven flip. That is the
same posture the verdict ladder takes when every evidence channel is dark.

## Why `create_witness_test` replaces rather than adds

The prompt states it explicitly because the alternative is worse than a wasted
call: an author that creates a *second* file leaves the pipeline with two
candidate witnesses and no principled way to pick. Replacement keeps the flip
oracle pointed at exactly one artifact.

## Related

- [witness-author.md](witness-author.md) — the call this repairs, and the full
  hard-requirements block still in context
- [plan-repair.md](plan-repair.md) — the same L-V2 bounded-repair pattern, but
  as a fresh completion with an echo
