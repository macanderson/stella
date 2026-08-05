#!/usr/bin/env bash
#
# repro-build.sh — build the shipping `stella` binary reproducibly (#910).
#
# Every release artifact is built through here, by both .github/workflows/release.yml
# and the degraded local path (scripts/release.sh), so there is exactly one
# definition of "the bytes we publish".
#
# WHAT WAS WRONG
#
# `strings` on a release binary found 553 absolute host paths — 506 under
# `$CARGO_HOME/registry/src/...` and 47 under
# `$RUSTUP_HOME/toolchains/<pin>/lib/rustlib/src/rust/...` — so the published
# SHA-256 was a property of the builder's home directory, not of the source. A
# user who cloned the tag and built it could not get our bytes, which meant the
# only thing linking the published binary to the published source was our
# assertion that they matched (bench/READINESS.md recorded exactly this).
#
# WHAT IS AND IS NOT REMAPPED
#
#   - `$CARGO_HOME` -> `/cargo`. The bulk of the leak: every panic location and
#     `file!()` in a registry dependency.
#   - `<sysroot>/lib/rustlib/src/rust` -> `/rustc/<commit-hash>`. This is the
#     subtle one. rustc emits std's paths as the virtual `/rustc/<hash>/...`,
#     but when the `rust-src` component is installed it translates them back to
#     the real sysroot path instead — so the same source and the same pinned
#     toolchain produce different bytes depending on whether the builder happens
#     to have rust-src (rust-analyzer installs it; a CI runner does not).
#     Remapping the sysroot back onto the virtual form collapses both cases.
#   - the workspace root -> `/stella`. Belt and braces: workspace member paths
#     are ALREADY relative in the binary (cargo invokes rustc from the workspace
#     root, and a release binary contains zero absolute checkout paths), so this
#     is a no-op today. It is set anyway because a build script that bakes an
#     absolute `OUT_DIR` into generated code would leak one silently, and the
#     two rebuild arms in release.yml deliberately check out at different paths.
#
# All three of those are rustc flags, and rustc compiles only part of this
# binary. The C that dependencies ship (tree-sitter) is compiled by cc/clang,
# which never sees RUSTFLAGS, so the same two prefixes are applied a second time
# as `-ffile-prefix-map` in CFLAGS/CXXFLAGS. Missing that half is what kept the
# `verify-reproducible` gate red from v0.6.75 to v0.6.106; the long comment at
# the C block below has the detail.
#
# The flags live HERE and not in `.cargo/config.toml` or `[build] rustflags`,
# for three independent reasons:
#
#   1. `.gitignore` ignores `.cargo/config.toml`, and scripts/check-no-scratch.sh
#      hard-fails on any tracked-but-ignored file, so it could not be committed.
#   2. cargo config values are literals with no `$HOME`/`$CARGO_HOME`
#      interpolation, and the two prefixes that matter are only knowable at run
#      time from `rustc --print sysroot` and the live environment.
#   3. A repo-wide rustflag would change the bytes of every dev build AND of the
#      frozen benchmark SUT (bench/evidence/run/build_sut.sh), invalidating the
#      recorded SHA in bench/READINESS.md. That script deliberately does not
#      route through here.
#
# Usage:
#   scripts/repro-build.sh [--locked] [--glibc <version>] <target-triple>
#
#     --locked           pass `--locked` to cargo (release.yml does; the local
#                        release path cannot, because it builds with a
#                        version-stamped manifest the lockfile has not caught up
#                        to yet)
#     --glibc <version>  cross-compile with `cargo zigbuild` against that glibc
#                        floor instead of building natively with `cargo build`
#
# Writes `stella.sha256` next to the binary and prints it as the last line, in
# `sha256sum` format with the artifact stem as the name:
#
#   <sha256>  stella-<version>-<target>
#
set -euo pipefail

# The workspace root, not the caller's cwd: workspace member paths are relative
# in the binary precisely because cargo is invoked from here. `pwd -P` and not
# plain `pwd` (the idiom in the other guard scripts) because this path is used
# as a --remap-path-prefix and has to be the one rustc sees — cargo canonicalises
# the manifest path, so a logical path through a symlinked checkout would remap
# a prefix that never appears.
repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
cd "$repo_root"

CRATE="stella-cli"
BIN="stella"

