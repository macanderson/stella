#!/usr/bin/env bash
#
# Narrow the gate's compile/test tiers to the crates a diff can actually break
# (#1135).
#
# `make gate` compiled, documented and tested all 21 workspace members on every
# invocation, and `.githooks/pre-push` ran it unconditionally. A 13-file
# test-only change confined to `stella-cli` therefore paid a full-workspace
# `cargo doc` + `clippy --all-targets` + `cargo test` — twice, once per push
# (observed while shipping #911). With no tier between "everything" and
# `SKIP_GATE=1`, the honest choice under time pressure was to skip the gate
# entirely, which is the worst possible default for a repo whose stated culture
# is that the gate must be believable.
#
# This script prints the cargo package scope for those tiers to stdout, on one
# line, in one of three shapes:
#
#   --workspace           every member — the safe answer, and the default
#   -p stella-cli ...     only the members the diff can reach
#   (empty)               no member can be reached; skip the compile tiers
#
# Its reasoning goes to stderr so a caller can show its work.
#
#   scripts/impacted-crates.sh --range <base>..<head>
#   git diff --name-only A B | scripts/impacted-crates.sh
#
# WHY THIS IS MORE THAN A REVERSE-DEPENDENCY WALK
#
# Cargo's dependency graph is not the whole build graph here. Three kinds of
# edge exist in this tree, and a scoper that knows only the first is wrong in a
# way that is invisible until CI disagrees with the local gate:
#
#   1. Ownership + reverse dependencies. A changed file under `crates/stella-core/`
#      belongs to `stella-core`, and every member that depends on it —
#      through normal, dev OR build dependencies — must be re-checked. Dev
#      dependencies count: crate A's *tests* can depend on B even when A's
#      library does not.
#
#   2. Compile-time escapes. `crates/stella-parity/src/lib.rs` does
#      `include_str!("../../stella-cli/src/subsession.rs")`, and
#      `crates/stella-tools/tests/doc_truth.rs` does `include_str!("../../README.md")`.
#      No cargo edge names either one. These are resolved literally below, so
#      touching that prompt file re-tests `stella-parity` even though nothing
#      depends on `stella-cli`.
#
#   3. Run-time escapes. `crates/stella-tools/tests/docs_in_sync.rs` walks
#      `website/content/docs/agent-tools/*.mdx`; `crates/stella-cli/src/tests.rs`
#      reads `website/content/docs/commands/meta.json`; and
#      `crates/stella-protocol/tests/wire_contract.rs` reads `docs/wire/`. These open
#      files by path at run time, so no static scan can resolve their targets.
#      They are detected — not enumerated in a hand-kept table that would rot —
#      by the shape they all share: `env!("CARGO_MANIFEST_DIR")` climbed with
#      `.parent()` or `".."`. Any crate matching that shape is re-checked
#      whenever the diff touches anything outside every crate directory.
#
# Escape edges (2 and 3) add the *reading* crate, and deliberately do not drag
# in its reverse dependents: a stale `README.md` can fail `stella-tools`'s own
# doc test, but it cannot change the code that `stella-tools`'s dependents
# compile against.
#
# EVERY UNCERTAIN ANSWER IS `--workspace`
#
# A wrong narrow answer is a gate that lies. Missing jq, missing cargo, a
# `cargo metadata` that will not resolve, an unreadable diff, a path this
# script cannot place, or a change to the scoping machinery itself all print
# `--workspace` and say why. The cheap global guards (`no-scratch`,
# `action-pins`, `file-size`, `invariants`, `doc-links`, `wire-schema`, …)
# are unscoped by construction — they are not this script's business, and
# `make gate` runs them on every push regardless of what is printed here.
#
# Uses portable POSIX tooling and no bash-4 features: macOS ships bash 3.2, so
# no associative arrays (same constraint as scripts/check-file-size.sh).
set -euo pipefail

full() {
	printf '%s\n' "--workspace"
	printf 'impacted-crates: full workspace — %s\n' "$1" >&2
	exit 0
}

range=""
while [ $# -gt 0 ]; do
	case "$1" in
	--range)
		[ $# -ge 2 ] || {
			echo "impacted-crates: --range needs an argument" >&2
			exit 2
		}
		range="$2"
		shift 2
		;;
	-h | --help)
		sed -n '2,30p' "$0"
		exit 0
		;;
	*)
		echo "impacted-crates: unknown argument: $1" >&2
		exit 2
		;;
	esac
done

if ! repo_root="$(git rev-parse --show-toplevel 2>/dev/null)"; then
	full "not a git repository"
fi
cd "$repo_root"

# ---------------------------------------------------------------------------
# 1. The changed paths.
# ---------------------------------------------------------------------------

