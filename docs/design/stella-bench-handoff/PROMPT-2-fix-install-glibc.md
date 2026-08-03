# Task: two host-side integrity defects in the Terminal-Bench adapter

You are working in the `stella` repo (`~/Projects/stella`). Two independent
defects, both in `bench/`. They are opposites, which is why they belong in one
PR: Part 1 is a **false negative** — a broken binary reaches the container and
nothing complains. Part 2 is a **false positive** — a loud error that is almost
always wrong. Together they mean the run log cannot be trusted in either
direction.

Evidence for both: `~/Desktop/stella-vs-claudecode-traces-2026-07-31.zip`
(also unpacked at `~/Desktop/bundle/`).

---

# PART 1 — a non-portable Stella binary can reach a benchmark container

## Symptom

During a Terminal-Bench 2.1 run on 2026-07-31, some trials died before the
agent ever started:

```
sha256sum /tmp/stella-upload && cp /tmp/stella-upload /usr/local/bin/stella \
  && chmod +x /usr/local/bin/stella && /usr/local/bin/stella --version
-> exit 1
/usr/local/bin/stella: /lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.32' not found
/usr/local/bin/stella: /lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.33' not found
/usr/local/bin/stella: /lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.34' not found
```

Harbor records this as `NonZeroAgentExitCodeError` and the trial scores 0.
It is indistinguishable in the results from the agent failing the task.

Affected trials: `bundle/live-run-hh8/jobs/hh8-armA-stella/qemu-alpine-ssh__*/`
and `bundle/rig-runs/jobs/dev2-armA-stella/qemu-startup__*/` (see `result.json`).

## Root cause — already diagnosed, do not re-derive

**This was an operator error, not a defect in the shipped build.**

`bench/evidence/run/build_sut.sh` already builds the portable binary correctly:

```bash
"$(rustup which cargo)" zigbuild --release --locked \
  --target x86_64-unknown-linux-gnu.2.17 --package stella-cli --bin stella
```

glibc 2.17 (RHEL 7 era) runs essentially anywhere. But the run in question was
provisioned with a hand-rolled script that did:

```bash
cargo build --release --locked -p stella-cli --bin stella
cp target/release/stella target/x86_64-unknown-linux-gnu/release/stella   # <-- the bug
```

That produced a glibc-2.35 binary (Ubuntu 22.04 build host) and copied it into
the exact path `bench/evidence/run/env.sh` expects via `STELLA_BINARY`. Every
downstream integrity check passed, because they all check the *wrong* property:

- the SHA-256 host/container match passed (it is the same file both sides)
- `--version` was the first thing to actually exercise the dynamic linker

## What to build

The real gap is that **nothing verifies the binary is the portable build**. A
substituted binary silently produced false failures and would have done so
across all 89 tasks. Close that.

1. **Preflight guard.** Before a run starts — the natural home is `preflight`
   in `bench/evidence/run/env.sh`, which already validates run inputs — assert
   that `$STELLA_BINARY` requires no glibc symbol above the floor the SUT build
   targets. `readelf --version-info` or `objdump -T` will list the required
   `GLIBC_x.y` versions; the max must be `<= 2.17`. Fail closed with a message
   that names the offending symbol and says to run `build_sut.sh`.
   This must run on the **host**, before any container is created — a check
   that only fires per-trial has already wasted the run.

2. **Adapter-side error quality.** In
   `bench/harbor_adapter/stella_harbor/__init__.py` (~line 1124, the
   upload/install block), a `--version` failure mentioning `GLIBC` currently
   surfaces as a generic non-zero exit. Classify it distinctly so it can never
   be silently counted as an agent failure. There is precedent for this kind of
   fail-closed classification in the same file.

3. **Decide the Alpine/musl question and write down the answer.** glibc 2.17
   fixes hosts with *older glibc*. It does **not** help a container with no
   glibc at all (Alpine ships musl). Check whether any Terminal-Bench 2.1 task
   image is musl-based — the image list is in this folder, or inspect the images
   directly. If any are, a `x86_64-unknown-linux-musl` static build is needed
   as well. The dependency tree supports it: `reqwest` is already
   `default-features = false, features = ["rustls-tls", ...]` (no OpenSSL) and
   `rusqlite` uses `bundled` (compiles SQLite from source). If no task image is
   musl-based, say so explicitly and do not add a second target — an unused
   build path is a liability.

4. **Regression test.** A test that would have caught the substituted binary.
   Assert on the *artifact*, not on the build command having been typed.

---

