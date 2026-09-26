#!/usr/bin/env bash
#
# Tests for check-deleted-tests.sh. Each case builds a throwaway history, runs
# the guard on it and checks the verdict. A guard that has stopped failing
# turns this suite red.
#
# Most cases check that the guard reads the PR's CURRENT description through
# the API instead of the stale event-payload snapshot. An edited description
# then counts on a re-run without a new push. D12 to D16 and D25 to D26 run
# the guard with no arguments on a merge commit, which is how ci.yml runs it.
# D21 to D24 check that the guard names the channel it read and the depth it
# could see.
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
collector="$repo_root/scripts/collect-python-tests.py"
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
  cp "$collector" "$dir/scripts/collect-python-tests.py"
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
  cp "$collector" "$dir/scripts/collect-python-tests.py"
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

# new_py_repo <name> [head-commit-message] — the Python analogue of new_repo,
# for the D17/D18 witness pair: a base commit with
# tests/test_thing.py holding one `def test_my_witness`, and a head commit
# that removes the whole file. Deleting the file, not just the function
# inside it, keeps the head tree free of a test-named file with nothing in
# it — the ordinary way a repository actually drops a file's last test, and
# the shape that does not also trip the collector's own anti-vacuity check
# (D19 below), which is a different, narrower question: whether a test-named
# file that still EXISTS ever comes back empty. Prints
# "<dir> <base-sha> <head-sha>".
new_py_repo() {
  local name="$1" dir
  dir="$tmp/$name"
  rm -rf "$dir"
  mkdir -p "$dir/scripts" "$dir/tests"
  cp "$guard" "$dir/scripts/check-deleted-tests.sh"
  cp "$collector" "$dir/scripts/collect-python-tests.py"
  git -C "$dir" init -q
  git -C "$dir" config user.email t@t.invalid
  git -C "$dir" config user.name t

  printf 'def test_my_witness():\n    assert True\n' >"$dir/tests/test_thing.py"
  git -C "$dir" add -A
  git -C "$dir" commit -q -m base
  local base_sha
  base_sha="$(git -C "$dir" rev-parse HEAD)"

  git -C "$dir" rm -q tests/test_thing.py
  git -C "$dir" commit -q -m "${2:-head}"
  local head_sha
  head_sha="$(git -C "$dir" rev-parse HEAD)"

  printf '%s %s %s' "$dir" "$base_sha" "$head_sha"
}

# new_py_vacuous_repo <name> — a repo whose one Python file is named the way
# pytest expects a test file to be named but does not parse (a leftover merge
# marker), for D19: the AST walk finds the file but can read no `def test_*`
# inside it, which must fail loudly as the collector or the tree breaking,
# not pass silently as an empty tree with nothing to compare. Prints the
# directory; caller diffs it against itself, since the point is the single
# tree, not a deletion.
new_py_vacuous_repo() {
  local name="$1" dir
  dir="$tmp/$name"
  rm -rf "$dir"
  mkdir -p "$dir/scripts" "$dir/tests"
  cp "$guard" "$dir/scripts/check-deleted-tests.sh"
  cp "$collector" "$dir/scripts/collect-python-tests.py"
  git -C "$dir" init -q
  git -C "$dir" config user.email t@t.invalid
  git -C "$dir" config user.name t
  printf '<<<<<<< HEAD\ndef test_a():\n    pass\n' >"$dir/tests/test_broken.py"
  git -C "$dir" add -A
  git -C "$dir" commit -q -m base
  printf '%s' "$dir"
}