if [ -n "$range" ]; then
	if ! changed="$(git diff --name-only "$range" 2>/dev/null)"; then
		full "cannot diff $range"
	fi
else
	changed="$(cat)"
fi

# Drop blanks so an empty stdin does not read as one empty path.
changed="$(printf '%s\n' "$changed" | sed '/^[[:space:]]*$/d')"

if [ -z "$changed" ]; then
	printf '\n'
	printf 'impacted-crates: nothing changed — no compile tiers needed\n' >&2
	exit 0
fi

# ---------------------------------------------------------------------------
# 2. Global build inputs — any of these and the answer is the whole workspace.
#
# The first group changes how *every* crate compiles (feature resolution, the
# lock, the toolchain, lint config, a build script's output). The second group
# is the gate machinery itself: if you are editing the thing that decides what
# gets tested, the only believable proof is the unscoped run. That rule makes
# this change's own push pay full price, which is the point.
# ---------------------------------------------------------------------------

global_re='^(Cargo\.toml|Cargo\.lock|rust-toolchain(\.toml)?|clippy\.toml|rustfmt\.toml|deny\.toml|\.cargo/.*|(.*/)?build\.rs)$'
machinery_re='^(Makefile|scripts/impacted-crates\.sh|\.githooks/pre-push)$'

hit="$(printf '%s\n' "$changed" | grep -E -m1 "$global_re" || true)"
[ -z "$hit" ] || full "$hit is a global build input"

hit="$(printf '%s\n' "$changed" | grep -E -m1 "$machinery_re" || true)"
[ -z "$hit" ] || full "$hit changes the gate machinery itself"

# ---------------------------------------------------------------------------
# 3. The workspace map, from cargo itself.
# ---------------------------------------------------------------------------

command -v jq >/dev/null 2>&1 || full "jq is not installed"
command -v cargo >/dev/null 2>&1 || full "cargo is not installed"
command -v rg >/dev/null 2>&1 || full "ripgrep (rg) is not installed"

if ! meta="$(cargo metadata --no-deps --format-version 1 2>/dev/null)"; then
	full "cargo metadata failed (a manifest or the lock may be broken)"
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# `<crate dir>\t<package name>`. The checkout prefix is stripped in shell
# rather than in jq: jq would need the absolute path spliced into a regex, and
# a checkout path containing a regex metacharacter (`.claude/worktrees/…` has
# one on the very first component) would silently mis-anchor it.
jq -r '.packages[] | (.manifest_path | sub("/Cargo\\.toml$"; "")) + "\t" + .name' \
	<<<"$meta" >"$work/dirs.abs" || full "cannot read package directories from cargo metadata"

: >"$work/dirs.rel"
while IFS=$'\t' read -r dir name; do
	rel="${dir#"$repo_root"/}"
	# Unstripped (a member outside the checkout) or empty (a package rooted at
	# the checkout itself) both make prefix matching meaningless.
	if [ "$rel" = "$dir" ] || [ -z "$rel" ]; then
		full "package $name is not inside the checkout"
	fi
	printf '%s\t%s\n' "$rel" "$name" >>"$work/dirs.rel"
done <"$work/dirs.abs"

# Longest directory first, so the deepest enclosing crate wins: a nested member
# must claim its own files rather than losing them to a shorter prefix.
awk -F'\t' '{ print length($1) "\t" $0 }' "$work/dirs.rel" |
	sort -rn | cut -f2- >"$work/dirs"

cut -f2 "$work/dirs" | sort -u >"$work/members"

# `<dependency>\t<dependent>` for workspace-internal edges only, all dependency
# kinds included (a dev-dependency is a real edge for `cargo test`).
jq -r '.packages[] as $p | $p.dependencies[] | .name + "\t" + $p.name' <<<"$meta" |
	sort -u >"$work/edges.raw"
awk -F'\t' 'NR==FNR { member[$0]=1; next } member[$1] { print }' \
	"$work/members" "$work/edges.raw" >"$work/edges"

# ---------------------------------------------------------------------------
# 4. Place every changed path.
# ---------------------------------------------------------------------------

: >"$work/seed"    # crates that own a changed file — these get the closure
: >"$work/extra"   # crates reached by an escape edge — no closure (see header)
outside=0

