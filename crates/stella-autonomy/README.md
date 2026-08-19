# stella-autonomy

The deterministic decision core of the `self-driving` delivery loop: fix a
batch of defects, audit what is left, file what it cannot fix, benchmark
against the comparator, ship, and repeat. Everything a machine can decide
without a model lives here — the AIMD controller that sizes a cycle to its
machine, the aperture ladder that keeps "no defects" a statement about a lens
rather than the code, the dry-streak oracle that advances the ladder, the
finding-dedup digest, the governor that turns supply x demand x calibration
into a cycle plan, the self-improvement signals, and the `runs.jsonl` fold
that resolves whether a run is really still running.

## Boundary

No I/O, no types from any other stella crate: every function is synchronous
over owned data, with the clock and the machine reading passed in as
parameters rather than read internally. That is what makes the whole crate
property-testable, and it is why it can be linked by two crates that must
never share I/O with each other.

`std`, `serde`, and `sha2` are the only dependencies — the exact third-party
set the code uses (`serde`/`serde_json` for the JSONL and JSON record shapes,
`sha2` for the dedup digest).

## Why this is a leaf crate, not part of `stella-core` and not a plugin

This code used to live at `crates/stella-core/src/self_driving.rs` — the
module doc said outright that it lived in the engine crate "so the model never
has to re-derive it and cannot get it subtly wrong." That reasoning was about
the *code*, never about the *crate*: purity is a property of synchronous
functions over owned data, and it survives a move. What changed is core's own
premise — `stella-core` is meant to be a bare loop with minimal tools and one
model, and an opinionated perpetual-delivery policy (the AIMD controller, the
aperture ladder) is not part of a bare loop. See `doc:pipeline-as-plugins` §10
("Track D — self-driving leaves core"), work item D1, for the extraction plan
this crate is the result of.

