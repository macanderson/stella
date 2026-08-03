#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
#
# Bring up a virtual screen, put a live terminal on it, and film it.
#
#   Xvfb :99  ->  xterm running render.py  ->  ffmpeg -f x11grab -> H.264 MP4
#
# The only genuinely delicate part is shutdown. An MP4 needs its moov atom
# written at the end, and a container stopped with SIGKILL leaves a truncated,
# unplayable file. So ffmpeg is backgrounded, SIGTERM/SIGINT are trapped, and
# the handler asks ffmpeg to finish cleanly (SIGINT, which ffmpeg treats as
# "stop encoding and finalise") before this script exits. Docker's default
# 10-second grace period is enough for that; the supervisor asks for more.
set -eu

WIDTH="${ARENA_WIDTH:-1440}"
HEIGHT="${ARENA_HEIGHT:-900}"
FPS="${ARENA_FPS:-10}"
FONT_SIZE="${ARENA_FONT_SIZE:-13}"
EVENTS="${ARENA_EVENTS:-/logs/agent/stella-events.jsonl}"
OUTPUT="${ARENA_OUTPUT:-/out/recording.mp4}"
DISPLAY_NUM="${ARENA_DISPLAY:-:99}"

mkdir -p "$(dirname "$OUTPUT")"

log() { printf '[arena-recorder] %s\n' "$*" >&2; }

Xvfb "$DISPLAY_NUM" -screen 0 "${WIDTH}x${HEIGHT}x24" -nolisten tcp >/dev/null 2>&1 &
XVFB_PID=$!

# Wait for the display to accept connections. Polling xdpyinfo is the only
# reliable signal; Xvfb's pid existing does not mean the socket is up, and a
# fixed sleep is either a stall or a race depending on the host.
i=0
while [ "$i" -lt 100 ]; do
  if xdpyinfo -display "$DISPLAY_NUM" >/dev/null 2>&1; then break; fi
  i=$((i + 1))
  sleep 0.1
done
if ! xdpyinfo -display "$DISPLAY_NUM" >/dev/null 2>&1; then
  log "FATAL: Xvfb never came up on $DISPLAY_NUM"
  kill "$XVFB_PID" 2>/dev/null || true
  exit 1
fi
log "display $DISPLAY_NUM up at ${WIDTH}x${HEIGHT}"

# Size the terminal to fill the screen — by MEASURING the cell, not guessing.
#
# `-fs` is a size in *points*, and the pixel size of a point depends on the
# display's DPI. Deriving rows/cols from the point size directly produced an
# xterm window ~1.4x the framebuffer: the header rendered, and everything
# below it was painted off-screen. The failure looks exactly like "the
# renderer is broken" and is entirely a geometry error, so the cell size is
# now obtained from a throwaway xterm rather than assumed.
probe_geometry() {
  DISPLAY="$DISPLAY_NUM" xterm -geometry 80x24+0+0 \
    -fa 'DejaVu Sans Mono' -fs "$FONT_SIZE" -bw 0 +sb -u8 \
    -title arena-probe -e sleep 30 >/dev/null 2>&1 &
  PROBE_PID=$!
  probe_i=0
  while [ "$probe_i" -lt 100 ]; do
    PROBE_GEOM="$(DISPLAY="$DISPLAY_NUM" xwininfo -name arena-probe 2>/dev/null \
      | awk '/Width:/{w=$2} /Height:/{h=$2} END{if (w>0 && h>0) print w" "h}')"
    [ -n "$PROBE_GEOM" ] && break
    probe_i=$((probe_i + 1))
    sleep 0.1
  done
  kill "$PROBE_PID" 2>/dev/null || true
  [ -n "$PROBE_GEOM" ] || return 1
  CELL_W=$(( $(echo "$PROBE_GEOM" | cut -d' ' -f1) / 80 ))
  CELL_H=$(( $(echo "$PROBE_GEOM" | cut -d' ' -f2) / 24 ))
  [ "$CELL_W" -gt 0 ] && [ "$CELL_H" -gt 0 ]
}

if probe_geometry; then
  COLS=$(( WIDTH / CELL_W ))
  ROWS=$(( HEIGHT / CELL_H ))
  log "measured cell ${CELL_W}x${CELL_H}px -> ${COLS}x${ROWS} cells"
else
  # Conservative fallback: undersized leaves a black margin, which is
  # cosmetic. Oversized paints the transcript off-screen, which is not.
  COLS=$(( WIDTH / 12 ))
  ROWS=$(( HEIGHT / 24 ))
  log "cell probe failed; falling back to ${COLS}x${ROWS}"
fi
[ "$COLS" -gt 40 ] || COLS=80
[ "$ROWS" -gt 10 ] || ROWS=24

# `-u8` forces UTF-8 interpretation. Without it xterm follows the locale, and
# a POSIX locale turns every box-drawing character into three latin-1 glyphs —
# which triples the width of a full-width rule, wraps it, and scrolls the
# transcript off screen. Belt and braces with LANG/LC_ALL in the image.
DISPLAY="$DISPLAY_NUM" xterm \
  -geometry "${COLS}x${ROWS}+0+0" \
  -fa 'DejaVu Sans Mono' -fs "$FONT_SIZE" \
  -bg '#07080B' -fg '#D6DBE5' \
  -b 18 -bw 0 +sb -u8 \
  -xrm 'xterm*allowTitleOps: false' \
  -e python3 /opt/arena/render.py "$EVENTS" >"${ARENA_RENDER_LOG:-/dev/null}" 2>&1 &
XTERM_PID=$!

# Give the terminal a beat to paint its first frame, so the recording does not
# open on an empty grey rectangle.
sleep 0.6

ffmpeg -nostdin -loglevel error \
  -f x11grab -video_size "${WIDTH}x${HEIGHT}" -framerate "$FPS" -i "$DISPLAY_NUM" \
  -c:v libx264 -preset veryfast -crf 26 -pix_fmt yuv420p \
  -movflags +faststart \
  -y "$OUTPUT" &
FFMPEG_PID=$!
log "recording -> $OUTPUT"

finish() {
  # SIGINT, not SIGTERM: ffmpeg finalises the container on INT and dies
  # abruptly on TERM. This is the difference between a playable file and a
  # truncated one.
  kill -INT "$FFMPEG_PID" 2>/dev/null || true
  wait "$FFMPEG_PID" 2>/dev/null || true
  kill "$XTERM_PID" "$XVFB_PID" 2>/dev/null || true
  log "finalised $OUTPUT"
  exit 0
}
trap finish TERM INT

wait "$FFMPEG_PID" 2>/dev/null || true
kill "$XTERM_PID" "$XVFB_PID" 2>/dev/null || true
