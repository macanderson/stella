#!/usr/bin/env bash
#
# Guard: the god-file prose matches scripts/file-size-baseline.txt. See #1435.
#
# `scripts/check-file-size.sh` enforces the 1500-line ratchet from
# `scripts/file-size-baseline.txt`, which `make file-size-update` regenerates.
# Two prose copies of *which files are god files* exist, and until this script
# nothing checked either:
#
#   1. The per-crate table in AGENTS.md § "God files — plan around them, never
#      into them".
#   2. The "God files — do not add lines" section in each crate's README —
#      the list in the eight crates that have them, and the "this crate has no
#      god files" claim in the other twelve.
#
# Both were verified by hand on 2026-08-04 and neither is touched by
# `make file-size-update`, so the next file split or rename stranded both
# silently. This is the shape that motivated `check-invariants.sh` (#630):
# duplicated normative prose with no tiebreaker.
#
# The baseline is the tiebreaker. It is generated, gate-enforced, and the only
# copy that can stay correct.
#
# ── Deliberately not checked ─────────────────────────────────────────────────
#
# The numeric ceilings. AGENTS.md names *which* files are closed to growth and
# deliberately does not repeat a single number — the baseline stays the only
# copy of those, and "fixing" that by putting numbers in prose would recreate
# the exact problem this guard exists to catch. Set membership only.
#
# The bench Python entries in the baseline have no prose table and are out of
# scope; only `crates/<crate>/` entries are read.
#
# ── If the file-budget design lands ──────────────────────────────────────────
#
# `doc:file-budget` Phase 1 deletes both `check-file-size.sh` and the baseline,
# and replaces the god-file table with banded budgets — at which point the two
# prose copies this guards stop existing and so does the drift. Whichever lands
# second reconciles with the first (#1438). Delete this script then; do not
# keep it limping against a file that is gone.
#
# Uses portable POSIX tools so it runs on a bare CI runner.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

baseline="scripts/file-size-baseline.txt"
agents="AGENTS.md"

fail=0
note() { printf 'check-god-files: %s\n' "$1" >&2; }

if [ ! -f "$baseline" ]; then
  note "FAIL — $baseline does not exist. It is the only correct copy of the"
  note "     god-file set; this guard has nothing to compare prose against."
  exit 1
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/stella-god-files.XXXXXX")"
trap 'rm -rf "$work"' EXIT

# ── The truth: "<crate> <crate-relative path>" per baselined workspace file ──

grep -v '^#' "$baseline" |
  awk '$2 ~ /^crates\// {
        path = $2
        sub(/^crates\//, "", path)
        slash = index(path, "/")
        printf "%s %s\n", substr(path, 1, slash - 1), substr(path, slash + 1)
      }' |
  LC_ALL=C sort >"$work/truth"

if [ ! -s "$work/truth" ]; then
  note "FAIL — no crates/ entries in $baseline; the extraction must have broken."
  exit 1
fi

crates_with_god_files="$(awk '{print $1}' "$work/truth" | LC_ALL=C sort -u)"

# Every workspace member, so the twelve clean crates can be held to their claim.
all_crates="$(
  find crates -mindepth 1 -maxdepth 1 -type d -exec basename {} \; |
    LC_ALL=C sort
)"

has_god_files() {
  printf '%s\n' "$crates_with_god_files" | grep -qx "$1"
}

# Backticked crate-relative source paths on stdin, one per line, sorted.
# `src/foo.rs` in either a table cell or a markdown link both reduce to this.
#
# shellcheck disable=SC2016
# The backticks are markdown, not command substitution — this pattern matches a
# literal `…` span, so single quotes are exactly right here.
backticked_paths() {
  tr ' ,|' '\n' | sed -n 's/.*`\(src\/[A-Za-z0-9_./-]*\.rs\)`.*/\1/p' | LC_ALL=C sort -u
}

