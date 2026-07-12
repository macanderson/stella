#!/bin/sh
# shellcheck shell=sh
#
# Stella installer — https://oxagen.sh/install
#
#   curl -fsSL https://oxagen.sh/install | sh
#
# Installs the `stella` CLI: a fast, BYOK, model-agnostic terminal coding
# agent from the makers of Oxagen (https://docs.oxagen.sh/stella).
#
# It downloads a prebuilt binary for your platform from GitHub Releases,
# verifies its SHA-256 checksum, and drops it in ~/.stella/bin (adding that
# to your PATH). When no prebuilt binary is available for your platform — or
# no release has been cut yet — it falls back to building from source with
# cargo.
#
# Configuration (env vars or flags after `sh -s --`):
#   STELLA_INSTALL_DIR       where to put the binary   (default ~/.stella/bin)
#   STELLA_VERSION           tag to install, e.g. v0.1.0   (default: latest)
#   STELLA_BUILD_FROM_SOURCE set to 1 to force a cargo build
#   STELLA_NO_MODIFY_PATH    set to 1 to skip editing your shell profile
#
#   curl -fsSL https://oxagen.sh/install | sh -s -- --version v0.1.0
#   curl -fsSL https://oxagen.sh/install | STELLA_INSTALL_DIR=/usr/local/bin sh
#
# The whole script is wrapped in main() and only invoked at the very end, so
# a truncated download (the classic curl|sh hazard) can never run a partial
# install.

set -eu

# --- constants ---------------------------------------------------------------

REPO="oxageninc/stella-cli"
BINARY="stella"
DEFAULT_INSTALL_DIR="${HOME}/.stella/bin"
RELEASES_URL="https://github.com/${REPO}/releases"
LATEST_API_URL="https://api.github.com/repos/${REPO}/releases/latest"

# --- output helpers ----------------------------------------------------------

# Colors, but only for a real terminal and when NO_COLOR is unset.
if [ -t 2 ] && [ -z "${NO_COLOR:-}" ] && [ "${TERM:-dumb}" != "dumb" ]; then
    _bold=$(printf '\033[1m')
    _red=$(printf '\033[31m')
    _green=$(printf '\033[32m')
    _yellow=$(printf '\033[33m')
    _cyan=$(printf '\033[36m')
    _reset=$(printf '\033[0m')
else
    _bold='' _red='' _green='' _yellow='' _cyan='' _reset=''
fi

say() { printf '%s%s stella%s %s\n' "$_bold" "$_cyan" "$_reset" "$1" >&2; }
info() { printf '       %s\n' "$1" >&2; }
warn() { printf '%s%s warn%s   %s\n' "$_bold" "$_yellow" "$_reset" "$1" >&2; }
success() { printf '%s%s ok%s     %s\n' "$_bold" "$_green" "$_reset" "$1" >&2; }
err() {
    printf '%s%s error%s  %s\n' "$_bold" "$_red" "$_reset" "$1" >&2
    exit 1
}

# --- small utilities ---------------------------------------------------------

have() { command -v "$1" >/dev/null 2>&1; }

need() {
    have "$1" || err "required command not found: $1"
}

# dl <output|-> <url> — download to a file, or stream to stdout with "-".
dl() {
    _out="$1"
    _url="$2"
    if [ "$DOWNLOADER" = curl ]; then
        if [ "$_out" = "-" ]; then
            curl -fsSL "$_url"
        else
            curl -fsSL -o "$_out" "$_url"
        fi
    else
        if [ "$_out" = "-" ]; then
            wget -qO- "$_url"
        else
            wget -qO "$_out" "$_url"
        fi
    fi
}

# sha256 <file> — print the hex digest, or empty if no tool is available.
sha256() {
    if have sha256sum; then
        sha256sum "$1" | cut -d' ' -f1
    elif have shasum; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        printf ''
    fi
}

# --- platform detection ------------------------------------------------------

# Echoes a Rust target triple for this machine, or empty if unsupported.
detect_target() {
    _os="$(uname -s)"
    _arch="$(uname -m)"

    case "$_os" in
        Darwin) _os=apple-darwin ;;
        Linux) _os=unknown-linux-gnu ;;
        *) printf ''; return 0 ;;
    esac

    case "$_arch" in
        x86_64 | amd64) _arch=x86_64 ;;
        arm64 | aarch64) _arch=aarch64 ;;
        *) printf ''; return 0 ;;
    esac

    printf '%s-%s' "$_arch" "$_os"
}

# --- version resolution ------------------------------------------------------

resolve_version() {
    if [ -n "${VERSION:-}" ]; then
        printf '%s' "$VERSION"
        return 0
    fi
    # Parse "tag_name": "vX.Y.Z" out of the latest-release JSON without jq.
    dl - "$LATEST_API_URL" 2>/dev/null \
        | grep -m1 '"tag_name"' \
        | sed 's/.*"tag_name"[[:space:]]*:[[:space:]]*"//; s/".*//'
}