# new_pr_merge <name> <pr-lib> <main-lib> <merge-lib> [pr-commit-message] builds
# the history ci.yml checks out on a pull request. After the base commit, the
# PR branch and main each gain a commit. HEAD merges the branch into main, so
# HEAD^1 is main's tip. Each lib argument is the text of crates/x/src/lib.rs
# on that side, and an empty one leaves the file alone. A merge-lib replaces
# what git merged. That is how a merge loses a test only main carried. The
# fifth argument names the PR branch's own commit, which the guard's
# `base_ref..head_ref` walk can read (it holds this commit and the merge
# commit, since `main moves on` is on `main`'s own side and is excluded);
# an empty value keeps the default "pr work". Prints the directory.
new_pr_merge() {
  local dir="$tmp/$1" pr_lib="$2" main_lib="$3" merge_lib="$4" pr_msg="${5:-pr work}"
  rm -rf "$dir"
  mkdir -p "$dir/scripts" "$dir/crates/x/src"
  cp "$guard" "$dir/scripts/check-deleted-tests.sh"
  cp "$collector" "$dir/scripts/collect-python-tests.py"
  git -C "$dir" init -q -b main
  git -C "$dir" config user.email t@t.invalid
  git -C "$dir" config user.name t
  git -C "$dir" config commit.gpgsign false
  printf '%s' "$lib_base" >"$dir/crates/x/src/lib.rs"
  git -C "$dir" add -A
  git -C "$dir" commit -q -m base

  git -C "$dir" checkout -q -b pr
  printf 'pr\n' >"$dir/pr.txt"
  [ -n "$pr_lib" ] && printf '%s' "$pr_lib" >"$dir/crates/x/src/lib.rs"
  git -C "$dir" add -A
  git -C "$dir" commit -q -m "$pr_msg"

  git -C "$dir" checkout -q main
  printf 'main\n' >"$dir/main.txt"
  [ -n "$main_lib" ] && printf '%s' "$main_lib" >"$dir/crates/x/src/lib.rs"
  git -C "$dir" add -A
  git -C "$dir" commit -q -m "main moves on"

  git -C "$dir" merge -q --no-ff --no-commit pr >/dev/null 2>&1
  [ -n "$merge_lib" ] && printf '%s' "$merge_lib" >"$dir/crates/x/src/lib.rs"
  git -C "$dir" add -A
  git -C "$dir" commit -q -m "Merge pull request #1"
  printf '%s' "$dir"
}

lib_base=$'#[test]\nfn my_witness() { assert!(true); }\n'

# want <name> <expect-pass|expect-fail> <needle> <base> <head> <pr_body> [fixture-args...]
#
# An empty base runs the guard with no base or head argument, as ci.yml does.
# A pass with a needle must also print it. That tells a real comparison apart
# from the guard skipping the tree.
want() {
  local name="$1" expect="$2" needle="$3" dir="$4" base="$5" head="$6" body="$7"
  shift 7
  local out rc refs=()
  [ -n "$base" ] && refs=("$base" "$head")
  out="$(cd "$dir" && PR_BODY="$body" ./scripts/check-deleted-tests.sh "${refs[@]+"${refs[@]}"}" "$@" 2>&1)"
  rc=$?
  last_out="$out"
  if [ "$expect" = "expect-pass" ]; then
    if [ "$rc" -ne 0 ]; then
      fail=$((fail + 1)); echo "FAIL $name — expected OK, got exit $rc:"; echo "$out"
      return
    fi
    case "$out" in
    *"$needle"*) pass=$((pass + 1)); echo "ok   $name" ;;
    *) fail=$((fail + 1)); echo "FAIL $name: passed without printing '$needle':"; echo "$out" ;;
    esac
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

# lacks <name> <needle> checks that the last `want` run did not print the
# needle. A message that says the right thing on every run is no signal.
lacks() {
  case "$last_out" in
  *"$2"*) fail=$((fail + 1)); echo "FAIL $1: printed '$2':"; echo "$last_out" ;;
  *) pass=$((pass + 1)); echo "ok   $1" ;;
  esac
}
last_out=""

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

