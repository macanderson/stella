#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
#
# Prepare a host to run Terminal-Bench 2.1 as an ArenaBench contest.
#
# Everything here is idempotent and free: it fetches, builds and checks, but it
# never starts a paid run. Launching is done from the web UI, where the tasks
# and contestants are chosen.
#
#   ./scripts/setup-terminal-bench.sh              # prepare everything
#   ./scripts/setup-terminal-bench.sh --with-sut   # also cross-build Stella
#
set -uo pipefail

TB_DATASET="terminal-bench/terminal-bench-2-1@sha256:7d7bdc1cbedad549fc1140404bd4dc45e5fd0ea7c4186773687d177ad3a0699a"
WITH_SUT=0
[ "${1:-}" = "--with-sut" ] && WITH_SUT=1

ok=0
say()  { printf '\n\033[1m%s\033[0m\n' "$*"; }
pass() { printf '  \033[32m✓\033[0m %s\n' "$*"; }
warn() { printf '  \033[33m!\033[0m %s\n' "$*"; }
fail() { printf '  \033[31m✗\033[0m %s\n' "$*"; ok=1; }

say "1. Toolchain"

if command -v harbor >/dev/null 2>&1; then
  pass "harbor $(harbor --version 2>/dev/null || echo '?') at $(command -v harbor)"
else
  fail "harbor is not on PATH — ArenaBench cannot run a benchmark without it"
fi

if docker info >/dev/null 2>&1; then
  pass "docker reachable"
else
  fail "docker is not reachable (start Docker Desktop, or \`colima start\`)"
fi

python3 - <<'PY' || fail "python 3.11+ is required"
import sys
raise SystemExit(0 if sys.version_info >= (3, 11) else 1)
PY
[ $? -eq 0 ] && pass "python $(python3 -V 2>&1 | cut -d' ' -f2)"

# Task images publish linux/amd64 only. Without this a multi-arch base builds
# arm64 on Apple silicon and then cannot exec an x86_64 agent binary — which
# Harbor records as an agent crash rather than a platform mistake.
export DOCKER_DEFAULT_PLATFORM=linux/amd64
pass "DOCKER_DEFAULT_PLATFORM=linux/amd64"

say "2. Dataset"

tasks="$(python3 -c "
import sys
sys.path.insert(0, '$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)')
from arenabench.registry import DEFAULT_REGISTRY as R
print(len(R.tasks('terminal-bench-2.1')))
" 2>/dev/null || echo 0)"

if [ "$tasks" -ge 89 ]; then
  pass "terminal-bench 2.1 present — $tasks tasks"
else
  warn "dataset not on disk ($tasks tasks found); fetching…"
  if harbor download "$TB_DATASET"; then
    pass "dataset fetched"
  else
    fail "harbor download failed"
  fi
fi

say "3. Recorder (optional — real MP4 screen capture)"

if ! docker info >/dev/null 2>&1; then
  warn "skipped: docker unavailable"
elif docker image inspect arenabench/recorder:1 >/dev/null 2>&1; then
  pass "arenabench/recorder:1 already built"
else
  warn "building arenabench/recorder:1 (one-time, ~1 minute)…"
  if docker build -q -t arenabench/recorder:1 "$(dirname "${BASH_SOURCE[0]}")/../recorder" >/dev/null; then
    pass "recorder image built"
  else
    warn "recorder build failed — matches will run without video"
  fi
fi

say "4. Stella contestant (optional)"

if [ -n "${ARENABENCH_STELLA_ADAPTER:-}" ] && [ -d "$ARENABENCH_STELLA_ADAPTER" ]; then
  pass "adapter at $ARENABENCH_STELLA_ADAPTER"
else
  warn "ARENABENCH_STELLA_ADAPTER unset — the Stella seat will not launch"
  warn "  export ARENABENCH_STELLA_ADAPTER=/path/to/stella/bench/harbor_adapter"
fi

if [ "$WITH_SUT" = 1 ]; then
  repo="${STELLA_REPO:-${ARENABENCH_STELLA_ADAPTER:-}/../..}"
  if [ -f "$repo/Cargo.toml" ]; then
    warn "cross-building the Stella binary for linux/x86_64 (glibc 2.17)…"
    ( cd "$repo" \
      && rustup target add x86_64-unknown-linux-gnu >/dev/null 2>&1 \
      && cargo zigbuild --release --locked \
           --target x86_64-unknown-linux-gnu.2.17 \
           --package stella-cli --bin stella ) \
      && pass "built $repo/target/x86_64-unknown-linux-gnu/release/stella" \
      || fail "cross-build failed (needs cargo-zigbuild and zig)"
  else
    fail "--with-sut: no Cargo.toml at $repo (set STELLA_REPO)"
  fi
fi

if [ -n "${STELLA_BINARY:-}" ] && [ -x "${STELLA_BINARY:-}" ]; then
  arch="$(file -b "$STELLA_BINARY" 2>/dev/null | head -c 60)"
  case "$arch" in
    *x86-64*|*x86_64*) pass "STELLA_BINARY is x86_64: $STELLA_BINARY" ;;
    *) fail "STELLA_BINARY is not x86_64 ($arch) — it will fail to exec in a task container" ;;
  esac
else
  warn "STELLA_BINARY unset or not executable"
fi

say "Next"
cat <<'EOF'
  arenabench serve

  Then in the browser:
    1. pick Terminal-Bench 2.1
    2. check the tasks you want  (start with 3-5 easy ones)
    3. seat your contestants and paste each one's .env
    4. tick "Record MP4" if you built the recorder
    5. start the match

  Zero-cost dry run: seat "Oracle" against "No-op". They make no model calls,
  so the whole arena works end to end for free.
EOF

exit $ok