# --- install from a prebuilt release -----------------------------------------

install_from_release() {
    _target="$1"
    _tag="$2"
    _asset="${BINARY}-${_target}.tar.gz"
    _base="${RELEASES_URL}/download/${_tag}/${_asset}"

    say "downloading ${BINARY} ${_bold}${_tag}${_reset} for ${_target}"

    _tmp="$(mktemp -d 2>/dev/null || mktemp -d -t stella)"
    # shellcheck disable=SC2064
    trap "rm -rf \"$_tmp\"" EXIT INT TERM

    if ! dl "${_tmp}/${_asset}" "$_base" 2>/dev/null; then
        rm -rf "$_tmp"; trap - EXIT INT TERM
        return 1
    fi

    # Verify the checksum when the release publishes one and we have a tool.
    if dl "${_tmp}/${_asset}.sha256" "${_base}.sha256" 2>/dev/null; then
        _want="$(cut -d' ' -f1 <"${_tmp}/${_asset}.sha256")"
        _got="$(sha256 "${_tmp}/${_asset}")"
        if [ -z "$_got" ]; then
            warn "no sha256 tool found; skipping checksum verification"
        elif [ "$_want" != "$_got" ]; then
            rm -rf "$_tmp"; trap - EXIT INT TERM
            err "checksum mismatch for ${_asset} (expected ${_want}, got ${_got})"
        else
            info "checksum verified"
        fi
    else
        warn "no published checksum for ${_asset}; skipping verification"
    fi

    tar -xzf "${_tmp}/${_asset}" -C "$_tmp" \
        || { rm -rf "$_tmp"; trap - EXIT INT TERM; err "failed to extract ${_asset}"; }

    # The binary may sit at the archive root or one directory down.
    if [ -f "${_tmp}/${BINARY}" ]; then
        _bin="${_tmp}/${BINARY}"
    else
        _bin="$(find "$_tmp" -type f -name "$BINARY" -perm -u+x 2>/dev/null | head -n1)"
        [ -n "$_bin" ] || _bin="$(find "$_tmp" -type f -name "$BINARY" 2>/dev/null | head -n1)"
    fi
    [ -n "${_bin:-}" ] && [ -f "$_bin" ] \
        || { rm -rf "$_tmp"; trap - EXIT INT TERM; err "'${BINARY}' not found inside ${_asset}"; }

    mkdir -p "$INSTALL_DIR"
    install -m 0755 "$_bin" "${INSTALL_DIR}/${BINARY}" 2>/dev/null \
        || { cp "$_bin" "${INSTALL_DIR}/${BINARY}" && chmod 0755 "${INSTALL_DIR}/${BINARY}"; }

    rm -rf "$_tmp"
    trap - EXIT INT TERM
    return 0
}

# --- install from source (cargo) ---------------------------------------------

install_from_source() {
    say "building ${BINARY} from source with cargo"

    if ! have cargo; then
        err "cargo not found. Install the Rust toolchain first:

    curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs | sh

then re-run this installer, or build directly:

    cargo install --locked --git https://github.com/${REPO} ${BINARY}-cli"
    fi

    info "this compiles a bundled DuckDB (C/C++); a C++ toolchain is required"
    info "(Xcode Command Line Tools on macOS, build-essential on Linux)"

    # `cargo install --root R` puts the binary in R/bin — so point R at the
    # parent of INSTALL_DIR when INSTALL_DIR ends in /bin, else use it as-is.
    case "$INSTALL_DIR" in
        */bin) _root="$(dirname "$INSTALL_DIR")" ;;
        *) _root="$INSTALL_DIR" ;;
    esac

    _ref=""
    [ -n "${VERSION:-}" ] && _ref="--tag ${VERSION}"

    # shellcheck disable=SC2086
    cargo install --locked --force --root "$_root" \
        --git "https://github.com/${REPO}" $_ref "${BINARY}-cli" \
        || err "cargo install failed"

    # Normalize: ensure the binary is at INSTALL_DIR/stella.
    if [ "${_root}/bin/${BINARY}" != "${INSTALL_DIR}/${BINARY}" ]; then
        mkdir -p "$INSTALL_DIR"
        cp "${_root}/bin/${BINARY}" "${INSTALL_DIR}/${BINARY}"
    fi
    return 0
}

# --- PATH wiring -------------------------------------------------------------

