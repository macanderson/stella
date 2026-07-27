#!/usr/bin/env bash
#
# Guard: the architectural invariants have exactly ONE normative home, and the
# numbering every citation depends on stays put. See #630.
#
# Two failures this catches, both of which had already happened:
#
#   1. **Duplication.** AGENTS.md, CONTRIBUTING.md and README.md each carried a
#      full copy. They had drifted: CONTRIBUTING.md was missing invariant #8
#      (provider feature parity) outright and carried a #3 shorn of the
#      paragraph explaining how it is enforced, while README.md silently
#      *renamed* two of them ("Typed errors, no panics" became "Fail loud,
#      recover gracefully"). Triplicated normative prose has no tiebreaker: a
#      reader obeys whichever copy they found first, and nothing tells them
#      they found the short one.
#
#   2. **Renumbering.** The list is cited BY NUMBER from Rust doc comments,
#      from runtime error strings in stella-store/src/content_free.rs, and from
#      four crate READMEs. Nothing checked those numbers, so inserting an entry
#      mid-list would have silently repointed every citation at the wrong rule
#      — invisibly, because a stale "#3" still renders as a plausible "#3".
#
# Uses portable POSIX grep/sed so it runs on a bare CI runner.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "check-invariants: not a git repository; skipping."
  exit 0
fi

home="AGENTS.md"
status=0

if [ ! -f "$home" ]; then
  echo "check-invariants: $home not found; skipping."
  exit 0
fi

# The normative form is a numbered list item opening with a bolded name:
#   `3. **Zero telemetry egress by default.** ...`
# Prose that merely mentions an invariant ("see the byte-stable prompts rule")
# does not match, and must not: pointing at a rule is the behaviour we want.
invariant_re='^[0-9]+\. \*\*[^*]+\*\*'

home_count="$(grep -cE "$invariant_re" "$home" || true)"

if [ "$home_count" -eq 0 ]; then
  echo "FAIL $home carries no numbered invariant list." >&2
  echo "     This guard expects the normative list to live here. If it moved," >&2
  echo "     update \$home in scripts/check-invariants.sh in the same PR." >&2
  exit 1
fi

# --- 1. Single home ---------------------------------------------------------
# Any OTHER tracked markdown file restating an invariant in normative form is a
# second copy. docs/llms.txt is generated from the website (`make llms-txt`),
# so a hit there is an artifact of its source, not an editable duplicate.
while IFS= read -r file; do
  [ -n "$file" ] || continue
  [ "$file" = "$home" ] && continue
  case "$file" in
    docs/llms.txt) continue ;;
  esac

  dupes="$(grep -nE "$invariant_re" "$file" || true)"
  [ -n "$dupes" ] || continue

  # A numbered+bolded list is not automatically an invariant restatement —
  # plenty of docs number bolded steps. Only fail when the bolded name matches
  # one the normative list actually defines.
  while IFS= read -r dupe; do
    [ -n "$dupe" ] || continue
    line="${dupe%%:*}"
    name="$(printf '%s\n' "$dupe" | sed -n 's/^[0-9]*:[0-9]*\. \*\*\([^*]*\)\*\*.*/\1/p')"
    [ -n "$name" ] || continue
    # Strip a trailing period so "Serde-first." matches "Serde-first".
    bare="$(printf '%s\n' "$name" | sed 's/\.$//')"
    if grep -qF "**$bare" "$home"; then
      echo "FAIL $file:$line restates invariant \"$bare\", which is normative in $home." >&2
      status=1
    fi
  done <<EOF2
$dupes
EOF2
done <<EOF
$(git ls-files '*.md')
EOF

if [ "$status" -ne 0 ]; then
  echo "" >&2
  echo "     The invariants have one home: $home. Replace the copy with a" >&2
  echo "     pointer to it. A second copy drifts, and when it does there is no" >&2
  echo "     way to tell which one governs." >&2
  exit 1
fi

# --- 2. Cited numbers resolve ----------------------------------------------
# Every "AGENTS.md invariant #N" / "AGENTS.md #N" / "AGENTS.md invariant N"
# citation, in Rust source or markdown, must name an entry that exists.
cited="$(git ls-files '*.rs' '*.md' \
  | grep -v '^docs/llms\.txt$' \
  | xargs grep -noE 'AGENTS\.md[^0-9]{0,20}#?[0-9]+' 2>/dev/null \
  | grep -iE 'invariant|AGENTS\.md #' || true)"

checked=0
while IFS= read -r hit; do
  [ -n "$hit" ] || continue
  file="${hit%%:*}"
  rest="${hit#*:}"
  line="${rest%%:*}"
  n="$(printf '%s\n' "$hit" | grep -oE '[0-9]+$' || true)"
  [ -n "$n" ] || continue
  checked=$((checked + 1))
  if ! grep -qE "^${n}\. \*\*" "$home"; then
    echo "FAIL $file:$line cites $home invariant #${n}, which does not exist." >&2
    echo "     $home defines 1..$home_count." >&2
    status=1
  fi
done <<EOF
$cited
EOF

if [ "$status" -eq 0 ]; then
  echo "check-invariants: OK — $home_count invariants, one normative home, $checked citation(s) resolve."
fi
exit "$status"
