# fullauto — the perpetual delivery loop

`fullauto` runs Stella's delivery cycle unattended and **does not terminate**:
size the cycle to the machine, fix a batch of defects, audit what is left, file
what it cannot fix, benchmark against Claude Code, ship, recalibrate, repeat.

```bash
scripts/fullauto.sh install-commands    # once, per machine
scripts/fullauto.sh preflight           # can this box run a cycle?
scripts/fullauto.sh plan --explain      # what this cycle would do, and why
```

Then, in Claude Code:

```
/loop 45m /fullauto
```

---

## Two properties define it

**It never finishes.** "No more defects" is always a statement about the *lens*,
never about the code. When a lens goes dry the **aperture** widens to the next
one; when every lens is dry the loop drops to `watch` — cheap sentinels, no spend
— and wakes when `main` moves, a defect is filed, or CI goes red. It changes duty
cycle instead of exiting.

**It sizes itself.** Nobody configures the batch size, the parallelism, or
whether to compile locally. `plan` measures the box's real disk, memory, CPU and
contention; `calibrate` moves the ceilings from evidence. The same command is
correct on a 16 GiB laptop on battery and on a 123 GiB rig, with no threshold
file to keep current.

## Where the halves live

The split is at the line between judgement and determinism, and the split is the
design:

| | |
|---|---|
| `scripts/fullauto.sh` | everything a machine can decide without a model — readiness, the governor, the ranked queue, the ledger, the dedup set, the aperture, the benchmark launch, the Homebrew upgrade. Gate-linted by `make shellcheck`. |
| `scripts/fullauto/commands/` | the `/fullauto` slash commands — the judgement half. Canonical copies; `install-commands` puts them in `~/.claude/commands/`. |

`.claude/` is gitignored here (#448 — session scratch must never reach the
remote), so the commands cannot live at `.claude/commands/` and survive. Edit the
copies here and re-run `install-commands`.

| Command | Phase |
|---|---|
| `/fullauto` | one full cycle — the thing you loop |
| `/fullauto:status` | where the loop stands (read-only, free) |
| `/fullauto:scale` | the resource governor — how a cycle sizes itself |
| `/fullauto:audit` | discover what the batch missed, through the current lens |
| `/fullauto:tickets` | file what could not be fixed, as handoffs |
| `/fullauto:bench` | Stella vs Claude Code on Terminal-Bench 2.1 |
| `/fullauto:upgrade` | install the released build, drop the dev-build PATH shadow |
| `/fullauto:evolve` | the meta-cycle — the loop improves the loop |

## The aperture ladder

```
rubric  properties  invariants  concurrency  performance
supply-chain  security  docs  soak  →  watch
```

Each is a different question, not a deeper pass of the same one. A codebase clean
under `rubric` can be full of races under `concurrency`, and re-running `rubric`
will never say so.

Findings are hashed into a **content digest** (lowercased, whitespace-collapsed,
line numbers normalized) and kept in `seen.txt`. A cycle is **dry** when it
produced zero digests that were not already there; two consecutive dry cycles
advances the aperture. Deduping on digests rather than issue numbers is what
stops a `wontfix` finding reappearing every cycle and pinning the ladder forever.

## The governor

```
supply       cores · load · total & free memory · free disk · battery · contention
demand       open defect count · how many are P0 · is main red
calibration  batch_ceiling · parallel_ceiling · clean-run streak   (AIMD)
                                  ↓
                          tier + concrete knobs
```

Additive increase on a clean cycle, multiplicative decrease on a resource
failure. `--outcome resource-fail` means killed compiler, OOM, ENOSPC, thrash —
**a red gate is `ok`**, because the batch was wrong, not too big. Getting that
distinction wrong teaches the controller to shrink away from work it could
handle. Full detail in `/fullauto:scale`.

The most consequential output is `FULLAUTO_LOCAL_BUILD=0` — *do not compile here,
push and read the gate from CI*. On a constrained box that is a **better** cycle,
not a degraded one: CI has more memory than the laptop, and its result is the one
that gates the merge anyway.

## State

`~/.fullauto/<repo>/` — outside the repository, because `make no-scratch` fails
the gate if a tracked file matches a `.gitignore` rule. Keyed off the **remote
URL**, not the directory name, so a git worktree shares the main checkout's
memory instead of starting a fresh one.

```
ledger.jsonl       one JSON record per cycle: fixed, filed, new, gate, bench, tier, aperture, outcome
seen.txt           every finding digest ever triaged
cycle              the cycle counter
aperture           which lens is currently open
calibration.json   the learned ceilings, clean-run streak, last clean head
```

## The three traps this encodes

All three fail silently, which is why they are constants in the script rather
than instructions in a prompt.

**`brew install stella` installs an Atari 2600 emulator.** Homebrew-core owns the
name. `macanderson/stella/stella` is the only safe spelling, and the wrong one
exits 0.

**There is no `alias stella`.** It resolves to the dev build through a PATH
*prepend* in `~/.zshrc`. `upgrade` comments that line out, backs the file up
first, and fails closed if the line is not present verbatim.

**`gh` colorizes `--json` output** when `CLICOLOR_FORCE` is set, even into a
redirect. ANSI escapes in a JSON payload are invisible in a terminal and fatal to
a parser, so every parsed `gh` call goes through `gh_plain`.

## Cost rails

- `$25` per cycle, `2 hours` maximum on the benchmark rig.
- The EC2 rig bills **$45.56/day** running. Every path that starts it stops it,
  including on failure and interrupt — but check `aws ec2 describe-instances`
  afterwards anyway.
- The cheap arm (`bench loop`) is CI-hosted, well under a dollar, and runs when
  the governor allows. The measured head-to-head runs on a `heavy` tier only.

## Environment

| Variable | Default | Purpose |
|---|---|---|
| `FULLAUTO_STATE_DIR` | `~/.fullauto/<repo>` | ledger, seen set, aperture, calibration |
| `FULLAUTO_COMMAND_DIR` | `~/.claude/commands` | where `install-commands` writes |
| `FULLAUTO_DRY_STREAK` | `2` | dry cycles that advance the aperture |
| `FULLAUTO_BATCH_MAX` | `20` | AIMD hard ceiling |
| `FULLAUTO_PARALLEL_MAX` | `4` | worktree ceiling |
| `FULLAUTO_DISK_FLOOR_GB` | `15` | below this, never build locally |
| `FULLAUTO_MEM_FLOOR_GB` | `4` | below this, never build locally |
| `FULLAUTO_MATCH` | `arenabench/matches/fable5-claude-code-vs-stella.toml` | the head-to-head |
| `FULLAUTO_RIG_INSTANCE` | `i-07d46341dcc9a31b3` | the Terminal-Bench rig |
| `FULLAUTO_ALLOW_EMULATED` | unset | permit a non-claim-eligible host for throwaway signal |
| `FULLAUTO_RC` | `~/.zshrc` | the rc file `upgrade` unshadows |
