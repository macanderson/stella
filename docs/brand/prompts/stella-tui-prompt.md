Restyle stella's TUI to the official brand. The brand is fixed; your job is translation to the terminal medium — keep the app's existing framework and behavior, change its presentation. Work within whatever TUI stack the codebase already uses (Ink / Textual / Ratatui / Bubble Tea / raw ANSI); everything below is framework-agnostic spec.

## brand in one line

stella is Latin for star. The mark is a comet — a four-point star with three trails, flying left→right. One color: Phosphor Gold. Principles: one shape · one color · assemble, don't spin · terminal-native. All UI copy is lowercase ("thinking…", "done", "install stella") — the brand name is never capitalized.

## the text-native logomark

The lockup in plain text is exactly this, and it's load-bearing everywhere:

```
≡✦ stella
```

`≡` (U+2261) is the three trails, `✦` (U+2726) is the star — both gold, name in bold default-foreground. ASCII fallback: `=* stella`. Hollow star `✧` (U+2727) means pending/in-progress; solid `✦` means done/active. Pad one cell of clearspace around the lockup; never let borders touch it.

## color system

Detect capability in this order: `$COLORTERM` contains `truecolor`/`24bit` → use hex; else 256-color → use the index column; else basic ANSI → last column. Respect `NO_COLOR` (plain text, keep bold/dim). Never repaint the user's terminal background — the app inherits it; only panels/badges may set bg.

| role | truecolor | 256 | 16-color |
|---|---|---|---|
| gold (brand accent) | `#FFB000` | 214 | yellow + bold |
| gold-dim (secondary accent) | `#FCD07D` | 222 | yellow |
| gold-deep (accent on LIGHT terminals) | `#A37200` | 136 | yellow |
| text | terminal default fg | default | default |
| muted (chrome, labels, borders text) | `#9B9890` | 246 | bright black |
| border | `#232327` | 235 | bright black |
| panel bg (sparingly) | `#131315` | 233 | — |
| error | `#E4573F` | 167 | red |
| success (semantic only) | terminal green | 71 | green |
| diff add / del | green 71 / red 167 | 71 / 167 | green / red |

Light-terminal adaptation: probe background via OSC 11 (fallback `$COLORFGBG`, config override `theme = auto|dark|light`). On light backgrounds swap gold→gold-deep for any colored TEXT (star/trail glyphs may stay `#FFB000`), and muted→`#6E6A5F` (241).

Budget: gold appears in at most ~5 places per screen — the lockup, the prompt star, the spinner, one primary action, one key stat. Everything else is default text and muted. Gold is the signal, never the surface: no gold backgrounds except the single primary button/toast. Green and red exist only for semantic success/diff/error — never decoration. Warnings are gold (`✦! lowercase message`), errors are red (`✕ message`); never color-only — always pair glyph + text.

## the spinner — assemble, don't spin

Kill every rotating/braille/dots spinner. stella's working indicator builds the comet (80ms/frame, fixed-width field):

```
working loop (repeat):        completion (play once, then show result):
1  ‐                          7   ≡ ✦
2  ‐‐                         8    ≡ ✦
3  ≡                          9      ≡ ✦
4  ≡ ✧                        10        ≡✦
5  ≡ ✦                        11          ✦
6  ≡ ✦   (hold)               12  (clear) → ✦ done in 4.2s
```

Trails/star gold; the working loop is frames 1–6 only; on task completion run the fly-off (7–12) once, then print the result line prefixed `✦`. On failure, stop the loop and print `✕` red. Provide `--no-animation` (and honor it when not a TTY): render static `≡✧` while working, `✦` when done. Blinking block cursor `█` in the input is allowed; nothing else ever blinks or pulses idly.

## components to restyle

- **splash/startup** — the **assemble mark**, not a one-liner. Shown only while session init is genuinely still running, it plays the identity assembling: the three comet trails slide in staggered, the four-point star pops with a brief bright flash, the wordmark types on letter by letter behind a gold block cursor, and the cursor blinks until init hands off. The star is not stored art — it is the astroid `|x|^(2/3) + |y|^(2/3) <= r^(2/3)` rasterized into half-blocks at ~30% of the frame height, so the mark scales with the terminal rather than shipping one bitmap. On a frame too small to carry that, it falls back to the one-line lockup `≡✦ stella` + dim `vX.Y.Z · the terminal agent` with a single reveal step. `✦ stella` rides the tab bar's top border once the deck is up.

  This is a deliberate divergence from "no big ASCII art", which the rest of this document still means everywhere else — the launch mark is the one place the identity gets to be the whole frame, and only while the user is already waiting. The contract that makes it safe is unchanged: it is a **pure function of elapsed time** (two renders at the same `t` are byte-identical, a dropped frame scrubs instead of stalling), **any key skips it immediately**, and reduced motion (`--no-anim` / `STELLA_NO_ANIM` / `NO_COLOR`) collapses it to one static finished frame. `cargo run -p stella-tui --example splash_frames [W H]` prints the shipped timeline.
- **input prompt**: gold `✦ ` as the prompt marker (the star IS the prompt). User input in default fg. While the agent works, the marker cell becomes the spinner field. Multiline continuation marker: dim `·`.
- **status bar**: left `≡✦ stella` · center cwd / model / branch in muted · right tokens + cost with gold numerals, muted units. Single line, no bg fill in dark terminals (optional panel bg 233).
- **panels/boxes**: rounded borders `╭─╮ │ ╰─╯` in border color; title lowercase muted, sits in the top border. Focused pane: border goes gold. ASCII fallback `+-|`.
- **section headers / overlines**: `UPPERCASE` dim, tracked with spaces if you like (`T O O L S`) — the only uppercase in the app.
- **tool-call / agent steps**: each step line starts `✧` (running) → `✦` (ok) / `✕` (failed), name bold, detail muted, duration right-aligned muted. Nesting indents by two cells.
- **progress bars**: filled `█` gold, empty `╌` border-color, percent in gold numerals: `██████╌╌╌╌ 62%`.
- **diffs**: `+` lines green, `-` lines red, context default, file header bold, hunk `@@` muted. No colored backgrounds.
- **markdown rendering**: headings bold (h1 prefixed with gold `#`), inline code on panel bg 233, code blocks in a bordered box with dim language tag, links underlined gold (gold-deep on light), bullets muted `·`, blockquote bar muted `▎`.
- **lists/menus**: selected row = gold `✦` marker + bold text (no full-row gold bg); unselected marker is a muted `·`.
- **primary action / confirm toast**: the one allowed gold fill — `[ install ]` gold bg, ink `#0B0B0C` text. Secondary actions are bordered, no fill.
- **errors**: red `✕`, lowercase message, dim hint line under it. **empty states**: muted text + a single small `✧`.
- **completion celebrations**: after a successful run, the comet flies off across the current line once (frames 7–12) — never confetti, never rainbow.

## acceptance checklist

Degrades cleanly to 256, 16-color, `NO_COLOR`, and non-UTF-8 (`=*`, `+-|`, `#` fallbacks). Legible on dark AND light terminals (gold-deep swap applied). No idle spinning anywhere; `--no-animation` static states work; non-TTY output is plain. Gold count ≤5 per screen; green/red only semantic. All copy lowercase except overlines. The lockup `≡✦ stella` renders correctly in the splash's small-frame fallback, the status bar, and the `--help` header. `--help` output styled: usage line with gold `✦` prompt examples, flags bold, descriptions muted.
