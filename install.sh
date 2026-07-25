#!/bin/sh
# Stella CLI installer.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/macanderson/stella/main/install.sh | sh
#
# Environment overrides:
#   STELLA_VERSION      install a specific version (e.g. "0.1.0" or "v0.1.0")
#                       instead of the latest release.
#   STELLA_INSTALL_DIR  install directory (default: $HOME/.local/bin).
#   STELLA_REQUIRE_PROVENANCE=1
#                       refuse to install unless the build-provenance
#                       attestation verifies. Requires `gh`. Off by default
#                       because `curl | sh` cannot assume gh is present, and
#                       releases published before provenance was introduced
#                       carry no attestation.
#
# Behavior: detects your OS/arch, downloads the matching prebuilt tarball from
# the GitHub Release, verifies its SHA-256 against SHA256SUMS, verifies the
# build-provenance attestation when `gh` is available, and installs the
# `stella` binary. If no prebuilt binary matches your platform, it falls back to
# `cargo install`.
#
# Note the checksum and the attestation answer different questions: SHA256SUMS
# proves the download was not corrupted, and is fetched over the same channel
# as the artifact it vouches for. The attestation proves the artifact was built
# by this repo's release workflow. A verifier that runs and *rejects* an
# artifact is always fatal, regardless of STELLA_REQUIRE_PROVENANCE.
#
# POSIX sh — no bashisms.

set -eu

REPO="macanderson/stella"
BIN="stella"
INSTALL_DIR="${STELLA_INSTALL_DIR:-$HOME/.local/bin}"
DOWNLOAD_BASE="https://github.com/${REPO}/releases/download"

# ---- logging -------------------------------------------------------------

info() { printf 'stella-install: %s\n' "$1" >&2; }
err() { printf 'stella-install: error: %s\n' "$1" >&2; }
die() {
  err "$1"
  exit 1
}

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    die "required command not found: $1"
  fi
}

# ---- platform detection --------------------------------------------------

detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Darwin) os_part="apple-darwin" ;;
    Linux) os_part="unknown-linux-gnu" ;;
    *)
      TARGET=""
      return 0
      ;;
  esac

  case "$arch" in
    x86_64 | amd64) arch_part="x86_64" ;;
    arm64 | aarch64) arch_part="aarch64" ;;
    *)
      TARGET=""
      return 0
      ;;
  esac

  TARGET="${arch_part}-${os_part}"
}

# Targets for which we publish prebuilt tarballs (keep in sync with
# .github/workflows/release.yml and [workspace.metadata.dist] in Cargo.toml).
is_supported_target() {
  case "$1" in
    aarch64-apple-darwin | x86_64-apple-darwin | \
      x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

# The prebuilt Linux tarballs are glibc (`-gnu`). On musl (Alpine) a glibc
# binary passes the checksum and "installs", then dies at exec with the loader's
# cryptic "no such file or directory". Detect musl and build from source instead.
is_musl() {
  [ "$(uname -s)" = "Linux" ] || return 1
  if command -v ldd >/dev/null 2>&1 && ldd --version 2>&1 | grep -qi musl; then
    return 0
  fi
  # ldd is often absent on musl systems; fall back to probing for its loader.
  [ -e /lib/ld-musl-x86_64.so.1 ] || [ -e /lib/ld-musl-aarch64.so.1 ]
}

# ---- version resolution --------------------------------------------------

# Resolve the release tag (e.g. "v0.1.0"). Honors STELLA_VERSION, otherwise
# queries the GitHub "latest release" API.
resolve_tag() {
  if [ -n "${STELLA_VERSION:-}" ]; then
    case "$STELLA_VERSION" in
      v*) TAG="$STELLA_VERSION" ;;
      *) TAG="v${STELLA_VERSION}" ;;
    esac
    return 0
  fi

  # Resolve "latest" via the release redirect rather than the JSON API: the
  # unauthenticated API is rate-limited to 60 req/hr/IP (CI farms and office
  # NATs hit it and the install dies), and this avoids scraping JSON.
  # /releases/latest 302-redirects to /releases/tag/<TAG>.
  effective="$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
    "https://github.com/${REPO}/releases/latest")" ||
    die "could not resolve the latest release"
  TAG="${effective##*/tag/}"
  case "$TAG" in
    v*) : ;;
    *) die "could not determine latest release tag (resolved to '${effective}')" ;;
  esac
}

