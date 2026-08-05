#!/usr/bin/env bash
#
# Guard: nothing outside docs/design/ cites docs/design/.
#
# docs/design/ is the scratchpad for work in flight. Anything in it may be
# rewritten, renamed or deleted at any moment, so a code comment that cites it
# is a reference with no owner: the design lands, the file moves, and the
# comment now points at nothing while still reading as authoritative.
#
# The durable home is docs/spec/. A document that code depends on belongs there,
# and moving it there is the fix for a citation this guard rejects -- not adding
# an entry to ALLOW below.
#
# ALLOW is for the tooling that manages docs/design itself, which necessarily
# names the path.

set -euo pipefail

cd "$(dirname "$0")/.."

ALLOW=(
  "scripts/check-design-refs.sh"   # this guard
  "scripts/check-doc-links.py"     # link checker: resolves links in both trees
  "Makefile"                       # doc-adopt promotes a doc OUT of docs/design
)

is_allowed() {
  local f="$1"
  for a in "${ALLOW[@]}"; do
    [ "$f" = "$a" ] && return 0
  done
  return 1
}

# Tracked files only, and never docs/ itself -- a design doc may cite a sibling.
# Written as a while-read rather than mapfile: macOS ships bash 3.2.
violations=()
while IFS= read -r f; do
  [ -n "$f" ] || continue
  is_allowed "$f" && continue
  violations+=("$f")
done <<EOF
$(git grep -l -F 'docs/design/' -- . ':!docs/' 2>/dev/null || true)
EOF

if [ ${#violations[@]} -gt 0 ]; then
  echo "check-design-refs: docs/design/ is work-in-flight and must not be cited."
  echo
  for f in "${violations[@]}"; do
    git grep -n -F 'docs/design/' -- "$f" | sed 's/^/  /'
  done
  echo
  echo "Fix by promoting the document to docs/spec/ and citing it there:"
  echo "  git mv docs/design/<doc>.md docs/spec/<doc>.md"
  echo "  then repoint the citation at docs/spec/<doc>.md"
  exit 1
fi

echo "check-design-refs: no code cites docs/design ✔"
