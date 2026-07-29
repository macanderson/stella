#!/usr/bin/env bash
#
# record-demo.sh — record a terminal session of stella (or any command) and
# render it into a timelapse gif/mp4, even for runs that take hours or days.
#
#   Usage:  scripts/record-demo.sh [options] [-- <command...>]
#           scripts/record-demo.sh --render-only <file.cast> [options]
#
#   With no command, records scripts/demo-scenario.sh — a many-turn
#   build/test/lint/doc marathon over the whole workspace that runs for
#   hours. Cap it with --limit for a short take.
#
#     -o, --out-dir DIR    output directory (default: recordings/)
#     -n, --name NAME      basename for the outputs (default: demo-<timestamp>)
#     -t, --target SECS    target video length; playback speed is computed
#                          from the recording's real duration (default: 60)
#     -l, --limit MINS     wall-clock cap on the recorded command; it is
#                          stopped cleanly at the limit and the cast is
#                          rendered as usual (default: 0 = no cap)
#     -i, --idle SECS      cap on recorded idle time between output events;
#                          dead air is collapsed at record time (default: 2)
#         --fps N          rendered frame-rate cap (default: 30)
#         --cols N         recorded terminal width (default: 100)
#         --rows N         recorded terminal height (default: 30)
#         --theme NAME     agg color theme (default: dracula)
#         --cast-only      record but skip rendering
#         --render-only F  skip recording; render an existing .cast file
#     -h, --help           show this header
#
# Why this shape: the recording layer is asciinema, whose .cast file is an
# append-only JSON-lines log of terminal output events. Its size scales with
# how much the command *prints*, not with wall-clock time, so a build or
# agent run that takes six hours produces a file measured in kilobytes or
# megabytes — and the video is rendered *after the fact* at whatever speed
# turns the real duration into the target length. That separation (record
# cheap, render later) is what makes extremely long horizons practical;
# screen-capturing pixels with ffmpeg for hours is the wrong tool for this.
#
# Recipes:
#
#   # The full multi-hour marathon, rendered as a 60-second timelapse:
#   scripts/record-demo.sh
#
#   # Short take: stop after 10 minutes, render a 30-second video:
#   scripts/record-demo.sh --limit 10 --target 30
#
#   # Record a specific command instead of the default scenario:
#   scripts/record-demo.sh -- make build-release
#
#   # Detach for a multi-hour run (survives your terminal closing):
#   nohup scripts/record-demo.sh -n overnight > record.log 2>&1 &
#
#   # Or keep it attachable under tmux:
#   tmux new -d -s rec 'scripts/record-demo.sh -n soak'
#
#   # Re-render yesterday's cast as a 30-second cut:
#   scripts/record-demo.sh --render-only recordings/soak.cast --target 30
#
#   # For multi-DAY horizons, record one segment per day and stitch the
#   # rendered mp4s: printf "file '%s'\n" recordings/day-*.mp4 > list.txt
#   #                ffmpeg -f concat -safe 0 -i list.txt -c copy full.mp4
#
# In the cloud: .github/workflows/record-demo.yml dispatches this script on a
# fresh runner (up to ~6 h) and uploads the .cast/.gif/.mp4 as an artifact.
#
# Tooling: asciinema (recorder) and agg (renderer) are auto-installed on
# first use — agg from its prebuilt release binary, falling back to cargo.
# ffmpeg is optional; without it you still get the .cast and .gif.

set -euo pipefail

# ── Output helpers (house style: scripts/release.sh) ────────────────────────
bold() { printf '\033[1m%s\033[0m\n' "$*"; }
info() { printf '\033[36m▸ %s\033[0m\n' "$*"; }
ok()   { printf '\033[32m✔ %s\033[0m\n' "$*"; }
die()  { printf '\033[31mERROR: %s\033[0m\n' "$*" >&2; exit 1; }

usage() { sed -n '2,29p' "$0" | sed 's/^#//; s/^ //'; }

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

OUT_DIR="recordings"
NAME="demo-$(date +%Y%m%d-%H%M%S)"
TARGET=60
LIMIT=0
IDLE=2
FPS=30
COLS=100
ROWS=30
THEME="dracula"
CAST_ONLY=0
RENDER_ONLY=""
CMD=()

