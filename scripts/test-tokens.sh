#!/usr/bin/env bash
#
# Tests for check-tokens.py's BAN CHECK, over every notation it claims to
# watch (#4910), for its CITATION CHECK, whose subject is a pair (#3653), and
# for its PAINT CHECK, whose subject is an absence (#4975).
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
# The citation cases earn their own section because that check's subject is a
# *pair* rather than a value. Neither older check can see a name quoted at a
# value that is some other token's: that value is live, so it passes a
# membership test while saying something false. A name quoted at a value the
# palette has moved past is the other failure mode, and looks nothing like it.
#
# The paint cases earn theirs because that check's subject is an ABSENCE, and
# every other check here is about something present. A token nothing paints is
# a value that is live, correctly quoted, and inside no token-only path -- so
# all three older checks pass it, which is exactly what happened to `comment`
# (#4946, #4975). Both directions get a case: a `painted` declaration with no
# site must fail, and a `gap` declaration that has since acquired one must fail
# too, or the ledger records a hole somebody already filled.
#
# Every fixture below reads its colours out of the real token file at run time,
# citation fixtures included — which is why this file is in neither `SELF` nor
# any other exemption, and must not become one. A suite that needed a blanket
# pass would be handing every file that calls itself a test the same pass.
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

# The citation fixtures' raw material, all of it derived rather than typed:
# three (css name, hex) pairs with distinct values, one hex that is neither a
# token nor banned, and one `--st-` name the palette does not declare. Emitted
# in one call because the alternative is six interpreter starts to learn what
# one file already says.
eval "$(python3 -c "
import json, sys
doc = json.loads(open(sys.argv[1]).read())
live = {t['hex'].upper() for t in doc['tokens']}
banned = {e['hex'].upper() for e in doc['banned']['values']}
names = {t['css'] for t in doc['tokens']}
picked, seen = [], set()
for t in doc['tokens']:
    h = t['hex'].upper()
    if h not in seen:
        seen.add(h)
        picked.append((t['css'], h))
    if len(picked) == 3:
        break
for i, (css, hexv) in enumerate(picked, 1):
    print('C%d_CSS=%s' % (i, css))
    print('C%d_HEX=%s' % (i, hexv))
# A value the palette neither holds nor bans: a misquote rather than a ban hit,
# which is the whole point of the pair these cases test.
base = picked[0][1]
for d in range(1, 256):
    cand = '#%s%02X' % (base[1:5], (int(base[5:7], 16) + d) % 256)
    if cand not in live and cand not in banned:
        print('WRONG_HEX=%s' % cand)
        break
else:
    raise SystemExit('no free hex adjacent to %s' % base)
for cand in ('--st-taupe', '--st-puce', '--st-mauve'):
    if cand not in names:
        print('UNKNOWN_CSS=%s' % cand)
        break
else:
    raise SystemExit('every candidate name is a real token')
" "$repo_root/$TOKENS_REL")"

# A throwaway git repository holding the real token file plus one source file.
# Real git, because the script discovers its inputs with `git ls-files` and a
# fixture that stubbed that would be testing a path nothing runs.
#
# The token file arrives with every `paint` posture rewritten to a gap. A
# fixture tree holds one source file, so it paints nothing, and the real file's
# seventeen `painted` declarations would fail check 4 in every case below --
# turning a suite about the ban and the citation into a suite about the paint
# sweep. Values, names and the ban list are copied untouched, which is what the
# other three checks read. The paint cases rewrite the posture they are about
# and read the token names out of the same file, so nothing here is typed.
#
# $1 = case name, $2 = file path within the root, $3 = file contents,
# $4 = optional `name=posture` (posture is `painted`, else an issue for a gap).
new_root() {
  local dir="$TMP/$1"
  mkdir -p "$dir/design/tokens" "$dir/$(dirname "$2")"
  python3 -c "
import json, sys
doc = json.loads(open(sys.argv[1]).read())
override = sys.argv[3]
name, _, posture = override.partition('=')
for tok in doc['tokens']:
    if 'name' not in tok:
        continue
    if tok['name'] == name:
        tok['paint'] = 'painted' if posture == 'painted' else {'gap': posture}
    else:
        tok['paint'] = {'gap': '#4975'}
open(sys.argv[2], 'w').write(json.dumps(doc, indent=2))
" "$repo_root/$TOKENS_REL" "$dir/$TOKENS_REL" "${4:-}"
  printf '%s\n' "$3" >"$dir/$2"
  git -C "$dir" init -q 2>/dev/null
  git -C "$dir" add -A 2>/dev/null
  echo "$dir"
}

