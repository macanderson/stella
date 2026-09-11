#!/usr/bin/env bash
#
# Tests for check-core-no-io.py.
#
#   ./scripts/test-core-no-io.sh
#
# Run it after touching that script. Not part of `make gate`: it builds
# throwaway crate fixtures, the same posture as `core-reachability-test`.
#
# ── Why a fixture instead of the real tree ───────────────────────────────────
#
# A green run over the real crate proves the tree is clean today. What needs
# proving is each way the guard can be wrong:
#
#   misses      a file read in shipping source passes. That is the defect
#               the guard exists for.
#   fabricates  a test body, a comment, or a test file is reported as I/O.
#               Then the reader stops trusting the guard.
#   manifest    an I/O crate in `[dependencies]` passes, or one in
#               `[dev-dependencies]` fails.
#   ratchet     a clock read past the baseline passes. Or `--update` records
#               new debt. Or a lowered count is never reclaimed.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-core-no-io.py"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

# A throwaway tree shaped like the real one. One crate, a clean manifest,
# one source file.
new_core() { # <case>
  local dir="$TMP/$1/crates/stella-core"
  mkdir -p "$dir/src" "$TMP/$1/scripts"
  printf '[package]\nname = "stella-core"\n\n[dependencies]\nserde = "1"\ntokio = { version = "1", features = ["sync"] }\n\n[dev-dependencies]\nrand = "0.10"\n' >"$dir/Cargo.toml"
  printf 'pub mod driver;\n' >"$dir/src/lib.rs"
  printf 'pub fn drive() {}\n' >"$dir/src/driver.rs"
  : >"$TMP/$1/scripts/core-no-io-baseline.txt"
  echo "$dir/src"
}

seed_baseline() { # <case> <lines…>
  local case="$1"
  shift
  : >"$TMP/$case/scripts/core-no-io-baseline.txt"
  for line in "$@"; do
    echo "$line" >>"$TMP/$case/scripts/core-no-io-baseline.txt"
  done
}

# want <name> <expect-pass|expect-fail> <case> [substring]
want() {
  local name="$1" expect="$2" case="$3" sub="${4:-}" out rc
  out="$(python3 "$SCRIPT" "$TMP/$case" 2>&1)"
  rc=$?
  if [ "$expect" = "expect-pass" ]; then
    if [ "$rc" -eq 0 ]; then
      pass=$((pass + 1)); echo "ok   $name"
    else
      fail=$((fail + 1)); echo "FAIL $name — expected OK, got:"; echo "$out"
    fi
    return
  fi
  if [ "$rc" -eq 0 ]; then
    fail=$((fail + 1)); echo "FAIL $name — the guard passed it:"; echo "$out"
    return
  fi
  case "$out" in
    *"$sub"*) pass=$((pass + 1)); echo "ok   $name" ;;
    *) fail=$((fail + 1)); echo "FAIL $name — failed for the wrong reason (wanted '$sub'):"; echo "$out" ;;
  esac
}

# ── The floor ────────────────────────────────────────────────────────────────
src="$(new_core clean)"
want "a clean crate passes" expect-pass clean

src="$(new_core fs)"
printf 'pub fn drive() { let _ = std::fs::read_to_string("x"); }\n' >"$src/driver.rs"
want "std::fs in shipping source is reported" expect-fail fs "driver.rs:1: std::fs"

src="$(new_core grouped)"
printf 'use std::{fs, path::Path};\npub fn drive() { let _ = fs::read_to_string(Path::new("x")); }\n' >"$src/driver.rs"
want "a grouped use of std::fs is reported" expect-fail grouped "std::fs"

src="$(new_core wallclock)"
printf 'pub fn now() -> u64 { std::time::SystemTime::now().elapsed().unwrap().as_millis() as u64 }\n' >"$src/driver.rs"
want "SystemTime::now is reported" expect-fail wallclock "SystemTime::now"

src="$(new_core entropy)"
printf 'pub fn draw() -> u64 { rand::rng().random_range(0..10) }\n' >"$src/driver.rs"
want "rand::rng is reported" expect-fail entropy "an entropy source"

src="$(new_core printing)"
printf 'pub fn drive() { println!("hi"); }\n' >"$src/driver.rs"
want "a print macro is reported" expect-fail printing "a print macro"

src="$(new_core spawn)"
printf 'pub fn drive() { let _ = std::process::Command::new("sh"); }\n' >"$src/driver.rs"
want "std::process is reported" expect-fail spawn "std::process"

