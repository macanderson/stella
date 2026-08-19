#!/usr/bin/env bash
#
# Tests for install-zsh-completions.sh.
#
#   ./scripts/test-install-zsh-completions.sh
#
# Run it after touching that script. Not part of `make gate`: nothing here
# compiles, but the installer writes into a home directory and the gate has no
# business doing that — the same call `make impacted-test` makes.
#
# ── What is actually under test ──────────────────────────────────────────────
#
# The installer runs at the tail of every `make build-release`, so its failure
# branches matter more than its happy path: it must never replace a working
# `_stella` with something that is not one, and it must never fail a build. A
# suite that only checked "the file appeared" would pass just as happily on a
# script that wrote whatever the binary printed, including a panic message —
# which zsh would load, and which would then complete nothing with no error to
# read.
#
# So every case below is a way the install can go wrong: a binary that exits
# non-zero, one that prints nothing, one that prints something that is not a
# completion function, and a missing binary — each of which must leave a good
# previously-installed file exactly where it was.
#
# $HOME is redirected into the temp tree throughout, because the installer
# deletes ~/.zcompdump* on a real change and this suite is not entitled to the
# runner's. The stub binary is a shell script: what is under test is the
# installer's verdict, not clap's rendering.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/install-zsh-completions.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

indent() {
  while IFS= read -r line; do printf '       %s\n' "$line"; done
}

# A stand-in for the shipping binary. $1 = path, $2 = a file holding what
# `completions zsh` should write to stdout, $3 = the exit code it writes it
# with. The payload rides in a file rather than in the stub's source, so a
# fixture containing quotes or `$` cannot rewrite the stub.
write_fake_stella() { # <path> <payload-file> <rc>
  cat >"$1" <<EOF
#!/usr/bin/env bash
[ "\${1:-}" = "completions" ] || exit 64
cat "$2"
exit $3
EOF
  chmod +x "$1"
}

# One assertion form: run the installer in an isolated home and compare its
# verdict. An expected failure must also name its reason, because "exit 1" is
# satisfied by a typo in the installer just as well as by the defect the case
# is about.
check() { # <name> <expect-pass|expect-fail> <want-substring> <bin> <dir> [extra-args...]
  local name="$1" expect="$2" want="$3" bin="$4" dir="$5"
  shift 5
  local out rc
  out="$(HOME="$TMP/home" ZSH="$TMP/home/.oh-my-zsh" STELLA_BIN="$bin" \
    "$SCRIPT" --dir "$dir" "$@" 2>&1)"
  rc=$?
  if [ "$expect" = "expect-pass" ] && [ "$rc" -ne 0 ]; then
    fail=$((fail + 1))
    echo "FAIL $name — expected success, got exit ${rc}:"
    echo "$out" | indent
    return
  fi
  if [ "$expect" = "expect-fail" ] && [ "$rc" -eq 0 ]; then
    fail=$((fail + 1))
    echo "FAIL $name — expected failure, but the installer accepted it:"
    echo "$out" | indent
    return
  fi
  case "$out" in
    *"$want"*)
      pass=$((pass + 1))
      echo "ok   $name"
      ;;
    *)
      fail=$((fail + 1))
      echo "FAIL $name — wrong reason (wanted '${want}'):"
      echo "$out" | indent
      ;;
  esac
}

assert_file() { # <name> <path> <want-substring>
  local name="$1" path="$2" want="$3"
  if [ ! -f "$path" ]; then
    fail=$((fail + 1))
    echo "FAIL $name — ${path} does not exist"
    return
  fi
  case "$(cat "$path")" in
    *"$want"*)
      pass=$((pass + 1))
      echo "ok   $name"
      ;;
    *)
      fail=$((fail + 1))
      echo "FAIL $name — ${path} does not contain '${want}':"
      indent < "$path"
      ;;
  esac
}

mkdir -p "$TMP/home" "$TMP/bin" "$TMP/payload" "$TMP/fpath"
DEST="$TMP/fpath"
TARGET="$DEST/_stella"