# want <name> <expect-pass|expect-ban|expect-fail|expect-absent> <file>
#      <contents> [substring] [token=posture]
#
# `expect-ban` and `expect-fail` are the same assertion — a non-zero exit — and
# both spellings exist because a misquote is not a ban and calling it one at the
# call site would read as a mistake.
#
# `expect-absent` is that assertion minus one clause: the paint check's subject
# is a token nothing names, and an absence has no file to name. Every other
# failure here is a value sitting at a line, so those must report where.
#
# The optional substring is checked on BOTH verdicts. On a failure it pins which
# value was named and in what words; on a pass it pins what the run reported,
# without which a guard that passed by scanning nothing satisfies the case.
#
# The optional `token=posture` is the paint cases' subject; see `new_root`.
want() {
  local name="$1" expect="$2" file="$3" contents="$4" sub="${5:-}" paint="${6:-}"
  local root out rc
  root="$(new_root "$(echo "$name" | tr ' /' '__')" "$file" "$contents" "$paint")"
  out="$(python3 "$SCRIPT" "$root" 2>&1)"
  rc=$?
  if [ "$expect" = "expect-pass" ]; then
    if [ "$rc" -ne 0 ]; then
      bad "$name — expected a pass, got exit $rc: $out"
      return
    fi
    case "$out" in
    *"$sub"*) ok "$name" ;;
    *) bad "$name — passed but did not report '$sub': $out" ;;
    esac
    return
  fi
  if [ "$rc" -eq 0 ]; then
    bad "$name — the offending value was NOT caught: $out"
    return
  fi
  # Naming the file is half the guard: an exit code alone sends a reader
  # hunting through the tree for a colour the script already located. An
  # absence is the one finding with nowhere to point.
  if [ "$expect" != "expect-absent" ]; then
    case "$out" in
    *"$file"*) ;;
    *)
      bad "$name — caught it but did not name $file: $out"
      return
      ;;
    esac
  fi
  case "$out" in
  *"$sub"*) ok "$name" ;;
  *) bad "$name — caught it but did not report '$sub': $out" ;;
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

# A palette that was never a kit is banned on the same list and for the same
# reason: nobody specified it, so it has no legitimate use. This is #3653's own
# Verify step 2, and until the deck's four invented values joined
# `banned.values` nothing in the tree rejected them — not retired, not a token,
# so both older checks passed them. The hex is read out of the list rather than
# typed, like every other fixture here.
DECK_HEX="$(python3 -c "
import json, sys
doc = json.loads(open(sys.argv[1]).read())
for e in doc['banned']['values']:
    if 'never a kit value' in e['was']:
        print(e['hex'].upper())
        break
else:
    raise SystemExit('no never-a-kit entry in banned.values')
" "$repo_root/$TOKENS_REL")"
want "the deck's invented palette is rejected as one the kit never defined" expect-ban \
  "website/public/presentations/investor-deck.html" "  --gold: $DECK_HEX;" \
  "never a kit value"

echo ""
echo "check-tokens citation check:"

# A token quoted at a value the palette has moved past. This is the shape the
# kit page and the design brief shipped, nine rows apiece (#3653).
want "a superseded value is named with both hexes" expect-fail \
  "docs/kit.md" "| $C1_CSS | $WRONG_HEX | the canvas |" \
  "$C1_CSS is quoted as $WRONG_HEX but holds $C1_HEX"

