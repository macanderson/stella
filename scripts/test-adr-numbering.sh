#!/usr/bin/env bash
# Hermetic self-test for scripts/check-adr-numbering.py.
#
# A guard that cannot fail is not a guard. This drives the checker over fixture
# directories it builds in a temp dir -- never `docs/adr/` -- and asserts it
# passes a clean tree and fails each violation it exists to catch, by name.
#
# Runs in `guard-self-tests.yml` beside the other `scripts/test-*.sh`. It is
# not a `make gate` step, because it proves the guard rather than the tree.
set -euo pipefail

GUARD="$(cd "$(dirname "$0")" && pwd)/check-adr-numbering.py"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

passed=0
failed=0

ok() { printf 'ok   %s\n' "$1"; passed=$((passed + 1)); }
no() { printf 'FAIL %s\n' "$1"; failed=$((failed + 1)); }

# Build a fixture directory: $1 = name, then the caller writes into $tmp/$1.
fixture() {
  local dir="$tmp/$1"
  mkdir -p "$dir"
  printf '# ADRs\n\n' > "$dir/README.md"
  echo "$dir"
}

# A well-formed record. $1=dir $2=NNNN $3=slug
record() {
  local dir="$1" n="$2" slug="$3"
  cat > "$dir/$n-$slug.md" <<EOF
---
id: adr/$n-$slug
title: "ADR $n: $slug"
status: accepted
---

# ADR $n: $slug
EOF
  printf '| [%s](%s-%s.md) | %s | Accepted |\n' "$n" "$n" "$slug" "$slug" >> "$dir/README.md"
}

expect_pass() {
  if python3 "$GUARD" "$1" >/dev/null 2>&1; then ok "$2"; else no "$2 (expected pass, got fail)"; fi
}

expect_fail() {
  local dir="$1" name="$2" needle="$3" out
  if out="$(python3 "$GUARD" "$dir" 2>&1)"; then
    no "$name (expected fail, got pass)"
  elif printf '%s' "$out" | grep -qF "$needle"; then
    ok "$name"
  else
    no "$name (failed, but never mentioned '$needle')"
  fi
}

# A1 — a clean directory passes.
d="$(fixture clean)"; record "$d" 0001 alpha; record "$d" 0002 beta
expect_pass "$d" "A1 a clean directory passes"

# A2 — the collision this guard was written for (#5175).
d="$(fixture dupe)"; record "$d" 0001 alpha; record "$d" 0001 beta
expect_fail "$d" "A2 two records sharing a number fail" "identifies 2 records"

# A3 — a half-finished renumber: filename moved, heading did not (#5165).
d="$(fixture heading)"; record "$d" 0001 alpha
sed -i.bak 's/^# ADR 0001:/# ADR 0009:/' "$d/0001-alpha.md" && rm -f "$d"/*.bak
expect_fail "$d" "A3 a heading disagreeing with the filename fails" "half-finished renumber"

# A4 — frontmatter id left behind by the same kind of rename.
d="$(fixture ident)"; record "$d" 0001 alpha
sed -i.bak 's|^id: adr/0001-alpha|id: adr/0009-alpha|' "$d/0001-alpha.md" && rm -f "$d"/*.bak
expect_fail "$d" "A4 an id disagreeing with the filename fails" "filename stem is"

# A5 — a record the index never lists.
d="$(fixture unindexed)"; record "$d" 0001 alpha
printf '# ADRs\n\n' > "$d/README.md"
expect_fail "$d" "A5 an unindexed record fails" "not listed in"

# A6 — MADR's `# 16.` heading style is accepted, not a false positive.
d="$(fixture madr)"
cat > "$d/0016-gamma.md" <<'EOF'
# 16. Gamma
EOF
printf '| [0016](0016-gamma.md) | Gamma | Accepted |\n' >> "$d/README.md"
expect_pass "$d" "A6 a MADR-style heading is accepted"

# A7 — a bare (unprefixed) id is accepted, matching the older convention.
d="$(fixture bareid)"
cat > "$d/0001-alpha.md" <<'EOF'
---
id: 0001-alpha
title: "ADR 0001: Alpha"
---

# ADR 0001: Alpha
EOF
printf '| [0001](0001-alpha.md) | Alpha | Accepted |\n' >> "$d/README.md"
expect_pass "$d" "A7 a bare id is accepted, not only the adr/ prefixed form"

# A8 — a record with no frontmatter at all is accepted (seven legacy ones).
d="$(fixture nofrontmatter)"
printf '# ADR 0005: Epsilon\n' > "$d/0005-epsilon.md"
printf '| [0005](0005-epsilon.md) | Epsilon | Accepted |\n' >> "$d/README.md"
expect_pass "$d" "A8 a record with no frontmatter is accepted"

printf '\npassed %d, failed %d\n' "$passed" "$failed"
[ "$failed" -eq 0 ]
