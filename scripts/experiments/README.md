# scripts/experiments — settle a claim instead of arguing it

These exist because `docs/spec/file-budget.md` shipped with several confident
mechanism claims that nobody had measured. When they were measured, **two of the
most confidently stated ones turned out to be wrong** while the document's
conclusion survived. That is the pattern these scripts exist to make cheap:
find out which of your reasons are real *before* someone builds a gate on top of
them.

## The rules every experiment here follows

1. **State the claim, and the prediction that would falsify it.** A script whose
   claim cannot come back false is not an experiment.
2. **Print `n` next to every number.** A table without its sample size is an
   opinion with a decimal point.
3. **Refuse a verdict below a minimum sample.** `UNDERPOWERED` is a real,
   frequent, correct result. `exp1` returns it today.
4. **Name the confounds in the docstring and handle them in code.** Both real
   findings here were nearly lost to one (see below).
5. **Never drop data silently.** If rows are excluded, print how many and why.
6. **Re-runnable with no arguments**, against the repo as it stands.

## The experiments

| Script | Question | Data | Status |
|---|---|---|---|
| `exp1_read_cost.py` | Does an agent read a big file whole? What does an edit cost? | `.stella/private/store.db` | §1.1 **refuted**; cost curve **underpowered** |
| `exp2_append_bias.py` | Do agents append to the end of files? | git history | §1.4 **refuted** (n=246,010) |
| `exp3_growth_ratchet.py` | Is growth one-way? Do big files grow faster? | git history | one-way **supported**; faster **refuted, reversed** |

```sh
python3 scripts/experiments/exp1_read_cost.py [path/to/store.db]
python3 scripts/experiments/exp2_append_bias.py [max_commits]
python3 scripts/experiments/exp3_growth_ratchet.py
```

All three are read-only, need no build, and finish in seconds. They are
deliberately **not** wired into `make gate` — they answer design questions, they
do not enforce anything, and a gate that runs them would just be slow.

## The confounds that nearly produced two wrong answers

Recorded because both were invisible until the result looked odd, and both would
have been reported as findings.

**The restructure that looked like a refactor.** EXP-3's first run reported that
files over 2,000 lines *shrink* by 26.86 lines per edit — a striking result, and
false. Commit `7df3d73f` moved 303 files under `crates/`, deleting 121,963
lines, which git recorded as delete+create rather than rename; every one of
those deletions landed in the largest bucket. Fixed with `-M -C` rename
detection and a mass-move exclusion. **The tell was that the result was too
good** — it is worth being suspicious of a number that hands you a headline.

**The bucket that was one file.** EXP-1's re-read tax climbs 1.49 → 3.00 → 3.39
→ 9.06 with file size, which reads as a clean size effect until you notice
`deck_ui.rs` is 75% of every read in the top bucket. It measures which file the
task happened to be about. The script now prints that share automatically, so
the trap is visible rather than remembered.

**The tautology in the test itself.** EXP-2 had to exclude file-creation commits
and cap per-hunk weighting: a new file is 100% "append" by construction, and one
large mechanical commit can carry a bucket on its own. Either would have
manufactured the exact result being tested.

## Adding one

Copy the docstring shape from `exp2_append_bias.py` — claim, both predictions,
method, confounds handled, usage — then make the verdict a function of the data
with an explicit underpowered branch. If you cannot write down what result would
change your mind, the experiment is not ready to write.

The open one is in `docs/spec/file-budget.md` §12.2: **does file size actually
cause merge conflicts?** Git merges by hunk, not by file, so two agents editing
distant regions of one 4,000-line file may never collide — in which case §1.3 is
wrong and should be deleted rather than defended.
