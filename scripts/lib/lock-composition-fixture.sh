#!/usr/bin/env bash
#
# The two-branch lock clash, built as a throwaway git repo (`#5943`).
#
# Shared by `scripts/test-merge-lockfile-sync.sh` and
# `scripts/test-recheck-lock-compositions.sh`. Sourced, never run.
#
# `build_lock_fixture DIR` leaves DIR on `main` with four branches.
#
# `main` holds every crate at 0.2.0 and a fresh lock. That is the sync.
# `feature` adds the `mike` crate at 0.1.0. It was cut before the sync.
# `caught-up` adds the `november` crate. It was cut after the sync.
# `docs-only` writes a README and no manifest.
#
# Merge `feature` into `main` and the break shows up. Git finds no clash.
# The merged root says 0.2.0. The merged lock still says `mike` 0.1.0.
# The last two branches are the controls. They must stay green.

# Each crate depends on the ones named after it. That gives its lock entry a
# list of deps. A short entry would sit next to the version line of the next
# crate. Git would call that a clash. That is not the shape we want.
_lock_fixture_member() {
  dir="$1"
  name="$2"
  shift 2
  mkdir -p "$dir/crates/$name/src"
  {
    echo '[package]'
    echo "name = \"$name\""
    echo 'version.workspace = true'
    echo 'edition.workspace = true'
    echo
    echo '[dependencies]'
    for dep in "$@"; do
      echo "$dep = { path = \"../$dep\" }"
    done
  } >"$dir/crates/$name/Cargo.toml"
  echo "pub fn hello() {}" >"$dir/crates/$name/src/lib.rs"
}

_lock_fixture_root() {
  dir="$1"
  version="$2"
  shift 2
  {
    echo '[workspace]'
    echo 'resolver = "2"'
    echo 'members = ['
    for member in "$@"; do
      echo "  \"crates/$member\","
    done
    echo ']'
    echo
    echo '[workspace.package]'
    echo "version = \"$version\""
    echo 'edition = "2021"'
  } >"$dir/Cargo.toml"
}

_lock_fixture_relock() { (cd "$1" && cargo generate-lockfile --offline >/dev/null 2>&1); }

# git with a name of its own. A box with no global one still runs this.
lock_fixture_git() { git -C "$1" -c user.email=t@example.com -c user.name=T "${@:2}"; }

build_lock_fixture() {
  dir="$1"
  mkdir -p "$dir"
  git -C "$dir" init -q -b main

  _lock_fixture_root "$dir" "0.1.0" alpha kilo zulu
  _lock_fixture_member "$dir" alpha kilo zulu
  _lock_fixture_member "$dir" kilo zulu
  _lock_fixture_member "$dir" zulu
  _lock_fixture_relock "$dir"
  lock_fixture_git "$dir" add -A
  lock_fixture_git "$dir" commit -qm "base"

  # Right about all it can see. `mike` sorts between `kilo` and `zulu`. Every
  # version in its lock matches every version in its manifests.
  lock_fixture_git "$dir" checkout -q -b feature
  _lock_fixture_root "$dir" "0.1.0" alpha kilo mike zulu
  _lock_fixture_member "$dir" mike zulu
  _lock_fixture_relock "$dir"
  lock_fixture_git "$dir" add -A
  lock_fixture_git "$dir" commit -qm "add the mike crate"

  lock_fixture_git "$dir" checkout -q -b docs-only main
  echo "hello" >"$dir/README.md"
  lock_fixture_git "$dir" add -A
  lock_fixture_git "$dir" commit -qm "add a readme"

  lock_fixture_git "$dir" checkout -q main
  _lock_fixture_root "$dir" "0.2.0" alpha kilo zulu
  _lock_fixture_relock "$dir"
  lock_fixture_git "$dir" add -A
  lock_fixture_git "$dir" commit -qm "sync versions to 0.2.0"

  # Cut after the sync. Its new crate is locked at the version `main` now
  # holds. It writes the lock and still merges clean.
  lock_fixture_git "$dir" checkout -q -b caught-up main
  _lock_fixture_root "$dir" "0.2.0" alpha kilo november zulu
  _lock_fixture_member "$dir" november zulu
  _lock_fixture_relock "$dir"
  lock_fixture_git "$dir" add -A
  lock_fixture_git "$dir" commit -qm "add the november crate"

  # `main` moves once more. Now `caught-up` is behind it and has a real merge
  # to make, not a fast-forward the guard skips.
  lock_fixture_git "$dir" checkout -q main
  echo "the crates are at 0.2.0" >"$dir/NOTES.md"
  lock_fixture_git "$dir" add -A
  lock_fixture_git "$dir" commit -qm "note the version"
}