src="$(new_core elapsed)"
printf 'pub fn drive(started: std::time::Instant) -> u128 { started.elapsed().as_millis() }\n' >"$src/driver.rs"
want "Instant::elapsed is reported as the clock read it hides" expect-fail elapsed "Instant::elapsed"

src="$(new_core timer)"
printf 'pub async fn drive() { let _ = tokio::time::timeout(std::time::Duration::from_secs(1), async {}).await; }\n' >"$src/driver.rs"
want "a tokio timer is reported" expect-fail timer "tokio's timer"

# ── Fabrication: what is not shipping code ───────────────────────────────────
src="$(new_core cfgtest)"
cat >"$src/driver.rs" <<'RS'
pub fn drive() {}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_a_fixture() {
        let _ = std::fs::read_to_string("fixture");
        let _ = std::time::SystemTime::now();
        println!("{}", rand::rng().random_range(0..10));
    }
}
RS
want "a #[cfg(test)] body may do anything" expect-pass cfgtest

src="$(new_core commented)"
printf '// Never std::fs::read here; SystemTime::now() is a port. See println!.\npub fn drive() { let _ = "std::fs::read"; }\n' >"$src/driver.rs"
want "a comment or a string is not a hit" expect-pass commented

src="$(new_core testfiles)"
mkdir -p "$src/driver/tests" "$TMP/testfiles/crates/stella-core/tests"
printf 'pub fn drive() {}\n#[cfg(test)]\nmod tests;\n' >"$src/driver.rs"
printf 'pub fn t() { let _ = std::fs::read_to_string("x"); }\n' >"$src/driver/tests.rs"
printf 'pub fn t() { let _ = std::fs::read_to_string("x"); }\n' >"$src/driver/tests/more.rs"
printf 'fn t() { let _ = std::fs::read_to_string("x"); }\n' >"$TMP/testfiles/crates/stella-core/tests/witness.rs"
want "tests.rs, a tests/ module, and an integration test are not shipping code" expect-pass testfiles

# A module is test code because the `mod` line that names it sits under
# `#[cfg(test)]`, whatever the file is called. `src/tests.rs` was the first
# such file, and it read as shipping code until this case.
src="$(new_core cfgmod)"
mkdir -p "$src/driver/helpers"
printf 'pub mod driver;\n#[cfg(test)]\npub(crate) mod doubles;\n' >"$src/lib.rs"
printf 'pub fn t() { let _ = std::fs::read_to_string("x"); }\n' >"$src/doubles.rs"
printf 'pub fn drive() {}\n#[cfg(test)]\n#[allow(dead_code)]\nmod helpers;\n' >"$src/driver.rs"
printf 'pub mod deep;\n' >"$src/driver/helpers.rs"
printf 'pub fn t() { let _ = std::time::SystemTime::now(); }\n' >"$src/driver/helpers/deep.rs"
want "a module named under #[cfg(test)] is not shipping code, by any name and at any depth" expect-pass cfgmod

# The control: the same file named by a plain `mod` line is shipping code.
src="$(new_core plainmod)"
printf 'pub mod driver;\npub(crate) mod doubles;\n' >"$src/lib.rs"
printf 'pub fn t() { let _ = std::fs::read_to_string("x"); }\n' >"$src/doubles.rs"
want "a module named by a plain mod line is shipping code" expect-fail plainmod "doubles.rs"

# ── The manifest ─────────────────────────────────────────────────────────────
src="$(new_core denied)"
printf '[package]\nname = "stella-core"\n\n[dependencies]\nrand = "0.10"\n' >"$TMP/denied/crates/stella-core/Cargo.toml"
want "an entropy crate in [dependencies] is reported" expect-fail denied "rand\` in [dependencies]"

src="$(new_core devdep)"
want "the same crate in [dev-dependencies] is allowed" expect-pass devdep

src="$(new_core tokiofs)"
printf '[package]\nname = "stella-core"\n\n[dependencies]\ntokio = { version = "1", features = ["sync", "fs"] }\n' >"$TMP/tokiofs/crates/stella-core/Cargo.toml"
want "a tokio I/O feature is reported" expect-fail tokiofs "tokio feature \`fs\`"

src="$(new_core tokiotime)"
printf '[package]\nname = "stella-core"\n\n[dependencies]\ntokio = { version = "1", features = ["sync", "time"] }\n' >"$TMP/tokiotime/crates/stella-core/Cargo.toml"
want "tokio's time feature is reported" expect-fail tokiotime "tokio feature \`time\`"

