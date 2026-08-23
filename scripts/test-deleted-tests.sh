#!/usr/bin/env bash
#
# Tests for check-deleted-tests.sh, focused on #4495: reading the PR's
# CURRENT description through the API instead of the stale event-payload
# snapshot, so an edited description counts on a re-run without a new push.
#
#   ./scripts/test-deleted-tests.sh
#
# Hermetic: every case builds a throwaway git repository with its own copy of
# the guard (the same shape test-gate-parity.sh and test-no-scratch.sh use —
# `cp` the real script into `<fixture>/scripts/`, then invoke it there so its
# own `dirname "$0"`-derived repo_root resolves to the fixture, not this
# repository) and drives it through `--fixture-pr-body`/`--fixture-pr-body-error`
# rather than a live `gh` call, matching check-main-red-hold.sh's fixture-flag
# idiom. No network and no `gh` needed.
#
# Not part of `make gate`: check-deleted-tests.sh itself has no `make gate`
# step either (it is inherently a two-tree question, run only in CI on
# `pull_request` — see its own header), so there is nothing here to add to a
# single-tree local gate.
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
guard="$repo_root/scripts/check-deleted-tests.sh"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/stella-deleted-tests.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT INT TERM

pass=0
fail=0

# new_repo <name> — a one-crate git repo with a base commit carrying
# `fn my_witness()` and a head commit that deletes it (the #1976 shape),
# seeded with a copy of the real guard under scripts/. Prints
# "<dir> <base-sha> <head-sha>".
new_repo() {
  local name="$1" dir
  dir="$tmp/$name"
  rm -rf "$dir"
  mkdir -p "$dir/scripts" "$dir/crates/x/src"
  cp "$guard" "$dir/scripts/check-deleted-tests.sh"
  git -C "$dir" init -q
  git -C "$dir" config user.email t@t.invalid
  git -C "$dir" config user.name t

  printf '#[test]\nfn my_witness() { assert!(true); }\n' >"$dir/crates/x/src/lib.rs"
  git -C "$dir" add -A
  git -C "$dir" commit -q -m base
  local base_sha
  base_sha="$(git -C "$dir" rev-parse HEAD)"

  printf 'pub fn noop() {}\n' >"$dir/crates/x/src/lib.rs"
  git -C "$dir" add -A
  git -C "$dir" commit -q -m "${2:-head}"
  local head_sha
  head_sha="$(git -C "$dir" rev-parse HEAD)"

  printf '%s %s %s' "$dir" "$base_sha" "$head_sha"
}

# new_repo_no_deletion <name> — a repo whose head does not touch the test at
# all, for the fast-path "nothing to acknowledge" case.
new_repo_no_deletion() {
  local name="$1" dir
  dir="$tmp/$name"
  rm -rf "$dir"
  mkdir -p "$dir/scripts" "$dir/crates/x/src"
  cp "$guard" "$dir/scripts/check-deleted-tests.sh"
  git -C "$dir" init -q
  git -C "$dir" config user.email t@t.invalid
  git -C "$dir" config user.name t
  printf '#[test]\nfn my_witness() { assert!(true); }\n' >"$dir/crates/x/src/lib.rs"
  git -C "$dir" add -A
  git -C "$dir" commit -q -m base
  local base_sha
  base_sha="$(git -C "$dir" rev-parse HEAD)"
  printf '#[test]\nfn my_witness() { assert!(true); }\npub fn extra() {}\n' >"$dir/crates/x/src/lib.rs"
  git -C "$dir" add -A
  git -C "$dir" commit -q -m head
  local head_sha
  head_sha="$(git -C "$dir" rev-parse HEAD)"
  printf '%s %s %s' "$dir" "$base_sha" "$head_sha"
}

