#!/bin/bash
# Shared environment + preflight for a Frontier-Bench development-baseline run.
# Sourced by every other script here. Never echoes a credential.
#
# Frontier-Bench is Harbor's successor to Terminal-Bench: 74 tasks across seven
# domains, where Terminal-Bench 2.1's ceiling had already been cleared. The
# agent side is identical — the same `stella_harbor:StellaAgent` installed-agent
# runs both, unmodified — so everything specific to this benchmark lives in this
# directory and nothing under `bench/harbor_adapter/` changes. That is
# deliberate: the adapter's Python tree is digest-frozen for the audited claim
# path, and a benchmark that only differs in *which dataset and how it is
# scheduled* must not perturb it.
#
# The shared half — the glibc floor, the target triple, the SUT path, the
# portability assert, the forced amd64 platform — is sourced from the
# Terminal-Bench lane rather than copied, so the two cannot drift. The halves
# that genuinely differ are re-declared below with the reason.
set -uo pipefail

FB_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# The frozen dataset, pinned by content digest. An unversioned name is not a
# freeze, and `@latest` least of all: Frontier-Bench is explicitly a *moving*
# benchmark — the announcement's whole point is that tasks are added and revised
# continuously — so the name that resolved to 74 tasks today resolves to a
# different set next month. Terminal-Bench could get away with a name because it
# had stopped moving. This one cannot.
#
# This exact digest is the leaderboard's own `DATASET_REF`. Resolving the
# dataset by name today yields
#   sha256:63f363a191f0a0429fd1c5b318080616bab839473ce27e39f44868d327b03a89
# instead, which exports a *byte-identical* 74-task tree — verified by diffing
# both exports, no file differs. They are two version records over the same
# content. The leaderboard's static analysis compares the ref *string* a job
# recorded, not the tasks it contains, so a run pinned to the equally-valid
# other digest is rejected on submission while being, in substance, the same
# run. Pin the string CI is looking for.
#
# Re-resolve for a newer snapshot with:
#   harbor download frontier-bench/frontier-bench --export -o <dir>   # prints it
export FB_DATASET="${FB_DATASET:-frontier-bench/frontier-bench@sha256:97fd2ba3aabdda16823a1a8ea695a3875e50e800caa60b450686deedc7171763}"
export FB_TASK_PREFIX="frontier-bench"
export FB_TASK_COUNT="${FB_TASK_COUNT:-74}"

# Trials per task. One is right for a development baseline, where the question
# is "does the harness work and roughly where do we land". It is *not* a
# leaderboard submission: that requires every task covered at five or more
# trials each, and CI counts them. Raising this multiplies cost by the same
# factor, so it is a deliberate act, not a default.
export FB_ATTEMPTS="${FB_ATTEMPTS:-1}"
export FB_SUBMISSION_MIN_ATTEMPTS=5

# Frontier-Bench tasks run far longer than Terminal-Bench ones: declared agent
# timeouts run from 30 minutes to 8 hours, with a 2-hour mode, against expert
# time estimates measured in whole days. Terminal-Bench's $0.60 per-trial cap
# would truncate most of this set mid-task and report the truncation as a task
# failure, which is a measurement error, not a thrifty default. `-` and not `:-`
# for the same reason as the Terminal-Bench lane: an explicitly empty value
# stays empty and means *no cap*, which is the posture a leaderboard-facing run
# has to be able to take.
export STELLA_BUDGET="${STELLA_BUDGET-2.50}"

# What the sourced Terminal-Bench env would otherwise default. Set before the
# source so its `${TB_DATASET:-...}` sees a value and leaves it alone; nothing
# here reads TB_DATASET afterwards, but leaving it pointing at Terminal-Bench
# while this lane runs Frontier-Bench is the kind of stale-variable trap that
# ends up in a log line and then in a claim.
export TB_DATASET="$FB_DATASET"

# shellcheck source=bench/evidence/run/env.sh
source "$FB_HERE/../run/env.sh"

# --- Everything below overrides the Terminal-Bench lane on purpose. ---

# Harbor 0.6.1 is the Terminal-Bench claim's audited constant. It cannot be this
# lane's constant. Every one of the 74 Frontier-Bench tasks declares
# `environment_mode = "separate"` under `[verifier]`, and 0.6.1's VerifierConfig
# has no such field — pydantic's default `extra="ignore"` drops it silently, so
# 0.6.1 runs the whole set with the verifier sharing the agent's container
# instead of the separate one the task asked for. It does not warn, and the run
# still produces rewards. Those rewards answer a different question than the
# leaderboard's, which is the worst possible failure mode: plausible numbers.
#
# So this lane pins its own Harbor, in its own virtualenv, and asserts it. The
# Terminal-Bench venv at bench/harbor_adapter/.venv is left untouched.
export FB_HARBOR_VERSION="${FB_HARBOR_VERSION:-0.20.0}"
export VENV="$FB_HERE/.venv"
export PATH="$VENV/bin:$PATH"
export PYTHONPATH="$TB_REPO/bench/harbor_adapter"

