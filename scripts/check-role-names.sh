#!/usr/bin/env bash
#
# Guard: the four agent-config role names have one spelling, across four
# languages. See #1449.
#
# ── The failure this exists to catch ─────────────────────────────────────────
#
# #1394 renamed the non-worker verification role, `judge` -> `verifier`. The
# Rust compiler caught every Rust call site. It caught none of the producers of
# that same name that are not Rust, and the fallout took three PRs:
#
#   crates/stella-tui/src/statline.rs   matched PipelineRole::Judge
#                                       -> compiler; `main` shipped red (#1417)
#   arenabench/arenabench/config.py     translated verifier -> judge
#                                       -> NOTHING
#   bench/terminal_bench_analysis/      posture digests went stale
#                                       -> one of four pinned by luck
#   crates/stella-observatory/…/index.html  filters agents by a literal list
#                                       -> NOTHING
#
# The arenabench one is the shape that matters, and it is why this guard is
# worth its weight: no error, no failing test, no stopped match. The seat runs
# with the verifier inheriting the worker baseline while the scoreboard reports
# it as configured — so the contest publishes a number for a pairing it never
# ran. A wrong measurement, not a crash, is the worst thing this repository can
# ship (CLAUDE.md, "Measure honestly, especially when it costs us").
#
# ── The normative home ───────────────────────────────────────────────────────
#
# `ENGINE_AGENT_NAMES` plus `RETIRED_ENGINE_AGENT_NAMES` in
# crates/stella-cli/src/settings/unknown.rs — together, the role words this
# workspace still recognizes in an `agents.<role>` block or a
# `pipeline_<role>_model` key.
#
# It was `role_key()` in crates/stella-cli/src/config_wiring.rs until #3908,
# which deleted the `EngineAgentKind` enum that function mapped. Repointed
# rather than deleted, exactly as the paragraph below has always demanded: the
# Python and JavaScript producers still spell these five words, so something
# still has to hold four languages to one spelling. What changed is only where
# the Rust side keeps them — a live set of one (`default`) and a retired set of
# five, which is what the settings surface now actually is.
#
# The guard therefore checks the UNION. That is deliberate and it is the
# honest reading: the harbor adapter still WRITES `pipeline_verifier_model`
# (as does ArenaBench, whose producers are checked by its own repo's guard
# since the ejection, #2380), Stella still recognizes it, and the writers and
# the reader must agree on its spelling for exactly as long as that is true. When slice 6 (#3910)
# stops the Python writing them — once a role name travels with the run as
# trace data (#3906) — this guard and its GATE_STEPS entry go together, and
# `RETIRED_ENGINE_AGENT_NAMES` can shrink to nothing.
#
# Note this is NOT `stella_protocol::ModelCallRole`, which has fourteen variants
# describing individual model calls (PlanRepair, WitnessAuthor, Summarization…).
# Those never cross a language boundary. The agent-config roles do, and
# they are the whole subject here.
#
# ── Aliases ──────────────────────────────────────────────────────────────────
#
# `judge` is a deliberately retired spelling. It survives in exactly two places,
# both of which read it and neither of which writes it: a serde `alias` in Rust,
# and `_ROLE_ALIASES` in arenabench's config loader, which upgrades an old match
# file on the way in. That is compatibility, not drift, so the alias table below
# names it explicitly rather than the guard pretending it does not exist.
#
# Uses portable POSIX tools so it runs on a bare CI runner.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

fail=0

# The verdict is decided before anything is written (#1815). Failure lines are
# buffered while the checks run and emitted in one final write: a guard that
# prints as it scans dies mid-report when its reader exits early, and under
# `set -euo pipefail` whatever partial state it had reached becomes the exit
# status. scripts/check-file-size.sh is the shape being copied.
report=""
note() { report="${report}check-role-names: $1"$'\n'; }

# Emission is best-effort: the verdict is already decided, so a reader that
# closed the pipe (`| head -1`, `| true`) must be able to change neither the
# report nor the exit code. SIGPIPE is ignored so a failed write surfaces as a
# discarded error instead of killing the script (#1815).
emit() {
  trap '' PIPE
  printf '%s' "$report" >&2 || true
}

