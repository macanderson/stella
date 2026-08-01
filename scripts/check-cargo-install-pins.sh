#!/usr/bin/env bash
#
# Guard: every `cargo install` in .github/workflows/ names an exact version
# for each crate it installs (#915).
#
# `--locked` pins a tool's own transitive dependency graph; it does not pin
# the tool itself. An unpinned `cargo install cargo-deny` in the
# supply-chain job resolved whatever that crate's newest published version
# was at run time — the one job whose purpose is keeping untrusted code out
# was also the only one fetching an unversioned executable from the network.
#
# Uses portable POSIX tools so it runs on a bare CI runner (no ripgrep).
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

workflows=".github/workflows"
if [ ! -d "$workflows" ]; then
  echo "check-cargo-install-pins: no $workflows directory; skipping."
  exit 0
fi

install_lines="$(grep -rnE '^[[:space:]]*run:[[:space:]]*cargo install\b' "$workflows" || true)"
if [ -z "$install_lines" ]; then
  echo "check-cargo-install-pins: no 'cargo install' lines found; skipping."
  exit 0
fi

fail=0
while IFS= read -r line; do
  [ -z "$line" ] && continue
  file_and_rest="${line%%:*}"
  rest="${line#*:}"
  lineno="${rest%%:*}"
  # Everything after `cargo install`, minus flags (anything starting with -).
  args="$(printf '%s\n' "$line" | sed -E 's/^[^:]+:[0-9]+:[[:space:]]*run:[[:space:]]*cargo install[[:space:]]*//')"
  for tok in $args; do
    case "$tok" in
      -*) continue ;;
      *@*) continue ;;
      *)
        echo "check-cargo-install-pins: $file_and_rest:$lineno installs '$tok' with no @version pin." >&2
        fail=1
        ;;
    esac
  done
done <<EOF
$install_lines
EOF

if [ "$fail" -ne 0 ]; then
  echo >&2
  echo "Pin each crate to an exact version, e.g.:" >&2
  echo >&2
  echo "    run: cargo install --locked cargo-deny@0.20.2" >&2
  exit 1
fi

echo "check-cargo-install-pins: OK — every 'cargo install' crate is pinned to a version."
