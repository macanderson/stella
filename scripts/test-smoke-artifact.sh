#!/usr/bin/env bash
#
# Tests for smoke-artifact.sh (#1626).
#
#   ./scripts/test-smoke-artifact.sh
#
# Run it after touching that script. Not part of `make gate`: it builds a
# dozen synthetic tarballs, which is a second or two not worth adding to every
# push — the same call `make impacted-test` makes.
#
# ── Why a synthetic artifact instead of a real one ───────────────────────────
#
# The script under test is the release pipeline's only assertion that the
# published binary RUNS, and it is worth exactly as much as its failure
# branches. A suite that only feeds it a good artifact proves the happy path
# and nothing else — and the happy path is the one case the release workflow
# would exercise on its own anyway.
#
# So every case here is a *specific* way a release can be broken, built on
# purpose: a tarball whose binary no longer matches the checksum shipped
# beside it, an archive with no binary in it, a binary that cannot start, one
# that starts but whose commands are broken, one stamped with the wrong
# version. Each must fail, and must fail for its own reason.
#
# Building a real stella takes a full-LTO release build; the fixture binary is
# a shell script that answers `--version` and `models`, because what is under
# test is the harness's verdict, not the compiler's output. The real binary is
# what the release workflow feeds this script.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/smoke-artifact.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

# Indent a captured failure so it reads as evidence under its case rather than
# as more test output.
indent() {
  while IFS= read -r line; do printf '       %s\n' "$line"; done
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# A stand-in for the release binary. $1 = destination, $2 = version it reports,
# $3 = exit code for `--version`, $4 = exit code for `models`.
write_fake_stella() { # <path> <version> <version-rc> <models-rc>
  cat >"$1" <<EOF
#!/usr/bin/env bash
case "\${1:-}" in
  --version) echo "stella $2 / fixture-target / release"; exit $3 ;;
  models)    echo "provider  model"; exit $4 ;;
esac
exit 64
EOF
  chmod +x "$1"
}

# Build an artifact directory the way the build job's upload leaves it: one
# tarball, one checksum naming the artifact stem. $1 = case name, remaining
# args tune the fixture. Echoes the directory.
make_artifact() { # <case> <version> <version-rc> <models-rc>
  local case_name="$1" version="$2" vrc="${3:-0}" mrc="${4:-0}"
  local dir="$TMP/$case_name" stem="stella-${version}-fixture-target"
  mkdir -p "$dir/build/$stem"
  write_fake_stella "$dir/build/$stem/stella" "$version" "$vrc" "$mrc"
  # The checksum is taken here, before any case corrupts the binary — exactly
  # as the real pipeline takes it before packaging.
  sha256_of "$dir/build/$stem/stella" >"$dir/${stem}.sha256"
  ( cd "$dir/build" && tar -czf "../${stem}.tar.gz" "$stem" )
  rm -rf "$dir/build"
  echo "$dir"
}

# One assertion form: run the script and compare its verdict. An expected
# failure must also name its reason, because "exit 1" is satisfied by a typo
# in the script just as well as by the defect the case is about.
check() { # <name> <expect-pass|expect-fail> <dir> [substring-of-error]
  local name="$1" expect="$2" dir="$3" want="${4:-}"
  local out rc
  out="$("$SCRIPT" "$dir" 2>&1)"
  rc=$?
  if [ "$expect" = "expect-pass" ]; then
    if [ "$rc" -eq 0 ]; then
      pass=$((pass + 1)); echo "ok   $name"
    else
      fail=$((fail + 1)); echo "FAIL $name — expected success, got exit ${rc}:"; echo "$out" | indent
    fi
    return
  fi
  if [ "$rc" -eq 0 ]; then
    fail=$((fail + 1)); echo "FAIL $name — expected failure, but the script passed a broken artifact:"; echo "$out" | indent
    return
  fi
  case "$out" in
    *"$want"*) pass=$((pass + 1)); echo "ok   $name" ;;
    *) fail=$((fail + 1)); echo "FAIL $name — failed for the wrong reason (wanted '${want}'):"; echo "$out" | indent ;;
  esac
}

# ── G: a well-formed artifact passes ─────────────────────────────────────────
# Without this the whole suite is satisfiable by a script that always fails.
good="$(make_artifact good 1.2.3)"
check "G1 a well-formed artifact passes" expect-pass "$good"

