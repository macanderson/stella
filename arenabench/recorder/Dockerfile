# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
#
# The ArenaBench screen recorder: a real X display, a real terminal emulator,
# and a real screen-capture encoder. `ffmpeg -f x11grab` reads the framebuffer
# of a live Xvfb display showing a live xterm, and encodes H.264 — so the MP4
# is genuine pixel capture, not a terminal-cast replay format re-rendered after
# the fact.
#
# Why a sidecar and not the task container: a Terminal-Bench task image is
# canonical. Installing Xvfb, xterm and ffmpeg into it would change the utility
# set the agent sees, invalidate the run's comparability, and in most images
# fail outright (no network, no apt). This container shares nothing with the
# task except a read-only bind mount of the log directory.
FROM debian:bookworm-slim

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      xvfb \
      x11-utils \
      xterm \
      ffmpeg \
      python3 \
      fonts-dejavu-core \
      procps \
 && rm -rf /var/lib/apt/lists/*

COPY render.py /opt/arena/render.py
COPY record.sh /opt/arena/record.sh
RUN chmod +x /opt/arena/record.sh

# UTF-8 is load-bearing, not cosmetic. The renderer draws box-drawing rules
# (U+2500); under the default POSIX locale xterm reads those three UTF-8 bytes
# as three latin-1 characters, so a full-width rule becomes ~3x too long, wraps
# several lines, and scrolls the transcript region off the screen. The symptom
# looks like "the body never renders" and the cause is a locale.
ENV LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    PYTHONIOENCODING=utf-8 \
    PYTHONUNBUFFERED=1

# Defaults are overridable per trial by the supervisor.
ENV ARENA_WIDTH=1440 \
    ARENA_HEIGHT=900 \
    ARENA_FPS=10 \
    ARENA_FONT_SIZE=13 \
    ARENA_EVENTS=/logs/agent/stella-events.jsonl \
    ARENA_OUTPUT=/out/recording.mp4

ENTRYPOINT ["/opt/arena/record.sh"]
