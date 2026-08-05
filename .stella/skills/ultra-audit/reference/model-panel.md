# The model panel

The premise: **a model is a poor verifier of its own output.** It repeats its own blind spots,
defends its own claims, and cannot see the bug it just wrote. Every trust-bearing step in this
audit is therefore performed by a model *other than* the one whose work is being judged, and
the score itself is a median across three independent frontier models rather than one model's
opinion.

This file is the contract. The harness enforces it mechanically; `scripts/progress.py
--provenance` proves after the fact that it actually happened, by reading the real model
recorded in each agent's transcript rather than trusting the harness's intent.

## The panel

| Slot | `opts.model` | Role |
|---|---|---|
| A | `opus` | Frontier. Discovery, judgment, scoring, synthesis. |
| B | `fable` | Frontier. Discovery, judgment, scoring. |
| C | `sonnet` | Frontier. Discovery, judgment, scoring. |

`haiku` is **not** on the panel. It never judges a finding and never scores a dimension. It is
permitted only for mechanical extraction where the answer is checkable by a script.

Omitting `opts.model` inherits the session model — which silently collapses the panel to one
model and destroys the whole design. **Every agent in this harness sets `model` explicitly.**

## The five rules

### 1. No model grades its own homework

For any finding, the verifier pool is `PANEL − {model that found it}`. A finding's author
model never sits alone in judgment of it. Same for fixes: the agent reviewing an applied fix
runs on a different model than the agent that wrote it.

### 2. Discovery is diversified, not consensus-driven

At discovery time, **disagreement is the product.** Units and lenses are assigned models
round-robin so no single model's blind spots shape the whole audit, and the two heaviest lenses
(`security` and `loop-correctness`, weight 3) run **twice on two different models** with the
*union* of their findings carried forward. Never intersect findings at discovery — that
throws away exactly the defects only one model could see.

### 3. Judgment is a quorum, and the vote is recorded

Every critical/high finding is put to cross-model refutation. Each verifier is told to *try to
refute*. The tally is recorded, not just the outcome:

| Agreement | Meaning | Treatment |
|---|---|---|
| `unanimous-real` | every cross-model verifier says real | enters the risk register at full confidence |
| `majority-real` | most say real | enters the register, flagged |
| `contested` | verifiers split | enters the register flagged **contested**; synthesis must resolve it by reading the code and say which way it went |
| `refuted` | most say not-a-defect | excluded from the register; kept in the report's refuted list with the reason |

A finding that **only its own author model believes** is labelled `single-model` in the
report. That label is information for the reader, not an automatic dismissal.

### 4. The score is a median of three blind panelists, and the spread is published

Three calibration agents — one per panel slot — independently score all 17 dimensions from the
*same* evidence bundle. They are blind to each other and blind to any prior score.

- **reported score** = median of the three
- **spread** = max − min
- **spread ≥ 10 on a dimension ⇒ `contested`**: the synthesis partner must read code and
  justify the number it lands on, and the report must show the disagreement.

A median is used rather than a mean because it is robust to one panelist being badly
miscalibrated, which happens.

Publishing the spread is not decoration. A `security` score of 74 where three models
independently said 73/74/75 means something quite different from 65/74/83, and the reader is
entitled to know which they are looking at.

### 5. Provenance is recorded and verified

Every agent prompt begins with exactly this line:

```
ULTRA-AUDIT-AGENT phase=<phase> role=<label> model=<assigned-slot>
```

Every finding carries `found_by_model`; every verdict carries the judging model. After the run,
`scripts/progress.py --provenance <transcript-dir>` extracts the **actual** model from each
agent's transcript and cross-checks it against the assigned slot. A mismatch means the mix did
not happen and the cross-model guarantees in the report are void.

Verify this. Do not report a cross-model audit you did not confirm was cross-model.

## What this costs, and the honest limitation

Cross-model verification roughly doubles the judgment phase. `--depth deep` (the default)
spends one cross-model verifier per severe finding plus a second for anything critical;
`--depth max` puts every severe finding to the full panel.

The limitation worth stating plainly: these three models share a great deal of training
lineage. Cross-model agreement is **weaker evidence of truth than three independent human
reviewers would be**, and unanimous agreement across the panel does not make a finding true.
What the panel reliably catches is the *single-model artifact* — the confident, plausible,
wrong finding that one model generates and would otherwise defend all the way into the report.
That failure mode is common enough that catching it is worth the spend. Claim that, and not
more.
