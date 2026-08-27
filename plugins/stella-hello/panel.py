#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh
"""stella-hello's panel process: read one request, write one frame, exit.

The whole protocol is here. The host writes a `PanelRequest` to this process's
stdin and closes it; this writes a `PanelResponse` and a newline to stdout and
exits. There is no loop, no daemon and no socket to manage — a panel is asked
for one frame at a time, so a plugin that draws a frame and stops is the normal
shape.

A frame paints in *token names* (`gold`, `silver`, `muted`), never RGB. The
host resolves them against the live theme, which is what lets a panel follow a
sixteen-colour terminal without knowing one exists, and what stops a plugin
authoring a colour the brand's hue clamp exists to refuse.
"""

import json
import sys

PROTOCOL_VERSION = 1

# Every glyph here is one cell wide. A panel is measured in cells, and the host
# clips a row that runs past its lease, so a wide glyph would cost the row's
# last character rather than rendering half.
METER_FULL = "█"
METER_EMPTY = "░"


def span(text, fg=None, bold=False):
    """One run of glyphs, styled by token name."""
    style = {}
    if fg:
        style["fg"] = fg
    if bold:
        style["emphasis"] = ["bold"]
    out = {"text": text}
    if style:
        out["style"] = style
    return out


def frame(lease):
    """The rows this plugin draws, given the rectangle it was leased."""
    cols = lease["rect"]["cols"]
    rows = lease["rect"]["rows"]

    # A meter that fits whatever width the host gave us, so the panel is
    # right at 40 columns and at 200.
    bar_width = max(0, min(24, cols - 12))
    filled = bar_width // 2
    lines = [
        {"spans": [span("hello from a plugin", "gold", bold=True)]},
        {"spans": []},
        {
            "spans": [
                span("leased  ", "muted"),
                span(f"{cols}×{rows} cells", "text"),
            ]
        },
        {
            "spans": [
                span("meter   ", "muted"),
                span(METER_FULL * filled, "gold"),
                span(METER_EMPTY * (bar_width - filled), "border"),
                span("  50%", "muted"),
            ]
        },
        {"spans": []},
        {"spans": [span("this row is drawn by a separate process", "silver")]},
        {"spans": [span("the border and title above are the host's", "dim")]},
    ]
    # Never return more rows than the lease has: the host would clip them, and
    # a plugin that relies on being clipped is a plugin that draws garbage the
    # day the host stops clipping.
    return lines[:rows]


def main():
    request = json.load(sys.stdin)
    lease = request["body"]
    response = {
        "point": "frame",
        "body": {
            "protocol_version": PROTOCOL_VERSION,
            "tick": lease["tick"],
            "paint": {"lines": frame(lease)},
        },
    }
    # The newline ends the frame. The host reads one line rather than to
    # end-of-file, so a panel that backgrounds a helper still answers on time.
    json.dump(response, sys.stdout)
    sys.stdout.write("\n")
    sys.stdout.flush()


if __name__ == "__main__":
    main()