# Append the install dir to the right shell profile (idempotent).
add_to_path() {
    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*)
            return 0 # already reachable
            ;;
    esac

    if [ "${NO_MODIFY_PATH}" = 1 ]; then
        warn "${INSTALL_DIR} is not on your PATH"
        info "add it with: export PATH=\"${INSTALL_DIR}:\$PATH\""
        return 0
    fi

    _shell_name="$(basename "${SHELL:-sh}")"
    _line="export PATH=\"${INSTALL_DIR}:\$PATH\""
    case "$_shell_name" in
        zsh) _profile="${ZDOTDIR:-$HOME}/.zshrc" ;;
        bash)
            if [ -f "${HOME}/.bashrc" ]; then
                _profile="${HOME}/.bashrc"
            else
                _profile="${HOME}/.bash_profile"
            fi
            ;;
        fish)
            _profile="${HOME}/.config/fish/config.fish"
            _line="fish_add_path \"${INSTALL_DIR}\""
            ;;
        *) _profile="${HOME}/.profile" ;;
    esac

    # Skip if this dir is already wired into the chosen profile.
    if [ -f "$_profile" ] && grep -Fq "$INSTALL_DIR" "$_profile" 2>/dev/null; then
        return 0
    fi

    mkdir -p "$(dirname "$_profile")"
    {
        printf '\n# Added by the stella installer (https://oxagen.sh/install)\n'
        printf '%s\n' "$_line"
    } >>"$_profile"

    ADDED_PROFILE="$_profile"
}

# --- usage -------------------------------------------------------------------

usage() {
    cat >&2 <<EOF
${_bold}Stella installer${_reset}

  curl -fsSL https://oxagen.sh/install | sh

Options (pass after 'sh -s --', or use the matching env var):
  --version <tag>        install a specific tag, e.g. v0.1.0   (STELLA_VERSION)
  --install-dir <dir>    install location    (STELLA_INSTALL_DIR; default ~/.stella/bin)
  --build-from-source    compile with cargo instead of downloading a binary
  --no-modify-path       do not edit your shell profile        (STELLA_NO_MODIFY_PATH)
  -h, --help             show this help
EOF
}

# --- main --------------------------------------------------------------------

main() {
    INSTALL_DIR="${STELLA_INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"
    VERSION="${STELLA_VERSION:-}"
    BUILD_FROM_SOURCE="${STELLA_BUILD_FROM_SOURCE:-0}"
    NO_MODIFY_PATH="${STELLA_NO_MODIFY_PATH:-0}"
    ADDED_PROFILE=""

    while [ $# -gt 0 ]; do
        case "$1" in
            --version) VERSION="${2:-}"; shift 2 ;;
            --version=*) VERSION="${1#*=}"; shift ;;
            --install-dir) INSTALL_DIR="${2:-}"; shift 2 ;;
            --install-dir=*) INSTALL_DIR="${1#*=}"; shift ;;
            --build-from-source) BUILD_FROM_SOURCE=1; shift ;;
            --no-modify-path) NO_MODIFY_PATH=1; shift ;;
            -h | --help) usage; exit 0 ;;
            *) err "unknown option: $1 (try --help)" ;;
        esac
    done

    need uname
    need tar
    if have curl; then
        DOWNLOADER=curl
    elif have wget; then
        DOWNLOADER=wget
    else
        err "need curl or wget to download"
    fi

    say "installing into ${_bold}${INSTALL_DIR}${_reset}"

    _installed=0
    if [ "$BUILD_FROM_SOURCE" = 1 ]; then
        install_from_source && _installed=1
    else
        _target="$(detect_target)"
        if [ -z "$_target" ]; then
            warn "no prebuilt binary for $(uname -s)/$(uname -m); building from source"
            install_from_source && _installed=1
        else
            _tag="$(resolve_version)"
            if [ -z "$_tag" ]; then
                warn "no published release found; building from source"
                install_from_source && _installed=1
            elif install_from_release "$_target" "$_tag"; then
                _installed=1
            else
                warn "prebuilt download failed for ${_target} ${_tag}; building from source"
                install_from_source && _installed=1
            fi
        fi
    fi

    [ "$_installed" = 1 ] || err "installation failed"

    add_to_path

    _v="$("${INSTALL_DIR}/${BINARY}" --version 2>/dev/null || printf '')"
    success "installed ${_bold}${_v:-$BINARY}${_reset} to ${INSTALL_DIR}/${BINARY}"

    if [ -n "$ADDED_PROFILE" ]; then
        info "added ${INSTALL_DIR} to your PATH in ${ADDED_PROFILE}"
        info "restart your shell or run: ${_bold}export PATH=\"${INSTALL_DIR}:\$PATH\"${_reset}"
    fi

    printf '\n' >&2
    info "next: set a provider key and start Stella"
    info "  ${_bold}export ANTHROPIC_API_KEY=...${_reset}   # or ZAI_API_KEY, OPENAI_API_KEY, ..."
    info "  ${_bold}stella${_reset}                          # interactive chat"
    info "docs: https://docs.oxagen.sh/docs/stella"
}

main "$@"