src="$(new_core tokiotable)"
printf '[package]\nname = "stella-core"\n\n[dependencies.tokio]\nversion = "1"\nfeatures = ["process"]\n' >"$TMP/tokiotable/crates/stella-core/Cargo.toml"
want "the table spelling of a tokio dependency is read too" expect-fail tokiotable "tokio feature \`process\`"

# ── The ratchet ──────────────────────────────────────────────────────────────
src="$(new_core clock)"
printf 'pub fn drive() { let _ = std::time::Instant::now(); let _ = std::time::Instant::now(); }\n' >"$src/driver.rs"
want "a clock read with no baseline entry is reported" expect-fail clock "Instant::now"

seed_baseline clock "2 crates/stella-core/src/driver.rs"
want "and passes at the recorded count" expect-pass clock

seed_baseline clock "1 crates/stella-core/src/driver.rs"
want "and fails one read over it" expect-fail clock "baseline 1"

# --update refuses to grow. The baseline can grow two ways. Both are refused.
seed_baseline clock "1 crates/stella-core/src/driver.rs"
out="$(python3 "$SCRIPT" --update "$TMP/clock" 2>&1)"
rc=$?
if [ "$rc" -ne 0 ] && [ -z "${out##*REFUSING*}" ]; then
  pass=$((pass + 1)); echo "ok   --update refuses to raise a count"
else
  fail=$((fail + 1)); echo "FAIL --update raised a count (rc=$rc):"; echo "$out"
fi
case "$(cat "$TMP/clock/scripts/core-no-io-baseline.txt")" in
  "1 crates/stella-core/src/driver.rs") pass=$((pass + 1)); echo "ok   and left the baseline untouched" ;;
  *) fail=$((fail + 1)); echo "FAIL --update wrote the baseline anyway:"; cat "$TMP/clock/scripts/core-no-io-baseline.txt" ;;
esac

seed_baseline clock
out="$(python3 "$SCRIPT" --update "$TMP/clock" 2>&1)"
rc=$?
if [ "$rc" -ne 0 ] && [ -z "${out##*REFUSING*}" ]; then
  pass=$((pass + 1)); echo "ok   --update refuses to add a file"
else
  fail=$((fail + 1)); echo "FAIL --update added a file (rc=$rc):"; echo "$out"
fi

# A count that dropped is stale. --update reclaims it.
src="$(new_core lowered)"
printf 'pub fn drive() { let _ = std::time::Instant::now(); }\n' >"$src/driver.rs"
seed_baseline lowered "3 crates/stella-core/src/driver.rs"
want "a count under its ceiling is reported stale" expect-fail lowered "STALE"
python3 "$SCRIPT" --update "$TMP/lowered" >/dev/null 2>&1
case "$(grep -v '^#' "$TMP/lowered/scripts/core-no-io-baseline.txt")" in
  "1 crates/stella-core/src/driver.rs") pass=$((pass + 1)); echo "ok   --update lowers a count to what the file reads now" ;;
  *) fail=$((fail + 1)); echo "FAIL --update did not lower the count:"; cat "$TMP/lowered/scripts/core-no-io-baseline.txt" ;;
esac

# An entry whose file reads the clock no more is retired.
src="$(new_core retired)"
seed_baseline retired "1 crates/stella-core/src/driver.rs"
python3 "$SCRIPT" --update "$TMP/retired" >/dev/null 2>&1
if [ -z "$(grep -v '^#' "$TMP/retired/scripts/core-no-io-baseline.txt" | tr -d '[:space:]')" ]; then
  pass=$((pass + 1)); echo "ok   --update retires an entry with no reads left"
else
  fail=$((fail + 1)); echo "FAIL --update kept a dead entry:"; cat "$TMP/retired/scripts/core-no-io-baseline.txt"
fi

# --update never writes over a red floor. A run judges more than the clock
# count. A baseline written beside an I/O hit would read as a pass.
src="$(new_core redfloor)"
printf 'pub fn drive() { let _ = std::fs::read_to_string("x"); }\n' >"$src/driver.rs"
out="$(python3 "$SCRIPT" --update "$TMP/redfloor" 2>&1)"
rc=$?
if [ "$rc" -ne 0 ] && [ -z "${out##*std::fs*}" ]; then
  pass=$((pass + 1)); echo "ok   --update still fails on a floor hit"
else
  fail=$((fail + 1)); echo "FAIL --update passed a floor hit:"; echo "$out"
fi

echo
echo "core-no-io: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
