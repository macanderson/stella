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

shopt -s nullglob
workflow_files=("$workflows"/*.yml "$workflows"/*.yaml)
shopt -u nullglob
if [ "${#workflow_files[@]}" -eq 0 ]; then
  echo "check-cargo-install-pins: no workflow files in $workflows; skipping."
  exit 0
fi

# A `cargo install` counts wherever a shell runs it: a one-line `run:` step or
# any line of a `run: |` block scalar. Step names and comments only talk about
# the command, so they are not matched.
install_lines="$(awk '
  FNR == 1 { inblock = 0 }
  {
    if ($0 ~ /^[[:space:]]*$/) next
    indent = match($0, /[^[:space:]]/) - 1
    if (inblock && indent <= base) inblock = 0
    if (!inblock) {
      if ($0 !~ /^[[:space:]]*(-[[:space:]]+)?run:/) next
      if ($0 ~ /^[[:space:]]*(-[[:space:]]+)?run:[[:space:]]*[|>]/) {
        base = indent
        inblock = 1
        next
      }
    } else if ($0 ~ /^[[:space:]]*#/) next
    at = index($0, "cargo install")
    if (at == 0) next
    printf "%s:%d:%s\n", FILENAME, FNR, substr($0, at + length("cargo install"))
  }
' "${workflow_files[@]}")"
if [ -z "$install_lines" ]; then
  echo "check-cargo-install-pins: no 'cargo install' lines found; skipping."
  exit 0
fi

fail=0

# The verdict is decided before anything is written (#1815). Failure lines are
# buffered while the scan runs and emitted in one final write: a guard that
# prints as it scans dies mid-report when its reader exits early, and under
# `set -euo pipefail` whatever partial state it had reached becomes the exit
# status. scripts/check-file-size.sh is the shape being copied.
report=""
note() { report="${report}$1"$'\n'; }

# Emission is best-effort: the verdict is already decided, so a reader that
# closed the pipe (`| head -1`, `| true`) must be able to change neither the
# report nor the exit code. SIGPIPE is ignored so a failed write surfaces as a
# discarded error instead of killing the script (#1815).
emit() {
  trap '' PIPE
  printf '%s' "$report" >&2 || true
}

while IFS= read -r line; do
  [ -z "$line" ] && continue
  file_and_rest="${line%%:*}"
  rest="${line#*:}"
  lineno="${rest%%:*}"
  # Everything after `cargo install`, minus flags (anything starting with -)
  # and the trailing backslash of a wrapped shell line.
  args="${rest#*:}"
  for tok in $args; do
    case "$tok" in
      -*) continue ;;
      \\) continue ;;
      *@*) continue ;;
      *)
        note "check-cargo-install-pins: $file_and_rest:$lineno installs '$tok' with no @version pin."
        fail=1
        ;;
    esac
  done
done <<EOF
$install_lines
EOF

if [ "$fail" -ne 0 ]; then
  note ""
  note "Pin each crate to an exact version, e.g.:"
  note ""
  note "    run: cargo install --locked cargo-deny@0.20.2"
  emit
  exit 1
fi

emit
echo "check-cargo-install-pins: OK — every 'cargo install' crate is pinned to a version." || true
