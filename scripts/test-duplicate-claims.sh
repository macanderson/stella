#!/usr/bin/env bash
#
# Tests for scripts/check-duplicate-claims.sh (#1722).
#
# Hermetic: every case drives the guard through --fixture, so nothing here
# needs a network, a token, or the repository's actual pull requests. That is
# the whole reason --fixture exists — a guard whose only input is `gh` can be
# proven to run, never to be RIGHT.
#
# The cases that matter are the failure branches. A guard that cannot be shown
# to fail is indistinguishable from one that always passes, which is the exact
# way #1714 left the empty-diff check believed-green for a week.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
guard="$repo_root/scripts/check-duplicate-claims.sh"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

pass=0
fail=0

# Run the guard over a fixture, asserting the exit code and that stdout+stderr
# contains `needle`.
expect() {
  name="$1"; want_code="$2"; needle="$3"; fixture="$4"; shift 4
  set +e
  out="$("$guard" --fixture "$fixture" "$@" 2>&1)"
  code=$?
  set -e
  if [ "$code" -ne "$want_code" ]; then
    echo "FAIL  $name — exit $code, wanted $want_code"
    printf '      %s\n' "$out"
    fail=$((fail + 1))
    return
  fi
  case "$out" in
    *"$needle"*) ;;
    *)
      echo "FAIL  $name — output did not contain: $needle"
      printf '      %s\n' "$out"
      fail=$((fail + 1))
      return
      ;;
  esac
  echo "ok    $name"
  pass=$((pass + 1))
}

# ── the incident this guard exists for ────────────────────────────────────
# #1616's real shape: three open PRs all claiming to close one issue.
cat > "$tmp/incident.json" <<'JSON'
[
  {"number": 1682, "title": "an opt-in deadline on a parked scope review", "body": "Implements the deadline.\n\nCloses #1616"},
  {"number": 1695, "title": "approval-wait deadline + panic-safe console drain", "body": "Same feature, other design.\n\nCloses #1616"},
  {"number": 1717, "title": "reconcile the two designs", "body": "Third pass.\n\nCloses #1616"}
]
JSON
expect "the #1616 incident is reported" 0 "claimed by 3 open pull requests" "$tmp/incident.json"
expect "and it fails under --strict" 1 "issue #1616" "$tmp/incident.json" --strict

# ── the ordinary case must stay quiet ─────────────────────────────────────
cat > "$tmp/clean.json" <<'JSON'
[
  {"number": 1, "title": "one", "body": "Closes #100"},
  {"number": 2, "title": "two", "body": "Closes #200"},
  {"number": 3, "title": "three", "body": "no trailer at all"}
]
JSON
expect "distinct claims pass" 0 "2 issue(s) claimed, none more than once" "$tmp/clean.json" --strict

# ── `Refs` is deliberately not a claim ────────────────────────────────────
# Two PRs advancing one issue from different directions is normal, and the
# repo's own convention says `Refs` does not close. Treating it as a duplicate
# would make the guard cry wolf on every epic.
cat > "$tmp/refs.json" <<'JSON'
[
  {"number": 1, "title": "part one", "body": "Refs #500"},
  {"number": 2, "title": "part two", "body": "Refs #500"}
]
JSON
expect "Refs is not a claim" 0 "no open pull request claims to close" "$tmp/refs.json" --strict

# ── keyword and case coverage ─────────────────────────────────────────────
# `Fixes`/`Resolves` close an issue exactly as `Closes` does, and GitHub is
# case-insensitive. A guard that only knew one spelling would miss the
# duplicate whenever the two authors happened to write different words — which
# is precisely the situation it exists for.
cat > "$tmp/keywords.json" <<'JSON'
[
  {"number": 1, "title": "a", "body": "fixes #77"},
  {"number": 2, "title": "b", "body": "RESOLVES #77"}
]
JSON
expect "Fixes and Resolves, any case, are the same claim" 1 "issue #77" "$tmp/keywords.json" --strict

# ── one PR naming the same issue twice is not a duplicate ─────────────────
# A body that says `Closes #9` in a summary and again in a trailer is one
# claim. Counting it twice would fail a perfectly ordinary PR.
cat > "$tmp/selfdupe.json" <<'JSON'
[
  {"number": 1, "title": "a", "body": "Closes #9 — and to be explicit, Closes #9"}
]
JSON
expect "one PR claiming an issue twice is one claim" 0 "none more than once" "$tmp/selfdupe.json" --strict

# ── degenerate inputs must not be reported as findings ────────────────────
printf '[]' > "$tmp/empty.json"
expect "no open PRs is not a finding" 0 "no open pull request claims" "$tmp/empty.json" --strict

cat > "$tmp/nullbody.json" <<'JSON'
[
  {"number": 1, "title": "a", "body": null},
  {"number": 2, "title": "b", "body": "Closes #4"}
]
JSON
expect "a null body is skipped, not crashed on" 0 "1 issue(s) claimed" "$tmp/nullbody.json" --strict

# ── unparseable input must never read as "no duplicates" ──────────────────
# The bug this guard shipped with for about ten minutes. `gh` styles even
# `--json` output when CLICOLOR_FORCE is exported, the ANSI escapes make the
# result invalid JSON, jq failed, and the failure was being swallowed into an
# empty result — so the guard reported OK against a repository whose open PRs
# all carried `Closes #N`. "I could not tell" and "there is nothing" must not
# be the same exit.
printf '\033[1;37m[\033[m\n  {"number": 1}\n' > "$tmp/ansi.json"
expect "ANSI-polluted JSON is an error, not an empty result" 2 "not a JSON array" "$tmp/ansi.json"

printf 'this is not json at all' > "$tmp/garbage.json"
expect "garbage input is an error too" 2 "not a JSON array" "$tmp/garbage.json" --strict

# ── a missing fixture is a usage error, never a silent pass ───────────────
set +e
out="$("$guard" --fixture "$tmp/does-not-exist.json" 2>&1)"
code=$?
set -e
if [ "$code" -eq 2 ]; then
  echo "ok    a missing fixture exits 2 rather than passing"
  pass=$((pass + 1))
else
  echo "FAIL  a missing fixture should exit 2, got $code: $out"
  fail=$((fail + 1))
fi

echo
echo "test-duplicate-claims: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
