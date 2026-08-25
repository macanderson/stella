#!/usr/bin/env bash
#
# test-record-demo-size.sh — the witness for `record-demo.sh --size` (#4375).
#
# Hermetic: it sources scripts/lib/agg-geometry.sh and checks the arithmetic
# against the raster sizes PR #4480 measured out of agg 1.9.0. Nothing is
# recorded, agg is never installed, ffmpeg is never called — which is the
# point of having the derivation in a sourceable library at all. Before it
# existed there was no way to check a size claim without a multi-minute
# render.
#
# Run: ./scripts/test-record-demo-size.sh   (or `make record-demo-size-test`)

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=scripts/lib/agg-geometry.sh
. "$HERE/lib/agg-geometry.sh"

FAILED=0

check() {
  local what="$1" want="$2" got="$3"
  if [ "$want" = "$got" ]; then
    printf '  ok   %s\n' "$what"
  else
    printf '  FAIL %s: want %s, got %s\n' "$what" "$want" "$got"
    FAILED=1
  fi
}

echo "agg raster model (measured, PR #4480):"
# The two figures #4375 quotes, reproduced exactly.
check "126x30 @ 20px" "1536x868" "$(agg_raster 126 30 20)"
check "126x30 @ 84px" "6451x3646" "$(agg_raster 126 30 84)"
check "100x30 @ 20px" "1224x868" "$(agg_raster 100 30 20)"

echo "--size inverts the model:"
# The derived size must FIT: its raster is inside the request on both axes.
for want in 1920x1080 3840x2160 1280x720; do
  w="${want%x*}"
  h="${want#*x}"
  font="$(agg_font_size_for 126 30 "$w" "$h")"
  raster="$(agg_raster 126 30 "$font")"
  rw="${raster%x*}"
  rh="${raster#*x}"
  if [ "$rw" -le "$w" ] && [ "$rh" -le "$h" ]; then
    printf '  ok   %s -> --font-size %s (%s)\n' "$want" "$font" "$raster"
  else
    printf '  FAIL %s -> --font-size %s rendered %s, which does not fit\n' \
      "$want" "$font" "$raster"
    FAILED=1
  fi
  # And it must be the LARGEST that fits: one pixel more overflows an axis.
  bigger="$(agg_raster 126 30 $((font + 1)))"
  bw="${bigger%x*}"
  bh="${bigger#*x}"
  if [ "$bw" -le "$w" ] && [ "$bh" -le "$h" ]; then
    printf '  FAIL %s left a font size on the table: %s also fits\n' "$want" "$bigger"
    FAILED=1
  fi
done

echo "--size never returns a font size agg refuses:"
check "absurdly small request" "1" "$(agg_font_size_for 200 60 10 10)"

echo "the ladder is derived, never upscaled:"
check "from a 6K master" \
  "6144x3456 3840x2160 1920x1080 1280x720 854x480 640x360" \
  "$(agg_ladder_for 6144 3456 | tr '\n' ' ' | sed 's/ $//')"
check "from a 4K master" \
  "3840x2160 1920x1080 1280x720 854x480 640x360" \
  "$(agg_ladder_for 3840 2160 | tr '\n' ' ' | sed 's/ $//')"
check "from a 1080p master" \
  "1920x1080 1280x720 854x480 640x360" \
  "$(agg_ladder_for 1920 1080 | tr '\n' ' ' | sed 's/ $//')"
check "from a master under every rung" "" "$(agg_ladder_for 320 200)"

echo "H.264 levels admit 60 fps at each rung:"
check "6144 wide" "6.1" "$(agg_h264_level 6144)"
check "3840 wide" "5.2" "$(agg_h264_level 3840)"
check "1920 wide" "4.2" "$(agg_h264_level 1920)"
check "1280 wide" "3.2" "$(agg_h264_level 1280)"
check "854 wide" "3.1" "$(agg_h264_level 854)"
check "640 wide" "3.1" "$(agg_h264_level 640)"

echo "the script advertises the flags it parses:"
for flag in --size --font-size --ladder; do
  if grep -q -- "$flag" "$HERE/record-demo.sh"; then
    printf '  ok   %s\n' "$flag"
  else
    printf '  FAIL %s is parsed but undocumented\n' "$flag"
    FAILED=1
  fi
done

if [ "$FAILED" -ne 0 ]; then
  echo "FAILED"
  exit 1
fi
echo "PASS"