while [ $# -gt 0 ]; do
  case "$1" in
    -o|--out-dir)   OUT_DIR="$2"; shift 2 ;;
    -n|--name)      NAME="$2"; shift 2 ;;
    -t|--target)    TARGET="$2"; shift 2 ;;
    -l|--limit)     LIMIT="$2"; shift 2 ;;
    -i|--idle)      IDLE="$2"; shift 2 ;;
    --fps)          FPS="$2"; shift 2 ;;
    --cols)         COLS="$2"; shift 2 ;;
    --rows)         ROWS="$2"; shift 2 ;;
    --theme)        THEME="$2"; shift 2 ;;
    --cast-only)    CAST_ONLY=1; shift ;;
    --render-only)  RENDER_ONLY="$2"; shift 2 ;;
    -h|--help)      usage; exit 0 ;;
    --)             shift; CMD=("$@"); break ;;
    *)              die "unknown option: $1 (see --help)" ;;
  esac
done

# No command → the long-horizon default scenario.
if [ -z "$RENDER_ONLY" ] && [ ${#CMD[@]} -eq 0 ]; then
  CMD=("$SCRIPT_DIR/demo-scenario.sh")
fi

# A pty is allocated by asciinema itself, so this works on headless CI
# runners — but TERM must be set for the recorded program's own sake, and
# SHELL pinned to bash so the %q-quoted command string parses as written.
export TERM="${TERM:-xterm-256color}"
SHELL="$(command -v bash)"
export SHELL

# ── Tool bootstrap ──────────────────────────────────────────────────────────
LOCAL_BIN="$HOME/.local/bin"
export PATH="$LOCAL_BIN:$HOME/.cargo/bin:$PATH"

ensure_asciinema() {
  command -v asciinema > /dev/null 2>&1 && return
  info "asciinema not found; installing..."
  # Pin to 2.x: asciinema 3.x records the new asciicast v3 format (relative
  # delta timestamps) and reworks the `rec` CLI, both of which break this
  # script — the exit-code smuggling and the absolute-timestamp duration
  # read below all assume 2.x.
  if command -v pipx > /dev/null 2>&1; then
    pipx install 'asciinema<3'
  elif command -v python3 > /dev/null 2>&1; then
    python3 -m pip install --user --quiet 'asciinema<3'
  else
    die "asciinema is required; install it (https://asciinema.org) and re-run"
  fi
  command -v asciinema > /dev/null 2>&1 || die "asciinema install failed"
  ok "asciinema installed"
}

agg_target() {
  local os arch
  os="$(uname -s)" arch="$(uname -m)"
  case "$os-$arch" in
    Linux-x86_64)              echo "x86_64-unknown-linux-gnu" ;;
    Linux-aarch64|Linux-arm64) echo "aarch64-unknown-linux-gnu" ;;
    Darwin-x86_64)             echo "x86_64-apple-darwin" ;;
    Darwin-arm64)              echo "aarch64-apple-darwin" ;;
    *)                         echo "" ;;
  esac
}

ensure_agg() {
  command -v agg > /dev/null 2>&1 && return
  info "agg (cast renderer) not found; installing..."
  local target url
  target="$(agg_target)"
  if [ -n "$target" ]; then
    url="https://github.com/asciinema/agg/releases/latest/download/agg-$target"
    mkdir -p "$LOCAL_BIN"
    if curl -fsSL "$url" -o "$LOCAL_BIN/agg"; then
      chmod +x "$LOCAL_BIN/agg"
    fi
  fi
  if ! command -v agg > /dev/null 2>&1; then
    command -v cargo > /dev/null 2>&1 ||
      die "could not download agg and cargo is unavailable to build it"
    info "prebuilt agg unavailable; building via cargo (one-time)..."
    cargo install --quiet --git https://github.com/asciinema/agg
  fi
  command -v agg > /dev/null 2>&1 || die "agg install failed"
  ok "agg installed"
}

# ── Record ──────────────────────────────────────────────────────────────────
mkdir -p "$OUT_DIR"
CAST="$OUT_DIR/$NAME.cast"

if [ -n "$RENDER_ONLY" ]; then
  [ -f "$RENDER_ONLY" ] || die "no such cast file: $RENDER_ONLY"
  CAST="$RENDER_ONLY"
  NAME="$(basename "$CAST" .cast)"
  OUT_DIR="$(dirname "$CAST")"