# The failure neither older check can see: the value is a LIVE token, so it
# passes the ban and passes a membership test, and is still the wrong colour for
# the name it is written beside. Two shipped stylesheets did exactly this.
want "a live token bound to the wrong name is flagged" expect-fail \
  "docs/kit.css" "  $C1_CSS: $C2_HEX;" \
  "$C1_CSS is quoted as $C2_HEX but holds $C1_HEX"

# A hex under an `--st-` name the palette does not declare. Every reader
# resolves it off their own fallback and the document goes on stating a rule
# nothing applies — the css-vars failure one notation over.
want "a hex under a name no token declares is flagged" expect-fail \
  "docs/kit.css" "  $UNKNOWN_CSS: $WRONG_HEX;" \
  "no token declares that name"

# The correct value passes, and the report says how many pairs it read — without
# which every case above is satisfiable by a guard that fails on everything.
want "a correct citation passes and is counted" expect-pass \
  "docs/kit.md" "| $C1_CSS | $C1_HEX | the canvas |" \
  "1 token citation(s) quote the palette"

# Lower case is the same colour. `website/src/app/tokens.css` writes its whole
# ramp that way, so a case-sensitive compare would have failed the tree this was
# written against.
want "a lower-case citation is read as the same value" expect-pass \
  "docs/kit.css" "  $C1_CSS: $(echo "$C1_HEX" | tr 'A-F' 'a-f');" \
  "1 token citation(s) quote the palette"

# Several tokens on one line: the hex after each name belongs to that name. A
# window reaching past the next `--st-` would report all three as wrong while
# all three are right, which is the shape a minified stylesheet ships in.
want "a minified line pairs each name with its own value" expect-pass \
  "docs/kit.css" ":root{$C1_CSS:$C1_HEX;$C2_CSS:$C2_HEX;$C3_CSS:$C3_HEX}" \
  "3 token citation(s) quote the palette"

# The other direction of the same rule: one wrong value among several on a line
# is named, and named as itself rather than as a neighbour.
want "the wrong value on a shared line is named as itself" expect-fail \
  "docs/kit.css" ":root{$C1_CSS:$C1_HEX;$C2_CSS:$WRONG_HEX;$C3_CSS:$C3_HEX}" \
  "$C2_CSS is quoted as $WRONG_HEX but holds $C2_HEX"

# A token named with no value beside it is prose, not a citation. Both brand
# documents discuss tokens by name in running text far more often than they
# tabulate them, and a guard reading those as claims would be unusable.
want "a token named in prose with no value is not a citation" expect-pass \
  "docs/kit.md" "Cards sit on $C2_CSS and hairlines on $C3_CSS." \
  "0 token citation(s) quote the palette"

# What SELF must NOT buy. A ban site quotes a retired value on purpose; that is
# no licence to misquote a live token, and an exemption suppressing all three
# checks would have hidden this.
want "SELF suppresses the ban and not the citation check" expect-fail \
  "crates/stella-tui/src/theme/tests.rs" "// $C1_CSS is $WRONG_HEX" \
  "$C1_CSS is quoted as $WRONG_HEX but holds $C1_HEX"

# Inside TOKEN_ONLY every hex must be a live token, whatever it is written next
# to. This is the check the citation one does not replace: an unnamed stray,
# under a variable the palette knows nothing about.
want "a stray hex in a token-only path is flagged" expect-fail \
  "docs/brand/css/tokens.css" "  --custom: $WRONG_HEX;" \
  "is not a token in"

echo ""
echo "check-tokens paint check:"

# The subject of every case below: one token that carries both notations, read
# out of the real file so a rename cannot leave these cases testing nothing.
eval "$(python3 -c "
import json, sys
doc = json.loads(open(sys.argv[1]).read())
for t in doc['tokens']:
    if t.get('rust'):
        print('P_NAME=%s' % t['name'])
        print('P_CSS=%s' % t['css'])
        print('P_RUST=%s' % t['rust'])
        break
else:
    raise SystemExit('no token declares a rust name')
" "$repo_root/$TOKENS_REL")"

