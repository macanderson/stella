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
#
# BOTH citation spellings are checked (#1665). Since #1440 a document can be
# cited by its stable id -- `doc:pipeline-journey` -- as well as by path, and an
# id citation names no path, so a path-only grep sails straight past the thing
# this guard exists to prevent. The id half resolves ids through
# docs/manifest.json, which is generated and committed, and reuses
# check-doc-links.py's citation form as the authority on what a citation is.
#
# python3 (not jq) is the dependency: this runs in GATE_GUARDS_FAST and ci.yml
# starts it before the Rust toolchain is installed, and a bare runner is not
# guaranteed to have jq. check-doc-links.py already relies on python3.

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

# The id-form half. Same exemptions, same exclusion of docs/ -- ALLOW is passed
# in rather than restated so the two spellings can never drift apart.
id_violations=$(python3 - "${ALLOW[@]}" <<'PY' || true
import json, re, subprocess, sys

allow = set(sys.argv[1:])

# The authority on what a citation looks like is check-doc-links.py's
# CITE_ID_RE; kept in the same shape here deliberately.
CITE_ID_RE = re.compile(r"\bdoc:([a-z0-9][a-z0-9/-]*[a-z0-9])")

try:
    with open("docs/manifest.json", encoding="utf-8") as fh:
        documents = json.load(fh).get("documents", {})
except (OSError, ValueError):
    # A missing or unparseable manifest is check-doc-links.py's failure to
    # report, not this guard's; the path-form half above still stands.
    sys.exit(0)

design = {
    doc_id
    for doc_id, meta in documents.items()
    if str(meta.get("path", "")).startswith("docs/design/")
}
if not design:
    sys.exit(0)

# Narrow with git grep before reading anything: only files that contain the
# literal "doc:" can possibly carry an id citation.
found = subprocess.run(
    ["git", "grep", "-n", "-I", "-F", "doc:", "--", ".", ":!docs/"],
    capture_output=True,
    text=True,
)
for line in found.stdout.splitlines():
    parts = line.split(":", 2)
    if len(parts) != 3:
        continue
    path, lineno, text = parts
    if path in allow:
        continue
    for match in CITE_ID_RE.finditer(text):
        if match.group(1) in design:
            print(f"  {path}:{lineno}:{text.strip()}")
            break
PY
)

if [ ${#violations[@]} -gt 0 ] || [ -n "$id_violations" ]; then
  echo "check-design-refs: docs/design/ is work-in-flight and must not be cited."
  echo
  # Guarded: `set -u` makes expanding an empty array an error on bash 3.2,
  # and either half can be the empty one now that there are two.
  if [ ${#violations[@]} -gt 0 ]; then
    for f in "${violations[@]}"; do
      git grep -n -F 'docs/design/' -- "$f" | sed 's/^/  /'
    done
  fi
  [ -n "$id_violations" ] && printf '%s\n' "$id_violations"
  echo
  echo "Fix by promoting the document to docs/spec/ and citing it there:"
  echo "  git mv docs/design/<doc>.md docs/spec/<doc>.md"
  echo "  make doc-links-fix   # repoints path citations after the move"
  echo
  echo "An id citation (doc:<id>) is rejected for the same reason as a path"
  echo "citation: promoting the document is the fix, and the id survives it."
  exit 1
fi

# The verdict is already decided; the write is best-effort. SIGPIPE is ignored
# and the write's failure discarded, so a reader that closed the pipe
# (`| head -1`, `| true`) cannot turn a green verdict into a failure (#1815).
trap '' PIPE
echo "check-design-refs: no code cites docs/design, by path or by id ✔" || true
