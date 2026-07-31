#!/bin/bash
# Provision a fresh Ubuntu 22.04+ x86_64 VM and run the whole development
# baseline unattended. Paste-and-forget: it installs the toolchain, builds the
# SUT, pulls every task image, gates on the sentinel, runs both phases, and
# writes the evidence.
#
# Run it as a user with docker access, on a host with >=8 vCPU, >=32 GiB RAM and
# >=250 GiB free. NOTHING here is Stella-specific tuning — the run parameters all
# come from bench/evidence/run/env.sh so the cloud run and a local one are the
# same measurement.
#
#   export OPENROUTER_API_KEY=sk-or-...
#   export TB_PREREG_URL=https://github.com/macanderson/stella/issues/NNN
#   bash cloud_bootstrap.sh 2>&1 | tee ~/tb-run.log
#
# Why a cloud host at all, beyond convenience: on a consumer link one task's
# verifier could not finish downloading 2.8 GiB of CUDA wheels inside its 1800s
# ceiling, so it returned no verdict and scored 0 for reasons that had nothing
# to do with the agent. Native amd64 also removes Rosetta from the measurement.
set -euo pipefail

: "${OPENROUTER_API_KEY:?export OPENROUTER_API_KEY before running}"
REPO_URL="${REPO_URL:-https://github.com/macanderson/stella.git}"
export TB_ROOT="${TB_ROOT:-$HOME/tb21}"
export TB_REPO="${TB_REPO:-$HOME/stella}"
RUN_ID="${RUN_ID:-tb21-dev-baseline-$(date -u +%Y%m%d-%H%M%S)}"
PREFIX="${PREFIX:-$RUN_ID}"

echo "=== 0. host sanity ==="
arch="$(uname -m)"
[ "$arch" = "x86_64" ] || { echo "FATAL: need a native x86_64 host, got $arch"; exit 1; }
nproc
awk '/MemTotal/{printf "%.1f GiB RAM\n", $2/1024/1024}' /proc/meminfo
df -h --output=avail / | tail -1

echo "=== 1. packages ==="
sudo apt-get update -qq
sudo apt-get install -y -qq git curl build-essential pkg-config libssl-dev jq
command -v docker >/dev/null || { curl -fsSL https://get.docker.com | sudo sh; sudo usermod -aG docker "$USER"; }
docker info >/dev/null 2>&1 || { echo "FATAL: docker not usable by $USER — re-login for the group to apply"; exit 1; }

echo "=== 2. toolchain ==="
command -v uv >/dev/null || curl -LsSf https://astral.sh/uv/install.sh | sh
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"
command -v rustup >/dev/null || curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
# Native x86_64 host: no cross-compilation, so no zig and no cargo-zigbuild.

echo "=== 3. source ==="
[ -d "$TB_REPO" ] || git clone --quiet "$REPO_URL" "$TB_REPO"
cd "$TB_REPO"
git fetch --quiet origin main
git checkout --quiet -B bench-run origin/main
SUT="$(git rev-parse HEAD)"
echo "SUT=$SUT ($(git describe --tags 2>/dev/null || echo no-tag))"

echo "=== 4. adapter venv (Harbor 0.6.1 is an audited constant — see the pin comment) ==="
uv sync --project "$TB_REPO/bench/harbor_adapter" --locked --extra dev
"$TB_REPO/bench/harbor_adapter/.venv/bin/harbor" --version

echo "=== 5. build the SUT natively ==="
mkdir -p "$TB_ROOT"
echo "$SUT" > "$TB_ROOT/sut_commit.txt"
STELLA_BUILD_GIT_SHA="$SUT" cargo build --release --locked -p stella-cli --bin stella
# env.sh expects the cross-build path; on a native host the release binary is
# already the right ELF, so publish it under the same name rather than teaching
# every script two layouts.
mkdir -p "$TB_REPO/target/x86_64-unknown-linux-gnu/release"
cp "$TB_REPO/target/release/stella" "$TB_REPO/target/x86_64-unknown-linux-gnu/release/stella"
sha256sum "$TB_REPO/target/x86_64-unknown-linux-gnu/release/stella" | awk '{print $1}' > "$TB_ROOT/binary_sha256.txt"
echo "binary_sha256=$(cat "$TB_ROOT/binary_sha256.txt")"
"$TB_REPO/target/x86_64-unknown-linux-gnu/release/stella" --version

echo "=== 6. dataset + task classification ==="
bash "$TB_REPO/bench/evidence/run/fetch_dataset.sh"

echo "=== 7. pre-pull every task image (a failed pull would score as a task failure) ==="
bash "$TB_REPO/bench/evidence/run/prepull.sh"
failed=$(grep -c . "$TB_ROOT/prepull_failed.txt" 2>/dev/null || true); failed=${failed:-0}
[ "$failed" -eq 0 ] || { echo "FATAL: $failed image(s) failed to pull"; cat "$TB_ROOT/prepull_failed.txt"; exit 1; }

echo "=== 8. readiness sentinel — must return reward 1.0 ==="
bash "$TB_REPO/bench/evidence/run/sentinel.sh" "sentinel-$RUN_ID" || {
  echo "FATAL: sentinel did not return reward 1.0; refusing to spend 89 trials on a broken path"; exit 1; }

echo "=== 9. the measured run ==="
# Phase B first: the memory-hungry tasks, alone. On a >=32 GiB host both phases
# could run at higher concurrency, but the split is kept so the cloud number and
# the local protocol describe the same procedure.
bash "$TB_REPO/bench/evidence/run/primary.sh" B "${PREFIX}-phaseB" || echo "phase B exited $?"
bash "$TB_REPO/bench/evidence/run/primary.sh" A "${PREFIX}-phaseA" || echo "phase A exited $?"

echo "=== 10. evidence ==="
TB_TASKS=89 bash "$TB_REPO/bench/evidence/run/finalize.sh" "$RUN_ID" "$PREFIX"
echo
echo "DONE. Evidence in $TB_REPO/bench/evidence/$RUN_ID"
echo "Copy it back with:  scp -r <host>:$TB_REPO/bench/evidence/$RUN_ID ."