# ── D10: the commit-message channel expires, and the guard says so ──────────
#
# `#5894` is the recorded instance: one tree, two runs, opposite answers. The
# branch commit that named the deleted test passed while it was the tip, and
# failed once `gh pr update-branch` put a merge commit on top of it.
#
# Reproduced rather than argued. The fixture builds the same shape — a branch
# commit naming the test, a merge on top of it, then the pull request's own
# merge commit — and CLONES IT AT DEPTH 2, which is what ci.yml checks out. The
# naming commit is then two generations back and outside the clone, so the
# `git log` walk cannot read it and the acknowledgement is gone.
d10="$tmp/expiring_ack"
mkdir -p "$d10/scripts" "$d10/crates/x/src"
cp "$guard" "$d10/scripts/check-deleted-tests.sh"
cp "$collector" "$d10/scripts/collect-python-tests.py"
git -C "$d10" init -q -b main
git -C "$d10" config user.email t@t.invalid
git -C "$d10" config user.name t
git -C "$d10" config commit.gpgsign false
printf '#[test]\nfn my_witness() { assert!(true); }\n' >"$d10/crates/x/src/lib.rs"
git -C "$d10" add -A
git -C "$d10" commit -q -m base

git -C "$d10" checkout -q -b branch
printf 'pub fn noop() {}\n' >"$d10/crates/x/src/lib.rs"
git -C "$d10" add -A
git -C "$d10" commit -q -m "drop my_witness, folded into a table test"

git -C "$d10" checkout -q main
printf 'unrelated\n' >"$d10/other.txt"
git -C "$d10" add -A
git -C "$d10" commit -q -m "main moves on"

# The branch absorbs main — the `gh pr update-branch` step that ended the
# acknowledgement — and then the pull request's own merge commit is built.
git -C "$d10" checkout -q branch
git -C "$d10" merge -q --no-ff --no-edit main -m "Merge branch 'main' into branch" >/dev/null 2>&1
git -C "$d10" checkout -q main
git -C "$d10" merge -q --no-ff --no-edit branch -m "Merge pull request #1" >/dev/null 2>&1

shallow="$tmp/expiring_ack_shallow"
if git clone -q --depth 2 --no-local "file://$d10" "$shallow" 2>/dev/null; then
  out="$(cd "$shallow" && PR_BODY="" ./scripts/check-deleted-tests.sh 2>&1)"
  rc=$?
  if [ "$rc" -eq 0 ]; then
    fail=$((fail + 1))
    echo "FAIL D10 — the depth-2 checkout should not have found the acknowledgement:"
    echo "$out"
  else
    case "$out" in
    *"The commit-message channel expires"*)
      pass=$((pass + 1))
      echo "ok   D10 a commit-message acknowledgement outside the depth-2 walk fails, and the text says why"
      ;;
    *)
      fail=$((fail + 1))
      echo "FAIL D10 — the failure text does not say the commit-message channel expires:"
      echo "$out"
      ;;
    esac
  fi
else
  fail=$((fail + 1))
  echo "FAIL D10 — could not build the shallow clone the case needs"
fi

# ── D11: the same history at full depth still passes ────────────────────────
#
# The pair for D10: the guard itself did not stop reading commit messages, so a
# clone that can see the naming commit still accepts it. The expiry belongs to
# the checkout depth, which is what the failure text now tells the author.
out="$(cd "$d10" && PR_BODY="" ./scripts/check-deleted-tests.sh 2>&1)"
rc=$?
if [ "$rc" -eq 0 ]; then
  pass=$((pass + 1))
  echo "ok   D11 the same history at full depth still finds the naming commit"
else
  fail=$((fail + 1))
  echo "FAIL D11 — expected OK at full depth, got exit $rc:"
  echo "$out"
fi

