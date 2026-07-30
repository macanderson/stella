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

export STELLA_BINARY="$TB_REPO/target/x86_64-unknown-linux-gnu/release/stella"
export STELLA_BUDGET="${STELLA_BUDGET:-0.60}"
export STELLA_DISABLE_REFLECTION=1

# The frozen dataset, pinned by digest. An unversioned name is not a freeze.
export TB_DATASET="${TB_DATASET:-terminal-bench/terminal-bench-2-1@sha256:7d7bdc1cbedad549fc1140404bd4dc45e5fd0ea7c4186773687d177ad3a0699a}"
export TB_MODEL="${TB_MODEL:-openrouter/z-ai/glm-5.1}"

# Task images publish linux/amd64 only and the SUT binary is x86_64. Force the
# platform so a multi-arch base cannot build arm64 and then fail to exec it.
export DOCKER_DEFAULT_PLATFORM=linux/amd64

export JOBS="$TB_ROOT/jobs"
export DATASET_DIR="$TB_ROOT/dataset"
mkdir -p "$JOBS"

[ -f "$TB_ROOT/sut_commit.txt" ] && export STELLA_SOURCE_COMMIT="$(cat "$TB_ROOT/sut_commit.txt")"

preflight() {
  test -n "${OPENROUTER_API_KEY:-}" || { echo "FATAL: OPENROUTER_API_KEY unset"; return 1; }
  test -x "$STELLA_BINARY" || { echo "FATAL: no SUT binary at $STELLA_BINARY (run build_sut.sh)"; return 1; }
  test "${#STELLA_SOURCE_COMMIT}" = 40 || { echo "FATAL: STELLA_SOURCE_COMMIT is not a full SHA"; return 1; }
  test "$(command -v harbor)" = "$VENV/bin/harbor" || { echo "FATAL: wrong harbor on PATH"; return 1; }
  test "$(harbor --version)" = "0.6.1" || { echo "FATAL: harbor $(harbor --version) != 0.6.1 (audited constant)"; return 1; }
  docker info >/dev/null 2>&1 || { echo "FATAL: docker unreachable"; return 1; }
}
