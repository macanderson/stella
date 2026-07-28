#!/usr/bin/env bash
#
# Tests for setup-claude-env.sh and claude-rustfmt-hook.sh.
#
#   ./scripts/test-claude-env.sh
#
# Run it after touching either script. Not part of `make gate`: it shells out to
# rustfmt and git a few dozen times and takes a couple of seconds, which is not
# worth adding to every push for two developer-tooling scripts.
#
# ── Why a fixture instead of the real checkout ───────────────────────────────
#
# Every mutating case here runs against a synthetic stella-shaped repo in a temp
# dir, never against the worktree you invoked it from. The hook's failure modes
# can only be tested by breaking things — writing 1600-line files into a crate,
# appending bogus entries to the file-size baseline, replacing settings.json
# with garbage — and a test suite that does that to your live worktree is one
# early exit away from leaving a probe file in stella-core/src or a corrupted
# baseline staged for commit. The fixture makes the blast radius a directory
# that gets deleted on exit, including on failure (see the trap).
#
# The scripts under test are COPIED into the fixture, so this exercises the real
# code, not a reimplementation of it.
#
# ── What "tested" means here ─────────────────────────────────────────────────
#
# Mostly negative cases. A hook that exits 0 on everything would sail through a
# suite that only checks the happy path, so each guard has a case that must fail
# and a neighbouring case that must not — H8/H9 straddle a baseline ceiling,
# H12/H13 straddle the line-counting rule. H13 in particular is the only case
# that can tell `wc -l` from `awk END{NR}`; without it that distinction is
# untested, because the two agree on every file that ends in a newline.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
FIX="$(mktemp -d)"
trap 'rm -rf "$FIX"' EXIT INT TERM

pass=0
fail=0

check() { # check <name> <want-exit> <got-exit> [want-substring] [output]
  local name="$1" want="$2" got="$3" substr="${4:-}" out="${5:-}"
  if [ "$want" != "$got" ]; then
    printf 'FAIL  %-46s exit want=%s got=%s\n' "$name" "$want" "$got"
    fail=$((fail + 1)); return
  fi
  if [ -n "$substr" ] && ! printf '%s' "$out" | grep -qF "$substr"; then
    printf 'FAIL  %-46s missing: %s\n' "$name" "$substr"
    fail=$((fail + 1)); return
  fi
  printf 'ok    %s\n' "$name"
  pass=$((pass + 1))
}

# Two assertion forms, because the things being asserted come in two shapes.
# `assert` takes the exit code of a real command or pipeline that already ran
# (grep, jq, ls). `assert_cmd` takes a command to run — used wherever the
# assertion is a bare test, since reading `$?` straight after `[ ... ]` is the
# pattern shellcheck flags as SC2319 and is genuinely easy to get wrong when a
# line is later inserted between the condition and the read.
assert() { # assert <name> <exit-code>
  if [ "$2" -eq 0 ]; then printf 'ok    %s\n' "$1"; pass=$((pass + 1))
  else printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); fi
}

assert_cmd() { # assert_cmd <name> <command...>
  local name="$1"; shift
  if "$@"; then printf 'ok    %s\n' "$name"; pass=$((pass + 1))
  else printf 'FAIL  %s\n' "$name"; fail=$((fail + 1)); fi
}

# ── Fixture: a minimal repo shaped enough like stella to satisfy the scripts ──
REPO="$FIX/stella"
mkdir -p "$REPO/stella-core/src" "$REPO/scripts" "$REPO/.githooks"
cp "$repo_root/scripts/setup-claude-env.sh" "$repo_root/scripts/claude-rustfmt-hook.sh" \
   "$REPO/scripts/"
chmod +x "$REPO/scripts/"*.sh

# Same pinned channel as the real tree, so the toolchain probe resolves the same
# way here as it does for a developer.
cp "$repo_root/rust-toolchain.toml" "$REPO/rust-toolchain.toml"
printf '[workspace]\nmembers = ["stella-core"]\n\n[workspace.package]\nedition = "2024"\n' \
  > "$REPO/Cargo.toml"
cat > "$REPO/scripts/file-size-baseline.txt" <<'EOF'
# Synthetic baseline for tests. Format: <ceiling> <path>.
2282 stella-core/src/grandfathered.rs
EOF

git -C "$REPO" init -q .
git -C "$REPO" config user.email test@example.com
git -C "$REPO" config user.name test
printf 'target/\nscratch/\n' > "$REPO/.gitignore"
git -C "$REPO" add -A >/dev/null 2>&1
git -C "$REPO" commit -qm init >/dev/null 2>&1

HOOK="$REPO/scripts/claude-rustfmt-hook.sh"
SETUP="$REPO/scripts/setup-claude-env.sh"
CACHE="$FIX/cache"
export STELLA_ENV_CACHE="$CACHE"