# The cases below run the guard with no arguments on a merge commit, as ci.yml
# does. The guard then picks HEAD^1 as its base. D10 takes this path too, but
# it tests the commit-message channel, and at depth 2.
#
# D12 and D13 are the shape the guard exists for. main gains a test after the
# PR branched, and the merge result drops it. Only HEAD^1 holds that test. A
# guard that compared against the merge base or the PR head would see nothing
# lost.
lib_main_added="${lib_base}"$'#[test]\nfn added_on_main() { assert!(true); }\n'
d12="$(new_pr_merge dropped_on_merge "" "$lib_main_added" "$lib_base")"
want "D12 a merge that drops a test only main held fails, naming it" \
  expect-fail "added_on_main" "$d12" "" "" ""
want "D13 the same merge passes once the PR body names the test" \
  expect-pass "each named in the PR description" "$d12" "" "" \
  "dropped added_on_main with the feature it covered"

# D14 drops a test main gained as #[tokio::test(...)], with #[ignore] between
# the attribute and its fn. A guard that read only #[test] would pass it.
lib_main_async="${lib_base}"$'#[tokio::test(flavor = "multi_thread")]\n#[ignore = "slow"]\nasync fn added_async_on_main() {}\n'
d14="$(new_pr_merge dropped_async_on_merge "" "$lib_main_async" "$lib_base")"
want "D14 a dropped #[tokio::test] behind a second attribute fails too" \
  expect-fail "added_async_on_main" "$d14" "" "" ""

# D15 and D16 rename a test on the PR branch. The guard keys on the bare fn
# name, so it reports the old name as lost. Naming the old name passes it.
lib_renamed=$'#[test]\nfn witness_as_table_rows() { assert!(true); }\n'
d15="$(new_pr_merge renamed_on_branch "$lib_renamed" "" "")"
want "D15 a rename passes when the PR body names the old test" \
  expect-pass "each named in the PR description" "$d15" "" "" \
  "renamed my_witness to witness_as_table_rows"
want "D16 a rename fails when the PR body names only the new test" \
  expect-fail "my_witness" "$d15" "" "" "added witness_as_table_rows"

# D25 drops the same test only main held, but names it in the PR branch's own
# commit instead of the PR description. That commit is inside the guard's
# base_ref..head_ref walk, so this proves the commit-message channel works
# under the real merge topology, not only the linear fixture D7 and D21 use.
d25="$(new_pr_merge dropped_named_in_commit "" "$lib_main_added" "$lib_base" \
  "drop added_on_main with its feature")"
want "D25 a merge that drops a test passes when the PR's own commit names it" \
  expect-pass "only in a commit message" "$d25" "" "" ""

# D26 is the no-args merge topology touching no test at all: main and the PR
# both keep the original file untouched, so the merge carries every test
# through and the fast "nothing lost" path fires regardless of PR_BODY.
d26="$(new_pr_merge no_test_touched "" "" "")"
want "D26 a merge that drops no test passes with no PR body at all" \
  expect-pass "none lost by the merge" "$d26" "" "" ""

# ── D17/D18: Python tests count, the D1/D2 shape ─────────────────────────────
read -r d17_dir d17_base d17_head <<EOF
$(new_py_repo py_unnamed_fails)
EOF
want "D17 a deleted Python test that is not named in the PR fails, naming it" \
  expect-fail "test_my_witness" "$d17_dir" "$d17_base" "$d17_head" ""

read -r d18_dir d18_base d18_head <<EOF
$(new_py_repo py_named_passes)
EOF
want "D18 the same Python deletion passes once the PR body names it" \
  expect-pass "each named in the PR description" "$d18_dir" "$d18_base" "$d18_head" \
  "dropped test_my_witness, folded into a table test"

# ── D19: the Python side's own anti-vacuity check ────────────────────────────
#
# A test-named file the walk cannot read is not the same shape as a tree
# with no Python at all — D8's fixture has zero `.py` files anywhere, and
# that passes, quietly, the same way the Rust side treats a tree with no
# `#[test]` attribute as real but rare. A file called like a pytest test
# that yields zero names is instead treated as this collector's own glob or
# parser breaking, and must fail loudly rather than compare two empty sets.
d19="$(new_py_vacuous_repo py_vacuous)"
d19_sha="$(git -C "$d19" rev-parse HEAD)"
want "D19 a test-named Python file that will not parse fails loudly, not silently empty" \
  expect-fail "collector's own glob or parser breaking" "$d19" "$d19_sha" "$d19_sha" ""

