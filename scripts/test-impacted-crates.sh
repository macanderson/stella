#!/usr/bin/env bash
#
# Tests for impacted-crates.sh (#1135).
#
#   ./scripts/test-impacted-crates.sh
#
# Run it after touching that script. Not part of `make gate`: it runs
# `cargo metadata` against a synthetic workspace a dozen times, which is a
# couple of seconds not worth adding to every push — the same call `make
# dev-env-test` makes.
#
# ── Why a fixture instead of the real checkout ───────────────────────────────
#
# Asserting against this repository's own crate graph would make the suite a
# transcript of today's membership list: adding a crate, or moving one file
# between two of them, would turn cases red without anything being wrong. The
# fixture is a five-crate workspace with each edge kind built in exactly once,
# so a case fails only when the *scoping rule* it names has broken.
#
# It also lets the escape-edge cases be real. The fixture's `reader` crate
# genuinely contains `include_str!` pointing outside itself and its `rt` crate
# genuinely climbs out of `CARGO_MANIFEST_DIR`, so the two scans under test run
# against real source rather than a mock.
#
# ── What "tested" means here ─────────────────────────────────────────────────
#
# The script's whole job is deciding what NOT to compile, so a bug that always
# answers `--workspace` is invisible to a suite that only checks safety, and a
# bug that always answers "nothing" is invisible to one that only checks
# narrowing. Every case therefore asserts the exact scope line, and the cases
# come in pairs that straddle each rule: E1/E2 straddle the reverse-dependency
# closure, G1/G3 straddle the global-build-input list, X1/X2 straddle the
# compile-time escape, R1/R2 straddle the run-time escape.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/impacted-crates.sh"
FIX="$(mktemp -d)"
trap 'rm -rf "$FIX"' EXIT INT TERM

pass=0
fail=0

# One assertion form: feed paths on stdin, compare the scope line exactly.
# Exact rather than substring, because "-p base" is a substring of the wrong
# answer "-p base -p mid" and the difference between them is the whole point.
want() { # want <name> <expected-stdout> <changed-path>...
	local name="$1" expect="$2"
	shift 2
	local got
	got="$(printf '%s\n' "$@" | (cd "$FIX" && "$SCRIPT" 2>/dev/null))"
	if [ "$got" = "$expect" ]; then
		printf 'ok    %s\n' "$name"
		pass=$((pass + 1))
	else
		printf 'FAIL  %-44s want=[%s] got=[%s]\n' "$name" "$expect" "$got"
		fail=$((fail + 1))
	fi
}

want_empty() { # want_empty <name> <changed-path>...
	local name="$1"
	shift
	want "$name" "" "$@"
}

# ── the fixture ──────────────────────────────────────────────────────────────
#
#   base  ← mid ← leaf        a three-link chain, for the closure
#   base  ← devdep            via [dev-dependencies] only
#   reader                    include_str!s ../README.md and base's source
#   rt                        climbs out of CARGO_MANIFEST_DIR at run time
#
# `reader` and `rt` deliberately depend on nothing: it must be the escape scans
# that pull them in, not a dependency edge standing in for them.

mkcrate() { # mkcrate <dir> <name> [manifest-tail]
	mkdir -p "$FIX/$1/src"
	{
		printf '[package]\nname = "%s"\nversion = "0.0.0"\nedition = "2021"\npublish = false\n' "$2"
		printf '%s\n' "${3:-}"
	} >"$FIX/$1/Cargo.toml"
	printf 'pub fn %s() {}\n' "$(printf '%s' "$2" | tr '-' '_')" >"$FIX/$1/src/lib.rs"
}

mkdir -p "$FIX/docs" "$FIX/site"
cat >"$FIX/Cargo.toml" <<'EOF'
[workspace]
resolver = "2"
members = ["crates/base", "crates/mid", "crates/leaf", "crates/devdep", "crates/reader", "crates/rt"]
EOF
printf 'readme\n' >"$FIX/README.md"
printf 'note\n' >"$FIX/docs/note.md"
printf 'page\n' >"$FIX/site/page.mdx"

mkcrate crates/base base
mkcrate crates/mid mid '[dependencies]
base = { path = "../base" }'
mkcrate crates/leaf leaf '[dependencies]
mid = { path = "../mid" }'
# Dev-dependency only: `cargo test -p devdep` compiles base, so a change to
# base must re-test devdep even though its library does not link base.
mkcrate crates/devdep devdep '[dev-dependencies]
base = { path = "../base" }'
mkcrate crates/reader reader
mkcrate crates/rt rt

