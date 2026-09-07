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
#
# The clash has to stay away, or the fixture tests the wrong thing. Git calls
# two edits a clash when they land near each other, and how near depends on
# the git build. So `mike` is added where its new lines sit far from any line
# the sync moved. Six `noop` crates pin their own versions. The sync leaves
# those alone, and they pad both the lock and the member list. The builder
# then tries the merge and says so if it ever clashes again.

_lock_fixture_pad="noop1 noop2 noop3 noop4 noop5 noop6"

# A crate at the shared workspace version. It depends on every crate named
# after it, which gives its lock entry a long list of deps.
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

# A crate that pins its own version. The sync never rewrites its lines, so it
# is safe padding between an added entry and a moved one.
_lock_fixture_pinned() {
  dir="$1"
  name="$2"
  mkdir -p "$dir/crates/$name/src"
  {
    echo '[package]'
    echo "name = \"$name\""
    echo 'version = "1.0.0"'
    echo 'edition.workspace = true'
    echo
    echo '[dependencies]'
    echo 'zulu = { path = "../zulu" }'
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
  git -C "$dir" init -q

  # shellcheck disable=SC2086 # the pad list is a word list on purpose.
  set -- alpha kilo $_lock_fixture_pad zulu
  _lock_fixture_root "$dir" "0.1.0" "$@"
  # shellcheck disable=SC2086
  _lock_fixture_member "$dir" alpha kilo $_lock_fixture_pad zulu
  # shellcheck disable=SC2086
  _lock_fixture_member "$dir" kilo $_lock_fixture_pad zulu
  for pad in $_lock_fixture_pad; do
    _lock_fixture_pinned "$dir" "$pad"
  done
  _lock_fixture_member "$dir" zulu
  _lock_fixture_relock "$dir"
  lock_fixture_git "$dir" add -A
  lock_fixture_git "$dir" commit -qm "base"
  lock_fixture_git "$dir" branch -q -M main

  # Right about all it can see. `mike` sorts between `kilo` and `noop1`. Every
  # version in its lock matches every version in its manifests.
  lock_fixture_git "$dir" checkout -q -b feature
  # shellcheck disable=SC2086
  _lock_fixture_root "$dir" "0.1.0" alpha kilo mike $_lock_fixture_pad zulu
  _lock_fixture_member "$dir" mike zulu
  _lock_fixture_relock "$dir"
  lock_fixture_git "$dir" add -A
  lock_fixture_git "$dir" commit -qm "add the mike crate"

  lock_fixture_git "$dir" checkout -q -b docs-only main
  echo "hello" >"$dir/README.md"
  lock_fixture_git "$dir" add -A
  lock_fixture_git "$dir" commit -qm "add a readme"

  lock_fixture_git "$dir" checkout -q main
  # shellcheck disable=SC2086
  _lock_fixture_root "$dir" "0.2.0" alpha kilo $_lock_fixture_pad zulu
  _lock_fixture_relock "$dir"
  lock_fixture_git "$dir" add -A
  lock_fixture_git "$dir" commit -qm "sync versions to 0.2.0"

  # Cut after the sync. Its new crate is locked at the version `main` now
  # holds. It writes the lock and still merges clean.
  lock_fixture_git "$dir" checkout -q -b caught-up main
  # shellcheck disable=SC2086
  _lock_fixture_root "$dir" "0.2.0" alpha kilo $_lock_fixture_pad november zulu
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

  # The whole point is a pair git merges with no clash. Say so here, where the
  # cause is plain, rather than letting a case fail for the wrong reason.
  if ! lock_fixture_git "$dir" merge --no-commit --no-ff feature >/dev/null 2>&1; then
    echo "lock-composition-fixture: this git calls the two branches a clash." >&2
    echo "     The fixture then builds some other shape, not the one under" >&2
    echo "     test. Pad the added entry further from the lines the sync" >&2
    echo "     moves." >&2
    lock_fixture_git "$dir" merge --abort >/dev/null 2>&1 || true
    lock_fixture_git "$dir" reset --hard >/dev/null 2>&1 || true
    return 1
  fi
  lock_fixture_git "$dir" merge --abort >/dev/null 2>&1 || true
  lock_fixture_git "$dir" reset --hard >/dev/null
  lock_fixture_git "$dir" checkout -q main
}
