---
id: audit/2026-08-ultraudit-round-3
title: Ultraudit round 3 — 2026-08-04
status: living
---

# Ultraudit round 3 — 2026-08-04

Round 3 of the recorded audit series, run against `9e348253` (v0.6.102) and
landed against `7c3ab928` (v0.6.109). 55 units, 529,419 lines, 1,039 files,
100% ownership proven disjoint and complete before launch.

**The run did not finish.** The account hit its session limit 75 minutes in.
Of 236 planned agents, 47 completed on the first pass and 125 across two
resumes; the cross-model refutation round, the blind three-model scoring panel,
the second-opinion phase and synthesis never ran. Everything below is therefore
**discovery-grade**: found by one model, not refuted by another, and not scored
by a panel that was blind to the finding's author.

Two consequences are worth stating rather than burying:

- **The 83.9 aggregate is not comparable to round 1 (78.92) or round 2
  (79.79).** Those came from a blind panel; this is 39 unit agents scoring their
  own units, which grades higher for structural reasons. Treat it as a shape,
  not a number, and do not record it as the series score.
- **Findings here have not survived refutation.** The panel's documented job is
  catching the confident, plausible, wrong finding a single model would defend
  into a report. That filter did not run.

## What the round measured

| | |
|---|---|
| Findings | 601 — 1 critical, 64 high, 201 medium, 335 low |
| Fixes applied by agents | 150, of which 33 drew a concern from a cross-model fix reviewer |
| Prior open findings re-verified | 8 — 5 fixed, 2 partially fixed, 1 not fixed |
| Coverage | 1,039 / 1,039 files owned |

### Round 2's open findings, re-checked

| Finding | Verdict |
|---|---|
| `start_process`/`send_stdin` shell boundary was decorative | **fixed** — the gate now reads the bytes written into a live interpreter, test-pinned |
| store.db grew without bound | **fixed** — `Store::prune` covers all 13 execution-keyed tables |
| cache-write tokens billed at zero | **fixed** |
| memory recall was superlinear and blocking | **fixed** |
| `run_command` orphaned its process group | **fixed** |
| per-dialect provider factory consolidation | partially fixed |
| `model_timeout` unbounded | partially fixed — knob landed, five management calls still pass `timeout: None` |
| `--output-format` honoured by only 2 of 36 subcommands | **not fixed** |

## The finding that outranked the rubric

`docs/design/stella-bench-handoff/tb909-key.pem` — a live RSA private key, the
EC2 bench rig's SSH key — was **tracked in a public repository**. No audit
dimension surfaced it; it was found while reorganising the docs tree.

The root cause is worth recording because it is a whole class of failure: a
`.gitignore` rule existed to prevent exactly this, and read

```
/docs/stella-bench-handoff/
```

while the bundle actually sat at `docs/design/stella-bench-handoff/`. Anchored
one directory too shallow, the rule matched nothing, and 903 files / 171 MB of
raw agent session output went with the key. A guard that cannot fail is
indistinguishable from a guard that passes.

The key is untracked and the bundle with it, but **removal from `HEAD` does not
un-publish anything**. The key pair must be revoked at the provider; that is the
only remediation that means anything, and it is not something a commit can do.

`scripts/check-no-secrets.sh` now reads what git actually tracks rather than
what `.gitignore` intends, because `.gitignore` has no opinion about a file
added explicitly or added before the rule existed.

## docs/design is now unciteable, on purpose

`docs/design/` held 190 citations from 126 files across rustdoc, `clippy.toml`,
`Cargo.toml` descriptions and CI scripts — nine of them pointing at documents
that no longer existed. A directory that code depends on is not a scratchpad,
whatever it is called.

Twenty-one documents that code genuinely depends on were promoted to
[`docs/spec/`](../spec/), every citation was repointed, and
`scripts/check-design-refs.sh` now fails the build if anything outside
`docs/design/` names a path inside it. `docs/design/` is down to 8 tracked
files and is now free to be rewritten, renamed or emptied without breaking a
single comment.

Flattening `diagnostics/diagnostics.md` to `docs/spec/diagnostics.md` repaired
the seven dead citations for free — they had already been written against the
flattened name.

## Highest-value findings still open

Ranked by consequence, not severity label. None has been refuted; each cites
the evidence that produced it.

1. **`--output-format` is a promise 34 of 36 subcommands ignore**
   (`crates/stella-cli/src/cli.rs:111`). A scripted caller gets human-coloured
   stdout with a JSON error envelope appended on failure — unparseable by any
   consumer. Two prior rounds have now reported it.
