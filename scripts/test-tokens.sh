#!/usr/bin/env bash
#
# Tests for check-tokens.py's BAN CHECK, over every notation it claims to
# watch (#4910).
#
#   ./scripts/test-tokens.sh    (or: make tokens-test)
#
# Run it after touching that script. Not part of `make gate`: it builds
# throwaway git repositories, the same posture as `make main-red-hold-test`.
#
# ── What the suite is for ────────────────────────────────────────────────────
#
# The ban check spends its life passing, because the tree is clean. That is
# what makes it worth testing and easy to break: a matcher that silently stops
# matching looks exactly like a tree with no retired hexes in it, and there is
# no third state to tell them apart.
#
# It had already happened once in the direction this suite now covers. `.rs`
# was in TEXT_SUFFIXES from the start, so the script opened every Rust file and
# reported clean over the surface where the retired kits actually shipped — but
# only for the `Color::Rgb(0x0A, ...)` spelling, which is the one this tree
# writes. The decimal spelling was matched all along, by RGB_FUNC's IGNORECASE
# reading `Rgb(` as `rgb(`. That distinction is not in #4910, which reports
# Rust as invisible outright; the case below found it, which is the argument
# for the suite in one line.
#
# So every notation gets a case that puts a banned value in it and demands the
# guard name it, and every notation gets its live-value twin that must pass.
# A matcher that fires on everything is as useless as one that fires on
# nothing, and only the pair can tell them apart.
#
# The `SELF` case is the one that reads backwards until you know why: a ban
# site holds retired values *in order to ban them*, so the guard finding them
# there is the failure, not the catch. #4066 is the merge where getting this
# wrong reported nineteen offenders that were all correct.
#
# bash 3.2 compatible.
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-tokens.py"
TOKENS_REL="design/tokens/stella-tokens.json"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

ok() { printf '  \033[32m✓\033[0m %s\n' "$*"; pass=$((pass + 1)); }
bad() { printf '  \033[31m✗\033[0m %s\n' "$*"; fail=$((fail + 1)); }

# One banned value and one live one, read out of the real token file so the
# fixtures cannot drift from the list the guard actually enforces.
BANNED_HEX="$(python3 -c "
import json, sys
doc = json.loads(open(sys.argv[1]).read())
print(doc['banned']['values'][0]['hex'].upper())
" "$repo_root/$TOKENS_REL")"
LIVE_HEX="$(python3 -c "
import json, sys
doc = json.loads(open(sys.argv[1]).read())
print(doc['tokens'][0]['hex'].upper())
" "$repo_root/$TOKENS_REL")"

# `#RRGGBB` -> `0xRR, 0xGG, 0xBB`, the way Rust writes it.
as_rust_tuple() {
  python3 -c "
import sys
h = sys.argv[1].lstrip('#')
print(', '.join('0x%s' % h[i:i+2].upper() for i in (0, 2, 4)))
" "$1"
}

# `#RRGGBB` -> `R, G, B`, decimal.
as_decimals() {
  python3 -c "
import sys
h = sys.argv[1].lstrip('#')
print(', '.join(str(int(h[i:i+2], 16)) for i in (0, 2, 4)))
" "$1"
}

# A throwaway git repository holding the real token file plus one source file.
# Real git, because the script discovers its inputs with `git ls-files` and a
# fixture that stubbed that would be testing a path nothing runs.
# $1 = case name, $2 = file path within the root, $3 = file contents.
new_root() {
  local dir="$TMP/$1"
  mkdir -p "$dir/design/tokens" "$dir/$(dirname "$2")"
  cp "$repo_root/$TOKENS_REL" "$dir/$TOKENS_REL"
  printf '%s\n' "$3" >"$dir/$2"
  git -C "$dir" init -q 2>/dev/null
  git -C "$dir" add -A 2>/dev/null
  echo "$dir"
}

