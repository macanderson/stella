#!/usr/bin/env bash
#
# Tests for scripts/sync-versions.sh's lockfile half (#3336).
#
# The release path stamps a new version into the manifests and then has to
# leave `Cargo.lock` resolving against them. When it does not, the required CI
# step "Cargo.lock is in sync with the manifests" goes red on `main` and every
# PR opened afterwards inherits the red gate — which is what happened on
# 2026-08-16, when the 0.9.50 sync left `stella-plugin` alone at 0.9.49.
#
# Two separate claims are proven here, because the incident report and the code
# disagree about which one was at fault and only one of them is testable by
# inspection:
#
#   1. `cargo update --workspace` DOES pick up a workspace member that is not
#      in the previous lock. The suspicion in #3336 was that the sync enumerates
#      a stale crate list, or relocks before a new member is visible. It does
#      not: `a member missing from the lock is added` below drives the real
#      script over a fixture in exactly that state and asserts the result passes
#      `cargo metadata --locked`.
#   2. The sync REFUSES to hand back a tree whose lock does not resolve. That is
#      the durable half, and it is the half that was missing: nothing in the
#      release path asserted the invariant CI checks, so the failure was only
#      ever discovered from a red `main`.
#
# Claim 2 needs a world where the relock silently does nothing — otherwise the
# fix under test can never be reached. `stub_cargo` below is that world: a
# `cargo` earlier on PATH that no-ops `cargo update` and delegates everything
# else to the real one. It models the postulated defect rather than asserting
# its absence, and `the stub alone does not fail a clean tree` is the control
# that keeps the stub from being the thing under test.
#
# Hermetic: every case builds a throwaway path-only workspace under mktemp with
# no registry dependencies, and CARGO_NET_OFFLINE pins it away from the network.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
sync="$repo_root/scripts/sync-versions.sh"

if ! command -v cargo >/dev/null 2>&1; then
  echo "skip test-release-lockfile — no cargo toolchain on PATH"
  exit 0
fi
if ! command -v perl >/dev/null 2>&1; then
  echo "skip test-release-lockfile — no perl on PATH (sync-versions.sh needs it)"
  exit 0
fi

real_cargo="$(command -v cargo)"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Path-only members resolve without an index; pin that so a test run cannot
# depend on the network being up (or on the registry cache being warm).
export CARGO_NET_OFFLINE=1

pass=0
fail=0

# A `cargo` that answers `update` with success and does nothing, and passes
# every other subcommand to the real one. This is the whole point of the
# fail-loud case: with the relock neutralized, a skewed lock survives the sync,
# and the only thing left that can catch it is the verification under test.
stub_cargo() {
  mkdir -p "$tmp/stubbin"
  cat >"$tmp/stubbin/cargo" <<STUB
#!/usr/bin/env bash
if [ "\${1:-}" = "update" ]; then exit 0; fi
exec "$real_cargo" "\$@"
STUB
  chmod +x "$tmp/stubbin/cargo"
  printf '%s' "$tmp/stubbin"
}

# Build a minimal, network-free workspace at $1 whose members are at version
# $2, write a matching lock, and add the homebrew formula sync-versions.sh
# rewrites. Every member named after the first is created on disk and listed in
# `members` but deliberately left OUT of the lock, which is the state a
# freshly-merged new crate leaves behind.
make_workspace() {
  dir="$1"
  version="$2"
  shift 2

  mkdir -p "$dir/crates/demo/src" "$dir/packaging/homebrew"
  cat >"$dir/Cargo.toml" <<TOML
[workspace]
members = ["crates/demo"]
resolver = "2"

[workspace.package]
version = "$version"
edition = "2021"
TOML
  cat >"$dir/crates/demo/Cargo.toml" <<'TOML'
[package]
name = "demo"
version.workspace = true
edition.workspace = true
TOML
  echo 'pub fn demo() {}' >"$dir/crates/demo/src/lib.rs"
  cat >"$dir/packaging/homebrew/stella.rb" <<TOML
class Stella < Formula
  url "https://github.com/macanderson/stella.git", tag: "v$version"
  version "$version"
end
TOML

  # The lock is generated while only `demo` is a member, so the extra members
  # below are absent from it — the shape a new crate arrives in.
  (cd "$dir" && "$real_cargo" generate-lockfile --offline >/dev/null 2>&1)

  for member in "$@"; do
    mkdir -p "$dir/crates/$member/src"
    cat >"$dir/crates/$member/Cargo.toml" <<TOML
[package]
name = "$member"
version.workspace = true
edition.workspace = true
TOML
    echo 'pub fn it() {}' >"$dir/crates/$member/src/lib.rs"
    perl -pi -e "s{^members = \\[\"crates/demo\"\\]}{members = [\"crates/demo\", \"crates/$member\"]}" \
      "$dir/Cargo.toml"
  done
}