payload() { printf '{"tool_name":"Edit","tool_input":{"file_path":"%s"}}' "$1"; }
pad() { # pad <count> ; emits <count> newline-terminated comment lines
  awk -v n="$1" 'BEGIN { for (i = 0; i < n; i++) print "// pad " i }'
}

echo "=== hook: inputs it must ignore ==="
out=$(payload "$REPO/Cargo.toml" | "$HOOK" 2>&1); check "H1  non-.rs file" 0 $? "" "$out"
echo 'fn main() {}' > "$FIX/outside.rs"
out=$(payload "$FIX/outside.rs" | "$HOOK" 2>&1); check "H2  .rs outside the repo" 0 $? "" "$out"
out=$(printf '' | "$HOOK" 2>&1); check "H3  empty stdin" 0 $? "" "$out"
out=$(printf 'not json' | "$HOOK" 2>&1); check "H4  unparseable payload" 0 $? "" "$out"
out=$(payload "$REPO/stella-core/src/nope.rs" | "$HOOK" 2>&1); check "H5  nonexistent file" 0 $? "" "$out"

echo
echo "=== hook: formatting ==="
probe="$REPO/stella-core/src/probe.rs"
printf 'fn  main( ) {\nlet x=1;\n   let y   =   2;\n}\n' > "$probe"
out=$(cd "$REPO" && payload "$probe" | "$HOOK" 2>&1); check "H6  small file passes" 0 $? "" "$out"
grep -q 'let x = 1;' "$probe"; assert "H6b rustfmt actually rewrote it" $?

echo
echo "=== hook: file-size ratchet ==="
big="$REPO/stella-core/src/big.rs"
pad 1600 > "$big"
out=$(cd "$REPO" && payload "$big" | "$HOOK" 2>&1); rc=$?
check "H7  new file over 1500 blocks" 2 $rc "NOT in scripts/file-size-baseline.txt" "$out"

gf="$REPO/stella-core/src/grandfathered.rs"
pad 2400 > "$gf"   # baseline ceiling is 2282
out=$(cd "$REPO" && payload "$gf" | "$HOOK" 2>&1); rc=$?
check "H8  grandfathered past ceiling blocks" 2 $rc "over its baseline ceiling of 2282" "$out"

pad 2000 > "$gf"   # same file, now under its ceiling
out=$(cd "$REPO" && payload "$gf" | "$HOOK" 2>&1); rc=$?
check "H9  grandfathered under ceiling passes" 0 $rc "" "$out"

mkdir -p "$REPO/scratch"
cp "$big" "$REPO/scratch/ignored.rs"
out=$(cd "$REPO" && payload "$REPO/scratch/ignored.rs" | "$HOOK" 2>&1); rc=$?
check "H10 gitignored file skipped" 0 $rc "" "$out"

# The gate judges tracked files with `wc -l`. These two cases straddle that:
# H12 confirms the number the hook reports is wc's, H13 confirms it is not
# awk's NR, which differs by one when the file lacks a trailing newline.
out=$(cd "$REPO" && payload "$big" | "$HOOK" 2>&1)
printf '%s' "$out" | grep -qF "$(wc -l <"$big" | tr -d ' ') lines"
assert "H12 reported count == wc -l" $?

edge="$REPO/stella-core/src/edge.rs"
# 1500 terminated lines + an unterminated final line: wc -l = 1500 (allowed),
# awk NR = 1501 (would block). The final line is invalid Rust on purpose so
# rustfmt bails and leaves the missing newline in place.
{ pad 1500; printf 'fn broken('; } > "$edge"
assert_cmd "H13a fixture really is 1500 by wc" \
  test "$(wc -l <"$edge" | tr -d ' ')" = "1500"
out=$(cd "$REPO" && payload "$edge" | "$HOOK" 2>&1); rc=$?
check "H13 unterminated last line counted as wc does" 0 $rc "" "$out"

echo
echo "=== setup: argument + location handling ==="
out=$("$SETUP" --bogus 2>&1); check "S1  unknown option" 2 $? "unknown option" "$out"
out=$(cd "$FIX" && "$SETUP" --check 2>&1); check "S2  outside a git repo" 1 $? "not inside a git checkout" "$out"
mkdir -p "$FIX/other" && git -C "$FIX/other" init -q .
out=$(cd "$FIX/other" && "$SETUP" --check 2>&1); check "S3  a git repo that is not stella" 1 $? "does not look like the stella workspace" "$out"

# The only genuinely fatal class: no usable Rust toolchain. git stays on PATH,
# since the script cannot even locate the repo without it.
BIN="$FIX/bin"; mkdir -p "$BIN"
for b in bash sh sed awk cksum grep cat basename dirname pwd du rm mkdir cp mv \
         chmod tr head printf wc env sort git jq; do
  src="$(command -v "$b" 2>/dev/null)" && ln -sf "$src" "$BIN/$b"