locked=""
glibc=""
target=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --locked) locked="--locked"; shift ;;
    --glibc)
      [ "$#" -ge 2 ] || { echo "repro-build: --glibc needs a version" >&2; exit 2; }
      glibc="$2"; shift 2 ;;
    -h | --help)
      sed -n '2,60p' "$0" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    -*)
      echo "repro-build: unknown option: $1" >&2; exit 2 ;;
    *)
      [ -z "$target" ] || { echo "repro-build: unexpected extra argument: $1" >&2; exit 2; }
      target="$1"; shift ;;
  esac
done

if [ -z "$target" ]; then
  echo "repro-build: usage: scripts/repro-build.sh [--locked] [--glibc <version>] <target-triple>" >&2
  exit 2
fi

sha256_of() {
  # Same two-tool fallback as install.sh: linux runners have sha256sum, macOS
  # runners have shasum and not the other way round.
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    echo "repro-build: no sha256 tool found (need sha256sum or shasum)" >&2
    exit 1
  fi
}

# ── The toolchain pin is a precondition, not a comment (#910 point 3) ────────
# A floating toolchain defeats everything below it: two rustc releases inline
# different std code and mangle symbols differently, so no amount of path
# remapping makes their output agree. rust-toolchain.toml carries the pin and
# rustup honors it for every cargo invocation inside this checkout — but until
# now nothing ever checked that the build actually ran on it. Assert it.
pin="$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' rust-toolchain.toml | head -n1)"
if [ -z "$pin" ]; then
  echo "repro-build: no 'channel = \"...\"' in rust-toolchain.toml — a release build needs a concrete toolchain pin." >&2
  exit 1
fi
case "$pin" in
  [0-9]*.[0-9]*)
    : ;;
  *)
    echo "repro-build: rust-toolchain.toml pins the floating channel '${pin}'." >&2
    echo "A release build needs a concrete version (e.g. channel = \"1.97.0\") or its output is not reproducible." >&2
    exit 1 ;;
esac
rustc_release="$(rustc -vV | sed -n 's/^release: //p')"
if [ "$rustc_release" != "$pin" ]; then
  echo "repro-build: rustc is ${rustc_release} but rust-toolchain.toml pins ${pin}." >&2
  echo "Run this from inside the checkout so rustup honors the pin (or 'rustup toolchain install ${pin}')." >&2
  exit 1
fi

# ── Path remapping ──────────────────────────────────────────────────────────
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
sysroot="$(rustc --print sysroot)"
rustc_hash="$(rustc -vV | sed -n 's/^commit-hash: //p')"
if [ -z "$rustc_hash" ]; then
  echo "repro-build: could not read rustc's commit-hash — cannot compute the std path remap." >&2
  exit 1
fi

remap="--remap-path-prefix=${repo_root}=/stella"
remap="${remap} --remap-path-prefix=${cargo_home}=/cargo"
remap="${remap} --remap-path-prefix=${sysroot}/lib/rustlib/src/rust=/rustc/${rustc_hash}"

if [ -n "${RUSTFLAGS:-}" ]; then
  echo "repro-build: warning: RUSTFLAGS was already set (${RUSTFLAGS})." >&2
  echo "repro-build: warning: the resulting binary is NOT the canonical reproducible artifact." >&2
fi
export RUSTFLAGS="${remap}${RUSTFLAGS:+ ${RUSTFLAGS}}"

