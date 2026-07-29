# The rubric

Seventeen dimensions, fixed weights, an absolute bar. **Do not change the weights between
runs** — they are the only thing that makes two scores comparable. If a dimension genuinely
must change, that starts a new series and every prior score must be recomputed or explicitly
marked incomparable.

This rubric is deliberately identical to the one used by the older `reaudit` skill, so any
history recorded by that skill continues in this one without a break.

## The 17 dimensions and their weights

Weight reflects what determines whether software is trustworthy in production, not how
interesting the dimension is to read about.

| Weight | Dimensions |
|---|---|
| **3** | `loop_correctness`, `security`, `agent_performance`, `system_architecture` |
| **2.5** | `durability`, `token_efficiency` |
| **2** | `maintainability`, `end_user_experience`, `compute_efficiency` |
| **1.5** | `data_storage`, `documentation`, `file_organization`, `vendor_lock_avoidance` |
| **1** | `formatting`, `language`, `file_names`, `symbol_names` |

Sum of weights = 33.

```
overall = Σ(score_d × weight_d) / Σ(weight_d)      # skip any dimension scored 0 (= N/A)
```

### What each dimension means

- **formatting** — mechanical consistency; is it enforced by a gate or by hope
- **language** — prose quality of comments, docs and user-facing strings; comments that
  *lie about the code beneath them* are the worst defect in this class
- **documentation** — module docs, ADRs, whether cited documents actually exist
- **file_names** — do names describe contents; how many conventions run at once
- **symbol_names** — precision, consistent verb choice, one word per concept
- **file_organization** — module decomposition, god-modules, files reachable from the tree
- **system_architecture** — layering, dependency direction, real seams vs leaky abstractions
- **data_storage** — schema, migrations, transaction boundaries, retention and growth bounds
- **vendor_lock_avoidance** — adapters cut per protocol/dialect, not per vendor
- **security** — the trust boundary and whether every stated guarantee actually holds
- **durability** — crash, corruption, partial-failure and cancellation resilience
- **maintainability** — test adversarialism, reviewability, lint policy
- **loop_correctness** — termination, cancellation, budget, retry, state-machine correctness
- **token_efficiency** — prompt-cache stability, bounded tool output, context economy
  (`0` = N/A for a workspace with no LLM surface)
- **agent_performance** — latency on the hot path; blocking work on an async runtime.
  For a non-agent product read this as **hot-path latency**
- **compute_efficiency** — CPU, allocations, redundant passes, render cost
- **end_user_experience** — errors that name the next action, install, headless output

`0` means **not applicable to this unit** and is excluded from every mean. Say so in
`score_notes`. Do not score 0 to avoid forming a judgment.

---

## Calibration: what 100 means

**100 is the Rust project itself** — `rust-lang/rust`: the language, the compiler, the
standard library, and Cargo, taken together as one engineering artifact. It is the ceiling
because of *process that compounds*, not because of any single clever file:

| Anchor | What the Rust project actually does |
|---|---|
| Change control | Every user-visible change goes through a public RFC with a final comment period and team sign-off. Nothing user-visible lands because one person thought it was a good idea. |
| Ecosystem-wide regression proof | **Crater** rebuilds and retests essentially the whole public ecosystem against a candidate change before it lands. The blast radius is measured, not estimated. |
| Backward compatibility | Editions (2015 → 2024) keep decade-old code compiling. Compatibility is a *guarantee with a mechanism*, not an intention. |
| Never-broken trunk | A merge queue tests the actual merge commit. Trunk is green by construction. |
| Diagnostics as a product | UI tests pin exact compiler output, so changing an error message is a reviewable diff. Errors carry spans, explain codes, and machine-applicable suggestions (`cargo fix`). |
| Soundness | Unsoundness is a release-blocking class of bug with its own label and triage. `unsafe` carries documented invariants. Miri and the sanitizers hunt UB in CI. |
| Documentation that cannot rot | Doc examples are compiled and executed by CI, so a stale example is a failing build. |
| Performance as a tracked invariant | Per-change performance measurement with regression flagging, on a public dashboard. |
| Portability | Multiple codegen backends and an explicit platform support-tier policy with named maintainers per tier. |
| Formatting and lint policy | `rustfmt` and a large `clippy` corpus, with lint levels used as deliberate policy rather than noise suppression. |

**Nothing you audit is going to be Rust.** That is the point of anchoring here: the number
stays honest for years and across every project, because the ceiling never moves.

| Band | Meaning |
|---|---|
| **100** | Reserved. The Rust project. Never awarded. |
| **96–99** | Reserved for a project with Rust-grade *process*: an ecosystem-wide regression gate, a decade of proven compatibility, formal or qualified specification work. Awarding a score in this band requires naming, in `rationale`, which anchors above the project actually satisfies. |
| **90–95** | Reference-grade for its language: ripgrep, tokio, rust-analyzer, SQLite, curl, PostgreSQL. Exemplary, and still not Rust. |
| **80–89** | Strong. Minor nits only. |
| **70–79** | Solid, with real gaps a new contributor would hit. |
| **60–69** | Mediocre. Notable debt. |
| **< 60** | Deficient. |

### Binding calibration rules

1. **A first-party workspace scoring above 92 on any dimension must justify it against the
   100-anchor table above**, naming the mechanism — not the intention — that earns it. "The
   code is very clean" does not earn 93.
2. **A wall of 90s is a failed audit. So is a wall of 60s.** Spread is evidence of reading.
3. **Thin evidence means a lower score.** A score without a `file:line` behind it is a guess,
   and a guess is scored down, not averaged out.
4. **The bar is absolute, never relative to the last run.** A dimension rises only if the
   tree is genuinely better against these anchors. Dimensions are allowed to fall, and saying
   so is the entire value of the exercise.
5. **Effort is not a score input.** Neither is the size of the diff, the count of fixes
   applied, or how hard the work was.
6. **Process counts as evidence, but only enforced process.** A documented rule with no gate
   behind it scores as an unenforced rule — check whether the guard can be defeated.

### Per-unit weighting inside a dimension

Cross-cutting lens scores are the primary signal for the dimensions that lens owns; per-unit
scores modulate by a few points. Weight units by **product significance, not line count** —
the shipping binary, the public API surface, the persistence layer and the security boundary
carry the most weight; benchmarks, fixtures and harnesses the least.

### Where the panel's scores come from

Three frontier models score every dimension independently and blind. The reported score is
the **median**; the spread is published as a confidence signal. See `model-panel.md` — the
rule that no model grades its own work is what keeps these bands meaningful.

## History

Scores are recorded per repository in `~/.claude/audits/<repo>.json`, outside any skill
directory, so the series survives skill edits. `scripts/score.py --record` writes it and
verifies that recomputing the previous round reproduces its published number — if it does
not, the weights drifted and the comparison is invalid.
