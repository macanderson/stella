#!/bin/bash
# Create this lane's virtualenv, pinned to a Harbor that understands the
# Frontier-Bench task schema.
#
# Kept separate from bench/harbor_adapter/.venv on purpose. That one is pinned
# to harbor==0.6.1, the Terminal-Bench claim's audited constant, and upgrading
# it in place would silently change what every Terminal-Bench rerun measures.
# Two benchmarks, two pins, one adapter.
set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/env.sh"

command -v uv >/dev/null 2>&1 || { echo "FATAL: uv is required"; exit 1; }

# --clear so this is idempotent. Without it the script fails the moment a venv
# exists, which is precisely when you most want to run it: after changing the
# Harbor pin or adding the modal extra. The venv is a derived, gitignored
# artifact rebuilt from two pinned inputs, so replacing it loses nothing.
uv venv "$VENV" --python 3.13 --clear || exit 1
# The `modal` extra unconditionally, not only when FB_ENV=modal. Harbor loads
# vendor SDKs lazily, so a docker-only run never imports it — but installing it
# up front means switching backends is one variable rather than a rebuild, and
# the preflight can then tell "modal is not authenticated" apart from "the SDK
# was never installed". Two different problems deserve two different messages.
uv pip install --python "$VENV/bin/python" "harbor[modal]==$FB_HARBOR_VERSION" || exit 1

got="$("$VENV/bin/harbor" --version)"
test "$got" = "$FB_HARBOR_VERSION" || {
  echo "FATAL: installed harbor $got != $FB_HARBOR_VERSION"; exit 1; }

# The adapter is imported from the repo (PYTHONPATH), not installed, so this is
# the moment to prove it still satisfies the newer Harbor's agent interface
# rather than discovering it one trial into a paid run.
PYTHONPATH="$TB_REPO/bench/harbor_adapter" "$VENV/bin/python" - <<'PY' || exit 1
from stella_harbor import StellaAgent
missing = sorted(getattr(StellaAgent, "__abstractmethods__", ()))
assert not missing, f"StellaAgent does not implement: {missing}"
assert StellaAgent.name() == "stella"
print(f"adapter OK against this Harbor: {StellaAgent.name()} (ATIF={StellaAgent.SUPPORTS_ATIF})")
PY

echo "venv ready: $VENV (harbor $got)"