# The failure this check exists for, and the one every other check here passes:
# a token declared `painted` that nothing in the tree names. `comment` shipped
# in exactly this state — a live value, correctly quoted, in no token-only path.
want "a painted token nothing names is caught" expect-absent \
  "website/src/app/page.tsx" "const x = 1;" \
  "$P_NAME: declares \`painted\`, and nothing in the tree names $P_CSS" \
  "$P_NAME=painted"

# The other direction, and the half a one-sided check would miss: a gap that
# somebody has since closed. Left standing, the ledger records a hole that is
# not there any more, and the next reader trusts it.
want "a gap the tree has closed is caught" expect-fail \
  "website/src/app/globals.css" "  color: var($P_CSS);" \
  "declares a gap citing #4975, but 1 site(s) name it" \
  "$P_NAME=#4975"

# Both notations satisfy a `painted` declaration. Two cases rather than one,
# because a matcher that only sees CSS looks exactly like a clean tree on the
# Rust side — the shape #4910 found one check over.
want "a var() use satisfies a painted declaration" expect-pass \
  "website/src/app/globals.css" "  color: var($P_CSS);" \
  "1 token(s) painted" \
  "$P_NAME=painted"

want "a token:: use satisfies a painted declaration" expect-pass \
  "crates/stella-tui/src/views/session.rs" \
  "let s = Style::new().fg(token::$P_RUST);" \
  "1 token(s) painted" \
  "$P_NAME=painted"

# A **declaration** is not a paint. This is the distinction the whole check
# turns on: `comment` had four of these across four surfaces and no reader ever
# saw the colour, so a mirrored stylesheet binding the name must not count.
want "a declaration in a mirror sheet is not a paint" expect-absent \
  "docs/brand/css/tokens.css" "  $P_CSS: $LIVE_HEX;" \
  "declares \`painted\`, and nothing in the tree names $P_CSS" \
  "$P_NAME=painted"

# `fallback.rs` names every terminal token by construction — `ansi16` is total
# over `token::ALL` — so counting it would pass six tokens nothing paints.
want "fallback.rs's total map does not count as a paint" expect-absent \
  "crates/stella-tui-theme/src/fallback.rs" \
  "        token::$P_RUST => Color::Black," \
  "declares \`painted\`, and nothing in the tree names $P_CSS" \
  "$P_NAME=painted"

# The door, in gen-tokens.py rather than here: a token with no posture at all.
# Checked through the generator because that is what refuses it, and a suite
# that only tested the sweep would leave the door untested.
paint_root="$(new_root "no_posture" "website/src/app/page.tsx" "const x = 1;")"
python3 -c "
import json, sys
doc = json.loads(open(sys.argv[1]).read())
for tok in doc['tokens']:
    tok.pop('paint', None)
open(sys.argv[1], 'w').write(json.dumps(doc, indent=2))
" "$paint_root/$TOKENS_REL"
out="$(python3 "$repo_root/scripts/gen-tokens.py" --check "$paint_root" 2>&1)"
rc=$?
case "$rc:$out" in
0:*) bad "a token with no \`paint\` posture was accepted: $out" ;;
*"no \`paint\` posture"*) ok "a token with no posture is refused at the door" ;;
*) bad "refused with the wrong message: $out" ;;
esac

# A gap with no issue is the silence the field exists to refuse. `{"gap": ""}`
# and `{"gap": "later"}` are both a token nobody has to explain, which is the
# state `comment` was in for its whole life.
gap_root="$(new_root "empty_gap" "website/src/app/page.tsx" "const x = 1;" \
  "$P_NAME=later")"
out="$(python3 "$repo_root/scripts/gen-tokens.py" --check "$gap_root" 2>&1)"
rc=$?
case "$rc:$out" in
0:*) bad "a gap citing no issue was accepted: $out" ;;
*"must cite an issue"*) ok "a gap that cites no issue is refused at the door" ;;
*) bad "refused with the wrong message: $out" ;;
esac

echo ""
echo "check-tokens, the real tree:"

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
