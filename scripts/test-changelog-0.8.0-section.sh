#!/usr/bin/env bash
#
# Pins that CHANGELOG.md carries a non-empty `## [0.8.0]` section, per the
# rule in scripts/changelog-roll.sh: "never emit a heading with nothing under
# it". Run directly: ./scripts/test-changelog-0.8.0-section.sh
#
# Fails today (no such section yet); must pass once the 0.8.0 release section
# is written.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
changelog="$repo_root/CHANGELOG.md"

if [ ! -f "$changelog" ]; then
  echo "FAIL: $changelog not found"
  exit 1
fi

if ! grep -q '^## \[0.8.0\]' "$changelog"; then
  echo "FAIL: no '## [0.8.0]' heading in CHANGELOG.md"
  exit 1
fi

# Body = everything between the [0.8.0] heading and the next '## ' heading.
body="$(awk '/^## \[0.8.0\]/{found=1; next} /^## /{if(found) exit} found{print}' "$changelog")"

if [ -z "$(echo "$body" | tr -d '[:space:]')" ]; then
  echo "FAIL: '## [0.8.0]' heading has an empty body"
  exit 1
fi

if ! echo "$body" | grep -q '^### '; then
  echo "FAIL: '## [0.8.0]' body has no '### ' subsection"
  exit 1
fi

if ! echo "$body" | grep -q '^- '; then
  echo "FAIL: '## [0.8.0]' body has no bullet entries"
  exit 1
fi

echo "ok: CHANGELOG.md has a non-empty [0.8.0] section"
exit 0
