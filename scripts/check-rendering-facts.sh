#!/usr/bin/env bash
#
# Guard: a v2 rendering must not draw a fact the SPEC retired.
#
# design/tui-v2/SPEC.md is normative and the SVGs under
# design/tui-v2/renderings/ illustrate it. Nothing held the two together, and
# both directions of that gap have already cost something (#5291):
#
#   - `01-session-turn-lifecycle.svg` drew `det 86%` twice, on the turn receipt
#     and on the status bar, months after SPEC §5 said "No `det %` here, or
#     anywhere" and views/status_bar.rs recorded the removal as the owner's
#     call with "Do not restore it".
#   - The same rendering omitted SPEC 5 item 4's pipeline line, and an issue
#     was then filed citing that rendering as the source for a line the
#     rendering never contained.
#
# A picture that contradicts the prose is worse than no picture: it is read as
# the spec, and the reader has no way to know which of the two is stale.
#
# WHAT THIS CHECKS, AND WHAT IT DOES NOT
#
# Only retirement, and only by exact string. "This rendering draws every cell
# the prose requires" is not a string match — the pipeline line's absence above
# is exactly that shape, and no grep would have found it. That half stays a
# review question. This half is the half a machine can hold, and it is the half
# that recurs: a fact is retired in one place and survives in the picture.
#
# BAN is therefore append-only in spirit and has no baseline. Every entry is a
# string the prose already forbids, so the list starts satisfied and a new entry
# is added by the change that retires the fact, not to record debt. If a hit is
# legitimate the answer is to fix the rendering, never to remove the entry.
#
# Renderings only. The prose names `det %` in order to forbid it, and the Rust
# module doc names it to record the removal; both are citations, and a guard
# that could not tell a citation from a use would ban the sentence that does
# the banning.

set -euo pipefail

cd "$(dirname "$0")/.."

# Both trees that hold a v2 rendering. design/ is the source (SPEC §15);
# website/public/tui/ is what the site serves — a byte-copy, held so below.
# Both stay in the fact sweep. Scoping it to design/ alone once let the
# served copies draw `det %` while the gate reported OK (#5276).
DIRS=(design/tui-v2/renderings website/public/tui)

# Files excused from byte-parity, as `name|issue that restores it`. An
# entry is a declared gap, not a licence. The two copies drifted for
# months, and three renderings taught three different facts.
PARITY_EXCEPTIONS=()

# One entry per retired fact: the pattern, then where the prose retires it.
# The pattern is a POSIX ERE, matched against the SVG source.
#
# The `det` pattern admits the qualified spellings, not just the bare one. It
# was written as `det [0-9]+%` first and sailed straight past `det est 84%` on
# the start-work estimate line — the exact cell SPEC §1 names when it strikes
# the metric ("the receipt, the task card and the start-work estimate").
BAN=(
  "det( [a-z]+)* [0-9]+%|SPEC.md §1 and §5 item 5: \"No \`det %\` here, or anywhere\""
)

for dir in "${DIRS[@]}"; do
  if [[ ! -d $dir ]]; then
    echo "check-rendering-facts: $dir is missing — nothing to check" >&2
    exit 1
  fi
done

fail=0
for entry in "${BAN[@]}"; do
  pattern=${entry%%|*}
  why=${entry#*|}
  # `grep -E` and not rg: this runs in GATE_GUARDS_FAST, which ci.yml starts
  # before installing anything, and a bare runner has grep.
  if hits=$(grep -REn --include='*.svg' -- "$pattern" "${DIRS[@]}" 2>/dev/null); then
    fail=1
    echo "check-rendering-facts: a rendering draws a fact the SPEC retired" >&2
    echo "  pattern: $pattern" >&2
    echo "  retired by: $why" >&2
    while IFS= read -r hit; do
      echo "    $hit" >&2
    done <<<"$hits"
    echo "  Fix the rendering and regenerate its PNG from the SVG." >&2
  fi
done

# The parity half. Each served SVG is the byte-copy of its design source,
# minus the declared exceptions above. Two drifting copies of one picture
# teach two facts, and neither is labelled stale. `^S expand` against
# `tab expand` was live for months, and the deck bound neither.
for src in design/tui-v2/renderings/svg/*.svg; do
  name=$(basename "$src")
  skip=0
  # The +-expansion keeps an empty list safe under `set -u` on bash 3.2,
  # which is what macOS ships and what `make gate` runs this with.
  for exception in ${PARITY_EXCEPTIONS[@]+"${PARITY_EXCEPTIONS[@]}"}; do
    [[ $name == "${exception%%|*}" ]] && skip=1
  done
  ((skip)) && continue
  served="website/public/tui/$name"
  if [[ ! -f $served ]]; then
    fail=1
    echo "check-rendering-facts: $served is missing — the site serves every design rendering" >&2
    continue
  fi
  if ! cmp -s "$src" "$served"; then
    fail=1
    echo "check-rendering-facts: $served differs from $src" >&2
    echo "  design/ is the source (SPEC §15). Edit it there, then: cp $src $served" >&2
    echo "  A divergence is declared in PARITY_EXCEPTIONS beside the issue that ends it." >&2
  fi
done

if ((fail)); then
  exit 1
fi

echo "check-rendering-facts: OK — no rendering draws a retired fact (${#BAN[@]} checked), and the served copies match design/"
