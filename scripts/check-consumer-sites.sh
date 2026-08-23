#!/usr/bin/env bash
#
# Guard: a ConsumerPosture::Behavioral row's `site` string still points at
# live code. See #4459, #3881, and AGENTS.md invariant 10.
#
# `crates/stella-protocol/src/event/consumers.rs` is explicit about what it
# does and does not machine-check: totality over `KNOWN_TYPE_TAGS`, no
# duplicate rows, every gap posture citing an issue, and posture/`surfaces`
# coherence are all enforced by `audit_ledger` and its property tests. The one
# thing the module doc names as NOT checked is a `Behavioral` row's `site`
# string — "prose for a human reviewer... a moved consumer leaves it stale in
# exactly the way a doc comment goes stale." #3881 (PR #4446) verified all of
# them by hand, which is exactly the kind of fact a review catches once and a
# later rename or deletion silently invalidates.
#
# This closes that gap the same way check-diagnostic-codes.sh and
# check-tool-docs.sh close theirs: not by proving the site is a *behavioral*
# consumer (that judgement stays with the reviewer, per the module doc), but
# by proving the string still names something that exists. A row reading
# "crate/path.rs::renamed_away" is a doc comment that went stale with nobody
# noticing — this is what notices.
#
# ── What it checks ────────────────────────────────────────────────────────
#
# Every `site: "..."` row in `crates/stella-protocol/src/event/tags.rs` (the
# file consumers.rs's rows are generated from, #2730) has the shape
# `<crate>/<path-within-crate>.rs::<symbol>`, optionally followed by a
# parenthetical human aside (`"...::observe (run terminator, ...)"`) and
# optionally itself `::`-qualified (`"...::TurnFriction::per_goal_round"`,
# read as "the type, then the trailing symbol"). For each row:
#
#   1. `crates/<path-within-crate>` must exist as a file.
#   2. The trailing `::`-segment, with any parenthetical aside dropped, must
#      appear somewhere in that file's text.
#
# ── What it deliberately does not check ─────────────────────────────────────
#
# Step 2 is a substring match, not a symbol-table lookup — the same
# shallowness `scripts/check-tool-docs.sh` accepts for the same reason: a
# shell script has no Rust parser, and a `rg -q` over the file text is what
# stays toolchain-free (this runs in `make guards-fast`, which compiles
# nothing). That means a comment or a string literal that happens to contain
# the symbol name would also satisfy it. What it reliably catches — the
# failure mode this guard exists for — is the file moving, the file being
# deleted, or the symbol being renamed or removed outright: the substring
# then has nowhere left to match. A rename to a *different but still-present*
# name (e.g. a symbol renamed to one that collides with unrelated text
# elsewhere in the same file) is the corresponding miss, and it is narrow: it
# needs the old name to remain in the file by coincidence, in a file the row
# already points at correctly.
# Both overridable so scripts/test-consumer-sites.sh can point this at a
# throwaway fixture instead of the real tree — the same shape
# check-module-reachability.py's `<root>` argument uses, for the same reason:
# proving the guard FAILS needs a broken tree to fail on, and this repository
# has none to spare. Absolute values make the override independent of the
# `cd` below.
#
#   $1          the tags-shaped file to scan (default: the real one).
#   CRATES_ROOT the directory site paths resolve against (default: "crates").
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

tags_file="${1:-crates/stella-protocol/src/event/tags.rs}"
crates_root="${CRATES_ROOT:-crates}"

# The verdict is decided before anything is written (#1815): failure lines are
# buffered and emitted in one final best-effort write, so a reader that closes
# the pipe early cannot change the report or the exit code.
report=""
note() { report="${report}check-consumer-sites: $1"$'\n'; }
emit() {
  trap '' PIPE
  printf '%s' "$report" >&2 || true
}

if [ ! -f "$tags_file" ]; then
  note "FAIL — $tags_file does not exist."
  note "     The Behavioral rows moved; update the path this guard reads."
  emit
  exit 1
fi

# One `site: "..."` per Behavioral row, in declaration order. `-o` and `-E`
# are both portable (BSD grep on macOS and GNU grep on the CI runner agree on
# them), so this needs no perl/python dependency. `grep -o` exits 1 on zero
# matches — not an error here, a tags file with no Behavioral rows is a real
# (if unlikely) shape — so `|| true` keeps that case out of `set -e`'s way;
# the empty-`sites` branch right below is what actually reports it.
sites="$(grep -oE 'site: "[^"]*"' "$tags_file" | sed -E 's/^site: "(.*)"$/\1/' || true)"

if [ -z "$sites" ]; then
  note "FAIL — no 'site: \"...\"' rows found in $tags_file."
  note "     Either every Behavioral row was removed, or the extraction regex"
  note "     no longer matches how they're written — check by hand."
  emit
  exit 1
fi

fail=0
checked=0

while IFS= read -r site; do
  [ -z "$site" ] && continue
  checked=$((checked + 1))

  case "$site" in
  *.rs::*)
    # `%%` removes the LONGEST matching suffix, so this finds the earliest
    # ".rs::" in the string — the file/symbol boundary — even though a
    # symbol may itself contain further "::" (a type-qualified method).
    file_part="${site%%.rs::*}.rs"
    # `#` removes the SHORTEST matching prefix, landing on that same
    # boundary from the other side.
    symbol_part="${site#*.rs::}"
    ;;
  *)
    note "FAIL — site '$site' is not '<crate>/<path>.rs::<symbol>' shaped."
    note "     (no '.rs::' separator found)."
    fail=1
    continue
    ;;
  esac

  path="${crates_root}/${file_part}"
  if [ ! -f "$path" ]; then
    note "FAIL — site '$site' names '$path', which does not exist."
    note "     The consuming file moved or was deleted; update the row in"
    note "     $tags_file."
    fail=1
    continue
  fi

  # Drop a trailing parenthetical human aside, then take the last
  # ::-qualified segment: "TurnFriction::per_goal_round" is a claim about
  # "per_goal_round" living in that file, not about "TurnFriction" also
  # appearing verbatim.
  symbol="${symbol_part%% (*}"
  symbol="${symbol##*::}"

  if [ -z "$symbol" ]; then
    note "FAIL — site '$site' names no symbol after the file path."
    fail=1
    continue
  fi

  if ! grep -qF -- "$symbol" "$path"; then
    note "FAIL — site '$site' names '$symbol', which does not appear in"
    note "     '$path' at all."
    note "     The consuming symbol was renamed or removed; update the row"
    note "     in $tags_file."
    fail=1
  fi
done <<EOF
$sites
EOF

if [ "$fail" -ne 0 ]; then
  note ""
  note "Each ConsumerPosture::Behavioral row's 'site' is prose for a reviewer"
  note "naming the code that consumes an AgentEvent variant (AGENTS.md"
  note "invariant 10). Point it at wherever the consumer lives now."
  emit
  exit 1
fi

emit
printf 'check-consumer-sites: OK — %s Behavioral site(s) in %s each name a symbol still present in the file they point at.\n' \
  "$checked" "$tags_file" || true
