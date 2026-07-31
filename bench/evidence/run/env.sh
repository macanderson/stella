#!/bin/bash
# Shared environment + preflight for a development-baseline run.
# Sourced by every other script here. Never echoes a credential.
set -uo pipefail

: "${TB_REPO:?set TB_REPO to the repository root}"
: "${TB_ROOT:?set TB_ROOT to an absolute scratch directory for dataset/jobs/logs}"
case "$TB_ROOT" in /*) ;; *) echo "FATAL: TB_ROOT must be absolute"; exit 1 ;; esac
mkdir -p "$TB_ROOT"

export VENV="$TB_REPO/bench/harbor_adapter/.venv"
export PATH="$VENV/bin:$PATH"
export PYTHONPATH="$TB_REPO/bench/harbor_adapter"

# The SUT is cross-compiled against an old glibc so it runs in every task image,
# including the two on `debian:bullseye-slim` (glibc 2.31). Both halves of that
# contract are pinned here — `build_sut.sh` builds to this floor and `preflight`
# refuses to start a run with a binary that exceeds it — because keeping them
# apart is how a glibc-2.35 binary reached five trials on 2026-07-31 wearing the
# portable build's filename. Constants, not overridable defaults: an operator
# able to raise the floor to silence the error is the failure being prevented.
export STELLA_TARGET_TRIPLE="x86_64-unknown-linux-gnu"
export STELLA_GLIBC_FLOOR="2.17"
export STELLA_BINARY="$TB_REPO/target/$STELLA_TARGET_TRIPLE/release/stella"
export STELLA_BUDGET="${STELLA_BUDGET:-0.60}"
export STELLA_DISABLE_REFLECTION=1

# The frozen dataset, pinned by digest. An unversioned name is not a freeze.
export TB_DATASET="${TB_DATASET:-terminal-bench/terminal-bench-2-1@sha256:7d7bdc1cbedad549fc1140404bd4dc45e5fd0ea7c4186773687d177ad3a0699a}"
export TB_MODEL="${TB_MODEL:-openrouter/z-ai/glm-5.1}"

# Which arm of the verification ladder this run measures (#1007).
#
# Unset = the control arm: every role inherits TB_MODEL, so no author is
# independent of the worker and the authored-witness tier cannot run on any
# task. That is the arm every published number so far came from.
#
# Set to a second provider/model on TB_MODEL's provider (the roster's other
# entries are openrouter/deepseek/deepseek-v4-pro and openrouter/x-ai/grok-4.5)
# to run the treatment arm. One credential reaches the container, resolved from
# the worker's provider, so the author must share that provider. The adapter
# reads this on the host and expresses it only as `pipeline_judge_model` inside
# the hashed posture — it is never forwarded into the task container.
export STELLA_WITNESS_AUTHOR_MODEL="${STELLA_WITNESS_AUTHOR_MODEL:-}"
if [ -z "$STELLA_WITNESS_AUTHOR_MODEL" ]; then unset STELLA_WITNESS_AUTHOR_MODEL; fi

# Task images publish linux/amd64 only and the SUT binary is x86_64. Force the
# platform so a multi-arch base cannot build arm64 and then fail to exec it.
export DOCKER_DEFAULT_PLATFORM=linux/amd64

export JOBS="$TB_ROOT/jobs"
export DATASET_DIR="$TB_ROOT/dataset"
mkdir -p "$JOBS"

[ -f "$TB_ROOT/sut_commit.txt" ] && export STELLA_SOURCE_COMMIT="$(cat "$TB_ROOT/sut_commit.txt")"

# Assert the SUT binary can actually execute inside a task container.
#
# Every other integrity check answers "did the file arrive intact" — the
# host/container SHA-256 comparison passes because it is the same file on both
# sides. Nothing answered "can this file run here", and the first thing to touch
# the dynamic linker was `stella --version`, inside the container, per trial,
# after the run had already started. Harbor recorded the loader's rejection as
# NonZeroAgentExitCodeError and the trial scored 0.0, indistinguishable from the
# agent failing the task. This check runs on the host before any container is
# created, so a substituted binary costs one message instead of a run.
assert_portable_binary() {
  local binary="${1:-$STELLA_BINARY}"
  local checker="$TB_REPO/bench/harbor_adapter/stella_harbor/portability.py"
  command -v python3 >/dev/null 2>&1 || {
    echo "FATAL: python3 is required to verify SUT binary portability"; return 1; }
  test -f "$checker" || { echo "FATAL: portability checker missing at $checker"; return 1; }
  python3 "$checker" "$binary" --max-glibc "$STELLA_GLIBC_FLOOR" || {
    echo "FATAL: $binary cannot execute in a Terminal-Bench task container."
    echo "       Rebuild with bench/evidence/run/build_sut.sh — do not substitute"
    echo "       a plain \`cargo build --release\` artifact into the target/ path."
    return 1
  }
}

preflight() {
  test -n "${OPENROUTER_API_KEY:-}" || { echo "FATAL: OPENROUTER_API_KEY unset"; return 1; }
  test -x "$STELLA_BINARY" || { echo "FATAL: no SUT binary at $STELLA_BINARY (run build_sut.sh)"; return 1; }
  assert_portable_binary || return 1
  test "${#STELLA_SOURCE_COMMIT}" = 40 || { echo "FATAL: STELLA_SOURCE_COMMIT is not a full SHA"; return 1; }
  test "$(command -v harbor)" = "$VENV/bin/harbor" || { echo "FATAL: wrong harbor on PATH"; return 1; }
  test "$(harbor --version)" = "0.6.1" || { echo "FATAL: harbor $(harbor --version) != 0.6.1 (audited constant)"; return 1; }
  docker info >/dev/null 2>&1 || { echo "FATAL: docker unreachable"; return 1; }
}