cat >>"$FIX/crates/reader/src/lib.rs" <<'EOF'
const README: &str = include_str!("../../../README.md");
const BASE_SRC: &str = include_str!("../../base/src/lib.rs");
EOF
cat >>"$FIX/crates/rt/src/lib.rs" <<'EOF'
pub fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("has a parent")
        .to_path_buf()
}
EOF

# `leaf` carries the build script, so the "a build script forces the full
# workspace" rule is tested on a real member rather than a bare path.
printf 'fn main() {}\n' >"$FIX/crates/leaf/build.rs"

git -C "$FIX" init -q
git -C "$FIX" add -A >/dev/null 2>&1

if ! (cd "$FIX" && cargo metadata --no-deps --format-version 1 >/dev/null 2>&1); then
	echo "test-impacted-crates: cargo metadata will not resolve the fixture; aborting" >&2
	exit 2
fi

# ── ownership and the reverse-dependency closure ─────────────────────────────

want "E1  a leaf crate impacts only itself" \
	"-p leaf" "crates/leaf/src/lib.rs"
want "E2  a base crate impacts its dependents" \
	"-p base -p devdep -p leaf -p mid -p reader" "crates/base/src/lib.rs"
want "E3  mid impacts leaf but not base" \
	"-p leaf -p mid" "crates/mid/src/lib.rs"
want "E4  a dev-dependency is a real edge" \
	"-p devdep" "crates/devdep/src/lib.rs"
want "E5  two crates union, not intersect" \
	"-p leaf -p mid -p rt" "crates/mid/src/lib.rs" "crates/rt/src/lib.rs"
want "E6  a crate's tests belong to that crate" \
	"-p leaf" "crates/leaf/tests/smoke.rs"

# ── global build inputs and the machinery rule ───────────────────────────────

want "G1  the workspace manifest is global" \
	"--workspace" "Cargo.toml"
want "G2  the lock is global" \
	"--workspace" "Cargo.lock"
want "G3  a crate's own manifest is NOT global" \
	"-p leaf" "crates/leaf/Cargo.toml"
want "G4  a build script is global" \
	"--workspace" "crates/leaf/build.rs"
want "G5  the toolchain pin is global" \
	"--workspace" "rust-toolchain.toml"
want "G6  lint config is global" \
	"--workspace" "clippy.toml"
want "G7  one global input outweighs a narrow one" \
	"--workspace" "crates/leaf/src/lib.rs" "Cargo.lock"
want "M1  the Makefile is machinery" \
	"--workspace" "Makefile"
want "M2  the scoper itself is machinery" \
	"--workspace" "scripts/impacted-crates.sh"
want "M3  the hook is machinery" \
	"--workspace" ".githooks/pre-push"

# ── compile-time escapes: include_str! reaching out of the crate ─────────────

want "X1  base's source re-tests the crate that include_str!s it" \
	"-p base -p devdep -p leaf -p mid -p reader" "crates/base/src/lib.rs"
want "X2  a sibling source with no include edge does not" \
	"-p leaf" "crates/leaf/src/lib.rs"
want "X3  an include target outside every crate names the reader" \
	"-p reader -p rt" "README.md"

# ── run-time escapes: CARGO_MANIFEST_DIR climbed with .parent() ──────────────

want "R1  an unreferenced repo file still re-tests the run-time reader" \
	"-p rt" "docs/note.md"
want "R2  ...and so does a file under a directory no crate names" \
	"-p rt" "site/page.mdx"
want "R3  an in-crate change does not wake the run-time reader" \
	"-p leaf" "crates/leaf/src/lib.rs"

# ── degenerate answers ───────────────────────────────────────────────────────

want_empty "N1  an empty diff needs no compile tiers"
want "N2  reaching every member collapses to --workspace" \
	"--workspace" "crates/base/src/lib.rs" "crates/rt/src/lib.rs"

# ── fail safe: an unusable workspace must widen, never narrow ────────────────

printf 'this is not toml [[[\n' >"$FIX/crates/base/Cargo.toml"
want "S1  a broken manifest falls back to the full workspace" \
	"--workspace" "crates/leaf/src/lib.rs"

echo
printf 'passed=%d failed=%d\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
