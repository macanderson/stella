#!/usr/bin/env bash
#
# Smoke: unpack a published release artifact and RUN it. See #1626.
#
#   ./scripts/smoke-artifact.sh <artifact-dir>
#
# The release pipeline could go green, reproducible, signed, attested and
# pushed to the Homebrew tap without a single process having executed the
# binary it publishes. `verify-reproducible` proves the build is
# *deterministic*, which a binary that segfaults on startup satisfies
# perfectly; `ci.yml` never runs on tags at all. The first time a published
# artifact was executed by anyone was by hand, on 2026-08-05, eight months
# into the project.
#
# <artifact-dir> is the directory an `actions/download-artifact` of one build
# matrix leg produces: exactly one `stella-<version>-<target>.tar.gz` and the
# `stella-<version>-<target>.sha256` beside it. Taking the *uploaded* artifact
# rather than a path in the build tree is the point — it is what a user
# receives, so this also covers the packaging step and not just the compiler.
#
# What it asserts, in order, cheapest first:
#
#   1. the artifact directory holds exactly one tarball and one checksum,
#   2. the tarball unpacks and contains an executable `stella`,
#   3. that binary hashes to the checksum published in SHA256SUMS.bin — the
#      number install.sh and the Homebrew formula verify against. Without this
#      the two halves of a release can describe different files.
#   4. the binary reports the version its artifact is *named* for,
#   5. `stella models` renders. This is `make smoke`'s command, chosen because
#      it needs no API key: stella is BYOK and the release workflow holds no
#      provider credentials.
#
# Deliberately a smoke gate, not a second CI run: macOS runners are the
# expensive ones and a full suite per release is not a cost this repo should
# carry. Two invocations.
#
# scripts/test-smoke-artifact.sh is its witness — it drives this script
# against synthetic good and broken artifacts, so each failure branch above is
# proven to fire rather than assumed to.
set -euo pipefail

die() {
  echo "::error::$*" >&2
  exit 1
}

dir="${1:-}"
[ -n "$dir" ] || die "usage: $0 <artifact-dir>"
[ -d "$dir" ] || die "artifact directory does not exist: ${dir}"

cd "$dir"

shopt -s nullglob
tarballs=(./*.tar.gz)
sums=(./*.sha256)
shopt -u nullglob

[ "${#tarballs[@]}" -eq 1 ] \
  || die "expected exactly one *.tar.gz in ${dir}, found ${#tarballs[@]} — the build job uploads one tarball per target."
[ "${#sums[@]}" -eq 1 ] \
  || die "expected exactly one *.sha256 in ${dir}, found ${#sums[@]} — repro-build.sh writes one checksum per target."

tarball="${tarballs[0]}"
sumfile="${sums[0]}"
stem="$(basename "${tarball%.tar.gz}")"

echo "artifact: ${stem}.tar.gz"

# A fresh directory, so a stale extraction from an earlier leg cannot be what
# gets executed.
workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT INT TERM
tar -xzf "$tarball" -C "$workdir" \
  || die "the published tarball ${stem}.tar.gz does not unpack."

bin="${workdir}/${stem}/stella"
[ -f "$bin" ] || die "the tarball contains no ${stem}/stella — packaging shipped an archive with no binary in it."
[ -x "$bin" ] || die "${stem}/stella is not executable — the archive lost its mode bits."

# `sha256sum` is GNU-only; macOS runners have `shasum`. Both print
# "<hash>  <name>", so the first field is the comparable half. The name column
# in the published file is the artifact *stem*, not the extracted path, which
# is why this compares hashes rather than running `sha256sum -c`.
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$bin" | cut -d' ' -f1)"
else
  actual="$(shasum -a 256 "$bin" | cut -d' ' -f1)"
fi
published="$(cut -d' ' -f1 <"$sumfile")"
[ -n "$published" ] || die "${stem}.sha256 is empty — nothing to verify the binary against."

if [ "$actual" != "$published" ]; then
  echo "published (SHA256SUMS.bin): ${published}" >&2
  echo "actual    (in the tarball): ${actual}" >&2
  die "the binary inside the published tarball is NOT the binary whose checksum ships beside it. install.sh and the Homebrew formula verify the published number, so users would be checking a hash that describes a different file."
fi
echo "checksum:  ${actual} (matches the published SHA256SUMS.bin entry)"

# ── Run it ───────────────────────────────────────────────────────────────────
# Everything above this line is still only inspecting bytes. A binary that
# cannot start satisfies all of it.

version_line="$("$bin" --version)" \
  || die "\`stella --version\` failed on the published binary — the artifact does not start."
echo "--version: ${version_line}"

# The version the artifact is NAMED for, derived from the stem rather than
# passed in, so this holds identically on a tag push and a workflow_dispatch.
# A prerelease stem (0.1.0-rc1-<target>) does not match and is skipped rather
# than mis-parsed — the assertion is worth having for the common case and not
# worth a wrong verdict for the rare one.
named_version="$(printf '%s' "$stem" | sed -n -E 's/^stella-([0-9]+\.[0-9]+\.[0-9]+)-.+$/\1/p')"
if [ -n "$named_version" ]; then
  case "$version_line" in
    *"$named_version"*) echo "version:   ${named_version} (matches the artifact name)" ;;
    *) die "the artifact is named for ${named_version} but the binary reports '${version_line}' — the tarball was assembled from a different build." ;;
  esac
fi

# `stella models` is `make smoke`'s command: it renders the provider table and
# needs no credentials.
"$bin" models >/dev/null \
  || die "\`stella models\` failed on the published binary — it starts but cannot reach a working command."
echo "models:    ok"

echo "smoke: ${stem} unpacked, verified against its published checksum, and ran."
