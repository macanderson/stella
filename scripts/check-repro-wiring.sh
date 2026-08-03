#!/usr/bin/env bash
#
# Guard: every release build goes through scripts/repro-build.sh, and a
# divergence between two independent rebuilds blocks publication (#910).
#
# Reproducibility is not a property of a script; it is a property of the path
# the shipped bytes actually took. The remapping in scripts/repro-build.sh is
# worth nothing the moment a `cargo build --release` reappears in release.yml —
# and that regression is invisible, because the resulting release looks exactly
# like a good one: it builds, it packages, it publishes, it attests. The only
# symptom is that a user who rebuilds the tag gets different bytes, which
# nobody notices until they try.
#
# So this asserts the wiring, and the `verify-reproducible` job in release.yml
# asserts the bytes. Neither substitutes for the other: this one runs in
# seconds on every PR with no toolchain, that one needs two runners and an hour
# of LTO and only runs on a tag.
#
# Uses portable POSIX tools so it runs on a bare CI runner (no ripgrep, no yq).
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

workflow=".github/workflows/release.yml"
local_release="scripts/release.sh"
builder="scripts/repro-build.sh"
packer="scripts/package-tarball.py"

fail=0
note() {
  echo "check-repro-wiring: $1" >&2
  fail=1
}

for f in "$workflow" "$local_release" "$builder" "$packer"; do
  if [ ! -f "$f" ]; then
    note "$f not found."
  fi
done
if [ "$fail" -ne 0 ]; then
  exit 1
fi

for f in "$builder" "$packer"; do
  [ -x "$f" ] || note "$f is not executable — the workflows invoke it directly."
done

# 1. No release build may bypass the builder. `cargo zigbuild` is matched too:
#    the local path cross-compiles the Linux targets with it, and it inherits
#    RUSTFLAGS the same way, so it must route through the builder as well.
#    bench/evidence/run/build_sut.sh is deliberately NOT covered — the frozen
#    benchmark SUT's recorded SHA depends on it staying un-remapped.
for f in "$workflow" "$local_release"; do
  # Blank out whole-line comments first: the prose in these files is allowed to
  # name the command it replaced, and a guard that trips on its own
  # explanation gets deleted rather than fixed.
  bare="$(sed 's/^[[:space:]]*#.*$//' "$f" | grep -nE 'cargo (zig)?build .*--release' || true)"
  if [ -n "$bare" ]; then
    note "$f builds a release binary directly instead of through ${builder}:"
    printf '%s\n' "$bare" >&2
  fi
done

# 2. Both release paths call the builder, and CI keeps `--locked` (#786: the
#    tagged tree's lockfile is synced to its own version, and the published
#    binary must be built from it).
grep -q "${builder} --locked" "$workflow" \
  || note "$workflow does not invoke '${builder} --locked'."
grep -q "$builder" "$local_release" \
  || note "$local_release does not invoke ${builder}."

# 3. The `release` job must wait on `verify-reproducible`, or a build whose
#    bytes could not be reproduced still gets published. Tracks the enclosing
#    top-level job rather than grepping for the name anywhere in the file.
release_needs="$(
  awk '
    /^[[:space:]][[:space:]][a-z][a-z0-9-]*:[[:space:]]*$/ {
      job = $1; sub(/:$/, "", job); next
    }
    job == "release" && /^[[:space:]]+needs:/ { print; exit }
  ' "$workflow"
)"
case "$release_needs" in
  *verify-reproducible*) : ;;
  "") note "$workflow has no 'release' job with a needs: line." ;;
  *) note "$workflow's release job does not need verify-reproducible (needs:${release_needs#*needs:})." ;;
esac

# 4. The bare-binary checksums have to reach the release, or an independent
#    rebuilder has no published number to compare their bytes against and the
#    whole fix is invisible from outside. Three distinct sites, checked as
#    three distinct things — a bare occurrence count passes on a workflow that
#    only *talks about* SHA256SUMS.bin in a comment.
grep -qE '>[[:space:]]*SHA256SUMS\.bin' "$workflow" \
  || note "$workflow never writes SHA256SUMS.bin — the per-target binary checksums are not generated."

# Once in the attest step's subject-path, once in the release's files.
published="$(grep -c 'artifacts/SHA256SUMS\.bin' "$workflow" || true)"
if [ "${published:-0}" -lt 2 ]; then
  note "$workflow references artifacts/SHA256SUMS.bin ${published:-0} time(s); expected it both attested and attached to the release."
fi

if [ "$fail" -ne 0 ]; then
  echo >&2
  echo "Release builds must go through ${builder} (see #910). It is the single" >&2
  echo "place that remaps \$CARGO_HOME and the rustup sysroot out of the binary," >&2
  echo "asserts the rust-toolchain.toml pin, and emits the per-target checksum." >&2
  exit 1
fi

echo "check-repro-wiring: OK — both release paths build through ${builder}, and publication waits on verify-reproducible."