# PART 2 — "could not write stella-events.jsonl" is a false alarm that hides real ones

## Symptom

The same run logged this 11 times:

```
stella-adapter: could not write stella-events.jsonl: [Errno 13] Permission denied:
  '/home/ubuntu/tb21/jobs/hh8-armA-stella/write-compressor__r2sneBP/agent/stella-events.jsonl'
```

Also for `kv-store-grpc`, `schemelike-metacircular-eval`,
`torch-tensor-parallelism`, `openssl-selfsigned-cert`, `log-summary-date-ranges`,
`regex-chess`, `regex-log`, `mteb-leaderboard`, `circuit-fibsqrt`,
`merge-diff-arc-agi-task`.

## What was actually measured — verify this, then fix it

**No telemetry was lost.** All 11 trials that logged the error have a *complete*
event stream ending in a `complete` event. Confirmed by walking every
`agent/stella-events.jsonl` in `bundle/live-run-hh8/jobs/hh8-armA-stella/`:

- 11 of 11 errored trials → stream ends in `complete`
- 9 trials have truncated streams → **none of them logged this error**; they are
  explained by `AgentTimeoutError` (5), the run being stopped mid-flight (3),
  and a non-zero exit (1)
- 1 trial has no events file at all (`qemu-alpine-ssh`) → that is the Part 1
  GLIBC crash; the binary never ran, so it never emitted events

So the error and the data loss are **anti-correlated**. Re-verify this before
you change anything; if your numbers disagree with the above, trust yours and
say so.

## Mechanism

The events file arrives by two paths that race:

1. Harbor downloads it from the container (`/logs/agent/stella-events.jsonl`,
   referenced around `__init__.py:1218`) — lands **root-owned**
2. `_write_log()` (`__init__.py:1793`) also writes it host-side as `ubuntu`:
   ```python
   path.parent.mkdir(parents=True, exist_ok=True)
   path.write_text(content, encoding="utf-8")
   ```

Whichever loses the race hits the other's file. `write_text` on a root-owned
file as `ubuntu` raises `EACCES`, caught at `__init__.py:1802` and printed as
"could not write". This matches the observed ownership split: files that logged
the error were `root:root`, files that did not were `ubuntu:ubuntu`.

## Why it matters even though nothing was lost

The message is **identical** whether the artifact is already present and
complete, or telemetry was genuinely lost. Eleven false alarms in one run train
the reader to ignore it, and a real loss would be invisible among them. A
monitoring line that cannot distinguish "fine" from "broken" is worse than none.

## What to build

1. **Decide which copy is authoritative** — the container download or the
   host-side write — and stop doing both. A redundant write whose only
   observable effect is an error is not redundancy.
2. If both must exist, make the host write **non-destructive**: skip when a
   file is already present, or compare content/digest and only then replace.
   Do not fix this by `chown`/`chmod`-ing in the harness or running the adapter
   as root — that widens privilege to silence a log line.
3. **Make the message honest.** Distinguish at minimum:
   - artifact already present and complete → debug-level, or silent
   - artifact absent and unwritable → loud, and it should mark the trial's
     telemetry as incomplete somewhere a reader can find it
   Apply the same treatment to the sibling `_TRAJECTORY_NAME` path at
   `__init__.py:1781`, which has the identical shape and the identical flaw.
4. **Regression test** covering both orderings: container copy lands first, and
   host write lands first. The test should assert that a complete artifact never
   produces a scary log line, and that a genuinely missing one always does.

---

## Constraints (both parts)

- Do not weaken or delete the SHA-256 host/container check. It is correct; it
  just answers a different question ("did the file arrive intact") than the one
  that failed in Part 1 ("can this file run here").
- Do not change the benchmark posture or scoring. Frozen measurement artifacts.
- `rg` / `fd`, not `grep` / `find`.
- Check gates by exit code, not by grepping output — this repo forces ANSI
  colour into piped output, so `rg "^error"` misses real failures.
- No Claude attribution in commits, tags, or PR text.
- Work in a git worktree; open a draft PR when green.

## Deliverable

One PR covering both parts, each with a regression test:

- **Part 1** — a non-portable `STELLA_BINARY` fails loudly on the host before a
  run begins, plus a written answer on the musl question. State plainly in the
  PR body that the observed failures came from a provisioning script that
  bypassed `build_sut.sh`; the fix is a guard against that class of mistake, not
  a change to how the shipped binary is built.
- **Part 2** — the events/trajectory write path no longer reports failure when
  the artifact is intact, and does report it unmissably when it is not.