# What clap_complete emits, trimmed to a first line and a body: the first line
# is what the installer checks, the rest is what must land on disk.
cat >"$TMP/payload/good" <<'EOF'
#compdef stella
_stella() { _arguments "1: :(run models)" }
EOF
: >"$TMP/payload/empty"
echo "thread 'main' panicked at src/main.rs:1" >"$TMP/payload/garbage"

write_fake_stella "$TMP/bin/good" "$TMP/payload/good" 0
write_fake_stella "$TMP/bin/empty" "$TMP/payload/empty" 0
write_fake_stella "$TMP/bin/garbage" "$TMP/payload/garbage" 0
write_fake_stella "$TMP/bin/broken" "$TMP/payload/good" 1
: >"$TMP/bin/not-executable"

# ── The happy path, once ─────────────────────────────────────────────────────
check "installs the completion function" \
  expect-pass "zsh completions installed" "$TMP/bin/good" "$DEST"
assert_file "installed file is the completion function" "$TARGET" "_stella()"

# A second identical run must not rewrite the file — the message is the only
# way a human can tell "already current" from "reinstalled every time".
check "second run reports it is already current" \
  expect-pass "already current" "$TMP/bin/good" "$DEST"

# ── Every way the install can go wrong, with a good file already in place ────
check "refuses a binary that exits non-zero" \
  expect-fail "failed" "$TMP/bin/broken" "$DEST"
assert_file "a failed binary leaves the good file alone" "$TARGET" "_stella()"

check "refuses empty output" \
  expect-fail "produced nothing" "$TMP/bin/empty" "$DEST"
assert_file "empty output leaves the good file alone" "$TARGET" "_stella()"

check "refuses output that is not a completion function" \
  expect-fail "did not produce a zsh completion function" "$TMP/bin/garbage" "$DEST"
assert_file "garbage output leaves the good file alone" "$TARGET" "_stella()"

check "refuses a STELLA_BIN that is not executable" \
  expect-fail "not executable" "$TMP/bin/not-executable" "$DEST"

# ── --best-effort: the build must not go red ─────────────────────────────────
# This is the mode `make build-release` uses, so it is the branch that decides
# whether a broken shell nicety can break a build.
check "--best-effort downgrades a failure to a warning" \
  expect-pass "warning: zsh completions not installed" \
  "$TMP/bin/garbage" "$DEST" --best-effort
assert_file "--best-effort leaves the good file alone" "$TARGET" "_stella()"

# ── The off-fpath note ───────────────────────────────────────────────────────
# A temp directory is on no one's fpath, so an install into one must say so:
# writing a file zsh never reads is the failure that otherwise looks like
# success.
rm -f "$TARGET"
check "names the fpath line when the directory is not on fpath" \
  expect-pass 'fpath=(' "$TMP/bin/good" "$DEST"

# ── The zcompdump sweep ──────────────────────────────────────────────────────
# compinit caches the set of completion functions it found, so a new _stella
# stays invisible until the dump is rebuilt. Only a real change may clear it.
rm -f "$TARGET"
: >"$TMP/home/.zcompdump"
check "clears the stale zcompdump on a real change" \
  expect-pass "cleared 1 stale zcompdump" "$TMP/bin/good" "$DEST"
if [ -e "$TMP/home/.zcompdump" ]; then
  fail=$((fail + 1))
  echo "FAIL zcompdump was reported cleared but still exists"
else
  pass=$((pass + 1))
  echo "ok   zcompdump is gone"
fi

: >"$TMP/home/.zcompdump"
check "leaves the zcompdump alone when nothing changed" \
  expect-pass "already current" "$TMP/bin/good" "$DEST"
if [ -e "$TMP/home/.zcompdump" ]; then
  pass=$((pass + 1))
  echo "ok   an unchanged install keeps the zcompdump"
else
  fail=$((fail + 1))
  echo "FAIL an unchanged install deleted the zcompdump anyway"
fi

echo
echo "${pass} passed, ${fail} failed"
[ "$fail" -eq 0 ]
