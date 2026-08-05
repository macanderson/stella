#!/usr/bin/env bash
# The shell face of `fullauto` — the autonomous delivery loop.
#
# The deterministic half of the loop is `stella fullauto` (#1548): the
# governor, the ledger, the aperture ladder, the dedup oracle, and the run
# lifecycle are engine code with types and tests (stella-core::fullauto +
# stella-cli's fullauto_cmd). This script keeps only what is genuinely shell
# work — the benchmark arms, the Homebrew ship step, the daemon's process
# supervision, and the Claude command install — and DELEGATES every ported
# verb one-for-one, so exactly one copy of every decision exists.
#
#   scripts/fullauto.sh preflight          # is this machine able to run a cycle?
#   scripts/fullauto.sh plan --explain     # -> stella fullauto plan
#   scripts/fullauto.sh queue --limit 20   # -> stella fullauto queue
#   scripts/fullauto.sh cycle-begin        # -> stella fullauto cycle begin
#   scripts/fullauto.sh seen --new <sha>…  # -> stella fullauto seen
#   scripts/fullauto.sh cycle-end …        # -> stella fullauto cycle end
#   scripts/fullauto.sh aperture --list    # -> stella fullauto aperture
#   scripts/fullauto.sh watch              # -> stella fullauto watch
#   scripts/fullauto.sh metrics            # -> stella fullauto metrics
#   scripts/fullauto.sh bench loop|h2h     # the comparator arm (h2h: --rig,
#                                          #   --rig --dry-run = free rehearsal)
#   scripts/fullauto.sh daemon start       # the loop outlives this terminal
#   scripts/fullauto.sh upgrade            # ship: brew from the tap, drop the shadow
#   scripts/fullauto.sh install-commands   # put /fullauto in ~/.claude/commands
#
# The loop does not terminate — see `stella fullauto aperture --list`. State
# lives at ~/.stella/fullauto/<slug>/ (outside the repo: `make no-scratch`,
# #448) and is owned by the binary; this script never writes a state file.
set -uo pipefail

# ---------------------------------------------------------------------------
# Constants that are facts about this machine and this repository
# ---------------------------------------------------------------------------

# Homebrew-core owns the name `stella` (an Atari 2600 emulator, 245 installs a
# year). `brew install stella` therefore installs the WRONG software and exits
# 0. The tap-qualified name is the only safe spelling; never shorten it.
readonly BREW_FORMULA="macanderson/stella/stella"
readonly BREW_TAP="macanderson/stella"
readonly BREW_TAP_REMOTE="https://github.com/macanderson/homebrew-tap.git"

# The PATH entry that shadows a released install with the local dev build. The
# `upgrade` subcommand removes exactly this string and refuses to touch the rc
# file if it does not find it verbatim — mangling a login shell is not a
# recoverable mistake.
#
# shellcheck disable=SC2016  # deliberate: this is the literal TEXT of a line in
# ~/.zshrc, matched byte-for-byte. Expanding $HOME here would stop it matching.
readonly SHADOW_PATH_LINE='export PATH="$HOME/Projects/stella/target/release:$PATH"'

# The Terminal-Bench rig. Native x86_64 Linux Docker host; see
# bench/RUNBOOK.md Part 0. It bills $1.90/hr RUNNING, so every path that starts
# it also stops it, including on failure.
readonly RIG_INSTANCE="${FULLAUTO_RIG_INSTANCE:-i-07d46341dcc9a31b3}"
readonly RIG_REGION="${FULLAUTO_RIG_REGION:-us-east-1}"
readonly RIG_USER="ubuntu"
# A credential is not a document: the key lives under the stella home, not in
# the repository. Its old home was the docs/design scratchpad, which
# check-design-refs forbids citing — and a key that sits in a docs tree is one
# `git add -A` away from being published.
readonly RIG_KEY="${FULLAUTO_RIG_KEY:-${STELLA_HOME:-$HOME/.stella}/keys/tb909-key.pem}"

# The head-to-head match: Claude Code vs Stella on Terminal-Bench 2.1, same
# model on both arms so the agent architecture is the variable under test.
readonly H2H_MATCH="${FULLAUTO_MATCH:-arenabench/matches/fable5-claude-code-vs-stella.toml}"

# The controller ceilings, the floors, the aperture ladder, and the dry-streak
# target all live in the binary now (stella-core::fullauto), still answering
# to the same FULLAUTO_* environment knobs they always did.

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
readonly REPO_ROOT