# ---- checksum ------------------------------------------------------------

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    die "no sha256 tool found (need sha256sum or shasum)"
  fi
}

# ---- provenance ----------------------------------------------------------

# Verify the GitHub build-provenance attestation for a downloaded artifact.
#
# Three outcomes, and the distinction between them is the whole design:
#
#   verified      -> continue
#   contradicted  -> die, always, no override
#   unverifiable  -> warn and continue, unless STELLA_REQUIRE_PROVENANCE=1
#
# "Unverifiable" means no `gh` on PATH, or a gh too old for `attestation
# verify`. Requiring gh outright would break `curl | sh` for most people, which
# is the install path this script exists to serve; silently skipping when gh
# *is* present would waste a guarantee that costs nothing to check.
verify_provenance() {
  artifact="$1"

  if ! command -v gh >/dev/null 2>&1; then
    if [ "${STELLA_REQUIRE_PROVENANCE:-0}" = "1" ]; then
      die "STELLA_REQUIRE_PROVENANCE=1 but gh is not installed; cannot verify provenance"
    fi
    info "provenance not verified (gh not installed) — checksum only"
    info "  to verify: install gh, then STELLA_REQUIRE_PROVENANCE=1 re-run this installer"
    return 0
  fi

  if ! gh attestation verify --help >/dev/null 2>&1; then
    if [ "${STELLA_REQUIRE_PROVENANCE:-0}" = "1" ]; then
      die "STELLA_REQUIRE_PROVENANCE=1 but this gh has no 'attestation verify'; upgrade gh"
    fi
    info "provenance not verified (gh too old for 'attestation verify') — checksum only"
    return 0
  fi

  info "verifying provenance"
  if out="$(gh attestation verify "$artifact" --repo "$REPO" 2>&1)"; then
    info "provenance ok"
    return 0
  fi

  # Every release published before provenance was added carries no attestation
  # at all, and so does any release cut from a fork. That is "unverifiable",
  # not "forged" — treating it as a hard failure would break `curl | sh` for
  # every older version the moment this landed. Distinguish the two by gh's
  # own wording rather than collapsing them into one exit code.
  if printf '%s' "$out" | grep -qi 'no attestation'; then
    if [ "${STELLA_REQUIRE_PROVENANCE:-0}" = "1" ]; then
      die "STELLA_REQUIRE_PROVENANCE=1 but ${version} carries no attestation.
     Releases published before provenance was introduced have none. Install a
     newer version, or unset STELLA_REQUIRE_PROVENANCE to accept checksum-only."
    fi
    info "provenance not verified (no attestation for ${version}) — checksum only"
    return 0
  fi

  # Reached only when a verifier ran and actively rejected the artifact.
  # Never soft, never overridable.
  die "provenance verification FAILED for $(basename "$artifact") — refusing to install.
     The checksum matched, but the artifact does not carry a valid build
     attestation from ${REPO}. Do not install this binary. To see why:
       gh attestation verify \"$artifact\" --repo ${REPO}
     Please report it at https://github.com/${REPO}/security"
}

# ---- cargo fallback ------------------------------------------------------

cargo_fallback() {
  info "$1"
  if ! command -v cargo >/dev/null 2>&1; then
    die "cargo not found; install Rust from https://rustup.rs then re-run"
  fi

  # Always pin the source build to a released tag. Left unpinned, `cargo install
  # --git` builds whatever `main` happens to be — an arbitrary, unreleased,
  # unchecksummed commit — which is exactly the guarantee the download path
  # exists to provide. `resolve_tag` runs before every caller and has already
  # normalized STELLA_VERSION (or the latest release) into TAG, so this is the
  # single source of truth for which ref gets built.
  ref_args="--tag ${TAG}"
  info "building stella ${TAG} from source with cargo (this may take a while)..."

  # Honor STELLA_INSTALL_DIR when it looks like a .../bin directory (the default
  # is ~/.local/bin): cargo installs the binary into <root>/bin.
  root_args=""
  case "$INSTALL_DIR" in
    */bin) root_args="--root ${INSTALL_DIR%/bin}" ;;
  esac

  # shellcheck disable=SC2086 # word-splitting of the optional arg groups is intended
  cargo install --locked ${ref_args} ${root_args} --git "https://github.com/${REPO}" stella-cli
  if [ -n "$root_args" ]; then
    info "installed stella to ${INSTALL_DIR}."
  else
    info "done. Ensure Cargo's bin dir (usually \$HOME/.cargo/bin) is on your PATH."
  fi
  exit 0
}

