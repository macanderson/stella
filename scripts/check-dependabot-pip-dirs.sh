#!/usr/bin/env bash
#
# Guard: every pip `directory:` in .github/dependabot.yml must hold a real
# Python manifest.
#
# Dependabot's pip updater does not look inside subfolders. A `directory:`
# line finds files only in that exact folder. `pip in /bench` named a
# folder with no manifest in it at all. Every weekly run failed fast, with
# "No files found in /bench". Two real manifests sat close by and were
# never checked. bench/harbor_adapter/pyproject.toml had its own correct
# entry. bench/terminal_bench_analysis/pyproject.toml had none. Nothing
# caught this. The failure happens on Dependabot's own servers. This
# repo's CI never sees it.
#
# This guard is a small, mechanical check. It cannot tell a wrong folder
# from a stale one left behind by a rename. It can catch a folder with
# nothing in it, which is the exact shape of the bug above.
#
# Not a full YAML parser. dependabot.yml's shape is simple: a flat list of
# entries, each indented the same way. A line-by-line scan is enough, and
# it avoids adding a YAML library dependency for one script.
# scripts/test-dependabot-pip-dirs.sh checks that this scan still matches
# the real file's shape.

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
# A test seam. The test suite points this at a fixture folder instead of
# the real repo (same pattern as scripts/check-action-pins.sh). The
# fixture must carry its own .github/dependabot.yml. That way directory:
# paths resolve inside the fixture, not inside the real bench/ tree.
if [ "${1:-}" = "--fixture-root" ]; then
  repo_root="$2"
fi

config="$repo_root/.github/dependabot.yml"
if [ ! -f "$config" ]; then
  echo "check-dependabot-pip-dirs: no such file: $config" >&2
  exit 1
fi

config_dir="$repo_root"

manifest_present() {
  # $1 = a directory: value from dependabot.yml, such as "/bench". These
  # are always relative to the repository root, not to this script.
  local dir="$config_dir${1%/}"
  [ -f "$dir/pyproject.toml" ] && return 0
  [ -f "$dir/setup.py" ] && return 0
  [ -f "$dir/setup.cfg" ] && return 0
  [ -f "$dir/Pipfile" ] && return 0
  compgen -G "$dir/requirements*.txt" >/dev/null 2>&1 && return 0
  return 1
}

fail=0
ecosystem=""
directory=""

check_pending() {
  if [ "$ecosystem" = "pip" ] && [ -n "$directory" ]; then
    if ! manifest_present "$directory"; then
      echo "check-dependabot-pip-dirs: pip entry 'directory: $directory' has no" \
        "pyproject.toml/setup.py/setup.cfg/Pipfile/requirements*.txt in it" >&2
      fail=1
    fi
  fi
}

while IFS= read -r line; do
  case "$line" in
    '  - package-ecosystem:'*)
      # A new update entry starts: flush the previous one, then reset.
      check_pending
      ecosystem="$(printf '%s' "$line" | sed -E 's/^[^:]+:[[:space:]]*//')"
      directory=""
      ;;
    '    directory:'*)
      directory="$(printf '%s' "$line" | sed -E 's/^[^:]+:[[:space:]]*//')"
      ;;
  esac
done < "$config"
check_pending

if [ "$fail" -ne 0 ]; then
  echo "check-dependabot-pip-dirs: FAILED" >&2
  exit 1
fi

echo "check-dependabot-pip-dirs: OK — every pip directory: has a manifest"