**It moved to a shared leaf crate, not into a plugin binary, because
[`stella-observatory`](../stella-observatory) links it deliberately** — read
the reason on `stella-autonomy`'s line in
[`stella-observatory/Cargo.toml`](../stella-observatory/Cargo.toml). The
observatory used to carry its own private `fold_runs` and `self_improvement`,
written back when the only other implementation was a shell script. The two
drifted: the dashboard and `stella self-driving metrics` disagreed about
whether the loop was `NOISY`, because one tested `2 * new_findings < n` and the
other tested `new_findings < n / 2` in integer arithmetic — every odd cycle
count landed on opposite sides (#1613). One copy in a leaf crate that both
readers link is the fix. Burying the fold inside a plugin executable instead
would recreate exactly that drift: the observatory must not link
`stella-core` (an observer must not pull in the machinery it observes), and it
has no way to link into another binary's private module. A leaf crate is the
[`stella-diff`](../stella-diff) / [`stella-home`](../stella-home) precedent
(#1139, #1511): shared by linking, without costing either caller its
isolation.

[`stella-cli`](../stella-cli) (`self_driving_cmd`, `self_driving_cmd/state.rs`,
`self_driving_cmd/probes.rs`) is the write side: it owns the machine probes,
the state directory under `~/.stella/self-driving/<slug>/`, and the `gh`
calls, and feeds their results into this crate's pure functions. The
observatory is the read side: `stella-observatory/src/self_driving.rs` reads
the same JSONL files back, read-only, and asks this crate the same questions
the CLI does — so the dashboard and the terminal cannot disagree.

## Layout

| File | What it holds |
|---|---|
| [`src/lib.rs`](src/lib.rs) | Everything: the dedup digest, the `AimdLimits`/`Calibration`/`calibrate` controller, the `Lens`/`Tooling`/`LENSES` aperture ladder and `advance`, `CycleRecord`/`dry_streak`, the `Supply`/`Demand`/`Floors`/`CyclePlan`/`plan_cycle` governor, `Metrics`/`metrics`/`starved`, `QueueIssue`/`rank_defects`, and `Liveness`/`liveness`/`RunRow`/`fold_runs`. |
| [`src/surface.rs`](src/surface.rs) | The host surface: `HOST_SURFACE`, `HOST_SURFACE_VERSION`, `HostVerb`, `Emits`, and the two-way `surface_drift` check the CLI's real clap tree is measured against (`doc:pipeline-as-plugins` §10, D2). |
| [`src/tests.rs`](src/tests.rs) | Witness tests ported from `scripts/test-self-driving.sh` (#1548), plus the property tests a generator can sweep that the shell driver never could — digest normalization, controller bounds, dry-streak suffix matching, demand's one-directional effect on the plan. |

## Semantics worth knowing

- **The dedup digest is a byte-for-byte contract with existing `seen.txt`
  files.** [`finding_digest`] normalizes (lowercase, collapse whitespace,
  `:<digits>` → `:L`) and hashes with SHA-256 truncated to 16 hex characters,
  matching `tr | sed | shasum -a 256 | cut -c1-16` exactly. Changing the
  normalization re-files every previously triaged finding as new.
- **AIMD is additive-increase, multiplicative-decrease, on purpose.** A clean
  cycle raises the batch ceiling by 2 (and the parallel ceiling by 1 every
  third clean cycle); a resource failure halves the batch and drops parallelism
  straight to serial. That shape is what provably converges instead of
  oscillating — see [`calibrate`].
- **The aperture ladder never terminates.** [`advance`] walks [`LENSES`] in
  order and returns [`WATCH`] once the ladder is exhausted (or the current
  lens is unrecognized) — "no more defects" is always a statement about the
  open lens, never a statement that the code is done.
- **`Calibration::extra` and `CycleRecord::extra` are load-bearing.** Both
  types `#[serde(flatten)]` unknown keys into an `extra` map rather than
  dropping them. A prior version rebuilt `calibration.json` from only the keys
  it owned and silently discarded `last_clean_head`, which left watch mode
  unable to tell "nothing changed" from "never looked."
- **`fold_runs` resolves a status the file cannot state on its own.** A run
  whose last record says `running` is `Liveness::Live` only if it still holds
  the live pointer *and* its heartbeat is within `stale_after_secs`; otherwise
  it is `Orphaned` (a different run holds the pointer) or `Stale` (heartbeat
  gone quiet) — both report as `crashed`, because only a reader can make that
  correction: the process that would have written the truth is gone. The
  heartbeat is the witness, never the pid — every self-driving subcommand is
  its own short-lived process, so a recorded pid is dead moments after it is
  written.
- **[`DEFAULT_STALE_AFTER_SECS`] is the one copy.** `stella-cli`'s env default
  and the observatory's dashboard both read this constant rather than each
  holding their own `900` — two of the three used to (#1613).

## Gotchas

- This crate has no `async`, no `tokio`, and must stay that way — a governor
  that reads the machine itself cannot be handed a fake `Supply` in a test.
  The caller (`stella-cli`) probes; this crate decides.
- `finding_digest`'s `":L"` uses a capital `L` deliberately, matching the
  shell pipeline's `sed` (its lowercasing ran *before* the substitution). A
  differently-cased marker silently re-reports every finding in every
  existing `seen.txt`.
- `metrics`'s NOISY/FRAGILE/STUCK thresholds and `starved`'s STARVED threshold
  are read by both `stella-cli` and `stella-observatory`. Changing a threshold
  here moves both surfaces at once — that consistency is the entire reason
  this crate exists (#1613). Do not let a surface grow its own copy.

## Extension recipe

Adding a new aperture lens: add a [`Lens`] entry to [`LENSES`] naming its
[`Tooling`] (a concrete `run`/`interpret` command, or `ModelOnly` with a note
saying what tooling would close the gap — no lens may be silently a no-op,
#1549), then extend `the_ladder_is_the_shell_drivers_ladder_in_order` in
`src/tests.rs` with the new name in its position.

Adding a new self-improvement signal: extend [`metrics`] (or add a
`starved`-shaped standalone function if the signal reads the calibration
rather than the ledger) with a new [`Signal`] naming a *specific* pathology,
never a vague health score, and add a fixture in `src/tests.rs` that raises it
and one that stays silent — a dashboard that always warns teaches the reader
to ignore it.

Adding a host verb: add a [`HostVerb`] to [`HOST_SURFACE`] with its path, its
[`Emits`] shape, and a one-line summary, placed in the order the cycle
actually uses it — the list is rendered in declaration order on purpose.
`stella-cli`'s `self_driving_cmd::surface` parity test walks the real clap
tree and fails in both directions, so a verb cannot ship undeclared and a
declared verb cannot vanish. Only bump [`HOST_SURFACE_VERSION`] when a verb is
renamed or removed, or when a verb's [`Emits`] changes; a brand-new verb is
additive and must not bump it.

## God files — none

This crate has no god files: no file exceeds the gate's 1500-line
ratchet (`scripts/check-file-size.sh`), and none may appear — a new file
crossing 1500 lines fails the gate outright, and
`scripts/file-size-baseline.txt` accepts no new entries. When a file here
approaches the limit, split it before it crosses.