crate_for() {
	# Longest-prefix match against the crate directories.
	local path="$1" dir name
	while IFS=$'\t' read -r dir name; do
		case "$path" in
		"$dir"/*) printf '%s\n' "$name"; return 0 ;;
		esac
	done <"$work/dirs"
	return 1
}

while IFS= read -r path; do
	if owner="$(crate_for "$path")"; then
		printf '%s\n' "$owner" >>"$work/seed"
	else
		outside=1
	fi
done <<<"$changed"

# ---------------------------------------------------------------------------
# 5. Escape edges.
# ---------------------------------------------------------------------------

# 5a. Compile-time: resolve every `include_str!`/`include_bytes!`/`include!`
#     literal against the including file, and record the ones that land outside
#     the including crate.
normalize() {
	# Collapse `.` and `..` in a relative path without touching the disk (the
	# target may have just been deleted, and macOS has no `realpath
	# --relative-to`).
	local path="$1" part out=""
	local IFS=/
	for part in $path; do
		case "$part" in
		'' | .) ;;
		..) out="${out%/*}" ;;
		*) out="$out/$part" ;;
		esac
	done
	printf '%s\n' "${out#/}"
}

# The explicit `.` and `</dev/null` are both load-bearing. Given no path
# argument and a non-TTY stdin, ripgrep searches STDIN rather than the working
# directory — and this script's stdin is a pipe that `cat` has already drained,
# so the scan would match nothing, exit 0, and silently drop every escape edge.
: >"$work/includes" # `<target path>\t<including crate>`
while IFS= read -r line; do
	src="${line%%:*}"
	src="${src#./}"
	lit="${line#*:}"
	lit="${lit#*\"}"
	lit="${lit%\"*}"
	[ -n "$lit" ] || continue
	case "$lit" in /*) continue ;; esac # absolute literals are not repo paths
	target="$(normalize "${src%/*}/$lit")"
	src_crate="$(crate_for "$src")" || continue
	tgt_crate="$(crate_for "$target" || true)"
	[ "$tgt_crate" = "$src_crate" ] && continue
	printf '%s\t%s\n' "$target" "$src_crate" >>"$work/includes"
done < <(rg --no-heading -H -o -g '*.rs' \
	'include(_str|_bytes)?!\("[^"]+"' . </dev/null 2>/dev/null || true)

if [ -s "$work/includes" ]; then
	awk -F'\t' 'NR==FNR { changed[$0]=1; next } changed[$1] { print $2 }' \
		<(printf '%s\n' "$changed") "$work/includes" >>"$work/extra"
fi

# 5b. Run-time: crates whose sources climb out of their own manifest directory.
#     Detected by shape rather than listed by name, so a new one cannot quietly
#     fall outside the gate's reach.
if [ "$outside" -eq 1 ]; then
	while IFS= read -r src; do
		src="${src#./}"
		[ -n "$src" ] || continue
		rg -q '\.parent\(\)|"\.\."' "$src" </dev/null 2>/dev/null || continue
		crate_for "$src" >>"$work/extra" || true
	done < <(rg -l -g '*.rs' 'CARGO_MANIFEST_DIR' . </dev/null 2>/dev/null || true)
fi

# ---------------------------------------------------------------------------
# 6. Reverse-dependency closure over the owned crates.
# ---------------------------------------------------------------------------

sort -u "$work/seed" >"$work/impacted"
frontier="$work/impacted"
while [ -s "$frontier" ]; do
	awk -F'\t' 'NR==FNR { f[$0]=1; next } f[$1] { print $2 }' \
		"$frontier" "$work/edges" | sort -u >"$work/next.raw"
	comm -23 "$work/next.raw" "$work/impacted" >"$work/next"
	[ -s "$work/next" ] || break
	sort -u "$work/impacted" "$work/next" >"$work/merged"
	mv "$work/merged" "$work/impacted"
	mv "$work/next" "$work/frontier"
	frontier="$work/frontier"
done

sort -u "$work/impacted" "$work/extra" >"$work/final.raw"
# Only ever name real members: an escape edge in a stale path, or a crate
# renamed out from under this script, must not become `-p <nonexistent>`.
comm -12 "$work/final.raw" "$work/members" >"$work/final"

# ---------------------------------------------------------------------------
# 7. Answer.
# ---------------------------------------------------------------------------

count="$(wc -l <"$work/final" | tr -d ' ')"
total="$(wc -l <"$work/members" | tr -d ' ')"

if [ "$count" -eq 0 ]; then
	printf '\n'
	printf 'impacted-crates: no crate is reachable from this diff — skipping the compile tiers\n' >&2
	exit 0
fi

# Scoping the whole workspace with 21 `-p` flags is slower than `--workspace`
# and reads worse in the log; collapse it.
if [ "$count" -ge "$total" ]; then
	full "the diff reaches every member"
fi

scope=""
while IFS= read -r name; do
	scope="$scope -p $name"
done <"$work/final"
printf '%s\n' "${scope# }"
printf 'impacted-crates: %s of %s members —%s\n' "$count" "$total" "$scope" >&2
