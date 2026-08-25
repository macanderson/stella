#!/usr/bin/env bash
#
# agg-geometry.sh — the raster arithmetic `record-demo.sh` needs to hit a
# requested output size, plus the delivery ladder derived from it.
#
# Sourced, never executed. Every function here is pure: it reads its
# arguments and prints a number. That is what lets scripts/test-record-demo.sh
# check the derivation without recording anything or installing agg.
#
# ── The raster model ────────────────────────────────────────────────────────
#
# agg 1.9.0's output size is exactly linear in --font-size, with no intercept
# (measured in PR #4480 across font sizes 14/20/28/40/84 and grids 100x30,
# 126x30 and 60x50):
#
#   width  = font_size * (0.6 * cols + 1.2)
#   height = font_size * (1.4 * rows + 1.4)
#
# The cell is 0.6 x 1.4 font-pixels and the padding totals 1.2 font-pixels
# horizontally and 1.4 vertically. So a requested WxH inverts to a font size,
# and the smaller of the two inversions is the one that fits inside both.
# ffmpeg then pads to the exact rung, because a cell count rarely divides a
# ladder size evenly.

# The font size whose raster fits inside WIDTHxHEIGHT for a COLSxROWS grid.
# Never returns 0: agg refuses a zero font size, and a requested size too
# small for the grid should render tiny rather than fail.
agg_font_size_for() {
  local cols="$1" rows="$2" width="$3" height="$4"
  awk -v c="$cols" -v r="$rows" -v w="$width" -v h="$height" 'BEGIN {
    fw = w / (0.6 * c + 1.2)
    fh = h / (1.4 * r + 1.4)
    f = (fw < fh) ? fw : fh
    f = int(f)
    if (f < 1) f = 1
    print f
  }'
}

# The raster agg produces for a COLSxROWS grid at FONT_SIZE, as "WxH".
agg_raster() {
  local cols="$1" rows="$2" font="$3"
  awk -v c="$cols" -v r="$rows" -v f="$font" 'BEGIN {
    printf "%dx%d", int(f * (0.6 * c + 1.2) + 0.5), int(f * (1.4 * r + 1.4) + 0.5)
  }'
}

# The standard delivery ladder, widest first. One rung per line, "WxH".
agg_ladder_rungs() {
  printf '%s\n' 6144x3456 3840x2160 1920x1080 1280x720 854x480 640x360
}

# The ladder rungs that fit inside a master of WIDTHxHEIGHT — every rung at or
# below it, widest first.
#
# It never upscales. A 6144-wide rung produced by stretching a 4K
# master is a 4K master with a bigger filename, and shipping one as a 6K cut
# would be the kind of flattering-but-false number this repository refuses.
agg_ladder_for() {
  local width="$1" height="$2" rung rw rh
  while read -r rung; do
    rw="${rung%x*}"
    rh="${rung#*x}"
    if [ "$rw" -le "$width" ] && [ "$rh" -le "$height" ]; then
      printf '%s\n' "$rung"
    fi
  done < <(agg_ladder_rungs)
}

# The H.264 level to declare for a WIDTHxHEIGHT rung at 60 fps.
#
# A level is an upper bound a decoder promises to handle, so declaring one too
# low is what breaks playback; the values below are the lowest that admit each
# rung's macroblock rate at 60 fps. 480p and 360p share 3.1 because 3.0's
# 40500 MB/s ceiling is under 640x360p60's 55200.
agg_h264_level() {
  local width="$1"
  if   [ "$width" -gt 3840 ]; then printf '6.1'
  elif [ "$width" -gt 1920 ]; then printf '5.2'
  elif [ "$width" -gt 1280 ]; then printf '4.2'
  elif [ "$width" -gt 854 ];  then printf '3.2'
  else                             printf '3.1'
  fi
}
