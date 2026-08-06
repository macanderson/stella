#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
#
# Bring up a virtual screen, put a live terminal on it, and film it.
#
#   Xvfb :99  ->  xterm running render.py  ->  ffmpeg -f x11grab -> H.264 MP4
#
# Shutdown used to be the delicate part: a plain MP4 needs its moov atom
# written at the end, so a container stopped with SIGKILL left a file with
# every frame in it and no index, which nothing can play. The output is now
# a fragmented MP4 (see the ffmpeg invocation), so the file is playable at
# every instant and a hard kill costs at most the final fragment. ffmpeg is
# still backgrounded with SIGTERM/SIGINT trapped so the graceful path flushes
# that last fragment; it is no longer the only thing standing between a run
# and a watchable recording.
set -eu

WIDTH="${ARENA_WIDTH:-1440}"
HEIGHT="${ARENA_HEIGHT:-900}"
FPS="${ARENA_FPS:-10}"
FONT_SIZE="${ARENA_FONT_SIZE:-13}"
EVENTS="${ARENA_EVENTS:-/logs/agent/stella-events.jsonl}"
OUTPUT="${ARENA_OUTPUT:-/out/recording.mp4}"
DISPLAY_NUM="${ARENA_DISPLAY:-:99}"

# x264 sizes its thread pool from the number of CPUs it can SEE, which is the
# host's core count -- `--cpus` is a scheduling quota and does not change it.
# Each thread carries its own frame buffers, so on a big machine the encoder
# alone outgrows the recorder's 512 MB cgroup: measured on a 32-vCPU bench rig,
# x264 chose 28 threads and the container was OOM-killed within a second of
# ffmpeg starting. The kill is SIGKILL, so nothing is logged and no fragment is
# flushed -- every recording came out as a 28-byte `ftyp` stub and the run
# looked like the renderer had silently done nothing.
#
# Pinning the pool makes the encoder's footprint a property of this file rather
# than of whatever host it lands on. Two threads is ample for a 10 fps terminal
# capture at `--cpus 0.5`, which cannot keep more than that busy anyway.
THREADS="${ARENA_THREADS:-2}"

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

# Fragmented MP4, NOT +faststart.
#
# `+faststart` writes the index (`moov`) only when encoding *ends*. That makes
# the graceful path below the single point of failure for the whole recording:
# if this container is SIGKILLed rather than asked to stop -- the match process
# tree is killed, the host reboots, Docker prunes -- the file is left as
# `ftyp | free | mdat(size=0)`, every frame present and no index, which no
# player on earth will open. Measured: 4 of 15 recordings from one head-to-head
# died exactly this way, three of them 96MB of perfectly good video that had to
# be rebuilt by hand.
#
# `+empty_moov` writes the index up front and `+frag_keyframe` closes a
# fragment on every keyframe, so the file on disk is playable at every instant,
# including while it is still being written. The graceful shutdown below is
# still worth doing -- it flushes the final fragment -- but it is now an
# optimisation rather than the difference between a video and 96MB of nothing.
ffmpeg -nostdin -loglevel error \
  -f x11grab -video_size "${WIDTH}x${HEIGHT}" -framerate "$FPS" -i "$DISPLAY_NUM" \
  -c:v libx264 -preset veryfast -crf 26 -pix_fmt yuv420p \
  -threads "$THREADS" \
  -movflags +frag_keyframe+empty_moov+default_base_moof \
  -y "$OUTPUT" &
FFMPEG_PID=$!
log "recording -> $OUTPUT"

finish() {
  # SIGINT, not SIGTERM: ffmpeg finalises the container on INT and dies
  # abruptly on TERM. With fragmented output the file is already playable, so
  # this now saves the last partial fragment rather than the whole recording.
  kill -INT "$FFMPEG_PID" 2>/dev/null || true
  wait "$FFMPEG_PID" 2>/dev/null || true
  kill "$XTERM_PID" "$XVFB_PID" 2>/dev/null || true
  log "finalised $OUTPUT"
  exit 0
}
trap finish TERM INT

wait "$FFMPEG_PID" 2>/dev/null || true
kill "$XTERM_PID" "$XVFB_PID" 2>/dev/null || true