# Retired spellings, and the modern role each maps to. A producer may mention
# one of these only as an alias — never as a member of its own role set.
retired_spellings="judge"

# ── The truth ────────────────────────────────────────────────────────────────

rust_home="crates/stella-cli/src/settings/unknown.rs"

if [ ! -f "$rust_home" ]; then
  note "FAIL — $rust_home does not exist; ENGINE_AGENT_NAMES +"
  note "     RETIRED_ENGINE_AGENT_NAMES are the normative home."
  emit
  exit 1
fi

# The two consts, each a `&[&str]` literal that may wrap across lines:
#   pub(crate) const ENGINE_AGENT_NAMES: &[&str] = &["default"];
#   pub(crate) const RETIRED_ENGINE_AGENT_NAMES: &[&str] =
#       &["worker", "verifier", "triage", "research", "plan"];
# Read from each const's own line through the terminating `];`.
roles="$(
  awk '
    /const (ENGINE_AGENT_NAMES|RETIRED_ENGINE_AGENT_NAMES) *:/ { inside = 1 }
    inside {
      line = $0
      while (match(line, /"[a-z_]+"/)) {
        print substr(line, RSTART + 1, RLENGTH - 2)
        line = substr(line, RSTART + RLENGTH)
      }
      if (/\];/) { inside = 0 }
    }
  ' "$rust_home" | LC_ALL=C sort -u
)"

if [ -z "$roles" ]; then
  note "FAIL — could not read any role from ENGINE_AGENT_NAMES /"
  note "     RETIRED_ENGINE_AGENT_NAMES in $rust_home."
  note "     If those consts moved or changed shape, repoint this guard;"
  note "     do not delete it. It is the only thing holding four languages"
  note "     to one spelling. Retiring it outright is #3910, and only after"
  note "     #3906 makes role names travel as trace data."
  emit
  exit 1
fi

roles_flat="$(printf '%s\n' "$roles" | tr '\n' ' ')"

is_role() {
  case " $roles_flat " in
  *" $1 "*) return 0 ;;
  *) return 1 ;;
  esac
}

# Extract every double- or single-quoted lowercase token from stdin, one per
# line, sorted and deduplicated. Used to read a literal list out of Python or
# JavaScript without teaching this script either language.
quoted_tokens() {
  tr ',' '\n' | sed -n 's/.*["'"'"']\([a-z_][a-z_]*\)["'"'"'].*/\1/p' | LC_ALL=C sort -u
}

# Compare a producer's role set against the Rust one.
#   $1 producer path (for the message), $2 what it is, $3 the extracted set
expect_exact_set() {
  path="$1"
  what="$2"
  found="$3"

  if [ -z "$found" ]; then
    note "FAIL — $path: could not extract $what."
    note "     The producer moved or changed shape. Repoint the extraction in"
    note "     this guard rather than dropping the producer from it."
    fail=1
    return
  fi

  missing="$(comm -23 <(printf '%s\n' "$roles") <(printf '%s\n' "$found") | tr '\n' ' ')"
  extra="$(comm -13 <(printf '%s\n' "$roles") <(printf '%s\n' "$found") | tr '\n' ' ')"

  if [ -n "${missing// /}" ]; then
    note "FAIL — $path ($what) is missing role(s): $missing"
    fail=1
  fi
  if [ -n "${extra// /}" ]; then
    note "FAIL — $path ($what) has role(s) the engine does not know: $extra"
    fail=1
  fi
}

