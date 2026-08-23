#!/usr/bin/env bash
#
# Tests that scripts/check-wire-paths.sh can still FAIL, and that
# scripts/wire-schema-paths.sh refuses what it cannot translate (#3836).
#
#   ./scripts/test-wire-paths.sh
#
# The guard's whole value is that a hand-written copy of wire-schema.yml's
# `paths:` filter cannot come back into `.githooks/pre-push` unnoticed, and that
# a `paths:` entry the parse does not understand stops the parse instead of
# silently narrowing the hook below the workflow. Both are properties of a
# grep and a translation table, and both go quietly permissive: widen either by
# one case and the guard reports green on the divergence it exists to catch.
#
# Hermetic: every case builds a throwaway tree with its own workflow and its own
# hook, copies the real scripts into it, and runs them there. Nothing reads or
# writes this repository. That is why this is not a `make gate` step, the same
# posture as scripts/test-file-size.sh.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"

pass=0
fail=0

tmp="$(mktemp -d "${TMPDIR:-/tmp}/stella-wire-paths.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT INT TERM

# fixture <name> [paths...] — a tree the guard passes on, watching the paths
# given (defaulting to the shape the real workflow uses).
fixture() {
  local dir="$tmp/$1"
  shift
  rm -rf "$dir"
  mkdir -p "$dir/scripts" "$dir/.githooks" "$dir/docs/wire" "$dir/crates/stella-protocol"
  cp "$repo_root/scripts/wire-schema-paths.sh" "$dir/scripts/"
  cp "$repo_root/scripts/check-wire-paths.sh" "$dir/scripts/"
  : >"$dir/docs/wire/agentevent.schema.json"
  mkdir -p "$dir/crates/stella-protocol/src"
  : >"$dir/crates/stella-protocol/src/event.rs"
  : >"$dir/scripts/check-wire-schema.sh"

  # Every fixture path is a literal, and several are globs on purpose (`**`,
  # and W7's mid-path `*`). Pathname expansion is off for the duration or the
  # shell resolves them against THIS repository — which it did, silently, and
  # the fixture then described a workflow nobody wrote.
  if [ "$#" -eq 0 ]; then
    set -- 'docs/wire/**' 'crates/stella-protocol/**' 'scripts/check-wire-schema.sh'
  fi

  {
    echo "name: wire-schema"
    echo "on:"
    echo "  pull_request:"
    echo "    paths:"
    set -f
    for p in "$@"; do echo "      - \"$p\""; done
    set +f
  } >"$dir/.github-workflow.tmp"
  mkdir -p "$dir/.github/workflows"
  mv "$dir/.github-workflow.tmp" "$dir/.github/workflows/wire-schema.yml"

  cat >"$dir/.githooks/pre-push" <<'EOF'
#!/usr/bin/env bash
# The filter is read, never restated — see docs/wire/ and #3836.
wire_touched=0
if ! wire_filter="$(./scripts/wire-schema-paths.sh filter 2>/dev/null)" || [ -z "$wire_filter" ]; then
  wire_touched=1
elif grep -Eq "$wire_filter" "$changed"; then
  wire_touched=1
fi
EOF

  printf '%s' "$dir"
}

# expect <name> <wanted-exit> <dir> [needle]
expect() {
  local name="$1" want="$2" dir="$3" needle="${4:-}"
  local out rc
  out="$(cd "$dir" && ./scripts/check-wire-paths.sh 2>&1)"
  rc=$?
  if [ "$rc" -ne "$want" ]; then
    echo "FAIL  $name — exit $rc, wanted $want"
    printf '%s\n' "$out" | sed 's/^/      /'
    fail=$((fail + 1))
    return
  fi
  if [ -n "$needle" ] && ! printf '%s' "$out" | grep -qF -- "$needle"; then
    echo "FAIL  $name — report never says '$needle'"
    printf '%s\n' "$out" | sed 's/^/      /'
    fail=$((fail + 1))
    return
  fi
  echo "ok    $name"
  pass=$((pass + 1))
}

# ── W1  the fixture is green ─────────────────────────────────────────────────
d="$(fixture baseline)"
expect "W1  a hook that derives its filter passes" 0 "$d"

