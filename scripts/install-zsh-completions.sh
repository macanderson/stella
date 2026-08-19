#!/usr/bin/env bash
#
# Reinstall the zsh completion function for `stella`.
#
#   ./scripts/install-zsh-completions.sh [--dir DIR] [--best-effort]
#
# `stella completions zsh` renders the completion function from clap's command
# tree, so the installed copy is only as fresh as the binary that produced it:
# every renamed subcommand, added flag or changed value hint silently rots the
# `_stella` sitting in your fpath. That is why `make build-release` calls this
# with `--best-effort` — the moment a new binary exists is the only moment the
# completions are known to be current, and a human will not remember.
#
# Where it writes. There is no single right directory, so the script does not
# guess one: it asks *your* zsh for its real `$fpath` and takes the first of
# these candidates that is on it —
#
#   1. --dir / $STELLA_ZSH_COMPLETIONS_DIR   (explicit wins, no probing)
#   2. ${ZSH:-~/.oh-my-zsh}/custom/completions
#   3. ~/.zsh/completions
#   4. $(brew --prefix)/share/zsh/site-functions
#
# oh-my-zsh's `custom/` tree is preferred over Homebrew's `site-functions`
# because the latter is package-manager territory: `brew` will happily delete a
# file it did not install, and a formula-installed `_stella` would then be the
# one on fpath with no sign of which copy won. When nothing is on fpath the
# script still installs — to candidate 2 or 3 — and prints the exact line to
# add to `.zshrc`, because writing into a directory zsh never reads is the
# failure that looks like success.
#
# Why it deletes ~/.zcompdump*: compinit caches the *set* of completion
# functions it found, so a brand-new `_stella` stays invisible to freshly
# started shells until that dump is rebuilt. The dump is a regenerable cache —
# the next shell rebuilds it — and it is only removed when the installed file
# actually changed. `STELLA_COMPLETIONS_KEEP_ZCOMPDUMP=1` opts out.
#
# `--best-effort` turns every failure below into a warning and exit 0: a build
# must not go red because a shell nicety could not be installed. Run it
# without that flag (`make completions`) to see the failure.
#
# scripts/test-install-zsh-completions.sh is its witness — it drives this
# script against a stub binary and a temp directory, so the "installs",
# "unchanged", "refuses non-completion output" and "reports an off-fpath
# directory" branches are proven rather than assumed.
set -euo pipefail

usage() {
  cat <<'EOF'
usage: scripts/install-zsh-completions.sh [--dir DIR] [--best-effort]

  --dir DIR      install _stella into DIR instead of probing $fpath
  --best-effort  warn and exit 0 on any failure (used by `make build-release`)

env: STELLA_BIN, STELLA_ZSH_COMPLETIONS_DIR, STELLA_COMPLETIONS_KEEP_ZCOMPDUMP
EOF
}

best_effort=0
dir_override="${STELLA_ZSH_COMPLETIONS_DIR:-}"

while [ $# -gt 0 ]; do
  case "$1" in
    --best-effort) best_effort=1 ;;
    --dir)
      shift
      [ $# -gt 0 ] || {
        echo "error: --dir needs a directory" >&2
        exit 2
      }
      dir_override="$1"
      ;;
    --dir=*) dir_override="${1#--dir=}" ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

