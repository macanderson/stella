#!/usr/bin/env bash
#
# demo-scenario.sh — a many-turn, multi-hour build-and-verify marathon over
# the whole workspace, written to be *recorded* (see scripts/record-demo.sh).
#
#   Usage:  scripts/demo-scenario.sh [--loops N] [--no-clean]
#
#     --loops N     number of full verify cycles (default: 3)
#     --no-clean    keep build caches between cycles (much faster; use for
#                   short takes — cold cycles are what make this take hours)
#
# Each cycle is a full cold rebuild + verification of the entire workspace:
# clean, debug build, clippy, release build, smoke run, full test suite,
# rustdoc. One cold cycle takes on the order of an hour on a modest machine,
# so the default three cycles run for hours — and every stage prints a
# numbered "turn" banner with a timestamp and elapsed clock, which is what
# makes the rendered timelapse legible instead of a wall of cargo output.
#
# There is deliberately no time limit here: cap wall-clock time from the
# recorder (`record-demo.sh --limit <minutes>`), which stops this script
# cleanly at the cutoff and still renders everything captured so far.

set -euo pipefail

LOOPS=3
CLEAN=1
while [ $# -gt 0 ]; do
  case "$1" in
    --loops)    LOOPS="$2"; shift 2 ;;
    --no-clean) CLEAN=0; shift ;;
    -h|--help)  sed -n '2,22p' "$0" | sed 's/^#//; s/^ //'; exit 0 ;;
    *)          echo "unknown option: $1" >&2; exit 1 ;;
  esac
done

cd "$(dirname "$0")/.."

START=$SECONDS
TURN=0

elapsed() {
  local s=$((SECONDS - START))
  printf '%dh%02dm%02ds' $((s / 3600)) $((s % 3600 / 60)) $((s % 60))
}

turn() {
  TURN=$((TURN + 1))
  printf '\n\033[1;35m━━━ turn %d · %s · elapsed %s ━━━\033[0m\n' \
    "$TURN" "$(date '+%H:%M:%S')" "$(elapsed)"
  printf '\033[1m» %s\033[0m\n' "$*"
}

# A recorder --limit cutoff arrives as SIGINT; leave with a readable coda
# instead of a bare ^C so the end of the timelapse still tells the story.
FINISHED=0
finish() {
  [ "$FINISHED" -eq 1 ] && return
  FINISHED=1
  printf '\n\033[1;35m━━━ stopped · %d turns · elapsed %s ━━━\033[0m\n' \
    "$TURN" "$(elapsed)"
}
trap finish EXIT
trap 'finish; trap - EXIT; exit 130' INT TERM

turn "toolchain — rustup resolves the pinned compiler (rust-toolchain.toml)"
rustc --version
cargo --version

turn "cargo fetch — download every workspace dependency"
cargo fetch

for ((cycle = 1; cycle <= LOOPS; cycle++)); do
  if [ "$CLEAN" -eq 1 ] && [ "$cycle" -gt 1 ]; then
    turn "cycle $cycle/$LOOPS — cargo clean (cold rebuild from nothing)"
    cargo clean
  fi

  turn "cycle $cycle/$LOOPS — debug build of the full workspace"
  cargo build --workspace

  turn "cycle $cycle/$LOOPS — clippy across every target (-D warnings)"
  cargo clippy --workspace --all-targets -- -D warnings

  turn "cycle $cycle/$LOOPS — optimized release build of the stella binary"
  cargo build --release -p stella-cli

  turn "cycle $cycle/$LOOPS — smoke: run the freshly built binary"
  # CARGO_TARGET_DIR-aware: a sourced .dev-env (scripts/setup-dev-env.sh)
  # relocates build output out of ./target.
  "${CARGO_TARGET_DIR:-target}/release/stella" models

  turn "cycle $cycle/$LOOPS — full test suite, every crate"
  cargo test --workspace

  turn "cycle $cycle/$LOOPS — rustdoc, workspace-wide (-D warnings)"
  RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
done

turn "marathon complete — $LOOPS full verify cycles"
