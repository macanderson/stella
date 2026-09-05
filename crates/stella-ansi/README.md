# stella-ansi

Strip ANSI escape sequences from text. What is left is what a person would
see on a plain page, with no terminal to read the color codes.

```rust
stella_ansi::strip_ansi("\u{1b}[32mok\u{1b}[0m\nline two"); // "ok\nline two"
```

## Boundary

One function, over a borrowed `&str`. No dependencies, no I/O. A child
process can add color codes to its own output. That output can end up in a
transcript. This crate is the one place that strips the codes out, before a
surface that cannot read them — a browser, a plain-text export — has to try.

Text with no escape byte in it comes back unchanged, and costs no new
memory. That is the common case, and why the return type is `Cow<'_, str>`
and not an owned `String`.

## Why this is its own crate

[`strip_ansi`] moved here from [`stella-tui`](../stella-tui). That crate
still holds the other half of ANSI work: turning styled text back into
color codes (`line_to_ansi`, `AnsiPalette`).

[`stella-observatory`](../stella-observatory) needed the strip function
too, for its own web page. A tool's colored output reaches that page's
`execution-journal` route just as raw as it reaches a terminal. A browser
cannot read the codes. It shows them as plain, ugly text instead.
`stella-observatory` could not depend on `stella-tui` to get the function.
That crate pulls in `ratatui`, a library the web page has no use for.
[`stella-diff`](../stella-diff) solved the same kind of problem before: a
small shared tool with no other crate baked in, so both sides can use it.
This crate follows that same shape. `stella-tui`'s `ansi` module
re-exports [`strip_ansi`] now, so no caller there had to change.

This crate is also a worked example of the three-case rule in AGENTS.md
§ "When a new crate is justified". It is case (a). The work sits behind a
clean seam. Left where it was, it would pull `ratatui` — a heavy library —
into a crate that carries almost nothing today: the observatory.

Strip the text **before** you cut it down to a fixed size, never after. A
cut made first spends part of its budget on bytes nobody sees. It can also
slice a color code in half, leaving junk in the output.
`stella-observatory`'s `journal::set_journal_body` strips first and cuts
second, and every caller here should follow that order.

## God files — do not add lines

This crate has no god files. No file here is close to the gate's 1500-line
limit (`scripts/check-file-size.sh`). A new file cannot cross that line
either: the gate blocks it, and `scripts/file-size-baseline.txt` will not
take a new entry. If a file here gets close to the limit, split it first.