2. **Five management model calls have no wall clock**
   (`pipeline.rs:1490,1581,1606`, `verifier_stage.rs:47,100`). A wedged provider
   parks a headless run indefinitely. The Verdict case is worse than a hang: it
   denies the ladder its fallback to a heuristic verdict.
3. **`stella-engine` cannot be embedded through its own facade**
   (`crates/stella-engine/src/lib.rs:122,132`). `Provider` is re-exported but
   `CompletionRequestRef`, `CompletionResult`, `CompletionUsage` and
   `CheckpointSink` are not, so no host can implement the crate's primary port
   without also linking `stella-protocol`. The crate's own tests import them
   directly, and `stella-serve` reaches around the facade. This is the
   drop-in-engine story failing at its first contact with a host.
4. **`stella-runtime` is unwired.** `RuntimeBuilder`, `SessionRuntime`,
   `RuntimeSpec` and `with_provider` have zero consumers workspace-wide, and
   `stella-serve` does not depend on the crate the docs say exists for it. The
   nineteen-step ordering it promises to centralise still lives in the CLI.
5. **`no_ambient_reads` guards only its own five files.** Everything the crate
   constructs is exempt: `Store::open` reads `STELLA_STORE_DURABILITY`,
   `stella-store/home.rs` reads HOME/APPDATA/XDG, `stella-model/credential.rs`
   falls back to env. The documented property does not hold below the crate.
6. **Tool-call ids collide across sub-agent boundaries**
   (`crates/stella-serve/src/remote.rs:506`). Two executors mint `tool-{n}` from
   independent counters over one `Pending` registry whose `insert` replaces on
   collision. `RemoteProvider` grew an `instance` field to fix exactly this, with
   the reasoning written down; the tool port never got it.
7. **The exported wire contract omits `ScopeReviewResultIn`**
   (`schema_export.rs:84`), so a host is told to expect a `scope_review_request`
   frame and given no type for the answer.
8. **Two hand-maintained pricing tables disagree ~25%** on the same model,
   citing the same source on the same day (`arenabench/pricing.py:73` vs
   `bench/terminal_bench_analysis/normalized_cost.py:125`).
9. **`ReasoningPosture::Controllable` conflates "honoured exactly" with
   "silently downgraded"** (`provider_parity.rs:163`). Four of seven Controllable
   rows collapse effort tiers, while the notice that would warn the user fires
   only for `Unsupported`.
10. **The brand's own lowercase rule is unenforced.** 389 capitalised `Stella`
    against 1806 lowercase in `website/content/docs`, including `description:`
    frontmatter that becomes each page's meta description and llms.txt entry.

## Dimension scores

Discovery-grade, from 39 unit agents. Spread is published because a dimension
where agents disagreed by 44 points is telling you something a median hides.

| Dimension | Score | Spread | Weight |
|---|---|---|---|
| system_architecture | 85 | 33 | 3 |
| loop_correctness | 84 | 24 | 3 |
| security | 84 | 21 | 3 |
| agent_performance | 82 | 19 | 3 |
| durability | 85 | 30 | 2.5 |
| token_efficiency | 85 | 20 | 2.5 |
| end_user_experience | 84 | 24 | 2 |
| maintainability | 81 | 44 | 2 |
| compute_efficiency | 80 | 32 | 2 |
| documentation | 88 | 32 | 1.5 |
| vendor_lock_avoidance | 85 | 34 | 1.5 |
| file_organization | 82 | 30 | 1.5 |
| data_storage | 80 | 44 | 1.5 |
| formatting | 88 | 15 | 1 |
| language | 87 | 11 | 1 |
| symbol_names | 86 | 12 | 1 |
| file_names | 85 | 20 | 1 |

The two widest spreads — `maintainability` and `data_storage`, both 44 — are
the honest signal in this table. Agents that read the store's migration and
prune paths scored it very differently from agents that read its telemetry and
export paths, and that disagreement is itself a finding: the crate does not
have one standard of care.

## How to finish this round

The partition, per-unit notes and measured baseline are reusable; the expensive
part is the agent time, not the setup.

```sh
python3 ~/.claude/skills/ultraudit/scripts/build_workflow.py \
  --gate gate.json --units units.json --baseline baseline.txt \
  --depth deep --history ~/.claude/audits/stella.json --out audit.js
```

Run it when the session budget can absorb ~240 agents in one window; a resume
across a limit reset works (this round did it twice) but the transcripts are
deleted with the worktree, so **commit before the worktree goes away**. This
round lost 128 files of applied fixes that way and re-derived them from the
findings.
