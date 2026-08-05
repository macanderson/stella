---
description: Where the loop stands — cycles run, dry streak, what is still open, and whether the next cycle is worth running.
---

# fullauto:status — where the loop is

Read-only. Changes nothing, spends nothing. Run it before starting a loop, after
an interrupted one, or any time you want to know whether the thing is converging.

```bash
scripts/fullauto.sh state
scripts/fullauto.sh preflight
scripts/fullauto.sh queue --limit 20
```

Then add the things the script cannot see:

```bash
gh pr list --author "@me" --state open --json number,title,statusCheckRollup
gh run list --branch main --limit 5 --json conclusion,name,url
gh issue list --state open --label bug --json number --jq 'length'
```

---

## Report this

```
fullauto · <repo> · cycle N

converging?   dry streak 1 / 2 — one more clean audit ends the loop
queue         14 open defects (2 P0, 5 P1, 7 P2)
in flight     PRs #1540 (green) #1541 (red — clippy)
main          green @ c46740a8
last bench    loop pass · h2h 4/6 vs Claude Code 3/6 (cycle 5)
seen          312 findings triaged across 7 cycles

next cycle    worth running — 2 P0 in the queue
```

## Reading the dry streak

The streak is the only signal that says whether to keep going. It counts
consecutive dry cycles under the current lens only — advancing the aperture
resets it to zero.

| Streak | Meaning |
|---|---|
| `0` | the last audit found something new — plenty left |
| `1` | one clean audit; could be convergence, could be an audit that looked in the same place twice |
| `2` | **this lens is done.** `cycle-end` advances the aperture; the next cycle audits through the new lens |

A streak stuck at `0` for several cycles while `fixed` is also `0` is the failure
mode to watch for: the loop is spinning. Three consecutive cycles with zero fixes
means something structural is wrong — a red main, a queue full of issues that all
need a product decision, an engine that cannot authenticate. Stop and look; do
not schedule cycle four.

## Reading `preflight`

Tiers, not a flat list, because they block different things:

| Tier | If it fails |
|---|---|
| Fix + gate | **stop** — nothing can run |
| Agent | falls back to the driving agent doing the fixing; say so in the report |
| Benchmark | the bench arm is skipped; the cycle is still valid |
| Ship | the ship phase is skipped; the cycle is still valid |

`gh` sitting in the required tier is deliberate. A loop that cannot file tickets
is a loop that destroys findings, and that is worse than a loop that does not run.

## Resetting

The state lives in `~/.fullauto/stella/` (`ledger.jsonl`, `seen.txt`, `cycle`) —
outside the repo, because `make no-scratch` fails the gate if a tracked file
matches a `.gitignore` rule.

To start a fresh convergence run over the same codebase, move it aside rather
than deleting it — the old ledger is the record of what previous cycles decided:

```bash
mv ~/.fullauto/stella ~/.fullauto/stella.$(date -u +%Y%m%d)
```

Clearing `seen.txt` alone re-opens every finding ever triaged, including the ones
deliberately closed as `wontfix`. That is almost never what you want.