# ── W2  the derivation itself ────────────────────────────────────────────────
d="$(fixture derives)"
got="$(cd "$d" && ./scripts/wire-schema-paths.sh filter)"
want='^(docs/wire/|crates/stella-protocol/|scripts/check-wire-schema\.sh$)'
if [ "$got" = "$want" ]; then
  echo "ok    W2  a subtree becomes a prefix and a file an anchored match"
  pass=$((pass + 1))
else
  echo "FAIL  W2  filter is '$got', wanted '$want'"
  fail=$((fail + 1))
fi

# ── W3  a hand-written copy coming back ──────────────────────────────────────
d="$(fixture handwritten)"
cat >"$d/.githooks/pre-push" <<'EOF'
#!/usr/bin/env bash
wire_touched=0
if grep -Eq '^(docs/wire/|crates/stella-protocol/)' "$changed"; then
  wire_touched=1
fi
EOF
expect "W3  a hand-written grep in the hook fails" 1 "$d" "greps for a wire path itself"

# ── W4  the hook that stops reading the deriver at all ───────────────────────
d="$(fixture no_call)"
cat >"$d/.githooks/pre-push" <<'EOF'
#!/usr/bin/env bash
wire_touched=1
EOF
expect "W4  a hook that never calls the deriver fails" 1 "$d" "does not read the path filter"

# ── W5  the literal filter pasted in ─────────────────────────────────────────
d="$(fixture literal)"
{
  echo '# a copy, in full:'
  echo 'literal='"'"'^(docs/wire/|crates/stella-protocol/|scripts/check-wire-schema\.sh$)'"'"
} >>"$d/.githooks/pre-push"
expect "W5  the filter pasted into the hook fails" 1 "$d" "literally"

# ── W6  a watched path that does not exist ───────────────────────────────────
d="$(fixture ghost_path 'docs/wire/**' 'crates/stella-vanished/**')"
expect "W6  a filter arm over a missing path fails" 1 "$d" "which does not exist"

# ── W7  a glob shape the parse cannot translate ──────────────────────────────
#
# The case that matters most: GitHub's glob syntax is richer than grep's, and
# approximating one in the other is how the hook silently stops matching what
# the workflow matches.
d="$(fixture untranslatable 'docs/wire/**' 'crates/*/src/wire.rs')"
expect "W7  a mid-path glob stops the parse" 1 "$d" "cannot translate"

# ── W8  no paths: block at all ───────────────────────────────────────────────
d="$(fixture no_paths)"
grep -v '^      - ' "$d/.github/workflows/wire-schema.yml" >"$d/wf.tmp"
grep -v '^    paths:' "$d/wf.tmp" >"$d/.github/workflows/wire-schema.yml"
expect "W8  a workflow with no paths: filter fails" 1 "$d" "found no \`paths:\` entries"

# ── W9  paths-ignore is never read as a match list ───────────────────────────
#
# The inverse question. A parse that read an ignore list as a filter would
# select exactly the paths the workflow excludes — green, and backwards.
d="$(fixture ignore_list)"
{
  echo "  push:"
  echo "    paths-ignore:"
  echo '      - "website/**"'
} >>"$d/.github/workflows/wire-schema.yml"
got="$(cd "$d" && ./scripts/wire-schema-paths.sh list)"
if printf '%s\n' "$got" | grep -q 'website'; then
  echo "FAIL  W9  paths-ignore leaked into the match list"
  fail=$((fail + 1))
else
  echo "ok    W9  paths-ignore is not read as a match list"
  pass=$((pass + 1))
fi

# ── W10  a truncated reader cannot change a red verdict ──────────────────────
d="$(fixture sigpipe)"
cat >"$d/.githooks/pre-push" <<'EOF'
#!/usr/bin/env bash
wire_touched=1
EOF
sig_fail=0
for _ in 1 2 3 4 5; do
  (cd "$d" && ./scripts/check-wire-paths.sh 2>&1 | head -1 >/dev/null)
  [ "${PIPESTATUS[0]}" -eq 1 ] || sig_fail=1
done
if [ "$sig_fail" -eq 0 ]; then
  echo "ok    W10 a truncated reader still gets exit 1"
  pass=$((pass + 1))
else
  echo "FAIL  W10 a truncated reader changed the verdict"
  fail=$((fail + 1))
fi

echo
echo "test-wire-paths: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