# ── The same remap, for the C the binary also contains ──────────────────────
# `--remap-path-prefix` is a rustc flag, and rustc is not the only compiler in
# this build. Several dependencies ship C that the `cc` crate compiles and links
# in, and those paths are emitted by cc/clang, which never sees RUSTFLAGS. So
# the remap above covered the Rust half of the binary and none of the C half.
#
# That is not hypothetical: `strings` on the published v0.6.105 x86_64 artifact
# still found absolute builder paths, all of one shape —
#
#   $CARGO_HOME/registry/src/index.crates.io-<hash>/tree-sitter-0.26.11/src/./query.c
#
# sitting inside `__assert_fail` call sites, i.e. `__FILE__` expansions from
# tree-sitter's own `assert()`s. Ten of them are enough: two builders with
# different $CARGO_HOME produce different bytes, which is precisely what the
# `verify-reproducible` arm in release.yml has been reporting on every tag since
# the gate landed (#1247) — v0.6.75 through v0.6.106 all failed here, and none
# of them published.
#
# `-ffile-prefix-map` is the C-side equivalent and closes both halves at once:
# it implies `-fdebug-prefix-map` (DWARF) and `-fmacro-prefix-map` (`__FILE__`,
# where these came from). GCC has had it since 8, clang since 10, and the
# `--glibc` path's `zig cc` is clang — every compiler this script can reach
# supports it, so it is set unconditionally rather than probed. A probe that
# silently skipped the flag would hand back a non-reproducible binary that looks
# exactly like a good one, which is the failure mode this whole file exists to
# prevent.
#
# There is no C counterpart to the sysroot remap: std's sources are Rust.
cmap="-ffile-prefix-map=${repo_root}=/stella"
cmap="${cmap} -ffile-prefix-map=${cargo_home}=/cargo"

if [ -n "${CFLAGS:-}" ] || [ -n "${CXXFLAGS:-}" ]; then
  echo "repro-build: warning: CFLAGS/CXXFLAGS were already set (${CFLAGS:-} ${CXXFLAGS:-})." >&2
  echo "repro-build: warning: the resulting binary is NOT the canonical reproducible artifact." >&2
fi
# The bare names, not `TARGET_CFLAGS`: cc-rs looks up `CFLAGS_<triple>`, then
# `{HOST,TARGET}_CFLAGS`, then plain `CFLAGS`, and falls through to the last of
# those for both the cross-compiled artifact and the host build scripts. Setting
# the bare name is the one spelling that reaches every compile.
export CFLAGS="${cmap}${CFLAGS:+ ${CFLAGS}}"
export CXXFLAGS="${cmap}${CXXFLAGS:+ ${CXXFLAGS}}"

# ── SOURCE_DATE_EPOCH ───────────────────────────────────────────────────────
# Honestly: rustc consumes nothing from this — it is not a determinism input for
# the binary. It is exported here because the packaging step downstream of this
# build (scripts/package-tarball.py) needs one timestamp for every file it
# writes, and because a caller that already set it (the reproducible-builds
# convention) must win over the commit time.
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --pretty=%ct)}"

version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
stem="${BIN}-${version}-${target}"

# CARGO_TARGET_DIR-aware: scripts/setup-dev-env.sh moves cargo's output out of
# ./target per worktree, and a hardcoded path would either miss the fresh
# binary or hash a stale one from an earlier non-isolated build.
target_root="${CARGO_TARGET_DIR:-target}"
out_dir="${target_root}/${target}/release"

echo "repro-build: ${stem}"
echo "  toolchain          ${rustc_release} (rust-toolchain.toml pin), commit ${rustc_hash}"
echo "  RUSTFLAGS          ${RUSTFLAGS}"
echo "  CFLAGS             ${CFLAGS}"
echo "  SOURCE_DATE_EPOCH  ${SOURCE_DATE_EPOCH}"

if [ -n "$glibc" ]; then
  echo "  build              cargo zigbuild --target ${target}.${glibc}"
  command -v cargo-zigbuild >/dev/null 2>&1 \
    || { echo "repro-build: --glibc needs cargo-zigbuild (cargo install cargo-zigbuild)" >&2; exit 1; }
  # zigbuild writes to target/<triple>/release, without the glibc suffix.
  # shellcheck disable=SC2086 # $locked is a single optional flag, intentionally unquoted
  cargo zigbuild --release $locked --target "${target}.${glibc}" --package "$CRATE" --bin "$BIN"
else
  echo "  build              cargo build --target ${target}"
  # shellcheck disable=SC2086 # $locked is a single optional flag, intentionally unquoted
  cargo build --release $locked --target "$target" --package "$CRATE" --bin "$BIN"
fi

bin_path="${out_dir}/${BIN}"
[ -x "$bin_path" ] || { echo "repro-build: expected binary not found: ${bin_path}" >&2; exit 1; }

# The name column is the artifact stem, not a path, so the per-target files
# concatenate straight into the release's SHA256SUMS.bin.
printf '%s  %s\n' "$(sha256_of "$bin_path")" "$stem" > "${out_dir}/${BIN}.sha256"
cat "${out_dir}/${BIN}.sha256"