# ---- PATH hint -----------------------------------------------------------

path_hint() {
  case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) : ;;
    *)
      info "note: ${INSTALL_DIR} is not on your PATH."
      info "add it, e.g.:  export PATH=\"${INSTALL_DIR}:\$PATH\""
      ;;
  esac
}

# ---- main ----------------------------------------------------------------

main() {
  need_cmd uname
  need_cmd curl
  need_cmd tar
  need_cmd mkdir
  need_cmd mktemp
  need_cmd awk

  detect_target
  # Resolve the tag BEFORE the fallback branches: both of them build from source
  # with cargo, and that build must be pinned to a release. Resolution is a
  # single HEAD request on the fast path, and it `die`s without a network — so
  # an offline musl user now fails here instead of inside `cargo install --git`,
  # which needs the network anyway.
  resolve_tag

  if [ -z "$TARGET" ] || ! is_supported_target "$TARGET"; then
    cargo_fallback "no prebuilt binary for this platform ($(uname -s) $(uname -m)); falling back to cargo."
  fi
  if is_musl; then
    cargo_fallback "musl libc detected; prebuilt binaries are glibc-only — building from source."
  fi

  version="${TAG#v}"

  asset="stella-${version}-${TARGET}.tar.gz"
  asset_url="${DOWNLOAD_BASE}/${TAG}/${asset}"
  sums_url="${DOWNLOAD_BASE}/${TAG}/SHA256SUMS"

  info "installing stella ${version} (${TARGET})"

  tmpdir="$(mktemp -d 2>/dev/null || mktemp -d -t stella)"
  trap 'rm -rf "$tmpdir"' EXIT INT TERM

  info "downloading ${asset}"
  if ! curl -fsSL "$asset_url" -o "${tmpdir}/${asset}"; then
    die "download failed: ${asset_url}"
  fi

  info "downloading checksums"
  if ! curl -fsSL "$sums_url" -o "${tmpdir}/SHA256SUMS"; then
    die "download failed: ${sums_url}"
  fi

  # Verify SHA-256 against the published SHA256SUMS.
  expected="$(awk -v f="$asset" '$2 == f {print $1}' "${tmpdir}/SHA256SUMS")"
  [ -n "$expected" ] || die "no checksum for ${asset} in SHA256SUMS"
  actual="$(sha256_of "${tmpdir}/${asset}")"
  if [ "$expected" != "$actual" ]; then
    die "checksum mismatch for ${asset}: expected ${expected}, got ${actual}"
  fi
  info "checksum ok"

  # SHA256SUMS proves the tarball was not corrupted in transit. It does not
  # prove where the tarball came from: it is fetched over the same channel,
  # from the same release, so anything able to replace one could replace both.
  # The provenance attestation closes that gap — it is bound to the release
  # workflow at a specific commit and cannot be reissued by whoever holds the
  # release.
  #
  # Verification needs `gh`, which a `curl | sh` install cannot assume. So:
  # verify whenever gh is present, and let anyone who requires the guarantee
  # demand it with STELLA_REQUIRE_PROVENANCE=1. A *failed* verification is
  # always fatal — the soft path is "no verifier available", never "verifier
  # said no".
  verify_provenance "${tmpdir}/${asset}"

  # Extract. The tarball contains a top-level "stella-<version>-<target>/" dir.
  tar -C "$tmpdir" -xzf "${tmpdir}/${asset}"
  src="${tmpdir}/stella-${version}-${TARGET}/${BIN}"
  [ -f "$src" ] || die "binary not found in archive: ${src}"

  mkdir -p "$INSTALL_DIR"
  install_path="${INSTALL_DIR}/${BIN}"
  # Install atomically: copy to a temp file in the same dir, make it executable,
  # then rename over the target. A plain `cp` over the destination truncates it
  # in place — fatal if the old binary is running (ETXTBSY / a half-written exec
  # left in PATH if the copy is interrupted). `mv` within a filesystem is atomic.
  tmp_bin="${INSTALL_DIR}/.${BIN}.tmp.$$"
  cp "$src" "$tmp_bin"
  chmod +x "$tmp_bin"
  mv -f "$tmp_bin" "$install_path"

  info "installed stella to ${install_path}"
  path_hint
  info "run 'stella --version' to verify."
}

main "$@"
