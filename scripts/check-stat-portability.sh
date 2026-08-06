#!/usr/bin/env bash
#
# Guard: file identity is read through `MetadataExt`, never off a raw
# `libc::stat`. See #1758.
#
# ── The failure this exists to catch ─────────────────────────────────────────
#
# `dev_t` is signed on macOS and unsigned on Linux. `libc::stat::st_dev` is
# therefore `i32` on one and `u64` on the other, and *no single expression*
# comparing it to a fixed-width integer compiles clean on both:
#
#   stat.st_dev == lock.dev()        // macOS: mismatched types (i32 vs u64)
#   stat.st_dev as u64 == …          // Linux: clippy::unnecessary_cast, -D warnings
#   stat.st_dev.cast_unsigned() == … // macOS: fine. Linux: fails the gate.
#
# That is the worst possible shape for this repository's workflow. The author
# is on macOS, `make gate` passes on their machine, the pre-push hook is happy,
# and the break appears an hour later on a Linux CI runner — or does not appear
# at all until someone else pushes. #1737 shipped exactly this: a rewrite from
# raw `libc::stat` to `MetadataExt` that landed with *both halves still in the
# file*, and `main` went red for every open PR behind it (#1752 unbroke it).
#
# `std::os::unix::fs::MetadataExt::dev()` and `::ino()` are declared `u64` on
# every Unix. Reading identity through them removes the skew instead of
# papering over it with a cast and an `#[allow]`, so the rule is simply: do not
# touch those two fields on a raw struct.
#
# ── Scope, and what is deliberately NOT checked ──────────────────────────────
#
# Only `st_dev` and `st_ino` — the identity pair. `st_mode` and `st_size` are
# skewed too (`st_mode` is `u16` on macOS, `u32` on Linux), but the code that
# reads them compares against `libc` constants of the *same* platform type, so
# the skew cancels and nothing is cast. `crates/stella-tools/src/rootfd.rs` is
# the live example: its `fstatat` wrapper genuinely cannot use `MetadataExt` —
# it needs a dirfd-relative, `AT_SYMLINK_NOFOLLOW` stat that `std::fs` cannot
# express — and it reads only `st_mode`/`st_size`. Widening this guard to those
# fields would flag correct code and teach people to silence guards.
#
# ── The escape hatch ─────────────────────────────────────────────────────────
#
# A dirfd-relative identity check is a real thing to want, and `MetadataExt`
# cannot express it. Such a line may carry a marker, on itself or on the line
# above, naming why:
#
#   // stat-portability: ok — fstatat on a borrowed dirfd; std cannot do this.
#   let same = raw.st_ino == want.ino();
#
# The reason is required, and it is the whole point: an unexplained silencer is
# the defect this repository refuses to ship (CLAUDE.md, "An expedient is a
# defect"). Whoever writes one still owns making the comparison portable.
#
# Uses portable POSIX tools so it runs on a bare CI runner.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

note() { printf 'check-stat-portability: %s\n' "$1" >&2; }

# Named explicitly so a moved tree fails loudly here rather than silently
# scanning nothing and reporting OK — a guard that quietly stops looking is
# worse than no guard, because the green tick is now a lie.
for root in crates bench; do
  if [ ! -d "$root" ]; then
    note "FAIL — '$root/' does not exist; repoint this guard's roots."
    exit 1
  fi
done

# Rust sources only, and only where workspace code lives. Comments are stripped
# before matching, so the prose above — which names the forbidden fields
# repeatedly — does not indict the guard's own subject matter. A source sweep
# that cannot tell code from prose flags the file documenting the rule, which
# is precisely how #1747 lost an hour (#1748).
# shellcheck disable=SC2016 # the awk program is deliberately single-quoted:
# its `$0` is awk's whole-line variable and must not be expanded by the shell.
offenders="$(
  find crates bench -name '*.rs' -type f -print0 2>/dev/null |
    xargs -0 awk '
    FNR == 1 { inblock = 0; prev = "" }
    {
      raw = $0
      code = ""
      i = 1
      n = length($0)
      while (i <= n) {
        two = substr($0, i, 2)
        if (inblock) {
          if (two == "*/") { inblock = 0; i += 2 } else { i++ }
          continue
        }
        if (two == "/*") { inblock = 1; i += 2; continue }
        if (two == "//") {
          # A "//" straight after a colon is a URL inside a string literal, not
          # the start of a comment. Keeping it costs nothing; treating it as a
          # comment would blind the rest of the line.
          if (i > 1 && substr($0, i - 1, 1) == ":") { code = code two; i += 2; continue }
          break
        }
        code = code substr($0, i, 1)
        i++
      }
      if (code ~ /\.st_dev|\.st_ino/) {
        if (raw !~ /stat-portability: ok/ && prev !~ /stat-portability: ok/) {
          line = code
          sub(/^[ \t]+/, "", line)
          printf "%s:%d: %s\n", FILENAME, FNR, line
        }
      }
      prev = raw
    }
  '
)"

if [ -n "$offenders" ]; then
  note "FAIL — raw \`libc::stat\` identity field(s) read:"
  printf '%s\n' "$offenders" | sed 's/^/  /' >&2
  note ""
  note "\`st_dev\` is signed on macOS and unsigned on Linux, so a comparison"
  note "against a fixed-width integer cannot compile clean on both: the cast"
  note "that satisfies macOS is an unnecessary_cast on Linux, and -D warnings"
  note "turns that into a red gate you will not see until CI runs."
  note ""
  note "Read identity through \`std::os::unix::fs::MetadataExt\` instead —"
  note "\`dev()\` and \`ino()\` are \`u64\` on every Unix:"
  note ""
  note "    use std::os::unix::fs::MetadataExt;"
  note "    let stat = file.metadata()?;"
  note "    if stat.dev() == other.dev() && stat.ino() == other.ino() { … }"
  note ""
  note "If you genuinely need a dirfd-relative stat that \`std::fs\` cannot"
  note "express, mark the line and say why:"
  note ""
  note "    // stat-portability: ok — <reason>"
  exit 1
fi

scanned="$(find crates bench -name '*.rs' -type f 2>/dev/null | wc -l | tr -d ' ')"
# The verdict is already decided; the write is best-effort. SIGPIPE is ignored
# and the write's failure discarded, so a reader that closed the pipe
# (`| head -1`, `| true`) cannot turn a green verdict into a failure (#1815).
trap '' PIPE
printf 'check-stat-portability: OK — %s Rust file(s), no raw stat identity reads.\n' \
  "$scanned" || true
