#!/usr/bin/env bash
#
# Tests for scripts/check-dependabot-pip-dirs.sh.
#
#   ./scripts/test-dependabot-pip-dirs.sh
#
# Not part of `make gate`. It builds throwaway fixture folders, the same
# way scripts/test-no-scratch.sh does.
#
# The witness: case B is the real bug this repo shipped. `pip in /bench`
# named a folder with no manifest in it, and every weekly run failed with
# "No files found in /bench". The guard fails on that fixture. It passes
# once the entry points at a folder that holds a pyproject.toml. Fail,
# then pass: that pair is what this repo calls a witness.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
guard="$repo_root/scripts/check-dependabot-pip-dirs.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

ok() { pass=$((pass + 1)); echo "ok   $1"; }
no() { fail=$((fail + 1)); echo "FAIL $1"; shift; [ $# -gt 0 ] && printf '%s\n' "$@"; }

# A fresh fixture tree with just a .github/dependabot.yml. $1 = fixture name,
# $2 = dependabot.yml body. Echoes the tree's root.
fixture() {
  local dir="$TMP/$1"
  mkdir -p "$dir/.github"
  printf '%s\n' "$2" > "$dir/.github/dependabot.yml"
  echo "$dir"
}

# A: no pip entries. Nothing to check. Must pass.
dir=$(fixture "a-no-pip" '
version: 2
updates:
  - package-ecosystem: cargo
    directory: /
    schedule:
      interval: weekly
')
if "$guard" --fixture-root "$dir" >/dev/null 2>&1; then
  ok "A: no pip entries passes"
else
  no "A: no pip entries should pass"
fi

# B: the real bug. directory: /bench, empty.
dir=$(fixture "b-empty-dir" '
version: 2
updates:
  - package-ecosystem: pip
    directory: /bench
    schedule:
      interval: weekly
')
if "$guard" --fixture-root "$dir" >/dev/null 2>&1; then
  no "B: empty pip directory should fail (the shipped defect's shape)"
else
  ok "B: empty pip directory fails"
fi

# C: same shape as B, but the folder holds a pyproject.toml.
dir=$(fixture "c-pyproject" '
version: 2
updates:
  - package-ecosystem: pip
    directory: /bench/terminal_bench_analysis
    schedule:
      interval: weekly
')
mkdir -p "$dir/bench/terminal_bench_analysis"
touch "$dir/bench/terminal_bench_analysis/pyproject.toml"
if "$guard" --fixture-root "$dir" >/dev/null 2>&1; then
  ok "C: pip directory with pyproject.toml passes"
else
  no "C: pip directory with pyproject.toml should pass"
fi

# D: a requirements.txt file counts too, not just pyproject.toml.
dir=$(fixture "d-requirements" '
version: 2
updates:
  - package-ecosystem: pip
    directory: /tools
    schedule:
      interval: weekly
')
mkdir -p "$dir/tools"
touch "$dir/tools/requirements-dev.txt"
if "$guard" --fixture-root "$dir" >/dev/null 2>&1; then
  ok "D: pip directory with requirements*.txt passes"
else
  no "D: pip directory with requirements*.txt should pass"
fi

# E: two pip entries. Only the second is broken. Both must be checked.
dir=$(fixture "e-second-broken" '
version: 2
updates:
  - package-ecosystem: pip
    directory: /good
    schedule:
      interval: weekly
  - package-ecosystem: pip
    directory: /bad
    schedule:
      interval: weekly
')
mkdir -p "$dir/good" "$dir/bad"
touch "$dir/good/setup.py"
if "$guard" --fixture-root "$dir" >/dev/null 2>&1; then
  no "E: second broken pip entry should still fail"
else
  ok "E: second broken pip entry fails"
fi

# F: this repo's own real config passes today.
if "$guard" >/dev/null 2>&1; then
  ok "F: this repository's real dependabot.yml passes"
else
  no "F: this repository's real dependabot.yml should pass"
fi

echo
echo "pass=$pass fail=$fail"
[ "$fail" -eq 0 ]