else
  ensure_asciinema
  # %q-quote each argument so multi-word args survive the trip through the
  # shell asciinema uses to run the command.
  CMD_STR="$(printf '%q ' "${CMD[@]}")"
  CMD_STR="${CMD_STR% }"
  if [ "$LIMIT" -gt 0 ] 2> /dev/null; then
    # `timeout` is GNU coreutils: absent from stock macOS, where brew installs
    # it as `gtimeout`. Resolve it up front — otherwise the failure is a
    # confusing "command not found" recorded *inside* the cast.
    TIMEOUT_BIN="$(command -v timeout || command -v gtimeout || true)"
    [ -n "$TIMEOUT_BIN" ] || die "--limit needs GNU timeout (macOS: brew install coreutils)"
    # No --foreground: timeout must signal the whole process group so the
    # actual worker (cargo, a test binary) sees the INT, not just the shell.
    # A hard kill follows 30s later if anything shrugs it off.
    CMD_STR="$TIMEOUT_BIN --signal=INT --kill-after=30 ${LIMIT}m $CMD_STR"
    info "wall-clock limit: ${LIMIT} minute(s)"
  fi
  bold "Recording: $CMD_STR"
  info "cast file: $CAST (idle capped at ${IDLE}s)"
  # asciinema 2.x exits 0 regardless of the recorded command's status, so
  # smuggle the real status out through a file. A failing build still leaves
  # a valid cast — render it anyway for the post-mortem.
  STATUS_FILE="$(mktemp)"
  set +e
  asciinema rec --overwrite --quiet \
    --idle-time-limit "$IDLE" \
    --cols "$COLS" --rows "$ROWS" \
    --command "{ $CMD_STR; }; echo \$? > $STATUS_FILE" \
    "$CAST"
  ASC_STATUS=$?
  set -e
  REC_STATUS="$(cat "$STATUS_FILE" 2> /dev/null || true)"
  rm -f "$STATUS_FILE"
  case "$REC_STATUS" in '' | *[!0-9]*) REC_STATUS=$ASC_STATUS ;; esac
  [ -s "$CAST" ] || die "recording produced no cast file"
  # 124 = timeout fired; 137 = the kill-after backstop had to finish the job.
  if { [ "$REC_STATUS" -eq 124 ] || [ "$REC_STATUS" -eq 137 ]; } && [ "$LIMIT" -gt 0 ]; then
    info "stopped at the ${LIMIT}-minute limit (planned cutoff)"
    REC_STATUS=0
  elif [ "$REC_STATUS" -ne 0 ]; then
    info "recorded command exited $REC_STATUS — rendering the cast anyway"
  fi
fi

# ── Render ──────────────────────────────────────────────────────────────────
if [ "$CAST_ONLY" -eq 1 ]; then
  ok "cast only: $CAST"
  exit "${REC_STATUS:-0}"
fi

# Real duration = timestamp of the last event line ("[12.34, \"o\", ...]").
DURATION="$(awk 'END { sub(/^\[/, ""); sub(/,.*/, ""); print }' "$CAST")"
case "$DURATION" in
  ''|*[!0-9.]*) die "could not read duration from $CAST (empty recording?)" ;;
esac

SPEED="$(awk -v d="$DURATION" -v t="$TARGET" \
  'BEGIN { s = d / t; if (s < 1) s = 1; printf "%.2f", s }')"

ensure_agg
GIF="$OUT_DIR/$NAME.gif"
info "rendering ${DURATION}s of terminal time at ${SPEED}x -> $GIF"
agg --speed "$SPEED" --fps-cap "$FPS" --theme "$THEME" "$CAST" "$GIF"

MP4=""
if command -v ffmpeg > /dev/null 2>&1; then
  MP4="$OUT_DIR/$NAME.mp4"
  info "encoding $MP4"
  # yuv420p + even dimensions for player compatibility everywhere.
  ffmpeg -hide_banner -loglevel error -y -i "$GIF" \
    -movflags faststart -pix_fmt yuv420p \
    -vf "scale=trunc(iw/2)*2:trunc(ih/2)*2" "$MP4"
else
  info "ffmpeg not found — skipping mp4 (the .gif and .cast are complete)"
fi

bold "Done."
ok "cast:  $CAST (${DURATION}s of terminal time, idle collapsed)"
ok "gif:   $GIF (${SPEED}x timelapse, ~${TARGET}s)"
[ -n "$MP4" ] && ok "video: $MP4"
exit "${REC_STATUS:-0}"
