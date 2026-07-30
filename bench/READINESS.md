# Terminal-Bench 2.1 — Readiness Report

Prepared 2026-07-23 as the offline preparation for the maintainer-audited public
Stella row described in [`terminal-bench-2.1-protocol.md`](terminal-bench-2.1-protocol.md).
Re-frozen 2026-07-30 under [#909](https://github.com/macanderson/stella/issues/909).

**Status: NOT submission-ready. The claim path was silently unlaunchable for
five days, and the audited run's host requirement is still unmet.** Two things
changed since the 07-23 report, and both had to be found by actually trying to
run the harness rather than by reading it:

1. **The claim launcher could not launch.** [#659](https://github.com/macanderson/stella/pull/659),
   an automated dependency update, moved `harbor==0.6.1` to `0.20.0` in
   `bench/harbor_adapter/pyproject.toml` and touched none of the five other
   sites that name that version — including `secure_launcher`'s guard, which
   *refuses to run* unless the installed version matches exactly. Every one of
   its 57 tests failed on that guard, unnoticed, because nothing had ever run
   the harness and `bench.yml` is path-filtered to `bench/**`. The pin is
   restored and `.github/dependabot.yml` now excludes the package.
2. **A development baseline has been measured and published.** See §8. It is
   *not* the audited row and does not become one; it exists so Stella has a
   falsifiable number at all, which is what #909 was opened about.

The audited row still needs the same three irreversible maintainer steps (mint
the spend-limited key, publish the machine-readable preregistration + intent
ledger, launch through `secure_launcher.py` on a **dedicated native x86_64 Linux
host**), plus the external Terminal-Bench maintainer trajectory review that is
outside this repository's control.

---

## 1. Frozen system under test (SUT)

Re-frozen 2026-07-30. The previous freeze (`fa2ec5b`, 0.5.1) is superseded: it
was 1.7 minor versions behind `main`, so a run against it would have measured
code nobody ships — the specific objection #909 raises.

| Field | Value |
|---|---|
| SUT commit | `0eeb8d4d9272e7416d3ebf09286d67adf534c696` |
| `git describe` | `v0.5.68-68-g0eeb8d4d` (version **0.6.10**) |
| Public? | Yes — **`origin/main` itself**, unmodified, no local patch of any kind |
| Previous freezes | `fa2ec5b` (0.5.1), `ec7ee03` (0.4.49) — both **superseded** |
| Frozen binary | `target/x86_64-unknown-linux-gnu/release/stella` |
| Binary format | ELF 64-bit x86-64 PIE, glibc 2.17 floor (`…-gnu.2.17`), stripped |
| **Binary SHA-256** | `cfb2b8ee7518e0c2dfe515d85e905f9ab0a103e55b7e2fc280d42a02793b160f` (reference — *host-specific*, see note) |
| Build stamp | `STELLA_BUILD_GIT_SHA=0eeb8d4d9272e7416d3ebf09286d67adf534c696` |
| Verified in container | `stella --version` → `stella 0.6.10-dev.0eeb8d4d…`; the adapter re-verified both the uploaded binary SHA and the embedded source commit on every trial |

> **Reproducibility:** release builds bake in the builder's rustup/cargo source
> paths (under `/Users/macanderson/…` here), so the byte-exact SHA above is
> host-specific and will differ on another machine. It is a *reference* proving
> the toolchain works and the stamp is correct — the authoritative binary
> identity is the source-commit stamp plus the SHA the run manifest freezes for
> the exact uploaded binary (the adapter re-verifies the upload SHA per trial).
| Toolchain | rustc/cargo 1.97.0 via `rustup which`; zig 0.16.0 + `cargo-zigbuild`; per-build Zig caches |

### Freeze decision (maintainer-approved)

The protocol was originally frozen against `ec7ee03` (0.4.49) with re-freeze
allowed only on a *telemetry-only-corrected* commit. Public `main` has since
advanced to **0.5.1** — 146 commits, +111,716/-23,110 across 550 files, a minor
bump that is **not** telemetry-only (it adds `apply_edits`, `stella arena`,
adaptive-context Phase 0/1, graph-derived planner, etc.). Because **no
preregistration has started** (no dedicated preregistration issue, no
`bench/evidence/`, no paid run — the earlier `#301` push was pre-publication
scaffolding), pinning the SUT to the current public release is *design
finalization before the audit clock starts*, not tampering. The maintainer chose
to finalize the SUT to the current public 0.5.1 commit `fa2ec5b`. The protocol's
"Immutable system under test" section has been updated to disclose this honestly.

> **Run-time note:** the SUT binary is stamped with, and must be run with,
> `STELLA_SOURCE_COMMIT=fa2ec5bdae6db739628f2c37bad2ffb3ce6fe4ef`. The
> preregistration-reconciliation commit (this PR) is a *descendant* that records
> the SUT; do **not** stamp/verify the binary against the PR commit SHA.

---

## 2. Blocker clearance (run-ledger amendment)

The protocol amendment forbids any further paid run until Stella "emits usage for
every paid model call, retains aborted turn spend, suppresses or meters post-turn
headless reflection, passes focused tests, and the Linux binary is rebuilt and
frozen." Verified on `fa2ec5b`:

| Criterion | Evidence |
|---|---|
| Usage emitted per paid call; abort-spend retained | Focused tests green: `stella-store` usage_completeness 5/5, `stella-cli` usage_completeness 3/3, `stella-pipeline` usage 8/8 — incl. `triage_success_emits_usage_before_budget_abort` and `aborted_pipeline_totals_match_every_management_and_execute_usage_record` |
| Fail-closed accounting | `AgentEvent::UsageIncomplete`; store migration v8→v9 "fail-closed paid-call accounting" (`usage_complete` column) |
| Reflection suppression | Behavior-verified at the gate: `agent::tests::explicit_reflection_opt_out_suppresses_every_one_shot_format` sets `STELLA_DISABLE_REFLECTION` and asserts `one_shot_reflection_enabled()` is `false` for Text/Json/StreamJson (2/2 green, incl. truthy-value parsing). End-to-end "zero post-answer model call" is first confirmed behaviorally by the paid readiness sentinel — this offline gate proves the decision, not the full round trip. |
| Linux binary rebuilt & frozen | See §1 (SHA `9069b990…`) |

---

## 3. Offline audit gates

| Gate | Result |
|---|---|
| Focused telemetry/usage-completeness tests | ✅ store 5/5, cli 3/3, pipeline 8/8 |
| CLI-contract smoke test (`bench/smoke/smoke_test.py`) | ✅ 5/5; `stella --version` = `stella 0.5.1` |
| Analyzer self-tests (`terminal_bench_analysis/tests`) | ✅ 234 passed |
| Harbor adapter self-tests (`harbor_adapter/tests`) | ✅ 226 passed (after the two fixes in §4; was 224/2) |
| Engine-posture parses through Stella's strict seam | ✅ `config::tests::the_benchmark_engine_posture_survives_the_trusted_launcher_seam` — proves `headless_scope_bypass:"on"` is accepted, not fail-closed |
| Readiness-fixture integrity (`synthetic-adapter-sentinel`) | ✅ no drift since the `#301` freeze (only commit touching it) → still hashes to pinned `05a040c7…`; value pinned in `test_secure_launcher.py` |
| Secret scan over git-tracked publication source | ✅ 1 finding = the scanner's *own synthetic test fixture* (`test_artifact_secret_scan.py`), by design; no real credential. (`.venv` hits are third-party dependency data, not publication content.) |
| Cross-build toolchain reachable from macOS host | ✅ produced the frozen ELF (§1) |

---

## 4. Reconciliation performed (SUT-drift consequences)

Freezing on 0.5.1 surfaced post-freeze drift in the frozen preregistration
artifacts. All reconciled in this PR; the complete coupled surface was verified
(the only SUT-derived sha256 literals in the protocol are the 3 posture hashes —
every other digest is an external dataset/comparator/fixture value, unchanged).

1. **Engine-posture SHA-256 recomputed for 0.5.1.** PR #322 added
   `headless_scope_bypass: "on"` to the canonical posture *after* the #301
   freeze, changing every posture hash. Recomputed via the adapter's own
   `_benchmark_engine_posture` (the same function `secure_launcher.py` uses to
   emit the manifest hashes, so hand values cannot diverge from the machine
   manifest):

   | model | frozen (stale) | recomputed 0.5.1 |
   |---|---|---|
   | deepseek-v4-pro | `fb18233a…` | `1740fa2f3f1bea66c348c7ffca151f526019ef0278829d23acb391e7b2f07159` |
   | z-ai/glm-5.2 | `de2a3109…` | `9b94f231d91e66c9793e2f61dd8c6edbb4472ea38e431681b5e854d9d22191ea` |
   | x-ai/grok-4.5 | `f43d8a25…` | `3c7d61553b7a4665ed974e6b32a7a20c1f8c59acaae2bcab3848eec2a39ca8dc` |
   | z-ai/glm-5.1 (primary) | — (manifest-generated at freeze) | `55fdf3421ae4c8625ab8bdedb11a59867b6d81a20ad378a2080e7e944229f4bd` |

   Updated in: protocol calibration table, protocol posture prose,
   `terminal_bench_analysis/README.md` example, and the adapter test assertion.

2. **Stale adapter test fixtures fixed.** `test_hashes_exact_uploaded_binary_and_records_source_commit`
   used an `_Environment` stub lacking `task_env_config`; `install()` →
   `_build_code_graph()` legitimately reads `task_env_config.workdir` to run
   `stella init` (code-graph indexing added after the fixture was written). Added
   the attribute. The posture assertion was updated to the recomputed hash.

> ✅ **CI gap fixed:** stella's CI was Rust-only (`fmt + clippy + test`), so the
> Python adapter/analyzer suites were **not gated** — which is how the adapter
> suite drifted red unnoticed. Added `.github/workflows/bench.yml` (gates both
> `pytest` suites on `bench/**` changes) and a `make bench-test` target for local
> parity, so this drift class cannot recur silently.

---

## 5. ⚠️ MAINTAINER SIGN-OFF REQUIRED — `headless_scope_bypass: "on"`

This is a **score-determining** posture setting, not hash bookkeeping. With it
**off**, a headless trial with no operator to approve an over-threshold plan
self-terminates any plan exceeding the step threshold (>5 steps) — most
multi-step Terminal-Bench tasks would be unwinnable. #322 set it **on** (after
the #301 freeze); that is the *entire* reason the posture hashes moved. It is
kept **on** (defensible — a disposable container with a per-trial budget cap as
the real guard) and is now disclosed in the protocol's posture prose. **The
value is defensible; the disclosure + your explicit sign-off is what's
mandatory** — an undisclosed score-affecting posture change is exactly what a
maintainer trajectory review fails.

---

## 6. Remaining steps — HUMAN-ONLY, in order

None of these were performed here; each is irreversible and/or spends real money
against the `$200` all-in OpenRouter authorization.

1. **Create the dedicated benchmark key** — via the Management API key:
   `name="stella-tb21-dedicated-key-v1"`, `limit=180`, `limit_reset=null`,
   `include_byok_in_limit=true`, `disabled=false`. Record fingerprint, verified
   name, and usage snapshots (never the raw value).
2. **Host attestation** — on a dedicated native x86_64 Linux Docker host meeting
   the thresholds (≥4 vCPU, ≥31 GiB `MemTotal`, ≥150 GiB free, zero unrelated
   containers). Commit `bench/evidence/host-attestations/<intent_sha256>.json`
   (`stella-tb21-host-report-v1`) in the intent's ledger commit.
3. **Publish the preregistration** — the dedicated owner-authored preregistration
   GitHub issue with its six unedited machine-readable comments (3
   preregistrations + 3 paid intents) and the append-only ledger snapshots. The
   single deliberate push of protocol + analyzer + readiness fixture + adapter +
   launcher **is** the readiness preregistration; get everything correct locally
   first so it is one push (a corrected re-push muddies the timestamp).
4. **Pre-launch scans on the resolved tree** — repeat the dataset scan
   (Dockerfiles/Compose/`task.toml` for Stella controls, cred names, `BASH_ENV`,
   `LD_PRELOAD`) after final cache resolution, and run `artifact_secret_scan.py`
   over the complete publication tree **with `--require-env`** (live key present).
5. **Readiness sentinel** — one attempt, `openrouter/deepseek/deepseek-v4-pro`,
   job `stella-readiness-synthetic-v1` (~$0.17). Gate to proceed: no agent
   exception, terminal `complete`, return code 0, external-verifier reward
   exactly `1.0`.
6. **Calibration** — 60 trials, job `stella-tb21-calibration-20260721` (~$10.20);
   apply the frozen selection rule.
7. **Primary (GLM-5.1)** — 445 trials, `n_concurrent=1`, `retry.max_retries=0`
   (~$75.65). Then apply the registered `64.72%` / `358,905,384`-token thresholds
   and the 79-task bootstrap confidence procedure.
8. **External review** — submit for the Terminal-Bench maintainer trajectory
   review (not in this repo's control).

Before each paid job the secure launcher fetches `/credits` and refuses launch if
the nominal allocation would cross the live balance or the remaining `$200`
authorization.

> ⚠️ **The `$200` figure is stale, and the nominal plan no longer fits inside
> it.** Measured 2026-07-30: `total_credits` 510.00, `total_usage` 433.26 →
> **$76.74 actually spendable**. The plan above totals `$86.02`, so the
> launcher's own `/credits` preflight would refuse the primary. Either top the
> account up or re-scope the study before step 1.

> ⚠️ **Step 5's gate cannot currently be passed.** It requires "no agent
> exception … return code 0", and the non-exit defect in §8.2 makes both
> impossible: the sentinel earns reward `1.0` and still ends as
> `AgentTimeoutError` with `stella_return_code_state: "unknown"`. Fix that first,
> or the audited run stops at its own readiness gate.

---

## 7. Artifact index

- Frozen binary: `target/x86_64-unknown-linux-gnu/release/stella` — sha256 `9069b990…` (host-specific reference, see §1 note). This preparation build relaxed the `#install` provenance guard from `==origin/main tip` to *ancestor-of-public-ref* so it could target the specific already-public release commit `fa2ec5b` rather than the moving tip. **The maintainer's actual paid claim build must use the stock, unmodified `#install` procedure (the `==@{upstream}` guard) against the final preregistration commit** — which will be the `origin/main` tip at that point, so the stock guard passes unchanged. Do not copy the relaxed guard into the paid run.
- Reconciled: `bench/terminal-bench-2.1-protocol.md`, `bench/terminal_bench_analysis/README.md`, `bench/harbor_adapter/tests/test_adapter.py`.

---

## 8. What switching the harness on actually found (2026-07-30, #909)

Everything in §§1–7 was established by *reading* the harness. #909's point was
that nothing had ever *run* it. Running it surfaced three things that no amount
of offline review had, and the first two are blockers for the audited row.

### 8.1 The claim path had been unlaunchable for five days — and CI said so

[#659](https://github.com/macanderson/stella/pull/659) moved `harbor==0.6.1` to
`0.20.0` in `bench/harbor_adapter/pyproject.toml` — one line, nothing else. The
version is not a dependency, it is an audited constant named in six places, one
of which is `secure_launcher`'s guard that refuses to launch on a mismatch. The
whole launcher suite failed on that guard: **57 failed / 169 passed**.

**The gate fired. The PR merged anyway.** `bench.yml` ran on #659 and reported
`harbor_adapter + analyzer pytest` → **FAILURE**; it was merged 12 minutes later.
Nothing malfunctioned. `main`'s protection requires exactly two contexts —

```
"fmt + clippy + test"
"cargo deny + cargo audit"
```

— and the bench suite is not one of them, so a red bench check does not block a
merge. It is decoration.

Pin restored → adapter **226 passed**, analyzer **240 passed**.
`dependabot.yml` now excludes the package in both bench ecosystems.

**Recommended, and owner-only:** add `harbor_adapter + analyzer pytest` to
`main`'s required contexts. Until then this recurs by construction — the next
bot bump will also go red and also merge. (Not done here: changing branch
protection is a repository-settings decision, not a code change.)

Two lessons, and the second is the expensive one:

* **An unexercised guard is indistinguishable from a broken one.** The version
  pin, the posture hashes, and the readiness fixture are all "verified" in §3 in
  exactly the sense this pin was — by reading, never by running.
* **An advisory gate is indistinguishable from no gate.** The failing check was
  visible on the PR for anyone who looked. Nobody had to look, so nobody did.

### 8.2 Stella completes its turn but the process does not exit

The readiness sentinel earns external-verifier reward **`1.0`** — Stella
diagnoses the `slugify` bug correctly (trailing-dash `strip("-")`), all three
tests pass, `stella_status: "completed"`, 7 steps, 141 stream events, accounting
state `complete`, $0.0297.

It is nevertheless recorded as a failure. The agent phase consumed exactly
300.06s of its 300s budget and ended in `AgentTimeoutError`, with
`stella_return_code_state: "unknown"` — Harbor waits for process exit, and the
process never exits after emitting its terminal `complete` event. (That event is
emitted **twice**, which is likely a related thread of the same defect.)

Consequences, in order of how much they matter:

1. **§6 step 5's gate is unpassable** — it requires no agent exception and return
   code 0. The audited run cannot legally start.
2. **Every trial burns its entire wall-clock timeout** regardless of when the
   work finished, so a 78-task phase costs the sum of its timeouts rather than
   the sum of its work. It also makes `exception_stats` useless as a health
   signal: successful trials and genuinely-timed-out ones are indistinguishable.
3. **The score is unaffected**, which is the only reason the run below is still
   meaningful. The verifier runs after the agent phase against the files Stella
   already wrote, so a lingering process costs wall clock, not correctness.

### 8.3 Registry flakiness can silently become "Stella failed"

The first launch attempt lost four trials in its opening minutes to
`TLS handshake timeout` while Docker Hub served image blobs. The container never
started, so the agent never ran and spend was **$0.0000** — but with
`--max-retries 0` each is a permanent reward-`0` row, and under a fixed
denominator that is arithmetically identical to Stella failing the task.

The attempt was aborted under the preregistration's operational-failure clause.
No reward was observed on any trial, so there was no outcome to select on, and
the relaunch is a fresh job name rather than a resume. The fix is to pre-pull all
89 task images with retries first, making image availability a **precondition**
of the run instead of a term in the measurement. Any future runner on a
consumer network should do the same.

### 8.4 The measured baseline

See `bench/evidence/` for the run manifest, per-trial rows, per-task results and
the two scripts that recompute the number from them. Its preregistration is
[#950](https://github.com/macanderson/stella/issues/950), filed before any paid
call. The manifest's `claim_eligibility` block states in full why it is a
development baseline and not the audited row — start there before quoting the
number anywhere.
