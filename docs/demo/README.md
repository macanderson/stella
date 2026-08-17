---
id: deck-film
title: "Demo recordings — the deck film pipeline"
status: living
---

# Demo recordings

Two different things live here, and conflating them is the mistake this page
exists to prevent.

| Artifact | What it is | How it is made |
| --- | --- | --- |
| `stella-deck.mp4` | The **hero film**: 1080p60, silent, loopable, 50.1s — the command deck touring every tab, with a camera that pushes in and pulls back. | `make deck-film` |
| `stella-demo.mp4` | An old **recorder stress-test**: a timelapse of this repository compiling. Not demo content, and never was. | `make record-demo` |

`AGENTS.md`'s companion rule in `CLAUDE.md` states the standard: *footage of
this repo compiling is never demo content.* `scripts/demo-scenario.sh` is a
load generator for `scripts/record-demo.sh`, nothing more.

## What the hero film shows, exactly

The deck's **own render**, over the **scripted session fixture** — not a live
agent run against a provider.

That distinction is load-bearing and is stated here rather than left for a
viewer to infer. Every glyph in the film comes from `render_deck`: the real
layouts, the real palette, the real panel geometry, the real diff rendering,
the real `agent_engine_config` editor. The session driving them is
`stella_tui::scenario::demo_inbound` — the same 38-event fixture the
golden-frame suite asserts against (`crates/stella-tui/tests/deck_render_snapshots.rs`),
folded a prefix at a time so the transcript streams rather than cuts.

So the film is an honest picture of **what the deck looks like** and a
dishonest one of **what a run costs**, if anyone reads the elapsed timers and
spend figures as measurements. They are fixture values. A film of a real run —
one that replays a captured `AgentEvent` log instead of the fixture — is
[#3556](https://github.com/macanderson/stella/issues/3556); it reuses every
stage below except the source of the events.

## The pipeline

```sh
make deck-film                     # both stages, into docs/demo/
```

Two stages, deliberately separate:

```sh
# 1. The film: a deterministic frame stream, ~1.8 MB of JSONL.
cargo run -q --release -p stella-tui --example deck_film > film.jsonl

# 2. The pixels: 1920x1080, 60 fps, H.264 High / yuv420p, faststart, no audio.
scripts/render-deck-film.py film.jsonl -o docs/demo/stella-deck.mp4 \
    --poster docs/demo/stella-deck-poster.png --poster-frame 1100
```

`--release` is not a suggestion: the driver rebuilds the session model from
scratch for every one of the 3,006 frames — which is what makes frame *n*
depend on nothing but *n* — and a debug build spends about twenty minutes on
it.

### Stage 1 — `crates/stella-tui/examples/deck_film.rs`

Emits styled character grids plus a camera track. It records the one thing that
is actually ours: `render_deck`'s output, which the golden suite already relies
on being a pure function of `(model, ui)`. A screen recording of a terminal
would instead record that terminal's font, colour handling, and compositor, and
could not be re-shot identically next month.

The shot list is the top of that file. `--shots` prints it:

```text
cargo run -q --release -p stella-tui --example deck_film -- --shots
```

`--verify` renders the film twice and compares it byte for byte. One clock read
survives, and it is declared: the launch mark's animation is driven by
`SplashState::elapsed()`, which `splash.rs` floors onto a 40 ms grid — so the
splash shot is deterministic in practice but is the one part of the film that
is not deterministic *by construction*.

### Stage 2 — `scripts/render-deck-film.py`

Rasterises each distinct grid **once** at 2.5x the width it occupies at zoom
1.0 (4800x1500, 30x50 px cells) and cuts every output frame as a sub-pixel crop
resized down to 1080p. That is the whole trick behind the zoom: at every point
in the camera track the frame is a *downscale* of a raster with more detail
than it needs, so text is supersampled rather than magnified. Scaling the
finished 1080p frame up instead — what a video editor's zoom does — is why most
terminal demos go soft the moment the camera moves.

The peak zoom in the film is checked against `--supersample` up front, and the
run refuses rather than quietly rendering one blurry shot.

The raster is shaped by the **font**, not by the frame: cell height is the cell
width over the advance ratio. Deriving it from `1080/rows` instead makes the
face taller than the cell is wide, and every run of text overlaps the one beside
it — a guard in `Rasteriser.__init__` now refuses that outright, because the
frames it produces look like a corrupt render rather than a misconfiguration.

Fonts fall back per glyph the way a terminal does: JetBrains Mono (the brand
face, `docs/brand/fonts/`, OFL-1.1) → DejaVu Sans Mono → FreeMono. The deck's
frames reach for 134 distinct characters and no single monospace face carries
all of them. A glyph that survives the whole chain is a hard error naming the
codepoint — a tofu box in a hero video is worse than a failed build.

## Publishing it

The film is cut for `<video autoplay loop muted playsinline>`: no audio track
at all, `faststart` so it begins before the whole file lands, and a fade to
black at both ends so the loop seam reads as a beat rather than a jump. The
poster still is the first frame a browser paints while the video loads.

Bitrate is a publishing decision, not a quality one, and this film is an
unusually expensive thing to encode: terminal text is high-frequency detail on
a flat ground, and the camera pans across it almost continuously, which is close
to the worst case for inter-frame prediction. x264's "visually lossless" CRF 18
puts this film at **31.2 MB (5.0 Mbps)** — more than a hero video should ask a
visitor to download — so the default is CRF 24. If you raise it further, judge
the result on a **zoomed** shot: the wide shots carry the least detail per pixel
and will look fine long after the close ones have started ringing.

## Re-cutting it

The shot list is data, in `shots()`. A shot is a scene, a duration, a camera
track, a cursor range, and how much of the scripted session has streamed in by
its end. To change the film, change that table — nothing else in either stage
knows what the film is about.

Two constraints the table cannot break on its own:

- **Peak zoom must stay at or below `--supersample`** (2.5 by default), or the
  renderer refuses.
- **The grid is 160x30 and both numbers are load-bearing.** 160 columns keeps
  the AGENTS dashboard above its compact threshold. 30 rows is what
  `--extents` — the row-density profile — reports as the height this fixture
  actually fills. The obvious 54 (which makes 160 columns exactly 16:9) leaves
  nine of the fourteen shots drawing nothing below row 9, a void no camera move
  can hide; 26 goes too far the other way and squeezes the transcript. Re-run
  `--extents` before changing it, and re-frame the shots that sit on the rows
  you moved.
