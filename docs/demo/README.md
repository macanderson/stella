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
| `stella-deck.mp4` | The **hero film**: 1080p60, silent, loopable, 68.7s — one scripted run told in four acts across the tui-v2 deck, with a camera that opens each tab wide and makes one push. `stella-deck-script.md` is its narration, cut to the same timestamps. | `make record-demo-video` |
| `stella-demo.mp4` | A **live-run timelapse** (#796): `stella run` building a small word-frequency CLI end to end against a real provider, recorded with `scripts/record-demo.sh`. Real, and dated — it predates the deck. | `scripts/record-demo.sh -- <command>` |

`AGENTS.md`'s companion rule in `CLAUDE.md` states the standard: *footage of
this repo compiling is never demo content.* `scripts/demo-scenario.sh` is a
load generator for `scripts/record-demo.sh`, nothing more.

## What the hero film shows, exactly

The deck's **own render**, over the **scripted session fixture** — not a live
agent run against a provider.

That distinction is required and is stated here rather than left for a
viewer to infer. Every glyph in the film comes from `render_deck`: the real
layouts, the real palette, the real panel geometry, the real diff rendering,
the real `agent_engine_config` editor. The session driving them is
`stella_tui::scenario::demo_inbound` — the same 37-event fixture the
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
make record-demo-video             # both stages, into docs/demo/
```

Two stages, deliberately separate:

```sh
# 1. The film: a deterministic frame stream, ~1.8 MB of JSONL.
cargo run -q --release -p stella-tui --example deck_film > film.jsonl

# 2. The pixels: 1920x1080, 60 fps, H.264 High / yuv420p, faststart, no audio.
scripts/render-deck-film.py film.jsonl -o docs/demo/stella-deck.mp4 \
    --poster docs/demo/stella-deck-poster.png --poster-frame 807
```

A larger master takes `--size`: `--size 6144x3456 --supersample 1.6 --crf 14`
is the 6K cut the delivery ladder (4K, 1080p, 720p, 480p, 360p) is scaled
down from, and at that size the raster behind each grid is ~9800 px wide, so
`--supersample` is read against the output frame and set just above the
film's peak zoom rather than left at 2.5.

`--release` is not a suggestion: the driver rebuilds the session model from
scratch for every one of the 4,122 frames — which is what makes frame *n*
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

Rasterises each distinct grid **once** at 2.5x the output resolution
(4788x2697, 42x93 px cells) and cuts every output frame as a sub-pixel crop
resized down to 1080p. That is the whole trick behind the zoom: at every point
in the camera track the frame is a *downscale* of a raster with more detail
than it needs, so text is supersampled rather than magnified. Scaling the
finished 1080p frame up instead — what a video editor's zoom does — is why most
terminal demos go soft the moment the camera moves.

The peak zoom in the film is checked against `--supersample` up front, and the
run refuses rather than quietly rendering one blurry shot.

**The cell is solved from the font, in this order: rows → cell height → font
size → cell width.** A monospace face pins two metrics that both have to fit —
an advance (0.600 em) and a line box (ascent + descent, 1.32 em) — and only one
of them can be chosen. Fixing the row count fixes the cell height; the largest
size whose *line box* fits gives the font; its advance gives the cell width.

Doing it the other way round is what shipped in the first cut: sizing the font
from the cell's width picks `S = cell_w / 0.6`, whose line box is then 32%
taller than the cell. Rows drew into each other, the baseline landed *below* the
cell floor so the bottom row was clipped by the frame edge, and the grid came
out 3.2:1 — a squeezed band inside a 16:9 frame. `Rasteriser.__init__` now
measures both metrics and refuses either failure by name, rather than rendering
something that reads as a corrupt terminal.

Fonts fall back per glyph the way a terminal does: JetBrains Mono (the brand
face, `docs/brand/fonts/`, OFL-1.1) → DejaVu Sans Mono → DejaVu Sans → Noto
Sans Symbols 2 → Noto Sans Symbols → FreeMono (Arial Unicode on macOS). The
deck's frames reach for 131 distinct characters and no single monospace face
carries all of them; the v2 deck's `☰`, `⏸`, `⎿`, `⌕` and the fullwidth `＋`
each needed a face the first cut's chain did not have. Each entry is looked
for at its Debian path and its Homebrew path, so the pipeline runs on a Mac
after `brew install --cask font-dejavu font-noto-sans-symbols
font-noto-sans-symbols-2`. A glyph that survives the whole chain is a hard
error naming the codepoint — a tofu box in a hero video is worse than a
failed build.

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

## Re-cut it every release

The film is a picture of the deck, so it goes stale exactly when the deck
changes — a tab added, a panel moved, a status field renamed. A demo showing a
UI the download no longer has is the kind of claim this repository does not
make, so `make record-demo-video` runs as part of cutting a release. It needs no
API key and costs about fifteen minutes.

**Watch the result before committing it.** The shot list frames rows and columns
that the deck's own layout decides, so a layout change can move content out of
shot — or into a frame that shows a rendering defect — without failing anything.
Nothing in the pipeline can check composition for you.

## Changing what it shows

The shot list is data, in `shots()`. A shot is a scene, a duration, a camera
track, a cursor range, and how many of the scripted session's events have
streamed in by its end — a count, named in `EVENTS`, because the story beats
are specific events: the shot that ends on the edit's inline diff ends on the
`ToolResult` that claims it. `film()` pins `EVENTS.all` against the fixture's
length, so a fixture edit that moves a beat fails the run by name. To change
the film, change that table — nothing else in either stage knows what the
film is about. The narration (`stella-deck-script.md`) is cut to the same
starts; re-time a shot and move its cue.

The camera has one rule, and the previous cut is the reason it has it: a tab
opens wide, holds, and makes one push onto the thing the narration names,
and every cut lands on wide. A cut that happens at 1.45x into a different
tab reads as a zoom at random, however deliberate the track was.

Two constraints the table cannot break on its own:

- **Peak zoom must stay at or below `--supersample`** (2.5 by default), or the
  renderer refuses.
- **The grid is 114x29, and it is a solution rather than a preference.** A grid
  that fills a 16:9 frame is one with roughly four columns per row — that falls
  straight out of the font's two ratios, and `COLS`' doc comment carries the
  derivation. Within that family the width is bounded on both sides: below ~108
  columns the deck's own composer footer collides with its counter (#3591), and
  wide is what produced the video this replaced. Re-run `--extents` (the
  row-density profile) after changing either number, and re-frame the shots that
  sit on rows you moved.