die() {
  if [ "$best_effort" -eq 1 ]; then
    echo "warning: zsh completions not installed: $*" >&2
    exit 0
  fi
  echo "error: $*" >&2
  exit 1
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# The binary. Prefer the release build this most likely just ran beside, then
# the debug build, then whatever is on PATH — an installed `stella` is still a
# better source of completions than none.
resolve_bin() {
  if [ -n "${STELLA_BIN:-}" ]; then
    [ -x "$STELLA_BIN" ] || die "STELLA_BIN is not executable: ${STELLA_BIN}"
    printf '%s\n' "$STELLA_BIN"
    return
  fi
  local target_dir="${CARGO_TARGET_DIR:-${repo_root}/target}"
  local candidate
  for candidate in "${target_dir}/release/stella" "${target_dir}/debug/stella"; do
    if [ -x "$candidate" ]; then
      printf '%s\n' "$candidate"
      return
    fi
  done
  if candidate="$(command -v stella 2>/dev/null)"; then
    printf '%s\n' "$candidate"
    return
  fi
  die "no stella binary found — run 'make build-release' first"
}

# The user's real fpath, read once. An interactive zsh sources their rc, which
# may print a banner to stdout, so the list rides between two markers rather
# than being taken as "whatever zsh printed".
read_fpath() {
  command -v zsh >/dev/null 2>&1 || return 0
  zsh -ic 'print -rl -- __STELLA_FPATH_BEGIN__ $fpath __STELLA_FPATH_END__' 2>/dev/null \
    | awk '/^__STELLA_FPATH_END__$/ { inside = 0 } inside { print } /^__STELLA_FPATH_BEGIN__$/ { inside = 1 }' \
    || true
}

FPATH_ENTRIES="$(read_fpath)"

on_fpath() {
  local entry
  while IFS= read -r entry; do
    [ "$entry" = "$1" ] && return 0
  done <<<"$FPATH_ENTRIES"
  return 1
}

resolve_dir() {
  if [ -n "$dir_override" ]; then
    printf '%s\n' "$dir_override"
    return
  fi

  local omz="${ZSH:-${HOME}/.oh-my-zsh}/custom/completions"
  local user_dir="${HOME}/.zsh/completions"
  local brew_dir=""
  if command -v brew >/dev/null 2>&1; then
    brew_dir="$(brew --prefix)/share/zsh/site-functions"
  fi

  local candidate
  for candidate in "$omz" "$user_dir" ${brew_dir:+"$brew_dir"}; do
    if on_fpath "$candidate"; then
      printf '%s\n' "$candidate"
      return
    fi
  done

  # Nothing on fpath. Install anyway, next to an existing oh-my-zsh if there is
  # one, and print the .zshrc line that makes it live.
  if [ -d "${ZSH:-${HOME}/.oh-my-zsh}" ]; then
    printf '%s\n' "$omz"
  else
    printf '%s\n' "$user_dir"
  fi
}

bin="$(resolve_bin)"
dir="$(resolve_dir)"
target="${dir}/_stella"

mkdir -p "$dir" 2>/dev/null || die "cannot create ${dir}"
[ -w "$dir" ] || die "${dir} is not writable"

tmp="$(mktemp "${TMPDIR:-/tmp}/_stella.XXXXXX")"
trap 'rm -f "$tmp"' EXIT

"$bin" completions zsh >"$tmp" || die "'${bin} completions zsh' failed"

# A completion function that is empty, or that is not one, is worse than the
# stale copy it would replace: zsh would load it and complete nothing.
[ -s "$tmp" ] || die "'${bin} completions zsh' produced nothing"
head -n 1 "$tmp" | grep -q '^#compdef stella$' \
  || die "'${bin} completions zsh' did not produce a zsh completion function"

if [ -f "$target" ] && cmp -s "$tmp" "$target"; then
  echo "zsh completions already current: ${target}"
  exit 0
fi

install -m 0644 "$tmp" "$target" || die "cannot write ${target}"
echo "zsh completions installed: ${target}  (from ${bin})"

if [ -z "${STELLA_COMPLETIONS_KEEP_ZCOMPDUMP:-}" ]; then
  # nullglob so an unmatched pattern expands to nothing rather than to itself.
  shopt -s nullglob
  dumps=("${HOME}"/.zcompdump*)
  shopt -u nullglob
  if [ "${#dumps[@]}" -gt 0 ]; then
    rm -f "${dumps[@]}"
    echo "cleared ${#dumps[@]} stale zcompdump file(s) so compinit re-scans"
  fi
fi

if ! on_fpath "$dir"; then
  cat >&2 <<EOF
note: ${dir} is not on your zsh \$fpath, so the file above is inert.
      Add this to ~/.zshrc *before* the line that runs compinit:

          fpath=(${dir} \$fpath)
EOF
fi

echo "open a new shell (or run 'exec zsh') to pick them up"