# Every role named in a `pipeline_<role>_model` key anywhere in a file must be a
# real role. A subset check, not an exact one: `default` has no flat key by
# design (`default_model` is its key), so the set is legitimately smaller.
#
# Redirected rather than piped, like producer 5 below: a `while read` on the
# right of a pipe runs in a subshell, where `fail=1` and the buffered note
# would be set and then thrown away.
expect_flat_keys_known() {
  path="$1"
  [ -f "$path" ] || return 0
  while IFS= read -r r; do
    [ -n "$r" ] || continue
    # Not a role name — these are the pipeline's own numeric settings.
    case "$r" in
    max_revisions | candidates) continue ;;
    esac
    if ! is_role "$r"; then
      note "FAIL — $path writes pipeline_${r}_model, and '$r' is not a role."
      fail=1
    fi
  done < <(sed -n 's/.*pipeline_\([a-z_][a-z_]*\)_model.*/\1/p' "$path" | LC_ALL=C sort -u)
}

# ── The producers ────────────────────────────────────────────────────────────
#
# Each entry states where the set lives and how it is spelled. Finding these
# again when one moves is half the work this guard does; a producer that
# vanishes should be deleted from here in the same PR, never silently skipped.

# 1-2 were ArenaBench's producers (model.py's ROLES tuple and
#     harbor_agent.py's per-role loop). They left with the ejection to
#     https://github.com/macanderson/arenabench (#2380), which carries its own
#     half of this guard; the boundary that still crosses repos is the harbor
#     adapter below. Numbering kept stable — citations name these entries.

# 3. The Observatory's agent filter — a JS literal, invisible to every Rust and
#    Python test in the tree. This is one of the two that silently vanished a
#    row in #1394.
p="crates/stella-observatory/src/assets/index.html"
if [ -f "$p" ]; then
  expect_exact_set "$p" "agent-name filter" \
    "$(grep -E '\]\.filter\(n *=> *agents\[n\]\)' "$p" | quoted_tokens)"
else
  note "FAIL — $p is gone; update this guard's producer list."
  fail=1
fi

# 4. The harbor adapter's posture writer. Every role it names goes into a hashed
#    posture, so a stale spelling here re-hashes every registered arm
#    (bench/READINESS.md §8.4.4).
expect_flat_keys_known "bench/harbor_adapter/stella_harbor/posture.py"

# 5. The TUI's second enum. Compiler-checked as a type, but its variants are a
#    parallel vocabulary that must stay inside the engine's — a subset, since
#    the deck has no `default` slot.
p="crates/stella-tui/src/deck.rs"
if [ -f "$p" ]; then
  # Redirected rather than piped: a `while read` on the right of a pipe runs in
  # a subshell, where `fail=1` would be set and then thrown away.
  while IFS= read -r variant; do
    [ -n "$variant" ] || continue
    if ! is_role "$variant"; then
      note "FAIL — $p: PipelineRole::${variant} is not an engine role."
      fail=1
    fi
  done < <(awk '
    /pub enum PipelineRole/ { inside = 1; next }
    inside && /^\}/         { exit }
    inside && /^ *[A-Z][A-Za-z]*,/ { gsub(/[ ,]/, ""); print tolower($0) }
  ' "$p")
fi

# ── Retired spellings ────────────────────────────────────────────────────────
#
# A retired name may appear only where it is being *translated*. Anywhere else
# it is the drift this guard exists to catch.

for old in $retired_spellings; do
  if is_role "$old"; then
    note "FAIL — '$old' is a retired SPELLING (a word we stopped using for a"
    note "     role that still exists), so it must not appear in either"
    note "     engine-agent name list in $rust_home. Note the two senses of"
    note "     'retired' in play: RETIRED_ENGINE_AGENT_NAMES holds correct"
    note "     spellings of roles that no longer exist. Only a retired"
    note "     spelling is drift."
    fail=1
  fi
done

if [ "$fail" -ne 0 ]; then
  note ""
  note "The role names live in $rust_home."
  note "Renaming one is a cross-language change: the Rust compiler will not"
  note "find the Python dict or the JavaScript literal, and neither will your"
  note "tests. Update every producer named above in the same PR."
  emit
  exit 1
fi

emit
printf 'check-role-names: OK — %d role(s) [%s] consistent across every producer.\n' \
  "$(printf '%s\n' "$roles" | wc -l | tr -d ' ')" "$(printf '%s' "$roles_flat" | sed 's/ $//')" || true