# ── C: the checksum half of a release must describe the binary users get ─────
tampered="$(make_artifact tampered 1.2.3)"
(
  cd "$tampered" || exit 1
  mkdir -p repack && tar -xzf ./*.tar.gz -C repack
  # Append a byte: same name, same mode, different bytes. This is the shape of
  # a packaging step that archives a binary other than the one it hashed.
  echo "# tampered" >>repack/stella-1.2.3-fixture-target/stella
  rm -f ./*.tar.gz
  ( cd repack && tar -czf ../stella-1.2.3-fixture-target.tar.gz stella-1.2.3-fixture-target )
  rm -rf repack
)
check "C1 a binary that does not match its published checksum is rejected" \
  expect-fail "$tampered" "NOT the binary whose checksum ships beside it"

empty_sum="$(make_artifact empty_sum 1.2.3)"
: >"$empty_sum/stella-1.2.3-fixture-target.sha256"
check "C2 an empty checksum file is rejected, not treated as a match" \
  expect-fail "$empty_sum" "is empty"

# ── P: the archive itself ────────────────────────────────────────────────────
no_bin="$(make_artifact no_bin 1.2.3)"
(
  cd "$no_bin" || exit 1
  mkdir -p repack/stella-1.2.3-fixture-target
  cp "$repo_root/README.md" repack/stella-1.2.3-fixture-target/
  rm -f ./*.tar.gz
  ( cd repack && tar -czf ../stella-1.2.3-fixture-target.tar.gz stella-1.2.3-fixture-target )
  rm -rf repack
)
check "P1 an archive with no binary in it is rejected" \
  expect-fail "$no_bin" "contains no"

not_exec="$(make_artifact not_exec 1.2.3)"
(
  cd "$not_exec" || exit 1
  mkdir -p repack && tar -xzf ./*.tar.gz -C repack
  chmod -x repack/stella-1.2.3-fixture-target/stella
  rm -f ./*.tar.gz
  ( cd repack && tar -czf ../stella-1.2.3-fixture-target.tar.gz stella-1.2.3-fixture-target )
  rm -rf repack
)
check "P2 an archive that lost its mode bits is rejected" \
  expect-fail "$not_exec" "not executable"

# ── R: the point of the job — the binary has to actually run ─────────────────
# A binary that segfaults on startup reproduces perfectly, so every check above
# this line passes for it.
dead="$(make_artifact dead 1.2.3 1 0)"
check "R1 a binary that cannot start is rejected" \
  expect-fail "$dead" "does not start"

broken_cmd="$(make_artifact broken_cmd 1.2.3 0 1)"
check "R2 a binary that starts but whose commands fail is rejected" \
  expect-fail "$broken_cmd" "cannot reach a working command"

# The artifact is named 1.2.3; the binary inside reports 9.9.9.
mis_stamped="$TMP/mis_stamped"
mkdir -p "$mis_stamped/build/stella-1.2.3-fixture-target"
write_fake_stella "$mis_stamped/build/stella-1.2.3-fixture-target/stella" "9.9.9" 0 0
sha256_of "$mis_stamped/build/stella-1.2.3-fixture-target/stella" >"$mis_stamped/stella-1.2.3-fixture-target.sha256"
( cd "$mis_stamped/build" && tar -czf ../stella-1.2.3-fixture-target.tar.gz stella-1.2.3-fixture-target )
rm -rf "$mis_stamped/build"
check "R3 a binary stamped with a different version than its artifact is rejected" \
  expect-fail "$mis_stamped" "assembled from a different build"

# ── A: the artifact directory's own shape ────────────────────────────────────
# `ls *.tar.gz | head -1` would silently smoke-test one leg and pass the other.
two="$(make_artifact two 1.2.3)"
cp "$two/stella-1.2.3-fixture-target.tar.gz" "$two/stella-1.2.3-other-target.tar.gz"
check "A1 two tarballs in one artifact directory are rejected, not silently narrowed" \
  expect-fail "$two" "expected exactly one"

none="$TMP/none"
mkdir -p "$none"
check "A2 an empty artifact directory is rejected" expect-fail "$none" "expected exactly one"

check "A3 a missing artifact directory is rejected" expect-fail "$TMP/does-not-exist" "does not exist"

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