# Run the real sync script over a fixture, asserting the exit code and that the
# combined output contains `needle` (pass an empty needle to skip that half).
# PATH_PREFIX, when set, is prepended to PATH for the run.
expect_sync() {
  name="$1"
  dir="$2"
  version="$3"
  want_code="$4"
  needle="$5"

  set +e
  out="$(cd "$dir" && PATH="${PATH_PREFIX:+$PATH_PREFIX:}$PATH" \
    NEW_VERSION="$version" "$sync" 2>&1)"
  code=$?
  set -e

  if [ "$code" -ne "$want_code" ]; then
    echo "FAIL  $name — exit $code, wanted $want_code"
    printf '      %s\n' "$out"
    fail=$((fail + 1))
    return
  fi
  if [ -n "$needle" ]; then
    case "$out" in
    *"$needle"*) ;;
    *)
      echo "FAIL  $name — output did not contain: $needle"
      printf '      %s\n' "$out"
      fail=$((fail + 1))
      return
      ;;
    esac
  fi
  echo "ok    $name"
  pass=$((pass + 1))
}

# Assert `cargo metadata --locked` accepts the fixture — the exact question
# ci.yml's "Cargo.lock is in sync with the manifests" step asks.
expect_locked() {
  name="$1"
  dir="$2"

  set +e
  out="$(cd "$dir" && "$real_cargo" metadata --locked --color never --format-version 1 2>&1 >/dev/null)"
  code=$?
  set -e
  if [ "$code" -eq 0 ]; then
    echo "ok    $name"
    pass=$((pass + 1))
  else
    echo "FAIL  $name — cargo metadata --locked rejected the synced tree"
    printf '      %s\n' "$out"
    fail=$((fail + 1))
  fi
}

# Assert every `[[package]]` version in the fixture's lock reads $2. The quote
# in the pattern is load-bearing: the lock's own format line is a bare
# `version = 4`, and counting it makes this assertion permanently off by one.
expect_lock_versions() {
  name="$1"
  dir="$2"
  version="$3"

  stale="$(grep -c '^version = "' "$dir/Cargo.lock" || true)"
  fresh="$(grep -c "^version = \"$version\"$" "$dir/Cargo.lock" || true)"
  if [ "$stale" -gt 0 ] && [ "$stale" -eq "$fresh" ]; then
    echo "ok    $name"
    pass=$((pass + 1))
  else
    echo "FAIL  $name — $fresh of $stale lock entries read $version"
    printf '      %s\n' "$(cat "$dir/Cargo.lock")"
    fail=$((fail + 1))
  fi
}

# ── an ordinary bump relocks and stays resolvable ─────────────────────────
make_workspace "$tmp/plain" "0.2.3"
expect_sync "an ordinary bump succeeds" "$tmp/plain" "0.2.4" 0 "OK — Cargo.lock resolves"
expect_locked "an ordinary bump leaves the lock resolvable" "$tmp/plain"
expect_lock_versions "an ordinary bump rewrites every lock entry" "$tmp/plain" "0.2.4"

# ── the #3336 hypothesis, tested rather than assumed ──────────────────────
# `newcrate` is a member of the manifest and absent from the lock — the state a
# just-merged new crate leaves behind, and the state the issue suspected the
# sync of mishandling. If `cargo update --workspace` enumerated a stale crate
# list, or relocked before the new member was visible, this is where it would
# show: the sync would exit 0 with `newcrate` missing or stale.
make_workspace "$tmp/newmember" "0.2.3" newcrate
expect_sync "a member missing from the lock is added" "$tmp/newmember" "0.2.4" 0 \
  "OK — Cargo.lock resolves"
expect_locked "a new member leaves the lock resolvable" "$tmp/newmember"
expect_lock_versions "a new member is stamped with the rest" "$tmp/newmember" "0.2.4"
if grep -q '^name = "newcrate"$' "$tmp/newmember/Cargo.lock"; then
  echo "ok    the new member reached the lock"
  pass=$((pass + 1))
else
  echo "FAIL  the new member never reached the lock"
  fail=$((fail + 1))
fi

# ── the durable half: a lock that does not resolve fails the sync ─────────
# With `cargo update` neutralized, the bumped manifest and the untouched lock
# disagree — precisely the tree that reddened main. Before #3336 the script
# exited 0 here and the caller committed it.
PATH_PREFIX="$(stub_cargo)"
make_workspace "$tmp/wedged" "0.2.3"
expect_sync "a lock that did not relock fails the sync" "$tmp/wedged" "0.2.4" 1 \
  "refusing to author a version-sync"
expect_sync "and the failure names the lock, not just the exit code" "$tmp/wedged" "0.2.4" 1 \
  "out of sync with the manifests"

# ── control: the stub is not what fails the run ───────────────────────────
# Same neutered `cargo update`, but the version is unchanged, so the lock still
# matches the manifest and the sync must succeed. Without this case, the two
# assertions above are consistent with "the stub breaks everything", which is
# not evidence for the check under test.
make_workspace "$tmp/control" "0.2.3"
expect_sync "the stub alone does not fail a clean tree" "$tmp/control" "0.2.3" 0 \
  "OK — Cargo.lock resolves"
unset PATH_PREFIX

echo
echo "test-release-lockfile: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
