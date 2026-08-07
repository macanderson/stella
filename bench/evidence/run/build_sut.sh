#!/bin/bash
# Build the frozen SUT binary and prove its provenance.
#
#   build_sut.sh [<commit>]
#
# The SUT is `origin/main` itself. The working tree may carry bench/ or docs
# changes, which compile into nothing, so the check that matters is not "tree is
# clean" but "every input to the Rust build is byte-identical to origin/main".
#
# Pass a commit to skip the fetch and build exactly that revision. A wrapper
# that fetches and checks out should pass what it checked out, because otherwise
# this script re-fetches and can resolve a *different* origin/main than the one
# the caller prepared against — a race whose symptom is a drift list of files
# nobody touched. When no commit is given the race is detected rather than
# guessed at: origin/main is recorded before and after the fetch, and a refusal
# says which of the two it is.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"
cd "$TB_REPO"

RUST_INPUTS=('*.rs' '*/Cargo.toml' Cargo.toml Cargo.lock rust-toolchain.toml)

if [ "$#" -gt 0 ]; then
  SUT="$(git rev-parse --verify "$1^{commit}")"
  BEFORE="$SUT"
else
  # `|| true`: a clone with no origin/main yet must reach the fetch, not die on
  # the probe that exists only to describe where the fetch started.
  BEFORE="$(git rev-parse --verify --quiet origin/main || true)"
  git fetch origin main -q
  SUT="$(git rev-parse origin/main)"
fi

drift="$(git diff --name-only "$SUT" -- "${RUST_INPUTS[@]}")"
if [ -n "$drift" ]; then
  # Separate the two causes before blaming the operator. If the tree is
  # byte-identical to where origin/main stood when this script started, the
  # drift is entirely the commits the fetch just brought in — the tree is
  # clean, and hunting it for local edits finds nothing because there are none.
  raced=""
  if [ -n "$BEFORE" ] && [ "$BEFORE" != "$SUT" ]; then
    if [ -z "$(git diff --name-only "$BEFORE" -- "${RUST_INPUTS[@]}")" ]; then
      raced="yes"
    fi
  fi
  if [ -n "$raced" ]; then
    moved="$(git rev-list --count "$BEFORE..$SUT")"
    echo "FATAL: origin/main moved while this script ran — this is NOT local contamination."
    echo "       Your tree is byte-identical to $BEFORE, where origin/main stood"
    echo "       when the build started. The fetch advanced it $moved commit(s) to $SUT,"
    echo "       and the files below are that difference, not edits of yours:"
    echo "$drift"
    echo "       Update the tree and re-run, or build the revision you prepared against:"
    echo "         bench/evidence/run/build_sut.sh $BEFORE"
    exit 1
  fi
  echo "FATAL: build inputs differ from $SUT:"; echo "$drift"; exit 1
fi
echo "$SUT" > "$TB_ROOT/sut_commit.txt"
echo "SUT=$SUT ($(git describe --tags "$SUT" 2>/dev/null || echo no-tag))"

ZC="$(mktemp -d "${TMPDIR:-/tmp}/stella-zig.XXXXXX")"
mkdir -p "$ZC/global" "$ZC/local"
rustup target add "$STELLA_TARGET_TRIPLE" >/dev/null
RUSTC="$(rustup which rustc)" \
ZIG_GLOBAL_CACHE_DIR="$ZC/global" ZIG_LOCAL_CACHE_DIR="$ZC/local" \
STELLA_BUILD_GIT_SHA="$SUT" \
"$(rustup which cargo)" zigbuild --release --locked \
  --target "$STELLA_TARGET_TRIPLE.$STELLA_GLIBC_FLOOR" --package stella-cli --bin stella

test -x "$STELLA_BINARY"
# Verify the artifact, not that the right command was typed. Building through
# this script and building through anything else are then distinguishable by
# inspecting the output, which is the only evidence a later run actually has.
assert_portable_binary
# And that the artifact carries the commit we asked for. `--max-behind 0`
# against $SUT itself is an equality assertion on the binary's own compile-time
# stamp, so a build that silently stamped something else — a stale target dir,
# an env var the caller already had set — is caught here rather than becoming
# the next thing that measures the wrong code.
assert_fresh_sut "$STELLA_BINARY" --reference "$SUT" --max-behind 0
file "$STELLA_BINARY"
shasum -a 256 "$STELLA_BINARY" | awk '{print $1}' > "$TB_ROOT/binary_sha256.txt"
echo "binary_sha256=$(cat "$TB_ROOT/binary_sha256.txt")"
