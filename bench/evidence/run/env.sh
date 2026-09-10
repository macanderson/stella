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

# Which arm of a two-build experiment this shell is, or empty for a single-build
# run. Only the binary may differ between two arms, so `TB_REPO` stays pointed at
# one checkout and supplies the adapter, the venv and the run scripts to both.
# The three lines above are why: `PYTHONPATH` follows `TB_REPO`, so an arm that
# moved it to fetch an old binary would swap in that checkout's adapter as well,
# and the arms would then differ by the measuring instrument too. Point
# `TB_BUILD_REPO` at the old checkout instead — `build_sut.sh` reads it, and
# nothing else does.
#
# Unset, every path below is what it was before arms existed, so a single-build
# run reads and writes the same files it always did.
export TB_ARM="${TB_ARM:-}"
case "$TB_ARM" in
  "") export TB_ARM_DIR="$TB_ROOT" ;;
  *[!a-z0-9-]*)
    echo "FATAL: TB_ARM must be lowercase letters, digits and hyphens; got '$TB_ARM'"
    exit 1 ;;
  *) export TB_ARM_DIR="$TB_ROOT/arms/$TB_ARM"; mkdir -p "$TB_ARM_DIR" ;;
esac

# The SUT is cross-compiled against an old glibc so it runs in every task image,
# including the two on `debian:bullseye-slim` (glibc 2.31). Both halves of that
# contract are pinned here — `build_sut.sh` builds to this floor and `preflight`
# refuses to start a run with a binary that exceeds it — because keeping them
# apart is how a glibc-2.35 binary reached five trials on 2026-07-31 wearing the
# portable build's filename. Constants, not overridable defaults: an operator
# able to raise the floor to silence the error is the failure being prevented.
export STELLA_TARGET_TRIPLE="x86_64-unknown-linux-gnu"
export STELLA_GLIBC_FLOOR="2.17"
# Where `cargo zigbuild` drops the artifact, in whichever checkout built it.
#
# `TB_` and not `STELLA_`: every script here sources this file before invoking
# Harbor, so anything exported is ambient in the adapter's process, and the
# adapter's ambient check refuses the run on any unregistered `STELLA_*` name.
# A build path means nothing inside a task container and has no business in
# that namespace.
export TB_BUILD_OUTPUT="${TB_BUILD_REPO:-$TB_REPO}/target/$STELLA_TARGET_TRIPLE/release/stella"
# Where the run reads it. An arm gets its own copy, so building the second arm
# cannot overwrite the first and leave the report citing the survivor's hash.
if [ -n "$TB_ARM" ]; then
  export STELLA_BINARY="$TB_ARM_DIR/stella"
else
  export STELLA_BINARY="$TB_BUILD_OUTPUT"
fi
# `-` and not `:-`: unset still takes the development default, but an
# explicitly empty value stays empty and means *no per-trial cap*, which a
# head-to-head against a comparator with no spend ceiling has to be able to
# say. With `:-` that posture is unreachable — the empty string is substituted
# away here, and every run silently carries a cap the other side does not.
export STELLA_SPEND_LIMIT="${STELLA_SPEND_LIMIT-0.60}"
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

[ -f "$TB_ARM_DIR/sut_commit.txt" ] && export STELLA_SOURCE_COMMIT="$(cat "$TB_ARM_DIR/sut_commit.txt")"

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

# Assert the SUT binary is close enough to `origin/main` to be reported as it.
#
# `assert_portable_binary` answers "can this run"; the SHA-256 comparison
# answers "did it arrive intact"; `STELLA_SOURCE_COMMIT` answers "which commit
# is it". None of them answers "is that commit anywhere near the one a reader
# will assume was measured" — and on 2026-08-07 the path this file exports as
# STELLA_BINARY was 291 commits behind main, accompanied by its own
# sut_commit.txt, so it looked pinned and passed everything. Pinning and
# freshness are different properties; this checks the second.
#
# The limit deliberately lives in the checker and is not restated here: one
# copy cannot drift from the other. Pass --max-behind to measure older code on
# purpose, which puts the distance in the launch command rather than in a
# default nobody reads.
# shellcheck disable=SC2120  # build_sut.sh calls this with arguments; shellcheck
# cannot see across the `source` boundary and so reads it as never-parameterised.
assert_fresh_sut() {
  local binary="${1:-$STELLA_BINARY}"
  [ "$#" -gt 0 ] && shift
  local checker="$TB_REPO/bench/harbor_adapter/stella_harbor/freshness.py"
  command -v python3 >/dev/null 2>&1 || {
    echo "FATAL: python3 is required to verify SUT binary freshness"; return 1; }
  test -f "$checker" || { echo "FATAL: freshness checker missing at $checker"; return 1; }
  python3 "$checker" "$binary" --repo "$TB_REPO" ${1+"$@"} || {
    echo "FATAL: $binary is not the code this run would be reported as measuring."
    echo "       Rebuild with bench/evidence/run/build_sut.sh. Repointing the"
    echo "       path or refreshing sut_commit.txt does not make the binary newer."
    return 1
  }
}

# Assert an arm's binary is the commit that arm was declared on.
#
# `assert_fresh_sut` with no reference asks "is this near origin/main", which is
# the right question for a single-build run and the wrong one for an arm of a
# two-build experiment. The control arm of the pipeline A/B is a thousand
# commits behind main on purpose, so the default refuses it — and raising the
# tolerance far enough to admit it would admit a stale binary on every other
# run too.
#
# So an arm is held to equality with the commit `build_sut.sh` recorded for it
# instead. That is stricter than the default, not looser: it fails on a binary
# one commit off, which the default would wave through, and it is the property
# the analysis checks anyway (`compare_arms.py --cross-sut` refuses an arm whose
# trials report a commit other than the one declared for it).
assert_arm_is_its_declared_commit() {
  # `:-` and not a bare expansion: `set -u` turns an unset variable into a bash
  # error that kills the shell before the message below can print, and an arm
  # with no `sut_commit.txt` is exactly the case this is here to name.
  local commit="${STELLA_SOURCE_COMMIT:-}"
  test "${#commit}" = 40 || {
    echo "FATAL: no recorded commit for arm '$TB_ARM' — run build_sut.sh for it first"
    return 1
  }
  assert_fresh_sut "$STELLA_BINARY" --reference "$commit" --max-behind 0
}

preflight() {
  test -n "${OPENROUTER_API_KEY:-}" || { echo "FATAL: OPENROUTER_API_KEY unset"; return 1; }
  test -x "$STELLA_BINARY" || { echo "FATAL: no SUT binary at $STELLA_BINARY (run build_sut.sh)"; return 1; }
  assert_portable_binary || return 1
  if [ -n "$TB_ARM" ]; then
    assert_arm_is_its_declared_commit || return 1
  else
    assert_fresh_sut || return 1
  fi
  local commit="${STELLA_SOURCE_COMMIT:-}"
  test "${#commit}" = 40 || { echo "FATAL: STELLA_SOURCE_COMMIT is not a full SHA"; return 1; }
  test "$(command -v harbor)" = "$VENV/bin/harbor" || { echo "FATAL: wrong harbor on PATH"; return 1; }
  test "$(harbor --version)" = "0.6.1" || { echo "FATAL: harbor $(harbor --version) != 0.6.1 (audited constant)"; return 1; }
  docker info >/dev/null 2>&1 || { echo "FATAL: docker unreachable"; return 1; }
}
