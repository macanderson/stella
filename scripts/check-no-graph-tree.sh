#!/usr/bin/env bash
# Did the `graph` feature really drop anything? (`#6286`)
#
# `cargo check -p stella-tools --no-default-features` says the code builds
# with no code-graph index. It says nothing about what is still linked. A
# crate elsewhere in the tree can switch an `optional = true` back on, and the
# build looks the same and costs the same. So this reads the dependency tree
# instead.
#
# The tree-sitter grammars are what a small host is trying not to build. The
# feature-off tree must have none of them. The feature-on tree must have some,
# or the two trees look alike and this check has stopped asking anything.
#
# rusqlite is out of scope here. `stella-tools` always takes `stella-store`,
# and `stella-store` always takes rusqlite, so a small build still bundles
# SQLite. Splitting that seam is its own job, tracked in `#6423`.
set -euo pipefail

cd "$(dirname "$0")/.."

fail=0

with_graph=$(cargo tree -p stella-tools -e normal | grep -c 'tree-sitter' || true)
without_graph=$(cargo tree -p stella-tools --no-default-features -e normal | grep -c 'tree-sitter' || true)

if [ "$without_graph" -ne 0 ]; then
  echo "no-graph-tree: FAIL — --no-default-features still pulls ${without_graph} tree-sitter edge(s)"
  echo "  the graph feature is meant to drop them; run:"
  echo "    cargo tree -p stella-tools --no-default-features -e normal -i tree-sitter"
  fail=1
fi

if [ "$with_graph" -eq 0 ]; then
  echo "no-graph-tree: FAIL — the default build pulls no tree-sitter edge either"
  echo "  the check above cannot tell the two configurations apart."
  echo "  A grammar was renamed, or the default feature set lost 'graph'."
  fail=1
fi

if [ "$fail" -eq 0 ]; then
  echo "no-graph-tree: ok — ${with_graph} tree-sitter edges with the graph feature, 0 without"
fi

exit "$fail"
