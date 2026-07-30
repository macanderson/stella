#!/bin/bash
# Build the frozen SUT binary and prove its provenance.
#
# The SUT is `origin/main` itself. The working tree may carry bench/ or docs
# changes, which compile into nothing, so the check that matters is not "tree is
# clean" but "every input to the Rust build is byte-identical to origin/main".
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"
cd "$TB_REPO"

git fetch origin main -q
SUT="$(git rev-parse origin/main)"
drift="$(git diff --name-only "$SUT" -- '*.rs' '*/Cargo.toml' Cargo.toml Cargo.lock rust-toolchain.toml)"
if [ -n "$drift" ]; then
  echo "FATAL: build inputs differ from $SUT:"; echo "$drift"; exit 1
fi
echo "$SUT" > "$TB_ROOT/sut_commit.txt"
echo "SUT=$SUT ($(git describe --tags "$SUT" 2>/dev/null || echo no-tag))"

ZC="$(mktemp -d "${TMPDIR:-/tmp}/stella-zig.XXXXXX")"
mkdir -p "$ZC/global" "$ZC/local"
rustup target add x86_64-unknown-linux-gnu >/dev/null
RUSTC="$(rustup which rustc)" \
ZIG_GLOBAL_CACHE_DIR="$ZC/global" ZIG_LOCAL_CACHE_DIR="$ZC/local" \
STELLA_BUILD_GIT_SHA="$SUT" \
"$(rustup which cargo)" zigbuild --release --locked \
  --target x86_64-unknown-linux-gnu.2.17 --package stella-cli --bin stella

test -x "$STELLA_BINARY"
file "$STELLA_BINARY"
shasum -a 256 "$STELLA_BINARY" | awk '{print $1}' > "$TB_ROOT/binary_sha256.txt"
echo "binary_sha256=$(cat "$TB_ROOT/binary_sha256.txt")"