# want <name> <expect-pass|expect-ban> <file> <contents>
want() {
  local name="$1" expect="$2" file="$3" contents="$4"
  local root out rc
  root="$(new_root "$(echo "$name" | tr ' /' '__')" "$file" "$contents")"
  out="$(python3 "$SCRIPT" "$root" 2>&1)"
  rc=$?
  if [ "$expect" = "expect-pass" ]; then
    if [ "$rc" -eq 0 ]; then
      ok "$name"
    else
      bad "$name — expected a pass, got exit $rc: $out"
    fi
    return
  fi
  if [ "$rc" -eq 0 ]; then
    bad "$name — the banned value was NOT caught: $out"
    return
  fi
  # Naming the file is half the guard: an exit code alone sends a reader
  # hunting through the tree for a colour the script already located.
  case "$out" in
  *"$file"*) ok "$name" ;;
  *) bad "$name — caught it but did not name $file: $out" ;;
  esac
}

echo "check-tokens ban check:"

# The notation that was already watched, as the control: if this fails, the
# suite is broken rather than the Rust matcher.
want "a banned hex literal is caught" expect-ban \
  "website/src/app/page.tsx" "const ground = \"$BANNED_HEX\";"

want "a live hex literal passes" expect-pass \
  "website/src/app/page.tsx" "const ground = \"$LIVE_HEX\";"

# The gap #4910 is about.
want "a banned Color::Rgb tuple is caught" expect-ban \
  "crates/stella-tui/src/palette.rs" \
  "pub const GROUND: Color = Color::Rgb($(as_rust_tuple "$BANNED_HEX"));"

want "a live Color::Rgb tuple passes" expect-pass \
  "crates/stella-tui/src/palette.rs" \
  "pub const GROUND: Color = Color::Rgb($(as_rust_tuple "$LIVE_HEX"));"

# Decimal channels were already covered, by accident: RGB_FUNC is IGNORECASE,
# so a decimal `Rgb(...)` matches it as an `rgb(` call. The channels are not
# spelled out here on purpose — this file is not a SELF entry and must not
# become one, because it never needs to name a retired value: every fixture
# reads its colours out of the token file at run time. The case stays since the
# coverage is real and should not silently depend on a flag on another pattern
# — but it passes with RUST_RGB removed, so it is not a witness for it. The two
# cases either side of it are.
want "a tuple written in decimal is caught (by either matcher)" expect-ban \
  "crates/stella-tui/src/palette.rs" \
  "pub const GROUND: Color = Color::Rgb($(as_decimals "$BANNED_HEX"));"

want "a bare Rgb( is caught, not only Color::Rgb(" expect-ban \
  "crates/stella-tui/src/palette.rs" \
  "pub const GROUND: Color = Rgb($(as_rust_tuple "$BANNED_HEX"));"

# The exemption, which reads backwards until you know what a ban site is.
want "a ban site may name the values it bans" expect-pass \
  "crates/stella-tui/src/theme/tests.rs" \
  "const RETIRED_INK: Color = Color::Rgb($(as_rust_tuple "$BANNED_HEX"));"

# Channels over 255 are not a colour, so the tuple is not one either.
want "an out-of-range tuple is not read as a colour" expect-pass \
  "crates/stella-tui/src/palette.rs" "let v = Rgb(300, 400, 500);"

# One literal, two matchers, one report. `Rgb(<decimal>)` is matched by both
# RGB_FUNC (IGNORECASE reads it as `rgb(`) and RUST_RGB, and before this was
# deduped the same line was listed twice and the count above the list said two
# places where there was one. Found by this suite failing on its own source
# once it was committed.
root="$(new_root "double_report" "crates/stella-tui/src/palette.rs" \
  "pub const GROUND: Color = Color::Rgb($(as_decimals "$BANNED_HEX"));")"
out="$(python3 "$SCRIPT" "$root" 2>&1)"
hits="$(printf '%s\n' "$out" | grep -c 'palette.rs:1:')"
if [ "$hits" -eq 1 ]; then
  ok "a literal both matchers see is reported once"
else
  bad "one literal was reported $hits time(s): $out"
fi

# The real tree, read-only, with no argument at all — the path every caller
# takes. A suite whose cases all pass a root would not notice the default
# breaking.
out="$(cd "$repo_root" && python3 "$SCRIPT" 2>&1)"
rc=$?
if [ "$rc" -eq 0 ]; then
  ok "the real tree passes, with no root argument"
else
  bad "the real tree should pass: exit $rc: $out"
fi

echo ""
echo "  $pass passed, $fail failed"
[ "$fail" -eq 0 ]