# ── D20: a collector that runs clean and finds nothing is still caught ──────
#
# D19 breaks the FILE (a syntax error). D20 breaks the COLLECTOR instead: it
# runs, exits 0, and prints no names, the shape a broken `git ls-tree` glob
# already produced once on the real tree (this guard's own fix for it is the
# `_python_files_at` note in scripts/collect-python-tests.py). If the guard
# trusted the collector's own silence, this tree would read as clean. The
# independent `python_test_shaped_count_at` count is what catches it instead.
new_py_broken_collector_repo() {
  local name="$1" dir
  dir="$tmp/$name"
  rm -rf "$dir"
  mkdir -p "$dir/scripts" "$dir/tests"
  cp "$guard" "$dir/scripts/check-deleted-tests.sh"
  printf '#!/usr/bin/env python3\nimport sys\nsys.exit(0)\n' >"$dir/scripts/collect-python-tests.py"
  chmod +x "$dir/scripts/collect-python-tests.py"
  git -C "$dir" init -q
  git -C "$dir" config user.email t@t.invalid
  git -C "$dir" config user.name t
  printf 'def test_my_witness():\n    assert True\n' >"$dir/tests/test_thing.py"
  git -C "$dir" add -A
  git -C "$dir" commit -q -m base
  printf '%s' "$dir"
}
d20="$(new_py_broken_collector_repo py_broken_collector)"
d20_sha="$(git -C "$d20" rev-parse HEAD)"
want "D20 a collector that exits clean but finds nothing still fails loudly" \
  expect-fail "collector's own glob or parser breaking" "$d20" "$d20_sha" "$d20_sha" ""

# ── D21 to D24: the guard names the channel it read ─────────────────────────
#
# A local run reads every commit message. CI reads two commits. So an OK that
# came from a commit message can turn red in CI with the same tree. D21 and
# D22 check that the OK line says which channel held each name. A name found
# in the description is safe, so D22 must stay quiet about commits. The old
# OK line said "the PR description or a commit" whichever one held the name.
want "D21 a name found only in a commit message passes, and the OK line says so" \
  expect-pass "only in a commit message" "$d7_dir" "$d7_base" "$d7_head" ""

read -r d22_dir d22_base d22_head <<EOF
$(new_repo both_channels "drop my_witness, folded into a table test")
EOF
want "D22 a name in both the description and a commit counts as the description" \
  expect-pass "each named in the PR description" "$d22_dir" "$d22_base" "$d22_head" \
  "dropped my_witness, folded into a table test"
lacks "D22 the OK line does not warn about a commit message" "only in a commit message"
lacks "D22 the OK line does not blur the two channels" "or a commit"

# D23 and D24 check the failure text against the checkout it ran on. A full
# clone must not be told its walk stopped at depth 2. The depth-2 clone from
# D10 must be told that its checkout is shallow.
want "D23 a failure on a full clone says the walk read the whole branch" \
  expect-fail "This checkout holds full history" "$d2_dir" "$d2_base" "$d2_head" ""
lacks "D23 the full-clone failure does not call the checkout shallow" "This checkout is shallow"
lacks "D23 the remedy does not offer a commit message as the fix" "(or a commit message)"

if [ -d "$shallow" ]; then
  want "D24 a failure on the depth-2 clone says the checkout is shallow" \
    expect-fail "This checkout is shallow" "$shallow" "" "" ""
else
  fail=$((fail + 1))
  echo "FAIL D24: D10 did not build the shallow clone this case reads"
fi

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