# Distinct from the Terminal-Bench lane's, so the two can share one TB_ROOT
# without one run's dataset export, resource plan, or job tree overwriting the
# other's.
export DATASET_DIR="$TB_ROOT/dataset-frontier"
export JOBS="$TB_ROOT/jobs-frontier"
export FB_PLAN="$TB_ROOT/frontier-plan.json"
mkdir -p "$JOBS"

# Four tasks (exam-pdf-eval, fp8-rmsnorm-gemm, jax-speedrun-gpu,
# math-eval-grader) declare `gpus = 1`. On a host without a GPU they do not
# error in any way that is distinguishable from Stella failing: the container
# comes up, the work is impossible, the verifier returns 0.0, and the row is
# arithmetically identical to a genuine miss. Terminal-Bench had no such tasks
# and so no such hazard.
#
# The default is therefore to exclude them and say so, loudly, in the score's
# denominator — not to include them and quietly absorb four guaranteed zeros.
# Set FB_ALLOW_GPU=1 only on a host that actually has one; `plan.py` checks.
export FB_ALLOW_GPU="${FB_ALLOW_GPU:-0}"

# Memory the plan leaves to the Docker daemon itself rather than handing to a
# task. A task declaring more than what remains is excluded as unrunnable.
#
# This is deliberately conservative: `memory_mb` is a cap Docker will happily
# accept on a smaller VM, so an 8 GB task on a 10 GB daemon usually *starts* —
# and then competes with the daemon, swaps, and gets OOM-killed partway in,
# which arrives as a reward-0 row indistinguishable from a genuine failure.
# Excluding it and saying so is the honest version of the same outcome. Lower
# this to attempt those tasks anyway; the exclusion list names every one.
export FB_MEMORY_HEADROOM_MB="${FB_MEMORY_HEADROOM_MB:-2048}"

# Assert `harbor run` still advertises every flag these scripts pass it.
#
# Running two Harbor major versions in one repo means the CLI is a moving
# contract, and it has already moved once: 0.6.1's `--agent-import-path` was
# folded into `--agent` by 0.20.0, which accepts either a built-in name or an
# import path. The Terminal-Bench lane still passes the old spelling and is
# still correct to, because it is pinned to 0.6.1. This lane would have died on
# `No such option` at the first trial of the first run.
#
# Checking the help text costs one subprocess and turns that into one message.
fb_assert_cli_flags() {
  local help missing=""
  # Scrub ANSI first. Harbor renders help through Rich, which styles each
  # option individually and leaves escape sequences *inside* the flag names —
  # a literal search for `--env` over the raw 42 KB finds nothing, so an
  # unscrubbed version of this check reports every flag missing and fails
  # closed on a perfectly good CLI.
  help="$(harbor run --help 2>&1 | LC_ALL=C sed $'s/\033\[[0-9;?]*[a-zA-Z]//g')" ||
    { echo "FATAL: \`harbor run --help\` failed"; return 1; }
  for flag in --env --dataset --path --agent --model --job-name --jobs-dir \
              --include-task-name --n-attempts --n-concurrent --max-retries; do
    case "$help" in *"$flag"*) ;; *) missing="$missing $flag" ;; esac
  done
  test -z "$missing" || {
    echo "FATAL: harbor $(harbor --version) does not advertise:$missing"
    echo "       The CLI contract moved. Update this lane's scripts before running."
    return 1; }
}

fb_preflight() {
  test -n "${OPENROUTER_API_KEY:-}" || { echo "FATAL: OPENROUTER_API_KEY unset"; return 1; }
  test -x "$STELLA_BINARY" || { echo "FATAL: no SUT binary at $STELLA_BINARY (run ../run/build_sut.sh)"; return 1; }
  # Reused, not reimplemented: one definition of the glibc floor for both lanes.
  assert_portable_binary || return 1
  test "${#STELLA_SOURCE_COMMIT}" = 40 || { echo "FATAL: STELLA_SOURCE_COMMIT is not a full SHA"; return 1; }
  test "$(command -v harbor)" = "$VENV/bin/harbor" || {
    echo "FATAL: wrong harbor on PATH — expected $VENV/bin/harbor"
    echo "       Create this lane's venv with: bench/evidence/frontier/setup_venv.sh"
    return 1; }
  test "$(harbor --version)" = "$FB_HARBOR_VERSION" || {
    echo "FATAL: harbor $(harbor --version) != $FB_HARBOR_VERSION"
    echo "       0.6.1 silently ignores this dataset's verifier environment_mode."
    return 1; }
  fb_assert_cli_flags || return 1
  docker info >/dev/null 2>&1 || { echo "FATAL: docker unreachable"; return 1; }
}

# The Terminal-Bench lane's `preflight` is still defined and still asserts
# harbor 0.6.1, which is false here. Point the familiar name at this lane's
# check so a script that calls `preflight` out of habit fails safe.
preflight() { fb_preflight "$@"; }