# want <name> <expect-pass|expect-fail> <needle> <base> <head> <pr_body> [fixture-args...]
want() {
  local name="$1" expect="$2" needle="$3" dir="$4" base="$5" head="$6" body="$7"
  shift 7
  local out rc
  out="$(cd "$dir" && PR_BODY="$body" ./scripts/check-deleted-tests.sh "$base" "$head" "$@" 2>&1)"
  rc=$?
  if [ "$expect" = "expect-pass" ]; then
    if [ "$rc" -eq 0 ]; then
      pass=$((pass + 1)); echo "ok   $name"
    else
      fail=$((fail + 1)); echo "FAIL $name — expected OK, got exit $rc:"; echo "$out"
    fi
    return
  fi
  if [ "$rc" -eq 0 ]; then
    fail=$((fail + 1)); echo "FAIL $name — expected the guard to fail, but it passed:"; echo "$out"
    return
  fi
  case "$out" in
  *"$needle"*) pass=$((pass + 1)); echo "ok   $name" ;;
  *) fail=$((fail + 1)); echo "FAIL $name — wrong reason (wanted '$needle'):"; echo "$out" ;;
  esac
}

# ── D1/D2: the old channel (PR_BODY), unchanged by #4495 ────────────────────
read -r d1_dir d1_base d1_head <<EOF
$(new_repo old_channel_pass)
EOF
want "D1 PR_BODY naming the test still acknowledges it (no PR_NUMBER, no fixture)" \
  expect-pass "" "$d1_dir" "$d1_base" "$d1_head" "dropped my_witness, folded into a table test"

read -r d2_dir d2_base d2_head <<EOF
$(new_repo old_channel_fail)
EOF
want "D2 an empty PR_BODY still fails, and explains a new push is needed" \
  expect-fail "replays that same stale text" "$d2_dir" "$d2_base" "$d2_head" ""

# ── D3/D4: the fix — a live-fetched body wins over a stale PR_BODY ───────────
read -r d3_dir d3_base d3_head <<EOF
$(new_repo live_wins)
EOF
want "D3 a live-fetched body naming the test passes even though PR_BODY does not" \
  expect-pass "" "$d3_dir" "$d3_base" "$d3_head" "totally unrelated text" \
  --fixture-pr-body "dropped my_witness, folded into a table test"

read -r d4_dir d4_base d4_head <<EOF
$(new_repo live_fails_too)
EOF
want "D4 a live-fetched body that still doesn't name it fails, saying CURRENT" \
  expect-fail "CURRENT description" "$d4_dir" "$d4_base" "$d4_head" "" \
  --fixture-pr-body "unrelated text"

# ── D5/D6: an unreachable API falls back to PR_BODY, out loud ───────────────
read -r d5_dir d5_base d5_head <<EOF
$(new_repo api_error_falls_back_pass)
EOF
want "D5 an unreachable API falls back to PR_BODY, which acknowledges it" \
  expect-pass "" "$d5_dir" "$d5_base" "$d5_head" "dropped my_witness, folded into a table test" \
  --fixture-pr-body-error

read -r d6_dir d6_base d6_head <<EOF
$(new_repo api_error_falls_back_fail)
EOF
want "D6 an unreachable API falls back to an empty PR_BODY, which still fails" \
  expect-fail "replays that same stale text" "$d6_dir" "$d6_base" "$d6_head" "" \
  --fixture-pr-body-error

# ── D7: the commit-message channel is untouched by #4495 ────────────────────
read -r d7_dir d7_base d7_head <<EOF
$(new_repo commit_trailer_pass "chore: drop my_witness, folded into a table test")
EOF
want "D7 naming the test in a commit message still acknowledges it" \
  expect-pass "" "$d7_dir" "$d7_base" "$d7_head" ""

# ── D8: nothing deleted — the fast path, unaffected by any of this ──────────
read -r d8_dir d8_base d8_head <<EOF
$(new_repo_no_deletion no_deletion)
EOF
want "D8 no test lost by the merge passes regardless of PR_BODY" \
  expect-pass "" "$d8_dir" "$d8_base" "$d8_head" ""

# ── D9: malformed usage fails loudly rather than silently ───────────────────
read -r d9_dir _ _ <<EOF
$(new_repo malformed)
EOF
out="$(cd "$d9_dir" && ./scripts/check-deleted-tests.sh --fixture-pr-body 2>&1)"
rc=$?
if [ "$rc" -eq 2 ]; then
  pass=$((pass + 1)); echo "ok   D9 --fixture-pr-body with no value exits 2"
else
  fail=$((fail + 1)); echo "FAIL D9 --fixture-pr-body with no value — expected exit 2, got $rc:"; echo "$out"
fi

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