# The god-file list itself, which is a markdown bullet list. Restricting to list
# items matters: the surrounding prose names the *sibling modules split out of*
# each god file as the precedent to follow (`registry/process_tools.rs` out of
# `registry.rs`), and those are examples of the remedy, not more god files.
# `|| true` is load-bearing under `set -o pipefail`: a README whose section
# carries no bullet list at all is not an error here, it is the case this guard
# most needs to report — a crate that just gained its first god file while its
# README still says it has none. Without it, grep's exit 1 kills the script
# before the comparison that would have said so.
listed_paths() {
  { grep '^- ' || true; } | backticked_paths
}

compare() {
  where="$1"
  crate="$2"
  expected_file="$3"
  found_file="$4"

  missing="$(comm -23 "$expected_file" "$found_file" | tr '\n' ' ')"
  extra="$(comm -13 "$expected_file" "$found_file" | tr '\n' ' ')"

  if [ -n "${missing// /}" ]; then
    note "FAIL — $where omits $crate god file(s): $missing"
    fail=1
  fi
  if [ -n "${extra// /}" ]; then
    note "FAIL — $where lists $crate file(s) that are not in the baseline: $extra"
    fail=1
  fi
}

# ── AGENTS.md's per-crate table ──────────────────────────────────────────────

if [ ! -f "$agents" ]; then
  note "FAIL — $agents does not exist."
  exit 1
fi

for crate in $crates_with_god_files; do
  awk -v c="$crate" '$0 ~ ("^\\| `" c "` \\|") { print }' "$agents" |
    backticked_paths >"$work/agents.$crate"

  if [ ! -s "$work/agents.$crate" ]; then
    note "FAIL — $agents's god-file table has no row for \`$crate\`, which has"
    note "     $(grep -c "^$crate " "$work/truth") baselined god file(s)."
    fail=1
    continue
  fi

  awk -v c="$crate" '$1 == c { print $2 }' "$work/truth" | LC_ALL=C sort >"$work/truth.$crate"
  compare "$agents's god-file table" "$crate" "$work/truth.$crate" "$work/agents.$crate"
done

# A crate with no baselined file must not appear in the table at all.
for crate in $all_crates; do
  has_god_files "$crate" && continue
  if grep -q "^| \`$crate\` |" "$agents"; then
    note "FAIL — $agents's god-file table lists \`$crate\`, which has no baselined file."
    note "     Its entries were split or shrank; drop the row."
    fail=1
  fi
done

# ── Each crate's README ──────────────────────────────────────────────────────

for crate in $all_crates; do
  readme="crates/$crate/README.md"
  [ -f "$readme" ] || continue

  # The "God files" section, up to the next H2.
  awk '
    /^## .*[Gg]od files/ { inside = 1; next }
    inside && /^## /     { exit }
    inside               { print }
  ' "$readme" >"$work/section"

  if [ ! -s "$work/section" ]; then
    note "FAIL — $readme has no \"God files\" section."
    note "     Every crate README carries one, so the constraint is in view"
    note "     wherever planning starts."
    fail=1
    continue
  fi

  if has_god_files "$crate"; then
    listed_paths <"$work/section" >"$work/readme.$crate"
    awk -v c="$crate" '$1 == c { print $2 }' "$work/truth" | LC_ALL=C sort >"$work/truth.$crate"
    compare "$readme" "$crate" "$work/truth.$crate" "$work/readme.$crate"
  elif ! grep -qi 'no god files' "$work/section"; then
    note "FAIL — $readme has no baselined god file, but its section does not say"
    note "     \"no god files\". Say so plainly, or the silence reads as an"
    note "     unmaintained list."
    fail=1
  fi
done

if [ "$fail" -ne 0 ]; then
  note ""
  note "$baseline is generated and gate-enforced — it is the only copy of this"
  note "set that can stay correct, so the prose follows it, never the reverse."
  note "After \`make file-size-update\`, update AGENTS.md's table and the affected"
  note "crate README in the same PR."
  exit 1
fi

printf 'check-god-files: OK — %d god file(s) across %d crate(s), named identically in %s and every crate README.\n' \
  "$(wc -l <"$work/truth" | tr -d ' ')" \
  "$(printf '%s\n' "$crates_with_god_files" | wc -l | tr -d ' ')" \
  "$agents"
