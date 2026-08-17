# stella-transcript

One transcript information model, two renderers: the jet-black web surface the
Observatory serves, and a character grid for the TUI. Pure functions over owned
data — nothing here reads a file, spawns a process, formats a timestamp or
touches the network.

## Boundary

Depends on exactly one workspace crate, [`stella-diff`](../stella-diff/README.md),
so a `write_file`/`edit_file`/`delete_file` call renders the same `@@` hunks the
Observatory's prompt-diff view already shows. Nothing else: no store, no
protocol, no terminal library. A caller converts its own event stream into
[`model::Run`] and gets a `String` (HTML) or a `Vec<grid::Line>` back.

The crate exists because there were two renderers and no shared model. The TUI
had `render/entry.rs` + `diff.rs` in Rust; the Observatory had a hand-rolled
JavaScript re-implementation inside a single 4,000-line embedded asset. That
copy had drifted to the point of rendering a file edit as raw body text in a
`<pre>` — no hunks, no line numbers, no word-level highlights, nothing. Two
renderings of one thing will always drift; two renderings *of one model* can
only differ in ink.

## What the model fixes structurally

The view this replaces was a flat dark dump. Each of its defects is structural
rather than cosmetic, so each is fixed structurally:

| Defect | Structural fix |
|---|---|
| The command appeared up to three times per call | `Call` owns its `Output`, so they cannot be rendered apart; the invocation lives in exactly one field; `Call::extra_args` drops any argument the header already showed; and `digest::command_bar` draws the `$` bar **only when the digest elided the command** |
| Call and result rendered as siblings | They are one node — there is no API that yields one without the other |
| A raw JSON argument blob | Arguments are key/value rows behind a toggle, and only the ones not already displayed |
| Accounting inline at the weight of the work | `digest::Chip` is the only carrier: right-aligned, muted, subordinate to the digest |
| `… 24 more lines` as dead text | `digest::fold_output` returns the hidden count *and* the hidden lines *and* the tail |

## Semantics worth knowing

- **Folds are the reader's state, not the transcript's.** `FoldState` holds
  overrides layered over a zoom-derived default, so collapsing a turn writes one
  entry and touches nothing below it. That is what makes "collapsing a parent
  preserves child fold state" fall out rather than needing to be maintained.
- **A failed step cannot be folded away.** `Status::pins_open` beats both the
  reader's override and the zoom preset, so a failure stays expanded after the
  run completes. Closing it is not a thing the UI can express.
- **A digest is a summary, not truncated content.** It is composed from named
  fields; output text never reaches it. `digest::elide` cuts in the middle,
  because both ends of an invocation carry information and a trailing cut keeps
  the wrong half.
- **Word-level highlights are dropped when they would saturate.** Above
  `word::SATURATION` of a line, the line tint already says everything and a
  second tint is noise. Below `word::SHORT_RUN` characters — or when the two
  changed tokens share an affix — the pair is re-diffed at character
  granularity, so `--fast` → `--fast2` highlights `2` and not the whole flag.
- **Context lines are never coloured.** Colouring them is what makes a diff read
  as "everything changed".
- **Costs are integer micro-dollars.** No float enters the formatting path, so a
  rendered cost is identical on every re-render and a rollup of forty steps is
  reproducible.

## TUI parity

`grid` emits styled `Cell`s and encodes at the very end, which is what lets a
golden test assert the *plain* grid (readable in a diff) while the ANSI encoders
are checked separately. `to_ansi256` paints word-level changes with `48;5;n`
background spans; `to_ansi16` degrades them to bold + underline, which is a real
loss of elegance and no loss of information.

Every gutter width is a `const` and every row is padded to it, so the fold
marker — one cell in both states — cannot move a column to its right.

## Trying it

```sh
cargo run -p stella-transcript --example demo -- html > /tmp/demo.html
cargo run -p stella-transcript --example demo -- tui
cargo run -p stella-transcript --example demo -- plain
```

The fixture is the session the design was drawn against, so the output is
directly comparable with the reference renderings.

## God files — do not add lines

This crate has no god files: no file exceeds the gate's 1500-line ratchet
(`scripts/check-file-size.sh`), and none may appear — a new file crossing
1500 lines fails the gate outright, and `scripts/file-size-baseline.txt`
accepts no new entries. When a file here approaches the limit, split it before
it crosses.
