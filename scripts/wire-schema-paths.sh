#!/usr/bin/env bash
#
# The wire-contract path set, derived from the workflow that gates it. See
# #3836, and #2847 for the pattern this copies.
#
# `.github/workflows/wire-schema.yml` decides, from its `paths:` filter, whether
# a change can have invalidated the generated artifacts under `docs/wire/`.
# `.githooks/pre-push` asks the same question about the pushed diff, to choose
# between `make guards-fast` (compiles nothing) and `make guards` (runs the two
# schema exporters). Until this script existed the hook carried its own regex,
# with a comment saying out loud that neither answer was derived from the other:
#
#     # Keep this list in sync with that workflow's `paths:` filter — both
#     # answer the same question, and neither is derived from the other.
#
# That is the shape #2847 abolished for the bench suites, where the divergence
# had already cost a red `main`. Here it would be quieter and in the direction
# that misleads: the hook narrows to `guards-fast` for a diff the workflow WOULD
# have considered wire-touching, and a stale generated schema leaves the machine
# believing it was checked. That is precisely the hole #1439 made this rung
# conditional to close.
#
#   list    print one path pattern per line, in workflow order, deduplicated
#   filter  print an anchored ERE matching those paths, for `grep -E`
#
# The parse is TOTAL by construction: a `paths:` entry this script cannot
# translate is a hard error, never a skip. A skipped entry is a narrower filter
# than the workflow's, which is the failure this file exists to prevent.
# `scripts/check-wire-paths.sh` (`make wire-paths`) runs the parse on every gate,
# so a workflow edit meets it immediately rather than at the next push.
#
# Two glob shapes are recognised, because they are the two the workflow uses:
#
#   dir/**    a directory and everything under it  ->  ^dir/
#   a/b.ext   one exact file                       ->  ^a/b\.ext$
#
# Anything else — a mid-path `*`, a `?`, a bare `**` — stops the parse rather
# than being approximated. GitHub's glob syntax is richer than the hook's grep,
# and quietly approximating one in the other is how the two answers diverge
# again with nothing to say so.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

workflow=".github/workflows/wire-schema.yml"

die() {
  printf 'wire-schema-paths: %s\n' "$@" >&2
  exit 1
}

[ -f "$workflow" ] || die "$workflow does not exist." \
  "  That workflow is the source of the path set; there is no second copy" \
  "  to fall back to, on purpose."

# ── The parse ────────────────────────────────────────────────────────────────
#
# Every entry under every `paths:` key, in file order. The workflow declares the
# same list twice — once for `pull_request`, once for `push` — so the union is
# taken and duplicates are dropped while order is preserved.
#
# `paths-ignore:` is deliberately not matched: it is the inverse question, and
# a filter that treated an ignore list as a match list would select exactly the
# paths the workflow excludes. Comment lines go first, for the reason
# scripts/bench-suites.sh drops them — this workflow's header discusses its own
# `paths:` in prose, and prose is not a filter.
patterns() {
  awk '
    /^[[:space:]]*#/ { next }
    /^[[:space:]]*paths:[[:space:]]*$/ { inside = 1; next }
    inside && /^[[:space:]]*-[[:space:]]*/ {
      entry = $0
      sub(/^[[:space:]]*-[[:space:]]*/, "", entry)
      sub(/[[:space:]]*$/, "", entry)
      gsub(/^"|"$/, "", entry)
      gsub(/^'"'"'|'"'"'$/, "", entry)
      if (entry != "" && !(entry in seen)) { seen[entry] = 1; print entry }
      next
    }
    inside { inside = 0 }
  ' "$workflow"
}

# ── The filter ───────────────────────────────────────────────────────────────

# Every ERE metacharacter in a literal path, backslash-escaped. The paths are
# ordinary today, but `.` in `wire-schema.yml` already matters: unescaped it
# matches any character, so the filter would accept `wire-schemaXyml` and, more
# to the point, would stop being a statement about the path the workflow named.
# shellcheck disable=SC2016 # the sed program is deliberately single-quoted: the
# `$`, `\\` and `&` inside it belong to sed, not to the shell.
ere_escape() {
  printf '%s' "$1" | sed 's/[.[\*^$()+?{}|\\]/\\&/g'
}

filter() {
  local list ere entry escaped
  list="$(patterns)"
  [ -n "$list" ] || die \
    "found no \`paths:\` entries in $workflow." \
    "  .githooks/pre-push reads its wire-touched predicate from that list; it" \
    "  has no copy of its own. If the trigger shape changed, teach this parse" \
    "  the new one rather than typing the pattern out somewhere else."

  ere=""
  while IFS= read -r entry; do
    [ -n "$entry" ] || continue
    case "$entry" in
    */\*\*)
      # A directory subtree. Strip the `**`, keep the trailing slash, and
      # anchor at the start only — everything below it matches.
      escaped="$(ere_escape "${entry%\*\*}")"
      ;;
    *\**| *\?*)
      die "cannot translate the \`paths:\` entry '$entry' from $workflow." \
        "  Recognised shapes are 'dir/**' and an exact file path; see the" \
        "  header of this script. Approximating a glob in grep is how the" \
        "  hook and the workflow diverge with nothing to say so."
      ;;
    *)
      # An exact file. Anchored at both ends so a longer path that merely
      # starts with it cannot match.
      escaped="$(ere_escape "$entry")\$"
      ;;
    esac
    ere="${ere:+$ere|}$escaped"
  done <<<"$list"

  printf '^(%s)\n' "$ere"
}

case "${1:-}" in
list) patterns ;;
filter) filter ;;
*)
  printf 'usage: %s {list|filter}\n' "$0" >&2
  exit 2
  ;;
esac