# The state must follow the REPOSITORY, not the directory it is checked out in.
# Keying off `basename "$REPO_ROOT"` gives every git worktree its own ledger,
# seen-set and calibration — so a cycle run from a worktree would re-file every
# finding the main checkout already triaged, and the controller would relearn
# the machine from scratch. The remote URL is the stable identity; the shared
# git common dir is the fallback when there is no remote.
repo_slug() {
  local url common
  url="$(git -C "$REPO_ROOT" config --get remote.origin.url 2>/dev/null)"
  if [ -n "$url" ]; then
    url="${url%.git}"
    basename "$url"
    return 0
  fi
  common="$(git -C "$REPO_ROOT" rev-parse --git-common-dir 2>/dev/null || echo "")"
  case "$common" in
    "")  basename "$REPO_ROOT" ;;
    /*)  basename "$(dirname "$common")" ;;
    *)   basename "$(dirname "$REPO_ROOT/$common")" ;;
  esac
}

# State lives under the stella home, not a top-level ~/.fullauto, so it sits
# beside every other durable per-user artifact and `STELLA_HOME` moves it with
# the rest. The observatory resolves the same path through `stella-home` and
# reads it read-only.
stella_home() {
  if [ -n "${STELLA_HOME:-}" ]; then printf '%s' "$STELLA_HOME"; else printf '%s' "$HOME/.stella"; fi
}

STATE_DIR="${FULLAUTO_STATE_DIR:-$(stella_home)/fullauto/$(repo_slug)}"
readonly STATE_DIR

# `gh` colorizes --json output even when it is redirected to a file, if
# CLICOLOR_FORCE is set in the environment — and it is, in this developer's
# shell. ANSI escapes inside a JSON payload are invisible in a terminal and
# fatal to every parser, which makes this the exact class of bug that survives
# a manual eyeball check. Every gh call that is PARSED goes through here.
gh_plain() { NO_COLOR=1 CLICOLOR_FORCE=0 gh "$@"; }

# Read-only views for preflight's status line. The binary owns every write to
# these (and to run.json / runs.jsonl / calibration.json / aperture beside
# them); this script only ever reads.
readonly LEDGER="$STATE_DIR/ledger.jsonl"
readonly SEEN="$STATE_DIR/seen.txt"
readonly CYCLE_FILE="$STATE_DIR/cycle"

# The daemon's own artifacts — process supervision is still shell work.
readonly DAEMON_PID="$STATE_DIR/daemon.pid"
readonly DAEMON_LOG="$STATE_DIR/daemon.log"

# ---------------------------------------------------------------------------
# Output
# ---------------------------------------------------------------------------

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  C_B=$'\033[1m'; C_R=$'\033[31m'; C_G=$'\033[32m'; C_Y=$'\033[33m'; C_0=$'\033[0m'
else
  C_B=''; C_R=''; C_G=''; C_Y=''; C_0=''
fi

say()  { printf '\n%s%s%s\n' "$C_B" "$*" "$C_0"; }
pass() { printf '  %s✓%s %s\n' "$C_G" "$C_0" "$*"; }
warn() { printf '  %s!%s %s\n' "$C_Y" "$C_0" "$*"; }
fail() { printf '  %s✗%s %s\n' "$C_R" "$C_0" "$*"; HARD_FAIL=1; }
die()  { printf '%sfullauto: %s%s\n' "$C_R" "$*" "$C_0" >&2; exit 1; }

HARD_FAIL=0

have() { command -v "$1" >/dev/null 2>&1; }

# Line count of a file that may not exist yet, as a plain number.
count_lines() {
  if [ -f "$1" ]; then wc -l < "$1" | tr -d ' '; else echo 0; fi
}

# The daemon and the bench arm still need somewhere to write their own
# artifacts (logs, pidfile, results). The state FILES beside them are seeded
# and written by the binary alone.
ensure_state_dir() {
  mkdir -p "$STATE_DIR" || die "cannot create state dir $STATE_DIR"
}

# Every ported verb needs the binary, and needs one NEW enough to carry the
# subcommand — an old release on PATH would fail with clap's "unrecognized
# subcommand", which reads like a bug in this script rather than a stale
# install. NEVER suggest bare `brew install stella`: homebrew-core's `stella`
# is an Atari 2600 emulator, and installing it exits 0.
STELLA_FULLAUTO_OK=""
require_stella() {
  # Memoized: the daemon loop delegates several times per cycle, and the
  # probe is a whole extra binary spawn each time otherwise.
  [ "$STELLA_FULLAUTO_OK" = "1" ] && return 0
  have stella || die "this verb lives in \`stella fullauto\` and needs stella on PATH.
   Install:  brew install macanderson/stella/stella
   Dev:      cargo build --release -p stella-cli  (then target/release/stella)"
  if ! stella fullauto --help >/dev/null 2>&1; then
    die "the stella on PATH ($(command -v stella)) predates \`stella fullauto\` — upgrade it (see \`$0 upgrade\`)"
  fi
  STELLA_FULLAUTO_OK=1
}

# The daemon's internal calls into the ported verbs go through here, so its
# state writes ride the exact code path every other caller uses.
run_fullauto() {
  require_stella
  stella fullauto "$@"
}

# Is the headless scope-review bypass on in any settings scope this run will
# see? Project wins per field, so project is checked first — but for a boolean
# gate like this, ON anywhere is what matters to the question being asked.
scope_bypass_on() {
  local f
  for f in "$REPO_ROOT/.stella/settings.json" "$(stella_home)/settings.json"; do
    [ -f "$f" ] || continue
    if python3 -c '
import json, sys
try:
    cfg = json.load(open(sys.argv[1])).get("agent_engine_config") or {}
except Exception:
    raise SystemExit(1)
raise SystemExit(0 if str(cfg.get("headless_scope_bypass", "")).lower() == "on" else 1)
' "$f" 2>/dev/null; then
      return 0
    fi
  done
  return 1
}

# ---------------------------------------------------------------------------
# preflight — can this machine run a cycle at all?
# ---------------------------------------------------------------------------
#
# Split into TIERS on purpose. A missing `harbor` blocks the benchmark arm and
# nothing else; a missing `gh` blocks ticket filing, which is the one thing the
# loop must never skip silently. Reporting them as one undifferentiated list
# would make the loop stop for things it could have worked around.

cmd_preflight() {
  local strict=0
  [ "${1:-}" = "--strict" ] && strict=1
  ensure_state_dir

  say "1. Repository"
  if [ -d "$REPO_ROOT/.git" ] || git -C "$REPO_ROOT" rev-parse --git-dir >/dev/null 2>&1; then
    pass "repo $REPO_ROOT @ $(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo '?')"
  else
    fail "not a git repository"
  fi
  if [ -f "$REPO_ROOT/AGENTS.md" ] && [ -f "$REPO_ROOT/Makefile" ]; then
    pass "AGENTS.md + Makefile present (this is the stella workspace)"
  else
    fail "AGENTS.md or Makefile missing — fullauto is scoped to the stella workspace"
  fi

  say "2. Fix + gate tier (required)"
  for t in git cargo make rg; do
    if have "$t"; then pass "$t"; else fail "$t is required to run the gate"; fi
  done
  if have gh && gh auth status >/dev/null 2>&1; then
    pass "gh authenticated as $(gh_plain api user --jq .login 2>/dev/null || echo '?')"
  else
    fail "gh is not authenticated — the loop cannot file the tickets it cannot fix"
  fi

  say "3. Agent tier (which engine does the fixing)"
  if have stella; then
    pass "stella on PATH -> $(command -v stella)"
    if [ "$(command -v stella)" = "$REPO_ROOT/target/release/stella" ]; then
      warn "that is the LOCAL dev build shadowing any release (see \`upgrade\`)"
    fi
    # The ported verbs (plan, cycle, aperture, seen, …) live in the binary
    # now, so a stale install does not merely weaken the fix batch — it
    # removes the loop's controls entirely.
    if stella fullauto --help >/dev/null 2>&1; then
      pass "stella fullauto present — the loop's deterministic verbs resolve"
    else
      fail "this stella predates \`stella fullauto\` — the delegated verbs cannot run; upgrade it"
    fi
    # Measured, not theorised: the first live daemon cycle died here. A cycle
    # of any real size trips scope review, and a headless run has nobody to
    # ask — so without the bypass EVERY daemon cycle burns its startup cost and
    # then aborts. Better to refuse the cycle than to discover it in the log.
    if scope_bypass_on; then
      pass "headless_scope_bypass: on — a headless cycle can pass scope review"
    else
      fail "headless_scope_bypass is OFF — a headless cycle aborts at scope review.
     Set it where the working tree is disposable:
       .stella/settings.json -> {\"agent_engine_config\": {\"headless_scope_bypass\": \"on\"}}"
    fi
  else
    warn "stella not on PATH — the fix batch falls back to the driving agent"
  fi

  say "4. Benchmark tier (optional per cycle)"
  if have arenabench; then pass "arenabench $(arenabench --version 2>/dev/null || echo '?')"
  else warn "arenabench absent — \`bench h2h\` unavailable (pip install arenabench)"; fi
  if have harbor; then pass "harbor $(harbor --version 2>/dev/null || echo '?')"
  else warn "harbor absent — the head-to-head cannot run locally"; fi
  if docker info >/dev/null 2>&1; then pass "docker reachable"
  else warn "docker unreachable — head-to-head must go to the rig"; fi
  if [ "$(uname -m)" = "x86_64" ] && [ "$(uname -s)" = "Linux" ]; then
    pass "native x86_64 Linux host — claim-eligible for a measured run"
  else
    warn "$(uname -s)/$(uname -m): NOT claim-eligible (RUNBOOK Part 0). Use --rig."
  fi
  for k in OPENROUTER_API_KEY CLAUDE_CODE_OAUTH_TOKEN; do
    if [ -n "${!k:-}" ]; then pass "$k present"; else warn "$k unset — that arm scores zero, which reads as a real loss"; fi
  done

  say "5. Ship tier (optional per cycle)"
  if have brew; then
    pass "brew $(brew --version 2>/dev/null | head -1)"
    if brew tap 2>/dev/null | grep -qx "$BREW_TAP"; then pass "tap $BREW_TAP present"
    else warn "tap $BREW_TAP missing — \`upgrade\` will add it"; fi
  else
    warn "brew absent — \`upgrade\` unavailable"
  fi

  say "6. State"
  pass "state dir $STATE_DIR"
  # Read-only, and tolerant of a directory the binary has not seeded yet —
  # preflight must be able to report on a machine where nothing ever ran.
  pass "cycle $(cat "$CYCLE_FILE" 2>/dev/null || echo 0) · $(count_lines "$SEEN") findings seen · $(count_lines "$LEDGER") cycles logged"

  echo
  if [ "$HARD_FAIL" -ne 0 ]; then
    printf '%sNOT READY%s — a required tier failed above.\n' "$C_R" "$C_0"
    [ "$strict" -eq 1 ] && exit 1
    return 1
  fi
  printf '%sREADY%s — required tiers green; warnings above only narrow the cycle.\n' "$C_G" "$C_0"
  return 0
}

# ---------------------------------------------------------------------------
# daemon — the loop outlives the terminal that started it
# ---------------------------------------------------------------------------
#
# `/loop /fullauto` dies with the terminal app. The state was always durable —
# it is files on disk — but the DRIVER was not, so quitting the terminal ended
# the loop with no record of why. The daemon detaches the driver from the tty:
# nohup so SIGHUP cannot reach it, a pidfile so it can be found again, and a
# run record so a kill -9 still leaves evidence.
#
# `daemon install` goes one step further and hands the job to launchd, which is
# what survives a logout or a reboot rather than merely a closed window.

daemon_alive() {
  [ -f "$DAEMON_PID" ] || return 1
  local pid
  pid="$(cat "$DAEMON_PID" 2>/dev/null)"
  [ -n "$pid" ] || return 1
  kill -0 "$pid" 2>/dev/null
}

# The command that runs ONE cycle. Defaults to handing the cycle prompt to
# Stella itself — this loop is Stella making Stella better, so the daemon
# dogfoods by default and only falls back when asked to.
cycle_command() {
  if [ -n "${FULLAUTO_CYCLE_CMD:-}" ]; then
    printf '%s' "$FULLAUTO_CYCLE_CMD"
    return 0
  fi
  # `stella run` takes its prompt positionally or on stdin, and reads stdin when
  # piped — which is the right channel for a prompt this long: an argv-borne
  # cycle prompt would be visible in `ps` and can hit ARG_MAX.
  #
  # The budget is per-cycle and enforced by the engine, aborting only at a safe
  # boundary between model calls (invariant 6) — never mid-tool.
  printf 'cat %s/scripts/fullauto/commands/fullauto.md | stella run --budget %s' \
    "$REPO_ROOT" "${FULLAUTO_BUDGET_USD:-10}"
}

cmd_daemon() {
  ensure_state_dir
  local verb="${1:-status}"; shift || true
  case "$verb" in
    start)     daemon_start "$@" ;;
    stop)      daemon_stop "$@" ;;
    status)    daemon_status ;;
    logs)      tail -n "${1:-40}" "$DAEMON_LOG" 2>/dev/null || echo "(no log yet)" ;;
    loop)      daemon_loop "$@" ;;
    install)   daemon_install ;;
    uninstall) daemon_uninstall ;;
    *) die "daemon: use start | stop | status | logs | install | uninstall" ;;
  esac
}

daemon_start() {
  local interval="${1:-2700}"
  if daemon_alive; then
    warn "daemon already running (pid $(cat "$DAEMON_PID"))"
    return 0
  fi
  # The run records the daemon writes now go through `stella fullauto`, so a
  # missing binary must fail HERE, in the terminal — not minutes later inside
  # a nohup'd loop whose death nobody sees.
  require_stella

  # nohup detaches from the terminal's SIGHUP; the background `&` detaches from
  # job control. Both are needed: job-control stop alone would take the process
  # down with the shell that spawned it.
  nohup "$0" daemon loop "$interval" "${2:-}" >> "$DAEMON_LOG" 2>&1 &
  local pid=$!
  echo "$pid" > "$DAEMON_PID"
  disown "$pid" 2>/dev/null || true
  say "daemon started (pid $pid, interval ${interval}s)"
  pass "log: $DAEMON_LOG"
  pass "it now survives this terminal closing; \`daemon install\` also survives logout"
}

daemon_loop() {
  local interval="${1:-2700}" once="${2:-}"
  export FULLAUTO_DRIVER=daemon
  run_fullauto run start >/dev/null

  # The cycle runs as a tracked BACKGROUND child so the signal handler can take
  # it down. Running it in the foreground meant a TERM reached this supervisor
  # and nothing else: `daemon stop` reported "daemon stopped" while the model
  # call kept running and kept spending. A stop that does not stop is worse
  # than no stop, because the operator believes it is over. (Measured: spend
  # continued climbing for minutes after the daemon was reported stopped.)
  CYCLE_PID=""
  # shellcheck disable=SC2317  # invoked via trap, not fallthrough
  daemon_shutdown() {
    if [ -n "$CYCLE_PID" ] && kill -0 "$CYCLE_PID" 2>/dev/null; then
      warn "stopping the in-flight cycle (pid $CYCLE_PID)"
      kill -TERM "-$CYCLE_PID" 2>/dev/null || kill -TERM "$CYCLE_PID" 2>/dev/null || true
      sleep 2
      kill -KILL "-$CYCLE_PID" 2>/dev/null || kill -KILL "$CYCLE_PID" 2>/dev/null || true
    fi
    run_fullauto run end --status cancelled --reason "daemon stopped" >/dev/null 2>&1
    rm -f "$DAEMON_PID"
    exit 0
  }
  trap daemon_shutdown TERM INT

  local cmd
  cmd="$(cycle_command)"
  while true; do
    printf '\n=== %s :: cycle start ===\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    run_fullauto phase cycle >/dev/null
    # `set -m` puts the child in its own process group, so `kill -TERM -PID`
    # above reaches the whole `sh -c … | stella run` pipeline rather than only
    # the wrapper shell.
    set -m
    sh -c "$cmd" &
    CYCLE_PID=$!
    set +m
    # Record WHICH process is doing the work. The supervisor's pid says a
    # daemon exists; this says the cycle is actually executing, and it is the
    # only handle anything outside this script has on the child — `sh -c` is
    # exec-optimized away, so the child is unrecognisable in the process table.
    run_fullauto run stamp-cycle-pid "$CYCLE_PID" >/dev/null
    wait "$CYCLE_PID" || warn "cycle command exited nonzero"
    CYCLE_PID=""
    run_fullauto run stamp-cycle-pid >/dev/null
    if [ "$once" = "--once" ]; then
      printf '=== %s :: single cycle done ===\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
      run_fullauto run end --status completed --reason "single cycle (--once)" >/dev/null
      rm -f "$DAEMON_PID"
      return 0
    fi
    printf '=== %s :: sleeping %ss ===\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$interval"
    run_fullauto phase sleeping >/dev/null
    sleep "$interval"
  done
}

daemon_stop() {
  if ! daemon_alive; then
    warn "no daemon running"
    rm -f "$DAEMON_PID"
    return 0
  fi
  local pid
  pid="$(cat "$DAEMON_PID")"
  kill -TERM "$pid" 2>/dev/null || true

  # Give the shutdown handler room to finish. It deliberately takes a couple of
  # seconds — it TERMs the in-flight cycle, waits, then KILLs it — and only
  # afterwards records the run as `cancelled`. A one-second escalation raced it
  # every time: the KILL landed mid-handler, the transition was never written,
  # and a run the operator stopped by hand went into the history as `crashed`.
  local waited=0
  while [ "$waited" -lt 8 ] && kill -0 "$pid" 2>/dev/null; do
    sleep 1
    waited=$((waited + 1))
  done
  if kill -0 "$pid" 2>/dev/null; then
    warn "daemon $pid did not stop after ${waited}s; sending KILL"
    kill -KILL "$pid" 2>/dev/null || true
    # The handler never ran, so nothing recorded the transition. Do it here
    # rather than leaving a hand-stopped run to age into `crashed`.
    run_fullauto run end --status cancelled --reason "daemon killed after refusing TERM" >/dev/null 2>&1 || true
  fi
  rm -f "$DAEMON_PID"
  say "daemon stopped"
}

daemon_status() {
  if daemon_alive; then
    pass "daemon running (pid $(cat "$DAEMON_PID"))"
  else
    warn "daemon not running"
  fi
  run_fullauto runs
}

launchd_label() { printf 'sh.oxagen.fullauto.%s' "$(repo_slug)"; }
launchd_plist() { printf '%s/Library/LaunchAgents/%s.plist' "$HOME" "$(launchd_label)"; }

# launchd is the only thing on macOS that restarts the loop after a logout or a
# reboot. A nohup'd process survives a closed window and nothing more.
daemon_install() {
  [ "$(uname -s)" = "Darwin" ] || die "daemon install is macOS-only (use systemd --user elsewhere)"
  local plist label
  plist="$(launchd_plist)"; label="$(launchd_label)"
  mkdir -p "$(dirname "$plist")" || die "cannot write $(dirname "$plist")"
  cat > "$plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$label</string>
  <key>ProgramArguments</key>
  <array>
    <string>$REPO_ROOT/scripts/fullauto.sh</string>
    <string>daemon</string>
    <string>loop</string>
    <string>2700</string>
  </array>
  <key>WorkingDirectory</key><string>$REPO_ROOT</string>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>$DAEMON_LOG</string>
  <key>StandardErrorPath</key><string>$DAEMON_LOG</string>
</dict>
</plist>
EOF
  pass "wrote $plist"
  if launchctl bootstrap "gui/$(id -u)" "$plist" 2>/dev/null; then
    pass "loaded — the loop now survives logout and reboot"
  else
    warn "wrote the plist but launchctl bootstrap failed; load it with:"
    printf '        launchctl bootstrap gui/%s %s\n' "$(id -u)" "$plist"
  fi
  printf '\n  Remove with: %s daemon uninstall\n' "$0"
}

daemon_uninstall() {
  local plist
  plist="$(launchd_plist)"
  launchctl bootout "gui/$(id -u)/$(launchd_label)" 2>/dev/null || true
  if [ -e "$plist" ]; then
    rm -f "$plist"
    pass "removed $plist"
  else
    warn "nothing to remove"
  fi
}

# ---------------------------------------------------------------------------
# bench — the comparator arm
# ---------------------------------------------------------------------------

cmd_bench() {
  local mode="${1:-loop}"; shift || true
  case "$mode" in
    loop) bench_loop "$@" ;;
    h2h)  bench_h2h "$@" ;;
    *) die "bench: mode must be loop (cheap, CI) or h2h (measured, vs Claude Code)" ;;
  esac
}

# The cheap arm: the nightly loop-health gate, dispatched in CI. Under a dollar,
# no local Docker, and it catches the regression class that matters most between
# releases — a turn that aborts having done nothing, dies without saying why, or
# is caught cycling.
bench_loop() {
  have gh || die "bench loop needs gh"
  say "dispatching nightly-bench.yml (loop health, CI-hosted)"
  gh workflow run nightly-bench.yml "$@" || die "workflow dispatch failed"
  sleep 6
  local run_id
  run_id="$(gh_plain run list --workflow=nightly-bench.yml --limit 1 --json databaseId --jq '.[0].databaseId')"
  pass "run $run_id — gh run watch $run_id"
  echo "$run_id"
}

# The measured arm: Claude Code vs Stella on Terminal-Bench 2.1, same model on
# both seats. Native x86_64 Linux only; the rig path starts the instance, runs,
# pulls results back, and ALWAYS stops it again — an idle rig is $45.56/day and
# has already tripped a $1,000 budget alert once.
#
# `--rig --dry-run` is the free rehearsal (#1550): every check the paid path
# depends on, then the exact commands it would run, with the instance never
# started. The paid path shipped unexecuted precisely because rehearsing it
# cost real money; the dry run is what keeps it verifiable from now on.
bench_h2h() {
  ensure_state_dir
  local use_rig=0 dry_run=0 out
  out="$STATE_DIR/h2h-$(date -u +%Y%m%dT%H%M%SZ).json"
  while [ $# -gt 0 ]; do
    case "$1" in
      --rig)     use_rig=1; shift ;;
      --dry-run) dry_run=1; shift ;;
      --out)     out="${2:?}"; shift 2 ;;
      *) die "bench h2h: unknown flag $1" ;;
    esac
  done
  if [ "$dry_run" -eq 1 ] && [ "$use_rig" -eq 0 ]; then
    die "--dry-run rehearses the rig path; add --rig"
  fi

  # An absolute FULLAUTO_MATCH points at an exported match file outside the
  # repo (the harbor `--path` shape); a relative one is resolved against the
  # repo root. The rig's own copy is always addressed relative to ~/stella,
  # so an absolute local override only affects the local arm.
  local match_path="$H2H_MATCH"
  case "$match_path" in
    /*) : ;;
    *) match_path="$REPO_ROOT/$match_path" ;;
  esac
  [ -f "$match_path" ] || die "match template not found: $match_path"

  if [ "$use_rig" -eq 0 ]; then
    have arenabench || die "arenabench not installed (pip install arenabench)"
    docker info >/dev/null 2>&1 || die "docker unreachable"
    if [ "$(uname -s)/$(uname -m)" != "Linux/x86_64" ] && [ -z "${FULLAUTO_ALLOW_EMULATED:-}" ]; then
      die "$(uname -s)/$(uname -m) is not a claim-eligible host (RUNBOOK Part 0).
     Use --rig, or set FULLAUTO_ALLOW_EMULATED=1 for a throwaway signal run."
    fi
    say "running $H2H_MATCH locally"
    DOCKER_DEFAULT_PLATFORM=linux/amd64 \
      arenabench run "$match_path" --progress --results "$out" \
      || die "arenabench run failed"
    pass "results -> $out"
    echo "$out"
    return 0
  fi

  # --- Free checks: everything the paid path depends on, at zero cost -------
  # Also the whole of --dry-run. Nothing below this line starts the instance
  # until the "starting rig" banner, so these can run any number of times
  # between real runs without spending a cent.
  say "rig preflight (free — the instance stays down)"
  if have aws; then pass "aws CLI present"; else fail "aws CLI missing (brew install awscli)"; fi
  if have ssh && have scp; then pass "ssh + scp present"; else fail "ssh/scp missing"; fi
  if [ -f "$RIG_KEY" ]; then
    chmod 600 "$RIG_KEY" 2>/dev/null || true
    pass "rig key: $RIG_KEY"
  else
    fail "rig key not found at $RIG_KEY (place it there, or point FULLAUTO_RIG_KEY at it)"
  fi
  local state
  if have aws; then
    if aws sts get-caller-identity --output text >/dev/null 2>&1; then
      pass "aws credentials valid"
    else
      fail "aws credentials invalid or expired (aws sts get-caller-identity failed)"
    fi
    state="$(aws ec2 describe-instances --region "$RIG_REGION" --instance-ids "$RIG_INSTANCE" \
             --query 'Reservations[0].Instances[0].State.Name' --output text 2>/dev/null || echo unknown)"
    case "$state" in
      stopped) pass "rig $RIG_INSTANCE exists and is stopped (not billing)" ;;
      running) warn "rig $RIG_INSTANCE is ALREADY RUNNING — billing \$1.90/hr right now" ;;
      unknown) fail "rig $RIG_INSTANCE not found in $RIG_REGION (or describe-instances failed)" ;;
      *)       warn "rig $RIG_INSTANCE is $state" ;;
    esac
  fi

  if [ "$dry_run" -eq 1 ]; then
    say "dry run — the paid path would execute, in order:"
    # shellcheck disable=SC2016  # deliberate: `$ip` is literal in the printed
    # plan — the paid path resolves it at run time, not this echo.
    {
      printf '  aws ec2 start-instances --region %s --instance-ids %s\n' "$RIG_REGION" "$RIG_INSTANCE"
      printf '  aws ec2 wait instance-running --region %s --instance-ids %s\n' "$RIG_REGION" "$RIG_INSTANCE"
      printf '  ip=$(aws ec2 describe-instances ... PublicIpAddress)  # read fresh: it changes across stop/start\n'
      printf '  ssh -i %s %s@$ip true                                 # polled, up to %s x 10s\n' \
        "$RIG_KEY" "$RIG_USER" "${FULLAUTO_RIG_SSH_TRIES:-20}"
      printf '  ssh ... "test -d ~/stella && command -v arenabench && docker info"  # remote preflight, BEFORE the match\n'
      printf '  ssh ... "cd ~/stella && DOCKER_DEFAULT_PLATFORM=linux/amd64 arenabench run %s --results /tmp/h2h.json"\n' \
        "$H2H_MATCH"
      printf '  scp ... %s@$ip:/tmp/h2h.json %s\n' "$RIG_USER" "$out"
      printf '  aws ec2 stop-instances --region %s --instance-ids %s  # EXIT/INT/TERM trap: fires on every path out\n' \
        "$RIG_REGION" "$RIG_INSTANCE"
    }
    echo
    if [ "$HARD_FAIL" -ne 0 ]; then
      printf '%sNOT READY%s — fix the failed checks above before paying for a run.\n' "$C_R" "$C_0"
      return 1
    fi
    printf '%sREADY%s — every check green; drop --dry-run to run it for real.\n' "$C_G" "$C_0"
    return 0
  fi

  [ "$HARD_FAIL" -eq 0 ] || die "rig preflight failed — nothing was started"

  # The rig bills by the hour. Stopping it is the only step in this script that
  # MUST happen even on failure, interrupt, or a dropped SSH connection — and a
  # stop REQUEST is not a stopped instance, so the trap also confirms the
  # transition and prints the manual command when it cannot.
  # shellcheck disable=SC2317  # invoked via trap, not fallthrough
  stop_rig() {
    # Run once: an INT would otherwise fire this handler, then EXIT would fire
    # it again on the way out. And everything here goes to stderr — the trap
    # fires AFTER the results path is echoed, and a caller capturing stdout
    # (`out=$(bench_h2h …)`) must receive the path as the last line, not this
    # commentary.
    trap - EXIT INT TERM
    say "stopping rig $RIG_INSTANCE (idle costs \$45.56/day)" >&2
    if ! aws ec2 stop-instances --region "$RIG_REGION" --instance-ids "$RIG_INSTANCE" >/dev/null 2>&1; then
      warn "STOP REQUEST FAILED — the rig is billing until you run:" >&2
      printf '        aws ec2 stop-instances --region %s --instance-ids %s\n' "$RIG_REGION" "$RIG_INSTANCE" >&2
      return
    fi
    if aws ec2 wait instance-stopped --region "$RIG_REGION" --instance-ids "$RIG_INSTANCE" >/dev/null 2>&1; then
      pass "rig confirmed stopped" >&2
    else
      warn "stop requested but not yet confirmed — verify with:" >&2
      printf "        aws ec2 describe-instances --region %s --instance-ids %s --query 'Reservations[0].Instances[0].State.Name' --output text\n" \
        "$RIG_REGION" "$RIG_INSTANCE" >&2
    fi
  }
  trap stop_rig EXIT INT TERM

  say "starting rig $RIG_INSTANCE"
  aws ec2 start-instances --region "$RIG_REGION" --instance-ids "$RIG_INSTANCE" >/dev/null \
    || die "could not start rig"
  aws ec2 wait instance-running --region "$RIG_REGION" --instance-ids "$RIG_INSTANCE" \
    || die "rig never reached running"

  local ip
  ip="$(aws ec2 describe-instances --region "$RIG_REGION" --instance-ids "$RIG_INSTANCE" \
        --query 'Reservations[0].Instances[0].PublicIpAddress' --output text)"
  if [ -z "$ip" ] || [ "$ip" = "None" ]; then die "rig has no public IP"; fi
  pass "rig at $ip (IP changes across stop/start — never hardcode it)"

  # SSH can be refused for a minute after instance-running; poll rather than
  # racing it. The bound is a knob because 20 x 10s is a guess, not a
  # measurement — pin FULLAUTO_RIG_SSH_TRIES when the rig proves slower.
  local tries=0 max_tries="${FULLAUTO_RIG_SSH_TRIES:-20}"
  until ssh -o StrictHostKeyChecking=accept-new -o ConnectTimeout=8 \
        -i "$RIG_KEY" "$RIG_USER@$ip" true 2>/dev/null; do
    tries=$((tries + 1))
    [ "$tries" -ge "$max_tries" ] && die "rig unreachable over ssh after $max_tries tries"
    sleep 10
  done
  pass "ssh up"

  # Assert the rig can host the match BEFORE paying for it: a missing checkout
  # or a bare PATH turns a multi-hour billed run into garbage, and each is a
  # one-second check. `die` exits through the trap, so a failed assert stops
  # the instance having spent cents, not hours.
  say "remote preflight"
  ssh -o StrictHostKeyChecking=accept-new -i "$RIG_KEY" "$RIG_USER@$ip" \
    'test -d "$HOME/stella" || { echo "~/stella missing on the rig" >&2; exit 70; }
     command -v arenabench >/dev/null 2>&1 || { echo "arenabench not on the rig PATH" >&2; exit 71; }
     docker info >/dev/null 2>&1 || { echo "docker unreachable on the rig" >&2; exit 72; }' \
    || die "the rig cannot host the match (see above) — stopping it before the match spends anything"
  pass "rig can host the match (~/stella, arenabench, docker)"

  say "running the head-to-head on the rig"
  ssh -o StrictHostKeyChecking=accept-new -i "$RIG_KEY" "$RIG_USER@$ip" \
    "set -e; cd ~/stella && git fetch --quiet && git checkout --quiet origin/main -- . || true; \
     DOCKER_DEFAULT_PLATFORM=linux/amd64 arenabench run $H2H_MATCH --progress --results /tmp/h2h.json" \
    || warn "the match returned nonzero — read the results before calling it a loss"

  # A missing results file is an error, not a phantom path: the old shape
  # warned and then echoed a path that did not exist, which every caller took
  # as success. When interpreting what does come back, the real reward lives
  # at `verifier_result.rewards.reward` — `verifier_result.reward` is always
  # null and scores every run as zero.
  if ! scp -o StrictHostKeyChecking=accept-new -i "$RIG_KEY" \
       "$RIG_USER@$ip:/tmp/h2h.json" "$out" 2>/dev/null; then
    die "no results file came back — the match died before writing /tmp/h2h.json; there is nothing to score"
  fi
  python3 -m json.tool "$out" >/dev/null 2>&1 \
    || die "results file does not parse as JSON: $out"
  pass "results -> $out"
  echo "$out"
}

# ---------------------------------------------------------------------------
# upgrade — ship, then consume what shipped
# ---------------------------------------------------------------------------
#
# The delivery half of "fully autonomous delivery": install the RELEASED build
# from the tap and stop shadowing it with the dev build, so what runs on this
# machine is what a user gets. Idempotent, backed up, and it fails closed: if
# the rc file does not contain the shadow line verbatim, it edits nothing.

cmd_upgrade() {
  local apply=1 keep_dev_alias=1 skip_brew=0 rc="${FULLAUTO_RC:-$HOME/.zshrc}"
  while [ $# -gt 0 ]; do
    case "$1" in
      --dry-run) apply=0; shift ;;
      --no-dev-alias) keep_dev_alias=0; shift ;;
      # The two halves are independent on purpose: the formula builds from
      # source and takes minutes, while the PATH surgery is instant and is the
      # part that can go wrong. Being able to run either alone is what makes
      # the risky half testable without paying for the slow half.
      --skip-brew) skip_brew=1; shift ;;
      --rc) rc="${2:?}"; shift 2 ;;
      *) die "upgrade: unknown flag $1" ;;
    esac
  done

  if [ "$skip_brew" -eq 1 ]; then
    warn "skipping the Homebrew half (--skip-brew): PATH surgery only"
  else
    upgrade_brew "$apply"
  fi
  upgrade_unshadow "$apply" "$rc" "$keep_dev_alias"
}

upgrade_brew() {
  local apply="$1"
  have brew || die "upgrade needs Homebrew"

  say "1. Tap"
  if brew tap 2>/dev/null | grep -qx "$BREW_TAP"; then
    pass "$BREW_TAP present"
  elif [ "$apply" -eq 1 ]; then
    brew tap "$BREW_TAP" "$BREW_TAP_REMOTE" || die "brew tap failed"
    pass "tapped $BREW_TAP"
  else
    warn "would run: brew tap $BREW_TAP $BREW_TAP_REMOTE"
  fi

  say "2. Formula"
  # NEVER `brew install stella`: homebrew-core's `stella` is an Atari 2600
  # emulator and installing it exits 0, which is the worst possible failure.
  if brew list --formula 2>/dev/null | grep -qx "stella"; then
    if [ "$apply" -eq 1 ]; then
      brew upgrade "$BREW_FORMULA" 2>&1 | tail -5 || warn "already at the newest version"
    else
      warn "would run: brew upgrade $BREW_FORMULA"
    fi
  else
    if [ "$apply" -eq 1 ]; then
      brew install "$BREW_FORMULA" || die "brew install $BREW_FORMULA failed"
    else
      warn "would run: brew install $BREW_FORMULA"
    fi
  fi
  local brewed
  brewed="$(brew --prefix 2>/dev/null)/bin/stella"
  if [ -x "$brewed" ]; then
    pass "installed: $brewed ($("$brewed" --version 2>/dev/null || echo '?'))"
  else
    warn "$brewed is not executable yet"
  fi
}

upgrade_unshadow() {
  local apply="$1" rc="$2" keep_dev_alias="$3"

  say "3. PATH shadow"
  if [ ! -f "$rc" ]; then
    warn "$rc does not exist — nothing to unshadow"
  elif ! grep -qxF "$SHADOW_PATH_LINE" "$rc"; then
    pass "no shadow line in $rc (already removed, or never present)"
  elif [ "$apply" -eq 0 ]; then
    warn "would comment out in $rc:"
    printf '        %s\n' "$SHADOW_PATH_LINE"
  else
    local backup
    backup="$rc.fullauto-$(date -u +%Y%m%dT%H%M%SZ).bak"
    cp "$rc" "$backup" || die "could not back up $rc"
    pass "backup: $backup"
    # Comment out rather than delete: a commented line is self-documenting and
    # trivially reversible; a deleted one leaves the reader wondering.
    python3 - "$rc" "$SHADOW_PATH_LINE" "$keep_dev_alias" <<'PY'
import sys
rc, shadow, keep = sys.argv[1], sys.argv[2], sys.argv[3] == "1"
lines = open(rc).read().splitlines()
out, replaced = [], False
for ln in lines:
    if ln == shadow and not replaced:
        # Say plainly that the surrounding commentary is now historical. The
        # block above this line explains why the shadow exists; leaving it
        # unqualified next to a disabled shadow makes the file lie to whoever
        # reads it next, and a stale comment is a bug like any other.
        out.append("# --- NOTE: the commentary above is HISTORICAL as of the line below. ---")
        out.append("# `scripts/fullauto.sh upgrade` disabled this prepend. It used to shadow")
        out.append("# the Homebrew release with the local dev build; `stella` now resolves to")
        out.append("# the tap build (macanderson/stella/stella), which is what a user gets.")
        out.append("# " + shadow)
        if keep:
            out.append("# The dev build is still one word away:")
            out.append('stella_dev() { "$HOME/Projects/stella/target/release/stella" "$@"; }')
        replaced = True
    else:
        out.append(ln)
open(rc, "w").write("\n".join(out) + "\n")
print("commented out" if replaced else "no change")
PY
    pass "shadow removed — restore with: cp $backup $rc"
  fi

  say "4. Resolution after a new shell"
  printf '  this shell:  %s\n' "$(command -v stella || echo '(none)')"
  printf '  new shell:   %s\n' "$(env -i HOME="$HOME" PATH="$(brew --prefix)/bin:/usr/bin:/bin" \
                                   sh -c 'command -v stella' 2>/dev/null || echo '(none)')"
  echo
  printf '  Open a new terminal (or run: exec zsh) for the change to take.\n'
}

# ---------------------------------------------------------------------------
# install-commands — put /fullauto where Claude Code will find it
# ---------------------------------------------------------------------------
#
# The canonical copies are committed here; `.claude/` is gitignored in this
# repo (#448), so the commands cannot live at .claude/commands and survive.
# This copies them into the user-scope command directory instead.

cmd_install_commands() {
  local src="$REPO_ROOT/scripts/fullauto/commands"
  local dst="${FULLAUTO_COMMAND_DIR:-$HOME/.claude/commands}"
  [ -d "$src" ] || die "no command sources at $src"
  mkdir -p "$dst/fullauto" || die "cannot write $dst"
  cp "$src/fullauto.md" "$dst/fullauto.md" || die "copy failed"
  cp "$src"/fullauto/*.md "$dst/fullauto/" || die "copy failed"
  say "installed into $dst"
  pass "/fullauto"
  local f
  for f in "$src"/fullauto/*.md; do
    pass "/fullauto:$(basename "$f" .md)"
  done
  echo
  printf '  Loop it with:  /loop 45m /fullauto\n'
}

# ---------------------------------------------------------------------------

# The header comment IS the usage text. Read it structurally — from line 2 to
# the first line that is not a comment — so adding a line to the header can
# never silently truncate or overrun the help output.
usage() {
  awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"
}

main() {
  local sub="${1:-}"; shift || true
  case "$sub" in
    preflight|doctor)  cmd_preflight "$@" ;;

    # The ported verbs (#1548): one exec each, flags passed through verbatim.
    # The spellings are identical on both sides, so nothing here can drift
    # from the binary's behaviour — and `make fullauto-test` drives these
    # delegations end-to-end.
    plan|calibrate|aperture|watch|metrics|queue|state|seen|runs|phase)
      require_stella
      exec stella fullauto "$sub" "$@" ;;
    run)
      require_stella
      case "${1:-status}" in
        status) shift || true; exec stella fullauto run list "$@" ;;
        *)      exec stella fullauto run "$@" ;;
      esac ;;
    cycle-begin)  require_stella; exec stella fullauto cycle begin "$@" ;;
    cycle-end)    require_stella; exec stella fullauto cycle end "$@" ;;

    daemon)            cmd_daemon "$@" ;;
    bench)             cmd_bench "$@" ;;
    upgrade)           cmd_upgrade "$@" ;;
    install-commands)  cmd_install_commands "$@" ;;
    ""|-h|--help)      usage ;;
    *) die "unknown subcommand: $sub (try --help)" ;;
  esac
}

main "$@"
