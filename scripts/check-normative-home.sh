#!/usr/bin/env bash
#
# Drift guard for the Context Graph Protocol (CGP) normative-home boundary.
# See context-graph-protocol#27 and CGP docs/adr/0007-protocol-product-boundary.md.
#
# The adaptive-context docs no longer restate frame/wire semantics — those live
# normatively in CGP. Each doc that references CGP as its normative home carries
#
#     <!-- NORMATIVE-HOME: macanderson/context-graph-protocol @ v<X.Y.Z> (...) -->
#
# This check keeps that pointer honest: the pinned release must be the
# contextgraph-* version the workspace actually builds against — the exact
# `="X.Y.Z"` requirement in the root Cargo.toml's [workspace.dependencies]
# (registry deps since #819; the git-rev era pinned a sha instead, and pinning
# by sha is what let the docs and the build drift apart unnoticed). If someone
# bumps the dependency without repinning the docs — or repins the docs without
# bumping the dependency — this fails loudly.
#
# HARD-FAIL, NEVER SKIP: the git-rev version of this guard exited 0 when it
# could not find a rev to compare against, and #819's move to registry deps
# turned that "skip" into the permanent state — the guard ran green in CI for
# every PR while checking nothing. If the anchor this script reads goes
# missing, that is a failure of the guard's premise and must break the build,
# not excuse it.
#
# Discovery matches the HTML-comment form `<!-- NORMATIVE-HOME:`, NOT a bare
# mention of the marker. Prose that *describes* the convention — docs/README.md
# explains it, in backticks — carries no `@ v<X.Y.Z>` and is not meant to: it
# is documentation of the mechanism, not a pinned doc. Matching the bare string
# made the README a checked file that could never pass, so the guard failed on
# every PR touching docs/**. Keep the `<!--` anchor.
#
# Uses portable POSIX grep/sed so it runs on a bare CI runner.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

# The version stella builds against: every contextgraph-* entry in the root
# manifest's [workspace.dependencies]. They are required to agree — a split
# pin would make "the CGP revision stella builds against" ambiguous.
cargo_versions="$(grep -E '^contextgraph-(types|host|trace|conformance)[[:space:]]*=' Cargo.toml \
  | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | sort -u)"
if [ -z "${cargo_versions:-}" ]; then
  echo "FAIL: no contextgraph-* version requirement found in the root Cargo.toml." >&2
  echo "      This guard compares NORMATIVE-HOME doc pins against that version;" >&2
  echo "      if the dependency moved, update this script in the same PR." >&2
  exit 1
fi
if [ "$(printf '%s\n' "$cargo_versions" | wc -l)" -ne 1 ]; then
  echo "FAIL: the contextgraph-* crates in Cargo.toml pin different versions:" >&2
  printf '      %s\n' "$cargo_versions" >&2
  echo "      They must agree for 'the CGP revision stella builds against' to mean anything." >&2
  exit 1
fi
cargo_version="$cargo_versions"

status=0
count=0
while IFS= read -r file; do
  [ -n "$file" ] || continue
  count=$((count + 1))
  marker_line="$(grep -E '<!--[[:space:]]*NORMATIVE-HOME:' "$file" | head -n1)"
  pin="$(printf '%s\n' "$marker_line" \
    | grep -oE '@[[:space:]]*v?[0-9]+\.[0-9]+\.[0-9]+' \
    | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -n1 || true)"
  if [ -z "$pin" ]; then
    legacy_sha="$(printf '%s\n' "$marker_line" \
      | grep -oE '@[[:space:]]*[0-9a-f]{7,40}' \
      | grep -oE '[0-9a-f]{7,40}' | head -n1 || true)"
    if [ -n "$legacy_sha" ]; then
      echo "FAIL $file: pins CGP by git sha ($legacy_sha), but since #819 the" >&2
      echo "     workspace consumes CGP from crates.io. Repin as '@ v$cargo_version'" >&2
      echo "     (keep the sha as prose provenance if the body was vendored at it)." >&2
    else
      echo "FAIL $file: NORMATIVE-HOME header present but no '@ v<X.Y.Z>' pin found." >&2
    fi
    status=1
    continue
  fi
  if [ "$pin" = "$cargo_version" ]; then
    echo "ok   $file  (pin v$pin matches Cargo.toml's =$cargo_version)"
  else
    echo "FAIL $file: pins CGP @ v$pin but stella builds against =$cargo_version." >&2
    echo "     Repin the doc and re-vendor its body in the same PR (see the" >&2
    echo "     re-sync instructions in the doc's header comment)." >&2
    status=1
  fi
done <<EOF
$(grep -rlE '<!--[[:space:]]*NORMATIVE-HOME:' docs 2>/dev/null || true)
EOF

if [ "$count" -eq 0 ]; then
  echo "FAIL: no NORMATIVE-HOME docs found under docs/." >&2
  echo "      Three docs are expected to carry the header (see docs/README.md);" >&2
  echo "      if the convention was retired, delete this script and its workflow" >&2
  echo "      step in the same PR." >&2
  exit 1
fi
exit "$status"