done
out=$(cd "$REPO" && env PATH="$BIN" "$SETUP" --check 2>&1); rc=$?
check "S4  no rustup is fatal" 1 $rc "required tool(s) missing" "$out"
printf '%s' "$out" | grep -qF "pins $(sed -n 's/^channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$REPO/rust-toolchain.toml")"
assert "S4b names the pinned channel" $?

echo
echo "=== setup: writes ==="
out=$(cd "$REPO" && "$SETUP" --check 2>&1)
assert_cmd "S5  --check writes nothing" test ! -f "$REPO/.claude/settings.json"

(cd "$REPO" && "$SETUP" >/dev/null 2>&1); rc1=$?
(cd "$REPO" && "$SETUP" >/dev/null 2>&1); rc2=$?
assert_cmd "S6  full run is idempotent" test "$rc1$rc2" = "00"
if command -v jq >/dev/null 2>&1; then
  jq -e '(.env.STELLA_HOME|length > 0) and (.env.CARGO_TARGET_DIR|length > 0)' \
    "$REPO/.claude/settings.json" >/dev/null 2>&1
  assert "S6b settings.json is valid JSON with the isolation env" $?
fi

# STELLA_HOME and CARGO_TARGET_DIR must differ per worktree — that is the whole
# point of the script, so it gets an explicit case rather than being assumed.
home_a="$(jq -r .env.STELLA_HOME "$REPO/.claude/settings.json" 2>/dev/null)"
REPO2="$FIX/stella2"
cp -R "$REPO" "$REPO2"
rm -rf "$REPO2/.claude"
(cd "$REPO2" && "$SETUP" >/dev/null 2>&1)
home_b="$(jq -r .env.STELLA_HOME "$REPO2/.claude/settings.json" 2>/dev/null)"
differing_homes() { [ -n "$home_a" ] && [ -n "$home_b" ] && [ "$home_a" != "$home_b" ]; }
assert_cmd "S6c two checkouts get different STELLA_HOME" differing_homes

echo
echo "=== setup: existing settings.json ==="
rm -f "$REPO/.claude/.stella-env-stamp"
printf '{ "model": "claude-opus-5", "env": { "MY_OWN_VAR": "keepme" } }\n' > "$REPO/.claude/settings.json"
(cd "$REPO" && "$SETUP" >/dev/null 2>&1)
if command -v jq >/dev/null 2>&1; then
  jq -e '.model == "claude-opus-5" and .env.MY_OWN_VAR == "keepme" and (.env.STELLA_HOME|length > 0)' \
    "$REPO/.claude/settings.json" >/dev/null 2>&1
  assert "S7  hand-written keys survive the merge" $?
fi
ls "$REPO/.claude/settings.json.bak."* >/dev/null 2>&1; assert "S7b a backup was written" $?
rm -f "$REPO/.claude/settings.json.bak."*

rm -f "$REPO/.claude/.stella-env-stamp"
printf '{ this is not json\n' > "$REPO/.claude/settings.json"
out=$(cd "$REPO" && "$SETUP" 2>&1)
if command -v jq >/dev/null 2>&1; then
  jq -e . "$REPO/.claude/settings.json" >/dev/null 2>&1; assert "S8  invalid JSON is replaced" $?
fi
printf '%s' "$out" | grep -qF 'not valid JSON'; assert "S8b and the replacement is announced" $?
rm -f "$REPO/.claude/settings.json.bak."*

(cd "$REPO" && "$SETUP" --no-hooks >/dev/null 2>&1)
if command -v jq >/dev/null 2>&1; then
  jq -e 'has("hooks") | not' "$REPO/.claude/settings.json" >/dev/null 2>&1
  assert "S9  --no-hooks omits the hook block" $?
fi

echo
echo "=== setup: --prune ==="
mkdir -p "$CACHE/dead/target" "$CACHE/live/target" "$CACHE/unowned/target"
printf '%s\n' "$FIX/deleted-worktree" > "$CACHE/dead/.owner"
printf '%s\n' "$REPO" > "$CACHE/live/.owner"
# "unowned" carries no .owner marker: it stands in for a cache directory some
# other tool created, which prune must never touch.
out=$(cd "$REPO" && "$SETUP" --prune 2>&1)
assert_cmd "P1  cache for a deleted worktree is removed" test ! -d "$CACHE/dead"
assert_cmd "P2  cache for a live worktree is kept" test -d "$CACHE/live"
assert_cmd "P3  unmarked cache is left alone" test -d "$CACHE/unowned"
printf '%s' "$out" | grep -qF 'no .owner marker, leaving alone'; assert "P4  and says so" $?

echo
printf 'passed=%d failed=%d\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
