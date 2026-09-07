#!/usr/bin/env bash
#
# Tests for `scripts/wait-for-armed-merge.sh`.
#
#   `./scripts/test-wait-for-armed-merge.sh`
#   `make wait-for-armed-merge-test`
#
# No network and no real `gh`. Each case puts a fake one first on `PATH`.
# It answers `gh pr view <branch> --json state`. It reads one state per
# poll from a small script. A case that names several states, and still
# gets the last one, proves the loop polls. It does not just read once and
# stop.
#
# `--poll-seconds 0` means "ask once, then give up." `dispatch-main-verification.sh`
# uses the same flag the same way. It is not a fast way to run several polls.
# A case that proves the loop keeps going uses a small real
# `--poll-seconds 1` and pays a few seconds of wall clock for it. A case
# that only needs one answer keeps `--poll-seconds 0`. That costs nothing.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/wait-for-armed-merge.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

pass=0
fail=0

ok() {
  printf '  \033[32m✓\033[0m %s\n' "$*"
  pass=$((pass + 1))
}
bad() {
  printf '  \033[31m✗\033[0m %s\n' "$*"
  fail=$((fail + 1))
}

# One fake `gh` per case, in its own folder. `states` holds one PR state per
# line, read in order — one line per `gh pr view` call — and the last line
# repeats once the script runs past the end, the same "hold the last answer"
# shape `test-dispatch-main-verification.sh`'s fake uses.
new_case() { # new_case <name> <states...>
  local dir="$work/$1"
  mkdir -p "$dir"
  shift
  printf '%s\n' "$@" >"$dir/states"
  cat >"$dir/gh" <<'SHIM'
#!/usr/bin/env bash
set -u
dir="$GH_SHIM_DIR"
printf '%s\n' "$*" >>"$dir/calls.log"
case "${1:-}" in
pr)
  if [ "${2:-}" = "view" ]; then
    [ -f "$dir/view-fails" ] && exit 1
    n=0
    [ -f "$dir/cursor" ] && n="$(cat "$dir/cursor")"
    n=$((n + 1))
    printf '%s' "$n" >"$dir/cursor"
    line="$(sed -n "${n}p" "$dir/states")"
    [ -z "$line" ] && line="$(tail -n 1 "$dir/states")"
    printf '%s\n' "$line"
    exit 0
  fi
  ;;
esac
echo "fake gh: unhandled call: $*" >&2
exit 3
SHIM
  chmod +x "$dir/gh"
  echo "$dir"
}

# The script under test, with that case's fake `gh` and nothing else changed.
run_case() { # run_case <dir> [args...]
  local dir="$1"
  shift
  PATH="$dir:$PATH" GH_SHIM_DIR="$dir" \
    "$SCRIPT" "$@" 2>&1
}

said() { # said <name> <text> <output>
  case "$3" in
  *"$2"*) ok "$1" ;;
  *) bad "$1 — wanted '$2', got: $3" ;;
  esac
}

view_calls() { # view_calls <dir>
  grep -c "^pr view" "$1/calls.log" 2>/dev/null || true
}

printf '\033[1mwait-for-armed-merge — the armed PR actually landing gets noticed\033[0m\n'

# ── It merges on the first poll. ─────────────────────────────────────────────
dir="$(new_case merged_first MERGED)"
out="$(run_case "$dir" bot/version-sync --poll-seconds 0)"
rc=$?
if [ "$rc" -eq 0 ]; then
  ok "a PR already merged exits 0"
else
  bad "a merged PR exited $rc: $out"
fi
said "and reports MERGED" "MERGED" "$out"

# ── It merges after sitting open a few polls — the loop actually polls. ─────
dir="$(new_case merged_later OPEN OPEN OPEN MERGED)"
out="$(run_case "$dir" bot/version-sync --timeout-seconds 30 --poll-seconds 1)"
rc=$?
if [ "$rc" -eq 0 ]; then
  ok "a PR that merges on a later poll exits 0"
else
  bad "a later merge exited $rc: $out"
fi
said "and still reports MERGED" "MERGED" "$out"
if [ "$(view_calls "$dir")" -ge 4 ]; then
  ok "and it actually polled — $(view_calls "$dir") calls, not one"
else
  bad "it did not poll: $(cat "$dir/calls.log")"
fi

# ── It is closed without merging. ────────────────────────────────────────────
dir="$(new_case closed OPEN CLOSED)"
out="$(run_case "$dir" bot/version-sync --timeout-seconds 10 --poll-seconds 1)"
rc=$?
if [ "$rc" -eq 0 ]; then
  ok "a closed PR exits 0"
else
  bad "a closed PR exited $rc: $out"
fi
said "and reports CLOSED, not a merge" "CLOSED" "$out"

# ── The actual gap this script closes: still open when asked. ───────────────
# `--poll-seconds 0` asks once and gives up, which is exactly a PR still
# sitting OPEN after this script's caller has already waited as long as it
# is willing to — the shape the workflow's own bounded wait degrades to
# once its budget runs out.
dir="$(new_case timeout OPEN)"
out="$(run_case "$dir" bot/version-sync --poll-seconds 0)"
rc=$?
if [ "$rc" -eq 0 ]; then
  ok "a PR that never resolves before the deadline still exits 0"
else
  bad "a timed-out wait exited $rc: $out"
fi
said "and reports TIMEOUT, not a false MERGED" "TIMEOUT" "$out"

# ── `gh` cannot even be asked once. ──────────────────────────────────────────
dir="$(new_case unreadable OPEN)"
touch "$dir/view-fails"
out="$(run_case "$dir" bot/version-sync --poll-seconds 0)"
rc=$?
if [ "$rc" -eq 0 ] && [ -z "${out##*UNKNOWN*}" ]; then
  ok "a state that can never be read is UNKNOWN, not a failure"
else
  bad "an unreadable state exited $rc: $out"
fi

nogh="$work/nogh"
mkdir -p "$nogh"
bash_bin="$(command -v bash)"
out="$(PATH="$nogh" "$bash_bin" "$SCRIPT" bot/version-sync --poll-seconds 0 2>&1)"
rc=$?
if [ "$rc" -eq 0 ] && [ -z "${out##*UNKNOWN*}" ]; then
  ok "no gh at all is UNKNOWN, not a failure"
else
  bad "a missing gh exited $rc: $out"
fi

# ── A caller mistake is loud. ────────────────────────────────────────────────
out="$("$SCRIPT" 2>&1)"
if [ $? -eq 2 ]; then
  ok "a missing branch argument exits 2"
else
  bad "a missing branch did not exit 2: $out"
fi

out="$("$SCRIPT" bot/version-sync --timeout-seconds banana 2>&1)"
if [ $? -eq 2 ]; then
  ok "a flag given a word instead of a number exits 2"
else
  bad "a bad number did not exit 2: $out"
fi

out="$("$SCRIPT" bot/version-sync --nonsense 2>&1)"
if [ $? -eq 2 ]; then
  ok "an unknown flag exits 2"
else
  bad "an unknown flag did not exit 2: $out"
fi

printf '\n'
if [ "$fail" -eq 0 ]; then
  printf '\033[32mwait-for-armed-merge-test: OK\033[0m — %d checks passed\n' "$pass"
  exit 0
fi
printf '\033[31mwait-for-armed-merge-test: FAILED\033[0m — %d passed, %d failed\n' "$pass" "$fail"
exit 1
